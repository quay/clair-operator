use std::{collections::BTreeMap, sync::Arc};

use k8s_openapi::api::core::v1::TypedLocalObjectReference;
use kube::runtime::controller::Error as CtrlErr;
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    core::{GroupVersionKind, ObjectMeta},
    discovery::oneshot,
};
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{
    clair_condition, prelude::*, want_dropins, COMPONENT_LABEL, DEFAULT_CONFIG_JSON,
    DEFAULT_CONFIG_YAML,
};
use clair_config;

static COMPONENT: &str = "clair";

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
    let root: Api<v1alpha1::Clair> = Api::default_namespaced(client.clone());
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    let ctl = Controller::new(root, ctlcfg.clone())
        .owns(
            Api::<v1alpha1::Indexer>::default_namespaced(client.clone()),
            ctlcfg.clone(),
        )
        .owns(
            Api::<v1alpha1::Matcher>::default_namespaced(client.clone()),
            ctlcfg.clone(),
        )
        .owns(
            Api::<v1alpha1::Notifier>::default_namespaced(client.clone()),
            ctlcfg,
        )
        .owns(
            Api::<core::v1::Secret>::default_namespaced(client.clone()),
            wcfg.clone(),
        )
        .owns(
            Api::<core::v1::ConfigMap>::default_namespaced(client.clone()),
            wcfg.clone(),
        )
        .owns(
            Api::<networking::v1::Ingress>::default_namespaced(client),
            wcfg,
        )
        .reconcile_all_on(sig)
        .graceful_shutdown_on(cancel.cancelled_owned());

    Ok(async move {
        info!("starting clair controller");
        ctl.run(reconcile, error_policy, ctx)
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
        debug!("clair controller finished");
        Ok(())
    }
    .boxed())
}

#[instrument(skip_all)]
fn error_policy(obj: Arc<v1alpha1::Clair>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::await_change()
}

#[instrument(skip_all)]
async fn reconcile(obj: Arc<v1alpha1::Clair>, ctx: Arc<Context>) -> Result<Action> {
    trace!("start");
    let req = Request::new(&ctx.client, obj.object_ref(&()));

    // TODO(hank) Add a gate struct that does all the rangefinding.

    assert!(obj.meta().name.is_some());
    let spec = &obj.spec;
    let mut next = if let Some(status) = &obj.status {
        status.clone()
    } else {
        trace!("no status present");
        Default::default()
    };

    // First, check that the databases are filled out:
    let action = "CheckDatabases".into();
    let type_ = clair_condition("SpecOK");
    trace!("databases check");
    if spec.databases.is_none() {
        req.publish(Event {
            action,
            type_: EventType::Warning,
            secondary: None,
            reason: "SpecIncomplete".into(),
            note: Some("\"/spec/databases\" must be populated".into()),
        })
        .await?;
        next.add_condition(Condition {
            last_transition_time: req.now(),
            observed_generation: obj.metadata.generation,
            message: "\"/spec/databases\" must be populated".into(),
            reason: "SpecIncomplete".into(),
            status: "False".into(),
            type_,
        });
        trace!("databases missing");
        return publish(obj, ctx, req, next).await;
    };
    if spec.notifier.unwrap_or(false) && spec.databases.as_ref().unwrap().notifier.is_none() {
        req.publish(Event {
            action,
            type_: EventType::Warning,
            secondary: None,
            reason: "SpecIncomplete".into(),
            note: Some("\"/spec/databases/notifier\" must be populated".into()),
        })
        .await?;
        next.add_condition(Condition {
            last_transition_time: req.now(),
            observed_generation: obj.metadata.generation,
            message: "\"/spec/databases/notifier\" must be populated".into(),
            reason: "SpecIncomplete".into(),
            status: "False".into(),
            type_,
        });
        trace!("databases missing (notifier)");
        return publish(obj, ctx, req, next).await;
    }
    trace!("databases ok");
    next.add_condition(Condition {
        last_transition_time: req.now(),
        observed_generation: obj.metadata.generation,
        message: "".into(),
        reason: "SpecComplete".into(),
        status: "True".into(),
        type_,
    });
    trace!("spec ok");

    // The spec should have enough information to describe the desired state.

    // Need to use a macro instead of a slice to work around async functions having distinct types
    // despite having the same signature.
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
        check_ingress,
        check_indexer,
        check_matcher,
        check_notifier,
    );

    trace!("done");
    publish(obj, ctx, req, next).await
}

