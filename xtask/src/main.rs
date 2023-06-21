#![doc = include_str!("../README.md")]

use std::{
    env,
    path::{Path, PathBuf},
    process,
};

use signal_hook::{consts::SIGINT, low_level::pipe};
use xshell::{cmd, Shell};

fn main() {
    use clap::{crate_authors, crate_name, crate_version, Arg, ArgAction, Command, ValueHint};
    let cmd = Command::new(crate_name!())
        .author(crate_authors!())
        .version(crate_version!())
        .about("Build + task support for clair-operator")
        .subcommand_required(true)
        .subcommands(&[
            Command::new("bundle")
                .hide(true) // Not ready yet.
                .about("generate OLM bundle")
                .args(&[
                    Arg::new("out_dir")
                    .long("out_dir")
                    .value_name("DIR")
                    .help("bundle output directory")
                    .long_help("Bundle output directory. If unspecified, `bundle` inside the workspace root will be used.")
                    .value_hint(ValueHint::DirPath),
                ]),
            Command::new("ci")
                .about("run CI setup, then tests")
                .args(&[
                    Arg::new("pass").trailing_var_arg(true).num_args(..),
                ]),
            Command::new("manifests")
                .about("generate CRD manifests into config/crd"),
            Command::new("demo")
                .about("spin up a kind instance with CRDs loaded and controller running")
                .args(&[
                    Arg::new("no_controller").long("no-run").help("don't automatically run controllers").action(ArgAction::SetTrue)
                ]),
        ]);

    if let Err(e) = match cmd.get_matches().subcommand() {
        Some(("bundle", m)) => bundle(crate_version!(), BundleOpts::from(m)),
        Some(("ci", m)) => ci(CiOpts::from(m)),
        Some(("manifests", _)) => manifests(),
        Some(("demo", m)) => demo(DemoOpts::from(m)),
        _ => unreachable!(),
    } {
        eprintln!("{e}");
        process::exit(1);
    }
}

type DynError = Box<dyn std::error::Error>;
type Result<T> = std::result::Result<T, DynError>;

fn demo(opts: DemoOpts) -> Result<()> {
    use std::{io::Read, os::unix::net::UnixStream, process::Command};
    let (mut rd, wr) = UnixStream::pair()?;
    pipe::register(SIGINT, wr)?;
    let ws = workspace();
    let bindir = ws.join(".bin");
    let cfgpath = ws.join("kubeconfig");
    let cargo = env::var_os("CARGO").unwrap();
    let sh = Shell::new()?;

    let p = env::var("PATH")?;
    let paths = std::iter::once(bindir).chain(std::env::split_paths(&p));
    sh.set_var("PATH", std::env::join_paths(paths)?);
    sh.change_dir(workspace());
    sh.set_var("KUBECONFIG", &cfgpath);
    eprintln!("# putting KUBECONFIG at {cfgpath:?}");
    sh.set_var(
        "RUST_LOG",
        "controller=debug,clair_config=debug,webhook=debug",
    );
    check_kubectl(&sh)?;
    check_kustomize(&sh)?;
    let _guard = KIND::new(&sh, true);

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
                .current_dir(workspace())
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

    eprintln!("");
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
    let cargo = env::var_os("CARGO").unwrap();
    let sh = Shell::new()?;
    sh.set_var("CI", "true");
    sh.set_var("KUBECONFIG", workspace().join("kubeconfig"));
    sh.set_var("RUST_TEST_TIME_INTEGRATION", "30000,3000000");
    sh.set_var(
        "RUST_LOG",
        "controller=trace,clair_config=trace,webhook=trace",
    );
    sh.set_var("RUST_BACKTRACE", "1");
    check_kubectl(&sh)?;
    let _kind = KIND::new(&sh, false)?;

    eprintln!("# adding CI label");
    cmd!(
        sh,
        "kubectl label namespace default projectclair.io/safe-to-run-tests=true"
    )
    .run()?;

    let coverage = cmd!(sh, "which grcov").run().is_ok();
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
    let ar = workspace().join("tests.tar.zst");
    let mut test_args = vec![];
    let w = workspace().to_string_lossy().to_string();
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
        test_args.push(&v);
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

fn check_kind(sh: &Shell) -> Result<()> {
    const VERSION: &str = "0.20.0";
    let os = env::consts::OS;
    let exe = env::consts::EXE_SUFFIX;
    let arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        arch => panic!("unmapped arch: {arch}"),
    };
    match cmd!(sh, "which kind").run() {
        Ok(_) => Ok(()),
        Err(_) => {
            cmd!(
                sh,
                "curl -fsSLo .bin/kind{exe} https://kind.sigs.k8s.io/dl/v{VERSION}/kind-{os}-{arch}"
            )
            .run()?;
            cmd!(sh, "chmod +x .bin/kind{exe}").run()?;
            Ok(())
        }
    }
}
fn check_kubectl(sh: &Shell) -> Result<()> {
    let version = k8s_version();
    let os = env::consts::OS;
    let exe = env::consts::EXE_SUFFIX;
    let arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        arch => panic!("unmapped arch: {arch}"),
    };
    match cmd!(sh, "which kubectl").run() {
        Ok(_) => Ok(()),
        Err(_) => {
            cmd!(
                sh,
                "curl -fsSLo .bin/kubectl{exe} https://storage.googleapis.com/kubernetes-release/release/{version}/bin/{os}/{arch}/kubectl{exe}"
            )
            .run()?;
            cmd!(sh, "chmod +x .bin/kubectl{exe}").run()?;
            Ok(())
        }
    }
}
fn check_kustomize(sh: &Shell) -> Result<()> {
    const VERSION: &str = "5.0.3";
    let os = env::consts::OS;
    let arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        arch => panic!("unmapped arch: {arch}"),
    };
    match cmd!(sh, "which kustomize").run() {
        Ok(_) => Ok(()),
        Err(_) => {
            // The kustomize install is excessively dumb.
            let _tmp = sh.create_temp_dir()?;
            let tmp = _tmp.path();
            cmd!(
                sh,
                "curl -fsSLo {tmp}/tgz https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2Fv{VERSION}/kustomize_v{VERSION}_{os}_{arch}.tar.gz"
            )
            .run()?;
            cmd!(sh, "tar -xzf -C .bin {tmp}/tgz").run()?;
            Ok(())
        }
    }
}

