use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
        DynamicObject,
    },
    Client,
};
use warp::{filters::BoxedFilter, Filter, Reply};

use crate::config;
use crate::*;

pub fn validate_clair(client: Client) -> BoxedFilter<(impl Reply,)> {
    let client = warp::any().map(move || client.clone());
    warp::path("validate")
        .and(warp::post())
        .and(warp::body::content_length_limit(4 << 20))
        .and(client.clone())
        .and(warp::body::json())
        .and_then(validate_inner)
        .map(|res| warp::reply::json(&res))
        .with(warp::reply::with::default_header(
            "content-type",
            "application/json",
        ))
        .boxed()
}

/// Validate_inner does the
async fn validate_inner(
    client: Client,
    body: AdmissionReview<v1alpha1::Clair>,
) -> Result<AdmissionReview<DynamicObject>, warp::Rejection> {
    let req: AdmissionRequest<v1alpha1::Clair> = body.try_into().map_err(|e| {
        warp::reject::custom(DeserializeError(format!(
            "unable to convert AdmissionReview: {e}"
        )))
    })?;

    if let Err(e) = check_required(&req).await {
        return Ok(AdmissionResponse::from(&req).deny(e).into_review());
    };
    if let Err(e) = check_immutable(&req).await {
        return Ok(AdmissionResponse::from(&req).deny(e).into_review());
    };

    match req.operation {
        Operation::Create | Operation::Update => (),
        Operation::Delete => {
            let mut res = AdmissionResponse::from(&req);
            res.allowed = true;
            let res = res.into_review();
            return Ok(res);
        }
        Operation::Connect => {
            let res = AdmissionResponse::from(&req)
                .deny("verb CONNECT makes no sense")
                .into_review();
            return Ok(res);
        }
    };
    let o = req.object.as_ref().expect("somehow None");
    let cfg = match next_config(o) {
        Ok(cfg) => cfg,
        Err(e) => {
            let res = AdmissionResponse::from(&req).deny(e).into_review();
            return Ok(res);
        }
    };
    let v = match config::validate(client.clone(), &cfg).await {
        Err(e) => {
            let res = AdmissionResponse::from(&req)
                .deny(format!("validation failed: {e}"))
                .into_review();
            return Ok(res);
        }
        Ok(v) => v,
    };
    for r in &[&v.indexer, &v.matcher, &v.notifier, &v.updater] {
        if let Err(e) = r {
            let res = AdmissionResponse::from(&req)
                .deny(format!("validation failed: {e}"))
                .into_review();
            return Ok(res);
        }
    }
    let mut res = AdmissionResponse::from(&req);
    res.allowed = true;
    let res = res.into_review();
    Ok(res)
}

/// Check_immutable checks for mutation of spec fields that (currently) cannot be changed after
/// creation.
async fn check_immutable(req: &AdmissionRequest<v1alpha1::Clair>) -> Result<(), ValidationFailure> {
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

/// Check_required checks for feilds that must be provided.
async fn check_required(req: &AdmissionRequest<v1alpha1::Clair>) -> Result<(), ValidationFailure> {
    if req.operation != Operation::Create && req.operation != Operation::Update {
        return Ok(());
    }
    let cur = req.object.as_ref().unwrap();
    let spec = &cur.spec;

    if spec.databases.is_none() {
        return Err("field \"/spec/databases\" must be provided".into());
    }
    if spec.notifier.is_some_and(|n| n) && spec.databases.as_ref().unwrap().notifier.is_none() {
        return Err(
            "field \"/spec/notifier\" is set but \"/spec/databases/notifier\" is not".into(),
        );
    }

    Ok(())
}

#[derive(Debug)]
struct DeserializeError(String);
impl warp::reject::Reject for DeserializeError {}

#[derive(Debug)]
struct ValidationFailure(String);
impl warp::reject::Reject for ValidationFailure {}
impl From<&str> for ValidationFailure {
    fn from(v: &str) -> Self {
        Self(String::from(v))
    }
}
impl ToString for ValidationFailure {
    fn to_string(&self) -> String {
        format!("validation failed: {}", &self.0)
    }
}
