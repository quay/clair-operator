//! Matchers holds the controller for the "Matcher" CRD.
use std::sync::{Arc, LazyLock};

use k8s_openapi::merge_strategies;
use kube::{
    api::{Api, Patch},
    client::Client,
    core::GroupVersionKind,
    runtime::controller::Error as CtrlErr,
    ResourceExt,
};
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, cmp_condition, merge_condition, prelude::*};
use clair_templates::{
    render_dropin, Build, DeploymentBuilder, HTTPRouteBuilder, HorizontalPodAutoscalerBuilder,
    ServiceBuilder,
};
use v1alpha1::Matcher;

//static COMPONENT: LazyLock<String> = LazyLock::new(|| Matcher::kind(&()).to_ascii_lowercase());
static SELF_GVK: LazyLock<GroupVersionKind> = LazyLock::new(|| GroupVersionKind {
    group: Matcher::group(&()).to_string(),
    version: Matcher::version(&()).to_string(),
    kind: Matcher::kind(&()).to_string(),
});

/// Controller is the Matcher controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    use gateway_networking_k8s_io::v1::{grpcroutes::GRPCRoute, httproutes::HTTPRoute};

    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    Ok(async move {
        info!("spawning matcher controller");

        let mut ctl = Controller::new(Api::<Matcher>::all(client.clone()), ctlcfg.clone())
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
        if ctx.gvk_exists(&crate::GATEWAY_NETWORKING_HTTPROUTE).await {
            ctl = ctl.owns(Api::<HTTPRoute>::all(client.clone()), ctlcfg.clone());
        }
        if ctx.gvk_exists(&crate::GATEWAY_NETWORKING_GRPCROUTE).await {
            ctl = ctl.owns(Api::<GRPCRoute>::all(client.clone()), ctlcfg.clone());
        }
        let ctl = ctl
            .reconcile_all_on(sig)
            .graceful_shutdown_on(cancel.cancelled_owned());

        if !ctx.gvk_exists(&SELF_GVK).await {
            error!("CRD is not queryable ({SELF_GVK:?}); is the CRD installed?");
            return Err(Error::BadName("no CRD".into()));
        }

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
        debug!("matcher controller finished");

        Ok(())
    }
    .boxed())
}

#[derive(Debug)]
struct Reconciler {
    matcher: Arc<Matcher>,
    ctx: Arc<Context>,
    namespace: String,
    api: Api<Matcher>,
}

