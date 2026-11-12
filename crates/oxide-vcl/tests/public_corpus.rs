//! The public corpus (`tests/corpus`, see its README): 52 programs from public
//! repositories, each with SCE vcl 1.4beta7's output. The port must match the
//! original on every one: acceptance, and the listing with comments and
//! whitespace ignored.
//!
//! Two notes on the references. Where the original timed out a loop
//! strategy (alarm scale 1), the port's step budget runs out on the same
//! strategies or loses the depth tie, so the same loop candidate wins. For
//! `general_quad` and `general_nospec_quad` the reference is the native
//! output (`native-stdout.bin`): emulated, period 47 normal1 hits the
//! watchdog and the loop lands at period 48; natively it finishes.

#[path = "support/corpus.rs"]
mod corpus;

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("tests/corpus")
}

#[test]
fn every_program_has_syntax_the_original_accepts() {
    let cases = corpus::cases(&root());
    for case in &cases {
        vcl::parser::parse(&case.source)
            .unwrap_or_else(|errors| panic!("{}: {errors:?}", case.name));
    }
    assert_eq!(cases.len(), 52);
}

#[test]
fn every_program_matches_the_original() {
    let cases = corpus::cases(&root());
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            corpus::compare(case)
                .err()
                .map(|why| format!("{}: {why}", case.name))
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
    let accepted = cases.iter().filter(|case| case.exit_code == 0).count();
    assert_eq!((accepted, cases.len() - accepted), (47, 5));
}

/// Every fix (`--fix=all`, docs/VCL_BUGS.md) changes only the programs that
/// have the defect it repairs: each other program keeps the original's
/// listing and gets no fix's warning. Both quake2-ps2 programs, which add
/// 0x8000 to VI values in two steps and keep state at the top of data
/// memory, are among the unchanged.
#[test]
fn the_fixes_change_only_programs_with_their_defects() {
    use vcl::diagnostics::CompileMessage::*;
    // In name order.
    const CHANGED: [&str; 6] = [
        // `clip`: accepted, as their declared versions.
        "openvcl__repros_clip_join",
        "openvcl__repros_clip_loop_entry",
        "openvcl__repros_clipw_implicit_w",
        // `imm`: `isw.x iVal, 2000(vi00)` is rejected.
        "openvcl__repros_immediates_out_of_range",
        // `clip`.
        "openvcl__test_repro_quad_adc_bug",
        // `clip`, and `imm` rewrites `iaddiu vi05,vi00,0xffff`.
        "openvcl__test_repro_quad_adc_bug_fjtrujy",
    ];
    let cases = corpus::cases(&root());
    let mut changed = Vec::new();
    for case in &cases {
        let arguments = case
            .arguments
            .iter()
            .map(String::as_str)
            .chain(["--fix=all"]);
        let options = vcl_cli::parse(arguments).unwrap().compile;
        let fixed = vcl::compile(&case.source, &case.path, &options);
        let same = match &fixed {
            Ok(compiled) => {
                case.exit_code == 0
                    && corpus::normalized(&compiled.assembly) == corpus::normalized(&case.expected)
                    && !compiled.warnings.iter().any(|warning| {
                        matches!(
                            &warning.message,
                            vcl::Message::Compile(
                                ImmediateOutOfField { .. }
                                    | FlagMaskTruncated { .. }
                                    | ImmediateUnchecked { .. }
                                    | ProgramTooLarge { .. }
                            )
                        )
                    })
            }
            Err(_) => case.exit_code != 0,
        };
        if !same {
            changed.push(case.name.as_str());
        }
    }
    assert_eq!(changed, CHANGED);
}

#[test]
fn memory_selector_does_not_become_a_vi_component_dependency() {
    use vcl::{
        analysis::prune,
        isa::instruction::{Direction, class},
        parser,
    };
    for lane in ["x", "y", "z", "w"] {
        let source = format!(
            "--enter\n\
             in_vi base(vi01)\n\
             --endenter\n\
             ilw.{lane} vi02,0(base)\n\
             --exit\n\
             out_vi vi02\n\
             --endexit"
        );
        let parsed = parser::parse(&source).unwrap();
        let instruction = parsed
            .statements
            .iter()
            .find_map(|statement| match &statement.kind {
                parser::StatementKind::Instruction(instruction)
                    if instruction.mnemonic == "ilw" =>
                {
                    Some(instruction)
                }
                _ => None,
            })
            .unwrap();
        let address = instruction
            .operands
            .iter()
            .find(|operand| operand.class == class::ADDRESS_REGISTER_OFFSET)
            .unwrap();
        assert_eq!(address.mask.suffix(), lane);
        let expanded = prune::expand(instruction);
        let base = expanded
            .operands
            .iter()
            .find(|operand| operand.class == class::ADDRESS_REGISTER_OFFSET)
            .unwrap();
        assert_eq!(base.mask, 1);
        assert_eq!(base.direction, Direction::Read);
    }
}
