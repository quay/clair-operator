#![doc = include_str!("../README.md")]

use std::{
    env,
    path::{Path, PathBuf},
    process,
};

use clap::{Arg, ArgAction, Command, ValueHint, crate_authors, crate_name, crate_version};
use signal_hook::{consts::SIGINT, low_level::pipe};
use xshell::{Shell, cmd};

use xtask::*;

fn main() {
    let deploy_args = [
        Arg::new("image")
            .long("image")
            .value_name("REPO")
            .help("container image repository")
            .long_help("Container image repository to use during build, if building an image.")
            .default_value(BUNDLE_IMAGE),
        Arg::new("version")
            .long("version")
            .value_name("vX.Y.Z")
            .help("bundle tag version")
            .long_help("Bundle tag version. If unset, one will be guessed based on git tags.")
            .default_value(crate_version!()),
    ];
    let out_dir = Arg::new("out_dir")
        .long("out-dir")
        .value_name("DIR")
        .value_hint(ValueHint::DirPath);
    let tag_version = Arg::new("version")
        .long("version")
        .value_name("vX.Y.Z")
        .help("bundle tag version")
        .long_help("Bundle tag version. If unset, one will be guessed based on cargo metadata.")
        .default_value(crate_version!());
    let dry_run = Arg::new("dry_run")
        .short('n')
        .long("dry-run")
        .help("dry run")
        .action(ArgAction::SetTrue);

    let cmd = Command::new(crate_name!())
        .author(crate_authors!())
        .version(crate_version!())
        .about("Build + task support for clair-operator")
        .subcommand_required(true)
        .subcommands(&[
            Command::new("ci")
                .display_order(1)
                .about("run CI setup, then tests")
                .args(&[Arg::new("pass").trailing_var_arg(true).num_args(..)]),
            Command::new("coverage")
                .display_order(1)
                .about("run tests with coverage enabled")
                .args(&[Arg::new("pass").trailing_var_arg(true).num_args(..)]),
            Command::new("bundle")
                .display_order(2)
                .about("generate OLM bundle")
                .args(&[
                    out_dir.clone()
                        .help("bundle output directory")
                        .long_help("Bundle output directory.")
                        .default_value("target/bundle"),
                    Arg::new("image")
                        .long("image")
                        .value_name("REPO")
                        .help("container image repository")
                        .long_help("Container image repository to use during build, if building an image.")
                        .default_value(BUNDLE_IMAGE),
                    tag_version.clone(),
                    Arg::new("build")
                        .long("build")
                        .help("build a bundle container")
                        .long_help("Build a bundle container, using the `image` and `version` flags to construct the container tag.")
                        .default_value_if("push", "true", Some("true"))
                        .action(ArgAction::SetTrue),
                    Arg::new("push")
                        .long("push")
                        .help("push a built bundle container")
                        .long_help("Push a built bundle container. Implies the `build` flag.")
                        .action(ArgAction::SetTrue),
                ]),
            Command::new("catalog")
                .display_order(2)
                .about("generate OLM catalog")
                .args(&[
                    Arg::new("bundle")
                        .long("bundle")
                        .value_name("TAG")
                        .help("bundle container image reference")
                        .long_help("Bundle container image reference to use during build.")
                        .default_value(BUNDLE_IMAGE),
                    tag_version.clone(),
                    out_dir.clone()
                        .help("catalog output directory")
                        .long_help("Catalog output directory.")
                        .default_value("target/catalog"),
                ]),
            Command::new("manifests")
                .display_order(2)
                .about("generate manifests for CRDs and operator")
                .args(&[
                    out_dir.clone()
                        .help("manifest output directory")
                        .long_help("Manifest output directory.")
                        .default_value(CONFIG_DIR.as_os_str()),
                ]),
            Command::new("generate")
                .display_order(2)
                .about("generate rust bindings for thrird-party CRDs")
                .subcommand_required(true)
                .subcommands(&[
                    Command::new("gateway-api").about("generate Gateway API bindings").args(&[dry_run.clone()]),
                    Command::new("olm").about("generate OLM bindings").args(&[dry_run.clone()]),
                ]),
            Command::new("install")
                .display_order(3)
                .about("install CRDs into the current kubernetes cluster"),
            Command::new("uninstall")
                .display_order(3)
                .about("uninstall CRDs from the current kubernetes cluster"),
            Command::new("deploy")
                .display_order(3)
                .about("install controller into the current kubernetes cluster").args(&deploy_args),
            Command::new("undeploy")
                .display_order(3)
                .about("uninstall controller from the current kubernetes cluster").args(&deploy_args),
            Command::new("demo")
                .display_order(4)
                .about("spin up a kind instance with CRDs loaded and controller running")
                .args(&[Arg::new("no_controller")
                    .long("no-run")
                    .help("don't automatically run controllers")
                    .action(ArgAction::SetTrue)]),
        ]);
    let sh = shell()
        .map_err(|err| {
            eprintln!("unable to create shell context: {err}");
            process::exit(1);
        })
        .unwrap();

    if let Err(e) = match cmd.get_matches().subcommand() {
        Some(("bundle", m)) => bundle(sh, m.into()),
        Some(("catalog", m)) => catalog(m.into()),
        Some(("ci", m)) => ci(m.into()),
        Some(("demo", m)) => demo(m.into()),
        Some(("deploy", m)) => deploy(sh, m.into()),
        Some(("install", _)) => install(sh),
        Some(("manifests", m)) => manifests::command(sh, m.into()),
        Some(("undeploy", m)) => undeploy(sh, m.into()),
        Some(("uninstall", _)) => uninstall(sh),
        Some(("coverage", m)) => coverage(sh, m.into()),
        Some(("generate", m)) => match m.subcommand() {
            Some(("gateway-api", m)) => generate::gateway_api(sh, m.into()),
            Some(("olm", m)) => generate::olm(sh, m.into()),
            Some((unknown, _)) => Err(format!("unknown subcommand: {unknown}").into()),
            None => Err("no subcommand provided".into()),
        },
        Some((unknown, _)) => Err(format!("unknown subcommand: {unknown}").into()),
        None => Err("no subcommand provided".into()),
    } {
        eprintln!("{e}");
        process::exit(1);
    }
}