impl From<(Arc<Matcher>, Arc<Context>)> for Reconciler {
    fn from(value: (Arc<Matcher>, Arc<Context>)) -> Self {
        let (matcher, ctx) = value;
        let namespace = matcher.namespace().unwrap(); // Matcher is namespace scoped
        let api: Api<Matcher> = Api::namespaced(ctx.client.clone(), &namespace);
        Self {
            matcher,
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
        self.matcher.name_unchecked()
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

    #[instrument(skip(self), ret)]
    async fn publish_dropin(&self) -> Result<()> {
        use self::core::v1::Service;

        let owner = match self
            .matcher
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
        trace!(owner = owner.name, "matcher is owned");

        let s = ServiceBuilder::try_from(self.matcher.as_ref())?.build();
        let srv = Api::<Service>::namespaced(self.client(), self.ns())
            .get(&s.name_any())
            .await?;

        let status = v1alpha1::WorkerStatus {
            dropin: render_dropin::<Matcher>(&srv),
            ..Default::default()
        };
        self.api
            .patch_status(&self.name(), &PATCH_PARAMS, &Patch::Apply(&status))
            .instrument(debug_span!("patch_status"))
            .await
            .inspect_err(|error| error!(%error, "unable to patch status on self"))?;

        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn deployment(&self) -> Result<()> {
        use apps::v1::Deployment;

        let api = Api::<Deployment>::namespaced(self.client(), self.ns());
        let status = self.matcher.status.clone().unwrap_or_default();

        let d = DeploymentBuilder::try_from(self.matcher.as_ref())?.build();
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
            observed_generation: self.matcher.metadata.generation,
            last_transition_time: meta::v1::Time(Utc::now()),
            reason: "DeploymentCreated".into(),
            status: "True".into(),
            type_: clair_condition("DeploymentCreated"),
        };
        self.set_condition(cnd).await?;

        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn service(&self) -> Result<()> {
        use self::core::v1::Service;

        let api = Api::<Service>::namespaced(self.client(), self.ns());
        let status = self.matcher.status.clone().unwrap_or_default();

        let s = ServiceBuilder::try_from(self.matcher.as_ref())?.build();
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
            observed_generation: self.matcher.metadata.generation,
            last_transition_time: meta::v1::Time(Utc::now()),
            reason: "ServiceCreated".into(),
            status: "True".into(),
            type_: clair_condition("ServiceCreated"),
        };
        self.set_condition(cnd).await?;

        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn horizontal_pod_autoscaler(&self) -> Result<()> {
        use self::autoscaling::v2::HorizontalPodAutoscaler;

        let api = Api::<HorizontalPodAutoscaler>::namespaced(self.client(), self.ns());
        let status = self.matcher.status.clone().unwrap_or_default();

        let s = HorizontalPodAutoscalerBuilder::try_from(self.matcher.as_ref())?.build();
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
            observed_generation: self.matcher.metadata.generation,
            last_transition_time: meta::v1::Time(Utc::now()),
            reason: "HorizontalPodAutoscalerCreated".into(),
            status: "True".into(),
            type_: clair_condition("HorizontalPodAutoscalerCreated"),
        };
        self.set_condition(cnd).await?;

        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn route(&self) -> Result<()> {
        use gateway_networking_k8s_io::v1::httproutes::*;

        let api = Api::<HTTPRoute>::namespaced(self.client(), self.ns());
        let status = self.matcher.status.clone().unwrap_or_default();

        let r = HTTPRouteBuilder::try_from(self.matcher.as_ref())?.build();
        let _r = api
            .patch(&r.name_any(), &PATCH_PARAMS, &Patch::Apply(r))
            .await
            .inspect_err(|error| error!(%error, "failed to patch HTTPRoute"))?;

        let route_ref = status.refs.as_ref().and_then(|d| {
            d.iter().find(|&objref| {
                objref.kind == HTTPRoute::kind(&())
                    && objref.api_group == HTTPRoute::group(&()).to_string().into()
            })
        });
        if route_ref.is_some() {
            debug!("no need to update status");
            return Ok(());
        }
        debug!("updating status");

        let cnd = Condition {
            message: "created HTTPRoute".into(),
            observed_generation: self.matcher.metadata.generation,
            last_transition_time: meta::v1::Time(Utc::now()),
            reason: "HTTPRouteCreated".into(),
            status: "True".into(),
            type_: clair_condition("HTTPRouteCreated"),
        };
        self.set_condition(cnd).await?;

        Ok(())
    }

    #[instrument(skip(self), ret)]
    async fn check_spec(&self) -> Result<Option<Action>> {
        let mut cnd = Condition {
            last_transition_time: meta::v1::Time(Utc::now()),
            observed_generation: self.matcher.metadata.generation,
            type_: clair_condition("SpecOK"),
            message: "".into(),
            reason: "SpecIncomplete".into(),
            status: "False".into(),
        };

        if self.matcher.spec.config.is_none() {
            error!("spec missing ConfigSource");
            self.set_condition(cnd).await?;
            return Ok(Action::requeue(Duration::from_secs(3600)).into());
        }
        if self.matcher.spec.image.is_none() {
            error!("spec missing image");
            self.set_condition(cnd).await?;
            return Ok(Action::requeue(Duration::from_secs(3600)).into());
        }

        cnd.status = "True".into();
        cnd.reason = "SpecComplete".into();
        self.set_condition(cnd).await?;
        Ok(None)
    }
}

/// Reconcile is the main entrypoint for the reconcile loop.
#[instrument(skip(ctx, matcher), fields(name = matcher.name_any(), namespace = matcher.namespace().unwrap()))]
async fn reconcile(matcher: Arc<Matcher>, ctx: Arc<Context>) -> Result<Action> {
    assert!(matcher.meta().name.is_some());
    info!("reconciling Matcher");
    let r = Reconciler::from((matcher.clone(), ctx.clone()));

    if let Some(a) = r.check_spec().await? {
        return Ok(a);
    };
    r.deployment().await?;
    r.service().await?;
    r.horizontal_pod_autoscaler().await?;
    r.route().await?;
    r.publish_dropin().await?;

    Ok(DEFAULT_REQUEUE.clone())
}

#[instrument(skip_all)]
fn handle_error(_obj: Arc<Matcher>, _err: &Error, _ctx: Arc<Context>) -> Action {
    Action::await_change()
}
