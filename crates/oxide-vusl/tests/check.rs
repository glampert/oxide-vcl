//! The checker's cases: `tests/vusl/check/<case>/input.vusl`, each run with
//! `oxide-vusl --check` from its directory. The diagnostics must be
//! `expected.stderr`, and the run fails exactly when they hold an error.
//! `VUSL_UPDATE=1` writes the expected files from the runs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cases() -> Vec<PathBuf> {
    let root = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vusl/check"
    ));
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("input.vusl").is_file())
        .collect();
    cases.sort();
    cases
}

fn run(case: &Path) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_oxide-vusl"))
        .args(["--check", "--color=never", "input.vusl"])
        .current_dir(case)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn every_case_gives_its_diagnostics() {
    let update = std::env::var_os("VUSL_UPDATE").is_some();
    let cases = cases();
    assert!(cases.len() >= 20, "found only {} cases", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let (status, stderr) = run(case);
        let expected_path = case.join("expected.stderr");
        if update {
            std::fs::write(&expected_path, &stderr).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
        let expected_status = if expected.contains(": error: ") { 1 } else { 0 };
        if stderr != expected || status != expected_status {
            failures.push(format!(
                "{name}: status {status} (expected {expected_status})\n--- expected\n{expected}--- got\n{stderr}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
