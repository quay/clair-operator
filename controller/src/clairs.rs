use std::{collections::BTreeMap, sync::Arc};

use k8s_openapi::{api::core::v1::TypedLocalObjectReference, ByteString, NamespaceResourceScope};
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    core::{GroupVersionKind, ObjectMeta},
    discovery::oneshot,
};
use serde::{de::DeserializeOwned, ser::Serialize};
use tokio::{task, time::Duration};

use crate::{
    clair_condition, prelude::*, want_dropins, COMPONENT_LABEL, DEFAULT_CONFIG_JSON,
    DEFAULT_CONFIG_YAML,
};
use clair_config;

static COMPONENT: &str = "clair";

#[instrument(skip_all)]
pub fn controller(
    set: &mut task::JoinSet<Result<()>>,
    cancel: CancellationToken,
    ctx: Arc<Context>,
) {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let wcfg = ctlcfg
        .clone()
        .labels(format!("{}={COMPONENT}", COMPONENT_LABEL.as_str()).as_str());
    let root: Api<v1alpha1::Clair> = Api::default_namespaced(client.clone());
    let ctl = Controller::new(root, ctlcfg)
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
        );
    info!("spawning clair controller");
    set.spawn(async move {
        tokio::select! {
            _ = ctl.run(reconcile, error_policy, ctx).for_each(|_| futures::future::ready(())) => debug!("clair controller finished"),
            _ = cancel.cancelled() => debug!("clair controller cancelled"),
        }
        Ok(())
    });
}

#[instrument(skip(_ctx),fields(%obj))]
fn error_policy(obj: Arc<v1alpha1::Clair>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::await_change()
}

