//! Templates holds the templating logic for controllers.

use std::collections::HashMap;

use k8s_openapi::api::core::v1::Service;
use serde;
use serde_json::json;

/// DEFAULT_CONFIG ...
pub static DEFAULT_CONFIG: &str = include_str!("default_config.json");

/// Render_dropin ...
pub fn render_dropin<O>(srv: &Service) -> Option<String>
where
    O: kube::Resource<DynamicType = ()>,
{
    use kube::ResourceExt;

    let name = srv.name_unchecked();
    let ns = srv.namespace().unwrap();
    let addr = format!("{name}.{ns}.svc.cluster.local");

    let v = match O::kind(&()).as_ref() {
        "Indexer" => json!([
          { "op": "add", "path": "/matcher/indexer_addr",  "value": addr },
          { "op": "add", "path": "/notifier/indexer_addr", "value": addr },
        ]),
        "Matcher" => json!([
          { "op": "add", "path": "/indexer/matcher_addr",  "value": addr },
          { "op": "add", "path": "/notifier/matcher_addr", "value": addr },
        ]),
        _ => return None,
    };

    serde_json::to_string(&v).ok()
}

/// Render ...
pub fn render<O, K>(owner: &O) -> K
where
    O: kube::Resource<DynamicType = ()>,
    K: kube::Resource<DynamicType = ()> + serde::de::DeserializeOwned,
{
    use kube::ResourceExt;

    let kind = O::kind(&()).to_ascii_lowercase();
    let name = format!("{}-{kind}", owner.name_any());
    let mut labels = HashMap::from([
        ("app.kubernetes.io/name", "clair"),
        ("app.kubernetes.io/managed-by", "clair-operator"),
        ("app.kubernetes.io/component", &kind),
    ]);
    let v = match K::kind(&()).as_ref() {
        "CronJob" => {
            labels.remove("app.kubernetes.io/component");
            let metadata = json!( {
              "name": name,
              "labels": labels,
            });
            let container = json!({
              "name": "clair",
              "image": crate::DEFAULT_IMAGE.as_str(),
              "ports": [
                { "name": "introspection", "containerPort": 8089 }
              ],
              "env": [
                {
                  "name": "GOMAXPROCS",
                  "valueFrom": {
                    "resourceFieldRef": {
                      "containerName": "clair",
                      "resource": "requests.cpu"
                    }
                  }
                }
              ],
              "volumeMounts": [],
              "workingDir": "/run/clair",
              "securityContext": {
                "allowPrivilegeEscalation": false
              },
              "resources": {
                "requests": { "cpu": "1" }
              },
              "startupProbe": {
                "tcpSocket": {
                  "port": "http"
                },
                "initialDelaySeconds": 5,
                "periodSeconds": 1
              },
              "livenessProbe": {
                "httpGet": { "path": "/healthz", "port": "introspection" },
                "initialDelaySeconds": 15,
                "periodSeconds": 20
              },
              "readinessProbe": {
                "httpGet": { "path": "/readyz", "port": "introspection" },
                "initialDelaySeconds": 5,
                "periodSeconds": 10
              }
            });
            json!({
              "apiVersion": "batch/v1",
              "kind": "CronJob",
              "metadata": metadata,
              "spec": {
                "concurrencyPolicy": "Forbid",
                "startingDeadlineSeconds": 10,
                "timeZone": "Etc/UTC",
                "schedule": "0 */8 * * *",
                "jobTemplate": {
                  "metadata": metadata,
                  "spec": {
                    "activeDeadlineSeconds": 3600,
                    "completionMode": "NonIndexed",
                    "completions": 1,
                    "parallelism": 1,
                    "template": {
                      "metadata": metadata,
                      "spec": {
                        "revisionHistoryLimit": 3,
                        "terminationGracePeriodSeconds": 10,
                        "securityContext": {
                          "runAsUser": 65532
                        },
                        "shareProcessNamespace": true,
                        "volumes": [],
                        "containers": [ container ]
                      }
                    }
                  }
                }
              }
            })
        }

        "Deployment" => {
            let container = json!({
                "name": "clair",
                "image": crate::DEFAULT_IMAGE.as_str(),
                "ports": [
                    { "name": "api", "containerPort": 6060 },
                    { "name": "introspection", "containerPort": 8089 },
                ],
                "env": [
                    {
                        "name": "GOMAXPROCS",
                        "valueFrom": {
                            "resourceFieldRef": {
                                "containerName": "clair",
                                "resource": "requests.cpu"
                            },
                        },
                    },
                    { "name": "CLAIR_MODE", "value": kind },
                ],
                "volumeMounts": [],
                "workingDir": "/run/clair",
                "securityContext": { "allowPrivilegeEscalation": false },
                "resources": {
                    "requests": { "cpu": "1" }
                },
                "startupProbe": {
                    "tcpSocket": { "port": "api" },
                    "initialDelaySeconds": 5,
                    "periodSeconds": 1
                },
                "livenessProbe": {
                    "httpGet": {
                        "path": "/healthz",
                        "port": "introspection"
                    },
                    "initialDelaySeconds": 15,
                    "periodSeconds": 20
                },
                "readinessProbe": {
                    "httpGet": {
                        "path": "/readyz",
                        "port": "introspection"
                    },
                    "initialDelaySeconds": 5,
                    "periodSeconds": 10
                }
            });

            json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {
                    "name": name,
                    "labels": &labels,
                },
                "spec": {
                    "selector": { "matchLabels": &labels },
                    "revisionHistoryLimit": 3,
                    "replicas": 1,
                    "template": {
                        "metadata": { "labels": &labels },
                        "spec": {
                            "terminationGracePeriodSeconds": 10,
                            "securityContext": { "runAsUser": 65532 },
                            "shareProcessNamespace": true,
                            "volumes": [],
                            "containers": [ container ]
                        }
                    }
                }
            })
        }

        "Service" => {
            json!({
              "apiVersion": "v1",
              "kind": "Service",
              "metadata": {
                "name": name,
                "labels": labels,
              },
              "spec": {
                "ports": [
                  {
                    "name": "api",
                    "port": 80,
                    "targetPort": "api"
                  }
                ],
                "selector": labels,
              }
            })
        }

        "HorizontalPodAutoscaler" => {
            json!({
              "apiVersion": "autoscaling/v2",
              "kind": "HorizontalPodAutoscaler",
              "metadata": {
                "name": name,
                "labels": labels,
              },
              "spec": {
                "minReplicas": 1,
                "maxReplicas": 10,
                "scaleTargetRef": {
                  "apiVersion": "apps/v1",
                  "kind": "Deployment",
                  "name": name,
                },
                "metrics": [
                  {
                    "type": "Resource",
                    "resource": {
                      "name": "cpu",
                      "target": {
                        "type": "Utilization",
                        "averageUtilization": 80
                      }
                    }
                  }
                ]
              }
            })
        }

        "Ingress" => {
            json!({})
        }

        _ => panic!("programmer error: unexpected type: {}", K::kind(&())),
    };
    let mut k: K =
        serde_json::from_value(v).expect("programmer error: unable to deserialize template");
    k.meta_mut().owner_references = owner.controller_owner_ref(&()).map(|r| vec![r]);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_json_diff::assert_json_eq;
    use serde_json::{from_str, to_value, Value};

    #[cfg(test)]
    mod indexer {
        use super::*;

        use api::v1alpha1::Indexer;

        #[test]
        fn deployment() {
            use k8s_openapi::api::apps::v1::Deployment;

            let indexer = Indexer::new("test", Default::default());
            let got: Deployment = render(&indexer);
            let got = to_value(got).unwrap();
            let want: Value = from_str(include_str!("_fixture/indexer/deployment.json")).unwrap();

            assert_json_eq!(got, want);
        }

        #[test]
        fn service() {
            use k8s_openapi::api::core::v1::Service;

            let indexer = Indexer::new("test", Default::default());
            let got: Service = render(&indexer);
            let got = to_value(got).unwrap();
            let want: Value = from_str(include_str!("_fixture/indexer/service.json")).unwrap();

            assert_json_eq!(got, want);
        }

        #[test]
        fn horizontal_pod_autoscaler() {
            use k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;

            let indexer = Indexer::new("test", Default::default());
            let got: HorizontalPodAutoscaler = render(&indexer);
            let got = to_value(got).unwrap();
            let want: Value = from_str(include_str!(
                "_fixture/indexer/horizontalpodautoscaler.json"
            ))
            .unwrap();

            assert_json_eq!(got, want);
        }

        #[test]
        fn cron_job() {
            use k8s_openapi::api::batch::v1::CronJob;

            let indexer = Indexer::new("test", Default::default());
            let got: CronJob = render(&indexer);
            let got = to_value(got).unwrap();
            let want: Value = from_str(include_str!("_fixture/indexer/cronjob.json")).unwrap();

            assert_json_eq!(got, want);
        }

        #[test]
        fn dropin() {
            use k8s_openapi::api::core::v1::Service;

            let mut srv: Service = from_str(include_str!("_fixture/indexer/service.json")).unwrap();
            srv.metadata.namespace = Some("test".into());
            let got = render_dropin::<Indexer>(&srv).unwrap();
            let got: Value = from_str(&got).unwrap();
            let want = json!([
              { "op": "add", "path": "/matcher/indexer_addr",  "value": "test-indexer.test.svc.cluster.local" },
              { "op": "add", "path": "/notifier/indexer_addr", "value": "test-indexer.test.svc.cluster.local" },
            ]);

            assert_json_eq!(got, want);
        }
    }
}
