//! Clairs holds the controller for the "Clair" CRD.

use std::sync::{Arc, LazyLock};

use futures::{
    future::FutureExt,
    stream::{self, StreamExt},
};
use k8s_openapi::api::{batch::v1::Job, core::v1::ConfigMap};
use kube::{
    Resource, ResourceExt,
    api::{Api, ListParams, Patch},
    core::GroupVersionKind,
    runtime::{
        controller::Error as CtrlErr,
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
    Context,
    condition::{Status::*, new as new_condition},
    image_version,
    prelude::*,
    util::check_owned_resource,
};
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

pub(crate) mod reason {
    use crate::condition::Reason as ConditionReason;
    use strum::{Display, EnumString, IntoStaticStr};

    #[derive(Debug, Display, IntoStaticStr, EnumString)]
    pub enum AdminPre {
        NewClair,
        ImageUpdated,
        JobFailed,
        JobSucceeded,
        JobNotComplete,
        JobMissing,
    }

    impl ConditionReason for AdminPre {}

    #[derive(Debug, Display, IntoStaticStr, EnumString)]
    pub enum Configuration {
        Reconciled,
    }

    impl ConditionReason for Configuration {}

    macro_rules! eq_impl{
        ($($ty:ty),+) => {
            $(
            impl PartialEq<String> for $ty {
                fn eq(&self, other: &String) -> bool {
                    self.to_string().eq(other)
                }
            }
            )+
        };
    }
    eq_impl!(AdminPre, Configuration);
}

pub(crate) mod event {
    use strum::{Display, EnumString, IntoStaticStr};

    #[derive(Debug, Display, IntoStaticStr, EnumString)]
    pub enum Reason {
        MissingRequiredField,
        DeleteRequested,
        ImageRefNotVersioned,
        AdminPostNotReady,
    }

    impl crate::event::Reason for Reason {}

    #[derive(Debug, Display, IntoStaticStr, EnumString)]
    pub enum Action {
        CheckSpec,
        Configuration,
        AdminPre,
        PromoteImage,
        Indexer,
        Matcher,
        Notifier,
        AdminPost,
        Cleanup,
    }

    impl crate::event::Action for Action {}
}

#[instrument(name = "reconcile", skip(ctx, clair), ret)]
async fn reconcile_one(clair: Arc<Clair>, ctx: Arc<Context>) -> Result<Action> {
    let mut missing = false;
    for (field, present) in [
        ("$.spec.databases", clair.spec.databases.is_some()),
        ("$.spec.image", clair.spec.image.is_some()),
    ] {
        if !present {
            missing = true;
            ctx.warn_note(
                clair.as_ref(),
                event::Reason::MissingRequiredField,
                event::Action::CheckSpec,
                format!("Clair `{}` missing `{field}`", clair.name_any()),
            )
            .await?;
        }
    }
    if missing {
        return Ok(Action::await_change());
    }

    configuration(&clair, &ctx).await?;

    if clair.status.as_ref().is_none_or(|s| s.config.is_none()) {
        return Ok(Action::requeue(Duration::from_millis(250))); // ???
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
                new_condition(clair, Type::ConfigReady, True, Reason::Reconciled, "ConfigSource object in desired state"),
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

    let job_type = Type::AdminPreJobDone;
    let pre_job_cnd = clair.find_condition(job_type);
    let spec_image = clair.spec.image.as_ref();
    let status_image = clair.status.as_ref().and_then(|s| s.image.as_ref());

    if spec_image.and_then(|img| image_version(img)).is_none() {
        ctx.info_note(
            clair,
            event::Reason::ImageRefNotVersioned,
            event::Action::AdminPre,
            r#"skipping "admin" jobs"#,
        )
        .await?;
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
            new_condition(
                clair,
                job_type,
                True,
                reason,
                "pre jobs are not needed on a fresh system",
            )
        }
        (Some(_), Some(ref job)) => {
            // Create the Job and report the update condition.
            let reason = Reason::ImageUpdated;
            info!(%reason, r#"creating "admin pre" job"#);
            jobs.create(&CREATE_PARAMS, job)
                .instrument(debug_span!("create"))
                .await?;
            new_condition(
                clair,
                job_type,
                False,
                reason,
                r#"spec changed, launching "admin pre" job"#,
            )
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
                                new_condition(
                                    clair,
                                    job_type,
                                    False,
                                    Reason::JobFailed,
                                    "job failed, please investigate",
                                )
                            }
                            (Some(1), _) => new_condition(
                                clair,
                                job_type,
                                True,
                                Reason::JobSucceeded,
                                "job completed successfully",
                            ),
                            _ => unreachable!(),
                        },
                        Some(_) | None => new_condition(
                            clair,
                            job_type,
                            False,
                            Reason::JobNotComplete,
                            "job not complete",
                        ),
                    }
                }
                None => new_condition(
                    clair,
                    job_type,
                    Unknown,
                    Reason::JobMissing,
                    format!(r#"unable to fetch job "{name}""#),
                ),
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
    let job_type = Type::AdminPreJobDone;
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

#[instrument(skip(clair, ctx), ret)]
async fn admin_post(clair: &Clair, ctx: &Context) -> Result<()> {
    use apps::v1::Deployment;
    info!(TODO = true, "write admin post job");

    let ns = clair
        .metadata
        .namespace
        .as_ref()
        .expect("Clair is namespaced");
    let name = clair.metadata.name.as_ref().expect("Clair has a name");
    let spec = &clair.spec;

    if spec
        .image
        .as_ref()
        .and_then(|img| image_version(img))
        .is_none()
    {
        ctx.info(
            clair,
            event::Reason::ImageRefNotVersioned,
            event::Action::AdminPost,
        )
        .await?;
        return Ok(());
    }

    // Check that the conditions on this object are in the correct state:
    let ok = [
        Some(Type::AdminPostJobDone),
        Some(Type::IndexerCreated),
        Some(Type::MatcherCreated),
        spec.notifier
            .and_then(|enable| enable.then_some(Type::NotifierCreated)),
    ]
    .into_iter()
    .flatten()
    .all(|typ| {
        clair
            .find_condition(typ)
            .inspect(|&cnd| debug!("type" = %cnd.type_, %cnd.status, "condition"))
            .is_some_and(|cnd| Status::True == cnd.status)
    });
    if !ok {
        ctx.info_note(
            clair,
            event::Reason::AdminPostNotReady,
            event::Action::AdminPost,
            "conditions not met",
        )
        .await?;
        return Ok(());
    }

    // Check that the dependant Deployments are in the correct state:
    let ok = stream::iter(
        [
            Some(Indexer::kind(&())),
            Some(Matcher::kind(&())),
            spec.notifier
                .and_then(|enable| enable.then(|| Notifier::kind(&()))),
        ]
        .into_iter()
        .filter_map(|k| k.map(|kind| format!("{name}-{kind}"))),
    )
    .then(|name| async move {
        Api::<Deployment>::namespaced(ctx.client.clone(), ns)
            .get_status(&name)
            .instrument(debug_span!("get_status"))
            .await
    })
    .try_all(|d: Deployment| async move {
        let name = d.metadata.name.as_ref().expect("Deployment has a name");
        d.status
            .as_ref()
            .and_then(|status| status.conditions.as_ref())
            .and_then(|cnds| {
                cnds.iter()
                    .inspect(|&cnd| debug!(name, "type" = %cnd.type_, %cnd.status, "condition"))
                    .find(|&cnd| cnd.type_ == "Ready" && cnd.status == "True")
            })
            .is_some()
    })
    .await?;
    if !ok {
        ctx.info_note(
            clair,
            event::Reason::AdminPostNotReady,
            event::Action::AdminPost,
            "Deployments not ready",
        )
        .await?;
        return Ok(());
    }

    // Now, do basically the same thing as the PreJob:

    let post_job_type = Type::AdminPostJobDone;
    let post_job_cnd = clair.find_condition(post_job_type);

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
    use event::Reason;

    // No real cleanup, so we just publish an event.
    ctx.info(
        clair.as_ref(),
        Reason::DeleteRequested,
        event::Action::Cleanup,
    )
    .await?;
    Ok(Action::await_change())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
    async fn finalizer() {
        let (testctx, fakeserver) = Context::clair_tests();
        let tc = ClairScenario::FinalizerCreation;
        let c = tc.object();
        let mocksrv = fakeserver.run(ClairScenario::FinalizerCreation);
        reconcile(Arc::new(c), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
    async fn finalized_clairs_causes_event() {
        let (testctx, fakeserver) = Context::clair_tests();
        let tc = ClairScenario::Finalize;
        let c = tc.object();
        let mocksrv = fakeserver.run(tc);
        reconcile(Arc::new(c), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
    async fn ready() {
        let (testctx, fakeserver) = Context::clair_tests();
        let tc = ClairScenario::Ready;
        let c = tc.object();
        let mocksrv = fakeserver.run(tc);
        /*
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
        */
        reconcile(Arc::new(c), testctx.clone())
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }
    #[cfg(test)]
    mod configuration {
        use std::str::FromStr;

        use crate::{clairs, testing::*, *};

        macro_rules! testcase {
            ($s:ident) => {
                #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
                async fn $s() {
                    let tc = ConfigurationScenario::from_str(stringify!($s)).unwrap();
                    let tc = ClairScenario::Configuration(tc);
                    let (testctx, fakeserver) = Context::clair_tests();
                    let c = tc.object();
                    let mocksrv = fakeserver.run(tc);
                    clairs::configuration(&c, &testctx)
                        .await
                        .expect("reconciler");
                    timeout_after_1s(mocksrv).await;
                }
            };
        }

        testcase!(create);
        testcase!(created);
    }

    #[cfg(test)]
    mod admin_pre {
        use std::str::FromStr;

        use crate::{clairs, testing::*, *};

        macro_rules! testcase {
            ($s:ident) => {
                #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
                async fn $s() {
                    let tc = AdminPreScenario::from_str(stringify!($s)).unwrap();
                    let tc = ClairScenario::AdminPre(tc);
                    let (testctx, fakeserver) = Context::clair_tests();
                    let c = tc.object();
                    let mocksrv = fakeserver.run(tc);
                    clairs::admin_pre(&c, &testctx).await.expect("reconciler");
                    timeout_after_1s(mocksrv).await;
                }
            };
        }

        testcase!(new);
        testcase!(unversioned);
        testcase!(spec_changed);
        testcase!(spec_unchanged_check);
        testcase!(spec_unchanged_done);
    }

    #[cfg(test)]
    mod promote_image {
        use std::str::FromStr;

        use crate::{clairs, testing::*, *};

        macro_rules! testcase {
            ($s:ident) => {
                #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
                async fn $s() {
                    let tc = PromoteImageScenario::from_str(stringify!($s)).unwrap();
                    let tc = ClairScenario::PromoteImage(tc);
                    let (testctx, fakeserver) = Context::clair_tests();
                    let c = tc.object();
                    let mocksrv = fakeserver.run(tc);
                    clairs::promote_image(&c, &testctx)
                        .await
                        .expect("reconciler");
                    timeout_after_1s(mocksrv).await;
                }
            };
        }

        testcase!(no_condition);
        testcase!(same_image);
        testcase!(old_condition);
        testcase!(not_ready);
        testcase!(ready);
    }

    #[cfg(test)]
    mod indexer {
        use std::str::FromStr;

        use crate::{clairs, testing::*, *};

        macro_rules! testcase {
            ($s:ident) => {
                #[self::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
                async fn $s() {
                    let tc = IndexerScenario::from_str(stringify!($s)).unwrap();
                    let tc = ClairScenario::Indexer(tc);
                    let (testctx, fakeserver) = Context::clair_tests();
                    let c = tc.object();
                    let mocksrv = fakeserver.run(tc);
                    clairs::indexer(&c, &testctx).await.expect("reconciler");
                    timeout_after_1s(mocksrv).await;
                }
            };
        }

        testcase!(create);
        testcase!(update);
    }
}
