#![doc = include_str!("../README.md")]

use std::{
    env,
    io::Read,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
};

use signal_hook::{consts::SIGINT, low_level::pipe};

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
            Command::new("ci").about("run CI setup, then tests").arg(Arg::new("pass").trailing_var_arg(true).num_args(..)),
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
    let (mut rd, wr) = UnixStream::pair()?;
    pipe::register(SIGINT, wr)?;

    let cfgpath = workspace().join("kubeconfig");
    eprintln!("putting KUBECONFIG at {cfgpath:?}");
    env::set_var("KUBECONFIG", &cfgpath);
    env::set_var("RUST_LOG", "controller=debug,clair_config=debug");
    let _guard = KIND::new()?;
    eprintln!("regenerating CRDs");
    let _ = Command::new(env::var_os("CARGO").unwrap())
        .args(["xtask", "manifests"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status();
    eprintln!("waiting for pods to ready");
    let _ = Command::new("kubectl")
        .args(&[
            "wait",
            "pods",
            "--for=condition=Ready",
            "--timeout=300s",
            "--all",
            "--all-namespaces",
        ])
        .status();
    eprintln!("loading CRDs");
    let kus = Command::new("kustomize")
        .current_dir(workspace())
        .stdout(process::Stdio::piped())
        .args(&["build", "config/crd"])
        .spawn()?;
    let _ = Command::new("kubectl")
        .args(&["apply", "-f", "-"])
        .stdin(kus.stdout.unwrap())
        .status()?;

    let _ctrl = if opts.run_controller {
        eprintln!("running controllers");
        Some(
            Command::new(env::var_os("CARGO").unwrap())
                .current_dir(workspace())
                .args(["run", "--package", "controller", "--", "run"])
                .spawn()?,
        )
    } else {
        None
    };

    eprintln!("take it for a spin:");
    eprintln!("\tKUBECONFIG={cfgpath:?} kubectl get crds");
    eprintln!("look in \"config/samples\" for some samples");
    eprintln!("^C to tear down");
    let mut _block = [0];
    rd.read_exact(&mut _block)?;

    eprintln!("");
    eprintln!("🫠");
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
    env::set_var("CI", "true");
    env::set_var("KUBECONFIG", workspace().join("kubeconfig"));
    env::set_var("RUST_TEST_TIME_INTEGRATION", "30000,3000000");
    env::set_var("RUST_LOG", "controller=trace,clair_config=trace");
    env::set_var("RUST_BACKTRACE", "1");
    let _guard = KIND::new()?;
    eprintln!("waiting for pods to ready");
    let _ = Command::new("kubectl")
        .args(&[
            "wait",
            "pods",
            "--for=condition=Ready",
            "--timeout=300s",
            "--all",
            "--all-namespaces",
        ])
        .status();
    eprintln!("adding CI label");
    let _ = Command::new("kubectl")
        .args(&[
            "label",
            "namespace",
            "default",
            "projectclair.io/safe-to-run-tests=true",
        ])
        .status();
    eprintln!("running CI tests");
    let use_nextest = Command::new(env::var_os("CARGO").unwrap())
        .args(&["nextest", "help"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()?
        .success();
    let w = workspace().to_string_lossy().to_string();
    let ar = workspace().join("tests.tar.zst");
    let mut test_args = vec![];
    if use_nextest {
        eprintln!("using nextest");
        test_args.extend_from_slice(&["nextest", "run", "--profile", "ci"]);
        if ar.exists() {
            eprintln!("using archive \"{}\"", ar.display());
            test_args.push("--archive-file");
            test_args.push(ar.to_str().unwrap());
            test_args.push("--workspace-remap");
            test_args.push(&w);
        }
    } else {
        test_args.push("test");
    }
    if !use_nextest || !ar.exists() {
        test_args.push("--features");
        test_args.push("test_ci");
    }
    test_args.push("--");
    for v in &opts.pass {
        test_args.push(&v);
    }
    let status = Command::new(env::var_os("CARGO").unwrap())
        .args(test_args)
        .current_dir(workspace())
        .status()?;
    if !status.success() {
        return Err("tests failed".into());
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

struct KIND {
    name: std::ffi::OsString,
}
impl Drop for KIND {
    fn drop(&mut self) {
        let mut cmd = Command::new("kind");
        cmd.current_dir(workspace());
        cmd.arg("delete");
        cmd.arg("cluster");
        cmd.arg("--name");
        cmd.arg(&self.name);
        let _ = cmd.status();
    }
}
impl KIND {
    fn new() -> Result<Self> {
        let k8s_ver = std::env::var("KUBE_VERSION").unwrap_or(String::from("1.25"));
        let kind_name = "ci";
        let kind_config = workspace()
            .join("controller/etc/tests/")
            .join(format!("kind-{}.yaml", k8s_ver));
        let mut cmd = Command::new("kind");
        cmd.current_dir(workspace());
        cmd.arg("--config");
        cmd.arg(&kind_config);
        cmd.arg("create");
        cmd.arg("cluster");
        cmd.arg("--name");
        cmd.arg(&kind_name);
        let status = cmd.status()?;
        if !status.success() {
            return Err("kind exit non-zero".into());
        }
        Ok(Self {
            name: kind_name.into(),
        })
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
