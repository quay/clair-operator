//! Clair_config is a module for validating a configuration with the
//! [`github.com/quay/clair/config`] module.
//!
//! Building this crate reqires `clang` and `go` toolchains installed.
//!
//! [`github.com/quay/clair/config`]: https://pkg.go.dev/github.com/quay/clair/config
#![warn(rustdoc::missing_crate_level_docs)]
#![warn(missing_docs)]

#[cfg(feature = "tokio")]
use std::sync::Arc;
use std::{
    collections::BTreeMap,
    ffi::{self, CStr},
    fmt::{Debug, Display, Formatter, Result as FmtResult},
    ptr,
};

use json_patch::{Patch, PatchError, merge, patch};
#[cfg(feature = "k8s")]
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use libc::free;
use serde_json::{Error as JsonError, Value, from_slice, to_vec};
#[cfg(feature = "tokio")]
use tokio::task::{JoinError, spawn_blocking};
use tracing::{debug, trace};

mod sys;

/// Error enumerates the errors reported by this module.
#[derive(Debug)]
pub enum Error {
    /// Configuration is invalid for some reason.
    Invalid(String),
    /// Validation failed for some reason.
    Validation(String),

    /// JSON serialiization or deserialization error.
    JSON(JsonError),
    /// JSON Patch error.
    Patch(PatchError),
    #[cfg(feature = "tokio")]
    /// Spawn error.
    Join(JoinError),

    /// Error for testing only.
    #[cfg(test)]
    Test(String),
}

impl std::error::Error for Error {}

impl Error {
    fn invalid<S: AsRef<str>>(msg: S) -> Error {
        Self::Invalid(String::from(msg.as_ref()))
    }
    fn validation<S: AsRef<str>>(msg: S) -> Error {
        Self::Validation(String::from(msg.as_ref()))
    }
    #[cfg(test)] // Only for the tests module
    fn test<S: AsRef<str>>(msg: S) -> Error {
        Self::Test(String::from(msg.as_ref()))
    }
}

impl From<JsonError> for Error {
    fn from(err: JsonError) -> Self {
        Self::JSON(err)
    }
}

impl From<PatchError> for Error {
    fn from(err: PatchError) -> Self {
        Self::Patch(err)
    }
}

#[cfg(feature = "tokio")]
impl From<JoinError> for Error {
    fn from(err: JoinError) -> Self {
        Self::Join(err)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        use Error::*;
        match self {
            Invalid(msg) => write!(f, "invalid ConfigSource: {msg}"),
            Validation(msg) => write!(f, "validation failure: {msg}"),
            JSON(err) => write!(f, "JSON error: {err}"),
            Patch(err) => write!(f, "json patch error: {err}"),
            #[cfg(feature = "tokio")]
            Join(err) => write!(f, "tokio join error: {err}"),
            #[cfg(test)]
            Test(msg) => write!(f, "testing error: {msg}"),
        }
    }
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Parts is the loaded parts of a Clair config, converted to JSON form.
pub struct Parts {
    root: Vec<u8>,
    dropins: BTreeMap<String, (Vec<u8>, bool)>,
}

impl Parts {
    /// Validate calls into [`config.Validate`] and reports the lints for every mode.
    /// This is done by composing the config in-process accoring to the [`cmd.Load`] documentation.
    /// The changes for defaults made by the `Validate` function are not returned, so that the config
    /// package can change the defaults as needed.
    ///
    /// [`config.Validate`]: https://pkg.go.dev/github.com/quay/clair/config#Validate
    /// [`cmd.Load`]: https://pkg.go.dev/github.com/quay/clair/v4/cmd#Load
    pub fn validate(&self) -> Result<Validate> {
        let doc = self.render()?;
        Ok(Validate {
            indexer: validate_config(&doc, "indexer"),
            matcher: validate_config(&doc, "matcher"),
            notifier: validate_config(&doc, "notifier"),
            updater: validate_config(&doc, "updater"),
        })
    }

