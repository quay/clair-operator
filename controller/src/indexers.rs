//! Indexers holds the controller for the "Indexer" CRD.
//!
//! ```mermaid
//! ```

use std::sync::{Arc, LazyLock};

use k8s_openapi::merge_strategies;
use kube::{
    ResourceExt,
    api::{Api, Patch},
    client::Client,
    core::GroupVersionKind,
    runtime::controller::Error as CtrlErr,
};
use serde_json::json;
use tokio::{
    signal::unix::{SignalKind, signal},
    time::Duration,
    try_join,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, cmp_condition, merge_condition, prelude::*};
use clair_templates::{
    Build, DeploymentBuilder, HorizontalPodAutoscalerBuilder, ServiceBuilder, render_dropin,
};
use v1alpha1::Indexer;

//static COMPONENT: LazyLock<String> = LazyLock::new(|| Indexer::kind(&()).to_ascii_lowercase());
static SELF_GVK: LazyLock<GroupVersionKind> = LazyLock::new(|| GroupVersionKind {
    group: Indexer::group(&()).to_string(),
    version: Indexer::version(&()).to_string(),
    kind: Indexer::kind(&()).to_string(),
});

/// Controller is the Indexer controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    use gateway_networking_k8s_io::v1::{grpcroutes::GRPCRoute, httproutes::HTTPRoute};

    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    Ok(async move {
        // Bail if the Indexer GVK isn't installed in the cluster.
        if !ctx.gvk_exists(&SELF_GVK).await {
            error!("CRD is not queryable ({SELF_GVK:?}); is the CRD installed?");
            return Err(Error::BadName("no CRD".into()));
        }
        info!("spawning indexer controller");

        // Set up the Controller for all the GVKs an Indexer owns.
        let mut ctl = Controller::new(Api::<Indexer>::all(client.clone()), ctlcfg.clone())
            .owns(
                Api::<apps::v1::Deployment>::all(client.clone()),
                ctlcfg.clone(),
            )
            .owns(
                Api::<autoscaling::v2::HorizontalPodAutoscaler>::all(client.clone()),
                ctlcfg.clone(),
            )
            .owns(
                Api::<core::v1::Service>::all(client.clone()),
                ctlcfg.clone(),
            );
        // Opportunisitically enable HTTP and gRPC support:
        if ctx.gvk_exists(&crate::GATEWAY_NETWORKING_HTTPROUTE).await {
            ctl = ctl.owns(Api::<HTTPRoute>::all(client.clone()), ctlcfg.clone());
        }
        if ctx.gvk_exists(&crate::GATEWAY_NETWORKING_GRPCROUTE).await {
            ctl = ctl.owns(Api::<GRPCRoute>::all(client.clone()), ctlcfg.clone());
        }
        // Finish set up.
        let ctl = ctl
            .reconcile_all_on(sig)
            .graceful_shutdown_on(cancel.cancelled_owned());

        // Run until the event stream closes.
        ctl.run(reconcile, handle_error, ctx)
            .for_each(|ret| {
                match ret {
                    Ok(_) => (),
                    Err(err) => match err {
                        CtrlErr::ObjectNotFound(objref) => error!(%objref, "object not found"),
                        CtrlErr::ReconcilerFailed(error, objref) => {
                            error!(%objref, %error, "reconcile error")
                        }
                        CtrlErr::QueueError(error) => error!(%error, "queue error"),
                        CtrlErr::RunnerError(error) => error!(%error, "runner error"),
                    },
                };
                futures::future::ready(())
            })
            .await;
        debug!("indexer controller finished");

        Ok(())
    }
    .boxed())
}

struct Reconciler {
    indexer: Arc<Indexer>,
    ctx: Arc<Context>,
    namespace: String,
    api: Api<Indexer>,
    recorder: Recorder,
}

