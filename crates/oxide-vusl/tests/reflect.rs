//! The reflection of the check cases that carry `expected.json` and
//! `expected.h`: each is run with `--check --reflect --header` and must
//! write those files. When a C++ compiler is on the PATH, every header is
//! also compiled in one translation unit, so two headers that share an
//! included struct are checked together. `VUSL_UPDATE=1` writes the
//! expected files from the runs.

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
        .filter(|path| path.join("expected.h").is_file())
        .collect();
    cases.sort();
    cases
}

fn scratch(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("oxide-vusl-reflect-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// The JSON and the header the case's program reflects as.
fn reflect(case: &Path, out: &Path) -> (String, String) {
    let name = case.file_name().unwrap().to_string_lossy().to_string();
    let json = out.join(format!("{name}.json"));
    // Not `{name}.h`: the directory goes on the include path, where a case
    // named like a system header (`features`) would stand in for it.
    let header = out.join(format!("{name}.vusl.h"));
    let output = Command::new(env!("CARGO_BIN_EXE_oxide-vusl"))
        .arg("--check")
        .arg(format!("--reflect={}", json.display()))
        .arg(format!("--header={}", header.display()))
        .arg("input.vusl")
        .current_dir(case)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        std::fs::read_to_string(json).unwrap(),
        std::fs::read_to_string(header).unwrap(),
    )
}

#[test]
fn cases_reflect_as_expected() {
    let update = std::env::var_os("VUSL_UPDATE").is_some();
    let cases = cases();
    assert!(cases.len() >= 3, "found only {} cases", cases.len());
    let out = scratch("expected");
    let mut failures = Vec::new();
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let (json, header) = reflect(case, &out);
        for (file, text) in [("expected.json", &json), ("expected.h", &header)] {
            let path = case.join(file);
            if update {
                std::fs::write(&path, text).unwrap();
            } else if std::fs::read_to_string(&path).unwrap_or_default() != *text {
                failures.push(format!("{name}/{file} differs:\n{text}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&out);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The headers in one translation unit, each twice, compiled as C++17.
#[test]
fn headers_compile_together() {
    let compiler = std::env::var("CXX").unwrap_or_else(|_| "c++".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        eprintln!("no C++ compiler ({compiler}): the headers are not compiled");
        return;
    }
    let out = scratch("compile");
    let mut unit = String::new();
    for case in cases() {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        reflect(&case, &out);
        unit.push_str(&format!(
            "#include \"{name}.vusl.h\"\n#include \"{name}.vusl.h\"\n"
        ));
    }
    unit.push_str("int main() { return 0; }\n");
    let source = out.join("unit.cpp");
    std::fs::write(&source, unit).unwrap();
    let output = Command::new(&compiler)
        .args([
            "-std=c++17",
            "-fsyntax-only",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pedantic",
        ])
        .arg(format!("-I{}", out.display()))
        .arg(&source)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&out);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
