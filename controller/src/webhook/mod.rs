//! Webhooks for the clair-operator.

use std::sync::Arc;

use axum::{Json, Router, extract, routing::post};
use tower_http::trace::TraceLayer;
#[allow(unused_imports)]
use tracing::{debug, error, info, instrument, trace};

/// State is the webhook application server state.
pub struct State {
    client: kube::Client,
}

impl State {
    /// New creates a new State.
    pub fn new(client: kube::Client) -> State {
        State { client }
    }
}

/// App returns an `axum::Router`.
pub fn app(srv: State) -> Router {
    let state = Arc::new(srv);
    trace!("state constructed");
    let app = Router::new()
        .route("/convert", post(convert::handler))
        .route("/v1alpha1/mutate", post(v1alpha1::mutate::handler))
        .route("/v1alpha1/validate", post(v1alpha1::validate::handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    trace!("router constructed");
    app
}

mod prelude {
    pub use std::sync::Arc;

    pub use axum::{Json, extract, http::StatusCode};
    pub use k8s_openapi::api::core;
    pub use kube::{
        api::Api,
        core::{
            DynamicObject, ResourceExt,
            admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
        },
    };
    pub use serde::Deserialize;
    pub use tracing::{debug, error, info, instrument, trace};

    pub use super::State;
}

mod convert {
    use super::*;

    #[instrument(skip_all)]
    pub async fn handler(extract::Json(_req): Json<()>) -> Json<()> {
        todo!()
    }
}

mod v1alpha1 {
    use super::prelude::*;
    use api::v1alpha1::*;

    /// Review is an enum containing any of the possible types that can be sent to the webhooks.
    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum Review {
        Clair(AdmissionReview<Clair>),
        Indexer(AdmissionReview<Indexer>),
        Matcher(AdmissionReview<Matcher>),
        Notifier(AdmissionReview<Notifier>),
        Updater(AdmissionReview<Updater>),
    }

    // Mutate functions:
    pub(super) mod mutate {
        use super::*;

        use json_patch::jsonptr::PointerBuf;
        use json_patch::{AddOperation as Add, Patch, PatchOperation as Op};
        use serde_json::Value;

        use crate::DEFAULT_IMAGE;

        #[instrument(skip_all)]
        pub async fn handler(
            extract::State(srv): extract::State<Arc<State>>,
            extract::Json(rev): Json<Review>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            match rev {
                Review::Clair(rev) => clair(srv, rev).await,
                Review::Indexer(rev) => indexer(srv, rev).await,
                Review::Matcher(rev) => matcher(srv, rev).await,
                Review::Notifier(rev) => notifier(srv, rev).await,
                Review::Updater(rev) => updater(srv, rev).await,
            }
        }

        #[instrument(skip_all)]
        async fn clair(
            _srv: Arc<State>,
            rev: AdmissionReview<Clair>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Clair> = rev.try_into().map_err(|err| {
                error!(error = %err, "unable to deserialize AdmissionReview");
                StatusCode::BAD_REQUEST
            })?;
            let mut res = AdmissionResponse::from(&req);

            let cur = req.object.as_ref().unwrap();
            if cur.spec.image.is_none() {
                res = res
                    .with_patch(Patch(vec![Op::Add(Add {
                        path: PointerBuf::from_tokens(["spec", "image"]),
                        value: Value::String(DEFAULT_IMAGE.clone()),
                    })]))
                    .expect("programmer error: unable to serialize known data");
            }

            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn indexer(
            _srv: Arc<State>,
            rev: AdmissionReview<Indexer>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Indexer> = rev.try_into().map_err(|err| {
                error!(error = %err, "unable to deserialize AdmissionReview");
                StatusCode::BAD_REQUEST
            })?;
            let res = AdmissionResponse::from(&req);
            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn matcher(
            _srv: Arc<State>,
            rev: AdmissionReview<Matcher>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Matcher> = rev.try_into().map_err(|err| {
                error!(error = %err, "unable to deserialize AdmissionReview");
                StatusCode::BAD_REQUEST
            })?;
            let res = AdmissionResponse::from(&req);
            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn notifier(
            _srv: Arc<State>,
            rev: AdmissionReview<Notifier>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Notifier> = rev.try_into().map_err(|err| {
                error!(error = %err, "unable to deserialize AdmissionReview");
                StatusCode::BAD_REQUEST
            })?;
            let res = AdmissionResponse::from(&req);
            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn updater(
            _srv: Arc<State>,
            rev: AdmissionReview<Updater>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Updater> = rev.try_into().map_err(|err| {
                error!(error = %err, "unable to deserialize AdmissionReview");
                StatusCode::BAD_REQUEST
            })?;
            let res = AdmissionResponse::from(&req);
            Ok(Json(res.into_review()))
        }
    }

    // Validate functions:
    pub(super) mod validate {
        use super::*;

        #[instrument(skip_all)]
        pub async fn handler(
            extract::State(srv): extract::State<Arc<State>>,
            extract::Json(rev): Json<Review>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            match rev {
                Review::Clair(rev) => clair(srv, rev).await,
                Review::Indexer(rev) => indexer(srv, rev).await,
                Review::Matcher(rev) => matcher(srv, rev).await,
                Review::Notifier(rev) => notifier(srv, rev).await,
                Review::Updater(rev) => updater(srv, rev).await,
            }
        }

        /// Custom `Either` type for our config handling.
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
        async fn clair(
            srv: Arc<State>,
            rev: AdmissionReview<Clair>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            debug!("start validate");
            let req: AdmissionRequest<Clair> = match rev.try_into() {
                Ok(req) => req,
                Err(err) => {
                    error!(error = %err, "unable to deserialize AdmissionReview");
                    return Ok(Json(AdmissionResponse::invalid(err).into_review()));
                }
            };
            let mut res = AdmissionResponse::from(&req);
            let _prev = req.old_object.as_ref();
            let cur = req.object.as_ref().unwrap();
            debug!(op = ?req.operation, "doing validation");

            if req.operation == Operation::Create || req.operation == Operation::Update {
                let spec = &cur.spec;
                if spec.image.is_none() {
                    trace!(op = ?req.operation, "image misconfigured");
                    return Ok(Json(
                        res.deny("field \"/spec/image\" must be provided")
                            .into_review(),
                    ));
                }
                trace!(op = ?req.operation, "image OK");

                if spec.databases.is_none() {
                    trace!(op = ?req.operation, "databases misconfigured");
                    return Ok(Json(
                        res.deny("field \"/spec/databases\" must be provided")
                            .into_review(),
                    ));
                }
                trace!(op = ?req.operation, "databases OK");

                if spec.notifier == Some(true)
                    && spec.databases.as_ref().unwrap().notifier.is_none()
                {
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

            let cm_api: Api<core::v1::ConfigMap> = Api::default_namespaced(srv.client.clone());
            let sec_api: Api<core::v1::Secret> = Api::default_namespaced(srv.client.clone());

            let cfgsrc = cur.spec.with_root(format!("{}-config", cur.name_any()));
            let root = match cm_api.get_opt(&cfgsrc.root.name).await {
                Ok(root) => root,
                Err(err) => return Ok(Json(AdmissionResponse::invalid(err).into_review())),
            };
            let root = if root.is_none() {
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
            let mut errd = 0usize;
            let warn = to_check
                .iter()
                .filter_map(|r| {
                    if let Err(err) = r {
                        errd = errd.saturating_add(1);
                        Some(err.to_string())
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
            info!("OK");
            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn indexer(
            _srv: Arc<State>,
            rev: AdmissionReview<Indexer>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Indexer> = match rev.try_into() {
                Ok(req) => req,
                Err(err) => {
                    error!(error = %err, "unable to deserialize AdmissionReview");
                    return Ok(Json(AdmissionResponse::invalid(err).into_review()));
                }
            };
            let res = AdmissionResponse::from(&req);
            let cur = req.object.as_ref().unwrap();
            debug!(op = ?req.operation, "doing validation");

            match req.operation {
                Operation::Create | Operation::Update => (),
                Operation::Delete | Operation::Connect => return Ok(Json(res.into_review())),
            };

            if cur.spec.config.is_none() {
                info!("missing config source");
                return Ok(Json(
                    res.deny("missing configuration source \"/spec/config\"")
                        .into_review(),
                ));
            }

            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn matcher(
            _srv: Arc<State>,
            rev: AdmissionReview<Matcher>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Matcher> = match rev.try_into() {
                Ok(req) => req,
                Err(err) => {
                    error!(error = %err, "unable to deserialize AdmissionReview");
                    return Ok(Json(AdmissionResponse::invalid(err).into_review()));
                }
            };
            let res = AdmissionResponse::from(&req);
            info!("TODO");
            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn notifier(
            _srv: Arc<State>,
            rev: AdmissionReview<Notifier>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Notifier> = match rev.try_into() {
                Ok(req) => req,
                Err(err) => {
                    error!(error = %err, "unable to deserialize AdmissionReview");
                    return Ok(Json(AdmissionResponse::invalid(err).into_review()));
                }
            };
            let res = AdmissionResponse::from(&req);
            info!("TODO");
            Ok(Json(res.into_review()))
        }

        #[instrument(skip_all)]
        async fn updater(
            _srv: Arc<State>,
            rev: AdmissionReview<Updater>,
        ) -> Result<Json<AdmissionReview<DynamicObject>>, StatusCode> {
            let req: AdmissionRequest<Updater> = match rev.try_into() {
                Ok(req) => req,
                Err(err) => {
                    error!(error = %err, "unable to deserialize AdmissionReview");
                    return Ok(Json(AdmissionResponse::invalid(err).into_review()));
                }
            };
            let res = AdmissionResponse::from(&req);
            info!("TODO");
            Ok(Json(res.into_review()))
        }
    }
}

#[cfg(test)]
mod tests {
    //use super::*;
}
