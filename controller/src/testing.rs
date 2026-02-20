//! Extras that only show up during tests.
#![allow(missing_docs)]
use std::{collections::BTreeMap, sync::Arc};

use assert_json_diff::assert_json_include;
use futures::future::TryFutureExt;
use http::{Request, Response, StatusCode};
use json_patch::merge;
use k8s_openapi::{
    DeepMerge,
    api::{batch::v1::Job, core::v1::ConfigMap, events::v1::Event},
    jiff::Timestamp,
};
use kube::{
    Resource, ResourceExt,
    client::{Body, Client},
    runtime::events::Recorder,
};
use serde_json::{Value, from_value, json};
use strum::{EnumIter, EnumString, IntoEnumIterator};
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
#[derive(Debug)]
pub enum ClairScenario {
    /// ...
    FinalizerCreation,
    /// ...
    Finalize,
    /// ...
    Ready,
    /// ...
    AdminPre(AdminPreScenario),
    /// ...
    Configuration(ConfigurationScenario),
}

impl ClairScenario {
    pub fn admin_pre() -> Vec<ClairScenario> {
        AdminPreScenario::iter().map(Self::AdminPre).collect()
    }

    pub fn object(&self) -> Clair {
        match self {
            Self::FinalizerCreation => from_value(json!({
                "version": Clair::api_version(&()),
                "kind": Clair::kind(&()),
                "metadata": {
                    "namespace": "default",
                    "name": "test",
                    "uid": "42",
                    "generation": 1,
                },
                "spec": {
                    "image": "example.com/clair:1.2.3",
                    "databases": {
                        "indexer": {
                            "name": "test",
                            "key": "database",
                        },
                        "matcher": {
                            "name": "test",
                            "key": "database",
                        },
                    },
                },
                "status": { },
            }))
            .expect("static JSON"),
            Self::Finalize => from_value(json!({
                "version": Clair::api_version(&()),
                "kind": Clair::kind(&()),
                "metadata": {
                    "namespace": "default",
                    "name": "test",
                    "uid": "42",
                    "finalizers": [ crate::clairs::CLAIR_FINALIZER ],
                    "generation": 1,
                },
                "spec": { },
                "status": { },
            }))
            .expect("static JSON"),
            Self::Ready => from_value(json!({
                "version": Clair::api_version(&()),
                "kind": Clair::kind(&()),
                "metadata": {
                    "namespace": "default",
                    "name": "test",
                    "uid": "42",
                    "finalizers": [ crate::clairs::CLAIR_FINALIZER ],
                    "generation": 1,
                },
                "spec": {
                    "image": "example.com/clair:1.2.3",
                    "databases": {
                        "indexer": {
                            "name": "test",
                            "key": "database",
                        },
                        "matcher": {
                            "name": "test",
                            "key": "database",
                        },
                    },
                },
                "status": {
                    "config": {
                        "root": {
                            "name": "test",
                            "key": "config.json",
                        },
                    },
                },
            }))
            .expect("static JSON"),
            Self::Configuration(scenario) => {
                use ConfigurationScenario::*;
                let c: Clair = from_value(json!({
                    "version": Clair::api_version(&()),
                    "kind": Clair::kind(&()),
                    "metadata": {
                        "namespace": "default",
                        "name": "test",
                        "uid": "42",
                        "finalizers": [ crate::clairs::CLAIR_FINALIZER ],
                        "generation": 2,
                    },
                    "spec": {
                        "image": "example.com/clair:1.2.3",
                        "databases": {
                            "indexer": {
                                "name": "test",
                                "key": "database",
                            },
                            "matcher": {
                                "name": "test",
                                "key": "database",
                            },
                        },
                    },
                    "status": { },
                }))
                .expect("static JSON");
                match scenario {
                    Create => (),
                };
                c
            }
            Self::AdminPre(scenario) => {
                use AdminPreScenario::*;
                let mut c: Clair = from_value(json!({
                    "version": Clair::api_version(&()),
                    "kind": Clair::kind(&()),
                    "metadata": {
                        "namespace": "default",
                        "name": "test",
                        "uid": "42",
                        "finalizers": [ crate::clairs::CLAIR_FINALIZER ],
                        "generation": 2,
                    },
                    "spec": {
                        "image": "example.com/clair:1.2.3",
                        "databases": {
                            "indexer": {
                                "name": "test",
                                "key": "database",
                            },
                            "matcher": {
                                "name": "test",
                                "key": "database",
                            },
                        },
                    },
                    "status": {
                        "config": {
                            "root": {
                                "name": "test",
                                "key": "config.json",
                            },
                        },
                    },
                }))
                .expect("static JSON");
                match scenario {
                    New => (),
                    Unversioned => {
                        c.spec.image = Some("example.com/clair:noversion".into());
                    }
                    SpecChanged => {
                        let status = c.status.as_mut().expect("status exists");
                        status.image = Some("example.com/clair:1.0.0".into());
                        status.conditions = vec![Condition {
                            last_transition_time: meta::v1::Time(Timestamp::now()),
                            message: "".into(),
                            observed_generation: 1.into(),
                            reason: "Testing".into(),
                            status: "True".into(),
                            type_: ConditionType::AdminPreJobDone.to_string(),
                        }]
                        .into();
                    }
                    SpecUnchangedCheck | SpecUnchangedDone => {
                        let status = c.status.as_mut().expect("status exists");
                        status.image = Some("example.com/clair:1.0.0".into());
                        status.conditions = vec![Condition {
                            last_transition_time: meta::v1::Time(Timestamp::now()),
                            message: "".into(),
                            observed_generation: 2.into(),
                            reason: "ImageUpdated".into(),
                            status: "False".into(),
                            type_: ConditionType::AdminPreJobDone.to_string(),
                        }]
                        .into();
                    }
                };
                c
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, EnumIter, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum ConfigurationScenario {
    Create,
}

#[derive(Clone, Copy, Debug, PartialEq, EnumIter, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum AdminPreScenario {
    Unversioned,
    New,
    SpecChanged,
    SpecUnchangedCheck,
    SpecUnchangedDone,
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
    pub fn run(mut self, scenario: ClairScenario) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            use ClairScenario::*;
            // moving self => one scenario per test
            match scenario {
                FinalizerCreation => {
                    let c = scenario.object();
                    self.handle_finalizer_creation(c).await
                }
                Finalize => {
                    let c = scenario.object();
                    self.handle_finalize(c).await
                }
                Ready => {
                    let c = scenario.object();
                    self.handle_ready(c).await
                }
                Configuration(which) => {
                    use ConfigurationScenario::*;
                    let c = scenario.object();
                    match which {
                        Create => self.configuration_create(c).await,
                    }
                }
                AdminPre(which) => {
                    use AdminPreScenario::*;
                    let c = scenario.object();
                    let meta = c.meta();
                    let ns = meta.namespace.as_ref().unwrap();
                    let name = meta.name.as_ref().unwrap();
                    self.state.insert(
                        format!("/v1/namespaces/{ns}/configmaps/{name}"),
                        json!({
                            "version": "v1",
                            "kind": "ConfigMap",
                            "metadata": {
                                "name": name,
                                "namespace": ns,
                            },
                            "data": {
                                "config.json": "{}",
                            },
                        }),
                    );
                    match which {
                        Unversioned => Ok(self), // do nothing, this should make no requests.
                        New => self.admin_pre_new(c).await,
                        SpecChanged => self.admin_pre_spec_changed(c).await,
                        SpecUnchangedCheck => {
                            self.state.insert(
                                format!("/batch/v1/namespaces/{ns}/jobs/{name}-admin-pre-1.2.3"),
                                json!({
                                    "version": "batch/v1",
                                    "kind": "Job",
                                    "metadata": {
                                        "name": name,
                                        "namespace": ns,
                                    },
                                    "spec": { },
                                    "status": {
                                        "active": 1,
                                    },
                                }),
                            );
                            self.admin_pre_spec_unchanged_check(c).await
                        }
                        SpecUnchangedDone => {
                            self.state.insert(
                                format!("/batch/v1/namespaces/{ns}/jobs/{name}-admin-pre-1.2.3"),
                                json!({
                                    "version": "batch/v1",
                                    "kind": "Job",
                                    "metadata": {
                                        "name": name,
                                        "namespace": ns,
                                    },
                                    "spec": { },
                                    "status": {
                                        "active": 0,
                                        "succeeded": 1,
                                    },
                                }),
                            );
                            self.admin_pre_spec_unchanged_done(c).await
                        }
                    }
                }
            }
            .expect("scenario completed without errors");
        })
    }

