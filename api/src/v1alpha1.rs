use k8s_openapi::{api::core, apimachinery::pkg::apis::meta};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

pub use crate::RefConfigOrSecret;

/// ClairSpec describes the desired state of a Clair instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema,
)]
#[kube(
    group = "projectclair.io",
    version = "v1alpha1",
    kind = "Clair",
    namespaced,
    status = "ClairStatus",
    derive = "PartialEq",
    shortname = "clair",
    category = "apps"
)]
#[serde(rename_all = "camelCase")]
pub struct ClairSpec {
    /// Databases indicates the Secret keys holding config drop-ins that services should connect
    /// to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub databases: Option<Databases>,
    /// Endpoint indicates how the Ingress should be created.
    ///
    /// If unspecified, the resulting endpoint will need to be read out of the status subresource.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<Endpoint>,
    /// Notifier enables the notifier subsystem.
    ///
    /// The operator does not start the notifier by default. If it's configured via a drop-in, this
    /// field should be set to start it.
    // TODO(hank) Perhaps the operator should just have a custom go tool to render a config out to
    // JSON and examine the resulting config as part of the reconcile loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifier: Option<bool>,
    /// Dropins references additional config drop-in files.
    ///
    /// See the Clair documentation for how config drop-ins are handled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropins: Vec<RefConfigOrSecret>,
    /// ConfigDialect specifies the format to generate for the main config.
    ///
    /// This setting affects what format config drop-ins must be in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dialect: Option<ConfigDialect>,
}

impl std::fmt::Display for Clair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "Clair({})",
            self.metadata.uid.as_deref().unwrap_or("<>")
        ))
    }
}

/// Databases specifies the config drop-ins for the various databases needed.
///
/// It's fine for all the fields to point to the same Secret key if it contains all the relevant
/// configuration.
// The generated openAPI schema for these SecretKeySelectors are patched to remove the nullability
// of the "name" member.
#[derive(Clone, Default, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Databases {
    /// Indexer references the Secret key holding database details for the indexer database.
    pub indexer: core::v1::SecretKeySelector,
    /// Matcher references the Secret key holding database details for the matcher database.
    pub matcher: core::v1::SecretKeySelector,
    /// Notifier references the Secret key holding database details for the notifier database.
    ///
    /// This is only required if the ClairSpec's "notifier" field is set to "true".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifier: Option<core::v1::SecretKeySelector>,
}

#[derive(Clone, Default, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<core::v1::LocalObjectReference>,
}

/// ClairStatus describes the observed state of a Clair instance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClairStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,

    // Misc other refs we may need to hold onto, like Ingresses, Deployments, etc.
    /// Refs holds on to references to objects needed by this instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<core::v1::TypedLocalObjectReference>,

    /// Endpoint is a reference to whatever object is providing ingress.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<core::v1::TypedLocalObjectReference>,

    // These are services, to be used by whatever's fronting.
    /// Indexer is the Service for the Indexer component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexer: Option<core::v1::TypedLocalObjectReference>,
    /// Matcher is the Service for the Matcher component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<core::v1::TypedLocalObjectReference>,
    /// Notifier is the Service for the Notifier component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifier: Option<core::v1::TypedLocalObjectReference>,

    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
    /*
    /// Database is the Service for the managed database engine, if used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<core::v1::TypedLocalObjectReference>,
    */
}

/// ConfigSource specifies all the config files that will be arranged for Clair to load.
///
/// All referenced configs need to be in the same dialect as specified on the parent ClairSpec to
/// load properly.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSource {
    /// Root is a reference to the main config.
    pub root: core::v1::ConfigMapKeySelector,
    /// Dropins is a list of references to drop-in configs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropins: Vec<RefConfigOrSecret>,
}

/// ConfigDialect selects between the dialects for a Clair config.
///
/// The default for the operator to create is JSON.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConfigDialect {
    #[default]
    JSON,
    YAML,
}

/// IndexerSpec describes the desired state of an Indexer instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema,
)]
#[kube(
    group = "projectclair.io",
    version = "v1alpha1",
    kind = "Indexer",
    namespaced,
    status = "IndexerStatus",
    shortname = "indexer",
    derive = "PartialEq",
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct IndexerSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}

/// IndexerStatus describes the observed state of a Indexer instance.
#[derive(Clone, Debug, Deserialize, Default, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexerStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,
    // Misc other refs we may need to hold onto, like Ingresses, Deployments, etc.
    /// Refs holds on to references to objects needed by this instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<core::v1::TypedLocalObjectReference>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}

/// MatcherSpec describes the desired state of an Matcher instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema,
)]
#[kube(
    group = "projectclair.io",
    version = "v1alpha1",
    kind = "Matcher",
    namespaced,
    status = "MatcherStatus",
    shortname = "matcher",
    derive = "PartialEq",
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct MatcherSpec {
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}
/// MatcherStatus describes the observed state of a Matcher instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatcherStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,
    // Misc other refs we may need to hold onto, like Ingresses, Deployments, etc.
    /// Refs holds on to references to objects needed by this instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<core::v1::TypedLocalObjectReference>,
}

/// UpdaterSpec describes the desired state of an Updater instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema,
)]
#[kube(
    group = "projectclair.io",
    version = "v1alpha1",
    kind = "Updater",
    namespaced,
    status = "UpdaterStatus",
    shortname = "updater",
    derive = "PartialEq",
    derive = "Default",
    printcolumn = r#"{"name":"Suspended","type":"boolean","jsonPath":".spec.suspend"}"#,
    printcolumn = r#"{"name":"Last Success","type":"date","format":"date-time","jsonPath":".status.cronJob.status.last_successful_time"}"#,
    printcolumn = r#"{"name":"Last Schedule","type":"date","format":"date-time","jsonPath":".status.cronJob.status.last_schedule_time"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterSpec {
    /// Update schedule in Cron format, see <https://en.wikipedia.org/wiki/Cron>.
    ///
    /// If not provided, a sensible default will be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Suspend subsequent runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspend: Option<bool>,

    pub image: Option<String>,
}

/// UpdaterStatus describes the observed state of a Updater instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,
    // Misc other refs we may need to hold onto, like Ingresses, Deployments, etc.
    /// Refs holds on to references to objects needed by this instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<core::v1::TypedLocalObjectReference>,
    /// CronJob the operator has configured for this Updater.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron_job: Option<core::v1::TypedLocalObjectReference>,
}

/// NotifierSpec describes the desired state of an Notifier instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema,
)]
#[kube(
    group = "projectclair.io",
    version = "v1alpha1",
    kind = "Notifier",
    namespaced,
    status = "NotifierStatus",
    shortname = "notifier",
    derive = "PartialEq",
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct NotifierSpec {
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}
/// NotifierStatus describes the observed state of a Notifier instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotifierStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,
    // Misc other refs we may need to hold onto, like Ingresses, Deployments, etc.
    /// Refs holds on to references to objects needed by this instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<core::v1::TypedLocalObjectReference>,
}
