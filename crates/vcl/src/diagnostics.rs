//! Diagnostics and output: every message the compiler and the `oxide-vcl`
//! driver print, and the writer that prints them.
//!
//! A message is a typed value ([`Message`]), not text. Its wording
//! (`oxide.rs`) is a sentence, the token it is about and a suggested fix,
//! and [`Reporter`] prints it as `file(line): severity: text` with the
//! source line, a caret under the token and the fix, whatever the compile
//! [`Mode`]. vcl 1.4beta7's own message texts are not reproduced: what the
//! port matches is the code it produces.
//!
//! [`Reporter`] owns standard output and standard error. The driver writes
//! everything through it: notices, warnings, errors, the help screen and the
//! assembly. Developer traces go through [`trace!`](crate::trace) and
//! [`trace_if!`](crate::trace_if), switched on by [`crate::debug`].

use std::fmt;
use std::io::{self, Write};

use crate::allocator::allocation::AllocationError;

mod oxide;
pub use oxide::Wording;

/// Which compiler this is: vcl 1.4beta7's behaviour, or oxide-vcl's with
/// its fixes and optimizations. It does not change how messages are printed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Mode {
    /// vcl 1.4beta7's behaviour (the default).
    #[default]
    Vcl,
    /// oxide-vcl's fixes and optimizations (`--mode=oxide`).
    Oxide,
}

impl Mode {
    /// The mode's name on the command line.
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Vcl => "vcl",
            Mode::Oxide => "oxide",
        }
    }

    /// The mode a command-line name selects.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "vcl" => Some(Mode::Vcl),
            "oxide" => Some(Mode::Oxide),
            _ => None,
        }
    }
}

/// The exit status of a failed run in vcl mode, whatever failed: vcl
/// 1.4beta7's `exit(-1)` as its parent sees it.
pub const VCL_EXIT_FAILURE: u8 = 255;
/// The exit status of a failed run in oxide mode: the program does not
/// compile, a file can't be read or written, or a warning fails the run.
pub const OXIDE_EXIT_FAILURE: u8 = 1;
/// The exit status of a bad command line in oxide mode.
pub const OXIDE_EXIT_USAGE: u8 = 2;

impl Mode {
    /// The exit status of a failed run.
    pub const fn failure_status(self) -> u8 {
        match self {
            Mode::Vcl => VCL_EXIT_FAILURE,
            Mode::Oxide => OXIDE_EXIT_FAILURE,
        }
    }

    /// The exit status of a bad command line.
    pub const fn usage_status(self) -> u8 {
        match self {
            Mode::Vcl => VCL_EXIT_FAILURE,
            Mode::Oxide => OXIDE_EXIT_USAGE,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One message with its severity and, for source diagnostics, its line
/// (0 when it has none).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: usize,
    pub message: Message,
}

impl Diagnostic {
    pub fn info(message: impl Into<Message>) -> Self {
        Self::new(Severity::Info, 0, message)
    }

    pub fn warning(message: impl Into<Message>) -> Self {
        Self::new(Severity::Warning, 0, message)
    }

    pub fn error(message: impl Into<Message>) -> Self {
        Self::new(Severity::Error, 0, message)
    }

    pub fn new(severity: Severity, line: usize, message: impl Into<Message>) -> Self {
        Self {
            severity,
            line,
            message: message.into(),
        }
    }

    pub fn at(mut self, line: usize) -> Self {
        self.line = line;
        self
    }

    /// The message text, without location.
    pub fn text(&self) -> String {
        self.message.text()
    }

    /// The text with its source line: `line N: text`, or the text alone
    /// when there is no line.
    pub fn located(&self) -> String {
        if self.line == 0 {
            self.text()
        } else {
            format!("line {}: {}", self.line, self.text())
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.located())
    }
}

impl std::error::Error for Diagnostic {}

/// Every message, grouped by where it comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Cli(CliMessage),
    Driver(DriverMessage),
    Parse(ParseMessage),
    Compile(CompileMessage),
}

impl Message {
    /// The message's one-line text.
    pub fn text(&self) -> String {
        self.wording().text
    }

