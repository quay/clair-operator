use std::{collections::BTreeMap, env, sync::Arc};

use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::{
    api::{core::v1::TypedLocalObjectReference, networking},
    serde::{de::DeserializeOwned, ser::Serialize},
    NamespaceResourceScope,
};
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    core::{GroupVersionKind, ObjectMeta},
    runtime::{
        controller::{Action, Controller},
        events::{Event, EventType, Recorder, Reporter},
        watcher,
    },
    Client, Discovery, Resource,
};
use tokio::{task, time::Duration};

use crate::*;

struct Context {
    client: Client,
}

fn error_policy(_obj: Arc<v1alpha1::Clair>, _e: &Error, _ctx: Arc<Context>) -> Action {
    debug!("error!");
    Action::await_change()
}

async fn reconcile(obj: Arc<v1alpha1::Clair>, ctx: Arc<Context>) -> Result<Action> {
    let reporter = Reporter {
        controller: OPERATOR_NAME.clone(),
        instance: env::var("CONTROLLER_POD_NAME").ok(),
    };
    let recorder = Recorder::new(ctx.client.clone(), reporter, obj.object_ref(&()));
    debug!("reconcile!");
    if obj.status.is_none() {
        // Initial reconcile.
        return initialize(obj, ctx, recorder).await;
    }
    let spec = &obj.spec;
    let status = obj.status.as_ref().unwrap();
    //let rev = obj.metadata.resource_version.as_ref().unwrap().to_string();
    let mut next = status.to_owned();

    if let Some(config) = status.config.as_ref() {
        // Check that our dropins are correct:
        if spec.dropins.iter().any(|d| !config.dropins.contains(d)) {
            let (cnds, ok) = check_config(obj.clone(), ctx.clone(), &recorder).await?;
            if ok {
                next.config.as_mut().unwrap().dropins = spec.dropins.clone();
            }
            next.conditions.extend(cnds);
        }
    }

    let api: Api<v1alpha1::Clair> = Api::default_namespaced(ctx.client.clone());
    let pp = PatchParams::apply(OPERATOR_NAME.as_str());
    let p = Patch::Apply(next);

    api.patch(obj.meta().name.as_ref().unwrap(), &pp, &p)
        .await?;
    Ok(Action::requeue(Duration::from_secs(300)))
}

async fn initialize(
    obj: Arc<v1alpha1::Clair>,
    ctx: Arc<Context>,
    recorder: Recorder,
) -> Result<Action> {
    let mut next: v1alpha1::ClairStatus = Default::default();
    let name = &obj.metadata.name.as_ref().unwrap();

    next.config = initialize_config(obj.clone(), ctx.clone(), &recorder).await?;
    next.indexer = initialize_subresource(
        obj.clone(),
        ctx.clone(),
        &recorder,
        v1alpha1::Indexer::new(&name, Default::default()),
    )
    .await?;
    next.matcher = initialize_subresource(
        obj.clone(),
        ctx.clone(),
        &recorder,
        v1alpha1::Matcher::new(&name, Default::default()),
    )
    .await?;
    next.notifier = initialize_subresource(
        obj.clone(),
        ctx.clone(),
        &recorder,
        v1alpha1::Notifier::new(&name, Default::default()),
    )
    .await?;
    next.endpoint = initialize_endpoint(obj.clone(), ctx.clone(), &recorder).await?;

    Ok(Action::await_change())
}

