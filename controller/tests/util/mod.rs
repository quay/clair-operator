use std::env;

pub fn in_ci() -> bool {
    env::var("CI").is_ok()
}
