#[allow(unused_imports)]
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

use kube::{CustomResourceExt, Resource};
use xshell::{Shell, cmd};

#[allow(unused_imports)]
use crate::{Result, WORKSPACE, check, olm::cluster_service_versions::*};
use api::v1alpha1::*;

macro_rules! write_crds {
    ($out_dir:ident,  $($kind:ty),+ $(,)?) =>{
        eprintln!("# writing to dir: {}", crate::rel($out_dir));
        $( write_crd::<$kind, _>($out_dir)?; )+
    }
}

pub fn command(sh: Shell, opts: ManifestsOpts) -> Result<()> {
    let out = opts.out_dir.join("crd");
    let out = out.as_path();
    std::fs::create_dir_all(out)?;
    write_crds!(out, Clair, Indexer, Matcher, Updater, Notifier);

    /*
    let out = out.as_path();
    std::fs::create_dir_all(out)?;
    write_csv(out)?;
    */
    let out = opts.out_dir.join("csv");
    write_csv(&sh, out)?;
    Ok(())
}

fn write_crd<K, P>(out_dir: P) -> Result<()>
where
    K: Resource<DynamicType = ()> + CustomResourceExt,
    P: AsRef<Path>,
{
    let doc = serde_json::to_value(K::crd())?;
    let out = out_dir.as_ref().join(format!("{}.yaml", K::crd_name()));
    let w = File::create(&out)?;
    serde_yaml::to_writer(&w, &doc)?;
    eprintln!("# wrote: {}", out.file_name().unwrap().to_string_lossy());
    Ok(())
}

