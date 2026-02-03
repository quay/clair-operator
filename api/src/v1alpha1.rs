//! Module `v1alpha1` implements the v1alpha1 Clair CRD API.
use k8s_openapi::{DeepMerge, api::core, apimachinery::pkg::apis::meta, merge_strategies};
use kube::{CustomResource, KubeSchema};
use schemars;
use serde::{Deserialize, Serialize};
use validator::Validate;

/// VERSION is the kubernetes API group's version.
pub static VERSION: &str = "v1alpha1";

/// ClairSpec describes the desired state of a Clair instance.
#[derive(
    KubeSchema, Clone, CustomResource, Debug, Default, Deserialize, PartialEq, Serialize, Validate,
)]
#[kube(
    group = "clairproject.org",
    version = "v1alpha1",
    kind = "Clair",
    namespaced,
    status = "ClairStatus",
    shortname = "clair",
    category = "apps",
    derive = "Default",
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = ("(has(self.notifier) && self.notifier) ? has(self.databases.notifier) : true", r#"notifier database configuration must be provided"#))]
pub struct ClairSpec {
    /// Databases indicates the Secret keys holding config drop-ins that services should connect
    /// to.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(required)]
    pub databases: Option<Databases>,
    /// Container image to use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Gateway indicates how the Gateway should be created.
    ///
    /// If unspecified, services will need to have their routing set up manually.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<RouteParentRef>,
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
    pub dropins: Vec<DropinSource>,
}

impl ClairSpec {
    /// With_root creates the desired ConfigSource, using the provided name as the root ConfigMap.
    pub fn with_root<S: ToString>(&self, name: S) -> ConfigSource {
        let mut dropins = self.dropins.clone();
        if let Some(db) = &self.databases {
            dropins.push(DropinSource {
                secret_key_ref: Some(db.indexer.clone()),
                config_map_key_ref: None,
            });
            dropins.push(DropinSource {
                secret_key_ref: Some(db.matcher.clone()),
                config_map_key_ref: None,
            });
            if let Some(db) = &db.notifier {
                dropins.push(DropinSource {
                    secret_key_ref: Some(db.clone()),
                    config_map_key_ref: None,
                });
            };
        };
        dropins.sort();
        dropins.dedup();
        let name = name.to_string();
        ConfigSource {
            root: ConfigMapKeySelector {
                name,
                key: "config.json".into(),
            },
            dropins,
        }
    }
}

impl DeepMerge for ClairSpec {
    fn merge_from(&mut self, other: Self) {
        self.image.merge_from(other.image);
        self.databases.merge_from(other.databases);
        self.gateway.merge_from(other.gateway);
        self.notifier.merge_from(other.notifier);
        merge_strategies::list::set(self.dropins.as_mut(), other.dropins);
    }
}

/// Databases specifies the config drop-ins for the various databases needed.
///
/// It's fine for all the fields to point to the same Secret key if it contains all the relevant
/// configuration.
// The generated openAPI schema for these SecretKeySelectors are patched to remove the nullability
// of the "name" member.
#[derive(Clone, Default, Debug, Deserialize, PartialEq, Serialize, Validate, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct Databases {
    /// Indexer references the Secret key holding database details for the indexer database.
    pub indexer: SecretKeySelector,
    /// Matcher references the Secret key holding database details for the matcher database.
    pub matcher: SecretKeySelector,
    /// Notifier references the Secret key holding database details for the notifier database.
    ///
    /// This is only required if the ClairSpec's "notifier" field is set to "true".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifier: Option<SecretKeySelector>,
}

impl DeepMerge for Databases {
    fn merge_from(&mut self, other: Self) {
        self.indexer.merge_from(other.indexer);
        self.matcher.merge_from(other.matcher);
        self.notifier.merge_from(other.notifier);
    }
}

