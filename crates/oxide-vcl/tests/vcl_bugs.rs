//! Original VCL defects, reproduced (docs/VCL_BUGS.md).
//!
//! Every case here is an original-binary observation under `local/evidence/oracle`
//! or in the public corpus (`tests/corpus`). The port reproduces the original by
//! default, defects included; these tests pin both the defect and the
//! port's compatibility. Each fixed defect also has a test with its
//! `--fix=NAME` (and `--mode=oxide`, which implies every fix).

#[path = "support/corpus.rs"]
mod corpus;
#[path = "support/vu_model.rs"]
mod vu_model;

use std::collections::BTreeSet;
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

// ----------------------------------------------
// F4: exit rows without an E bit (`--fix=end`)
// ----------------------------------------------

/// A probe's directory under `tests/probes`.
fn probe_directory(case: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/probes")
        .join(case)
}

/// Compiles a probe with the original's flags followed by `extra`.
fn compile_probe(case: &str, extra: &[&str]) -> String {
    let directory = probe_directory(case);
    let input = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
    let result = std::fs::read_to_string(directory.join("result.json")).unwrap();
    let start = result.find("\"guest_argv\"").unwrap();
    let list = &result[start..];
    let list = &list[list.find('[').unwrap() + 1..list.find(']').unwrap()];
    let all: Vec<&str> = list
        .split(',')
        .map(|item| item.trim().trim_matches('"'))
        .collect();
    let flags = all[1..all.len() - 1]
        .iter()
        .copied()
        .chain(extra.iter().copied());
    let options = cli::parse(flags).unwrap().compile;
    vcl::compile(&input, &format!("/work/{case}.vcl"), &options)
        .unwrap()
        .assembly
}

/// Runs a listing from its first row with VF01 = `SOURCE`.
fn execute_from_entry(assembly: &str) -> Result<vu_model::Execution, String> {
    let mut state = State::default();
    state.set_vector(1, SOURCE);
    Program::parse(assembly)?.execute(state, 64)
}

const SOURCE: [f32; 4] = [1.5, 2.0, -0.5, 4.0];

/// `src * src + (src + src)`, lane by lane.
fn square_plus_double() -> [u32; 4] {
    SOURCE.map(|lane| (lane * lane + (lane + lane)).to_bits())
}

/// The listing's tables: each one's rows, split at the table comments (vcl
/// mode) or labels.
fn tables(assembly: &str) -> Vec<Vec<String>> {
    let mut tables: Vec<Vec<String>> = vec![Vec::new()];
    for line in assembly.lines() {
        let code = line.split(';').next().unwrap();
        if line.starts_with("; _L") || code.trim_end().ends_with(':') {
            if !tables.last().unwrap().is_empty() {
                tables.push(Vec::new());
            }
        } else if code.starts_with("         ") && !code.trim().is_empty() {
            tables.last_mut().unwrap().push(code.trim().to_string());
        }
    }
    tables.retain(|table| !table.is_empty());
    tables
}

#[test]
fn final_compression_can_delete_the_end_row() {
    // vcl 1.4beta7 puts the E bit on the row walked second to last; its
    // final compression removes that empty row and its delay slot, so the
    // program has no E bit and the VU runs past its end.
    let listing = compile_probe("external-vf-two", &[]);
    assert!(!listing.contains("[E]"), "{listing}");
    let error = execute_from_entry(&listing).unwrap_err();
    assert!(
        error.contains("left program before E termination"),
        "{error}"
    );

    // A `--cont` stream loses its E bit the same way, so the program runs on
    // into the next stream instead of stopping there.
    let listing = compile_probe("end-cont-lost", &[]);
    let streams = tables(&listing);
    assert_eq!(streams.len(), 2, "{listing}");
    assert!(
        !streams[0].iter().any(|row| row.contains("[E]")),
        "{listing}"
    );
    let execution = execute_from_entry(&listing).unwrap();
    assert_eq!(execution.steps, 7, "stopped only at the final exit");
    assert_ne!(execution.state.vf[4], [0; 4], "ran the continuation");
}