    async fn handle_finalizer_creation(mut self, mut c: Clair) -> Result<Self> {
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

        c.metadata.finalizers = vec![clairs::CLAIR_FINALIZER.into()].into();
        let response = serde_json::to_vec(&c).unwrap(); // respond as the apiserver would have
        send.send_response(Response::builder().body(Body::from(response)).unwrap());

        Ok(self)
    }

    async fn event(mut self, c: Clair, ev: Event) -> Result<(Self, Clair)> {
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

        Ok((self, c))
    }

    /// Handles a GET for a resource of type `R`.
    async fn check_resource<R, S>(mut self, c: Clair, name: Option<S>) -> Result<(Self, Clair)>
    where
        R: Resource<DynamicType = ()>,
        S: AsRef<str>,
    {
        let name = name
            .map(|v| v.as_ref().to_string())
            .unwrap_or_else(|| c.name_any());
        let (request, send) = self.next_request().await.expect("service not called");
        let uri = request.uri().to_string();
        eprintln!("{}\t{}", request.method(), &uri);
        assert_eq!(request.method(), http::Method::GET, "unexpected method");
        // Need these asserts because core types use `/api/` and everything else uses `/apis/`.
        assert!(uri.starts_with("/api"), "unexpected path: {uri}");
        let key = format!(
            "/{}/namespaces/default/{}/{}",
            R::api_version(&()),
            R::plural(&()),
            &name,
        );
        assert!(uri.ends_with(&key), "unexpected path: {key}");

        let response = if let Some(v) = self.state.get(&key) {
            eprintln!("found: {key}");
            Response::builder()
                .body(Body::from(serde_json::to_vec(v).unwrap()))
                .unwrap()
        } else {
            not_found::<R, _>(name)
        };
        send.send_response(response);

        Ok((self, c))
    }