fn demo(opts: DemoOpts) -> Result<()> {
    use std::{io::Read, os::unix::net::UnixStream, process::Command};
    let (mut rd, wr) = UnixStream::pair()?;
    pipe::register(SIGINT, wr)?;
    let cfgpath = WORKSPACE.join("kubeconfig");
    let cargo: &Path = &CARGO;
    let sh = shell()?;

    sh.set_var("KUBECONFIG", &cfgpath);
    eprintln!("# putting KUBECONFIG at {cfgpath:?}");
    sh.set_var(
        "RUST_LOG",
        "controller=debug,clair_config=debug,webhook=debug",
    );
    check::kubectl(&sh)?;
    check::kustomize(&sh)?;
    let _guard = KinDBuilder::default()
        .with_gateway()
        .with_istio()
        .build(&sh)?;

    eprintln!("# loading CRDs");
    cmd!(sh, "{cargo} xtask install").run()?;

    let _ctrl = if opts.run_controller {
        eprintln!("# running controllers");
        Some(
            Command::new(cargo)
                .env("KUBECONFIG", &cfgpath)
                .current_dir(WORKSPACE.as_path())
                .args(["run", "--package", "controller", "--", "run"])
                .spawn()?,
        )
    } else {
        None
    };

    eprintln!("# take it for a spin:");
    eprintln!("#\tKUBECONFIG={cfgpath:?} kubectl get crds");
    let samples = CONFIG_DIR.join("samples");
    eprintln!("# look in \"{}\" for some samples", rel(&samples));
    eprintln!("# ^C to tear down");
    let mut _block = [0];
    rd.read_exact(&mut _block)?;

    eprintln!();
    eprintln!("# 🫠");
    Ok(())
}