    /// The text with the token it is about and a suggested fix.
    pub fn wording(&self) -> Wording {
        oxide::wording(self)
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

macro_rules! message_from {
    ($($variant:ident($kind:ty)),*) => {$(
        impl From<$kind> for Message {
            fn from(message: $kind) -> Self {
                Message::$variant(message)
            }
        }
        impl PartialEq<$kind> for Message {
            fn eq(&self, other: &$kind) -> bool {
                matches!(self, Message::$variant(message) if message == other)
            }
        }
        impl fmt::Display for $kind {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&Message::$variant(self.clone()).text())
            }
        }
    )*};
}
message_from!(
    Cli(CliMessage),
    Driver(DriverMessage),
    Parse(ParseMessage),
    Compile(CompileMessage)
);

/// Command-line notices and errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliMessage {
    /// `-a`, which has no effect.
    AlwaysOn,
    /// `-P`, deprecated.
    PruneDeprecated,
    /// `-S`, which has no effect.
    StaggeredMemory,
    /// `-Z`, deprecated.
    ImmediateFixupsDeprecated,
    /// `-g` followed by anything but `+`.
    BadGaspOption { argument: String },
    /// An unknown switch.
    BadArgument { argument: String },
    /// `--mode=` with an unknown mode.
    BadMode { argument: String },
    /// `--fix=` naming no fix, or an unknown one (`name`).
    BadFix { argument: String, name: String },
    /// `--opt=` naming no optimization, or an unknown one (`name`).
    BadOptimization { argument: String, name: String },
    /// `--opt=` without `--mode=oxide`: vcl mode never optimizes.
    OptimizationsNeedOxideMode { argument: String },
    /// More than one input file.
    TooManyArguments,
    /// `-o` naming the input file.
    SameInputAndOutput,
}

/// The driver's own failures and notes: files, standard streams and the
/// preprocessors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DriverMessage {
    OpenInput {
        path: String,
        error: String,
    },
    ReadInput {
        error: String,
    },
    NotUtf8,
    WriteTemporary {
        path: String,
        error: String,
    },
    CreateTemporary {
        path: String,
        error: String,
    },
    ReadTemporary {
        path: String,
        error: String,
    },
    StartPreprocessor {
        program: String,
        error: String,
    },
    PreprocessorFailed {
        program: String,
        status: String,
    },
    KeptTemporaries {
        input: String,
        output: String,
    },
    WriteOutput {
        path: String,
        error: String,
    },
    WriteStandardOutput {
        error: String,
    },
    /// `--warnings-as-errors` failing a run that printed warnings.
    WarningsAsErrors,
}

/// Source errors found while parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseMessage {
    NonAscii,
    UnexpectedContinuation,
    UnexpectedComma,
    TooManyTokens,
    EmptyLabel,
    LabelBits,
    MultipleSynthetics,
    MultipleInstructions,
    RawPairRequired,
    MissingDirectiveArgument {
        directive: String,
    },
    BadRawOption {
        value: String,
    },
    BadSyntax,
    RawInRawLoop,
    EndRawOutsideRaw,
    EndRawInRawLoop,
    NestedRawLoop,
    EndRawLoopOutsideRawLoop,
    CoreWorkArea,
    WorkAreaDevice,
    WorkAreaOutsideMemory,
    UniqueTooLong,
    UnknownDirective {
        name: String,
    },
    RegisterRangeDashes {
        range: String,
    },
    NotAllocablePhysical {
        register: String,
    },
    RegisterNotAllocable {
        register: String,
    },
    UnknownInstruction {
        opcode: String,
    },
    InvalidInstructionMask,
    LoopIdTooLarge,
    InstructionBits,
    RawOnlyInstruction {
        name: String,
    },
    MaskNotAccepted {
        name: String,
    },
    XyzMaskRequired,
    EmptyMask,
    OperandTooLong,
    MissingOperand {
        position: usize,
        name: String,
    },
    ScalarNotThird,
    VectorMasksDiffer,
    CannotDerive {
        name: String,
    },
    SingleComponentRequired,
    MultipleTaggedOperands,
    AccNotFirst,
    MalformedMemoryOperand {
        operand: String,
    },
    /// An immediate that does not parse, or (with `type_name`, vcl's name
    /// for the target type) parses but cannot be evaluated; `detail` says why.
    BadImmediate {
        instruction: String,
        expression: String,
        detail: String,
        type_name: Option<String>,
        /// A memory operand's displacement rather than an immediate operand.
        offset: bool,
    },
    PostincrementRequired,
    PredecrementRequired,
    InvalidMemoryMask,
    MemoryMaskRequired,
    MemoryMaskDiffers,
    GeneralizedScalarComponent,
    DuplicateComponent,
    InvalidOperandMask,
    ScalarComponentRequired,
    FieldSelectionsDiffer,
    OperandMustBe {
        expected: String,
    },
    ForcedRegisterNotPhysical {
        register: String,
    },
    EmptyRegisterName,
    RegisterNameTooLong {
        name: String,
    },
    InvalidRegisterCharacter {
        character: char,
        name: String,
    },
    ReservedRegisterName {
        name: String,
    },
    DuplicateLabel {
        name: String,
    },
    VisibilityMismatch,
    SyntheticContext,
    LoopSizeInRaw,
    InstructionInEnterExit,
    UnterminatedEnterExit,
    NoEnter,
    NoExit,
}

