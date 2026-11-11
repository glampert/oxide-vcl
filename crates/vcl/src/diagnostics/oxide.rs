//! oxide-vcl's own wording for every message (`--mode=oxide`): a clearer
//! sentence, the token in the source line it is about (for the caret
//! underline) and, where there is one, a suggested fix.
//!
//! None of this is vcl 1.4beta7's text; vcl mode never uses it.

use super::{CliMessage, CompileMessage, DriverMessage, Message, ParseMessage, Severity};
use crate::allocator::allocation::AllocationError;

/// A message in oxide wording.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Wording {
    /// The one-line message.
    pub text: String,
    /// The token the message is about, underlined in the source line when it
    /// appears there.
    pub subject: Option<String>,
    /// A suggested fix or an explanation, printed as `= help: ...`.
    pub help: Option<String>,
}

impl Wording {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

/// The long options oxide-vcl adds, for "did you mean" suggestions.
const LONG_OPTIONS: [&str; 5] = [
    "--help",
    "--version",
    "--mode=vcl",
    "--mode=oxide",
    "--warnings-as-errors",
];

pub fn wording(message: &Message) -> Wording {
    match message {
        Message::Cli(message) => cli(message),
        Message::Driver(message) => driver(message),
        Message::Parse(message) => parse(message),
        Message::Compile(message) => compile(message),
    }
}

/// The severity a message has in oxide mode, where it differs from the one
/// it was raised with.
pub fn severity(message: &Message, raised: Severity) -> Severity {
    match message {
        // vcl prints `-M` as a warning, but it only confirms the option.
        Message::Cli(CliMessage::TimidMemory) => Severity::Info,
        _ => raised,
    }
}

fn cli(message: &CliMessage) -> Wording {
    use CliMessage::*;
    match message {
        AlwaysOn => Wording::new("-a has no effect: what it enabled is always on"),
        SizeReductionDisabled => Wording::new("-C: the code size reduction pass is off"),
        AlignmentDisabled => Wording::new("-f: .align directives are left out of the listing"),
        KeepTemporaries => Wording::new("-K: the preprocessors' temporary files are kept"),
        TimidMemory => Wording::new(
            "-M: timid memory access: loads and stores that may touch the same fields keep their source order",
        ),
        PruneDeprecated => Wording::new("-P is deprecated and has no effect")
            .help("remove it from the command line"),
        StaggeredMemory => Wording::new("-S (staggered memory access in loops) has no effect"),
        ImmediateFixupsDeprecated => Wording::new("-Z is deprecated and has no effect")
            .help("remove it from the command line"),
        EndBitDisabled => Wording::new("-e: no instruction gets the [E] bit"),
        BadGaspOption { argument } => Wording::new(format!("unknown option '{argument}'"))
            .help("use -g to run gasp first, or -g+ for gasp with its alternate syntax"),
        BadArgument { argument } => {
            let wording = Wording::new(format!("unknown option '{argument}'"));
            match suggestion(argument) {
                Some(option) => wording.help(format!("did you mean '{option}'?")),
                None => wording.help(
                    "options come before the input file and take their value attached \
                     (-oout.vsm, not -o out.vsm); oxide-vcl --help lists them",
                ),
            }
        }
        BadMode { argument } => {
            let name = argument.strip_prefix("--mode=").unwrap_or(argument);
            Wording::new(format!("unknown mode '{name}'"))
                .help("use --mode=vcl (the default) or --mode=oxide")
        }
        TooManyArguments => Wording::new("more than one input file").help(
            "oxide-vcl compiles one file per run, and options must come before it; \
             an option's value is attached to it (-oout.vsm), so a separate word is read as \
             another input file",
        ),
        SameInputAndOutput => Wording::new("the output file (-o) is the input file")
            .help("writing the listing would overwrite the source; choose another name, such as one ending in .vsm"),
    }
}

/// The long option closest to a mistyped one, when it is close enough.
fn suggestion(argument: &str) -> Option<&'static str> {
    if !argument.starts_with("--") {
        return None;
    }
    let (word, _) = argument.split_once('=').unwrap_or((argument, ""));
    LONG_OPTIONS
        .iter()
        .map(|option| {
            let (name, _) = option.split_once('=').unwrap_or((option, ""));
            (distance(word, name), *option)
        })
        .filter(|(distance, _)| *distance <= 3)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, option)| option)
}

