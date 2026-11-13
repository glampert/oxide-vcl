//! Immediates that do not parse, or parse but cannot be evaluated. vcl
//! 1.4beta7 rejects them while decoding the instruction, with "failed to
//! parse immediate '...'" or "failed to evaluate tree for '...' to type
//! EXP_DATATYPE_UINT" and then "failed to decode an immediate" (observed,
//! `expr-power`, `expr-logical-or`). The port reports them while parsing, on
//! their line: vcl mode with those two lines, oxide mode with what is wrong.

use vcl::diagnostics::{Message, ParseMessage};
use vcl_cli as cli;

fn program(line: &str) -> String {
    format!(
        ".init_vi_all\n\
         .init_vf_all\n\
         --enter\n\
         in_vi a(vi01)\n\
         in_vf fA(vf01)\n\
         --endenter\n\
         {line}\n\
         --exit\n\
         out_vi b(vi02)\n\
         --endexit\n"
    )
}

/// The first error of a line, in `mode`.
fn error(line: &str, mode: vcl::Mode) -> (usize, Message, String) {
    let flag = match mode {
        vcl::Mode::Vcl => "--mode=vcl",
        vcl::Mode::Oxide => "--mode=oxide",
    };
    let options = cli::parse(["-t1", "-uIMM", flag]).unwrap().compile;
    let errors = vcl::compile(&program(line), "/work/imm.vcl", &options).unwrap_err();
    let diagnostic = errors[0].diagnostic();
    (
        errors[0].line,
        diagnostic.message.clone(),
        diagnostic.text(mode),
    )
}

#[test]
fn vcl_mode_reports_the_original_lines() {
    for (line, text) in [
        (
            "iaddiu b,a,2**3",
            "failed to evaluate tree for '2**3' to type EXP_DATATYPE_UINT\n\
             ERROR: failed to decode an immediate",
        ),
        (
            "iaddi b,a,1||0",
            "failed to evaluate tree for '1||0' to type EXP_DATATYPE_SINT",
        ),
        (
            "loi ~1.5",
            "failed to evaluate tree for '~1.5' to type EXP_DATATYPE_FLOAT",
        ),
        (
            "lq fB,2**3(a)",
            "failed to evaluate tree for '2**3' to type EXP_DATATYPE_SINT",
        ),
        (
            "iaddiu b,a,(1))",
            "failed to parse immediate '(1))'\n\
             ERROR: failed to decode an immediate",
        ),
        ("loi sin(1)", "failed to parse immediate 'sin(1)'"),
    ] {
        let (number, message, found) = error(line, vcl::Mode::Vcl);
        assert_eq!(number, 7, "{line}");
        assert!(
            matches!(message, Message::Parse(ParseMessage::BadImmediate { .. })),
            "{line}: {message:?}"
        );
        assert!(found.starts_with(text), "{line}: {found}");
    }
}

#[test]
fn oxide_mode_says_what_is_wrong() {
    for (line, text) in [
        (
            "iaddiu b,a,2**3",
            "iaddiu's immediate '2**3' cannot be computed: '**' is read but never computed",
        ),
        (
            "iaddiu b,a,3*",
            "'3*' is not a valid immediate for iaddiu: the expression ends after '*'",
        ),
        (
            "iaddiu b,a,(1))",
            "'(1))' is not a valid immediate for iaddiu: a ')' has no '(' to close",
        ),
        (
            "iaddiu b,a,(1",
            "'(1' is not a valid immediate for iaddiu: a '(' is never closed",
        ),
        (
            "iaddiu b,a,*3",
            "'*3' is not a valid immediate for iaddiu: '*' has no value on its left",
        ),
        (
            "iaddiu b,a,foo(1)",
            "a '(' follows 'foo', but a VCL expression has no function calls",
        ),
        (
            "iaddiu b,a,1/0",
            "iaddiu's immediate '1/0' cannot be computed: integer division by zero",
        ),
        (
            "loi ~1.5",
            "loi's immediate '~1.5' cannot be computed: '~' and '!' apply to integers",
        ),
        ("lq fB,2**3(a)", "lq's offset '2**3' cannot be computed"),
    ] {
        let (number, _, found) = error(line, vcl::Mode::Oxide);
        assert_eq!(number, 7, "{line}");
        assert!(found.contains(text), "{line}: {found}");
    }
}

#[test]
fn symbolic_and_valid_immediates_still_compile() {
    let options = cli::parse(["-t1", "-uIMM"]).unwrap().compile;
    for line in [
        "iaddiu b,a,BASE+4",
        "iaddiu b,a,foo**2",
        "iaddiu b,a,(1<<4)+2*3",
    ] {
        assert!(
            vcl::compile(&program(line), "/work/imm.vcl", &options).is_ok(),
            "{line}"
        );
    }
}
