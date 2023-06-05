use std::process;

use controller::updaters;
mod util;
use util::*;

#[test]
fn simple() {
    if !in_ci() {
        eprintln!("skipping");
        process::exit(0);
    }
    println!("hello world");
}
