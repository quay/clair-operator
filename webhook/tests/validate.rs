use hyper::{Request, StatusCode};
use kube::core::admission::AdmissionReview;
use serde_json::{from_slice, json, to_vec};
use test_log::test;
use tower::ServiceExt; // for `oneshot` and `ready`

use api::v1alpha1;
use util::app;

mod util;

#[test(tokio::test)]
async fn validate() {
    use v1alpha1::Clair;
    let app = app().await;

    let adm: Vec<u8> = to_vec(&json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request":{
            "kind": {
                "group": "projectclair.io",
                "version": "v1alpha1",
                "kind": "Clair",
            },
            "resource": {
                "group": "projectclair.io",
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
            Request::post("/v1alpha1/validate/clair")
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .body(adm.into())
                .expect("unable to build request"),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let buf = hyper::body::to_bytes(response.into_body())
        .await
        .expect("error reading response body");
    let rev: AdmissionReview<Clair> = from_slice(&buf).expect("error deserializing response");
    assert!(rev.response.is_some());
    let response = rev.response.unwrap();
    assert!(!response.allowed);
}
