//! IN/OUT base bindings across webs, against the original's observations in
//! `local/evidence/oracle/binding-*`.
//!
//! The original decoder keeps one current base binding per name.
//! A mapped `in_vi`/`out_vi` operand replaces it (warning "changing binding
//! for t from VI02 to VI03"), and an unmapped one takes it, across regions
//! too. Each interface operand keeps the base it saw, and `regweb` rejects a
//! web that reaches two different bases. So `in_vi t(vi02)`, a redefinition
//! of `t` and `out_vi t(vi03)` compile, while an unchanged value cannot move.

use std::path::{Path, PathBuf};
use vcl_cli as cli;

fn directory(case: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("local/evidence/oracle")
        .join(case)
}

fn read(case: &str, file: &str) -> String {
    String::from_utf8_lossy(&std::fs::read(directory(case).join(file)).unwrap()).into_owned()
}

/// The original's guest arguments, without the program name and input path.
fn arguments(case: &str) -> Vec<String> {
    let result = read(case, "result.json");
    let start = result.find("\"guest_argv\"").unwrap();
    let list = &result[start..];
    let list = &list[list.find('[').unwrap() + 1..list.find(']').unwrap()];
    let all: Vec<String> = list
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .collect();
    all[1..all.len() - 1].to_vec()
}

fn port(case: &str) -> Result<vcl::Compiled, String> {
    let source = read(case, "input.vcl");
    let options = cli::parse(arguments(case)).unwrap().compile;
    vcl::compile(&source, &format!("/work/{case}.vcl"), &options).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn accepted(case: &str) -> bool {
    read(case, "result.json").contains("\"exit_code\": 0,")
}

/// Comments and whitespace are presentation.
fn normalized(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            let code = line.split(';').next().unwrap();
            code.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn interface_bindings_match_the_original() {
    let root =
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("local/evidence/oracle");
    let mut count = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !name.starts_with("binding-") {
            continue;
        }
        match port(&name) {
            Ok(compiled) => {
                assert!(accepted(&name), "{name}: the original rejects it");
                assert_eq!(
                    normalized(&compiled.assembly),
                    normalized(&read(&name, "stdout.bin")),
                    "{name}"
                );
            }
            Err(error) => {
                assert!(!accepted(&name), "{name}: port rejects it: {error}");
                let stderr = read(&name, "stderr.bin");
                let reason = [
                    "can't map to different base registers",
                    "can't figure out which base register to map",
                ]
                .into_iter()
                .find(|reason| stderr.contains(reason))
                .unwrap_or_else(|| panic!("{name}: unexpected original error"));
                assert!(error.contains(reason), "{name}: {error}");
            }
        }
        count += 1;
    }
    assert_eq!(count, 16);
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn a_redefined_input_leaves_in_another_register() {
    let compiled = port("binding-redefined").unwrap();
    assert!(
        normalized(&compiled.assembly)
            .iter()
            .any(|line| line.ends_with("ilw.x VI03,10(VI01)"))
    );
    assert_eq!(
        compiled
            .warnings
            .iter()
            .map(|warning| warning.message.to_string())
            .collect::<Vec<_>>(),
        ["changing binding for t from VI02 to VI03"]
    );
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn rebinding_warns_like_the_original() {
    let compiled = port("binding-two-inputs").unwrap();
    assert_eq!(
        compiled
            .warnings
            .iter()
            .map(|warning| warning.message.to_string())
            .collect::<Vec<_>>(),
        [
            "changing binding for t from VI02 to VI03",
            "changing binding for t from VI03 to VI02",
        ]
    );
    for line in read("binding-two-inputs", "stderr.bin").lines() {
        if let Some(warning) = line.strip_prefix("WARN: ") {
            assert!(
                compiled
                    .warnings
                    .iter()
                    .any(|ours| ours.message.to_string() == warning)
            );
        }
    }
}

/// A web bound to VF00 is VF00: a write into it is removed and its reader
/// reads VF00 (observed from vcl 1.4beta7, `dead-write-zero-read`), while a
/// copy into it fails the copy detector even when another exit binds the
/// name elsewhere (`copy-output-zero-exits`).
#[test]
fn a_web_bound_to_vf00_is_vf00() {
    let read = ".init_vf_all\n\
                .init_vi_all\n\
                --enter\n\
                in_vf src(vf01)\n\
                --endenter\n\
                add res, src, src\n\
                add other, res, res\n\
                --exit\n\
                out_vf res(vf00)\n\
                out_vf other(vf02)\n\
                --endexit\n";
    let options = cli::parse(["-t1", "-uB"].map(String::from).to_vec())
        .unwrap()
        .compile;
    let compiled = vcl::compile(read, "read.vcl", &options).unwrap();
    let code: Vec<String> = compiled
        .assembly
        .lines()
        .map(|line| line.split(';').next().unwrap().split_whitespace().collect())
        .filter(|line: &String| line.starts_with("add") || line.starts_with("NOP"))
        .collect();
    assert_eq!(code[0], "add[E]VF02,VF00,VF00NOP", "{}", compiled.assembly);

    let exits = ".init_vf_all\n\
                 .init_vi_all\n\
                 --enter\n\
                 in_vi flag(vi01)\n\
                 in_vf src(vf01)\n\
                 --endenter\n\
                 ibeq flag, vi00, other\n\
                 move res, src\n\
                 --exit\n\
                 out_vf res(vf00)\n\
                 --endexit\n\
                 other:\n\
                 sub res, src, src\n\
                 --exit\n\
                 out_vf res(vf02)\n\
                 --endexit\n";
    let errors = vcl::compile(exits, "exits.vcl", &options).unwrap_err();
    assert_eq!(errors[0].exit_status(), 0);
    assert_eq!(errors[0].line, 8);
    assert!(
        errors[0]
            .message
            .to_string()
            .contains("const zero register")
    );
}