    async fn create_resource<R>(mut self, c: Clair) -> Result<(Self, Clair)>
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

        Ok((self, c))
    }

    /// Handles a PATCH for a resource of type `R`.
    async fn update_resource<R, S>(mut self, c: Clair, name: Option<S>) -> Result<(Self, Clair)>
    where
        R: Resource<DynamicType = ()>,
        S: AsRef<str>,
    {
        let name = name
            .map(|v| v.as_ref().to_string())
            .unwrap_or_else(|| c.name_any());
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
            .and_modify(|v| merge(v, &obj))
            .or_insert_with(|| obj);
        let response = Response::builder()
            .body(Body::from(serde_json::to_vec(obj).unwrap()))
            .unwrap();
        send.send_response(response);

        Ok((self, c))
    }

    async fn status_patch(mut self, mut c: Clair) -> Result<(Self, Clair)> {
        let (request, send) = self.next_request().await.expect("service not called");
        eprintln!("{}\t{}", request.method(), request.uri());
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
        let response = serde_json::to_vec(&c).unwrap();
        // pass through document "patch accepted"
        send.send_response(Response::builder().body(Body::from(response)).unwrap());

        Ok((self, c))
    }

    async fn configuration_create(self, c: Clair) -> Result<Self> {
        let (srv, _c) = self
            .check_resource::<ConfigMap, &str>(c, None)
            .and_then(|(srv, c)| srv.create_resource::<ConfigMap>(c))
            .and_then(|(srv, c)| srv.status_patch(c))
            .and_then(|(srv, c)| {
                srv.event(
                    c,
                    Event {
                        type_: Some("Normal".into()),
                        reason: Some("Clair requires ConfigMap \"test\"".into()),
                        action: Some("CreatedConfigMap".into()),
                        ..Default::default()
                    },
                )
            })
            .and_then(|(srv, c)| srv.status_patch(c))
            .await?;

        Ok(srv)
    }

    async fn admin_pre_new(self, c: Clair) -> Result<Self> {
        let (srv, c) = self.status_patch(c).await?;
        let status = c.status.as_ref().expect("have status");
        let conditions = status.conditions.as_ref().expect("have conditions");
        assert_eq!(conditions.len(), 1, "unexpected number of conditions");
        Ok(srv)
    }

    async fn admin_pre_spec_changed(self, c: Clair) -> Result<Self> {
        use crate::clairs::reason::AdminPre as Reason;

        let (srv, c) = self
            .create_resource::<Job>(c)
            .and_then(|(srv, c)| srv.status_patch(c))
            .await?;

        let status = c.status.as_ref().expect("status exists");
        let conditions = status.conditions.as_ref().expect("conditions exist");
        assert_eq!(conditions.len(), 1, "unexpected number of conditions");

        let cnd = &conditions[0];
        println!("{cnd:?}");
        assert_eq!(
            ConditionType::AdminPreJobDone,
            cnd.type_,
            "unexpected Condition type"
        );
        assert_eq!(cnd.status, "False", "unexpected Condition status");
        assert_eq!(
            Reason::ImageUpdated,
            cnd.reason,
            "unexpected Condition reason"
        );

        Ok(srv)
    }

    async fn admin_pre_spec_unchanged_check(self, c: Clair) -> Result<Self> {
        use crate::clairs::reason::AdminPre as Reason;

        let (srv, c) = self
            .check_resource::<Job, _>(c, Some("test-admin-pre-1.2.3"))
            .and_then(|(srv, c)| srv.status_patch(c))
            .await?;

        let status = c.status.as_ref().expect("status exists");
        let conditions = status.conditions.as_ref().expect("conditions exist");
        assert_eq!(conditions.len(), 1, "unexpected number of conditions");

        let cnd = &conditions[0];
        println!("{cnd:?}");
        assert_eq!(
            ConditionType::AdminPreJobDone,
            cnd.type_,
            "unexpected Condition type"
        );
        assert_eq!(cnd.status, "False", "unexpected Condition status");
        assert_eq!(
            Reason::JobNotComplete,
            cnd.reason,
            "unexpected Condition reason"
        );

        Ok(srv)
    }

    async fn admin_pre_spec_unchanged_done(self, c: Clair) -> Result<Self> {
        use crate::clairs::reason::AdminPre as Reason;

        let (srv, c) = self
            .check_resource::<Job, _>(c, Some("test-admin-pre-1.2.3"))
            .and_then(|(srv, c)| srv.status_patch(c))
            .await?;

        let status = c.status.as_ref().expect("status exists");
        let conditions = status.conditions.as_ref().expect("conditions exist");
        assert_eq!(conditions.len(), 1, "unexpected number of conditions");

        let cnd = &conditions[0];
        println!("{cnd:?}");
        assert_eq!(
            ConditionType::AdminPreJobDone,
            cnd.type_,
            "unexpected Condition type"
        );
        assert_eq!(cnd.status, "True", "unexpected Condition status");
        assert_eq!(
            Reason::JobSucceeded,
            cnd.reason,
            "unexpected Condition reason"
        );

        Ok(srv)
    }

    async fn handle_ready(mut self, c: Clair) -> Result<Self> {
        self.state.insert(
            "/v1/namespaces/default/configmaps/test".into(),
            json!({
                "version": "v1",
                "kind": "ConfigMap",
                "metadata": {
                    "name": "test",
                    "namespace": "default",
                },
                "data": {
                    "config.json": "{}",
                },
            }),
        );

        // ConfigMap
        let (srv, c) = self
            .check_resource::<ConfigMap, &str>(c, None)
            .and_then(|(srv, c)| srv.update_resource::<ConfigMap, &str>(c, None))
            .and_then(|(srv, c)| srv.status_patch(c))
            .await?;
        // AdminPre
        let (srv, c) = srv.status_patch(c).await?;
        // Image promotion
        let (srv, c) = srv.status_patch(c).await?;
        // Indexer
        let (srv, c) = srv
            .check_resource::<Indexer, &str>(c, None)
            .and_then(|(srv, c)| srv.create_resource::<Indexer>(c))
            .and_then(|(srv, c)| srv.status_patch(c))
            .and_then(|(srv, c)| {
                srv.event(
                    c,
                    Event {
                        type_: Some("Normal".into()),
                        action: Some("CreatedIndexer".into()),
                        reason: Some("Clair requires Indexer \"test\"".into()),
                        ..Default::default()
                    },
                )
            })
            .await?;
        // Matcher
        let (srv, _c) = srv
            .check_resource::<Matcher, &str>(c, None)
            .and_then(|(srv, c)| srv.create_resource::<Matcher>(c))
            .and_then(|(srv, c)| srv.status_patch(c))
            .and_then(|(srv, c)| {
                srv.event(
                    c,
                    Event {
                        type_: Some("Normal".into()),
                        action: Some("CreatedMatcher".into()),
                        reason: Some("Clair requires Matcher \"test\"".into()),
                        ..Default::default()
                    },
                )
            })
            .await?;

        Ok(srv)
    }

    async fn handle_finalize(self, c: Clair) -> Result<Self> {
        let (srv, _c) = self
            .event(
                c,
                Event {
                    type_: Some("Warning".into()),
                    reason: Some("MissingRequiredField".to_string()),
                    action: Some("Reconcile".into()),
                    ..Default::default()
                },
            )
            .and_then(|(srv, mut c)| {
                c.spec.image = Some("example.com/clair:test".into());
                srv.event(
                    c,
                    Event {
                        type_: Some("Warning".into()),
                        reason: Some("MissingRequiredField".to_string()),
                        action: Some("Reconcile".into()),
                        ..Default::default()
                    },
                )
            })
            .await?;

        Ok(srv)
    }
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
