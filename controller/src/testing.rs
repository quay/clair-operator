//! Extras that only show up during tests.
#![allow(missing_docs)]
use std::{collections::BTreeMap, sync::Arc};

use assert_json_diff::assert_json_include;
use http::{Request, Response, StatusCode};
use k8s_openapi::{
    DeepMerge,
    api::core::v1::ConfigMap,
    api::events::v1::Event,
};
use kube::{
    Resource, ResourceExt,
    client::{Body, Client},
    runtime::events::Recorder,
};
use serde_json::{Value, json};
use tower_test::mock::SendResponse;

use super::*;
use api::v1alpha1::{Clair, ClairStatus, Indexer, Matcher};

pub use test_log::test;

impl Context {
    pub fn clair_tests() -> (Arc<Self>, ClairServerVerifier) {
        let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let mock_client = Client::new(mock_service, "default");
        let mock_recorder = Recorder::new(mock_client.clone(), REPORTER.clone());
        let ctx = Self {
            client: mock_client,
            recorder: mock_recorder,
            //metrics: Arc::default(),
        };
        (Arc::new(ctx), ClairServerVerifier::new(handle))
    }
}

pub mod clair {
    use crate::clairs::*;
    use api::v1alpha1::{Clair, ClairSpec, ClairStatus, Databases, SecretKeySelector};
    use kube::{Resource, ResourceExt};

    /// Return an empty Clair instance.
    pub fn test(spec: Option<ClairSpec>) -> Clair {
        let mut c = Clair::new("test", spec.unwrap_or_default());
        c.meta_mut().namespace = Some("default".into());

        c
    }

    pub fn finalized(mut c: Clair) -> Clair {
        c.finalizers_mut().push(CLAIR_FINALIZER.into());
        c
    }

