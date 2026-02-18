//! Clairs holds the controller for the "Clair" CRD.

use std::sync::{Arc, LazyLock};

use k8s_openapi::api::{batch::v1::Job, core::v1::ConfigMap};
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

use crate::{Context, image_version, prelude::*, util::check_owned_resource};
use api::v1alpha1::{Clair, ClairStatus, DropinSelector, Indexer, Matcher, Notifier};
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

#[instrument(
    target = "",
    name = "Clairs",
    skip(ctx, clair),
    fields(
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

    info!("reconciling");
    finalizer(&api, CLAIR_FINALIZER, clair, |event| async {
        match event {
            Finalizer::Apply(clair) => reconcile_one(clair, ctx.clone()).await,
            Finalizer::Cleanup(clair) => cleanup_one(clair, ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::Finalizer(Box::new(e)))
}

mod reason {
    use std::fmt::{Display, Formatter, Result};

    pub(super) enum Event {
        MissingRequiredField,
        DeleteRequested,
    }

    impl Display for Event {
        fn fmt(&self, f: &mut Formatter<'_>) -> Result {
            use Event::*;
            f.write_str(match self {
                MissingRequiredField => stringify!(MissingRequiredField),
                DeleteRequested => stringify!(DeleteRequested),
            })
        }
    }
    impl From<Event> for String {
        fn from(r: Event) -> Self {
            r.to_string()
        }
    }

    pub(super) enum AdminPre {
        NewClair,
        ImageUpdated,
        JobFailed,
        JobSucceeded,
        JobNotComplete,
        JobMissing,
    }

    impl Display for AdminPre {
        fn fmt(&self, f: &mut Formatter<'_>) -> Result {
            use AdminPre::*;
            f.write_str(match self {
                NewClair => "NewClair",
                ImageUpdated => "ImageUpdated",
                JobFailed => "JobFailed",
                JobSucceeded => "JobSucceeded",
                JobNotComplete => "JobNotComplete",
                JobMissing => "JobMissing",
            })
        }
    }
    impl From<AdminPre> for String {
        fn from(r: AdminPre) -> Self {
            r.to_string()
        }
    }

    pub(super) enum Configuration {
        Reconciled,
    }

    impl Display for Configuration {
        fn fmt(&self, f: &mut Formatter<'_>) -> Result {
            use Configuration::*;
            f.write_str(match self {
                Reconciled => "ConfigurationReconciled",
            })
        }
    }
    impl From<Configuration> for String {
        fn from(r: Configuration) -> Self {
            r.to_string()
        }
    }
}

#[instrument(name = "reconcile", skip(ctx, clair), ret)]
async fn reconcile_one(clair: Arc<Clair>, ctx: Arc<Context>) -> Result<Action> {
    use reason::Event as Reason;
    let oref = clair.object_ref(&());

    let mut missing = false;
    for (field, present) in [
        ("$.spec.databases", clair.spec.databases.is_some()),
        ("$.spec.image", clair.spec.image.is_some()),
    ] {
        if !present {
            missing = true;
            let reason = Reason::MissingRequiredField;
            info!(field, %reason, "skipping reconciliation");
            ctx.recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: reason.into(),
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

    configuration(&clair, &ctx).await?;

    if clair.status.as_ref().is_none_or(|s| s.config.is_none()) {
        return Ok(Action::requeue(Duration::from_millis(250)));
    }
    admin_pre(&clair, &ctx).await?;
    promote_image(&clair, &ctx).await?;
    indexer(&clair, &ctx).await?;
    matcher(&clair, &ctx).await?;
    if clair.spec.notifier.unwrap_or_default() {
        notifier(&clair, &ctx).await?;
    }
    admin_post(&clair, &ctx).await?;

    Ok(DEFAULT_REQUEUE.clone())
}

#[instrument(skip(ctx, clair), ret)]
async fn configuration(clair: &Clair, ctx: &Context) -> Result<()> {
    use reason::Configuration as Reason;

    let ns = clair.namespace().expect("Clair is namespaced");
    let name = clair.metadata.name.as_ref().expect("Clair has a name");
    let cm = check_owned_resource::<_, ConfigMap, ConfigMapBuilder>(clair, ctx).await?;
    // TODO(hank): There's got to be a more elegant way to do this via `futures`.
    let cm = if let Some(cm) = cm {
        cm
    } else {
        Api::<ConfigMap>::namespaced(ctx.client.clone(), &ns)
            .get(name)
            .await?
    };
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
        "status": {
            "config": cfgsrc,
            "conditions": [
                 Condition {
                    message: "ConfigSource object in desired state".into(),
                    observed_generation: clair.metadata.generation,
                    last_transition_time: meta::v1::Time(Timestamp::now()),
                    reason: Reason::Reconciled.into(),
                    status: "True".into(),
                    type_: ConditionType::ConfigReady.into(),
                }
            ],
        }
    }));

    let clairs = Api::<Clair>::namespaced(ctx.client.clone(), &ns);
    clairs
        .patch_status(name, &PATCH_PARAMS, &status_update)
        .await?;

    Ok(())
}

#[instrument(skip(ctx, clair), ret)]
async fn indexer(clair: &Clair, ctx: &Context) -> Result<()> {
    check_owned_resource::<_, Indexer, IndexerBuilder>(clair, ctx)
        .await
        .and(Ok(()))
}

#[instrument(skip(ctx, clair), ret)]
async fn matcher(clair: &Clair, ctx: &Context) -> Result<()> {
    check_owned_resource::<_, Matcher, MatcherBuilder>(clair, ctx)
        .await
        .and(Ok(()))
}

#[instrument(skip(ctx, clair), ret)]
async fn notifier(clair: &Clair, ctx: &Context) -> Result<()> {
    check_owned_resource::<_, Notifier, NotifierBuilder>(clair, ctx)
        .await
        .and(Ok(()))
}

/// The admin_pre step is responsible for arranging for the admin pre-upgrade job to run and
/// tracking its state.
#[instrument(skip(clair, ctx), ret)]
async fn admin_pre(clair: &Clair, ctx: &Context) -> Result<()> {
    use reason::AdminPre as Reason;

    let ns = clair.namespace().expect("Clair is namespaced");
    let name = clair.name_any();
    let clairs = Api::<Clair>::namespaced(ctx.client.clone(), &ns);
    let jobs = Api::<Job>::namespaced(ctx.client.clone(), &ns);

    let job_type = ConditionType::AdminPreJobDone;
    let pre_job_cnd = clair.find_condition(job_type);
    let spec_image = clair.spec.image.as_ref();
    let status_image = clair.status.as_ref().and_then(|s| s.image.as_ref());

    if spec_image.and_then(|img| image_version(img)).is_none() {
        // TODO(hank): Event
        info!(r#"container image ref is not versioned, skipping "admin" jobs"#);
        return Ok(());
    }

    debug!(
        have_condition = pre_job_cnd.is_some(),
        spec_image, status_image, r#"checking if "admin pre" job should be created"#
    );
    let create = pre_job_cnd.is_some_and(|cnd| {
        spec_image != status_image && clair.metadata.generation != cnd.observed_generation
    });
    let job = if create {
        JobBuilder::admin_pre(clair)?.build().into()
    } else {
        None
    };

    let cnd = match (pre_job_cnd, job) {
        (None, Some(_)) => unreachable!(),
        (None, None) => {
            // Create "empty" condition":
            let reason = Reason::NewClair;
            info!(%reason, r#"skipping "admin pre" job"#);
            Condition {
                message: "pre jobs are not needed on a fresh system".into(),
                observed_generation: clair.metadata.generation,
                last_transition_time: meta::v1::Time(Timestamp::now()),
                status: "True".into(),
                type_: job_type.into(),
                reason: reason.into(),
            }
        }
        (Some(cnd), Some(ref job)) => {
            // Create the Job and report the update condition.
            let reason = Reason::ImageUpdated;
            info!(%reason, r#"creating "admin pre" job"#);
            jobs.create(&CREATE_PARAMS, job)
                .instrument(debug_span!("create"))
                .await?;
            Condition {
                message: "spec changed, launching \"admin pre\" job".into(),
                observed_generation: clair.metadata.generation,
                last_transition_time: meta::v1::Time(Timestamp::now()),
                status: "False".into(),
                reason: reason.into(),
                ..cnd.clone()
            }
        }
        // Haven't marked the Job as completed:
        (Some(cnd), None) if cnd.status != "True" => {
            let name = JobBuilder::admin_pre_name(clair)?;
            info!(name, r#"checking "admin pre" job"#);
            match jobs.get_opt(&name).await? {
                Some(job) => {
                    let status = job.status.unwrap_or_default();
                    // Assume there's precisely 1 run.
                    debug_assert!(
                        status.active.is_none_or(|ct| ct <= 1),
                        "status.active has count > 1: {:?}",
                        status.active
                    );
                    match status.active {
                        Some(0) => match (status.succeeded, status.failed) {
                            (_, Some(1)) => {
                                // TODO(hank) Emit an event so someone takes a gander.
                                Condition {
                                    message: "job failed, please investigate".into(),
                                    observed_generation: clair.metadata.generation,
                                    last_transition_time: meta::v1::Time(Timestamp::now()),
                                    reason: Reason::JobFailed.into(),
                                    status: "False".into(),
                                    ..cnd.clone()
                                }
                            }
                            (Some(1), _) => Condition {
                                message: "job completed successfully".into(),
                                observed_generation: clair.metadata.generation,
                                last_transition_time: meta::v1::Time(Timestamp::now()),
                                reason: Reason::JobSucceeded.into(),
                                status: "True".into(),
                                ..cnd.clone()
                            },
                            _ => unreachable!(),
                        },
                        Some(_) | None => Condition {
                            message: "job not complete".into(),
                            observed_generation: clair.metadata.generation,
                            last_transition_time: meta::v1::Time(Timestamp::now()),
                            reason: Reason::JobNotComplete.into(),
                            status: "False".into(),
                            ..cnd.clone()
                        },
                    }
                }
                None => Condition {
                    message: format!(r#"unable to fetch job "{name}""#),
                    observed_generation: clair.metadata.generation,
                    last_transition_time: meta::v1::Time(Timestamp::now()),
                    reason: Reason::JobMissing.into(),
                    status: "Unknown".into(),
                    ..cnd.clone()
                },
            }
        }
        // The Job is completed, no need to touch the status.
        (Some(_), None) => return Ok(()),
    };

    let update = Patch::Apply(json!({
        "apiVersion": Clair::api_version(&()),
        "kind": Clair::kind(&()),
        "status": {
            "conditions": [cnd],
        },
    }));
    trace!("patching status");
    clairs
        .patch_status(&name, &PATCH_PARAMS, &update)
        .instrument(debug_span!("patch_status"))
        .await?;

    Ok(())
}

#[instrument(skip(clair, ctx), ret)]
async fn promote_image(clair: &Clair, ctx: &Context) -> Result<()> {
    let job_type = ConditionType::AdminPreJobDone;
    let pre_job_cnd = clair.find_condition(job_type);
    let spec_image = clair.spec.image.as_ref();
    let status_image = clair.status.as_ref().and_then(|s| s.image.as_ref());
    let image_same = spec_image == status_image;

    let promote = match (pre_job_cnd, image_same) {
        (None, _) => true, // If there's no condition, assume there's no previous image to update from.
        (Some(_), true) => false, // Nothing to do.
        (Some(cnd), false) if clair.metadata.generation != cnd.observed_generation => false, // Here, the generation on the condition is not current, so do nothing.
        (Some(cnd), false) => cnd.status == "True", // This is the "currentmost" situation. Just report if the status is "True"
    };
    debug!(
        have_condition = pre_job_cnd.is_some(),
        spec_image, status_image, promote, "checking if image should be promoted"
    );

    if promote {
        let ns = clair
            .meta()
            .namespace
            .as_ref()
            .expect("Clair is namespaced");
        let name = clair.meta().name.as_ref().expect("Clair has a name");
        let clairs = Api::<Clair>::namespaced(ctx.client.clone(), ns);
        let next_status = ClairStatus {
            image: spec_image.cloned(),
            ..Default::default()
        };
        let update = Patch::Apply(json!({
            "apiVersion": Clair::api_version(&()),
            "kind": Clair::kind(&()),
            "status": next_status,
        }));

        debug!("patching status");
        clairs
            .patch_status(name, &PATCH_PARAMS, &update)
            .instrument(debug_span!("patch_status"))
            .await?;
    }

    Ok(())
}

#[instrument(skip(clair, _ctx), ret)]
async fn admin_post(clair: &Clair, _ctx: &Context) -> Result<()> {
    info!(TODO = true, "write admin post job");

    let _ns = clair.namespace().expect("Clair is namespaced");

    let post_job_type = ConditionType::AdminPostJobDone;
    let post_job_cnd = clair
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|cs| cs.iter().find(|&c| post_job_type == c.type_));

    match post_job_cnd {
        Some(c) if c.status == "True" => (), // Continue
        Some(c) => {
            info!(type = %post_job_type, status = c.status, "condition not met");
            return Ok(());
        }
        None => {
            debug!(type = %post_job_type, "no condition");
            return Ok(());
        }
    };

    Ok(())
}

#[instrument(name = "cleanup", skip(ctx, clair))]
async fn cleanup_one(clair: Arc<Clair>, ctx: Arc<Context>) -> Result<Action> {
    use reason::Event as Reason;

    let oref = clair.object_ref(&());
    // No real cleanup, so we just publish an event.
    ctx.recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: Reason::DeleteRequested.into(),
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
