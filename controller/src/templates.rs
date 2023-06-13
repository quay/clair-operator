use std::{borrow::Cow, collections::HashMap};

use k8s_openapi::serde;
use lazy_static::lazy_static;
use tracing::trace;

// TODO(hank) Set up compile-time compression for these assets.
#[iftree::include_file_tree(
    "
paths = '''
**
!tests
!README.md
'''
base_folder = 'etc/'
#template.identifiers = false
"
)]
pub struct Asset {
    relative_path: &'static str,
    pub get_bytes: fn() -> Cow<'static, [u8]>,
}

lazy_static! {
    static ref TEMPLATES: HashMap<String, Cow<'static, [u8]>> = {
        ASSETS
            .iter()
            .filter_map(|a| {
                a.relative_path
                    .strip_prefix("templates/")
                    .map(|p| (p.to_string(), (a.get_bytes)()))
            })
            .collect()
    };
    static ref DROPINS: HashMap<String, Cow<'static, [u8]>> = {
        ASSETS
            .iter()
            .filter_map(|a| {
                if a.relative_path.ends_with("_dropin.json-patch") {
                    Some((a.relative_path.to_string(), (a.get_bytes)()))
                } else {
                    None
                }
            })
            .collect()
    };
}

pub type DynError = Box<dyn std::error::Error>;

// Descibe how Asset.get_bytes will behave:
#[cfg(debug_assertions)]
const FROM_DISK: bool = true;
#[cfg(not(debug_assertions))]
const FROM_DISK: bool = false;

pub async fn resource_for<S, K>(kind: S) -> Result<K, DynError>
where
    S: AsRef<str>,
    K: kube::Resource<DynamicType = ()> + serde::de::DeserializeOwned,
{
    use json_patch::Patch;
    use serde_json::Value;
    let kn = K::kind(&()).to_ascii_lowercase();
    let base_file = format!("{kn}.yaml");
    let patch_file = format!("{kn}-{}.yaml-patch", kind.as_ref());
    trace!(
        base_file,
        patch_file,
        embed = !FROM_DISK,
        "looking for resources"
    );

    let mut doc: Value = TEMPLATES
        .get(&base_file)
        .ok_or_else(|| -> DynError { format!("missing template: {base_file}").into() })
        .map(|b| serde_yaml::from_slice(b))??;
    let patch: Option<Patch> = TEMPLATES
        .get(&patch_file)
        .and_then(|b| serde_yaml::from_slice(b).ok());

    if let Some(patch) = patch.as_ref() {
        trace!("found patch");
        json_patch::patch(&mut doc, patch)?;
    }
    serde_json::from_value(doc).map_err(|err| err.into())
}

/// Returns as json.
pub async fn dropin_for<S>(kind: S) -> Result<Cow<'static, [u8]>, DynError>
where
    S: AsRef<str>,
{
    let kind = kind.as_ref();
    let base_file = format!("{kind}_dropin.json-patch");
    trace!(base_file, embed = !FROM_DISK, "looking for resource");

    DROPINS
        .get(&base_file)
        .map(Clone::clone)
        .ok_or_else(|| -> DynError { format!("missing dropin: {base_file}").into() })
}