struct DemoOpts {
    run_controller: bool,
}

impl From<&clap::ArgMatches> for DemoOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        DemoOpts {
            run_controller: !m.get_one::<bool>("no_controller").cloned().unwrap_or(false),
        }
    }
}

fn ci(opts: CiOpts) -> Result<()> {
    let cargo: &Path = &CARGO;
    let sh = shell()?;
    sh.set_var("CI", "true");
    sh.set_var("KUBECONFIG", WORKSPACE.join("kubeconfig"));
    sh.set_var("RUST_TEST_TIME_INTEGRATION", "30000,3000000");
    sh.set_var(
        "RUST_LOG",
        "controller=trace,clair_config=trace,webhook=trace",
    );
    sh.set_var("RUST_BACKTRACE", "1");
    check::kubectl(&sh)?;
    let _kind = KinDBuilder::default().with_gateway().build(&sh)?;

    eprintln!("# adding CI label");
    cmd!(
        sh,
        "kubectl label namespace default clairproject.org/safe-to-run-tests=true"
    )
    .run()?;

    let coverage = cmd!(sh, "which grcov")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok();
    if coverage {
        sh.set_var("CARGO_INCREMENTAL", "0");
        sh.set_var("RUSTFLAGS", "-Cinstrument-coverage");
        sh.set_var("LLVM_PROFILE_FILE", "ci_test_%m_%p.profraw");
    } else {
        eprintln!("# skipping code coverage");
    };
    eprintln!("# running CI tests");
    let use_nextest = cmd!(sh, "{cargo} nextest help")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok();
    let mut test_args = vec![];
    let ar = WORKSPACE.join("tests.tar.zst");
    let w = WORKSPACE.to_string_lossy().to_string();
    if use_nextest {
        eprintln!("# using nextest");
        test_args.extend_from_slice(&["nextest", "run", "--profile", "ci"]);
        if ar.exists() {
            eprintln!("# using archive \"{}\"", ar.display());
            test_args.push("--archive-file");
            test_args.push(ar.to_str().unwrap());
            test_args.push("--workspace-remap");
            test_args.push(&w);
        } else {
            test_args.push("--features");
            test_args.push("test_ci");
        }
    } else {
        test_args.extend_from_slice(&["test", "--features", "test_ci", "--"]);
    }
    for v in &opts.pass {
        test_args.push(v);
    }
    cmd!(sh, "{cargo} {test_args...}").run()?;
    if coverage {
        let out_dir = "target/debug/coverage";
        sh.create_dir(out_dir)?;
        cmd!(
            sh,
            "grcov . --binary-path ./target/debug --source-dir . --output-types html,lcov,markdown --branch --ignore-not-existing --keep-only '*/src/*' --ignore xtask --output-path {out_dir}"
        )
        .run()?;
        cmd!(sh, "git clean --quiet --force :/*.profraw").run()?;
    }
    Ok(())
}

struct CiOpts {
    pass: Vec<String>,
}

