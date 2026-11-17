//! `oxide-vcl`: the command-line driver. It parses the original's command line,
//! runs the optional preprocessor, compiles with the `vcl` crate and writes
//! the results, with the original's exit status in vcl mode (255 on any
//! failure).
//!
//! Everything it prints goes through [`vcl::diagnostics::Reporter`].

use std::io::{self, Read};
use vcl::diagnostics::{Diagnostic, DriverMessage, Reporter};
use vcl::driver::write_all_or_nothing;
use vcl::preprocessor::{self, FileSystem, PreprocessFailure};
use vcl_cli::{self as cli, Arguments};

/// Whether the source is preprocessed: `-G`, `-g` or `-g+`, without `-W`.
fn preprocesses(arguments: &Arguments) -> bool {
    !arguments.skip_preprocessing
        && (arguments.preprocess_cpp
            || arguments.preprocess_gasp
            || arguments.preprocess_gasp_alternate)
}

/// Runs the preprocessor over the source, in standard mode for `-G` and in
/// MASP mode for `-g`. Its warnings are printed here; the files it read are
/// handed to the reporter for quoting.
fn preprocess(
    source: String,
    arguments: &Arguments,
    reporter: &mut Reporter,
) -> Result<String, RunError> {
    if !preprocesses(arguments) {
        return Ok(source);
    }
    if arguments.preprocess_gasp_alternate {
        return Err(DriverMessage::AlternateSyntax.into());
    }
    let result = preprocessor::preprocess(
        input_name(arguments),
        &source,
        &arguments.preprocess,
        &FileSystem,
    );
    let (files, warnings) = match &result {
        Ok(done) => (&done.files, &done.warnings),
        Err(failure) => (&failure.files, &failure.warnings),
    };
    for file in files {
        reporter.add_source(&file.name, &file.text);
    }
    for warning in warnings {
        reporter.report(warning);
    }
    let preprocessed = result.map_err(RunError::Preprocess)?;
    // Warnings that fail the run stop it before anything is written.
    if arguments.warnings_as_errors && reporter.warnings() > 0 {
        return Err(RunError::Warnings);
    }
    // The compiler's diagnostics name the line each one came from.
    reporter.set_line_origins(preprocessed.line_origins);
    Ok(preprocessed.text)
}

/// Why a run stopped.
enum RunError {
    /// A file or stream failure.
    Driver(DriverMessage),
    /// The preprocessor stopped at an error.
    Preprocess(Box<PreprocessFailure>),
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
    arguments.input.as_deref().unwrap_or("<stdin>")
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
    let source = String::from_utf8(source).map_err(|_| DriverMessage::NotUtf8)?;
    let source = preprocess(source, arguments, reporter)?;
    if arguments.preprocess_only {
        return write_preprocessed(&source, arguments, reporter);
    }
    reporter.set_source(input_name(arguments), &source);
    let filename = arguments.input.as_deref().unwrap_or("_stdin_");
    let output = vcl::compile(&source, filename, &arguments.compile).map_err(RunError::Compile)?;
    for warning in &output.warnings {
        reporter.report(warning);
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

/// `--preprocess-only`: the preprocessed source, to `-o` or standard output.
fn write_preprocessed(
    source: &str,
    arguments: &Arguments,
    reporter: &mut Reporter,
) -> Result<(), RunError> {
    if let Some(path) = &arguments.output {
        write_all_or_nothing(&[(path.as_str(), source.as_bytes())])?;
        return Ok(());
    }
    reporter
        .text(source)
        .map_err(|e| DriverMessage::WriteStandardOutput {
            error: e.to_string(),
        })?;
    Ok(())
}

fn main() {
    let mut reporter = Reporter::stdio();
    let arguments = match cli::parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(error) => {
            reporter.set_color(error.color.enabled_here());
            for notice in &error.notices {
                reporter.report(notice);
            }
            reporter.report(&error.diagnostic);
            std::process::exit(i32::from(error.exit_status()));
        }
    };
    reporter.set_warnings_as_errors(arguments.warnings_as_errors);
    reporter.set_color(arguments.color.enabled_here());

    for notice in &arguments.notices {
        reporter.report(notice);
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

    let failure_status = i32::from(arguments.mode().failure_status());
    match run(&arguments, &mut reporter) {
        Ok(()) => {}
        Err(RunError::Compile(errors)) => {
            let diagnostics: Vec<Diagnostic> = errors.iter().map(|e| e.diagnostic()).collect();
            reporter.compile_errors(&diagnostics);
            std::process::exit(failure_status);
        }
        Err(RunError::Preprocess(failure)) => {
            reporter.compile_errors(&[failure.error]);
            std::process::exit(failure_status);
        }
        Err(RunError::Warnings) => {
            reporter.warnings_failed();
            std::process::exit(failure_status);
        }
        Err(RunError::Driver(message)) => {
            reporter.report(&Diagnostic::error(message));
            std::process::exit(failure_status);
        }
    }
}