/// Levenshtein distance, ignoring case.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<u8> = a.bytes().map(|byte| byte.to_ascii_lowercase()).collect();
    let b: Vec<u8> = b.bytes().map(|byte| byte.to_ascii_lowercase()).collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, &left) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, &right) in b.iter().enumerate() {
            let cost = usize::from(left != right);
            current.push(
                (previous[j] + cost)
                    .min(previous[j + 1] + 1)
                    .min(current[j] + 1),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// An I/O error's text without Rust's ` (os error N)` suffix.
fn io_text(error: &str) -> &str {
    match error.rfind(" (os error ") {
        Some(start) if error.ends_with(')') => &error[..start],
        _ => error,
    }
}

fn driver(message: &DriverMessage) -> Wording {
    use DriverMessage::*;
    match message {
        OpenInput { path, error } => Wording::new(format!(
            "can't read input file '{path}': {}",
            io_text(error)
        )),
        ReadInput { error } => {
            Wording::new(format!("can't read standard input: {}", io_text(error)))
        }
        NotUtf8 => Wording::new("the source is not valid UTF-8")
            .help("VCL source is ASCII text; check the file's encoding"),
        WriteTemporary { path, error } => Wording::new(format!(
            "can't write the preprocessor's input file '{path}': {}",
            io_text(error)
        )),
        CreateTemporary { path, error } => Wording::new(format!(
            "can't create the preprocessor's output file '{path}': {}",
            io_text(error)
        )),
        ReadTemporary { path, error } => Wording::new(format!(
            "can't read the preprocessor's output file '{path}': {}",
            io_text(error)
        )),
        StartPreprocessor { program, error } => {
            let option = if program == "gcc" { "-G" } else { "-g" };
            Wording::new(format!("can't run {program}: {}", io_text(error))).help(format!(
                "{option} runs `{program}` from PATH; install it, or preprocess the source \
                 yourself and drop {option}"
            ))
        }
        PreprocessorFailed { program, status } => Wording::new(format!(
            "{program} failed ({status}); its own messages are above"
        )),
        KeptTemporaries { input, output } => Wording::new(format!(
            "kept the preprocessor's input '{input}' and output '{output}'"
        )),
        WriteOutput { path, error } => {
            Wording::new(format!("can't write '{path}': {}", io_text(error)))
        }
        WriteStandardOutput { error } => Wording::new(format!(
            "can't write the listing to standard output: {}",
            io_text(error)
        )),
        WarningsAsErrors => Wording::new("warnings are errors (--warnings-as-errors)"),
        // vcl's own start-up and abort lines; oxide mode does not print them.
        OsInit | MovesNotDetected | BasicProcessingFailed | Abort => {
            Wording::new(message.vcl_text())
        }
    }
}

fn parse(message: &ParseMessage) -> Wording {
    use ParseMessage::*;
    match message {
        NonAscii => Wording::new("the line contains a non-ASCII character").help(
            "VCL source is ASCII; look for typographic quotes or dashes and non-breaking spaces",
        ),
        UnexpectedContinuation => {
            Wording::new("a continuation line ('=') with no instruction line before it to continue")
                .subject("=")
        }
        UnexpectedComma => Wording::new("unexpected comma")
            .subject(",")
            .help("a comma separates two operands: check for a leading comma or two in a row"),
        TooManyTokens => Wording::new("too many tokens on one line (at most 200)")
            .help("split the line, or shorten a long continuation"),
        EmptyLabel => Wording::new("a label with no name").subject(":"),
        LabelBits => Wording::new("a label takes only the [M], [D] and [T] bits")
            .help("[E] belongs on an instruction, not on a label"),
        MultipleSynthetics => Wording::new("more than one synthetic instruction on a line")
            .help("put each --enter, --exit, in_vf, out_vf, --cont, ... on its own line"),
        MultipleInstructions => Wording::new("more than one instruction on a line").help(
            "outside raw code each instruction goes on its own line; pairs of upper and lower \
             instructions are written only in .raw blocks",
        ),
        RawPairRequired => Wording::new("raw code takes an upper and a lower instruction on each line")
            .help("write both halves of the pair, using nop for an empty slot: `add.xyz vf01, vf02, vf03  nop`"),
        MissingDirectiveArgument { directive } => {
            Wording::new(format!("{directive} is missing an argument")).subject(directive.clone())
        }
        BadRawOption { value } => Wording::new(format!("unknown raw option '{value}'"))
            .subject(value.clone())
            .help("use reorder or noreorder, or nothing (noreorder)"),
        BadSyntax => Wording::new("unknown .syntax")
            .subject(".syntax")
            .help("use `.syntax new` or `.syntax old`"),
        RawInRawLoop => Wording::new("a raw block inside a raw loop")
            .help("a raw loop is already raw code; end it with .endvsmloop first"),
        EndRawOutsideRaw => Wording::new("the end of a raw block, outside one")
            .help("each .endvsm (or .endraw) closes a .vsm (or .raw) block"),
        EndRawInRawLoop => Wording::new("a raw block's end inside a raw loop")
            .help("end a raw loop with .endvsmloop (or .endrawloop)"),
        NestedRawLoop => Wording::new("a raw loop inside another raw loop")
            .help("end the first with .endvsmloop; raw loops can't nest"),
        EndRawLoopOutsideRawLoop => Wording::new("the end of a raw loop, outside one")
            .help("each .endvsmloop (or .endrawloop) closes a .vsmloop (or .rawloop)"),
        CoreWorkArea => Wording::new("a work area on the core is not supported")
            .subject("core")
            .help("place it in vu0 or vu1 memory"),
        WorkAreaDevice => Wording::new("unknown work area device")
            .subject(".vu_work_area")
            .help("the device is vu0 or vu1: `.vu_work_area vu1 <address> <size>`"),
        WorkAreaOutsideMemory => Wording::new("the work area does not fit in the device's memory")
            .subject(".vu_work_area")
            .help("vu0 has 256 quadwords and vu1 1024: the address must be 0 or more, the size more than 0, and address + size at most the device's size"),
        UniqueTooLong => Wording::new(".unique's name is longer than 128 characters")
            .subject(".unique"),
        UnknownDirective { name } => Wording::new(format!("unknown directive '{name}'"))
            .subject(name.clone()),
        RegisterRangeDashes { range } => {
            Wording::new(format!("'{range}' is not a register range"))
                .subject(range.clone())
                .help("a range has one '-' between two registers: vf01-vf31")
        }
        NotAllocablePhysical { register } => {
            Wording::new(format!("'{register}' is not a register the allocator can use"))
                .subject(register.clone())
                .help("list VF registers as vf01 to vf31 and VI registers as vi01 to vi15")
        }
        RegisterNotAllocable { register } => Wording::new(format!(
            "'{register}' is the constant register and can't be allocated"
        ))
        .subject(register.clone())
        .help("vf00 and vi00 hold constants; start the range at vf01 or vi01"),
        UnknownInstruction { opcode } => {
            let wording =
                Wording::new(format!("unknown instruction '{opcode}'")).subject(opcode.clone());
            match closest_instruction(opcode) {
                Some(name) => wording.help(format!("did you mean '{name}'?")),
                None => wording,
            }
        }
        InvalidInstructionMask => Wording::new("the instruction's field mask is not valid").help(
            "write the fields after a dot in x, y, z, w order, each at most once: add.xyz",
        ),
        LoopIdTooLarge => Wording::new("loop stage larger than 32")
            .help("a raw loop's stage prefix (`2..add`) takes a number up to 32, or `+`"),
        InstructionBits => Wording::new("these [E]/[M]/[D]/[T] bits aren't allowed here").help(
            "outside raw code an instruction takes only [D] and [T]; raw code also takes [M]; \
             the compiler sets [E] itself",
        ),
        RawOnlyInstruction { name } => {
            Wording::new(format!("'{name}' can only be used in raw code"))
                .subject(name.clone())
                .help("put it in a .raw block, or use the instruction the compiler schedules for it")
        }
        MaskNotAccepted { name } => Wording::new(format!("'{name}' takes no field mask"))
            .subject(name.clone())
            .help(format!("remove the mask: `{name}`")),
        XyzMaskRequired => Wording::new("this instruction works on xyz only")
            .help("remove the mask, or write .xyz"),
        EmptyMask => Wording::new("an empty field mask")
            .help("name at least one of x, y, z, w after the dot, or remove the dot"),
        OperandTooLong => Wording::new("an operand longer than 1023 characters"),
        MissingOperand { position, name } => {
            Wording::new(format!("'{name}' is missing operand {position}"))
                .subject(name.clone())
                .help("check the operand count and the commas between operands")
        }
        ScalarNotThird => Wording::new("a broadcast field can only be the third operand")
            .help("write the scalar last: `add dst, src, other[x]`"),
        VectorMasksDiffer => Wording::new("the vector operands select different fields").help(
            "give every vector operand the same fields, or put the mask on the instruction, as in add.xyz",
        ),
        CannotDerive { name } => Wording::new(format!(
            "no instruction '{name}' matches these operands"
        ))
        .help("check the operands: a broadcast field, i, q or acc selects the instruction's form"),
        SingleComponentRequired => Wording::new("this instruction works on one field")
            .help("select a single field: x, y, z or w"),
        MultipleTaggedOperands => Wording::new("more than one tagged operand")
            .help("an instruction takes one memory tag at most"),
        AccNotFirst => Wording::new("acc can only be the first operand")
            .subject("acc")
            .help("acc is the destination of the accumulating forms (adda, mula, ...)"),
        MalformedMemoryOperand { operand } => {
            Wording::new(format!("'{operand}' is not a memory operand"))
                .subject(operand.clone())
                .help("write an offset and an integer register in parentheses: 16(vi01)")
        }
        PostincrementRequired => Wording::new("this instruction needs a post-incremented address")
            .help("write the register as (vi01++)"),
        PredecrementRequired => Wording::new("this instruction needs a pre-decremented address")
            .help("write the register as (--vi01)"),
        InvalidMemoryMask => Wording::new("the memory operand's field mask is not valid")
            .help("write the fields after the parentheses in x, y, z, w order: (vi01)xyz"),
        MemoryMaskRequired => Wording::new("the memory operand needs a field mask")
            .help("add the fields after the parentheses, (vi01)xyzw, or put them on the instruction"),
        MemoryMaskDiffers => Wording::new("the memory operand and the instruction select different fields")
            .help("use the same fields in both, or write them once on the instruction"),
        GeneralizedScalarComponent => Wording::new("a broadcast operand selects one field")
            .help("write a single field in the brackets: other[x]"),
        DuplicateComponent => Wording::new("a field is selected twice")
            .help("name each of x, y, z and w at most once"),
        InvalidOperandMask => Wording::new("the operand's field selection is not valid")
            .help("write the fields after a dot in x, y, z, w order: src.xy"),
        ScalarComponentRequired => Wording::new("this operand selects one field")
            .help("write a single field: src.x"),
        FieldSelectionsDiffer => Wording::new("the instruction and the operand select different fields")
            .help("use the same fields on both, or write them once on the instruction"),
        OperandMustBe { expected } => Wording::new(format!("this operand must be {expected}")),
        ForcedRegisterNotPhysical { register } => {
            Wording::new(format!("'{register}' is not a physical register"))
                .subject(register.clone())
                .help("bind to a register by number: (vf01) for VF, (vi01) for VI")
        }
        EmptyRegisterName => Wording::new("a register operand with no name"),
        RegisterNameTooLong { name } => {
            Wording::new("a register name longer than 62 characters").subject(name.clone())
        }
        InvalidRegisterCharacter { character, name } => {
            let wording = Wording::new(format!("'{character}' can't be used in a register name"))
                .subject(name.clone());
            if *character == '.' {
                wording.help(
                    "only old-syntax in_vf and out_vf take fields on the name (in_vf src.xyz(vf01)); \
                     in new syntax put them on the instruction: out_vf.xyz res(vf02)",
                )
            } else {
                wording.help("register names use letters, digits, '_', '$', '[' and ']'")
            }
        }
        ReservedRegisterName { name } => {
            Wording::new(format!("'{name}' is an instruction name and can't name a register"))
                .subject(name.clone())
                .help("rename the register")
        }
        DuplicateLabel { name } => {
            Wording::new(format!("label '{name}' is defined twice")).subject(name.clone())
        }
        VisibilityMismatch => Wording::new("this .endshow/.endhide does not match the open block")
            .help("close a .show block with .endshow and a .hide block with .endhide"),
        SyntheticContext => Wording::new("this synthetic instruction is out of place").help(
            "in_vf and in_vi go inside --enter ... --endenter, out_vf and out_vi inside \
             --exit ... --endexit; these blocks don't nest",
        ),
        LoopSizeInRaw => Wording::new("--LoopExtra and --LoopAbs can't be used in raw code"),
        InstructionInEnterExit => Wording::new("an instruction inside an --enter or --exit block")
            .help("only in_ and out_ declarations go there; close the block first"),
        UnterminatedEnterExit => Wording::new("an --enter or --exit block is never closed")
            .help("end it with --endenter or --endexit"),
        NoEnter => Wording::new("the program has no --enter block")
            .help("start it with --enter ... --endenter, declaring its inputs with in_vf/in_vi"),
        NoExit => Wording::new("the program has no --exit block")
            .help("end it with --exit ... --endexit (or --cont), declaring its outputs with out_vf/out_vi"),
    }
}

/// The instruction whose name is closest to a mistyped one.
fn closest_instruction(opcode: &str) -> Option<&'static str> {
    let (name, _) = opcode.split_once('.').unwrap_or((opcode, ""));
    let name = name.trim_start_matches('~');
    let limit = if name.len() <= 4 { 1 } else { 2 };
    let (closest, name) = crate::isa::instruction::INSTRUCTIONS
        .iter()
        .filter(|instruction| !instruction.name.starts_with('-') && instruction.name.len() > 1)
        .map(|instruction| (distance(name, instruction.name), instruction.name))
        .min_by_key(|(distance, _)| *distance)?;
    // A name in the table is unknown only in this syntax, not misspelt.
    (closest > 0 && closest <= limit).then_some(name)
}

fn family(vector: bool) -> &'static str {
    if vector { "VF" } else { "VI" }
}