/// Errors and warnings found while compiling a parsed program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileMessage {
    // Names and bindings.
    ReservedName {
        name: String,
        kind: String,
    },
    InvalidPhysicalRegister {
        name: String,
    },
    InvalidComponentMask,
    UnmappableBase {
        name: String,
    },
    ConflictingBases {
        first: String,
        second: String,
    },
    BindingChanged {
        name: String,
        from: String,
        to: String,
    },
    UsedBeforeSet {
        register: String,
        component: String,
    },
    OperandPoolUnsupported,
    ExplicitMacWrite,
    MoveDestinationUnbound,
    ZeroCopyDestination,
    // Loop hints and synthetics.
    LoopCsKnownCount,
    LoopCsSafeCount,
    LoopAbsCount,
    LoopExtraCount,
    MissingLoopHint,
    LoopHintNotConstant,
    LoopPeriodRange,
    UnknownSynthetic,
    ContWithEndMode,
    /// Old syntax read a symbolic name's trailing letters as a field
    /// selection: `token` is the name as written, `name` and `fields` how it
    /// was read.
    FieldLettersInName {
        token: String,
        name: String,
        fields: String,
    },
    // Raw code.
    RawBranchDelay,
    RawBranchDelayBeforeBoundary,
    BranchInRawDelaySlot,
    WrongRawPipe,
    StagedRawLoopBranch,
    StagedRawLoopTarget,
    LoopIdOnNop,
    InvalidStagedRawPair,
    RawLoopPeriodRange,
    InvalidRawLoopStage,
    MaxLoopIdZero,
    RawLoopWithoutBranch,
    TooManyLoops,
    // Control flow.
    DuplicateStreamLabel {
        label: String,
    },
    PipesWriteSameRegister,
    UnknownBranchTarget {
        target: String,
    },
    UnknownBranchTag {
        tag: String,
    },
    DuplicateBranchTag,
    UntaggedIndirectBranch,
    // Allocation.
    Allocation(AllocationError),
    /// No register of the family is allowed at all (no `.init_vf`/`.init_vi`).
    AllocationPoolEmpty {
        vector: bool,
    },
    /// Every allowed register of the family is in use.
    AllocationPoolFull {
        vector: bool,
        register: String,
    },
    /// A work area in vcl mode, which vcl 1.4beta7 rejects.
    WorkAreaNeedsOxideMode,
    // Spilling to a work area (oxide mode only; vcl has no counterpart).
    /// A value did not fit in the registers and lives in the work area.
    RegisterSpilled {
        name: String,
        vector: bool,
        device: String,
        address: u32,
    },
    /// Registers still do not fit, and every qword of the work areas holds a
    /// spilled value.
    WorkAreaFull {
        vector: bool,
        register: String,
        size: u32,
    },
    /// A work area's address or size is not a number.
    WorkAreaNumber {
        token: String,
    },
    // Fixes for vcl 1.4beta7's defects (`--fix=`); vcl has no counterpart.
    /// A numeric immediate outside its instruction's encoding field, which
    /// the assembler would truncate.
    ImmediateOutOfField {
        instruction: String,
        value: String,
        low: i64,
        high: i64,
    },
    /// A flag-test mask with bits past the flag's width. Those bits do not
    /// exist, so the test is the same without them.
    FlagMaskTruncated {
        instruction: String,
        value: String,
        bits: u8,
    },
    /// A symbolic immediate in a narrow field: only the assembler can check it.
    ImmediateUnchecked {
        instruction: String,
        value: String,
        bits: u8,
    },
    /// More instructions than VU1 micro memory holds.
    ProgramTooLarge {
        rows: usize,
    },
    /// A `loi` expression whose function call cannot be evaluated (oxide
    /// mode): `detail` says why.
    LoiFunction {
        expression: String,
        detail: String,
    },
    /// A failure inside one of the port's own phases, with that phase's
    /// description. vcl has no counterpart message.
    Internal(String),
}

