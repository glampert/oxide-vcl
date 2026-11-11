//! The probe suite (`tests/probes`, see its README): small programs written
//! for this project to pin down one behaviour of vcl 1.4beta7 each, with the
//! original's recorded output. The port must match every one, the same
//! comparison as the public corpus: acceptance, and the listing with comments
//! and whitespace ignored. Unlike the local evidence they come from, these run
//! everywhere, CI included.

#[path = "support/corpus.rs"]
mod corpus;

use std::path::PathBuf;

/// Probes whose listing the port does not reproduce yet, with the reason.
/// A listed probe that starts matching fails the test, so the list shrinks.
const KNOWN_DIFFERENCES: &[(&str, &str)] = &[
    // Driver behaviour, which this in-process comparison does not model; the
    // command-line rules have their own tests in `vcl_cli`.
    (
        "cli-g-invalid",
        "-gbad is a command-line error before compiling",
    ),
    (
        "cli-output-empty",
        "a bare -o is rejected when the output opens",
    ),
    (
        "cli-output-file",
        "the listing goes to the -o file, not stdout",
    ),
    (
        "cli-same-file",
        "-o naming the input is a command-line error",
    ),
    // Port differences.
    (
        "comments-raw",
        "a raw block's registers are reassigned to VF02 where vcl picks VF03",
    ),
    (
        "cli-symbolic",
        "-s prints the port's register-web names (__web_N), not vcl's",
    ),
    (
        "label-raw-split",
        "a copy next to raw blocks is coalesced onto its source register",
    ),
    (
        "raw-boundary-debug",
        "raw boundary reduction is partial (docs/RAW_BOUNDARIES.md)",
    ),
    (
        "raw-boundary-flags-observed",
        "raw boundary reduction is partial (docs/RAW_BOUNDARIES.md)",
    ),
    (
        "raw-boundary-min2",
        "raw boundary reduction is partial (docs/RAW_BOUNDARIES.md)",
    ),
    (
        "raw-boundary-min4",
        "raw boundary reduction is partial (docs/RAW_BOUNDARIES.md)",
    ),
    (
        "raw-boundary-min4-no-compress",
        "raw boundary reduction is partial (docs/RAW_BOUNDARIES.md)",
    ),
];

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/probes"))
}

#[test]
fn every_probe_matches_the_original() {
    let cases = corpus::cases(&root());
    assert!(cases.len() > 800, "only {} probes", cases.len());
    let mut failures = Vec::new();
    let mut fixed = Vec::new();
    for case in &cases {
        let known = KNOWN_DIFFERENCES.iter().any(|(name, _)| *name == case.name);
        match (corpus::compare(case), known) {
            (Err(error), false) => failures.push(format!("{}: {error}", case.name)),
            (Ok(()), true) => fixed.push(case.name.clone()),
            _ => {}
        }
    }
    assert!(
        failures.is_empty() && fixed.is_empty(),
        "{} of {} probes differ:\n{}\nlisted as known differences but now match: {fixed:?}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
