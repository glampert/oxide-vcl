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
    /// `--reflect=FILE`: the reflection as JSON.
    pub reflect: Option<String>,
    /// `--header=FILE`: the reflection as a C++ header.
    pub header: Option<String>,
    /// `--emit-vcl=FILE`: the generated VCL too.
    pub emit_vcl: Option<String>,
    /// `--stop-at-vcl`: write the generated VCL, not the listing.
    pub stop_at_vcl: bool,
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

/// The oxide-vcl optimizations `list`, the value of `argument`, names.
fn optimizations(argument: &str, list: &str) -> Result<vcl::Optimizations, VuslMessage> {
    vcl::Optimizations::from_list(list).map_err(|name| VuslMessage::BadOptimization {
        argument: argument.to_string(),
        name,
    })
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
    // The first --opt= or --no-opt=, which --mode=vcl rejects wherever it
    // comes on the line.
    let mut optimization_argument = None;
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
            "--stop-at-vcl" => parsed.stop_at_vcl = true,
            "--warnings-as-errors" => parsed.warnings_as_errors = true,
            "-" => {
                if parsed.input.is_some() {
                    return Err(fail(VuslMessage::ExtraInput { argument }));
                }
                parsed.input = Some(argument);
            }
            _ => {
                if let Some(path) = argument.strip_prefix("--emit-vcl=") {
                    parsed.emit_vcl = Some(path.to_string());
                } else if let Some(name) = argument.strip_prefix("--mode=") {
                    let Some(mode) = vcl::Mode::from_name(name) else {
                        return Err(fail(VuslMessage::BadMode {
                            value: name.to_string(),
                        }));
                    };
                    parsed.compile.mode = mode;
                } else if let Some(list) = argument.strip_prefix("--opt=") {
                    let chosen = optimizations(&argument, list).map_err(fail)?;
                    let earlier = parsed.compile.optimizations.unwrap_or_default();
                    parsed.compile.optimizations = Some(earlier.union(chosen));
                    optimization_argument.get_or_insert(argument);
                } else if let Some(list) = argument.strip_prefix("--no-opt=") {
                    let disabled = optimizations(&argument, list).map_err(fail)?;
                    let compile = &mut parsed.compile;
                    compile.disabled_optimizations = compile.disabled_optimizations.union(disabled);
                    optimization_argument.get_or_insert(argument);
                } else if let Some(path) = argument.strip_prefix("--reflect=") {
                    parsed.reflect = Some(path.to_string());
                } else if let Some(path) = argument.strip_prefix("--header=") {
                    parsed.header = Some(path.to_string());
                } else if let Some(name) = argument.strip_prefix("--color=") {
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
    if let Some(argument) = optimization_argument
        && parsed.compile.mode != vcl::Mode::Oxide
    {
        return Err(fail(VuslMessage::OptimizationsNeedOxideMode { argument }));
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
        let parsed = parse(["--reflect=p.json", "--header=p.h", "p.vusl"]).unwrap();
        assert_eq!(
            (parsed.reflect.as_deref(), parsed.header.as_deref()),
            (Some("p.json"), Some("p.h"))
        );
        let parsed = parse(["--emit-vcl=p.vcl", "--stop-at-vcl", "--mode=vcl", "p.vusl"]).unwrap();
        assert_eq!(parsed.emit_vcl.as_deref(), Some("p.vcl"));
        assert!(parsed.stop_at_vcl);
        assert_eq!(parsed.compile.mode, vcl::Mode::Vcl);
        assert_eq!(parse(["p.vusl"]).unwrap().compile.mode, vcl::Mode::Oxide);
        assert!(parse(["--help"]).unwrap().help);
        assert!(parse(["-v"]).unwrap().version);
    }

    #[test]
    fn optimizations_pass_through_to_oxide_vcl() {
        use vcl::{Optimization, Optimizations};
        let enabled = |arguments: &[&str]| {
            let compile = parse(arguments.iter().copied()).unwrap().compile;
            vcl::Options {
                mode: compile.mode,
                optimizations: compile.optimizations,
                disabled_optimizations: compile.disabled_optimizations,
                ..vcl::Options::default()
            }
            .enabled_optimizations()
        };
        let all = Optimizations::all();
        assert_eq!(enabled(&["p.vusl"]), all);
        let without_loops = all.without(Optimizations::NONE.with(Optimization::Loops));
        assert_eq!(enabled(&["--no-opt=loops", "p.vusl"]), without_loops);
        // --no-opt= takes names off whatever --opt= chose, in either order.
        let unswitch = Optimizations::NONE.with(Optimization::Unswitch);
        assert_eq!(
            enabled(&["--opt=unswitch,exit", "--no-opt=exit", "p.vusl"]),
            unswitch
        );
        assert_eq!(
            enabled(&["--no-opt=exit", "--opt=exit,unswitch", "p.vusl"]),
            unswitch
        );
        assert_eq!(enabled(&["--opt=none", "p.vusl"]), Optimizations::NONE);
        assert_eq!(enabled(&["--no-opt=all", "p.vusl"]), Optimizations::NONE);
        for (argument, name) in [
            ("--opt=fast", "fast"),
            ("--no-opt=", ""),
            ("--opt=all,", ""),
        ] {
            rejects(
                &[argument, "p.vusl"],
                VuslMessage::BadOptimization {
                    argument: argument.into(),
                    name: name.into(),
                },
            );
        }
        // vcl mode never optimizes, wherever --mode= comes.
        for arguments in [
            &["--mode=vcl", "--no-opt=loops", "p.vusl"][..],
            &["--no-opt=loops", "--mode=vcl", "p.vusl"],
        ] {
            rejects(
                arguments,
                VuslMessage::OptimizationsNeedOxideMode {
                    argument: "--no-opt=loops".into(),
                },
            );
        }
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
        let value = "fast".to_string();
        rejects(&["--mode=fast", "a.vusl"], VuslMessage::BadMode { value });
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
