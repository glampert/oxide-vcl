//! Command-line parsing for the `oxide-vusl` driver. A value follows its
//! option as the next argument (`-o out.vsm`, `-I include`) or glued to it
//! (`-oout.vsm`, `-Iinclude`, `--color=never`). `-` as the input reads
//! standard input.

use vcl::diagnostics::{Diagnostic, OXIDE_EXIT_USAGE};
use vcl::driver::Color;
use vusl::diagnostics::VuslMessage;

/// The project's version, shared by every crate in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The UTC date this driver was built (see `build.rs`).
pub const BUILD_DATE: &str = env!("VUSL_BUILD_DATE");

/// The line printed by `-v` and `--version`.
pub fn version() -> String {
    format!("oxide-vusl {VERSION}, built {BUILD_DATE}\n")
}

/// The help screen printed by `-h` and `--help`.
pub fn help() -> String {
    include_str!("help.txt")
        .replace("{version}", VERSION)
        .replace("{date}", BUILD_DATE)
}

/// A parsed command line: the compile's settings plus what the driver does
/// around it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Arguments {
    /// The program to compile; `-` for standard input.
    pub input: Option<String>,
    /// `-o`: where the output goes; standard output when absent.
    pub output: Option<String>,
    /// `-I` and `-D`, and everything else the compiler itself takes.
    pub compile: vusl::Options,
    /// `--preprocess-only`: write the preprocessed program and stop.
    pub preprocess_only: bool,
    /// `--check`: check the program and stop.
    pub check: bool,
    /// `--warnings-as-errors`.
    pub warnings_as_errors: bool,
    /// `--color=`.
    pub color: Color,
    pub help: bool,
    pub version: bool,
}

/// A command line the driver rejects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliError {
    pub diagnostic: Box<Diagnostic>,
    /// The `--color=` choice, for printing the error itself.
    pub color: Color,
}

impl CliError {
    /// The exit status of a run with a bad command line.
    pub fn exit_status(&self) -> u8 {
        OXIDE_EXIT_USAGE
    }
}

/// Whether `name` is a C identifier.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `-D`'s argument as a macro: `NAME` defines it as 1.
fn define(argument: &str) -> Option<(String, String)> {
    let (name, value) = argument.split_once('=').unwrap_or((argument, "1"));
    let reserved = vcl::preprocessor::is_builtin_macro(name) || name == vusl::PREDEFINED_MACRO;
    (is_identifier(name) && !reserved).then(|| (name.to_string(), value.to_string()))
}

