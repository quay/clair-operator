//! Indexers holds the controller for the "Indexer" CRD.

use std::sync::Arc;

use kube::{runtime::controller::Error as CtrlErr, Api};
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, prelude::*, COMPONENT_LABEL};

static COMPONENT: &str = "indexer";

/// Controller is the Indexer controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    let ctl = Controller::new(
        Api::<v1alpha1::Indexer>::default_namespaced(client.clone()),
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
        info!("spawning indexer controller");
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
        debug!("indexer controller finished");
        Ok(())
    }
    .boxed())
}

#[instrument(skip_all)]
async fn reconcile(obj: Arc<v1alpha1::Indexer>, ctx: Arc<Context>) -> Result<Action> {
    trace!("start");
    let req = Request::new(&ctx.client, obj.object_ref(&()));
    assert!(obj.meta().name.is_some());
    let spec = &obj.spec;
    let mut next = obj.status.clone().unwrap_or_default();

    // Check the spec:
    let action = "CheckConfig".into();
    let type_ = clair_condition("SpecOK");
    debug!("configsource check");
    if obj.spec.config.is_none() {
        trace!("configsource missing");
        req.publish(Event {
            type_: EventType::Warning,
            reason: "Initialization".into(),
            note: Some("missing field \"/spec/config\"".into()),
            action,
            secondary: None,
        })
        .await?;
        next.add_condition(meta::v1::Condition {
            last_transition_time: req.now(),
            message: "\"/spec/config\" missing".into(),
            observed_generation: obj.metadata.generation,
            reason: "SpecIncomplete".into(),
            status: "False".into(),
            type_,
        });
        return publish(obj, ctx, req, next).await;
    }
    debug!("configsource ok");
    debug!(
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
    debug!("spec ok");

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
        check_dropin,
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
async fn publish(
    obj: Arc<v1alpha1::Indexer>,
    ctx: Arc<Context>,
    _req: Request,
    next: v1alpha1::IndexerStatus,
) -> Result<Action> {
    let api: Api<v1alpha1::Indexer> = Api::default_namespaced(ctx.client.clone());
    let name = obj.name_any();

    let prev = obj.metadata.resource_version.clone().unwrap();
    let mut cur = None;
    let mut c = v1alpha1::Indexer::new(&name, Default::default());
    c.status = Some(next);
    let mut ct = 0;
    while ct < 3 {
        c.metadata.resource_version = obj.metadata.resource_version.clone();
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
async fn check_dropin(
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    use self::core::v1::ConfigMap;
    use self::v1alpha1::ConfigMapKeySelector;

    let owner = match obj
        .owner_references()
        .iter()
        .find(|&r| r.controller.unwrap_or(false))
    {
        None => {
            trace!("not owned, skipping dropin");
            return Ok(true);
        }
        Some(o) => o,
    };
    trace!(owner = owner.name, "indexer is owned");

    let name = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<ConfigMap>())
        .map(|cm| cm.name)
        .or_else(|| {
            Some(format!(
                "{}-{}",
                obj.name_any(),
                v1alpha1::Indexer::kind(&()).to_ascii_lowercase()
            ))
        })
        .unwrap();
    trace!(name, "looking for ConfigMap");
    let srvname = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<core::v1::Service>())
        .map(|cm| cm.name)
        .or_else(|| {
            Some(format!(
                "{}-{}",
                obj.name_any(),
                v1alpha1::Indexer::kind(&()).to_ascii_lowercase()
            ))
        })
        .unwrap();
    trace!(name = srvname, "assuming Service");
    let clair: v1alpha1::Clair = Api::default_namespaced(ctx.client.clone())
        .get_status(&owner.name)
        .await?;
    let flavor = clair.spec.config_dialect.unwrap_or_default();

    let mut ct = 0;
    while ct < 3 {
        ct += 1;
        let api: Api<core::v1::ConfigMap> = Api::default_namespaced(ctx.client.clone());
        let mut entry = api.entry(&name).await?.or_insert(|| {
            trace!(%flavor, "creating ConfigMap");
            let (k, mut cm) = futures::executor::block_on(default_dropin(obj, flavor, ctx))
                .expect("dropin failed");
            let p = cm.data.as_mut().unwrap().get_mut(&k).unwrap();
            *p = p.replace("⚠️", &srvname);
            cm
        });
        let cm = entry.get_mut();
        if let Some(k) = cm.annotations().get(crate::DROPIN_LABEL.as_str()) {
            if let Some(data) = cm.data.as_ref() {
                if !data.contains_key(k) {
                    trace!(name = cm.name_any(), "ConfigMap missing key");
                    let _ = futures::executor::block_on(_req.publish(Event {
                        action: "ReconcileConfig".into(),
                        reason: "Reconcile".into(),
                        note: Some(format!("missing expected key: {k}")),
                        secondary: Some(cm.object_ref(&())),
                        type_: EventType::Warning,
                    }));
                } else {
                    trace!(name = cm.name_any(), "ConfigMap ok");
                }
            }
        }
        let key = if let Some(key) = cm.annotations().get(crate::DROPIN_LABEL.as_str()) {
            key.clone()
        } else {
            trace!(name = cm.name_any(), "skipping owner update");
            return Ok(true);
        };
        next.add_ref(cm);
        match entry.commit(&CREATE_PARAMS).await {
            Ok(()) => (),
            Err(err) => match err {
                CommitError::Validate(reason) => {
                    debug!(reason = reason.to_string(), "commit failed, retrying");
                    continue;
                }
                CommitError::Save(_) => return Err(Error::Commit(err)),
            },
        };

        trace!("attempting owner update");
        let dropin = ConfigMapKeySelector {
            name: name.clone(),
            key: key.clone(),
        };
        let api: Api<v1alpha1::Clair> = Api::default_namespaced(ctx.client.clone());
        let entry = api.entry(&owner.name).await?;
        match entry {
            Entry::Vacant(_) => (),
            Entry::Occupied(entry) => {
                let mut change = false;
                let mut entry = entry.and_modify(|c| {
                    let v = &mut c.spec.dropins;
                    if v.iter_mut().all(|d| {
                        if let Some(c) = &d.config_map_key_ref {
                            c != &dropin
                        } else {
                            true
                        }
                    }) {
                        trace!("appending dropin");
                        change = true;
                        v.push(v1alpha1::DropinSource {
                            secret_key_ref: None,
                            config_map_key_ref: Some(ConfigMapKeySelector {
                                key,
                                name: name.clone(),
                            }),
                        });
                    }
                });
                if !change {
                    debug!("no update needed");
                    return Ok(true);
                }
                match entry.commit(&CREATE_PARAMS).await {
                    Ok(()) => {
                        debug!("updated owning Clair");
                        return Ok(true);
                    }
                    Err(err) => match err {
                        CommitError::Validate(reason) => {
                            debug!(reason = reason.to_string(), "commit failed, retrying")
                        }
                        CommitError::Save(_) => return Err(Error::Commit(err)),
                    },
                };
            }
        };
    }
    Ok(false)
}

#[instrument(skip_all)]
async fn check_config(
    obj: &v1alpha1::Indexer,
    _ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    debug!(TODO = "hank", "re-check config");
    next.config = obj.spec.config.clone();
    Ok(true)
}

#[instrument(skip_all)]
async fn check_deployment(
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
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
                v1alpha1::Indexer::kind(&()).to_ascii_lowercase()
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
                            value: Some(v1alpha1::Indexer::kind(&()).to_ascii_lowercase()),
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
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    let name = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<core::v1::Service>())
        .map(|r| r.name)
        .or_else(|| {
            Some(format!(
                "{}-{}",
                obj.name_any(),
                v1alpha1::Indexer::kind(&()).to_ascii_lowercase()
            ))
        })
        .unwrap();
    let api = Api::<core::v1::Service>::default_namespaced(ctx.client.clone());

    let mut ct = 0;
    while ct < 3 {
        ct += 1;
        trace!(ct, "reconcile attempt");
        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                futures::executor::block_on(new_templated(obj, ctx)).expect("template failed")
            })
            .and_modify(|s| {
                s.labels_mut()
                    .insert(COMPONENT_LABEL.to_string(), COMPONENT.into());
            });

        next.add_ref(entry.get());
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
async fn check_hpa(
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    let name = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<autoscaling::v2::HorizontalPodAutoscaler>())
        .map(|r| r.name)
        .or_else(|| {
            Some(format!(
                "{}-{}",
                obj.name_any(),
                v1alpha1::Indexer::kind(&()).to_ascii_lowercase()
            ))
        })
        .unwrap();
    let dname = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<apps::v1::Deployment>())
        .map(|r| r.name)
        .or_else(|| {
            Some(format!(
                "{}-{}",
                obj.name_any(),
                v1alpha1::Indexer::kind(&()).to_ascii_lowercase()
            ))
        })
        .unwrap();
    let api =
        Api::<autoscaling::v2::HorizontalPodAutoscaler>::default_namespaced(ctx.client.clone());

    let mut ct = 0;
    while ct < 3 {
        ct += 1;
        trace!(ct, "reconcile attempt");
        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                futures::executor::block_on(new_templated(obj, ctx)).expect("template failed")
            })
            .and_modify(|h| {
                h.labels_mut()
                    .insert(COMPONENT_LABEL.to_string(), COMPONENT.into());
                if let Some(ref mut spec) = h.spec {
                    spec.scale_target_ref.name = dname.clone();
                };
                // TODO(hank) Check if the metrics API is enabled and if the frontend supports
                // request-per-second metrics.
            });

        next.add_ref(entry.get());
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
async fn check_creation(
    obj: &v1alpha1::Indexer,
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

#[instrument(skip_all)]
fn handle_error(_obj: Arc<v1alpha1::Indexer>, _err: &Error, _ctx: Arc<Context>) -> Action {
    Action::await_change()
}