/// The writer for everything the driver prints. Every diagnostic is
/// `location: severity: text` with its source line and a suggested fix (see
/// [`Reporter::set_source`]).
pub struct Reporter<O: Write = io::Stdout, E: Write = io::Stderr> {
    out: O,
    err: E,
    source: Option<Source>,
    warnings_as_errors: bool,
    errors: usize,
    warnings: usize,
}

/// The program being compiled, for locations and source lines.
struct Source {
    file: String,
    lines: Vec<String>,
}

impl Reporter {
    /// A reporter on the process's standard output and standard error.
    pub fn stdio() -> Self {
        Self::new(io::stdout(), io::stderr())
    }
}

impl<O: Write, E: Write> Reporter<O, E> {
    pub fn new(out: O, err: E) -> Self {
        Self {
            out,
            err,
            source: None,
            warnings_as_errors: false,
            errors: 0,
            warnings: 0,
        }
    }

    /// The source file's name and the text the compiler reads (after
    /// preprocessing), quoted under each diagnostic.
    pub fn set_source(&mut self, file: &str, text: &str) {
        self.source = Some(Source {
            file: file.to_string(),
            lines: text.lines().map(str::to_string).collect(),
        });
    }

    /// `--warnings-as-errors`: each warning is printed as an error.
    pub fn set_warnings_as_errors(&mut self, enabled: bool) {
        self.warnings_as_errors = enabled;
    }

    /// The warnings printed so far, before `--warnings-as-errors` promotes
    /// them.
    pub fn warnings(&self) -> usize {
        self.warnings
    }

    /// The writers, for tests that capture output.
    pub fn into_inner(self) -> (O, E) {
        (self.out, self.err)
    }

    /// A note or warning, or an error that stops the run without a count
    /// (a command line or a file the driver cannot use).
    pub fn report(&mut self, diagnostic: &Diagnostic) {
        self.render(diagnostic);
    }

    /// Compile errors, each in full, then the count.
    pub fn compile_errors(&mut self, errors: &[Diagnostic]) {
        for error in errors {
            self.render(error);
        }
        self.summary();
    }

    /// `--warnings-as-errors` failing a run whose only problems were
    /// warnings, already printed.
    pub fn warnings_failed(&mut self) {
        self.summary();
    }

    /// The version line (`-v`), on standard output whatever `-q` says.
    pub fn version(&mut self, text: &str) {
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }

    /// The help screen, on standard error.
    pub fn help(&mut self, text: &str) {
        self.stderr(format_args!("{text}"));
    }

    /// The assembly listing, on standard output, between vcl's `VCL CODE`
    /// banner and five closing separators, which an `-o` file does not get
    /// (observed from vcl 1.4beta7, `cli-output-file`).
    pub fn assembly(&mut self, text: &str) -> io::Result<()> {
        const SEPARATOR: &str = ";-------------------------\n";
        let banner =
            format!("{SEPARATOR}{SEPARATOR};-----VCL CODE------------\n{SEPARATOR}{SEPARATOR}");
        self.out.write_all(banner.as_bytes())?;
        self.out.write_all(text.as_bytes())?;
        self.out.write_all(SEPARATOR.repeat(5).as_bytes())?;
        self.out.flush()
    }

