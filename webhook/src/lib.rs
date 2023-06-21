//! Webhooks for the clair-operator.

use std::sync::Arc;

use axum::{extract, http::StatusCode, routing::post, Json, Router};
use futures::Stream;
use hyper::server::accept;
use k8s_openapi::api::core;
use kube::{
    api::Api,
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
        DynamicObject, ResourceExt,
    },
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info, instrument, trace};

use api::v1alpha1;

pub struct State {
    client: kube::Client,
}

impl State {
    pub fn new(client: kube::Client) -> State {
        State { client }
    }
}

pub fn app(srv: State) -> Router {
    let state = Arc::new(srv);
    trace!("state constructed");
    let app = Router::new()
        .route("/convert", post(convert))
        .route("/v1alpha1/mutate/clair", post(mutate_v1alpha1_clair))
        .route("/v1alpha1/validate/clair", post(validate_v1alpha1_clair))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    trace!("router constructed");
    app
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
    trace!("router constructed");
    let s = accept::from_stream(stream);
    trace!("stream constructed");
    let app = app(srv);

    info!("webhook server starting");
    axum::Server::builder(s)
        .serve(app.into_make_service())
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await
}

#[instrument(skip_all)]
async fn convert(extract::Json(_req): Json<()>) -> Json<()> {
    todo!()
}

#[instrument(skip_all)]
async fn mutate_v1alpha1_clair(
    extract::State(_srv): extract::State<Arc<State>>,
    extract::Json(rev): Json<AdmissionReview<v1alpha1::Clair>>,
) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
    let req: AdmissionRequest<v1alpha1::Clair> = rev.try_into().map_err(|err| {
        error!(error = %err, "unable to deserialize AdmissionReview");
        StatusCode::BAD_REQUEST
    })?;
    let res = AdmissionResponse::from(&req);
    Ok(Json(res.into_review()))
}

enum Either {
    ConfigMap(core::v1::ConfigMap),
    Secret(core::v1::Secret),
}
impl From<core::v1::ConfigMap> for Either {
    fn from(value: core::v1::ConfigMap) -> Self {
        Self::ConfigMap(value)
    }
}
impl From<core::v1::Secret> for Either {
    fn from(value: core::v1::Secret) -> Self {
        Self::Secret(value)
    }
}

#[instrument(skip_all)]
async fn validate_v1alpha1_clair(
    extract::State(srv): extract::State<Arc<State>>,
    extract::Json(rev): Json<AdmissionReview<v1alpha1::Clair>>,
) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
    debug!("start validate");
    let req: AdmissionRequest<v1alpha1::Clair> = match rev.try_into() {
        Ok(req) => req,
        Err(err) => {
            error!(error = %err, "unable to deserialize AdmissionReview");
            return Ok(Json(AdmissionResponse::invalid(err).into_review()));
        }
    };
    let mut res = AdmissionResponse::from(&req);
    let prev = req.old_object.as_ref();
    let cur = req.object.as_ref().unwrap();
    debug!(op = ?req.operation, "doing validation");

    if req.operation == Operation::Create || req.operation == Operation::Update {
        let spec = &cur.spec;
        if spec.databases.is_none() {
            trace!(op = ?req.operation, "databases misconfigured");
            return Ok(Json(
                res.deny("field \"/spec/databases\" must be provided")
                    .into_review(),
            ));
        }
        trace!(op = ?req.operation, "databases OK");
        if spec.notifier == Some(true) && spec.databases.as_ref().unwrap().notifier.is_none() {
            trace!(op = ?req.operation, "notifier misconfigured");
            return Ok(Json(
                res.deny("field \"/spec/notifier\" is set but \"/spec/databases/notifier\" is not")
                    .into_review(),
            ));
        }
        trace!(op = ?req.operation, "notifier OK");
        for (i, d) in spec.dropins.iter().enumerate() {
            if d.config_map_key_ref.is_none() && d.secret_key_ref.is_none() {
                trace!(op = ?req.operation, index = i, "dropins misconfigured");
                return Ok(Json(
                    res.deny(format!("invalid dropin at index {i}: no ref specified"))
                        .into_review(),
                ));
            }
        }
        trace!(op = ?req.operation, "dropins OK");
    }

    if req.operation == Operation::Update {
        let prev = prev.unwrap();
        if prev.spec.config_dialect != cur.spec.config_dialect {
            trace!(op = ?req.operation, "unable to change configDialect");
            return Ok(Json(
                res.deny("cannot change field \"/spec/configDialect\"")
                    .into_review(),
            ));
        }
    }

    let cm_api: Api<core::v1::ConfigMap> = Api::default_namespaced(srv.client.clone());
    let sec_api: Api<core::v1::Secret> = Api::default_namespaced(srv.client.clone());

    let cfgsrc = cur.spec.with_root(format!("{}-config", cur.name_any()));
    let root = match cm_api.get_opt(&cfgsrc.root.name).await {
        Ok(root) => root,
        Err(err) => return Ok(Json(AdmissionResponse::invalid(err).into_review())),
    };
    let root = if let None = root {
        return Ok(Json(res.deny("no such config: {name}").into_review()));
    } else {
        root.unwrap()
    };

    let mut b = match clair_config::Builder::from_root(&root, cfgsrc.root.key.clone()) {
        Ok(b) => b,
        Err(err) => return Ok(Json(AdmissionResponse::invalid(err).into_review())),
    };
    let mut ds = Vec::new();
    for d in cfgsrc.dropins.iter() {
        if let Some(r) = &d.config_map_key_ref {
            let name = &r.name;
            let m = match cm_api.get_opt(name).await {
                Ok(m) => m,
                Err(err) => return Ok(Json(AdmissionResponse::invalid(err).into_review())),
            };
            if m.is_none() {
                return Ok(Json(res.deny("no such config: {name}").into_review()));
            };
            ds.push((Either::from(m.unwrap()), &r.key));
        } else if let Some(r) = &d.secret_key_ref {
            let name = &r.name;
            let m = match sec_api.get_opt(name).await {
                Ok(m) => m,
                Err(err) => return Ok(Json(AdmissionResponse::invalid(err).into_review())),
            };
            if m.is_none() {
                return Ok(Json(res.deny("no such config: {name}").into_review()));
            };
            ds.push((Either::from(m.unwrap()), &r.key));
        } else {
            unreachable!()
        }
    }
    for (d, key) in ds {
        b = match match d {
            Either::ConfigMap(v) => b.add(v, key),
            Either::Secret(v) => b.add(v, key),
        } {
            Ok(b) => b,
            Err(err) => return Ok(Json(AdmissionResponse::invalid(err).into_review())),
        };
    }

    let p: clair_config::Parts = b.into();
    let v = match p.validate().await {
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
    if !warn.is_empty() {
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
}