#[test]
fn the_end_fix_keeps_an_end_row_and_its_delay_slot() {
    let unfixed = compile_probe("end-cont-lost", &[]);
    for flags in [["--fix=end"], ["--fix=all"], ["--mode=oxide"]] {
        // The exit stops where it should, with every result written: the
        // last instruction row takes the E bit, and an empty delay row
        // follows it.
        let listing = compile_probe("external-vf-two", &flags);
        let last = tables(&listing).pop().unwrap();
        assert!(
            last[last.len() - 2].starts_with("add[E]"),
            "{flags:?}: {listing}"
        );
        assert_eq!(
            last[last.len() - 1].split_whitespace().collect::<Vec<_>>(),
            ["NOP", "NOP"]
        );
        let execution = execute_from_entry(&listing).unwrap();
        assert_eq!(execution.state.vf[4], square_plus_double(), "{flags:?}");

        // The `--cont` stream stops at its own E row, its third instruction,
        // before the continuation runs.
        let listing = compile_probe("end-cont-lost", &flags);
        let rows: Vec<String> = tables(&listing).concat();
        let end = rows.iter().position(|row| row.contains("[E]")).unwrap();
        assert_eq!(end, 2, "{flags:?}: {listing}");
        assert_eq!(
            rows[end + 1].split_whitespace().collect::<Vec<_>>(),
            ["NOP", "NOP"]
        );
        let execution = execute_from_entry(&listing).unwrap();
        assert_eq!(execution.steps, end + 2, "{flags:?}");
        assert_eq!(
            execution.state.vf[4], [0; 4],
            "{flags:?}: ran the continuation"
        );
        // The final exit kept its E row, so it is unchanged.
        let unfixed_rows = tables(&unfixed).concat();
        assert_eq!(rows[end + 2..], unfixed_rows[3..], "{flags:?}");
    }
}

#[test]
fn the_end_fix_respects_disabled_end_bits() {
    // `-e` asks for no E bit at all, so the fix adds nothing.
    let disabled = compile_probe("external-vf-two", &["-e"]);
    assert_eq!(
        compile_probe("external-vf-two", &["-e", "--fix=end"]),
        disabled
    );
    assert!(!disabled.contains("[E]"));
}

// ----------------------------------------------
// F3: clip history counted as read (`--fix=clip`)
// ----------------------------------------------

/// A program that loads `fA` from qword 1, runs `body`, and stores VI01 to
/// qword 2, with `inputs` in its `--enter` block.
fn clip_program(inputs: &str, body: &str) -> String {
    format!(
        ".init_vi_all\n\
         .init_vf_all\n\
         .syntax new\n\
         --enter\n\
         {inputs}\n\
         --endenter\n\
         lq fA, 1(vi00)\n\
         {body}\n\
         isw.x vi01, 2(vi00)\n\
         --exit\n\
         --endexit\n"
    )
}

fn compile_with(source: &str, flags: &[&str]) -> Result<String, String> {
    let flags = ["-t1", "-uCLIP"].iter().chain(flags);
    let options = cli::parse(flags.copied()).unwrap().compile;
    vcl::compile(source, "/work/clip.vcl", &options)
        .map(|compiled| compiled.assembly)
        .map_err(|errors| format!("{errors:?}"))
}

/// Programs whose clip-flag reads see only judgments their own CLIPWs
/// made, on every path: the `clip` fix accepts them. (FCSET before the
/// first CLIPW defines the whole history, so such a program needs no fix.)
const UNOBSERVED_HISTORY: [&str; 6] = [
    // The current judgment only.
    "clipw.xyz fA, fA[w]\nfcand vi01, 0x3f",
    // FCOR tests the bits its mask clears: here the current judgment.
    "clipw.xyz fA, fA[w]\nfcor vi01, 0xffffc0",
    // Two judgments for two CLIPWs; FCGET reads both.
    "clipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\nfcand vi01, 0xfff",
    "clipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\nfcget vi01",
    // Four CLIPWs define the whole flag (the quad ADC pattern).
    "clipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\n\
     fcand vi01, 0xffffff",
    "clipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\n\
     fceq vi01, 0",
];

