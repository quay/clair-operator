use std::{
    borrow::Cow,
    env,
    ffi::OsStr,
    ops::Not,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use scopeguard::guard;
use serde::Deserialize;
use xshell::{Shell, cmd};

pub mod check;
pub mod find;
pub mod generate;
pub mod manifests;
pub mod olm;

pub type DynError = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, DynError>;

pub static CARGO: LazyLock<PathBuf> = LazyLock::new(|| env::var_os("CARGO").unwrap().into());
pub static GITHUB_ACTIONS: LazyLock<bool> =
    LazyLock::new(|| env::var_os("GITHUB_ACTIONS").is_some_and(|v| v == "true"));

macro_rules! log_group {
    ($msg:expr) => {
        let _group = GITHUB_ACTIONS.then(|| {
            println!("::group::{}", $msg);
            ::scopeguard::guard((), |_| {
                println!("::endgroup::");
            })
        });
    };
}

// Paths:
pub static WORKSPACE: LazyLock<PathBuf> = LazyLock::new(|| {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
});
pub static CONFIG_DIR: LazyLock<PathBuf> = LazyLock::new(|| WORKSPACE.join("etc/operator/config"));
pub static BIN_DIR: LazyLock<PathBuf> = LazyLock::new(|| WORKSPACE.join(".bin"));

// Versions:
static METADATA: LazyLock<CargoMetadata> = LazyLock::new(|| {
    let cargo: &Path = &CARGO;
    let sh = Shell::new().expect("unable to create xshell");
    let out = cmd!(sh, "{cargo} metadata --format-version=1")
        .quiet()
        .output()
        .expect("failed to get cargo metadata");
    serde_json::from_slice(&out.stdout).expect("unable to parse JSON")
});

#[derive(Deserialize)]
struct CargoMetadata {
    metadata: Metadata,
}

impl CargoMetadata {
    fn kube(&self) -> String {
        self.metadata.ci.kube.clone()
    }
    fn kind(&self) -> String {
        self.metadata.ci.kind.clone()
    }
    fn kustomize(&self) -> String {
        self.metadata.ci.kustomize.clone()
    }
    fn operator_sdk(&self) -> String {
        self.metadata.ci.operator_sdk.clone()
    }
    fn operator_api(&self) -> String {
        self.metadata.ci.operator_api.clone()
    }
    fn opm(&self) -> String {
        self.metadata.ci.opm.clone()
    }
    fn istio(&self) -> String {
        self.metadata.ci.istio.clone()
    }
    fn gateway_api(&self) -> String {
        self.metadata.ci.gateway_api.clone()
    }
    fn kopium(&self) -> String {
        self.metadata.ci.kopium.clone()
    }
}

#[derive(Deserialize)]
struct Metadata {
    ci: CiVersions,
}

#[derive(Deserialize)]
struct CiVersions {
    #[serde(rename = "kube-version")]
    kube: String,
    #[serde(rename = "kind-version")]
    kind: String,
    #[serde(rename = "kustomize-version")]
    kustomize: String,
    #[serde(rename = "operator-sdk-version")]
    operator_sdk: String,
    #[serde(rename = "operator-api-version")]
    operator_api: String,
    #[serde(rename = "opm-version")]
    opm: String,
    #[serde(rename = "istio-version")]
    istio: String,
    #[serde(rename = "gateway-api-version")]
    gateway_api: String,
    #[serde(rename = "kopium-version")]
    kopium: String,
}

pub static KUBE_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("KUBE_VERSION").unwrap_or_else(|_| METADATA.kube()));
pub static KIND_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("KIND_VERSION").unwrap_or_else(|_| METADATA.kind()));
pub static KUSTOMIZE_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("KUSTOMIZE_VERSION").unwrap_or_else(|_| METADATA.kustomize()));
pub static OPERATOR_SDK_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("OPERATOR_SDK_VERSION").unwrap_or_else(|_| METADATA.operator_sdk()));
pub static OPERATOR_API_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("OPERATOR_API_VERSION").unwrap_or_else(|_| METADATA.operator_api()));
pub static OPM_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("OPM_VERSION").unwrap_or_else(|_| METADATA.opm()));
pub static KOPIUM_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("KOPIUM_VERSION").unwrap_or_else(|_| METADATA.kopium()));
pub static ISTIO_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("ISTIO_VERSION").unwrap_or_else(|_| METADATA.istio()));
pub static GATEWAY_API_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("GATEWAY_API_VERSION").unwrap_or_else(|_| METADATA.gateway_api()));

// URLs:
pub static INGRESS_NGINX_MANIFEST_URL: LazyLock<String> = LazyLock::new(|| {
    env::var("INGRESS_NGINX_MANIFEST_URL")
            .unwrap_or(String::from("https://raw.githubusercontent.com/kubernetes/ingress-nginx/main/deploy/static/provider/kind/deploy.yaml"))
});
pub static GATEWAY_API_MANIFEST_URL: LazyLock<String> = LazyLock::new(|| {
    env::var("GATEWAY_API_MANIFEST_URL").unwrap_or(
    format!("https://github.com/kubernetes-sigs/gateway-api/releases/download/v{}/standard-install.yaml", GATEWAY_API_VERSION.as_str()))
});

// Container images:
pub const BUNDLE_IMAGE: &str = "quay.io/projectclair/clair-bundle";
pub const CATALOG_IMAGE: &str = "quay.io/projectclair/clair-catalog";