    pub fn ready() -> Clair {
        let spec = ClairSpec {
            image: Some(crate::DEFAULT_IMAGE.to_string()),
            databases: Some(Databases {
                indexer: SecretKeySelector {
                    name: "test".into(),
                    key: "database".into(),
                },
                matcher: SecretKeySelector {
                    name: "test".into(),
                    key: "database".into(),
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut c = finalized(test(spec.into()));
        c.metadata.uid = "42".to_string().into();

        c
    }

    pub fn with_status(mut c: Clair, status: ClairStatus) -> Clair {
        c.status = Some(status);
        c
    }
}

pub async fn timeout_after_1s(handle: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(std::time::Duration::from_secs(1), handle)
        .await
        .expect("timeout on mock apiserver")
        .expect("scenario succeeded")
}

// We wrap tower_test::mock::Handle
type ApiServerHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;

pub struct ClairServerVerifier {
    handle: ApiServerHandle,
    state: BTreeMap<String, Value>,
}

/// Scenarios we want to test for
pub enum ClairScenario {
    /// ...
    FinalizerCreation(Clair),
    /// ...
    Event(Clair, Event),
    ///// We expect exactly one `patch_status` call to the `Clair` resource
    //StatusPatch(Clair),
    /// ...
    Ready(Clair),
}

impl ClairServerVerifier {
    fn new(handle: ApiServerHandle) -> Self {
        Self {
            handle,
            state: BTreeMap::new(),
        }
    }

    #[inline]
    fn next_request(
        &mut self,
    ) -> impl Future<Output = Option<(Request<Body>, SendResponse<Response<Body>>)>> {
        self.handle.next_request()
    }

    /// Tests only get to run specific scenarios that has matching handlers
    ///
    /// This setup makes it easy to handle multiple requests by chaining handlers together.
    ///
    /// NB: If the controller is making more calls than we are handling in the scenario,
    /// you then typically see a `KubeError(Service(Closed(())))` from the reconciler.
    ///
    /// You should await the `JoinHandle` (with a timeout) from this function to ensure that the
    /// scenario runs to completion (i.e. all expected calls were responded to),
    /// using the timeout to catch missing api calls to Kubernetes.
    pub fn run(self, scenario: ClairScenario) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            use ClairScenario::*;
            // moving self => one scenario per test
            match scenario {
                FinalizerCreation(c) => self.handle_finalizer_creation(c).await,
                Event(c, ev) => {
                    self.handle_event(c.clone(), ev.clone())
                        .await
                        .unwrap()
                        .handle_event(c, ev)
                        .await
                }
                Ready(c) => self.handle_ready(c).await,
                //Scenario::EventPublishThenStatusPatch(reason, doc) => {
                //    self.handle_event_create(reason)
                //        .await
                //        .unwrap()
                //        .handle_status_patch(doc)
                //        .await
                //}
                //Scenario::RadioSilence => Ok(self),
                //Scenario::Cleanup(reason, doc) => {
                //    self.handle_event_create(reason)
                //        .await
                //        .unwrap()
                //        .handle_finalizer_removal(doc)
                //        .await
                //}
            }
            .expect("scenario completed without errors");
        })
    }

    async fn handle_finalizer_creation(mut self, c: Clair) -> Result<Self> {
        let (request, send) = self.next_request().await.expect("service not called");
        // We expect a json patch to the specified document adding our finalizer
        assert_eq!(request.method(), http::Method::PATCH);
        assert_eq!(
            request.uri().to_string(),
            format!(
                "/apis/clairproject.org/v1alpha1/namespaces/default/clairs/{}?",
                c.name_any()
            )
        );
        let expected_patch = serde_json::json!([
            { "op": "test", "path": "/metadata/finalizers", "value": null },
            { "op": "add", "path": "/metadata/finalizers", "value": vec![clairs::CLAIR_FINALIZER] }
        ]);
        let req_body = request.into_body().collect_bytes().await.unwrap();
        let runtime_patch: serde_json::Value =
            serde_json::from_slice(&req_body).expect("valid document from runtime");
        assert_json_include!(actual: runtime_patch, expected: expected_patch);

        let c = clair::finalized(c);
        let response = serde_json::to_vec(&c).unwrap(); // respond as the apiserver would have
        send.send_response(Response::builder().body(Body::from(response)).unwrap());

        Ok(self)
    }

    /// Tests that the next request is an Event matching "ev".
    ///
    /// Echoes back the sent event.
    async fn handle_event(mut self, c: Clair, ev: Event) -> Result<Self> {
        let (request, send) = self.next_request().await.expect("service not called");
        let uri = request.uri().to_string();
        eprintln!("{}\t{}", request.method(), &uri);
        assert!(
            matches!(*request.method(), http::Method::POST | http::Method::PATCH),
            "unexpected method"
        );
        assert!(
            uri.starts_with("/apis/events.k8s.io/v1/namespaces/default/events"),
            "unexpected path"
        );

        let req_body = request.into_body().collect_bytes().await.unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&req_body).expect("event object is json");
        let event: Event = serde_json::from_value(json).expect("valid event");

        if let Some(ref note) = event.note {
            if note.contains("$.spec.databases") {
                assert!(c.spec.databases.is_none(), "unexpected event");
            }
            if note.contains("$.spec.image") {
                assert!(c.spec.image.is_none(), "unexpected event");
            }
        }
        assert_eq!(event.type_, ev.type_, "unexpected \"type\"");
        assert_eq!(event.reason, ev.reason, "unexpected \"reason\"");
        assert_eq!(event.action, ev.action, "unexpected \"action\"");

        let response = serde_json::to_vec(&event).unwrap();
        send.send_response(Response::builder().body(Body::from(response)).unwrap());

        Ok(self)
    }

