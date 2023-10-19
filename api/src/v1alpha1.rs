//! Module `v1alpha1` implements the v1alpha1 Clair CRD API.
use k8s_openapi::{api::core, apimachinery::pkg::apis::meta, merge_strategies, DeepMerge};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

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
    /// .
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
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
    pub dropins: Vec<DropinSource>,
    /// ConfigDialect specifies the format to generate for the main config.
    ///
    /// This setting affects what format config drop-ins must be in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dialect: Option<ConfigDialect>,
}

impl ClairSpec {
    /// With_root creates the desired ConfigSource, using the provided name as the root config.
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
        let flavor = self.config_dialect.unwrap_or_default();
        ConfigSource {
            root: ConfigMapKeySelector {
                name,
                key: format!("config.{flavor}"),
            },
            dropins,
        }
    }
}

impl DeepMerge for ClairSpec {
    fn merge_from(&mut self, other: Self) {
        self.image.merge_from(other.image);
        self.databases.merge_from(other.databases);
        self.endpoint.merge_from(other.endpoint);
        self.notifier.merge_from(other.notifier);
        merge_strategies::list::set(self.dropins.as_mut(), other.dropins);
        self.config_dialect.merge_from(other.config_dialect);
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

/// Endpoint describes how a frontend (e.g. an Ingress) should be configured.
#[derive(Clone, Default, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// Hostname indicates the desired hostname.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// TLS inicates the `kubernetes.io/tls`-typed Secret that should be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<core::v1::LocalObjectReference>,
}

impl DeepMerge for Endpoint {
    fn merge_from(&mut self, other: Self) {
        self.hostname.merge_from(other.hostname);
        self.tls.merge_from(other.tls);
    }
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
    /// Previous_version is the previous verison of a deployed Clair instance, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    /// Current_version is the current verison of a deployed Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
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
#[derive(
    Clone, Debug, Deserialize, PartialEq, PartialOrd, Eq, Ord, Serialize, Validate, JsonSchema,
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
    JsonSchema,
)]
#[serde(rename_all = "camelCase")]
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
    JsonSchema,
)]
#[serde(rename_all = "camelCase")]
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
    JsonSchema,
)]
#[serde(rename_all = "camelCase")]
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
/*
/// ImageRefSpec is the spec of an ImageRef.
#[derive(
    CustomResource, Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema,
)]
#[kube(
    group = "projectclair.io",
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
#[derive(Clone, Debug, Deserialize, Default, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageRefStatus {}
*/

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
    /// Image is the image that should be used in the managed deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}

impl DeepMerge for IndexerSpec {
    fn merge_from(&mut self, other: Self) {
        self.image.merge_from(other.image);
        self.config.merge_from(other.config);
    }
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
    /// Image is the image that should be used in the managed deployment.
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}
/// MatcherStatus describes the observed state of a Matcher instance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
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

    /// Image is the image that should be used in the managed deployment.
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}

/// UpdaterStatus describes the observed state of a Updater instance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
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
    /// Image is the image that should be used in the managed deployment.
    pub image: Option<String>,
    /// Config is configuration sources for the Clair instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
}
/// NotifierStatus describes the observed state of a Notifier instance.
#[derive(Clone, Default, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
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

/// SubSpecCommon is helper for the common "subresource" types.
pub trait SubSpecCommon: private::SubSpecCommon {
    /// Set_values sets the common parts of the spec.
    fn set_values<S: ToString>(&mut self, img: S, cfg: Option<ConfigSource>) {
        self.set_image(img);
        self.set_config(cfg);
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
