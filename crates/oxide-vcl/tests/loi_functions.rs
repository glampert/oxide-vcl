//! The math functions of openvcl's `loi` expressions, in oxide mode
//! (`crates/vcl/src/compiler/loi_functions.rs`). vcl 1.4beta7 rejects every
//! call (observed: `loi sin(1)` fails with "failed to parse immediate"), and
//! vcl mode still does. The values are the float bits openvcl emits for the
//! same lines.

use vcl::diagnostics::{CompileMessage, Message};
use vcl_cli as cli;

/// A program whose I register is loaded by `loi {expression}`.
fn program(expression: &str) -> String {
    format!(
        ".init_vf_all\n\
         --enter\n\
         --endenter\n\
         loi {expression}\n\
         addi fOut, vf00, i\n\
         --exit\n\
         out_vf fOut(vf01)\n\
         --endexit\n"
    )
}

fn compile(expression: &str, mode: &str) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    let options = cli::parse(["-t1", "-uLOI", &format!("--mode={mode}")])
        .unwrap()
        .compile;
    vcl::compile(&program(expression), "/work/loi.vcl", &options)
}

/// The operand of the listing's `loi`.
fn loaded(expression: &str) -> String {
    let listing = compile(expression, "oxide").unwrap().assembly;
    let line = listing
        .lines()
        .find(|line| line.contains("loi "))
        .unwrap_or_else(|| panic!("{listing}"));
    line.split("loi")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn loi_calls_take_openvcls_values_in_oxide_mode() {
    for (expression, bits) in [
        // openvcl's README example.
        ("sin(45 * (pi()/1.8e2))", "0x3f3504f3"),
        // Arguments after a comma and a space stay in the operand.
        ("pow(2, 10)", "0x44800000"),
        ("pow(2, abs(-3)) + atan2(1, 2)", "0x41076b1a"),
        ("sqrt(pow(3,2)+pow(4,2))", "0x40a00000"),
        ("pi()", "0x40490fdb"),
        ("log10(1000)", "0x40400000"),
    ] {
        assert_eq!(loaded(expression), bits, "{expression}");
    }
    // An expression without a call is what it was.
    assert_eq!(loaded("2+0.5"), "0x40200000");
}

#[test]
fn vcl_mode_still_rejects_calls() {
    for expression in ["sin(1)", "sin(45 * (pi()/1.8e2))", "pow(2, 10)"] {
        assert!(compile(expression, "vcl").is_err(), "{expression}");
    }
}

#[test]
fn bad_calls_are_errors_on_their_line() {
    for (expression, detail) in [
        ("foo(1)", "unknown function 'foo'"),
        ("SIN(1)", "unknown function 'SIN'"),
        ("sin(1, 2)", "sin takes 1 argument, not 2"),
        ("pi(1)", "pi takes 0 arguments, not 1"),
        (
            "sin(angle)",
            "a function's arguments must be numbers, and 'angle' is not",
        ),
    ] {
        let errors = compile(expression, "oxide").unwrap_err();
        let error = &errors[0];
        assert_eq!(error.line, 4, "{expression}");
        let Message::Compile(CompileMessage::LoiFunction { detail: found, .. }) =
            &error.diagnostic().message
        else {
            panic!("{expression}: {:?}", error.diagnostic().message);
        };
        assert_eq!(found, detail, "{expression}");
    }
}
