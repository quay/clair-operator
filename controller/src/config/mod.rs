use std::path::Path;

use anyhow::anyhow;
use json_patch;
pub use k8s_openapi::{api::*, apimachinery::pkg::apis::meta};
use kube::{api::Api, Client};
use serde_json;
use serde_yaml;

use crate::*;
mod sys;

// TODO(hank) This crate should be split out for build-time reasons. Invoking the go toolchain and
// then needing to link against the result is slow.

/// Validate calls into [`config.Validate`] and reports the lints for every mode.
/// This is done by composing the config in-process accoring to the [`cmd.Load`] documentation.
/// The changes for defaults made by the `Validate` function are not returned, so that the config
/// package can change the defaults as needed.
///
/// [`config.Validate`]: https://pkg.go.dev/github.com/quay/clair/config#Validate
/// [`cmd.Load`]: https://pkg.go.dev/github.com/quay/clair/v4/cmd#Load
pub async fn validate(client: Client, cfg: &v1alpha1::ConfigSource) -> Result<Validate> {
    let flavor = match Path::new(&cfg.root.key).extension() {
        Some(ext) => match ext.to_str() {
            Some("json") => v1alpha1::ConfigDialect::JSON,
            Some("yaml") => v1alpha1::ConfigDialect::YAML,
            Some(ext) => return Err(Error::from(anyhow!("unknown file extension: {ext}"))),
            None => return Err(Error::from(anyhow!("not valid UTF-8"))),
        },
        None => return Err(Error::from(anyhow!("missing file extension"))),
    };
    let (doc, _) = load_config(&client, Source::ConfigMap(&cfg.root)).await?;
    let doc = to_json(doc, &flavor).await?;
    let mut doc: serde_json::Value = serde_json::from_slice(&doc)?;

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
                panic!("");
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
            return Err(Error::Other(anyhow!("???")));
        };
        let (buf, is_patch) = load_config(&client, src).await?;
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
    pub indexer: Result<Vec<String>>,
    pub matcher: Result<Vec<String>>,
    pub updater: Result<Vec<String>>,
    pub notifier: Result<Vec<String>>,
}

pub fn fmt_warnings(v: Vec<String>) -> String {
    let mut s = String::from("warnings:\n");
    for w in v {
        s.push('\t');
        s.push_str(&w);
        s.push('\n');
    }
    s
}

// To_json returns the bytes jsonified.
async fn to_json(buf: Vec<u8>, flavor: &v1alpha1::ConfigDialect) -> Result<Vec<u8>> {
    match flavor {
        v1alpha1::ConfigDialect::JSON => Ok(buf),
        v1alpha1::ConfigDialect::YAML => {
            let v = serde_yaml::from_slice::<serde_json::Value>(&buf)?;
            Ok(serde_json::to_vec(&v)?)
        }
    }
}

// Source is a discriminator between a ConfigMap and a Secret.
enum Source<'a> {
    ConfigMap(&'a core::v1::ConfigMapKeySelector),
    Secret(&'a core::v1::SecretKeySelector),
}

// Load_config returns the bytes of the referenced config and whether it should be interpreted as
// a patch or not.
async fn load_config(client: &Client, config_ref: Source<'_>) -> Result<(Vec<u8>, bool)> {
    let maps = match config_ref {
        Source::ConfigMap(r) => {
            let api: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
            let name = r
                .name
                .as_ref()
                .ok_or(Error::MissingName("ConfigMapKeySelector"))?;
            let cm = api.get(name).await?;
            (cm.data, cm.binary_data)
        }
        Source::Secret(r) => {
            let api: Api<core::v1::Secret> = Api::default_namespaced(client.clone());
            let name = r
                .name
                .as_ref()
                .ok_or(Error::MissingName("SecretKeySelector"))?;
            let cm = api.get(name).await?;
            (cm.string_data, cm.data)
        }
    };
    let key = match config_ref {
        Source::ConfigMap(r) => &r.key,
        Source::Secret(r) => &r.key,
    };
    let is_patch = key.ends_with("-patch");
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

    Err(Error::from(anyhow!("missing key",)))
}

// Validate_config wraps a call to [config.Validate].
//
// [config.Validate]: https://pkg.go.dev/github.com/quay/clair/config#Validate
async fn validate_config<S: AsRef<str>>(buf: &[u8], mode: S) -> Result<Vec<String>> {
    use libc::free;
    use std::ffi::{self, CStr};
    use tokio::task;
    // Allocate a spot to hold the returning string data.
    let mut warnings: *mut ffi::c_char = std::ptr::null_mut();
    // Make the slice that go expects.
    let buf = sys::GoSlice {
        data: buf.as_ptr() as *mut ffi::c_void,
        cap: buf.len() as i64,
        len: buf.len() as i64,
    };
    // Make the string that go expects.
    let mode = mode.as_ref();
    let mode = sys::GoString {
        p: mode.as_ptr() as *const i8,
        n: mode.len() as isize,
    };
    // This is a large-ish unsafe block, but the Validate, from_ptr, and free are all unsafe.
    let res: Result<Vec<String>, anyhow::Error> = task::block_in_place(|| unsafe {
        let exit = sys::Validate(buf, &mut warnings, mode);
        let res = match exit {
            0 => Ok(CStr::from_ptr(warnings)
                .to_string_lossy()
                .split_terminator('\n')
                .map(String::from)
                .collect()),
            _ => Err(anyhow!(
                "{} (exit code {exit})",
                CStr::from_ptr(warnings).to_string_lossy()
            )),
        };
        free(warnings as *mut ffi::c_void);
        res
    });
    res.map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_indexer() -> Result<()> {
        let buf: Vec<u8> = Vec::from("{}");
        let ws = validate_config(&buf, "indexer").await?;
        for w in &ws {
            eprintln!("{}", w);
        }
        Ok(())
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_matcher() -> Result<()> {
        let buf: Vec<u8> = Vec::from(r#"{"matcher":{"indexer_addr":"indexer"}}"#);
        let ws = validate_config(&buf, "matcher").await?;
        for w in &ws {
            eprintln!("{}", w);
        }
        Ok(())
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_notifier() -> Result<()> {
        let buf: Vec<u8> =
            Vec::from(r#"{"notifier":{"indexer_addr":"indexer","matcher_addr":"matcher"}}"#);
        let ws = validate_config(&buf, "notifier").await?;
        for w in &ws {
            eprintln!("{}", w);
        }
        Ok(())
    }

    // TODO(hank) This test will need to be updated when the config go module is update.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn go_config_updater() -> Result<()> {
        let buf: Vec<u8> = Vec::from("{}");
        if validate_config(&buf, "updater").await.is_ok() {
            Err(Error::from(anyhow!("expected error")))
        } else {
            Ok(())
        }
    }
}
