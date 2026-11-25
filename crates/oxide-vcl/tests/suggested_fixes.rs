//! Help that suggests a fix: the program with the fix applied compiles.

use vcl::diagnostics::Wording;

/// A new-syntax program running `body`, with `declarations` in its --enter
/// block and `result` as its output.
fn program(declarations: &str, body: &str) -> String {
    format!(
        ".syntax new\n\
         .init_vf_all\n\
         .init_vi_all\n\
         --enter\n\
         {declarations}\n\
         --endenter\n\
         {body}\n\
         --exit\n\
         out_vf result(vf02)\n\
         --endexit\n"
    )
}

fn compile(source: &str) -> Result<(), Wording> {
    let options = vcl_cli::parse(["-t1", "-uDEMO"]).unwrap().compile;
    vcl::compile(source, "fix.vcl", &options)
        .map(|_| ())
        .map_err(|errors| errors[0].message.wording())
}

/// The help's suggestion: what follows its last colon.
fn suggestion(wording: &Wording) -> &str {
    let help = wording.help.as_deref().unwrap();
    help.rsplit(": ").next().unwrap()
}

#[test]
fn a_register_read_before_it_is_written_suggests_its_declaration() {
    // Each body writes all of result from the register it reads.
    let cases = [
        ("R", "rget result, r", "in_hw_r r"),
        ("Q", "addq result, vf00, q", "in_hw_q q"),
        ("P", "mfp result, p", "in_hw_p p"),
        ("I", "addi result, vf00, i", "in_hw_i i"),
        ("ACC.x", "madd result, vf00, vf00", "in_hw_acc acc"),
        ("VF03.x", "add result, vf03, vf00", "in_vf vf03"),
        ("VI03", "mfir result, vi03", "in_vi vi03"),
        (
            "CLIP",
            "fcand vi01, 1\nmfir result, vi01",
            "in_hw_clip clip",
        ),
    ];
    for (name, body, declaration) in cases {
        let wording = compile(&program("", body)).unwrap_err();
        assert_eq!(
            wording.text,
            format!("'{name}' is read before anything writes it"),
            "{body}"
        );
        assert_eq!(suggestion(&wording), declaration, "{body}");
        compile(&program(declaration, body))
            .unwrap_or_else(|wording| panic!("{body}: {wording:?}"));
    }
}

#[test]
fn mac_flags_read_first_are_not_suggested_as_an_input() {
    let wording = compile(&program("", "fmand vi01, vi00\nmfir result, vi01")).unwrap_err();
    assert_eq!(wording.text, "'MAC' is read before anything writes it");
    assert!(!wording.help.unwrap().contains("in_hw_mac"));
}

#[test]
fn a_symbolic_register_read_first_still_suggests_a_binding() {
    let wording = compile(&program("", "add result, seed, vf00")).unwrap_err();
    assert_eq!(wording.text, "'seed.x' is read before anything writes it");
    assert!(wording.help.unwrap().contains("in_vf seed(vf01)"));
}

#[test]
fn a_one_field_operand_suggests_its_spelling_per_syntax() {
    let body = "rinit r, seed.x\nrget result, r";
    let wording = compile(&program("in_vf seed(vf01)", body)).unwrap_err();
    assert_eq!(wording.text, "this operand selects one field");
    assert_eq!(suggestion(&wording), "src[x]");
    compile(&program(
        "in_vf seed(vf01)",
        "rinit r, seed[x]\nrget result, r",
    ))
    .unwrap();

    let old = ".init_vf_all\n\
               --enter\n\
               in_vf seed(vf01)\n\
               --endenter\n\
               rinit R, seed\n\
               rget result, R\n\
               --exit\n\
               out_vf result(vf02)\n\
               --endexit\n";
    let wording = compile(old).unwrap_err();
    assert_eq!(suggestion(&wording), "src.x or srcx");
    compile(&old.replace("rinit R, seed\n", "rinit R, seed.x\n")).unwrap();
    compile(&old.replace("rinit R, seed\n", "rinit R, seedx\n")).unwrap();
}
