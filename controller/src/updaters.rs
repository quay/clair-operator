use std::sync::Arc;

use tokio::{task, time::Duration};

use crate::prelude::*;
use crate::*;

fn error_policy(_obj: Arc<v1alpha1::Updater>, _e: &Error, _ctx: Arc<Context>) -> Action {
    debug!("error!");
    Action::await_change()
}

async fn reconcile(_obj: Arc<v1alpha1::Updater>, _ctx: Arc<Context>) -> Result<Action> {
    debug!("reconcile!");
    Ok(Action::requeue(Duration::from_secs(300)))
}

pub fn controller(set: &mut task::JoinSet<Result<()>>, ctx: Arc<Context>) {
    let cfg = watcher::Config::default();
    let client = ctx.client.clone();
    let updaters: Api<v1alpha1::Updater> = Api::default_namespaced(client.clone());
    let configmaps: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
    let secrets: Api<core::v1::ConfigMap> = Api::default_namespaced(client.clone());
    let srvs: Api<core::v1::Service> = Api::default_namespaced(client.clone());
    let deploys: Api<apps::v1::Deployment> = Api::default_namespaced(client);
    let ctl = Controller::new(updaters, cfg.clone())
        .owns(configmaps, cfg.clone())
        .owns(secrets, cfg.clone())
        .owns(srvs, cfg.clone())
        .owns(deploys, cfg);
    info!("spawning updater controller");
    set.spawn(async move {
        ctl.run(reconcile, error_policy, ctx)
            .for_each(|_| futures::future::ready(()))
            .await;
        Ok(())
    });
}
