//! `oxide-vusl`: the VUSL driver. It reads a program, preprocesses it in C
//! syntax, checks it, generates VCL and compiles that with `vcl::compile`,
//! writing the listing and the reflection files (`docs/VUSL.md`). Exit statuses are oxide-vcl's in oxide
//! mode: 1 when a run fails, 2 for a bad command line.
//!
//! Everything it prints goes through [`vcl::diagnostics::Reporter`].

use std::io::{self, Read};
use vcl::diagnostics::{Diagnostic, DriverMessage, OXIDE_EXIT_FAILURE, Reporter};
use vcl::driver::write_all_or_nothing;
use vcl::preprocessor::{FileSystem, PreprocessFailure};
use vusl_cli::{self as cli, Arguments};

/// The driver's name, which locates its diagnostics.
const PROGRAM: &str = "oxide-vusl";

/// Why a run stopped.
enum RunError {
    /// A file, a stream, or a stage the driver cannot run.
    Driver(Box<Diagnostic>),
    /// The preprocessor stopped at an error.
    Preprocess(Box<PreprocessFailure>),
    /// The program did not check, or its code could not be generated.
    Check(Box<Diagnostic>),
    /// vcl rejected the generated VCL.
    Compile(Vec<Diagnostic>),
    /// `--warnings-as-errors`, and warnings were printed.
    Warnings,
}

impl From<DriverMessage> for RunError {
    fn from(message: DriverMessage) -> Self {
        Self::Driver(Box::new(Diagnostic::error(message)))
    }
}

/// The input's name in diagnostics.
fn input_name(arguments: &Arguments) -> &str {
    match arguments.input.as_deref() {
        Some("-") | None => "<stdin>",
        Some(path) => path,
    }
}

/// The program's text, from its file or standard input.
fn read_input(arguments: &Arguments) -> Result<String, RunError> {
    let bytes = match arguments.input.as_deref() {
        Some("-") | None => {
            let mut bytes = Vec::new();
            io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|e| DriverMessage::ReadInput {
                    error: e.to_string(),
                })?;
            bytes
        }
        Some(path) => std::fs::read(path).map_err(|e| DriverMessage::OpenInput {
            path: path.to_string(),
            error: e.to_string(),
        })?,
    };
    Ok(String::from_utf8(bytes).map_err(|_| DriverMessage::NotUtf8)?)
}

/// Preprocesses the program. Its warnings are printed here; the files it
/// read are handed to the reporter for quoting.
fn preprocess(
    text: &str,
    arguments: &Arguments,
    reporter: &mut Reporter,
) -> Result<vcl::preprocessor::Preprocessed, RunError> {
    let result = vusl::preprocess(input_name(arguments), text, &arguments.compile, &FileSystem);
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
    if arguments.warnings_as_errors && reporter.warnings() > 0 {
        return Err(RunError::Warnings);
    }
    Ok(preprocessed)
}

/// Text to `-o`, or to standard output.
fn write_text(text: &str, arguments: &Arguments, reporter: &mut Reporter) -> Result<(), RunError> {
    if let Some(path) = &arguments.output {
        write_all_or_nothing(&[(path.as_str(), text.as_bytes())])?;
        return Ok(());
    }
    reporter
        .text(text)
        .map_err(|e| DriverMessage::WriteStandardOutput {
            error: e.to_string(),
        })?;
    Ok(())
}

