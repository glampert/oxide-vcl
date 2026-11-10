//! Original VCL defects, reproduced (docs/VCL_BUGS.md).
//!
//! Every case here is an original-binary observation under `local/evidence/oracle`
//! or in the public corpus (`tests/corpus`). The port reproduces the original by
//! default, defects included; these tests pin both the defect and the
//! port's compatibility. When the opt-in fixes land, each defect test gains
//! a fixed-mode counterpart instead of changing.

#[path = "support/corpus.rs"]
mod corpus;
#[path = "support/vu_model.rs"]
mod vu_model;

use std::path::{Path, PathBuf};
use vcl_cli as cli;
use vu_model::{Program, State};

fn directory(case: &str) -> PathBuf {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let probe = root.join("local/evidence/oracle").join(case);
    if probe.is_dir() {
        probe
    } else {
        root.join("tests/corpus").join(case)
    }
}

/// The original's guest arguments, without the program name and input path.
fn arguments(case: &str) -> Vec<String> {
    let result = std::fs::read_to_string(directory(case).join("result.json")).unwrap();
    let start = result.find("\"guest_argv\"").unwrap();
    let list = &result[start..];
    let list = &list[list.find('[').unwrap() + 1..list.find(']').unwrap()];
    let all: Vec<String> = list
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .collect();
    all[1..all.len() - 1].to_vec()
}

fn original(case: &str) -> String {
    std::fs::read_to_string(directory(case).join("stdout.bin")).unwrap()
}

fn port(case: &str) -> Result<String, String> {
    let input = std::fs::read_to_string(directory(case).join("input.vcl")).unwrap();
    let source = corpus::without_header(&input);
    let options = cli::parse(arguments(case)).unwrap().compile;
    vcl::compile(source, &format!("/work/{case}.vcl"), &options)
        .map(|compiled| compiled.assembly)
        .map_err(|errors| format!("{errors:?}"))
}

fn accepted(case: &str) -> bool {
    let result = std::fs::read_to_string(directory(case).join("result.json")).unwrap();
    result.contains("\"exit_code\": 0,")
}

/// Lower instructions of the pair listing, as (mnemonic, operands).
fn lowers(assembly: &str) -> Vec<(String, Vec<String>)> {
    assembly
        .lines()
        .filter_map(|line| {
            let line = line.split(';').next().unwrap();
            let leading = line.len() - line.trim_start().len();
            let column = leading + 59;
            if leading < 9 || line.len() <= column {
                return None;
            }
            let mut fields = line[column..].split_whitespace();
            let mnemonic = fields.next()?.to_ascii_lowercase();
            let operands = fields
                .collect::<String>()
                .split(',')
                .map(str::to_string)
                .collect();
            Some((mnemonic, operands))
        })
        .collect()
}

fn number(text: &str) -> Option<i64> {
    if let Some(hex) = text.strip_prefix("0x") {
        // The original prints IADDIU/ISUBIU payloads as 32-bit hex.
        return i64::from_str_radix(hex, 16).ok();
    }
    text.parse().ok()
}

/// Every numeric immediate that does not fit its encoding field
/// (VU User's Manual, instruction reference): what dvp-as would truncate.
fn out_of_field(assembly: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (mnemonic, operands) in lowers(assembly) {
        let base = mnemonic.split('.').next().unwrap();
        let (operand, range) = match base {
            "iaddi" => (2, -16..=15),
            "iaddiu" | "isubiu" => (2, 0..=0x7fff),
            "lq" | "sq" | "ilw" | "isw" => (1, -1024..=1023),
            "fcand" | "fcor" | "fceq" => (1, 0..=0xff_ffff),
            "fcset" => (0, 0..=0xff_ffff),
            "fsand" | "fsor" | "fseq" => (1, 0..=0xfff),
            "fsset" => (0, 0..=0xfff),
            _ => continue,
        };
        let Some(text) = operands.get(operand) else {
            continue;
        };
        let text = text.split('(').next().unwrap();
        if let Some(value) = number(text)
            && !range.contains(&value)
        {
            found.push(format!("{base} {text}"));
        }
    }
    found
}

