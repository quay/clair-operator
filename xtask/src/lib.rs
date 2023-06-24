use std::{
    env,
    path::{Path, PathBuf},
};

use lazy_static::lazy_static;

pub type DynError = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, DynError>;

lazy_static! {
    pub static ref CARGO: PathBuf = env::var_os("CARGO").unwrap().into();
    pub static ref WORKSPACE: PathBuf = Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf();
    pub static ref BIN_DIR: PathBuf = WORKSPACE.join(".bin");
    pub static ref KUBE_VERSION: String = env::var("KUBE_VERSION").unwrap_or(String::from("1.25"));
    pub static ref INGRESS_MANIFEST:String  = env::var("INGRESS_MANIFEST")
            .unwrap_or(String::from("https://raw.githubusercontent.com/kubernetes/ingress-nginx/main/deploy/static/provider/kind/deploy.yaml"));
}
pub static BUNDLE_IMAGE: &str = "quay.io/projectclair/clair-bundle";
pub static CATALOG_IMAGE: &str = "quay.io/projectclair/clair-catalog";
