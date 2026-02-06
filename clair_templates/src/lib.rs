#![cfg_attr(debug_assertions, warn(missing_docs))]
#![cfg_attr(debug_assertions, warn(rustdoc::broken_intra_doc_links))]
#![cfg_attr(not(debug_assertions), deny(missing_docs))]
#![cfg_attr(not(debug_assertions), deny(rustdoc::broken_intra_doc_links))]

//! Clair_templates holds the templating logic for controllers.
//!
//! To create Kubernetes objects, use [`TryFrom`] on a reference to a `clairproject.org/v1alpha1`
//! object.
//!
//! ```
//! # use api::v1alpha1::*;
//! # use serde_json::{from_value, json};
//! use clair_templates::{IndexerBuilder, Build};
//!
//! // Get this from the k8s API in a real use.
//! let c: Clair = from_value(json!({
//!     "metadata":{
//!         "name": "example",
//!         "namespace": "default",
//!         "uid": "6060",
//!     },
//!     "spec": {
//!         "image": "quay.io/projectquay/clair:nightly",
//!     },
//!     "status": {
//!         "config": {
//!             "root": {
//!                 "name": "clair-config",
//!                 "key": "config.json",
//!             },
//!         },
//!     },
//! })).unwrap();
//!
//! IndexerBuilder::try_from(&c).unwrap().build();
//! ```

use std::{collections::BTreeMap, sync::LazyLock};

use gateway_networking_k8s_io::v1::httproutes::HTTPRoute;
use k8s_openapi::{
    api::{apps::v1::*, autoscaling::v2::*, batch::v1::*, core::v1::*},
    apimachinery::pkg::{
        api::resource::Quantity,
        apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference},
        util::intstr::IntOrString,
    },
};
use kube::{Resource, ResourceExt};
use serde_json::{json, to_string as json_string};

use api::v1alpha1::*;

/// DEFAULT_CONFIG is a minimal default configuration for a Clair system.
///
/// The returned value depends on the minimum version feature selected.
pub static DEFAULT_CONFIG: LazyLock<String> = LazyLock::new(|| {
    #[cfg(feature = "v1_4")]
    let v = json!({
        "http_listen_addr": ":6060",
        "introspection_addr": ":8089",
        "log_level": "info",
        "matcher": { "migrations": true },
        "indexer": { "migrations": true },
        "notifier": { "migrations": true },
        "metrics": {
            "name": "prometheus"
        },
    });
    #[cfg(feature = "v1_5")]
    let v = json!({
       "log_level": "info",
       "matcher": { "migrations": true },
       "indexer": { "migrations": true },
       "notifier": { "migrations": true },
       "metrics": {
           "name": "prometheus"
       },
       "api": {
            "v1": {
                "enabled": true,
                "network": "tcp",
                "address": ":6060",
            },
       },
       "introspection": {
           "network": "tcp",
           "address": ":8089",
       },
    });
    json_string(&v).expect("programmer error: static data")
});

const CONFIG_ROOT_KEY: &str = "config.json";
const CONFIG_ROOT_VOLUME_NAME: &str = "root-config";
const CONFIG_DROPIN_VOLUME_NAME: &str = "dropin-config";
const CONFIG_FILENAME: &str = "/etc/clair/config.json";
const LAYER_VOLUME_NAME: &str = "layer-scratch";

/// Error is the error domain for creating templates.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Unable to determine a namespace.
    #[error("unable to determine namespace")]
    Namespace,
    /// Missing an image name.
    #[error("missing required information: image")]
    MissingImage,
    /// Missing the configuration source.
    #[error("missing required information: ConfigSource")]
    MissingConfigSource,
    /// Unable to construct an owner reference.
    #[error("unable to construct owner reference")]
    OwnerReference,
    /// Error while parsing a value.
    #[error("parse error: {0}")]
    Parse(#[from] strum::ParseError),
    /// Any other error.
    #[error("other error: {0}")]
    Other(&'static str),
}

// Some helpers:

/// S is a helper to return an `Option<String>`.
#[inline]
fn s<S: ToString>(v: S) -> Option<String> {
    v.to_string().into()
}

/// Render_dropin returns a config json-patch as a string.
///
/// If used for a resource that doesn't need a dropin, [`None`] is reported.
pub fn render_dropin<O>(srv: &Service) -> Option<String>
where
    O: Resource<DynamicType = ()>,
{
    let name = srv.name_unchecked();
    let ns = srv.namespace().expect("Services are namespaced");
    let addr = format!("{name}.{ns}.svc.cluster.local");

    let v = match O::kind(&()).as_ref() {
        "Indexer" => json!([
          { "op": "add", "path": "/matcher/indexer_addr",  "value": addr },
          { "op": "add", "path": "/notifier/indexer_addr", "value": addr },
        ]),
        "Matcher" => json!([
          { "op": "add", "path": "/indexer/matcher_addr",  "value": addr },
          { "op": "add", "path": "/notifier/matcher_addr", "value": addr },
        ]),
        _ => return None,
    };

    json_string(&v).ok()
}

/// Standard_Labels is the standard set of labels for resources created by this module.
fn standard_labels<S: ToString>(component: S) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".into(), "clair".into()),
        (
            "app.kubernetes.io/managed-by".into(),
            "clair-operator".into(),
        ),
        ("app.kubernetes.io/component".into(), component.to_string()),
    ])
}

