//! Clair_config is a module for validating a configuration with the
//! [`github.com/quay/clair/config`] module.
//!
//! Building this crate reqires `clang` and `go` toolchains installed.
//!
//! [`github.com/quay/clair/config`]: https://pkg.go.dev/github.com/quay/clair/config
#![warn(rustdoc::missing_crate_level_docs)]
#![warn(missing_docs)]

use k8s_openapi::api::core;
use kube::{api::Api, Client};
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
    /// Generic k8s access error.
    Kube(kube::Error),

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
impl From<kube::Error> for Error {
    fn from(err: kube::Error) -> Self {
        Self::Kube(err)
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
            Error::Kube(err) => write!(f, "k8s client error: {err}"),
            #[cfg(test)]
            Error::Test(msg) => write!(f, "testing error: {msg}"),
        }
    }
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Validate calls into [`config.Validate`] and reports the lints for every mode.
/// This is done by composing the config in-process accoring to the [`cmd.Load`] documentation.
/// The changes for defaults made by the `Validate` function are not returned, so that the config
/// package can change the defaults as needed.
///
/// [`config.Validate`]: https://pkg.go.dev/github.com/quay/clair/config#Validate
/// [`cmd.Load`]: https://pkg.go.dev/github.com/quay/clair/v4/cmd#Load
pub async fn validate(client: &Client, cfg: &v1alpha1::ConfigSource) -> Result<Validate> {
    let flavor = match &cfg.root.key.rsplit_once('.') {
        Some((_, ext)) => match ext {
            &"json" => v1alpha1::ConfigDialect::JSON,
            &"yaml" => v1alpha1::ConfigDialect::YAML,
            ext => return Err(Error::invalid(format!("unknown file extension: {ext}"))),
        },
        None => return Err(Error::invalid("missing file extension")),
    };
    debug!(?flavor, "config flavor from root name");
    let (doc, _) = load_config(client, Source::ConfigMap(&cfg.root)).await?;
    let doc = to_json(doc, &flavor).await?;
    let mut doc: serde_json::Value = serde_json::from_slice(&doc)?;

    assert!(cfg
        .dropins
        .iter()
        .all(|c| c.config_map.is_some() || c.secret.is_some()));

    let mut order = cfg
        .dropins
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let key = if let Some(cmref) = &v.config_map.as_ref() {
                &cmref.key
            } else if let Some(secref) = &v.secret.as_ref() {
                &secref.key
            } else {
                unreachable!()
            };
            (i, key)
        })
        .collect::<Vec<_>>();
    order.sort_by_key(|(_, k)| k.as_str());
    for (i, _) in order {
        let r = &cfg.dropins[i];
        let src = if let Some(cmref) = &r.config_map {
            Source::ConfigMap(cmref)
        } else if let Some(secref) = &r.secret {
            Source::Secret(secref)
        } else {
            unreachable!()
        };
        let (buf, is_patch) = load_config(client, src).await?;
        let buf = to_json(buf, &flavor).await?;
        if is_patch {
            let p: json_patch::Patch = serde_json::from_slice(&buf)?;
            json_patch::patch(&mut doc, &p)?;
        } else {
            let m: serde_json::Value = serde_json::from_slice(&buf)?;
            json_patch::merge(&mut doc, &m);
        }
    }

    let doc = serde_json::to_vec(&doc)?;
    Ok(Validate {
        indexer: validate_config(&doc, "indexer").await,
        matcher: validate_config(&doc, "matcher").await,
        updater: validate_config(&doc, "updater").await,
        notifier: validate_config(&doc, "notifier").await,
    })
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
async fn to_json(buf: Vec<u8>, flavor: &v1alpha1::ConfigDialect) -> Result<Vec<u8>> {
    match flavor {
        v1alpha1::ConfigDialect::JSON => Ok(buf),
        v1alpha1::ConfigDialect::YAML => {
            let v = serde_yaml::from_slice::<serde_json::Value>(&buf)?;
            Ok(serde_json::to_vec(&v)?)
        }
    }
}

/// Source is a discriminator between a ConfigMap and a Secret.
enum Source<'a> {
    ConfigMap(&'a core::v1::ConfigMapKeySelector),
    Secret(&'a core::v1::SecretKeySelector),
}

/// Load_config returns the bytes of the referenced config and whether it should be interpreted as
/// a patch or not.
async fn load_config(client: &Client, config_ref: Source<'_>) -> Result<(Vec<u8>, bool)> {
    trace!("loading config");
    let maps = match config_ref {
        Source::ConfigMap(r) => {
            trace!(name = r.name, key = r.key, "checking ConfigMap");
            let api: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
            let name = r
                .name
                .as_ref()
                .ok_or_else(|| Error::invalid("ConfigMapKeySelector missing \"name\""))?;
            let cm = api.get(name).await?;
            (cm.data, cm.binary_data)
        }
        Source::Secret(r) => {
            trace!(name = r.name, key = r.key, "checking Secret");
            let api: Api<core::v1::Secret> = Api::default_namespaced(client.clone());
            let name = r
                .name
                .as_ref()
                .ok_or_else(|| Error::invalid("SecretKeySelector missing \"name\""))?;
            let s = api.get(name).await?;
            (None, s.data)
        }
    };
    let key = match config_ref {
        Source::ConfigMap(r) => &r.key,
        Source::Secret(r) => &r.key,
    };
    let is_patch = key.ends_with("-patch");
    trace!(is_patch, strdata=?maps.0, bindata=?maps.1, "checking keys");
    if let Some(map) = maps.0 {
        if let Some(v) = map.get(key) {
            return Ok((Vec::from(v.as_str()), is_patch));
        }
    }
    if let Some(map) = maps.1 {
        if let Some(v) = map.get(key) {
            return Ok((v.0.clone(), is_patch));
        }
    }

    Err(Error::invalid("missing key"))
}

// Validate_config wraps a call to [config.Validate].
//
// [config.Validate]: https://pkg.go.dev/github.com/quay/clair/config#Validate
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