fn k8s_version() -> String {
    std::env::var("KUBE_VERSION").unwrap_or(String::from("1.25"))
}

struct KIND {
    name: std::ffi::OsString,
}
impl Drop for KIND {
    fn drop(&mut self) {
        let name = &self.name;
        let sh = Shell::new().unwrap();
        cmd!(sh, "kind delete cluster --name {name}").run().unwrap();
    }
}
impl KIND {
    fn new(sh: &Shell, ingress: bool) -> Result<Self> {
        use scopeguard::guard;
        use std::{thread, time};
        let ingress_manifest  = std::env::var("INGRESS_MANIFEST")
            .unwrap_or(String::from("https://raw.githubusercontent.com/kubernetes/ingress-nginx/main/deploy/static/provider/kind/deploy.yaml"));
        let k8s_ver = k8s_version();
        let name = "ci";
        // TODO(hank) Move the KIND configs out of the controller crate.
        let config = workspace()
            .join("etc/tests/")
            .join(format!("kind-{k8s_ver}.yaml"));
        sh.change_dir(workspace());
        check_kind(&sh)?;
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
                        } else {
                            ()
                        }
                    }
                };
                thread::sleep(time::Duration::from_secs(1));
            }
        }
        Ok(Self { name: name.into() })
    }
}

macro_rules! write_crds {
    ($out_dir:expr,  $($kind:ty),+ $(,)?) =>{
        let out = $out_dir;
        println!("writing to dir: {}", out.display());
        $( write_crd::<$kind, _>(out)?; )+
    }
}

fn bundle(v: &str, opts: BundleOpts) -> Result<()> {
    use api::v1alpha1;
    use std::fs::create_dir_all;

    let out_dir = &opts.out_dir;
    create_dir_all(out_dir)?;
    create_dir_all(out_dir.join(v))?;
    let manifests = &out_dir.join(v).join("manifests");
    create_dir_all(manifests)?;
    create_dir_all(out_dir.join(v).join("tests"))?;

    write_crds!(
        out_dir,
        v1alpha1::Clair,
        v1alpha1::Indexer,
        v1alpha1::Matcher,
        v1alpha1::Updater,
        v1alpha1::Notifier,
    );
    Ok(())
}

fn manifests() -> Result<()> {
    use api::v1alpha1;
    write_crds!(
        &workspace().join("config/crd"),
        v1alpha1::Clair,
        v1alpha1::Indexer,
        v1alpha1::Matcher,
        v1alpha1::Updater,
        v1alpha1::Notifier,
    );
    Ok(())
}

use kube::{CustomResourceExt, Resource};

fn write_crd<K, P>(out_dir: P) -> Result<()>
where
    K: Resource<DynamicType = ()> + CustomResourceExt,
    P: AsRef<Path>,
{
    use std::fs::File;
    let out_dir = out_dir.as_ref();

    let doc = serde_json::to_value(K::crd())?;
    let out = out_dir.join(format!("{}.yaml", K::crd_name()));
    let w = File::create(&out)?;
    serde_yaml::to_writer(&w, &doc)?;
    println!("wrote: {}", out.file_name().unwrap().to_string_lossy());
    Ok(())
}

struct BundleOpts {
    out_dir: PathBuf,
}
impl From<&clap::ArgMatches> for BundleOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        Self {
            out_dir: m
                .get_one::<String>("out_dir")
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace().join("bundle")),
        }
    }
}

fn workspace() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}