/// RouteParentRef serves the same purpose as a Gateway API [ParentReference].
///
/// [ParentReference]: https://gateway-api.sigs.k8s.io/reference/spec/#parentreference
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouteParentRef {
    /// Group is the group of the referent.
    ///
    /// When unspecified, "gateway.networking.k8s.io" is inferred. To set the core API group (such
    /// as for a "Service" kind referent), Group must be explicitly set to "" (empty string).
    ///
    /// Support: Core
    /// MaxLength: 253
    /// Pattern: ^$\|^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(
        length(max = 253),
        regex(
            pattern = r#"^$\|^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$"#
        )
    )]
    pub group: Option<String>,

    /// Kind is kind of the referent.
    ///
    /// There are two kinds of parent resources with "Core" support:
    ///
    /// - Gateway (Gateway conformance profile)
    /// - Service (Mesh conformance profile, ClusterIP Services only)
    ///
    /// Support for other resources is Implementation-Specific.
    ///
    /// MaxLength: 63
    /// MinLength: 1
    /// Pattern: ^[a-zA-Z]([-a-zA-Z0-9]*[a-zA-Z0-9])?$
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(
        length(max = 253, min = 1),
        regex(pattern = r#"^[a-zA-Z]([-a-zA-Z0-9]*[a-zA-Z0-9])?$"#)
    )]
    pub kind: Option<String>,

    /// Namespace is the namespace of the referent.
    ///
    /// When unspecified, this refers to the local namespace of the Route.
    ///
    /// Note that there are specific rules for ParentRefs which cross namespace boundaries.
    /// Cross-namespace references are only valid if they are explicitly allowed by something in
    /// the namespace they are referring to. For example: Gateway has the AllowedRoutes field, and
    /// ReferenceGrant provides a generic way to enable any other kind of cross-namespace
    /// reference.
    ///
    /// ParentRefs from a Route to a Service in the same namespace are "producer" routes, which
    /// apply default routing rules to inbound connections from any namespace to the Service.
    /// ParentRefs from a Route to a Service in a different namespace are "consumer" routes, and
    /// these routing rules are only applied to outbound connections originating from the same
    /// namespace as the Route, for which the intended destination of the connections are a Service
    /// targeted as a ParentRef of the Route.
    ///
    /// Support: Core
    /// MaxLength: 63
    /// MinLength: 1
    /// Pattern: ^[a-z0-9]([-a-z0-9]*[a-z0-9])?$
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(
        length(max = 63, min = 1),
        regex(pattern = r#"^[a-z0-9]([-a-z0-9]*[a-z0-9])?$"#)
    )]
    pub namespace: Option<String>,

    /// Name is the name of the referent.
    ///
    /// Support: Core
    /// MaxLength: 253
    /// MinLength: 1
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 253, min = 1), required)]
    pub name: Option<String>,

    /// SectionName is the name of a section within the target resource.
    ///
    /// In the following resources, SectionName is interpreted as the following:
    ///
    /// - Gateway: Listener name. When both Port (experimental) and SectionName are specified, the
    ///   name and port of the selected listener must match both specified values.
    /// - Service: Port name. When both Port (experimental) and SectionName are specified, the name
    ///   and port of the selected listener must match both specified values.
    ///
    /// Implementations MAY choose to support attaching Routes to other resources. If that is the
    /// case, they MUST clearly document how SectionName is interpreted.
    ///
    /// When unspecified (empty string), this will reference the entire resource. For the purpose
    /// of status, an attachment is considered successful if at least one section in the parent
    /// resource accepts it. For example, Gateway listeners can restrict which Routes can attach to
    /// them by Route kind, namespace, or hostname. If 1 of 2 Gateway listeners accept attachment
    /// from the referencing Route, the Route MUST be considered successfully attached. If no
    /// Gateway listeners accept attachment from this Route, the Route MUST be considered detached
    /// from the Gateway.
    ///
    /// Support: Core
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_name: Option<String>,
}