/// Same normalization as `support/corpus.rs`: comments and whitespace
/// are presentation.
fn normalized(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            let code = line.split(';').next().unwrap();
            code.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn checker_probes_match_the_original() {
    let root =
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence/oracle");
    let mut count = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !name.starts_with("checker-") || name.starts_with("checker-parse-") {
            continue;
        }
        let expected = normalized(&original(&name));
        let actual = normalized(&port(&name).unwrap());
        assert!(accepted(&name), "{name}");
        assert_eq!(actual, expected, "{name}");
        count += 1;
    }
    assert_eq!(count, 26);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn out_of_field_immediates_reach_the_original_output() {
    for (case, expected) in [
        (
            "checker-imm-memory-source",
            &["lq 1024", "ilw 2047", "sq -1025", "isw -2000"][..],
        ),
        (
            "checker-imm-iaddiu-source",
            &["iaddiu 0x00009c40", "isubiu 0x00011170"],
        ),
        (
            "checker-imm-flags-source",
            &["fcset 16777218", "fcand 16777217", "fsand 0x00001001"],
        ),
        ("checker-imm-repair-add", &["lq 1025", "lq 1025"]),
        ("checker-imm-repair-subtract", &["lq -1026", "lq -1026"]),
        (
            "openvcl__repros_immediates_out_of_range",
            &["isubiu 0xffffffff", "isw 2000", "iaddiu 0x00009c40"],
        ),
    ] {
        let mut found = out_of_field(&original(case));
        let mut expected: Vec<String> = expected.iter().map(|item| item.to_string()).collect();
        found.sort();
        expected.sort();
        assert_eq!(found, expected, "{case}: original");
        let mut ported = out_of_field(&port(case).unwrap());
        ported.sort();
        assert_eq!(ported, found, "{case}: port");
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn iaddi_is_normalized_rather_than_truncated() {
    for case in ["checker-imm-iaddi-normal", "checker-imm-iaddi-raw"] {
        let output = original(case);
        assert!(out_of_field(&output).is_empty(), "{case}");
        let lowers = lowers(&output);
        assert!(lowers.iter().any(|(m, o)| m == "isubiu" && o[2] == "18"));
        assert!(lowers.iter().any(|(m, o)| m == "iaddiu" && o[2] == "100"));
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn other_probes_stay_inside_their_fields() {
    let root =
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence/oracle");
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !name.starts_with("checker-") || name.starts_with("checker-imm-") || !accepted(&name) {
            continue;
        }
        assert!(out_of_field(&original(&name)).is_empty(), "{name}");
    }
}

fn float(value: f32) -> u32 {
    value.to_bits()
}

/// Run an output with qwords 1 and 2 holding the two operands.
fn run(assembly: &str, base: u16) -> State {
    let mut state = State::default();
    state.memory[1] = [float(1.5), float(2.0), float(-0.5), float(4.0)];
    state.memory[2] = [float(2.0), float(0.25), float(3.0), float(-1.0)];
    for (index, qword) in state.memory.iter_mut().enumerate().skip(3) {
        *qword = [float(index as f32); 4];
    }
    state.vi[1] = base;
    let execution = Program::parse(assembly)
        .unwrap()
        .execute(state, 256)
        .unwrap();
    assert!(execution.steps < 256);
    execution.state
}

fn product() -> [u32; 4] {
    [float(3.0), float(0.5), float(-1.5), float(-4.0)]
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn stores_and_loads_through_different_bases_are_reordered() {
    // Sequential semantics: the load at qword 901 (through VI00) reads the
    // product the store just wrote there (through iWalk = 900).
    let state = run(&original("openvcl__repros_store_load_alias"), 0);
    assert_ne!(state.memory[5], product(), "the load was not hoisted");
    assert_eq!(state.memory[5], [float(901.0); 4], "it reads stale memory");
    assert_eq!(
        state,
        run(&port("openvcl__repros_store_load_alias").unwrap(), 0)
    );
    // Timid memory (-M) orders them.
    let state = run(&original("checker-mem-store-load-timid"), 0);
    assert_eq!(state.memory[5], product());
    assert_eq!(
        state,
        run(&port("checker-mem-store-load-timid").unwrap(), 0)
    );

    // The last store to qword 901 must be fA; the original lets the
    // earlier product store land last.
    let first = [float(1.5), float(2.0), float(-0.5), float(4.0)];
    let state = run(&original("openvcl__repros_store_store_alias"), 0);
    assert_eq!(state.memory[901], product(), "the stores were swapped");
    assert_eq!(
        state,
        run(&port("openvcl__repros_store_store_alias").unwrap(), 0)
    );
    let state = run(&original("checker-mem-store-store-timid"), 0);
    assert_eq!(state.memory[901], first);

    // A copy of the base register is a different register to the original.
    let state = run(&original("checker-mem-copied-base"), 10);
    assert_eq!(state.memory[5], [float(17.0); 4], "it reads stale memory");
    assert_eq!(state, run(&port("checker-mem-copied-base").unwrap(), 10));
    // The same base register orders them.
    let state = run(&original("checker-mem-same-base"), 10);
    assert_eq!(state.memory[5], product());
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn clip_history_must_be_declared_even_when_unobserved() {
    // CLIPW shifts the earlier judgments, so the original counts it as a
    // read of the whole history: a program that tests only the current
    // judgment (FCAND 0x3f) is still rejected without `in_hw_clip`.
    for case in [
        "openvcl__repros_clipw_implicit_w",
        "openvcl__repros_clip_join",
        "openvcl__repros_clip_loop_entry",
        "openvcl__test_repro_quad_adc_bug",
        "openvcl__test_repro_quad_adc_bug_fjtrujy",
    ] {
        assert!(!accepted(case), "{case}");
        let error = port(case).unwrap_err();
        assert!(error.contains("CLIPA"), "{case}: {error}");
    }
    for case in [
        "checker-lat-clip-join-declared",
        "checker-lat-clip-loop-declared",
        "checker-lat-quad-adc-declared",
    ] {
        assert!(accepted(case), "{case}");
        assert!(port(case).is_ok(), "{case}");
    }
}

/// The checker's OpenVCL liveness defects do not occur in the original:
/// executing its output gives the sequential result.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn values_survive_sibling_loops_back_edges_and_nested_loops() {
    fn memory(state: &mut State, base: usize, count: usize) {
        for index in 0..count {
            state.memory[base + index][0] = (index as u32 * 7 + 3) % 50;
        }
    }
    fn word(state: &State, address: usize) -> u16 {
        state.memory[address][0] as u16
    }
    let execute = |case: &str, n: u16| {
        let mut state = State::default();
        memory(&mut state, 100, 200);
        state.vi[1] = n;
        state.vi[2] = 100;
        let output = original(case);
        let result = Program::parse(&output)
            .unwrap()
            .execute(state.clone(), 4096)
            .unwrap()
            .state;
        let ported = Program::parse(&port(case).unwrap())
            .unwrap()
            .execute(state.clone(), 4096)
            .unwrap()
            .state;
        assert_eq!(result, ported, "{case}");
        (state, result)
    };

    // Sibling loops: `last` comes from the first loop's final load.
    let (mut expected, result) = execute("checker-live-crossloop", 3);
    let mut ptr = 100;
    let mut last = 0;
    for _ in 0..3 {
        last = word(&expected, ptr);
        ptr += 1;
    }
    for _ in 0..2 {
        let sum = (0..6).map(|k| word(&expected, ptr + k)).sum::<u16>() + last;
        expected.memory[ptr + 6][0] = u32::from(sum);
        ptr += 8;
    }
    assert_eq!(result.memory, expected.memory);

    // Back edge: `prev` is read above its mid-loop write.
    let (mut expected, result) = execute("checker-live-backedge", 4);
    let (mut ptr, mut prev) = (100, 0);
    for _ in 0..4 {
        let current = word(&expected, ptr);
        expected.memory[ptr + 1][0] = u32::from(current + prev);
        prev = current + 1;
        let sum = (2..6).map(|k| word(&expected, ptr + k)).sum::<u16>();
        expected.memory[ptr + 6][0] = u32::from(sum);
        ptr += 8;
    }
    assert_eq!(result.memory, expected.memory);

    // Nested loops: the outer counter is read inside the inner loop.
    let (mut expected, result) = execute("checker-live-nested", 3);
    let mut ptr = 100;
    for n in (1..=3).rev() {
        for _ in 0..3 {
            let sum = (0..5).map(|k| word(&expected, ptr + k)).sum::<u16>() + n;
            expected.memory[ptr + 5][0] = u32::from(sum);
            ptr += 6;
        }
    }
    assert_eq!(result.memory, expected.memory);
}

/// The original's token decoder has a minimum length for
/// each memory operand form: 4 bytes for ILWR/ISWR `(reg)mask` and 5 for
/// ILW/ISW `off(reg)mask` and the increment forms. A one-letter base with a
/// one-character displacement (`0(p)`, `0(a)`) is rejected whatever the
/// name; `10(p)`, `0(p)x` and `(pqr)` decode. The port follows the same
/// rules, including lowercase-only trailing masks and the tag removal
/// before the length check.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn memory_operand_shapes_match_the_original() {
    let root =
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence/oracle");
    let mut count = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !name.starts_with("checker-parse-") {
            continue;
        }
        match port(&name) {
            Ok(assembly) => {
                assert!(accepted(&name), "{name}: the original rejects it");
                assert_eq!(
                    normalized(&assembly),
                    normalized(&original(&name)),
                    "{name}"
                );
            }
            Err(error) => assert!(!accepted(&name), "{name}: port rejects it: {error}"),
        }
        count += 1;
    }
    assert_eq!(count, 16);
}
