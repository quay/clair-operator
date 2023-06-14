#![allow(unused_imports)]

use kube::{
    api::{Patch, PostParams},
    runtime::controller::Error as CtrlErr,
    Api,
};
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, prelude::*, COMPONENT_LABEL};
static COMPONENT: &str = "indexer";

/// .
///
/// # Errors
///
/// This function will return an error if .
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let wcfg = ctlcfg
        .clone()
        .labels(format!("{}={COMPONENT}", COMPONENT_LABEL.as_str()).as_str());
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    let ctl = Controller::new(
        Api::<v1alpha1::Matcher>::default_namespaced(client.clone()),
        ctlcfg,
    )
    .owns(
        Api::<apps::v1::Deployment>::default_namespaced(client.clone()),
        wcfg.clone(),
    )
    .owns(
        Api::<autoscaling::v2::HorizontalPodAutoscaler>::default_namespaced(client.clone()),
        wcfg.clone(),
    )
    .owns(Api::<core::v1::Service>::default_namespaced(client), wcfg)
    .reconcile_all_on(sig)
    .graceful_shutdown_on(cancel.cancelled_owned());

    Ok(async move {
        info!("spawning matcher controller");
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

#[instrument(skip(ctx),fields(%obj))]
async fn reconcile(obj: Arc<v1alpha1::Matcher>, ctx: Arc<Context>) -> Result<Action> {
    trace!("start");
    let req = Request::new(&ctx.client, obj.object_ref(&()));
    assert!(obj.meta().name.is_some());
    let spec = &obj.spec;
    let mut next = if let Some(status) = &obj.status {
        status.clone()
    } else {
        trace!("no status present");
        Default::default()
    };

    // Check the spec:
    let action = "CheckConfig".into();
    let type_ = clair_condition("SpecOK");
    trace!("configsource check");
    if obj.spec.config.is_none() {
        req.publish(Event {
            type_: EventType::Warning,
            reason: "Initialization".into(),
            note: Some("missing field \"/spec/config\"".into()),
            action,
            secondary: None,
        })
        .await?;
        next.add_condition(Condition {
            last_transition_time: req.now(),
            message: "\"/spec/config\" missing".into(),
            observed_generation: obj.metadata.generation,
            reason: "SpecIncomplete".into(),
            status: "False".into(),
            type_,
        });
        return publish(obj, ctx, req, next).await;
    }
    trace!("configsource ok");
    trace!(
        provided = spec.image.is_some(),
        image = spec.image_default(&crate::DEFAULT_IMAGE),
        "image check"
    );
    next.add_condition(Condition {
        last_transition_time: req.now(),
        observed_generation: obj.metadata.generation,
        message: "".into(),
        reason: "SpecComplete".into(),
        status: "True".into(),
        type_: clair_condition("SpecOK"),
    });
    trace!("spec ok");

    macro_rules! check_all {
        ($($fn:ident),+ $(,)?) => {
            let mut r#continue = true;
            $(
            // Otherwise the compiler will complain about the last assignment:
            #[allow(unused_assignments)]
            if !r#continue {
                trace!(step=stringify!($fn), "skipping check");
            } else {
                trace!(step=stringify!($fn), "running check");
                r#continue = $fn(&obj, &ctx, &req, &mut next).await?;
            }
            )+
        }
    }
    check_all!(
        check_config,
        check_deployment,
        check_service,
        check_hpa,
        check_creation
    );

    trace!("done");
    publish(obj, ctx, req, next).await
}

#[instrument(skip_all)]
fn handle_error(_obj: Arc<v1alpha1::Matcher>, _err: &Error, _ctx: Arc<Context>) -> Action {
    Action::await_change()
}

#[instrument(skip_all)]
async fn publish(
    obj: Arc<v1alpha1::Matcher>,
    ctx: Arc<Context>,
    _req: Request,
    next: v1alpha1::MatcherStatus,
) -> Result<Action> {
    let api: Api<v1alpha1::Matcher> = Api::default_namespaced(ctx.client.clone());
    let name = &obj.metadata.name.as_ref().unwrap();

    let changed = obj.status.is_none() || obj.status.as_ref().unwrap() == &next;
    let mut c = v1alpha1::Matcher::clone(&obj);
    c.status = Some(next);
    c.metadata.managed_fields = None; // ???

    api.patch_status(name, &PatchParams::apply(CONTROLLER_NAME), &Patch::Apply(c))
        .await?;
    trace!(changed, "patched status");
    if changed {
        Ok(Action::await_change())
    } else {
        Ok(Action::requeue(Duration::from_secs(3600)))
    }
}

#[instrument(skip_all)]
async fn check_config(
    obj: &v1alpha1::Matcher,
    _ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::MatcherStatus,
) -> Result<bool> {
    if obj.status.is_none() || obj.status.as_ref().unwrap().config.is_none() {
        next.config = obj.spec.config.clone();
        return Ok(false);
    }
    let want = obj.spec.config.as_ref().unwrap();
    let got = obj.status.as_ref().unwrap().config.as_ref().unwrap();
    if got != want {
        next.config = obj.spec.config.clone();
        // TODO(hank) Touch the deployment
        return Ok(false);
    }
    Ok(true)
}
#[instrument(skip_all)]
async fn check_deployment(
    obj: &v1alpha1::Matcher,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::MatcherStatus,
) -> Result<bool> {
    use self::core::v1::EnvVar;
    let dref = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<apps::v1::Deployment>());
    let cfgsrc = obj
        .spec
        .config
        .as_ref()
        .ok_or(Error::BadName("missing needed spec field: config".into()))?;
    if dref.is_none() {
        let mut deploy: apps::v1::Deployment = new_templated(obj, ctx).await?;
        let image = obj.spec.image_default(&crate::DEFAULT_IMAGE);
        let (vols, mounts, config) = make_volumes(cfgsrc);
        if let Some(ref mut spec) = deploy.spec {
            if let Some(ref mut spec) = spec.template.spec {
                spec.volumes = Some(vols);
                let c = &mut spec.containers[0];
                c.image = Some(image);
                c.volume_mounts = Some(mounts);
                c.env.get_or_insert(vec![]).push(EnvVar {
                    name: "CLAIR_CONF".into(),
                    value: Some(config),
                    value_from: None,
                });
            };
        };

        let api = Api::<apps::v1::Deployment>::default_namespaced(ctx.client.clone());
        let deploy = api.create(&CREATE_PARAMS, &deploy).await?;
        debug!(name = deploy.name_unchecked(), "created Deployment");
        next.add_ref(&deploy);
        return Ok(false);
    }
    // TODO
    Ok(true)
}

