//! Clairs holds the controller for the "Clair" CRD.

use std::{collections::BTreeMap, sync::Arc};

use k8s_openapi::{api::core::v1::TypedLocalObjectReference, merge_strategies, DeepMerge};
use kube::{
    api::{Api, ListParams, Patch},
    client::Client,
    core::{GroupVersionKind, ObjectMeta},
    runtime::controller::Error as CtrlErr,
};
use serde_json::json;
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, prelude::*, COMPONENT_LABEL, DEFAULT_CONFIG_JSON, DEFAULT_IMAGE};
use clair_config;

static COMPONENT: &str = "clair";

/// Controller is the Clair controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let root: Api<v1alpha1::Clair> = Api::all(client.clone());
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    let ctl = Controller::new(root, ctlcfg.clone())
        .owns(
            Api::<v1alpha1::Indexer>::all(client.clone()),
            ctlcfg.clone(),
        )
        .owns(
            Api::<v1alpha1::Matcher>::all(client.clone()),
            ctlcfg.clone(),
        )
        .owns(
            Api::<v1alpha1::Notifier>::all(client.clone()),
            ctlcfg.clone(),
        )
        .owns(Api::<core::v1::Secret>::all(client.clone()), ctlcfg.clone())
        .owns(
            Api::<core::v1::ConfigMap>::all(client.clone()),
            ctlcfg.clone(),
        )
        .owns(Api::<batch::v1::Job>::all(client.clone()), ctlcfg.clone()) /*
        .owns(
            Api::<networking::v1::Ingress>::default_namespaced(client),
            ctlcfg,
        )*/
        .reconcile_all_on(sig)
        .graceful_shutdown_on(cancel.cancelled_owned());

    Ok(async move {
        info!("starting clair controller");

        let clairs: Api<v1alpha1::Clair> = Api::all(client.clone());
        if let Err(e) = clairs.list(&ListParams::default().limit(1)).await {
            error!("CRD is not queryable; {e:?}. Is the CRD installed?");
            return Err(Error::BadName("no CRD".into()));
        }

        ctl.run(reconcile, error_policy, ctx)
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

fn error_policy(obj: Arc<v1alpha1::Clair>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::requeue(Duration::from_secs(5))
}

#[instrument(skip(ctx, clair),fields(name = clair.name_any(), namespace = clair.namespace().unwrap()))]
async fn reconcile(clair: Arc<v1alpha1::Clair>, ctx: Arc<Context>) -> Result<Action> {
    let ns = clair.namespace().unwrap(); // Clair is namespace scoped
    let client = &ctx.client;
    let clairs: Api<v1alpha1::Clair> = Api::namespaced(client.clone(), &ns);

    info!("reconciling Clair");
    let owner_ref = clair.controller_owner_ref(&()).unwrap();
    let name = clair.name_any();
    let spec = &clair.spec;

    // Configuration
    'configuration_check: {
        use self::core::v1::ConfigMap;
        let api = Api::<ConfigMap>::namespaced(client.clone(), &ns);

        let mut cm = ConfigMap {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(ns.clone()),
                owner_references: Some(vec![owner_ref.clone()]),
                labels: Some(BTreeMap::from([(
                    COMPONENT_LABEL.to_string(),
                    COMPONENT.into(),
                )])),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut contents = BTreeMap::new();
        let mut dropins = spec.dropins.clone();

        contents.insert("config.json".to_string(), DEFAULT_CONFIG_JSON.into());
        if let Some(ref status) = clair.status {
            let want = ["Indexer", "Matcher"];
            if let Some(objref) = status.refs.iter().find(|&objref| {
                objref.api_group.as_ref().is_some_and(|s| s == api::GROUP)
                    && want.contains(&objref.kind.as_str())
            }) {
                let kind = objref.kind.as_str().to_ascii_lowercase();
                let status = match kind.as_str() {
                    "indexer" => Api::<v1alpha1::Indexer>::namespaced(client.clone(), &ns)
                        .get_status(&objref.name)
                        .await
                        .ok()
                        .map(|obj| obj.status),
                    "matcher" => Api::<v1alpha1::Matcher>::namespaced(client.clone(), &ns)
                        .get_status(&objref.name)
                        .await
                        .ok()
                        .map(|obj| obj.status),
                    _ => unreachable!(),
                }
                .flatten()
                .unwrap_or_default();

                if let Some(dropin) = status.dropin {
                    let key = format!("00-{kind}.json-patch");
                    contents.insert(key.clone(), dropin);
                    dropins.push(v1alpha1::DropinSource {
                        config_map_key_ref: Some(v1alpha1::ConfigMapKeySelector {
                            name: cm.name_any(),
                            key,
                        }),
                        ..Default::default()
                    });
                }
            }
        }
        cm.data = Some(contents);
        let serverside = PatchParams::apply(CONTROLLER_NAME);
        let cm = api
            .patch(&cm.name_any(), &serverside, &Patch::Apply(cm))
            .await?;

        if let Some(dbs) = &spec.databases {
            for &sec in &[&dbs.indexer, &dbs.matcher] {
                dropins.push(v1alpha1::DropinSource {
                    config_map_key_ref: None,
                    secret_key_ref: Some(sec.clone()),
                });
            }
            if let Some(ref sec) = dbs.notifier {
                dropins.push(v1alpha1::DropinSource {
                    config_map_key_ref: None,
                    secret_key_ref: Some(sec.clone()),
                });
            };
        }
        dropins.sort();
        dropins.dedup();

        let config = Some(v1alpha1::ConfigSource {
            root: v1alpha1::ConfigMapKeySelector {
                name: cm.name_any(),
                key: "config.json".into(),
            },
            dropins,
        });
        if clair.status.is_some() && clair.status.as_ref().unwrap().config == config {
            break 'configuration_check;
        }

        let status = json!({
          "status": {
            "config": config,
            "conditions": [
              Condition {
                message: "updated ConfigSource object".into(),
                observed_generation: clair.metadata.generation,
                last_transition_time: meta::v1::Time(Utc::now()),
                reason: "ConfigSourcePatched".into(),
                status: "True".into(),
                type_: clair_condition("ConfigReady"),
              }
            ],
          },
        });
        clairs
            .patch_status(&clair.name_any(), &PATCH_PARAMS, &Patch::Merge(&status))
            .await?;
    }

    // admin job
    'admin_check: {
        trace!("checking admin job");
        use batch::v1::Job;
        let api = Api::<Job>::namespaced(client.clone(), &ns);
        // Need to:
        // - Determine if the image version is going to get updated.
        //   - If not, check that the "post" job has run OK.
        //     - If not, warn somehow.
        //   - If so, check that the "pre" job has run OK.
        //     - If not, block the changes.
        if clair.status.is_none() {
            break 'admin_check;
        }
        let status = clair.status.as_ref().unwrap();
        if let Some(ref v) = status.current_version {
            if v != spec.image.as_ref().unwrap_or(&DEFAULT_IMAGE) {
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
                                break 'admin_check;
                            }
                            "False" | "Unknown" => {
                                // The Job is either running or needs to be created.
                                if let Some(j) = api.get_opt("name").await? {
                                    let s =
                                        j.status.unwrap_or_default().succeeded.unwrap_or_default();
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
    }

    'app_config_check: {
        trace!("checking subresource types");

        let clair = clairs.get_status(&clair.name_any()).await?;
        let status = if let Some(ref status) = clair.status {
            status
        } else {
            break 'app_config_check;
        };
        let config = if let Some(ref c) = status.config {
            c
        } else {
            debug!("no config on next config");
            break 'app_config_check;
        };
        let p: clair_config::Parts = load_clair_config(&ctx.client, config).await?;
        let v = p.validate().await?;
        //let action = String::from("ConfigValidation");
        //let reason = String::from("ConfigAdded");
        let message = String::from("🆗");
        let now = meta::v1::Time(Utc::now());
        let generation = clair.metadata.generation;

        let mut conditions = vec![];

        for (sub, res) in [
            (
                clair.status.as_ref().and_then(|s| s.indexer.as_ref()),
                v.indexer,
            ),
            (
                clair.status.as_ref().and_then(|s| s.matcher.as_ref()),
                v.matcher,
            ),
            (
                clair.status.as_ref().and_then(|s| s.notifier.as_ref()),
                v.notifier,
            ),
            //obj.status.as_ref().and_then(|s| s.updater.as_ref()),
        ]
        .iter()
        .filter(|(sub, _)| sub.is_some())
        .map(|(sub, res)| (sub.unwrap(), res))
        {
            /*
            use std::str::FromStr;
            let gv = sub
                .api_group
                .as_ref()
                .map(|g| kube::core::GroupVersion::from_str(g.as_str()).unwrap())
                .unwrap();
            let gvk = GroupVersionKind::gvk(&gv.group, &gv.version, &sub.kind);
            let (ar, _) = kube::discovery::pinned_kind(&ctx.client, &gvk).await?;
            let api =
                Api::<kube::api::DynamicObject>::default_namespaced_with(ctx.client.clone(), &ar);
            */
            let type_ = clair_condition(format!("{}ConfigValidated", sub.kind));

            match res {
                Ok(_) => {
                    conditions.push(Condition {
                        last_transition_time: now.clone(),
                        message: message.clone(),
                        observed_generation: generation,
                        reason: format!("{}ValidationSuccess", sub.kind),
                        status: "True".to_string(),
                        type_,
                    });
                }
                Err(err) => {
                    conditions.push(Condition {
                        last_transition_time: now.clone(),
                        message: err.to_string(),
                        observed_generation: generation,
                        reason: format!("{}ValidationFailure", sub.kind),
                        status: "False".to_string(),
                        type_,
                    });
                    continue;
                }
            };
            /*
            use json_patch::{AddOperation, Patch as JsonPatch, PatchOperation};
            let patch = JsonPatch(vec![PatchOperation::Add(AddOperation {
                path: "/spec/config".into(),
                value: serde_json::to_value(config.clone())?,
            })]);
            let patch = Patch::Json::<()>(patch);
            api.patch(&sub.name, &PATCH_PARAMS, &patch).await?;
            info!(name = sub.name, kind = sub.kind, "updated subresource");
            */

            // TODO(hank) Publish conditions
        }
    }

    'indexer_check: {
        use v1alpha1::{Indexer, IndexerSpec};

        let api = Api::<Indexer>::namespaced(client.clone(), &ns);
        let clair = clairs.get_status(&clair.name_any()).await?;
        let status = if let Some(ref status) = clair.status {
            status
        } else {
            break 'indexer_check;
        };

        let idx = Indexer {
            metadata: ObjectMeta {
                name: Some(clair.name_any()),
                owner_references: Some(vec![owner_ref.clone()]),
                labels: Some(BTreeMap::from([(
                    COMPONENT_LABEL.to_string(),
                    COMPONENT.into(),
                )])),
                ..Default::default()
            },
            spec: IndexerSpec {
                image: Some(ctx.image.clone()),
                config: status.config.clone(),
            },
            ..Default::default()
        };

        let serverside = PatchParams::apply(CONTROLLER_NAME);
        let idx = api
            .patch(&idx.name_any(), &serverside, &Patch::Apply(idx))
            .await?;

        if clair.status.is_some() && clair.status.as_ref().unwrap().indexer.is_some() {
            break 'indexer_check;
        }

        debug!("initial creation");
        let status = json!({
            "status": {
                "indexer": TypedLocalObjectReference {
                    kind: Indexer::kind(&()).to_string(),
                    api_group: Some(Indexer::api_version(&()).to_string()),
                    name: idx.name_any(),
                }
            }
        });
        clairs
            .patch_status(&clair.name_any(), &PATCH_PARAMS, &Patch::Merge(&status))
            .await?;
    }

    'matcher_check: {
        use v1alpha1::{Matcher, MatcherSpec};

        let api = Api::<Matcher>::namespaced(client.clone(), &ns);
        let clair = clairs.get_status(&clair.name_any()).await?;
        let status = if let Some(ref status) = clair.status {
            status
        } else {
            break 'matcher_check;
        };

        let idx = Matcher {
            metadata: ObjectMeta {
                name: Some(clair.name_any()),
                owner_references: Some(vec![owner_ref.clone()]),
                labels: Some(BTreeMap::from([(
                    COMPONENT_LABEL.to_string(),
                    COMPONENT.into(),
                )])),
                ..Default::default()
            },
            spec: MatcherSpec {
                image: Some(ctx.image.clone()),
                config: status.config.clone(),
            },
            ..Default::default()
        };

        let serverside = PatchParams::apply(CONTROLLER_NAME);
        let idx = api
            .patch(&idx.name_any(), &serverside, &Patch::Apply(idx))
            .await?;

        if clair.status.is_some() && clair.status.as_ref().unwrap().matcher.is_some() {
            break 'matcher_check;
        }

        debug!("initial creation");
        let status = json!({
            "status": {
                "matcher": TypedLocalObjectReference {
                    kind: Matcher::kind(&()).to_string(),
                    api_group: Some(Matcher::api_version(&()).to_string()),
                    name: idx.name_any(),
                }
            }
        });
        clairs
            .patch_status(&clair.name_any(), &PATCH_PARAMS, &Patch::Merge(&status))
            .await?;
    }

    'notifier_check: {
        use v1alpha1::{Notifier, NotifierSpec};
        if !clair.spec.notifier.unwrap_or_default() {
            break 'notifier_check;
        }

        let api = Api::<Notifier>::namespaced(client.clone(), &ns);
        let clair = clairs.get_status(&clair.name_any()).await?;
        let status = if let Some(ref status) = clair.status {
            status
        } else {
            break 'notifier_check;
        };

        let idx = Notifier {
            metadata: ObjectMeta {
                name: Some(clair.name_any()),
                owner_references: Some(vec![owner_ref.clone()]),
                labels: Some(BTreeMap::from([(
                    COMPONENT_LABEL.to_string(),
                    COMPONENT.into(),
                )])),
                ..Default::default()
            },
            spec: NotifierSpec {
                image: Some(ctx.image.clone()),
                config: status.config.clone(),
            },
            ..Default::default()
        };

        let serverside = PatchParams::apply(CONTROLLER_NAME);
        let idx = api
            .patch(&idx.name_any(), &serverside, &Patch::Apply(idx))
            .await?;

        if clair.status.is_some() && clair.status.as_ref().unwrap().notifier.is_some() {
            break 'notifier_check;
        }

        debug!("initial creation");
        let status = json!({
            "status": {
                "notifier": TypedLocalObjectReference {
                    kind: Notifier::kind(&()).to_string(),
                    api_group: Some(Notifier::api_version(&()).to_string()),
                    name: idx.name_any(),
                }
            }
        });
        clairs
            .patch_status(&clair.name_any(), &PATCH_PARAMS, &Patch::Merge(&status))
            .await?;
    }

    'endpoint_check: {
        let status = if let Some(ref s) = clair.status {
            s
        } else {
            info!("no status object, skipping");
            break 'endpoint_check;
        };
        if status.indexer.is_none() && status.matcher.is_none() && status.notifier.is_none() {
            info!("no endpoints configured, skipping");
            break 'endpoint_check;
        }

        /*
        if status.endpoint.is_none() {
            info!("no endpoint, creating");
            let objref = initialize_endpoint(obj, ctx, req).await?;
            next.endpoint = objref;
        }
        */
        let gvk = 'lookup: {
            for gvk in [
                GroupVersionKind::gvk("gateway.networking.k8s.io", "v1beta1", "Gateway"),
                GroupVersionKind::gvk("networking.k8s.io", "v1", "Ingress"),
            ] {
                if ctx.gvk_exists(&gvk).await {
                    break 'lookup Some(gvk);
                }
            }
            None
        };
        if let Some(gvk) = gvk {
            warn!(TODO = true, kind = &gvk.kind, "should create a resource");
            let name = match gvk.kind.as_str() {
                "Gateway" => "TODO".into(),
                "Ingress" => {
                    /*
                    use networking::v1::Ingress;
                    //let action = String::from("IngressCreation");
                    let ingress = new_ingress(&clair).await?;
                    let api = Api::<Ingress>::namespaced(client.clone(), &ns);
                    let ingress = api
                        .patch(&ingress.name_any(), &serverside, &Patch::Apply(ingress))
                        .await?;
                    ingress.name_any()
                    */
                    "TODO".into()
                }
                _ => unreachable!(),
            };
            let _endpoint_ref = Some(TypedLocalObjectReference {
                api_group: Some(gvk.api_version()),
                kind: gvk.kind.to_string(),
                name,
            });
        }
    }

    'ingress_check: {
        break 'ingress_check;
    }

    Ok(DEFAULT_REQUEUE.clone())
}