impl DeepMerge for RouteParentRef {
    fn merge_from(&mut self, other: Self) {
        self.group.merge_from(other.group);
        self.kind.merge_from(other.kind);
        self.namespace.merge_from(other.namespace);
        self.name.merge_from(other.name);
        self.section_name.merge_from(other.section_name);
    }
}

/// ClairStatus describes the observed state of a Clair instance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClairStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "schema::conditions")]
    pub conditions: Option<Vec<meta::v1::Condition>>,

    // Misc other refs we may need to hold onto, like Ingresses, Deployments, etc.
    /// Refs holds on to references to objects needed by this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[x_kube(merge_strategy = ListMerge::Map(vec!["kind".into()]))]
    pub refs: Option<Vec<core::v1::TypedLocalObjectReference>>,

    /// Indexer is the created Indexer component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexer: Option<core::v1::TypedLocalObjectReference>,
    /// Matcher is the created Matcher component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<core::v1::TypedLocalObjectReference>,
    /// Notifier is the created Notifier component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifier: Option<core::v1::TypedLocalObjectReference>,

    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
    /// Image is the image that should be used in the managed deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /*
    /// Previous_version is the previous verison of a deployed Clair instance, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    /// Current_version is the current verison of a deployed Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    /// Database is the Service for the managed database engine, if used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<core::v1::TypedLocalObjectReference>,
    */
}

/// ConfigSource specifies all the config files that will be arranged for Clair to load.
///
/// All referenced configs need to be in the same dialect as specified on the parent ClairSpec to
/// load properly.
#[derive(
    Clone, Debug, Deserialize, PartialEq, PartialOrd, Eq, Ord, Serialize, Validate, KubeSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSource {
    /// Root is a reference to the main config.
    pub root: ConfigMapKeySelector,
    /// Dropins is a list of references to drop-in configs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropins: Vec<DropinSource>,
}

impl DeepMerge for ConfigSource {
    fn merge_from(&mut self, other: Self) {
        self.root.merge_from(other.root);
        merge_strategies::list::set(self.dropins.as_mut(), other.dropins);
    }
}

