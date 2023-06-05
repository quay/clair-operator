use std::{
    path::{Path, PathBuf},
    process,
};

fn main() {
    use clap::{crate_authors, crate_name, crate_version, Arg, Command, ValueHint};
    let cmd = Command::new(crate_name!())
        .author(crate_authors!())
        .version(crate_version!())
        .about("Build + task support for clair-operator")
        .subcommand(
            Command::new("bundle")
                .about("generate OLM bundle")
                .args(&[
                    Arg::new("out_dir")
                    .long("out_dir")
                    .value_name("DIR")
                    .help("bundle output directory")
                    .long_help("Bundle output directory. If unspecified, `bundle` inside the workspace root will be used.")
                    .value_hint(ValueHint::DirPath),
                    Arg::new("patch_dir")
                    .long("patch_dir")
                    .value_name("DIR")
                    .help("CRD patch directory")
                    .long_help("CRD patch directory. If unspecified, `api/patch` inside the workspace root will be used.")
                    .value_hint(ValueHint::DirPath),
                ]),
        );

    if let Err(e) = match cmd.get_matches().subcommand() {
        Some(("bundle", m)) => bundle(crate_version!(), BundleOpts::from(m)),
        Some((n, _)) => Err(format!("bad subcommand: {n}").into()),
        None => Err("need subcommand".into()),
    } {
        eprintln!("{e}");
        process::exit(1);
    }
}

type DynError = Box<dyn std::error::Error>;
type Result<T> = std::result::Result<T, DynError>;

fn bundle(v: &str, opts: BundleOpts) -> Result<()> {
    use api::v1alpha1;
    use std::fs::create_dir_all;

    let out_dir = &opts.out_dir;
    create_dir_all(out_dir)?;
    create_dir_all(out_dir.join(v))?;
    let manifests = &out_dir.join(v).join("manifests");
    create_dir_all(manifests)?;
    create_dir_all(out_dir.join(v).join("tests"))?;
    let patches = &opts.patch_dir;

    // TODO(hank) Could write a macro to spit all these out.
    write_crd::<v1alpha1::Clair, _>(manifests, patches)?;
    write_crd::<v1alpha1::Indexer, _>(manifests, patches)?;
    write_crd::<v1alpha1::Matcher, _>(manifests, patches)?;
    write_crd::<v1alpha1::Updater, _>(manifests, patches)?;
    write_crd::<v1alpha1::Notifier, _>(manifests, patches)?;

    Ok(())
}

use kube::{CustomResourceExt, Resource};

fn write_crd<K, P>(out_dir: P, patch_dir: P) -> Result<()>
where
    K: Resource<DynamicType = ()> + CustomResourceExt,
    P: AsRef<Path>,
{
    use std::fs::File;
    let patch_dir = patch_dir.as_ref();
    let out_dir = out_dir.as_ref();

    let mut doc = serde_json::to_value(K::crd())?;
    let patchfile = patch_dir
        .join(K::version(&()).as_ref())
        .join(K::kind(&()).as_ref())
        .with_extension("yaml");
    if let Ok(mut f) = File::open(&patchfile) {
        let p: json_patch::Patch = serde_yaml::from_reader(&mut f)?;
        json_patch::patch(&mut doc, &p)?;
    };
    let out = out_dir.join(format!("{}.yaml", K::crd_name()));
    let w = File::create(&out)?;
    serde_yaml::to_writer(&w, &doc)?;
    println!("wrote: {}", out.file_name().unwrap().to_string_lossy());
    Ok(())
}

struct BundleOpts {
    out_dir: PathBuf,
    /// Needed because the kube crate doesn't have annotations for some features.
    patch_dir: PathBuf,
}
impl From<&clap::ArgMatches> for BundleOpts {
    fn from(m: &clap::ArgMatches) -> Self {
        Self {
            out_dir: m
                .get_one::<String>("out_dir")
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace().join("bundle")),
            patch_dir: m
                .get_one::<String>("patch_dir")
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace().join("api/patch")),
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
