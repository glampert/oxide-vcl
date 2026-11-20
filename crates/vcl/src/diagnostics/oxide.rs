//! The wording of every message: a sentence, the token in the source line it
//! is about (for the caret underline) and, where there is one, a suggested
//! fix. None of this is vcl 1.4beta7's text.

use super::{
    CliMessage, CompileMessage, DriverMessage, FrontendMessage, Message, ParseMessage,
    PreprocessMessage,
};
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
const LONG_OPTIONS: [&str; 13] = [
    "--help",
    "--version",
    "--mode=vcl",
    "--mode=oxide",
    "--warnings-as-errors",
    "--color=auto",
    "--define=",
    "--vcl-boilerplate",
    "--fold-constants",
    "--flatten-subscripts",
    "--warn-undef",
    "--comment-char=",
    "--preprocess-only",
];

pub fn wording(message: &Message) -> Wording {
    match message {
        Message::Cli(message) => cli(message),
        Message::Driver(message) => driver(message),
        Message::Parse(message) => parse(message),
        Message::Compile(message) => compile(message),
        Message::Preprocess(message) => preprocess(message),
        Message::Frontend(message) => frontend(message),
    }
}

/// A frontend's message, worded by the frontend.
fn frontend(message: &FrontendMessage) -> Wording {
    Wording {
        text: message.text.clone(),
        subject: message.subject.clone(),
        help: message.help.clone(),
    }
}

