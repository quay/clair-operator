use std::{
    borrow::Cow,
    env,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use xshell::{Shell, cmd};

pub mod check;
pub mod find;
pub mod manifests;
pub mod olm;

pub type DynError = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, DynError>;

pub static CARGO: LazyLock<PathBuf> = LazyLock::new(|| env::var_os("CARGO").unwrap().into());

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

/// This is the oldest k8s that KinD supports.
pub static KUBE_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("KUBE_VERSION").unwrap_or(String::from("1.29.3")));
pub static KIND_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("KIND_VERSION").unwrap_or(String::from("0.27.0")));
pub static KUSTOMIZE_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("KUSTOMIZE_VERSION").unwrap_or(String::from("5.6.0")));
pub static OPERATOR_SDK_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("OPERATOR_SDK_VERSION").unwrap_or(String::from("1.39.2")));
pub static OPM_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("OPM_VERSION").unwrap_or(String::from("1.51.0")));
pub static ISTIO_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("ISTIO_VERSION").unwrap_or(String::from("1.25.2")));
pub static GATEWAY_API_VERSION: LazyLock<String> =
    LazyLock::new(|| env::var("GATEWAY_API_VERSION").unwrap_or(String::from("1.2.1")));

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
        use scopeguard::guard;

        check::kubectl(sh)?;
        check::kind(sh)?;
        if self.istio {
            check::istioctl(sh)?;
        }

        let name = "ci";
        let (k8s_ver, _patch) = KUBE_VERSION.rsplit_once('.').unwrap();
        eprintln!("# using k8s version: {k8s_ver}");
        let config = WORKSPACE
            .join("etc/tests/")
            .join(format!("kind-{k8s_ver}.yaml"));
        sh.change_dir(WORKSPACE.as_path());

        // Put the guard here so that it gets torn down if the cluster gets into a half-constructed
        // state.
        let mut ok = guard(false, |ok| {
            if !ok {
                let _ = cmd!(sh, "kind --quiet delete cluster --name {name}").run();
            }
        });
        cmd!(sh, "kind --quiet --config {config} create cluster").run()?;
        eprintln!("# waiting for pods to ready");
        cmd!(
            sh,
            "kubectl wait pods --for=condition=Ready --timeout=300s --all --all-namespaces"
        )
        .run()?;

        // Load any CRDs requested:
        if self.gateway {
            eprintln!("# installing Gateway APIs");
            let manifest = GATEWAY_API_MANIFEST_URL.as_str();
            cmd!(sh, "kubectl apply -f {manifest}").run()?;
        }

        // Install any services requested:
        if [self.ingress_nginx, self.istio].iter().any(|&v| v) {
            if self.ingress_nginx {
                eprintln!("# installing ingress-nginx");
                let ingress_manifest = INGRESS_NGINX_MANIFEST_URL.as_str();
                cmd!(sh, "kubectl apply -f {ingress_manifest}").run()?;
            }
            if self.istio {
                eprintln!("# installing istio");
                cmd!(sh, "istioctl install --set profile=minimal -y").run()?;
            }
            eprintln!("# installed services, waiting for pods to ready");
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