/// Make_volumes creates a vector of required volume specifications needed to make use of the
/// `ConfigSource`.
fn make_volumes(cfgsrc: &ConfigSource) -> Vec<Volume> {
    enum Projection {
        ConfigMap(String, KeyToPath),
        Secret(String, KeyToPath),
    }

    let sources = cfgsrc
        .dropins
        .iter()
        .map(|d| {
            let name = d.name.clone();
            let kp = KeyToPath {
                path: d.key.clone(),
                key: d.key.clone(),
                mode: None,
            };
            let p = match d.type_ {
                DropinType::ConfigMap => Projection::ConfigMap(name, kp),
                DropinType::Secret => Projection::Secret(name, kp),
            };

            let mut v: VolumeProjection = Default::default();
            match p {
                Projection::ConfigMap(name, item) => {
                    v.config_map = ConfigMapProjection {
                        name,
                        optional: false.into(),
                        items: vec![item].into(),
                    }
                    .into()
                }
                Projection::Secret(name, item) => {
                    v.secret = SecretProjection {
                        name,
                        optional: false.into(),
                        items: vec![item].into(),
                    }
                    .into()
                }
            };
            v
        })
        .collect::<Vec<_>>();

    vec![
        Volume {
            name: CONFIG_ROOT_VOLUME_NAME.into(),
            config_map: Some(ConfigMapVolumeSource {
                name: cfgsrc.root.name.clone(),
                items: Some(vec![KeyToPath {
                    key: cfgsrc.root.key.clone(),
                    path: cfgsrc.root.key.clone(),
                    mode: Some(0o666),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: CONFIG_DROPIN_VOLUME_NAME.into(),
            projected: Some(ProjectedVolumeSource {
                sources: sources.into(),
                default_mode: Some(0o644),
            }),
            ..Default::default()
        },
    ]
}

/// Build is a common trait for constructing an object from a builder.
pub trait Build {
    /// Output is the output type.
    type Output;

    /// Build constructs and returns the final object.
    ///
    /// This is infallible because values are checked when set on the builder.
    fn build(self) -> Self::Output;
}

// TODO(hank): These WorkerBuilder types can probably get consolidated with some judicious
// application of generics.

// TODO(hank): Most of these Builders could probably capture a reference to the "from" object and
// only clone things as needed during `build`.

// As a rule of thumb, `From` should be used for deriving new builders from local types, and
// `TryFrom` for deriving builders from "remote" types. That means the error checking only happens
// at the top-most interaction and doesn't have to get pushed all the way to a `build()` call.

/// IndexerBuilder constructs an [`Indexer`] from a [`Clair`].
pub struct IndexerBuilder {
    namespace: String,
    name: String,
    ctl_ref: OwnerReference,
    image: String,
    cfgsrc: ConfigSource,
    gateway: Option<RouteParentRef>,
}

impl TryFrom<&Clair> for IndexerBuilder {
    type Error = Error;

    fn try_from(value: &Clair) -> Result<Self, Self::Error> {
        let name = value.name_unchecked();
        let namespace = value.namespace().ok_or(Error::Namespace)?;
        let image = value.spec.image.clone().ok_or(Error::MissingImage)?;
        let cfgsrc = value
            .status
            .as_ref()
            .and_then(|status| status.config.clone())
            .ok_or(Error::MissingConfigSource)?;
        let ctl_ref = value
            .controller_owner_ref(&())
            .ok_or(Error::Other("unable to construct controller ref"))?;
        let gateway = value.spec.gateway.clone();

        Ok(Self {
            namespace,
            name,
            image,
            cfgsrc,
            ctl_ref,
            gateway,
        })
    }
}

impl Build for IndexerBuilder {
    type Output = Indexer;

    fn build(self) -> Self::Output {
        let labels = standard_labels(Indexer::kind(&()).to_ascii_lowercase());

        Indexer {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                owner_references: vec![self.ctl_ref].into(),
                labels: labels.into(),
                ..Default::default()
            },
            spec: IndexerSpec {
                image: self.image.into(),
                gateway: self.gateway,
                config: self.cfgsrc.into(),
            },
            ..Default::default()
        }
    }
}

/// MatcherBuilder constructs a [`Matcher`] from a [`Clair`].
pub struct MatcherBuilder {
    namespace: String,
    name: String,
    ctl_ref: OwnerReference,
    image: String,
    cfgsrc: ConfigSource,
    gateway: Option<RouteParentRef>,
}

impl TryFrom<&Clair> for MatcherBuilder {
    type Error = Error;
    fn try_from(value: &Clair) -> Result<Self, Self::Error> {
        let name = value.name_unchecked();
        let namespace = value.namespace().ok_or(Error::Namespace)?;
        let image = value.spec.image.clone().ok_or(Error::MissingImage)?;
        let cfgsrc = value
            .status
            .as_ref()
            .and_then(|status| status.config.clone())
            .ok_or(Error::MissingConfigSource)?;
        let ctl_ref = value
            .controller_owner_ref(&())
            .ok_or(Error::Other("unable to construct controller ref"))?;
        let gateway = value.spec.gateway.clone();

        Ok(Self {
            namespace,
            name,
            image,
            cfgsrc,
            ctl_ref,
            gateway,
        })
    }
}

impl Build for MatcherBuilder {
    type Output = Matcher;

    fn build(self) -> Self::Output {
        let labels = standard_labels(Matcher::kind(&()).to_ascii_lowercase());
        Matcher {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                owner_references: vec![self.ctl_ref].into(),
                labels: labels.into(),
                ..Default::default()
            },
            spec: MatcherSpec {
                image: self.image.into(),
                gateway: self.gateway,
                config: self.cfgsrc.into(),
            },
            ..Default::default()
        }
    }
}

/// NotifierBuilder constructs a [`Notifier`] from a [`Clair`].
pub struct NotifierBuilder {
    namespace: String,
    name: String,
    ctl_ref: OwnerReference,
    image: String,
    cfgsrc: ConfigSource,
    gateway: Option<RouteParentRef>,
}

impl TryFrom<&Clair> for NotifierBuilder {
    type Error = Error;
    fn try_from(value: &Clair) -> Result<Self, Self::Error> {
        let name = value.name_unchecked();
        let namespace = value.namespace().ok_or(Error::Namespace)?;
        let image = value.spec.image.clone().ok_or(Error::MissingImage)?;
        let cfgsrc = value
            .status
            .as_ref()
            .and_then(|status| status.config.clone())
            .ok_or(Error::MissingConfigSource)?;
        let ctl_ref = value
            .controller_owner_ref(&())
            .ok_or(Error::Other("unable to construct controller ref"))?;
        let gateway = value.spec.gateway.clone();

        Ok(Self {
            namespace,
            name,
            image,
            cfgsrc,
            ctl_ref,
            gateway,
        })
    }
}

impl Build for NotifierBuilder {
    type Output = Notifier;

    fn build(self) -> Self::Output {
        let labels = standard_labels(Notifier::kind(&()).to_ascii_lowercase());

        Notifier {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                owner_references: vec![self.ctl_ref].into(),
                labels: labels.into(),
                ..Default::default()
            },
            spec: NotifierSpec {
                image: self.image.into(),
                gateway: self.gateway,
                config: self.cfgsrc.into(),
            },
            ..Default::default()
        }
    }
}

/// ConfigMapBuilder builds a [`ConfigMap`] owned buy the [`Clair`] instance used to construct the
/// builder.
pub struct ConfigMapBuilder {
    namespace: String,
    name: String,
    owner_ref: OwnerReference,
    root_key: Option<String>,
    root_value: Option<String>,
}

impl TryFrom<&Clair> for ConfigMapBuilder {
    type Error = Error;

    fn try_from(value: &Clair) -> Result<Self, Self::Error> {
        let namespace = value.namespace().ok_or(Error::Namespace)?;
        let name = value.name_unchecked();
        let owner_ref = value
            .controller_owner_ref(&())
            .ok_or(Error::Other("unable to construct controller ref"))?;

        Ok(Self {
            namespace,
            name,
            owner_ref,
            root_key: None,
            root_value: None,
        })
    }
}

impl ConfigMapBuilder {
    /// Set the key for the Clair configuration in the resulting [`ConfigMap`].
    pub fn with_key<S: ToString>(self, key: S) -> Self {
        Self {
            root_key: key.to_string().into(),
            ..self
        }
    }

    /// Set the value for the Clair configuration in the resulting [`ConfigMap`].
    pub fn with_config<S: ToString>(self, config: S) -> Self {
        Self {
            root_value: config.to_string().into(),
            ..self
        }
    }
}

impl Build for ConfigMapBuilder {
    type Output = ConfigMap;

    fn build(self) -> Self::Output {
        let labels = standard_labels(<ConfigMap>::kind(&()).to_ascii_lowercase());

        ConfigMap {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                owner_references: vec![self.owner_ref].into(),
                labels: labels.into(),
                ..Default::default()
            },
            data: BTreeMap::from([(
                self.root_key.unwrap_or_else(|| CONFIG_ROOT_KEY.to_string()),
                self.root_value.unwrap_or_else(|| DEFAULT_CONFIG.clone()),
            )])
            .into(),
            ..Default::default()
        }
    }
}

/// CronJobBuilder is a builder for a [`CronJob`].
pub struct CronJobBuilder {
    namespace: String,
    name: String,
    kind: ContainerKind, // Needed for a macro later.
    image: String,
    cfgsrc: ConfigSource,
    owner_ref: OwnerReference,
}

impl TryFrom<&Updater> for CronJobBuilder {
    type Error = Error;

    fn try_from(value: &Updater) -> Result<Self, Self::Error> {
        let namespace = value.namespace().ok_or(Error::Namespace)?;
        let name = format!(
            "{}-{}",
            value.name_unchecked(),
            Updater::kind(&()).to_ascii_lowercase()
        );
        let image = value.spec.image.clone().ok_or(Error::MissingImage)?;
        let cfgsrc = value
            .status
            .as_ref()
            .and_then(|status| status.config.clone())
            .ok_or(Error::MissingConfigSource)?;
        let owner_ref = value.owner_ref(&()).ok_or(Error::OwnerReference)?;

        Ok(Self {
            namespace,
            name,
            image,
            cfgsrc,
            owner_ref,
            kind: ContainerKind::Updater,
        })
    }
}

impl Build for CronJobBuilder {
    type Output = CronJob;

    fn build(self) -> Self::Output {
        let kind = Updater::kind(&()).to_ascii_lowercase();
        let labels = standard_labels(kind);
        let container = ContainerBuilder::from(&self).build();
        let volumes = make_volumes(&self.cfgsrc);

        CronJob {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                labels: labels.clone().into(),
                owner_references: vec![self.owner_ref].into(),
                ..Default::default()
            },
            spec: CronJobSpec {
                concurrency_policy: s("Forbid"),
                starting_deadline_seconds: 10.into(),
                time_zone: s("Etc/UTC"),
                schedule: "0 */8 * * *".to_string(),
                job_template: JobTemplateSpec {
                    metadata: ObjectMeta {
                        labels: labels.clone().into(),
                        ..Default::default()
                    }
                    .into(),
                    spec: JobSpec {
                        active_deadline_seconds: 3600.into(),
                        completion_mode: s("NonIndexed"),
                        completions: 1.into(),
                        parallelism: 1.into(),
                        template: PodTemplateSpec {
                            metadata: ObjectMeta {
                                labels: labels.clone().into(),
                                ..Default::default()
                            }
                            .into(),
                            spec: PodSpec {
                                termination_grace_period_seconds: 10.into(),
                                share_process_namespace: true.into(),
                                security_context: PodSecurityContext {
                                    run_as_user: 65532.into(),
                                    ..Default::default()
                                }
                                .into(),
                                containers: vec![container],
                                volumes: volumes.into(),
                                ..Default::default()
                            }
                            .into(),
                        },
                        ..Default::default()
                    }
                    .into(),
                },
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }
}

/// JobBuilder is a builder for a [`Job`].
pub struct JobBuilder {
    namespace: String,
    name: String,
    kind: JobKind,
    image: String,
    version: String,
    cfgsrc: ConfigSource,
    owner_ref: OwnerReference,
}

#[derive(Clone, Copy, strum::Display, strum::EnumString, strum::AsRefStr)]
#[strum(serialize_all = "kebab-case")]
enum JobKind {
    AdminPre,
    AdminPost,
}

impl JobBuilder {
    /// Admin_pre creates a builder to run a `clairctl admin pre` command.
    pub fn admin_pre(clair: &Clair) -> Result<Self, Error> {
        Self::new(clair, JobKind::AdminPre)
    }

    /// Admin_post creates a builder to run a `clairctl admin post` command.
    pub fn admin_post(clair: &Clair) -> Result<Self, Error> {
        Self::new(clair, JobKind::AdminPost)
    }

    fn new(clair: &Clair, kind: JobKind) -> Result<Self, Error> {
        let cfgsrc = clair
            .status
            .as_ref()
            .and_then(|status| status.config.clone())
            .ok_or(Error::MissingConfigSource)?;
        let image = clair.spec.image.clone().ok_or(Error::MissingImage)?;
        let version = image
            .rsplit_once(':')
            .map(|(_, tag)| tag)
            .ok_or(Error::Other("image ref missing tag"))?
            .to_string();
        let name = format!("{}-{kind}-{version}", clair.name_unchecked());
        let namespace = clair.namespace().ok_or(Error::Namespace)?;
        let owner_ref = clair.owner_ref(&()).ok_or(Error::OwnerReference)?;

        Ok(Self {
            namespace,
            name,
            kind,
            image,
            version,
            cfgsrc,
            owner_ref,
        })
    }
}

impl Build for JobBuilder {
    type Output = Job;

    fn build(self) -> Self::Output {
        let container = ContainerBuilder::from(&self).args([self.version]).build();
        let volumes = make_volumes(&self.cfgsrc);
        let labels = standard_labels(Clair::kind(&()).to_ascii_lowercase());

        Job {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                labels: labels.clone().into(),
                owner_references: vec![self.owner_ref].into(),
                ..Default::default()
            },
            spec: JobSpec {
                active_deadline_seconds: 3600.into(),
                completion_mode: s("NonIndexed"),
                completions: 1.into(),
                parallelism: 1.into(),
                template: PodTemplateSpec {
                    metadata: ObjectMeta {
                        labels: labels.into(),
                        ..Default::default()
                    }
                    .into(),
                    spec: PodSpec {
                        termination_grace_period_seconds: 10.into(),
                        share_process_namespace: true.into(),
                        security_context: PodSecurityContext {
                            run_as_user: 65532.into(),
                            ..Default::default()
                        }
                        .into(),
                        containers: vec![container],
                        volumes: volumes.into(),
                        ..Default::default()
                    }
                    .into(),
                },
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }
}

/// HorizontalPodAutoscalerBuilder is a builder for a [`HorizontalPodAutoscaler`].
pub struct HorizontalPodAutoscalerBuilder {
    namespace: String,
    name: String,
    kind: HorizontalPodAutoscalerKind,
    owner_ref: OwnerReference,
}

/// This is a macro to write the repetitive [`TryFrom`] implementations.
macro_rules! tryfrom_impls_hpa {
    ($($from:ty),+) => {
        $(
        impl TryFrom<&$from> for HorizontalPodAutoscalerBuilder {
            type Error = Error;

            fn try_from(value: &$from) -> Result<Self, Self::Error> {
                let k = stringify!($from).to_ascii_lowercase();
                let namespace = value.namespace().ok_or(Error::Namespace)?;
                let name = format!( "{}-{k}", value.name_unchecked());
                let kind = HorizontalPodAutoscalerKind::try_from(k.as_str())?;
                let owner_ref = value.owner_ref(&()).ok_or(Error::OwnerReference)?;

                Ok(Self {
                    namespace,
                    name,
                    kind,
                    owner_ref,
                })
            }
        }
        )+
    };
}
tryfrom_impls_hpa!(Indexer, Matcher, Notifier);

#[derive(Clone, Copy, strum::Display, strum::EnumString, strum::AsRefStr)]
#[strum(serialize_all = "lowercase")]
enum HorizontalPodAutoscalerKind {
    Indexer,
    Matcher,
    Notifier,
}

impl Build for HorizontalPodAutoscalerBuilder {
    type Output = HorizontalPodAutoscaler;

    fn build(self) -> Self::Output {
        let labels = standard_labels(self.kind);

        HorizontalPodAutoscaler {
            metadata: ObjectMeta {
                name: self.name.clone().into(),
                namespace: self.namespace.into(),
                labels: labels.into(),
                owner_references: vec![self.owner_ref].into(),
                ..Default::default()
            },
            spec: HorizontalPodAutoscalerSpec {
                max_replicas: 10,
                scale_target_ref: CrossVersionObjectReference {
                    api_version: s("apps/v1"),
                    kind: "Deployment".into(),
                    name: self.name,
                },
                metrics: vec![MetricSpec {
                    type_: "Resource".into(),
                    resource: ResourceMetricSource {
                        name: "cpu".into(),
                        target: MetricTarget {
                            type_: "Utilization".into(),
                            average_utilization: 80.into(),
                            ..Default::default()
                        },
                    }
                    .into(),
                    ..Default::default()
                }]
                .into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }
}

/// ServiceBuilder is a builder for a [`Service`].
pub struct ServiceBuilder {
    namespace: String,
    name: String,
    kind: ServiceKind,
    owner_ref: OwnerReference,
}

/// This is a macro to write the repetitive [`TryFrom`] implementations.
macro_rules! tryfrom_impls_service {
    ($($from:ty),+) => {
        $(
        impl TryFrom<&$from> for ServiceBuilder {
            type Error = Error;

            fn try_from(value: &$from) -> Result<Self, Self::Error> {
                let k = stringify!($from).to_ascii_lowercase();
                let namespace = value.namespace().ok_or(Error::Namespace)?;
                let name = format!( "{}-{k}", value.name_unchecked());
                let kind = ServiceKind::try_from(k.as_str())?;
                let owner_ref = value.owner_ref(&()).ok_or(Error::OwnerReference)?;

                Ok(Self {
                    namespace,
                    name,
                    kind,
                    owner_ref,
                })
            }
        }
        )+
    };
}
tryfrom_impls_service!(Indexer, Matcher, Notifier);

#[derive(Clone, Copy, strum::Display, strum::EnumString, strum::AsRefStr)]
#[strum(serialize_all = "lowercase")]
enum ServiceKind {
    Indexer,
    Matcher,
    Notifier,
}

static API_PORT: LazyLock<ServicePort> = LazyLock::new(|| ServicePort {
    name: s("api"),
    port: 80,
    target_port: IntOrString::String("api".into()).into(),
    ..Default::default()
});

impl Build for ServiceBuilder {
    type Output = Service;

    fn build(self) -> Self::Output {
        let labels = standard_labels(self.kind);

        Service {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                labels: labels.clone().into(),
                owner_references: vec![self.owner_ref].into(),
                ..Default::default()
            },
            spec: ServiceSpec {
                selector: labels.into(),
                ports: vec![API_PORT.clone()].into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }
}

/// HTTPRouteBuilder is a builder for a [`HTTPRoute`].
pub struct HTTPRouteBuilder {
    namespace: String,
    name: String,
    kind: RouteKind,
    owner_ref: OwnerReference,
    service: Service,

    gateway: Option<RouteParentRef>,
}

/// This is a macro to write the repetitive [`TryFrom`] implementations.
macro_rules! tryfrom_impls_httproute {
    ($($from:ty),+) => {
        $(
        impl TryFrom<&$from> for HTTPRouteBuilder {
            type Error = Error;

            fn try_from(value: &$from) -> Result<Self, Self::Error> {
                let k = stringify!($from).to_ascii_lowercase();
                let namespace = value.namespace().ok_or(Error::Namespace)?;
                let name = format!( "{}-{k}", value.name_unchecked());
                let kind = RouteKind::try_from(k.as_str())?;
                let owner_ref = value.owner_ref(&()).ok_or(Error::OwnerReference)?;
                let gateway = value.spec.gateway.clone();
                let service = ServiceBuilder::try_from(value)?.build();

                Ok(Self {
                    namespace,
                    name,
                    kind,
                    owner_ref,
                    service,
                    gateway,
                })
            }
        }
        )+
    };
}
tryfrom_impls_httproute!(Indexer, Matcher, Notifier);

impl Build for HTTPRouteBuilder {
    type Output = HTTPRoute;

    fn build(self) -> Self::Output {
        use gateway_networking_k8s_io::v1::httproutes::*;

        let r = HTTPRoute {
            metadata: ObjectMeta {
                name: self.name.clone().into(),
                owner_references: vec![self.owner_ref].into(),
                ..Default::default()
            },
            spec: HttpRouteSpec {
                ..Default::default()
            },
            ..Default::default()
        };
        if self.gateway.is_none() {
            return r;
        }
        let gateway = self.gateway.unwrap();

        let parent_ref = HttpRouteParentRefs {
            namespace: gateway.namespace.clone(),
            name: gateway.name.clone().unwrap_or_else(|| self.name.clone()),

            group: gateway.group.clone(),
            kind: gateway.kind.clone(),
            section_name: gateway.section_name.clone(),

            ..Default::default()
        };
        let rule = HttpRouteRules {
            matches: vec![HttpRouteRulesMatches::from(&self.kind)].into(),
            backend_refs: vec![HttpRouteRulesBackendRefs {
                namespace: self.namespace.clone().into(),
                name: self.service.name_any(),
                group: Service::group(&()).to_string().into(),
                kind: Service::kind(&()).to_string().into(),
                ..Default::default()
            }]
            .into(),
            ..Default::default()
        };

        HTTPRoute {
            metadata: ObjectMeta {
                name: self.name.clone().into(),
                ..Default::default()
            },
            spec: HttpRouteSpec {
                parent_refs: vec![parent_ref].into(),
                rules: vec![rule].into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, strum::Display, strum::EnumString, strum::AsRefStr)]
#[strum(serialize_all = "lowercase")]
enum RouteKind {
    Indexer,
    Matcher,
    Notifier,
}

impl From<&RouteKind> for gateway_networking_k8s_io::v1::httproutes::HttpRouteRulesMatches {
    fn from(kind: &RouteKind) -> Self {
        use gateway_networking_k8s_io::v1::httproutes::*;

        let value = s(match kind {
            RouteKind::Indexer => "/indexer/api/v1/",
            RouteKind::Matcher => "/matcher/api/v1/",
            RouteKind::Notifier => "/notifier/api/v1/",
        });

        HttpRouteRulesMatches {
            path: HttpRouteRulesMatchesPath {
                r#type: HttpRouteRulesMatchesPathType::PathPrefix.into(),
                value,
            }
            .into(),
            ..Default::default()
        }
    }
}

impl From<&RouteKind> for Vec<gateway_networking_k8s_io::v1::grpcroutes::GrpcRouteRulesMatches> {
    /// None, yet.
    fn from(_value: &RouteKind) -> Self {
        //use gateway_networking_k8s_io::v1::grpcroutes::*;
        vec![]
    }
}

/// DeploymentBuilder is a builder for a [`Deployment`].
pub struct DeploymentBuilder {
    namespace: String,
    name: String,
    kind: DeploymentKind,
    image: String,
    cfgsrc: ConfigSource,
    owner_ref: OwnerReference,
}

/// This is a macro to write the repetitive [`TryFrom`] implementations.
macro_rules! tryfrom_impls_deployment {
    ($($from:ty),+) => {
        $(
        impl TryFrom<&$from> for DeploymentBuilder {
            type Error = Error;

            fn try_from(value: &$from) -> Result<Self, Self::Error> {
                let k = stringify!($from).to_ascii_lowercase();
                let namespace = value.namespace().ok_or(Error::Namespace)?;
                let name = format!("{}-{k}", value.name_unchecked());
                let kind = DeploymentKind::try_from(k.as_str())?;
                let image = value.spec.image.clone().ok_or(Error::MissingImage)?;
                let cfgsrc = value
                    .spec
                    .config
                    .clone()
                    .ok_or(Error::MissingConfigSource)?;
                let owner_ref = value.owner_ref(&()).ok_or(Error::OwnerReference)?;

                Ok(Self {
                    namespace,
                    name,
                    kind,
                    cfgsrc,
                    image,
                    owner_ref,
                })
            }
        }
        )+
    };
}
tryfrom_impls_deployment!(Indexer, Matcher, Notifier);

#[derive(Clone, Copy, strum::Display, strum::EnumString, strum::AsRefStr, PartialEq)]
#[strum(serialize_all = "lowercase")]
enum DeploymentKind {
    Indexer,
    Matcher,
    Notifier,
}

impl DeploymentBuilder {
    /// Image sets the "image" for the resulting [`Deployment`].
    pub fn image<S>(self, image: S) -> Self
    where
        S: ToString,
    {
        Self {
            image: image.to_string(),
            ..self
        }
    }

    /// Config_source sets the "config source" for the resulting [`Deployment`].
    pub fn config_source(self, cfgsrc: &ConfigSource) -> Self {
        let cfgsrc = cfgsrc.clone();
        Self { cfgsrc, ..self }
    }
}

impl Build for DeploymentBuilder {
    type Output = Deployment;

    fn build(self) -> Self::Output {
        let labels = standard_labels(self.kind);
        let mut container = ContainerBuilder::from(&self).build();
        let mut volumes = make_volumes(&self.cfgsrc);
        if self.kind == DeploymentKind::Indexer {
            volumes.push(Volume {
                name: LAYER_VOLUME_NAME.to_string(),
                ephemeral: EphemeralVolumeSource {
                    volume_claim_template: PersistentVolumeClaimTemplate {
                        metadata: ObjectMeta {
                            ..Default::default()
                        }
                        .into(),
                        spec: PersistentVolumeClaimSpec {
                            access_modes: vec!["ReadWriteOnce".into()].into(),
                            resources: VolumeResourceRequirements {
                                requests: BTreeMap::from([(
                                    "storage".into(),
                                    Quantity("10Gi".into()),
                                )])
                                .into(),
                                ..Default::default()
                            }
                            .into(),
                            ..Default::default()
                        },
                    }
                    .into(),
                }
                .into(),
                ..Default::default()
            });

            container
                .volume_mounts
                .get_or_insert_default()
                .push(VolumeMount {
                    name: LAYER_VOLUME_NAME.to_string(),
                    mount_path: "/var/tmp".into(),
                    ..Default::default()
                });
        }

        Deployment {
            metadata: ObjectMeta {
                name: self.name.into(),
                namespace: self.namespace.into(),
                labels: labels.clone().into(),
                owner_references: vec![self.owner_ref].into(),
                ..Default::default()
            },
            spec: DeploymentSpec {
                revision_history_limit: 3.into(),
                progress_deadline_seconds: 60.into(),
                strategy: DeploymentStrategy {
                    type_: s("Recreate"),
                    ..Default::default()
                }
                .into(),
                selector: LabelSelector {
                    match_labels: labels.clone().into(),
                    ..Default::default()
                },
                template: PodTemplateSpec {
                    metadata: ObjectMeta {
                        labels: labels.clone().into(),
                        ..Default::default()
                    }
                    .into(),
                    spec: PodSpec {
                        termination_grace_period_seconds: 10.into(),
                        share_process_namespace: true.into(),
                        security_context: PodSecurityContext {
                            run_as_user: 65532.into(),
                            ..Default::default()
                        }
                        .into(),
                        containers: vec![container],
                        volumes: volumes.into(),
                        ..Default::default()
                    }
                    .into(),
                },
                ..Default::default()
            }
            .into(),
            status: None,
        }
    }
}

/// ContainerBuilder is a helper to build Container objects for kubernetes.
struct ContainerBuilder {
    kind: ContainerKind,
    image: String,
    cfgsrc: ConfigSource,
    argv: Option<Vec<String>>,
}

/// This is a macro to write the repetitive [`From`] implementations.
macro_rules! from_impls_container{
    ($($from:ty),+) => {
        $(
        impl From<&$from> for ContainerBuilder {
            fn from(value: &$from) -> Self {
                let kind = value.kind.into();
                let image = value.image.clone();
                let cfgsrc = value.cfgsrc.clone();

                Self {
                    kind,
                    image,
                    cfgsrc,
                    argv: None,
                }
            }
        }
        )+
    };
}
from_impls_container!(DeploymentBuilder, JobBuilder, CronJobBuilder);

impl ContainerBuilder {
    /// Set the arguments for the Container.
    fn args<V>(self, argv: V) -> Self
    where
        V: IntoIterator,
        V::Item: ToString,
    {
        Self {
            argv: argv
                .into_iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .into(),
            ..self
        }
    }
}

impl Build for ContainerBuilder {
    type Output = Container;

    fn build(self) -> Container {
        let cfgsrc = &self.cfgsrc;

        let mut c = Container {
            name: "clair".into(),
            image: self.image.into(),
            args: self.argv,
            command: vec!["clair".into()].into(),
            working_dir: s("/run/clair"),
            security_context: SecurityContext {
                allow_privilege_escalation: false.into(),
                ..Default::default()
            }
            .into(),
            env: vec![EnvVar {
                name: "CLAIR_CONF".into(),
                value: s(CONFIG_FILENAME),
                ..Default::default()
            }]
            .into(),
            volume_mounts: vec![
                VolumeMount {
                    name: CONFIG_ROOT_VOLUME_NAME.into(),
                    mount_path: CONFIG_FILENAME.into(),
                    sub_path: s(&cfgsrc.root.key),
                    ..Default::default()
                },
                VolumeMount {
                    name: CONFIG_DROPIN_VOLUME_NAME.into(),
                    mount_path: format!("{CONFIG_FILENAME}.d"),
                    ..Default::default()
                },
            ]
            .into(),
            ports: vec![ContainerPort {
                name: s("introspection"),
                container_port: 8089,
                ..Default::default()
            }]
            .into(),
            resources: ResourceRequirements {
                requests: BTreeMap::from([("cpu".into(), Quantity("1".into()))]).into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };

        // Modify environment:
        match self.kind {
            ContainerKind::Indexer
            | ContainerKind::Matcher
            | ContainerKind::Notifier
            | ContainerKind::Updater => {
                c.env.get_or_insert_default().push(EnvVar {
                    name: "CLAIR_MODE".into(),
                    value: s(self.kind),
                    ..Default::default()
                });
            }
            _ => {}
        };

        // Modify ports:
        match self.kind {
            ContainerKind::Indexer | ContainerKind::Matcher | ContainerKind::Notifier => {
                c.ports.get_or_insert_default().push(ContainerPort {
                    name: s("api"),
                    container_port: 6060,
                    ..Default::default()
                });
                c.startup_probe = Probe {
                    tcp_socket: TCPSocketAction {
                        port: IntOrString::String("api".into()),
                        ..Default::default()
                    }
                    .into(),
                    initial_delay_seconds: 5.into(),
                    period_seconds: 1.into(),
                    ..Default::default()
                }
                .into();
                c.liveness_probe = Probe {
                    http_get: HTTPGetAction {
                        port: IntOrString::String("introspection".into()),
                        path: s("/healthz"),
                        ..Default::default()
                    }
                    .into(),
                    initial_delay_seconds: 15.into(),
                    period_seconds: 20.into(),
                    ..Default::default()
                }
                .into();
                c.readiness_probe = Probe {
                    http_get: HTTPGetAction {
                        port: IntOrString::String("introspection".into()),
                        path: s("/readyz"),
                        ..Default::default()
                    }
                    .into(),
                    initial_delay_seconds: 5.into(),
                    period_seconds: 10.into(),
                    ..Default::default()
                }
                .into();
            }
            ContainerKind::AdminPre | ContainerKind::AdminPost => {
                c.ports = None;
            }
            _ => {}
        }

        // Modify container command:
        match self.kind {
            ContainerKind::AdminPre => {
                c.command = ["clairctl", "admin", "pre"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
                    .into();
            }
            ContainerKind::AdminPost => {
                c.command = ["clairctl", "admin", "post"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
                    .into();
            }
            _ => {}
        };

        c
    }
}

/// ContainerKind enumerates all the ways a Clair container is used.
#[derive(Clone, Copy, strum::Display, strum::EnumString, strum::AsRefStr)]
#[strum(serialize_all = "lowercase")]
enum ContainerKind {
    /// Container used as an indexer.
    Indexer,
    /// Container used as an matcher.
    Matcher,
    /// Container used as an notifier.
    Notifier,
    /// Container used as an updater.
    Updater,
    /// Container used to run `admin pre`.
    AdminPre,
    /// Container used to run `admin post`.
    AdminPost,
}

impl From<DeploymentKind> for ContainerKind {
    fn from(value: DeploymentKind) -> Self {
        match value {
            DeploymentKind::Indexer => ContainerKind::Indexer,
            DeploymentKind::Matcher => ContainerKind::Matcher,
            DeploymentKind::Notifier => ContainerKind::Notifier,
        }
    }
}
impl From<JobKind> for ContainerKind {
    fn from(value: JobKind) -> Self {
        match value {
            JobKind::AdminPre => ContainerKind::AdminPre,
            JobKind::AdminPost => ContainerKind::AdminPost,
        }
    }
}

/// ConfigSourceBuilder is a builder for a [`ConfigSource`] based on the [`ConfigMap`] used to
/// create it.
pub struct ConfigSourceBuilder {
    name: String,
    key: String,
    root_dropins: Vec<DropinSelector>,
    ext_dropins: Option<Vec<DropinSelector>>,
}

impl TryFrom<&ConfigMap> for ConfigSourceBuilder {
    type Error = Error;

    fn try_from(value: &ConfigMap) -> Result<Self, Self::Error> {
        let name = value.name_unchecked();
        let root_dropins = value
            .data
            .as_ref()
            .map(|kv| {
                kv.keys()
                    .map(|key| DropinSelector {
                        type_: DropinType::ConfigMap,
                        name: name.clone(),
                        key: key.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            name,
            key: CONFIG_ROOT_KEY.to_string(),
            root_dropins,
            ext_dropins: None,
        })
    }
}

impl ConfigSourceBuilder {
    /// Set the key containing the root the Clair configuration in the original [`ConfigMap`].
    pub fn with_key<S: ToString>(self, key: S) -> Self {
        Self {
            key: key.to_string(),
            ..self
        }
    }

    /// Add additional configuration dropins for the resulting [`ConfigSource`].
    ///
    /// Repeated calls are additive.
    pub fn with_dropins<D: Iterator<Item = DropinSelector>>(self, dropins: D) -> Self {
        let mut next = self.ext_dropins.unwrap_or_default();
            next.extend(dropins);
            next.sort();
            next.dedup();
        Self {
            ext_dropins: if next.len() == 0{None} else {Some(next)},
            ..self
        }
    }
}

impl Build for ConfigSourceBuilder {
    type Output = ConfigSource;

    fn build(self) -> Self::Output {
        let root = DropinSelector::config_map(&self.name, &self.key);
        let mut dropins: Vec<_> = self
            .root_dropins
            .into_iter()
            .chain(self.ext_dropins.into_iter().flatten())
            .filter(|d| d != &root)
            .collect();
        dropins.sort();
        dropins.dedup();

        use api::v1alpha1::ConfigMapKeySelector;
        let root = ConfigMapKeySelector {
            name: self.name,
            key: self.key,
        };

        ConfigSource { root, dropins }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    use assert_json_diff::assert_json_eq;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, from_str, to_value};
    use simple_txtar::Archive;

    type Result = std::result::Result<(), Box<dyn std::error::Error>>;

    fn load_fixure<K>(modpath: &str, name: &str) -> (K, Value)
    where
        K: DeserializeOwned,
    {
        let prefix = modpath
            .rsplit_once("::")
            .map(|(pre, suf)| if suf != "" { suf } else { pre })
            .unwrap();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(prefix)
            .join(format!("{name}.txtar"));
        let path = path.as_path().to_str().expect("programmer error");
        eprintln!("looking for: {path}");
        let ar = Archive::from_file(path).expect("unable to load txtar");

        let v: K = {
            let f = ar.get("input.json").expect("malformed txtar");
            from_str(&f.content).expect("bad json")
        };
        let want: Value = {
            let f = ar.get("want.json").expect("malformed txtar");
            from_str(&f.content).expect("bad json")
        };
        (v, want)
    }

    #[test]
    fn config_source() -> Result {
        let (cm, want) = load_fixure::<ConfigMap>(module_path!(), "config_source");
        let got = ConfigSourceBuilder::try_from(&cm)?.build();
        let got = to_value(got)?;

        assert_json_eq!(got, want);
        Ok(())
    }

    #[cfg(test)]
    mod clair {
        use super::*;

        use api::v1alpha1::Clair;

        macro_rules! job_testcase {
            ($($name:ident),+) => {
                $(
                #[test]
                fn $name() -> Result {
                    let fixture = format!("job_{}", stringify!($name));
                    let (clair, want) = load_fixure::<Clair>(module_path!(), &fixture);
                    let got = JobBuilder::$name(&clair)?.build();
                    let got = to_value(got)?;

                    assert_json_eq!(got, want);
                    Ok(())
                }
                )+
            };
        }
        job_testcase!(admin_pre, admin_post);

        macro_rules! cr_testcase{
            ($(($name:ident, $builder:ident)),+) => {
                $(
                #[test]
                fn $name() -> Result {
                    let fixture = stringify!($name).to_ascii_lowercase();
                    let (clair, want) = load_fixure::<Clair>(module_path!(), &fixture);
                    let got = $builder::try_from(&clair)?.build();
                    let got = to_value(got)?;

                    assert_json_eq!(got, want);
                    Ok(())
                }
                )+
            };
        }
        cr_testcase!(
            (indexer, IndexerBuilder),
            (matcher, MatcherBuilder),
            (notifier, NotifierBuilder),
            (configmap, ConfigMapBuilder)
        );
    }

    #[cfg(test)]
    mod indexer {
        use super::*;

        use api::v1alpha1::Indexer;

        #[test]
        fn deployment() -> Result {
            let (indexer, want) = load_fixure::<Indexer>(module_path!(), "deployment");
            let got = DeploymentBuilder::try_from(&indexer)?.build();
            let got = to_value(got)?;

            assert_json_eq!(got, want);
            Ok(())
        }

        #[test]
        fn service() -> Result {
            let (indexer, want) = load_fixure::<Indexer>(module_path!(), "service");
            let got = ServiceBuilder::try_from(&indexer)?.build();
            let got = to_value(got)?;

            assert_json_eq!(got, want);
            Ok(())
        }
        #[test]
        fn horizontal_pod_autoscaler() -> Result {
            let (indexer, want) = load_fixure::<Indexer>(module_path!(), "horizontalpodautoscaler");
            let got = HorizontalPodAutoscalerBuilder::try_from(&indexer)?.build();
            let got = to_value(got)?;

            assert_json_eq!(got, want);
            Ok(())
        }

        #[test]
        fn dropin() {
            let mut srv: Service = from_str(r#"{"metadata":{"name":"test-indexer"}}"#).unwrap();
            srv.metadata.namespace = Some("test".into());
            let got = render_dropin::<Indexer>(&srv).unwrap();
            let got: Value = from_str(&got).unwrap();
            let want = json!([
              { "op": "add", "path": "/matcher/indexer_addr",  "value": "test-indexer.test.svc.cluster.local" },
              { "op": "add", "path": "/notifier/indexer_addr", "value": "test-indexer.test.svc.cluster.local" },
            ]);

            assert_json_eq!(got, want);
        }
    }
}
