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
/// (0 when it has none). A diagnostic about a file other than the one being
/// compiled, such as one the preprocessor reads, carries its own
/// [`SourceLocation`] instead, and may come with [`Note`]s: the chain of
/// includes or macro expansions behind it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: usize,
    pub message: Message,
    pub location: Option<SourceLocation>,
    pub notes: Vec<Note>,
}

/// A place in a named source file. Lines and columns count from 1; 0 means
/// unknown (a column of 0 leaves the caret to the message's subject).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct SourceLocation {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

impl SourceLocation {
    pub fn new(file: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            file: file.into(),
            line,
            column,
        }
    }
}

/// A line of context printed after a diagnostic, at its own place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note {
    pub location: Option<SourceLocation>,
    pub message: Message,
}

impl Note {
    pub fn new(location: Option<SourceLocation>, message: impl Into<Message>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }
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
            location: None,
            notes: Vec::new(),
        }
    }

    pub fn at(mut self, line: usize) -> Self {
        self.line = line;
        self
    }

    /// The diagnostic at a place in a named file, with its line as `line`.
    pub fn located_at(mut self, location: SourceLocation) -> Self {
        self.line = location.line;
        self.location = Some(location);
        self
    }

    /// The diagnostic with notes after it.
    pub fn with_notes(mut self, notes: Vec<Note>) -> Self {
        self.notes = notes;
        self
    }

    /// The message text, without location.
    pub fn text(&self) -> String {
        self.message.text()
    }

    /// The text with its source line: `line N: text` (`file(N): text` for
    /// one with a location), or the text alone when there is no line.
    pub fn located(&self) -> String {
        if let Some(location) = &self.location {
            if location.line == 0 {
                format!("{}: {}", location.file, self.text())
            } else {
                format!("{}({}): {}", location.file, location.line, self.text())
            }
        } else if self.line == 0 {
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
    Preprocess(PreprocessMessage),
    Frontend(FrontendMessage),
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
    Compile(CompileMessage),
    Preprocess(PreprocessMessage),
    Frontend(FrontendMessage)
);

/// A message from a frontend built on the compiler, such as VUSL's, already
/// worded by it: the frontend keeps its own typed messages and prints them
/// through [`Reporter`] as these.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrontendMessage {
    /// The one-line message.
    pub text: String,
    /// The token it is about, underlined in the quoted source line.
    pub subject: Option<String>,
    /// A suggested fix or an explanation, printed as `= help: ...`.
    pub help: Option<String>,
    /// About the program's source: a diagnostic with only a line is located
    /// in the program (through its line origins), as a compile error is.
    /// Otherwise it is about the command line or the driver, and is located
    /// by the program's name.
    pub in_source: bool,
}

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
    /// `--color=` naming neither `auto`, `always` nor `never`.
    BadColor { argument: String },
    /// `--fix=` naming no fix, or an unknown one (`name`).
    BadFix { argument: String, name: String },
    /// `--opt=` naming no optimization, or an unknown one (`name`).
    BadOptimization { argument: String, name: String },
    /// `--opt=` without `--mode=oxide`: vcl mode never optimizes.
    OptimizationsNeedOxideMode { argument: String },
    /// A preprocessor option (`--define=`, `--preprocess-only`, ...) without
    /// `-G` or `-g`.
    PreprocessorOptionWithoutPreprocessor { argument: String },
    /// `--define=` with a name that cannot be a macro or variable.
    BadDefine { argument: String, name: String },
    /// `--comment-char=` without exactly one character.
    BadCommentChar { argument: String },
    /// More than one input file.
    TooManyArguments,
    /// `-o` naming the input file.
    SameInputAndOutput,
}

/// The driver's own failures and notes: files and standard streams.
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
    /// `-g+`: gasp's alternate syntax, which the preprocessor does not take.
    AlternateSyntax,
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