/// DropinSource represents a source for the value of a Clair configuration dropin.
#[derive(
    KubeSchema,
    Clone,
    Debug,
    Default,
    Deserialize,
    Eq,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Validate,
)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = ("(has(self.configMapKeyRef) && !has(self.secretKeyRef)) || (!has(self.configMapKeyRef) && has(self.secretKeyRef))", r#"exactly one key ref must be provided"#))]
pub struct DropinSource {
    /// Selects a key of a ConfigMap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_map_key_ref: Option<ConfigMapKeySelector>,
    /// Selects a key of a Secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_key_ref: Option<SecretKeySelector>,
}

/// SecretKeySelector selects a key from a Secret.
#[derive(
    Clone,
    Default,
    Debug,
    Deserialize,
    PartialEq,
    PartialOrd,
    Eq,
    Ord,
    Serialize,
    Validate,
    KubeSchema,
)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = ("self.name != '' && self.key != ''", r#""key" and "name" must be populated"#))]
pub struct SecretKeySelector {
    /// The key to select.
    pub key: String,
    /// The name of the referent.
    pub name: String,
}

impl DeepMerge for SecretKeySelector {
    fn merge_from(&mut self, other: Self) {
        if !other.key.is_empty() {
            self.key = other.key.clone();
        }
        if !other.name.is_empty() {
            self.name = other.name.clone();
        }
    }
}

/// ConfigMapKeySelector selects a key from a ConfigMap.
#[derive(
    Clone,
    Default,
    Eq,
    Ord,
    Debug,
    Deserialize,
    PartialEq,
    PartialOrd,
    Serialize,
    Validate,
    KubeSchema,
)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = ("self.name != '' && self.key != ''", r#""key" and "name" must be populated"#))]
pub struct ConfigMapKeySelector {
    /// The key to select.
    pub key: String,
    /// The name of the referent.
    pub name: String,
}

impl DeepMerge for ConfigMapKeySelector {
    fn merge_from(&mut self, other: Self) {
        if !other.key.is_empty() {
            self.key = other.key.clone();
        }
        if !other.name.is_empty() {
            self.name = other.name.clone();
        }
    }
}

/*
/// ConfigDialect selects between the dialects for a Clair config.
///
/// The default for the operator to create is JSON.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConfigDialect {
    /// JSON indicates a JSON config.
    #[default]
    JSON,
    /// YAML indicates a YAML config.
    YAML,
}

impl std::fmt::Display for ConfigDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigDialect::JSON => write!(f, "json"),
            ConfigDialect::YAML => write!(f, "yaml"),
        }
    }
}

impl DeepMerge for ConfigDialect {
    fn merge_from(&mut self, other: Self) {
        *self = other;
    }
}

// ImageRef exists to have some Object to hang pre/post Jobs off of.
// I don't think this is actually needed -- The can/could be driven off of a Condition.
/// ImageRefSpec is the spec of an ImageRef.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema,
)]
#[kube(
    group = "clairproject.org",
    version = "v1alpha1",
    kind = "ImageRef",
    namespaced,
    status = "ImageRefStatus",
    derive = "PartialEq",
    shortname = "imgref",
    category = "apps"
)]
#[serde(rename_all = "camelCase")]
pub struct ImageRefSpec {
    pub repository: String, // TODO(hank) verification
    pub tag: String,        // TODO(hank) verification
}

/// ImageRefStatus is the status of an ImageRef.
#[derive(Clone, Debug, Deserialize, Default, PartialEq, Serialize, Validate, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageRefStatus {}
*/

/// IndexerSpec describes the desired state of an Indexer instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema,
)]
#[kube(
    group = "clairproject.org",
    version = "v1alpha1",
    kind = "Indexer",
    namespaced,
    status = "WorkerStatus",
    shortname = "indexer",
    derive = "Default",
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
pub struct IndexerSpec {
    /// Image is the image that should be used in the managed deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(required)]
    pub config: Option<ConfigSource>,
    /// Gateway is the object to attach Gateway API routes to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<RouteParentRef>,
}

impl DeepMerge for IndexerSpec {
    fn merge_from(&mut self, other: Self) {
        self.image.merge_from(other.image);
        self.config.merge_from(other.config);
    }
}
/// WorkerStatus describes the observed state of a worker instance.
#[derive(Clone, Debug, Deserialize, Default, PartialEq, Serialize, Validate, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    /// Conditions reports k8s-style conditions for various parts of the system.
    #[schemars(schema_with = "schema::conditions")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<meta::v1::Condition>>,

    // Misc other refs we may need to hold onto, like Ingresses, Deployments, etc.
    /// Refs holds on to references to objects needed by this instance.
    #[schemars(schema_with = "schema::typed_local_object_references")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<core::v1::TypedLocalObjectReference>>,

    /// Dropin is a generated JSON dropin configuration that a Clair instance may use to construct
    /// its configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropin: Option<String>,
}

/// IndexerStatus describes the observed state of a Indexer instance.
#[derive(Clone, Debug, Deserialize, Default, PartialEq, Serialize, Validate, KubeSchema)]
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
    /// Dropin is a generated dropin configuration that a Clair instance may use to construct its
    /// configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropin: Option<String>,
}

/// MatcherSpec describes the desired state of an Matcher instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema,
)]
#[kube(
    group = "clairproject.org",
    version = "v1alpha1",
    kind = "Matcher",
    namespaced,
    status = "WorkerStatus",
    shortname = "matcher",
    derive = "PartialEq",
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct MatcherSpec {
    /// Image is the image that should be used in the managed deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
    /// Gateway is the object to attach Gateway API routes to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<RouteParentRef>,
}

/// MatcherStatus describes the observed state of a Matcher instance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema)]
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
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
    /// Dropin is a generated dropin configuration that a Clair instance may use to construct its
    /// configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropin: Option<String>,
}

