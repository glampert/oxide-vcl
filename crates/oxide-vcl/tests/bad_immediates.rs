//! Immediates that do not parse, or parse but cannot be evaluated. vcl
//! 1.4beta7 rejects them while decoding the instruction (observed,
//! `expr-power`, `expr-logical-or`); the port reports them while parsing,
//! on their line, saying what is wrong, in both modes.

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
        diagnostic.text(),
    )
}

#[test]
fn the_error_says_what_is_wrong() {
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
        for mode in [vcl::Mode::Vcl, vcl::Mode::Oxide] {
            let (number, message, found) = error(line, mode);
            assert_eq!(number, 7, "{line}");
            assert!(
                matches!(message, Message::Parse(ParseMessage::BadImmediate { .. })),
                "{line}: {message:?}"
            );
            assert!(found.contains(text), "{line} {mode}: {found}");
        }
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