/// The preprocessor's errors, warnings and notes (`-G`, `-g`). Their texts
/// are vclpp's, the preprocessor the built-in one follows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreprocessMessage {
    // Notes.
    /// The `#include` that led to the file a diagnostic is in.
    IncludedFrom,
    /// More `#include`s in a long chain, left out.
    MoreIncludes {
        count: usize,
    },
    /// The invocation of the macro the code came out of.
    InExpansionOf {
        name: String,
    },
    /// More expansions in a long chain, left out.
    MoreExpansions {
        count: usize,
    },
    /// Where an `#include` looked for its file.
    NotFoundAt {
        path: String,
    },
    /// `<file>` with no `-I` directories to look in.
    NoIncludeDirectories,
    FirstOneHere,
    FirstUsedHere,
    PreviousDefinitionHere,
    DefinedHere {
        name: String,
    },

    // Lexing.
    Lex {
        error: crate::preprocessor::lexer::LexErrorKind,
    },
    LexWarning {
        warning: crate::preprocessor::lexer::LexWarningKind,
    },

    // Directives.
    UnknownDirective {
        name: String,
    },
    /// `#error` and the rest of its line, with a space before it.
    ErrorDirective {
        text: String,
    },
    /// `#warning` and the rest of its line, with a space before it.
    WarningDirective {
        text: String,
    },
    ExtraTokens {
        directive: String,
    },
    /// `#pragma` other than `once`, with the rest of its line.
    UnknownPragma {
        text: String,
    },
    ExpectedMacroName {
        directive: String,
    },
    DefinedAsMacroName,
    ChangeBuiltin {
        name: String,
        undefining: bool,
    },
    /// A `#define` after code that names the macro, which it does not apply to.
    DefinedAfterUse {
        name: String,
    },
    Redefined {
        name: String,
    },

    // #include.
    IncludeExpectsFile,
    IncludeMissingGreater,
    IncludeEmptyName,
    IncludeTooDeep {
        depth: usize,
    },
    IncludeNotFound {
        name: String,
    },
    IncludeUnreadable {
        path: String,
    },

    // Function-like macros.
    MissingParameterClose {
        name: String,
    },
    ExpectedParameterBeforeClose {
        name: String,
    },
    EllipsisNotLast {
        name: String,
    },
    ExpectedCommaOrClose {
        name: String,
    },
    ExpectedParameterName {
        name: String,
    },
    DuplicateParameter {
        parameter: String,
        name: String,
    },
    HashWithoutParameter {
        name: String,
    },
    PasteAtEnd {
        name: String,
        block: bool,
    },

    // Block macros.
    BlockMacroWithoutColon {
        name: String,
    },
    LostCommaBeforeFirst {
        name: String,
    },
    LostCommaAfter {
        parameter: String,
        name: String,
    },
    MissingCommaAfter {
        parameter: String,
        name: String,
    },
    ExtraneousComma {
        parameter: String,
        name: String,
    },
    UnterminatedMacro {
        name: String,
    },
    TextAfterEndmacro,
    DirectiveInMacro {
        directive: String,
        name: String,
    },
    EndmacroWithoutMacro,

    // Programs.
    VuprogTwice,
    EndvuprogTwice,
    ExpectedProgramName,
    TextAfterProgramName,
    EndvuprogWithoutVuprog,
    TextAfterEndvuprog,
    MissingVuprog,
    MissingEndvuprog,

    // Conditionals.
    UnterminatedConditional {
        directive: String,
    },
    WithoutIf {
        directive: String,
    },
    ElifAfterElse,
    ElseAfterElse,
    DefinedWithoutName,
    DefinedMissingClose {
        name: String,
    },
    NoExpression {
        directive: String,
    },
    /// `--warn-undef`: a name left in an `#if` after expansion.
    NotDefinedIsZero {
        name: String,
    },

    // Integer expressions, in `context` (`#if`, `constant expression`, ...).
    FloatInExpression {
        context: String,
        number: String,
    },
    NotInExpression {
        context: String,
        token: String,
    },
    Expression {
        context: String,
        error: crate::preprocessor::expression::ExpressionError,
    },

    // Expansion.
    NotABlockMacro {
        name: String,
    },
    RecursiveBlockMacro {
        name: String,
    },
    BlockMacroInContext {
        name: String,
        context: String,
    },
    UnterminatedArguments {
        name: String,
    },
    UnbalancedArgument {
        token: String,
        name: String,
    },
    TooFewArguments {
        name: String,
        at_least: usize,
        given: usize,
    },
    WrongArgumentCount {
        name: String,
        takes: usize,
        given: usize,
    },
    EmptyBlockArgument {
        index: usize,
        parameter: String,
        name: String,
    },
    InvalidPaste {
        left: String,
        right: String,
    },

    // MASP mode (`-g`).
    /// `#macro`, `#endmacro`, `#vuprog` or `#endvuprog`, which are not masp's.
    NotInMaspMode {
        directive: String,
    },
    MaspUnterminatedAif,
    MaspEndlessWhile {
        passes: u64,
    },
    MaspMissingEnd,
    /// A `.macro`, `.arepeat` or `.awhile` without its closing directive.
    MaspUnterminatedBlock {
        open: String,
        close: String,
    },
    /// `.endm`, `.aendr` or `.aendw` with nothing open.
    MaspCloseWithoutOpen {
        close: String,
        open: String,
    },
    /// `.assigna` or `.assignc` without a name in column 1.
    MaspAssignNeedsName {
        directive: String,
    },
    /// A GASP directive that masp carries out itself, such as `.sdata`.
    MaspRefusedDirective {
        name: String,
    },
    MaspExpectedMacroName,
    MaspMissingParameterName {
        name: String,
    },
    MaspNotAParameterName {
        parameter: String,
        name: String,
    },
    MaspExpansionTooDeep {
        depth: usize,
        name: String,
    },
    MaspNoSuchParameter {
        name: String,
        parameter: String,
    },
    MaspPositionalAfterNamed {
        argument: String,
        name: String,
    },
    MaspTooManyArguments {
        name: String,
        takes: usize,
    },
    MaspExitmOutsideMacro,
    MaspAelseWithoutAif,
    MaspAelseAfterAelse,
    MaspAendiWithoutAif,
    MaspRepeatTooMany {
        count: i64,
        max: u64,
    },
    MaspIncludeExpectsFile,
    MaspIncludeEmptyName,
    MaspIncludeTooDeep {
        depth: usize,
    },
    MaspUndefinedVariable {
        name: String,
    },
    /// A quoted string compared with something other than a string.
    MaspStringComparison {
        context: String,
    },
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
    /// A `--call` block not followed by a JALR (oxide mode).
    CallWithoutJalr,
    /// A `--call` declaration that is not a physical register or a
    /// hardware resource.
    CallRegisterNotPhysical {
        name: String,
    },
    /// `--return` without an integer register to return through.
    ReturnRegister,
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
        /// When the value crosses a library call (oxide mode): the values that
        /// cross calls, which a work area saves across them first.
        crossing: Vec<String>,
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
    /// A value crossing a library call did not fit in the registers left to
    /// it, and is saved in the work area across each call.
    RegisterSavedAcrossCalls {
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
    files: Vec<Source>,
    /// For each line of the program, the file and line it came from, when a
    /// preprocessor made it.
    line_origins: Vec<Option<SourceLocation>>,
    warnings_as_errors: bool,
    /// Whether diagnostics carry ANSI colors ([`Reporter::set_color`]).
    color: bool,
    /// The name diagnostics with no file are located by, and the summary
    /// starts with ([`Reporter::set_program_name`]).
    program: String,
    errors: usize,
    warnings: usize,
}

