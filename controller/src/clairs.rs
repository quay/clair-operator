//! Clairs holds the controller for the "Clair" CRD.

use std::sync::{Arc, LazyLock};

use api::v1alpha1::ClairStatus;
use k8s_openapi::{api::core::v1::ConfigMap, merge_strategies};
use kube::{
    Resource, ResourceExt,
    api::{Api, ListParams, Patch},
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
};
use tokio_stream::wrappers::SignalStream;
use tracing::*;

use crate::{
    Context, clair_condition, cmp_condition, merge_condition, prelude::*,
    util::check_owned_resource,
};
use api::v1alpha1::{Clair, DropinSelector, Indexer, Matcher, Notifier};
use clair_templates::{
    Build, ConfigMapBuilder, ConfigSourceBuilder, IndexerBuilder, JobBuilder, MatcherBuilder,
    NotifierBuilder,
};

pub(crate) static CLAIR_FINALIZER: &str = "clairs.clairproject.org";
static SELF_GVK: LazyLock<GroupVersionKind> = LazyLock::new(|| GroupVersionKind {
    group: Clair::group(&()).to_string(),
    version: Clair::version(&()).to_string(),
    kind: Clair::kind(&()).to_string(),
});

/// Controller is the Clair controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<State>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let root: Api<Clair> = Api::all(client.clone());
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    Ok(async move {
        if let Err(e) = root.list(&ListParams::default().limit(1)).await {
            error!("CRD ({SELF_GVK:?}) is not queryable ({e:?}); is the CRD installed?");
            return Err(Error::BadName("no CRD".into()));
        }

        let ctl = Controller::new(root, ctlcfg.clone())
            .owns(Api::<Indexer>::all(client.clone()), ctlcfg.clone())
            .owns(Api::<Matcher>::all(client.clone()), ctlcfg.clone())
            .owns(Api::<Notifier>::all(client.clone()), ctlcfg.clone())
            .owns(Api::<core::v1::Secret>::all(client.clone()), ctlcfg.clone())
            .owns(
                Api::<core::v1::ConfigMap>::all(client.clone()),
                ctlcfg.clone(),
            )
            .owns(Api::<batch::v1::Job>::all(client.clone()), ctlcfg.clone())
            .reconcile_all_on(sig)
            .graceful_shutdown_on(cancel.cancelled_owned());
        info!("starting clair controller");

        ctl.run(reconcile, error_policy, Context::from(ctx).into())
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

fn error_policy(obj: Arc<Clair>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::requeue(Duration::from_secs(5))
}

#[instrument(skip(ctx, clair),fields(
    trace_id,
    kind = Clair::kind(&()).as_ref(),
    namespace = clair.namespace().unwrap(),
    name = clair.name_any(),
    generation = clair.metadata.generation,
    resource_version = clair.metadata.resource_version
))]
async fn reconcile(clair: Arc<Clair>, ctx: Arc<Context>) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    if trace_id != opentelemetry::trace::TraceId::INVALID {
        Span::current().record("trace_id", field::display(&trace_id));
    }
    let ns = clair.namespace().unwrap();
    let api: Api<Clair> = Api::namespaced(ctx.client.clone(), &ns);

    info!(r#"reconciling Clair "{}" in {}"#, clair.name_any(), ns);
    finalizer(&api, CLAIR_FINALIZER, clair, |event| async {
        match event {
            Finalizer::Apply(clair) => reconcile_one(clair, ctx.clone()).await,
            Finalizer::Cleanup(clair) => cleanup_one(clair, ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::Finalizer(Box::new(e)))
}

#[instrument(skip(ctx, clair))]
async fn reconcile_one(clair: Arc<Clair>, ctx: Arc<Context>) -> Result<Action> {
    let oref = clair.object_ref(&());

    let mut missing = false;
    for (field, present) in [
        ("$.spec.databases", clair.spec.databases.is_some()),
        ("$.spec.image", clair.spec.image.is_some()),
    ] {
        if !present {
            missing = true;
            info!(field, "missing required field, skipping reconciliation");
            ctx.recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "MissingRequiredField".into(),
                        note: format!("Clair `{}` missing `{field}`", clair.name_any()).into(),
                        action: "Reconcile".into(),
                        secondary: None,
                    },
                    &oref,
                )
                .await
                .map_err(Error::Kube)?;
        }
    }
    if missing {
        return Ok(Action::await_change());
    }

    reconcile_configuration(&clair, &ctx).await?;

    if clair.status.as_ref().is_none_or(|s| s.config.is_none()) {
        return Ok(Action::requeue(Duration::from_millis(250)));
    }
    //reconcile_admin_pre(&clair, &ctx).await?;
    check_owned_resource::<_, Indexer, IndexerBuilder>(&clair, &ctx).await?;
    check_owned_resource::<_, Matcher, MatcherBuilder>(&clair, &ctx).await?;
    if clair.spec.notifier.unwrap_or_default() {
        check_owned_resource::<_, Notifier, NotifierBuilder>(&clair, &ctx).await?;
    }
    //reconcile_admin_post(&clair, &ctx).await?;

    Ok(DEFAULT_REQUEUE.clone())
}

#[instrument(skip(ctx, clair), ret)]
async fn reconcile_configuration(clair: &Clair, ctx: &Context) -> Result<()> {
    let cm = check_owned_resource::<_, ConfigMap, ConfigMapBuilder>(&clair, &ctx).await?;
    let cfgsrc = ConfigSourceBuilder::try_from(&cm)?
        .with_dropins(clair.spec.databases.as_ref().into_iter().flat_map(|db| {
            trace!("have databases");
            [Some(&db.indexer), Some(&db.matcher), db.notifier.as_ref()]
                .into_iter()
                .flatten()
                .map(|s| DropinSelector::secret(&s.name, &s.key))
        }))
        .with_dropins(clair.spec.dropins.iter().cloned())
        .build();

    trace!(config_source=?cfgsrc, "created ConfigSource");

    debug!("updating config");
    let status_update = Patch::Apply(json!({
        "apiVersion": Clair::api_version(&()),
        "kind": Clair::kind(&()),
        "status": ClairStatus {
            config: cfgsrc.into(),
            conditions: vec![
                 Condition {
                    message: "ConfigSource object in desired state".into(),
                    observed_generation: clair.metadata.generation,
                    last_transition_time: meta::v1::Time(Timestamp::now()),
                    reason: "ConfigSourceReconciled".into(),
                    status: "True".into(),
                    type_: clair_condition("ConfigReady").into(),
                }
            ].into(),
            ..Default::default()
        }
    }));

    let ns = clair.namespace().expect("Clair is namespaced");
    let name = clair.metadata.name.as_ref().expect("Clair has a name");
    let clairs = Api::<Clair>::namespaced(ctx.client.clone(), &ns);
    clairs
        .patch_status(name, &PATCH_PARAMS, &status_update)
        .await?;

    Ok(())
}

#[allow(dead_code)]
/// The admin_pre step is responsible for arranging for the admin pre-upgrade jobs to run and
/// for "promoting" the version.
#[instrument(skip(clair, ctx), ret)]
async fn reconcile_admin_pre(clair: &Clair, ctx: &Context) -> Result<()> {
    use batch::v1::Job;

    let ns = clair.namespace().expect("Clair is namespaced");
    let name = clair.name_any();
    let mut update = vec![];
    let mut promote = false;
    let cnds = clair
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let clairs = Api::<Clair>::namespaced(ctx.client.clone(), &ns);
    let jobs = Api::<Job>::namespaced(ctx.client.clone(), &ns);

    // If there are no conditions, record the Job as done and continue.
    //
    // If there are conditions, check in order:
    // - If the PreJob condition is not current to the spec:
    //   - Check on the current image:
    //     - If changed, start a the new job and set the condtion to False.
    // - If the PreJob condition is current to the spec:
    //   - If false, check on the job and update if need be.
    //   - If true, swap the new image into the status.

    let job_type = clair_condition("AdminPreJobDone");
    if let Some(cnd) = cnds.iter().find(|&c| c.type_ == job_type) {
        debug!("checking Condition");
        if cnd.observed_generation != clair.metadata.generation {
            debug!(
                observed = cnd.observed_generation,
                current = clair.metadata.generation,
                "generation differs"
            );
            if clair.spec.image.as_ref() == clair.status.as_ref().and_then(|s| s.image.as_ref()) {
                debug!("\"spec.image\" not changed");
                update.push(Condition {
                    message: "spec.image not changed".into(),
                    observed_generation: clair.metadata.generation,
                    last_transition_time: meta::v1::Time(Timestamp::now()),
                    reason: "NoImageUpdate".into(),
                    status: "True".into(),
                    type_: job_type,
                });
            } else {
                debug!("starting \"admin pre\" job");
                update.push(Condition {
                    message: "spec.image changed, launching \"admin pre\" job".into(),
                    observed_generation: clair.metadata.generation,
                    last_transition_time: meta::v1::Time(Timestamp::now()),
                    reason: "ImageUpdated".into(),
                    status: "False".into(),
                    type_: job_type,
                });
                info!(TODO = true, "launch job");

                let j = JobBuilder::admin_pre(clair)?.build();
                jobs.create(&CREATE_PARAMS, &j)
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
                    if clair.spec.image.as_ref()
                        != clair.status.as_ref().and_then(|s| s.image.as_ref())
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
            observed_generation: clair.metadata.generation,
            last_transition_time: meta::v1::Time(Timestamp::now()),
            reason: "NewClair".into(),
            status: "True".into(),
            type_: job_type,
        });
    }

    if !update.is_empty() {
        let next = clairs
            .get_status(&name)
            .instrument(debug_span!("get_status"))
            .await
            .map(|mut next| {
                next.meta_mut().managed_fields = None;
                let status = next.status.get_or_insert_default();
                if promote {
                    status.image = clair.spec.image.clone();
                }
                let cnds = status.conditions.get_or_insert_default();
                merge_strategies::list::map(cnds, update, &[cmp_condition], merge_condition);
                next
            })?;
        trace!("patching status");
        clairs
            .patch_status(&name, &PATCH_PARAMS, &Patch::Apply(&next))
            .instrument(debug_span!("patch_status"))
            .await?;
    }

    Ok(())
}

#[allow(dead_code)]
#[instrument(skip(_clair, _ctx), ret)]
async fn reconcile_admin_post(_clair: Clair, _ctx: &Context) -> Result<()> {
    info!(TODO = true, "write admin post job");
    Ok(())
}

#[instrument(skip(ctx, clair))]
async fn cleanup_one(clair: Arc<Clair>, ctx: Arc<Context>) -> Result<Action> {
    let oref = clair.object_ref(&());
    // No real cleanup, so we just publish an event.
    ctx.recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "DeleteRequested".into(),
                note: Some(format!("Delete `{}`", clair.name_any())),
                action: "Deleting".into(),
                secondary: None,
            },
            &oref,
        )
        .await
        .map_err(Error::Kube)?;
    Ok(Action::await_change())
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::events::v1::Event;

    use super::*;
    use crate::testing::*;
    use api::v1alpha1::{ConfigMapKeySelector, ConfigSource};

    #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
    async fn clairs_without_finalizer_gets_a_finalizer() {
        let (testctx, fakeserver) = Context::clair_tests();
        let c = clair::test(None);
        let mocksrv = fakeserver.run(ClairScenario::FinalizerCreation(c.clone()));
        reconcile(Arc::new(c), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
    async fn finalized_clairs_causes_event() {
        let (testctx, fakeserver) = Context::clair_tests();
        let c = clair::finalized(clair::test(None));
        let mocksrv = fakeserver.run(ClairScenario::Event(
            c.clone(),
            Event {
                type_: Some("Warning".into()),
                reason: Some("MissingRequiredField".to_string()),
                action: Some("Reconcile".into()),
                ..Default::default()
            },
        ));
        reconcile(Arc::new(c), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
    async fn ready_clairs() {
        let (testctx, fakeserver) = Context::clair_tests();
        let c = clair::ready();
        let mocksrv = fakeserver.run(ClairScenario::Ready(c.clone()));
        reconcile(Arc::new(c.clone()), testctx.clone())
            .await
            .expect("reconciler");
        let c = clair::with_status(
            c,
            ClairStatus {
                config: ConfigSource {
                    root: ConfigMapKeySelector {
                        name: "test".into(),
                        key: "config.json".into(),
                    },
                    dropins: vec![],
                }
                .into(),
                ..Default::default()
            },
        );
        reconcile(Arc::new(c), testctx.clone())
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }
}
