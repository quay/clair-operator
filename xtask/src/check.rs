use std::env::{self, consts::*};

use xshell::{cmd, Shell};

use crate::*;

pub fn kind(sh: &Shell) -> Result<()> {
    const VERSION: &str = "0.20.0";
    let arch = match ARCH {
        "x86_64" => "amd64",
        arch => panic!("unmapped arch: {arch}"),
    };
    match cmd!(sh, "which kind")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
    {
        Ok(_) => Ok(()),
        Err(_) => {
            cmd!(
                sh,
                "curl -fsSLo .bin/kind{EXE_SUFFIX} https://kind.sigs.k8s.io/dl/v{VERSION}/kind-{OS}-{arch}"
            )
            .run()?;
            cmd!(sh, "chmod +x .bin/kind{EXE_SUFFIX}").run()?;
            Ok(())
        }
    }
}

pub fn kubectl(sh: &Shell) -> Result<()> {
    let version = KUBE_VERSION.as_str();
    let arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        arch => panic!("unmapped arch: {arch}"),
    };
    match cmd!(sh, "which kubectl")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
    {
        Ok(_) => Ok(()),
        Err(_) => {
            cmd!(
                sh,
                "curl -fsSLo .bin/kubectl{EXE_SUFFIX} https://storage.googleapis.com/kubernetes-release/release/{version}/bin/{OS}/{arch}/kubectl{EXE_SUFFIX}"
            )
            .run()?;
            cmd!(sh, "chmod +x .bin/kubectl{EXE_SUFFIX}").run()?;
            Ok(())
        }
    }
}

pub fn kustomize(sh: &Shell) -> Result<()> {
    const VERSION: &str = "5.0.3";
    let arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        arch => panic!("unmapped arch: {arch}"),
    };
    match cmd!(sh, "which kustomize")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
    {
        Ok(_) => Ok(()),
        Err(_) => {
            // The kustomize install is excessively dumb.
            let _tmp = sh.create_temp_dir()?;
            let tmp = _tmp.path();
            cmd!(
                sh,
                "curl -fsSLo {tmp}/tgz https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2Fv{VERSION}/kustomize_v{VERSION}_{OS}_{arch}.tar.gz"
            )
            .run()?;
            cmd!(sh, "tar -xzf -C .bin {tmp}/tgz").run()?;
            Ok(())
        }
    }
}

pub fn operator_sdk(sh: &Shell) -> Result<()> {
    const VERSION: &str = "1.29.0";
    let arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        arch => panic!("unmapped arch: {arch}"),
    };
    match cmd!(sh, "which operator-sdk")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
    {
        Ok(_) => Ok(()),
        Err(_) => {
            cmd!(
                sh,
                "curl -fsSLo .bin/operator-sdk{EXE_SUFFIX} https://github.com/operator-framework/operator-sdk/releases/download/v{VERSION}/operator-sdk_{OS}_{arch}"
            )
            .run()?;
            cmd!(sh, "chmod +x .bin/operator-sdk{EXE_SUFFIX}").run()?;
            Ok(())
        }
    }
}