fn compile(message: &CompileMessage) -> Wording {
    use CompileMessage::*;
    match message {
        ReservedName { name, kind } => {
            let family = family(kind.contains("floating"));
            Wording::new(format!(
                "'{name}' is a hardware register and can't name a {family} register"
            ))
            .subject(name.clone())
            .help("rename it: acc, i, q, p, r, clip, mac, ext and the stat_ names belong to the hardware")
        }
        InvalidPhysicalRegister { name } => {
            Wording::new(format!("'{name}' is not a physical register"))
                .subject(name.clone())
                .help("VF registers are vf00 to vf31, VI registers vi00 to vi15")
        }
        InvalidComponentMask => Wording::new("the field mask is not valid")
            .help("write the fields in x, y, z, w order, each at most once"),
        UnmappableBase { name } => Wording::new(format!("'{name}' has no register to bind to"))
            .subject(name.clone())
            .help(format!("give it a register: {name}(vf01)")),
        ConflictingBases { first, second } => {
            Wording::new(format!("one value must live in both {first} and {second}")).help(
                "a value bound to one register at an input or output and to another elsewhere \
             can't be both; bind both places to the same register, or copy it with move",
            )
        }
        BindingChanged { name, from, to } => Wording::new(format!(
            "'{name}' was bound to {from} and is now bound to {to}"
        ))
        .subject(name.clone())
        .help(
            "a name keeps one register; use another name for the second binding if that is meant",
        ),
        UsedBeforeSet {
            register,
            component,
        } => Wording::new(format!(
            "'{register}.{component}' is read before anything writes it"
        ))
        .subject(register.clone())
        .help(format!(
            "write '{register}' earlier, or declare it as an input of the --enter block \
             (in_vf {register}(vf01) or in_vi {register}(vi01)); in old syntax a name ending \
             in x, y, z or w loses those letters as fields"
        )),
        OperandPoolUnsupported => Wording::new("this operand's register kind can't be allocated"),
        ExplicitMacWrite => Wording::new("the MAC flags can't be an input")
            .subject("in_hw_mac")
            .help("MAC flags are set by the instructions that compute them; remove in_hw_mac"),
        MoveDestinationUnbound => Wording::new("this copy's destination has no register"),
        ZeroCopyDestination => {
            Wording::new("this copy writes to the constant register (vf00 or vi00)").help(
                "vf00 and vi00 hold constants and can't be written; bind the destination to \
                 another register",
            )
        }
        LoopCsKnownCount => {
            Wording::new("--LoopCS: the minimum known count (argument 1) must be at least 1")
        }
        LoopCsSafeCount => {
            Wording::new("--LoopCS: the minimum safe count (argument 2) must be 0 or more")
        }
        LoopAbsCount => Wording::new("--LoopAbs: the extra loop count must be at least 2"),
        LoopExtraCount => Wording::new("--LoopExtra: the extra loop count must be 0 or more"),
        MissingLoopHint => Wording::new("the loop hint is missing its count")
            .help("write a number after the hint: --LoopExtra 1"),
        LoopHintNotConstant => Wording::new("a loop hint's count must be a number"),
        LoopPeriodRange => Wording::new("the loop is too long to schedule")
            .help("split the loop body, or compile it as straight-line code with -L"),
        UnknownSynthetic => Wording::new("unknown synthetic instruction"),
        ContWithEndMode => Wording::new("--cont after this block already ended")
            .help("--cont ends a block; it can't follow an exit or a branch"),
        FieldLettersInName {
            token,
            name,
            fields,
        } => Wording::new(format!(
            "'{token}' is read as register '{name}' with field selection .{fields}"
        ))
        .subject(token.clone())
        .help(format!(
            "old syntax takes a name's trailing x, y, z and w letters as fields: rename it, or \
             write '{name}.{fields}' if the fields are meant (vcl mode accepts it with a warning)"
        )),
        RawBranchDelay => Wording::new("a raw branch needs one instruction pair in its delay slot")
            .help("follow the branch with exactly one pair, `nop nop` if there is nothing to do"),
        RawBranchDelayBeforeBoundary => {
            Wording::new("a raw branch needs its delay-slot pair before the raw block ends")
                .help("add the delay-slot pair before the block's end")
        }
        BranchInRawDelaySlot => Wording::new("a branch in a raw branch's delay slot")
            .help("the pair after a branch runs before it is taken; move the second branch"),
        WrongRawPipe => Wording::new("this instruction is in the wrong half of the raw pair")
            .help("write the upper (floating point) instruction first and the lower one second"),
        StagedRawLoopBranch => {
            Wording::new("a staged raw loop must end with a conditional branch to itself")
        }
        StagedRawLoopTarget => {
            Wording::new("a staged raw loop's branch must target the loop itself")
        }
        LoopIdOnNop => Wording::new("a stage number on a nop")
            .help("put the stage prefix on a real instruction"),
        InvalidStagedRawPair => Wording::new("this staged raw pair is not valid"),
        RawLoopPeriodRange => Wording::new("the raw loop is too long"),
        InvalidRawLoopStage => Wording::new("this raw loop stage is not valid"),
        MaxLoopIdZero => Wording::new("a raw loop with no stage numbers")
            .help("mark each instruction with its iteration stage: `1..add`, `2..mul`"),
        RawLoopWithoutBranch => Wording::new("a raw loop without a branch back to its start"),
        TooManyLoops => Wording::new("too many loops"),
        DuplicateStreamLabel { label } => {
            Wording::new(format!("label '{label}' is defined twice")).subject(label.clone())
        }
        PipesWriteSameRegister => Wording::new("both halves of the pair write the same register")
            .help("the upper and lower instructions of a pair must write different registers"),
        UnknownBranchTarget { target } => Wording::new(format!("no label '{target}' to branch to"))
            .subject(target.clone())
            .help("define the label, or check its spelling"),
        UnknownBranchTag { tag } => {
            Wording::new(format!("no label tagged '{tag}' for this indirect branch"))
                .subject(tag.clone())
        }
        DuplicateBranchTag => Wording::new("an indirect branch lists the same target twice"),
        UntaggedIndirectBranch => Wording::new("an indirect branch needs its possible targets")
            .help("list the labels it can jump to as tags: `jr target:first:second`"),
        Allocation(failure) => allocation(failure),
        AllocationPoolEmpty { vector } => {
            let family = family(*vector);
            let lower = family.to_ascii_lowercase();
            let last = if *vector { "31" } else { "15" };
            Wording::new(format!("no {family} register is available to allocate")).help(format!(
                "allow the allocator {family} registers with .init_{lower}_all, or a range: \
                 .init_{lower} {lower}01-{lower}{last}"
            ))
        }
        AllocationPoolFull { vector, register } => {
            let family = family(*vector);
            let lower = family.to_ascii_lowercase();
            Wording::new(format!(
                "out of {family} registers for '{register}': every allowed {family} register is in use"
            ))
            .subject(register.clone())
            .help(format!(
                "allow more registers with .init_{lower}_all or a wider .init_{lower} range, or \
                 keep fewer values live at once"
            ))
        }
        Internal(detail) => internal(detail),
    }
}

