#![warn(rustdoc::missing_crate_level_docs)]
#![warn(missing_docs)]

//! Controller implements common functionality for the controller binary and controller functions
//! themselves.

use std::{collections::HashMap, env, pin::Pin, sync::LazyLock};

use chrono::Utc;
use futures::Future;
use k8s_openapi::{api::core, apimachinery::pkg::apis::meta};
use kube::{api::GroupVersionKind, runtime::events};
use regex::Regex;
use tokio::sync::RwLock;
use tracing::{error, info, instrument, trace, warn};

use api::v1alpha1;

/// Prelude is the common types for CRD controllers.
pub(crate) mod prelude {
    #![allow(unused_imports)]
    pub use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

    pub use chrono::Utc;
    pub use futures::prelude::*;
    pub use k8s_openapi::{
        api::*,
        apimachinery::pkg::apis::meta::{self, v1::Condition},
    };
    pub use kube::{
        self,
        api::{
            entry::{CommitError, Entry},
            Api, PatchParams, PostParams,
        },
        runtime::{
            controller::{Action, Controller},
            events::{Event, EventType, Recorder, Reporter},
            watcher,
        },
        Resource, ResourceExt,
    };
    pub use tokio_util::sync::CancellationToken;
    pub use tracing::{debug, error, info, instrument, trace, warn};

    pub use api::v1alpha1;

    pub use super::templates;
    pub use super::{make_volumes, new_templated};
    pub use super::{Context, ControllerFuture, Error, Request, Result};
    pub use super::{CONTROLLER_NAME, CREATE_PARAMS, DEFAULT_REQUEUE, PATCH_PARAMS};
}

pub mod clairs;
pub mod indexers;
//pub mod matchers;
//pub mod subresource;
//mod worker;

pub mod templates;
pub mod updaters;
pub mod webhook;

