//! The preprocessor's cases (`tests/preprocessor/<group>/<case>/`, docs/TESTING.md
//! "Preprocessor"), most of them vclpp's own test suite: each runs
//! `oxide-vcl <args> --preprocess-only input.vcl` from its directory.
//!
//! A case holds:
//!
//! - `input.vcl`, and whatever files it includes;
//! - `args`: the options, on one line (`-G` or `-g`, and the rest);
//! - `expected.vsm`: when present, the run must succeed and print exactly
//!   this; when absent, it must fail and print nothing;
//! - `expected.stderr`: the diagnostics, when there are any.
//!
//! `VCL_UPDATE_PREPROCESSOR=1` rewrites each case's expected files from the
//! run instead of comparing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cases() -> Vec<PathBuf> {
    let root = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/preprocessor"
    ));
    let mut cases = Vec::new();
    for group in std::fs::read_dir(&root).unwrap() {
        let group = group.unwrap().path();
        if !group.is_dir() {
            continue;
        }
        for case in std::fs::read_dir(&group).unwrap() {
            let case = case.unwrap().path();
            if case.join("input.vcl").is_file() {
                cases.push(case);
            }
        }
    }
    cases.sort();
    cases
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[test]
fn every_case_preprocesses_as_expected() {
    let update = std::env::var_os("VCL_UPDATE_PREPROCESSOR").is_some();
    let cases = cases();
    assert!(cases.len() >= 60, "{} cases", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let name = case
            .strip_prefix(case.parent().unwrap().parent().unwrap())
            .unwrap();
        let args = read(&case.join("args")).unwrap_or_else(|| "-G".into());
        let run = Command::new(env!("CARGO_BIN_EXE_oxide-vcl"))
            .current_dir(case)
            .args(args.split_whitespace())
            .args(["--preprocess-only", "input.vcl"])
            .output()
            .unwrap();
        let stdout = String::from_utf8(run.stdout).unwrap();
        let stderr = String::from_utf8(run.stderr).unwrap();
        let (vsm, diagnostics) = (case.join("expected.vsm"), case.join("expected.stderr"));
        if update {
            let write = |path: &Path, text: &str, keep: bool| {
                if keep {
                    std::fs::write(path, text).unwrap();
                } else {
                    let _ = std::fs::remove_file(path);
                }
            };
            write(&vsm, &stdout, run.status.success());
            write(&diagnostics, &stderr, !stderr.is_empty());
            continue;
        }
        let problem = match read(&vsm) {
            Some(_) if !run.status.success() => Some(format!("failed with {}", run.status)),
            Some(expected) if expected != stdout => Some("output differs from expected.vsm".into()),
            None if run.status.success() => Some("succeeded, but was expected to fail".into()),
            None if !stdout.is_empty() => Some("failed, but printed output".into()),
            _ if read(&diagnostics).unwrap_or_default() != stderr => {
                Some("diagnostics differ from expected.stderr".into())
            }
            _ => None,
        };
        if let Some(problem) = problem {
            failures.push(format!(
                "{}: {problem}\n--- stdout\n{stdout}--- stderr\n{stderr}",
                name.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The vclpp flags a corpus header records, as oxide-vcl options.
fn translated(vclpp_flags: &str) -> Vec<String> {
    let mut words = vclpp_flags.split_whitespace();
    let mut options = vec!["-G".to_string()];
    while let Some(word) = words.next() {
        options.push(match word {
            "-m" => {
                options[0] = "-g".into();
                continue;
            }
            "-I" => format!("-I{}", words.next().unwrap()),
            "-j" => "--vcl-boilerplate".into(),
            "-x" => "--fold-constants".into(),
            "-f" => "--flatten-subscripts".into(),
            "-Wundef" => "--warn-undef".into(),
            "-Werror" => "--warnings-as-errors".into(),
            other => panic!("unknown vclpp flag {other}"),
        });
    }
    options
}

/// The corpus inputs vclpp produced (`Produced: vclpp ...` in their
/// header), from the sources in `tests/corpus/sources`: the preprocessor
/// gives the same text, but for the boilerplate's `.init_vi_all`, which
/// replaces vclpp's VI02-VI14 list.
#[test]
fn vclpp_made_corpus_inputs_come_out_the_same() {
    let corpus = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus"));
    let mut checked = 0;
    for entry in std::fs::read_dir(&corpus).unwrap() {
        let case = entry.unwrap().path();
        let Some(input) = read(&case.join("input.vcl")) else {
            continue;
        };
        let header: Vec<&str> = input.lines().take(3).collect();
        let Some(produced) = header[2].strip_prefix("; Produced: vclpp") else {
            continue;
        };
        // `; From <repository> at <commit>: <path>`
        let (project, path) = header[1]
            .split_once(" at ")
            .and_then(|(from, rest)| Some((from.rsplit('/').next()?, rest.split_once(": ")?.1)))
            .unwrap();
        let produced = produced.split(" (vclpp").next().unwrap();
        // `vclpp, run from engine/`: no flags, from that directory.
        let (flags, directory) = match produced.strip_prefix(", run from ") {
            Some(directory) => ("", directory.trim_end_matches('/')),
            None => (produced, ""),
        };
        let root = corpus.join("sources").join(project).join(directory);
        let source = path.strip_prefix(&format!("{directory}/")).unwrap_or(path);
        let run = Command::new(env!("CARGO_BIN_EXE_oxide-vcl"))
            .current_dir(&root)
            .args(translated(flags))
            .args(["--preprocess-only", source])
            .output()
            .unwrap();
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            run.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let end = input.find("-- end of corpus header --\n").unwrap();
        let expected: String = input[end + "-- end of corpus header --\n".len()..]
            .lines()
            .map(|line| {
                if line.starts_with(".init_vi VI02,") {
                    ".init_vi_all\n".to_string()
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
        assert_eq!(String::from_utf8(run.stdout).unwrap(), expected, "{name}");
        checked += 1;
    }
    assert_eq!(checked, 22);
}
