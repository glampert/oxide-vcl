//! `-c` output: vcl prints each row twice, first as a comment with the
//! source's register names, then the row itself one space further in,
//! followed by a blank line; each walked row that is not printed is a bare
//! `; ` line (observed from vcl 1.4beta7, `comments-*` and `cli-comments`
//! probes). These tests compare the driver's standard output with vcl's line
//! by line, apart from what the port does not reproduce yet: the version
//! line (deliberately oxide-vcl's own), the loop optimizer's report, stall
//! notes and the bracketed numbers of the table comments.

use std::path::{Path, PathBuf};
use std::process::Command;

fn probe(name: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/probes")).join(name)
}

/// The recorded arguments with the input file name for a local run.
fn arguments(case: &Path) -> (Vec<String>, String) {
    let result = std::fs::read_to_string(case.join("result.json")).unwrap();
    let start = result.find("\"guest_argv\"").unwrap();
    let list = &result[start..result[start..].find(']').unwrap() + start];
    let mut arguments: Vec<String> = list
        .split('"')
        // The first entry is the program name.
        .skip(5)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let mut input = String::new();
    for argument in &mut arguments {
        if let Some(name) = argument.strip_prefix("/work/") {
            input = name.to_string();
            *argument = name.to_string();
        }
    }
    (arguments, input)
}

/// Run the driver on a probe's input with its recorded arguments.
fn run(case: &Path, mode: Option<&str>) -> String {
    let (arguments, input) = arguments(case);
    let directory = std::env::temp_dir().join(format!(
        "oxide-vcl-comments-{}-{}-{}",
        std::process::id(),
        case.file_name().unwrap().to_string_lossy(),
        mode.unwrap_or("default")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::copy(case.join("input.vcl"), directory.join(&input)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_oxide-vcl"))
        .args(mode)
        .args(&arguments)
        .current_dir(&directory)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(directory.join(&input));
    let _ = std::fs::remove_dir(&directory);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The listing without what the port does not reproduce yet.
fn comparable(listing: &str) -> Vec<String> {
    listing
        // vcl compiled `/work/NAME.vcl`, which names unlabelled streams.
        .replace("__v__work_", "__v_")
        .lines()
        .filter(|line| !line.starts_with("; flowMon::Emit() "))
        .filter(|line| !(line.starts_with("; === ") && !line.starts_with("; ====")))
        .map(|line| {
            let line = line.split("       ;\tSTALL_").next().unwrap();
            if !line.starts_with("; _L") {
                return line.to_string();
            }
            // `; _LNOPT_w=[ strategy ] printed [a b] walked   [name]`: blank `a b`.
            let strategy = line.find(']').unwrap() + 1;
            let open = strategy + line[strategy..].find('[').unwrap();
            let close = open + line[open..].find(']').unwrap();
            format!("{}[a b]{}", &line[..open], &line[close + 1..])
        })
        .collect()
}

fn assert_matches_vcl(name: &str) {
    let case = probe(name);
    let expected = comparable(&std::fs::read_to_string(case.join("stdout.bin")).unwrap());
    let actual = comparable(&run(&case, None));
    for (line, (want, got)) in expected.iter().zip(&actual).enumerate() {
        assert_eq!(got, want, "{name}: line {}", line + 1);
    }
    assert_eq!(actual.len(), expected.len(), "{name}: line count");
}

#[test]
fn a_single_pair_and_its_padding_row() {
    assert_matches_vcl("cli-comments");
}

#[test]
fn compressed_rows_print_as_bare_comment_lines() {
    assert_matches_vcl("comments-pairs");
    let listing = run(&probe("comments-pairs"), None);
    assert_eq!(listing.lines().filter(|line| *line == "; ").count(), 9);
}

#[test]
fn field_selections_follow_the_source_name() {
    assert_matches_vcl("comments-fields");
    let listing = run(&probe("comments-fields"), None);
    assert!(listing.contains("mulax         ACC,mat0,vecx"), "{listing}");
    assert!(listing.contains("div           Q,VF00w,resw"), "{listing}");
}

#[test]
fn an_exit_padding_stream_keeps_the_plain_layout() {
    assert_matches_vcl("comments-branch");
    let listing = run(&probe("comments-branch"), None);
    let after = listing.split("skip:\n").nth(1).unwrap();
    assert!(after.starts_with("         NOP[E]"), "{after}");
}

#[test]
fn pipelined_loop_parts_name_source_registers() {
    assert_matches_vcl("comments-loop");
    let listing = run(&probe("comments-loop"), None);
    assert!(!listing.contains("__web_"), "{listing}");
    assert!(
        listing.contains("ibne          count,VI00,EXPL_C_loop__MAIN_LOOP"),
        "{listing}"
    );
}

/// Raw code is commented with the registers the source names, before
/// reassignment. (The port assigns this raw block different registers
/// from vcl's, a known difference, so only the comment copies compare.)
#[test]
fn raw_code_comments_show_its_own_registers() {
    let case = probe("comments-raw");
    let copies = |listing: &str| -> Vec<String> {
        listing
            .lines()
            .filter(|line| line.starts_with(";         "))
            .map(|line| line.split("       ;\tSTALL_").next().unwrap().to_string())
            .collect()
    };
    let expected = copies(&std::fs::read_to_string(case.join("stdout.bin")).unwrap());
    assert_eq!(copies(&run(&case, None)), expected);
    assert!(expected[0].contains("add           VF03,VF00,VF00"));
}

#[test]
fn oxide_mode_keeps_its_own_comment_layout() {
    let listing = run(&probe("comments-pairs"), Some("--mode=oxide"));
    assert!(listing.contains("; source line 7\n"), "{listing}");
    assert!(
        !listing.contains("_LNOPT_") && !listing.contains("VCL CODE"),
        "{listing}"
    );
}