impl From<&clap::ArgMatches> for CiOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        CiOpts {
            pass: m
                .get_many::<String>("pass")
                .unwrap_or_default()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

fn bundle(sh: Shell, opts: BundleOpts) -> Result<()> {
    let out_dir = &opts.out_dir;
    let image = &opts.image;
    let v: String = if let Some(ref v) = opts.version {
        v.to_string()
    } else {
        generate_version(&sh)?
    };
    let cargo: &Path = &CARGO;
    let tag = format!("{image}:{v}");
    cmd!(sh, "{cargo} xtask manifests").run()?;
    check::operator_sdk(&sh)?;
    check::kustomize(&sh)?;
    let _tmp = sh.create_temp_dir()?;
    let tmp = _tmp.path();

    sh.change_dir(CONFIG_DIR.join("manager"));
    cmd!(sh, "kustomize edit set image controller={tag}").run()?;
    sh.change_dir(WORKSPACE.as_path());

    let tmpfile = tmp.join("out");
    let mdir = CONFIG_DIR.join("manifests");
    cmd!(sh, "kustomize build --output={tmpfile} {mdir}").run()?;
    let out = sh.read_binary_file(tmpfile)?;

    let args = [
        "--quiet",
        "--default-channel=stable",
        "--channels=stable,testing,next",
        "--package=clair",
        "--manifests",
        "--metadata",
    ];
    sh.remove_path(out_dir)?;
    sh.create_dir(out_dir)?;
    sh.change_dir(out_dir);
    cmd!(sh, "operator-sdk generate bundle {args...} --version={v}")
        .stdin(&out)
        .run()?;

    let script = "/project_layout/s/unknown/clair-operator/";
    for f in ["bundle/metadata/annotations.yaml", "bundle.Dockerfile"] {
        let sed = cmd!(sh, "sed {script} {f}").output()?;
        sh.write_file(f, &sed.stdout)?;
    }

    eprintln!("# wrote bundle to: {}", rel(out_dir));

    if opts.build || opts.push {
        let builder = find::builder(&sh)?;
        if opts.build {
            cmd!(
                sh,
                "{builder} build --quiet --tag={tag} --file=bundle.Dockerfile ."
            )
            .run()?;
        };
        if opts.push {
            cmd!(sh, "{builder} push {tag}").run()?;
        };
    };

    Ok(())
}

struct BundleOpts {
    out_dir: PathBuf,
    image: String,
    version: Option<String>,
    build: bool,
    push: bool,
}

impl From<&clap::ArgMatches> for BundleOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        let mut out_dir = m.get_one::<String>("out_dir").map(PathBuf::from).unwrap();
        if !out_dir.is_absolute() {
            out_dir = WORKSPACE.join(out_dir);
        }
        Self {
            out_dir,
            image: m.get_one::<String>("image").unwrap().to_string(),
            version: m.get_one::<String>("version").cloned(),
            build: m.get_one::<bool>("build").cloned().unwrap_or_default(),
            push: m.get_one::<bool>("push").cloned().unwrap_or_default(),
        }
    }
}

fn install(sh: Shell) -> Result<()> {
    let cargo: &Path = &CARGO;
    cmd!(sh, "{cargo} xtask manifests").run()?;
    check::kustomize(&sh)?;
    check::kubectl(&sh)?;
    let _tmp = sh.create_temp_dir()?;
    let tmpfile = _tmp.path().join("out");
    let crds = CONFIG_DIR.join("crd");
    cmd!(sh, "kustomize build --output={tmpfile} {crds}").run()?;
    cmd!(sh, "kubectl apply --filename={tmpfile}").run()?;
    Ok(())
}

fn uninstall(sh: Shell) -> Result<()> {
    let cargo: &Path = &CARGO;
    cmd!(sh, "{cargo} xtask manifests").run()?;
    check::kustomize(&sh)?;
    check::kubectl(&sh)?;
    let _tmp = sh.create_temp_dir()?;
    let tmpfile = _tmp.path().join("out");
    let crds = CONFIG_DIR.join("crd");
    cmd!(sh, "kustomize build --output={tmpfile} {crds}").run()?;
    cmd!(sh, "kubectl delete --file={tmpfile}").run()?;
    Ok(())
}

fn deploy(sh: Shell, opts: DeployOpts) -> Result<()> {
    let tag = opts.tag(&sh)?;
    check::kubectl(&sh)?;
    check::kustomize(&sh)?;
    sh.change_dir(CONFIG_DIR.join("manager"));
    cmd!(sh, "kustomize edit set image controller={tag}").run()?;
    sh.change_dir(WORKSPACE.as_path());
    let _tmp = sh.create_temp_dir()?;
    let tmpfile = _tmp.path().join("out");
    let dir = CONFIG_DIR.join("default");
    cmd!(sh, "kustomize build --output={tmpfile} {dir}").run()?;
    cmd!(sh, "kubectl apply --file={tmpfile}").run()?;
    Ok(())
}