/// UpdaterSpec describes the desired state of an Updater instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema,
)]
#[kube(
    group = "clairproject.org",
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
    /// Image is the image that should be used in the managed deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}

/// UpdaterStatus describes the observed state of a Updater instance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema)]
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
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}

/// NotifierSpec describes the desired state of an Notifier instance.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, KubeSchema,
)]
#[kube(
    group = "clairproject.org",
    version = "v1alpha1",
    kind = "Notifier",
    namespaced,
    status = "WorkerStatus",
    shortname = "notifier",
    derive = "PartialEq",
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct NotifierSpec {
    /// Image is the image that should be used in the managed deployment.
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
    /// Gateway is the object to attach Gateway API routes to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<RouteParentRef>,
}

/// NotifierStatus describes the observed state of a Notifier instance.
#[derive(Clone, Default, Debug, Deserialize, PartialEq, Serialize, Validate, KubeSchema)]
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
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}

/*
/// Private holds traits that external modules can't name, and so can't implement.
mod private {
    use k8s_openapi::{api::core, apimachinery::pkg::apis::meta};
    pub trait CrdCommon {
        type Spec: SpecCommon;
        type Status: StatusCommon;
        fn get_status(&self) -> Option<&Self::Status>;
        fn set_status(&mut self, s: Self::Status);
        fn get_spec(&self) -> &Self::Spec;
        fn set_spec(&mut self, s: Self::Spec);
    }
    pub trait StatusCommon {
        fn get_conditions(&self) -> &Vec<meta::v1::Condition>;
        fn set_conditions(&mut self, cnd: Vec<meta::v1::Condition>);
        fn get_refs(&self) -> &Vec<core::v1::TypedLocalObjectReference>;
        fn set_refs(&mut self, refs: Vec<core::v1::TypedLocalObjectReference>);
    }
    pub trait SubStatusCommon: StatusCommon {
        fn get_config(&self) -> Option<&super::ConfigSource>;
        fn set_config(&mut self, cfg: Option<super::ConfigSource>);
    }
    pub trait SpecCommon {
        fn get_image(&self) -> Option<&String>;
        fn set_image<S: ToString>(&mut self, img: S);
    }
    pub trait SubSpecCommon: SpecCommon {
        fn get_config(&self) -> Option<&super::ConfigSource>;
        fn set_config(&mut self, cfg: Option<super::ConfigSource>);
    }
}

/// CrdCommon is a trait to write generic helper functions for the CRDs in this module.
pub trait CrdCommon: private::CrdCommon + kube::Resource<DynamicType = ()> {
    /// Spec is the associated Spec type.
    type Spec: SpecCommon;
    /// Status is the associated Status type.
    type Status: StatusCommon;
}

macro_rules! impl_crds {
    ($(($kind:ty, $spec:ty, $status:ty)),+ $(,)?) => {
        $(
        impl std::fmt::Display for $kind {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_fmt(format_args!(
                    "{}({})",
                    stringify!($kind),
                    self.metadata.uid.as_deref().unwrap_or("<>"),
                ))
            }
        }
        impl private::CrdCommon for $kind {
            type Status = $status;
            type Spec = $spec;
            fn get_status(&self) -> Option<&Self::Status> {
                self.status.as_ref()
            }
            fn set_status(&mut self, s: Self::Status) {
                self.status = Some(s);
            }
            fn get_spec(&self) -> &Self::Spec {
                &self.spec
            }
            fn set_spec(&mut self, s: Self::Spec) {
                self.spec = s;
            }
        }
        impl CrdCommon for $kind {
            type Status = $status;
            type Spec = $spec;
        }
        )+
    };
}
impl_crds!(
    (Clair, ClairSpec, ClairStatus),
    (Indexer, IndexerSpec, IndexerStatus),
    (Matcher, MatcherSpec, MatcherStatus),
    (Notifier, NotifierSpec, NotifierStatus),
    (Updater, UpdaterSpec, UpdaterStatus),
);

/// StatusCommon is common helpers for dealing with status objects.
pub trait StatusCommon: private::StatusCommon {
    /// Add_condition adds a Condition, ensuring the list is deduplicated.
    fn add_condition(&mut self, cnd: meta::v1::Condition) {
        use self::meta::v1::Condition;
        let mut found = false;
        let mut out: Vec<Condition> = self
            .get_conditions()
            .iter()
            .map(|c| {
                if c.type_ == cnd.type_ {
                    found = true;
                    &cnd
                } else {
                    c
                }
            })
            .cloned()
            .collect();
        if !found {
            out.push(cnd);
        }
        out.sort_unstable_by_key(|c| c.type_.clone());
        self.set_conditions(out);
    }

    /// Add_ref adds a reference to `obj`, ensuring the list is deduplicated.
    fn add_ref<K>(&mut self, obj: &K)
    where
        K: kube::Resource<DynamicType = ()>,
    {
        use self::core::v1::TypedLocalObjectReference;
        let r = TypedLocalObjectReference {
            kind: K::kind(&()).to_string(),
            api_group: Some(K::api_version(&()).to_string()),
            name: obj.meta().name.as_ref().unwrap().clone(),
        };
        let mut found = false;
        let mut out: Vec<TypedLocalObjectReference> = self
            .get_refs()
            .iter()
            .map(|c| {
                if c.kind == r.kind {
                    found = true;
                    &r
                } else {
                    c
                }
            })
            .cloned()
            .collect();
        if !found {
            out.push(r);
        }
        out.sort_unstable_by_key(|c| c.kind.clone());
        self.set_refs(out);
    }

    /// Has_ref returns the reference for the type `K`, if present.
    fn has_ref<K>(&self) -> Option<core::v1::TypedLocalObjectReference>
    where
        K: kube::Resource<DynamicType = ()>,
    {
        let kind = K::kind(&());
        self.get_refs().iter().find(|r| r.kind == kind).cloned()
    }
}

macro_rules! impl_status {
    ($($kind:ty),+ $(,)?) => {
        $(
        impl private::StatusCommon for $kind {
            fn get_conditions(&self) -> &Vec<meta::v1::Condition> {
                &self.conditions
            }
            fn set_conditions(&mut self, cnd: Vec<meta::v1::Condition>) {
                self.conditions = cnd;
            }
            fn get_refs(&self) -> &Vec<core::v1::TypedLocalObjectReference> {
                &self.refs
            }
            fn set_refs(&mut self, refs: Vec<core::v1::TypedLocalObjectReference>) {
                self.refs = refs;
            }
        }
        impl StatusCommon for $kind {}
        )+
    };
}
impl_status!(
    ClairStatus,
    IndexerStatus,
    MatcherStatus,
    NotifierStatus,
    UpdaterStatus,
);

/// SpecCommon is helpers for working Spec objects.
pub trait SpecCommon: private::SpecCommon {
    /// Image_default reports the desired image, or "img" if unspecified.
    fn image_default(&self, img: &String) -> String {
        self.get_image().unwrap_or(img).clone()
    }
}

macro_rules! impl_spec {
    ($($kind:ty),+ $(,)?) => {
        $(
        impl private::SpecCommon for $kind {
            fn get_image(&self) -> Option<&String> {
                self.image.as_ref()
            }
            fn set_image<S: ToString>(&mut self, img:S) {
                self.image = Some(img.to_string());
            }
        }
        impl SpecCommon for $kind {}
        )+
    };
}
impl_spec!(
    ClairSpec,
    IndexerSpec,
    MatcherSpec,
    NotifierSpec,
    UpdaterSpec,
);

/// SubStatusCommon is helpers for the common "subresource" types.
pub trait SubStatusCommon: private::SubStatusCommon {
    /// .
    fn get_config(&self) -> Option<&ConfigSource> {
        private::SubStatusCommon::get_config(self)
    }
    /// .
    fn set_config(&mut self, cfg: Option<ConfigSource>) {
        private::SubStatusCommon::set_config(self, cfg)
    }
}
macro_rules! impl_substatus {
    ($($kind:ty),+ $(,)?) => {
        $(
        impl private::SubStatusCommon for $kind {
            fn get_config(&self) -> Option<&ConfigSource> {
                self.config.as_ref()
            }
            fn set_config(&mut self, cfg: Option<ConfigSource>) {
                self.config = cfg;
            }
        }
        impl SubStatusCommon for $kind {}
        )+
    };
}
impl_substatus!(IndexerStatus, MatcherStatus, NotifierStatus);

/// SubSpecCommon is helpers for the common "subresource" types.
pub trait SubSpecCommon: private::SubSpecCommon {
    /// Set_values sets the common parts of the spec.
    fn set_values<S: ToString>(&mut self, img: S, cfg: Option<ConfigSource>) {
        self.set_image(img);
        self.set_config(cfg);
    }

    /// .
    fn get_config(&self) -> Option<&ConfigSource> {
        private::SubSpecCommon::get_config(self)
    }
}
macro_rules! impl_subspec {
    ($($kind:ty),+ $(,)?) => {
        $(
        impl private::SubSpecCommon for $kind {
            fn get_config(&self) -> Option<&ConfigSource> {
                self.config.as_ref()
            }
            fn set_config(&mut self, cfg: Option<ConfigSource>) {
                self.config = cfg;
            }
        }
        impl SubSpecCommon for $kind {}
        )+
    };
}
impl_subspec!(IndexerSpec, MatcherSpec, NotifierSpec);
*/