impl From<(Arc<Indexer>, Arc<Context>)> for Reconciler {
    fn from(value: (Arc<Indexer>, Arc<Context>)) -> Self {
        use crate::REPORTER;

        let (indexer, ctx) = value;
        let namespace = indexer.namespace().unwrap(); // Indexer is namespace scoped
        let api: Api<Indexer> = Api::namespaced(ctx.client.clone(), &namespace);
        let recorder = Recorder::new(ctx.client.clone(), REPORTER.clone());
        Self {
            indexer,
            ctx,
            namespace,
            api,
            recorder,
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
        self.indexer.name_unchecked()
    }

    #[instrument(skip(self), ret)]
    async fn set_condition(&self, cnd: Condition) -> Result<()> {
        let mut next = self
            .api
            .get_status(&self.name())
            .instrument(debug_span!("get_status"))
            .await?;
        next.meta_mut().managed_fields = None;
        let status = next.status.get_or_insert_default();
        let cnds = status.conditions.get_or_insert_default();
        merge_strategies::list::map(cnds, vec![cnd], &[cmp_condition], merge_condition);
        debug!(payload = ?next, "patching status");
        self.api
            .patch_status(&self.name(), &PATCH_PARAMS, &Patch::Apply(&next))
            .instrument(debug_span!("patch_status"))
            .await?;
        Ok(())
    }

    /// Publish_dropin renders a Clair configuration dropin that points any Indexer URL fields to
    /// this Indexer's Service endpoint.
    ///
    /// The dropin is only created if the Indexer is owned by a Clair.
    #[instrument(skip(self), ret)]
    async fn publish_dropin(&self) -> Result<()> {
        use self::core::v1::Service;

        let owner = match self
            .indexer
            .owner_references()
            .iter()
            .find(|&r| r.controller.unwrap_or(false))
        {
            None => {
                trace!("not owned, skipping dropin generation");
                return Ok(());
            }
            Some(o) => o,
        };
        trace!(owner = owner.name, "indexer is owned");

        let s = ServiceBuilder::try_from(self.indexer.as_ref())?.build();
        let srv = Api::<Service>::namespaced(self.client(), self.ns())
            .get(&s.name_any())
            .await?;

        let status = v1alpha1::WorkerStatus {
            dropin: render_dropin::<Indexer>(&srv),
            ..Default::default()
        };
        let cur = self
            .api
            .patch_status(&self.name(), &PATCH_PARAMS, &Patch::Apply(&status))
            .instrument(debug_span!("patch_status"))
            .await
            .inspect_err(|error| error!(%error, "unable to patch status on self"))?;

        if cur.resource_version() != self.indexer.resource_version() {
            self.recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "DropinUpdated".into(),
                        action: "Patch".into(),
                        note: None,
                        secondary: None,
                    },
                    &cur.object_ref(&()),
                )
                .await?;
        }
        Ok(())
    }

    /// Deployment attempts to bring the owned Deployment into spec.
    #[instrument(skip(self), ret)]
    async fn deployment(&self) -> Result<()> {
        use apps::v1::Deployment;

        let api = Api::<Deployment>::namespaced(self.client(), self.ns());
        let status = self.indexer.status.clone().unwrap_or_default();

        let d = DeploymentBuilder::try_from(self.indexer.as_ref())?.build();
        trace!(?d, "created Deployment");
        let _d = api
            .patch(&d.name_any(), &PATCH_PARAMS, &Patch::Apply(d))
            .instrument(debug_span!("patch", kind = "Deployment"))
            .await?;

        let deployment_ref = status.refs.as_ref().and_then(|d| {
            d.iter().find(|&objref| {
                objref.kind == Deployment::kind(&())
                    && objref.api_group == Deployment::group(&()).to_string().into()
            })
        });
        if deployment_ref.is_some() {
            debug!("no need to update status");
            return Ok(());
        }
        debug!("updating status");

        let cnd = Condition {
            message: "created Deployment".into(),
            observed_generation: self.indexer.metadata.generation,
            last_transition_time: meta::v1::Time(Timestamp::now()),
            reason: "DeploymentCreated".into(),
            status: "True".into(),
            type_: clair_condition("DeploymentCreated"),
        };
        self.set_condition(cnd).await?;

        Ok(())
    }

    /// Service attempts to bring the owned Service into spec.
    #[instrument(skip(self), ret)]
    async fn service(&self) -> Result<()> {
        use self::core::v1::Service;

        let api = Api::<Service>::namespaced(self.client(), self.ns());
        let status = self.indexer.status.clone().unwrap_or_default();

        let s = ServiceBuilder::try_from(self.indexer.as_ref())?.build();
        let _s = api
            .patch(&s.name_any(), &PATCH_PARAMS, &Patch::Apply(s))
            .await
            .inspect_err(|error| error!(%error, "failed to patch Service"))?;

        let service_ref = status.refs.as_ref().and_then(|d| {
            d.iter().find(|&objref| {
                objref.kind == Service::kind(&())
                    && objref.api_group == Service::group(&()).to_string().into()
            })
        });
        if service_ref.is_some() {
            debug!("no need to update status");
            return Ok(());
        }
        debug!("updating status");

        let cnd = Condition {
            message: "created Service".into(),
            observed_generation: self.indexer.metadata.generation,
            last_transition_time: meta::v1::Time(Timestamp::now()),
            reason: "ServiceCreated".into(),
            status: "True".into(),
            type_: clair_condition("ServiceCreated"),
        };
        self.set_condition(cnd).await?;

        Ok(())
    }

    /// Horizontal_pod_autoscaler attempts to bring the owned HorizontalPodAutoscaler into spec.
    #[instrument(skip(self), ret)]
    async fn horizontal_pod_autoscaler(&self) -> Result<()> {
        use self::autoscaling::v2::HorizontalPodAutoscaler;

        let api = Api::<HorizontalPodAutoscaler>::namespaced(self.client(), self.ns());
        let status = self.indexer.status.clone().unwrap_or_default();

        let s = HorizontalPodAutoscalerBuilder::try_from(self.indexer.as_ref())?.build();
        let _s = api
            .patch(&s.name_any(), &PATCH_PARAMS, &Patch::Apply(s))
            .await
            .inspect_err(|error| error!(%error, "failed to patch HorizontalPodAutoscaler"))?;

        let service_ref = status.refs.as_ref().and_then(|d| {
            d.iter().find(|&objref| {
                objref.kind == HorizontalPodAutoscaler::kind(&())
                    && objref.api_group == HorizontalPodAutoscaler::group(&()).to_string().into()
            })
        });
        if service_ref.is_some() {
            debug!("no need to update status");
            return Ok(());
        }
        debug!("updating status");

        let cnd = Condition {
            message: "created HorizontalPodAutoscaler".into(),
            observed_generation: self.indexer.metadata.generation,
            last_transition_time: meta::v1::Time(Timestamp::now()),
            reason: "HorizontalPodAutoscalerCreated".into(),
            status: "True".into(),
            type_: clair_condition("HorizontalPodAutoscalerCreated"),
        };
        self.set_condition(cnd).await?;

        Ok(())
    }

    /// Check_spec reports `Some` if the spec is incomplete or `None` if processing can proceed.
    #[instrument(skip(self), ret)]
    async fn check_spec(&self) -> Result<Option<Action>> {
        let objref = self.indexer.object_ref(&());
        let mut cnd = Condition {
            last_transition_time: meta::v1::Time(Timestamp::now()),
            observed_generation: self.indexer.metadata.generation,
            type_: clair_condition("SpecOK"),
            message: "".into(),
            reason: "SpecIncomplete".into(),
            status: "False".into(),
        };
        let mut res = Action::requeue(Duration::from_secs(3600)).into();
        let mut ev = Event {
            type_: EventType::Warning,
            action: "CheckSpec".into(),
            reason: "".into(),
            note: None,
            secondary: None,
        };

        if self.indexer.spec.config.is_none() {
            let note = "spec missing ConfigSource";
            error!(hint = "is the admission webhook running?", note);
            ev.note = Some(note.into());
        } else if self.indexer.spec.image.is_none() {
            let note = "spec missing image";
            error!(hint = "is the admission webhook running?", note);
            ev.note = Some(note.into());
        } else {
            cnd.status = "True".into();
            cnd.reason = "SpecComplete".into();
            res = None;
            ev.type_ = EventType::Normal;
        }

        try_join!(
            self.set_condition(cnd),
            self.recorder.publish(&ev, &objref).map_err(Error::from)
        )?;
        Ok(res)
    }
}

