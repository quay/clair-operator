//! Clair_config is a module for validating a configuration with the
//! [`github.com/quay/clair/config`] module.
//!
//! Building this crate reqires `clang` and `go` toolchains installed.
//!
//! [`github.com/quay/clair/config`]: https://pkg.go.dev/github.com/quay/clair/config
#![warn(rustdoc::missing_crate_level_docs)]
#![warn(missing_docs)]

use std::collections::BTreeMap;

use k8s_openapi::api::core;
use tracing::{debug, trace};

use api::v1alpha1;

mod sys;

/// Error enumerates the errors reported by this module.
#[derive(Debug)]
pub enum Error {
    /// Configuration is invalid for some reason.
    Invalid(String),
    /// Validation failed for some reason.
    Validation(String),

    /// YAML deserialization error.
    YAML(serde_yaml::Error),
    /// JSON serialiization or deserialization error.
    JSON(serde_json::Error),
    /// JSON Patch error
    Patch(json_patch::PatchError),

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

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::JSON(err)
    }
}
impl From<serde_yaml::Error> for Error {
    fn from(err: serde_yaml::Error) -> Self {
        Self::YAML(err)
    }
}
impl From<json_patch::PatchError> for Error {
    fn from(err: json_patch::PatchError) -> Self {
        Self::Patch(err)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Invalid(msg) => write!(f, "invalid ConfigSource: {msg}"),
            Error::Validation(msg) => write!(f, "validation failure: {msg}"),
            Error::YAML(err) => write!(f, "YAML error: {err}"),
            Error::JSON(err) => write!(f, "JSON error: {err}"),
            Error::Patch(err) => write!(f, "json patch error: {err}"),
            #[cfg(test)]
            Error::Test(msg) => write!(f, "testing error: {msg}"),
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
    pub async fn validate(&self) -> Result<Validate> {
        let doc = serde_json::from_slice(&self.root)?;
        let doc = self
            .dropins
            .iter()
            .fold(doc, |mut doc, (name, (buf, patch))| {
                if *patch {
                    let p: json_patch::Patch =
                        serde_json::from_slice(buf).expect("failed to load patch");
                    trace!(name, "applying patch");
                    json_patch::patch(&mut doc, &p).expect("failed to apply patch");
                } else {
                    let m: serde_json::Value =
                        serde_json::from_slice(buf).expect("failed to parse JSON");
                    trace!(name, "merging config");
                    json_patch::merge(&mut doc, &m);
                };
                doc
            });
        trace!("config rendered");
        let doc = serde_json::to_vec(&doc)?;
        Ok(Validate {
            indexer: validate_config(&doc, "indexer").await,
            matcher: validate_config(&doc, "matcher").await,
            notifier: validate_config(&doc, "notifier").await,

            updater: validate_config(&doc, "updater").await,
        })
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
    flavor: v1alpha1::ConfigDialect,

    root: Vec<u8>,
    dropins: BTreeMap<String, (Vec<u8>, bool)>,
}

impl Builder {
    /// From_root constructs a Builder starting with the root config.
    pub fn from_root<S: ToString>(v: &core::v1::ConfigMap, key: S) -> Result<Self> {
        let key = key.to_string();
        let root = if let Some(map) = &v.data {
            map.get(&key).map(|s| s.clone().into_bytes())
        } else if let Some(map) = &v.binary_data {
            map.get(&key).map(|b| b.0.clone())
        } else {
            unreachable!()
        }
        .ok_or_else(|| Error::invalid(format!("missing key: {key}")))?;
        trace!(key, "loaded key");
        let flavor = match key.rsplit_once('.') {
            Some((_, ext)) => match ext {
                "json" => v1alpha1::ConfigDialect::JSON,
                "yaml" => v1alpha1::ConfigDialect::YAML,
                ext => return Err(Error::invalid(format!("unknown file extension: {ext}"))),
            },
            None => return Err(Error::invalid("missing file extension")),
        };
        trace!(%flavor, "guessed config flavor");
        let root = to_json(root, &flavor)?;
        trace!(key, "converted to JSON");
        debug!("created Builder");
        Ok(Builder {
            flavor,
            root,
            dropins: Default::default(),
        })
    }

    /// Add adds a dropin, converting to JSON if needed.
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
        let buf = to_json(buf, &self.flavor)?;
        trace!(key, is_patch, "converted to JSON");
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

impl Sealed for core::v1::ConfigMap {}
impl K8sMap for core::v1::ConfigMap {
    fn value(&self, key: String) -> Option<Vec<u8>> {
        if let Some(data) = &self.data {
            if let Some(buf) = data.get(&key) {
                return Some(buf.clone().into_bytes());
            };
        };
        if let Some(data) = &self.binary_data {
            if let Some(buf) = data.get(&key) {
                return Some(buf.0.clone());
            };
        };
        None
    }
}

impl Sealed for core::v1::Secret {}
impl K8sMap for core::v1::Secret {
    fn value(&self, key: String) -> Option<Vec<u8>> {
        if let Some(data) = &self.data {
            if let Some(buf) = data.get(&key) {
                return Some(buf.0.clone());
            };
        };
        None
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

impl std::fmt::Display for Warnings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "warnings ({} mode):", self.mode)?;
        for w in self.out.lines() {
            writeln!(f, "\t{w}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Warnings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/// To_json returns the bytes jsonified.
fn to_json(buf: Vec<u8>, flavor: &v1alpha1::ConfigDialect) -> Result<Vec<u8>> {
    match flavor {
        v1alpha1::ConfigDialect::JSON => Ok(buf),
        v1alpha1::ConfigDialect::YAML => {
            let v = serde_yaml::from_slice::<serde_json::Value>(&buf)?;
            Ok(serde_json::to_vec(&v)?)
        }
    }
}

/// Validate_config wraps a call to [config.Validate].
///
/// The use of `block_in_place` here means we have a depenedency on tokio, but should make the ffi
/// play nicer with the multi-threaded runtime (in theory).
///
/// [config.Validate]: https://pkg.go.dev/github.com/quay/clair/config#Validate
async fn validate_config<S: AsRef<str>>(buf: &[u8], mode: S) -> Result<Warnings> {
    use libc::free;
    use std::ffi::{self, CStr};
    use tokio::task;
    // Allocate a spot to hold the returning string data.
    let mut out: *mut ffi::c_char = std::ptr::null_mut();
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
    let res: Result<String, String> = task::block_in_place(|| unsafe {
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
    });
    res.map_err(Error::validation)
        .map(|out| Warnings { mode, out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_indexer() -> Result<()> {
        let buf: Vec<u8> = Vec::from("{}");
        let ws = validate_config(&buf, "indexer").await?;
        eprintln!("{ws}");
        Ok(())
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_matcher() -> Result<()> {
        let buf: Vec<u8> = Vec::from(r#"{"matcher":{"indexer_addr":"indexer"}}"#);
        let ws = validate_config(&buf, "matcher").await?;
        eprintln!("{ws}");
        Ok(())
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_notifier() -> Result<()> {
        let buf: Vec<u8> =
            Vec::from(r#"{"notifier":{"indexer_addr":"indexer","matcher_addr":"matcher"}}"#);
        let ws = validate_config(&buf, "notifier").await?;
        eprintln!("{ws}");
        Ok(())
    }

    // TODO(hank) This test will need to be updated when the config go module is updated.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_updater() -> Result<()> {
        let buf: Vec<u8> = Vec::from("{}");
        if validate_config(&buf, "updater").await.is_ok() {
            Err(Error::test("expected error"))
        } else {
            Ok(())
        }
    }
}
