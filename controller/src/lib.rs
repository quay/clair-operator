// Use block for this module
use anyhow::anyhow;
use lazy_static::lazy_static;
use serde_json;
use serde_yaml;

// Re-exports for everyone's easy use.
pub(crate) use api::v1alpha1;
pub(crate) use k8s_openapi::{api::core, apimachinery::pkg::apis::meta};
pub(crate) use kube;
pub(crate) use tracing::{debug, error, info};

pub mod clairs;
pub mod config;
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
}

/// Result typedef for the controller.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Get_rev_annotation reports the revision annotation used throughout the controller.
pub fn get_rev_annotation(metadata: &meta::v1::ObjectMeta) -> Option<&str> {
    let annotations = metadata.annotations.as_ref()?;
    annotations.get(REV_ANNOTATION.as_str()).map(String::as_str)
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
pub fn condition<S: AsRef<str>>(s: S) -> String {
    let mut k = String::from("clair.projectquay.io/");
    s.as_ref()
        .chars()
        .map(|c| match c {
            '_' | ' ' | '\t' | '\n' => '-',
            _ => c,
        })
        .for_each(|c| k.push(c));
    k
}

fn keyify<S: AsRef<str>>(s: S) -> String {
    let mut k = String::from("clair.projectquay.io/");
    s.as_ref()
        .chars()
        .map(|c| match c {
            '_' | ' ' | '\t' | '\n' => '-',
            _ => c.to_ascii_lowercase(),
        })
        .for_each(|c| k.push(c));
    k
}

lazy_static! {
    static ref REV_ANNOTATION: String = keyify("TODO-real-name");

    /// OPERATOR_NAME is the name the controller uses whenever it needs a human-readable name.
    pub static ref OPERATOR_NAME: String = keyify(format!("operator-{}", env!("CARGO_PKG_VERSION")));

    /// DEFAULT_CONFIG_JSON is the JSON version of the default config.
    pub static ref DEFAULT_CONFIG_JSON: String = (|| {
            let v = serde_yaml::from_str::<serde_json::Value>(DEFAULT_CONFIG_YAML).unwrap();
            serde_json::to_string(&v).unwrap()
    })();
}

/// DEFAULT_CONFIG_YAML is the YAML version of the default config.
pub const DEFAULT_CONFIG_YAML: &'static str = include_str!("../etc/default_config.yaml");