/// Parses the arguments after the program's name.
pub fn parse<I, S>(arguments: I) -> Result<Arguments, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    // The colors are known before anything else, so an error about any
    // argument prints in the colors asked for.
    let color = arguments
        .iter()
        .filter_map(|argument| argument.strip_prefix("--color="))
        .filter_map(Color::from_name)
        .next_back()
        .unwrap_or_default();
    let fail = move |message: VuslMessage| CliError {
        diagnostic: Box::new(Diagnostic::error(message)),
        color,
    };
    let mut parsed = Arguments {
        color,
        ..Arguments::default()
    };
    let mut rest = arguments.into_iter();
    while let Some(argument) = rest.next() {
        // An option's value: the rest of the argument, or the next one.
        let mut value = |option: &str, glued: &str| -> Result<String, CliError> {
            if glued.is_empty() {
                rest.next().ok_or_else(|| {
                    fail(VuslMessage::MissingValue {
                        option: option.to_string(),
                    })
                })
            } else {
                Ok(glued.to_string())
            }
        };
        match argument.as_str() {
            "-h" | "--help" => parsed.help = true,
            "-v" | "--version" => parsed.version = true,
            "--preprocess-only" => parsed.preprocess_only = true,
            "--check" => parsed.check = true,
            "--warnings-as-errors" => parsed.warnings_as_errors = true,
            "-" => {
                if parsed.input.is_some() {
                    return Err(fail(VuslMessage::ExtraInput { argument }));
                }
                parsed.input = Some(argument);
            }
            _ => {
                if let Some(name) = argument.strip_prefix("--color=") {
                    if Color::from_name(name).is_none() {
                        return Err(fail(VuslMessage::BadColor {
                            value: name.to_string(),
                        }));
                    }
                } else if let Some(glued) = argument.strip_prefix("-o") {
                    parsed.output = Some(value("-o", glued)?);
                } else if let Some(glued) = argument.strip_prefix("-I") {
                    let directory = value("-I", glued)?;
                    parsed.compile.include_dirs.push(directory);
                } else if let Some(glued) = argument.strip_prefix("-D") {
                    let text = value("-D", glued)?;
                    let Some(definition) = define(&text) else {
                        return Err(fail(VuslMessage::BadDefine { argument: text }));
                    };
                    parsed.compile.defines.push(definition);
                } else if argument.starts_with('-') {
                    return Err(fail(VuslMessage::UnknownOption { option: argument }));
                } else if parsed.input.is_some() {
                    return Err(fail(VuslMessage::ExtraInput { argument }));
                } else {
                    parsed.input = Some(argument);
                }
            }
        }
    }
    if parsed.input.is_none() && !parsed.help && !parsed.version {
        return Err(fail(VuslMessage::NoInput));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts that the command line is rejected with `expected`.
    fn rejects(arguments: &[&str], expected: VuslMessage) {
        let error = parse(arguments.iter().copied()).unwrap_err();
        assert_eq!(
            error.diagnostic.message,
            vcl::Message::from(expected),
            "{arguments:?}"
        );
    }

    #[test]
    fn values_follow_or_are_glued_to_their_option() {
        let parsed = parse([
            "-o", "out.vsm", "-Iinc", "-I", "lib", "-DA", "-D", "B=2", "p.vusl",
        ])
        .unwrap();
        assert_eq!(parsed.input.as_deref(), Some("p.vusl"));
        assert_eq!(parsed.output.as_deref(), Some("out.vsm"));
        assert_eq!(parsed.compile.include_dirs, ["inc", "lib"]);
        assert_eq!(
            parsed.compile.defines,
            [("A".to_string(), "1".to_string()), ("B".into(), "2".into())]
        );
        assert_eq!(
            parse(["-oout.vsm", "-"]).unwrap().output.as_deref(),
            Some("out.vsm")
        );
    }

    #[test]
    fn flags_and_colors() {
        let parsed = parse([
            "--preprocess-only",
            "--check",
            "--warnings-as-errors",
            "--color=always",
            "p.vusl",
        ])
        .unwrap();
        assert!(parsed.preprocess_only && parsed.check && parsed.warnings_as_errors);
        assert_eq!(parsed.color, Color::Always);
        assert!(parse(["--help"]).unwrap().help);
        assert!(parse(["-v"]).unwrap().version);
    }

    #[test]
    fn bad_command_lines_say_why() {
        rejects(&[], VuslMessage::NoInput);
        let argument = "b.vusl".to_string();
        rejects(&["a.vusl", "b.vusl"], VuslMessage::ExtraInput { argument });
        let option = "--colour=never".to_string();
        rejects(
            &[&option, "a.vusl"],
            VuslMessage::UnknownOption {
                option: option.clone(),
            },
        );
        let value = "sometimes".to_string();
        rejects(
            &["--color=sometimes", "a.vusl"],
            VuslMessage::BadColor { value },
        );
        let option = "-o".to_string();
        rejects(&["a.vusl", "-o"], VuslMessage::MissingValue { option });
        for bad in ["1A", "A-B", "__LINE__", "__VUSL__", ""] {
            let argument = bad.to_string();
            rejects(&["-D", bad, "a.vusl"], VuslMessage::BadDefine { argument });
        }
        // An error prints in the colors asked for, wherever --color= is.
        let error = parse(["--bad", "--color=never"]).unwrap_err();
        assert_eq!(error.color, Color::Never);
        assert_eq!(error.exit_status(), OXIDE_EXIT_USAGE);
    }
}
