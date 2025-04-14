#![warn(rustdoc::missing_crate_level_docs)]
#![warn(missing_docs)]
//! Api contains the versions of the Clair CRDs.

use k8s_openapi::api;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

pub mod v1alpha1;

/// GROUP is the kubernetes API group.
pub static GROUP: &str = "clairproject.org";

/// RefConfigOrSecret references either a ConfigMap key or a Secret key.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefConfigOrSecret {
    /// Secret indicates the Secret and key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<api::core::v1::SecretKeySelector>,
    /// Config_map indicates the ConfigMap and key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_map: Option<api::core::v1::ConfigMapKeySelector>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use kube::core::{CustomResourceExt, Resource};

    #[test]
    fn dummy() {
        println!("name = {}", v1alpha1::Clair::crd_name());
        println!("kind = {}", v1alpha1::Clair::kind(&()));
    }
}
