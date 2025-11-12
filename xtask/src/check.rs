use std::{
    env::{self, consts::*},
    sync::LazyLock,
};

use xshell::{Shell, cmd};

use crate::*;

static ARCH: LazyLock<&'static str> = LazyLock::new(|| {
    let arch = self::env::consts::ARCH;
    match arch {
        "aarch64" => "arm64",
        "powerpc64" => "ppc64le",
        "s309x" => "s390x",
        "x86_64" => "amd64",
        arch => panic!("unhandled arch: {arch}"),
    }
});

pub fn kind(sh: &Shell) -> Result<()> {
    let version: &str = &KIND_VERSION;
    let arch: &str = &ARCH;
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
            "curl -fsSLo {exe} https://kind.sigs.k8s.io/dl/v{version}/kind-{OS}-{arch}"
        )
        .run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}

pub fn kubectl(sh: &Shell) -> Result<()> {
    let version: &str = &KUBE_VERSION;
    let arch: &str = &ARCH;
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
            "curl -fsSLo {exe} https://dl.k8s.io/release/v{version}/bin/{OS}/{arch}/kubectl{EXE_SUFFIX}"
        )
        .run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}

pub fn kustomize(sh: &Shell) -> Result<()> {
    let version: &str = &KUSTOMIZE_VERSION;
    let arch: &str = &ARCH;
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
            "curl -fsSLo {tmp}/tgz https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2Fv{version}/kustomize_v{version}_{OS}_{arch}.tar.gz"
        )
        .run()?;
        cmd!(sh, "tar -xz -C {dir} -f {tmp}/tgz").run()?;
    }
    Ok(())
}

pub fn operator_sdk(sh: &Shell) -> Result<()> {
    let version: &str = &OPERATOR_SDK_VERSION;
    let arch: &str = &ARCH;
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
            "curl -fsSLo {exe} https://github.com/operator-framework/operator-sdk/releases/download/v{version}/operator-sdk_{OS}_{arch}"
        )
        .run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}

pub fn opm(sh: &Shell) -> Result<()> {
    let version: &str = &OPM_VERSION;
    let arch: &str = &ARCH;
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
            "curl -fsSLo {exe} https://github.com/operator-framework/operator-registry/releases/download/v{version}/{OS}-{arch}-opm"
        ).run()?;
        cmd!(sh, "chmod +x {exe}").run()?;
    }
    Ok(())
}

pub fn istioctl(sh: &Shell) -> Result<()> {
    let version: &str = &ISTIO_VERSION;
    let arch: &str = &ARCH;
    if cmd!(sh, "which istioctl")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_err()
    {
        let dir = BIN_DIR.as_path();
        sh.create_dir(dir)?;
        let _tmp = sh.create_temp_dir()?;
        let tmp = _tmp.path();
        cmd!(
            sh,
            "curl -fsSLo {tmp}/tgz https://github.com/istio/istio/releases/download/{version}/istio-{version}-{OS}-{arch}.tar.gz"
        )
        .run()?;
        cmd!(
            sh,
            "tar -xz -C {dir} -f {tmp}/tgz --strip-components=2 */bin/istioctl"
        )
        .run()?;
    }
    Ok(())
}