fn preprocess(message: &PreprocessMessage) -> Wording {
    use PreprocessMessage::*;
    let plural =
        |count: usize, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    match message {
        IncludedFrom => Wording::new("in the file included from here"),
        MoreIncludes { count } => Wording::new(format!("[{count} more includes]")),
        InExpansionOf { name } => {
            Wording::new(format!("in expansion of macro '{name}'")).subject(name.clone())
        }
        MoreExpansions { count } => Wording::new(format!("[{count} more expansions]")),
        NotFoundAt { path } => Wording::new(format!("not found at '{path}'")),
        NoIncludeDirectories => {
            Wording::new("<file> is only looked for in the -I directories, and there are none")
        }
        FirstOneHere => Wording::new("the first one is here"),
        FirstUsedHere => Wording::new("first used here"),
        PreviousDefinitionHere => Wording::new("the previous definition is here"),
        DefinedHere { name } => {
            Wording::new(format!("'{name}' is defined here")).subject(name.clone())
        }
        Lex { error } => Wording::new(error.to_string()),
        LexWarning { warning } => Wording::new(warning.to_string()),
        UnknownDirective { name } => {
            Wording::new(format!("unknown preprocessor directive '#{name}'"))
        }
        ErrorDirective { text } => Wording::new(format!("#error{text}")),
        WarningDirective { text } => Wording::new(format!("#warning{text}")),
        ExtraTokens { directive } => Wording::new(format!(
            "extra tokens at the end of the #{directive} directive"
        )),
        UnknownPragma { text } => Wording::new(format!("ignoring unknown '#pragma{text}'")),
        ExpectedMacroName { directive } => {
            Wording::new(format!("expected a macro name after #{directive}"))
        }
        DefinedAsMacroName => {
            Wording::new("'defined' cannot be used as a macro name").subject("defined")
        }
        ChangeBuiltin { name, undefining } => Wording::new(format!(
            "cannot {} built-in macro '{name}'",
            if *undefining { "undefine" } else { "redefine" }
        ))
        .subject(name.clone()),
        DefinedAfterUse { name } => Wording::new(format!(
            "'{name}' is defined after code that uses it; those uses are not expanded \
             (vclpp 1 expanded them)"
        ))
        .subject(name.clone()),
        Redefined { name } => Wording::new(format!("'{name}' redefined")).subject(name.clone()),
        IncludeExpectsFile => Wording::new("expected \"file\" or <file> after #include"),
        IncludeMissingGreater => Wording::new("missing '>' after the #include file name"),
        IncludeEmptyName => Wording::new("empty file name in #include"),
        IncludeTooDeep { depth } => Wording::new(format!(
            "#include nested {depth} levels deep: does a file include itself?"
        )),
        IncludeNotFound { name } => Wording::new(format!("cannot find include file '{name}'"))
            .help("-I<dir> adds a directory to look in"),
        IncludeUnreadable { path } => Wording::new(format!("cannot read include file '{path}'")),
        MissingParameterClose { name } => Wording::new(format!(
            "missing ')' in the parameter list of macro '{name}'"
        )),
        ExpectedParameterBeforeClose { name } => Wording::new(format!(
            "expected a parameter name before ')' in macro '{name}'"
        )),
        EllipsisNotLast { name } => Wording::new(format!(
            "'...' must be the last parameter of macro '{name}'"
        )),
        ExpectedCommaOrClose { name } => Wording::new(format!(
            "expected ',' or ')' in the parameter list of macro '{name}'"
        )),
        ExpectedParameterName { name } => {
            Wording::new(format!("expected a parameter name in macro '{name}'"))
        }
        DuplicateParameter { parameter, name } => Wording::new(format!(
            "duplicate parameter '{parameter}' in macro '{name}'"
        ))
        .subject(parameter.clone()),
        HashWithoutParameter { name } => Wording::new(format!(
            "'#' is not followed by a parameter of macro '{name}'"
        )),
        PasteAtEnd { name, block } => Wording::new(format!(
            "'##' cannot be at either end of {}'{name}'",
            if *block { "a line of macro " } else { "macro " }
        ))
        .subject("##"),
        BlockMacroWithoutColon { name } => Wording::new(format!(
            "unexpected text after the name of macro '{name}'; a parameter list starts \
             with ':', as in '#macro {name}: param1, param2'"
        )),
        LostCommaBeforeFirst { name } => Wording::new(format!(
            "lost comma before the first parameter of macro '{name}'"
        )),
        LostCommaAfter { parameter, name } => Wording::new(format!(
            "lost comma after parameter '{parameter}' of macro '{name}'"
        )),
        MissingCommaAfter { parameter, name } => Wording::new(format!(
            "missing comma after parameter '{parameter}' of macro '{name}'"
        )),
        ExtraneousComma { parameter, name } => Wording::new(format!(
            "extraneous comma after the last parameter '{parameter}' of macro '{name}'"
        )),
        UnterminatedMacro { name } => {
            Wording::new(format!("unterminated #macro '{name}': missing #endmacro"))
                .subject(name.clone())
        }
        TextAfterEndmacro => Wording::new("unexpected text after #endmacro"),
        DirectiveInMacro { directive, name } => Wording::new(format!(
            "preprocessor directive '#{directive}' inside #macro '{name}'"
        )),
        EndmacroWithoutMacro => Wording::new("#endmacro without #macro").subject("endmacro"),
        VuprogTwice => Wording::new("#vuprog appears twice").subject("vuprog"),
        EndvuprogTwice => Wording::new("#endvuprog appears twice").subject("endvuprog"),
        ExpectedProgramName => Wording::new("expected a program name after #vuprog"),
        TextAfterProgramName => Wording::new("unexpected text after the program name"),
        EndvuprogWithoutVuprog => Wording::new("#endvuprog without #vuprog").subject("endvuprog"),
        TextAfterEndvuprog => Wording::new("unexpected text after #endvuprog"),
        MissingVuprog => Wording::new("program start directive '#vuprog' was not found"),
        MissingEndvuprog => Wording::new("program end directive '#endvuprog' was not found"),
        UnterminatedConditional { directive } => {
            Wording::new(format!("unterminated #{directive}: missing #endif"))
                .subject(directive.clone())
        }
        WithoutIf { directive } => {
            Wording::new(format!("#{directive} without #if")).subject(directive.clone())
        }
        ElifAfterElse => Wording::new("#elif after #else").subject("elif"),
        ElseAfterElse => Wording::new("#else after #else").subject("else"),
        DefinedWithoutName => {
            Wording::new("'defined' must be followed by a macro name").subject("defined")
        }
        DefinedMissingClose { name } => {
            Wording::new(format!("missing ')' after 'defined({name}'")).subject("defined")
        }
        NoExpression { directive } => {
            Wording::new(format!("#{directive} with no expression")).subject(directive.clone())
        }
        NotDefinedIsZero { name } => Wording::new(format!(
            "'{name}' is not defined, evaluates to 0 [--warn-undef]"
        ))
        .subject(name.clone()),
        FloatInExpression { context, number } => Wording::new(format!(
            "{context}: floating-point number '{number}' in an integer expression"
        ))
        .subject(number.clone()),
        NotInExpression { context, token } => Wording::new(format!(
            "{context}: '{token}' cannot appear in an integer expression"
        ))
        .subject(token.clone()),
        Expression { context, error } => Wording::new(format!("{context}: {error}")),
        NotABlockMacro { name } => Wording::new(format!(
            "'{name}' is not a block macro: no '#macro {name}' has been seen"
        ))
        .subject(name.clone()),
        RecursiveBlockMacro { name } => {
            Wording::new(format!("recursive invocation of block macro '{name}'"))
                .subject(name.clone())
        }
        BlockMacroInContext { name, context } => Wording::new(format!(
            "block macro '{name}' cannot be invoked in {context}"
        ))
        .subject(name.clone()),
        UnterminatedArguments { name } => Wording::new(format!(
            "unterminated argument list invoking macro '{name}'"
        ))
        .subject(name.clone()),
        UnbalancedArgument { token, name } => Wording::new(format!(
            "unbalanced '{token}' in the arguments of macro '{name}'"
        ))
        .subject(token.clone()),
        TooFewArguments {
            name,
            at_least,
            given,
        } => Wording::new(format!(
            "macro '{name}' takes at least {}, but {given} were given",
            plural(*at_least, "argument")
        ))
        .subject(name.clone()),
        WrongArgumentCount { name, takes, given } => Wording::new(format!(
            "macro '{name}' takes {}, but {given} {} given",
            plural(*takes, "argument"),
            if *given == 1 { "was" } else { "were" }
        ))
        .subject(name.clone()),
        EmptyBlockArgument {
            index,
            parameter,
            name,
        } => Wording::new(format!(
            "argument {index} ('{parameter}') of block macro '{name}' is empty"
        ))
        .subject(name.clone()),
        InvalidPaste { left, right } => Wording::new(format!(
            "pasting '{left}' and '{right}' does not give a valid token"
        ))
        .subject("##"),
        NotInMaspMode { directive } => Wording::new(format!(
            "#{directive} is not available in MASP mode{}",
            if directive == "macro" {
                ": use .macro"
            } else {
                ""
            }
        ))
        .subject(directive.clone()),
        MaspUnterminatedAif => Wording::new("unterminated .aif: missing .aendi"),
        MaspEndlessWhile { passes } => {
            Wording::new(format!(".awhile did not end after {passes} passes"))
        }
        MaspMissingEnd => Wording::new("the program does not end with .end"),
        MaspUnterminatedBlock { open, close } => {
            Wording::new(format!("unterminated .{open}: missing .{close}"))
        }
        MaspCloseWithoutOpen { close, open } => Wording::new(format!(".{close} without .{open}")),
        MaspAssignNeedsName { directive } => Wording::new(format!(
            ".{directive} takes the name it assigns in column 1, as in 'name .{directive} value'"
        )),
        MaspRefusedDirective { name } => Wording::new(format!(
            "MASP directive '.{name}' is not supported by the preprocessor"
        )),
        MaspExpectedMacroName => Wording::new("expected a macro name after .macro"),
        MaspMissingParameterName { name } => {
            Wording::new(format!("missing parameter name in macro '{name}'"))
        }
        MaspNotAParameterName { parameter, name } => Wording::new(format!(
            "'{parameter}' is not a parameter name in macro '{name}': parameters are separated \
             by commas"
        )),
        MaspExpansionTooDeep { depth, name } => Wording::new(format!(
            "macro expansions nested {depth} levels deep: does macro '{name}' invoke itself?"
        )),
        MaspNoSuchParameter { name, parameter } => Wording::new(format!(
            "macro '{name}' has no parameter named '{parameter}'"
        )),
        MaspPositionalAfterNamed { argument, name } => Wording::new(format!(
            "argument '{argument}' after one given by name, invoking macro '{name}': after one, \
             all have to be"
        )),
        MaspTooManyArguments { name, takes } => Wording::new(format!(
            "too many arguments for macro '{name}': it takes {takes}"
        )),
        MaspExitmOutsideMacro => Wording::new(".exitm outside a macro"),
        MaspAelseWithoutAif => Wording::new(".aelse without .aif"),
        MaspAelseAfterAelse => Wording::new(".aelse after .aelse"),
        MaspAendiWithoutAif => Wording::new(".aendi without .aif"),
        MaspRepeatTooMany { count, max } => Wording::new(format!(
            ".arepeat count {count} is more than the {max} passes a loop can make"
        )),
        MaspIncludeExpectsFile => Wording::new("expected \"file\" after .include"),
        MaspIncludeEmptyName => Wording::new("empty file name in .include"),
        MaspIncludeTooDeep { depth } => Wording::new(format!(
            ".include nested {depth} levels deep: does a file include itself?"
        )),
        MaspUndefinedVariable { name } => Wording::new(format!(
            "MASP variable '{name}' is not defined: .assigna and .assignc define them"
        )),
        MaspStringComparison { context } => Wording::new(format!(
            "{context}: a quoted string can only be compared with another one"
        )),
    }
}

