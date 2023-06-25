use std::{
    borrow::Cow,
    env,
    path::{Path, PathBuf},
};

use lazy_static::lazy_static;
use xshell::{cmd, Shell};

pub mod check;
pub mod find;

pub type DynError = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, DynError>;

lazy_static! {
    pub static ref CARGO: PathBuf = env::var_os("CARGO").unwrap().into();
    pub static ref WORKSPACE: PathBuf = Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf();
    pub static ref CONFIG_DIR: PathBuf = WORKSPACE.join("etc/operator/config");
    pub static ref BIN_DIR: PathBuf = WORKSPACE.join(".bin");
    pub static ref KUBE_VERSION: String = env::var("KUBE_VERSION").unwrap_or(String::from("1.25"));
    pub static ref INGRESS_MANIFEST:String  = env::var("INGRESS_MANIFEST")
            .unwrap_or(String::from("https://raw.githubusercontent.com/kubernetes/ingress-nginx/main/deploy/static/provider/kind/deploy.yaml"));
}
pub const BUNDLE_IMAGE: &str = "quay.io/projectclair/clair-bundle";
pub const CATALOG_IMAGE: &str = "quay.io/projectclair/clair-catalog";

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

pub fn rel<'a>(p: &'a Path) -> Cow<'a, str> {
    p.strip_prefix(WORKSPACE.as_path())
        .unwrap()
        .to_string_lossy()
}

pub struct Kind {
    name: std::ffi::OsString,
}
impl Drop for Kind {
    fn drop(&mut self) {
        let name = &self.name;
        let sh = shell().unwrap();
        cmd!(sh, "kind delete cluster --name {name}").run().unwrap();
    }
}
impl Kind {
    pub fn new(sh: &Shell, ingress: bool) -> Result<Self> {
        use scopeguard::guard;
        use std::{thread, time};
        let ingress_manifest = INGRESS_MANIFEST.as_str();
        let k8s_ver = KUBE_VERSION.as_str();
        let name = "ci";
        // TODO(hank) Move the KIND configs out of the controller crate.
        let config = WORKSPACE
            .join("etc/tests/")
            .join(format!("kind-{k8s_ver}.yaml"));
        sh.change_dir(WORKSPACE.as_path());
        check::kubectl(sh)?;
        check::kind(sh)?;
        cmd!(sh, "kind --config {config} create cluster --name {name}").run()?;
        let mut ok = guard(true, |ok| {
            if !ok {
                let _ = cmd!(sh, "kind delete cluster --name {name}").run();
            }
        });
        eprintln!("# waiting for pods to ready");
        cmd!(
            sh,
            "kubectl wait pods --for=condition=Ready --timeout=300s --all --all-namespaces"
        )
        .run()
        .map_err(|err| {
            *ok = false;
            err
        })?;
        if ingress {
            cmd!(sh, "kubectl apply -f {ingress_manifest}")
                .run()
                .map_err(|err| {
                    *ok = false;
                    err
                })?;
            'wait: for n in 0..=5 {
                let exec = cmd!(
                    sh,
                    "kubectl wait --namespace ingress-nginx --for=condition=Ready pod --selector=app.kubernetes.io/component=controller --timeout=90s"
                )
                .run();
                match exec {
                    Ok(_) => break 'wait,
                    Err(err) => {
                        if n == 5 {
                            *ok = false;
                            return Err(Box::new(err));
                        }
                    }
                };
                thread::sleep(time::Duration::from_secs(1));
            }
        }
        Ok(Self { name: name.into() })
    }
}
