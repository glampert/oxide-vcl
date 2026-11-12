//! Command-line parsing for the `oxide-vcl` driver, following the rules
//! observed from vcl 1.4beta7.
//!
//! The original does not use getopt: it switches on the second byte of each
//! leading argument, and values are the remainder of that same argument.
//! Runtime evidence lives under `local/evidence/oracle/cli-*`.

use std::fmt;
use vcl::diagnostics::{CliMessage, Diagnostic};

/// The project's version, shared by every crate in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The UTC date this driver was built (see `build.rs`).
pub const BUILD_DATE: &str = env!("VCL_BUILD_DATE");

/// The line printed by `-v` and `--version`.
pub fn version() -> String {
    format!("oxide-vcl {VERSION}, built {BUILD_DATE}\n")
}

/// The help screen printed by `-h` and `--help`.
pub fn help() -> String {
    include_str!("help.txt")
        .replace("{version}", VERSION)
        .replace("{date}", BUILD_DATE)
}

/// A parsed command line: the compiler's settings plus what the driver does
/// around compilation (files, preprocessing, diagnostics).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Arguments {
    /// Settings passed to [`vcl::compile`].
    pub compile: vcl::Options,
    /// Source file; standard input when `None`.
    pub input: Option<String>,
    /// Assembly output file (`-o`); standard output when `None`.
    pub output: Option<String>,
    /// Label header file (`-j`).
    pub header: Option<String>,
    /// Print the help screen and stop (`-h`, `--help`).
    pub help: bool,
    /// Print the version and stop (`-v`, `--version`).
    pub version: bool,
    pub quiet: bool,
    /// Fail the run when it prints a warning (`--warnings-as-errors`).
    pub warnings_as_errors: bool,
    /// `file(line): error: message` diagnostics (`-p`).
    pub ms_diagnostics: bool,
    /// Run the C preprocessor first (`-G`).
    pub preprocess_cpp: bool,
    /// Run gasp (`-g`).
    pub preprocess_gasp: bool,
    /// Run gasp with its alternate syntax (`-g+`).
    pub preprocess_gasp_alternate: bool,
    /// Skip every preprocessing stage (`-W`).
    pub skip_preprocessing: bool,
    /// Keep the preprocessing stages' temporary files (`-K`).
    pub keep_temps: bool,
    /// `-I...` arguments, forwarded to the preprocessors in order.
    pub preprocessor_arguments: Vec<String>,
    /// Notices in the original's parse order, already honoring `-q` at that point.
    pub notices: Vec<Diagnostic>,
}

impl Arguments {
    fn info(&mut self, message: CliMessage) {
        if !self.quiet {
            self.notices.push(Diagnostic::info(message));
        }
    }

    fn warn(&mut self, message: CliMessage) {
        self.notices.push(Diagnostic::warning(message));
    }

    /// The mode chosen with `--mode=` (vcl when absent).
    pub fn mode(&self) -> vcl::Mode {
        self.compile.mode
    }
}

/// A command line the driver rejects, with the notices printed before it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliError {
    pub diagnostic: Box<Diagnostic>,
    /// Some failures occur only after OS_Init has already logged its message.
    pub os_initialized: bool,
    pub output: Option<String>,
    pub quiet: bool,
    pub mode: vcl::Mode,
    pub notices: Vec<Diagnostic>,
}

impl CliError {
    fn new(message: CliMessage, initialized: bool, options: Arguments) -> Self {
        Self {
            diagnostic: Box::new(Diagnostic::error(message)),
            os_initialized: initialized,
            output: options.output,
            quiet: options.quiet,
            mode: options.compile.mode,
            notices: options.notices,
        }
    }

    /// The rejected message, for comparisons in tests.
    pub fn message(&self) -> &vcl::Message {
        &self.diagnostic.message
    }

    /// Original `exit(-1)` argument, as observed at the i386 syscall boundary.
    pub const fn raw_exit_code(&self) -> u32 {
        u32::MAX
    }