/// Reconcile is the main entrypoint for the reconcile loop.
#[instrument(skip(ctx, indexer), fields(name = indexer.name_any(), namespace = indexer.namespace().unwrap()))]
async fn reconcile(indexer: Arc<Indexer>, ctx: Arc<Context>) -> Result<Action> {
    assert!(indexer.meta().name.is_some());
    info!("reconciling Indexer");
    let r = Reconciler::from((indexer.clone(), ctx.clone()));

    if let Some(a) = r.check_spec().await? {
        return Ok(a);
    };
    r.deployment().await?;
    r.service().await?;
    r.horizontal_pod_autoscaler().await?;
    r.publish_dropin().await?;

    Ok(DEFAULT_REQUEUE.clone())
}

#[allow(dead_code)]
async fn patch_condition<K>(client: Client, obj: &K, cnd: Condition) -> Result<()>
where
    K: Resource<DynamicType = (), Scope = kube::core::NamespaceResourceScope>
        + serde::de::DeserializeOwned,
{
    let ns = obj.namespace().unwrap();
    let api = Api::<K>::namespaced(client, &ns);
    let status = json!({
        "status": { "conditions": [ cnd ] },
    });
    api.patch_status(
        &obj.name_unchecked(),
        &PatchParams::default(),
        &Patch::Merge(&status),
    )
    .await?;
    Ok(())
}

/*
#[instrument(skip_all)]
async fn check_creation(
    obj: &Indexer,
    _ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    let refs = [
        obj.status
            .as_ref()
            .and_then(|s| s.has_ref::<core::v1::ConfigMap>()),
        obj.status
            .as_ref()
            .and_then(|s| s.has_ref::<apps::v1::Deployment>()),
        obj.status
            .as_ref()
            .and_then(|s| s.has_ref::<core::v1::Service>()),
        obj.status
            .as_ref()
            .and_then(|s| s.has_ref::<autoscaling::v2::HorizontalPodAutoscaler>()),
    ];
    let ok = refs.iter().all(|r| r.is_some());
    let status = if ok { "True" } else { "False" }.to_string();
    let message = if ok {
        "🆗".to_string()
    } else {
        format!(
            "missing: {}",
            refs.iter()
                .filter_map(|r| r.as_ref().map(|r| r.kind.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    next.add_condition(meta::v1::Condition {
        last_transition_time: req.now(),
        observed_generation: obj.metadata.generation,
        reason: "ObjectsCreated".into(),
        type_: clair_condition("Initialized"),
        message,
        status,
    });
    Ok(ok)
}
*/

#[instrument(skip_all)]
fn handle_error(_obj: Arc<Indexer>, _err: &Error, _ctx: Arc<Context>) -> Action {
    Action::await_change()
}
