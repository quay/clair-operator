use clap::ArgMatches;
use xshell::{Shell, cmd};

use super::{GATEWAY_API_VERSION, OPERATOR_API_VERSION, Result, WORKSPACE, check::kopium};

pub fn olm(sh: Shell, opts: OlmOpts) -> Result<()> {
    static TYPES: [&str; 1] = ["cluster_service_versions"];
    kopium(&sh)?;
    let version = OPERATOR_API_VERSION.as_str();
    let out_dir = WORKSPACE.join("xtask/src/olm");

    let tmp = sh.create_temp_dir()?;
    for t in TYPES {
        let tn = t.replace('_', "");
        let tmp = tmp.path().join(&tn).with_extension("yaml");
        let tmp = tmp.as_path();
        cmd!(
            sh,
            "curl -sSfLo {tmp} https://github.com/operator-framework/api/raw/refs/tags/v{version}/crds/operators.coreos.com_{tn}.yaml")
            .quiet()
            .run()?;
        let out = cmd!(
            &sh,
            "kopium --auto --derive Default --smart-derive-elision --filename {tmp}"
        )
        .read()?;
        let f = out_dir.join(t).with_extension("rs");
        if opts.dry_run {
            eprintln!("# would write to: {}", f.display());
            println!("{out}");
        } else {
            sh.write_file(&f, out)?;
            cmd!(&sh, "rustfmt --quiet {f}").quiet().run()?;
        }
    }

    Ok(())
}

pub struct OlmOpts {
    dry_run: bool,
}

impl From<&ArgMatches> for OlmOpts {
    fn from(m: &ArgMatches) -> Self {
        Self {
            dry_run: m.get_one::<bool>("dry_run").cloned().unwrap_or_default(),
        }
    }
}

pub fn gateway_api(sh: Shell, opts: GatewayApiOpts) -> Result<()> {
    static TYPES: [&str; 6] = [
        "backendtlspolicies",
        "gatewayclasses",
        "gateways",
        "grpcroutes",
        "httproutes",
        "referencegrants",
    ];
    kopium(&sh)?;
    let v = GATEWAY_API_VERSION
        .split_once('.')
        .expect("dotted version string")
        .0;
    let version = GATEWAY_API_VERSION.as_str();
    let out_dir = WORKSPACE
        .join("gateway_networking_k8s_io/src")
        .join(format!("v{v}"));

    let tmp = sh.create_temp_dir()?;
    for t in TYPES {
        let tmp = tmp.path().join(t).with_extension("yaml");
        let tmp = tmp.as_path();
        cmd!(
            sh,
            "curl -sSfLo {tmp} https://github.com/kubernetes-sigs/gateway-api/raw/refs/tags/v{version}/config/crd/standard/gateway.networking.k8s.io_{t}.yaml")
            .quiet()
            .run()?;
        let out = cmd!(
            &sh,
            "kopium --auto --derive Default --smart-derive-elision --filename {tmp}"
        )
        .read()?;
        let f = out_dir.join(t).with_extension("rs");
        if opts.dry_run {
            eprintln!("# would write to: {}", f.display());
            println!("{out}");
        } else {
            sh.write_file(&f, out)?;
            cmd!(&sh, "rustfmt --quiet {f}").quiet().run()?;
        }
    }

    Ok(())
}

pub struct GatewayApiOpts {
    dry_run: bool,
}

impl From<&ArgMatches> for GatewayApiOpts {
    fn from(m: &ArgMatches) -> Self {
        Self {
            dry_run: m.get_one::<bool>("dry_run").cloned().unwrap_or_default(),
        }
    }
}