    /// vcl's status 255, or 2 in oxide mode (the usual status of a usage
    /// error).
    pub const fn exit_status(&self) -> u8 {
        match self.mode {
            vcl::Mode::Vcl => unix_exit_status(self.raw_exit_code()),
            vcl::Mode::Oxide => 2,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic.fmt(formatter)
    }
}

impl std::error::Error for CliError {}

/// Linux exposes the low byte of a normal process's exit argument to its parent.
pub const fn unix_exit_status(raw_code: u32) -> u8 {
    raw_code as u8
}

/// Parse arguments excluding the executable's name.
pub fn parse<I, S>(arguments: I) -> Result<Arguments, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    let mut options = Arguments::default();
    // An error stops the parse where it is, so the mode is looked up first,
    // anywhere on the line, for the error to be reported in it.
    if let Some(mode) = arguments
        .iter()
        .filter_map(|argument| argument.strip_prefix("--mode="))
        .filter_map(vcl::Mode::from_name)
        .next_back()
    {
        options.compile.mode = mode;
    }
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|value| value.starts_with('-')) {
        // oxide-vcl's own options, in both modes; vcl 1.4beta7 has no long
        // options and no -v.
        match argument.as_str() {
            "--help" => {
                options.help = true;
                return Ok(options);
            }
            "--version" | "-v" => {
                options.version = true;
                return Ok(options);
            }
            "--warnings-as-errors" => {
                options.warnings_as_errors = true;
                index += 1;
                continue;
            }
            _ => {}
        }
        // oxide-vcl's own option; vcl 1.4beta7 has no long options.
        if let Some(name) = argument.strip_prefix("--mode=") {
            let Some(mode) = vcl::Mode::from_name(name) else {
                return Err(CliError::new(
                    CliMessage::BadMode {
                        argument: argument.clone(),
                    },
                    false,
                    options,
                ));
            };
            options.compile.mode = mode;
            index += 1;
            continue;
        }
        // oxide-vcl's own option: fixes for vcl 1.4beta7's defects.
        if let Some(list) = argument.strip_prefix("--fix=") {
            match parse_fixes(list) {
                Ok(fixes) => options.compile.fixes = options.compile.fixes.union(fixes),
                Err(name) => {
                    return Err(CliError::new(
                        CliMessage::BadFix {
                            argument: argument.clone(),
                            name,
                        },
                        false,
                        options,
                    ));
                }
            }
            index += 1;
            continue;
        }
        let selector = argument.as_bytes().get(1).copied().unwrap_or(0);
        // VCL arguments are byte-oriented. A non-ASCII selector cannot match
        // any switch, so only take this UTF-8 slice when the selector is ASCII.
        let value = if selector.is_ascii() && argument.len() >= 2 {
            &argument[2..]
        } else {
            ""
        };
        match selector {
            b'I' => options.preprocessor_arguments.push(argument.clone()),
            b'A' | b'a' => options.info(CliMessage::AlwaysOn),
            b'C' => {
                options.compile.disable_size_reduction = true;
                options.info(CliMessage::SizeReductionDisabled);
            }
            // Register dumps during external allocation: not produced.
            b'D' => {}
            b'F' | b'f' => {
                options.compile.disable_alignment = true;
                options.info(CliMessage::AlignmentDisabled);
            }
            b'G' => options.preprocess_cpp = true,
            b'H' | b'h' => {
                options.help = true;
                return Ok(options);
            }
            b'o' => options.output = Some(value.to_owned()),
            b'K' => {
                options.keep_temps = true;
                options.info(CliMessage::KeepTemporaries);
            }
            b'M' => {
                options.compile.timid_memory = true;
                options.warn(CliMessage::TimidMemory);
            }
            b'P' => options.warn(CliMessage::PruneDeprecated),
            // Undocumented in the original, with no observed effect.
            b'Q' => {}
            // Staggered loop memory access is not implemented: notice only.
            b'S' => options.info(CliMessage::StaggeredMemory),
            b'U' => options.compile.unique_labels = true,
            b'W' | b'w' => options.skip_preprocessing = true,
            b'Z' => options.warn(CliMessage::ImmediateFixupsDeprecated),
            b'c' => options.compile.comments = true,
            b'd' => {
                options.compile.dumb = true;
                options.compile.disable_loops = true;
            }
            b'L' => options.compile.disable_loops = true,
            b'e' => {
                options.compile.disable_end_bit = true;
                options.info(CliMessage::EndBitDisabled);
            }
            b'g' => match value.as_bytes().first() {
                None => options.preprocess_gasp = true,
                Some(b'+') => options.preprocess_gasp_alternate = true,
                _ => {
                    return Err(CliError::new(
                        CliMessage::BadGaspOption {
                            argument: argument.clone(),
                        },
                        false,
                        options,
                    ));
                }
            },
            b'j' => options.header = Some(value.to_owned()),
            b'm' => options.compile.mpg = Some(strtol_decimal_i32(value)),
            b'n' => options.compile.new_syntax = true,
            b'p' => options.ms_diagnostics = true,
            b'q' => options.quiet = true,
            b's' => options.compile.symbolic = true,
            // The optimizer here has no time limit, so the value is unused.
            b't' => {}
            b'u' => options.compile.unique = Some(value.to_owned()),
            _ => {
                return Err(CliError::new(
                    CliMessage::BadArgument {
                        argument: argument.clone(),
                    },
                    false,
                    options,
                ));
            }
        }
        index += 1;
    }
    if arguments.len() - index > 1 {
        return Err(CliError::new(CliMessage::TooManyArguments, true, options));
    }
    options.input = arguments.get(index).cloned();
    if options.input.is_some() && options.input == options.output {
        return Err(CliError::new(CliMessage::SameInputAndOutput, true, options));
    }
    Ok(options)
}

