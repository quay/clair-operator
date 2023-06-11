// Use block for this module
use anyhow::anyhow;
use api::v1alpha1;
use lazy_static::lazy_static;

// Re-exports for everyone's easy use.
pub(crate) mod prelude {
    pub use api::v1alpha1;
    pub use chrono::Utc;
    pub use futures::prelude::*;
    pub use k8s_openapi::{api::*, apimachinery::pkg::apis::meta};
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

    pub use super::OPERATOR_NAME;
    pub use super::{Context, Error, Result};
}

pub mod clairs;
pub mod config;
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
}

/// Result typedef for the controller.
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub struct Context {
    pub client: kube::Client,
    pub assets: templates::Assets,
    pub image: String,
}
impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ctx")
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

    want
}

lazy_static! {
    /// OPERATOR_NAME is the name the controller uses whenever it needs a human-readable name.
    pub static ref OPERATOR_NAME: String = format!("clair-operator-{}", env!("CARGO_PKG_VERSION"));

    /// DEFAULT_CONFIG_JSON is the JSON version of the default config.
    pub static ref DEFAULT_CONFIG_JSON: String = {
            let v = serde_yaml::from_str::<serde_json::Value>(DEFAULT_CONFIG_YAML).unwrap();
            serde_json::to_string(&v).unwrap()
    };

    pub static ref COMPONENT_LABEL: String = k8s_label("component");

    pub static ref APP_NAME_LABEL: String = k8s_label("clair");

    pub static ref DEFAULT_IMAGE: String = format!("quay.io/projectquay/clair:{}", env!("DEFAULT_CLAIR_TAG"));

    pub static ref DEFAULT_CERT_DIR: std::path::PathBuf = std::env::temp_dir().join("k8s-webhook-server/serving-certs");
}

/// DEFAULT_CONFIG_YAML is the YAML version of the default config.
pub const DEFAULT_CONFIG_YAML: &str = include_str!("../etc/default_config.yaml");
