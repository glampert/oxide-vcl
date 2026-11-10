//! Sets `cfg(local_evidence)` when the unpublished original-binary
//! observations are present under the repository's `local/evidence`.
//! Tests that compare against them are ignored without it.

fn main() {
    let oracle = "../../local/evidence/oracle";

    println!("cargo::rustc-check-cfg=cfg(local_evidence)");
    println!("cargo::rerun-if-changed={oracle}");

    if std::path::Path::new(oracle).is_dir() {
        println!("cargo::rustc-cfg=local_evidence");
    }
}
