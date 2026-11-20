//! The code generator's idioms: `tests/vusl/codegen/<case>/input.vusl`,
//! each with the VCL it generates, `expected.vcl`, from
//! `oxide-vusl --stop-at-vcl` in its directory. Each case's comment names
//! the hand-written sequence it stands for; the VCL must also compile.
//! `VUSL_UPDATE=1` writes the expected files from the runs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cases() -> Vec<PathBuf> {
    let root = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vusl/codegen"
    ));
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("input.vusl").is_file())
        .collect();
    cases.sort();
    cases
}

fn run(case: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_oxide-vusl"))
        .args(["--color=never"])
        .args(args)
        .arg("input.vusl")
        .current_dir(case)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn every_case_generates_its_vcl() {
    let update = std::env::var_os("VUSL_UPDATE").is_some();
    let cases = cases();
    assert!(cases.len() >= 15, "found only {} cases", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let (status, vcl, stderr) = run(case, &["--stop-at-vcl"]);
        if status != 0 {
            failures.push(format!("{name}: status {status}\n{stderr}"));
            continue;
        }
        let expected_path = case.join("expected.vcl");
        if update {
            std::fs::write(&expected_path, &vcl).unwrap();
        } else {
            let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
            if vcl != expected {
                failures.push(format!(
                    "{name}: the VCL differs\n--- expected\n{expected}--- got\n{vcl}"
                ));
            }
        }
        let (status, _, stderr) = run(case, &[]);
        if status != 0 {
            failures.push(format!("{name}: vcl rejected it\n{stderr}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