/// Shell constructs a [Shell] with the environment modified in a consistent way.
pub fn shell() -> xshell::Result<Shell> {
    let sh = Shell::new()?;
    let p = env::var("PATH").expect("PATH environment variable missing");
    let paths = std::iter::once(BIN_DIR.to_path_buf()).chain(std::env::split_paths(&p));
    sh.set_var(
        "PATH",
        std::env::join_paths(paths).expect("unable to reconstruct PATH"),
    );
    sh.change_dir(WORKSPACE.as_path());

    Ok(sh)
}

/// Rel constructs a path relative to the workspace.
pub fn rel<'a>(p: &'a Path) -> Cow<'a, str> {
    p.strip_prefix(WORKSPACE.as_path())
        .unwrap()
        .to_string_lossy()
}

/// KinD is a running KinD cluster.
///
/// It deletes the cluster on drop.
pub struct KinD {
    name: String,
}

impl Drop for KinD {
    fn drop(&mut self) {
        let name = self.name.as_str();
        let sh = shell().unwrap();
        cmd!(sh, "kind --quiet delete cluster --name {name}")
            .run()
            .unwrap();
    }
}

#[derive(Default)]
pub struct KinDBuilder {
    ingress_nginx: bool,
    gateway: bool,
    istio: bool,
}

impl KinDBuilder {
    pub fn with_ingress_nginx(self) -> Self {
        Self {
            ingress_nginx: true,
            ..self
        }
    }

    pub fn with_gateway(self) -> Self {
        Self {
            gateway: true,
            ..self
        }
    }

    pub fn with_istio(self) -> Self {
        Self {
            istio: true,
            ..self
        }
    }

    /// If this fails, check the KinD "[known issues]."
    /// A likely culprit is the user `inotify` limits.
    ///
    /// [known issues]: https://kind.sigs.k8s.io/docs/user/known-issues/
    pub fn build(self, sh: &Shell) -> Result<KinD> {
        check::kubectl(sh)?;
        check::kind(sh)?;
        if self.istio {
            check::istioctl(sh)?;
        }

        let name = "ci";
        let (k8s_ver, _patch) = KUBE_VERSION.rsplit_once('.').unwrap();
        println!(
            "{}using k8s version: {k8s_ver}",
            if *GITHUB_ACTIONS { "::notice::" } else { "# " }
        );
        let config = WORKSPACE
            .join("etc/tests/")
            .join(format!("kind-{k8s_ver}.yaml"));
        sh.change_dir(WORKSPACE.as_path());

        let quiet = GITHUB_ACTIONS.not().then(|| "--quiet");
        // Put the guard here so that it gets torn down if the cluster gets into a half-constructed
        // state.
        let mut ok = guard(false, |ok| {
            if !ok {
                let _ = cmd!(sh, "kind --quiet delete cluster --name {name}").run();
            }
        });

        {
            log_group!("create cluster");
            cmd!(sh, "kind {quiet...} --config {config} create cluster").run()?;
            cmd!(
                sh,
                "kubectl wait pods --for=condition=Ready --timeout=300s --all --all-namespaces"
            )
            .run()?;
        }

        // Load any CRDs requested:
        if self.gateway {
            log_group!("installing Gateway APIs");
            let manifest = GATEWAY_API_MANIFEST_URL.as_str();
            cmd!(sh, "kubectl apply -f {manifest}").run()?;
        }

        // Install any services requested:
        if [self.ingress_nginx, self.istio].iter().any(|&v| v) {
            log_group!("installing extra services");
            if self.ingress_nginx {
                let ingress_manifest = INGRESS_NGINX_MANIFEST_URL.as_str();
                cmd!(sh, "kubectl apply -f {ingress_manifest}").run()?;
            }
            if self.istio {
                cmd!(sh, "istioctl install --set profile=minimal -y").run()?;
            }
            cmd!(
                sh,
                "kubectl wait pods --for=condition=Ready --timeout=300s --all --all-namespaces"
            )
            .run()?;
        }

        *ok = true;
        Ok(KinD { name: name.into() })
    }
}

pub fn run_test_coverage<V, I>(sh: &Shell, args: I) -> Result<()>
where
    V: AsRef<OsStr>,
    I: IntoIterator<Item = V>,
{
    let cargo: &Path = &CARGO;
    let ws = WORKSPACE.to_path_buf();
    let bin_dir = WORKSPACE.join("target/debug/deps");
    let cov_dir = WORKSPACE.join("target/coverage");

    let _ = sh.remove_path(&cov_dir);
    sh.create_dir(&cov_dir)?;

    cmd!(sh, "{cargo} test {args...}")
        .envs([
            ("CARGO_INCREMENTAL", "0"),
            ("RUSTFLAGS", "-Cinstrument-coverage"),
        ])
        .env("LLVM_PROFILE_FILE", cov_dir.join("%p-%m.profraw"))
        .run()?;

    let args = [
        "--output-types",
        "html,lcov,markdown",
        "--branch",
        "--ignore-not-existing",
        "--source-dir",
        ".",
        "--ignore",
        "../*",
        "--ignore",
        "/*",
        "--ignore",
        "xtask/*",
        "--ignore",
        "*/src/tests/*",
    ];
    cmd!(sh,
            "grcov {cov_dir} --binary-path {bin_dir} --prefix-dir {ws} --output-path {cov_dir} {args...}").run()?;

    let report = cov_dir.join("markdown.md");
    let md = sh.read_file(&report)?;
    let path = GITHUB_ACTIONS
        .then(|| sh.var_os("GITHUB_STEP_SUMMARY"))
        .flatten();
    if let Some(path) = path {
        sh.write_file(path, md)?;
    } else {
        print!("$ cat {}\n{md}", rel(&report));
    }

    Ok(())
}