    /// One diagnostic:
    ///
    /// ```text
    /// prog.vcl(7): error: unknown instruction 'ad'
    ///     7 |     ad res, src, src
    ///       |     ^^
    ///       = help: did you mean 'add'?
    /// ```
    ///
    /// Source diagnostics are located by file and line (the file alone when
    /// there is no line); command-line and driver ones by `oxide-vcl`.
    fn render(&mut self, diagnostic: &Diagnostic) {
        let mut severity = diagnostic.severity;
        let mut suffix = "";
        if severity == Severity::Warning {
            self.warnings += 1;
            if self.warnings_as_errors {
                severity = Severity::Error;
                suffix = " [--warnings-as-errors]";
            }
        }
        if severity == Severity::Error {
            self.errors += 1;
        }
        let label = match severity {
            Severity::Info => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        let wording = diagnostic.message.wording();
        let in_source = matches!(diagnostic.message, Message::Parse(_) | Message::Compile(_));
        let source = self.source.as_ref().filter(|_| in_source);
        let location = match source {
            Some(source) if diagnostic.line != 0 => format!("{}({})", source.file, diagnostic.line),
            Some(source) => source.file.clone(),
            None => "oxide-vcl".to_string(),
        };
        let mut text = format!("{location}: {label}: {}{suffix}\n", wording.text);
        let quoted = source
            .and_then(|source| source.lines.get(diagnostic.line.checked_sub(1)?))
            .map(|line| expand_tabs(line.trim_end()));
        let gutter = " ".repeat(diagnostic.line.to_string().len() + 4);
        if let Some(line) = &quoted {
            text.push_str(&format!(
                "{:>width$} | {line}\n",
                diagnostic.line,
                width = gutter.len() - 1
            ));
            if let Some((start, length)) = wording
                .subject
                .as_deref()
                .and_then(|subject| find_token(line, subject))
            {
                text.push_str(&format!(
                    "{gutter}| {}{}\n",
                    " ".repeat(start),
                    "^".repeat(length)
                ));
            }
        }
        if let Some(help) = &wording.help {
            let indent = if quoted.is_some() {
                gutter.as_str()
            } else {
                "  "
            };
            text.push_str(&format!("{indent}= help: {help}\n"));
        }
        self.stderr(format_args!("{text}"));
    }

    /// The last line of a failed compile.
    fn summary(&mut self) {
        let plural = |count: usize, word: &str| {
            format!("{count} {word}{}", if count == 1 { "" } else { "s" })
        };
        let errors = plural(self.errors.max(1), "error");
        // Under --warnings-as-errors the warnings were counted as errors.
        let warnings = if self.warnings_as_errors {
            0
        } else {
            self.warnings
        };
        let counts = if warnings == 0 {
            errors
        } else {
            format!("{errors}, {}", plural(warnings, "warning"))
        };
        self.stderr(format_args!("oxide-vcl: {counts}; no output written\n"));
    }

    fn stderr(&mut self, text: fmt::Arguments<'_>) {
        let _ = self.err.write_fmt(text);
        let _ = self.err.flush();
    }
}

/// Tabs as spaces to the next multiple of four, so a caret line lines up.
fn expand_tabs(line: &str) -> String {
    let mut expanded = String::new();
    for c in line.chars() {
        if c == '\t' {
            expanded.push(' ');
            while !expanded.len().is_multiple_of(4) {
                expanded.push(' ');
            }
        } else {
            expanded.push(c);
        }
    }
    expanded
}

/// Where `token` appears in `line`, ignoring case: preferably as a whole
/// word, else anywhere. Returns the start and length in characters.
fn find_token(line: &str, token: &str) -> Option<(usize, usize)> {
    if token.is_empty() || !line.is_ascii() || !token.is_ascii() {
        return None;
    }
    let haystack = line.to_ascii_lowercase();
    let needle = token.to_ascii_lowercase();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let matches: Vec<usize> = haystack.match_indices(&needle).map(|(at, _)| at).collect();
    let whole = matches.iter().copied().find(|&at| {
        !word(haystack[..at].chars().next_back())
            && !word(haystack[at + needle.len()..].chars().next())
    });
    whole
        .or_else(|| matches.first().copied())
        .map(|at| (at, needle.len()))
}

/// Print a developer trace line on standard error. Traces are switched on by
/// `VCL_*` environment variables ([`crate::debug`]); see also
/// [`trace_if!`](crate::trace_if). They are not part of the compiler's
/// output.
#[macro_export]
macro_rules! trace {
    ($($argument:tt)*) => {
        $crate::diagnostics::trace_line(format_args!($($argument)*))
    };
}

#[doc(hidden)]
pub fn trace_line(text: fmt::Arguments<'_>) {
    let _ = writeln!(io::stderr(), "{text}");
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(
        warnings_as_errors: bool,
        report: impl FnOnce(&mut Reporter<Vec<u8>, Vec<u8>>),
    ) -> String {
        let mut reporter = Reporter::new(Vec::new(), Vec::new());
        reporter.set_source("prog.vcl", "--enter\n\tad res, src, src\n--endenter\n");
        reporter.set_warnings_as_errors(warnings_as_errors);
        report(&mut reporter);
        let (out, err) = reporter.into_inner();
        assert!(out.is_empty(), "diagnostics print nothing on stdout");
        String::from_utf8(err).unwrap()
    }

    #[test]
    fn modes_have_command_line_names() {
        assert_eq!(Mode::default(), Mode::Vcl);
        for mode in [Mode::Vcl, Mode::Oxide] {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(Mode::from_name("sce"), None);
    }

    #[test]
    fn errors_quote_the_line_and_underline_the_token() {
        let error = Diagnostic::error(ParseMessage::UnknownInstruction {
            opcode: "ad".into(),
        })
        .at(2);
        let err = capture(false, |reporter| reporter.compile_errors(&[error]));
        assert_eq!(
            err,
            "prog.vcl(2): error: unknown instruction 'ad'\n   \
             2 |     ad res, src, src\n     \
             |     ^^\n     \
             = help: did you mean 'add'?\n\
             oxide-vcl: 1 error; no output written\n"
        );
    }

    #[test]
    fn diagnostics_without_a_line_name_the_file_or_the_driver() {
        let err = capture(false, |reporter| {
            reporter.report(&Diagnostic::warning(ParseMessage::NoExit));
            reporter.report(&Diagnostic::info(CliMessage::AlwaysOn));
        });
        assert_eq!(
            err,
            "prog.vcl: warning: the program has no --exit block\n  \
             = help: end it with --exit ... --endexit (or --cont), declaring its outputs with out_vf/out_vi\n\
             oxide-vcl: note: -a has no effect: what it enabled is always on\n"
        );
    }

    #[test]
    fn warnings_as_errors_promote_and_count() {
        let warning = Diagnostic::warning(CliMessage::PruneDeprecated);
        let err = capture(true, |reporter| {
            reporter.report(&warning);
            assert_eq!(reporter.warnings(), 1);
            reporter.warnings_failed();
        });
        assert_eq!(
            err,
            "oxide-vcl: error: -P is deprecated and has no effect [--warnings-as-errors]\n  \
             = help: remove it from the command line\n\
             oxide-vcl: 1 error; no output written\n"
        );
        // Without the flag, warnings are counted beside the errors.
        let err = capture(false, |reporter| {
            reporter.report(&warning);
            reporter.compile_errors(&[Diagnostic::error(ParseMessage::EmptyLabel).at(9)]);
        });
        assert!(
            err.ends_with(
                "prog.vcl(9): error: a label with no name\n\
                 oxide-vcl: 1 error, 1 warning; no output written\n"
            ),
            "{err}"
        );
    }

    #[test]
    fn the_listing_sits_between_vcls_banners() {
        let mut reporter = Reporter::new(Vec::new(), Vec::new());
        reporter.assembly("\t\t.vu\n").unwrap();
        let (out, err) = reporter.into_inner();
        let out = String::from_utf8(out).unwrap();
        assert!(err.is_empty());
        assert!(out.starts_with(";-------------------------\n"));
        assert!(out.contains(";-----VCL CODE------------\n"));
        assert!(out.ends_with(&";-------------------------\n".repeat(5)));
    }

    #[test]
    fn tokens_are_found_as_words_first() {
        assert_eq!(find_token("add cat, caty, cat", "cat"), Some((4, 3)));
        assert_eq!(find_token("add caty, src, src", "CATY"), Some((4, 4)));
        assert_eq!(find_token("addx res, src", "src"), Some((10, 3)));
        assert_eq!(find_token("nop", "add"), None);
        assert_eq!(expand_tabs("\tad\tx"), "    ad  x");
    }

    #[test]
    fn messages_compare_with_their_group() {
        let used = CompileMessage::UsedBeforeSet {
            register: "base".into(),
            component: "x".into(),
        };
        let message = Message::from(used.clone());
        assert_eq!(message.to_string(), message.wording().text);
        assert!(message == used);
    }
}
