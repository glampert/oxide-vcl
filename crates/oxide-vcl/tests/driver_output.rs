//! The driver's exact output: what reaches standard output and standard
//! error, and the exit status, for notices, warnings and each kind of
//! failure. vcl mode keeps the port's layout of vcl 1.4beta7's messages;
//! oxide mode prints the same until it has wordings of its own.

use std::path::PathBuf;
use std::process::Command;

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
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
    for (file, _) in files {
        let _ = std::fs::remove_file(directory.join(file));
    }
    let _ = std::fs::remove_dir(&directory);
    Run {
        status: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
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

const ABORT: &str = "ERROR: something went wrong, vcl's outta here.\n";

/// Each case in both modes, which print the same today.
fn both_modes(name: &str, arguments: &[&str], files: &[(&str, &str)], check: impl Fn(&Run)) {
    check(&run(name, arguments, files));
    let oxide: Vec<&str> = std::iter::once("--mode=oxide")
        .chain(arguments.iter().copied())
        .collect();
    check(&run(&format!("{name}-oxide"), &oxide, files));
}

#[test]
fn notices_print_in_argument_order() {
    both_modes(
        "notices",
        &["-a", "-M", "-C", "good.vcl"],
        &[("good.vcl", GOOD)],
        |run| {
            assert_eq!(run.status, 0);
            assert_eq!(
                run.stderr,
                "INFO: -a option is now on always\n\
             WARNING: timid memory access mode enabled\n\
             INFO: disabling code size reduction pass..\n"
            );
            assert!(run.stdout.contains(".vu"), "{}", run.stdout);
        },
    );
}

#[test]
fn command_line_errors_end_with_the_abort_line() {
    both_modes(
        "bad-arg",
        &["-M", "-x", "good.vcl"],
        &[("good.vcl", GOOD)],
        |run| {
            assert_eq!(run.status, 255);
            assert_eq!(
                run.stderr,
                "WARNING: timid memory access mode enabled\n\
             ERROR: bad args '-x', try -h for help\n\n"
            );
            assert_eq!(run.stdout, ABORT);
        },
    );
    both_modes(
        "same-file",
        &["-ogood.vcl", "good.vcl"],
        &[("good.vcl", GOOD)],
        |run| {
            assert_eq!(run.status, 255);
            assert_eq!(run.stderr, "can't have infile==outfile\n\n");
            assert_eq!(run.stdout, ABORT);
        },
    );
}

#[test]
fn an_unknown_mode_is_an_unknown_switch() {
    let run = run(
        "bad-mode",
        &["--mode=fast", "good.vcl"],
        &[("good.vcl", GOOD)],
    );
    assert_eq!(run.status, 255);
    assert_eq!(
        run.stderr,
        "ERROR: bad args '--mode=fast', try -h for help\n\n"
    );
    assert_eq!(run.stdout, ABORT);
}

#[test]
fn compile_errors_share_one_prefix() {
    both_modes("errors", &["bad.vcl"], &[("bad.vcl", BAD)], |run| {
        assert_eq!(run.status, 255);
        assert_eq!(
            run.stderr,
            "ERROR: line 4: unknown instruction 'frob'\n\
             line 5: unknown directive '.bogus'\n"
        );
        assert_eq!(run.stdout, ABORT);
    });
    both_modes(
        "errors-ms",
        &["-p", "bad.vcl"],
        &[("bad.vcl", BAD)],
        |run| {
            assert_eq!(
                run.stderr,
                "ERROR: bad.vcl(4): error: unknown instruction 'frob'\n\
             bad.vcl(5): error: unknown directive '.bogus'\n"
            );
        },
    );
}

#[test]
fn driver_failures_name_the_file() {
    both_modes("missing", &["missing.vcl"], &[], |run| {
        assert_eq!(run.status, 255);
        assert!(
            run.stderr
                .starts_with("ERROR: failed to open file 'missing.vcl': "),
            "{}",
            run.stderr
        );
        assert_eq!(run.stdout, ABORT);
    });
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
    both_modes(
        "warnings",
        &["-q", "loop.vcl"],
        &[("loop.vcl", source)],
        |run| {
            assert_eq!(run.status, 0);
            assert_eq!(
                run.stderr,
                "WARNING: line 7: for --LoopCS, the minimum known count (arg 1) must be must be >0\n"
            );
        },
    );
}

#[test]
fn help_names_both_modes() {
    let help = run("help", &["-h"], &[]);
    assert_eq!(help.status, 0);
    assert_eq!(help.stdout, "");
    assert!(help.stderr.contains("--mode=vcl") && help.stderr.contains("--mode=oxide"));
    assert_eq!(run("help-quiet", &["-q", "-h"], &[]).stderr, "");
}
