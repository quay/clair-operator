//! Matchers holds the controller for the "Matcher" CRD.

use kube::{runtime::controller::Error as CtrlErr, Api};
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, prelude::*, COMPONENT_LABEL};

static COMPONENT: &str = "matcher";

/// .
///
/// # Errors
///
/// This function will return an error if .
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    let ctl = Controller::new(
        Api::<v1alpha1::Matcher>::default_namespaced(client.clone()),
        ctlcfg.clone(),
    )
    .owns(
        Api::<apps::v1::Deployment>::default_namespaced(client.clone()),
        ctlcfg.clone(),
    )
    .owns(
        Api::<autoscaling::v2::HorizontalPodAutoscaler>::default_namespaced(client.clone()),
        ctlcfg.clone(),
    )
    .owns(Api::<core::v1::Service>::default_namespaced(client), ctlcfg)
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

#[instrument(skip_all)]
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
            {
                'checks: {
$(
                    debug!(step = stringify!($fn), "running check");
                    let cont = $fn(&obj, &ctx, &req, &mut next).await?;
                    debug!(step = stringify!($fn), "continue" = cont, "ran check");
                    if !cont {
                        break 'checks
                    }
)+
                }
            }
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
    let name = obj.name_any();

    let prev = obj.metadata.resource_version.clone().unwrap();
    let mut cur = None;
    let mut c = v1alpha1::Matcher::new(&name, Default::default());
    c.status = Some(next);
    let mut ct = 0;

    while ct < 3 {
        c.metadata.resource_version = Some(prev.clone());
        ct += 1;
        let buf = serde_json::to_vec(&c)?;
        match api.replace_status(&name, &CREATE_PARAMS, buf).await {
            Ok(c) => {
                cur = c.resource_version();
                break;
            }
            Err(err) => error!(error=%err, "problem updating status"),
        }
    }

    if let Some(cur) = cur {
        debug!(attempt = ct, prev, cur, "published status");
        if cur == prev {
            // If there was no change, queue out in the future.
            Ok(Action::requeue(Duration::from_secs(3600)))
        } else {
            // Handled, so discard the event.
            Ok(Action::await_change())
        }
    } else {
        // Unable to update, so requeue soon.
        Ok(Action::requeue(Duration::from_secs(5)))
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
    let name = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<apps::v1::Deployment>())
        .map(|cm| cm.name)
        .or_else(|| {
            Some(format!(
                "{}-{}",
                obj.name_any(),
                v1alpha1::Matcher::kind(&()).to_ascii_lowercase()
            ))
        })
        .unwrap();
    trace!(name, "looking for Deployment");
    let cfgsrc = obj
        .spec
        .config
        .as_ref()
        .ok_or(Error::BadName("missing needed spec field: config".into()))?;
    trace!("have configsource");
    let api = Api::<apps::v1::Deployment>::default_namespaced(ctx.client.clone());
    let want_image = obj.spec.image_default(&crate::DEFAULT_IMAGE);

    let mut ct = 0;
    while ct < 3 {
        ct += 1;
        trace!(ct, "reconcile attempt");
        let mut entry = api.entry(&name).await?.or_insert(|| {
            trace!(ct, name, "creating");
            futures::executor::block_on(new_templated(obj, ctx)).expect("template failed")
        });
        let d = entry.get_mut();
        trace!("checking deployment");
        d.labels_mut()
            .insert(COMPONENT_LABEL.to_string(), COMPONENT.into());
        let (mut vols, mut mounts, config) = make_volumes(cfgsrc);
        if let Some(ref mut spec) = d.spec {
            if spec.selector.match_labels.is_none() {
                spec.selector.match_labels = Some(Default::default());
            }
            spec.selector
                .match_labels
                .as_mut()
                .unwrap()
                .insert(COMPONENT_LABEL.to_string(), COMPONENT.into());
            if let Some(ref mut meta) = spec.template.metadata {
                if meta.labels.is_none() {
                    meta.labels = Some(Default::default());
                }
                meta.labels
                    .as_mut()
                    .unwrap()
                    .insert(COMPONENT_LABEL.to_string(), COMPONENT.into());
            }
            if let Some(ref mut spec) = spec.template.spec {
                if let Some(ref mut vs) = spec.volumes {
                    vols.append(vs);
                    vols.sort_by_key(|v| v.name.clone());
                    vols.dedup_by_key(|v| v.name.clone());
                    *vs = vols;
                };
                if let Some(ref mut c) = spec.containers.iter_mut().find(|c| c.name == "clair") {
                    c.image = Some(want_image.clone());
                    if c.volume_mounts.is_none() {
                        c.volume_mounts = Some(Default::default());
                    }
                    if let Some(ref mut ms) = c.volume_mounts {
                        ms.append(&mut mounts);
                        ms.sort_by_key(|m| m.name.clone());
                        ms.dedup_by_key(|m| m.name.clone());
                    };
                    if c.env.is_none() {
                        c.env = Some(Default::default());
                    }
                    if let Some(ref mut es) = c.env {
                        es.push(EnvVar {
                            name: "CLAIR_CONF".into(),
                            value: Some(config),
                            value_from: None,
                        });
                        es.push(EnvVar {
                            name: "CLAIR_MODE".into(),
                            value: Some(v1alpha1::Matcher::kind(&()).to_ascii_lowercase()),
                            value_from: None,
                        });
                        es.sort_by_key(|e| e.name.clone());
                        es.dedup_by_key(|e| e.name.clone());
                    };
                };
            }
            trace!(?spec, "deployment spec");
        };
        next.add_ref(d);
        match entry.commit(&CREATE_PARAMS).await {
            Ok(()) => break,
            Err(err) => {
                trace!(error = ?err, "commit error");
                match err {
                    CommitError::Validate(reason) => {
                        debug!(reason = reason.to_string(), "commit failed, retrying")
                    }
                    CommitError::Save(_) => return Err(Error::Commit(err)),
                };
            }
        };
    }
    trace!(ct, "reconciled");
    Ok(ct != 3)
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
