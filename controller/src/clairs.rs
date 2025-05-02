//! Clairs holds the controller for the "Clair" CRD.

use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock},
};

use k8s_openapi::{api::core::v1::TypedLocalObjectReference, merge_strategies};
use kube::{
    api::{Api, Patch},
    client::Client,
    core::{GroupVersionKind, ObjectMeta},
    runtime::controller::Error as CtrlErr,
};
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, prelude::*, COMPONENT_LABEL, DEFAULT_CONFIG_JSON};
use clair_templates::{Build, IndexerBuilder, JobBuilder, MatcherBuilder, NotifierBuilder};
use v1alpha1::Clair;

static COMPONENT: LazyLock<String> = LazyLock::new(|| Clair::kind(&()).to_ascii_lowercase());
static SELF_GVK: LazyLock<GroupVersionKind> = LazyLock::new(|| GroupVersionKind {
    group: Clair::group(&()).to_string(),
    version: Clair::version(&()).to_string(),
    kind: Clair::kind(&()).to_string(),
});

/// Controller is the Clair controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let root: Api<v1alpha1::Clair> = Api::all(client.clone());
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    Ok(async move {
        let ctl = Controller::new(root, ctlcfg.clone())
            .owns(
                Api::<v1alpha1::Indexer>::all(client.clone()),
                ctlcfg.clone(),
            )
            .owns(
                Api::<v1alpha1::Matcher>::all(client.clone()),
                ctlcfg.clone(),
            )
            .owns(
                Api::<v1alpha1::Notifier>::all(client.clone()),
                ctlcfg.clone(),
            )
            .owns(Api::<core::v1::Secret>::all(client.clone()), ctlcfg.clone())
            .owns(
                Api::<core::v1::ConfigMap>::all(client.clone()),
                ctlcfg.clone(),
            )
            .owns(Api::<batch::v1::Job>::all(client.clone()), ctlcfg.clone())
            .reconcile_all_on(sig)
            .graceful_shutdown_on(cancel.cancelled_owned());
        info!("starting clair controller");

        if !ctx.gvk_exists(&SELF_GVK).await {
            error!("CRD is not queryable ({SELF_GVK:?}); is the CRD installed?");
            return Err(Error::BadName("no CRD".into()));
        }

        ctl.run(reconcile, error_policy, ctx)
            .for_each(|ret| {
                if let Err(err) = ret {
                    match err {
                        CtrlErr::ObjectNotFound(objref) => error!(%objref, "object not found"),
                        CtrlErr::ReconcilerFailed(error, objref) => {
                            error!(%objref, %error, "reconcile error")
                        }
                        CtrlErr::QueueError(error) => error!(%error, "queue error"),
                        CtrlErr::RunnerError(error) => error!(%error, "runner error"),
                    };
                }
                futures::future::ready(())
            })
            .await;
        debug!("clair controller finished");
        Ok(())
    }
    .boxed())
}

fn error_policy(obj: Arc<v1alpha1::Clair>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::requeue(Duration::from_secs(5))
}

#[instrument(skip(ctx, clair),fields(
    name = clair.name_any(),
    namespace = clair.namespace().unwrap(),
    generation = clair.metadata.generation,
    resource_version = clair.metadata.resource_version
))]
async fn reconcile(clair: Arc<Clair>, ctx: Arc<Context>) -> Result<Action> {
    info!("reconciling Clair");
    let r = Reconciler::from((clair.clone(), ctx.clone()));

    for (field, present) in [
        ("$.spec.databases", clair.spec.databases.is_some()),
        ("$.spec.image", clair.spec.image.is_some()),
    ] {
        if !present {
            info!(field, "missing required field, skipping reconciliation");
            return Ok(Action::await_change());
        }
    }

    r.configuration().await?;
    r.admin_pre().await?;
    r.indexer().await?;
    r.matcher().await?;
    r.notifier().await?;
    r.admin_post().await?;

    Ok(DEFAULT_REQUEUE.clone())
}

#[derive(Debug)]
struct Reconciler {
    clair: Arc<Clair>,
    ctx: Arc<Context>,
    namespace: String,
    api: Api<Clair>,
}

