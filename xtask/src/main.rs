#![doc = include_str!("../README.md")]

use std::{
    env,
    path::{Path, PathBuf},
    process,
};

use kube::{CustomResourceExt, Resource};
use signal_hook::{consts::SIGINT, low_level::pipe};
use xshell::{cmd, Shell};

mod check;
mod find;

use xtask::*;

fn main() {
    use clap::{crate_authors, crate_name, crate_version, Arg, ArgAction, Command, ValueHint};
    let cmd = Command::new(crate_name!())
        .author(crate_authors!())
        .version(crate_version!())
        .about("Build + task support for clair-operator")
        .subcommand_required(true)
        .subcommands(&[
            Command::new("bundle")
                .about("generate OLM bundle")
                .args(&[Arg::new("out_dir")
                    .long("out_dir")
                    .value_name("DIR")
                    .help("bundle output directory")
                    .long_help("Bundle output directory.")
                    .default_value("target/operator")
                    .value_hint(ValueHint::DirPath)]),
            Command::new("bundle-image")
                .about("generate OLM bundle image")
                .args(&[
                    Arg::new("out_dir")
                        .long("out_dir")
                        .value_name("DIR")
                        .help("bundle output directory")
                        .long_help("Bundle output directory.")
                        .default_value("target/operator")
                        .value_hint(ValueHint::DirPath),
                    Arg::new("image")
                        .long("image")
                        .value_name("TAG")
                        .help("container image reference")
                        .long_help("Container image reference to use during build.")
                        .default_value(format!("{BUNDLE_IMAGE}:v{}", crate_version!())),
                ]),
            Command::new("ci")
                .about("run CI setup, then tests")
                .args(&[Arg::new("pass").trailing_var_arg(true).num_args(..)]),
            Command::new("manifests").about("generate CRD manifests into config/crd"),
            Command::new("demo")
                .about("spin up a kind instance with CRDs loaded and controller running")
                .args(&[Arg::new("no_controller")
                    .long("no-run")
                    .help("don't automatically run controllers")
                    .action(ArgAction::SetTrue)]),
        ]);

    if let Err(e) = match cmd.get_matches().subcommand() {
        Some(("bundle", m)) => bundle(crate_version!(), BundleOpts::from(m)),
        Some(("bundle-image", m)) => bundle_image(BundleImageOpts::from(m)),
        Some(("ci", m)) => ci(CiOpts::from(m)),
        Some(("manifests", _)) => manifests(),
        Some(("demo", m)) => demo(DemoOpts::from(m)),
        _ => unreachable!(),
    } {
        eprintln!("{e}");
        process::exit(1);
    }
}

