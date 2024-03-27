use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::apps;
use k8s_openapi::NamespaceResourceScope;
use kube::core::PartialObjectMeta;
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use serde::de::DeserializeOwned;
use tracing::trace;

use super::{Context, Result};

pub async fn create_if_needed<K, R>(
    obj: Arc<PartialObjectMeta<K>>,
    ctx: Arc<Context>,
) -> Result<Action>
where
    R: Resource<Scope = NamespaceResourceScope, DynamicType = ()>
        + Clone
        + DeserializeOwned
        + Debug,
    K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>
        + Clone
        + DeserializeOwned
        + Debug,
{
    use kube::api::Api;

    let guess_name = format!("{}-{}", obj.name_any(), K::kind(&()).to_ascii_lowercase());
    let deployment = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<apps::v1::Deployment>())
        .map(|r| r.name)
        .or_else(|| Some(guess_name.clone()))
        .unwrap();
    let res_name = obj
        .status
        .as_ref()
        .and_then(|s| s.has_ref::<R>())
        .map(|r| r.name)
        .or_else(|| Some(guess_name.clone()))
        .unwrap();
    let kind = K::kind(&()).to_string();
    let ns = obj.namespace().unwrap();
    let object_name = obj.name_any();
    let api: Api<PartialObjectMeta<R>> = Api::namespaced(ctx.client, &ns);

    for n in 0..3 {
        trace!(n, "reconcile attempt");
        let mut entry = api.entry(&res_name).await?;
        match entry {
            kube::api::entry::Entry::Occupied(_) => todo!(),
            kube::api::entry::Entry::Vacant(_) => todo!(),
        }
    }

    Ok(Action::requeue(Duration::from_secs(5 * 60)))
}
