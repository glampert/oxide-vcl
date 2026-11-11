//! Diagnostics and output: every message the compiler and the `oxide-vcl`
//! driver print, and the writer that prints them.
//!
//! A message is a typed value ([`Message`]), not text. It is rendered when it
//! is printed, in one of two [`Mode`]s:
//!
//! - [`Mode::Vcl`] (the default) keeps vcl 1.4beta7's wording and layout,
//!   which the tests assert;
//! - [`Mode::Oxide`] words every message its own way (`oxide.rs`), prints
//!   it as `file(line): severity: text` with the source line, a caret under
//!   the token it is about and a suggested fix, and ends a failed run with a
//!   count instead of vcl's abort line.
//!
//! [`Reporter`] owns standard output and standard error. The driver writes
//! everything through it: notices, warnings, errors, the help screen and the
//! assembly. Developer traces (the `VCL_*_TRACE` environment switches) go
//! through [`trace!`](crate::trace).

use std::fmt;
use std::io::{self, Write};

use crate::allocator::allocation::AllocationError;

mod oxide;
pub use oxide::Wording;

/// Which compiler this is: vcl 1.4beta7 exactly, or oxide-vcl with its own
/// fixes, optimizations and messages.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Mode {
    /// vcl 1.4beta7's behaviour and messages (the default).
    #[default]
    Vcl,
    /// oxide-vcl's fixes, optimizations and messages (`--mode=oxide`).
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

    /// The message text in `mode`, without prefix or line.
    pub fn text(&self, mode: Mode) -> String {
        self.message.text(mode)
    }

    /// The severity the message is printed with in `mode`.
    pub fn severity_in(&self, mode: Mode) -> Severity {
        match mode {
            Mode::Vcl => self.severity,
            Mode::Oxide => oxide::severity(&self.message, self.severity),
        }
    }

    /// The prefix the line starts with in `mode` (`ERROR: ` and so on).
    pub fn prefix(&self, mode: Mode) -> &'static str {
        match mode {
            Mode::Vcl | Mode::Oxide => self.message.vcl_prefix().unwrap_or(match self.severity {
                Severity::Info => "INFO: ",
                Severity::Warning => "WARNING: ",
                Severity::Error => "ERROR: ",
            }),
        }
    }

    /// The text with its source line: `line N: text`, or the text alone
    /// when there is no line.
    pub fn located(&self, mode: Mode) -> String {
        if self.line == 0 {
            self.text(mode)
        } else {
            format!("line {}: {}", self.line, self.text(mode))
        }
    }
}

/// Renders in vcl mode: `line N: text`, or the text alone without a line.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.located(Mode::Vcl))
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
    /// The message text in `mode`.
    pub fn text(&self, mode: Mode) -> String {
        match mode {
            Mode::Vcl => self.vcl_text(),
            Mode::Oxide => self.oxide_text(),
        }
    }

    /// vcl 1.4beta7's wording, which the port reproduces.
    pub fn vcl_text(&self) -> String {
        match self {
            Message::Cli(message) => message.vcl_text(),
            Message::Driver(message) => message.vcl_text(),
            Message::Parse(message) => message.vcl_text(),
            Message::Compile(message) => message.vcl_text(),
        }
    }

    /// oxide-vcl's wording.
    pub fn oxide_text(&self) -> String {
        self.wording().text
    }

    /// oxide-vcl's wording with the token it is about and a suggested fix.
    pub fn wording(&self) -> Wording {
        oxide::wording(self)
    }

    /// A prefix other than the severity's, as vcl prints it (`USER: `, or
    /// none at all).
    fn vcl_prefix(&self) -> Option<&'static str> {
        match self {
            Message::Cli(CliMessage::BadGaspOption { .. }) => Some("USER: "),
            Message::Cli(CliMessage::TooManyArguments | CliMessage::SameInputAndOutput) => Some(""),
            _ => None,
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.vcl_text())
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
    )*};
}
message_from!(
    Cli(CliMessage),
    Driver(DriverMessage),
    Parse(ParseMessage),
    Compile(CompileMessage)
);

