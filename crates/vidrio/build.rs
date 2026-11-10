//! Links the GCC runtime statically: copies `libgcc_eh.a` as `libgcc_s.a` into
//! `OUT_DIR` and puts it first on the link path, so the ELF does not need
//! `libgcc_s.so.1` (vault/CONTRACT.md §3.1).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
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
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::copy(&path, out_dir.join("libgcc_s.a")).expect("failed to copy libgcc_eh.a");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rustc-link-search=native={}", out_dir.display());
}
