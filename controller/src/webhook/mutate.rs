use std::convert::Infallible;

use bytes::{Buf, Bytes};
use json_patch::{AddOperation, Patch, PatchOperation};
use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
        DynamicObject,
    },
    Client,
};
use warp::{filters::BoxedFilter, Filter, Reply};

use super::{predicate, DynError};
use crate::prelude::*;

pub fn webhook(client: Client) -> BoxedFilter<(impl Reply,)> {
    let client = warp::any().map(move || client.clone());
    predicate("mutate")
        .and(client)
        .and(warp::body::bytes())
        .and_then(mutate_v1alpha1)
        .map(|reply| warp::reply::with_header(reply, "content-type", "application/json"))
        .boxed()
}

macro_rules! mutate_kinds {
    ($rev:expr, $client:expr, $body:expr, $( ( $name:literal, $func:ident) ),+) => {{
        let req: AdmissionRequest<_> = match $rev.clone().try_into() {
            Ok(req) => req,
            Err(err) => {
                error!("invalid request: {}", err.to_string());
                return Ok(warp::reply::json(
                    &AdmissionResponse::invalid(err.to_string()).into_review(),
                ));
            }
        };
        let kind = req.kind.kind;
        match kind.as_str() {
            $(
            $name => match serde_json::from_reader($body.reader()) {
                Ok(r) => $func($client, r).await,
                Err(err) => Err(err.into()),
            },
            )+
            k => Err(format!("unknown kind: {k}").into()),
        }
    }};
}

async fn mutate_v1alpha1(client: Client, body: Bytes) -> Result<impl Reply, Infallible> {
    let rev: AdmissionReview<DynamicObject> = match serde_json::from_reader(body.clone().reader()) {
        Ok(v) => v,
        Err(err) => {
            error!("invalid request: {}", err.to_string());
            return Ok(warp::reply::json(
                &AdmissionResponse::invalid(err.to_string()).into_review(),
            ));
        }
    };
    let res = mutate_kinds!(
        rev,
        client,
        body,
        ("Clair", mutate_clair),
        ("Indexer", mutate_ok),
        ("Matcher", mutate_ok),
        ("Notifier", mutate_ok),
        ("Updater", mutate_ok)
    );
    match res {
        Ok(res) => Ok(warp::reply::json(&res.into_review())),
        Err(err) => {
            error!("invalid request: {}", err.to_string());
            Ok(warp::reply::json(
                &AdmissionResponse::invalid(err.to_string()).into_review(),
            ))
        }
    }
}

/// Mutate_ok just reports OK without changes.
async fn mutate_ok(
    _client: Client,
    rev: AdmissionReview<DynamicObject>,
) -> Result<AdmissionResponse, DynError> {
    let req: AdmissionRequest<_> = rev.try_into()?;
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;
    Ok(res)
}

/// Mutate_clair sets defaults for Clair kinds.
async fn mutate_clair(
    _client: Client,
    rev: AdmissionReview<v1alpha1::Clair>,
) -> Result<AdmissionResponse, DynError> {
    let req: AdmissionRequest<_> = rev.try_into()?;
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;

    if req.operation != Operation::Create {
        return Ok(res);
    }
    let mut patches = Vec::new();
    let spec = &req.object.as_ref().unwrap().spec;
    if spec.config_dialect.is_none() {
        res.warnings
            .get_or_insert(Vec::new())
            .push("set /spec/configDialect to default `\"json\"`".into());
        patches.push(PatchOperation::Add(AddOperation {
            path: "/spec/configDialect".into(),
            value: serde_json::json!("json"),
        }));
    }
    if spec.notifier.is_none() {
        res.warnings
            .get_or_insert(Vec::new())
            .push("set /spec/notifier to default `false`".into());
        patches.push(PatchOperation::Add(AddOperation {
            path: "/spec/notifier".into(),
            value: serde_json::json!("false"),
        }));
    }

    let res = res.with_patch(Patch(patches))?;
    Ok(res)
}
