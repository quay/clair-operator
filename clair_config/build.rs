//! Build script.
//!
//! This builds and links the config wrapper at `go/`.

use std::{
    env,
    fs::File,
    path::PathBuf,
    process::{self, Command},
    time::{Duration, SystemTime},
};

fn main() {
    // The generated header is created after the stdout capture file, and so will _always_ mark
    // this crate as needing to be rebuilt. Capturing the start time means the script can back-date
    // the mtime after successfully running.
    let start = SystemTime::now();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let go_src = src_dir.join("go");

    for f in &["go.sum", "main.go"] {
        println!("cargo:rerun-if-changed={}", go_src.join(f).display(),);
    }
    println!("cargo:rustc-link-lib=static=config");
    println!("cargo:rustc-link-search=native={}", out_dir.display());

    let mut cmd = Command::new("go");
    cmd.current_dir(&go_src)
        .envs([
            ("GOOS", env::var("CARGO_CFG_TARGET_OS").unwrap()),
            (
                "GOARCH",
                map_platform(env::var("CARGO_CFG_TARGET_ARCH").unwrap()).into(),
            ),
        ])
        .args([
            "build",
            "-ldflags=-s -w",
            "-trimpath",
            "-buildmode=c-archive",
            &format!("-o={}", out_dir.join("libconfig.a").to_string_lossy()),
            ".",
        ]);
    if let Err(e) = dbg!(cmd).status() {
        eprintln!("{e}");
        process::exit(1);
    }

    let header = out_dir.join("libconfig.h");
    let cb = Box::new(bindgen::CargoCallbacks::new());
    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .parse_callbacks(cb)
        .generate()
        .expect("Unable to generate bindings");
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    let t = start
        .checked_sub(Duration::from_secs(1))
        .expect("unable to set time to before build.rs start");
    let f = File::open(&header).expect("unable to open generated header");
    f.set_modified(t).expect("unable to set header mtime");
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
