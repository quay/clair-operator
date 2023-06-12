use std::{borrow::Cow, env};

use anyhow::anyhow;
use api::v1alpha1;
// TODO(hank) Use std::sync::LazyLock once it stabilizes.
use chrono::Utc;
use k8s_openapi::{api::core, apimachinery::pkg::apis::meta};
use kube::runtime::events;
use lazy_static::lazy_static;

// Re-exports for everyone's easy use.
pub(crate) mod prelude {
    pub use api::v1alpha1;
    pub use chrono::Utc;
    pub use futures::prelude::*;
    pub use k8s_openapi::{
        api::*,
        apimachinery::pkg::apis::meta::{self, v1::Condition},
    };
    pub use kube::{
        self,
        api::{Api, PatchParams, PostParams},
        runtime::{
            controller::{Action, Controller},
            events::{Event, EventType, Recorder, Reporter},
            watcher,
        },
        Resource, ResourceExt,
    };
    pub use tokio_util::sync::CancellationToken;
    pub use tracing::{debug, error, info, instrument, trace, warn};

    pub use super::templates;
    pub use super::OPERATOR_NAME;
    pub use super::{Context, Error, Request, Result};
}

pub mod clairs;
pub mod indexers;
pub mod templates;
pub mod updaters;
pub mod webhook;

