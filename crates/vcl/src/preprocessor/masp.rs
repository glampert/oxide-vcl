//! MASP mode's first stage: the macro language of masp and GASP, the
//! assembler preprocessors that classic VU code, like ps2gl's, was written
//! for.
//!
//! The reader carries out `.macro`, `.aif`, `.arepeat`, `.awhile`,
//! `.assigna`/`.assignc`, `.equ`, `.include` and `.end`. It yields the lines
//! that result to the engine, which runs the C directives over them, as cpp
//! would run over masp's output. Lines that start with `#` are C directives,
//! passed on untouched.
//!
//! As in masp, macros expand into text, a line at a time: `\param` is
//! replaced wherever it appears, even inside a word (`temp\@`), and the
//! expanded lines are read again as input, so they can hold directives and
//! invoke other macros.

use std::collections::HashMap;
use std::rc::Rc;

use super::engine::{Engine, Fallible};
use super::lexer::{Loc, Origin, Punct, Token, TokenKind};
use crate::diagnostics::{Note, PreprocessMessage};

/// Deep enough for any sensible nesting, shallow enough to stop a macro that
/// invokes itself, or a file that includes itself, long before the stack runs
/// out.
const MAX_INCLUDE_DEPTH: usize = 200;
const MAX_EXPANSION_DEPTH: usize = 200;

/// More passes than a loop in VU code needs; an `.awhile` whose condition
/// never turns false stops here.
const MAX_LOOP_PASSES: u64 = 100_000;

/// The GASP directives that masp carries out itself (it turns `.sdata` into
/// `.byte`, for one) and the preprocessor does not. Passed on as they are,
/// they would mean something else to VCL.
const REFUSED_DIRECTIVES: [&str; 20] = [
    "alternate",
    "assign",
    "data",
    "datab",
    "export",
    "form",
    "heading",
    "org",
    "page",
    "print",
    "program",
    "radix",
    "res",
    "sdata",
    "sdatab",
    "sdatac",
    "sdataz",
    "sres",
    "sresc",
    "sresz",
];

/// A line of source, with the lines continuing it (`+` in column 1) joined
/// to it and its comments removed.
#[derive(Clone, Debug)]
struct TextLine {
    text: String,
    /// Of its first line.
    loc: Loc,
}