/// Command-line notices and errors (observed from vcl 1.4beta7, `cli-*`
/// probes).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliMessage {
    /// `-a`
    AlwaysOn,
    /// `-C`
    SizeReductionDisabled,
    /// `-f`
    AlignmentDisabled,
    /// `-K`
    KeepTemporaries,
    /// `-M`
    TimidMemory,
    /// `-P`
    PruneDeprecated,
    /// `-S`
    StaggeredMemory,
    /// `-Z`
    ImmediateFixupsDeprecated,
    /// `-e`
    EndBitDisabled,
    /// `-g` followed by anything but `+`.
    BadGaspOption { argument: String },
    /// An unknown switch.
    BadArgument { argument: String },
    /// `--mode=` with an unknown mode.
    BadMode { argument: String },
    /// More than one input file.
    TooManyArguments,
    /// `-o` naming the input file.
    SameInputAndOutput,
}

impl CliMessage {
    pub fn vcl_text(&self) -> String {
        match self {
            CliMessage::AlwaysOn => "-a option is now on always".into(),
            CliMessage::SizeReductionDisabled => "disabling code size reduction pass..".into(),
            CliMessage::AlignmentDisabled => "will skip align emit".into(),
            CliMessage::KeepTemporaries => {
                "keep temporary files in /tmp enabled (/tmp/vclin. etc".into()
            }
            CliMessage::TimidMemory => "timid memory access mode enabled".into(),
            CliMessage::PruneDeprecated => {
                " prune disabled (not) this options was deprecated, and now is disabled".into()
            }
            CliMessage::StaggeredMemory => "stagger ram access...".into(),
            CliMessage::ImmediateFixupsDeprecated => {
                " disabled immediate fixups - this option is now deprecated and removed".into()
            }
            CliMessage::EndBitDisabled => "disabling [e] bit setting".into(),
            CliMessage::BadGaspOption { argument } => {
                format!("bad command line option '{argument}' .. only -g+ or -g allowed")
            }
            // vcl has no long options: `--mode=...` is an unknown switch to it.
            CliMessage::BadArgument { argument } | CliMessage::BadMode { argument } => {
                format!("bad args '{argument}', try -h for help")
            }
            CliMessage::TooManyArguments => "bad args (-o now specifies output file..)".into(),
            CliMessage::SameInputAndOutput => "can't have infile==outfile".into(),
        }
    }
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
    /// vcl's start-up line, printed before the copy-detector failure.
    OsInit,
    /// The copy-detector failure's follow-up lines.
    MovesNotDetected,
    BasicProcessingFailed,
    /// The last line of a failed run, on standard output.
    Abort,
    /// `--warnings-as-errors` failing a run that printed warnings.
    WarningsAsErrors,
}

