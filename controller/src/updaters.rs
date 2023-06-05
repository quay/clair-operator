use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::api::{apps, core};
use kube::{
    runtime::{
        controller::{Action, Controller},
        watcher,
    },
    Api, Client,
};
use tokio::{task, time::Duration};

use crate::*;

struct Context {
    _client: Client,
}

fn error_policy(_obj: Arc<v1alpha1::Updater>, _e: &Error, _ctx: Arc<Context>) -> Action {
    debug!("error!");
    Action::await_change()
}

async fn reconcile(_obj: Arc<v1alpha1::Updater>, _ctx: Arc<Context>) -> Result<Action> {
    debug!("reconcile!");
    Ok(Action::requeue(Duration::from_secs(300)))
}

pub fn controller(set: &mut task::JoinSet<Result<()>>, client: Client) {
    let cfg = watcher::Config::default();
    let updaters: Api<v1alpha1::Updater> = Api::default_namespaced(client.clone());
    let configmaps: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
    let secrets: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
    let srvs: Api<core::v1::Service> = Api::default_namespaced(client.clone());
    let deploys: Api<apps::v1::Deployment> = Api::default_namespaced(client.clone());
    let ctl = Controller::new(updaters, cfg.clone())
        .owns(configmaps, cfg.clone())
        .owns(secrets, cfg.clone())
        .owns(srvs, cfg.clone())
        .owns(deploys, cfg.clone());
    let ctx = Arc::new(Context {
        _client: client.clone(),
    });
    info!("spawning updater controller");
    set.spawn(async move {
        ctl.run(reconcile, error_policy, ctx)
            .for_each(|_| futures::future::ready(()))
            .await;
        Ok(())
    });
}
