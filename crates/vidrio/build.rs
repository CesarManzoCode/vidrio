//! Links the GCC runtime statically: copies `libgcc_eh.a` as `libgcc_s.a` into
//! `OUT_DIR` and puts it first on the link path, so the ELF does not need
//! `libgcc_s.so.1` (vault/CONTRACT.md §3.1). Also runs the Web IDL
//! bindings generator of `bindgen/` over `idl/` and writes
//! `OUT_DIR/bindings.rs` (see `src/bindings/`).

use std::path::PathBuf;
use std::process::Command;

#[allow(dead_code)]
#[path = "bindgen/mod.rs"]
mod bindgen;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=bindgen");
    println!("cargo:rerun-if-changed=idl");
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    if let Err(e) = bindgen::generate(&manifest_dir.join("idl"), &out_dir.join("bindings.rs")) {
        panic!("Web IDL bindings generator: {e}");
    }
    let output = Command::new("cc")
        .arg("-print-file-name=libgcc_eh.a")
        .output()
        .expect("failed to run `cc -print-file-name=libgcc_eh.a`");
    assert!(
        output.status.success(),
        "`cc -print-file-name=libgcc_eh.a` failed"
    );
    let path = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    // cc echoes the bare name back when it cannot find the file.
    assert!(
        path.is_absolute() && path.is_file(),
        "libgcc_eh.a not found (cc printed {path:?})"
    );
    std::fs::copy(&path, out_dir.join("libgcc_s.a")).expect("failed to copy libgcc_eh.a");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rustc-link-search=native={}", out_dir.display());
}
