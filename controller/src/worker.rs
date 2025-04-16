use std::{
    fmt::Debug,
    marker::PhantomData,
    sync::{Arc, LazyLock},
};

use api::v1alpha1::IndexerStatus;
use k8s_openapi::{DeepMerge, NamespaceResourceScope};
use kube::{
    api::{Api, Patch, PostParams},
    client::Client,
    core::{GroupVersionKind, ObjectMeta},
    discovery::oneshot,
    runtime::{
        controller::Error as CtrlErr,
        finalizer::{finalizer, Event as Finalizer},
        watcher::Config,
    },
    ResourceExt,
};

use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::{
    signal::unix::{signal, SignalKind},
    time::Duration,
};
use tokio_stream::wrappers::SignalStream;

use crate::{clair_condition, prelude::*, COMPONENT_LABEL};

static COMPONENT: &str = "indexer";
static SERVERSIDE: LazyLock<PatchParams> = LazyLock::new(|| PatchParams::apply(CONTROLLER_NAME));
static DEFAULT_REQUEUE: LazyLock<Action> =
    LazyLock::new(|| Action::requeue(Duration::from_secs(60 * 60)));

struct Controller<K>
where
    K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>
        + Clone
        + DeserializeOwned
        + Debug,
{
    component: String,
    _ty: PhantomData<K>,
}

impl<K> Controller<K>
where
    K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>
        + Clone
        + DeserializeOwned
        + Debug,
{
    fn new() -> Self {
        Self {
            component: K::kind(&()).to_ascii_lowercase(),
            _ty: PhantomData,
        }
    }

    fn api<S: AsRef<str>>(client: Client, ns: S) -> Api<K> {
        Api::namespaced(client, ns.as_ref())
    }

    async fn patch_condition(client: Client, obj: &K, cnd: Condition) -> Result<()> {
        let ns = obj.namespace().unwrap();
        let status = json!({
            "status": { "conditions": [ cnd ] },
        });
        Self::api(client, &ns)
            .patch_status(
                &obj.name_unchecked(),
                &PatchParams::default(),
                &Patch::Merge(&status),
            )
            .await?;
        Ok(())
    }
}