    #[cfg(feature = "tokio")]
    /// Validate_async is an asynchronous version of [`validate`].
    ///
    /// It uses tokio's [`block_in_place`] to signal to the executor that the thread is about to do
    /// FFI.
    pub async fn validate_async(&self) -> Result<Validate> {
        let doc = Arc::new(self.render()?);

        let (indexer_doc, matcher_doc, notifier_doc, updater_doc) =
            (doc.clone(), doc.clone(), doc.clone(), doc.clone());
        let (indexer, matcher, notifier, updater) = tokio::try_join!(
            spawn_blocking(move || validate_config(&indexer_doc, "indexer")),
            spawn_blocking(move || validate_config(&matcher_doc, "matcher")),
            spawn_blocking(move || validate_config(&notifier_doc, "notifier")),
            spawn_blocking(move || validate_config(&updater_doc, "updater")),
        )?;
        Ok(Validate {
            indexer,
            matcher,
            notifier,
            updater,
        })
    }

    fn render(&self) -> Result<Vec<u8>> {
        let doc = from_slice(&self.root)?;
        let doc = self
            .dropins
            .iter()
            .fold(doc, |mut doc, (name, (buf, is_patch))| {
                if *is_patch {
                    let p: Patch = from_slice(buf).expect("failed to load patch");
                    trace!(name, "applying patch");
                    patch(&mut doc, &p).expect("failed to apply patch");
                } else {
                    let m: Value = from_slice(buf).expect("failed to parse JSON");
                    trace!(name, "merging config");
                    merge(&mut doc, &m);
                };
                doc
            });
        trace!("config rendered");
        let doc = to_vec(&doc)?;
        Ok(doc)
    }
}

impl From<Builder> for Parts {
    fn from(b: Builder) -> Self {
        Self {
            root: b.root,
            dropins: b.dropins,
        }
    }
}

/// Builder constructs all the root and dropins for a configuration.
pub struct Builder {
    root: Vec<u8>,
    dropins: BTreeMap<String, (Vec<u8>, bool)>,
}

impl Builder {
    #[cfg(feature = "k8s")]
    /// From_root constructs a Builder starting with the root config.
    pub fn from_root<S: ToString>(v: &ConfigMap, key: S) -> Result<Self> {
        let key = key.to_string();
        let root = if let Some(map) = &v.data {
            map.get(&key).map(|s| s.clone().into_bytes())
        } else if let Some(map) = &v.binary_data {
            map.get(&key).map(|b| b.0.clone())
        } else {
            unreachable!()
        }
        .ok_or_else(|| Error::invalid(format!("missing key: {key}")))?;
        Ok(Builder {
            root,
            dropins: Default::default(),
        })
    }

    /// Add adds a dropin.
    pub fn add<M, S>(mut self, map: M, key: S) -> Result<Self>
    where
        M: K8sMap,
        S: ToString,
    {
        let key = key.to_string();
        let is_patch = key.ends_with("-patch");
        let buf = map
            .value(key.clone())
            .ok_or_else(|| Error::invalid(format!("missing key: {key}")))?;
        trace!(key, is_patch, "loaded key");
        self.dropins.insert(key, (buf, is_patch));
        debug!("added dropin");
        Ok(self)
    }
}

mod private {
    pub trait Sealed {}
}
use private::Sealed;

/// K8sMap is a k8s map-type: a ConfigMap or a Secret.
pub trait K8sMap: Sealed {
    /// Value returns the value for the key.
    fn value(&self, key: String) -> Option<Vec<u8>>;
}

#[cfg(feature = "k8s")]
impl Sealed for ConfigMap {}
#[cfg(feature = "k8s")]
impl K8sMap for ConfigMap {
    fn value(&self, key: String) -> Option<Vec<u8>> {
        if let Some(data) = &self.data
            && let Some(buf) = data.get(&key)
        {
            return Some(buf.clone().into_bytes());
        };
        if let Some(data) = &self.binary_data
            && let Some(buf) = data.get(&key)
        {
            return Some(buf.0.clone());
        };
        None
    }
}

#[cfg(feature = "k8s")]
impl Sealed for Secret {}
#[cfg(feature = "k8s")]
impl K8sMap for Secret {
    fn value(&self, key: String) -> Option<Vec<u8>> {
        if let Some(data) = &self.data
            && let Some(buf) = data.get(&key)
        {
            return Some(buf.0.clone());
        };
        None
    }
}

impl Sealed for BTreeMap<String, String> {}
impl K8sMap for BTreeMap<String, String> {
    fn value(&self, key: String) -> Option<Vec<u8>> {
        self.get(&key).map(|v| v.clone().into_bytes())
    }
}

impl Sealed for BTreeMap<String, Vec<u8>> {}
impl K8sMap for BTreeMap<String, Vec<u8>> {
    fn value(&self, key: String) -> Option<Vec<u8>> {
        self.get(&key).map(|v| v.clone())
    }
}

/// Validate reports results for all the Clair operating modes.
///
/// The "updater" mode is not implemented in the upstream config module yet.
pub struct Validate {
    /// Result for validating for the "indexer" mode.
    pub indexer: Result<Warnings>,
    /// Result for validating for the "matcher" mode.
    pub matcher: Result<Warnings>,
    /// Result for validating for the "notifier" mode.
    pub notifier: Result<Warnings>,
    /// Result for validating for the "updater" mode.
    ///
    /// *NB* Unimplemented.
    pub updater: Result<Warnings>,
}

/// Warnings is the non-fatal warnings produced by the validator.
pub struct Warnings {
    mode: String,
    out: String,
}

impl Display for Warnings {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        writeln!(f, "warnings ({} mode):", self.mode)?;
        for w in self.out.lines() {
            writeln!(f, "\t{w}")?;
        }
        Ok(())
    }
}