impl DriverMessage {
    pub fn vcl_text(&self) -> String {
        match self {
            DriverMessage::OpenInput { path, error } => {
                format!("failed to open file '{path}': {error}")
            }
            DriverMessage::ReadInput { error } => error.clone(),
            DriverMessage::NotUtf8 => "source is not valid UTF-8".into(),
            DriverMessage::WriteTemporary { path, error } => {
                format!("failed to write '{path}': {error}")
            }
            DriverMessage::CreateTemporary { path, error } => {
                format!("failed to create '{path}': {error}")
            }
            DriverMessage::ReadTemporary { path, error } => {
                format!("failed to read '{path}': {error}")
            }
            DriverMessage::StartPreprocessor { program, error } => {
                format!("failed to start {program}: {error}")
            }
            DriverMessage::PreprocessorFailed { program, status } => {
                format!("{program} failed: {status}")
            }
            DriverMessage::KeptTemporaries { input, output } => {
                format!("kept {input} and {output}")
            }
            DriverMessage::WriteOutput { path, error } => {
                format!("failed to write '{path}': {error}")
            }
            DriverMessage::WriteStandardOutput { error } => error.clone(),
            DriverMessage::OsInit => "OS_Init(Linux)".into(),
            DriverMessage::MovesNotDetected => "failed to detect moves!".into(),
            DriverMessage::BasicProcessingFailed => {
                "failed to do basic operation processing".into()
            }
            DriverMessage::Abort => "something went wrong, vcl's outta here.".into(),
            // oxide-vcl's own option, so its own message.
            DriverMessage::WarningsAsErrors => {
                "warnings treated as errors (--warnings-as-errors)".into()
            }
        }
    }
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
    MissingDirectiveArgument { directive: String },
    BadRawOption { value: String },
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
    UnknownDirective { name: String },
    RegisterRangeDashes { range: String },
    NotAllocablePhysical { register: String },
    RegisterNotAllocable { register: String },
    UnknownInstruction { opcode: String },
    InvalidInstructionMask,
    LoopIdTooLarge,
    InstructionBits,
    RawOnlyInstruction { name: String },
    MaskNotAccepted { name: String },
    XyzMaskRequired,
    EmptyMask,
    OperandTooLong,
    MissingOperand { position: usize, name: String },
    ScalarNotThird,
    VectorMasksDiffer,
    CannotDerive { name: String },
    SingleComponentRequired,
    MultipleTaggedOperands,
    AccNotFirst,
    MalformedMemoryOperand { operand: String },
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
    OperandMustBe { expected: String },
    ForcedRegisterNotPhysical { register: String },
    EmptyRegisterName,
    RegisterNameTooLong { name: String },
    InvalidRegisterCharacter { character: char, name: String },
    ReservedRegisterName { name: String },
    DuplicateLabel { name: String },
    VisibilityMismatch,
    SyntheticContext,
    LoopSizeInRaw,
    InstructionInEnterExit,
    UnterminatedEnterExit,
    NoEnter,
    NoExit,
}

