use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{
    api::TypeMeta,
    core::{
        admission::{AdmissionRequest, AdmissionReview, Operation},
        GroupVersionKind, GroupVersionResource, Resource,
    },
};
use lazy_static::lazy_static;
use reqwest::StatusCode;
use test_log::test;
use tokio::{sync::oneshot, task};
use warp::{self, Filter};

use api::v1alpha1;
use controller::*;

mod util;

lazy_static! {
    static ref ADMISSION_TYPE: TypeMeta = TypeMeta {
        api_version: "admission.k8s.io/v1".to_string(),
        kind: "AdmissionReview".to_string(),
    };
}

#[test(tokio::test)]
#[cfg_attr(not(feature = "test_ci"), ignore)]
async fn mutate() {
    let srv = TestServer::new().await;
    let c = make_client();
    // Setup above here.

    let objtype = TypeMeta {
        api_version: v1alpha1::Clair::api_version(&()).to_string(),
        kind: v1alpha1::Clair::kind(&()).to_string(),
    };
    let obj = v1alpha1::Clair {
        metadata: ObjectMeta {
            name: Some("test".into()),
            ..Default::default()
        },
        spec: v1alpha1::ClairSpec {
            ..Default::default()
        },
        status: None,
    };
    let v = AdmissionRequest {
        types: ADMISSION_TYPE.clone(),
        uid: "test".into(),
        name: "v1alpha1.validate.projectclair.io".into(),
        namespace: Some("default".into()),
        user_info: Default::default(),
        operation: Operation::Create,
        dry_run: false,
        old_object: None,
        object: Some(obj),
        options: None,
        sub_resource: None,
        resource: GroupVersionResource::gvr(
            &v1alpha1::Clair::group(&()),
            &v1alpha1::Clair::version(&()),
            &v1alpha1::Clair::plural(&()),
        ),
        kind: GroupVersionKind::try_from(&objtype).expect("programmer error"),
        request_sub_resource: None,
        request_kind: None,
        request_resource: None,
    };
    let res = c
        .post(format!("http://{}/mutate/v1alpha1", &srv.addr))
        .json(&AdmissionReview {
            response: None,
            types: ADMISSION_TYPE.clone(),
            request: Some(v),
        })
        .send()
        .await;
    let res = match res {
        Ok(res) => res,
        Err(err) => panic!("unexpected request error: {err}"),
    };
    match res.status() {
        StatusCode::OK => (),
        c => panic!("unexpected status code: {c}"),
    }
    let _: AdmissionReview<v1alpha1::Clair> =
        res.json().await.expect("error deserializing response");

    // Teardown below here.
    srv.done().await;
}

fn make_client() -> reqwest::Client {
    use reqwest::{
        header::{HeaderMap, HeaderName, HeaderValue},
        Client,
    };
    use std::str::FromStr;
    Client::builder()
        .default_headers(HeaderMap::from_iter(vec![
            (
                HeaderName::from_str("accept").expect("programmer error"),
                HeaderValue::from_static("application/json"),
            ),
            (
                HeaderName::from_str("content-type").expect("programmer error"),
                HeaderValue::from_static("application/json"),
            ),
        ]))
        .build()
        .expect("unable to build HTTP client")
}

struct TestServer {
    addr: std::net::SocketAddr,
    tx: oneshot::Sender<()>,
    h: task::JoinHandle<()>,
}
impl TestServer {
    async fn new() -> Self {
        let (tx, rx) = oneshot::channel::<()>();
        let client = match kube::Client::try_default().await {
            Ok(c) => c,
            Err(e) => panic!("error starting webhook server: {e}"),
        };
        let index = warp::path::end().map(|| {
            warp::http::Response::builder()
                .header("content-type", "text/plain; charset=utf-8")
                .body("hello from clair-operator\n")
        });
        let routes = index
            .or(webhook::validate(client.clone()))
            .or(webhook::mutate(client));

        let (addr, srv) =
            warp::serve(routes).bind_with_graceful_shutdown(([127, 0, 0, 42], 0), async {
                rx.await.ok();
            });
        let h = task::spawn(srv);
        Self { addr, tx, h }
    }

    async fn done(self) {
        let _ = self.tx.send(());
        self.h.await.expect("unable to join spawned webserver");
    }
}

#[test(tokio::test)]
async fn validate() {}