    async fn handle_ready(mut self, mut c: Clair) -> Result<Self> {
        self = // Initial ConfigMap check + creation:
            self
            .handle_check_resource::<ConfigMap>(&c)
            .await?
            .handle_create_resource::<ConfigMap>(&c)
            .await?
            .handle_status_patch(&mut c)
            .await?
            .handle_event(
                c.clone(),
                Event {
                    type_: Some("Normal".into()),
                    action: Some("CreatedConfigMap".into()),
                    reason: Some("Clair requires ConfigMap \"test\"".into()),
                    ..Default::default()
                },
            )
            .await?
            // Update config source:
            .handle_status_patch(&mut c)
            .await?
            // requeue happens 
            // Subsequent ConfigMap check + reconcile:
            .handle_check_resource::<ConfigMap>(&c)
            .await?
            .handle_update_resource::<ConfigMap, _>(&c, "test")
            .await?
            .handle_status_patch(&mut c)
            .await?
            // Indexer check + creation:
            .handle_check_resource::<Indexer>(&c)
            .await?
            .handle_create_resource::<Indexer>(&c)
            .await?
            .handle_status_patch(&mut c)
            .await?
            .handle_event(
                c.clone(),
                Event {
                    type_: Some("Normal".into()),
                    action: Some("CreatedIndexer".into()),
                    reason: Some("Clair requires Indexer \"test\"".into()),
                    ..Default::default()
                },
            )
            .await?
            // Matcher check + creation:
            .handle_check_resource::<Matcher>(&c)
            .await?
            .handle_create_resource::<Matcher>(&c)
            .await?
            .handle_status_patch(&mut c)
            .await?
            .handle_event(
                c.clone(),
                Event {
                    type_: Some("Normal".into()),
                    action: Some("CreatedMatcher".into()),
                    reason: Some("Clair requires Matcher \"test\"".into()),
                    ..Default::default()
                },
            )
            .await?;

        Ok(self)
    }

    /// Handles a GET for a resource of type `R`.
    async fn handle_check_resource<R: Resource<DynamicType = ()>>(
        mut self,
        c: &Clair,
    ) -> Result<Self> {
        let name = c.name_any();
        let (request, send) = self.next_request().await.expect("service not called");
        let uri = request.uri().to_string();
        eprintln!("{}\t{}", request.method(), &uri);
        assert_eq!(request.method(), http::Method::GET, "unexpected method");
        // Need these asserts because core types use `/api/` and everything else uses `/apis/`.
        assert!(uri.starts_with("/api"), "unexpected path");
        let key = format!(
            "/{}/namespaces/default/{}/{}",
            R::api_version(&()),
            R::plural(&()),
            &name,
        );
        assert!(uri.ends_with(&key), "unexpected path");

        let response = if let Some(v) = self.state.get(&key) {
            Response::builder()
                .body(Body::from(serde_json::to_vec(v).unwrap()))
                .unwrap()
        } else {
            not_found::<R, _>(name)
        };
        send.send_response(response);

        Ok(self)
    }

    /// Handles a POST for a resource of type `R`.
    async fn handle_create_resource<R>(mut self, _c: &Clair) -> Result<Self>
    where
        R: Resource<DynamicType = ()>,
    {
        let (request, send) = self.next_request().await.expect("service not called");
        let uri = request.uri().to_string();
        eprintln!("{}\t{}", request.method(), &uri);
        assert_eq!(request.method(), http::Method::POST, "unexpected method");
        // Need these asserts because core types use `/api/` and everything else uses `/apis/`.
        assert!(uri.starts_with("/api"), "unexpected path");
        let pat = format!(
            "/{}/namespaces/default/{}?&fieldManager={}",
            R::api_version(&()),
            R::plural(&()),
            crate::CONTROLLER_NAME,
        );
        assert!(uri.ends_with(&pat), "unexpected path");

        let req_body = request.into_body().collect_bytes().await.unwrap();
        let obj: serde_json::Value = serde_json::from_slice(&req_body).expect("object is json");
        let name = obj
            .get("metadata")
            .expect("object has metadata")
            .get("name")
            .expect("metadata has name")
            .as_str()
            .expect("name is a string");

        let key = format!(
            "/{}/namespaces/default/{}/{}",
            R::api_version(&()),
            R::plural(&()),
            name,
        );

        assert!(!self.state.contains_key(&key), "double-create of {key}");
        self.state.insert(key, obj);
        send.send_response(Response::builder().body(Body::from(req_body)).unwrap());

        Ok(self)
    }

