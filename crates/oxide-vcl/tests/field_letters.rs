//! Names ending in x, y, z or w. Old syntax reads a symbolic name's trailing
//! lane letters as a field selection (`copy` is register `cop` with field y),
//! in instructions and in `in_vf`/`out_vf` alike (observed from vcl
//! 1.4beta7, `field-letters-*` probes). vcl compiles such programs silently.
//! In vcl mode the port compiles them the same way and warns, since the name
//! is most likely misspelt; in oxide mode they fail to compile.

use vcl::diagnostics::{CompileMessage, Severity};
use vcl::{Message, Mode, Options};

fn options(mode: Mode) -> Options {
    Options {
        mode,
        ..Options::default()
    }
}

/// `in_vf NAME(vf01)` read by an instruction with an explicit mask.
fn input(name: &str) -> String {
    format!(
        ".init_vf_all\n\
         --enter\n\
         in_vf {name}(vf01)\n\
         --endenter\n\
         add.xyzw res, {name}, {name}\n\
         --exit\n\
         out_vf res(vf02)\n\
         --endexit\n"
    )
}

/// An instruction with an explicit mask writing NAME, read by `out_vf`.
fn output(name: &str) -> String {
    format!(
        ".init_vf_all\n\
         --enter\n\
         in_vf src(vf01)\n\
         --endenter\n\
         add.xyzw {name}, src, src\n\
         --exit\n\
         out_vf {name}(vf02)\n\
         --endexit\n"
    )
}

fn field_letters(token: &str, name: &str, fields: &str) -> CompileMessage {
    CompileMessage::FieldLettersInName {
        token: token.into(),
        name: name.into(),
        fields: fields.into(),
    }
}

/// The listing's instruction rows, whitespace collapsed.
fn rows(assembly: &str) -> Vec<String> {
    assembly
        .lines()
        .filter(|line| line.starts_with("         "))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn vcl_mode_compiles_like_vcl_and_warns_once_per_name() {
    for fields in ["x", "y", "z", "w", "yz", "xyzw"] {
        let token = format!("cat{fields}");
        for (source, line) in [(input(&token), 3), (output(&token), 5)] {
            let compiled = vcl::compile(&source, "f.vcl", &options(Mode::Vcl))
                .unwrap_or_else(|errors| panic!("{token}: {errors:?}"));
            // vcl reads every field of `cat`: a full-width add.
            assert_eq!(
                rows(&compiled.assembly),
                ["add[E] VF02,VF01,VF01 NOP", "NOP NOP"],
                "{token}"
            );
            assert_eq!(
                compiled.warnings.len(),
                1,
                "{token}: {:?}",
                compiled.warnings
            );
            let warning = &compiled.warnings[0];
            assert_eq!(warning.severity, Severity::Warning);
            assert_eq!(warning.line, line, "{token}");
            assert!(
                warning.message == field_letters(&token, "cat", fields),
                "{token}: {warning:?}"
            );
        }
    }
}

#[test]
fn vcl_mode_warning_text() {
    let compiled = vcl::compile(&output("caty"), "f.vcl", &options(Mode::Vcl)).unwrap();
    let mut reporter = vcl::diagnostics::Reporter::new(Mode::Vcl, Vec::new(), Vec::new());
    reporter.notice(&compiled.warnings[0]);
    let (_, err) = reporter.into_inner();
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "WARNING: line 5: 'caty' is read as register 'cat' with field y: old syntax takes a \
         name's trailing x, y, z and w as fields; rename it, or write 'cat.y' if the field is \
         meant\n"
    );
}

#[test]
fn a_move_destination_is_read_the_same_way() {
    let source = ".init_vf_all\n\
                  --enter\n\
                  in_vf first(vf01)\n\
                  --endenter\n\
                  move.xyzw copy, first\n\
                  --exit\n\
                  out_vf copy(vf05)\n\
                  --endexit\n";
    let compiled = vcl::compile(source, "f.vcl", &options(Mode::Vcl)).unwrap();
    // A lone vector move issues as MAX in the upper pipe, as in vcl.
    assert_eq!(rows(&compiled.assembly)[0], "max[E] VF05,VF01,VF01 NOP");
    assert!(compiled.warnings[0].message == field_letters("copy", "cop", "y"));
    let errors = vcl::compile(source, "f.vcl", &options(Mode::Oxide)).unwrap_err();
    assert!(errors[0].message == field_letters("copy", "cop", "y"));
}

#[test]
fn oxide_mode_rejects_the_name() {
    for fields in ["x", "y", "z", "w", "yz", "xyzw"] {
        let token = format!("cat{fields}");
        for (source, line) in [(input(&token), 3), (output(&token), 5)] {
            let errors = vcl::compile(&source, "f.vcl", &options(Mode::Oxide)).unwrap_err();
            assert_eq!(errors.len(), 1, "{token}");
            assert_eq!(errors[0].line, line, "{token}");
            assert!(
                errors[0].message == field_letters(&token, "cat", fields),
                "{token}: {:?}",
                errors[0]
            );
            assert_eq!(errors[0].exit_status(), 255);
        }
    }
}

/// Selections that are deliberate keep compiling in both modes, silently.
#[test]
fn deliberate_selections_are_not_flagged() {
    let cases = [
        // A broadcast selects its field with the letter.
        ".init_vf_all\n--enter\nin_vf src(vf01)\nin_vf scale(vf02)\n--endenter\n\
         mulx res, src, scalex\n--exit\nout_vf res(vf03)\n--endexit\n",
        // A dotted selection.
        ".init_vf_all\n--enter\nin_vf src(vf01)\n--endenter\n\
         add.xyz res, src, src\n--exit\nout_vf res.xyz(vf02)\n--endexit\n",
        // A physical register with fields.
        ".init_vf_all\n--enter\nin_vf src(vf01)\n--endenter\n\
         add.xyz vf03, src, src\nadd.xyz res, vf03xyz, src\n--exit\nout_vf res.xyz(vf02)\n--endexit\n",
        // A name made only of lane letters stays whole.
        ".init_vf_all\n--enter\nin_vf xyzw(vf01)\n--endenter\n\
         add res, xyzw, xyzw\n--exit\nout_vf res(vf02)\n--endexit\n",
        // Integer registers have no fields.
        ".init_vi_all\n--enter\nin_vi county(vi01)\n--endenter\n\
         iaddiu county, county, 1\n--exit\nout_vi county(vi02)\n--endexit\n",
    ];
    for source in cases {
        for mode in [Mode::Vcl, Mode::Oxide] {
            let compiled = vcl::compile(source, "f.vcl", &options(mode))
                .unwrap_or_else(|errors| panic!("{mode}: {errors:?}\n{source}"));
            assert!(
                !compiled.warnings.iter().any(|warning| matches!(
                    warning.message,
                    Message::Compile(CompileMessage::FieldLettersInName { .. })
                )),
                "{mode}: {:?}\n{source}",
                compiled.warnings
            );
        }
    }
}

/// Glued letters do not select an interface operand's fields: `out_vf
/// resxyz` reads all of `res`, which an `add.xyz` leaves partly unset (vcl
/// rejects it, `field-letters-out-glued-partial`).
#[test]
fn glued_interface_letters_keep_every_field() {
    let source = ".init_vf_all\n--enter\nin_vf src(vf01)\n--endenter\n\
                  add.xyz res, src, src\n--exit\nout_vf resxyz(vf02)\n--endexit\n";
    let errors = vcl::compile(source, "f.vcl", &options(Mode::Vcl)).unwrap_err();
    assert!(
        errors[0].to_string().contains("res.w used before set"),
        "{errors:?}"
    );
}
