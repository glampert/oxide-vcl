//! `oxide-vcl`: the command-line driver. It parses the original's command line,
//! runs the optional preprocessors, compiles with the `vcl` crate and writes
//! the results, keeping the original's diagnostics and exit statuses.
//!
//! Everything it prints goes through [`vcl::diagnostics::Reporter`], in the
//! mode `--mode=` selects (vcl when absent).

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::Command;
use vcl::diagnostics::{Diagnostic, DriverMessage, ErrorStyle, Reporter};
use vcl_cli::{self as cli, Arguments};

/// The original's preprocessing stages, in the order it runs them: the C
/// preprocessor (`-G`), gasp (`-g`) and gasp with its alternate syntax (`-g+`).
/// Each enabled stage reads the previous stage's output.
fn preprocess(
    mut source: Vec<u8>,
    arguments: &Arguments,
    reporter: &mut Reporter,
) -> Result<Vec<u8>, DriverMessage> {
    if arguments.skip_preprocessing {
        return Ok(source);
    }
    let stages: [(bool, &str, &[&str]); 3] = [
        (
            arguments.preprocess_cpp,
            "gcc",
            &["-P", "-E", "-C", "-x", "assembler-with-cpp"],
        ),
        (arguments.preprocess_gasp, "gasp", &["-p", "-s", "-c", ";"]),
        (
            arguments.preprocess_gasp_alternate,
            "gasp",
            &["-p", "-a", "-s", "-c", ";"],
        ),
    ];
    for (stage, (enabled, program, switches)) in stages.into_iter().enumerate() {
        if enabled {
            source = run_stage(stage, program, switches, &source, arguments, reporter)?;
        }
    }
    Ok(source)
}

/// Run one stage on a temporary copy of the source, as the original does
/// (`program switches -I... input > output`). `-K` keeps both files.
fn run_stage(
    stage: usize,
    program: &str,
    switches: &[&str],
    source: &[u8],
    arguments: &Arguments,
    reporter: &mut Reporter,
) -> Result<Vec<u8>, DriverMessage> {
    let temporary = |kind: &str| -> PathBuf {
        std::env::temp_dir().join(format!("vcl{kind}_{}_{stage}", std::process::id()))
    };
    let (input, output) = (temporary("i"), temporary("o"));
    let display = |path: &PathBuf| path.display().to_string();
    std::fs::write(&input, source).map_err(|e| DriverMessage::WriteTemporary {
        path: display(&input),
        error: e.to_string(),
    })?;
    let result = (|| {
        let file = std::fs::File::create(&output).map_err(|e| DriverMessage::CreateTemporary {
            path: display(&output),
            error: e.to_string(),
        })?;
        let status = Command::new(program)
            .args(switches)
            .args(&arguments.preprocessor_arguments)
            .arg(&input)
            .stdout(file)
            .status()
            .map_err(|e| DriverMessage::StartPreprocessor {
                program: program.into(),
                error: e.to_string(),
            })?;
        if !status.success() {
            return Err(DriverMessage::PreprocessorFailed {
                program: program.into(),
                status: status.to_string(),
            });
        }
        std::fs::read(&output).map_err(|e| DriverMessage::ReadTemporary {
            path: display(&output),
            error: e.to_string(),
        })
    })();
    if arguments.keep_temps {
        if !arguments.quiet {
            reporter.notice(&Diagnostic::info(DriverMessage::KeptTemporaries {
                input: display(&input),
                output: display(&output),
            }));
        }
    } else {
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
    }
    result
}

/// Why a run stopped.
enum RunError {
    /// A file, stream or preprocessor failure.
    Driver(DriverMessage),
    /// The program did not compile.
    Compile(Vec<vcl::CompileError>),
    /// `--warnings-as-errors`, and warnings were printed.
    Warnings,
}

impl From<DriverMessage> for RunError {
    fn from(message: DriverMessage) -> Self {
        Self::Driver(message)
    }
}

/// The input's name in diagnostics.
fn input_name(arguments: &Arguments) -> &str {
    match (&arguments.input, arguments.mode()) {
        (Some(path), _) => path,
        (None, vcl::Mode::Vcl) => "_stdin_",
        (None, vcl::Mode::Oxide) => "<stdin>",
    }
}

