//! Clairs holds the controller for the "Clair" CRD.

use std::{collections::BTreeMap, sync::Arc};

use k8s_openapi::{api::core::v1::TypedLocalObjectReference, merge_strategies, DeepMerge};
use kube::runtime::controller::Error as CtrlErr;
use kube::{
    api::{Api, Patch, PostParams},
    core::{GroupVersionKind, ObjectMeta},
    discovery::oneshot,
};
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{
    clair_condition, prelude::*, COMPONENT_LABEL, DEFAULT_CONFIG_JSON, DEFAULT_CONFIG_YAML,
    DEFAULT_IMAGE,
};
use clair_config;

static COMPONENT: &str = "clair";

/// Controller is the Clair controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
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
            ctlcfg.clone(),
        )
        .owns(
            Api::<core::v1::Secret>::default_namespaced(client.clone()),
            ctlcfg.clone(),
        )
        .owns(
            Api::<core::v1::ConfigMap>::default_namespaced(client.clone()),
            ctlcfg.clone(),
        )
        .owns(
            Api::<batch::v1::Job>::default_namespaced(client.clone()),
            ctlcfg.clone(),
        )
        .owns(
            Api::<networking::v1::Ingress>::default_namespaced(client),
            ctlcfg,
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
                        CtrlErr::RunnerError(error) => error!(%error, "runner error"),
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

fn error_policy(obj: Arc<v1alpha1::Clair>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::requeue(Duration::from_secs(1))
}

#[instrument(skip_all)]
async fn reconcile(obj: Arc<v1alpha1::Clair>, ctx: Arc<Context>) -> Result<Action> {
    trace!("start");
    let req = Request::new(&ctx.client, obj.object_ref(&()));

    // TODO(hank) Add a gate struct that does all the rangefinding.

    assert!(obj.meta().name.is_some());
    let spec = &obj.spec;
    let mut next = obj.status.clone().unwrap_or_default();

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
        check_dropins,
        check_admin_job,
        check_subs,
        check_ingress,
        check_indexer,
        check_matcher,
        check_notifier,
    );

    publish(obj, ctx, req, next).await
}

#[instrument(skip_all)]
async fn publish(
    obj: Arc<v1alpha1::Clair>,
    ctx: Arc<Context>,
    _req: Request,
    next: v1alpha1::ClairStatus,
) -> Result<Action> {
    trace!("publishing updates");
    let api: Api<v1alpha1::Clair> = Api::default_namespaced(ctx.client.clone());
    let name = obj.name_any();

    let prev = obj.metadata.resource_version.clone().unwrap();
    let mut cur = None;
    let mut ct = 0;
    while ct < 3 {
        ct += 1;
        let patch = serde_json::json!({ "status": &next });
        match api
            .patch_status(&name, &PATCH_PARAMS, &Patch::Merge(patch))
            .await
        {
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
                    let _ = req
                        .publish(Event {
                            type_: EventType::Warning,
                            reason: "Success".into(),
                            note: None,
                            action,
                            secondary: Some(v.object_ref(&())),
                        })
                        .await;
                    debug!(name = v.name_any(), "ingress created");
                    Ok(v.name_any())
                }
                Err(e) => {
                    let _ = req
                        .publish(Event {
                            type_: EventType::Warning,
                            reason: "Failed".into(),
                            note: Some(e.to_string()),
                            action,
                            secondary: None,
                        })
                        .await;
                    error!(error = ?e, "ingress creation failure");
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
    use networking::v1::{
        HTTPIngressPath, IngressBackend, IngressRule, IngressServiceBackend, IngressSpec,
        IngressTLS,
    };
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
        spec.merge_from(IngressSpec {
            tls: Some(vec![IngressTLS {
                hosts: endpoint.hostname.as_ref().map(|n| vec![n.into()]),
                secret_name: endpoint.tls.as_ref().and_then(|ref t| t.name.clone()),
            }]),
            ..Default::default()
        });
    }
    // Swap the hostname if provided.
    if let Some(hostname) = obj
        .spec
        .endpoint
        .as_ref()
        .and_then(|e| e.hostname.as_ref().map(|s| s.as_str()))
    {
        let rule = spec
            .rules
            .as_mut()
            .expect("template should have rules")
            .first_mut()
            .expect("template should have one entry");
        rule.merge_from(IngressRule {
            host: Some(hostname.into()),
            ..Default::default()
        });
        let paths = rule
            .http
            .as_mut()
            .expect("template should have http rule")
            .paths
            .as_mut();
        merge_strategies::list::map(
            paths,
            ["indexer", "matcher", "notifier"]
                .iter()
                .map(|n| HTTPIngressPath {
                    path: Some(format!("/{n}")),
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: format!("{n}-{hostname}"),
                            port: None,
                        }),
                        resource: None,
                    },
                    ..Default::default()
                })
                .collect::<Vec<HTTPIngressPath>>(),
            &[|a, b| a.path == b.path],
            |a, b| a.merge_from(b),
        )
    }
    trace!("created Ingress");
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
            trace!("created ConfigMap");
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
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .and_modify(|cm| {
            if let Some(ref mut ev) = &mut ev {
                ev.secondary = Some(cm.object_ref(&()));
            };
            let data = cm.data.get_or_insert_with(BTreeMap::default);
            let key = match flavor {
                v1alpha1::ConfigDialect::JSON => "config.json".into(),
                v1alpha1::ConfigDialect::YAML => "config.yaml".into(),
            };
            data.entry(key).or_insert(match flavor {
                v1alpha1::ConfigDialect::JSON => String::from_utf8(DEFAULT_CONFIG_JSON.to_vec())
                    .expect("programmer error: default config not utf-8"),
                v1alpha1::ConfigDialect::YAML => String::from_utf8(DEFAULT_CONFIG_YAML.to_vec())
                    .expect("programmer error: default config not utf-8"),
            });
        });
    entry.commit(&CREATE_PARAMS).await?;
    let key = format!("config.{flavor}");
    if next.config.is_none() {
        debug!("initial creation");
        next.config = Some(v1alpha1::ConfigSource {
            root: v1alpha1::ConfigMapKeySelector {
                name,
                key: key.clone(),
            },
            dropins: vec![],
        });
        if let Some(ev) = ev {
            req.publish(ev).await?;
        };
    } else if let Some(cfg) = next.config.as_mut() {
        cfg.root.name = name;
    }
    Ok(true)
}

