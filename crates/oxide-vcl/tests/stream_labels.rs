//! Generated stream labels, against the original's observations in
//! `local/evidence/oracle/label-*` and the other probes.
//!
//! The original's stream builder gives every source entry a flow UID from one
//! counter for the whole program, and names an unlabeled stream
//! `__v_<unique>_<uid>`. So the label of a later region depends on everything
//! before it: three straight-line regions are `__v_FIX_4`, `__v_FIX_8` and
//! `__v_FIX_12`, not 4, 5 and 6.

use std::path::{Path, PathBuf};
use vcl_cli as cli;

fn root() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence/oracle")
}

fn read(case: &str, file: &str) -> String {
    String::from_utf8_lossy(&std::fs::read(root().join(case).join(file)).unwrap()).into_owned()
}

/// The original's guest arguments without the program name, and its input
/// path, or None when the original rejected the input.
fn arguments(case: &str) -> Option<(Vec<String>, String)> {
    let result = read(case, "result.json");
    if !result.contains("\"exit_code\": 0,") {
        return None;
    }
    let start = result.find("\"guest_argv\"").unwrap();
    let list = &result[start..];
    let list = &list[list.find('[').unwrap() + 1..list.find(']').unwrap()];
    let mut all: Vec<String> = list
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .collect();
    let path = all.pop().filter(|path| path.starts_with('/'))?;
    Some((all.split_off(1), path))
}

fn compiled(case: &str) -> Option<Result<vcl::Compiled, String>> {
    let (arguments, path) = arguments(case)?;
    let options = cli::parse(arguments)
        .ok()
        .filter(|options| !options.help && options.output.is_none())?;
    Some(
        vcl::compile(&read(case, "input.vcl"), &path, &options.compile)
            .map_err(|e| format!("{e:?}")),
    )
}

fn stream_labels(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("__v_") && line.ends_with(':'))
        .map(str::to_string)
        .collect()
}

fn port(case: &str) -> String {
    compiled(case)
        .unwrap()
        .unwrap_or_else(|errors| panic!("{case}: {errors}"))
        .assembly
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn later_regions_follow_the_original_stream_counter() {
    for (case, expected) in [
        ("label-unreferenced", &["__v_FIX_4:", "__v_FIX_8:"][..]),
        ("label-end-label", &["__v_FIX_4:", "__v_FIX_10:"]),
        ("label-flagged", &["__v_FIX_4:", "__v_FIX_11:"]),
        ("label-barrier", &["__v_FIX_4:", "__v_FIX_9:"]),
        ("label-cont", &["__v_FIX_4:", "__v_FIX_9:"]),
        ("label-exitm", &["__v_FIX_4:", "__v_FIX_8:"]),
        ("label-tag", &["__v_FIX_4:", "__v_FIX_10:"]),
        ("label-target-first", &["__v_FIX_10:"]),
        ("label-empty", &["__v_FIX_8:"]),
        ("label-passthrough", &["__v_FIX_4:", "__v_FIX_10:"]),
        ("label-raw-branch", &["__v_FIX_5:", "__v_FIX_12:"]),
        ("label-raw-split", &["__v_FIX_5:", "__v_FIX_12:"]),
        ("label-raw-submode", &["__v_FIX_5:", "__v_FIX_12:"]),
        ("label-raw-loop", &["__v_FIX_14:"]),
    ] {
        assert_eq!(
            stream_labels(&read(case, "stdout.bin")),
            expected,
            "{case}: original"
        );
        assert_eq!(stream_labels(&port(case)), expected, "{case}");
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn every_accepted_probe_names_its_streams_like_the_original() {
    let mut count = 0;
    for entry in std::fs::read_dir(root()).unwrap() {
        let case = entry.unwrap().file_name().into_string().unwrap();
        if !root().join(&case).join("input.vcl").exists() {
            continue;
        }
        let Some(Ok(compiled)) = compiled(&case) else {
            continue;
        };
        assert_eq!(
            stream_labels(&compiled.assembly),
            stream_labels(&read(&case, "stdout.bin")),
            "{case}"
        );
        count += 1;
    }
    assert!(count >= 700, "{count}");
}