// TODO(hank): Maybe just keep the kustomize setup?
#[allow(dead_code)]
fn write_csv<P>(sh: &Shell, out_dir: P) -> Result<()>
where
    P: AsRef<Path>,
{
    check::kustomize(sh)?;

    let dir = WORKSPACE.join("xtask");
    sh.change_dir(&dir);

    let out_dir = out_dir.as_ref();
    cmd!(
        sh,
        "kustomize build --output {out_dir}/clair.csv.yaml src/manifests/csv"
    )
    .run()?;

    /*
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::*;
    let mut defs = ClusterServiceVersionCustomresourcedefinitions {
        owned: vec![{
            let crd = Clair::crd();
            let plural = &crd.spec.names.plural;
            let group = &crd.spec.group;
            ClusterServiceVersionCustomresourcedefinitionsOwned {
                name: format!("{plural}.{group}").into(),
                version: crd.spec.versions.get(0).map(|v| v.name.clone()).unwrap(),
                kind: crd.spec.names.kind.clone(),
                description: "Clair system definition".to_string().into(),
                display_name: crd.spec.names.kind.clone().into(),

                resources: [
                    ("configmaps", "ConfigMap", "v1"),
                    ("secrets", "Secret", "v1"),
                    ("indexers", "Indexer", "clairproject.org/v1alpha1"),
                    ("matchers", "Matcher", "clairproject.org/v1alpha1"),
                    ("notifiers", "Notifier", "clairproject.org/v1alpha1"),
                    ("updaters", "Updater", "clairproject.org/v1alpha1"),
                ]
                .into_iter()
                .map(|(name, kind, version)| {
                    ClusterServiceVersionCustomresourcedefinitionsOwnedResources {
                        name: name.to_string(),
                        kind: kind.to_string(),
                        version: version.to_string(),
                    }
                })
                .collect::<Vec<_>>()
                .into(),

                spec_descriptors: vec![
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "databases.indexer.name".into(),
                        x_descriptors: ["urn:alm:descriptor:io.kubernetes:Secret"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "databases.indexer.key".into(),
                        x_descriptors: ["urn:alm:descriptor:com:tectonic.ui:text"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "databases.matcher.name".into(),
                        x_descriptors: ["urn:alm:descriptor:io.kubernetes:Secret"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "databases.matcher.key".into(),
                        x_descriptors: ["urn:alm:descriptor:com:tectonic.ui:text"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "databases.notifier".into(),
                        x_descriptors: [
                            "urn:alm:descriptor:com.tectonic.ui:fieldDependency:notifier:true",
                        ]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                        .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "databases.notifier.name".into(),
                        x_descriptors: ["urn:alm:descriptor:io.kubernetes:Secret"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "databases.notifier.key".into(),
                        x_descriptors: ["urn:alm:descriptor:com:tectonic.ui:text"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        display_name: "Notifier".to_string().into(),
                        description: "Enable the Notifier component.".to_string().into(),
                        path: "notifier".into(),
                        x_descriptors: ["urn:alm:descriptor:com.tectonic.ui:booleanSwitch"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        display_name: "Container Image".to_string().into(),
                        description: "The Clair container image to use for Deployments."
                            .to_string()
                            .into(),
                        path: "image".into(),
                        x_descriptors: [
                            "urn:alm:descriptor:com:tectonic.ui:text",
                            "urn:alm:descriptor:com.tectonic.ui:advanced",
                        ]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                        .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        display_name: "Dropins".to_string().into(),
                        description: "Additional configuration dropins.".to_string().into(),
                        path: "dropins".into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "dropins[0].configMapKeyRef.name".into(),
                        x_descriptors: ["urn:alm:descriptor:io.kubernetes:ConfigMap"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "dropins[0].configMapKeyRef.key".into(),
                        x_descriptors: ["urn:alm:descriptor:com:tectonic.ui:text"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "dropins[0].secretKeyRef.name".into(),
                        x_descriptors: ["urn:alm:descriptor:io.kubernetes:Secret"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                    ClusterServiceVersionCustomresourcedefinitionsOwnedSpecDescriptors {
                        path: "dropins[0].secretKeyRef.key".into(),
                        x_descriptors: ["urn:alm:descriptor:com:tectonic.ui:text"]
                            .into_iter()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .into(),
                        ..Default::default()
                    },
                ]
                .into(),

                ..Default::default()
            }
        }]
        .into(),

        required: [
            ("gateways.gateway.networking.k8s.io", "v1", "Gateway"),
            ("gateways.gateway.networking.k8s.io", "v1", "HTTPRoute"),
            ("gateways.gateway.networking.k8s.io", "v1", "GRPCRoute"),
        ]
        .into_iter()
        .map(
            |(name, version, kind)| ClusterServiceVersionCustomresourcedefinitionsRequired {
                name: name.to_string(),
                version: version.to_string(),
                kind: kind.to_string(),
                ..Default::default()
            },
        )
        .collect::<Vec<_>>()
        .into(),
    };
    defs.owned.get_or_insert_default().extend(
        [
            Indexer::crd(),
            Matcher::crd(),
            Notifier::crd(),
            //Updater::crd(),
        ]
        .map(|crd| {
            let plural = &crd.spec.names.plural;
            let group = &crd.spec.group;
            let kind = crd.spec.names.kind.clone();
            ClusterServiceVersionCustomresourcedefinitionsOwned {
                name: format!("{plural}.{group}").into(),
                version: crd.spec.versions.get(0).map(|v| v.name.clone()).unwrap(),
                kind: kind.clone(),
                description: format!("{kind} worker definition").into(),
                display_name: crd.spec.names.kind.clone().into(),

                resources: [
                    ("configmaps", "ConfigMap", "v1"),
                    ("secrets", "Secret", "v1"),
                    ("services", "Service", "v1"),
                    ("deployments", "Deployment", "apps/v1"),
                    (
                        "horizontalpodautoscalers",
                        "HorizontalPodAutoscaler",
                        "autoscaling/v2",
                    ),
                ]
                .into_iter()
                .map(|(name, kind, version)| {
                    ClusterServiceVersionCustomresourcedefinitionsOwnedResources {
                        name: name.to_string(),
                        kind: kind.to_string(),
                        version: version.to_string(),
                    }
                })
                .collect::<Vec<_>>()
                .into(),
                //spec_descriptors: vec![].into(),
                ..Default::default()
            }
        }),
    );

    let csv = ClusterServiceVersion {
        metadata: ObjectMeta {
            name: format!("clair.v{}", env!("CARGO_PKG_VERSION")).into(),
            labels: BTreeMap::from([
                ("operatorframework.io/arch.amd64".into(), "supported".into()),
                ("operatorframework.io/os.linux".into(), "supported".into()),
            ])
            .into(),
            annotations: BTreeMap::from([
                ("alm-examples".into(), "[]".into()),
                ("capabilities".into(), "Basic Install".into()),
            ])
            .into(),
            ..Default::default()
        },
        spec: ClusterServiceVersionSpec {
            version: env!("CARGO_PKG_VERSION").to_string().into(),
            maturity: "alpha".to_string().into(),
            min_kube_version: "1.28.0".to_string().into(),
            display_name: "Clair Operator".into(),
            description: "This is an operator for Clair.".to_string().into(),
            keywords: ["clair", "security"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
                .into(),
            links: vec![ClusterServiceVersionLinks {
                name: "Source Code".to_string().into(),
                url: "https://github.com/quay/clair-operator".to_string().into(),
            }]
            .into(),
            maintainers: vec![ClusterServiceVersionMaintainers {
                name: "The Clair Authors".to_string().into(),
                email: "clair-dev@googlegroups.com".to_string().into(),
            }]
            .into(),
            provider: ClusterServiceVersionProvider {
                name: "The Clair Authors".to_string().into(),
                url: "https://clairproject.org/".to_string().into(),
            }
            .into(),
            install: ClusterServiceVersionInstall {
                strategy: "deployment".into(),
                spec: ClusterServiceVersionInstallSpec {
                    cluster_permissions: None,
                    permissions: vec![ClusterServiceVersionInstallSpecPermissions {
                        service_account_name: "clair-operator".into(),
                        rules: vec![],
                    }]
                    .into(),
                    deployments: vec![
                        // ClusterServiceVersionInstallSpecDeployments{},
                    ],
                }
                .into(),
            },

            customresourcedefinitions: defs.into(),

            /*
            webhookdefinitions: vec![
                ClusterServiceVersionWebhookdefinitions{
                    admission_review_versions: vec!["v1".into()].into(),
                    generate_name: "clair-webhook".into(),
                    side_effects: "None".into(),
                    r#type: ClusterServiceVersionWebhookdefinitionsType::ValidatingAdmissionWebhook.into(),
                    ..Default::default()
                },
            ].into(),
            */
            ..Default::default()
        },
        ..Default::default()
    };

    let doc = serde_json::to_value(csv)?;
    let out = out_dir.as_ref().join("clair.csv.yaml");
    let w = File::create(&out)?;
    serde_yaml::to_writer(&w, &doc)?;
    eprintln!("# wrote: {}", out.file_name().unwrap().to_string_lossy());
    */
    Ok(())
}

pub struct ManifestsOpts {
    out_dir: PathBuf,
}

impl From<&clap::ArgMatches> for ManifestsOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        let mut out_dir = m.get_one::<String>("out_dir").map(PathBuf::from).unwrap();
        if !out_dir.is_absolute() {
            out_dir = WORKSPACE.join(out_dir);
        }
        Self { out_dir }
    }
}