fn run(arguments: &Arguments, reporter: &mut Reporter) -> Result<(), RunError> {
    let text = read_input(arguments)?;
    let preprocessed = preprocess(&text, arguments, reporter)?;
    if arguments.preprocess_only {
        return write_text(&preprocessed.text, arguments, reporter);
    }
    let checked = match vusl::check(input_name(arguments), &preprocessed) {
        Ok(checked) => checked,
        Err(failure) => {
            for warning in &failure.warnings {
                reporter.report(warning);
            }
            return Err(RunError::Check(Box::new(failure.error)));
        }
    };
    for warning in &checked.warnings {
        reporter.report(warning);
    }
    if arguments.warnings_as_errors && reporter.warnings() > 0 {
        return Err(RunError::Warnings);
    }
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    if arguments.check {
        // The reflection needs only the checked program.
        reflection_files(&checked, None, arguments, &mut files);
        return write_files(&files);
    }
    if arguments.stop_at_vcl {
        let vcl = checked
            .generate()
            .map_err(|failure| RunError::Check(Box::new(failure.error)))?;
        if let Some(path) = &arguments.emit_vcl {
            files.push((path.clone(), vcl.text.clone().into_bytes()));
        }
        reflection_files(&checked, None, arguments, &mut files);
        if let Some(path) = &arguments.output {
            files.push((path.clone(), vcl.text.clone().into_bytes()));
        }
        write_files(&files)?;
        if arguments.output.is_none() {
            write_text(&vcl.text, arguments, reporter)?;
        }
        return Ok(());
    }
    let listing = format!("{}.vcl", checked.program.name);
    let (vcl, compiled) = match vusl::compile(&checked, &arguments.compile) {
        Ok(compiled) => compiled,
        Err(failure) => match *failure {
            vusl::CompileFailure::Generate(failure) => {
                return Err(RunError::Check(Box::new(failure.error)));
            }
            vusl::CompileFailure::Vcl { vcl, errors } => {
                // The generated lines are shown where they came from.
                reporter.set_source(&listing, &vcl.text);
                reporter.set_line_origins(vcl.origins.clone());
                return Err(RunError::Compile(
                    errors.iter().map(|error| error.diagnostic()).collect(),
                ));
            }
        },
    };
    if let Some(path) = &arguments.emit_vcl {
        files.push((path.clone(), vcl.text.clone().into_bytes()));
    }
    if !compiled.warnings.is_empty() {
        reporter.set_source(&listing, &vcl.text);
        reporter.set_line_origins(vcl.origins.clone());
        for warning in &compiled.warnings {
            reporter.report(warning);
        }
    }
    if arguments.warnings_as_errors && reporter.warnings() > 0 {
        return Err(RunError::Warnings);
    }
    reflection_files(
        &checked,
        Some(compiled.instruction_pairs),
        arguments,
        &mut files,
    );
    if let Some(path) = &arguments.output {
        files.push((path.clone(), compiled.assembly.clone().into_bytes()));
    }
    write_files(&files)?;
    if arguments.output.is_none() {
        write_text(&compiled.assembly, arguments, reporter)?;
    }
    Ok(())
}

/// The reflection files asked for.
fn reflection_files(
    checked: &vusl::Checked,
    instructions: Option<usize>,
    arguments: &Arguments,
    files: &mut Vec<(String, Vec<u8>)>,
) {
    if arguments.reflect.is_none() && arguments.header.is_none() {
        return;
    }
    let reflection = checked.reflection(instructions);
    if let Some(path) = &arguments.reflect {
        files.push((
            path.clone(),
            vusl::reflect::json::write(&reflection).into_bytes(),
        ));
    }
    if let Some(path) = &arguments.header {
        files.push((
            path.clone(),
            vusl::reflect::cpp::write(&reflection).into_bytes(),
        ));
    }
}

/// Every output file, or none.
fn write_files(files: &[(String, Vec<u8>)]) -> Result<(), RunError> {
    let files: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    write_all_or_nothing(&files)?;
    Ok(())
}

fn main() {
    let mut reporter = Reporter::stdio();
    reporter.set_program_name(PROGRAM);
    let arguments = match cli::parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(error) => {
            reporter.set_color(error.color.enabled_here());
            reporter.report(&error.diagnostic);
            std::process::exit(i32::from(error.exit_status()));
        }
    };
    reporter.set_warnings_as_errors(arguments.warnings_as_errors);
    reporter.set_color(arguments.color.enabled_here());

    if arguments.version {
        reporter.version(&cli::version());
        return;
    }
    if arguments.help {
        reporter.help(&cli::help());
        return;
    }

    let failure_status = i32::from(OXIDE_EXIT_FAILURE);
    match run(&arguments, &mut reporter) {
        Ok(()) => {}
        Err(RunError::Preprocess(failure)) => {
            reporter.compile_errors(&[failure.error]);
            std::process::exit(failure_status);
        }
        Err(RunError::Check(error)) => {
            reporter.compile_errors(&[*error]);
            std::process::exit(failure_status);
        }
        Err(RunError::Compile(errors)) => {
            reporter.compile_errors(&errors);
            std::process::exit(failure_status);
        }
        Err(RunError::Warnings) => {
            reporter.warnings_failed();
            std::process::exit(failure_status);
        }
        Err(RunError::Driver(diagnostic)) => {
            reporter.report(&diagnostic);
            std::process::exit(failure_status);
        }
    }
}
