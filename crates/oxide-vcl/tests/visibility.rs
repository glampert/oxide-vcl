//! `.show`/`.hide` scopes give symbolic names inside them their own identity
//! (observed from vcl 1.4beta7): `.hide list` makes the listed names local,
//! `.show list` every other name. The synthetic tests need no local data;
//! the last one compares the port with the original's retained runs.

#[path = "support/evidence.rs"]
mod evidence;

use std::path::Path;

fn compile(source: &str) -> Result<String, String> {
    vcl::compile(source, "scopes.vcl", &vcl::Options::default())
        .map(|compiled| compiled.assembly)
        .map_err(|errors| errors[0].message.to_string())
}

/// Instruction rows of the output, with whitespace collapsed.
fn rows(assembly: &str) -> Vec<String> {
    assembly
        .lines()
        .filter(|line| line.starts_with("         "))
        .map(|line| {
            line.split(';')
                .next()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn a_shown_block_keeps_its_temporary_apart_from_the_callers() {
    // The block's `t` is local, so the last `add` reads the caller's `t`
    // (VF03), not the block's `a + a`.
    let assembly = compile(
        ".init_vf_all\n\
         --enter\n\
         in_vf a(vf01)\n\
         in_vf t(vf03)\n\
         --endenter\n\
         .show a, b\n\
         add t, a, a\n\
         mul b, t, t\n\
         .endshow\n\
         add c, b, t\n\
         --exit\n\
         out_vf c(vf02)\n\
         --endexit\n",
    )
    .unwrap();
    let last = rows(&assembly)
        .into_iter()
        .rfind(|row| row.starts_with("add"))
        .unwrap();
    assert!(last.contains("VF03"), "{assembly}");
}

#[test]
fn a_hidden_name_is_a_new_register_inside_the_scope() {
    // The hidden `a` is `1a`, which nothing sets.
    let error = compile(
        ".init_vf_all\n\
         --enter\n\
         in_vf a(vf01)\n\
         --endenter\n\
         .hide a\n\
         add b, a, a\n\
         .endhide\n\
         --exit\n\
         out_vf b(vf02)\n\
         --endexit\n",
    )
    .unwrap_err();
    assert!(error.contains("1a.x used before set"), "{error}");
}

#[test]
fn a_local_name_does_not_exist_outside_its_scope() {
    for scope in [".hide t", ".show a"] {
        let close = if scope.starts_with(".hide") {
            ".endhide"
        } else {
            ".endshow"
        };
        let error = compile(&format!(
            ".init_vf_all\n\
             --enter\n\
             in_vf a(vf01)\n\
             --endenter\n\
             {scope}\n\
             add t, a, a\n\
             {close}\n\
             mul b, t, t\n\
             --exit\n\
             out_vf b(vf02)\n\
             --endexit\n"
        ))
        .unwrap_err();
        assert!(error.contains("t.x used before set"), "{scope}: {error}");
    }
}

#[test]
fn each_scope_gets_its_own_number() {
    // The second scope's `t` is `2t`: reading it before the second `add`
    // sets it fails, although the first scope's `t` (`1t`) was set.
    let error = compile(
        ".init_vf_all\n\
         --enter\n\
         in_vf a(vf01)\n\
         --endenter\n\
         .hide t\n\
         add t, a, a\n\
         .endhide\n\
         .hide t\n\
         mul b, t, t\n\
         .endhide\n\
         --exit\n\
         out_vf b(vf02)\n\
         --endexit\n",
    )
    .unwrap_err();
    assert!(error.contains("2t.x used before set"), "{error}");
}

#[test]
fn nested_scopes_resolve_from_the_inside_out() {
    // Inside `.show a, b` and `.hide u`, `t` belongs to the outer scope and
    // `u` to the inner one; `b` is the caller's.
    let assembly = compile(
        ".init_vf_all\n\
         --enter\n\
         in_vf a(vf01)\n\
         --endenter\n\
         .show a, b\n\
         add t, a, a\n\
         .hide u\n\
         add u, t, a\n\
         mul b, u, t\n\
         .endhide\n\
         .endshow\n\
         --exit\n\
         out_vf b(vf02)\n\
         --endexit\n",
    );
    assert!(assembly.is_ok(), "{assembly:?}");
    let error = compile(
        ".init_vf_all\n\
         --enter\n\
         in_vf a(vf01)\n\
         --endenter\n\
         .show a, b\n\
         .hide u\n\
         add u, a, a\n\
         .endhide\n\
         mul b, u, a\n\
         .endshow\n\
         --exit\n\
         out_vf b(vf02)\n\
         --endexit\n",
    )
    .unwrap_err();
    // After `.endhide`, `u` is the `.show` scope's own local again.
    assert!(error.contains("1u.x used before set"), "{error}");
}

#[test]
fn physical_underscore_and_differently_cased_names_pass_through() {
    for (hidden, input, output) in [
        (
            "vf01",
            "in_vf vf01",
            "add vf03, vf01, vf01\n\
             --exit\n\
             out_vf vf03",
        ),
        (
            "_a",
            "in_vf _a(vf01)",
            "add b, _a, _a\n\
             --exit\n\
             out_vf b(vf02)",
        ),
        (
            "T",
            "in_vf t(vf01)",
            "add b, t, t\n\
                                --exit\n\
                                out_vf b(vf02)",
        ),
    ] {
        let source = format!(
            ".init_vf_all\n\
             --enter\n\
             {input}\n\
             --endenter\n\
             .hide {hidden}\n\
             {output}\n\
             --endexit\n\
             .endhide\n"
        );
        assert!(compile(&source).is_ok(), "{hidden}: {:?}", compile(&source));
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn visibility_runs_match_the_original() {
    let root = evidence::path("oracle");
    let mut cases = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let directory = entry.unwrap().path();
        let name = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if !name.starts_with("visibility-") {
            continue;
        }
        cases += 1;
        let read = |file: &str| std::fs::read_to_string(Path::new(&directory).join(file)).unwrap();
        let source = read("input.vcl");
        let options = vcl_cli::parse(["-t1", "-uDEMO"]).unwrap().compile;
        let port = vcl::compile(&source, "/work/scope.vcl", &options);
        if read("result.json").contains("\"exit_status\": 0,") {
            let compiled = port.unwrap_or_else(|errors| panic!("{name}: {errors:?}"));
            assert_eq!(
                rows(&compiled.assembly),
                rows(&read("stdout.bin")),
                "{name}"
            );
        } else {
            assert!(port.is_err(), "{name}: accepted");
        }
    }
    assert_eq!(cases, 13);
}
