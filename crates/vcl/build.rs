//! Sets `cfg(local_evidence)` when the unpublished original-binary
//! observations are present under the repository's `local/evidence`.
//! Tests that compare against them are ignored without it.
//!
//! Cargo takes a watched path that does not exist as changed, and reruns
//! the script (and rebuilds the crate) every time, so without the evidence
//! the script watches only itself: a clone that gains `local/evidence` later
//! needs `cargo clean -p vcl` to see it.

fn main() {
    let oracle = "../../local/evidence/oracle";
    let present = std::path::Path::new(oracle).is_dir();

    println!("cargo::rustc-check-cfg=cfg(local_evidence)");
    println!(
        "cargo::rerun-if-changed={}",
        if present { oracle } else { "build.rs" }
    );

    if present {
        println!("cargo::rustc-cfg=local_evidence");
    }
}
