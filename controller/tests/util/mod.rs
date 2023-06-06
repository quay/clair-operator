use std::env;

/// In_ci reports if the test is being run in CI.
pub fn in_ci() -> bool {
    env::var("CI").is_ok_and(|v| v == "true")
}