// NB The docs are unclear, but backtraces are unsupported on stable.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("tracing_subscriber error: {0}")]
    TracingConfig(#[from] tracing_subscriber::filter::ParseError),
    #[error("tracing error: {0}")]
    Tracing(#[from] tracing::subscriber::SetGlobalDefaultError),
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
    #[error("kubeconfig error: {0}")]
    KubeConfig(#[from] kube::config::InferConfigError),
    #[error("kube error: {0}")]
    KubeGV(#[from] kube::core::gvk::ParseGroupVersionError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tokio error: {0}")]
    Tokio(#[from] tokio::task::JoinError),
    #[error("missing name for kubernetes object: {0}")]
    MissingName(&'static str),
    #[error("bad name for kubernetes object: {0}")]
    BadName(String),
    #[error("some other error: {0}")]
    Other(#[from] anyhow::Error),
    #[error("json error: {0}")]
    JSON(#[from] serde_json::Error),
    #[error("yaml error: {0}")]
    YAML(#[from] serde_yaml::Error),
    #[error("json patch error: {0}")]
    JSONPatch(#[from] json_patch::PatchError),
    #[error("parse error: {0}")]
    AddrParse(#[from] std::net::AddrParseError),
    #[error("assets error: {0}")]
    Assets(String),
    #[error("tls error: {0}")]
    TLS(#[from] tokio_native_tls::native_tls::Error),
    #[error("clair config error: {0}")]
    Config(#[from] clair_config::Error),
}

/// Result typedef for the controller.
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub struct Context {
    pub client: kube::Client,
    pub image: String,
}
impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ctx")
    }
}

pub struct Request {
    now: meta::v1::Time,
    recorder: events::Recorder,
}

lazy_static! {
    static ref REPORTER: events::Reporter = {
        events::Reporter {
            controller: OPERATOR_NAME.to_string(),
            instance: env::var("CONTROLLER_POD_NAME").ok(),
        }
    };
}

impl Request {
    pub fn new(c: &kube::Client, oref: core::v1::ObjectReference) -> Request {
        Request {
            now: meta::v1::Time(Utc::now()),
            recorder: events::Recorder::new(c.clone(), REPORTER.clone(), oref),
        }
    }
    fn now(&self) -> meta::v1::Time {
        self.now.clone()
    }
    pub async fn publish(&self, ev: events::Event) -> Result<()> {
        Ok(self.recorder.publish(ev).await?)
    }
}

pub fn next_config(c: &v1alpha1::Clair) -> Result<v1alpha1::ConfigSource> {
    let mut dropins = c.spec.dropins.clone();
    if let Some(db) = &c.spec.databases {
        dropins.push(v1alpha1::RefConfigOrSecret {
            secret: Some(db.indexer.clone()),
            config_map: None,
        });
        dropins.push(v1alpha1::RefConfigOrSecret {
            secret: Some(db.matcher.clone()),
            config_map: None,
        });
        if let Some(db) = &db.notifier {
            dropins.push(v1alpha1::RefConfigOrSecret {
                secret: Some(db.clone()),
                config_map: None,
            });
        };
    };
    let status = c.status.as_ref().ok_or(anyhow!("no status field"))?;
    let cfgsrc = status.config.as_ref().ok_or(anyhow!("no config field"))?;
    Ok(v1alpha1::ConfigSource {
        root: cfgsrc.root.clone(),
        dropins,
    })
}

// Condition is like keyify, but does not force lower-case.
fn condition<S: ToString, K: AsRef<str>>(space: S, key: K) -> String {
    let mut out = space.to_string();
    key.as_ref()
        .chars()
        .map(|c| match c {
            '_' | ' ' | '\t' | '\n' => '-',
            _ => c,
        })
        .for_each(|c| out.push(c));
    out
}

fn keyify<S: ToString, K: AsRef<str>>(space: S, key: K) -> String {
    let mut out = space.to_string();
    key.as_ref()
        .chars()
        .map(|c| match c {
            '_' | ' ' | '\t' | '\n' => '-',
            _ => c.to_ascii_lowercase(),
        })
        .for_each(|c| out.push(c));
    out
}

pub fn clair_condition<S: AsRef<str>>(s: S) -> String {
    condition("projectclair.io/", s)
}

pub fn clair_label<S: AsRef<str>>(s: S) -> String {
    keyify("projectclair.io/", s)
}

pub fn k8s_label<S: AsRef<str>>(s: S) -> String {
    keyify("app.kubernetes.io/", s)
}

pub fn want_dropins(spec: &v1alpha1::ClairSpec) -> Vec<v1alpha1::RefConfigOrSecret> {
    let mut want = spec.dropins.clone();
    if let Some(dbs) = &spec.databases {
        for &sec in &[&dbs.indexer, &dbs.matcher] {
            want.push(v1alpha1::RefConfigOrSecret {
                config_map: None,
                secret: Some(sec.clone()),
            });
        }
        if let Some(ref sec) = dbs.notifier {
            want.push(v1alpha1::RefConfigOrSecret {
                config_map: None,
                secret: Some(sec.clone()),
            });
        };
    }
    want.dedup();
    want
}

#[cfg(debug_assertions)]
const DEFAULT_CONTAINER_TAG: &str = "nightly";
#[cfg(not(debug_assertions))]
const DEFAULT_CONTAINER_TAG: &str = "4.6.1";
const DEFAULT_CONTAINER_REPOSITORY: &str = "quay.io/projectquay/clair";
lazy_static! {
    /// DEFAULT_IMAGE is the container image to use for Clair deployments if not specified in a
    /// CRD.
    ///
    /// The repository and tag components can be changed by providing the environment variables
    /// `CONTAINER_REPOSITORY` or `CONTAINER_TAG` respectively at compile-time.
    pub static ref DEFAULT_IMAGE: String = format!(
        "{}:{}",
        option_env!("CONTAINER_REPOSITORY").unwrap_or(DEFAULT_CONTAINER_REPOSITORY),
        option_env!("CONTAINER_TAG").unwrap_or(DEFAULT_CONTAINER_TAG),
    );
}

lazy_static! {
    /// DEFAULT_CONFIG_JSON is the JSON version of the default config.
    pub static ref DEFAULT_CONFIG_JSON: Cow<'static, [u8]> = {
        let v: serde_json::Value  = serde_yaml::from_slice(&(templates::base::DEFAULT_CONFIG_YAML.get_bytes)()).unwrap();
        serde_json::to_vec(&v).unwrap().into()
    };
    /// DEFAULT_CONFIG_YAML is the YAML version of the default config.
    pub static ref DEFAULT_CONFIG_YAML: Cow<'static, [u8]> = {
        // Doesn't really need to be a lazy_static, but keeps it consistent with the json.
        (templates::base::DEFAULT_CONFIG_YAML.get_bytes)()
    };

    pub static ref COMPONENT_LABEL: String = k8s_label("component");

    pub static ref APP_NAME_LABEL: String = k8s_label("clair");

    pub static ref DEFAULT_CERT_DIR: std::path::PathBuf = std::env::temp_dir().join("k8s-webhook-server/serving-certs");
}

/// OPERATOR_NAME is the name the controller uses whenever it needs a human-readable name.
pub const OPERATOR_NAME: &str = concat!("clair-operator-", env!("CARGO_PKG_VERSION"));