impl From<(Arc<Clair>, Arc<Context>)> for Reconciler {
    fn from(value: (Arc<Clair>, Arc<Context>)) -> Self {
        let (clair, ctx) = value;
        let namespace = clair.namespace().unwrap(); // Clair is namespace scoped
        let api: Api<Clair> = Api::namespaced(ctx.client.clone(), &namespace);
        Self {
            clair,
            ctx,
            namespace,
            api,
        }
    }
}

impl Reconciler {
    fn client(&self) -> Client {
        self.ctx.client.clone()
    }
    fn ns(&self) -> &str {
        self.namespace.as_str()
    }
    fn name(&self) -> String {
        self.clair.name_unchecked()
    }

    #[instrument(skip(self), ret)]
    async fn configuration(&self) -> Result<()> {
        use self::core::v1::ConfigMap;
        let api = Api::<ConfigMap>::namespaced(self.client(), self.ns());

        let owner_ref = self.clair.owner_ref(&()).unwrap();
        let mut cm = ConfigMap {
            metadata: ObjectMeta {
                name: Some(self.name()),
                namespace: Some(self.namespace.clone()),
                owner_references: Some(vec![owner_ref]),
                labels: Some(BTreeMap::from([(
                    COMPONENT_LABEL.to_string(),
                    COMPONENT.to_string(),
                )])),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut contents = BTreeMap::new();
        let mut created_dropins = Vec::new();

        contents.insert("config.json".to_string(), DEFAULT_CONFIG_JSON.into());
        if let Some(ref status) = self.clair.status {
            // For all of the resources owned by this Clair instance, see if they've published a
            // dropin snippet to their status resource.
            //
            // If so, pull it into the ConfigMap managed by this Clair instance and put a reference
            // in the main ConfigSource.
            let to_check = [status.indexer.as_ref(), status.matcher.as_ref()];
            for objref in to_check.into_iter().flatten() {
                let kind = objref.kind.as_str().to_ascii_lowercase();
                debug!(kind, "checking created object");
                let dropin = match kind.as_str() {
                    "indexer" => Api::<v1alpha1::Indexer>::namespaced(self.client(), self.ns())
                        .get_status(&objref.name)
                        .instrument(debug_span!("get_status", kind))
                        .await
                        .ok()
                        .and_then(|obj| obj.status),
                    "matcher" => Api::<v1alpha1::Matcher>::namespaced(self.client(), self.ns())
                        .get_status(&objref.name)
                        .instrument(debug_span!("get_status", kind))
                        .await
                        .ok()
                        .and_then(|obj| obj.status),
                    _ => unreachable!(),
                }
                .and_then(|s| s.dropin);

                debug!(kind, found = dropin.is_some(), "checking dropin");
                if let Some(dropin) = dropin {
                    let key = format!("00-{kind}.json-patch");
                    contents.insert(key.clone(), dropin);
                    created_dropins.push(v1alpha1::DropinSource {
                        config_map_key_ref: Some(v1alpha1::ConfigMapKeySelector {
                            name: cm.name_any(),
                            key,
                        }),
                        ..Default::default()
                    });
                }
            }
        }
        cm.data = Some(contents);

        let cm = api
            .patch(&cm.name_any(), &PATCH_PARAMS, &Patch::Apply(cm))
            .instrument(debug_span!("patch", kind = "ConfigMap"))
            .await?;
        info!(
            config_map.name = cm.metadata.name,
            config_map.generation = cm.metadata.generation,
            config_map.resource_version = cm.metadata.resource_version,
            "patched ConfigMap"
        );

        if let Some(dbs) = &self.clair.spec.databases {
            trace!("have databases");
            for &sec in &[&dbs.indexer, &dbs.matcher] {
                created_dropins.push(v1alpha1::DropinSource {
                    config_map_key_ref: None,
                    secret_key_ref: Some(sec.clone()),
                });
            }
            if let Some(ref sec) = dbs.notifier {
                trace!("have notifier database");
                created_dropins.push(v1alpha1::DropinSource {
                    config_map_key_ref: None,
                    secret_key_ref: Some(sec.clone()),
                });
            };
        }
        trace!(?created_dropins, "created dropins");

        let mut dropins = self.clair.spec.dropins.clone();
        merge_strategies::list::set(&mut dropins, created_dropins);
        let config = v1alpha1::ConfigSource {
            root: v1alpha1::ConfigMapKeySelector {
                name: cm.name_any(),
                key: "config.json".into(),
            },
            dropins,
        };
        trace!(config_source=?config, "created ConfigSource");
        if self
            .clair
            .status
            .as_ref()
            .and_then(|s| s.config.as_ref())
            .map(|c| c == &config)
            .unwrap_or_default()
        {
            debug!("no need to update status");
            return Ok(());
        }
        debug!("updating status");

        let mut next = self
            .api
            .get_status(&self.name())
            .instrument(debug_span!("get_status"))
            .await?;
        {
            next.meta_mut().managed_fields = None;
            let status = next.status.get_or_insert_default();

            let update = status.config.is_some();
            status.config = config.into();

            let cnds = status.conditions.get_or_insert_default();
            let type_ = clair_condition("ConfigReady");
            let mut cnd = Condition {
                message: "created ConfigSource object".into(),
                observed_generation: self.clair.metadata.generation,
                last_transition_time: meta::v1::Time(Utc::now()),
                reason: "ConfigSourceCreated".into(),
                status: "True".into(),
                type_: type_.clone(),
            };
            if update {
                cnd.reason = "ConfigSourceUpdated".into();
                cnd.message = "updated ConfigSource object".into();
            }
            match cnds
                .iter_mut()
                .find(|cnd| cnd.type_.as_str() == type_.as_str())
            {
                None => cnds.push(cnd),
                Some(e) => *e = cnd,
            };
        }
        debug!(patch = ?next, "applying patch");
        self.api
            .patch_status(&self.clair.name_any(), &PATCH_PARAMS, &Patch::Apply(&next))
            .await?;
        Ok(())
    }

    /// The admin_pre step is responsible for arranging for the admin pre-upgrade jobs to run and
    /// for "promoting" the version.
    #[instrument(skip(self), ret)]
    async fn admin_pre(&self) -> Result<()> {
        use batch::v1::Job;
        let job_type = clair_condition("AdminPreJobDone");
        let mut update = vec![];
        let mut promote = false;
        let cnds = self
            .clair
            .status
            .as_ref()
            .and_then(|s| s.conditions.clone())
            .unwrap_or_default();
        let api = Api::<Job>::namespaced(self.client(), self.ns());

        // If there are no conditions, record the Job as done and continue.
        //
        // If there are conditions, check in order:
        // - If the PreJob condition is not current to the spec:
        //   - Check on the current image:
        //     - If changed, start a the new job and set the condtion to False.
        // - If the PreJob condition is current to the spec:
        //   - If false, check on the job and update if need be.
        //   - If true, swap the new image into the status.

        if let Some(cnd) = cnds.iter().find(|&c| c.type_ == job_type) {
            debug!("checking Condition");
            if cnd.observed_generation != self.clair.metadata.generation {
                debug!(
                    observed = cnd.observed_generation,
                    current = self.clair.metadata.generation,
                    "generation differs"
                );
                if self.clair.spec.image.as_ref()
                    == self.clair.status.as_ref().and_then(|s| s.image.as_ref())
                {
                    debug!("\"spec.image\" not changed");
                    update.push(Condition {
                        message: "spec.image not changed".into(),
                        observed_generation: self.clair.metadata.generation,
                        last_transition_time: meta::v1::Time(Utc::now()),
                        reason: "NoImageUpdate".into(),
                        status: "True".into(),
                        type_: job_type,
                    });
                } else {
                    debug!("starting \"admin pre\" job");
                    update.push(Condition {
                        message: "spec.image changed, launching \"admin pre\" job".into(),
                        observed_generation: self.clair.metadata.generation,
                        last_transition_time: meta::v1::Time(Utc::now()),
                        reason: "ImageUpdated".into(),
                        status: "False".into(),
                        type_: job_type,
                    });
                    info!(TODO = true, "launch job");

                    let j = JobBuilder::admin_pre(self.clair.as_ref())?.build();
                    api.create(&CREATE_PARAMS, &j)
                        .instrument(debug_span!("create"))
                        .await?;
                }
            } else {
                debug!("checking ");
                match cnd.status.as_str() {
                    "False" => {
                        info!(TODO = true, "check job");
                    }
                    "True" => {
                        if self.clair.spec.image.as_ref()
                            != self.clair.status.as_ref().and_then(|s| s.image.as_ref())
                        {
                            debug!("promoting image");
                            promote = true;
                        }
                    }
                    "Unknown" => {
                        error!(condition = job_type, "job in unknown state???");
                        return Ok(());
                    }
                    _ => unreachable!(),
                }
            }
        } else {
            debug!("fresh instance, skipping \"admin pre\" job");
            promote = true;
            update.push(Condition {
                message: "pre jobs are not needed on a fresh system".into(),
                observed_generation: self.clair.metadata.generation,
                last_transition_time: meta::v1::Time(Utc::now()),
                reason: "NewClair".into(),
                status: "True".into(),
                type_: job_type,
            });
        }

        if !update.is_empty() {
            let next = self
                .api
                .get_status(&self.name())
                .instrument(debug_span!("get_status"))
                .await
                .map(|mut next| {
                    next.meta_mut().managed_fields = None;
                    let status = next.status.get_or_insert_default();
                    if promote {
                        status.image = self.clair.spec.image.clone();
                    }
                    let cnds = status.conditions.get_or_insert_default();
                    merge_strategies::list::map(cnds, update, &[cmp_condition], merge_condition);
                    next
                })?;
            trace!("patching status");
            self.api
                .patch_status(&self.name(), &PATCH_PARAMS, &Patch::Apply(&next))
                .instrument(debug_span!("patch_status"))
                .await?;
        }

        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn admin_post(&self) -> Result<()> {
        info!(TODO = true, "write admin post job");
        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn indexer(&self) -> Result<()> {
        use v1alpha1::Indexer;

        let api = Api::<Indexer>::namespaced(self.client(), self.ns());
        let status = if let Some(status) = self.clair.status.as_ref() {
            status
        } else {
            info!("no status object, unable to create Indexer (missing ConfigSource)");
            return Ok(());
        };

        let idx = IndexerBuilder::try_from(self.clair.as_ref())?.build();

        trace!(?idx, "created Indexer");
        let idx = api
            .patch(&idx.name_any(), &PATCH_PARAMS, &Patch::Apply(idx))
            .instrument(debug_span!("patch", kind = "Indexer"))
            .await?;

        if status.indexer.is_some() {
            debug!("no need to update status");
            return Ok(());
        }
        debug!("updating status");

        let mut next = self
            .api
            .get_status(&self.name())
            .instrument(debug_span!("get_status"))
            .await?;
        {
            next.meta_mut().managed_fields = None;
            let status = next.status.get_or_insert_default();

            let mut cnd = Condition {
                message: "created Indexer object".into(),
                observed_generation: self.clair.metadata.generation,
                last_transition_time: meta::v1::Time(Utc::now()),
                reason: "IndexerCreated".into(),
                status: "True".into(),
                type_: clair_condition("IndexerCreated"),
            };
            if status.indexer.is_some() {
                cnd.message = "updated Indexer object".into();
                cnd.reason = "IndexerUpdated".into();
            }
            status.indexer = TypedLocalObjectReference {
                kind: Indexer::kind(&()).to_string(),
                api_group: Indexer::api_version(&()).to_string().into(),
                name: idx.name_any(),
            }
            .into();

            let cnds = status.conditions.get_or_insert_default();
            merge_strategies::list::map(cnds, vec![cnd], &[cmp_condition], merge_condition);
        }
        debug!(payload = ?next, "patching status");
        self.api
            .patch_status(&self.name(), &PATCH_PARAMS, &Patch::Apply(&next))
            .instrument(debug_span!("patch_status"))
            .await?;

        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn matcher(&self) -> Result<()> {
        use v1alpha1::Matcher;

        let api = Api::<Matcher>::namespaced(self.client(), self.ns());
        let status = if let Some(status) = self.clair.status.as_ref() {
            status
        } else {
            info!("no status object, unable to create Matcher (missing ConfigSource)");
            return Ok(());
        };

        let m = MatcherBuilder::try_from(self.clair.as_ref())?.build();

        trace!(?m, "created Matcher");
        let m = api
            .patch(&m.name_any(), &PATCH_PARAMS, &Patch::Apply(m))
            .instrument(debug_span!("patch", kind = "Matcher"))
            .await?;
        if status.matcher.is_some() {
            return Ok(());
        }
        debug!("updating status");

        let mut next = self
            .api
            .get_status(&self.name())
            .instrument(debug_span!("get_status"))
            .await?;
        {
            next.meta_mut().managed_fields = None;
            let status = next.status.get_or_insert_default();

            status.matcher = TypedLocalObjectReference {
                kind: Matcher::kind(&()).to_string(),
                api_group: Matcher::api_version(&()).to_string().into(),
                name: m.name_any(),
            }
            .into();

            let cnds = status.conditions.get_or_insert_default();
            let type_ = clair_condition("MatcherCreated");
            let cnd = Condition {
                message: "created Matcher object".into(),
                observed_generation: self.clair.metadata.generation,
                last_transition_time: meta::v1::Time(Utc::now()),
                reason: "MatcherPatched".into(),
                status: "True".into(),
                type_: type_.clone(),
            };
            match cnds
                .iter_mut()
                .find(|cnd| cnd.type_.as_str() == type_.as_str())
            {
                None => cnds.push(cnd),
                Some(e) => *e = cnd,
            };
        }
        self.api
            .patch_status(&self.clair.name_any(), &PATCH_PARAMS, &Patch::Apply(&next))
            .instrument(debug_span!("patch_status"))
            .await?;
        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn notifier(&self) -> Result<()> {
        use v1alpha1::Notifier;

        if !self.clair.spec.notifier.unwrap_or_default() {
            debug!("Notifier not asked for, skipping");
            return Ok(());
        }

        let api = Api::<Notifier>::namespaced(self.client(), self.ns());
        let status = if let Some(status) = self.clair.status.as_ref() {
            status
        } else {
            info!("no status object, unable to create Notifier (missing ConfigSource)");
            return Ok(());
        };

        let n = NotifierBuilder::try_from(self.clair.as_ref())?.build();

        trace!(?n, "created Notifier");
        let n = api
            .patch(&n.name_any(), &PATCH_PARAMS, &Patch::Apply(n))
            .instrument(debug_span!("patch", kind = "Notifier"))
            .await?;
        if status.notifier.is_some() {
            return Ok(());
        }
        debug!("updating status");

        let mut next = self
            .api
            .get_status(&self.name())
            .instrument(debug_span!("get_status"))
            .await?;
        {
            next.meta_mut().managed_fields = None;
            let status = next.status.get_or_insert_default();

            status.notifier = TypedLocalObjectReference {
                kind: Notifier::kind(&()).to_string(),
                api_group: Notifier::api_version(&()).to_string().into(),
                name: n.name_any(),
            }
            .into();

            let cnds = status.conditions.get_or_insert_default();
            let type_ = clair_condition("NotifierCreated");
            let cnd = Condition {
                message: "created Notifier object".into(),
                observed_generation: self.clair.metadata.generation,
                last_transition_time: meta::v1::Time(Utc::now()),
                reason: "NotifierPatched".into(),
                status: "True".into(),
                type_: type_.clone(),
            };
            match cnds
                .iter_mut()
                .find(|cnd| cnd.type_.as_str() == type_.as_str())
            {
                None => cnds.push(cnd),
                Some(e) => *e = cnd,
            };
        }
        self.api
            .patch_status(&self.clair.name_any(), &PATCH_PARAMS, &Patch::Apply(&next))
            .instrument(debug_span!("patch_status"))
            .await?;

        Ok(())
    }
}

fn cmp_condition(a: &Condition, b: &Condition) -> bool {
    a.type_.as_str() == b.type_.as_str()
}
fn merge_condition(to: &mut Condition, from: Condition) {
    to.last_transition_time = from.last_transition_time;
    if let Some(g) = from.observed_generation {
        to.observed_generation = Some(g);
    }
    if !from.message.is_empty() {
        to.message = from.message;
    }
    if !from.reason.is_empty() {
        to.reason = from.reason;
    }
    if !from.status.is_empty() {
        to.status = from.status;
    }
}

/*
/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "doc-controller".into(),
        }
    }
}
impl Diagnostics {
    fn recorder(&self, client: Client) -> Recorder {
        Recorder::new(client, self.reporter.clone())
    }
}
*/