mod schema {
    use k8s_openapi::{api::core, apimachinery::pkg::apis::meta};
    use schemars::{Schema, generate::SchemaGenerator};
    use serde_json::json;

    pub fn conditions(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = generator.subschema_for::<Vec<meta::v1::Condition>>();

        schema
            .ensure_object()
            .entry("x-kubernetes-list-type")
            .or_insert_with(|| json!("map"));
        schema
            .ensure_object()
            .entry("x-kubernetes-list-map-keys")
            .or_insert_with(|| json!(["type"]));
        schema
            .ensure_object()
            .insert("items".into(), condition(generator).into());

        schema
    }

    pub fn condition(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = generator.subschema_for::<meta::v1::Condition>();

        schema.ensure_object().entry("required").or_insert_with(|| {
            json!(["type", "status", "lastTransitionTime", "reason", "message"])
        });

        schema
            .ensure_object()
            .entry("properties")
            .or_insert_with(|| json!({
                "type": {
                    "type": "string",
                    "pattern": r#"^([a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*/)?(([A-Za-z0-9][-A-Za-z0-9_.]*)?[A-Za-z0-9])$"#,
                    "max_length": 316,
                },
                "status": {
                    "enum": ["True", "False", "Unknown"],
                },
                "observedGeneration": {
                    "type": "number",
                    "minimum": 0,
                },
                "lastTransitionTime": { "format": "date-time" },
                "reason": {
                    "type": "string",
                    "pattern": r#"^[A-Za-z]([A-Za-z0-9_,:]*[A-Za-z0-9_])?$"#,
                    "min_length": 1,
                    "max_length": 1024,
                },
                "message": {
                    "type": "string",
                    "max_length": 32768,
                },
            }));

        schema
    }

    pub fn typed_local_object_references(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = generator.subschema_for::<Vec<core::v1::TypedLocalObjectReference>>();

        schema
            .ensure_object()
            .entry("x-kubernetes-list-type")
            .or_insert_with(|| json!("map"));
        schema
            .ensure_object()
            .entry("x-kubernetes-list-map-keys")
            .or_insert_with(|| json!(["kind"]));

        schema
    }
}
