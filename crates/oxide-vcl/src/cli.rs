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
    /// When diagnostics are colored (`--color=`).
    pub color: Color,
    /// Preprocess in standard mode first (`-G`).
    pub preprocess_cpp: bool,
    /// Preprocess in MASP mode first (`-g`).
    pub preprocess_gasp: bool,
    /// gasp's alternate syntax (`-g+`), which the preprocessor does not
    /// support.
    pub preprocess_gasp_alternate: bool,
    /// Skip preprocessing (`-W`).
    pub skip_preprocessing: bool,
    /// `-K`: accepted; the built-in preprocessor has no temporary files.
    pub keep_temps: bool,
    /// The preprocessor's settings: `-I` and the long options below.
    pub preprocess: vcl::preprocessor::PreprocessOptions,
    /// `--preprocess-only`: write the preprocessed source and stop.
    pub preprocess_only: bool,
    /// The preprocessor's long options given, which need `-G` or `-g`.
    pub preprocessor_options: Vec<String>,
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

/// When diagnostics carry ANSI colors (`--color=`), as with clang and gcc.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Color {
    /// On when standard error is a terminal, unless `NO_COLOR` is set or
    /// `TERM` is `dumb`.
    #[default]
    Auto,
    Always,
    Never,
}

impl Color {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    /// Whether to color: for `Auto`, whether standard error is a terminal
    /// and the environment allows colors (no `NO_COLOR`, `TERM` not
    /// `dumb`).
    pub fn enabled(self, terminal: bool, environment_allows: bool) -> bool {
        match self {
            Self::Auto => terminal && environment_allows,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// A command line the driver rejects, with the notices printed before it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliError {
    pub diagnostic: Box<Diagnostic>,
    pub output: Option<String>,
    pub quiet: bool,
    pub mode: vcl::Mode,
    /// The `--color=` setting, for printing the error.
    pub color: Color,
    pub notices: Vec<Diagnostic>,
}

impl CliError {
    fn new(message: CliMessage, options: Arguments) -> Self {
        Self {
            diagnostic: Box::new(Diagnostic::error(message)),
            output: options.output,
            quiet: options.quiet,
            mode: options.compile.mode,
            color: options.color,
            notices: options.notices,
        }
    }

    /// The rejected message, for comparisons in tests.
    pub fn message(&self) -> &vcl::Message {
        &self.diagnostic.message
    }

    /// The exit status: vcl's 255, or 2 in oxide mode (the usual status of
    /// a usage error).
    pub const fn exit_status(&self) -> u8 {
        self.mode.usage_status()
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic.fmt(formatter)
    }
}

impl std::error::Error for CliError {}

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
    // So is the color setting, for the error to be printed with it.
    if let Some(color) = arguments
        .iter()
        .filter_map(|argument| argument.strip_prefix("--color="))
        .filter_map(Color::from_name)
        .next_back()
    {
        options.color = color;
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
        if let Some(name) = argument.strip_prefix("--color=") {
            let Some(color) = Color::from_name(name) else {
                return Err(CliError::new(
                    CliMessage::BadColor {
                        argument: argument.clone(),
                    },
                    options,
                ));
            };
            options.color = color;
            index += 1;
            continue;
        }
        // oxide-vcl's own options: vclpp's settings for the preprocessor.
        if let Some(handled) = preprocessor_option(argument, &mut options) {
            if let Err(message) = handled {
                return Err(CliError::new(message, options));
            }
            options.preprocessor_options.push(argument.clone());
            index += 1;
            continue;
        }
        // oxide-vcl's own option; vcl 1.4beta7 has no long options.
        if let Some(name) = argument.strip_prefix("--mode=") {
            let Some(mode) = vcl::Mode::from_name(name) else {
                return Err(CliError::new(
                    CliMessage::BadMode {
                        argument: argument.clone(),
                    },
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
                        options,
                    ));
                }
            }
            index += 1;
            continue;
        }
        // oxide-vcl's own options: which optimizations oxide mode runs, and
        // which it does not.
        let optimization_list = argument
            .strip_prefix("--opt=")
            .map(|list| (list, true))
            .or_else(|| argument.strip_prefix("--no-opt=").map(|list| (list, false)));
        if let Some((list, enable)) = optimization_list {
            if options.compile.mode != vcl::Mode::Oxide {
                return Err(CliError::new(
                    CliMessage::OptimizationsNeedOxideMode {
                        argument: argument.clone(),
                    },
                    options,
                ));
            }
            match parse_optimizations(list) {
                Ok(chosen) if enable => {
                    let earlier = options
                        .compile
                        .optimizations
                        .unwrap_or(vcl::Optimizations::NONE);
                    options.compile.optimizations = Some(earlier.union(chosen));
                }
                Ok(disabled) => {
                    let compile = &mut options.compile;
                    compile.disabled_optimizations = compile.disabled_optimizations.union(disabled);
                }
                Err(name) => {
                    return Err(CliError::new(
                        CliMessage::BadOptimization {
                            argument: argument.clone(),
                            name,
                        },
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
            b'I' => {
                if !value.is_empty() {
                    options.preprocess.include_dirs.push(value.to_owned());
                }
            }
            b'A' | b'a' => options.info(CliMessage::AlwaysOn),
            b'C' => options.compile.disable_size_reduction = true,
            // Register dumps during external allocation: not produced.
            b'D' => {}
            b'F' | b'f' => options.compile.disable_alignment = true,
            b'G' => options.preprocess_cpp = true,
            b'H' | b'h' => {
                options.help = true;
                return Ok(options);
            }
            b'o' => options.output = Some(value.to_owned()),
            b'K' => options.keep_temps = true,
            b'M' => options.compile.timid_memory = true,
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
            b'e' => options.compile.disable_end_bit = true,
            b'g' => match value.as_bytes().first() {
                None => options.preprocess_gasp = true,
                Some(b'+') => options.preprocess_gasp_alternate = true,
                _ => {
                    return Err(CliError::new(
                        CliMessage::BadGaspOption {
                            argument: argument.clone(),
                        },
                        options,
                    ));
                }
            },
            b'j' => options.header = Some(value.to_owned()),
            b'm' => options.compile.mpg = Some(strtol_decimal_i32(value)),
            b'n' => options.compile.new_syntax = true,
            // Microsoft-style `file(line): error:` diagnostics are the only
            // layout now: accepted and ignored.
            b'p' => {}
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
                    options,
                ));
            }
        }
        index += 1;
    }
    if arguments.len() - index > 1 {
        return Err(CliError::new(CliMessage::TooManyArguments, options));
    }
    if let Some(argument) = options.preprocessor_options.first()
        && !(options.preprocess_cpp || options.preprocess_gasp || options.preprocess_gasp_alternate)
    {
        let argument = argument.clone();
        return Err(CliError::new(
            CliMessage::PreprocessorOptionWithoutPreprocessor { argument },
            options,
        ));
    }
    options.preprocess.masp = options.preprocess_gasp;
    // In MASP mode, as in masp, --define sets a variable rather than a macro.
    if options.preprocess.masp {
        options.preprocess.masp_variables = std::mem::take(&mut options.preprocess.defines);
    } else if let Some((name, _)) = options
        .preprocess
        .defines
        .iter()
        .find(|(name, _)| name == "defined" || vcl::preprocessor::is_builtin_macro(name))
    {
        let name = name.clone();
        return Err(CliError::new(
            CliMessage::BadDefine {
                argument: format!("--define={name}"),
                name,
            },
            options,
        ));
    }
    options.input = arguments.get(index).cloned();
    if options.input.is_some() && options.input == options.output {
        return Err(CliError::new(CliMessage::SameInputAndOutput, options));
    }
    Ok(options)
}

/// One of the preprocessor's long options, set in `options`: `None` when
/// `argument` is none of them.
fn preprocessor_option(argument: &str, options: &mut Arguments) -> Option<Result<(), CliMessage>> {
    let settings = &mut options.preprocess;
    match argument {
        "--vcl-boilerplate" => settings.vcl_boilerplate = true,
        "--fold-constants" => settings.fold_constants = true,
        "--flatten-subscripts" => settings.flatten_subscripts = true,
        "--warn-undef" => settings.warn_undef = true,
        "--preprocess-only" => options.preprocess_only = true,
        _ => {
            if let Some(definition) = argument.strip_prefix("--define=") {
                let (name, value) = definition.split_once('=').unwrap_or((definition, "1"));
                if !is_identifier(name) {
                    return Some(Err(CliMessage::BadDefine {
                        argument: argument.to_owned(),
                        name: name.to_owned(),
                    }));
                }
                settings.defines.push((name.to_owned(), value.to_owned()));
            } else {
                let chars = argument.strip_prefix("--comment-char=")?;
                if chars.chars().count() != 1 {
                    return Some(Err(CliMessage::BadCommentChar {
                        argument: argument.to_owned(),
                    }));
                }
                settings.comment_chars.push_str(chars);
            }
        }
    }
    Some(Ok(()))
}

/// A C identifier: a letter or `_`, then letters, digits and `_`.
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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

/// The optimizations an `--opt=` or `--no-opt=` list names: `all`, `none`, or optimization
/// names separated by commas. The error is the first name that is none of
/// these (empty for an empty list).
fn parse_optimizations(list: &str) -> Result<vcl::Optimizations, String> {
    let mut optimizations = vcl::Optimizations::NONE;
    for name in list.split(',') {
        match name {
            "all" => optimizations = optimizations.union(vcl::Optimizations::all()),
            "none" => {}
            _ => {
                let optimization =
                    vcl::Optimization::from_name(name).ok_or_else(|| name.to_owned())?;
                optimizations = optimizations.with(optimization);
            }
        }
    }
    Ok(optimizations)
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
    use vcl::diagnostics::{OXIDE_EXIT_USAGE, VCL_EXIT_FAILURE};

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
        assert_eq!(error.to_string(), "unknown mode 'sce'");
        assert_eq!(error.exit_status(), VCL_EXIT_FAILURE);
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
            vec!["--fix=imm,alias,clip,end,size,operands,entry"],
            vec![
                "--fix=end",
                "--fix=clip,size",
                "--fix=imm,alias,operands",
                "--fix=end,entry",
            ],
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
            ("--fix=alias-warn", "alias-warn"),
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
            assert!(error.to_string().contains("fix"), "{argument}");
            assert_eq!(error.exit_status(), VCL_EXIT_FAILURE);
        }
        let error = parse(["--mode=oxide", "--fix=alias-warn"]).unwrap_err();
        assert_eq!(error.exit_status(), OXIDE_EXIT_USAGE);
        assert!(error.diagnostic.text().contains("unknown fix 'alias-warn'"));
    }

    #[test]
    fn optimizations_are_chosen_in_oxide_mode_only() {
        let oxide = parse(["--mode=oxide", "a.vcl"]).unwrap().compile;
        assert_eq!(oxide.optimizations, None);
        assert_eq!(oxide.enabled_optimizations(), vcl::Optimizations::all());
        for flags in [
            vec!["--mode=oxide", "--opt=none"],
            vec!["--opt=none", "--mode=oxide"],
            vec!["--mode=oxide", "--opt=all", "--opt=none"],
        ] {
            let options = parse(flags.iter().copied().chain(["a.vcl"]))
                .unwrap()
                .compile;
            assert!(options.optimizations.is_some(), "{flags:?}");
        }
        let none = parse(["--mode=oxide", "--opt=none", "a.vcl"])
            .unwrap()
            .compile;
        assert_eq!(none.optimizations, Some(vcl::Optimizations::NONE));
        assert!(none.enabled_optimizations().is_empty());

        let error = parse(["--opt=all", "a.vcl"]).unwrap_err();
        assert!(
            *error.message()
                == CliMessage::OptimizationsNeedOxideMode {
                    argument: "--opt=all".into()
                }
        );
        assert_eq!(error.to_string(), "--opt=all applies only in oxide mode");
        assert_eq!(error.exit_status(), VCL_EXIT_FAILURE);
        let exit = parse(["--mode=oxide", "--opt=none", "--opt=exit", "a.vcl"])
            .unwrap()
            .compile;
        assert!(exit.optimizes(vcl::Optimization::Exit));
        for (argument, name) in [
            ("--opt=", ""),
            ("--opt=fast", "fast"),
            ("--opt=all,", ""),
            ("--opt=None", "None"),
        ] {
            let error = parse(["--mode=oxide", argument, "a.vcl"]).unwrap_err();
            assert!(
                *error.message()
                    == CliMessage::BadOptimization {
                        argument: argument.into(),
                        name: name.into()
                    },
                "{argument}"
            );
            assert!(error.to_string().contains("optimization"), "{argument}");
            assert_eq!(error.exit_status(), OXIDE_EXIT_USAGE);
        }
    }

    #[test]
    fn optimizations_are_turned_off_by_name() {
        let without = |flags: &[&str]| {
            parse(flags.iter().copied().chain(["a.vcl"]))
                .unwrap()
                .compile
                .enabled_optimizations()
        };
        let all = vcl::Optimizations::all();
        let unswitch = vcl::Optimizations::NONE.with(vcl::Optimization::Unswitch);
        // --no-opt= takes names off whatever --opt= chose, in either order.
        for flags in [
            &["--mode=oxide", "--no-opt=unswitch"][..],
            &["--no-opt=unswitch", "--mode=oxide", "--opt=all"],
            &["--mode=oxide", "--no-opt=unswitch", "--opt=all"],
            &["--mode=oxide", "--no-opt=unswitch,unswitch"],
        ] {
            assert_eq!(without(flags), all.without(unswitch), "{flags:?}");
        }
        assert!(without(&["--mode=oxide", "--no-opt=all"]).is_empty());
        assert_eq!(without(&["--mode=oxide", "--no-opt=none"]), all);
        assert!(without(&["--mode=oxide", "--opt=unswitch", "--no-opt=unswitch"]).is_empty());

        let error = parse(["--no-opt=loops", "a.vcl"]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "--no-opt=loops applies only in oxide mode"
        );
        assert_eq!(error.exit_status(), VCL_EXIT_FAILURE);
        let error = parse(["--mode=oxide", "--no-opt=fast", "a.vcl"]).unwrap_err();
        assert!(
            *error.message()
                == CliMessage::BadOptimization {
                    argument: "--no-opt=fast".into(),
                    name: "fast".into()
                }
        );
        assert_eq!(error.exit_status(), OXIDE_EXIT_USAGE);
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
    fn color_is_auto_unless_chosen() {
        assert_eq!(parse(["file.vcl"]).unwrap().color, Color::Auto);
        let options = parse(["--color=never", "--color=always", "file.vcl"]).unwrap();
        assert_eq!(options.color, Color::Always);
        let error = parse(["--color=never", "--color=sometimes", "file.vcl"]).unwrap_err();
        assert_eq!(
            error.message(),
            &CliMessage::BadColor {
                argument: "--color=sometimes".into()
            }
        );
        // The error is printed with the valid setting.
        assert_eq!(error.color, Color::Never);
        assert!(Color::Auto.enabled(true, true));
        assert!(!Color::Auto.enabled(false, true));
        assert!(!Color::Auto.enabled(true, false));
        assert!(Color::Always.enabled(false, false));
        assert!(!Color::Never.enabled(true, true));
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
        assert_eq!(error.exit_status(), OXIDE_EXIT_USAGE);
        let error = parse(["-o", "out.vsm", "--mode=oxide", "file.vcl"]).unwrap_err();
        assert_eq!(error.mode, vcl::Mode::Oxide);
        let error = parse(["-x"]).unwrap_err();
        assert_eq!(
            (error.mode, error.exit_status()),
            (vcl::Mode::Vcl, VCL_EXIT_FAILURE)
        );
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
        assert_eq!(error.to_string(), "more than one input file");
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
        assert_eq!(options.preprocess.include_dirs, ["include"]);
        assert!(
            *parse(["-gbad"]).unwrap_err().message()
                == CliMessage::BadGaspOption {
                    argument: "-gbad".into()
                }
        );
    }

    #[test]
    fn preprocessor_options_need_g_or_big_g() {
        let options = parse([
            "-G",
            "-Ivu1",
            "-Iinclude",
            "--define=kBase=10",
            "--define=FLAG",
            "--vcl-boilerplate",
            "--fold-constants",
            "--flatten-subscripts",
            "--warn-undef",
            "--preprocess-only",
            "in.vcl",
        ])
        .unwrap();
        let settings = &options.preprocess;
        assert!(!settings.masp);
        assert_eq!(settings.include_dirs, ["vu1", "include"]);
        assert_eq!(
            settings.defines,
            [("kBase".into(), "10".into()), ("FLAG".into(), "1".into())]
        );
        assert!(settings.vcl_boilerplate && settings.fold_constants);
        assert!(settings.flatten_subscripts && settings.warn_undef);
        assert!(options.preprocess_only);
        // In MASP mode a definition sets a variable.
        let masp = parse(["--define=count=3", "--comment-char=!", "-g"]).unwrap();
        assert!(masp.preprocess.masp && masp.preprocess.defines.is_empty());
        assert_eq!(
            masp.preprocess.masp_variables,
            [("count".into(), "3".into())]
        );
        assert_eq!(masp.preprocess.comment_chars, "!");
        assert!(
            *parse(["--fold-constants", "in.vcl"]).unwrap_err().message()
                == CliMessage::PreprocessorOptionWithoutPreprocessor {
                    argument: "--fold-constants".into()
                }
        );
        for (argument, name) in [("--define=1x", "1x"), ("--define=__LINE__", "__LINE__")] {
            assert!(
                *parse(["-G", argument]).unwrap_err().message()
                    == CliMessage::BadDefine {
                        argument: format!("--define={name}"),
                        name: name.into()
                    },
                "{argument}"
            );
        }
        assert!(
            *parse(["-g", "--comment-char=ab"]).unwrap_err().message()
                == CliMessage::BadCommentChar {
                    argument: "--comment-char=ab".into()
                }
        );
    }

    #[test]
    fn quiet_applies_in_argument_order_and_keeps_warnings() {
        // -f confirms nothing; -a's note comes before -q, -S's after.
        let early = parse(["-a", "-q", "-f", "-S", "-P", "-Z"]).unwrap();
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
    }

    #[test]
    fn vcl_mode_fails_with_status_255() {
        let error = parse(["--unknown"]).unwrap_err();
        assert_eq!(error.exit_status(), VCL_EXIT_FAILURE);
    }
}