fn run(arguments: &Arguments, reporter: &mut Reporter) -> Result<(), RunError> {
    let source = if let Some(path) = &arguments.input {
        std::fs::read(path).map_err(|e| DriverMessage::OpenInput {
            path: path.clone(),
            error: e.to_string(),
        })?
    } else {
        let mut source = Vec::new();
        io::stdin()
            .read_to_end(&mut source)
            .map_err(|e| DriverMessage::ReadInput {
                error: e.to_string(),
            })?;
        source
    };
    let source = preprocess(source, arguments, reporter)?;
    let source = String::from_utf8(source).map_err(|_| DriverMessage::NotUtf8)?;
    reporter.set_source(input_name(arguments), &source);
    let filename = arguments.input.as_deref().unwrap_or("_stdin_");
    let output = vcl::compile(&source, filename, &arguments.compile).map_err(RunError::Compile)?;
    for warning in &output.warnings {
        reporter.notice(warning);
    }
    // Warnings from the command line count too.
    if arguments.warnings_as_errors && reporter.warnings() > 0 {
        return Err(RunError::Warnings);
    }
    let failed = |path: &str, e: io::Error| DriverMessage::WriteOutput {
        path: path.to_string(),
        error: e.to_string(),
    };
    let to_stdout = |reporter: &mut Reporter| {
        reporter
            .assembly(&output.assembly)
            .map_err(|e| DriverMessage::WriteStandardOutput {
                error: e.to_string(),
            })
    };
    match arguments.mode() {
        // The listing, then the header, each written in place.
        vcl::Mode::Vcl => {
            if let Some(path) = &arguments.output {
                std::fs::write(path, output.assembly.as_bytes()).map_err(|e| failed(path, e))?;
            } else {
                to_stdout(reporter)?;
            }
            if let Some(path) = &arguments.header {
                std::fs::write(path, output.header.as_bytes()).map_err(|e| failed(path, e))?;
            }
        }
        // All files or none, so that a failed run never leaves a new but
        // incomplete target for make to take as up to date.
        vcl::Mode::Oxide => {
            let mut files = Vec::new();
            if let Some(path) = &arguments.output {
                files.push((path.as_str(), output.assembly.as_bytes()));
            }
            if let Some(path) = &arguments.header {
                files.push((path.as_str(), output.header.as_bytes()));
            }
            write_all_or_nothing(&files)?;
            if arguments.output.is_none() {
                to_stdout(reporter)?;
            }
        }
    }
    Ok(())
}

/// Write each file to a temporary file beside it, then rename them all into
/// place. A failed write leaves every target as it was.
fn write_all_or_nothing(files: &[(&str, &[u8])]) -> Result<(), DriverMessage> {
    let failed = |path: &str, error: io::Error| DriverMessage::WriteOutput {
        path: path.to_string(),
        error: error.to_string(),
    };
    let mut staged: Vec<(PathBuf, &str)> = Vec::new();
    let discard = |staged: &[(PathBuf, &str)]| {
        for (temporary, _) in staged {
            let _ = std::fs::remove_file(temporary);
        }
    };
    for &(path, bytes) in files {
        let target = std::path::Path::new(path);
        let Some(name) = target.file_name() else {
            discard(&staged);
            return Err(failed(path, io::Error::from(io::ErrorKind::InvalidInput)));
        };
        let mut temporary_name = std::ffi::OsString::from(".");
        temporary_name.push(name);
        temporary_name.push(format!(".{}.tmp", std::process::id()));
        let temporary = target.with_file_name(temporary_name);
        if let Err(error) = std::fs::write(&temporary, bytes) {
            let _ = std::fs::remove_file(&temporary);
            discard(&staged);
            return Err(failed(path, error));
        }
        staged.push((temporary, path));
    }
    for (index, (temporary, path)) in staged.iter().enumerate() {
        if let Err(error) = std::fs::rename(temporary, path) {
            discard(&staged[index..]);
            return Err(failed(path, error));
        }
    }
    Ok(())
}

fn main() {
    let mut reporter = Reporter::stdio(vcl::Mode::default());
    let arguments = match cli::parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(error) => {
            reporter.set_mode(error.mode);
            for notice in &error.notices {
                reporter.notice(notice);
            }
            reporter.command_line_error(&error.diagnostic);
            std::process::exit(i32::from(error.exit_status()));
        }
    };
    reporter.set_mode(arguments.mode());
    reporter.set_warnings_as_errors(arguments.warnings_as_errors);

    for notice in &arguments.notices {
        reporter.notice(notice);
    }

    if arguments.version {
        reporter.version(&cli::version());
        return;
    }

    if arguments.help {
        if !arguments.quiet {
            reporter.help(&cli::help());
        }
        return;
    }

    // vcl exits with 255 on every failure but one; oxide mode with 1.
    let failure_status = match arguments.mode() {
        vcl::Mode::Vcl => 255,
        vcl::Mode::Oxide => 1,
    };
    match run(&arguments, &mut reporter) {
        Ok(()) => {}
        Err(RunError::Compile(errors)) => {
            // vcl's copy detector fails with an error but exits with status
            // 0; oxide mode treats it as the error it is.
            if arguments.mode() == vcl::Mode::Vcl
                && let Some(error) = errors.iter().find(|error| error.exit_status() == 0)
            {
                reporter.copy_detection_failure(&error.diagnostic());
                return;
            }
            let diagnostics: Vec<Diagnostic> = errors.iter().map(|e| e.diagnostic()).collect();
            let style = if arguments.ms_diagnostics {
                ErrorStyle::Microsoft {
                    file: input_name(&arguments),
                }
            } else {
                ErrorStyle::Lines
            };
            reporter.compile_errors(&diagnostics, style);
            std::process::exit(failure_status);
        }
        Err(RunError::Warnings) => {
            reporter.warnings_failed();
            std::process::exit(failure_status);
        }
        Err(RunError::Driver(message)) => {
            reporter.failure(&Diagnostic::error(message));
            std::process::exit(failure_status);
        }
    }
}