#[instrument(skip_all)]
async fn new_gateway(
    client: &Client,
    clair: &v1alpha1::Clair,
    spec: &v1alpha1::Gateway,
) -> Result<gateway_api::apis::standard::gateways::Gateway> {
    use gateway_api::apis::standard::*;

    let gateway_class_name = if let Some(ref name) = spec.gateway_class_name {
        name.clone()
    } else {
        let api = Api::<gatewayclasses::GatewayClass>::all(client.clone());
        let lp = ListParams::default();
        let gwcs = api.list(&lp).await?;
        let found = gwcs
            .iter()
            .find(|&gwc| {
                gwc.status
                    .as_ref()
                    .and_then(|status| {
                        status.conditions.as_ref().and_then(|cnds| {
                            cnds.iter()
                                .find(|&cnd| cnd.type_ == "Accepted" && cnd.status == "True")
                                .map(|_| true)
                        })
                    })
                    .unwrap_or_default()
            })
            .ok_or_else(|| Error::MissingName("gatewayclass"))?;
        found.name_unchecked()
    };

    let listener = if let Some(ref tls) = spec.tls {
        gateways::GatewayListeners {
            hostname: spec.hostname.clone(),
            port: 443,
            protocol: "HTTPS".into(),
            tls: Some(gateways::GatewayListenersTls {
                mode: Some(gateways::GatewayListenersTlsMode::Terminate),
                certificate_refs: Some(vec![gateways::GatewayListenersTlsCertificateRefs {
                    group: None,
                    kind: Some("Secret".into()),
                    namespace: clair.namespace(),
                    name: tls.name.clone(),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        }
    } else {
        gateways::GatewayListeners {
            hostname: spec.hostname.clone(),
            port: 80,
            protocol: "HTTP".into(),
            ..Default::default()
        }
    };
    let spec = gateways::GatewaySpec {
        gateway_class_name,
        listeners: vec![listener],
        ..Default::default()
    };
    let g = gateways::Gateway::new(&clair.name_any(), spec);
    let ns = clair.namespace().unwrap();
    let api = Api::<gateways::Gateway>::namespaced(client.clone(), &ns);

    let g = api
        .patch(
            &clair.name_any(),
            &PatchParams::apply(CONTROLLER_NAME),
            &Patch::Apply(g),
        )
        .await?;
    Ok(g)
}

#[instrument(skip_all)]
async fn new_ingress(obj: &v1alpha1::Clair) -> Result<networking::v1::Ingress> {
    use networking::v1::{
        HTTPIngressPath, Ingress, IngressBackend, IngressRule, IngressServiceBackend, IngressSpec,
        IngressTLS,
    };
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    let mut v: Ingress = templates::render(obj);
    v.metadata.owner_references = Some(vec![oref]);
    v.metadata.name = Some(obj.name_any());
    crate::set_component_label(v.meta_mut(), COMPONENT);

    let spec = v.spec.as_mut().expect("bad Ingress from template");
    // Attach TLS config if provided.
    if let Some(ref endpoint) = obj.spec.gateway {
        spec.merge_from(IngressSpec {
            tls: Some(vec![IngressTLS {
                hosts: endpoint.hostname.as_ref().map(|n| vec![n.into()]),
                secret_name: endpoint.tls.as_ref().map(|t| t.name.clone()),
            }]),
            ..Default::default()
        });
    }

    // Swap the hostname if provided.
    if let Some(hostname) = obj
        .spec
        .gateway
        .as_ref()
        .and_then(|e| e.hostname.as_deref())
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

/*
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
    let _status = if let Some(s) = &obj.status {
        s
    } else {
        trace!("no status present");
        return Ok(true);
    };

    if spec.endpoint.is_none() {
        trace!("no endpoint configured");
        return Ok(true);
    }

    let mut ct = 0;
    while ct < 3 {
        ct += 1;

        let mut entry = api
            .entry(&name)
            .await?
            .or_insert(|| {
                trace!("need to create ingress");
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
                if spec.endpoint.is_none() {
                    return;
                }
                let endpoint = spec.endpoint.as_ref().unwrap();
                trace!(?endpoint, "found endpoint");
                if endpoint.hostname.is_none() {
                    return;
                }
                let hostname = endpoint.hostname.as_ref().unwrap();
                trace!(?hostname, "found hostname");
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
*/

/*
/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "doc-controller".into(),
        }
    }
}
impl Diagnostics {
    fn recorder(&self, client: Client) -> Recorder {
        Recorder::new(client, self.reporter.clone())
    }
}
*/