/// Programs that read a judgment no CLIPW made on some path: they stay
/// rejected with the fix.
const OBSERVED_HISTORY: [&str; 6] = [
    "clipw.xyz fA, fA[w]\nfcand vi01, 0xffffff",
    "clipw.xyz fA, fA[w]\nfcand vi01, 0x40",
    // The manual's FCOR example tests three judgments.
    "clipw.xyz fA, fA[w]\nfcor vi01, 0xffdf7d",
    "clipw.xyz fA, fA[w]\nfceq vi01, 0",
    "clipw.xyz fA, fA[w]\nfcget vi01",
    // Three CLIPWs on one path, one on the other.
    "ilw.x iFmt, 0(vi00)\nibeq iFmt, vi00, lOne\nclipw.xyz fA, fA[w]\nclipw.xyz fA, fA[w]\n\
     lOne:\nclipw.xyz fA, fA[w]\nfcand vi01, 0xfff",
];

#[test]
fn undeclared_clip_history_is_rejected_even_when_unobserved() {
    for body in UNOBSERVED_HISTORY.iter().chain(&OBSERVED_HISTORY) {
        let error = compile_with(&clip_program("", body), &[]).unwrap_err();
        assert!(error.contains("CLIPA"), "{body}: {error}");
        // Declared, the same program compiles.
        compile_with(&clip_program("in_hw_clip CLIP", body), &[]).unwrap();
    }
}

#[test]
fn the_clip_fix_declares_history_that_nothing_observes() {
    for flags in [["--fix=clip"], ["--fix=all"], ["--mode=oxide"]] {
        for body in UNOBSERVED_HISTORY {
            // The program schedules exactly as it does with the history
            // declared.
            let fixed = compile_with(&clip_program("", body), &flags);
            let declared = compile_with(&clip_program("in_hw_clip CLIP", body), &flags);
            assert_eq!(fixed, declared, "{flags:?}: {body}");
            assert!(fixed.is_ok(), "{flags:?}: {body}");
        }
        for body in OBSERVED_HISTORY {
            let error = compile_with(&clip_program("", body), &flags).unwrap_err();
            assert!(error.contains("CLIP"), "{flags:?}: {body}: {error}");
        }
    }
    // Other fixes leave it alone.
    let error = compile_with(&clip_program("", UNOBSERVED_HISTORY[0]), &["--fix=end"]);
    assert!(error.unwrap_err().contains("CLIPA"));
}

#[test]
fn the_clip_fix_compiles_the_repros_as_their_declared_versions() {
    // Each public repro, fixed, equals the original's output for the same
    // program with `in_hw_clip CLIP` declared (`tests/probes`).
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    for (repro, declared) in [
        (
            "openvcl__repros_clip_join",
            "checker-lat-clip-join-declared",
        ),
        (
            "openvcl__repros_clip_loop_entry",
            "checker-lat-clip-loop-declared",
        ),
        (
            "openvcl__test_repro_quad_adc_bug",
            "checker-lat-quad-adc-declared",
        ),
    ] {
        let input =
            std::fs::read_to_string(root.join("tests/corpus").join(repro).join("input.vcl"))
                .unwrap();
        let source = corpus::without_header(&input);
        let options = cli::parse(["-t1", "-uFIX", "--fix=clip"]).unwrap().compile;
        let fixed = vcl::compile(source, "/work/repro.vcl", &options).unwrap();
        let original =
            std::fs::read_to_string(probe_directory(declared).join("stdout.bin")).unwrap();
        assert_eq!(
            normalized(&fixed.assembly),
            normalized(&original),
            "{repro}"
        );
    }
    // The two that test only defined judgments compile too.
    for repro in [
        "openvcl__repros_clipw_implicit_w",
        "openvcl__test_repro_quad_adc_bug_fjtrujy",
    ] {
        let input =
            std::fs::read_to_string(root.join("tests/corpus").join(repro).join("input.vcl"))
                .unwrap();
        let options = cli::parse(["-t1", "-uFIX", "--fix=clip"]).unwrap().compile;
        vcl::compile(corpus::without_header(&input), "/work/repro.vcl", &options).unwrap();
    }
}

