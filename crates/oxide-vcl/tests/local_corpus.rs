//! Extended local corpora, for programs that may not be distributed with the
//! repository (for example disassembled VU programs). Without `local/corpus/manifest.json`
//! this test passes and does nothing.
//!
//! The manifest lists sets of cases:
//!
//! ```json
//! {
//!   "sets": [
//!     { "name": "my-set", "path": "my-set", "known_differences": ["case_a"] }
//!   ]
//! }
//! ```
//!
//! `path` is relative to `local/corpus`. It holds one directory per case in
//! the public corpus format (`tests/corpus/README.md`): `input.vcl`, the
//! original vcl's `result.json` and `stdout.bin`, and optionally `stdin.bin`,
//! `stderr.bin` and `native-stdout.bin`. Every case must match the original,
//! the same comparison as `crates/oxide-vcl/tests/public_corpus.rs`. Cases listed in
//! `known_differences` are reported instead, and so is a listed case that now
//! matches, so the list can shrink.

#[path = "support/corpus.rs"]
mod corpus;

use corpus::json;
use std::path::PathBuf;

struct Set {
    name: String,
    path: PathBuf,
    known_differences: Vec<String>,
}

fn sets() -> Option<Vec<Set>> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/corpus");
    let text = std::fs::read_to_string(root.join("manifest.json")).ok()?;
    let manifest =
        json::parse(&text).unwrap_or_else(|error| panic!("local/corpus/manifest.json: {error}"));
    let entries = manifest
        .get("sets")
        .and_then(|value| value.as_array())
        .expect("local/corpus/manifest.json: no \"sets\" array");
    Some(
        entries
            .iter()
            .map(|entry| {
                let text = |key: &str| entry.get(key).and_then(|value| value.as_str());
                let name = text("name").expect("a set without a name").to_string();
                let path = root.join(text("path").unwrap_or(&name));
                let known_differences = entry
                    .get("known_differences")
                    .and_then(|value| value.as_array())
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|value| value.as_str().map(String::from))
                    .collect();
                Set {
                    name,
                    path,
                    known_differences,
                }
            })
            .collect(),
    )
}

/// Compares one set; returns its regressions.
fn check(set: &Set) -> Vec<String> {
    assert!(
        set.path.is_dir(),
        "{}: {} is missing",
        set.name,
        set.path.display()
    );
    let cases = corpus::cases(&set.path);
    assert!(!cases.is_empty(), "{}: no cases", set.name);
    let mut regressions = Vec::new();
    let mut differing = Vec::new();
    let mut fixed = Vec::new();
    for case in &cases {
        let known = set.known_differences.contains(&case.name);
        match (corpus::compare(case), known) {
            (Ok(()), false) => {}
            (Ok(()), true) => fixed.push(case.name.clone()),
            (Err(why), false) => regressions.push(format!("{}/{}: {why}", set.name, case.name)),
            (Err(_), true) => differing.push(case.name.clone()),
        }
    }
    eprintln!(
        "{}: {} cases, {} match",
        set.name,
        cases.len(),
        cases.len() - regressions.len() - differing.len()
    );
    if !differing.is_empty() {
        eprintln!("{}: known differences: {differing:?}", set.name);
    }
    if !fixed.is_empty() {
        eprintln!(
            "{}: now matching, remove from known_differences: {fixed:?}",
            set.name
        );
    }
    regressions
}

#[test]
fn local_corpora_match_the_original() {
    let Some(sets) = sets() else {
        eprintln!("local/corpus/manifest.json absent: skipped");
        return;
    };
    let regressions: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = sets
            .iter()
            .map(|set| scope.spawn(move || check(set)))
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    assert!(regressions.is_empty(), "{regressions:#?}");
}