#[instrument(skip_all)]
async fn check_service(
    obj: &v1alpha1::Matcher,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::MatcherStatus,
) -> Result<bool> {
    let sref = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<core::v1::Service>());
    if sref.is_none() {
        let srv: core::v1::Service = new_templated(obj, ctx).await?;
        let api = Api::<core::v1::Service>::default_namespaced(ctx.client.clone());
        let srv = api.create(&CREATE_PARAMS, &srv).await?;
        debug!(name = srv.name_unchecked(), "created Service");
        next.add_ref(&srv);
        return Ok(false);
    }
    // TODO
    Ok(true)
}

#[instrument(skip_all)]
async fn check_hpa(
    obj: &v1alpha1::Matcher,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::MatcherStatus,
) -> Result<bool> {
    let href = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<autoscaling::v2::HorizontalPodAutoscaler>());
    if href.is_none() {
        let hpa: autoscaling::v2::HorizontalPodAutoscaler = new_templated(obj, ctx).await?;
        let api =
            Api::<autoscaling::v2::HorizontalPodAutoscaler>::default_namespaced(ctx.client.clone());
        let hpa = api.create(&CREATE_PARAMS, &hpa).await?;
        debug!(name = hpa.name_unchecked(), "created HPA");
        next.add_ref(&hpa);
        return Ok(false);
    }
    // TODO
    Ok(true)
}

#[instrument(skip_all)]
async fn check_creation(
    obj: &v1alpha1::Matcher,
    _ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::MatcherStatus,
) -> Result<bool> {
    let refs = [
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
        "".to_string()
    } else {
        format!(
            "missing: {}",
            refs.iter()
                .filter_map(|r| r.as_ref().map(|r| r.kind.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    next.add_condition(Condition {
        last_transition_time: req.now(),
        observed_generation: obj.metadata.generation,
        reason: "ObjectsCreated".into(),
        type_: clair_condition("Initialized"),
        message,
        status,
    });
    if ok {
        req.publish(Event {
            type_: EventType::Normal,
            reason: "Initialization".into(),
            note: Some("👍".into()),
            action: "ObjectCreation".into(),
            secondary: None,
        })
        .await?;
    }
    Ok(ok)
}
