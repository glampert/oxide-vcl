//! Corpus cases: a directory per program holding `input.vcl`, the original's
//! `result.json`, `stdin.bin`, `stdout.bin` and usually `stderr.bin`, and an
//! optional `native-stdout.bin` that replaces `stdout.bin` as the reference.
//! Shared by `crates/oxide-vcl/tests/public_corpus.rs` and `crates/oxide-vcl/tests/local_corpus.rs`.

#![allow(dead_code)]

#[path = "json.rs"]
pub mod json;
#[path = "sha256.rs"]
pub mod sha256;

use std::path::{Path, PathBuf};
use vcl_cli as cli;

/// The provenance header of a public corpus input ends with this line.
pub const HEADER_END: &str = "; -- end of corpus header --";

pub struct Case {
    pub name: String,
    pub directory: PathBuf,
    /// The input exactly as the original compiled it.
    pub source: String,
    /// The original's options, without the program name and input path.
    pub arguments: Vec<String>,
    /// The input path the original was given.
    pub path: String,
    pub exit_code: i64,
    /// The reference listing.
    pub expected: String,
}

/// Every case directory under `root` (holding `input.vcl` and `result.json`),
/// sorted by name.
pub fn cases(root: &Path) -> Vec<Case> {
    let mut directories: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("{}: {error}", root.display()))
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.join("input.vcl").is_file() && path.join("result.json").is_file())
        .collect();
    directories.sort();
    directories
        .iter()
        .map(|directory| load(directory))
        .collect()
}

/// Drops a leading provenance header (through [`HEADER_END`]).
pub fn without_header(text: &str) -> &str {
    if !text.starts_with("; oxide-vcl corpus program") {
        return text;
    }
    let end = text
        .find(HEADER_END)
        .expect("corpus header without its end line");
    let rest = &text[end + HEADER_END.len()..];
    rest.strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest)
}

pub fn load(directory: &Path) -> Case {
    let name = directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let read = |file: &str| {
        let bytes = std::fs::read(directory.join(file))
            .unwrap_or_else(|error| panic!("{name}/{file}: {error}"));
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let result =
        json::parse(&read("result.json")).unwrap_or_else(|error| panic!("{name}: {error}"));
    let input = read("input.vcl");
    let source = without_header(&input).to_string();
    if let Some(recorded) = result.get("input_sha256").and_then(|value| value.as_str()) {
        assert_eq!(
            sha256::hex(source.as_bytes()),
            recorded,
            "{name}: input.vcl is not the input the original compiled"
        );
    }
    let argv: Vec<String> = result
        .get("guest_argv")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{name}: no guest_argv"))
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect();
    let path = argv.last().cloned().unwrap();
    let arguments = argv[1..argv.len() - 1].to_vec();
    let exit_code = result
        .get("exit_code")
        .and_then(|value| value.as_f64())
        .unwrap_or_else(|| panic!("{name}: no exit_code")) as i64;
    let reference = if directory.join("native-stdout.bin").is_file() {
        "native-stdout.bin"
    } else {
        "stdout.bin"
    };
    let expected = read(reference);
    Case {
        name,
        directory: directory.to_path_buf(),
        source,
        arguments,
        path,
        exit_code,
        expected,
    }
}

/// Comments and whitespace are presentation; everything else must be equal.
pub fn normalized(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            let code = line.split(';').next().unwrap();
            code.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}

/// Compiles the case with the port and compares it with the original:
/// acceptance, and for an accepted program its normalized listing.
pub fn compare(case: &Case) -> Result<(), String> {
    let options =
        cli::parse(case.arguments.clone()).map_err(|error| format!("options: {error:?}"))?;
    match vcl::compile(&case.source, &case.path, &options.compile) {
        Ok(compiled) if case.exit_code == 0 => {
            let (actual, expected) = (normalized(&compiled.assembly), normalized(&case.expected));
            if actual == expected {
                return Ok(());
            }
            let line = actual
                .iter()
                .zip(&expected)
                .position(|(a, b)| a != b)
                .unwrap_or(actual.len().min(expected.len()));
            Err(format!(
                "listing differs at normalized line {line} ({} vs {} lines)",
                actual.len(),
                expected.len()
            ))
        }
        Ok(_) => Err(format!(
            "port accepts, the original exits {}",
            case.exit_code
        )),
        // The copy detector's abort: an ERROR, no listing and exit 0, which
        // the driver reproduces from `CompileError::exit_status`.
        Err(errors)
            if case.exit_code == 0
                && normalized(&case.expected).is_empty()
                && errors.iter().all(|error| error.exit_status() == 0) =>
        {
            Ok(())
        }
        Err(errors) if case.exit_code == 0 => Err(format!(
            "port rejects what the original accepts: {errors:?}"
        )),
        Err(_) => Ok(()),
    }
}
