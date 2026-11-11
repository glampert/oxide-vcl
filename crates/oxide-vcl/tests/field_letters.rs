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

// ----------------------------------------------
// New syntax
// ----------------------------------------------

/// New syntax selects fields with brackets (`m[x]`) and instruction
/// suffixes, so a name keeps its trailing letters: `caty` is `caty` on every
/// side, in both modes and without a warning (observed from vcl 1.4beta7,
/// `field-letters-new-*`).
#[test]
fn new_syntax_keeps_names_whole() {
    let body = |declaration: &str, statement: &str, output: &str| {
        format!(
            ".init_vf_all\n\
             --enter\n\
             {declaration}\n\
             --endenter\n\
             {statement}\n\
             --exit\n\
             {output}\n\
             --endexit\n"
        )
    };
    for fields in ["x", "y", "z", "w", "yz"] {
        let name = format!("cat{fields}");
        let sources = [
            body(
                &format!("in_vf {name}(vf01)"),
                &format!("add res, {name}, {name}"),
                "out_vf res(vf02)",
            ),
            body(
                "in_vf src(vf01)",
                &format!("add {name}, src, src"),
                &format!("out_vf {name}(vf02)"),
            ),
        ];
        for source in &sources {
            for mode in [Mode::Vcl, Mode::Oxide] {
                // `.syntax new` and `-n` alike.
                for (text, new_syntax) in [
                    (format!(".syntax new\n{source}"), false),
                    (source.clone(), true),
                ] {
                    let options = Options {
                        mode,
                        new_syntax,
                        ..Options::default()
                    };
                    let compiled = vcl::compile(&text, "f.vcl", &options)
                        .unwrap_or_else(|errors| panic!("{name} {mode}: {errors:?}"));
                    assert_eq!(
                        rows(&compiled.assembly)[0],
                        "add[E] VF02,VF01,VF01 NOP",
                        "{name}"
                    );
                    assert!(
                        compiled.warnings.is_empty(),
                        "{name}: {:?}",
                        compiled.warnings
                    );
                }
            }
        }
    }
    let source = ".syntax new\n.init_vf_all\n--enter\nin_vf first(vf01)\n--endenter\n\
                  move copy, first\n--exit\nout_vf copy(vf05)\n--endexit\n";
    for mode in [Mode::Vcl, Mode::Oxide] {
        let compiled = vcl::compile(source, "f.vcl", &options(mode)).unwrap();
        assert_eq!(rows(&compiled.assembly)[0], "max[E] VF05,VF01,VF01 NOP");
        assert!(compiled.warnings.is_empty());
    }
}

/// A glued broadcast is just another name in new syntax.
#[test]
fn new_syntax_glued_broadcast_is_a_name() {
    let source = ".syntax new\n.init_vf_all\n--enter\nin_vf src(vf01)\nin_vf scale(vf02)\n\
                  --endenter\nadd res, src, scalex\n--exit\nout_vf res(vf03)\n--endexit\n";
    let errors = vcl::compile(source, "f.vcl", &options(Mode::Vcl)).unwrap_err();
    assert!(
        errors[0].to_string().contains("scalex.x used before set"),
        "{errors:?}"
    );
}

/// Interface names take no dotted fields in new syntax, nor integer ones in
/// either syntax: vcl rejects the name (`field-letters-new-out-dotted`,
/// `field-letters-new-in-dotted`, `field-letters-new-in-vi-dotted`,
/// `field-letters-in-vi-dotted`). New syntax writes `out_vf.xyz res(vf02)`.
#[test]
fn dotted_interface_names_are_rejected() {
    let cases = [
        (
            ".syntax new\n.init_vf_all\n--enter\nin_vf src(vf01)\n--endenter\n\
             add.xyz res, src, src\n--exit\nout_vf res.xyz(vf02)\n--endexit\n",
            8,
            "res.xyz",
        ),
        (
            ".syntax new\n.init_vf_all\n--enter\nin_vf src.xyz(vf01)\n--endenter\n\
             add.xyz res, src, src\n--exit\nout_vf.xyz res(vf02)\n--endexit\n",
            4,
            "src.xyz",
        ),
        (
            ".syntax new\n.init_vi_all\n--enter\nin_vi cnt.x(vi01)\n--endenter\n\
             iaddiu cnt, cnt, 1\n--exit\nout_vi cnt(vi02)\n--endexit\n",
            4,
            "cnt.x",
        ),
        (
            ".init_vi_all\n--enter\nin_vi cnt.x(vi01)\n--endenter\n\
             iaddiu cnt, cnt, 1\n--exit\nout_vi cnt(vi02)\n--endexit\n",
            3,
            "cnt.x",
        ),
    ];
    for (source, line, name) in cases {
        for mode in [Mode::Vcl, Mode::Oxide] {
            let errors = vcl::compile(source, "f.vcl", &options(mode)).unwrap_err();
            assert_eq!(errors[0].line, line, "{name}");
            assert_eq!(
                errors[0].to_string(),
                format!("line {line}: can't use '.' in the name of a register : '{name}'")
            );
        }
    }
    // The new-syntax spelling compiles.
    let source = ".syntax new\n.init_vf_all\n--enter\nin_vf src(vf01)\n--endenter\n\
                  add.xyz res, src, src\n--exit\nout_vf.xyz res(vf02)\n--endexit\n";
    assert!(vcl::compile(source, "f.vcl", &options(Mode::Vcl)).is_ok());
}

/// Each syntax reads the name its own way, so a name that ends in a field
/// letter cannot cross from old to new syntax: old-syntax `caty` is `cat`,
/// new-syntax `caty` is another register (`field-letters-new-mixed-old-new`).
#[test]
fn a_name_does_not_cross_syntaxes() {
    let source = ".init_vf_all\n--enter\nin_vf src(vf01)\n--endenter\n\
                  add.xyzw caty, src, src\n.syntax new\nadd res, caty, src\n\
                  --exit\nout_vf res(vf02)\n--endexit\n";
    let errors = vcl::compile(source, "f.vcl", &options(Mode::Vcl)).unwrap_err();
    assert!(
        errors[0].to_string().contains("caty.x used before set"),
        "{errors:?}"
    );
}
