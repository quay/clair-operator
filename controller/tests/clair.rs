use std::collections::BTreeMap;

use k8s_openapi::{ByteString, api::core, apimachinery::pkg::apis::meta};

use api::v1alpha1::Clair;
use controller::{Context, Error, clairs};
mod util;
use util::prelude::*;

#[crate::test(tokio::test(flavor = "multi_thread", worker_threads = 1))]
#[cfg_attr(not(feature = "test_ci"), ignore)]
async fn initialize() -> Result<(), Error> {
    let ctx = util::test_context().await;
    util::load_crds(&ctx.client).await?;

    let token = CancellationToken::new();
    let mut ctrls = task::JoinSet::new();
    ctrls.spawn(clairs::controller(token.clone(), ctx.clone())?);
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
    use self::core::v1::Secret;
    use self::meta::v1::ObjectMeta;
    const NAME: &str = "clair-initialize-test";
    let cfgname = format!("{NAME}-db");

    let dbcfg = json!({
        "indexers": {"connstring": ""},
        "matchers": {"connstring": ""},
    })
    .to_string();
    let s: Secret = Secret {
        metadata: ObjectMeta {
            name: Some(cfgname.clone()),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            "db.json".into(),
            ByteString(dbcfg.into()),
        )])),
        ..Default::default()
    };
    let _s = Api::<Secret>::default_namespaced(ctx.client.clone())
        .create(&PostParams::default(), &s)
        .await?;

    let api: Api<Clair> = Api::default_namespaced(ctx.client.clone());
    let c: Clair = serde_json::from_value(json!({
        "apiVersion": format!("{}/v1alpha1", api::GROUP),
        "kind": "Clair",
        "metadata": {"name": NAME},
        "spec": {
            "databases": {
                "indexer": { "name": cfgname, "key": "db.json" },
                "matcher": { "name": cfgname, "key": "db.json" },
            },
            "image": "quay.io/projectquay/clair:nightly",
        },
    }))?;
    eprintln!("attempting to create new Clair");
    let post_params = PostParams {
        dry_run: false,
        field_manager: Some("controller-tests".into()),
    };
    api.create(&post_params, &c).await?;
    eprintln!("created new Clair");
    let mut rev = "".into();
    loop {
        let m = api.get_status(NAME).await?;
        let cur = m.metadata.resource_version.unwrap();
        if cur == rev && m.status.is_some() {
            break;
        }
        rev = cur;
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    eprintln!("Clair settled: {rev}");

    // Check Clair members
    let got = api.get_status(NAME).await?;
    eprintln!("{}", serde_json::to_string_pretty(&got)?);
    let mut got = serde_json::to_value(got)?;
    let tests: Patch = serde_json::from_value(json! {
        [
            {"op": "test", "path": "/metadata/name", "value": NAME},
            {"op": "test", "path": "/status/config/root/name", "value": NAME},
        ]
    })?;
    json_patch::patch(&mut got, &tests)?;

    /*
    // Check Ingress members
    let api: Api<networking::v1::Ingress> = Api::default_namespaced(api.into_client());
    let got = api.get_status(NAME).await?;
    let mut got = serde_json::to_value(got)?;
    let tests: Patch = serde_json::from_value(json! {
        [
            {"op": "test", "path": "/metadata/name", "value": NAME},
        ]
    })?;
    json_patch::patch(&mut got, &tests)?;
    */

    Ok(())
}
