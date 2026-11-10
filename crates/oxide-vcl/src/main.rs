//! `oxide-vcl`: the command-line driver. It parses the original's command line,
//! runs the optional preprocessors, compiles with the `vcl` crate and writes
//! the results, keeping the original's diagnostics and exit statuses.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Command;
use vcl_cli::{self as cli, Arguments, DiagnosticKind, NoticeLevel};

fn notices(notices: &[cli::Notice]) {
    for notice in notices {
        let prefix = match notice.level {
            NoticeLevel::Info => "INFO: ",
            NoticeLevel::Warning => "WARNING: ",
        };
        eprint!("{prefix}{}", notice.message);
    }
}

/// The original's preprocessing stages, in the order it runs them: the C
/// preprocessor (`-G`), gasp (`-g`) and gasp with its alternate syntax (`-g+`).
/// Each enabled stage reads the previous stage's output.
fn preprocess(mut source: Vec<u8>, arguments: &Arguments) -> Result<Vec<u8>, String> {
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
            source = run_stage(stage, program, switches, &source, arguments)?;
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
) -> Result<Vec<u8>, String> {
    let temporary = |kind: &str| -> PathBuf {
        std::env::temp_dir().join(format!("vcl{kind}_{}_{stage}", std::process::id()))
    };
    let (input, output) = (temporary("i"), temporary("o"));
    std::fs::write(&input, source)
        .map_err(|e| format!("failed to write '{}': {e}", input.display()))?;
    let result = (|| {
        let file = std::fs::File::create(&output)
            .map_err(|e| format!("failed to create '{}': {e}", output.display()))?;
        let status = Command::new(program)
            .args(switches)
            .args(&arguments.preprocessor_arguments)
            .arg(&input)
            .stdout(file)
            .status()
            .map_err(|e| format!("failed to start {program}: {e}"))?;
        if !status.success() {
            return Err(format!("{program} failed: {status}"));
        }
        std::fs::read(&output).map_err(|e| format!("failed to read '{}': {e}", output.display()))
    })();
    if arguments.keep_temps {
        if !arguments.quiet {
            eprintln!("INFO: kept {} and {}", input.display(), output.display());
        }
    } else {
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
    }
    result
}

enum RunError {
    Standard(String),
    LegacyCopy(String),
}

impl From<String> for RunError {
    fn from(message: String) -> Self {
        Self::Standard(message)
    }
}

fn run(arguments: &Arguments) -> Result<(), RunError> {
    let source = if let Some(path) = &arguments.input {
        std::fs::read(path).map_err(|e| format!("failed to open file '{path}': {e}"))?
    } else {
        let mut source = Vec::new();
        io::stdin()
            .read_to_end(&mut source)
            .map_err(|e| e.to_string())?;
        source
    };
    let source = preprocess(source, arguments)?;
    let source = String::from_utf8(source).map_err(|_| "source is not valid UTF-8".to_string())?;
    let filename = arguments.input.as_deref().unwrap_or("_stdin_");
    let output = vcl::compile(&source, filename, &arguments.compile).map_err(|errors| {
        if let Some(error) = errors.iter().find(|error| error.exit_status() == 0) {
            return RunError::LegacyCopy(format!("INFO: OS_Init(Linux)\nERROR: {}\nERROR: failed to detect moves!\nERROR: failed to do basic operation processing", error.message));
        }
        RunError::Standard(errors
            .into_iter()
            .map(|e| {
                if arguments.ms_diagnostics {
                    format!("{filename}({}): error: {}", e.line, e.message)
                } else {
                    e.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"))
    })?;
    for warning in output.warnings {
        eprintln!("WARNING: {warning}");
    }
    if let Some(path) = &arguments.output {
        std::fs::write(path, output.assembly.as_bytes())
            .map_err(|e| format!("failed to write '{path}': {e}"))?;
    } else {
        io::stdout()
            .write_all(output.assembly.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    if let Some(path) = &arguments.header {
        std::fs::write(path, output.header.as_bytes())
            .map_err(|e| format!("failed to write '{path}': {e}"))?;
    }
    Ok(())
}

fn main() {
    let arguments = match cli::parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(error) => {
            notices(&error.notices);
            let prefix = match error.kind {
                DiagnosticKind::Error => "ERROR: ",
                DiagnosticKind::User => "USER: ",
                DiagnosticKind::Plain => "",
            };
            eprintln!("{prefix}{}", error.message);
            println!("ERROR: something went wrong, vcl's outta here.");
            std::process::exit(i32::from(error.exit_status()));
        }
    };

    notices(&arguments.notices);

    if arguments.help {
        if !arguments.quiet {
            eprint!("{}", cli::help());
        }
        return;
    }

    match run(&arguments) {
        Ok(()) => {}
        Err(RunError::LegacyCopy(diagnostic)) => eprintln!("{diagnostic}"),
        Err(RunError::Standard(error)) => {
            eprintln!("ERROR: {error}");
            println!("ERROR: something went wrong, vcl's outta here.");
            std::process::exit(255);
        }
    }
}
