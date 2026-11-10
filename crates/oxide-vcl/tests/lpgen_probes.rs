//! Normal loops scheduled by the lpgen port against saved output from the
//! supplied ELF (`local/evidence/lpgen/<probe>`). Comments and whitespace are
//! ignored; labels, pairs, registers and modifiers must match.

use std::path::Path;
use vcl_cli as cli;

fn normalized(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            line.split(';')
                .next()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn probe(name: &str, flags: &[&str]) {
    let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("local/evidence/lpgen")
        .join(name);
    let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
    let expected = std::fs::read_to_string(directory.join("stdout.bin")).unwrap();
    let options = cli::parse(flags.iter().copied()).unwrap().compile;
    let compiled = vcl::compile(&source, &format!("{name}.vcl"), &options)
        .unwrap_or_else(|errors| panic!("{name}: unexpectedly rejected: {errors:?}"));
    assert_eq!(
        normalized(&compiled.assembly),
        normalized(&expected),
        "{name}: output differs"
    );
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn counted_vector_loop() {
    probe("loop-vector-counted", &["-t1", "-uLOOPBUILD"]);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn add_stream() {
    probe("lpgen-add-stream", &["-t1", "-uLPGEN"]);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn quad_increment_loads_and_stores() {
    probe("lpgen-lqi-sqi", &["-t1", "-uLPGEN"]);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn transform_with_stalled_prologue() {
    probe("lpgen-transform", &["-t1", "-uLPGEN"]);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn two_loads_with_synthesized_move() {
    probe("lpgen-two-loads", &["-t1", "-uLPGEN"]);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn safe_hints_with_part_local_registers() {
    probe("lpgen-safe-hints", &["-t1", "-uLPGEN"]);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn chain_with_branches_to_exit_point() {
    probe("lpgen-chain", &["-t1", "-uLPGEN"]);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn project_with_divide_in_flight_across_parts() {
    probe("lpgen-project", &["-t1", "-uLPGEN"]);
}
