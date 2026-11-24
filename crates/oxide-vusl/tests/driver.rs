//! The `oxide-vusl` binary end to end: files in, output and diagnostics out,
//! with the exit statuses.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// One run of the driver.
struct Run {
    status: i32,
    stdout: String,
    stderr: String,
    /// The files in the directory afterwards, by name.
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

/// Runs the driver in a fresh directory holding `files`, with `stdin` on
/// its standard input.
fn run_with_input(name: &str, arguments: &[&str], files: &[(&str, &str)], stdin: &str) -> Run {
    let directory: PathBuf =
        std::env::temp_dir().join(format!("oxide-vusl-driver-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for (file, text) in files {
        std::fs::write(directory.join(file), text).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_oxide-vusl"))
        .args(arguments)
        .current_dir(&directory)
        .env_remove("NO_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        found.push((name, std::fs::read_to_string(&path).unwrap_or_default()));
        let _ = std::fs::remove_file(&path);
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

fn run(name: &str, arguments: &[&str], files: &[(&str, &str)]) -> Run {
    run_with_input(name, arguments, files, "")
}

const FRAME: &str = "#pragma once\n\
                     struct Frame {\n\
                     \x20   vec4 gsScale; // qword 0\n\
                     };\n";

const PROGRAM: &str = "#pragma program(particles)\n\
                       #include \"frame.vush\"\n\
                       #include \"frame.vush\"\n\
                       void main(layout(address = 0) uniform Frame frame)\n\
                       {\n\
                       \x20   #pragma unroll(COUNT)\n\
                       \x20   for (int16 r = 0; r < COUNT; ++r) { }\n\
                       }\n";

const PREPROCESSED: &str = "#pragma program(particles)\n\
                            struct Frame {\n\
                            \x20   vec4 gsScale;\n\
                            };\n\
                            void main(layout(address = 0) uniform Frame frame)\n\
                            {\n\
                            \x20   #pragma unroll(4)\n\
                            \x20   for (int16 r = 0; r < 4; ++r) { }\n\
                            }\n";

#[test]
fn preprocess_only_writes_the_preprocessed_program() {
    let files = [("p.vusl", PROGRAM), ("frame.vush", FRAME)];
    let result = run(
        "stdout",
        &["--preprocess-only", "-DCOUNT=4", "p.vusl"],
        &files,
    );
    assert_eq!((result.status, result.stderr.as_str()), (0, ""));
    assert_eq!(result.stdout, PREPROCESSED);

    let result = run(
        "file",
        &["--preprocess-only", "-D", "COUNT=4", "-o", "p.i", "p.vusl"],
        &files,
    );
    assert_eq!((result.status, result.stderr.as_str()), (0, ""));
    assert_eq!(result.stdout, "");
    assert_eq!(result.file("p.i"), Some(PREPROCESSED));
}

#[test]
fn standard_input_is_read_for_a_dash() {
    let result = run_with_input("stdin", &["--preprocess-only", "-"], &[], "a = b;\n");
    assert_eq!((result.status, result.stderr.as_str()), (0, ""));
    assert_eq!(result.stdout, "a = b;\n");
}

#[test]
fn preprocessor_errors_name_the_line_and_fail_the_run() {
    let result = run(
        "error",
        &["--preprocess-only", "-o", "p.i", "p.vusl"],
        &[("p.vusl", "a;\n#vuprog x\n")],
    );
    assert_eq!(result.status, 1);
    assert_eq!(
        result.stderr,
        "p.vusl(2): error: unknown preprocessor directive '#vuprog'\n   \
         2 | #vuprog x\n     \
         |  ^\n\
         oxide-vusl: 1 error; no output written\n"
    );
    assert_eq!(result.file("p.i"), None);
}

#[test]
fn warnings_print_and_fail_the_run_only_when_asked() {
    let files = [("p.vusl", "#warning careful\na;\n")];
    let result = run("warning", &["--preprocess-only", "p.vusl"], &files);
    assert_eq!(result.status, 0);
    assert_eq!(result.stdout, "a;\n");
    assert!(
        result
            .stderr
            .starts_with("p.vusl(1): warning: #warning careful\n")
    );

    let result = run(
        "warnings-as-errors",
        &[
            "--preprocess-only",
            "--warnings-as-errors",
            "-o",
            "p.i",
            "p.vusl",
        ],
        &files,
    );
    assert_eq!(result.status, 1);
    assert!(
        result
            .stderr
            .starts_with("p.vusl(1): error: #warning careful [--warnings-as-errors]\n")
    );
    assert!(
        result
            .stderr
            .ends_with("oxide-vusl: 1 error; no output written\n")
    );
    assert_eq!(result.file("p.i"), None);
}

#[test]
fn programs_check_and_compile() {
    let program = "struct Block { vec4 a; vec4 b; };\n\
                   void main(layout(address = 0) inout Block block)\n\
                   {\n\
                   \x20   block.b = block.a * 2.0;\n\
                   }\n";
    let files = [("p.vusl", program)];
    let result = run("check", &["--check", "p.vusl"], &files);
    assert_eq!((result.status, result.stderr.as_str()), (0, ""));
    assert_eq!(result.stdout, "");

    // The listing to standard output, the generated VCL to a file.
    let result = run("compile", &["--emit-vcl=p.vcl", "p.vusl"], &files);
    assert_eq!((result.status, result.stderr.as_str()), (0, ""));
    assert!(result.stdout.contains("p_CodeStart:"), "{}", result.stdout);
    let vcl = result.file("p.vcl").unwrap();
    assert!(
        vcl.contains(".name p\n") && vcl.contains("        loi        2.0\n"),
        "{vcl}"
    );

    // --stop-at-vcl writes the VCL instead, and -o takes the listing.
    let result = run("stop", &["--stop-at-vcl", "p.vusl"], &files);
    assert_eq!(result.status, 0);
    assert!(
        result.stdout.starts_with("; generated by oxide-vusl"),
        "{}",
        result.stdout
    );
    let result = run("output", &["-o", "p.vsm", "--header=p.h", "p.vusl"], &files);
    assert_eq!((result.status, result.stdout.as_str()), (0, ""));
    assert!(result.file("p.vsm").unwrap().contains("p_CodeEnd:"));
    assert!(result.file("p.h").unwrap().contains("kInstructions = "));
}

#[test]
fn optimizations_are_chosen_on_the_command_line() {
    let program = |pragma: &str| {
        format!(
            "struct Batch {{\n\
             \x20   layout(lane = w) int16 count;\n\
             \x20   vec4 scale;\n\
             \x20   vec4 items[count];\n\
             }};\n\
             void main(layout(address = 0) inout Batch batch)\n\
             {{\n\
             {pragma}\
             \x20   for (inout vec4 item : batch.items)\n\
             \x20       item = item * batch.scale;\n\
             }}\n"
        )
    };
    let pipelined = |name: &str, arguments: &[&str], pragma: &str| {
        let source = program(pragma);
        let result = run(name, arguments, &[("p.vusl", source.as_str())]);
        assert_eq!((result.status, result.stderr.as_str()), (0, ""), "{name}");
        result.stdout.contains("__MAIN_LOOP")
    };
    // oxide-vcl's loops optimization pipelines a loop without a pragma...
    assert!(pipelined("opt-default", &["p.vusl"], ""));
    assert!(!pipelined(
        "opt-no-loops",
        &["--no-opt=loops", "p.vusl"],
        ""
    ));
    assert!(!pipelined("opt-none", &["--opt=none", "p.vusl"], ""));
    // ...and a pipeline pragma asks for it whatever the options say.
    let pragma = "    #pragma pipeline(min = 1)\n";
    assert!(pipelined(
        "opt-pragma",
        &["--no-opt=loops", "p.vusl"],
        pragma
    ));

    let result = run("opt-vcl", &["--mode=vcl", "--opt=all", "p.vusl"], &[]);
    assert_eq!(result.status, 2);
    assert_eq!(
        result.stderr,
        "oxide-vusl: error: --opt=all applies only in oxide mode\n  \
         = help: drop --mode=vcl, which compiles the generated VCL without optimizations\n"
    );
}

#[test]
fn check_errors_name_the_line_in_the_included_file() {
    let files = [
        ("p.vusl", "#include \"lib.vush\"\nvoid main() {}\n"),
        ("lib.vush", "struct Block {\n    int count;\n};\n"),
    ];
    let result = run("check-error", &["--check", "p.vusl"], &files);
    assert_eq!(result.status, 1);
    assert_eq!(
        result.stderr,
        "lib.vush(2): error: unknown type 'int'\n   \
         2 |     int count;\n     \
         |     ^^^\n     \
         = help: VU integers are 16 bits: write int16\n\
         oxide-vusl: 1 error; no output written\n"
    );
}

#[test]
fn files_the_driver_cannot_read_fail_the_run() {
    let result = run("missing", &["missing.vusl"], &[]);
    assert_eq!(result.status, 1);
    assert!(
        result
            .stderr
            .starts_with("oxide-vusl: error: can't read input file 'missing.vusl': "),
        "{}",
        result.stderr
    );
}

#[test]
fn bad_command_lines_exit_with_the_usage_status() {
    let result = run("usage", &["--frob", "p.vusl"], &[]);
    assert_eq!(result.status, 2);
    assert_eq!(
        result.stderr,
        "oxide-vusl: error: unknown option '--frob'\n  \
         = help: run 'oxide-vusl --help' for the options\n"
    );
    let result = run("colors", &["--color=always"], &[]);
    assert_eq!(result.status, 2);
    assert!(
        result
            .stderr
            .starts_with("\x1b[1moxide-vusl:\x1b[0m \x1b[1;31merror\x1b[0m: "),
        "{:?}",
        result.stderr
    );
}

#[test]
fn version_and_help() {
    let result = run("version", &["--version"], &[]);
    assert_eq!(result.status, 0);
    assert!(
        result.stdout.starts_with("oxide-vusl "),
        "{}",
        result.stdout
    );
    let result = run("help", &["--help"], &[]);
    assert_eq!(result.status, 0);
    assert!(
        result
            .stderr
            .contains("usage: oxide-vusl [options] <inputfile>")
    );
}