#[instrument(skip_all)]
async fn publish(
    obj: Arc<v1alpha1::Clair>,
    ctx: Arc<Context>,
    _req: Request,
    next: v1alpha1::ClairStatus,
) -> Result<Action> {
    let api: Api<v1alpha1::Clair> = Api::default_namespaced(ctx.client.clone());
    let name = obj.name_any();

    let prev = obj.metadata.resource_version.clone().unwrap();
    let mut cur = None;
    let mut c = v1alpha1::Clair::new(&name, Default::default());
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
async fn initialize_endpoint(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
) -> Result<Option<TypedLocalObjectReference>> {
    use futures::stream;
    let params = PostParams {
        dry_run: false,
        field_manager: Some(CONTROLLER_NAME.to_string()),
    };

    debug!("initializing endpoint");
    let avail = stream::iter(&[
        GroupVersionKind::gvk("networking.k8s.io", "v1", "Ingress"),
        //GroupVersionKind::gvk("gateway.networking.k8s.io", "v1beta1", "Gateway"),
    ])
    .filter_map(|gvk| async { oneshot::pinned_kind(&ctx.client, gvk).await.ok() })
    .collect::<Vec<_>>()
    .await;
    let ar = if let Some((ar, _)) = avail.first() {
        ar
    } else {
        return Ok(None);
    };
    debug!(kind = ar.kind, "discoved endpoint kind");

    let name = match ar.kind.as_str() {
        "Gateway" => unimplemented!(), // TODO(hank) Support a Gateway.
        "Ingress" => {
            let action = String::from("IngressCreation");
            let ingress = new_ingress(obj, ctx, req).await?;
            let api = Api::<networking::v1::Ingress>::default_namespaced(ctx.client.clone());
            let ingress = api.create(&params, &ingress).await;
            match ingress {
                Ok(v) => {
                    req.publish(Event {
                        type_: EventType::Warning,
                        reason: "Success".into(),
                        note: None,
                        action,
                        secondary: Some(v.object_ref(&())),
                    })
                    .await?;
                    Ok(v.name_any())
                }
                Err(e) => {
                    req.publish(Event {
                        type_: EventType::Warning,
                        reason: "Failed".into(),
                        note: Some(e.to_string()),
                        action,
                        secondary: None,
                    })
                    .await?;
                    Err(e)
                }
            }
        }
        _ => unreachable!(),
    }?;
    debug!(kind = ar.kind, name, "initialized endpoint");
    Ok(Some(TypedLocalObjectReference {
        api_group: Some(ar.api_version.to_string()),
        kind: ar.kind.to_string(),
        name,
    }))
}

#[instrument(skip_all)]
async fn new_ingress(
    obj: &v1alpha1::Clair,
    _ctx: &Context,
    _req: &Request,
) -> Result<networking::v1::Ingress> {
    use networking::v1::IngressTLS;
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    let mut v: networking::v1::Ingress = match templates::resource_for("clair").await {
        Ok(v) => v,
        Err(err) => return Err(Error::Assets(err.to_string())),
    };
    v.metadata.owner_references = Some(vec![oref]);
    v.metadata.name = Some(obj.name_any());
    crate::set_component_label(v.meta_mut(), COMPONENT);
    let spec = v.spec.as_mut().expect("bad Ingress from template");
    // Attach TLS config if provided.
    if let Some(ref endpoint) = obj.spec.endpoint {
        if let Some(ref tls) = endpoint.tls {
            spec.tls.get_or_insert_with(Vec::new).push(IngressTLS {
                hosts: endpoint.hostname.as_ref().map(|name| vec![name.into()]),
                secret_name: tls.name.clone(),
            });
        }
    }
    // Swap the hostname if provided.
    if let Some(rules) = spec.rules.as_mut() {
        for r in rules.iter_mut().filter(|r| r.host == Some("⚠️".to_string())) {
            let mut ok = false;
            if let Some(ref endpoint) = obj.spec.endpoint {
                if let Some(ref name) = endpoint.hostname {
                    r.host = Some(name.clone());
                    ok = true;
                }
            }
            if !ok {
                r.host = None;
            }
            if let Some(http) = r.http.as_mut() {
                for p in http.paths.iter_mut() {
                    if let Some(srv) = p.backend.service.as_mut() {
                        srv.name = srv.name.replace("⚠️", &obj.name_any());
                    }
                }
            }
        }
    }
    Ok(v)
}

#[instrument(skip_all)]
async fn check_config(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    use self::core::v1::ConfigMap;

    let spec = &obj.spec;
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    let name = format!("{}-config", obj.name_any());
    let mut ev: Option<Event> = None;
    let api: Api<core::v1::ConfigMap> = Api::default_namespaced(ctx.client.clone());

    let flavor = spec.config_dialect.unwrap_or_default();

    let mut entry = api
        .entry(&name)
        .await?
        .or_insert(|| {
            trace!(owner=?oref, "created ConfigMap");
            ev = Some(Event {
                action: "CreateConfig".into(),
                reason: "Initialization".into(),
                secondary: None,
                note: None,
                type_: EventType::Normal,
            });
            ConfigMap {
                metadata: ObjectMeta {
                    name: Some(name.clone()),
                    owner_references: Some(vec![oref]),
                    labels: Some(BTreeMap::from([(
                        COMPONENT_LABEL.to_string(),
                        COMPONENT.into(),
                    )])),
                    annotations: Some(BTreeMap::from([(
                        PARENT_VERSION_ANNOTATION.to_string(),
                        obj.resource_version().unwrap_or_default(),
                    )])),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .and_modify(|cm| {
            if let Some(ref mut ev) = &mut ev {
                ev.secondary = Some(cm.object_ref(&()));
            };
            cm.annotations_mut().insert(
                PARENT_VERSION_ANNOTATION.to_string(),
                obj.resource_version().unwrap_or_default(),
            );
            let data = cm.data.get_or_insert_with(BTreeMap::default);
            match flavor {
                v1alpha1::ConfigDialect::JSON => data.insert(
                    "config.json".into(),
                    String::from_utf8(DEFAULT_CONFIG_JSON.to_vec())
                        .expect("programmer error: default config not utf-8"),
                ),
                v1alpha1::ConfigDialect::YAML => data.insert(
                    "config.yaml".into(),
                    String::from_utf8(DEFAULT_CONFIG_YAML.to_vec())
                        .expect("programmer error: default config not utf-8"),
                ),
            };
        });
    entry.commit(&CREATE_PARAMS).await?;
    let key = format!("config.{flavor}");
    if next.config.is_none() {
        debug!("initial creation");
        next.config = Some(v1alpha1::ConfigSource {
            root: core::v1::ConfigMapKeySelector {
                name: Some(name),
                key: key.clone(),
                optional: Some(false),
            },
            dropins: vec![],
        });
        if let Some(ev) = ev {
            req.publish(ev).await?;
        };
    }

    let mut config = next.config.clone().unwrap();
    let want = want_dropins(spec);
    let needs_update = want.iter().any(|d| !config.dropins.contains(d));
    debug!(needs_update, "ConfigSource status");
    if needs_update {
        config.dropins = want;
    }

    let v = clair_config::validate(ctx.client.clone(), &config).await?;
    let action = String::from("ConfigValidation");
    let reason = String::from("ConfigAdded");
    let message = String::from("🆗");
    for (sub, res) in [
        (
            obj.status.as_ref().and_then(|s| s.indexer.as_ref()),
            v.indexer,
        ),
        (
            obj.status.as_ref().and_then(|s| s.matcher.as_ref()),
            v.matcher,
        ),
        (
            obj.status.as_ref().and_then(|s| s.notifier.as_ref()),
            v.notifier,
        ),
        //obj.status.as_ref().and_then(|s| s.updater.as_ref()),
    ]
    .iter()
    .filter(|(sub, _)| sub.is_some())
    .map(|(sub, res)| (sub.unwrap(), res))
    {
        use std::str::FromStr;
        let gv = sub
            .api_group
            .as_ref()
            .map(|g| kube::core::GroupVersion::from_str(g.as_str()).unwrap())
            .unwrap();
        let gvk = GroupVersionKind::gvk(&gv.group, &gv.version, &sub.kind);
        let (ar, _) = kube::discovery::pinned_kind(&ctx.client, &gvk).await?;
        let api = Api::<kube::api::DynamicObject>::default_namespaced_with(ctx.client.clone(), &ar);
        let type_ = clair_condition(format!("{}ConfigValidated", sub.kind));
        debug!(
            kind = sub.kind,
            name = sub.name,
            "updating dependent resource"
        );

        match res {
            Ok(_) => {
                next.add_condition(Condition {
                    last_transition_time: req.now(),
                    message: message.clone(),
                    observed_generation: obj.metadata.generation,
                    reason: format!("{}ValidationSuccess", sub.kind),
                    status: "True".to_string(),
                    type_,
                });
            }
            Err(err) => {
                next.add_condition(Condition {
                    last_transition_time: req.now(),
                    message: err.to_string(),
                    observed_generation: obj.metadata.generation,
                    reason: format!("{}ValidationFailure", sub.kind),
                    status: "False".to_string(),
                    type_,
                });
                let _ = req
                    .publish(Event {
                        type_: EventType::Warning,
                        reason: reason.clone(),
                        note: Some(err.to_string()),
                        action: action.clone(),
                        secondary: None, // TODO(hank) Reference the subresource.
                    })
                    .await;
                info!(
                    kind = sub.kind,
                    name = sub.name,
                    "config validation failed, skipping"
                );
                continue;
            }
        };

        let patch = serde_json::json!({
            "apiVersion": sub.api_group,
            "kind": sub.kind,
            "metadata": {
                "name": sub.name,
            },
            "spec": {
                "config": config.clone(),
            },
        });
        trace!(%patch, "applying patch");
        let patch = Patch::Merge(patch);
        let params = PatchParams::apply(CONTROLLER_NAME);
        api.patch(&sub.name, &params, &patch).await?;
        info!(name = sub.name, kind = sub.kind, "updated subresource");
    }
    next.config = Some(config);
    Ok(true)
}

#[instrument(skip_all)]
async fn check_ingress(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    use self::networking::v1::Ingress;

    let api = Api::<Ingress>::default_namespaced(ctx.client.clone());
    let name = obj.name_any();

    let mut ct = 0;
    while ct < 3 {
        ct += 1;

        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                futures::executor::block_on(new_ingress(obj, ctx, req)).expect("template failed")
            })
            .and_modify(|_ing| warn!(TOOD = "hank", "reconcile Ingress"));
        next.endpoint = {
            let name = entry.get().name_any();
            Some(TypedLocalObjectReference {
                kind: Ingress::kind(&()).to_string(),
                api_group: Some(Ingress::api_version(&()).to_string()),
                name,
            })
        };
        match entry.commit(&CREATE_PARAMS).await {
            Ok(()) => break,
            Err(err) => match err {
                CommitError::Validate(reason) => {
                    debug!(reason = reason.to_string(), "commit failed, retrying")
                }
                CommitError::Save(_) => return Err(Error::Commit(err)),
            },
        };
    }

    // For now, assume that if it exists it's fine.
    // In the future:
    // - cross-check the hostname
    // - cross-check the tls

    Ok(true)
}

#[instrument(skip_all)]
async fn check_indexer(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    let api = Api::<v1alpha1::Indexer>::default_namespaced(ctx.client.clone());
    let name = obj.name_any();

    let mut ct = 0;
    while ct < 3 {
        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                debug!("creating indexer");
                let mut idx = v1alpha1::Indexer::new(&obj.name_any(), Default::default());
                idx.labels_mut()
                    .entry(COMPONENT_LABEL.to_string())
                    .or_insert_with(|| COMPONENT.to_string());
                idx.owner_references_mut().push(
                    obj.controller_owner_ref(&())
                        .expect("unable to create owner ref"),
                );
                idx
            })
            .and_modify(|idx| {
                idx.spec.image = Some(ctx.image.clone());
                idx.spec.config = next.config.clone();
                idx.annotations_mut()
                    .entry(PARENT_VERSION_ANNOTATION.to_string())
                    .and_modify(|v| *v = obj.resource_version().unwrap())
                    .or_insert_with(|| obj.resource_version().unwrap());
            });
        next.indexer = {
            let idx = entry.get();
            Some(TypedLocalObjectReference {
                kind: v1alpha1::Indexer::kind(&()).to_string(),
                api_group: Some(v1alpha1::Indexer::api_version(&()).to_string()),
                name: idx.name_any(),
            })
        };
        match entry.commit(&CREATE_PARAMS).await {
            Ok(()) => break,
            Err(err) => match err {
                CommitError::Validate(reason) => {
                    debug!(reason = reason.to_string(), "commit failed, retrying")
                }
                CommitError::Save(_) => return Err(Error::Commit(err)),
            },
        };
        ct += 1;
    }
    debug!("indexer up-to-date");
    Ok(true)
}

#[instrument(skip_all)]
async fn check_matcher(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    let api = Api::<v1alpha1::Matcher>::default_namespaced(ctx.client.clone());
    let name = obj.name_any();

    let mut ct = 0;
    while ct < 3 {
        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                debug!("creating matcher");
                let mut idx = v1alpha1::Matcher::new(&obj.name_any(), Default::default());
                idx.labels_mut()
                    .entry(COMPONENT_LABEL.to_string())
                    .or_insert_with(|| COMPONENT.to_string());
                idx.owner_references_mut().push(
                    obj.controller_owner_ref(&())
                        .expect("unable to create owner ref"),
                );
                idx
            })
            .and_modify(|idx| {
                idx.spec.image = Some(ctx.image.clone());
                idx.spec.config = next.config.clone();
                idx.annotations_mut()
                    .entry(PARENT_VERSION_ANNOTATION.to_string())
                    .and_modify(|v| *v = obj.resource_version().unwrap())
                    .or_insert_with(|| obj.resource_version().unwrap());
            });
        next.matcher = {
            let idx = entry.get();
            Some(TypedLocalObjectReference {
                kind: v1alpha1::Matcher::kind(&()).to_string(),
                api_group: Some(v1alpha1::Matcher::api_version(&()).to_string()),
                name: idx.name_any(),
            })
        };
        match entry.commit(&CREATE_PARAMS).await {
            Ok(()) => break,
            Err(err) => match err {
                CommitError::Validate(reason) => {
                    debug!(reason = reason.to_string(), "commit failed, retrying")
                }
                CommitError::Save(_) => return Err(Error::Commit(err)),
            },
        };
        ct += 1;
    }
    debug!("matcher up-to-date");
    Ok(true)
}

