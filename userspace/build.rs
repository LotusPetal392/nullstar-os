use std::{env, path::PathBuf};

fn main() {
    let manifest_directory =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR was not set"));
    let linker_script = manifest_directory.join("linker.ld");

    println!("cargo:rerun-if-changed={}", linker_script.display());
    let target = env::var("TARGET").expect("TARGET was not set");
    if target != "x86_64-unknown-none" {
        return;
    }

    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
    println!("cargo:rustc-link-arg=-no-pie");
    println!("cargo:rustc-link-arg=--build-id=none");
}
