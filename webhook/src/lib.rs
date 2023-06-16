//! Webhooks for the clair-operator.

use std::sync::Arc;

use axum::{extract, http::StatusCode, routing::post, Json, Router};
use futures::Stream;
use hyper::server::accept;
use kube::core::{
    admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
    DynamicObject, ResourceExt,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use api::v1alpha1;
use clair_config;

pub struct State {
    client: kube::Client,
}

impl State {
    pub fn new(client: kube::Client) -> State {
        State { client }
    }
}

pub async fn run<S, IE, IO>(
    stream: S,
    srv: State,
    cancel: CancellationToken,
) -> Result<(), hyper::Error>
where
    S: Stream<Item = Result<IO, IE>>,
    IE: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let state = Arc::new(srv);
    let app = Router::new()
        .route("/convert", post(convert))
        .route("/v1alpha1/mutate/clair", post(mutate_v1alpha1_clair))
        .route("/v1alpha1/validate/clair", post(validate_v1alpha1_clair))
        .with_state(state);
    let s = accept::from_stream(stream);

    axum::Server::builder(s)
        .serve(app.into_make_service())
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await
}

async fn convert(extract::Json(_req): Json<()>) -> Json<()> {
    todo!()
}

async fn mutate_v1alpha1_clair(
    extract::State(_srv): extract::State<Arc<State>>,
    extract::Json(rev): Json<AdmissionReview<v1alpha1::Clair>>,
) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
    let req: AdmissionRequest<v1alpha1::Clair> = rev.try_into().map_err(|_| {
        // TODO(hank) Log something.
        StatusCode::BAD_REQUEST
    })?;
    let res = AdmissionResponse::from(&req);
    Ok(Json(res.into_review()))
}

async fn validate_v1alpha1_clair(
    extract::State(_srv): extract::State<Arc<State>>,
    extract::Json(rev): Json<AdmissionReview<v1alpha1::Clair>>,
) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
    let req: AdmissionRequest<v1alpha1::Clair> = match rev.try_into() {
        Ok(req) => req,
        Err(err) => return Ok(Json(AdmissionResponse::invalid(err).into_review())),
    };
    let mut res = AdmissionResponse::from(&req);
    let prev = req.old_object.as_ref().unwrap();
    let cur = req.object.as_ref().unwrap();

    if req.operation == Operation::Create || req.operation == Operation::Update {
        let spec = &cur.spec;
        if spec.databases.is_none() {
            return Ok(Json(
                res.deny("field \"/spec/databases\" must be provided")
                    .into_review(),
            ));
        }
        if spec.notifier == Some(true) && spec.databases.as_ref().unwrap().notifier.is_none() {
            return Ok(Json(
                res.deny("field \"/spec/notifier\" is set but \"/spec/databases/notifier\" is not")
                    .into_review(),
            ));
        }
    }

    if req.operation == Operation::Update {
        if prev.spec.config_dialect != cur.spec.config_dialect {
            return Ok(Json(
                res.deny("cannot change field \"/spec/configDialect\"")
                    .into_review(),
            ));
        }
    }

    let obj = req.object.as_ref().expect("somehow None");
    let cfgsrc = obj.spec.with_root(format!("{}-config", obj.name_any()));
    let v = match clair_config::validate(&_srv.client, &cfgsrc).await {
        Ok(v) => v,
        Err(_err) => {
            // TODO(hank) log
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let to_check = [&v.indexer, &v.matcher, &v.notifier, &v.updater];
    let mut errd = 0;
    let warn = to_check
        .iter()
        .filter_map(|r| {
            if let Err(err) = r {
                errd += 1;
                Some(format!("{err}"))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if warn.len() != 0 {
        res.warnings = Some(warn);
    }

    if errd == to_check.len() && req.operation == Operation::Update {
        return Ok(Json(
            res.deny("configuration change is extremely invalid")
                .into_review(),
        ));
    }
    Ok(Json(res.into_review()))
}

#[cfg(test)]
mod tests {
    //use super::*;

    #[test]
    fn it_works() {
        assert!(true);
    }
}
