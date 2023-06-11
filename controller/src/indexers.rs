use std::{env, sync::Arc};

use kube::{
    api::{Patch, PostParams},
    Api,
};
use tokio::{task, time::Duration};

use crate::{clair_condition, prelude::*, COMPONENT_LABEL};

static COMPONENT: &str = "indexer";

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
    let ctl = Controller::new(
        Api::<v1alpha1::Indexer>::default_namespaced(client.clone()),
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
    .owns(Api::<core::v1::Service>::default_namespaced(client), wcfg);

    info!("spawning indexer controller");
    set.spawn(async move {
        tokio::select! {
            _ = ctl.run(reconcile, error_policy, ctx).for_each(|_| futures::future::ready(())) => debug!("indexer controller finished"),
            _ = cancel.cancelled() => debug!("indexer controller cancelled"),
        }
        Ok(())
    });
}

#[instrument(skip_all)]
async fn reconcile(obj: Arc<v1alpha1::Indexer>, ctx: Arc<Context>) -> Result<Action> {
    let now = meta::v1::Time(Utc::now());
    let reporter = Reporter {
        controller: OPERATOR_NAME.clone(),
        instance: env::var("CONTROLLER_POD_NAME").ok(),
    };
    let recorder = Recorder::new(ctx.client.clone(), reporter, obj.object_ref(&()));
    let mut next = obj.status.clone().unwrap_or_default();
    let api = Api::<v1alpha1::Indexer>::default_namespaced(ctx.client.clone());
    debug!("reconcile!");

    if obj.status.is_none() {
        if obj.spec.config.is_none() {
            recorder
                .publish(Event {
                    type_: EventType::Warning,
                    reason: "Initialization".into(),
                    note: Some("missing field \"/spec/config\"".into()),
                    action: "ConfigValidation".into(),
                    secondary: None,
                })
                .await?;
            next.conditions.push(meta::v1::Condition {
                last_transition_time: now.clone(),
                message: "\"/spec/config\" missing".into(),
                observed_generation: obj.metadata.generation,
                reason: "SpecIncomplete".into(),
                status: "False".into(),
                type_: clair_condition("Initialized"),
            });
            let params = PatchParams {
                field_manager: Some(OPERATOR_NAME.to_string()),
                ..Default::default()
            };
            let patch = Patch::Apply(v1alpha1::Indexer {
                metadata: meta::v1::ObjectMeta {
                    name: Some(obj.name_any()),
                    ..Default::default()
                },
                status: Some(next),
                ..Default::default()
            });
            api.patch_status(obj.name_any().as_str(), &params, &patch)
                .await?;
            return Ok(Action::await_change());
        }
        return initialize(obj, ctx, recorder).await;
    }

    debug!("reconcile!");
    Ok(Action::requeue(Duration::from_secs(300)))
}

#[instrument(skip_all)]
async fn initialize(
    obj: Arc<v1alpha1::Indexer>,
    ctx: Arc<Context>,
    recorder: Recorder,
) -> Result<Action> {
    use self::core::v1::TypedLocalObjectReference;
    let client = &ctx.client;
    let now = meta::v1::Time(Utc::now());
    let params = PostParams {
        dry_run: false,
        field_manager: Some(OPERATOR_NAME.to_string()),
    };
    let mut next = if let Some(ref status) = obj.status {
        status.clone()
    } else {
        Default::default()
    };

    let cfgsrc = obj
        .spec
        .config
        .as_ref()
        .expect("missing needed spec field: config");

    let deploy = new_deployment(&obj, &ctx, cfgsrc).await?;
    let api = Api::<apps::v1::Deployment>::default_namespaced(client.clone());
    let deploy = api.create(&params, &deploy).await?;
    debug!(name = deploy.name_unchecked(), "created Deployment");
    next.refs.push(TypedLocalObjectReference {
        api_group: Some(apps::v1::Deployment::api_version(&()).to_string()),
        kind: apps::v1::Deployment::kind(&()).to_string(),
        name: deploy.name_any(),
    });

    let srv = new_service(&obj, &ctx).await?;
    let api = Api::<core::v1::Service>::default_namespaced(client.clone());
    let srv = api.create(&params, &srv).await?;
    debug!(name = srv.name_unchecked(), "created Service");
    next.refs.push(TypedLocalObjectReference {
        api_group: Some(core::v1::Service::api_version(&()).to_string()),
        kind: core::v1::Service::kind(&()).to_string(),
        name: srv.name_any(),
    });

    let hpa = new_hpa(&obj, &ctx, &deploy).await?;
    let api = Api::<autoscaling::v2::HorizontalPodAutoscaler>::default_namespaced(client.clone());
    let hpa = api.create(&params, &hpa).await?;
    debug!(name = hpa.name_unchecked(), "created HPA");
    next.refs.push(TypedLocalObjectReference {
        api_group: Some(autoscaling::v2::HorizontalPodAutoscaler::api_version(&()).to_string()),
        kind: autoscaling::v2::HorizontalPodAutoscaler::kind(&()).to_string(),
        name: hpa.name_any(),
    });

    next.conditions.push(meta::v1::Condition {
        last_transition_time: now.clone(),
        message: "".into(),
        observed_generation: obj.metadata.generation,
        reason: "ObjectsCreated".into(),
        status: "True".into(),
        type_: clair_condition("Initialized"),
    });
    let api = Api::<v1alpha1::Indexer>::default_namespaced(client.clone());
    let mut obj = v1alpha1::Indexer::clone(&obj);
    obj.status = Some(next);
    api.replace_status(&obj.name_any(), &params, serde_json::to_vec(&obj)?)
        .await?;
    recorder
        .publish(Event {
            type_: EventType::Normal,
            reason: "Initialization".into(),
            note: Some("👍".into()),
            action: "ObjectCreation".into(),
            secondary: None,
        })
        .await?;
    Ok(Action::await_change())
}