//#[instrument(skip_all)]
async fn check_admin_job(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    _req: &Request,
    _next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    trace!("checking admin job");
    let ns = obj.namespace().unwrap_or("default".to_string());
    let api: Api<batch::v1::Job> = Api::namespaced(ctx.client.clone(), &ns);
    // Need to:
    // - Determine if the image version is going to get updated.
    //   - If not, check that the "post" job has run OK.
    //     - If not, warn somehow.
    //   - If so, check that the "pre" job has run OK.
    //     - If not, block the changes.
    if obj.status.is_none() {
        return Ok(true);
    }
    let status = obj.status.as_ref().unwrap();
    if let Some(ref v) = status.current_version {
        if v != &obj.spec.image_default(&DEFAULT_IMAGE) {
            // Version mismatch, find out why.
        } else {
            // Version is current, check for post job.
            let cnd_post = clair_condition("AdminPostComplete");
            match status.conditions.iter().find(|c| c.type_ == cnd_post) {
                Some(c) => {
                    // Condition exists
                    match c.status.as_str() {
                        "True" => {
                            // Great, done.
                            return Ok(true);
                        }
                        "False" | "Unknown" => {
                            // The Job is either running or needs to be created.
                            if let Some(j) = api.get_opt("name").await? {
                                let s = j.status.unwrap_or_default().succeeded.unwrap_or_default();
                                debug!("succeeded: {}", s);
                            } else {
                                unimplemented!("create?")
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                None => {
                    // No post job, seemingly. Check for Job creation.
                    unimplemented!("no post job")
                }
            };
        }
    } else {
        // TODO(hank) In setup, no current version.
    }
    Ok(true)
}

#[instrument(skip_all)]
async fn check_subs(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    trace!("checking subresource types");

    let config = if let Some(c) = next.config.as_ref() {
        c.clone()
    } else {
        debug!("no config on next config");
        return Ok(true);
    };
    let p: clair_config::Parts = load_clair_config(&ctx.client, &config).await?;
    let v = p.validate().await?;
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
        use json_patch::{AddOperation, Patch as JsonPatch, PatchOperation};
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

        let patch = JsonPatch(vec![PatchOperation::Add(AddOperation {
            path: "/spec/config".into(),
            value: serde_json::to_value(config.clone())?,
        })]);
        let patch = Patch::Json::<()>(patch);
        api.patch(&sub.name, &PATCH_PARAMS, &patch).await?;
        info!(name = sub.name, kind = sub.kind, "updated subresource");
    }
    trace!("done");
    Ok(true)
}

#[instrument(skip_all)]
async fn load_clair_config(
    client: &kube::Client,
    cfgsrc: &v1alpha1::ConfigSource,
) -> Result<clair_config::Parts> {
    use clair_config::Builder;
    let cm_api: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
    let sec_api: Api<core::v1::Secret> = Api::default_namespaced(client.clone());

    let root = cm_api
        .get_opt(&cfgsrc.root.name)
        .await?
        .ok_or_else(|| Error::BadName(format!("no such config: {}", &cfgsrc.root.name)))?;

    let mut b = Builder::from_root(&root, &cfgsrc.root.key)?;
    for d in cfgsrc.dropins.iter() {
        if let Some(r) = &d.config_map_key_ref {
            let name = &r.name;
            let m = cm_api
                .get_opt(name)
                .await?
                .ok_or_else(|| Error::BadName(format!("no such config: {name}")))?;
            b = b.add(m, &r.key)?;
        } else if let Some(r) = &d.secret_key_ref {
            let name = &r.name;
            let m = sec_api
                .get_opt(name)
                .await?
                .ok_or_else(|| Error::BadName(format!("no such config: {name}")))?;
            b = b.add(m, &r.key)?;
        } else {
            unreachable!()
        }
    }
    Ok(b.into())
}

#[instrument(skip_all)]
async fn check_dropins(
    obj: &v1alpha1::Clair,
    _ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    let spec = &obj.spec;
    let config = next.config.as_mut().unwrap();

    let mut want = spec.dropins.clone();
    if let Some(dbs) = &spec.databases {
        for &sec in &[&dbs.indexer, &dbs.matcher] {
            want.push(v1alpha1::DropinSource {
                config_map_key_ref: None,
                secret_key_ref: Some(sec.clone()),
            });
        }
        if let Some(ref sec) = dbs.notifier {
            want.push(v1alpha1::DropinSource {
                config_map_key_ref: None,
                secret_key_ref: Some(sec.clone()),
            });
        };
    }
    want.sort();
    want.dedup();
    let needs_update = want.iter().any(|d| !config.dropins.contains(d));
    debug!(needs_update, "ConfigSource status");
    config.dropins = want;
    Ok(true)
}

#[instrument(skip_all)]
async fn check_ingress(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    use self::networking::v1::{Ingress, IngressTLS};

    let api = Api::<Ingress>::default_namespaced(ctx.client.clone());
    let name = obj.name_any();
    let spec = &obj.spec;

    let mut ct = 0;
    while ct < 3 {
        ct += 1;

        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                futures::executor::block_on(new_ingress(obj, ctx, req)).expect("template failed")
            })
            .and_modify(|ing| {
                let tgt = ing.spec.as_mut().expect("invalid IngressSpec");
                if let Some(rules) = &tgt.rules {
                    if rules.len() != 1 {
                        info!(
                            name,
                            reason = "rules array",
                            "Ingress in unexepected state, skipping."
                        );
                        return;
                    }
                }
                if let Some(tls) = &tgt.tls {
                    if tls.len() > 1 {
                        info!(
                            name,
                            reason = "tls array",
                            "Ingress in unexepected state, skipping."
                        );
                        return;
                    }
                }
                if let Some(endpoint) = &spec.endpoint {
                    if let Some(hostname) = &endpoint.hostname {
                        if let Some(tls) = &endpoint.tls {
                            if let Some(tgt) = tgt.tls.as_mut() {
                                match tgt.len() {
                                    0 => {
                                        tgt.push(IngressTLS {
                                            hosts: Some(vec![hostname.clone()]),
                                            secret_name: tls.name.clone(),
                                        });
                                    }
                                    1 => {
                                        tgt[0].hosts = Some(vec![hostname.clone()]);
                                        tgt[0].secret_name = tls.name.clone();
                                    }
                                    _ => unreachable!(),
                                };
                            }
                        }
                        tgt.rules.as_mut().expect("rules missing")[0].host = Some(hostname.clone());
                    }
                }
            });

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
                CommitError::Save(reason) => {
                    debug!(reason = reason.to_string(), "save failed, retrying")
                }
            },
        };
    }
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