fn cli(message: &CliMessage) -> Wording {
    use CliMessage::*;
    match message {
        AlwaysOn => Wording::new("-a has no effect: what it enabled is always on"),
        PruneDeprecated => Wording::new("-P is deprecated and has no effect")
            .help("remove it from the command line"),
        StaggeredMemory => Wording::new("-S (staggered memory access in loops) has no effect"),
        ImmediateFixupsDeprecated => Wording::new("-Z is deprecated and has no effect")
            .help("remove it from the command line"),
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
        BadColor { argument } => {
            let name = argument.strip_prefix("--color=").unwrap_or(argument);
            Wording::new(format!("unknown color setting '{name}'"))
                .help("use --color=auto (the default), --color=always or --color=never")
        }
        BadFix { name, .. } => {
            let names: Vec<_> = crate::Fix::ALL.iter().map(|fix| fix.name()).collect();
            let wording = if name.is_empty() {
                Wording::new("--fix= names no fix")
            } else {
                Wording::new(format!("unknown fix '{name}'"))
            };
            wording.help(format!(
                "use --fix=all, or a comma-separated list of: {}",
                names.join(", ")
            ))
        }
        BadOptimization { argument, name } => {
            let names: Vec<_> = crate::Optimization::ALL
                .iter()
                .map(|optimization| optimization.name())
                .collect();
            // `--opt=` or `--no-opt=`.
            let option = argument
                .split_once('=')
                .map_or(argument.as_str(), |(option, _)| option);
            let wording = if name.is_empty() {
                Wording::new(format!("{option}= names no optimization"))
            } else {
                Wording::new(format!("unknown optimization '{name}'"))
            };
            wording.help(format!(
                "use {option}=all, {option}=none, or a comma-separated list of: {}",
                names.join(", ")
            ))
        }
        OptimizationsNeedOxideMode { argument } => {
            Wording::new(format!("{argument} applies only in oxide mode")).help(
                "add --mode=oxide; vcl mode always generates the code vcl 1.4beta7 generates",
            )
        }
        PreprocessorOptionWithoutPreprocessor { argument } => {
            Wording::new(format!("{argument} only applies when the source is preprocessed"))
                .help("add -G to preprocess in standard mode, or -g for MASP mode")
        }
        BadDefine { argument, name } => {
            Wording::new(format!("{argument}: cannot define '{name}'")).help(
                "the name must be an identifier other than 'defined' or a built-in macro",
            )
        }
        BadCommentChar { argument } => {
            Wording::new(format!("{argument} takes exactly one character"))
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
        AlternateSyntax => Wording::new(
            "-g+ asks for gasp's alternate macro syntax, which the preprocessor does not support",
        )
        .help("use -g, and write the macros in masp's standard syntax"),
        WriteOutput { path, error } => {
            Wording::new(format!("can't write '{path}': {}", io_text(error)))
        }
        WriteStandardOutput { error } => Wording::new(format!(
            "can't write the listing to standard output: {}",
            io_text(error)
        )),
        WarningsAsErrors => Wording::new("warnings are errors (--warnings-as-errors)"),
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
        BadImmediate {
            instruction,
            expression,
            detail,
            type_name,
            offset,
        } => {
            let what = if *offset { "offset" } else { "immediate" };
            let wording = match type_name {
                None => Wording::new(format!(
                    "'{expression}' is not a valid {what} for {instruction}: {detail}"
                )),
                Some(_) => Wording::new(format!(
                    "{instruction}'s {what} '{expression}' cannot be computed: {detail}"
                )),
            };
            let functions = if instruction == "loi" {
                ", and loi may call math functions such as sin(x) and pow(x, y)"
            } else {
                ""
            };
            wording.subject(expression.clone()).help(format!(
                "an immediate is a number, a name, or an expression of them with + - * / % \
                 << >> & ^ | && and unary - + ~ ! # (absolute value), grouped by parentheses; \
                 an expression with a name is left to the assembler{functions}"
            ))
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
        CallWithoutJalr => Wording::new("a --call block must be followed by a JALR")
            .help("write the call after --endcall: jalr vi15, address"),
        CallRegisterNotPhysical { name } => Wording::new(format!(
            "--call declares physical registers, not '{name}'"
        ))
        .help("name the register the callee reads or writes, such as vi11 or vf24.xyz"),
        ReturnRegister => Wording::new("--return needs the integer register to return through")
            .help("write --return vi15"),
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
                "allow more registers with .init_{lower}_all or a wider .init_{lower} range, \
                 keep fewer values live at once, or give values room to spill to in data \
                 memory: .vu_work_area vu1,<address>,<qwords>"
            ))
        }
        RegisterSpilled {
            name,
            vector,
            device,
            address,
        } => Wording::new(format!(
            "'{name}' does not fit in the {} registers: it is kept in {device} data memory at \
             qword {address}, the work area",
            family(*vector)
        ))
        .subject(name.clone()),
        WorkAreaNeedsOxideMode => Wording::new(
            "a work area needs --mode=oxide: vcl 1.4beta7 rejects every .vu_work_area",
        )
        .subject(".vu_work_area")
        .help("compile with --mode=oxide to spill registers that do not fit into the area"),
        WorkAreaFull {
            vector,
            register,
            size,
        } => {
            let family = family(*vector);
            let lower = family.to_ascii_lowercase();
            Wording::new(format!(
                "out of {family} registers for '{register}', and all {size} qwords of the work \
                 area already hold spilled values"
            ))
            .subject(register.clone())
            .help(format!(
                "make the .vu_work_area larger, allow more registers with .init_{lower}_all, or \
                 keep fewer values live at once"
            ))
        }
        WorkAreaNumber { token } => Wording::new(format!(
            "the work area's address or size '{token}' is not a number"
        ))
        .subject(token.clone())
        .help("write a decimal or 0x hexadecimal number of qwords"),
        ImmediateOutOfField {
            instruction,
            value,
            low,
            high,
        } => {
            let help = if matches!(instruction.as_str(), "lq" | "sq" | "ilw" | "isw") {
                "a memory offset reaches -1024 to 1023 qwords from its base register: move the \
                 base closer, for example with an IADDIU before the access"
                    .to_string()
            } else if matches!(instruction.as_str(), "iaddiu" | "isubiu") {
                "neither IADDIU nor ISUBIU can add 0x8000 to a VI register in one step: add it \
                 in two, for example iaddiu …,0x7fff then iaddiu …,1"
                    .to_string()
            } else {
                format!("use a value from {low} to {high}")
            };
            Wording::new(format!(
                "{value} does not fit {instruction}'s immediate field ({low} to {high}); the \
                 assembler would keep only its low bits"
            ))
            .subject(value.clone())
            .help(help)
        }
        FlagMaskTruncated {
            instruction,
            value,
            bits,
        } => Wording::new(format!(
            "{instruction}'s mask {value} has bits past the flag's {bits} bits, which do not exist"
        ))
        .subject(value.clone())
        .help(format!(
            "the test is the same without them; keep the mask within its low {bits} bits"
        )),
        ImmediateUnchecked {
            instruction,
            value,
            bits,
        } => Wording::new(format!(
            "'{value}' is not a number, so its fit in {instruction}'s {bits}-bit field is not checked"
        ))
        .subject(value.clone())
        .help("the assembler keeps only the field's bits of the value it resolves"),
        ProgramTooLarge { rows } => Wording::new(format!(
            "the program is {rows} instructions, more than the 2048 of VU1 micro memory"
        ))
        .help("split it into programs that are loaded separately"),
        LoiFunction { expression, detail } => Wording::new(format!(
            "cannot evaluate '{expression}': {detail}"
        ))
        .subject(expression.clone())
        .help(format!(
            "loi expressions can call {}, with numbers as arguments",
            crate::parser::expression::function_names()
                .map(|name| format!("{name}()"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
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
    fn wording_names_its_subject() {
        let message = Message::from(ParseMessage::DuplicateLabel { name: "top".into() });
        assert_eq!(message.wording().subject.as_deref(), Some("top"));
        let internal = wording(&CompileMessage::Internal("phase failed".into()).into());
        assert!(internal.text.starts_with("internal compiler error: "));
        assert!(internal.help.unwrap().contains("bug in oxide-vcl"));
    }
}
