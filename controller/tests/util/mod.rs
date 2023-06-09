#![allow(dead_code)]
use std::sync::Arc;

use tracing::trace;

use api::v1alpha1;
use controller::*;

pub async fn test_context() -> Arc<Context> {
    let config = kube::Config::infer()
        .await
        .expect("unable to infer kubeconfig");
    let client = kube::client::ClientBuilder::try_from(config.clone())
        .expect("unable to create client builder")
        .build();
    Arc::new(Context {
        client,
        assets: templates::Assets::new("/nonexistent"),
        image: DEFAULT_IMAGE.clone(),
    })
}

macro_rules! load_each {
    ($api:ident, $($kind:ty),+) => {
        use kube::{api::PostParams, CustomResourceExt, ResourceExt};
        let params = PostParams::default();
        $({
        let crd = <$kind>::crd();
        let name = crd.name_any();
        trace!(name, "checking CRD");
        if $api.get_metadata_opt(&name).await?.is_none() {
            trace!(name, "creating CRD");
            $api.create(&params, &crd).await?;
        }
        trace!(name, "CRD ok");
        })+
    }
}

pub async fn load_crds(client: &kube::Client) -> Result<()> {
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
    use kube::api::Api;
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());

    load_each!(
        api,
        v1alpha1::Clair,
        v1alpha1::Indexer,
        v1alpha1::Matcher,
        v1alpha1::Notifier,
        v1alpha1::Updater
    );

    Ok(())
}

pub mod prelude {
    pub use std::sync::Arc;

    pub use json_patch::Patch;
    pub use k8s_openapi::api::networking;
    pub use kube::{api::PostParams, Api};
    pub use serde_json::json;
    pub use test_log::test;
    pub use tokio::{signal, task, time::Duration};
    pub use tokio_util::sync::CancellationToken;
}