impl Debug for Warnings {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let mut maxlen = 256;
        let trail = if self.out.len() > maxlen {
            maxlen -= 3;
            "..."
        } else {
            ""
        };
        write!(
            f,
            "warnings({}): {out:.*}{trail}",
            self.mode,
            maxlen,
            out = self.out,
        )
    }
}

/// Validate_config wraps a call to [config.Validate].
///
/// This function should be considered blocking, so use any runtime functions as needed.
///
/// [config.Validate]: https://pkg.go.dev/github.com/quay/clair/config#Validate
fn validate_config<S: AsRef<str>>(buf: &[u8], mode: S) -> Result<Warnings> {
    // Allocate a spot to hold the returning string data.
    let mut out: *mut ffi::c_char = ptr::null_mut();
    // Make the slice that go expects.
    let buf = sys::GoSlice {
        data: buf.as_ptr() as *mut ffi::c_void,
        cap: buf.len() as i64,
        len: buf.len() as i64,
    };
    // Make the string that go expects.
    let mode = mode.as_ref().to_string();
    let m = sys::GoString {
        p: mode.as_ptr() as *const i8,
        n: mode.len() as isize,
    };
    // This is a large-ish unsafe block, but the Validate, from_ptr, and free are all unsafe.
    let res: Result<String, String> = unsafe {
        let exit = sys::Validate(buf, &mut out, m);
        let res = match exit {
            0 => Ok(CStr::from_ptr(out)
                .to_str()
                .expect("programmer error: invalid UTF8 from go side")
                .to_string()),
            _ => Err(format!(
                "{} (exit code {exit})",
                CStr::from_ptr(out).to_string_lossy()
            )),
        };
        free(out as *mut ffi::c_void);
        res
    };
    res.map_err(Error::validation)
        .map(|out| Warnings { mode, out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_config_indexer() -> Result<()> {
        let buf: Vec<u8> = Vec::from("{}");
        let ws = validate_config(&buf, "indexer")?;
        eprintln!("{ws}");
        Ok(())
    }
    #[test]
    fn go_config_matcher() -> Result<()> {
        let buf: Vec<u8> = Vec::from(r#"{"matcher":{"indexer_addr":"indexer"}}"#);
        let ws = validate_config(&buf, "matcher")?;
        eprintln!("{ws}");
        Ok(())
    }
    #[test]
    fn go_config_notifier() -> Result<()> {
        let buf: Vec<u8> =
            Vec::from(r#"{"notifier":{"indexer_addr":"indexer","matcher_addr":"matcher"}}"#);
        let ws = validate_config(&buf, "notifier")?;
        eprintln!("{ws}");
        Ok(())
    }
    // TODO(hank) This test will need to be updated when the config go module is updated.
    #[test]
    fn go_config_updater() -> Result<()> {
        let buf: Vec<u8> = Vec::from("{}");
        if validate_config(&buf, "updater").is_ok() {
            Err(Error::test("expected error"))
        } else {
            Ok(())
        }
    }
}