fn demo(opts: DemoOpts) -> Result<()> {
    use std::{io::Read, os::unix::net::UnixStream, process::Command};
    let (mut rd, wr) = UnixStream::pair()?;
    pipe::register(SIGINT, wr)?;
    let bindir = WORKSPACE.join(".bin");
    let cfgpath = WORKSPACE.join("kubeconfig");
    let cargo: &Path = &CARGO;
    let sh = Shell::new()?;

    let p = env::var("PATH")?;
    let paths = std::iter::once(bindir).chain(std::env::split_paths(&p));
    sh.set_var("PATH", std::env::join_paths(paths)?);
    sh.change_dir(WORKSPACE.as_path());
    sh.set_var("KUBECONFIG", &cfgpath);
    eprintln!("# putting KUBECONFIG at {cfgpath:?}");
    sh.set_var(
        "RUST_LOG",
        "controller=debug,clair_config=debug,webhook=debug",
    );
    check::kubectl(&sh)?;
    check::kustomize(&sh)?;
    let _guard = Kind::new(&sh, true);

    eprintln!("# regenerating CRDs");
    cmd!(sh, "{cargo} xtask manifests")
        .ignore_stdout()
        .ignore_stderr()
        .run()?;
    eprintln!("# loading CRDs");
    let _tmp = sh.create_temp_dir()?;
    let crds = _tmp.path().join("crds");
    cmd!(sh, "kustomize build config/crd -o {crds}").run()?;
    cmd!(sh, "kubectl apply -f {crds}").run()?;

    let _ctrl = if opts.run_controller {
        eprintln!("# running controllers");
        Some(
            Command::new(cargo)
                .current_dir(WORKSPACE.as_path())
                .args(["run", "--package", "controller", "--", "run"])
                .spawn()?,
        )
    } else {
        None
    };

    eprintln!("# take it for a spin:");
    eprintln!("#\tKUBECONFIG={cfgpath:?} kubectl get crds");
    eprintln!("# look in \"config/samples\" for some samples");
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
    let sh = Shell::new()?;
    sh.set_var("CI", "true");
    sh.set_var("KUBECONFIG", WORKSPACE.join("kubeconfig"));
    sh.set_var("RUST_TEST_TIME_INTEGRATION", "30000,3000000");
    sh.set_var(
        "RUST_LOG",
        "controller=trace,clair_config=trace,webhook=trace",
    );
    sh.set_var("RUST_BACKTRACE", "1");
    check::kubectl(&sh)?;
    let _kind = Kind::new(&sh, false)?;

    eprintln!("# adding CI label");
    cmd!(
        sh,
        "kubectl label namespace default projectclair.io/safe-to-run-tests=true"
    )
    .run()?;

    let coverage = cmd!(sh, "which grcov").quiet().run().is_ok();
    if coverage {
        sh.set_var("CARGO_INCREMENTAL", "0");
        sh.set_var("RUSTFLAGS", "-Cinstrument-coverage");
        sh.set_var("LLVM_PROFILE_FILE", "ci_test_%m_%p.profraw");
    } else {
        eprintln!("# skipping code coverage");
    };
    eprintln!("# running CI tests");
    let use_nextest = cmd!(sh, "{cargo} nextest help")
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok();
    let ar = WORKSPACE.join("tests.tar.zst");
    let mut test_args = vec![];
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

struct Kind {
    name: std::ffi::OsString,
}
impl Drop for Kind {
    fn drop(&mut self) {
        let name = &self.name;
        let sh = Shell::new().unwrap();
        cmd!(sh, "kind delete cluster --name {name}").run().unwrap();
    }
}
impl Kind {
    fn new(sh: &Shell, ingress: bool) -> Result<Self> {
        use scopeguard::guard;
        use std::{thread, time};
        let ingress_manifest = INGRESS_MANIFEST.as_str();
        let k8s_ver = KUBE_VERSION.as_str();
        let name = "ci";
        // TODO(hank) Move the KIND configs out of the controller crate.
        let config = WORKSPACE
            .join("etc/tests/")
            .join(format!("kind-{k8s_ver}.yaml"));
        sh.change_dir(WORKSPACE.as_path());
        check::kind(sh)?;
        cmd!(sh, "kind --config {config} create cluster --name {name}").run()?;
        let mut ok = guard(true, |ok| {
            if !ok {
                let _ = cmd!(sh, "kind delete cluster --name {name}").run();
            }
        });
        eprintln!("# waiting for pods to ready");
        cmd!(
            sh,
            "kubectl wait pods --for=condition=Ready --timeout=300s --all --all-namespaces"
        )
        .run()
        .map_err(|err| {
            *ok = false;
            err
        })?;
        if ingress {
            cmd!(sh, "kubectl apply -f {ingress_manifest}")
                .run()
                .map_err(|err| {
                    *ok = false;
                    err
                })?;
            'wait: for n in 0..=5 {
                let exec = cmd!(
                    sh,
                    "kubectl wait --namespace ingress-nginx --for=condition=Ready pod --selector=app.kubernetes.io/component=controller --timeout=90s"
                )
                .run();
                match exec {
                    Ok(_) => break 'wait,
                    Err(err) => {
                        if n == 5 {
                            *ok = false;
                            return Err(Box::new(err));
                        }
                    }
                };
                thread::sleep(time::Duration::from_secs(1));
            }
        }
        Ok(Self { name: name.into() })
    }
}

