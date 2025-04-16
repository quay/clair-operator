#![allow(dead_code)]
use std::{process::Command, sync::Arc};

use tracing::trace;

use controller::*;

pub async fn test_context() -> Arc<Context> {
    let config = kube::Config::infer()
        .await
        .expect("unable to infer kubeconfig");
    let client = kube::client::ClientBuilder::try_from(config.clone())
        .expect("unable to create client builder")
        .build();
    Arc::new(Context::new(client, DEFAULT_IMAGE.as_str()))
}

pub async fn load_crds(client: &kube::Client) -> Result<()> {
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
    use kube::{
        api::{Api, PostParams},
        ResourceExt,
    };
    use serde::Deserialize;
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());

    let dir = workspace().join("etc/operator/config/crd");
    let out = Command::new("kustomize")
        .args(["build", dir.to_str().unwrap()])
        .output()?;
    let dec = serde_yaml::Deserializer::from_slice(&out.stdout);
    let params = PostParams::default();
    for doc in dec {
        let crd = CustomResourceDefinition::deserialize(doc)?;
        let name = crd.name_any();
        trace!(name, "checking CRD");
        if api.get_metadata_opt(&name).await?.is_none() {
            trace!(name, "creating CRD");
            if let Err(err) = api.create(&params, &crd).await {
                match err {
                    kube::Error::Api(res) => match res.code {
                        409 => (),
                        _ => return Err(Error::from(kube::Error::Api(res))),
                    },
                    _ => return Err(Error::from(err)),
                };
            };
        }
        trace!(name, "CRD ok");
    }

    Ok(())
}

fn workspace() -> std::path::PathBuf {
    std::path::Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}

#[allow(unused_imports)]
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
