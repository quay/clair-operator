use k8s_openapi::api;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

pub mod v1alpha1;

/// RefURI is either a URI directly, or a ConfigMap or Secret key that can be deferenced to get a
/// URI.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefURI {
    #[validate(url)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<api::core::v1::SecretKeySelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_map: Option<api::core::v1::ConfigMapKeySelector>,
}

/// RefConfigOrSecret references either a ConfigMap key or a Secret key.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Validate, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefConfigOrSecret {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<api::core::v1::SecretKeySelector>,
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