// ----------------------------------------------
// F1: immediates outside their field (`--fix=imm`)
// ----------------------------------------------

/// A one-region program with `inputs`, `body` and `outputs`.
fn region_program(inputs: &str, body: &str, outputs: &str) -> String {
    format!(
        ".init_vf_all\n\
         .init_vi_all\n\
         --enter\n\
         {inputs}\n\
         --endenter\n\
         {body}\n\
         --exit\n\
         {outputs}\n\
         --endexit\n"
    )
}

fn compiled(source: &str, flags: &[&str]) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    let flags = ["-t1", "-uIMM"].iter().chain(flags);
    let options = cli::parse(flags.copied()).unwrap().compile;
    vcl::compile(source, "/work/imm.vcl", &options)
}

/// The text of every warning and error, in vcl's wording.
fn messages(result: &Result<vcl::Compiled, Vec<vcl::CompileError>>) -> Vec<String> {
    match result {
        Ok(compiled) => compiled
            .warnings
            .iter()
            .map(|warning| warning.located(vcl::Mode::Vcl))
            .collect(),
        Err(errors) => errors
            .iter()
            .map(|error| error.diagnostic().located(vcl::Mode::Vcl))
            .collect(),
    }
}

const IMM_FLAGS: [&[&str]; 3] = [&["--fix=imm"], &["--fix=all"], &["--mode=oxide"]];

#[test]
fn the_imm_fix_rewrites_vi_steps_exactly() {
    // `checker-imm-iaddiu-source`: vcl 1.4beta7 prints 40000 and 70000,
    // which the assembler truncates to 7232 and 4464.
    let unfixed = compile_probe("checker-imm-iaddiu-source", &[]);
    assert_eq!(
        out_of_field(&unfixed),
        ["iaddiu 0x00009c40", "isubiu 0x00011170"]
    );
    for flags in IMM_FLAGS {
        let listing = compile_probe("checker-imm-iaddiu-source", flags);
        assert!(out_of_field(&listing).is_empty(), "{flags:?}: {listing}");
        let steps: Vec<_> = lowers(&listing)
            .into_iter()
            .filter(|(mnemonic, _)| mnemonic == "isubiu")
            .map(|(_, operands)| operands[2].clone())
            .collect();
        assert_eq!(steps, ["0x000063c0", "0x00001170"], "{flags:?}");
        // The 16-bit results are the source's: 100 + 40000 and 100 - 70000.
        let mut state = State::default();
        state.vi[1] = 100;
        let state = Program::parse(&listing)
            .unwrap()
            .execute(state, 64)
            .unwrap()
            .state;
        assert_eq!(state.vi[2], 40100, "{flags:?}");
        assert_eq!(
            state.vi[3],
            100u16.wrapping_sub(70000u32 as u16),
            "{flags:?}"
        );
    }

    // Steps inside the field are left as written, 0x7fff included; those
    // outside it take the other instruction.
    for (step, expected) in [
        ("iaddiu b,a,0x7fff", ("iaddiu", "0x00007fff")),
        ("isubiu b,a,5", ("isubiu", "0x00000005")),
        ("iaddiu b,a,0xffff", ("isubiu", "0x00000001")),
        ("isubiu b,a,-1", ("iaddiu", "0x00000001")),
        ("iaddiu b,a,-32767", ("isubiu", "0x00007fff")),
    ] {
        let source = region_program("in_vi a(vi01)", step, "out_vi b(vi02)");
        let listing = compiled(&source, &["--fix=imm"]).unwrap().assembly;
        let (mnemonic, operands) = lowers(&listing)
            .into_iter()
            .find(|(mnemonic, _)| mnemonic.starts_with("i"))
            .unwrap();
        assert_eq!(
            (mnemonic.as_str(), operands[2].as_str()),
            expected,
            "{step}"
        );
    }

    // Neither instruction adds 0x8000 in one step.
    for step in ["iaddiu b,a,0x8000", "isubiu b,a,32768", "iaddiu b,a,-32768"] {
        let source = region_program("in_vi a(vi01)", step, "out_vi b(vi02)");
        assert!(compiled(&source, &[]).is_ok(), "{step}");
        let result = compiled(&source, &["--fix=imm"]);
        assert!(
            messages(&result)[0].starts_with("line 6: immediate"),
            "{step}: {result:?}"
        );
    }
}