fn undeploy(sh: Shell, _opts: DeployOpts) -> Result<()> {
    check::kubectl(&sh)?;
    check::kustomize(&sh)?;
    let _tmp = sh.create_temp_dir()?;
    let tmpfile = _tmp.path().join("out");
    let dir = CONFIG_DIR.join("default");
    cmd!(sh, "kustomize build --output={tmpfile} {dir}").run()?;
    cmd!(sh, "kubectl delete --file={tmpfile}").run()?;
    unimplemented!()
}

struct DeployOpts {
    image: String,
    version: Option<String>,
}

impl DeployOpts {
    fn tag(&self, sh: &Shell) -> Result<String> {
        let mut buf = String::new();
        buf.push_str(&self.image);
        buf.push(':');
        if let Some(v) = &self.version {
            buf.push_str(v);
        } else {
            let v = generate_version(sh)?;
            buf.push_str(&v);
        };
        Ok(buf)
    }
}

impl From<&clap::ArgMatches> for DeployOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        Self {
            image: m.get_one::<String>("image").unwrap().to_string(),
            version: m.get_one::<String>("version").cloned(),
        }
    }
}

fn generate_version(sh: &Shell) -> Result<String> {
    const MANGLE: &str = r#"
    NF==3{
        sub(/v/, "", $1)
        split($1, v, /\./)
        v[2]++
        sub(/g/, "", $3)
        printf "v%d.%d.%d-pre%d.%s", v[1], v[2], v[3], $2, $3
    }
    NF==1{ print $0 }
    "#;
    let desc = cmd!(sh, "git describe --tags --match=v*.*.*").read()?;
    let v = cmd!(sh, "awk -F - {MANGLE}").stdin(&desc).read()?;
    Ok(v)
}

fn catalog(opts: CatalogOpts) -> Result<()> {
    let _bundle = &opts.bundle;
    let out_dir = &opts.out_dir;
    let sh = shell()?;
    check::opm(&sh)?;
    let _v = if let Some(v) = opts.version {
        v
    } else {
        generate_version(&sh)?
    };
    /*
    let bundles: Vec<String> = cmd!(sh, "git tag --list v*.*.*")
        .read()?
        .lines()
        .chain(std::iter::once(v.as_str()))
        .filter_map(|t| {
            if t != "v0.0.0" {
                Some(format!("{bundle}:{t}"))
            } else {
                None
            }
        })
        .collect();
    */
    sh.remove_path(out_dir)?;
    sh.create_dir(out_dir)?;
    sh.change_dir(out_dir);

    let catalog = "clair-catalog";
    sh.create_dir(catalog)?;
    cmd!(sh, "opm generate dockerfile {catalog}").run()?;

    let template = WORKSPACE.join("etc/operator/template.yaml");
    let pkg = cmd!(
        sh,
        "opm alpha render-template semver --output=json {template}"
    )
    .read()?;
    sh.write_file(out_dir.join(catalog).join("operator.json"), &pkg)?;

    cmd!(sh, "opm validate {catalog}").run()?;

    Ok(())
}

struct CatalogOpts {
    bundle: String,
    out_dir: PathBuf,
    version: Option<String>,
}

impl From<&clap::ArgMatches> for CatalogOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        Self {
            bundle: m.get_one::<String>("bundle").unwrap().to_string(),
            out_dir: m.get_one::<String>("out_dir").map(PathBuf::from).unwrap(),
            version: m.get_one::<String>("version").cloned(),
        }
    }
}

fn coverage(sh: Shell, opts: CoverageOpts) -> Result<()> {
    cmd!(sh, "which grcov").ignore_stdout().run()?;
    run_test_coverage(&sh, &opts.pass)?;

    Ok(())
}

struct CoverageOpts {
    pass: Vec<String>,
}

impl From<&clap::ArgMatches> for CoverageOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        CoverageOpts {
            pass: m
                .get_many::<String>("pass")
                .unwrap_or_default()
                .map(ToString::to_string)
                .collect(),
        }
    }
}
