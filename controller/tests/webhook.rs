use axum::{
    body::Body,
    http::{StatusCode, request::Request},
};
use kube::core::admission::AdmissionReview;
use serde_json::{from_slice, json, to_vec};
use test_log::test;
use tower::util::ServiceExt; // for `oneshot` and `ready`

use api::v1alpha1::*;
use util::app;

mod util {
    use controller::webhook;

    pub async fn app() -> axum::Router {
        let client = match kube::Client::try_default().await {
            Ok(c) => c,
            Err(e) => panic!("error starting webhook server: {e}"),
        };
        let s = webhook::State::new(client);
        webhook::app(s)
    }
}

const RESPONSE_LIMIT: usize = 1024 * 1024 * 10;

mod validate {
    use super::*;
    mod clair {
        use super::*;

        #[self::test(tokio::test)]
        #[cfg_attr(not(feature = "test_ci"), ignore)]
        async fn empty() {
            let app = app().await;

            let adm: Vec<u8> = to_vec(&json!({
                "apiVersion": "admission.k8s.io/v1",
                "kind": "AdmissionReview",
                "request":{
                    "kind": {
                        "group": "clairproject.org",
                        "version": "v1alpha1",
                        "kind": "Clair",
                    },
                    "resource": {
                        "group": "clairproject.org",
                        "version": "v1alpha1",
                        "resource": "clairs",
                    },
                    "uid": "00",
                    "name": "test",
                    "namespace": "default",
                    "operation": "CREATE",
                    "object": Clair::new("test", Default::default()),
                    "userInfo":{
                        "username": "admin",
                        "uid": "0",
                        "groups": ["admin"],
                    },
                },
            }))
            .expect("JSON serialization failure");
            let response = app
                .oneshot(
                    Request::post("/v1alpha1/validate")
                        .header("content-type", "application/json")
                        .header("accept", "application/json")
                        .body(Body::from(adm))
                        .expect("unable to build request"),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let buf = axum::body::to_bytes(response.into_body(), RESPONSE_LIMIT)
                .await
                .expect("error reading response body");
            let rev: AdmissionReview<Clair> =
                from_slice(&buf).expect("error deserializing response");
            assert!(rev.response.is_some());
            let response = rev.response.unwrap();
            assert!(!response.allowed);
        }
    }
}

mod mutate {
    use super::*;
    mod clair {
        use super::*;

        #[self::test(tokio::test)]
        #[cfg_attr(not(feature = "test_ci"), ignore)]
        async fn none() {
            let app = app().await;

            let mut object = Clair::new("test", Default::default());
            object.spec.databases = Databases {
                matcher: SecretKeySelector {
                    name: "configmap".into(),
                    key: "db.json".into(),
                },
                indexer: SecretKeySelector {
                    name: "configmap".into(),
                    key: "db.json".into(),
                },
                ..Default::default()
            }
            .into();
            object.spec.image = "localhost/test:1".to_string().into();

            let adm: Vec<u8> = to_vec(&json!({
                "apiVersion": "admission.k8s.io/v1",
                "kind": "AdmissionReview",
                "request":{
                    "kind": {
                        "group": "clairproject.org",
                        "version": "v1alpha1",
                        "kind": "Clair",
                    },
                    "resource": {
                        "group": "clairproject.org",
                        "version": "v1alpha1",
                        "resource": "clairs",
                    },
                    "uid": "00",
                    "name": "test",
                    "namespace": "default",
                    "operation": "CREATE",
                    "object": object,
                    "userInfo":{
                        "username": "admin",
                        "uid": "0",
                        "groups": ["admin"],
                    },
                },
            }))
            .expect("JSON serialization failure");
            let response = app
                .oneshot(
                    Request::post("/v1alpha1/mutate")
                        .header("content-type", "application/json")
                        .header("accept", "application/json")
                        .body(Body::from(adm))
                        .expect("unable to build request"),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let buf = axum::body::to_bytes(response.into_body(), RESPONSE_LIMIT)
                .await
                .expect("error reading response body");
            let rev: AdmissionReview<Clair> =
                from_slice(&buf).expect("error deserializing response");
            assert!(rev.response.is_some());
            let response = rev.response.unwrap();
            assert!(response.allowed);
            assert!(response.patch.is_none());
        }
    }
}