// NB The docs are unclear, but backtraces are unsupported on stable.
/// Error ...
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// TracingConfig indicates the error came from the tracing setup.
    #[error("tracing_subscriber error: {0}")]
    TracingConfig(#[from] tracing_subscriber::filter::ParseError),
    /// Tracing indicates the error came from installing the tracing subsriber.
    #[error("tracing error: {0}")]
    Tracing(#[from] tracing::subscriber::SetGlobalDefaultError),
    /// Kube is a generic error from the `kube` crate.
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
    /// KubeConfig inidicates the process was unable to find a kubeconfig.
    #[error("kubeconfig error: {0}")]
    KubeConfig(#[from] kube::config::InferConfigError),
    /// Commit inidicates there was an error in a "create-or-get then modify" process.
    #[error("commit error: {0}")]
    Commit(#[from] kube::api::entry::CommitError),
    //#[error("kube error: {0}")]
    //KubeGV(#[from] kube::core::gvk::ParseGroupVersionError),
    /// Io inidicates some OS-level I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON inidicates a JSON serialization failed.
    #[error("json error: {0}")]
    JSON(#[from] serde_json::Error),
    #[error("yaml error: {0}")]
    /// YAML indicates a YAML serialization failed.
    YAML(#[from] serde_yaml::Error),
    /// JSONPatch indicates a JSON patch failed.
    #[error("json patch error: {0}")]
    JSONPatch(#[from] json_patch::PatchError),
    /// AddrParse inidicates  the provided string failed to pars into an address.
    #[error("parse error: {0}")]
    AddrParse(#[from] std::net::AddrParseError),
    /// Tokio inidicates an error starting tasks.
    #[error("tokio error: {0}")]
    Tokio(#[from] tokio::task::JoinError),
    /// TLS inidicates some TLS error.
    #[error("tls error: {0}")]
    TLS(#[from] openssl::ssl::Error),
    /// ...
    //#[error("webhook server error: {0}")]
    //Webhook(#[from] hyper::Error),

    /// MissingName inidcates a name was needed and not provided.
    #[error("missing name for kubernetes object: {0}")]
    MissingName(&'static str),
    /// BadName inidicates a disallowed name for a kubernetes object.
    #[error("bad name for kubernetes object: {0}")]
    BadName(String),
    /// Other is a catch-all error.
    #[error("some other error: {0}")]
    Other(#[from] anyhow::Error),
    /// Assets means there was an error loading an asset needed for a template.
    #[error("assets error: {0}")]
    Assets(String),
    /// Config means the Clair config validation process failed.
    #[error("clair config error: {0}")]
    Config(#[from] clair_config::Error),
}

/// Result typedef for controllers.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Context is common context for controllers.
pub struct Context {
    /// Client is a k8s client. This should be only ever be `clone()`'d out of the Context.
    pub client: kube::Client,
    /// Image is the fallback container image to use.
    pub image: String,
    /// ...
    kinds: RwLock<HashMap<GroupVersionKind, bool>>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ctx")
    }
}

impl Context {
    /// New creates a Context object.
    pub fn new<S>(client: kube::Client, image: S) -> Self
    where
        S: ToString,
    {
        let image = image.to_string();
        Self {
            client,
            image,
            kinds: RwLock::new(HashMap::new()),
        }
    }

    /// Gvk_exists reports if the supplied GroupVersionKind is known to exist in this cluster.
    ///
    /// This method may need to make requests to the API server.
    /// This method assumes that a successful response never changes. If a resource is added or
    /// removed from the cluster after this has returned, the process will need to be restarted to
    /// see it.
    pub async fn gvk_exists(&self, gvk: &GroupVersionKind) -> bool {
        use kube::discovery::oneshot;
        {
            let kinds = self.kinds.read().await;
            if let Some(&ok) = kinds.get(gvk) {
                return ok;
            }
        }

        // Do the lookup without locking the hashmap so that readers aren't blocked behind a
        // network request. At worst, we'll end up with a few extra API calls. The context can get
        // "pre-queried" if that's a concern.
        let lookup = oneshot::pinned_kind(&self.client, gvk).await;
        let exists = match lookup {
            Ok((_, _)) => true,
            Err(error) => {
                match error {
                    kube::Error::Discovery(error) => info!(%error, ?gvk, "GVK not available"),
                    _ => {
                        error!(%error, ?gvk, "api query error");
                        // return early so that we'll retry on the next lookup
                        return false;
                    }
                };
                false
            }
        };

        {
            let mut kinds = self.kinds.write().await;
            kinds.insert(gvk.clone(), exists);
        }
        exists
    }
}

/// Request is common per-request data for controllers.
pub struct Request {
    now: meta::v1::Time,
    recorder: events::Recorder,
}

impl Request {
    /// New constructs a Request for the current reconcile request.
    pub fn new(c: &kube::Client) -> Request {
        Request {
            now: meta::v1::Time(Utc::now()),
            recorder: events::Recorder::new(c.clone(), REPORTER.clone()),
        }
    }
    /// Now reports the "now" of this request.
    pub fn now(&self) -> meta::v1::Time {
        self.now.clone()
    }
    /// Publish publishes a kubernetes Event.
    pub async fn publish(
        &self,
        ev: &events::Event,
        reference: &core::v1::ObjectReference,
    ) -> Result<()> {
        Ok(self.recorder.publish(ev, reference).await?)
    }
}

/// ControllerFuture is the type the controller constructors should return.
pub type ControllerFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

static REPORTER: LazyLock<events::Reporter> = LazyLock::new(|| events::Reporter {
    controller: CONTROLLER_NAME.to_string(),
    instance: Some(
        env::var("CONTROLLER_POD_NAME")
            .unwrap_or_else(|_err| "weird-controller-environment".into()),
    ),
});

/// Condition is like [keyify], but does not force lower-case.
fn condition<S: ToString, K: AsRef<str>>(space: S, key: K) -> String {
    let mut out = space.to_string();
    out.push('/');
    key.as_ref()
        .chars()
        .map(|c| match c {
            '_' | ' ' | '\t' | '\n' => '-',
            _ => c,
        })
        .for_each(|c| out.push(c));
    out
}

/// Keyify sanitizes the key for use in k8s metadata.
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

/// Clair_condition returns the provided argument as a name in the clair-controller's space,
/// sutable for use as a condition type.
pub fn clair_condition<S: AsRef<str>>(s: S) -> String {
    condition(api::GROUP, s)
}

/// Clair_label returns the provided argument as a name in the clair-controller's space, sutable
/// for use as an annotation or label.
pub fn clair_label<S: AsRef<str>>(s: S) -> String {
    keyify(api::GROUP, s)
}

/// K8s_label returns the provided argument as a name in the "app.kubernetes.io" space, sutable for
/// use as an annotation or label.
pub fn k8s_label<S: AsRef<str>>(s: S) -> String {
    keyify("app.kubernetes.io", s)
}

/// The semver regexp:
static SEMVER_REGEXP: LazyLock<Regex> = LazyLock::new(|| {
    const RE: &str = r#"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$"#;
    Regex::new(RE).expect("programmer error: bad static regexp")
});

/// Image_version returns the version for an image, if present.
///
/// Semver versions are the only accepted version strings.
pub fn image_version(img: &str) -> Option<&str> {
    img.split_once(':')
        .map(|(_, t)| t)
        .filter(|t| SEMVER_REGEXP.is_match(t))
}

/// New_templated returns a `K` with patches for `S` applied and the owner set to `obj`.
#[instrument(skip_all)]
pub async fn new_templated<S, K>(obj: &S, _ctx: &Context) -> Result<K>
where
    S: kube::Resource<DynamicType = ()>,
    K: kube::Resource<DynamicType = ()> + serde::de::DeserializeOwned,
{
    Ok(templates::render(obj))
    /*
    use kube::ResourceExt;
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    let okind = S::kind(&()).to_ascii_lowercase();

    let kind = K::kind(&()).to_ascii_lowercase();
    trace!(kind, owner = ?oref, "requesting template");
    let mut v: K = match crate::templates::resource_for(&okind).await {
        Ok(v) => v,
        Err(err) => return Err(Error::Assets(err.to_string())),
    };
    v.meta_mut().owner_references = Some(vec![oref]);
    v.meta_mut().name = Some(format!("{}-{okind}", obj.name_any()));

    Ok(v)
    */
}

/// Make_volumes generates the Volumes and VolumeMounts for the provided ConfigSource. The created
/// Volumes and VolumeMounts are returned, along with the created path of the root config.
#[instrument(skip_all)]
pub fn make_volumes(
    cfgsrc: &v1alpha1::ConfigSource,
) -> (Vec<core::v1::Volume>, Vec<core::v1::VolumeMount>, String) {
    use self::core::v1::{
        ConfigMapProjection, ConfigMapVolumeSource, KeyToPath, ProjectedVolumeSource,
        SecretProjection, Volume, VolumeMount, VolumeProjection,
    };
    let mut vols = Vec::new();
    let mut mounts = Vec::new();

    let root = String::from("/etc/clair/");
    let rootname = String::from("root-config");
    let filename = format!("{root}/{}", &cfgsrc.root.key);
    assert!(filename.ends_with(".json") || filename.ends_with(".yaml"));
    vols.push(Volume {
        name: rootname.clone(),
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
    });
    trace!(filename, "arranged for root config to be mounted");
    mounts.push(VolumeMount {
        name: rootname,
        mount_path: filename.clone(),
        sub_path: Some(cfgsrc.root.key.clone()),
        ..Default::default()
    });

    let mut proj = Vec::new();
    trace!(?cfgsrc.dropins, "dropins");
    for d in cfgsrc.dropins.iter() {
        assert!(d.config_map_key_ref.is_some() || d.secret_key_ref.is_some());
        trace!(?d.config_map_key_ref, ?d.secret_key_ref, "checking dropin");
        let mut v: VolumeProjection = Default::default();
        if let Some(cfgref) = d.config_map_key_ref.as_ref() {
            v.config_map = Some(ConfigMapProjection {
                name: cfgref.name.clone(),
                optional: Some(false),
                items: Some(vec![KeyToPath {
                    path: cfgref.key.clone(),
                    key: cfgref.key.clone(),
                    mode: None,
                }]),
            })
        } else if let Some(secref) = d.secret_key_ref.as_ref() {
            v.secret = Some(SecretProjection {
                name: secref.name.clone(),
                optional: Some(false),
                items: Some(vec![KeyToPath {
                    path: secref.key.clone(),
                    key: secref.key.clone(),
                    mode: None,
                }]),
            })
        } else {
            unreachable!()
        };
        proj.push(v);
    }
    trace!(?proj, "projected volumes");
    vols.push(Volume {
        name: "dropins".into(),
        projected: Some(ProjectedVolumeSource {
            sources: Some(proj),
            default_mode: Some(0o644),
        }),
        ..Default::default()
    });
    let mut cfg_d = filename.clone();
    cfg_d.push_str(".d");
    trace!(dir = cfg_d, "arranged for dropins to be mounted");
    mounts.push(VolumeMount {
        name: "dropins".into(),
        mount_path: cfg_d,
        ..Default::default()
    });

    (vols, mounts, filename)
}

/// Set_component_label sets the component label to `c`.
pub fn set_component_label(meta: &mut meta::v1::ObjectMeta, c: &str) {
    let mut l = meta.labels.take().unwrap_or_default();
    l.insert(COMPONENT_LABEL.to_string(), c.into());
    meta.labels.replace(l);
}

// Tricks to create the DEFAULT_IMAGE value:
#[cfg(debug_assertions)]
const DEFAULT_CONTAINER_TAG: &str = "nightly";
#[cfg(not(debug_assertions))]
const DEFAULT_CONTAINER_TAG: &str = "4.8.0";
const DEFAULT_CONTAINER_REPOSITORY: &str = "quay.io/projectquay/clair";

/// DEFAULT_IMAGE is the container image to use for Clair deployments if not specified in a
/// CRD.
///
/// The repository and tag components can be changed by providing the environment variables
/// `CONTAINER_REPOSITORY` or `CONTAINER_TAG` respectively at compile-time.
pub static DEFAULT_IMAGE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}:{}",
        option_env!("CONTAINER_REPOSITORY").unwrap_or(DEFAULT_CONTAINER_REPOSITORY),
        option_env!("CONTAINER_TAG").unwrap_or(DEFAULT_CONTAINER_TAG),
    )
});

/// DEFAULT_CONFIG_JSON is the JSON version of the default config.
pub static DEFAULT_CONFIG_JSON: &str = include_str!("default_config.json");

/// DEFAULT_CONFIG_YAML is the YAML version of the default config.
pub static DEFAULT_CONFIG_YAML: LazyLock<String> = LazyLock::new(|| {
    let v: serde_json::Value =
        serde_json::from_str(DEFAULT_CONFIG_JSON).expect("programmer error: bad config");
    serde_yaml::to_string(&v).expect("programmer error: bad config")
});

/// COMPONENT_LABEL is the well-know "component" label.
pub static COMPONENT_LABEL: LazyLock<String> = LazyLock::new(|| k8s_label("component"));

/// APP_NAME_LABEL is a label for Clair in the "app.kubernetes.io" space.
pub static APP_NAME_LABEL: LazyLock<String> = LazyLock::new(|| k8s_label("clair"));

/// DROPIN_LABEL is a label denoting which key in a ConfigMap is the managed dropin.
///
/// TODO(hank): This is actually an annotation.
pub static DROPIN_LABEL: LazyLock<String> = LazyLock::new(|| clair_label("dropin-key"));

/// CREATE_PARAMS is default post paramaters.
pub static CREATE_PARAMS: LazyLock<kube::api::PostParams> =
    LazyLock::new(|| kube::api::PostParams {
        dry_run: false,
        field_manager: Some(String::from(CONTROLLER_NAME)),
    });

/// PATCH_PARAMS is default patch paramaters.
pub static PATCH_PARAMS: LazyLock<kube::api::PatchParams> =
    LazyLock::new(|| kube::api::PatchParams::apply(CONTROLLER_NAME).validation_strict());

/// DEFAULT_REQUEUE is the default requeuing time for controllers.
pub static DEFAULT_REQUEUE: LazyLock<kube::runtime::controller::Action> = LazyLock::new(|| {
    kube::runtime::controller::Action::requeue(tokio::time::Duration::from_secs(60 * 60))
});

/// CONTROLLER_NAME is the name the controller uses whenever it needs a human-readable name.
pub const CONTROLLER_NAME: &str = "clair-controller";

/// GVK for `gateway.networking.k8s.io/v1/Gateway`.
pub static GATEWAY_NETWORKING_GATEWAY: LazyLock<GroupVersionKind> =
    LazyLock::new(|| GroupVersionKind::gvk("gateway.networking.k8s.io", "v1", "Gateway"));
/// GVK for `gateway.networking.k8s.io/v1/HTTPRoute`.
pub static GATEWAY_NETWORKING_HTTPROUTE: LazyLock<GroupVersionKind> =
    LazyLock::new(|| GroupVersionKind::gvk("gateway.networking.k8s.io", "v1", "HTTPRoute"));
/// GVK for `gateway.networking.k8s.io/v1/GRPCRoute`.
pub static GATEWAY_NETWORKING_GRPCROUTE: LazyLock<GroupVersionKind> =
    LazyLock::new(|| GroupVersionKind::gvk("gateway.networking.k8s.io", "v1", "GRPCRoute"));

/*
use futures::future;
use kube::discovery;
use tokio::time::{self, Duration, Instant, Interval};

pub struct Discovery {
    client: Client,
    d: discovery::Discovery,
    t: Interval,
}

impl Discovery {
    pub fn new(client: Client) -> Discovery {
        let t = time::interval_at(Instant::now(), Duration::from_secs(60 * 60 * 2));
        Discovery { client, d, t }
    }

    async fn client(&mut self) -> Result<(), kube::Error> {
        tokio::select! {
                    _ = self.t.tick() => {
                        let d = discovery::Discovery::new(self.client.clone()).filter(&[
                    "networking.k8s.io",
                    "batch",
                    "gateway.networking.k8s.io",
                ])
        .run().await?;
                self.d = d;
                    }
                    _= future::ready(()) => {}
                }
        Ok(())
    }

    pub fn get(&self, group: &str) -> Option<&discovery::ApiGroup> {
        None
    }
    pub fn has_group(&self, group: &str) -> bool {
        false
    }
    pub fn resolve_gvk(
        &self,
        gvk: &GroupVersionKind,
    ) -> Option<(discovery::ApiResource, discovery::ApiCapabilities)> {
        unimplemented!()
    }
}
*/