    /// Handles a PATCH for a resource of type `R`.
    async fn handle_update_resource<R, S>(mut self, _c: &Clair, name: S) -> Result<Self>
    where
        R: Resource<DynamicType = ()>,
        S: AsRef<str>,
    {
        let name = name.as_ref();
        let (request, send) = self.next_request().await.expect("service not called");
        let uri = request.uri().to_string();
        eprintln!("{}\t{}", request.method(), &uri);
        assert_eq!(request.method(), http::Method::PATCH, "unexpected method");
        // Need these asserts because core types use `/api/` and everything else uses `/apis/`.
        assert!(uri.starts_with("/api"), "unexpected path");
        let key = format!(
            "/{}/namespaces/default/{}/{}",
            R::api_version(&()),
            R::plural(&()),
            name,
        );
        let pat = format!(
            "{}?&fieldManager={}&fieldValidation=Strict",
            key,
            crate::CONTROLLER_NAME,
        );
        assert!(uri.ends_with(&pat), "unexpected path");

        let req_body = request.into_body().collect_bytes().await.unwrap();
        let obj: serde_json::Value = serde_json::from_slice(&req_body).expect("object is json");
        let objname = obj
            .get("metadata")
            .expect("object has metadata")
            .get("name")
            .expect("metadata has name")
            .as_str()
            .expect("name is a string");
        assert_eq!(name, objname, "patch to wrong resource?");

        let obj = self
            .state
            .entry(key)
            .and_modify(|v| merge(v, obj.clone()))
            .or_insert_with(|| obj);
        let response = Response::builder()
            .body(Body::from(serde_json::to_vec(obj).unwrap()))
            .unwrap();
        send.send_response(response);

        Ok(self)
    }

    async fn handle_status_patch(mut self, c: &mut Clair) -> Result<Self> {
        let (request, send) = self.next_request().await.expect("service not called");
        eprintln!("{}\t{}", request.method(), request.uri().to_string());
        assert_eq!(request.method(), http::Method::PATCH, "unexpected method");
        assert_eq!(
            request.uri().to_string(),
            format!(
                "/apis/{}/namespaces/default/{}/{}/status?&fieldManager={}&fieldValidation=Strict",
                Clair::api_version(&()),
                Clair::plural(&()),
                c.name_any(),
                crate::CONTROLLER_NAME,
            ),
            "unexpected path",
        );

        let req_body = request.into_body().collect_bytes().await.unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&req_body).expect("patch_status object is json");
        let status_json = json.get("status").expect("status object").clone();
        let status: ClairStatus = serde_json::from_value(status_json).expect("valid status");
        /*
        assert_eq!(
            status.hidden, c.spec.hide,
            "status.hidden iff doc.spec.hide"
        );
        */
        c.status.merge_from(status.into());
        let response = serde_json::to_vec(c).unwrap();
        // pass through document "patch accepted"
        send.send_response(Response::builder().body(Body::from(response)).unwrap());

        Ok(self)
    }
}

// Not-to-spec merge function cribbed from stackoverflow.
fn merge(a: &mut Value, b: Value) {
    if let Value::Object(a) = a {
        if let Value::Object(b) = b {
            for (k, v) in b {
                if v.is_null() {
                    a.remove(&k);
                } else {
                    merge(a.entry(k).or_insert(Value::Null), v);
                }
            }

            return;
        }
    }

    *a = b;
}

fn not_found<R: Resource<DynamicType = ()>, S: ToString>(name: S) -> Response<Body> {
    let err = json!({
        "code": 404,
        "status": "Failure",
        "reason": "NotFound",
        "details": {
            "group": R::group(&()),
            "kind": R::kind(&()),
            "name": name.to_string(),
        },
    });
    let response = serde_json::to_vec(&err).unwrap();
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from(response))
        .unwrap()
}