impl ParseMessage {
    pub fn vcl_text(&self) -> String {
        use ParseMessage::*;
        match self {
            NonAscii => "source contains a non-ASCII character".into(),
            UnexpectedContinuation => "unexpected line continuation".into(),
            UnexpectedComma => "unexpected comma".into(),
            TooManyTokens => "too many tokens on a line (maximum 200)".into(),
            EmptyLabel => "empty label".into(),
            LabelBits => "invalid E/M/D/T bits on a label".into(),
            MultipleSynthetics => "only one synthetic instruction is allowed on a line".into(),
            MultipleInstructions => "more than one instruction on a line in VCL mode".into(),
            RawPairRequired => "raw mode requires two instructions on each instruction line".into(),
            MissingDirectiveArgument { directive } => format!("missing argument to {directive}"),
            BadRawOption { value } => format!("bad option '{value}' to raw directive"),
            BadSyntax => ".syntax expects 'new' or 'old'".into(),
            RawInRawLoop => "raw directive inside a raw loop".into(),
            EndRawOutsideRaw => "endraw directive outside raw mode".into(),
            EndRawInRawLoop => "endraw directive inside a raw loop".into(),
            NestedRawLoop => "nested raw loops are invalid".into(),
            EndRawLoopOutsideRawLoop => "endrawloop directive outside a raw loop".into(),
            CoreWorkArea => "core work areas are not implemented in the original binary".into(),
            WorkAreaDevice => "work area device must be core, vu0 or vu1".into(),
            WorkAreaOutsideMemory => "work area is outside device memory".into(),
            UniqueTooLong => "unique name too long".into(),
            UnknownDirective { name } => format!("unknown directive '{name}'"),
            RegisterRangeDashes { range } => {
                format!("too many '-' characters in register range '{range}'")
            }
            NotAllocablePhysical { register } => {
                format!("not an allocable physical register: '{register}'")
            }
            RegisterNotAllocable { register } => format!("register '{register}' is not allocable"),
            UnknownInstruction { opcode } => format!("unknown instruction '{opcode}'"),
            InvalidInstructionMask => "invalid instruction component mask".into(),
            LoopIdTooLarge => "instruction loop ID is larger than 32".into(),
            InstructionBits => "invalid E/M/D/T bits for instruction context".into(),
            RawOnlyInstruction { name } => {
                format!("instruction '{name}' is supported only in raw mode")
            }
            MaskNotAccepted { name } => {
                format!("instruction '{name}' does not accept a component mask")
            }
            XyzMaskRequired => "XYZ instruction requires mask xyz".into(),
            EmptyMask => "empty instruction mask".into(),
            OperandTooLong => "gathered operand exceeds 1023 characters".into(),
            MissingOperand { position, name } => format!("missing operand {position} for '{name}'"),
            ScalarNotThird => {
                "a scalar can only be the third operand in a generalized instruction".into()
            }
            VectorMasksDiffer => "vector operand component masks do not match".into(),
            CannotDerive { name } => format!("cannot derive instruction '{name}'"),
            SingleComponentRequired => "instruction requires a single component".into(),
            MultipleTaggedOperands => "only one tagged operand is allowed per instruction".into(),
            AccNotFirst => "ACC can only be the first vector operand".into(),
            MalformedMemoryOperand { operand } => {
                format!("failed to decode memory operand '{operand}'")
            }
            PostincrementRequired => "postincrement operand requires (register++)".into(),
            PredecrementRequired => "predecrement operand requires (--register)".into(),
            InvalidMemoryMask => "invalid memory component mask".into(),
            MemoryMaskRequired => "memory component mask is required".into(),
            MemoryMaskDiffers => "memory and instruction component masks differ".into(),
            GeneralizedScalarComponent => {
                "generalized scalar requires one selected component".into()
            }
            DuplicateComponent => "duplicate operand component selection".into(),
            InvalidOperandMask => "invalid operand component mask".into(),
            ScalarComponentRequired => "scalar operand requires one selected component".into(),
            FieldSelectionsDiffer => "instruction and operand field selections differ".into(),
            OperandMustBe { expected } => format!("operand must be {expected}"),
            ForcedRegisterNotPhysical { register } => {
                format!("forced register '{register}' is not physical")
            }
            EmptyRegisterName => "empty register name".into(),
            RegisterNameTooLong { name } => format!("register name '{name}' is too long"),
            InvalidRegisterCharacter { character, name } => {
                format!("can't use '{character}' in the name of a register : '{name}'")
            }
            ReservedRegisterName { name } => {
                format!("instruction name '{name}' is reserved as a register name")
            }
            DuplicateLabel { name } => format!("duplicate label '{name}'"),
            VisibilityMismatch => "visibility scope mode mismatch".into(),
            SyntheticContext => "synthetic instruction in invalid enter/exit context".into(),
            LoopSizeInRaw => "loop-size synthetic is invalid in raw mode".into(),
            InstructionInEnterExit => "ordinary instruction inside an enter/exit block".into(),
            UnterminatedEnterExit => "unterminated enter/exit block".into(),
            NoEnter => "source contains no enter block".into(),
            NoExit => "source contains no exit block".into(),
        }
    }
}

impl fmt::Display for ParseMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.vcl_text())
    }
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
    /// A failure inside one of the port's own phases, with that phase's
    /// description. vcl has no counterpart message.
    Internal(String),
}

