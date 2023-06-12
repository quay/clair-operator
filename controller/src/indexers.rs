use std::sync::Arc;

use kube::{
    api::{Patch, PostParams},
    Api,
};
use lazy_static::lazy_static;
use tokio::{task, time::Duration};

use self::core::v1::TypedLocalObjectReference;
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
        next.conditions.push(meta::v1::Condition {
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
        image = image_any(spec),
        "image check"
    );
    next.conditions.push(Condition {
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

fn image_any(spec: &v1alpha1::IndexerSpec) -> String {
    spec.image
        .as_ref()
        .cloned()
        .unwrap_or_else(|| crate::DEFAULT_IMAGE.clone())
}

#[instrument(skip_all)]
async fn publish(
    obj: Arc<v1alpha1::Indexer>,
    ctx: Arc<Context>,
    _req: Request,
    mut next: v1alpha1::IndexerStatus,
) -> Result<Action> {
    let api: Api<v1alpha1::Indexer> = Api::default_namespaced(ctx.client.clone());
    let name = &obj.metadata.name.as_ref().unwrap();
    let changed = obj.status.is_none() || obj.status.as_ref().unwrap() == &next;
    let mut c = v1alpha1::Indexer::clone(&obj);
    next.conditions
        .sort_by_key(|c| -c.observed_generation.unwrap_or_default());
    next.conditions.dedup_by(|a, b| a.type_ == b.type_);

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

fn has_ref<K>(status: &Option<v1alpha1::IndexerStatus>) -> Option<TypedLocalObjectReference>
where
    K: Resource<DynamicType = ()>,
{
    if status.is_none() {
        return None;
    }
    let kind = K::kind(&());
    status
        .as_ref()
        .unwrap()
        .refs
        .iter()
        .find(|r| r.kind == kind)
        .cloned()
}

lazy_static! {
    static ref CREATE_PARAMS: PostParams = PostParams {
        dry_run: false,
        field_manager: Some(String::from(OPERATOR_NAME)),
    };
}

#[instrument(skip_all)]
async fn check_config(
    obj: &v1alpha1::Indexer,
    _ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
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
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    let dref = has_ref::<apps::v1::Deployment>(&obj.status);
    if dref.is_none() {
        let cfgsrc = obj
            .spec
            .config
            .as_ref()
            .expect("missing needed spec field: config");
        let deploy = new_deployment(obj, ctx, cfgsrc).await?;
        let api = Api::<apps::v1::Deployment>::default_namespaced(ctx.client.clone());
        let deploy = api.create(&CREATE_PARAMS, &deploy).await?;
        debug!(name = deploy.name_unchecked(), "created Deployment");
        next.refs.push(TypedLocalObjectReference {
            api_group: Some(apps::v1::Deployment::api_version(&()).to_string()),
            kind: apps::v1::Deployment::kind(&()).to_string(),
            name: deploy.name_any(),
        });
        return Ok(false);
    }
    // TODO
    Ok(true)
}

#[instrument(skip_all)]
async fn check_service(
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    let sref = has_ref::<core::v1::Service>(&obj.status);
    if sref.is_none() {
        let srv = new_service(obj, ctx).await?;
        let api = Api::<core::v1::Service>::default_namespaced(ctx.client.clone());
        let srv = api.create(&CREATE_PARAMS, &srv).await?;
        debug!(name = srv.name_unchecked(), "created Service");
        next.refs.push(TypedLocalObjectReference {
            api_group: Some(core::v1::Service::api_version(&()).to_string()),
            kind: core::v1::Service::kind(&()).to_string(),
            name: srv.name_any(),
        });
        return Ok(false);
    }
    // TODO
    Ok(true)
}

#[instrument(skip_all)]
async fn check_hpa(
    obj: &v1alpha1::Indexer,
    ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    let href = has_ref::<autoscaling::v2::HorizontalPodAutoscaler>(&obj.status);
    if href.is_none() {
        let hpa = new_hpa(obj, ctx).await?;
        let api =
            Api::<autoscaling::v2::HorizontalPodAutoscaler>::default_namespaced(ctx.client.clone());
        let hpa = api.create(&CREATE_PARAMS, &hpa).await?;
        debug!(name = hpa.name_unchecked(), "created HPA");
        next.refs.push(TypedLocalObjectReference {
            api_group: Some(autoscaling::v2::HorizontalPodAutoscaler::api_version(&()).to_string()),
            kind: autoscaling::v2::HorizontalPodAutoscaler::kind(&()).to_string(),
            name: hpa.name_any(),
        });
        return Ok(false);
    }
    // TODO
    Ok(true)
}

#[instrument(skip_all)]
async fn check_creation(
    obj: &v1alpha1::Indexer,
    _ctx: &Context,
    req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    let refs = [
        has_ref::<apps::v1::Deployment>(&obj.status),
        has_ref::<core::v1::Service>(&obj.status),
        has_ref::<autoscaling::v2::HorizontalPodAutoscaler>(&obj.status),
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

    next.conditions.push(meta::v1::Condition {
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

    let mut v: apps::v1::Deployment = match templates::resource_for("indexer").await {
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
async fn new_service(obj: &v1alpha1::Indexer, _ctx: &Context) -> Result<core::v1::Service> {
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");

    let mut v: core::v1::Service = match templates::resource_for("indexer").await {
        Ok(v) => v,
        Err(err) => return Err(Error::Assets(err.to_string())),
    };
    v.metadata.owner_references = Some(vec![oref]);
    v.metadata.name = Some(format!("{}-indexer", obj.name_any()));
    Ok(v)
}
async fn new_hpa(
    obj: &v1alpha1::Indexer,
    _ctx: &Context,
) -> Result<autoscaling::v2::HorizontalPodAutoscaler> {
    let oref = obj
        .controller_owner_ref(&())
        .expect("unable to create owner ref");
    let dref = has_ref::<apps::v1::Deployment>(&obj.status)
        .ok_or(Error::BadName("missing Deployment reference".into()))?; //TODO(hank) use a better
                                                                        //error

    let mut v: autoscaling::v2::HorizontalPodAutoscaler =
        match templates::resource_for("indexer").await {
            Ok(v) => v,
            Err(err) => return Err(Error::Assets(err.to_string())),
        };
    v.metadata.owner_references = Some(vec![oref]);
    v.metadata.name = Some(format!("{}-indexer", obj.name_any()));
    if let Some(ref mut spec) = v.spec {
        spec.scale_target_ref.name = dref.name;
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