#[test]
fn the_imm_fix_rejects_offsets_and_flag_values_outside_their_fields() {
    for flags in IMM_FLAGS {
        for case in [
            "lq a,1024(p)",
            "sq a,-1025(p)",
            "ilw.x iv,2047(p)",
            "isw.x iv,-2000(p)",
            "lq a,1000+24(p)",
            "fceq vi01,0x1000000",
            "fcset 0x1000002",
            "fsset 0x1000",
            "fsset -1",
        ] {
            // The status and clip flags are set before the case reads them.
            let body = format!("lq a,0(p)\niaddiu iv,p,1\nadd a,a,a\n{case}");
            let source = region_program(
                "in_vi p(vi02)\nin_hw_clip CLIP",
                &body,
                "out_vf a(vf01)\nout_vi iv(vi03)",
            );
            assert!(compiled(&source, &[]).is_ok(), "{case}");
            let result = compiled(&source, flags);
            assert!(result.is_err(), "{flags:?}: {case}");
            let message = &messages(&result)[0];
            assert!(
                message.starts_with("line 10: immediate"),
                "{case}: {message}"
            );
        }
        // The public repro: `isw.x iVal, 2000(vi00)`.
        let input = std::fs::read_to_string(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .join("tests/corpus/openvcl__repros_immediates_out_of_range/input.vcl"),
        )
        .unwrap();
        let result = compiled(corpus::without_header(&input), flags);
        assert!(
            messages(&result)[0].contains("immediate 2000 of isw"),
            "{result:?}"
        );
    }
}

#[test]
fn the_imm_fix_warns_about_masks_and_symbols_it_cannot_check() {
    // Bits past a flag do not exist, so FCAND, FCOR, FSAND and FSOR test the
    // same thing without them: a warning, and the listing keeps the bits
    // that exist, as the assembler would.
    let source = region_program(
        "in_vf a(vf01)\nin_hw_clip CLIP",
        "fcand vi01,0x1000001\nadd vf02,a,a\nfsand vi02,0x1001",
        "out_vi vi01\nout_vi vi02\nout_vf vf02",
    );
    let unfixed = compiled(&source, &[]).unwrap();
    assert!(unfixed.warnings.is_empty());
    assert_eq!(
        out_of_field(&unfixed.assembly),
        ["fcand 16777217", "fsand 0x00001001"]
    );
    let result = compiled(&source, &["--fix=imm"]);
    assert_eq!(
        messages(&result),
        [
            "line 7: fcand mask 0x1000001 has bits past the flag's 24 bits; they are ignored",
            "line 9: fsand mask 0x1001 has bits past the flag's 12 bits; they are ignored",
        ]
    );
    assert!(out_of_field(&result.unwrap().assembly).is_empty());

    // A symbolic offset or IADDI step is the assembler's to check.
    let source = region_program(
        "in_vi p(vi01)",
        "lq a,OFFSET(p)\niaddi next1,p,STEP",
        "out_vf a(vf01)\nout_vi next1(vi02)",
    );
    let result = compiled(&source, &["--fix=imm"]);
    assert_eq!(
        messages(&result),
        [
            "line 6: symbolic immediate OFFSET of lq is not checked against its 11-bit field",
            "line 7: symbolic immediate STEP of iaddi is not checked against its 5-bit field",
        ]
    );
    assert_eq!(
        result.unwrap().assembly,
        compiled(&source, &[]).unwrap().assembly
    );
}