#[instrument(skip(ctx),fields(%obj))]
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
        next.conditions.push(Condition {
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
        next.conditions.push(Condition {
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
    next.conditions.push(Condition {
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
    let name = &obj.metadata.name.as_ref().unwrap();
    let changed = obj.status.is_none() || obj.status.as_ref().unwrap() == &next;
    let mut c = v1alpha1::Clair::clone(&obj);
    c.status = Some(next);
    c.metadata.managed_fields = None; // ???

    api.patch_status(name, &PatchParams::apply(OPERATOR_NAME), &Patch::Apply(c))
        .await?;
    trace!(changed, "patched status");
    if changed {
        Ok(Action::await_change())
    } else {
        Ok(Action::requeue(Duration::from_secs(3600)))
    }
}

#[instrument(skip_all)]
async fn initialize_config(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
) -> Result<Option<v1alpha1::ConfigSource>> {
    let spec = &obj.spec;
    let params = PostParams {
        dry_run: false,
        field_manager: Some(OPERATOR_NAME.to_string()),
    };
    let action = String::from("ConfigCreation");
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    let name = &obj.metadata.name.as_ref().unwrap();

    debug!("initializing root config");
    let mut data = BTreeMap::new();
    let key = match spec.config_dialect.unwrap_or_default() {
        v1alpha1::ConfigDialect::JSON => {
            data.insert(
                "config.json".to_string(),
                ByteString(DEFAULT_CONFIG_JSON.to_vec()),
            );
            "config.json"
        }
        v1alpha1::ConfigDialect::YAML => {
            data.insert(
                "config.yaml".to_string(),
                ByteString(DEFAULT_CONFIG_YAML.to_vec()),
            );
            "config.yaml"
        }
    };

    let cfgmap = core::v1::ConfigMap {
        metadata: ObjectMeta {
            name: Some(format!("{}-config", name.as_str())),
            owner_references: Some(vec![oref]),
            ..Default::default()
        },
        binary_data: Some(data),
        ..Default::default()
    };
    let api: Api<core::v1::ConfigMap> = Api::default_namespaced(ctx.client.clone());
    let cfgmap = api.create(&params, &cfgmap).await?;
    req.publish(Event {
        type_: EventType::Normal,
        reason: "Success".into(),
        note: None,
        action,
        secondary: Some(cfgmap.object_ref(&())),
    })
    .await?;
    debug!(name = cfgmap.name_any(), "initialized root config");
    Ok(Some(v1alpha1::ConfigSource {
        root: core::v1::ConfigMapKeySelector {
            name: Some(cfgmap.name_any()),
            key: key.to_string(),
            optional: Some(false),
        },
        dropins: vec![],
    }))
}

#[instrument(skip_all)]
async fn initialize_subresource<K>(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    mut new: K,
) -> Result<Option<TypedLocalObjectReference>>
where
    K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>,
    K: DeserializeOwned,
    K: Serialize,
    K: Clone,
    K: std::fmt::Debug,
{
    let params = PostParams {
        dry_run: false,
        field_manager: Some(OPERATOR_NAME.to_string()),
    };
    let action = format!("{}Creation", K::kind(&()));
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    new.meta_mut().owner_references = Some(vec![oref]);
    let api: Api<K> = Api::default_namespaced(ctx.client.clone());

    debug!(kind = K::kind(&()).as_ref(), "initializing subresource");
    let new = api.create(&params, &new).await?;
    req.publish(Event {
        type_: EventType::Normal,
        reason: "Success".into(),
        note: None,
        action,
        secondary: Some(new.object_ref(&())),
    })
    .await?;
    debug!(
        kind = K::kind(&()).as_ref(),
        name = new.name_any(),
        "initialized subresource"
    );
    Ok(Some(TypedLocalObjectReference {
        api_group: Some(K::api_version(&()).to_string()),
        name: new.name_any(),
        kind: K::kind(&()).to_string(),
    }))
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
        field_manager: Some(OPERATOR_NAME.to_string()),
    };

    debug!("initializing endpoint");
    let avail = stream::iter(&[
        GroupVersionKind::gvk("networking.k8s.io", "v1", "Ingress"),
        GroupVersionKind::gvk("gateway.networking.k8s.io", "v1beta1", "Gateway"),
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
    if obj.status.is_none() || obj.status.as_ref().unwrap().config.is_none() {
        trace!("initializing ConfigSource");
        next.config = initialize_config(obj, ctx, req).await?;
        return Ok(false);
    }

    let spec = &obj.spec;
    let config = next.config.as_ref().unwrap();
    let action = String::from("ConfigValidation");
    let reason = String::from("ConfigAdded");

    let want = want_dropins(spec);
    let needs_update = want.iter().any(|d| !config.dropins.contains(d));
    debug!(needs_update, "ConfigSource status");
    if !needs_update {
        return Ok(true);
    }
    let config = v1alpha1::ConfigSource {
        dropins: want,
        ..config.clone()
    };

    let type_ = clair_condition("ConfigValidated");
    let v = clair_config::validate(ctx.client.clone(), &config).await?;
    let mut todo = vec![("Indexer", v.indexer), ("Matcher", v.matcher)];
    if spec.notifier.unwrap_or_default() {
        todo.push(("Notifier", v.notifier));
    }
    for (kind, res) in todo {
        let (ev, cnd) = match res {
            Ok(ws) => (
                Event {
                    type_: EventType::Normal,
                    reason: reason.clone(),
                    note: Some(format!("{ws}")),
                    action: action.clone(),
                    secondary: None, // TODO(hank) Reference the subresource.
                },
                meta::v1::Condition {
                    last_transition_time: req.now(),
                    message: "".to_string(),
                    observed_generation: obj.metadata.generation,
                    reason: format!("{}ValidationSuccess", kind),
                    status: "True".to_string(),
                    type_: type_.clone(),
                },
            ),
            Err(e) => (
                Event {
                    type_: EventType::Warning,
                    reason: reason.clone(),
                    note: Some(e.to_string()),
                    action: action.clone(),
                    secondary: None, // TODO(hank) Reference the subresource.
                },
                meta::v1::Condition {
                    last_transition_time: req.now(),
                    message: e.to_string(),
                    observed_generation: obj.metadata.generation,
                    reason: format!("{}ValidationFailure", kind),
                    status: "False".to_string(),
                    type_: type_.clone(),
                },
            ),
        };
        req.publish(ev).await?;
        next.conditions.push(cnd);
    }
    let ok = next
        .conditions
        .iter()
        .filter_map(|cnd| {
            if cnd.type_ == type_ {
                Some(cnd.status == "True")
            } else {
                None
            }
        })
        .all(|x| x);
    debug!(ok, "config validation");
    if ok {
        next.config = Some(config);
    }
    Ok(true)
}

#[instrument(skip_all)]
async fn check_ingress(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    if obj.status.is_none() || obj.status.as_ref().unwrap().endpoint.is_none() {
        next.endpoint = initialize_endpoint(obj, ctx, req).await?;
        return Ok(false);
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
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    if obj.status.is_none() || obj.status.as_ref().unwrap().indexer.is_none() {
        let new = v1alpha1::Indexer::new(
            &obj.name_any(),
            v1alpha1::IndexerSpec {
                image: Some(ctx.image.clone()),
                config: next.config.clone(),
            },
        );
        next.indexer = initialize_subresource(obj, ctx, req, new).await?;
        return Ok(false);
    }
    unimplemented!()
}
#[instrument(skip_all)]
async fn check_matcher(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    if obj.status.is_none() || obj.status.as_ref().unwrap().matcher.is_none() {
        let new = v1alpha1::Matcher::new(
            &obj.name_any(),
            v1alpha1::MatcherSpec {
                image: Some(ctx.image.clone()),
                config: next.config.clone(),
            },
        );
        next.matcher = initialize_subresource(obj, ctx, req, new).await?;
        return Ok(false);
    }
    unimplemented!()
}
#[instrument(skip_all)]
async fn check_notifier(
    obj: &v1alpha1::Clair,
    ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::ClairStatus,
) -> Result<bool> {
    if !obj.spec.notifier.unwrap_or(false) {
        return Ok(true);
    }
    if obj.status.is_none() || obj.status.as_ref().unwrap().notifier.is_none() {
        let new = v1alpha1::Notifier::new(
            &obj.name_any(),
            v1alpha1::NotifierSpec {
                image: Some(ctx.image.clone()),
                config: next.config.clone(),
            },
        );
        next.notifier = initialize_subresource(obj, ctx, req, new).await?;
        return Ok(false);
    }
    unimplemented!()
}