/// The fixes a `--fix=` list names: `all`, or fix names separated by commas.
/// The error is the first name that is not a fix (empty for an empty list).
fn parse_fixes(list: &str) -> Result<vcl::Fixes, String> {
    let mut fixes = vcl::Fixes::NONE;
    for name in list.split(',') {
        match name {
            "all" => fixes = fixes.union(vcl::Fixes::all()),
            _ => fixes.insert(vcl::Fix::from_name(name).ok_or_else(|| name.to_owned())?),
        }
    }
    Ok(fixes)
}

/// i386 glibc `strtol(value, NULL, 10)`: whitespace/sign, a decimal prefix,
/// zero for no digits, and saturation at the signed 32-bit limits.
fn strtol_decimal_i32(value: &str) -> i32 {
    let mut bytes = value.bytes().skip_while(u8::is_ascii_whitespace).peekable();
    let negative = match bytes.peek() {
        Some(b'-') => {
            bytes.next();
            true
        }
        Some(b'+') => {
            bytes.next();
            false
        }
        _ => false,
    };
    let limit = if negative {
        2_147_483_648_u64
    } else {
        i32::MAX as u64
    };
    let magnitude = bytes
        .take_while(u8::is_ascii_digit)
        .fold(0_u64, |number, byte| {
            number
                .saturating_mul(10)
                .saturating_add(u64::from(byte - b'0'))
                .min(limit)
        });
    if negative {
        -(magnitude as i64) as i32
    } else {
        magnitude as i32
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use vcl::diagnostics::Severity;

    #[test]
    fn mode_defaults_to_vcl_and_oxide_is_explicit() {
        assert_eq!(parse(["a.vcl"]).unwrap().mode(), vcl::Mode::Vcl);
        assert_eq!(
            parse(["--mode=vcl", "a.vcl"]).unwrap().mode(),
            vcl::Mode::Vcl
        );
        let options = parse(["-q", "--mode=oxide", "-C", "a.vcl"]).unwrap();
        assert_eq!(options.mode(), vcl::Mode::Oxide);
        assert_eq!(options.compile.mode, vcl::Mode::Oxide);
        assert!(options.quiet && options.compile.disable_size_reduction);
        let error = parse(["--mode=sce", "a.vcl"]).unwrap_err();
        assert!(
            *error.message()
                == CliMessage::BadMode {
                    argument: "--mode=sce".into()
                }
        );
        // vcl has no long options, so its wording is the unknown-switch one.
        assert_eq!(error.to_string(), "bad args '--mode=sce', try -h for help");
        assert_eq!(error.exit_status(), 255);
    }

    #[test]
    fn fixes_are_chosen_by_name_and_accumulate() {
        let none = parse(["a.vcl"]).unwrap().compile;
        assert!(none.fixes.is_empty() && !none.fixes(vcl::Fix::End));
        let end = parse(["--fix=end", "a.vcl"]).unwrap().compile;
        assert_eq!(end.fixes, vcl::Fixes::NONE.with(vcl::Fix::End));
        assert_eq!(end.mode, vcl::Mode::Vcl);
        for flags in [
            vec!["--fix=all"],
            vec!["--fix=end,all"],
            vec!["--fix=imm,clip,end,size"],
            vec!["--fix=end", "--fix=clip,size", "--fix=imm", "--fix=end"],
        ] {
            let options = parse(flags.iter().copied().chain(["a.vcl"]))
                .unwrap()
                .compile;
            assert_eq!(options.fixes, vcl::Fixes::all(), "{flags:?}");
        }
        // Oxide mode implies every fix without recording any.
        let oxide = parse(["--mode=oxide", "a.vcl"]).unwrap().compile;
        assert!(oxide.fixes.is_empty() && oxide.fixes(vcl::Fix::End));

        for (argument, name) in [
            ("--fix=", ""),
            ("--fix=alias", "alias"),
            ("--fix=end,", ""),
            ("--fix=End", "End"),
        ] {
            let error = parse([argument, "a.vcl"]).unwrap_err();
            assert!(
                *error.message()
                    == CliMessage::BadFix {
                        argument: argument.into(),
                        name: name.into()
                    },
                "{argument}"
            );
            assert_eq!(
                error.to_string(),
                format!("bad args '{argument}', try -h for help")
            );
            assert_eq!(error.exit_status(), 255);
        }
        let error = parse(["--mode=oxide", "--fix=alias"]).unwrap_err();
        assert_eq!(error.exit_status(), 2);
        assert!(
            error
                .diagnostic
                .text(vcl::Mode::Oxide)
                .contains("unknown fix 'alias'")
        );
    }

    #[test]
    fn defaults_and_stdin() {
        let options = parse(Vec::<String>::new()).unwrap();
        assert_eq!(options, Arguments::default());
    }

    #[test]
    fn help_is_immediate_and_case_aliases_are_explicit() {
        for flag in ["-h", "-H", "-help", "-Hanything", "--help"] {
            assert!(parse([flag, "--invalid", "one", "two"]).unwrap().help);
        }
        for flag in ["-v", "--version"] {
            let options = parse([flag, "--invalid"]).unwrap();
            assert!(options.version && !options.help);
        }
        for flag in ["--helpme", "--versions", "-vx", "--", "-", "-N"] {
            assert!(matches!(
                parse([flag]).unwrap_err().message(),
                vcl::Message::Cli(CliMessage::BadArgument { .. })
            ));
        }
    }

    #[test]
    fn warnings_as_errors_is_a_long_option() {
        let options = parse(["--warnings-as-errors", "-q", "file.vcl"]).unwrap();
        assert!(options.warnings_as_errors && options.quiet);
        assert_eq!(options.input.as_deref(), Some("file.vcl"));
        assert!(!parse(["file.vcl"]).unwrap().warnings_as_errors);
        assert!(parse(["--warnings-as-error"]).is_err());
    }

    #[test]
    fn command_line_errors_use_the_mode_given_anywhere() {
        let error = parse(["-x", "--mode=oxide"]).unwrap_err();
        assert_eq!(error.mode, vcl::Mode::Oxide);
        assert_eq!(error.exit_status(), 2);
        let error = parse(["-o", "out.vsm", "--mode=oxide", "file.vcl"]).unwrap_err();
        assert_eq!(error.mode, vcl::Mode::Oxide);
        let error = parse(["-x"]).unwrap_err();
        assert_eq!((error.mode, error.exit_status()), (vcl::Mode::Vcl, 255));
    }

    #[test]
    fn values_are_attached_and_last_value_wins() {
        let options = parse(["-oold", "-onew.vsm", "-jlabels.inc", "-uID", "file.vcl"]).unwrap();
        assert_eq!(options.output.as_deref(), Some("new.vsm"));
        assert_eq!(options.header.as_deref(), Some("labels.inc"));
        assert_eq!(options.compile.unique.as_deref(), Some("ID"));
        assert_eq!(options.input.as_deref(), Some("file.vcl"));
        assert_eq!(
            parse(["-o", "file.vcl"]).unwrap().output.as_deref(),
            Some("")
        );
        assert!(parse(["-o", "output.vsm", "input.vcl"]).is_err());
    }

    #[test]
    fn parsing_stops_at_first_filename_without_restricting_extension() {
        assert_eq!(
            parse(["file.asm"]).unwrap().input.as_deref(),
            Some("file.asm")
        );
        let error = parse(["file.vcl", "-q"]).unwrap_err();
        assert!(*error.message() == CliMessage::TooManyArguments);
        assert!(error.os_initialized);
        assert_eq!(
            error.to_string(),
            "bad args (-o now specifies output file..)"
        );
    }

    #[test]
    fn timeout_is_accepted_and_numbers_use_decimal_strtol() {
        assert_eq!(parse(["-t1", "-tjunk"]).unwrap(), Arguments::default());
        for (value, expected) in [
            ("", 0),
            ("0", 0),
            ("-5", -5),
            ("junk", 0),
            ("0x20", 0),
            ("3junk", 3),
            (" +12", 12),
            ("999999999999999999999", i32::MAX),
        ] {
            assert_eq!(strtol_decimal_i32(value), expected);
        }
    }

    #[test]
    fn mpg_keeps_signed_decimal_value() {
        assert_eq!(parse(["-m"]).unwrap().compile.mpg, Some(0));
        assert_eq!(parse(["-m16tail"]).unwrap().compile.mpg, Some(16));
        assert_eq!(parse(["-m-10"]).unwrap().compile.mpg, Some(-10));
        assert_eq!(
            parse(["-m9999999999999"]).unwrap().compile.mpg,
            Some(i32::MAX)
        );
        assert_eq!(
            parse(["-m-9999999999999"]).unwrap().compile.mpg,
            Some(i32::MIN)
        );
    }

    #[test]
    fn most_switches_ignore_suffix_but_g_is_special() {
        let options = parse([
            "-quiet",
            "-funrelated",
            "-dxxx",
            "-W",
            "-g+extra",
            "-Iinclude",
        ])
        .unwrap();
        assert!(
            options.quiet
                && options.compile.disable_alignment
                && options.compile.dumb
                && options.compile.disable_loops
        );
        assert!(options.preprocess_gasp_alternate && options.skip_preprocessing);
        assert_eq!(options.preprocessor_arguments, ["-Iinclude"]);
        assert!(
            *parse(["-gbad"]).unwrap_err().message()
                == CliMessage::BadGaspOption {
                    argument: "-gbad".into()
                }
        );
    }

    #[test]
    fn quiet_applies_in_argument_order_and_keeps_warnings() {
        let early = parse(["-a", "-q", "-f", "-P", "-Z"]).unwrap();
        assert_eq!(early.notices.len(), 3);
        assert_eq!(early.notices[0].severity, Severity::Info);
        assert!(
            early.notices[1..]
                .iter()
                .all(|notice| notice.severity == Severity::Warning)
        );
        assert!(parse(["-q", "-h"]).unwrap().quiet);
    }

    #[test]
    fn same_input_output_is_checked_after_options() {
        let error = parse(["-ofile.vcl", "file.vcl"]).unwrap_err();
        assert!(*error.message() == CliMessage::SameInputAndOutput);
        assert_eq!(error.output.as_deref(), Some("file.vcl"));
        assert!(error.os_initialized);
    }

    #[test]
    fn failures_retain_raw_syscall_code_and_parent_visible_status() {
        let error = parse(["--unknown"]).unwrap_err();
        assert_eq!(error.raw_exit_code(), 0xffff_ffff);
        assert_eq!(error.exit_status(), 255);
        assert_eq!(unix_exit_status(0x1234), 0x34);
    }
}