fn bundle(v: &str, opts: BundleOpts) -> Result<()> {
    manifests()?;
    let out_dir = WORKSPACE.join(&opts.out_dir);
    let sh = Shell::new()?;
    sh.change_dir(WORKSPACE.as_path());
    check::operator_sdk(&sh)?;
    check::kustomize(&sh)?;
    let _tmp = sh.create_temp_dir()?;

    let tmpfile = _tmp.path().join("out");
    cmd!(sh, "kustomize build --output={tmpfile} config/manifests").run()?;
    let out = sh.read_binary_file(tmpfile)?;

    let args = [
        "--quiet",
        "--default-channel=stable",
        "--channels=stable,testing,next",
        "--package=clair",
        "--manifests",
        "--metadata",
    ];
    sh.remove_path(&out_dir)?;
    sh.create_dir(&out_dir)?;
    sh.change_dir(&out_dir);
    cmd!(sh, "operator-sdk generate bundle {args...} --version={v}")
        .stdin(&out)
        .run()?;

    let script = "/project_layout/s/unknown/clair-operator/";
    for f in ["bundle/metadata/annotations.yaml", "bundle.Dockerfile"] {
        let sed = cmd!(sh, "sed {script} {f}").output()?;
        sh.write_file(f, &sed.stdout)?;
    }

    Ok(())
}

macro_rules! write_crds {
    ($out_dir:expr,  $($kind:ty),+ $(,)?) =>{
        let out = $out_dir;
        eprintln!("# writing to dir: {}", &out);
        $( write_crd::<$kind, _>(out)?; )+
    }
}

fn manifests() -> Result<()> {
    use api::v1alpha1;
    write_crds!(
        "config/crd",
        v1alpha1::Clair,
        v1alpha1::Indexer,
        v1alpha1::Matcher,
        v1alpha1::Updater,
        v1alpha1::Notifier,
    );
    Ok(())
}

fn write_crd<K, P>(out_dir: P) -> Result<()>
where
    K: Resource<DynamicType = ()> + CustomResourceExt,
    P: AsRef<Path>,
{
    use std::fs::File;

    let doc = serde_json::to_value(K::crd())?;
    let out = WORKSPACE
        .join(out_dir.as_ref())
        .join(format!("{}.yaml", K::crd_name()));
    let w = File::create(&out)?;
    serde_yaml::to_writer(&w, &doc)?;
    eprintln!("# wrote: {}", out.file_name().unwrap().to_string_lossy());
    Ok(())
}

struct BundleOpts {
    out_dir: PathBuf,
}
impl From<&clap::ArgMatches> for BundleOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        Self {
            out_dir: m.get_one::<String>("out_dir").map(PathBuf::from).unwrap(),
        }
    }
}

fn bundle_image(opts: BundleImageOpts) -> Result<()> {
    let cargo: &Path = &CARGO;
    let dir_arg = &opts.out_dir;
    let image = &opts.image;
    let out_dir = WORKSPACE.join(&opts.out_dir);
    let sh = Shell::new()?;
    let builder = find::builder(&sh)?;

    cmd!(sh, "{cargo} xtask bundle --out_dir={dir_arg}").run()?;
    sh.change_dir(out_dir);
    cmd!(
        sh,
        "{builder} build --quiet --tag={image} --file=bundle.Dockerfile ."
    )
    .run()?;

    Ok(())
}
struct BundleImageOpts {
    out_dir: PathBuf,
    image: String,
}
impl From<&clap::ArgMatches> for BundleImageOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        Self {
            out_dir: m.get_one::<String>("out_dir").map(PathBuf::from).unwrap(),
            image: m.get_one::<String>("image").unwrap().to_string(),
        }
    }
}