async fn initialize_config(
    obj: Arc<v1alpha1::Clair>,
    ctx: Arc<Context>,
    recorder: &Recorder,
) -> Result<Option<v1alpha1::ConfigSource>> {
    let spec = &obj.spec;
    let params = PostParams {
        dry_run: false,
        field_manager: Some(OPERATOR_NAME.to_string()),
    };
    let action = String::from("ConfigCreation");
    let owner = Resource::object_ref(obj.as_ref(), &());
    let owner = meta::v1::OwnerReference {
        api_version: owner.api_version.unwrap(),
        block_owner_deletion: None,
        controller: Some(false),
        kind: owner.kind.unwrap(),
        name: owner.name.unwrap(),
        uid: owner.uid.unwrap(),
    };
    let name = &obj.metadata.name.as_ref().unwrap();
    let mut data = BTreeMap::new();
    let key = match spec.config_dialect.clone().unwrap_or_default() {
        v1alpha1::ConfigDialect::JSON => {
            data.insert("config.json".to_string(), DEFAULT_CONFIG_JSON.to_string());
            "config.json"
        }
        v1alpha1::ConfigDialect::YAML => {
            data.insert("config.yaml".to_string(), DEFAULT_CONFIG_YAML.to_string());
            "config.yaml"
        }
    };

    let api: Api<core::v1::ConfigMap> = Api::default_namespaced(ctx.client.clone());
    let new = core::v1::ConfigMap {
        metadata: ObjectMeta {
            generate_name: Some(format!("{}-", name.as_str())),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        immutable: Some(true),
        binary_data: None,
        data: Some(data),
    };
    Ok(match api.create(&params, &new).await {
        Ok(v) => {
            recorder
                .publish(Event {
                    type_: EventType::Normal,
                    reason: "Success".into(),
                    note: None,
                    action,
                    secondary: Some(v.object_ref(&())),
                })
                .await?;
            Some(v1alpha1::ConfigSource {
                root: core::v1::ConfigMapKeySelector {
                    name: v.meta().name.clone(),
                    key: key.to_string(),
                    optional: Some(false),
                },
                dropins: vec![],
            })
        }
        Err(e) => {
            recorder
                .publish(Event {
                    type_: EventType::Warning,
                    reason: "Failed".into(),
                    note: Some(e.to_string()),
                    action,
                    secondary: None,
                })
                .await?;
            None
        }
    })
}

async fn initialize_subresource<K>(
    obj: Arc<v1alpha1::Clair>,
    ctx: Arc<Context>,
    recorder: &Recorder,
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
    let owner = Resource::object_ref(obj.as_ref(), &());
    let owner = meta::v1::OwnerReference {
        api_version: owner.api_version.unwrap(),
        block_owner_deletion: Some(true),
        controller: Some(true),
        kind: owner.kind.unwrap(),
        name: owner.name.unwrap(),
        uid: owner.uid.unwrap(),
    };
    new.meta_mut().owner_references = Some(vec![owner]);
    let api: Api<K> = Api::default_namespaced(ctx.client.clone());

    Ok(match api.create(&params, &new).await {
        Ok(v) => {
            recorder
                .publish(Event {
                    type_: EventType::Normal,
                    reason: "Success".into(),
                    note: None,
                    action,
                    secondary: Some(v.object_ref(&())),
                })
                .await?;
            Some(TypedLocalObjectReference {
                api_group: Some(K::api_version(&()).to_string()),
                name: v.meta().name.to_owned().unwrap(),
                kind: K::kind(&()).to_string(),
            })
        }
        Err(e) => {
            recorder
                .publish(Event {
                    type_: EventType::Warning,
                    reason: "Failed".into(),
                    note: Some(e.to_string()),
                    action,
                    secondary: None,
                })
                .await?;
            None
        }
    })
}

async fn initialize_endpoint(
    obj: Arc<v1alpha1::Clair>,
    ctx: Arc<Context>,
    recorder: &Recorder,
) -> Result<Option<TypedLocalObjectReference>> {
    let params = PostParams {
        dry_run: false,
        field_manager: Some(OPERATOR_NAME.to_string()),
    };
    let owner = Resource::object_ref(obj.as_ref(), &());
    let owner = meta::v1::OwnerReference {
        api_version: owner.api_version.unwrap(),
        block_owner_deletion: Some(true),
        controller: Some(true),
        kind: owner.kind.unwrap(),
        name: owner.name.unwrap(),
        uid: owner.uid.unwrap(),
    };

    let disc = Discovery::new(ctx.client.clone());
    let ar = &[
        GroupVersionKind::gvk("route.openshift.io", "v1", "Route"),
        GroupVersionKind::gvk("networking.k8s.io", "v1", "Ingress"),
        GroupVersionKind::gvk("gateway.networking.k8s.io", "v1beta1", "Gateway"),
    ]
    .iter()
    .filter_map(|ref gvk| disc.resolve_gvk(gvk))
    .take(1)
    .next();
    let ar = if let Some((ar, _)) = ar {
        ar
    } else {
        return Ok(None);
    };

    Ok(match ar.kind.as_str() {
        "Route" => None,   // TODO(hank) Support a Route.
        "Gateway" => None, // TODO(hank) Support a Gateway.
        "Ingress" => {
            let action = String::from("IngressCreation");
            let api: Api<networking::v1::Ingress> = Api::default_namespaced(ctx.client.clone());
            let new = networking::v1::Ingress {
                metadata: ObjectMeta {
                    name: obj.metadata.name.to_owned(),
                    owner_references: Some(vec![owner]),
                    ..Default::default()
                },
                spec: Default::default(),
                status: None,
            };
            match api.create(&params, &new).await {
                Ok(v) => {
                    recorder
                        .publish(Event {
                            type_: EventType::Warning,
                            reason: "Success".into(),
                            note: None,
                            action,
                            secondary: Some(v.object_ref(&())),
                        })
                        .await?;
                    Some(TypedLocalObjectReference {
                        api_group: Some(ar.api_version.to_string()),
                        kind: ar.kind.to_string(),
                        name: v.metadata.name.unwrap(),
                    })
                }
                Err(e) => {
                    recorder
                        .publish(Event {
                            type_: EventType::Warning,
                            reason: "Failed".into(),
                            note: Some(e.to_string()),
                            action,
                            secondary: None,
                        })
                        .await?;
                    None
                }
            }
        }
        _ => unreachable!(),
    })
}

async fn check_config(
    obj: Arc<v1alpha1::Clair>,
    ctx: Arc<Context>,
    recorder: &Recorder,
) -> Result<(Vec<meta::v1::Condition>, bool)> {
    let now = meta::v1::Time(Utc::now());
    let spec = &obj.clone().spec;
    let status = obj.status.as_ref().unwrap();
    let mut cfg = status.config.as_ref().unwrap().clone();
    cfg.dropins = spec.dropins.clone();
    let mut next = Vec::new();

    let v = config::validate(ctx.client.clone(), &cfg).await?;
    let mut todo = vec![("Indexer", v.indexer), ("Matcher", v.matcher)];
    if spec.notifier.unwrap_or_default() {
        todo.push(("Notifier", v.notifier));
    }
    for (kind, res) in todo {
        let (ev, cnd) = match res {
            Ok(ws) => (
                Event {
                    type_: EventType::Normal,
                    reason: "ConfigAdded".into(),
                    note: Some(config::fmt_warnings(ws)),
                    action: "ConfigValidation".into(),
                    secondary: None, // TODO(hank) Reference the subresource.
                },
                meta::v1::Condition {
                    last_transition_time: now.clone(),
                    message: "".to_string(),
                    observed_generation: obj.metadata.generation,
                    reason: format!("{}ValidationSuccess", kind),
                    status: "True".to_string(),
                    type_: condition("ConfigValidated"),
                },
            ),
            Err(e) => (
                Event {
                    type_: EventType::Warning,
                    reason: "ConfigAdded".into(),
                    note: Some(e.to_string()),
                    action: "ConfigValidation".into(),
                    secondary: None, // TODO(hank) Reference the subresource.
                },
                meta::v1::Condition {
                    last_transition_time: now.clone(),
                    message: e.to_string(),
                    observed_generation: obj.metadata.generation,
                    reason: format!("{}ValidationFailure", kind),
                    status: "False".to_string(),
                    type_: condition("ConfigValidated"),
                },
            ),
        };
        recorder.publish(ev).await?;
        next.push(cnd);
    }
    let ok = next.iter().all(|ref cnd| cnd.status == "True");
    Ok((next, ok))
}

pub fn controller(set: &mut task::JoinSet<Result<()>>, client: Client) {
    let cfg = watcher::Config::default();
    let ctx = Arc::new(Context {
        client: client.clone(),
    });
    let root: Api<v1alpha1::Clair> = Api::default_namespaced(client.clone());
    let secrets: Api<core::v1::Secret> = Api::default_namespaced(client.clone());
    let configmaps: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
    let ctl = Controller::new(root, cfg.clone())
        .owns(secrets, cfg.clone())
        .owns(configmaps, cfg.clone());
    info!("spawning clair controller");
    set.spawn(async move {
        ctl.run(reconcile, error_policy, ctx)
            .for_each(|_| futures::future::ready(()))
            .await;
        Ok(())
    });
}
