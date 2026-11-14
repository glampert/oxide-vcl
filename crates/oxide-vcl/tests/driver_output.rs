//! The driver's exact output: what reaches standard output and standard
//! error, the exit status and the files written, for notices, warnings and
//! each kind of failure. Both modes print the same diagnostics
//! (`file(line): severity: text`, the source line, a suggested fix and a
//! count); vcl mode keeps vcl 1.4beta7's exit status 255, oxide mode exits
//! with 1, or 2 for a bad command line.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
    /// Each file of the run's directory after the run, with its contents.
    files: Vec<(String, String)>,
}

impl Run {
    fn file(&self, name: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(file, _)| file == name)
            .map(|(_, text)| text.as_str())
    }
}

/// Run the driver in a fresh directory holding `files`.
fn run(name: &str, arguments: &[&str], files: &[(&str, &str)]) -> Run {
    let directory: PathBuf =
        std::env::temp_dir().join(format!("oxide-vcl-driver-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for (file, text) in files {
        std::fs::write(directory.join(file), text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_oxide-vcl"))
        .args(arguments)
        .current_dir(&directory)
        .output()
        .unwrap();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        found.push((name, std::fs::read_to_string(&path).unwrap_or_default()));
        remove(&path);
    }
    found.sort();
    let _ = std::fs::remove_dir(&directory);
    Run {
        status: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
        files: found,
    }
}

fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

const GOOD: &str = "--enter\n\
                    --endenter\n\
                    --exit\n\
                    --endexit\n";

const BAD: &str = ".init_vf_all\n\
                   --enter\n\
                   --endenter\n\
                   frob vf01\n\
                   .bogus\n\
                   --exit\n\
                   --endexit\n";

/// Reads `copy`, which old syntax reads as `cop` with field y.
const FIELD_LETTERS: &str = ".init_vf_all\n\
                             --enter\n\
                             in_vf first(vf01)\n\
                             --endenter\n\
                             move.xyzw copy, first\n\
                             --exit\n\
                             out_vf copy(vf05)\n\
                             --endexit\n";

/// The status of a failed run: vcl's 255 in vcl mode, 1 (or 2 for a bad
/// command line) in oxide mode.
fn failure_status(mode: &str, command_line: bool) -> i32 {
    match (mode, command_line) {
        ("--mode=vcl", _) => 255,
        (_, true) => 2,
        _ => 1,
    }
}

const MODES: [&str; 2] = ["--mode=vcl", "--mode=oxide"];

#[test]
fn notices_say_only_what_matters() {
    // -M and -C only confirm themselves, and print nothing.
    for mode in MODES {
        let run = run(
            "notices",
            &[mode, "-a", "-M", "-C", "good.vcl"],
            &[("good.vcl", GOOD)],
        );
        assert_eq!(run.status, 0);
        assert_eq!(
            run.stderr,
            "oxide-vcl: note: -a has no effect: what it enabled is always on\n"
        );
        assert!(run.stdout.contains(".vu"), "{}", run.stdout);
    }
}

#[test]
fn command_line_errors_name_the_problem() {
    let files = [("good.vcl", GOOD)];
    for mode in MODES {
        let bad = run("bad-arg", &[mode, "-x", "good.vcl"], &files);
        assert_eq!(bad.status, failure_status(mode, true));
        assert_eq!(bad.stdout, "");
        assert!(
            bad.stderr
                .starts_with("oxide-vcl: error: unknown option '-x'\n  = help: "),
            "{}",
            bad.stderr
        );
        let same = run("same-file", &[mode, "-ogood.vcl", "good.vcl"], &files);
        assert_eq!(same.status, failure_status(mode, true));
        assert!(
            same.stderr
                .starts_with("oxide-vcl: error: the output file (-o) is the input file\n")
        );
        assert_eq!(same.file("good.vcl"), Some(GOOD));
        let unknown = run("bad-mode", &["--mode=fast", "good.vcl"], &files);
        assert!(
            unknown
                .stderr
                .starts_with("oxide-vcl: error: unknown mode 'fast'\n"),
            "{}",
            unknown.stderr
        );
    }
    // A near miss of a long option is suggested.
    let typo = run("typo", &["--warning-as-errors", "good.vcl"], &files);
    assert_eq!(
        typo.stderr,
        "oxide-vcl: error: unknown option '--warning-as-errors'\n  \
         = help: did you mean '--warnings-as-errors'?\n"
    );
    // The mode applies wherever it is on the line, even after the error.
    let late = run(
        "late-mode",
        &["-o", "out.vsm", "--mode=oxide", "good.vcl"],
        &files,
    );
    assert_eq!(late.status, 2);
    assert!(
        late.stderr
            .starts_with("oxide-vcl: error: more than one input file\n")
    );
}

#[test]
fn compile_errors_quote_the_source() {
    let files = [("bad.vcl", BAD)];
    let expected = "bad.vcl(4): error: unknown instruction 'frob'\n   \
                    4 | frob vf01\n     \
                    | ^^^^\n\
                    bad.vcl(5): error: unknown directive '.bogus'\n   \
                    5 | .bogus\n     \
                    | ^^^^^^\n\
                    oxide-vcl: 2 errors; no output written\n";
    for mode in MODES {
        // -p is accepted and changes nothing.
        for arguments in [vec![mode, "bad.vcl"], vec![mode, "-p", "bad.vcl"]] {
            let run = run("errors", &arguments, &files);
            assert_eq!(run.status, failure_status(mode, false));
            assert_eq!(run.stdout, "");
            assert_eq!(run.stderr, expected, "{arguments:?}");
        }
    }
    // A misspelt instruction gets a suggestion; standard input is <stdin>.
    let typo = ".init_vf_all\n--enter\nin_vf src(vf01)\n--endenter\n\
                \tad res, src, src\n--exit\nout_vf res(vf02)\n--endexit\n";
    let output = Command::new(env!("CARGO_BIN_EXE_oxide-vcl"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(typo.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert_eq!(output.status.code(), Some(255));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "<stdin>(5): error: unknown instruction 'ad'\n   \
         5 |     ad res, src, src\n     \
         |     ^^\n     \
         = help: did you mean 'add'?\n\
         oxide-vcl: 1 error; no output written\n"
    );
}

#[test]
fn driver_failures_name_the_file() {
    for mode in MODES {
        let run = run("missing", &[mode, "missing.vcl"], &[]);
        assert_eq!(run.status, failure_status(mode, false));
        assert_eq!(run.stdout, "");
        assert!(
            run.stderr
                .starts_with("oxide-vcl: error: can't read input file 'missing.vcl': "),
            "{}",
            run.stderr
        );
        assert!(!run.stderr.contains("os error"), "{}", run.stderr);
    }
}

#[test]
fn loop_hint_warnings_carry_their_line() {
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi cnt(vi01)\n\
                  --endenter\n\
                  top:\n\
                  --LoopCS 0, 0\n\
                  iaddi cnt, cnt, -1\n\
                  ibne cnt, vi00, top\n\
                  --exit\n\
                  --endexit\n";
    let files = [("loop.vcl", source)];
    for mode in MODES {
        let run = run("warnings", &[mode, "-q", "loop.vcl"], &files);
        assert_eq!(run.status, 0);
        assert_eq!(
            run.stderr,
            "loop.vcl(7): warning: --LoopCS: the minimum known count (argument 1) must be at least 1\n   \
             7 | --LoopCS 0, 0\n"
        );
        assert!(run.stdout.contains(".vu"));
    }
}

/// vcl's copy detector rejects a copy into VI00 but exits with status 0
/// (`copy-reorder-constant-dest`), and so a copy into a name an EXIT binds to
/// VF00 (`copy-output-zero-binding`); oxide mode fails the run.
#[test]
fn a_copy_into_the_constant_register_fails_in_oxide_mode() {
    let raw = ".init_vf_all\n.init_vi_all\n--enter\nin_vf source(vf01)\nin_vi base(vi01)\n\
               --endenter\n.vsm reorder\nnop iaddiu vi00,base,0\n.endvsm\n--exit\n\
               out_vi base(vi01)\n--endexit\n";
    let bound = ".init_vf_all\n\
                 .init_vi_all\n\
                 --enter\n\
                 in_vf src(vf01)\n\
                 --endenter\n\
                 move res, vf00\n\
                 --exit\n\
                 out_vf res(vf00)\n\
                 --endexit\n";
    for (name, source, line) in [("raw", raw, 8), ("bound", bound, 6)] {
        let files = [("copy.vcl", source)];
        let error = format!(
            "copy.vcl({line}): error: this copy writes to the constant register (vf00 or vi00)\n"
        );
        let vcl = run(
            &format!("copy-zero-{name}"),
            &["-oout.vsm", "copy.vcl"],
            &files,
        );
        assert_eq!(vcl.status, 0, "{name}");
        assert!(vcl.stderr.starts_with(&error), "{name}: {}", vcl.stderr);
        let oxide = run(
            &format!("copy-zero-{name}-oxide"),
            &["--mode=oxide", "-oout.vsm", "copy.vcl"],
            &files,
        );
        assert_eq!(oxide.status, 1, "{name}");
        assert!(oxide.stderr.starts_with(&error), "{name}: {}", oxide.stderr);
        for run in [vcl, oxide] {
            assert_eq!(run.file("out.vsm"), None, "{name}");
        }
    }
}

#[test]
fn warnings_as_errors_fail_the_run_in_both_modes() {
    let files = [("field.vcl", FIELD_LETTERS)];
    let flags = ["--warnings-as-errors", "-oout.vsm", "-jout.h", "field.vcl"];
    // Without the flag the warning leaves the run successful.
    let plain = run("werror-off", &["-oout.vsm", "field.vcl"], &files);
    assert_eq!(plain.status, 0);
    assert!(plain.file("out.vsm").is_some());

    let vcl = run("werror", &flags, &files);
    assert_eq!(vcl.status, 255);
    assert!(
        vcl.stderr
            .starts_with("field.vcl(5): error: 'copy' is read as register 'cop'"),
        "{}",
        vcl.stderr
    );
    assert!(
        vcl.stderr
            .ends_with("oxide-vcl: 1 error; no output written\n"),
        "{}",
        vcl.stderr
    );

    // Oxide mode rejects field letters outright; a command-line warning
    // shows the promotion.
    let source = ".init_vf_all\n--enter\nin_vf first(vf01)\n--endenter\n\
                  add res, first, first\n--exit\nout_vf res(vf05)\n--endexit\n";
    let files = [("good.vcl", source)];
    let oxide = run(
        "werror-oxide",
        &[
            "--mode=oxide",
            "-P",
            "--warnings-as-errors",
            "-oout.vsm",
            "good.vcl",
        ],
        &files,
    );
    assert_eq!(oxide.status, 1);
    assert_eq!(
        oxide.stderr,
        "oxide-vcl: error: -P is deprecated and has no effect [--warnings-as-errors]\n  \
         = help: remove it from the command line\n\
         oxide-vcl: 1 error; no output written\n"
    );
    let warned = run(
        "werror-oxide-off",
        &["--mode=oxide", "-P", "-oout.vsm", "good.vcl"],
        &files,
    );
    assert_eq!(warned.status, 0);
    assert!(
        warned
            .stderr
            .starts_with("oxide-vcl: warning: -P is deprecated")
    );
    assert!(warned.file("out.vsm").is_some());

    for run in [vcl, oxide] {
        assert_eq!(run.file("out.vsm"), None);
        assert_eq!(run.file("out.h"), None);
    }
}

/// A failed run writes nothing and leaves an earlier output as it was, so
/// make never sees a new target from a failed build. Oxide mode writes the
/// listing and the header together, through temporary files it removes.
#[test]
fn a_failed_run_leaves_the_outputs_alone() {
    for (name, mode) in [("stale", "--mode=vcl"), ("stale-oxide", "--mode=oxide")] {
        let files = [("bad.vcl", BAD), ("out.vsm", "old listing\n")];
        let run = run(name, &[mode, "-oout.vsm", "-jout.h", "bad.vcl"], &files);
        assert_ne!(run.status, 0);
        let names: Vec<&str> = run.files.iter().map(|(file, _)| file.as_str()).collect();
        assert_eq!(names, ["bad.vcl", "out.vsm"], "{name}");
        assert_eq!(run.file("out.vsm"), Some("old listing\n"));
    }
    let source = ".init_vf_all\n--enter\nin_vf first(vf01)\n--endenter\n\
                  add res, first, first\n--exit\nout_vf res(vf05)\n--endexit\n";
    let good = run(
        "outputs-oxide",
        &["--mode=oxide", "-oout.vsm", "-jout.h", "good.vcl"],
        &[("good.vcl", source)],
    );
    assert_eq!(good.status, 0);
    let names: Vec<&str> = good.files.iter().map(|(file, _)| file.as_str()).collect();
    assert_eq!(names, ["good.vcl", "out.h", "out.vsm"]);
    assert!(good.file("out.vsm").unwrap().contains(".vu"));
    // A header that can't be written keeps the listing from being written.
    let unwritable = run(
        "outputs-oxide-unwritable",
        &["--mode=oxide", "-oout.vsm", "-jmissing/out.h", "good.vcl"],
        &[("good.vcl", source)],
    );
    assert_eq!(unwritable.status, 1);
    assert!(
        unwritable
            .stderr
            .starts_with("oxide-vcl: error: can't write 'missing/out.h': "),
        "{}",
        unwritable.stderr
    );
    let names: Vec<&str> = unwritable
        .files
        .iter()
        .map(|(file, _)| file.as_str())
        .collect();
    assert_eq!(names, ["good.vcl"]);
}

#[test]
fn help_names_both_modes() {
    let help = run("help", &["-h"], &[]);
    assert_eq!(help.status, 0);
    assert_eq!(help.stdout, "");
    assert!(help.stderr.contains("--mode=vcl") && help.stderr.contains("--mode=oxide"));
    assert_eq!(run("help-quiet", &["-q", "-h"], &[]).stderr, "");
}

#[test]
fn long_help_and_version() {
    let help = run("help-long", &["--help"], &[]);
    assert_eq!(help.status, 0);
    assert_eq!(help.stderr, run("help-short", &["-h"], &[]).stderr);
    for (name, flag) in [("version-short", "-v"), ("version-long", "--version")] {
        for quiet in [false, true] {
            let flags: &[&str] = if quiet { &["-q", flag] } else { &[flag] };
            let output = run(name, flags, &[]);
            assert_eq!(output.status, 0);
            assert_eq!(output.stderr, "");
            assert!(
                output
                    .stdout
                    .starts_with(&format!("oxide-vcl {}", vcl_cli::VERSION))
            );
            assert_eq!(output.stdout.lines().count(), 1);
        }
    }
}
