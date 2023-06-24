use std::env::{self, consts::*};

use lazy_static::lazy_static;
use xshell::{cmd, Shell};

use crate::*;

lazy_static! {
    static ref ARCH: &'static str = (|| {
        let arch = self::env::consts::ARCH;
        match arch {
            "aarch64" => "arm64",
            "powerpc64" => "ppc64le",
            "s309x" => "s390x",
            "x86_64" => "amd64",
            arch => panic!("unhandled arch: {arch}"),
        }
    })();
}

pub fn kind(sh: &Shell) -> Result<()> {
    const VERSION: &str = "0.20.0";
    let arch: &'static str = &ARCH;
    if cmd!(sh, "which kind")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_err()
    {
        let exe = format!("{}/kind{EXE_SUFFIX}", BIN_DIR.display());
        sh.create_dir(BIN_DIR.as_path())?;
        cmd!(
            sh,
            "curl -fsSLo {exe} https://kind.sigs.k8s.io/dl/v{VERSION}/kind-{OS}-{arch}"
        )
        .run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}

pub fn kubectl(sh: &Shell) -> Result<()> {
    let version = KUBE_VERSION.as_str();
    let arch: &'static str = &ARCH;
    if cmd!(sh, "which kubectl")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_err()
    {
        let exe = format!("{}/kubectl{EXE_SUFFIX}", BIN_DIR.display());
        sh.create_dir(BIN_DIR.as_path())?;
        cmd!(
            sh,
            "curl -fsSLo {exe} https://storage.googleapis.com/kubernetes-release/release/{version}/bin/{OS}/{arch}/kubectl{EXE_SUFFIX}"
        )
        .run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}

pub fn kustomize(sh: &Shell) -> Result<()> {
    const VERSION: &str = "5.0.3";
    let arch: &'static str = &ARCH;
    if cmd!(sh, "which kustomize")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_err()
    {
        // The kustomize install is excessively dumb.
        let dir = BIN_DIR.as_path();
        sh.create_dir(dir)?;
        let _tmp = sh.create_temp_dir()?;
        let tmp = _tmp.path();
        cmd!(
            sh,
            "curl -fsSLo {tmp}/tgz https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2Fv{VERSION}/kustomize_v{VERSION}_{OS}_{arch}.tar.gz"
        )
        .run()?;
        cmd!(sh, "tar -xzf -C {dir} {tmp}/tgz").run()?;
    }
    Ok(())
}

pub fn operator_sdk(sh: &Shell) -> Result<()> {
    const VERSION: &str = "1.29.0";
    let arch: &'static str = &ARCH;
    if cmd!(sh, "which operator-sdk")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_err()
    {
        let exe = format!("{}/operator-sdk{EXE_SUFFIX}", BIN_DIR.display());
        sh.create_dir(BIN_DIR.as_path())?;
        cmd!(
            sh,
            "curl -fsSLo {exe} https://github.com/operator-framework/operator-sdk/releases/download/v{VERSION}/operator-sdk_{OS}_{arch}"
        )
        .run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}

pub fn opm(sh: &Shell) -> Result<()> {
    const VERSION: &str = "1.28.0";
    let arch: &'static str = &ARCH;
    if cmd!(sh, "which opm")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_err()
    {
        let exe = format!("{}/opm{EXE_SUFFIX}", BIN_DIR.display());
        sh.create_dir(BIN_DIR.as_path())?;
        cmd!(
            sh,
            "curl -fsSLo {exe} https://github.com/operator-framework/operator-registry/releases/download/v{VERSION}/{OS}-{arch}-opm"
        ).run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}
