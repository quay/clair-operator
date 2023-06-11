use std::convert::Infallible;

use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
        DynamicObject,
    },
    Client,
};
use warp::{filters::BoxedFilter, Filter, Reply};

use super::{predicate, DynError};
use crate::config;
use crate::next_config;
use crate::prelude::*;

pub fn webhook(client: Client) -> BoxedFilter<(impl Reply,)> {
    let client = warp::any().map(move || client.clone());
    predicate("validate")
        .and(client)
        .and(warp::body::json())
        .and_then(validate_v1alpha1)
        .map(|reply| warp::reply::with_header(reply, "content-type", "application/json"))
        .boxed()
}

macro_rules! validate_kinds {
    ($req:expr, $client:expr, $v:expr, $( ( $name:literal, $func:ident) ),+) => {
        match $req.kind.kind.as_str() {
            $(
            $name => match serde_json::from_value($v) {
                Ok(r) => $func($client, r).await,
                Err(err) => Err(err.into()),
            },
            )+
            k => Err(format!("unknown kind: {k}").into()),
        }
    };
}

async fn validate_v1alpha1(
    client: Client,
    body: AdmissionReview<DynamicObject>,
) -> Result<impl Reply, Infallible> {
    let req: AdmissionRequest<_> = match body.clone().try_into() {
        Ok(req) => req,
        Err(err) => {
            error!("invalid request: {}", err.to_string());
            return Ok(warp::reply::json(
                &AdmissionResponse::invalid(err.to_string()).into_review(),
            ));
        }
    };
    let v = match serde_json::to_value(body) {
        Ok(v) => v,
        Err(err) => {
            error!("invalid request: {}", err.to_string());
            return Ok(warp::reply::json(
                &AdmissionResponse::invalid(err.to_string()).into_review(),
            ));
        }
    };

    let res = validate_kinds!(
        req,
        client,
        v,
        ("Clair", validate_clair),
        ("Indexer", validate_indexer),
        ("Matcher", validate_matcher),
        ("Notifier", validate_notifier),
        ("Updater", validate_updater)
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

/// Validate_clair is the hook for the Clair type.
async fn validate_clair(
    client: Client,
    rev: AdmissionReview<v1alpha1::Clair>,
) -> Result<AdmissionResponse, DynError> {
    let req: AdmissionRequest<_> = rev.try_into()?;
    check_clair_required(&req).await?;
    check_clair_immutable(&req).await?;

    let o = req.object.as_ref().expect("somehow None");
    let cfg = next_config(o)?;
    let v = config::validate(client.clone(), &cfg).await?;
    for r in &[&v.indexer, &v.matcher, &v.notifier, &v.updater] {
        if let Err(e) = r {
            return Err(format!("validation failed: {e}").into());
        }
    }
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;
    Ok(res)
}
async fn check_clair_immutable(req: &AdmissionRequest<v1alpha1::Clair>) -> Result<(), DynError> {
    if req.operation != Operation::Update {
        return Ok(());
    }
    let prev = req.old_object.as_ref().unwrap();
    let cur = req.object.as_ref().unwrap();

    if prev.spec.config_dialect != cur.spec.config_dialect {
        return Err("cannot change field \"/spec/configDialect\"".into());
    }

    Ok(())
}
async fn check_clair_required(req: &AdmissionRequest<v1alpha1::Clair>) -> Result<(), DynError> {
    if req.operation != Operation::Create && req.operation != Operation::Update {
        return Ok(());
    }
    let cur = req.object.as_ref().unwrap();
    let spec = &cur.spec;

    if spec.databases.is_none() {
        return Err("field \"/spec/databases\" must be provided".into());
    }
    if spec.notifier == Some(true) && spec.databases.as_ref().unwrap().notifier.is_none() {
        return Err(
            "field \"/spec/notifier\" is set but \"/spec/databases/notifier\" is not".into(),
        );
    }

    Ok(())
}

async fn validate_indexer(
    _client: Client,
    rev: AdmissionReview<v1alpha1::Indexer>,
) -> Result<AdmissionResponse, DynError> {
    let req: AdmissionRequest<_> = rev.try_into()?;
    check_indexer_required(&req).await?;
    check_indexer_immutable(&req).await?;
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;
    Ok(res)
}
async fn check_indexer_required(req: &AdmissionRequest<v1alpha1::Indexer>) -> Result<(), DynError> {
    if req.operation != Operation::Create && req.operation != Operation::Update {
        return Ok(());
    }
    let cur = req.object.as_ref().unwrap();
    let _spec = &cur.spec;

    Ok(())
}
async fn check_indexer_immutable(
    req: &AdmissionRequest<v1alpha1::Indexer>,
) -> Result<(), DynError> {
    if req.operation != Operation::Update {
        return Ok(());
    }
    let _prev = req.old_object.as_ref().unwrap();
    let _cur = req.object.as_ref().unwrap();

    Ok(())
}

async fn validate_matcher(
    _client: Client,
    rev: AdmissionReview<v1alpha1::Matcher>,
) -> Result<AdmissionResponse, DynError> {
    let req: AdmissionRequest<_> = rev.try_into()?;
    check_matcher_required(&req).await?;
    check_matcher_immutable(&req).await?;
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;
    Ok(res)
}
async fn check_matcher_required(req: &AdmissionRequest<v1alpha1::Matcher>) -> Result<(), DynError> {
    if req.operation != Operation::Create && req.operation != Operation::Update {
        return Ok(());
    }
    let cur = req.object.as_ref().unwrap();
    let _spec = &cur.spec;

    Ok(())
}
async fn check_matcher_immutable(
    req: &AdmissionRequest<v1alpha1::Matcher>,
) -> Result<(), DynError> {
    if req.operation != Operation::Update {
        return Ok(());
    }
    let _prev = req.old_object.as_ref().unwrap();
    let _cur = req.object.as_ref().unwrap();

    Ok(())
}

async fn validate_notifier(
    _client: Client,
    rev: AdmissionReview<v1alpha1::Notifier>,
) -> Result<AdmissionResponse, DynError> {
    let req: AdmissionRequest<_> = rev.try_into()?;
    check_notifier_required(&req).await?;
    check_notifier_immutable(&req).await?;
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;
    Ok(res)
}
async fn check_notifier_required(
    req: &AdmissionRequest<v1alpha1::Notifier>,
) -> Result<(), DynError> {
    if req.operation != Operation::Create && req.operation != Operation::Update {
        return Ok(());
    }
    let cur = req.object.as_ref().unwrap();
    let _spec = &cur.spec;

    Ok(())
}
async fn check_notifier_immutable(
    req: &AdmissionRequest<v1alpha1::Notifier>,
) -> Result<(), DynError> {
    if req.operation != Operation::Update {
        return Ok(());
    }
    let _prev = req.old_object.as_ref().unwrap();
    let _cur = req.object.as_ref().unwrap();

    Ok(())
}

async fn validate_updater(
    _client: Client,
    rev: AdmissionReview<v1alpha1::Updater>,
) -> Result<AdmissionResponse, DynError> {
    let req: AdmissionRequest<_> = rev.try_into()?;
    check_updater_required(&req).await?;
    check_updater_immutable(&req).await?;
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;
    Ok(res)
}
async fn check_updater_required(req: &AdmissionRequest<v1alpha1::Updater>) -> Result<(), DynError> {
    if req.operation != Operation::Create && req.operation != Operation::Update {
        return Ok(());
    }
    let cur = req.object.as_ref().unwrap();
    let _spec = &cur.spec;

    Ok(())
}
async fn check_updater_immutable(
    req: &AdmissionRequest<v1alpha1::Updater>,
) -> Result<(), DynError> {
    if req.operation != Operation::Update {
        return Ok(());
    }
    let _prev = req.old_object.as_ref().unwrap();
    let _cur = req.object.as_ref().unwrap();

    Ok(())
}
