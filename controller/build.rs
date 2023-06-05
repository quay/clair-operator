//! Build script for building and linking the go config helper.

use std::{
    env,
    path::PathBuf,
    process::{self, Command},
};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    println!("cargo:rustc-env=DEFAULT_CLAIR_TAG=4.6.1");

    for f in &["go.mod", "main.go"] {
        println!(
            "cargo:rerun-if-changed={}",
            src_dir.join("go/config").join(f).to_string_lossy(),
        );
    }
    println!("cargo:rustc-link-lib=static=config");
    println!(
        "cargo:rustc-link-search=native={}",
        out_dir.to_string_lossy()
    );

    let mut cmd = Command::new("go");
    cmd.current_dir(&src_dir.join("go/config"));
    // I don't think this needs mapping:
    cmd.env("GOOS", env::var("CARGO_CFG_TARGET_OS").unwrap());
    // This does:
    cmd.env(
        "GOARCH",
        map_platform(env::var("CARGO_CFG_TARGET_ARCH").unwrap()),
    );
    cmd.args(&[
        "build",
        "-buildmode=c-archive",
        &format!("-o={}", out_dir.join("libconfig.a").to_string_lossy()),
        ".",
    ]);
    if let Err(e) = dbg!(cmd).status() {
        eprintln!("{e}");
        process::exit(1);
    }

    let bindings = bindgen::Builder::default()
        .header(out_dir.join("libconfig.h").to_string_lossy())
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .generate()
        .expect("Unable to generate bindings");
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

fn map_platform<S: AsRef<str>>(p: S) -> &'static str {
    match p.as_ref() {
        "aarch64" => "arm64",
        "powerpc64" => "ppc64",
        "x86" => "386",
        "x86_64" => "amd64",
        _ => panic!("unhandled platform: {}", p.as_ref()),
    }
}
