//! Indexers holds the controller for the "Indexer" CRD.
//!
//! ```mermaid
//! ```

use std::sync::{Arc, LazyLock};

use kube::{
    api::{Api, Patch},
    client::Client,
    runtime::controller::Error as CtrlErr,
    ResourceExt,
};

use serde_json::json;
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, prelude::*, COMPONENT_LABEL};
use v1alpha1::Indexer;

static COMPONENT: LazyLock<String> = LazyLock::new(|| Indexer::kind(&()).to_ascii_lowercase());

/// Controller is the Indexer controller.
///
/// An error is returned if any setup fails.
#[instrument(skip_all)]
pub fn controller(cancel: CancellationToken, ctx: Arc<Context>) -> Result<ControllerFuture> {
    let client = ctx.client.clone();
    let ctlcfg = watcher::Config::default();
    let sig = SignalStream::new(signal(SignalKind::user_defined1())?);

    let ctl = Controller::new(
        Api::<Indexer>::default_namespaced(client.clone()),
        ctlcfg.clone(),
    )
    .owns(
        Api::<apps::v1::Deployment>::default_namespaced(client.clone()),
        ctlcfg.clone(),
    )
    .owns(
        Api::<apps::v1::StatefulSet>::default_namespaced(client.clone()),
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
                        CtrlErr::RunnerError(error) => error!(%error, "runner error"),
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

/// Reconcile is the main entrypoint for the reconcile loop.
#[instrument(skip(ctx, indexer), fields(name = indexer.name_any(), namespace = indexer.namespace().unwrap()))]
async fn reconcile(indexer: Arc<Indexer>, ctx: Arc<Context>) -> Result<Action> {
    assert!(indexer.meta().name.is_some());
    // Setup:
    let ns = indexer.namespace().unwrap(); // Indexer is namespace scoped
    let client = &ctx.client;
    let indexers: Api<Indexer> = Api::namespaced(client.clone(), &ns);

    info!("reconciling Indexer");
    //let req = Request::new(client);
    let name = format!("{}-{}", indexer.name_any(), COMPONENT.as_str());

    // Load the spec:
    let (cfgsrc, image) = {
        // This should all be done in the admission webhook:
        /*
        let mut cnd = Condition {
            last_transition_time: req.now(),
            observed_generation: indexer.metadata.generation,
            type_: clair_condition("SpecOK"),
            message: "".into(),
            reason: "".into(),
            status: "".into(),
        };
        debug!("configsource check");
        if indexer.spec.config.is_none() {
            trace!("configsource missing");

            cnd = Condition {
                message: "\"/spec/config\" missing".into(),
                reason: "SpecIncomplete".into(),
                status: "False".into(),
                ..cnd
            };

            req.publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "Initialization".into(),
                    note: Some("missing field \"/spec/config\"".into()),
                    action: "CheckConfig".into(),
                    secondary: None,
                },
                &indexer.object_ref(&()),
            )
            .await?;
        } else {
            debug!("configsource ok");
            cnd = Condition {
                reason: "SpecComplete".into(),
                status: "True".into(),
                ..cnd
            };
        }
        patch_condition(client.clone(), indexer.as_ref(), cnd).await?;
        */

        if indexer.spec.config.is_none() {
            error!(
                hint = "is the admission webhook running?",
                "spec missing ConfigSource"
            );
            return Ok(Action::requeue(Duration::from_secs(3600)));
        }
        (
            indexer.spec.config.as_ref().unwrap(),
            indexer.spec.image.as_ref().unwrap_or(&crate::DEFAULT_IMAGE),
        )
    };
    debug!("have needed spec fields");

    // Deployment check
    {
        use self::core::v1::EnvVar;
        use apps::v1::Deployment;

        let (volumes, volume_mounts, config) = make_volumes(cfgsrc);
        let mut d: Deployment = templates::render(indexer.as_ref());
        d.labels_mut()
            .insert(COMPONENT_LABEL.to_string(), COMPONENT.to_string());
        {
            let spec = d.spec.get_or_insert_default();
            spec.selector
                .match_labels
                .get_or_insert_default()
                .insert(COMPONENT_LABEL.to_string(), COMPONENT.to_string());
            spec.template
                .metadata
                .get_or_insert_default()
                .labels
                .get_or_insert_default()
                .insert(COMPONENT_LABEL.to_string(), COMPONENT.to_string());
            let podspec = spec.template.spec.get_or_insert_default();
            podspec
                .volumes
                .get_or_insert_default()
                .extend_from_slice(&volumes);
            if let Some(c) = podspec.containers.iter_mut().find(|c| c.name == "clair") {
                c.image = Some(image.clone());
                c.volume_mounts
                    .get_or_insert_default()
                    .extend_from_slice(&volume_mounts);
                c.env.get_or_insert_default().extend_from_slice(&[EnvVar {
                    name: "CLAIR_CONF".into(),
                    value: Some(config),
                    value_from: None,
                }]);
            }
        }

        let _d = Api::<Deployment>::namespaced(client.clone(), &ns)
            .patch(&name, &PATCH_PARAMS, &Patch::Apply(d))
            .await
            .inspect_err(|error| error!(%error, "failed to patch Deployment"))?;
        let type_ = clair_condition("DeploymentCreated");
        if !indexer
            .status
            .as_ref()
            .is_some_and(|s| s.conditions.iter().any(|c| c.type_ == type_))
        {
            let status = json!({
              "status": {
                "conditions": [
                  Condition {
                    message: "created Deployment".into(),
                    observed_generation: indexer.metadata.generation,
                    last_transition_time: meta::v1::Time(Utc::now()),
                    reason: "DeploymentCreated".into(),
                    status: "True".into(),
                    type_,
                  }
                ],
              },
            });
            indexers
                .patch_status(&indexer.name_any(), &PATCH_PARAMS, &Patch::Merge(&status))
                .await
                .inspect_err(|error| error!(%error, "unable to patch status on self"))?;
            debug!("updated status: Deployment");
        }
    }
    debug!("created Deployment");

    {
        use self::core::v1::Service;

        let mut s: Service = new_templated(indexer.as_ref(), &ctx).await?;
        s.labels_mut()
            .insert(COMPONENT_LABEL.to_string(), COMPONENT.to_string());

        let _s = Api::<Service>::namespaced(client.clone(), &ns)
            .patch(&name, &PATCH_PARAMS, &Patch::Apply(s))
            .await
            .inspect_err(|error| error!(%error, "failed to patch Service"))?;
        let type_ = clair_condition("ServiceCreated");
        if !indexer
            .status
            .as_ref()
            .is_some_and(|s| s.conditions.iter().any(|c| c.type_ == type_))
        {
            let status = json!({
              "status": {
                "conditions": [
                  Condition {
                    message: "created Service".into(),
                    observed_generation: indexer.metadata.generation,
                    last_transition_time: meta::v1::Time(Utc::now()),
                    reason: "ServiceCreated".into(),
                    status: "True".into(),
                    type_,
                  }
                ],
              },
            });
            indexers
                .patch_status(&indexer.name_any(), &PATCH_PARAMS, &Patch::Merge(&status))
                .await
                .inspect_err(|error| error!(%error, "unable to patch status on self"))?;
            debug!("updated status: Service");
        }
    }
    debug!("created Service");

    'dropin_check: {
        use self::core::v1::Service;

        let owner = match indexer
            .owner_references()
            .iter()
            .find(|&r| r.controller.unwrap_or(false))
        {
            None => {
                trace!("not owned, skipping dropin generation");
                break 'dropin_check;
            }
            Some(o) => o,
        };
        trace!(owner = owner.name, "indexer is owned");

        let srv = Api::<Service>::namespaced(client.clone(), &ns)
            .get(&name)
            .await?;

        let dropin = templates::render_dropin::<Indexer>(&srv)
            .expect("should always have a dropin for Indexer");
        let dropin = json!({ "status": { "dropin": dropin } });
        indexers
            .patch_status(&indexer.name_any(), &PATCH_PARAMS, &Patch::Merge(dropin))
            .await
            .inspect_err(|error| error!(%error, "unable to patch status on self"))?;
    }
    debug!("added dropin");

    {
        use autoscaling::v2::HorizontalPodAutoscaler;

        let mut hpa: HorizontalPodAutoscaler = new_templated(indexer.as_ref(), &ctx).await?;
        hpa.labels_mut()
            .insert(COMPONENT_LABEL.to_string(), COMPONENT.to_string());
        hpa.spec.get_or_insert_default().scale_target_ref.name = name.clone();
        // TODO(hank) Check if the metrics API is enabled and if the frontend supports
        // request-per-second metrics.

        Api::<HorizontalPodAutoscaler>::namespaced(client.clone(), &ns)
            .patch(&name, &PATCH_PARAMS, &Patch::Apply(hpa))
            .await
            .inspect_err(|error| error!(%error, "failed to patch HorizontalPodAutoscaler"))?;

        let type_ = clair_condition("HorizontalPodAutoscalerCreated");
        if !indexer
            .status
            .as_ref()
            .is_some_and(|s| s.conditions.iter().any(|c| c.type_ == type_))
        {
            let status = json!({
              "status": {
                "conditions": [
                  Condition {
                    message: "created HorizontalPodAutoscaler".into(),
                    observed_generation: indexer.metadata.generation,
                    last_transition_time: meta::v1::Time(Utc::now()),
                    reason: "HorizontalPodAutoscalerCreated".into(),
                    status: "True".into(),
                    type_,
                  }
                ],
              },
            });
            indexers
                .patch_status(&indexer.name_any(), &PATCH_PARAMS, &Patch::Merge(&status))
                .await
                .inspect_err(|error| error!(%error, "unable to patch status on self"))?;
            debug!("updated status: HorizontalPodAutoscaler");
        }
    }
    debug!("created HorizontalPodAutoscaler");

    Ok(DEFAULT_REQUEUE.clone())
}

#[allow(dead_code)]
async fn patch_condition<K>(client: Client, obj: &K, cnd: Condition) -> Result<()>
where
    K: Resource<DynamicType = (), Scope = kube::core::NamespaceResourceScope>
        + serde::de::DeserializeOwned,
{
    let ns = obj.namespace().unwrap();
    let api = Api::<K>::namespaced(client, &ns);
    let status = json!({
        "status": { "conditions": [ cnd ] },
    });
    api.patch_status(
        &obj.name_unchecked(),
        &PatchParams::default(),
        &Patch::Merge(&status),
    )
    .await?;
    Ok(())
}

#[instrument(skip_all)]
async fn check_config(
    obj: &Indexer,
    _ctx: &Context,
    _req: &Request,
    next: &mut v1alpha1::IndexerStatus,
) -> Result<bool> {
    debug!(TODO = "hank", "re-check config");
    next.config = obj.spec.config.clone();
    Ok(true)
}

/*
#[instrument(skip_all)]
async fn check_creation(
    obj: &Indexer,
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
*/

#[instrument(skip_all)]
fn handle_error(_obj: Arc<Indexer>, _err: &Error, _ctx: Arc<Context>) -> Action {
    Action::await_change()
}
