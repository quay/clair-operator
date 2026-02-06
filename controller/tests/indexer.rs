use std::pin::pin;

use api::v1alpha1::Indexer;
use controller::{Error, State, indexers};
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core;
use kube::runtime::{WatchStreamExt, watcher};

mod util;
use util::prelude::*;

#[crate::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
#[cfg_attr(not(feature = "test_ci"), ignore)]
async fn initialize() -> Result<(), Error> {
    let ctx = util::test_context().await;
    util::load_crds(&ctx.client).await?;

    let token = CancellationToken::new();
    let mut ctrls = task::JoinSet::new();
    ctrls.spawn(indexers::controller(token.clone(), ctx.clone())?);
    ctrls.spawn(initialize_inner(ctx));

    loop {
        tokio::select! {
            _ = signal::ctrl_c() => token.cancel(),
            res = ctrls.join_next() => {
                eprintln!("task finished");
                if res.is_none() {
                    break;
                }
                match res.unwrap()? {
                    Ok(_) => {},
                    Err(err) => return Err(err),
                };
                token.cancel();
            },
            else => break,
        }
    }
    Ok(())
}
async fn initialize_inner(ctx: Arc<State>) -> Result<(), Error> {
    use self::core::v1::ConfigMap;
    const NAME: &str = "indexers-initialize-test";
    let cm: Api<ConfigMap> = Api::default_namespaced(ctx.client.clone());
    let api: Api<Indexer> = Api::default_namespaced(ctx.client.clone());
    let params = PostParams::default();

    let root: ConfigMap = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": format!("{NAME}-config")},
        "spec": {
            "data": {
                "config.json": json!({}).to_string(),
            },
        },
    }))?;
    cm.create(&params, &root).await?;

    let indexer: Indexer = serde_json::from_value(json!({
        "apiVersion": format!("{}/v1alpha1", api::GROUP),
        "kind": "Indexer",
        "metadata": {"name": NAME},
        "spec": {
            "image": ctx.image,
            "config": {
                "root": {
                    "name": format!("{NAME}-config"),
                    "key": "config.json",
                },
            },
        },
    }))?;
    api.create(&params, &indexer).await?;

    let generation: i64 = 0;
    let watcher_config = watcher::Config::default().timeout(60).streaming_lists();
    let wait = watcher(api.clone(), watcher_config)
        .default_backoff()
        .applied_objects()
        .try_take_while(|indexer| {
            let cur = indexer.metadata.generation.unwrap();
            eprintln!(
                "cur: {cur} gen: {generation} status: {}",
                indexer.status.is_some()
            );
            futures::future::ready(Ok(cur == generation || indexer.status.is_none()))
        })
        .take(1);
    let got = if let Some(got) = pin!(wait).next().await {
        got.expect("no error")
    } else {
        panic!("nothing")
    };

    use jsonpath_rust::JsonPath;

    let got = serde_json::to_value(got)?;
    let v = got
        .query("$.metadata.name")
        .ok()
        .and_then(|vs| vs.first().and_then(|v| v.as_str()))
        .expect("$.metadata.name not populated");
    eprintln!("name: {v}");
    assert_eq!(v, NAME);

    for kind in ["Deployment", "Service", "HorizontalPodAutoscaler"] {
        let v = got
            .query(format!("$.status.conditions[?value(@.type) == \"{kind}Created\"]").as_str())
            .unwrap_or_else(|error| panic!("condition for {kind} not populated: {error}"));
        eprintln!("resource {kind}: {v:?}");
    }

    Ok(())
}