/// Runs a repair probe: VF01 = 0.5, VF02 = 0, VI01 = 5 iterations, VI02 =
/// `pointer`, and each qword holding its own index.
fn run_repair_loop(listing: &str, pointer: u16) -> State {
    let mut state = State::default();
    state.set_vector(1, [0.5; 4]);
    state.vi[1] = 5;
    state.vi[2] = pointer;
    for (index, qword) in state.memory.iter_mut().enumerate().skip(1) {
        *qword = [float(index as f32); 4];
    }
    Program::parse(listing)
        .unwrap()
        .execute(state, 256)
        .unwrap()
        .state
}

#[test]
fn the_imm_fix_keeps_repaired_offsets_in_their_fields() {
    // Each iteration steps the pointer by 3, then loads at 1022 (or -1023
    // after a subtracting step): b sums half of each loaded index.
    for (case, pointer, offset) in [
        ("checker-imm-repair-add", 0u16.wrapping_sub(20), 1022),
        ("checker-imm-repair-subtract", 1040, -1023),
    ] {
        let step: i64 = if offset > 0 { 3 } else { -3 };
        let expected: f32 = (1..=5)
            .map(|k| (i64::from(pointer as i16) + k * step + offset) as f32 * 0.5)
            .sum();

        // vcl 1.4beta7 moves the load across the step and repairs its offset
        // to 1025 (-1026), which the assembler wraps.
        let unfixed = compile_probe(case, &[]);
        assert_eq!(out_of_field(&unfixed).len(), 2, "{case}");

        for flags in IMM_FLAGS {
            let listing = compile_probe(case, flags);
            assert!(
                out_of_field(&listing).is_empty(),
                "{case} {flags:?}: {listing}"
            );
            let offsets: BTreeSet<_> = lowers(&listing)
                .into_iter()
                .filter(|(mnemonic, _)| mnemonic == "lq")
                .map(|(_, operands)| operands[1].clone())
                .collect();
            assert_eq!(
                offsets,
                BTreeSet::from([format!("{offset}(VI02)")]),
                "{case}"
            );
            // The loop is still pipelined, and computes the source's sum.
            assert!(listing.contains("MAIN_LOOP"), "{case} {flags:?}");
            let state = run_repair_loop(&listing, pointer);
            assert_eq!(state.vf[2], [float(expected); 4], "{case} {flags:?}");
        }
    }
}

// ----------------------------------------------
// F5: program size (`--fix=size`)
// ----------------------------------------------

/// A raw program whose last branch jumps back over `rows` rows.
fn far_branch_program(rows: usize) -> String {
    let mut lines = vec![
        ".init_vi_all".to_string(),
        "--enter".into(),
        "in_vi flag(vi01)".into(),
        "--endenter".into(),
        ".vsm".into(),
        "top:".into(),
    ];
    lines.extend(std::iter::repeat_n("nop nop".to_string(), rows));
    lines.extend([
        "nop ibne flag, vi00, top".into(),
        "nop nop".into(),
        ".endvsm".into(),
        "--exit".into(),
        "--endexit".into(),
    ]);
    lines.join("\n") + "\n"
}

#[test]
fn the_size_fix_warns_only_about_programs_too_large() {
    // A branch 1025 rows back is past its offset's reach, but the program
    // counter wraps around micro memory, so it still lands: no warning.
    let far = far_branch_program(1024);
    for flags in [&["--fix=size"][..], &["--fix=all"], &["--mode=oxide"]] {
        assert_eq!(messages(&compiled(&far, flags)), Vec::<String>::new());
    }
    assert_eq!(
        compiled(&far, &[]).unwrap().assembly,
        compiled(&far, &["--fix=size"]).unwrap().assembly
    );

    // A program larger than VU1 micro memory (its exit adds padding rows).
    let source = far_branch_program(2047);
    let large = compiled(&source, &["--fix=size"]);
    let rows = large.as_ref().unwrap().instruction_pairs;
    assert!(rows > 2048);
    assert_eq!(
        messages(&large),
        [format!(
            "program is {rows} instructions; VU1 micro memory holds 2048"
        )]
    );
    assert!(compiled(&source, &[]).unwrap().warnings.is_empty());
}