impl CompileMessage {
    pub fn vcl_text(&self) -> String {
        use CompileMessage::*;
        match self {
            ReservedName { name, kind } => format!("can't use {name} as a name for one of the {kind}"),
            InvalidPhysicalRegister { name } => format!("invalid physical register {name}"),
            InvalidComponentMask => "invalid component mask".into(),
            UnmappableBase { name } => {
                format!("can't figure out which base register to map {name} to")
            }
            ConflictingBases { first, second } => format!(
                "regweb:: set map - can't map to different base registers.. try to map both {first} and {second}"
            ),
            BindingChanged { name, from, to } => {
                format!("changing binding for {name} from {from} to {to}")
            }
            UsedBeforeSet {
                register,
                component,
            } => format!("{register}.{component} used before set"),
            OperandPoolUnsupported => "operand register pool is not implemented for this target".into(),
            ExplicitMacWrite => "explicit mac register - but I'm writing ASTATUS here??".into(),
            MoveDestinationUnbound => "move destination is not bound".into(),
            ZeroCopyDestination => "found a move to a const zero register".into(),
            LoopCsKnownCount => {
                "for --LoopCS, the minimum known count (arg 1) must be must be >0".into()
            }
            LoopCsSafeCount => {
                "for --LoopCS, the minimum safe count (arg 2) must be must be >=0".into()
            }
            LoopAbsCount => "failed to get immediate field 0 (extra loop count) while processing --LoopAbs. value must be >=2".into(),
            LoopExtraCount => "failed to get immediate field 0 (extra loop count) while processing --LoopExtra. value must be >=0".into(),
            MissingLoopHint => "missing loop hint operand".into(),
            LoopHintNotConstant => "loop hint must be a numeric constant".into(),
            LoopPeriodRange => "loop period exceeds supported schedule range".into(),
            UnknownSynthetic => "unknown synthetic instruction".into(),
            ContWithEndMode => "CONT directive encountered - but end mode isn't null".into(),
            // oxide-vcl's own message: vcl reads the name the same way silently.
            FieldLettersInName {
                token,
                name,
                fields,
            } => format!(
                "'{token}' is read as register '{name}' with field {fields}: old syntax takes a name's trailing x, y, z and w as fields; rename it, or write '{name}.{fields}' if the field is meant"
            ),
            RawBranchDelay => "raw branch requires one explicit delay pair".into(),
            RawBranchDelayBeforeBoundary => {
                "raw branch requires one explicit delay pair before a boundary".into()
            }
            BranchInRawDelaySlot => "branch inside a raw branch delay slot".into(),
            WrongRawPipe => "instruction is in the wrong raw issue pipe".into(),
            StagedRawLoopBranch => {
                "staged raw loop requires a terminating conditional self branch".into()
            }
            StagedRawLoopTarget => "staged raw loop branch must target its own stream".into(),
            LoopIdOnNop => "loop id set on a nop".into(),
            InvalidStagedRawPair => "invalid staged raw pair".into(),
            RawLoopPeriodRange => "raw loop period exceeds supported range".into(),
            InvalidRawLoopStage => "invalid raw loop stage".into(),
            MaxLoopIdZero => "max loop id 0 in loop".into(),
            RawLoopWithoutBranch => "raw loop has no branch".into(),
            TooManyLoops => "too many loop constructs".into(),
            DuplicateStreamLabel { label } => format!("duplicate label {label}"),
            PipesWriteSameRegister => {
                "invalid combination: both issue pipes write the same register".into()
            }
            UnknownBranchTarget { target } => format!("unknown branch target {target}"),
            UnknownBranchTag { tag } => format!("unknown indirect branch tag {tag}"),
            DuplicateBranchTag => "addNewRef - already present".into(),
            UntaggedIndirectBranch => "indirect branches require explicit target tags".into(),
            Allocation(failure) => failure.to_string(),
            AllocationPoolEmpty { vector } => format!(
                "vcl failed to allocate a register of type {}, but there's no registers of this type available for it to use.. maybe adding a .init_.. directive would help",
                register_kind(*vector)
            ),
            AllocationPoolFull { vector, register } => {
                let directive = if *vector { ".init_vf" } else { ".init_vi" };
                format!(
                    "vcl failed to allocate a register of type {} for '{register}': every allowed register is in use; {directive} or {directive}_all allows more",
                    register_kind(*vector)
                )
            }
            Internal(detail) => detail.clone(),
        }
    }

    /// Warnings that carry their line in the text in vcl's layout.
    fn vcl_line_in_text(&self) -> bool {
        matches!(
            self,
            CompileMessage::LoopCsKnownCount
                | CompileMessage::LoopCsSafeCount
                | CompileMessage::LoopAbsCount
                | CompileMessage::FieldLettersInName { .. }
        )
    }
}

fn register_kind(vector: bool) -> &'static str {
    if vector {
        "VU User floating point"
    } else {
        "VU User integer"
    }
}

impl fmt::Display for CompileMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.vcl_text())
    }
}

/// How compile errors name their location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorStyle<'a> {
    /// `line N: message` (vcl's default).
    Lines,
    /// `file(line): error: message` (`-p`).
    Microsoft { file: &'a str },
}

