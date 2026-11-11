//! Binary-derived parser and source-level IR for VCL.

pub mod expression;

pub use crate::diagnostics::ParseMessage;
use crate::isa::instruction::{
    self, Direction, Hook, ImmediateKind, InstructionDef, InstructionKind, Mask, MaskMode,
    OperandSpec, Syntax, class, id, subtype,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    pub statements: Vec<Statement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub line: usize,
    pub kind: StatementKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementKind {
    Label(Label),
    Directive(Directive),
    Instruction(Instruction),
    RawText(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub modifiers: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Directive {
    Vu,
    Mpg(String),
    Name(String),
    Global(String),
    Syntax(Syntax),
    InitRegisters {
        vector: bool,
        enable: bool,
        names: Vec<String>,
    },
    InitAll {
        vector: bool,
    },
    Raw {
        reorder: bool,
    },
    EndRaw,
    RawLoop {
        reorder: bool,
    },
    EndRawLoop,
    WorkArea {
        device: String,
        address: i32,
        size: u32,
    },
    Align(Vec<String>),
    NoAligns,
    NoEbits,
    Unique(Option<String>),
    Visibility {
        show: bool,
        names: Vec<String>,
    },
    EndVisibility {
        show: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseMode {
    Vcl,
    Raw,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub metadata: &'static InstructionDef,
    pub mnemonic: String,
    pub mask: Mask,
    /// M=1, E=2, D=4, T=8, exactly as the original.
    pub modifiers: u8,
    pub operands: Vec<Operand>,
    pub mode: ParseMode,
    pub raw_reorder: bool,
    pub loop_id: u16,
    pub optional: bool,
    /// Proven compiler-generated copy; source-written abstract moves stay None.
    pub generated_copy: Option<crate::analysis::copies::Kind>,
}

impl Instruction {
    pub fn synthetic(&self) -> Option<Synthetic> {
        use subtype::synthetic::*;
        self.metadata
            .is_synthetic()
            .then_some(match self.metadata.subtype {
                ENTER => Synthetic::Enter,
                END_ENTER => Synthetic::EndEnter,
                EXIT => Synthetic::Exit,
                END_EXIT => Synthetic::EndExit,
                CONTINUE => Synthetic::Continue,
                INPUT => Synthetic::Input,
                OUTPUT => Synthetic::Output,
                LOOP_COUNT => Synthetic::LoopCount,
                LOOP_EXTRA => Synthetic::LoopExtra,
                BARRIER => Synthetic::Barrier,
                EXIT_MACRO => Synthetic::ExitMacro,
                LOOP_ABSOLUTE => Synthetic::LoopAbsolute,
                _ => Synthetic::Unknown,
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Synthetic {
    Enter,
    EndEnter,
    Exit,
    EndExit,
    Continue,
    Input,
    Output,
    LoopCount,
    LoopExtra,
    Barrier,
    ExitMacro,
    LoopAbsolute,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operand {
    pub text: String,
    pub class: u8,
    pub direction: Direction,
    pub implicit: bool,
    pub mask: Mask,
    pub register: Option<String>,
    pub forced_register: Option<String>,
    pub immediate: Option<String>,
    /// Original operand conversion selector survives opcode normalization.
    pub immediate_kind: ImmediateKind,
    pub tags: Vec<String>,
    /// The whole token when old syntax read the trailing x/y/z/w letters of
    /// a symbolic name as a field selection with no dot before them (`copy`
    /// as `cop` with field y): most likely a misspelt name.
    pub field_letters: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: ParseMessage,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParserOptions {
    pub syntax: Syntax,
    pub allow_incomplete: bool,
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    comma: bool,
}

struct State {
    syntax: Syntax,
    mode: ParseMode,
    reorder: bool,
    raw_loop: bool,
    saved_mode: ParseMode,
    saved_reorder: bool,
}

impl State {
    fn new(syntax: Syntax) -> Self {
        Self {
            syntax,
            mode: ParseMode::Vcl,
            reorder: false,
            raw_loop: false,
            saved_mode: ParseMode::Vcl,
            saved_reorder: false,
        }
    }
}

/// Parse and validate a complete source stream. As in the original build pass,
/// a complete stream must contain at least one enter and at least one exit.
pub fn parse(source: &str) -> Result<Program, Vec<ParseError>> {
    parse_with_options(source, ParserOptions::default())
}

/// `allow_incomplete` accepts fragments for editors and analysis tools; lexical,
/// operand and directive errors still fail. It only relaxes stream validation.
pub fn parse_with_options(
    source: &str,
    options: ParserOptions,
) -> Result<Program, Vec<ParseError>> {
    let mut errors = Vec::new();
    let lines = logical_lines(source, &mut errors);
    let mut state = State::new(options.syntax);
    let mut program = Program::default();
    for (line, text, passthrough) in lines {
        if passthrough {
            program.statements.push(Statement {
                line,
                kind: StatementKind::RawText(text),
            });
            continue;
        }
        let tokens = match tokenize(&text) {
            Ok(tokens) => tokens,
            Err(message) => {
                errors.push(ParseError { line, message });
                continue;
            }
        };
        if tokens.is_empty() {
            continue;
        }
        if tokens[0].text.starts_with('.') {
            match parse_directive(&tokens, &mut state) {
                Ok(directive) => program.statements.push(Statement {
                    line,
                    kind: StatementKind::Directive(directive),
                }),
                Err(message) => errors.push(ParseError { line, message }),
            }
            continue;
        }
        let mut cursor = 0;
        if tokens[0].text.ends_with(':') {
            let (name, modifiers) = strip_modifiers(tokens[0].text.trim_end_matches(':'));
            let name = name.to_string();
            if name.is_empty() {
                errors.push(ParseError {
                    line,
                    message: ParseMessage::EmptyLabel,
                });
                continue;
            }
            cursor += 1;
            let mut modifiers = modifiers;
            if tokens
                .get(cursor)
                .is_some_and(|token| token.text.starts_with('['))
            {
                let (rest, extra) = strip_modifiers(&tokens[cursor].text);
                if rest.is_empty() {
                    modifiers |= extra;
                    cursor += 1;
                }
            }
            if modifiers & !0x0d != 0 {
                errors.push(ParseError {
                    line,
                    message: ParseMessage::LabelBits,
                });
            }
            program.statements.push(Statement {
                line,
                kind: StatementKind::Label(Label { name, modifiers }),
            });
        }
        let start = program.statements.len();
        while cursor < tokens.len() {
            match parse_instruction(&tokens, &mut cursor, &state) {
                Ok(instruction) => program.statements.push(Statement {
                    line,
                    kind: StatementKind::Instruction(instruction),
                }),
                Err(message) => {
                    errors.push(ParseError { line, message });
                    break;
                }
            }
        }
        let instructions: Vec<_> = program.statements[start..]
            .iter()
            .filter_map(|statement| {
                if let StatementKind::Instruction(instruction) = &statement.kind {
                    Some(instruction)
                } else {
                    None
                }
            })
            .collect();
        let ordinary = instructions
            .iter()
            .filter(|instruction| !instruction.metadata.is_synthetic())
            .count();
        if instructions
            .iter()
            .any(|instruction| instruction.metadata.is_synthetic())
        {
            if instructions.len() != 1 {
                errors.push(ParseError {
                    line,
                    message: ParseMessage::MultipleSynthetics,
                });
            }
        } else if (state.mode == ParseMode::Vcl && ordinary > 1)
            || (state.mode == ParseMode::Raw && ordinary != 0 && ordinary != 2)
        {
            errors.push(ParseError {
                line,
                message: if state.mode == ParseMode::Vcl {
                    ParseMessage::MultipleInstructions
                } else {
                    ParseMessage::RawPairRequired
                },
            });
        }
    }
    validate_stream(&program, options.allow_incomplete, &mut errors);
    if errors.is_empty() {
        Ok(program)
    } else {
        Err(errors)
    }
}

// The original lexer carries C block-comment state across lines. A line whose
// first token is '=' continues the preceding instruction; it is not an opcode.
fn logical_lines(source: &str, errors: &mut Vec<ParseError>) -> Vec<(usize, String, bool)> {
    let mut lines: Vec<(usize, String, bool)> = Vec::new();
    let mut in_comment = false;
    let mut passthrough = false;
    for (index, source_line) in source.lines().enumerate() {
        let line = index + 1;
        if passthrough {
            if source_line.trim_start().starts_with("*----") {
                passthrough = false;
            } else {
                lines.push((line, source_line.to_string(), true));
            }
            continue;
        }
        if !in_comment && source_line.trim_start().starts_with("----*") {
            passthrough = true;
            continue;
        }
        let bytes = source_line.as_bytes();
        let mut out = String::new();
        let mut position = 0;
        while position < bytes.len() {
            if in_comment {
                if bytes[position..].starts_with(b"*/") {
                    in_comment = false;
                    position += 2;
                    out.push(' ');
                } else {
                    position += 1;
                }
            } else if bytes[position..].starts_with(b"/*") {
                in_comment = true;
                position += 2;
                out.push(' ');
            } else if bytes[position..].starts_with(b"//") || bytes[position] == b';' {
                break;
            } else if bytes[position].is_ascii() {
                out.push(char::from(bytes[position]));
                position += 1;
            } else {
                errors.push(ParseError {
                    line,
                    message: ParseMessage::NonAscii,
                });
                position += 1;
            }
        }
        let trimmed = out.trim();
        if trimmed == "=" || trimmed.starts_with("= ") || trimmed.starts_with("=\t") {
            if let Some(previous) = lines.last_mut().filter(|previous| !previous.2) {
                previous.1.push(' ');
                previous.1.push_str(trimmed[1..].trim());
            } else {
                errors.push(ParseError {
                    line,
                    message: ParseMessage::UnexpectedContinuation,
                });
            }
        } else if !trimmed.is_empty() {
            lines.push((line, out, false));
        }
    }
    // The original accepts EOF while a block comment or passthrough block is
    // open; retain that behavior instead of manufacturing a new diagnostic.
    lines
}

fn tokenize(source: &str) -> Result<Vec<Token>, ParseMessage> {
    let mut result: Vec<Token> = Vec::new();
    let mut token = String::new();
    for c in source.chars() {
        if c.is_ascii_whitespace() || c == ',' {
            if !token.is_empty() {
                result.push(Token {
                    text: std::mem::take(&mut token),
                    comma: false,
                });
            }
            if c == ',' {
                let previous = result.last_mut().ok_or(ParseMessage::UnexpectedComma)?;
                if previous.comma {
                    return Err(ParseMessage::UnexpectedComma);
                }
                previous.comma = true;
            }
        } else {
            // A single colon terminates a label token, even with no following
            // whitespace. Operand gathering subsequently joins tag tokens.
            if token.ends_with(':') && !token.ends_with("::") && c != ':' {
                result.push(Token {
                    text: std::mem::take(&mut token),
                    comma: false,
                });
            }
            token.push(c);
        }
    }
    if !token.is_empty() {
        result.push(Token {
            text: token,
            comma: false,
        });
    }
    if result.len() > 200 {
        return Err(ParseMessage::TooManyTokens);
    }
    Ok(result)
}

fn argument(tokens: &[Token], index: usize, directive: &str) -> Result<String, ParseMessage> {
    tokens
        .get(index)
        .map(|token| token.text.clone())
        .ok_or_else(|| ParseMessage::MissingDirectiveArgument {
            directive: directive.into(),
        })
}

fn parse_directive(tokens: &[Token], state: &mut State) -> Result<Directive, ParseMessage> {
    let name = tokens[0].text.to_ascii_lowercase();
    let args: Vec<String> = tokens[1..].iter().map(|token| token.text.clone()).collect();
    let reorder = || -> Result<bool, ParseMessage> {
        match args
            .first()
            .map(|argument| argument.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("noreorder") => Ok(false),
            Some("reorder") => Ok(true),
            Some(value) => Err(ParseMessage::BadRawOption {
                value: value.into(),
            }),
        }
    };
    Ok(match name.as_str() {
        ".vu" => Directive::Vu,
        ".mpg" => Directive::Mpg(argument(tokens, 1, &name)?),
        ".name" => Directive::Name(argument(tokens, 1, &name)?),
        ".global" => Directive::Global(args.first().cloned().unwrap_or_default()),
        ".syntax" => {
            state.syntax = match argument(tokens, 1, &name)?.to_ascii_lowercase().as_str() {
                "new" => Syntax::New,
                "old" => Syntax::Old,
                _ => return Err(ParseMessage::BadSyntax),
            };
            Directive::Syntax(state.syntax)
        }
        ".init_vf" | ".init_vi" | ".rem_vf" | ".rem_vi" => {
            let vector = name.ends_with("vf");
            for register in &args {
                validate_register_range(register, vector)?;
            }
            Directive::InitRegisters {
                vector,
                enable: name.starts_with(".init"),
                names: args,
            }
        }
        ".init_vf_all" | ".init_vi_all" => Directive::InitAll {
            vector: name == ".init_vf_all",
        },
        ".raw" | ".vsm" => {
            if state.raw_loop {
                return Err(ParseMessage::RawInRawLoop);
            }
            state.mode = ParseMode::Raw;
            state.reorder = reorder()?;
            Directive::Raw {
                reorder: state.reorder,
            }
        }
        ".endraw" | ".endvsm" => {
            if state.mode != ParseMode::Raw {
                return Err(ParseMessage::EndRawOutsideRaw);
            }
            if state.raw_loop {
                return Err(ParseMessage::EndRawInRawLoop);
            }
            state.mode = ParseMode::Vcl;
            state.reorder = false;
            Directive::EndRaw
        }
        ".rawloop" | ".vsmloop" => {
            if state.raw_loop {
                return Err(ParseMessage::NestedRawLoop);
            }
            state.saved_mode = state.mode;
            // The original saves the mode but does not save the submode;
            // restoring it yields the zero-initialized submode byte.
            state.saved_reorder = false;
            state.mode = ParseMode::Raw;
            state.reorder = reorder()?;
            state.raw_loop = true;
            Directive::RawLoop {
                reorder: state.reorder,
            }
        }
        ".endrawloop" | ".endvsmloop" => {
            if !state.raw_loop {
                return Err(ParseMessage::EndRawLoopOutsideRawLoop);
            }
            state.mode = state.saved_mode;
            state.reorder = state.saved_reorder;
            state.raw_loop = false;
            Directive::EndRawLoop
        }
        ".vu_work_area" => {
            let device = argument(tokens, 1, &name)?.to_ascii_lowercase();
            let capacity = match device.as_str() {
                "vu0" => 0x100,
                "vu1" => 0x400,
                "core" => {
                    return Err(ParseMessage::CoreWorkArea);
                }
                _ => return Err(ParseMessage::WorkAreaDevice),
            };
            let address = decimal_prefix(&argument(tokens, 2, &name)?);
            let size = decimal_prefix(&argument(tokens, 3, &name)?);
            if size <= 0 || address < 0 || i64::from(address) + i64::from(size) > capacity {
                return Err(ParseMessage::WorkAreaOutsideMemory);
            }
            Directive::WorkArea {
                device,
                address,
                size: size as u32,
            }
        }
        ".align" => Directive::Align(args),
        ".noaligns" => Directive::NoAligns,
        ".noebits" => Directive::NoEbits,
        ".unique" => {
            if args.first().is_some_and(|value| value.len() > 128) {
                return Err(ParseMessage::UniqueTooLong);
            }
            Directive::Unique(args.first().cloned())
        }
        ".show" | ".hide" => Directive::Visibility {
            show: name == ".show",
            names: args,
        },
        ".endshow" | ".endhide" => Directive::EndVisibility {
            show: name == ".endshow",
        },
        _ => return Err(ParseMessage::UnknownDirective { name }),
    })
}

/// The original uses decimal strtol and accepts trailing text.
fn decimal_prefix(value: &str) -> i32 {
    let value = value.trim_start();
    let negative = value.starts_with('-');
    let digits = value.trim_start_matches(['+', '-']);
    let magnitude = digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0_i64, |result, digit| {
            (result * 10 + i64::from(digit - b'0')).min(i64::from(i32::MAX) + 1)
        });
    if negative {
        (-magnitude).max(i64::from(i32::MIN)) as i32
    } else {
        magnitude.min(i64::from(i32::MAX)) as i32
    }
}

fn physical_register(value: &str, vector: bool) -> Option<u8> {
    let value = value.to_ascii_lowercase();
    let digits = value.strip_prefix(if vector { "vf" } else { "vi" })?;
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let number: u8 = digits.parse().ok()?;
    (number < if vector { 32 } else { 16 }).then_some(number)
}

fn validate_register_range(value: &str, vector: bool) -> Result<(), ParseMessage> {
    let bounds: Vec<_> = value.split('-').collect();
    if bounds.len() > 2 {
        return Err(ParseMessage::RegisterRangeDashes {
            range: value.into(),
        });
    }
    for bound in bounds {
        if physical_register(bound, vector).is_none() {
            return Err(ParseMessage::NotAllocablePhysical {
                register: bound.into(),
            });
        }
        if physical_register(bound, vector) == Some(0) {
            return Err(ParseMessage::RegisterNotAllocable {
                register: bound.into(),
            });
        }
    }
    Ok(())
}

fn strip_modifiers(value: &str) -> (String, u8) {
    let mut result = String::new();
    let mut flags = 0;
    let mut input = value;
    while let Some(start) = input.find('[') {
        result.push_str(&input[..start]);
        let tail = &input[start + 1..];
        if let Some(end) = tail.find(']') {
            let mut current = 0;
            let valid = tail[..end].bytes().all(|byte| {
                current |= match byte.to_ascii_lowercase() {
                    b'm' => 1,
                    b'e' => 2,
                    b'd' => 4,
                    b't' => 8,
                    _ => return false,
                };
                true
            });
            if valid {
                flags |= current;
            } else {
                result.push_str(&input[start..start + end + 2]);
            }
            input = &tail[end + 1..];
        } else {
            result.push_str(&input[start..]);
            input = "";
        }
    }
    result.push_str(input);
    (result, flags)
}

fn opcode_parts(value: &str) -> (String, Option<Mask>, u8, bool, u16) {
    let (without_flags, flags) = strip_modifiers(value);
    let optional = without_flags.starts_with('~');
    let mut opcode = without_flags.trim_start_matches('~');
    let mut loop_id = 0;
    if let Some((prefix, rest)) = opcode.split_once("..") {
        loop_id = if prefix == "+" {
            0x1024
        } else {
            decimal_prefix(prefix) as u16
        };
        opcode = rest;
    }
    let (name, mask) = match opcode.split_once('.') {
        Some((name, mask)) => (name, Mask::parse(mask)),
        None => (opcode, None),
    };
    (name.to_ascii_lowercase(), mask, flags, optional, loop_id)
}

fn starts_instruction(value: &str) -> bool {
    let (name, _, _, _, _) = opcode_parts(value);
    instruction::lookup(&name, Syntax::Old).is_some()
}

fn parse_instruction(
    tokens: &[Token],
    cursor: &mut usize,
    state: &State,
) -> Result<Instruction, ParseMessage> {
    let source_opcode = &tokens[*cursor].text;
    let (name, specified_mask, modifiers, optional, explicit_loop_id) = opcode_parts(source_opcode);
    let metadata = instruction::lookup(&name, state.syntax).ok_or_else(|| {
        ParseMessage::UnknownInstruction {
            opcode: source_opcode.to_string(),
        }
    })?;
    if source_opcode.contains('.') && !source_opcode.contains("..") && specified_mask.is_none() {
        return Err(ParseMessage::InvalidInstructionMask);
    }
    if explicit_loop_id > 32 && explicit_loop_id != 0x1024 {
        return Err(ParseMessage::LoopIdTooLarge);
    }
    let allowed_flags = if state.mode == ParseMode::Raw {
        0x0d
    } else {
        0x0c
    };
    if modifiers & !allowed_flags != 0 {
        return Err(ParseMessage::InstructionBits);
    }
    if state.mode == ParseMode::Vcl && metadata.kind == InstructionKind::RawOnly {
        return Err(ParseMessage::RawOnlyInstruction { name: name.clone() });
    }
    let mut mask = match (metadata.mask_mode, specified_mask) {
        (MaskMode::Unmasked, Some(_)) => {
            return Err(ParseMessage::MaskNotAccepted { name: name.clone() });
        }
        (MaskMode::Xyz, None | Some(Mask::XYZ)) => Mask::XYZ,
        (MaskMode::Xyz, _) => return Err(ParseMessage::XyzMaskRequired),
        (_, Some(mask)) if mask.0 == 0 => return Err(ParseMessage::EmptyMask),
        (_, Some(mask)) => mask,
        _ => Mask::XYZW,
    };
    *cursor += 1;

    let texts = read_operand_texts(tokens, cursor, metadata, &name)?;
    let mut operands = parse_operands(
        metadata,
        &texts,
        mask,
        specified_mask.is_some(),
        state.syntax,
    )?;
    if metadata.index < id::GENERALIZED_END
        && operands.iter().take(2).any(|operand| {
            operand.class == class::GENERALIZED && bracket_field(&operand.text).is_some()
        })
    {
        return Err(ParseMessage::ScalarNotThird);
    }
    if specified_mask.is_none() {
        mask = infer_mask(&operands, mask)?;
    }
    for operand in &mut operands {
        if operand.implicit && operand.class == class::ACC {
            operand.mask = mask;
        }
    }

    let mut metadata = if metadata.index < id::GENERALIZED_END {
        concretize_generalized(&name, &mut operands)?
    } else {
        metadata
    };
    metadata = apply_operand_hooks(metadata, &mut operands);
    if metadata.mask_mode == MaskMode::OneLane
        && metadata.subtype == subtype::memory::LOAD
        && !mask.is_scalar()
        && specified_mask.is_some()
    {
        return Err(ParseMessage::SingleComponentRequired);
    }
    let tagged = operands
        .iter()
        .filter(|operand| !operand.tags.is_empty())
        .count();
    if tagged > 1 {
        return Err(ParseMessage::MultipleTaggedOperands);
    }
    normalize_zero_sources(metadata.index, &mut operands);

    Ok(Instruction {
        metadata,
        mnemonic: metadata.name.into(),
        mask,
        modifiers,
        operands,
        mode: state.mode,
        raw_reorder: state.reorder,
        // The parser's raw-loop group counter is distinct from
        // the decoded instruction's parse loop id.
        // A raw-loop directive does not supply an implicit stage prefix.
        loop_id: explicit_loop_id,
        optional,
        generated_copy: None,
    })
}

/// Gather the token text of each explicit operand, advancing the cursor.
fn read_operand_texts(
    tokens: &[Token],
    cursor: &mut usize,
    metadata: &InstructionDef,
    name: &str,
) -> Result<Vec<String>, ParseMessage> {
    let explicit_count = metadata.explicit_operands().count();
    let mut texts = Vec::new();
    for operand_index in 0..explicit_count {
        let mut text = String::new();
        let last = operand_index + 1 == explicit_count;
        while *cursor < tokens.len() {
            let token = &tokens[*cursor];
            if last && !text.is_empty() && starts_instruction(&token.text) {
                break;
            }
            text.push_str(&token.text);
            *cursor += 1;
            if text.len() > 1023 {
                return Err(ParseMessage::OperandTooLong);
            }
            if token.comma {
                break;
            }
        }
        // --exitm's empty macro operand is an intentional exception.
        if text.is_empty()
            && !(metadata.is_synthetic() && metadata.subtype == subtype::synthetic::EXIT_MACRO)
        {
            return Err(ParseMessage::MissingOperand {
                position: operand_index + 1,
                name: name.to_string(),
            });
        }
        texts.push(text);
    }
    Ok(texts)
}

/// Take the instruction mask from the first vector-like operand when none
/// was written, checking that the vector operands agree.
fn infer_mask(operands: &[Operand], mut mask: Mask) -> Result<Mask, ParseMessage> {
    if let Some(operand) = operands.iter().find(|operand| {
        use class::*;
        !operand.implicit
            && matches!(
                operand.class,
                VECTOR
                    | ACC
                    | DECLARED_VECTOR
                    | GENERALIZED
                    | ADDRESS_REGISTER
                    | ADDRESS_REGISTER_OFFSET
            )
    }) {
        mask = operand.mask;
    }
    let mismatched = operands
        .iter()
        .filter(|operand| {
            !operand.implicit
                && matches!(
                    operand.class,
                    class::VECTOR | class::ACC | class::DECLARED_VECTOR
                )
        })
        .any(|operand| operand.mask != mask);
    if mismatched {
        return Err(ParseMessage::VectorMasksDiffer);
    }
    Ok(mask)
}

/// Resolve a generalized spelling to its concrete opcode and retype the
/// operands accordingly.
fn concretize_generalized(
    name: &str,
    operands: &mut [Operand],
) -> Result<&'static InstructionDef, ParseMessage> {
    let derived = derive_generalized_name(name, operands)?;
    let concrete =
        instruction::lookup(&derived, Syntax::Old).ok_or_else(|| ParseMessage::CannotDerive {
            name: derived.clone(),
        })?;
    // Generalized spellings carry I/Q/vector lane selection in operands;
    // adapt the explicit strings to the concrete opcode before decoding.
    for (operand, spec) in operands.iter_mut().zip(concrete.operands) {
        operand.class = spec.class;
        operand.direction = spec.direction;
        // The generalized third operand was decoded as GENERALIZED with
        // a vector mask. Resolving it to a scalar I/Q/P/R resource changes
        // its component count as well as its class.
        if matches!(spec.class, class::I..=class::R) {
            operand.mask = Mask::X;
        }
    }
    Ok(concrete)
}

/// Apply the original's per-opcode operand hooks (register-address
/// loads/stores, EFU input fields); returns the possibly replaced descriptor.
fn apply_operand_hooks(
    mut metadata: &'static InstructionDef,
    operands: &mut [Operand],
) -> &'static InstructionDef {
    // The original's hook for register-address loads/stores replaces register-address ILWR/ISWR with the
    // corresponding immediate-address form and a signed zero displacement.
    if let Some(name) = match metadata.name {
        "ilwr" => Some("ilw"),
        "iswr" => Some("isw"),
        _ => None,
    } {
        metadata = instruction::lookup(name, Syntax::Old).expect("original table has ILW/ISW");
        if let Some(address) = operands.get_mut(1) {
            address.class = class::ADDRESS_REGISTER_OFFSET;
            address.immediate = Some("0".into());
        }
    }
    // The original's EFU hook installs the fixed input fields before flow.
    // These descriptor operands are FL4 even for XYZ/XY/XZ operations.
    if let Hook::Efu(fields) = metadata.hook
        && !fields.is_scalar()
    {
        operands[1].mask = fields;
    }
    metadata
}

fn is_zero_register(operand: &Operand) -> bool {
    operand.register.as_deref().is_some_and(|name| {
        let name = name.to_ascii_lowercase();
        let prefix = match instruction::register_family(operand.class) {
            instruction::RegisterFamily::Vector => "vf",
            instruction::RegisterFamily::Integer => "vi",
            _ => return false,
        };
        name.strip_prefix(prefix)
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b == b'0'))
    })
}

/// The decoder's normalizer runs on every decoded instruction.
/// ADD/IADD move a pool-zero first source behind a nonzero second source;
/// SUB/ISUB with one register in both sources read the pool zero twice.
fn normalize_zero_sources(index: u16, operands: &mut [Operand]) {
    if operands.len() < 3 {
        return;
    }
    match index {
        id::ADD | id::IADD => {
            if is_zero_register(&operands[1]) && !is_zero_register(&operands[2]) {
                let (first, second) = operands.split_at_mut(2);
                std::mem::swap(&mut first[1].register, &mut second[0].register);
                std::mem::swap(&mut first[1].text, &mut second[0].text);
                first[1].forced_register = second[0].forced_register.take();
            }
        }
        id::SUB | id::ISUB => {
            let same = match (&operands[1].register, &operands[2].register) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => false,
            };
            if same {
                let zero = if index == id::SUB { "vf00" } else { "vi00" };
                for operand in &mut operands[1..3] {
                    operand.register = Some(zero.into());
                    operand.text = zero.into();
                    operand.forced_register = None;
                }
            }
        }
        _ => {}
    }
}

fn derive_generalized_name(name: &str, operands: &[Operand]) -> Result<String, ParseMessage> {
    let mut name = name.to_string();
    if operands.iter().take(2).any(|operand| {
        operand.class == class::GENERALIZED && bracket_field(&operand.text).is_some()
    }) {
        return Err(ParseMessage::ScalarNotThird);
    }
    if operands.first().is_some_and(|operand| {
        operand
            .register
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("acc"))
    }) && !name.ends_with('a')
    {
        name.push('a');
    }
    if let Some(operand) = operands.get(2) {
        match operand.register.as_deref() {
            Some(register) if register.eq_ignore_ascii_case("i") => name.push('i'),
            Some(register) if register.eq_ignore_ascii_case("q") => name.push('q'),
            _ if bracket_field(&operand.text).is_some() && operand.mask.is_scalar() => {
                name.push_str(&operand.mask.suffix())
            }
            _ => {}
        }
    }
    if operands
        .iter()
        .skip(1)
        .filter(|operand| !operand.implicit)
        .any(|operand| {
            operand
                .register
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("acc"))
        })
    {
        return Err(ParseMessage::AccNotFirst);
    }
    Ok(name)
}

fn parse_operands(
    metadata: &InstructionDef,
    texts: &[String],
    mask: Mask,
    forced_mask: bool,
    syntax: Syntax,
) -> Result<Vec<Operand>, ParseMessage> {
    let mut result = Vec::new();
    let mut explicit = 0;
    for spec in metadata.operands {
        if spec.implicit {
            let register = if spec.patch != 0 {
                result
                    .get(usize::from(spec.patch - 1))
                    .and_then(|operand: &Operand| operand.register.clone())
            } else {
                instruction::implicit_register(spec.class).map(str::to_string)
            };
            let implicit_mask = match spec.class {
                class::ACC => mask,
                class::ACC_XYZ => Mask::XYZ,
                _ => Mask::X,
            };
            result.push(Operand {
                text: register.clone().unwrap_or_default(),
                class: spec.class,
                direction: spec.direction,
                implicit: true,
                mask: implicit_mask,
                register,
                forced_register: None,
                immediate: None,
                immediate_kind: spec.immediate_kind,
                tags: Vec::new(),
                field_letters: None,
            });
        } else {
            result.push(parse_operand(
                &texts[explicit],
                *spec,
                mask,
                forced_mask,
                syntax,
            )?);
            explicit += 1;
        }
    }
    Ok(result)
}

/// Splits a memory operand as the original's token decoder
/// does, returning the displacement end, the address register and the
/// closing parenthesis. The decoder checks each class's shape on the text
/// after tags are removed:
///
/// - 28 `(reg)` and 29 `off(reg)`: at least 3 bytes, ending in `)`. 29's
///   register follows the last `(`, so a displacement keeps its own
///   parentheses (`(0x10+2)(vi01)`).
/// - 30 `(reg)mask` (ILWR/ISWR): at least 4 bytes, starting with `(`.
/// - 31 `off(reg)mask` (ILW/ISW): at least 5 bytes.
/// - 32 `(reg++)` and 33 `(--reg)`: at least 5 bytes.
///
/// In the register forms only lowercase `x`, `y`, `z` and `w` may follow the `)`.
/// The length floors count the mask. So `ilw.x t,0(p)` and `ilwr.x t,(p)`
/// fail while `0(p)x`, `10(p)` and `(p)x` decode, whatever the register is
/// named.
fn memory_operand(body: &str, operand_class: u8) -> Result<(usize, &str, usize), ParseMessage> {
    use class::*;
    let bytes = body.as_bytes();
    let length = bytes.len();
    let minimum = match operand_class {
        ADDRESS | ADDRESS_OFFSET => 3,
        ADDRESS_REGISTER => 4,
        _ => 5,
    };
    let malformed = || ParseMessage::MalformedMemoryOperand {
        operand: body.to_string(),
    };
    if length < minimum {
        return Err(malformed());
    }
    let last = length - 1;
    match operand_class {
        ADDRESS | ADDRESS_INCREMENT | ADDRESS_DECREMENT
            if bytes[0] != b'(' || bytes[last] != b')' =>
        {
            Err(malformed())
        }
        ADDRESS => Ok((0, &body[1..last], last)),
        ADDRESS_OFFSET => {
            if bytes[last] != b')' {
                return Err(malformed());
            }
            let open = body[..last].rfind('(').ok_or_else(malformed)?;
            Ok((open, &body[open + 1..last], last))
        }
        ADDRESS_REGISTER | ADDRESS_REGISTER_OFFSET => {
            let lanes = bytes
                .iter()
                .rev()
                .take_while(|byte| matches!(byte, b'x' | b'y' | b'z' | b'w'))
                .count();
            if lanes == length || bytes[last - lanes] != b')' {
                return Err(malformed());
            }
            let close = last - lanes;
            let open = if operand_class == ADDRESS_REGISTER {
                (bytes[0] == b'(').then_some(0)
            } else {
                body[..close].rfind('(')
            }
            .ok_or_else(malformed)?;
            Ok((open, &body[open + 1..close], close))
        }
        ADDRESS_INCREMENT => body[1..last]
            .strip_suffix("++")
            .map(|register| (0, register, last))
            .ok_or(ParseMessage::PostincrementRequired),
        _ => body[1..last]
            .strip_prefix("--")
            .map(|register| (0, register, last))
            .ok_or(ParseMessage::PredecrementRequired),
    }
}

/// The original bracket decoder recognizes only lane letters.
/// A trailing numeric index remains part of the virtual register name.
fn bracket_field(text: &str) -> Option<(&str, Mask)> {
    let (name, fields) = text.strip_suffix(']')?.rsplit_once('[')?;
    Some((name, Mask::parse(fields)?))
}

fn parse_operand(
    text: &str,
    spec: OperandSpec,
    instruction_mask: Mask,
    forced_mask: bool,
    syntax: Syntax,
) -> Result<Operand, ParseMessage> {
    let (body, tag_string) = text
        .split_once(':')
        .map_or((text, ""), |(body, tags)| (body, tags));
    let tags = tag_string
        .split(':')
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect();
    let mut operand = Operand {
        text: body.to_string(),
        class: spec.class,
        direction: spec.direction,
        implicit: false,
        mask: Mask::X,
        register: None,
        forced_register: None,
        immediate: None,
        immediate_kind: spec.immediate_kind,
        tags,
        field_letters: None,
    };
    use class::*;
    match spec.class {
        IMMEDIATE => {
            operand.immediate = Some(body.to_string());
            return Ok(operand);
        }
        ADDRESS..=ADDRESS_DECREMENT => {
            parse_memory_operand(
                &mut operand,
                body,
                spec.class,
                instruction_mask,
                forced_mask,
            )?;
            return Ok(operand);
        }
        _ => {}
    }

    let mut register = body.to_string();
    if matches!(spec.class, DECLARED_VECTOR | DECLARED_INTEGER)
        && let Some((name, mapping)) = body.split_once('(')
    {
        register = name.into();
        operand.forced_register = Some(mapping.trim_end_matches(')').to_string());
    }
    let (register, operand_mask) = strip_field_suffix(&mut operand, register, spec.class, syntax)?;
    assign_vector_mask(
        &mut operand,
        spec.class,
        operand_mask,
        instruction_mask,
        forced_mask,
    )?;
    let register = fixed_register_name(spec.class, register)?;

    validate_register_name(&register)?;
    if let Some(mapping) = &operand.forced_register {
        let vector = spec.class == DECLARED_VECTOR;
        if physical_register(mapping, vector).is_none() {
            return Err(ParseMessage::ForcedRegisterNotPhysical {
                register: mapping.clone(),
            });
        }
    }
    operand.register = Some(register);
    Ok(operand)
}

/// Decode a memory operand's displacement, register and lane mask into
/// `operand`.
fn parse_memory_operand(
    operand: &mut Operand,
    body: &str,
    operand_class: u8,
    instruction_mask: Mask,
    forced_mask: bool,
) -> Result<(), ParseMessage> {
    use class::*;
    let (open, register, close) = memory_operand(body, operand_class)?;
    if matches!(operand_class, ADDRESS_OFFSET | ADDRESS_REGISTER_OFFSET) {
        operand.immediate = Some(if open == 0 { "0" } else { &body[..open] }.into());
    }
    if matches!(operand_class, ADDRESS_REGISTER | ADDRESS_REGISTER_OFFSET) {
        operand.mask = Mask::parse(&body[close + 1..]).ok_or(ParseMessage::InvalidMemoryMask)?;
        if operand.mask.0 == 0 {
            operand.mask = if forced_mask {
                instruction_mask
            } else {
                return Err(ParseMessage::MemoryMaskRequired);
            };
        }
        if forced_mask && operand.mask != instruction_mask {
            return Err(ParseMessage::MemoryMaskDiffers);
        }
    }
    validate_register_name(register)?;
    operand.register = Some(register.to_string());
    Ok(())
}

/// Strip the field suffix from a register name, per syntax and operand
/// class; returns the bare name and the fields it selected, if any.
fn strip_field_suffix(
    operand: &mut Operand,
    mut register: String,
    operand_class: u8,
    syntax: Syntax,
) -> Result<(String, Option<Mask>), ParseMessage> {
    use class::*;
    let mut operand_mask = None;
    if syntax == Syntax::New
        && matches!(
            operand_class,
            SCALAR | VECTOR_XYZ | FIELD_X..=FIELD_W | GENERALIZED
        )
    {
        if let Some((name, mask)) = bracket_field(&register) {
            if operand_class == GENERALIZED && !mask.is_scalar() {
                return Err(ParseMessage::GeneralizedScalarComponent);
            }
            operand_mask = Some(mask);
            register = name.to_string();
        }
    } else if syntax == Syntax::Old
        && matches!(
            operand_class,
            VECTOR | SCALAR | VECTOR_XYZ | FIELD_X..=FIELD_W | DECLARED_VECTOR
        )
    {
        operand_mask = strip_lane_letters(operand, &mut register, operand_class)?;
    } else if let Some((name, suffix)) = register
        .rsplit_once('.')
        // An interface name takes no dotted fields except an old-syntax
        // vector one: vcl rejects `in_vi cnt.x(vi01)`, and in new syntax
        // `out_vf res.xyz(vf02)`, with its invalid-character error (observed
        // from vcl 1.4beta7, `field-letters-new-*`).
        .filter(|_| {
            !(operand_class == DECLARED_INTEGER
                || (operand_class == DECLARED_VECTOR && syntax == Syntax::New))
        })
    {
        operand_mask = Some(Mask::parse(suffix).ok_or(ParseMessage::InvalidOperandMask)?);
        register = name.into();
    }
    Ok((register, operand_mask))
}

/// Old syntax: strip a trailing lane-letter string from `register`, and
/// return the fields it selects.
fn strip_lane_letters(
    operand: &mut Operand,
    register: &mut String,
    operand_class: u8,
) -> Result<Option<Mask>, ParseMessage> {
    use class::*;
    let mut operand_mask = None;
    // The original scans a maximal case-insensitive trailing lane string
    // and strips that string and its optional preceding dot, in vector
    // interface operands too: `in_vf caty(vf01)` binds `cat`. A name made
    // only of lane letters stays whole.
    let start = register
        .trim_end_matches(['x', 'X', 'y', 'Y', 'z', 'Z', 'w', 'W'])
        .len();
    if start > 0 && start < register.len() {
        let fields = &register[start..];
        let mask = Mask::parse(fields).expect("trimmed suffix contains only lane letters");
        if mask.0.count_ones() as usize != fields.len() {
            return Err(ParseMessage::DuplicateComponent);
        }
        let token = register.clone();
        register.truncate(start);
        let dotted = register.ends_with('.');
        // An interface operand takes its fields only from a dotted
        // suffix (`res.xyz`); glued letters (`resxyz`) only shorten the
        // name and the operand keeps every field (observed from vcl
        // 1.4beta7, `field-letters-*`).
        if dotted || operand_class != DECLARED_VECTOR {
            operand_mask = Some(mask);
        }
        if dotted {
            register.pop();
        } else if matches!(operand_class, VECTOR | DECLARED_VECTOR)
            && physical_register(register, true).is_none()
            && physical_register(register, false).is_none()
        {
            // An ordinary or interface operand: a broadcast (`scalex`)
            // selects its field this way, but these do not.
            operand.field_letters = Some(token);
        }
    }
    Ok(operand_mask)
}

/// Set the lane mask of a vector-like operand from the class's fixed
/// field, the suffix and the instruction mask.
fn assign_vector_mask(
    operand: &mut Operand,
    operand_class: u8,
    operand_mask: Option<Mask>,
    instruction_mask: Mask,
    forced_mask: bool,
) -> Result<(), ParseMessage> {
    use class::*;
    let required_mask = match operand_class {
        VECTOR_XYZ | ACC_XYZ => Some(Mask::XYZ),
        FIELD_X => Some(Mask::X),
        FIELD_Y => Some(Mask::Y),
        FIELD_Z => Some(Mask::Z),
        FIELD_W => Some(Mask::W),
        _ => None,
    };
    let vector = matches!(
        operand_class,
        VECTOR | SCALAR | ACC..=FIELD_W | DECLARED_VECTOR | GENERALIZED
    );
    if vector {
        if operand_class == SCALAR && !operand_mask.is_some_and(Mask::is_scalar) {
            return Err(ParseMessage::ScalarComponentRequired);
        }
        if let Some(required) = required_mask {
            if operand_mask.is_some_and(|mask| mask != required) {
                return Err(ParseMessage::FieldSelectionsDiffer);
            }
            operand.mask = required;
        } else if forced_mask && !matches!(operand_class, SCALAR | GENERALIZED) {
            operand.mask = instruction_mask;
        } else {
            operand.mask = operand_mask.unwrap_or(instruction_mask);
        }
    }
    Ok(())
}

/// The canonical name of a fixed hardware register operand (I, P, Q, R,
/// ACC, CLIP, the integer flag), checking the spelling; other classes keep
/// their name.
fn fixed_register_name(operand_class: u8, register: String) -> Result<String, ParseMessage> {
    use class::*;
    match operand_class {
        I | P | Q | R => {
            let expected = match operand_class {
                I => "I",
                P => "P",
                Q => "Q",
                _ => "R",
            };
            if !register.eq_ignore_ascii_case(expected) {
                return Err(ParseMessage::OperandMustBe {
                    expected: expected.to_string(),
                });
            }
            Ok(expected.into())
        }
        ACC | ACC_XYZ => Ok("ACC".into()),
        CLIP => Ok("CLIP".into()),
        INTEGER_FLAG if physical_register(&register, false) != Some(1) => {
            Err(ParseMessage::OperandMustBe {
                expected: "vi01".into(),
            })
        }
        _ => Ok(register),
    }
}

fn validate_register_name(name: &str) -> Result<(), ParseMessage> {
    if name.is_empty() {
        return Err(ParseMessage::EmptyRegisterName);
    }
    if name.len() >= 63 {
        return Err(ParseMessage::RegisterNameTooLong { name: name.into() });
    }
    if let Some(character) = name
        .chars()
        .find(|&c| !(c.is_ascii_alphanumeric() || "[]_$".contains(c)))
    {
        return Err(ParseMessage::InvalidRegisterCharacter {
            character,
            name: name.into(),
        });
    }
    if name.len() >= 2 && instruction::lookup(name, Syntax::New).is_some() {
        return Err(ParseMessage::ReservedRegisterName { name: name.into() });
    }
    Ok(())
}

fn validate_stream(program: &Program, allow_incomplete: bool, errors: &mut Vec<ParseError>) {
    let mut context = None;
    let mut enters = 0;
    let mut exits = 0;
    let mut labels = std::collections::HashSet::new();
    let mut visibility = Vec::new();
    for statement in &program.statements {
        let failure = match &statement.kind {
            StatementKind::Label(label) if !labels.insert(label.name.clone()) => {
                Some(ParseMessage::DuplicateLabel {
                    name: label.name.clone(),
                })
            }
            StatementKind::Directive(Directive::Visibility { show, .. }) => {
                visibility.push(*show);
                None
            }
            StatementKind::Directive(Directive::EndVisibility { show })
                if visibility.last().is_some_and(|previous| previous != show) =>
            {
                Some(ParseMessage::VisibilityMismatch)
            }
            StatementKind::Directive(Directive::EndVisibility { .. }) => {
                visibility.pop();
                None
            }
            StatementKind::Instruction(instruction) => match instruction.synthetic() {
                Some(Synthetic::Enter) if context.is_none() => {
                    context = Some(Synthetic::Enter);
                    enters += 1;
                    None
                }
                Some(Synthetic::EndEnter) if context == Some(Synthetic::Enter) => {
                    context = None;
                    None
                }
                Some(Synthetic::Exit | Synthetic::ExitMacro) if context.is_none() => {
                    context = Some(Synthetic::Exit);
                    exits += 1;
                    None
                }
                Some(Synthetic::EndExit) if context == Some(Synthetic::Exit) => {
                    context = None;
                    None
                }
                Some(Synthetic::Input) if context == Some(Synthetic::Enter) => None,
                Some(Synthetic::Output) if context == Some(Synthetic::Exit) => None,
                Some(Synthetic::Continue) if context.is_none() => {
                    exits += 1;
                    None
                }
                Some(
                    Synthetic::Enter
                    | Synthetic::EndEnter
                    | Synthetic::Exit
                    | Synthetic::ExitMacro
                    | Synthetic::EndExit
                    | Synthetic::Input
                    | Synthetic::Output
                    | Synthetic::Continue,
                ) => Some(ParseMessage::SyntheticContext),
                Some(Synthetic::LoopExtra | Synthetic::LoopAbsolute)
                    if instruction.mode == ParseMode::Raw =>
                {
                    Some(ParseMessage::LoopSizeInRaw)
                }
                None if context.is_some() => Some(ParseMessage::InstructionInEnterExit),
                _ => None,
            },
            _ => None,
        };
        if let Some(message) = failure {
            errors.push(ParseError {
                line: statement.line,
                message,
            });
        }
    }
    if !allow_incomplete {
        let line = program
            .statements
            .last()
            .map_or(1, |statement| statement.line);
        if context.is_some() {
            errors.push(ParseError {
                line,
                message: ParseMessage::UnterminatedEnterExit,
            });
        }
        if enters == 0 {
            errors.push(ParseError {
                line,
                message: ParseMessage::NoEnter,
            });
        }
        if exits == 0 {
            errors.push(ParseError {
                line,
                message: ParseMessage::NoExit,
            });
        }
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(source: &str) -> Program {
        parse_with_options(
            source,
            ParserOptions {
                allow_incomplete: true,
                ..ParserOptions::default()
            },
        )
        .unwrap_or_else(|errors| panic!("{source:?}: {errors:?}"))
    }

    fn instructions(program: &Program) -> Vec<&Instruction> {
        program
            .statements
            .iter()
            .filter_map(|statement| match &statement.kind {
                StatementKind::Instruction(instruction) => Some(instruction),
                _ => None,
            })
            .collect()
    }

    fn rejected(source: &str, needle: impl std::fmt::Display) {
        let needle = needle.to_string();
        let errors = parse_with_options(
            source,
            ParserOptions {
                allow_incomplete: true,
                ..ParserOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.to_string().contains(&needle)),
            "{errors:?}"
        );
    }

    #[test]
    fn complete_program_retains_input_and_output_physical_bindings() {
        let program = parse(
            "--enter\n\
             in_vf a(vf01)\n\
             --endenter\n\
             add b,a,a\n\
             --exit\n\
             out_vf b(vf02)\n\
             --endexit\n",
        )
        .unwrap();
        let instructions = instructions(&program);
        assert_eq!(instructions[1].operands[0].register.as_deref(), Some("a"));
        assert_eq!(
            instructions[1].operands[0].forced_register.as_deref(),
            Some("vf01")
        );
        assert_eq!(instructions[3].metadata.index, 34);
        assert_eq!(
            instructions[5].operands[0].forced_register.as_deref(),
            Some("vf02")
        );
        assert_eq!(instructions[5].synthetic(), Some(Synthetic::Output));
    }

    #[test]
    fn incomplete_and_misplaced_synthetic_blocks_are_reported() {
        assert!(
            parse(
                "--enter\n\
                   --endenter\n"
            )
            .unwrap_err()
            .iter()
            .any(|error| error.message.to_string().contains("no exit"))
        );
        assert!(
            parse(
                "--enter\n\
                   --endenter\n\
                   --exitm\n"
            )
            .unwrap_err()
            .iter()
            .any(|error| error.message.to_string().contains("unterminated"))
        );
        rejected(
            "--exit\n\
                  in_vf a\n\
                  --endexit",
            "invalid enter/exit",
        );
        rejected(
            "--enter\n\
                  add b,a,a\n\
                  --endenter",
            "ordinary instruction",
        );
    }

    #[test]
    fn comments_match_the_binary_comment_modes() {
        let program = fragment(
            "; comment\n\
                      add b,a,a // comment\n\
                      /* long\n\
                      comment */ mul c,b,a ; tail\n",
        );
        let instructions = instructions(&program);
        assert_eq!(instructions.len(), 2);
        assert_eq!(program.statements[1].line, 4);
        rejected(
            "add b,a,a # note",
            "can't use '#' in the name of a register : 'a#note'",
        );
    }

    #[test]
    fn continuation_gathers_an_operand_across_lines() {
        let program = fragment(
            "add b,\n\
                                = a, a\n",
        );
        let instructions = instructions(&program);
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].operands[2].register.as_deref(), Some("a"));
        rejected("= add a,b,c", ParseMessage::UnexpectedContinuation);
    }

    #[test]
    fn labels_may_share_a_line_and_preserve_modifiers() {
        let program = fragment(
            "target:[m] add b,a,a\n\
                                b target\n",
        );
        assert_eq!(
            program.statements[0].kind,
            StatementKind::Label(Label {
                name: "target".into(),
                modifiers: 1
            })
        );
        assert_eq!(
            instructions(&program)[1].operands[0].immediate.as_deref(),
            Some("target")
        );
        rejected(
            "a:\n\
                  a:\n",
            "duplicate label",
        );
    }

    #[test]
    fn instruction_and_operand_masks_keep_lane_selections() {
        let program = fragment(
            "add.xyz b,a,a\n\
                                add b.xy,a.xy,a.xy\n\
                                addx.xz b,a,c\n",
        );
        let instructions = instructions(&program);
        assert_eq!(instructions[0].mask, Mask::XYZ);
        assert_eq!(instructions[1].mask, Mask(3));
        assert_eq!(instructions[2].operands[2].mask, Mask::X);
        rejected("add b.x,a.y,a.x", "component masks do not match");
        rejected("add.bad b,a,a", "component mask");
        rejected(
            ".raw\n\
                  nop.xyz nop",
            "does not accept",
        );
    }

    #[test]
    fn new_syntax_generalized_fmac_resolves_scalar_and_accumulator_forms() {
        let program = fragment(
            ".syntax new\n\
             add.xyz b,a,c[x]\n\
             mul acc,a,b\n\
             add b,a,I\n\
             add b,a,Q\n\
             add.x b,a,a\n\
             .syntax old\n\
             add b,a,a\n",
        );
        let instructions = instructions(&program);
        let names: Vec<_> = instructions
            .iter()
            .map(|instruction| instruction.mnemonic.as_str())
            .collect();
        assert_eq!(names, ["addx", "mula", "addi", "addq", "add", "add"]);
        assert_eq!(instructions[0].operands[2].register.as_deref(), Some("c"));
        assert_eq!(instructions[0].operands[2].class, class::FIELD_X);
        rejected(
            ".syntax new\n\
                  add b[x],a,a",
            "scalar can only",
        );
        rejected(
            ".syntax new\n\
                  add b,a,acc",
            "ACC can only",
        );
    }

    #[test]
    fn old_explicit_broadcast_requires_matching_scalar_field() {
        let program = fragment(
            "div Q,a.x,b.w\n\
                                addx b,a,cx\n",
        );
        let instructions = instructions(&program);
        assert_eq!(instructions[0].operands[1].mask, Mask::X);
        assert_eq!(instructions[0].operands[2].mask, Mask::W);
        assert_eq!(instructions[1].operands[2].register.as_deref(), Some("c"));
        rejected("div Q,a,b.w", "scalar operand requires");
        rejected("addx b,a,c.y", "field selections differ");
    }

    #[test]
    fn old_fields_strip_the_maximal_suffix_and_optional_dot() {
        let program = fragment(
            "mtir vi02,valuew\n\
             mtir vi03,value.W\n\
             add.xy dst,valuexy,value.xy\n\
             add.x dst,matrix,matrix",
        );
        let parsed = instructions(&program);
        for instruction in &parsed[..2] {
            assert_eq!(instruction.operands[1].register.as_deref(), Some("value"));
            assert_eq!(instruction.operands[1].mask, Mask::W);
        }
        assert_eq!(parsed[2].operands[1].register.as_deref(), Some("value"));
        assert_eq!(parsed[2].operands[1].mask, Mask(3));
        assert_eq!(parsed[3].operands[1].register.as_deref(), Some("matri"));
        rejected("mtir vi02,valuexx", "duplicate operand");
        rejected("mtir vi02,value", "scalar operand requires");
    }

    #[test]
    fn new_numeric_indices_are_names_and_lane_brackets_are_fields() {
        let program = fragment(
            ".syntax new\n\
             madd dst[0],model[0],model[0][w]\n\
             add.x dst[0],model[0],model[1]\n\
             add dst,matrix,matrix",
        );
        let parsed = instructions(&program);
        assert_eq!(parsed[0].mnemonic, "maddw");
        assert_eq!(parsed[0].operands[0].register.as_deref(), Some("dst[0]"));
        assert_eq!(parsed[0].operands[1].register.as_deref(), Some("model[0]"));
        assert_eq!(parsed[0].operands[2].register.as_deref(), Some("model[0]"));
        assert_eq!(parsed[0].operands[2].mask, Mask::W);
        assert_eq!(parsed[1].mnemonic, "add");
        assert_eq!(parsed[1].operands[2].register.as_deref(), Some("model[1]"));
        assert_eq!(parsed[2].operands[1].register.as_deref(), Some("matrix"));
        rejected(
            ".syntax new\n\
             add dst,model,model[xy]",
            "one selected component",
        );
        rejected(
            ".syntax new\n\
                  mtir vi02,valuex",
            "scalar operand requires",
        );
    }

    #[test]
    fn efu_hook_installs_fixed_input_fields_before_flow() {
        let program = fragment(
            "eatanxy P,value\n\
             eatanxz P,value\n\
             eleng P,value\n\
             esadd P,value\n\
             esum P,value\n\
             ersqrt P,valuew",
        );
        let parsed = instructions(&program);
        let masks: Vec<_> = parsed.iter().map(|i| i.operands[1].mask.0).collect();
        assert_eq!(masks, [3, 5, 7, 7, 15, 8]);
    }

    #[test]
    fn generalized_scalar_hardware_has_one_component_and_implicit_acc_is_allowed() {
        let program = fragment(
            ".syntax new\n\
             add b,a,I\n\
             mul b,a,Q\n\
             madd b,a,a\n\
             msub b,a,a\n\
             madda ACC,a,a\n\
             msuba ACC,a,a\n",
        );
        let parsed = instructions(&program);
        assert_eq!(parsed[0].metadata.name, "addi");
        assert_eq!(parsed[0].operands[2].mask, Mask::X);
        assert_eq!(parsed[1].metadata.name, "mulq");
        assert_eq!(parsed[1].operands[2].mask, Mask::X);
        assert_eq!(parsed[2].metadata.name, "madd");
        assert!(
            parsed[2]
                .operands
                .iter()
                .any(|operand| operand.implicit && operand.class == class::ACC)
        );
        assert_eq!(parsed[3].metadata.name, "msub");
        assert_eq!(parsed[4].metadata.name, "madda");
        assert_eq!(parsed[5].metadata.name, "msuba");
        rejected(
            ".syntax new\n\
                  add b,ACC,a",
            "ACC can only",
        );
    }

    #[test]
    fn memory_operands_preserve_displacement_increment_and_alias_tags() {
        let program = fragment(
            "lq vf02, 4(vi01):array:other\n\
             lqi vf03,(ptr++)\n\
             sqd vf03,(--ptr)\n\
             ilw vi02,0(vi03)x\n",
        );
        let instructions = instructions(&program);
        assert_eq!(
            instructions[0].operands[1].register.as_deref(),
            Some("vi01")
        );
        assert_eq!(instructions[0].operands[1].immediate.as_deref(), Some("4"));
        assert_eq!(instructions[0].operands[1].tags, ["array", "other"]);
        assert_eq!(instructions[1].operands[1].register.as_deref(), Some("ptr"));
        assert_eq!(instructions[2].operands[1].register.as_deref(), Some("ptr"));
        assert_eq!(instructions[3].mask, Mask::X);
        rejected("lqi vf03,(ptr)", "postincrement");
        rejected("ilw vi02,0(vi03)", "mask is required");
    }

    #[test]
    fn memory_operands_follow_the_original_length_and_shape_rules() {
        let program = fragment(
            "ilw.x vi02,10(p)\n\
             ilw.x vi02,0(p)x\n\
             ilw.x vi02,(pqr)\n\
             ilwr.x vi02,(pq)\n\
             lq vf01,(p)\n",
        );
        let parsed = instructions(&program);
        assert_eq!(parsed[0].operands[1].immediate.as_deref(), Some("10"));
        assert_eq!(parsed[2].operands[1].register.as_deref(), Some("pqr"));
        assert_eq!(parsed[2].operands[1].immediate.as_deref(), Some("0"));
        assert_eq!(parsed[3].operands[1].register.as_deref(), Some("pq"));
        assert_eq!(parsed[4].operands[1].immediate.as_deref(), Some("0"));
        for source in [
            "ilw.x vi02,0(p)",
            "ilw.x vi02,0(a)",
            "ilw.x vi02,(pq)",
            "ilw.x vi02,0(p):tag",
            "ilw.x vi02,0(p)X",
            "ilwr.x vi02,(p)",
            "ilwr.x vi02,0(pq)",
            "lq vf01,0(p)x",
            "lq vf01,()",
            "lqi vf01,x(p++)",
            "lqd vf01,x(--p)",
        ] {
            rejected(source, "failed to decode memory operand");
        }
    }

    #[test]
    fn parenthesized_displacements_keep_the_final_register_pair() {
        let program = fragment(
            "lq vf02,(0x10+2)(vi01)\n\
                                ilw.x vi02,(4*(2+3))(vi03)x\n",
        );
        let parsed = instructions(&program);
        assert_eq!(parsed[0].operands[1].register.as_deref(), Some("vi01"));
        assert_eq!(parsed[0].operands[1].immediate.as_deref(), Some("(0x10+2)"));
        assert_eq!(parsed[1].operands[1].register.as_deref(), Some("vi03"));
        assert_eq!(
            parsed[1].operands[1].immediate.as_deref(),
            Some("(4*(2+3))")
        );
        assert_eq!(parsed[1].mask, Mask::X);
    }

    #[test]
    fn register_word_memory_forms_apply_original_zero_displacement_hook() {
        let program = fragment(
            "ilwr.x vi04,(vi03)x\n\
                                iswr.x vi01,(vi03)x\n",
        );
        let parsed = instructions(&program);
        assert_eq!(parsed[0].metadata.name, "ilw");
        assert_eq!(parsed[1].metadata.name, "isw");
        for instruction in parsed {
            assert_eq!(
                instruction.operands[1].class,
                class::ADDRESS_REGISTER_OFFSET
            );
            assert_eq!(instruction.operands[1].immediate.as_deref(), Some("0"));
            assert_eq!(instruction.operands[1].register.as_deref(), Some("vi03"));
        }
    }

    #[test]
    fn indirect_branch_target_tags_are_retained() {
        let program = fragment("jr vi01:target0:target1\n");
        assert_eq!(
            instructions(&program)[0].operands[0].tags,
            ["target0", "target1"]
        );
    }

    #[test]
    fn implicit_operands_retain_original_classes_and_register_names() {
        let program = fragment("madd b,a,a\n");
        let instruction = instructions(&program)[0];
        assert!(
            instruction
                .operands
                .iter()
                .any(|operand| operand.implicit && operand.register.as_deref() == Some("ACC"))
        );
        assert!(
            instruction
                .operands
                .iter()
                .any(|operand| operand.implicit && operand.register.as_deref() == Some("MAC"))
        );
        assert!(instruction.operands.iter().any(|operand| operand.implicit
            && operand.class == class::STATUS_FROM_ALU
            && operand.register.is_none()));
    }

    #[test]
    fn raw_modes_require_two_issue_slots_and_accept_aliases() {
        let program = fragment(
            ".vsm reorder\n\
                                add vf02,vf01,vf01 nop\n\
                                .endraw\n",
        );
        let instructions = instructions(&program);
        assert_eq!(instructions.len(), 2);
        assert!(
            instructions
                .iter()
                .all(|instruction| instruction.mode == ParseMode::Raw && instruction.raw_reorder)
        );
        rejected(
            ".raw\n\
                  nop\n\
                  .endraw",
            "requires two",
        );
        rejected("add b,a,a add c,a,a", "more than one");
        rejected(
            ".raw\n\
                  nop | nop\n\
                  .endraw",
            "unknown instruction",
        );
    }

    #[test]
    fn raw_loop_restores_prior_mode() {
        let program = fragment(
            ".vsmloop\n\
                                nop nop\n\
                                .endrawloop\n\
                                add b,a,a\n",
        );
        let instructions = instructions(&program);
        assert_eq!(instructions[0].loop_id, 0);
        assert_eq!(instructions[2].mode, ParseMode::Vcl);
        rejected(
            ".rawloop\n\
                  .rawloop",
            "nested",
        );
        rejected(".endrawloop", "outside");
    }

    #[test]
    fn endbit_is_rejected_in_source_even_in_raw_mode() {
        rejected(
            ".raw\n\
                  nop[e] nop\n\
                  .endraw",
            "E/M/D/T",
        );
        rejected("add[m] b,a,a", "E/M/D/T");
        let program = fragment("add[dt] b,a,a\n");
        assert_eq!(instructions(&program)[0].modifiers, 12);
    }

    #[test]
    fn register_pool_directives_preserve_ranges_and_reject_invalid_bounds() {
        let program = fragment(
            ".init_vf VF03-vf31,vf02\n\
                                .rem_vi vi02\n\
                                .init_vf_all\n",
        );
        assert_eq!(
            program.statements[0].kind,
            StatementKind::Directive(Directive::InitRegisters {
                vector: true,
                enable: true,
                names: vec!["VF03-vf31".into(), "vf02".into()]
            })
        );
        rejected(".init_vf 3", "physical register");
        rejected(".init_vi vi00", "not allocable");
        rejected(".init_vi vi16", "physical register");
        // Reversed ranges are accepted by the original, which swaps bounds.
        fragment(".init_vf vf31-vf03");
    }

    #[test]
    fn work_areas_use_decimal_strtol_and_device_capacity() {
        let program = fragment(
            ".vu_work_area vu1,12,32\n\
                                .vu_work_area vu0,0x20,1\n",
        );
        assert_eq!(
            program.statements[1].kind,
            StatementKind::Directive(Directive::WorkArea {
                device: "vu0".into(),
                address: 0,
                size: 1
            })
        );
        rejected(".vu_work_area vu0,255,2", "outside");
        rejected(".vu_work_area core,0,1", "not implemented");
    }

    #[test]
    fn only_binary_directive_spellings_are_accepted() {
        rejected(".name_vf temp", "unknown directive");
        rejected(".vcl", "unknown directive");
        rejected(".syntax modern", "expects");
        rejected(".raw reordered", "bad option");
        fragment(
            ".NaMe FOO\n\
                  .global FOO\n\
                  .noebits\n\
                  .noaligns\n\
                  .align 8\n",
        );
    }

    #[test]
    fn visibility_scopes_check_closing_mode() {
        fragment(
            ".show a,b\n\
                  .hide c\n\
                  .endhide\n\
                  .endshow\n",
        );
        rejected(
            ".hide a\n\
                  .endshow",
            "scope mode mismatch",
        );
        // Closing an absent scope is only a warning in the binary.
        fragment(".endshow");
    }

    #[test]
    fn passthrough_blocks_bypass_instruction_parsing() {
        let program = fragment(
            "----*\n\
                                .text\n\
                                .globl example\n\
                                *----\n\
                                add b,a,a\n",
        );
        assert_eq!(
            program.statements[0].kind,
            StatementKind::RawText(".text".into())
        );
        assert_eq!(instructions(&program).len(), 1);
    }

    #[test]
    fn malformed_operand_lists_do_not_panic() {
        rejected(",add b,a,a", ParseMessage::UnexpectedComma);
        rejected("add b,,a,a", ParseMessage::UnexpectedComma);
        rejected("add b,a", "missing operand");
        rejected("in_vf x(no_such_register)", "not physical");
        rejected(
            "add toolong-name,a,a",
            "can't use '-' in the name of a register",
        );
        rejected("add mul,a,a", "reserved");
    }

    #[test]
    fn loop_prefix_and_optional_marker_are_retained() {
        let program = fragment(
            "~2..add b,a,a\n\
                                +..mul c,b,a\n",
        );
        let instructions = instructions(&program);
        assert!(instructions[0].optional);
        assert_eq!(instructions[0].loop_id, 2);
        assert_eq!(instructions[1].loop_id, 0x1024);
        rejected("33..add b,a,a", "larger than 32");
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn parses_every_saved_successful_original_program() {
        let directory = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle");
        let mut checked = 0;
        let mut failures = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let source = path.join("input.vcl");
            let result = path.join("result.json");
            if !source.is_file() || !result.is_file() {
                continue;
            }
            let result = std::fs::read_to_string(result).unwrap();
            if !result.contains("\"exit_code\": 0,") {
                continue;
            }
            let options = ParserOptions {
                syntax: if result.contains("\"-n\"") {
                    Syntax::New
                } else {
                    Syntax::Old
                },
                ..ParserOptions::default()
            };
            checked += 1;
            if let Err(errors) =
                parse_with_options(&std::fs::read_to_string(source).unwrap(), options)
            {
                failures.push(format!(
                    "{}: {errors:?}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
        assert!(checked >= 30, "expected retained original-program corpus");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
