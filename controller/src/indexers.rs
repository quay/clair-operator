//! Indexers holds the controller for the "Indexer" CRD.
//!
//! ```mermaid
//! ```

use std::sync::{Arc, LazyLock};

use kube::{
    ResourceExt,
    api::{Api, Patch},
    client::Client,
    core::GroupVersionKind,
    runtime::{
        controller::Error as CtrlErr,
        events::{Event, EventType},
        finalizer::{Event as Finalizer, finalizer},
    },
};
use serde_json::json;
use tokio::{
    signal::unix::{SignalKind, signal},
    time::Duration,
    try_join,
};
use tokio_stream::wrappers::SignalStream;
use tracing::*;

use crate::{Context, clair_condition, prelude::*, util::check_owned_resource};
use clair_templates::{Build, ServiceBuilder, render_dropin};
use v1alpha1::Indexer;

pub(crate) static INDEXER_FINALIZER: &str = "indexers.clairproject.org";
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
pub fn controller(cancel: CancellationToken, ctx: Arc<State>) -> Result<ControllerFuture> {
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
        ctl.run(reconcile, error_policy, Context::from(ctx).into())
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

fn error_policy(obj: Arc<Indexer>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::requeue(Duration::from_secs(5))
}

/// Reconcile is the main entrypoint for the reconcile loop.
#[instrument(skip(ctx, indexer),fields(
    trace_id,
    name = indexer.name_any(),
    namespace = indexer.namespace().unwrap(),
    generation = indexer.metadata.generation,
    resource_version = indexer.metadata.resource_version
))]
async fn reconcile(indexer: Arc<Indexer>, ctx: Arc<Context>) -> Result<Action> {
    debug_assert!(indexer.meta().name.is_some());
    let trace_id = telemetry::get_trace_id();
    if trace_id != opentelemetry::trace::TraceId::INVALID {
        Span::current().record("trace_id", field::display(&trace_id));
    }
    let ns = indexer.namespace().unwrap();
    let api: Api<Indexer> = Api::namespaced(ctx.client.clone(), &ns);

    info!(r#"reconciling Indexer "{}" in {}"#, indexer.name_any(), ns);
    finalizer(&api, INDEXER_FINALIZER, indexer, |event| async {
        match event {
            Finalizer::Apply(indexer) => reconcile_one(indexer, ctx.clone()).await,
            Finalizer::Cleanup(indexer) => cleanup_one(indexer, ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::Finalizer(Box::new(e)))
    /*
    let r = Reconciler::from((indexer.clone(), ctx.clone()));


    if let Some(a) = r.check_spec().await? {
        return Ok(a);
    };
    r.deployment().await?;
    r.service().await?;
    r.horizontal_pod_autoscaler().await?;
    r.publish_dropin().await?;

    Ok(DEFAULT_REQUEUE.clone())
    */
}

#[instrument(skip(ctx, obj))]
async fn reconcile_one(obj: Arc<Indexer>, ctx: Arc<Context>) -> Result<Action> {
    use clair_templates::{DeploymentBuilder, HorizontalPodAutoscalerBuilder, ServiceBuilder};
    use k8s_openapi::api::{
        apps::v1::Deployment, autoscaling::v2::HorizontalPodAutoscaler, core::v1::Service,
    };

    if let Some(a) = check_spec(&obj, &ctx).await? {
        return Ok(a);
    };

    check_owned_resource::<Indexer, Deployment, DeploymentBuilder>(&obj, &ctx).await?;
    check_owned_resource::<Indexer, Service, ServiceBuilder>(&obj, &ctx).await?;
    check_owned_resource::<Indexer, HorizontalPodAutoscaler, HorizontalPodAutoscalerBuilder>(
        &obj, &ctx,
    )
    .await?;

    publish_dropin(&obj, &ctx).await?;

    Ok(DEFAULT_REQUEUE.clone())
}

#[instrument(skip(ctx, obj))]
async fn cleanup_one(obj: Arc<Indexer>, ctx: Arc<Context>) -> Result<Action> {
    // No real cleanup, so we just publish an event.
    ctx.recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "DeleteRequested".into(),
                note: Some(format!("Delete `{}`", obj.name_any())),
                action: "Deleting".into(),
                secondary: None,
            },
            &obj.object_ref(&()),
        )
        .await?;
    Ok(Action::await_change())
}

/// Check_spec reports `Some` if the spec is incomplete or `None` if processing can proceed.
#[instrument(skip(indexer, ctx), ret)]
async fn check_spec(indexer: &Indexer, ctx: &Context) -> Result<Option<Action>> {
    let mut cnd = Condition {
        last_transition_time: meta::v1::Time(Timestamp::now()),
        observed_generation: indexer.metadata.generation,
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

    if indexer.spec.config.is_none() {
        let note = "spec missing ConfigSource";
        error!(hint = "is the admission webhook running?", note);
        ev.note = Some(note.into());
    } else if indexer.spec.image.is_none() {
        let note = "spec missing image";
        error!(hint = "is the admission webhook running?", note);
        ev.note = Some(note.into());
    } else {
        cnd.status = "True".into();
        cnd.reason = "SpecComplete".into();
        res = None;
        ev.type_ = EventType::Normal;
    }

    let ns = indexer.namespace().expect("Indexer is namespaced");
    let api: Api<Indexer> = Api::namespaced(ctx.client.clone(), &ns);
    let objref = indexer.object_ref(&());
    try_join!(
        set_condition(api, indexer, cnd),
        ctx.recorder.publish(&ev, &objref).map_err(Error::from)
    )?;

    Ok(res)
}

/// Publish_dropin renders a Clair configuration dropin that points any Indexer URL fields to
/// this Indexer's Service endpoint.
///
/// The dropin is only created if the Indexer is owned by a Clair.
#[instrument(skip(indexer, ctx), ret)]
async fn publish_dropin(indexer: &Indexer, ctx: &Context) -> Result<()> {
    use self::core::v1::ConfigMap;

    let owner = match indexer
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
    let ns = indexer.namespace().expect("Indexer is namespaced");
    let cm_name = indexer
        .spec
        .config
        .as_ref()
        .expect("\"check_spec\" step should have ensured spec is populated")
        .root
        .name
        .as_str();

    let s = ServiceBuilder::try_from(indexer)?.build();
    let dropin = render_dropin::<Indexer>(&s).expect("Indexer should have configuration dropin");
    let config_patch = Patch::Apply(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "data": {
            "00-indexer.json-patch": dropin,
        },
    }));
    let cms = Api::<ConfigMap>::namespaced(ctx.client.clone(), &ns);
    cms.patch(cm_name, &PATCH_PARAMS, &config_patch)
        .instrument(debug_span!("patch"))
        .await
        .inspect_err(|error| error!(%error, "unable to patch ConfigMap"))?;

    ctx.recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "Reconcile".into(),
                action: "DropinReconciled".into(),
                note: None,
                secondary: None,
            },
            &indexer.object_ref(&()),
        )
        .await?;
    Ok(())
}

async fn set_condition(api: Api<Indexer>, indexer: &Indexer, cnd: Condition) -> Result<()> {
    let name = indexer.uid().expect("Indexer should have UID");
    let patch = Patch::Apply(json!({
        "apiVersion": "v1alpha1",
        "kind": "Indexer",
        "status": {
            "conditions": [cnd],
        },
    }));
    api.patch_status(&name, &PATCH_PARAMS, &patch)
        .instrument(debug_span!("patch_status"))
        .await?;
    Ok(())
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
