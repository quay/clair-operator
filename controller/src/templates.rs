use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use k8s_openapi::serde;
use tracing::debug;

#[iftree::include_file_tree(
    "
paths = '*'
base_folder = 'etc/templates/'
template.identifiers = false
"
)]
pub struct Asset {
    relative_path: &'static str,
    contents_bytes: &'static [u8],
}

pub struct Assets {
    dir: Option<PathBuf>,
    tmpls: HashMap<String, &'static [u8]>,
}

pub type DynError = Box<dyn std::error::Error>;

impl Assets {
    pub fn new<P: AsRef<Path>>(root: P) -> Assets {
        let root = root.as_ref();
        let dir = if root.exists() {
            Some(root.into())
        } else {
            None
        };

        Assets {
            dir,
            tmpls: ASSETS
                .iter()
                .map(|a| (a.relative_path.into(), a.contents_bytes))
                .collect(),
        }
    }

    pub async fn resource_for<S, K>(&self, kind: S) -> Result<K, DynError>
    where
        S: AsRef<str>,
        K: kube::Resource<DynamicType = ()> + serde::de::DeserializeOwned,
    {
        use json_patch::Patch;
        use serde_json::Value;
        let kn = K::kind(&()).to_ascii_lowercase();
        let base_file = format!("{kn}.yaml");
        let patch_file = format!("{kn}-{}.yaml-patch", kind.as_ref());
        debug!(
            base_file,
            patch_file,
            embed = self.dir.is_none(),
            "looking for resources"
        );

        let mut doc: Value = if let Some(p) = self.dir.as_ref() {
            fs::File::open(p.clone().join(&base_file))
                .map(|ref mut f| serde_yaml::from_reader(f))??
        } else {
            self.tmpls
                .get(&base_file)
                .ok_or_else(|| -> DynError { format!("missing template: {base_file}").into() })
                .map(|b| serde_yaml::from_slice(b))??
        };

        let patch: Option<Patch> = if let Some(p) = self.dir.as_ref() {
            fs::File::open(p.clone().join(&patch_file))
                .map_err(DynError::from)
                .and_then(|ref mut f| serde_yaml::from_reader(f).map_err(DynError::from))
                .ok()
        } else {
            self.tmpls
                .get(&patch_file)
                .and_then(|b| serde_yaml::from_slice(b).ok())
        };

        if let Some(patch) = patch.as_ref() {
            debug!("found patch");
            json_patch::patch(&mut doc, patch)?;
        }
        serde_json::from_value(doc).map_err(|err| err.into())
    }
}
