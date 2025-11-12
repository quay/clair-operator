use xshell::{Shell, cmd};

use crate::*;

pub fn builder(sh: &Shell) -> Result<String> {
    for exe in ["podman", "docker"] {
        if cmd!(sh, "which {exe}")
            .quiet()
            .ignore_stdout()
            .ignore_stderr()
            .run()
            .is_ok()
        {
            return Ok(exe.to_string());
        }
    }
    Err("failed to find \"podman\" or \"docker\"".into())
}
