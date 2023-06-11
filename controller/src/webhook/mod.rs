mod mutate;
mod validate;

pub use mutate::webhook as mutate;
pub use validate::webhook as validate;

use warp::{filters::BoxedFilter, http::header::HeaderValue, Filter};

type DynError = Box<dyn std::error::Error>;

#[derive(Debug)]
struct ValidationFailure(String);
impl warp::reject::Reject for ValidationFailure {}
impl From<&str> for ValidationFailure {
    fn from(v: &str) -> Self {
        Self(String::from(v))
    }
}
impl ToString for ValidationFailure {
    fn to_string(&self) -> String {
        format!("validation failed: {}", &self.0)
    }
}

fn predicate<S: ToString>(prefix: S) -> BoxedFilter<()> {
    let prefix = prefix.to_string();
    warp::path(prefix)
        .and(warp::path("v1alpha1"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::body::content_length_limit(4 << 20))
        // TODO(hank) These header checks should parse the header properly.
        .and(
            warp::header::value("content-type")
                .and_then(|ct: HeaderValue| async move {
                    let ct = ct
                        .to_str()
                        .map_err(|e| ValidationFailure(format!("header error: {e}")))?;
                    if ct.starts_with("application/json") {
                        Ok(())
                    } else {
                        Err(warp::reject())
                    }
                })
                .untuple_one(),
        )
        .and(
            warp::header::value("accept")
                .and_then(|ct: HeaderValue| async move {
                    let ct = ct
                        .to_str()
                        .map_err(|e| ValidationFailure(format!("header error: {e}")))?;
                    if ct.contains("application/json") {
                        Ok(())
                    } else {
                        Err(warp::reject())
                    }
                })
                .untuple_one(),
        )
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::{
        api::TypeMeta,
        core::{admission::AdmissionReview, DynamicObject},
    };

    #[tokio::test]
    async fn check_validate() {
        let p = predicate("validate");

        assert!(!warp::test::request().path("/").matches(&p).await);
        assert!(!warp::test::request().path("/validate").matches(&p).await);
        assert!(
            !warp::test::request()
                .path("/validate")
                .method("POST")
                .matches(&p)
                .await
        );
        let v: AdmissionReview<DynamicObject> = AdmissionReview {
            request: None,
            response: None,
            types: TypeMeta {
                api_version: "admission.k8s.io/v1".to_string(),
                kind: "AdmissionReview".to_string(),
            },
        };
        assert!(
            warp::test::request()
                .path("/validate/v1alpha1")
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .method("POST")
                .json(&v)
                .matches(&p)
                .await
        );
    }

    #[tokio::test]
    async fn check_mutate() {
        let p = predicate("mutate");

        assert!(!warp::test::request().path("/").matches(&p).await);
        assert!(!warp::test::request().path("/mutate").matches(&p).await);
        assert!(
            !warp::test::request()
                .path("/mutate")
                .method("POST")
                .matches(&p)
                .await
        );
        let v: AdmissionReview<DynamicObject> = AdmissionReview {
            request: None,
            response: None,
            types: TypeMeta {
                api_version: "admission.k8s.io/v1".to_string(),
                kind: "AdmissionReview".to_string(),
            },
        };
        assert!(
            warp::test::request()
                .path("/mutate/v1alpha1")
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .method("POST")
                .json(&v)
                .matches(&p)
                .await
        );
    }
}