#[instrument(skip_all)]
async fn new_deployment(
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    cfgsrc: &v1alpha1::ConfigSource,
) -> Result<apps::v1::Deployment> {
    use self::core::v1::EnvVar;
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    let image = obj.spec.image.as_ref().unwrap_or(&ctx.image).clone();

    let mut v: apps::v1::Deployment = match ctx.assets.resource_for("indexer").await {
        Ok(v) => v,
        Err(err) => return Err(Error::Assets(err.to_string())),
    };
    let (vols, mounts, config) = make_volumes(cfgsrc);
    if let Some(ref mut spec) = v.spec {
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
    v.metadata.owner_references = Some(vec![oref]);
    v.metadata.name = Some(format!("{}-indexer", obj.name_any()));
    Ok(v)
}
fn make_volumes(
    cfgsrc: &v1alpha1::ConfigSource,
) -> (Vec<core::v1::Volume>, Vec<core::v1::VolumeMount>, String) {
    use self::core::v1::{
        ConfigMapProjection, ConfigMapVolumeSource, KeyToPath, ProjectedVolumeSource,
        SecretProjection, Volume, VolumeMount, VolumeProjection,
    };
    let mut vols = Vec::new();
    let mut mounts = Vec::new();

    let root = String::from("/etc/clair/");
    let rootname = String::from("root-config");
    let filename = root + &cfgsrc.root.key;
    assert!(filename.ends_with(".json") || filename.ends_with(".yaml"));
    vols.push(Volume {
        name: rootname.clone(),
        config_map: Some(ConfigMapVolumeSource {
            name: cfgsrc.root.name.clone(),
            items: Some(vec![KeyToPath {
                key: cfgsrc.root.key.clone(),
                path: cfgsrc.root.key.clone(),
                mode: Some(0o666),
            }]),
            ..Default::default()
        }),
        ..Default::default()
    });
    debug!(filename, "arranged for root config to be mounted");
    mounts.push(VolumeMount {
        name: rootname,
        mount_path: filename.clone(),
        sub_path: Some(cfgsrc.root.key.clone()),
        ..Default::default()
    });

    let mut proj = Vec::new();
    for d in cfgsrc.dropins.iter() {
        assert!(d.config_map.is_some() || d.secret.is_some());
        let mut v: VolumeProjection = Default::default();
        if let Some(cfgref) = d.config_map.as_ref() {
            v.config_map = Some(ConfigMapProjection {
                name: cfgref.name.clone(),
                optional: Some(false),
                items: Some(vec![KeyToPath {
                    key: cfgref.key.clone(),
                    ..Default::default()
                }]),
            })
        } else if let Some(secref) = d.secret.as_ref() {
            v.secret = Some(SecretProjection {
                name: secref.name.clone(),
                optional: Some(false),
                items: Some(vec![KeyToPath {
                    key: secref.key.clone(),
                    ..Default::default()
                }]),
            })
        };
        proj.push(v);
    }
    vols.push(Volume {
        name: "dropins".into(),
        projected: Some(ProjectedVolumeSource {
            sources: Some(proj),
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut cfg_d = filename.clone();
    cfg_d.push_str(".d");
    debug!(dir = cfg_d, "arranged for dropins to be mounted");
    mounts.push(VolumeMount {
        name: "dropins".into(),
        mount_path: cfg_d,
        ..Default::default()
    });

    (vols, mounts, filename)
}

#[instrument(skip_all)]
async fn new_service(obj: &v1alpha1::Indexer, ctx: &Context) -> Result<core::v1::Service> {
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");

    let mut v: core::v1::Service = match ctx.assets.resource_for("indexer").await {
        Ok(v) => v,
        Err(err) => return Err(Error::Assets(err.to_string())),
    };
    v.metadata.owner_references = Some(vec![oref]);
    v.metadata.name = Some(format!("{}-indexer", obj.name_any()));
    Ok(v)
}
async fn new_hpa(
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    deploy: &apps::v1::Deployment,
) -> Result<autoscaling::v2::HorizontalPodAutoscaler> {
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");

    let mut v: autoscaling::v2::HorizontalPodAutoscaler =
        match ctx.assets.resource_for("indexer").await {
            Ok(v) => v,
            Err(err) => return Err(Error::Assets(err.to_string())),
        };
    v.metadata.owner_references = Some(vec![oref]);
    v.metadata.name = Some(format!("{}-indexer", obj.name_any()));
    if let Some(ref mut spec) = v.spec {
        spec.scale_target_ref.name = deploy.name_any();
    };

    // TODO(hank) Check if the metrics API is enabled and if the frontend supports
    // request-per-second metrics.

    Ok(v)
}

fn error_policy(obj: Arc<v1alpha1::Indexer>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!(
        error = err.to_string(),
        obj.metadata.name, obj.metadata.uid, "reconcile error"
    );
    Action::await_change()
}
