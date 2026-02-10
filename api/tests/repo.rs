use std::fs::DirEntry;
use std::{error::Error, fs::File, path::Path};

use kube::{CustomResourceExt, Resource};
use xshell::Shell;

use api::v1alpha1::*;

macro_rules! write_crds {
    ($out_dir:ident,  $($kind:ty),+ $(,)?) =>{
        $( write_crd::<$kind, _>($out_dir)?; )+
    }
}

fn write_crd<K, P>(out_dir: P) -> Result<(), Box<dyn Error>>
where
    K: Resource<DynamicType = ()> + CustomResourceExt,
    P: AsRef<Path>,
{
    let doc = serde_json::to_value(K::crd())?;
    let out = out_dir.as_ref().join(format!("{}.yaml", K::crd_name()));
    let w = File::create(&out)?;
    serde_yaml::to_writer(&w, &doc)?;
    Ok(())
}

#[test]
fn up_to_date() -> Result<(), Box<dyn Error>> {
    let sh = Shell::new()?;
    let tmp = sh.create_temp_dir()?;
    let out = tmp.path();
    write_crds!(out, Clair, Indexer, Matcher, Updater, Notifier);

    let mut got = std::fs::read_dir(out)?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    let mut want = std::fs::read_dir("../etc/operator/config/crd")?
        .filter_map(|r| {
            if r.as_ref().is_ok_and(|ent| {
                ent.file_name()
                    .to_str()
                    .expect("path shouldn't have non-utf8 characters")
                    .ends_with(".clairproject.org.yaml")
            }) {
                Some(r.unwrap())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    got.sort_by_key(DirEntry::file_name);
    want.sort_by_key(DirEntry::file_name);
    assert_eq!(
        got.iter().map(DirEntry::file_name).collect::<Vec<_>>(),
        want.iter().map(DirEntry::file_name).collect::<Vec<_>>(),
        "expected same files"
    );

    let ok = got
        .into_iter()
        .zip(want)
        .map(|(got, want)| {
            let name = got.file_name();
            let got = std::fs::read_to_string(got.path()).expect("can read back file");
            let want = std::fs::read_to_string(want.path()).expect("can read manifest");
            (name, (got, want))
        })
        .all(|(name, (got, want))| {
            let ok = got == want;
            println!(
                "{}:\t{}",
                name.to_string_lossy(),
                if ok { "OK" } else { "mismatch" }
            );
            ok
        });

    if !ok {
        Err("need to regenerate manifests".into())
    } else {
        Ok(())
    }
}