struct MaspMacro {
    /// As written in its `.macro`.
    name: String,
    /// Each parameter's name with its default value.
    params: Vec<(String, String)>,
    body: Vec<TextLine>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FrameKind {
    File,
    /// A macro expansion.
    Macro,
    /// An `.arepeat` or `.awhile` body, replayed.
    Loop,
}

/// Where lines are read from.
struct Frame {
    kind: FrameKind,
    lines: Vec<TextLine>,
    next: usize,
    /// The number of open conditionals on entry.
    conditional_depth: usize,
    /// The expansion its lines come out of, if any.
    origin: Option<Rc<Origin>>,
    /// Macro: each parameter with its value.
    arguments: Vec<(String, String)>,
    /// Macro: the value of `\@`.
    counter: String,
    /// Loop: `.arepeat` passes after this one.
    repeats_left: u64,
    /// Loop: `.awhile`'s condition, evaluated before each pass.
    while_condition: Option<TextLine>,
    passes: u64,
}

impl Frame {
    fn new(kind: FrameKind, lines: Vec<TextLine>, conditional_depth: usize) -> Self {
        Self {
            kind,
            lines,
            next: 0,
            conditional_depth,
            origin: None,
            arguments: Vec::new(),
            counter: String::new(),
            repeats_left: 0,
            while_condition: None,
            passes: 0,
        }
    }
}

/// An `.aif` and its `.aelse`.
struct Conditional {
    opened_at: Loc,
    parent_active: bool,
    branch_taken: bool,
    active: bool,
    seen_else: bool,
}

/// A line taken apart: `label: word operand` or `label: .directive operand`,
/// or `SYMBOL .equ operand` for the directives that name a symbol in column 1.
#[derive(Default)]
struct Statement {
    /// A name in column 1, with its colon, which masp lets it go without.
    label: String,
    /// Where the label ends in the line, its colon included.
    label_end: usize,
    /// The column-1 name of `.equ`, `.assigna` and `.assignc`.
    symbol: String,
    /// A `.` word, lower-cased, without the `.`.
    directive: String,
    /// Otherwise the first word, which may name a macro.
    word: String,
    /// The rest of the line, after the directive or the word.
    operand: String,
}

/// The MASP stage's state.
#[derive(Default)]
pub(super) struct Masp {
    main_file: usize,
    frames: Vec<Frame>,
    conditionals: Vec<Conditional>,
    /// By lower-cased name: names ignore case.
    macros: HashMap<String, MaspMacro>,
    /// `.assigna`/`.assignc`, read as `\&NAME`.
    variables: HashMap<String, String>,
    /// `.equ`, by name.
    equates: HashMap<String, String>,
    put_back: Vec<Vec<Token>>,
    /// `\@` of the next expansion.
    expansions: u64,
    /// Set the next line off by a blank one.
    blank_pending: bool,
    /// `.end` was reached.
    ended: bool,
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_name_char(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit()
}

/// The length of the name that starts at `position`, or 0 if none does.
fn name_length(text: &str, position: usize) -> usize {
    let bytes = text.as_bytes();
    if position >= bytes.len() || !is_name_start(bytes[position]) {
        return 0;
    }
    let mut end = position + 1;
    while end < bytes.len() && is_name_char(bytes[end]) {
        end += 1;
    }
    end - position
}

fn trim(text: &str) -> &str {
    text.trim_matches([' ', '\t'])
}

/// Splits text at its commas, except those inside quotes, and trims each
/// part. Blank text has no parts; `a,` has two, the second empty.
fn split_at_commas(text: &str) -> Vec<String> {
    if trim(text).is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut in_string = false;
    let mut start = 0;
    for (index, c) in text.char_indices() {
        if c == ',' && !in_string {
            parts.push(trim(&text[start..index]).to_string());
            start = index + 1;
        } else if c == '"' {
            in_string = !in_string;
        }
    }
    parts.push(trim(&text[start..]).to_string());
    parts
}

/// The comparisons of `.aif` and `.awhile`.
fn compare<T: PartialOrd>(left: &T, op: &str, right: &T) -> bool {
    match op {
        "eq" => left == right,
        "ne" => left != right,
        "lt" => left < right,
        "le" => left <= right,
        "gt" => left > right,
        _ => left >= right,
    }
}

fn comparison_named(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    ["eq", "ne", "lt", "le", "gt", "ge"]
        .contains(&lower.as_str())
        .then_some(lower)
}

fn parse_statement(text: &str) -> Statement {
    let mut statement = Statement::default();
    let bytes = text.as_bytes();
    let mut position = 0;
    // A name in column 1 is the name an .equ, .assigna, .assignc or .macro
    // after it gives a value to, or else a label, which masp lets go without
    // its colon.
    let length = name_length(text, 0);
    if length != 0 {
        let colon = bytes.get(length) == Some(&b':');
        if !colon {
            let dot = text[length..]
                .find(|c| c != ' ' && c != '\t')
                .map(|offset| length + offset);
            let directive_length = match dot {
                Some(dot) if dot != length && bytes[dot] == b'.' => name_length(text, dot + 1),
                _ => 0,
            };
            if directive_length != 0 {
                let dot = dot.unwrap();
                let directive = text[dot + 1..dot + 1 + directive_length].to_ascii_lowercase();
                if matches!(directive.as_str(), "equ" | "assigna" | "assignc") {
                    statement.symbol = text[..length].to_string();
                    statement.operand = text[dot + 1 + directive_length..].to_string();
                    statement.directive = directive;
                    return statement;
                }
                if directive == "macro" {
                    // `name .macro parameters` names the macro as
                    // `.macro name parameters` does.
                    statement.operand = format!(
                        " {}{}",
                        &text[..length],
                        &text[dot + 1 + directive_length..]
                    );
                    statement.directive = directive;
                    return statement;
                }
            }
        }
        statement.label = format!("{}:", &text[..length]);
        statement.label_end = length + usize::from(colon);
        position = statement.label_end;
    }
    let Some(start) = text[position..]
        .find(|c| c != ' ' && c != '\t')
        .map(|offset| position + offset)
    else {
        return statement;
    };
    if bytes[start] == b'.' {
        let length = name_length(text, start + 1);
        if length != 0 {
            statement.directive = text[start + 1..start + 1 + length].to_ascii_lowercase();
            statement.operand = text[start + 1 + length..].to_string();
        }
    } else {
        let length = name_length(text, start);
        if length != 0 {
            statement.word = text[start..start + length].to_string();
            statement.operand = text[start + length..].to_string();
        }
    }
    statement
}

/// The line with its label given a colon, if it was written without, and
/// some space after it.
fn with_label(statement: &Statement, text: &str) -> String {
    if statement.label.is_empty() {
        return text.to_string();
    }
    let rest = &text[statement.label_end..];
    let spaced = rest.is_empty() || rest.starts_with([' ', '\t']);
    if spaced {
        format!("{}{rest}", statement.label)
    } else {
        format!("{}\t{rest}", statement.label)
    }
}

/// A macro line with `\@` and the parameters replaced by their values.
fn substitute_arguments(text: &str, expansion: &Frame) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'@' {
                out.push_str(&expansion.counter);
                index += 2;
                continue;
            }
            // The longest name after the backslash, if it is a parameter's;
            // masp leaves any other name as it is.
            let length = name_length(text, index + 1);
            if length != 0 {
                let name = &text[index + 1..index + 1 + length];
                if let Some((_, value)) =
                    expansion.arguments.iter().find(|(param, _)| param == name)
                {
                    out.push_str(value);
                    index += 1 + length;
                    continue;
                }
            }
        }
        let width = text[index..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[index..index + width]);
        index += width;
    }
    out
}