#[instrument(skip_all)]
async fn check_notifier(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    if !obj.spec.notifier.unwrap_or(false) {
        trace!("notifier not asked for");
        return Ok(true);
    }
    let api = Api::<v1alpha1::Notifier>::default_namespaced(ctx.client.clone());
    let name = obj.name_any();

    let mut ct = 0;
    while ct < 3 {
        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                debug!("creating notifier");
                let mut idx = v1alpha1::Notifier::new(&obj.name_any(), Default::default());
                idx.labels_mut()
                    .entry(COMPONENT_LABEL.to_string())
                    .or_insert_with(|| COMPONENT.to_string());
                idx.owner_references_mut().push(
                    obj.controller_owner_ref(&())
                        .expect("unable to create owner ref"),
                );
                idx
            })
            .and_modify(|idx| {
                idx.spec.image = Some(ctx.image.clone());
                idx.spec.config = next.config.clone();
                idx.annotations_mut()
                    .entry(PARENT_VERSION_ANNOTATION.to_string())
                    .and_modify(|v| *v = obj.resource_version().unwrap())
                    .or_insert_with(|| obj.resource_version().unwrap());
            });
        next.notifier = {
            let idx = entry.get();
            Some(TypedLocalObjectReference {
                kind: v1alpha1::Notifier::kind(&()).to_string(),
                api_group: Some(v1alpha1::Notifier::api_version(&()).to_string()),
                name: idx.name_any(),
            })
        };
        match entry.commit(&CREATE_PARAMS).await {
            Ok(()) => break,
            Err(err) => match err {
                CommitError::Validate(reason) => {
                    debug!(reason = reason.to_string(), "commit failed, retrying")
                }
                CommitError::Save(_) => return Err(Error::Commit(err)),
            },
        };
        ct += 1;
    }
    debug!("notifier up-to-date");
    Ok(true)
}
