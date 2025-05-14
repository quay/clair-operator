//! Clair_templates holds the templating logic for controllers.
//!
//! To create Kubernetes objects, use [TryFrom] on a reference to a `clairproject.org/v1alpha1`
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
use serde_json::json;

use api::v1alpha1::*;

/// DEFAULT_CONFIG is a minimal default configuration for a Clair system.
pub static DEFAULT_CONFIG: LazyLock<String> = LazyLock::new(|| {
    let v = json!({
      "http_listen_addr": ":6060",
      "introspection_addr": ":8089",
      "log_level": "info",
      "matcher":  { "migrations": true },
      "indexer":  { "migrations": true },
      "notifier": { "migrations": true },
      "metrics": {
        "name": "prometheus"
      },
    });
    serde_json::to_string(&v).expect("programmer error: static data")
});

const CONFIG_ROOT_VOLUME_NAME: &str = "root-config";
const CONFIG_DROPIN_VOLUME_NAME: &str = "dropin-config";
const CONFIG_FILENAME: &str = "/etc/clair/config.json";
const LAYER_VOLUME_NAME: &str = "layer-scratch";

/// Error is the error domain for creating templates.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("unable to determine namespace")]
    Namespace,
    #[error("missing required information: image")]
    MissingImage,
    #[error("missing required information: ConfigSource")]
    MissingConfigSource,
    #[error("unable to construct owner reference")]
    OwnerReference,
    #[error("parse error: {0}")]
    Parse(#[from] strum::ParseError),
    #[error("other error: {0}")]
    Other(&'static str),
}

pub fn render_dropin<O>(srv: &Service) -> Option<String>
where
    O: Resource<DynamicType = ()>,
{
    let name = srv.name_unchecked();
    let ns = srv.namespace().unwrap();
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

    serde_json::to_string(&v).ok()
}

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

fn make_volumes(cfgsrc: &ConfigSource) -> Vec<Volume> {
    enum Projection {
        ConfigMap(String, KeyToPath),
        Secret(String, KeyToPath),
    }

    let sources = cfgsrc
        .dropins
        .iter()
        .map(|d| {
            let p = match (d.config_map_key_ref.as_ref(), d.secret_key_ref.as_ref()) {
                (Some(cfgref), None) => Projection::ConfigMap(
                    cfgref.name.clone(),
                    KeyToPath {
                        path: cfgref.key.clone(),
                        key: cfgref.key.clone(),
                        mode: None,
                    },
                ),
                (None, Some(secref)) => Projection::Secret(
                    secref.name.clone(),
                    KeyToPath {
                        path: secref.key.clone(),
                        key: secref.key.clone(),
                        mode: None,
                    },
                ),
                (Some(_), Some(_)) => unreachable!(),
                (None, None) => unreachable!(),
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

pub trait Build {
    type Output;

    fn build(self) -> Self::Output;
}

// TODO(hank): These WorkerBuilder types can probably get consolidated with some judicious
// application of generics.

// TODO(hank): Most of these Builders could probably capture a reference to the "from" object and
// only clone things as needed during `build`.

// As a rule of thumb, `From` should be used for deriving new builders from local types, and
// `TryFrom` for deriving builders from "remote" types. That means the error checking only happens
// at the top-most interaction and doesn't have to get pushed all the way to a `build()` call.

/// IndexerBuilder constructs an `Indexer` from a `Clair`.
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
                concurrency_policy: "Forbid".to_string().into(),
                starting_deadline_seconds: 10.into(),
                time_zone: "Etc/UTC".to_string().into(),
                schedule: "0 */8 * * *".to_string(),
                job_template: JobTemplateSpec {
                    metadata: ObjectMeta {
                        labels: labels.clone().into(),
                        ..Default::default()
                    }
                    .into(),
                    spec: JobSpec {
                        active_deadline_seconds: 3600.into(),
                        completion_mode: "NonIndexed".to_string().into(),
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
    pub fn admin_pre(clair: &Clair) -> Result<Self, Error> {
        Self::new(clair, JobKind::AdminPre)
    }

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
                completion_mode: "NonIndexed".to_string().into(),
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

pub struct HorizontalPodAutoscalerBuilder {
    namespace: String,
    name: String,
    kind: HorizontalPodAutoscalerKind,
    owner_ref: OwnerReference,
}

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
                    api_version: "apps/v1".to_string().into(),
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

pub struct ServiceBuilder {
    namespace: String,
    name: String,
    kind: ServiceKind,
    owner_ref: OwnerReference,
}

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
    name: "api".to_string().into(),
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

pub struct HTTPRouteBuilder {
    namespace: String,
    name: String,
    kind: RouteKind,
    owner_ref: OwnerReference,
    service: Service,

    gateway: Option<RouteParentRef>,
}
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
            spec: HTTPRouteSpec {
                ..Default::default()
            },
            ..Default::default()
        };
        if self.gateway.is_none() {
            return r;
        }
        let gateway = self.gateway.unwrap();

        let parent_ref = HTTPRouteParentRefs {
            namespace: gateway.namespace.clone(),
            name: gateway.name.clone().unwrap_or_else(|| self.name.clone()),

            group: gateway.group.clone(),
            kind: gateway.kind.clone(),
            section_name: gateway.section_name.clone(),

            ..Default::default()
        };
        let rule = HTTPRouteRules {
            matches: vec![HTTPRouteRulesMatches::from(&self.kind)].into(),
            backend_refs: vec![HTTPRouteRulesBackendRefs {
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
            spec: HTTPRouteSpec {
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

impl From<&RouteKind> for gateway_networking_k8s_io::v1::httproutes::HTTPRouteRulesMatches {
    fn from(value: &RouteKind) -> Self {
        use gateway_networking_k8s_io::v1::httproutes::*;

        let prefix = match value {
            RouteKind::Indexer => "/indexer/api/v1/",
            RouteKind::Matcher => "/matcher/api/v1/",
            RouteKind::Notifier => "/notifier/api/v1/",
        }
        .to_string();

        HTTPRouteRulesMatches {
            path: HTTPRouteRulesMatchesPath {
                r#type: HTTPRouteRulesMatchesPathType::PathPrefix.into(),
                value: prefix.into(),
            }
            .into(),
            ..Default::default()
        }
    }
}

impl From<&RouteKind> for Vec<gateway_networking_k8s_io::v1::grpcroutes::GRPCRouteRulesMatches> {
    /// None, yet.
    fn from(_value: &RouteKind) -> Self {
        //use gateway_networking_k8s_io::v1::grpcroutes::*;
        vec![]
    }
}

pub struct DeploymentBuilder {
    namespace: String,
    name: String,
    kind: DeploymentKind,
    image: String,
    cfgsrc: ConfigSource,
    owner_ref: OwnerReference,
}
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
    pub fn image<S>(self, image: S) -> Self
    where
        S: ToString,
    {
        Self {
            image: image.to_string(),
            ..self
        }
    }
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
                    type_: "Recreate".to_string().into(),
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
            working_dir: "/run/clair".to_string().into(),
            security_context: SecurityContext {
                allow_privilege_escalation: false.into(),
                ..Default::default()
            }
            .into(),
            env: vec![EnvVar {
                name: "CLAIR_CONF".into(),
                value: CONFIG_FILENAME.to_string().into(),
                ..Default::default()
            }]
            .into(),
            volume_mounts: vec![
                VolumeMount {
                    name: CONFIG_ROOT_VOLUME_NAME.into(),
                    mount_path: CONFIG_FILENAME.into(),
                    sub_path: Some(cfgsrc.root.key.clone()),
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
                name: "introspection".to_string().into(),
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
                    value: self.kind.to_string().into(),
                    ..Default::default()
                });
            }
            _ => {}
        };

        // Modify ports:
        match self.kind {
            ContainerKind::Indexer | ContainerKind::Matcher | ContainerKind::Notifier => {
                c.ports.get_or_insert_default().push(ContainerPort {
                    name: "api".to_string().into(),
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
                        path: "/healthz".to_string().into(),
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
                        path: "/readyz".to_string().into(),
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

#[derive(Clone, Copy, strum::Display, strum::EnumString, strum::AsRefStr)]
#[strum(serialize_all = "lowercase")]
enum ContainerKind {
    Indexer,
    Matcher,
    Notifier,
    Updater,
    AdminPre,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    use assert_json_diff::assert_json_eq;
    use serde::de::DeserializeOwned;
    use serde_json::{from_str, to_value, Value};
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
            (notifier, NotifierBuilder)
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