fn allocation(failure: &AllocationError) -> Wording {
    internal(&format!("register allocation failed: {failure:?}"))
}

fn internal(detail: &str) -> Wording {
    Wording::new(format!("internal compiler error: {detail}"))
        .help("this is a bug in oxide-vcl; please report it with the source file and command line")
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_options_are_suggested_for_near_misses() {
        assert_eq!(
            suggestion("--warning-as-errors"),
            Some("--warnings-as-errors")
        );
        assert_eq!(suggestion("--verison"), Some("--version"));
        assert_eq!(suggestion("--hlep"), Some("--help"));
        assert_eq!(suggestion("--mod=oxide"), Some("--mode=vcl"));
        assert_eq!(suggestion("--frobnicate"), None);
        assert_eq!(suggestion("-x"), None);
    }

    #[test]
    fn instructions_are_suggested_for_near_misses() {
        assert_eq!(closest_instruction("ad"), Some("add"));
        assert_eq!(closest_instruction("ad.xyz"), Some("add"));
        assert_eq!(
            closest_instruction("ilwr"),
            None,
            "an exact name needs no suggestion"
        );
        assert_eq!(closest_instruction("frob"), None);
        assert_eq!(closest_instruction("frobnicate"), None);
    }

    #[test]
    fn io_errors_lose_their_os_error_suffix() {
        assert_eq!(
            io_text("No such file or directory (os error 2)"),
            "No such file or directory"
        );
        assert_eq!(io_text("broken pipe"), "broken pipe");
    }

    #[test]
    fn oxide_wording_differs_from_vcl_and_names_its_subject() {
        let message = Message::from(ParseMessage::DuplicateLabel { name: "top".into() });
        assert_ne!(message.oxide_text(), message.vcl_text());
        assert_eq!(message.wording().subject.as_deref(), Some("top"));
        let internal = wording(&CompileMessage::Internal("phase failed".into()).into());
        assert!(internal.text.starts_with("internal compiler error: "));
        assert!(internal.help.unwrap().contains("bug in oxide-vcl"));
    }

    #[test]
    fn timid_memory_is_a_note_in_oxide_mode() {
        let message = Message::from(CliMessage::TimidMemory);
        assert_eq!(severity(&message, Severity::Warning), Severity::Info);
        let other = Message::from(CliMessage::PruneDeprecated);
        assert_eq!(severity(&other, Severity::Warning), Severity::Warning);
    }
}
