use k8s_openapi::api::core;

use api::v1alpha1::Indexer;
use controller::{indexers, Context, Error};
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
async fn initialize_inner(ctx: Arc<Context>) -> Result<(), Error> {
    use self::core::v1::ConfigMap;
    const NAME: &'static str = "indexers-initialize-test";
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
        "apiVersion": "projectclair.io/v1alpha1",
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

    let mut gen: i64 = 0;
    loop {
        let m = api.get_metadata(NAME).await?;
        let cur = m.metadata.generation.unwrap();
        if cur == gen {
            break;
        }
        gen = cur;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let got = api.get(NAME).await?;
    let mut got = serde_json::to_value(got)?;
    let tests: Patch = serde_json::from_value(json! {
        [
            {"op": "test", "path": "/metadata/name", "value": NAME},
            {"op": "test", "path": "/status/config/root/name", "value": format!("{NAME}-config")},
        ]
    })?;
    json_patch::patch(&mut got, &tests)?;

    Ok(())
}
