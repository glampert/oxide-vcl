//! FCAND, FCEQ and FCOR always write VI01, and allocation keeps VI01 free
//! for them: under `.init_vi_all` no variable live across one is given VI01,
//! even when every other VI register is in use (observed from vcl 1.4beta7,
//! `vi01-n14-fc`, `vi01-n15-fc`, `vi01-n14-branch`). Without such an
//! instruction VI01 is an ordinary register (`vi01-n15-nofc`). The
//! preprocessor's `--vcl-boilerplate` relies on this when it declares
//! `.init_vi_all`.

/// A program that loads `count` VI variables, runs CLIPW and, with
/// `flag_test`, FCAND, and then stores every variable: all of them are live
/// across the FCAND.
fn program(count: usize, flag_test: bool) -> String {
    let mut source = String::from(
        ".init_vi_all\n.init_vf_all\n.syntax new\n.vu\n\
         --enter\nin_hw_clip CLIP\n--endenter\n\
         lq vPos, 0(vi00)\n",
    );
    for index in 1..=count {
        source.push_str(&format!("ilw.x iVar{index}, {index}(vi00)\n"));
    }
    source.push_str("clipw.xyz vPos, vPos[w]\n");
    if flag_test {
        source.push_str("fcand vi01, 0x3f\nisw.x vi01, 100(vi00)\n");
    }
    for index in 1..=count {
        source.push_str(&format!("isw.x iVar{index}, {}(vi00)\n", index + 20));
    }
    source.push_str("--exit\n--endexit\n");
    source
}

/// The listing's instructions that name VI01, as `mnemonic operands`.
fn vi01_uses(source: &str) -> Vec<String> {
    let assembly = vcl::compile(source, "flags.vcl", &vcl::Options::default())
        .unwrap()
        .assembly;
    let mut uses = Vec::new();
    for line in assembly.lines() {
        let words: Vec<&str> = line.split(';').next().unwrap().split_whitespace().collect();
        for pair in words.windows(2) {
            if pair[1].contains("VI01") {
                uses.push(format!("{} {}", pair[0], pair[1]));
            }
        }
    }
    uses
}

#[test]
fn no_variable_live_across_fcand_is_given_vi01() {
    for count in [14, 15] {
        assert_eq!(
            vi01_uses(&program(count, true)),
            ["fcand VI01,63", "isw.x VI01,100(VI00)"],
            "{count} variables"
        );
    }
}

#[test]
fn without_a_flag_test_vi01_holds_a_variable() {
    let uses = vi01_uses(&program(15, false));
    assert!(
        uses.iter().any(|part| part.starts_with("ilw.x VI01,")),
        "{uses:?}"
    );
}
