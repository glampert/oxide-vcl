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