/// The ANSI styles of a colored diagnostic, as clang and gcc color theirs:
/// the place and the message bold, the severity in its color, the caret
/// green.
mod style {
    pub const BOLD: &str = "1";
    pub const ERROR: &str = "1;31";
    pub const WARNING: &str = "1;33";
    pub const NOTE: &str = "1;36";
    pub const HELP: &str = "1;36";
    pub const CARET: &str = "1;32";
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
            files: Vec::new(),
            line_origins: Vec::new(),
            warnings_as_errors: false,
            color: false,
            program: "oxide-vcl".to_string(),
            errors: 0,
            warnings: 0,
        }
    }

    /// The driver's name, `oxide-vcl` unless another driver says otherwise:
    /// command-line and driver diagnostics are located by it, and the
    /// summary of a failed run starts with it.
    pub fn set_program_name(&mut self, name: &str) {
        self.program = name.to_string();
    }

    /// The source file's name and the text the compiler reads (after
    /// preprocessing), quoted under each diagnostic.
    pub fn set_source(&mut self, file: &str, text: &str) {
        self.source = Some(Source {
            file: file.to_string(),
            lines: text.lines().map(str::to_string).collect(),
        });
    }

    /// Another file whose lines diagnostics may quote, such as one the
    /// preprocessor read. A file added twice keeps its first text.
    pub fn add_source(&mut self, file: &str, text: &str) {
        if self.source_named(file).is_none() {
            self.files.push(Source {
                file: file.to_string(),
                lines: text.lines().map(str::to_string).collect(),
            });
        }
    }

    /// Where each line of the program came from, for a preprocessed one:
    /// source diagnostics name that file and line, and quote that line.
    pub fn set_line_origins(&mut self, origins: Vec<Option<SourceLocation>>) {
        self.line_origins = origins;
    }

    /// `--warnings-as-errors`: each warning is printed as an error.
    pub fn set_warnings_as_errors(&mut self, enabled: bool) {
        self.warnings_as_errors = enabled;
    }

    /// `--color`: diagnostics in ANSI colors, for a terminal. Only what goes
    /// to standard error is colored; the listing never is.
    pub fn set_color(&mut self, enabled: bool) {
        self.color = enabled;
    }

    /// `text` in the ANSI style `code` when colors are on, else as it is.
    fn paint(&self, text: &str, code: &str) -> String {
        if self.color && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
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

    /// Text as it is, on standard output: `--preprocess-only`'s source.
    pub fn text(&mut self, text: &str) -> io::Result<()> {
        self.out.write_all(text.as_bytes())?;
        self.out.flush()
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
    /// there is no line); command-line and driver ones by the program's
    /// name, `oxide-vcl` by default.
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
        let mut text = String::new();
        let indent = self.render_one(
            &mut text,
            severity,
            suffix,
            &diagnostic.message,
            diagnostic.location.as_ref(),
            diagnostic.line,
        );
        for note in &diagnostic.notes {
            // A note with no place of its own reads as part of the message.
            if note.location.is_none() {
                let label = self.paint("note", style::NOTE);
                text.push_str(&format!("{indent}= {label}: {}\n", note.message.text()));
                continue;
            }
            let line = note.location.as_ref().map_or(0, |location| location.line);
            self.render_one(
                &mut text,
                Severity::Info,
                "",
                &note.message,
                note.location.as_ref(),
                line,
            );
        }
        self.stderr(format_args!("{text}"));
    }

    /// One message, its quoted line and its help, at `location` when it has
    /// one, else at `line` of the program being compiled. Returns the indent
    /// of the lines under it.
    fn render_one(
        &self,
        text: &mut String,
        severity: Severity,
        suffix: &str,
        message: &Message,
        location: Option<&SourceLocation>,
        line: usize,
    ) -> String {
        let label = match severity {
            Severity::Info => self.paint("note", style::NOTE),
            Severity::Warning => self.paint("warning", style::WARNING),
            Severity::Error => self.paint("error", style::ERROR),
        };
        let wording = message.wording();
        let in_source = match message {
            Message::Parse(_) | Message::Compile(_) | Message::Preprocess(_) => true,
            Message::Frontend(message) => message.in_source,
            Message::Cli(_) | Message::Driver(_) => false,
        };
        // A line of preprocessed code is shown where it came from.
        let origin = match location {
            None if in_source => line
                .checked_sub(1)
                .and_then(|index| self.line_origins.get(index))
                .and_then(Option::as_ref)
                .filter(|origin| origin.line != 0),
            _ => None,
        };
        let line = origin.map_or(line, |origin| origin.line);
        let (file, source, column) = match location.or(origin) {
            Some(location) => (
                Some(location.file.as_str()),
                self.source_named(&location.file),
                location.column,
            ),
            None => {
                let source = self.source.as_ref().filter(|_| in_source);
                (source.map(|source| source.file.as_str()), source, 0)
            }
        };
        let place = match file {
            Some(file) if line != 0 => format!("{file}({line})"),
            Some(file) => file.to_string(),
            None => self.program.clone(),
        };
        let place = self.paint(&format!("{place}:"), style::BOLD);
        let headline = self.paint(&format!("{}{suffix}", wording.text), style::BOLD);
        text.push_str(&format!("{place} {label}: {headline}\n"));
        let raw = source.and_then(|source| source.lines.get(line.checked_sub(1)?));
        let quoted = raw.map(|line| expand_tabs(line.trim_end()));
        let gutter = " ".repeat(line.to_string().len() + 4);
        if let (Some(raw), Some(quoted)) = (raw, &quoted) {
            text.push_str(&format!(
                "{:>width$} | {quoted}\n",
                line,
                width = gutter.len() - 1
            ));
            let underline = if column != 0 {
                caret_at(raw, column, wording.subject.as_deref())
            } else {
                wording
                    .subject
                    .as_deref()
                    .and_then(|subject| find_token(quoted, subject))
            };
            if let Some((start, length)) = underline {
                text.push_str(&format!(
                    "{gutter}| {}{}\n",
                    " ".repeat(start),
                    self.paint(&"^".repeat(length), style::CARET)
                ));
            }
        }
        let indent = if quoted.is_some() {
            gutter
        } else {
            "  ".to_string()
        };
        if let Some(help) = &wording.help {
            let label = self.paint("help", style::HELP);
            text.push_str(&format!("{indent}= {label}: {help}\n"));
        }
        indent
    }

    /// The source a location names: a file added with
    /// [`Reporter::add_source`], or the program being compiled.
    fn source_named(&self, file: &str) -> Option<&Source> {
        // A preprocessor's files first: the program, under the input's name,
        // is then the preprocessed text, and the file its first version.
        self.files
            .iter()
            .chain(&self.source)
            .find(|source| source.file == file)
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
        let program = self.program.clone();
        self.stderr(format_args!("{program}: {counts}; no output written\n"));
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

/// The caret under a diagnostic with a column: from that column (counted in
/// characters of the raw line, tabs as one) for the subject's length when the
/// subject is written there, else for one character. Returns the start and
/// length in the tab-expanded line.
fn caret_at(raw: &str, column: usize, subject: Option<&str>) -> Option<(usize, usize)> {
    let before: String = raw.chars().take(column - 1).collect();
    if before.chars().count() != column - 1 {
        return None;
    }
    let start = expand_tabs(&before).len();
    let rest: String = raw.chars().skip(column - 1).collect();
    let length = subject
        .filter(|subject| {
            !subject.is_empty()
                && rest
                    .to_ascii_lowercase()
                    .starts_with(&subject.to_ascii_lowercase())
        })
        .map_or(1, |subject| subject.chars().count());
    Some((start, length))
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
    fn colors_mark_the_severity_the_message_and_the_caret() {
        let error = Diagnostic::error(ParseMessage::UnknownInstruction {
            opcode: "ad".into(),
        })
        .at(2);
        let mut reporter = Reporter::new(Vec::new(), Vec::new());
        reporter.set_source("prog.vcl", "--enter\n\tad res, src, src\n--endenter\n");
        reporter.set_color(true);
        reporter.compile_errors(&[error]);
        reporter.report(&Diagnostic::warning(ParseMessage::NoExit));
        reporter.report(&Diagnostic::info(CliMessage::AlwaysOn));
        let (_, err) = reporter.into_inner();
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "\x1b[1mprog.vcl(2):\x1b[0m \x1b[1;31merror\x1b[0m: \x1b[1munknown instruction 'ad'\x1b[0m\n   \
             2 |     ad res, src, src\n     \
             |     \x1b[1;32m^^\x1b[0m\n     \
             = \x1b[1;36mhelp\x1b[0m: did you mean 'add'?\n\
             oxide-vcl: 1 error; no output written\n\
             \x1b[1mprog.vcl:\x1b[0m \x1b[1;33mwarning\x1b[0m: \x1b[1mthe program has no --exit block\x1b[0m\n  \
             = \x1b[1;36mhelp\x1b[0m: end it with --exit ... --endexit (or --cont), declaring its outputs with out_vf/out_vi\n\
             \x1b[1moxide-vcl:\x1b[0m \x1b[1;36mnote\x1b[0m: \x1b[1m-a has no effect: what it enabled is always on\x1b[0m\n"
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
    fn located_diagnostics_quote_their_own_file_and_notes_follow() {
        let mut reporter = Reporter::new(Vec::new(), Vec::new());
        reporter.set_source("prog.vcl", "unused\n");
        reporter.add_source("lib.i", "#define P(a) a\n\tP(+)\n");
        let error = Diagnostic::error(PreprocessMessage::IncludedFrom)
            .located_at(SourceLocation::new("lib.i", 1, 9))
            .with_notes(vec![Note::new(
                Some(SourceLocation::new("lib.i", 2, 2)),
                PreprocessMessage::InExpansionOf { name: "P".into() },
            )]);
        reporter.compile_errors(&[error]);
        let (_, err) = reporter.into_inner();
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "lib.i(1): error: in the file included from here\n   \
             1 | #define P(a) a\n     \
             |         ^\n\
             lib.i(2): note: in expansion of macro 'P'\n   \
             2 |     P(+)\n     \
             |     ^\n\
             oxide-vcl: 1 error; no output written\n"
        );
    }

    #[test]
    fn preprocessed_lines_are_shown_where_they_came_from() {
        let mut reporter = Reporter::new(Vec::new(), Vec::new());
        reporter.set_source("prog.vcl", ".vu\n\tad res, src, src\n");
        reporter.add_source("lib.i", "#macro M\n    ad res, src, src\n#endmacro\n");
        reporter.set_line_origins(vec![None, Some(SourceLocation::new("lib.i", 2, 0))]);
        let error = Diagnostic::error(ParseMessage::UnknownInstruction {
            opcode: "ad".into(),
        })
        .at(2);
        reporter.compile_errors(&[error]);
        let (_, err) = reporter.into_inner();
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.starts_with(
                "lib.i(2): error: unknown instruction 'ad'\n   \
                 2 |     ad res, src, src\n     \
                 |     ^^\n"
            ),
            "{err}"
        );
    }

    #[test]
    fn frontend_messages_are_located_where_the_frontend_says() {
        let mut reporter = Reporter::new(Vec::new(), Vec::new());
        reporter.set_program_name("oxide-vusl");
        reporter.add_source("prog.vusl", "void main()\n{\n    vec2 a = b.yx;\n}\n");
        let message = FrontendMessage {
            text: "'b.yx' reorders lanes".into(),
            subject: Some("b.yx".into()),
            help: Some("move the lanes with a masked assignment".into()),
            in_source: true,
        };
        let error = Diagnostic::error(message).located_at(SourceLocation::new("prog.vusl", 3, 14));
        let usage = Diagnostic::error(FrontendMessage {
            text: "no input file".into(),
            ..FrontendMessage::default()
        });
        reporter.report(&usage);
        reporter.compile_errors(&[error]);
        let (_, err) = reporter.into_inner();
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "oxide-vusl: error: no input file\n\
             prog.vusl(3): error: 'b.yx' reorders lanes\n   \
             3 |     vec2 a = b.yx;\n     \
             |              ^^^^\n     \
             = help: move the lanes with a masked assignment\n\
             oxide-vusl: 2 errors; no output written\n"
        );
    }

    #[test]
    fn frontend_messages_with_a_line_follow_the_line_origins() {
        let mut reporter = Reporter::new(Vec::new(), Vec::new());
        reporter.set_source("prog.vcl", ".vu\n\tadd res, src, src\n");
        reporter.add_source("prog.vusl", "a = b + b;\n");
        reporter.set_line_origins(vec![None, Some(SourceLocation::new("prog.vusl", 1, 0))]);
        let message = FrontendMessage {
            text: "out of registers".into(),
            in_source: true,
            ..FrontendMessage::default()
        };
        reporter.compile_errors(&[Diagnostic::error(message).at(2)]);
        let (_, err) = reporter.into_inner();
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.starts_with("prog.vusl(1): error: out of registers\n   1 | a = b + b;\n"),
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