impl Engine<'_> {
    /// Starts the MASP stage on the main file.
    pub(super) fn masp_start(&mut self, main_file: usize) {
        self.masp.main_file = main_file;
        for (name, value) in &self.options.masp_variables {
            self.masp.variables.insert(name.clone(), value.clone());
        }
        let lines = self.masp_split_lines(main_file);
        self.masp.frames.push(Frame::new(FrameKind::File, lines, 0));
    }

    /// The next line of the program, as tokens, or `None` at its end.
    pub(super) fn masp_next_line(&mut self) -> Fallible<Option<Vec<Token>>> {
        if let Some(line) = self.masp.put_back.pop() {
            return Ok(Some(line));
        }
        while let Some(raw) = self.masp_read_line()? {
            if let Some(line) = self.masp_process_line(&raw)? {
                return Ok(Some(line));
            }
        }
        self.masp.frames.clear();
        Ok(None)
    }

    /// Hands a line back, for the next call to return again.
    pub(super) fn masp_unget_line(&mut self, line: Vec<Token>) {
        self.masp.put_back.push(line);
    }

    // ----------------------------------------------
    // Reading
    // ----------------------------------------------

    fn masp_split_lines(&self, file: usize) -> Vec<TextLine> {
        let text = &self.sources.file(file).text;
        // A UTF-8 byte order mark is not part of the text.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut lines: Vec<TextLine> = Vec::new();
        let mut in_block_comment = false;
        for (number, physical) in text.split_terminator('\n').enumerate() {
            let physical = physical.strip_suffix('\r').unwrap_or(physical);
            // A `+` in column 1 continues the line before.
            let continues = !in_block_comment && physical.starts_with('+') && !lines.is_empty();
            let code = self.masp_strip_comments(physical, &mut in_block_comment);
            if continues {
                lines
                    .last_mut()
                    .unwrap()
                    .text
                    .push_str(code.get(1..).unwrap_or(""));
            } else {
                lines.push(TextLine {
                    text: code,
                    loc: Loc {
                        file,
                        line: number + 1,
                        column: 1,
                    },
                });
            }
        }
        lines
    }

    fn masp_strip_comments(&self, line: &str, in_block_comment: &mut bool) -> String {
        let bytes = line.as_bytes();
        let mut code = String::new();
        let mut in_string = false;
        let mut index = 0;
        while index < bytes.len() {
            let c = bytes[index];
            let next = bytes.get(index + 1).copied().unwrap_or(0);
            let width = line[index..].chars().next().map_or(1, char::len_utf8);
            if *in_block_comment {
                if c == b'*' && next == b'/' {
                    *in_block_comment = false;
                    index += 1;
                }
                index += width;
                continue;
            }
            if in_string {
                code.push_str(&line[index..index + width]);
                in_string = c != b'"';
                index += width;
                continue;
            }
            if c == b'"' {
                in_string = true;
            } else if c == b';'
                || self.options.comment_chars.as_bytes().contains(&c)
                || (c == b'/' && next == b'/')
            {
                break;
            } else if c == b'/' && next == b'*' {
                // As in C, a comment separates what is on either side of it.
                *in_block_comment = true;
                code.push(' ');
                index += 2;
                continue;
            }
            code.push_str(&line[index..index + width]);
            index += width;
        }
        // The whitespace before a comment goes with it.
        code.truncate(code.trim_end_matches([' ', '\t']).len());
        code
    }

    fn masp_read_line(&mut self) -> Fallible<Option<TextLine>> {
        while !self.masp.ended && !self.masp.frames.is_empty() {
            let top = self.masp.frames.last_mut().unwrap();
            if top.next < top.lines.len() {
                let raw = top.lines[top.next].clone();
                top.next += 1;
                let text = if top.kind == FrameKind::Macro {
                    substitute_arguments(&raw.text, top)
                } else {
                    raw.text
                };
                return Ok(Some(TextLine { text, loc: raw.loc }));
            }
            self.masp_end_frame()?;
        }
        Ok(None)
    }

    fn masp_end_frame(&mut self) -> Fallible<()> {
        let top = self.masp.frames.last().unwrap();
        if let Some(open) = self.masp.conditionals.get(top.conditional_depth) {
            let at = open.opened_at;
            return Err(self.masp_error_at(at, PreprocessMessage::MaspUnterminatedAif));
        }
        // A loop goes round again while it has passes left.
        if top.kind == FrameKind::Loop {
            if top.repeats_left > 0 {
                let top = self.masp.frames.last_mut().unwrap();
                top.repeats_left -= 1;
                top.next = 0;
                return Ok(());
            }
            if let Some(condition) = top.while_condition.clone() {
                let top = self.masp.frames.last_mut().unwrap();
                top.passes += 1;
                if top.passes > MAX_LOOP_PASSES {
                    return Err(self.masp_error(
                        &condition,
                        PreprocessMessage::MaspEndlessWhile {
                            passes: MAX_LOOP_PASSES,
                        },
                    ));
                }
                let text = self.masp_substitute_symbols(&condition.text, &condition)?;
                if self.masp_evaluate_condition(&text, &condition, ".awhile")? {
                    self.masp.frames.last_mut().unwrap().next = 0;
                    return Ok(());
                }
            }
        }
        self.masp_pop_frame();
        Ok(())
    }

    fn masp_pop_frame(&mut self) {
        let top = self.masp.frames.pop().unwrap();
        let top_level_expansion = top.kind == FrameKind::Macro
            && top
                .origin
                .as_ref()
                .is_some_and(|origin| origin.parent.is_none());
        let main_file = top.kind == FrameKind::File && self.masp.frames.is_empty();
        // An expansion written in the source is set off by blank lines.
        if top_level_expansion {
            self.masp.blank_pending = true;
        }
        if main_file && !self.masp.ended {
            let at = Loc {
                file: self.masp.main_file,
                line: 0,
                column: 0,
            };
            self.warn_at(at, PreprocessMessage::MaspMissingEnd, Vec::new());
        }
    }

    /// The lines up to the directive that closes `open`, read from where the
    /// opening directive is, which has to end there.
    fn masp_capture_body(
        &mut self,
        open: &str,
        close: &str,
        opener: &TextLine,
    ) -> Fallible<Vec<TextLine>> {
        let mut body = Vec::new();
        let mut depth = 1;
        loop {
            let source = self.masp.frames.last_mut().unwrap();
            let Some(line) = source.lines.get(source.next).cloned() else {
                return Err(self.masp_error(
                    opener,
                    PreprocessMessage::MaspUnterminatedBlock {
                        open: open.into(),
                        close: close.into(),
                    },
                ));
            };
            source.next += 1;
            let mut line = line;
            if source.kind == FrameKind::Macro {
                line.text = substitute_arguments(&line.text, source);
            }
            let directive = parse_statement(&line.text).directive;
            if directive == open {
                depth += 1;
            } else if directive == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(body);
                }
            }
            body.push(line);
        }
    }

    // ----------------------------------------------
    // Lines
    // ----------------------------------------------

    /// Carries out one line, returning the line of code it leaves, if any.
    fn masp_process_line(&mut self, raw: &TextLine) -> Fallible<Option<Vec<Token>>> {
        let Some(first) = raw.text.find(|c| c != ' ' && c != '\t') else {
            return Ok(None);
        };
        let statement = parse_statement(&raw.text);
        // In code being skipped, only the nesting of .aif matters.
        if !self.masp_is_active() {
            match statement.directive.as_str() {
                "aif" => self.masp.conditionals.push(Conditional {
                    opened_at: raw.loc,
                    parent_active: false,
                    branch_taken: true,
                    active: false,
                    seen_else: false,
                }),
                "aelse" => self.masp_else_conditional(raw)?,
                "aendi" => self.masp_close_conditional(raw)?,
                _ => {}
            }
            return Ok(None);
        }
        // C directives are the engine's.
        if raw.text[first..].starts_with('#') {
            return self.masp_make_line(&raw.text, raw);
        }
        if !statement.symbol.is_empty() {
            self.masp_assign_symbol(&statement, raw)?;
            return Ok(None);
        }
        if !statement.directive.is_empty() && self.masp_carry_out_directive(&statement, raw)? {
            // A label on the line stays, on a line of its own; masp drops it.
            if statement.label.is_empty() {
                return Ok(None);
            }
            return self.masp_make_line(&statement.label, raw);
        }
        // Code, or a macro invocation. Variables and .equ symbols are
        // replaced first, so that they can name the macro too. A macro's name
        // is followed by a space or nothing: `maxx.x` is an instruction even
        // with a macro called maxx.
        let text = self.masp_substitute_symbols(&raw.text, raw)?;
        let code = parse_statement(&text);
        let invocation = !code.word.is_empty()
            && (code.operand.is_empty() || code.operand.starts_with([' ', '\t']));
        if invocation
            && self
                .masp
                .macros
                .contains_key(&code.word.to_ascii_lowercase())
        {
            // The expansion is set off by blank lines, a label on its line
            // included. The label stays, on a line of its own; masp drops it.
            let top_level = self.masp_origin().is_none();
            self.masp.blank_pending = self.masp.blank_pending || top_level;
            let labelled = if code.label.is_empty() {
                None
            } else {
                self.masp_make_line(&code.label, raw)?
            };
            self.masp_invoke_macro(&code.word.to_ascii_lowercase(), &code.operand, raw)?;
            if labelled.is_some() {
                self.masp.blank_pending = false;
            }
            return Ok(labelled);
        }
        self.masp_make_line(&with_label(&code, &text), raw)
    }

    /// Carries out masp's directives; false for any other, which passes on
    /// as code (VCL's `.name`, for one).
    fn masp_carry_out_directive(
        &mut self,
        statement: &Statement,
        raw: &TextLine,
    ) -> Fallible<bool> {
        let name = statement.directive.as_str();
        match name {
            "macro" => self.masp_define_macro(statement, raw)?,
            "exitm" => self.masp_exit_macro(raw)?,
            "aif" => self.masp_open_conditional(statement, raw)?,
            "aelse" => self.masp_else_conditional(raw)?,
            "aendi" => self.masp_close_conditional(raw)?,
            "arepeat" => self.masp_repeat(statement, raw)?,
            "awhile" => self.masp_loop_while(statement, raw)?,
            "include" => self.masp_include_file(statement, raw)?,
            "end" => self.masp.ended = true,
            "endm" | "aendr" | "aendw" => {
                let open = match name {
                    "endm" => "macro",
                    "aendr" => "arepeat",
                    _ => "awhile",
                };
                return Err(self.masp_error(
                    raw,
                    PreprocessMessage::MaspCloseWithoutOpen {
                        close: name.into(),
                        open: open.into(),
                    },
                ));
            }
            "assigna" | "assignc" => {
                return Err(self.masp_error(
                    raw,
                    PreprocessMessage::MaspAssignNeedsName {
                        directive: name.into(),
                    },
                ));
            }
            _ if REFUSED_DIRECTIVES.contains(&name) => {
                return Err(self.masp_error(
                    raw,
                    PreprocessMessage::MaspRefusedDirective { name: name.into() },
                ));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn masp_assign_symbol(&mut self, statement: &Statement, raw: &TextLine) -> Fallible<()> {
        let operand = self.masp_substitute_symbols(&statement.operand, raw)?;
        if statement.directive == "assignc" {
            // The text, without the quotes around it.
            let value = trim(&operand);
            let value = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                &value[1..value.len() - 1]
            } else {
                value
            };
            self.masp
                .variables
                .insert(statement.symbol.clone(), value.to_string());
            return Ok(());
        }
        let value = self
            .masp_evaluate(&operand, raw, &format!(".{}", statement.directive))?
            .to_string();
        if statement.directive == "equ" {
            self.masp.equates.insert(statement.symbol.clone(), value);
        } else {
            self.masp.variables.insert(statement.symbol.clone(), value);
        }
        Ok(())
    }

    fn masp_make_line(&mut self, text: &str, raw: &TextLine) -> Fallible<Option<Vec<Token>>> {
        let mut out = self.masp_tokenize(text, raw)?;
        if out.is_empty() {
            return Ok(None);
        }
        // The blank line goes before code, not before a directive, which
        // leaves no line.
        if self.masp.blank_pending && !out[0].is(Punct::Hash) {
            out[0].blank_before = true;
            self.masp.blank_pending = false;
        }
        Ok(Some(out))
    }

    // ----------------------------------------------
    // Directives
    // ----------------------------------------------

    fn masp_define_macro(&mut self, statement: &Statement, raw: &TextLine) -> Fallible<()> {
        let operand = trim(&statement.operand);
        let length = name_length(operand, 0);
        if length == 0 {
            return Err(self.masp_error(raw, PreprocessMessage::MaspExpectedMacroName));
        }
        let name = operand[..length].to_string();
        let mut params: Vec<(String, String)> = Vec::new();
        for param in split_at_commas(&operand[length..]) {
            let (param_name, default) = match param.find('=') {
                Some(equals) => (trim(&param[..equals]), trim(&param[equals + 1..])),
                None => (trim(&param), ""),
            };
            if param_name.is_empty() || name_length(param_name, 0) != param_name.len() {
                let message = if param.is_empty() {
                    PreprocessMessage::MaspMissingParameterName { name }
                } else {
                    PreprocessMessage::MaspNotAParameterName {
                        parameter: param.clone(),
                        name,
                    }
                };
                return Err(self.masp_error(raw, message));
            }
            if params.iter().any(|(existing, _)| existing == param_name) {
                return Err(self.masp_error(
                    raw,
                    PreprocessMessage::DuplicateParameter {
                        parameter: param_name.into(),
                        name,
                    },
                ));
            }
            params.push((param_name.to_string(), default.to_string()));
        }
        let body = self.masp_capture_body("macro", "endm", raw)?;
        // As in masp, a macro defined again replaces the one before.
        self.masp
            .macros
            .insert(name.to_ascii_lowercase(), MaspMacro { name, params, body });
        Ok(())
    }

    fn masp_invoke_macro(&mut self, key: &str, arguments: &str, raw: &TextLine) -> Fallible<()> {
        let definition = &self.masp.macros[key];
        let name = definition.name.clone();
        let depth = self
            .masp
            .frames
            .iter()
            .filter(|frame| frame.kind == FrameKind::Macro)
            .count();
        if depth >= MAX_EXPANSION_DEPTH {
            return Err(self.masp_error(
                raw,
                PreprocessMessage::MaspExpansionTooDeep {
                    depth: MAX_EXPANSION_DEPTH,
                    name,
                },
            ));
        }
        // The parameters' defaults, until the arguments are in.
        let mut values = definition.params.clone();
        let lines = definition.body.clone();
        let mut positional = 0;
        let mut by_name = false;
        for argument in split_at_commas(arguments) {
            // `name=value` gives a parameter by name. As in masp, the
            // arguments after one have to as well.
            let length = name_length(&argument, 0);
            let equals = (length != 0)
                .then(|| argument[length..].find(|c| c != ' ' && c != '\t'))
                .flatten()
                .map(|offset| length + offset);
            if let Some(equals) = equals.filter(|&at| argument.as_bytes()[at] == b'=') {
                let parameter = &argument[..length];
                let Some(slot) = values.iter_mut().find(|(param, _)| param == parameter) else {
                    return Err(self.masp_error(
                        raw,
                        PreprocessMessage::MaspNoSuchParameter {
                            name,
                            parameter: parameter.into(),
                        },
                    ));
                };
                slot.1 = trim(&argument[equals + 1..]).to_string();
                by_name = true;
                continue;
            }
            if by_name {
                return Err(self.masp_error(
                    raw,
                    PreprocessMessage::MaspPositionalAfterNamed { argument, name },
                ));
            }
            if positional >= values.len() {
                let takes = values.len();
                return Err(
                    self.masp_error(raw, PreprocessMessage::MaspTooManyArguments { name, takes })
                );
            }
            values[positional].1 = argument;
            positional += 1;
        }
        let mut expansion = Frame::new(FrameKind::Macro, lines, self.masp.conditionals.len());
        expansion.arguments = values;
        expansion.counter = self.masp.expansions.to_string();
        self.masp.expansions += 1;
        expansion.origin = Some(Rc::new(Origin {
            macro_name: name,
            invoked_at: raw.loc,
            parent: self.masp_origin(),
        }));
        self.masp.frames.push(expansion);
        Ok(())
    }

    fn masp_exit_macro(&mut self, raw: &TextLine) -> Fallible<()> {
        let Some(position) = self
            .masp
            .frames
            .iter()
            .rposition(|frame| frame.kind == FrameKind::Macro)
        else {
            return Err(self.masp_error(raw, PreprocessMessage::MaspExitmOutsideMacro));
        };
        // The expansion ends, with any loops and .aifs inside it.
        let depth = self.masp.frames[position].conditional_depth;
        self.masp.conditionals.truncate(depth);
        while self.masp.frames.len() > position {
            self.masp_pop_frame();
        }
        Ok(())
    }

    fn masp_open_conditional(&mut self, statement: &Statement, raw: &TextLine) -> Fallible<()> {
        let text = self.masp_substitute_symbols(&statement.operand, raw)?;
        let active = self.masp_evaluate_condition(&text, raw, ".aif")?;
        self.masp.conditionals.push(Conditional {
            opened_at: raw.loc,
            parent_active: true,
            branch_taken: active,
            active,
            seen_else: false,
        });
        Ok(())
    }

    fn masp_frame_conditional_depth(&self) -> usize {
        self.masp
            .frames
            .last()
            .map_or(0, |frame| frame.conditional_depth)
    }

    fn masp_else_conditional(&mut self, raw: &TextLine) -> Fallible<()> {
        if self.masp.conditionals.len() <= self.masp_frame_conditional_depth() {
            return Err(self.masp_error(raw, PreprocessMessage::MaspAelseWithoutAif));
        }
        if self.masp.conditionals.last().unwrap().seen_else {
            return Err(self.masp_error(raw, PreprocessMessage::MaspAelseAfterAelse));
        }
        let open = self.masp.conditionals.last_mut().unwrap();
        open.seen_else = true;
        open.active = open.parent_active && !open.branch_taken;
        open.branch_taken = open.branch_taken || open.active;
        Ok(())
    }

    fn masp_close_conditional(&mut self, raw: &TextLine) -> Fallible<()> {
        if self.masp.conditionals.len() <= self.masp_frame_conditional_depth() {
            return Err(self.masp_error(raw, PreprocessMessage::MaspAendiWithoutAif));
        }
        self.masp.conditionals.pop();
        Ok(())
    }

    fn masp_repeat(&mut self, statement: &Statement, raw: &TextLine) -> Fallible<()> {
        let text = self.masp_substitute_symbols(&statement.operand, raw)?;
        let count = self.masp_evaluate(&text, raw, ".arepeat")?;
        if count > MAX_LOOP_PASSES as i64 {
            return Err(self.masp_error(
                raw,
                PreprocessMessage::MaspRepeatTooMany {
                    count,
                    max: MAX_LOOP_PASSES,
                },
            ));
        }
        let body = self.masp_capture_body("arepeat", "aendr", raw)?;
        if count <= 0 || body.is_empty() {
            return Ok(());
        }
        let mut repeat = Frame::new(FrameKind::Loop, body, self.masp.conditionals.len());
        repeat.repeats_left = count as u64 - 1;
        repeat.origin = self.masp_origin();
        self.masp.frames.push(repeat);
        Ok(())
    }

    fn masp_loop_while(&mut self, statement: &Statement, raw: &TextLine) -> Fallible<()> {
        // The condition is kept as written, to be evaluated again before each
        // pass.
        let condition = TextLine {
            text: statement.operand.clone(),
            loc: raw.loc,
        };
        let body = self.masp_capture_body("awhile", "aendw", raw)?;
        let text = self.masp_substitute_symbols(&condition.text, &condition)?;
        if !self.masp_evaluate_condition(&text, &condition, ".awhile")? {
            return Ok(());
        }
        let mut repeat = Frame::new(FrameKind::Loop, body, self.masp.conditionals.len());
        repeat.while_condition = Some(condition);
        repeat.passes = 1;
        repeat.origin = self.masp_origin();
        self.masp.frames.push(repeat);
        Ok(())
    }

    fn masp_include_file(&mut self, statement: &Statement, raw: &TextLine) -> Fallible<()> {
        let operand = self.masp_substitute_symbols(&statement.operand, raw)?;
        let operand = trim(&operand);
        if operand.len() < 2 || !operand.starts_with('"') || !operand.ends_with('"') {
            return Err(self.masp_error(raw, PreprocessMessage::MaspIncludeExpectsFile));
        }
        let name = &operand[1..operand.len() - 1];
        if name.is_empty() {
            return Err(self.masp_error(raw, PreprocessMessage::MaspIncludeEmptyName));
        }
        let files = self
            .masp
            .frames
            .iter()
            .filter(|frame| frame.kind == FrameKind::File)
            .count();
        if files >= MAX_INCLUDE_DEPTH {
            return Err(self.masp_error(
                raw,
                PreprocessMessage::MaspIncludeTooDeep {
                    depth: MAX_INCLUDE_DEPTH,
                },
            ));
        }
        let path = match self.sources.find_include(name, false, raw.loc.file) {
            Ok(path) => path,
            Err(tried) => {
                let notes = tried
                    .iter()
                    .map(|candidate| {
                        Note::new(
                            None,
                            PreprocessMessage::NotFoundAt {
                                path: candidate.to_string_lossy().into_owned(),
                            },
                        )
                    })
                    .collect();
                let at = self.masp_at(raw);
                return Err(self.error_with(
                    &at,
                    PreprocessMessage::IncludeNotFound { name: name.into() },
                    notes,
                ));
            }
        };
        let Some(file) = self.sources.load(&path, raw.loc) else {
            return Err(self.masp_error(
                raw,
                PreprocessMessage::IncludeUnreadable {
                    path: path.to_string_lossy().into_owned(),
                },
            ));
        };
        let lines = self.masp_split_lines(file);
        let frame = Frame::new(FrameKind::File, lines, self.masp.conditionals.len());
        self.masp.frames.push(frame);
        Ok(())
    }

    // ----------------------------------------------
    // Substitution and evaluation
    // ----------------------------------------------

    fn masp_substitute_symbols(&self, text: &str, raw: &TextLine) -> Fallible<String> {
        // \&NAME first, since a variable's value can hold .equ symbols.
        let bytes = text.as_bytes();
        let mut with_variables = String::with_capacity(text.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\\' && bytes.get(index + 1) == Some(&b'&') {
                let length = name_length(text, index + 2);
                if length != 0 {
                    let name = &text[index + 2..index + 2 + length];
                    let Some(value) = self.masp.variables.get(name) else {
                        return Err(self.masp_error(
                            raw,
                            PreprocessMessage::MaspUndefinedVariable { name: name.into() },
                        ));
                    };
                    with_variables.push_str(value);
                    index += 2 + length;
                    continue;
                }
            }
            let width = text[index..].chars().next().map_or(1, char::len_utf8);
            with_variables.push_str(&text[index..index + width]);
            index += width;
        }
        if self.masp.equates.is_empty() {
            return Ok(with_variables);
        }
        // Then each .equ symbol, as a whole word. As in masp, that includes
        // words in quotes.
        let text = with_variables;
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut index = 0;
        while index < bytes.len() {
            let c = bytes[index];
            // A number, with any letters in it (0x1F), is not a name.
            if c.is_ascii_digit() {
                let mut end = index + 1;
                while end < bytes.len() && is_name_char(bytes[end]) {
                    end += 1;
                }
                out.push_str(&text[index..end]);
                index = end;
                continue;
            }
            let length = name_length(&text, index);
            if length != 0 {
                let name = &text[index..index + length];
                // A name after a backslash is a parameter reference masp left
                // alone.
                let after_backslash = index > 0 && bytes[index - 1] == b'\\';
                match self.masp.equates.get(name) {
                    Some(value) if !after_backslash => out.push_str(value),
                    _ => out.push_str(name),
                }
                index += length;
                continue;
            }
            let width = text[index..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&text[index..index + width]);
            index += width;
        }
        Ok(out)
    }

    fn masp_evaluate(&mut self, text: &str, raw: &TextLine, context: &str) -> Fallible<i64> {
        let tokens = self.masp_tokenize(text, raw)?;
        let at = self.masp_at(raw);
        self.evaluate(&tokens, &at, context)
    }

    fn masp_evaluate_condition(
        &mut self,
        text: &str,
        raw: &TextLine,
        context: &str,
    ) -> Fallible<bool> {
        let tokens = self.masp_tokenize(text, raw)?;
        let at = self.masp_at(raw);
        // A comparison - EQ, NE, LT, LE, GT or GE - between two operands,
        // outside parentheses.
        let mut comparison = None;
        let mut depth = 0i32;
        for (index, token) in tokens.iter().enumerate() {
            if token.is(Punct::OpenParen) {
                depth += 1;
            } else if token.is(Punct::CloseParen) {
                depth -= 1;
            } else if depth == 0
                && token.is_identifier()
                && let Some(op) = comparison_named(&token.text)
            {
                comparison = Some((index, op));
                break;
            }
        }
        // Without one, the condition holds if its value is not zero.
        let Some((op_index, op)) = comparison else {
            return Ok(self.evaluate(&tokens, &at, context)? != 0);
        };
        let (left, right) = (&tokens[..op_index], &tokens[op_index + 1..]);
        // Two quoted strings compare as text.
        let is_string = |side: &[Token]| side.len() == 1 && side[0].kind == TokenKind::String;
        if is_string(left) && is_string(right) {
            let unquoted = |token: &Token| token.text[1..token.text.len() - 1].to_string();
            return Ok(compare(&unquoted(&left[0]), &op, &unquoted(&right[0])));
        }
        let has_string = |side: &[Token]| side.iter().any(|token| token.kind == TokenKind::String);
        if has_string(left) || has_string(right) {
            return Err(self.masp_error(
                raw,
                PreprocessMessage::MaspStringComparison {
                    context: context.into(),
                },
            ));
        }
        let left = self.evaluate(left, &at, context)?;
        let right = self.evaluate(right, &at, context)?;
        Ok(compare(&left, &op, &right))
    }

    // ----------------------------------------------
    // Helpers
    // ----------------------------------------------

    fn masp_is_active(&self) -> bool {
        self.masp.conditionals.last().is_none_or(|open| open.active)
    }

    fn masp_origin(&self) -> Option<Rc<Origin>> {
        self.masp
            .frames
            .last()
            .and_then(|frame| frame.origin.clone())
    }

    /// A token at the start of a line, out of the current expansion, for
    /// diagnostics.
    fn masp_at(&self, raw: &TextLine) -> Token {
        let mut token = super::lexer::tokenize("x", raw.loc.file, raw.loc.line)
            .unwrap()
            .tokens
            .remove(0);
        token.column = raw.loc.column;
        token.origin = self.masp_origin();
        token
    }

    fn masp_error(&self, raw: &TextLine, message: PreprocessMessage) -> Box<crate::Diagnostic> {
        self.error(&self.masp_at(raw), message)
    }

    fn masp_error_at(&self, loc: Loc, message: PreprocessMessage) -> Box<crate::Diagnostic> {
        self.masp_error(
            &TextLine {
                text: String::new(),
                loc,
            },
            message,
        )
    }

    /// Splits text from a line into tokens: each token's place is in the
    /// line's file, with its column counted in `text`, and it comes out of
    /// the current expansion.
    fn masp_tokenize(&mut self, text: &str, raw: &TextLine) -> Fallible<Vec<Token>> {
        let origin = self.masp_origin();
        let lexed = super::lexer::tokenize(text, raw.loc.file, raw.loc.line);
        let lexed = match lexed {
            Ok(lexed) => lexed,
            Err(error) => {
                let mut at = self.masp_at(raw);
                at.line = error.line;
                at.column = 0;
                return Err(self.error(&at, PreprocessMessage::Lex { error: error.kind }));
            }
        };
        for warning in lexed.warnings {
            let at = Loc {
                column: 0,
                line: warning.line,
                ..raw.loc
            };
            self.warn_at(
                at,
                PreprocessMessage::LexWarning {
                    warning: warning.kind,
                },
                Vec::new(),
            );
        }
        let mut tokens = lexed.tokens;
        for token in &mut tokens {
            token.origin = origin.clone();
        }
        Ok(tokens)
    }
}
