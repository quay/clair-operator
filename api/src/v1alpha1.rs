use k8s_openapi::{api::core, apimachinery::pkg::apis::meta};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

use super::*;

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
    shortname = "cl",
    category = "apps"
)]
#[serde(rename_all = "camelCase")]
pub struct ClairSpec {
    /// Databases indicates the Secret keys holding config drop-ins that services should connect
    /// to.
    ///
    /// If not provided, a database engine will be started and used on the default storage class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub databases: Option<Databases>,
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

/// Databases specifies the config drop-ins for the various databases needed.
///
/// It's fine for all the fields to point to the same Secret key if it contains all the relevant
/// configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
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
    /// Database is the Service for the managed database engine, if used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<core::v1::TypedLocalObjectReference>,
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
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
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
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
pub struct IndexerSpec {
    pub info: String,
}

/// IndexerStatus describes the observed state of a Indexer instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexerStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,
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
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
pub struct MatcherSpec {
    pub info: String,
}
/// MatcherStatus describes the observed state of a Matcher instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatcherStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,
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
    derive = "PartialEq",
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
}

/// UpdaterStatus describes the observed state of a Updater instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,

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
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
pub struct NotifierSpec {
    pub info: String,
}
/// NotifierStatus describes the observed state of a Notifier instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotifierStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    //#[schemars(schema_with = "conditions")]
    pub conditions: Vec<meta::v1::Condition>,
}