/// The writer for everything the driver prints. In [`Mode::Vcl`] each method
/// reproduces vcl 1.4beta7's layout byte for byte as the port prints it; in
/// [`Mode::Oxide`] every diagnostic is `location: severity: text` with its
/// source line and a suggested fix (see [`Reporter::set_source`]).
pub struct Reporter<O: Write = io::Stdout, E: Write = io::Stderr> {
    mode: Mode,
    out: O,
    err: E,
    source: Option<Source>,
    warnings_as_errors: bool,
    errors: usize,
    warnings: usize,
}

/// The program being compiled, for oxide mode's locations and source lines.
struct Source {
    file: String,
    lines: Vec<String>,
}

impl Reporter {
    /// A reporter on the process's standard output and standard error.
    pub fn stdio(mode: Mode) -> Self {
        Self::new(mode, io::stdout(), io::stderr())
    }
}

impl<O: Write, E: Write> Reporter<O, E> {
    pub fn new(mode: Mode, out: O, err: E) -> Self {
        Self {
            mode,
            out,
            err,
            source: None,
            warnings_as_errors: false,
            errors: 0,
            warnings: 0,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// The source file's name and the text the compiler reads (after
    /// preprocessing), which oxide mode quotes under each diagnostic.
    pub fn set_source(&mut self, file: &str, text: &str) {
        self.source = Some(Source {
            file: file.to_string(),
            lines: text.lines().map(str::to_string).collect(),
        });
    }

    /// `--warnings-as-errors`: oxide mode prints each warning as an error.
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

    /// A notice or warning on standard error: `PREFIX: text`.
    pub fn notice(&mut self, diagnostic: &Diagnostic) {
        if self.mode == Mode::Oxide {
            self.render(diagnostic);
            return;
        }
        if diagnostic.severity == Severity::Warning {
            self.warnings += 1;
        }
        let line = match &diagnostic.message {
            Message::Compile(message) if diagnostic.line != 0 && message.vcl_line_in_text() => {
                diagnostic.located(self.mode)
            }
            _ => diagnostic.text(self.mode),
        };
        self.stderr(format_args!("{}{line}\n", diagnostic.prefix(self.mode)));
    }

    /// A command-line error, after its notices: the message and then the
    /// abort line on standard output.
    pub fn command_line_error(&mut self, diagnostic: &Diagnostic) {
        if self.mode == Mode::Oxide {
            self.render(diagnostic);
            return;
        }
        // The port ends the message with a blank line.
        self.stderr(format_args!(
            "{}{}\n\n",
            diagnostic.prefix(self.mode),
            diagnostic.text(self.mode)
        ));
        self.abort();
    }

    /// A failure that stops the run: the error, then the abort line.
    pub fn failure(&mut self, diagnostic: &Diagnostic) {
        if self.mode == Mode::Oxide {
            self.render(diagnostic);
            return;
        }
        self.stderr(format_args!(
            "{}{}\n",
            diagnostic.prefix(self.mode),
            diagnostic.located(self.mode)
        ));
        self.abort();
    }

    /// Compile errors, one per line under a single `ERROR: ` prefix, then
    /// the abort line. Oxide mode prints each in full, then the count.
    pub fn compile_errors(&mut self, errors: &[Diagnostic], style: ErrorStyle<'_>) {
        if self.mode == Mode::Oxide {
            for error in errors {
                self.render(error);
            }
            self.summary();
            return;
        }
        let lines: Vec<String> = errors
            .iter()
            .map(|error| match style {
                ErrorStyle::Lines => error.located(self.mode),
                ErrorStyle::Microsoft { file } => {
                    format!("{file}({}): error: {}", error.line, error.text(self.mode))
                }
            })
            .collect();
        self.stderr(format_args!("ERROR: {}\n", lines.join("\n")));
        self.abort();
    }

    /// `--warnings-as-errors` failing a run whose only problems were
    /// warnings, already printed.
    pub fn warnings_failed(&mut self) {
        if self.mode == Mode::Oxide {
            self.summary();
        } else {
            self.failure(&Diagnostic::error(DriverMessage::WarningsAsErrors));
        }
    }

    /// vcl's copy-detector failure, which exits with status 0: the start-up
    /// line, the error and its two follow-up errors.
    pub fn copy_detection_failure(&mut self, error: &Diagnostic) {
        let mode = self.mode;
        let lines = [
            Diagnostic::info(DriverMessage::OsInit),
            Diagnostic::error(error.message.clone()),
            Diagnostic::error(DriverMessage::MovesNotDetected),
            Diagnostic::error(DriverMessage::BasicProcessingFailed),
        ];
        for line in &lines {
            self.stderr(format_args!("{}{}\n", line.prefix(mode), line.text(mode)));
        }
    }

    /// The last line of a failed run, on standard output.
    pub fn abort(&mut self) {
        let abort = Diagnostic::error(DriverMessage::Abort);
        let text = format!("{}{}\n", abort.prefix(self.mode), abort.text(self.mode));
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
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

    /// The assembly listing, on standard output. In vcl mode it sits between
    /// vcl's `VCL CODE` banner and five closing separators, which an `-o`
    /// file does not get (observed from vcl 1.4beta7, `cli-output-file`).
    pub fn assembly(&mut self, text: &str) -> io::Result<()> {
        const SEPARATOR: &str = ";-------------------------\n";
        if self.mode == Mode::Vcl {
            let banner =
                format!("{SEPARATOR}{SEPARATOR};-----VCL CODE------------\n{SEPARATOR}{SEPARATOR}");
            self.out.write_all(banner.as_bytes())?;
        }
        self.out.write_all(text.as_bytes())?;
        if self.mode == Mode::Vcl {
            self.out.write_all(SEPARATOR.repeat(5).as_bytes())?;
        }
        self.out.flush()
    }

    /// One diagnostic in oxide layout:
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
        let mut severity = diagnostic.severity_in(Mode::Oxide);
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

    /// Oxide mode's last line of a failed compile.
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
/// `VCL_*` environment variables at each call site; they are not part of
/// either mode's output.
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

    fn capture(report: impl FnOnce(&mut Reporter<Vec<u8>, Vec<u8>>)) -> (String, String) {
        let mut reporter = Reporter::new(Mode::Vcl, Vec::new(), Vec::new());
        report(&mut reporter);
        let (out, err) = reporter.into_inner();
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
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
    fn notices_use_their_severity_prefix() {
        let (out, err) = capture(|reporter| {
            reporter.notice(&Diagnostic::info(CliMessage::AlwaysOn));
            reporter.notice(&Diagnostic::warning(CliMessage::TimidMemory));
        });
        assert_eq!(out, "");
        assert_eq!(
            err,
            "INFO: -a option is now on always\nWARNING: timid memory access mode enabled\n"
        );
    }

    #[test]
    fn loop_hint_warnings_carry_their_line_in_the_text() {
        let (_, err) = capture(|reporter| {
            reporter.notice(&Diagnostic::warning(CompileMessage::LoopCsKnownCount).at(7));
            reporter.notice(
                &Diagnostic::warning(CompileMessage::BindingChanged {
                    name: "a".into(),
                    from: "VF01".into(),
                    to: "VF02".into(),
                })
                .at(9),
            );
        });
        assert_eq!(
            err,
            "WARNING: line 7: for --LoopCS, the minimum known count (arg 1) must be must be >0\n\
             WARNING: changing binding for a from VF01 to VF02\n"
        );
    }

    #[test]
    fn command_line_errors_keep_their_prefixes() {
        let (out, err) = capture(|reporter| {
            reporter.command_line_error(&Diagnostic::error(CliMessage::BadGaspOption {
                argument: "-gbad".into(),
            }));
            reporter.command_line_error(&Diagnostic::error(CliMessage::SameInputAndOutput));
        });
        assert_eq!(
            err,
            "USER: bad command line option '-gbad' .. only -g+ or -g allowed\n\n\
             can't have infile==outfile\n\n"
        );
        assert_eq!(
            out,
            "ERROR: something went wrong, vcl's outta here.\n".repeat(2)
        );
    }

    #[test]
    fn compile_errors_share_one_prefix() {
        let errors = [
            Diagnostic::error(ParseMessage::UnknownInstruction {
                opcode: "frob".into(),
            })
            .at(5),
            Diagnostic::error(ParseMessage::UnknownDirective {
                name: ".bogus".into(),
            })
            .at(6),
        ];
        let (out, err) = capture(|reporter| reporter.compile_errors(&errors, ErrorStyle::Lines));
        assert_eq!(
            err,
            "ERROR: line 5: unknown instruction 'frob'\nline 6: unknown directive '.bogus'\n"
        );
        assert_eq!(out, "ERROR: something went wrong, vcl's outta here.\n");
        let (_, err) = capture(|reporter| {
            reporter.compile_errors(&errors[..1], ErrorStyle::Microsoft { file: "a.vcl" })
        });
        assert_eq!(err, "ERROR: a.vcl(5): error: unknown instruction 'frob'\n");
    }

    #[test]
    fn copy_detection_failure_prints_vcl_follow_up_lines() {
        let error = Diagnostic::error(CompileMessage::ZeroCopyDestination).at(3);
        let (out, err) = capture(|reporter| reporter.copy_detection_failure(&error));
        assert_eq!(out, "");
        assert_eq!(
            err,
            "INFO: OS_Init(Linux)\n\
             ERROR: found a move to a const zero register\n\
             ERROR: failed to detect moves!\n\
             ERROR: failed to do basic operation processing\n"
        );
    }

    fn capture_oxide(
        warnings_as_errors: bool,
        report: impl FnOnce(&mut Reporter<Vec<u8>, Vec<u8>>),
    ) -> String {
        let mut reporter = Reporter::new(Mode::Oxide, Vec::new(), Vec::new());
        reporter.set_source("prog.vcl", "--enter\n\tad res, src, src\n--endenter\n");
        reporter.set_warnings_as_errors(warnings_as_errors);
        report(&mut reporter);
        let (out, err) = reporter.into_inner();
        assert!(
            out.is_empty(),
            "oxide mode prints nothing on stdout for diagnostics"
        );
        String::from_utf8(err).unwrap()
    }

    #[test]
    fn oxide_errors_quote_the_line_and_underline_the_token() {
        let error = Diagnostic::error(ParseMessage::UnknownInstruction {
            opcode: "ad".into(),
        })
        .at(2);
        let err = capture_oxide(false, |reporter| {
            reporter.compile_errors(&[error], ErrorStyle::Lines)
        });
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
    fn oxide_locates_by_file_alone_without_a_line() {
        let err = capture_oxide(false, |reporter| {
            reporter.notice(&Diagnostic::warning(ParseMessage::NoExit));
            reporter.notice(&Diagnostic::info(CliMessage::AlwaysOn));
        });
        assert_eq!(
            err,
            "prog.vcl: warning: the program has no --exit block\n  \
             = help: end it with --exit ... --endexit (or --cont), declaring its outputs with out_vf/out_vi\n\
             oxide-vcl: note: -a has no effect: what it enabled is always on\n"
        );
    }

    #[test]
    fn warnings_as_errors_promote_in_oxide_and_count() {
        let warning = Diagnostic::warning(CliMessage::PruneDeprecated);
        let err = capture_oxide(true, |reporter| {
            reporter.notice(&warning);
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
        let err = capture_oxide(false, |reporter| {
            reporter.notice(&warning);
            reporter.compile_errors(
                &[Diagnostic::error(ParseMessage::EmptyLabel).at(9)],
                ErrorStyle::Lines,
            );
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
    fn vcl_mode_fails_warnings_with_its_own_error() {
        let (out, err) = capture(|reporter| {
            reporter.set_warnings_as_errors(true);
            reporter.notice(&Diagnostic::warning(CliMessage::PruneDeprecated));
            reporter.warnings_failed();
        });
        assert_eq!(
            err,
            "WARNING:  prune disabled (not) this options was deprecated, and now is disabled\n\
             ERROR: warnings treated as errors (--warnings-as-errors)\n"
        );
        assert_eq!(out, "ERROR: something went wrong, vcl's outta here.\n");
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
        let message = Message::from(CompileMessage::UsedBeforeSet {
            register: "base".into(),
            component: "x".into(),
        });
        assert_eq!(message.to_string(), "base.x used before set");
        assert!(
            message
                == CompileMessage::UsedBeforeSet {
                    register: "base".into(),
                    component: "x".into()
                }
        );
    }
}
