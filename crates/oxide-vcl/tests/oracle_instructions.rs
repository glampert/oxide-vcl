//! Descriptor-derived regression corpus checked against the original binary.
//!
//! The source bank covers all 168 eligible source instruction descriptors,
//! including generalized arithmetic, scalar forms, flags, memory and EXT.
//! It retains modifier bits, destination/source registers and issue slots.

use std::path::Path;
use vcl_cli as cli;

fn pairs(source: &str) -> Vec<(String, String)> {
    source
        .lines()
        .filter_map(|line| {
            if line.len() <= 68 || !line[..9].bytes().all(|byte| byte.is_ascii_whitespace()) {
                return None;
            }
            let upper = line[..68].trim();
            let lower = line[68..].split(';').next().unwrap().trim();
            if upper.is_empty()
                || lower.is_empty()
                || upper.starts_with('.')
                || upper.starts_with(';')
            {
                return None;
            }
            let normalize = |field: &str| {
                let mut parts = field.splitn(2, char::is_whitespace);
                let opcode = parts.next().unwrap().to_ascii_lowercase();
                let operands = parts
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<String>();
                if operands.is_empty() {
                    opcode
                } else {
                    format!("{opcode} {operands}")
                }
            };
            Some((normalize(upper), normalize(lower)))
        })
        .collect()
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn all_successful_descriptor_cases_match_original_pairs() {
    let root =
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence/oracle");
    let options = cli::parse(["-t1", "-uINSTRUCTION"]).unwrap().compile;
    let mut checked = 0;
    let mut directories: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    directories.sort();
    for directory in directories {
        let name = directory.file_name().unwrap().to_string_lossy();
        if !name.starts_with("instruction-") {
            continue;
        }
        let metadata = std::fs::read_to_string(directory.join("result.json")).unwrap();
        if !metadata.contains("\"exit_code\": 0,")
            || !metadata.contains("\"emulator_error\": false")
        {
            continue;
        }
        let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        let expected = std::fs::read_to_string(directory.join("stdout.bin")).unwrap();
        let result =
            vcl::compile(&source, "instruction-probe.vcl", &options).unwrap_or_else(|errors| {
                panic!("{name}: original accepted, port rejected: {errors:?}")
            });
        assert_eq!(
            pairs(&result.assembly),
            pairs(&expected),
            "{name}: normalized pairs differ"
        );
        checked += 1;
    }
    assert_eq!(checked, 379, "descriptor fixture corpus incomplete");
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn all_descriptor_templates_preserve_original_acceptance() {
    let root =
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence/oracle");
    let options = cli::parse(["-t1", "-uINSTRUCTION"]).unwrap().compile;
    let mut accepted = 0;
    let mut rejected = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let directory = entry.unwrap().path();
        let name = directory.file_name().unwrap().to_string_lossy();
        if !name.starts_with("instruction-") {
            continue;
        }
        let metadata = std::fs::read_to_string(directory.join("result.json")).unwrap();
        assert!(
            metadata.contains("\"emulator_error\": false")
                && metadata.contains("\"guest_exited\": true")
                && metadata.contains("\"emulator_timeout\": false"),
            "{name}: oracle did not complete"
        );
        let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        let result = vcl::compile(&source, "instruction-probe.vcl", &options);
        if metadata.contains("\"exit_code\": 0,") {
            assert!(
                result.is_ok(),
                "{name}: original accepted, port rejected: {result:?}"
            );
            accepted += 1;
        } else {
            assert!(result.is_err(), "{name}: original rejected, port accepted");
            rejected += 1;
        }
    }
    assert_eq!(
        accepted, 379,
        "successful descriptor fixture corpus incomplete"
    );
    assert_eq!(rejected, 6, "negative descriptor fixture corpus incomplete");
}
