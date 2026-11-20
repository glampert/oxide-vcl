//! The preprocessor proper: reads the source a line at a time, carries out
//! directives, and expands macros in the code (`expander.rs`).

use std::collections::HashMap;
use std::path::PathBuf;

use super::PreprocessOptions;
use super::expression::{self, ExpressionError};
use super::lexer::{self, Loc, Punct, Token, TokenKind};
use super::macros::{Macro, MacroKind, MacroTable};
use super::source::Sources;
use crate::diagnostics::{Diagnostic, Note, PreprocessMessage, Severity, SourceLocation};

/// An error that stops the run, fully reported.
pub(super) type Fallible<T> = Result<T, Box<Diagnostic>>;

/// Deep enough for any sensible nesting, shallow enough to stop a file that
/// includes itself (without `#pragma once` or a guard) long before the
/// stack runs out.
const MAX_INCLUDE_DEPTH: usize = 200;

/// A long chain of `#include`s or of macro expansions is shortened to this
/// many entries at each end.
const CHAIN_ENDS_SHOWN: usize = 4;

/// A file being read: the main one, or one it includes.
struct FileFrame {
    tokens: Vec<Token>,
    next: usize,
    /// The number of open conditionals when the file was entered.
    conditional_depth: usize,
    /// Only its directives count (MASP mode's `#include`).
    macros_only: bool,
}

/// An `#if`, `#ifdef` or `#ifndef` and the `#elif`/`#else` after it.
struct Conditional {
    /// The directive that opened it.
    directive: String,
    opened_at: Token,
    /// The code around it is not being skipped.
    parent_active: bool,
    /// One of its branches has been taken.
    branch_taken: bool,
    /// The current branch is being taken.
    active: bool,
    seen_else: bool,
}

pub(super) struct Engine<'a> {
    pub options: &'a PreprocessOptions,
    pub sources: Sources<'a>,
    pub macros: MacroTable,
    pub warnings: Vec<Diagnostic>,
    /// The code, a token at a time; a token that starts a line says so.
    pub output: Vec<Token>,
    pub program_name: String,
    /// `__COUNTER__`'s next value.
    pub counter: u64,
    files: Vec<FileFrame>,
    conditionals: Vec<Conditional>,
    /// Names left in the code that are not macros, where first seen.
    used_undefined: HashMap<String, Loc>,
    vuprog_at: Option<Token>,
    endvuprog_at: Option<Token>,
    /// MASP mode: the main file's lines, through masp's directives.
    pub masp: super::masp::Masp,
}

/// The text of a directive's tokens from `first` on, spaced as written,
/// with a space before it.
fn rest_of_line(line: &[Token], first: usize) -> String {
    let mut text = String::new();
    for (index, token) in line.iter().enumerate().skip(first) {
        text.push_str(if index == first { " " } else { &token.leading });
        text.push_str(&token.text);
    }
    text
}

pub(super) fn number_token(value: i64, in_place_of: &Token) -> Token {
    let mut token = in_place_of.clone();
    token.kind = TokenKind::Number;
    token.punct = Punct::None;
    token.is_float = false;
    token.text = value.to_string();
    token
}

impl<'a> Engine<'a> {
    pub(super) fn new(options: &'a PreprocessOptions, sources: Sources<'a>) -> Self {
        Self {
            options,
            sources,
            macros: MacroTable::new(),
            warnings: Vec::new(),
            output: Vec::new(),
            program_name: String::new(),
            counter: 0,
            files: Vec::new(),
            conditionals: Vec::new(),
            used_undefined: HashMap::new(),
            vuprog_at: None,
            endvuprog_at: None,
            masp: Default::default(),
        }
    }

    /// Preprocesses the main file, leaving the code in `output`.
    pub(super) fn run(&mut self, main_file: usize) -> Fallible<()> {
        self.define_command_line_macros()?;
        // In MASP mode, masp's directives are carried out first, and the
        // lines that result come here, like masp's output going through cpp.
        if self.options.masp {
            self.masp_start(main_file);
        } else {
            self.enter_file(main_file)?;
        }
        while let Some(mut line) = self.next_line()? {
            if line[0].is(Punct::Hash) {
                self.handle_directive(&line)?;
            } else if self.is_active() {
                self.process_code_line(std::mem::take(&mut line))?;
            }
        }
        if self.options.masp {
            if let Some(open) = self.conditionals.last() {
                return Err(self.error(
                    &open.opened_at,
                    PreprocessMessage::UnterminatedConditional {
                        directive: open.directive.clone(),
                    },
                ));
            }
            return Ok(());
        }
        let main = Loc {
            file: main_file,
            line: 0,
            column: 0,
        };
        // VUSL has no #vuprog: its programs are named by a pragma.
        if !self.options.c_syntax {
            if self.vuprog_at.is_none() {
                self.warn_at(main, PreprocessMessage::MissingVuprog, Vec::new());
            }
            if self.endvuprog_at.is_none() {
                self.warn_at(main, PreprocessMessage::MissingEndvuprog, Vec::new());
            }
        }
        Ok(())
    }

    // ----------------------------------------------
    // Diagnostics
    // ----------------------------------------------

    pub(super) fn location(&self, loc: Loc) -> SourceLocation {
        SourceLocation::new(
            self.sources.file(loc.file).name.clone(),
            loc.line,
            loc.column,
        )
    }

    fn note_at(&self, loc: Option<Loc>, message: PreprocessMessage) -> Note {
        Note::new(loc.map(|loc| self.location(loc)), message)
    }

    /// The chain of invocations behind a token out of a macro expansion.
    fn expansion_notes(&self, token: &Token) -> Vec<Note> {
        let mut notes = Vec::new();
        let mut origin = token.origin.as_deref();
        while let Some(expansion) = origin {
            notes.push(self.note_at(
                Some(expansion.invoked_at),
                PreprocessMessage::InExpansionOf {
                    name: expansion.macro_name.clone(),
                },
            ));
            origin = expansion.parent.as_deref();
        }
        shorten(&mut notes, |count| {
            Note::new(None, PreprocessMessage::MoreExpansions { count })
        });
        notes
    }

    /// The chain of `#include`s that led to a file.
    fn include_notes(&self, file: usize) -> Vec<Note> {
        let mut notes = Vec::new();
        let mut from = self.sources.file(file).included_from;
        while let Some(loc) = from {
            notes.push(self.note_at(Some(loc), PreprocessMessage::IncludedFrom));
            from = self.sources.file(loc.file).included_from;
        }
        shorten(&mut notes, |count| {
            Note::new(None, PreprocessMessage::MoreIncludes { count })
        });
        notes
    }

    fn diagnostic(
        &self,
        severity: Severity,
        loc: Loc,
        message: PreprocessMessage,
        mut notes: Vec<Note>,
    ) -> Diagnostic {
        notes.extend(self.include_notes(loc.file));
        Diagnostic::new(severity, loc.line, message)
            .located_at(self.location(loc))
            .with_notes(notes)
    }

    /// An error at a place.
    pub(super) fn error_at(&self, loc: Loc, message: PreprocessMessage) -> Box<Diagnostic> {
        Box::new(self.diagnostic(Severity::Error, loc, message, Vec::new()))
    }

    /// An error at a token, with the expansions it came out of.
    pub(super) fn error(&self, token: &Token, message: PreprocessMessage) -> Box<Diagnostic> {
        self.error_with(token, message, Vec::new())
    }

    pub(super) fn error_with(
        &self,
        token: &Token,
        message: PreprocessMessage,
        notes: Vec<Note>,
    ) -> Box<Diagnostic> {
        let mut all = self.expansion_notes(token);
        all.extend(notes);
        Box::new(self.diagnostic(Severity::Error, token.loc(), message, all))
    }

    pub(super) fn warn_at(&mut self, loc: Loc, message: PreprocessMessage, notes: Vec<Note>) {
        let warning = self.diagnostic(Severity::Warning, loc, message, notes);
        self.warnings.push(warning);
    }

    pub(super) fn warn(&mut self, token: &Token, message: PreprocessMessage, notes: Vec<Note>) {
        let mut all = self.expansion_notes(token);
        all.extend(notes);
        self.warn_at(token.loc(), message, all);
    }

    /// The language of the files.
    pub(super) fn syntax(&self) -> lexer::Syntax {
        if self.options.c_syntax {
            lexer::Syntax::C
        } else {
            lexer::Syntax::Vcl
        }
    }

    /// Splits a loaded file into tokens.
    pub(super) fn tokenize_file(&mut self, file: usize) -> Fallible<Vec<Token>> {
        let text = self.sources.file(file).text.clone();
        self.tokenize_text(&text, file, 1)
    }

    /// Splits text from `file`, starting on `first_line`, into tokens.
    pub(super) fn tokenize_text(
        &mut self,
        text: &str,
        file: usize,
        first_line: usize,
    ) -> Fallible<Vec<Token>> {
        let at = |line| Loc {
            file,
            line,
            column: 0,
        };
        let lexed =
            lexer::tokenize_with(text, file, first_line, self.syntax()).map_err(|error| {
                self.error_at(at(error.line), PreprocessMessage::Lex { error: error.kind })
            })?;
        for warning in lexed.warnings {
            self.warn_at(
                at(warning.line),
                PreprocessMessage::LexWarning {
                    warning: warning.kind,
                },
                Vec::new(),
            );
        }
        Ok(lexed.tokens)
    }

    // ----------------------------------------------
    // Lines and files
    // ----------------------------------------------

    fn enter_file(&mut self, file: usize) -> Fallible<()> {
        let tokens = self.tokenize_file(file)?;
        // In MASP mode, files only come here through #include, which imports
        // their macros as `cpp -imacros` would: their other lines are C
        // declarations, not code for VCL.
        self.files.push(FileFrame {
            tokens,
            next: 0,
            conditional_depth: self.conditionals.len(),
            macros_only: self.options.masp,
        });
        Ok(())
    }

    fn leave_file(&mut self) -> Fallible<()> {
        // Conditionals do not carry across the end of a file.
        if self.conditionals.len() > self.file_conditional_depth() {
            let open = self.conditionals.last().unwrap();
            return Err(self.error(
                &open.opened_at,
                PreprocessMessage::UnterminatedConditional {
                    directive: open.directive.clone(),
                },
            ));
        }
        self.files.pop();
        Ok(())
    }

    /// The next line of the innermost file, if it has one left.
    fn take_line(frame: &mut FileFrame) -> Option<Vec<Token>> {
        if frame.next >= frame.tokens.len() {
            return None;
        }
        let mut line = vec![std::mem::replace(
            &mut frame.tokens[frame.next],
            placeholder(),
        )];
        frame.next += 1;
        while frame.next < frame.tokens.len() && !frame.tokens[frame.next].starts_line {
            line.push(std::mem::replace(
                &mut frame.tokens[frame.next],
                placeholder(),
            ));
            frame.next += 1;
        }
        Some(line)
    }

    fn next_line(&mut self) -> Fallible<Option<Vec<Token>>> {
        // An included file comes first; when there is none, MASP mode reads
        // on from the MASP stage.
        while let Some(frame) = self.files.last_mut() {
            match Self::take_line(frame) {
                Some(line) if !frame.macros_only || line[0].is(Punct::Hash) => {
                    return Ok(Some(line));
                }
                Some(_) => {}
                None => self.leave_file()?,
            }
        }
        if self.options.masp {
            return self.masp_next_line();
        }
        Ok(None)
    }

    /// The line after the current one, for an argument list that runs past
    /// the end of its line: up to the end of the file, or a directive, which
    /// ends the line.
    pub(super) fn more_line(&mut self) -> Fallible<Option<Vec<Token>>> {
        let Some(frame) = self.files.last_mut() else {
            // A code line from the MASP stage: so are the ones after it.
            if !self.options.masp {
                return Ok(None);
            }
            let Some(line) = self.masp_next_line()? else {
                return Ok(None);
            };
            if line[0].is(Punct::Hash) {
                self.masp_unget_line(line);
                return Ok(None);
            }
            return Ok(Some(line));
        };
        if frame
            .tokens
            .get(frame.next)
            .is_some_and(|token| token.is(Punct::Hash))
        {
            return Ok(None);
        }
        Ok(Self::take_line(frame))
    }

    fn is_active(&self) -> bool {
        self.conditionals.last().is_none_or(|open| open.active)
    }

    fn file_conditional_depth(&self) -> usize {
        self.files.last().map_or(0, |frame| frame.conditional_depth)
    }

    fn process_code_line(&mut self, line: Vec<Token>) -> Fallible<()> {
        let expanded = self.expand_line(line)?;
        self.record_undefined_names(&expanded);
        self.output.extend(expanded);
        Ok(())
    }

    /// Remembers the names left in the code: a macro defined after this
    /// would not apply here.
    fn record_undefined_names(&mut self, tokens: &[Token]) {
        for token in tokens {
            if token.is_identifier() && !self.macros.is_defined(&token.text) {
                self.used_undefined
                    .entry(token.text.clone())
                    .or_insert_with(|| token.use_loc());
            }
        }
    }

    // ----------------------------------------------
    // Directives
    // ----------------------------------------------

    fn handle_directive(&mut self, line: &[Token]) -> Fallible<()> {
        // A `#` alone on its line does nothing, as in C.
        if line.len() == 1 {
            return Ok(());
        }
        let directive = &line[1];
        // Conditionals are followed even in code being skipped, to find where
        // it ends.
        match directive.text.as_str() {
            "if" => return self.handle_if(line),
            "ifdef" => return self.handle_ifdef(line, false),
            "ifndef" => return self.handle_ifdef(line, true),
            "elif" => return self.handle_elif(line),
            "else" => return self.handle_else(line),
            "endif" => return self.handle_endif(line),
            _ => {}
        }
        if !self.is_active() {
            return Ok(());
        }
        // vclpp's own macros and programs have no place among masp's.
        if self.options.masp
            && matches!(
                directive.text.as_str(),
                "macro" | "endmacro" | "vuprog" | "endvuprog"
            )
        {
            return Err(self.error(
                directive,
                PreprocessMessage::NotInMaspMode {
                    directive: directive.text.clone(),
                },
            ));
        }
        match directive.text.as_str() {
            "include" => self.handle_include(line),
            "define" => self.handle_define(line),
            "undef" => self.handle_undef(line),
            "macro" => self.handle_block_macro(line),
            "vuprog" if !self.options.c_syntax => self.handle_vuprog(line),
            "endvuprog" if !self.options.c_syntax => self.handle_endvuprog(line),
            "pragma" => self.handle_pragma(line),
            "error" => Err(self.error(
                directive,
                PreprocessMessage::ErrorDirective {
                    text: rest_of_line(line, 2),
                },
            )),
            "warning" => {
                let text = rest_of_line(line, 2);
                self.warn(
                    directive,
                    PreprocessMessage::WarningDirective { text },
                    Vec::new(),
                );
                Ok(())
            }
            "endmacro" => Err(self.error(directive, PreprocessMessage::EndmacroWithoutMacro)),
            name => Err(self.error(
                directive,
                PreprocessMessage::UnknownDirective { name: name.into() },
            )),
        }
    }

    fn handle_include(&mut self, line: &[Token]) -> Fallible<()> {
        if line.len() < 3 {
            return Err(self.error(&line[1], PreprocessMessage::IncludeExpectsFile));
        }
        let first = &line[2];
        let mut name = String::new();
        let mut angled = false;
        let mut next = 3;
        if first.kind == TokenKind::String {
            name = first.text[1..first.text.len() - 1].to_string();
        } else if first.is(Punct::Less) {
            angled = true;
            while next < line.len() && !line[next].is(Punct::Greater) {
                if next != 3 {
                    name.push_str(&line[next].leading);
                }
                name.push_str(&line[next].text);
                next += 1;
            }
            if next == line.len() {
                return Err(self.error(first, PreprocessMessage::IncludeMissingGreater));
            }
            next += 1;
        } else {
            return Err(self.error(first, PreprocessMessage::IncludeExpectsFile));
        }
        if name.is_empty() {
            return Err(self.error(first, PreprocessMessage::IncludeEmptyName));
        }
        self.warn_extra_tokens(line, next);
        if self.files.len() >= MAX_INCLUDE_DEPTH {
            return Err(self.error(
                first,
                PreprocessMessage::IncludeTooDeep {
                    depth: MAX_INCLUDE_DEPTH,
                },
            ));
        }
        let path: PathBuf = match self.sources.find_include(&name, angled, line[0].file) {
            Ok(path) => path,
            Err(tried) => {
                let mut notes: Vec<Note> = tried
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
                if tried.is_empty() {
                    notes.push(Note::new(None, PreprocessMessage::NoIncludeDirectories));
                }
                return Err(self.error_with(
                    first,
                    PreprocessMessage::IncludeNotFound { name },
                    notes,
                ));
            }
        };
        if self.sources.is_include_once(&path) {
            return Ok(());
        }
        let included_from = Loc {
            file: line[0].file,
            line: line[0].line,
            column: 0,
        };
        let Some(file) = self.sources.load(&path, included_from) else {
            return Err(self.error(
                first,
                PreprocessMessage::IncludeUnreadable {
                    path: path.to_string_lossy().into_owned(),
                },
            ));
        };
        self.enter_file(file)
    }

    /// The token a directive is missing something after: its name, or the
    /// line's third token.
    fn name_or_last(line: &[Token]) -> &Token {
        &line[2.min(line.len() - 1)]
    }

    fn handle_define(&mut self, line: &[Token]) -> Fallible<()> {
        if line.len() < 3 || !line[2].is_identifier() {
            return Err(self.error(
                Self::name_or_last(line),
                PreprocessMessage::ExpectedMacroName {
                    directive: "define".into(),
                },
            ));
        }
        let name = &line[2];
        self.check_macro_name(name, false)?;
        let mut definition = Macro::new(&name.text, MacroKind::ObjectLike, name.loc());
        let mut next = 3;
        // A `(` right after the name, with no space between, opens a
        // parameter list.
        if next < line.len() && line[next].is(Punct::OpenParen) && line[next].leading.is_empty() {
            definition.kind = MacroKind::FunctionLike;
            let open = &line[next];
            next += 1;
            let mut expect_param = true;
            loop {
                let macro_name = definition.name.clone();
                let Some(token) = line.get(next) else {
                    return Err(self.error(
                        open,
                        PreprocessMessage::MissingParameterClose { name: macro_name },
                    ));
                };
                next += 1;
                if token.is(Punct::CloseParen) {
                    if expect_param && !definition.params.is_empty() {
                        return Err(self.error(
                            token,
                            PreprocessMessage::ExpectedParameterBeforeClose { name: macro_name },
                        ));
                    }
                    break;
                }
                if !expect_param {
                    if definition.variadic {
                        return Err(self.error(
                            token,
                            PreprocessMessage::EllipsisNotLast { name: macro_name },
                        ));
                    }
                    if !token.is(Punct::Comma) {
                        return Err(self.error(
                            token,
                            PreprocessMessage::ExpectedCommaOrClose { name: macro_name },
                        ));
                    }
                    expect_param = true;
                    continue;
                }
                if token.is(Punct::Ellipsis) {
                    definition.variadic = true;
                } else if !token.is_identifier() || token.text == "__VA_ARGS__" {
                    return Err(self.error(
                        token,
                        PreprocessMessage::ExpectedParameterName { name: macro_name },
                    ));
                } else if definition.params.contains(&token.text) {
                    return Err(self.error(
                        token,
                        PreprocessMessage::DuplicateParameter {
                            parameter: token.text.clone(),
                            name: macro_name,
                        },
                    ));
                } else {
                    definition.params.push(token.text.clone());
                }
                expect_param = false;
            }
        }
        // The body is one line, however many lines it was written across.
        definition.body = line[next..].to_vec();
        for token in &mut definition.body {
            token.starts_line = false;
        }
        if definition.kind == MacroKind::FunctionLike {
            for (index, token) in definition.body.iter().enumerate() {
                let parameter_after = definition
                    .body
                    .get(index + 1)
                    .and_then(|next| definition.param_index(next));
                if token.is(Punct::Hash) && parameter_after.is_none() {
                    return Err(self.error(
                        token,
                        PreprocessMessage::HashWithoutParameter {
                            name: definition.name.clone(),
                        },
                    ));
                }
            }
        }
        self.check_paste_operators(&definition)?;
        self.define_macro(definition, name);
        Ok(())
    }

    fn handle_undef(&mut self, line: &[Token]) -> Fallible<()> {
        if line.len() < 3 || !line[2].is_identifier() {
            return Err(self.error(
                Self::name_or_last(line),
                PreprocessMessage::ExpectedMacroName {
                    directive: "undef".into(),
                },
            ));
        }
        self.check_macro_name(&line[2], true)?;
        self.warn_extra_tokens(line, 3);
        self.macros.undefine(&line[2].text);
        Ok(())
    }

    fn handle_block_macro(&mut self, line: &[Token]) -> Fallible<()> {
        if line.len() < 3 || !line[2].is_identifier() {
            return Err(self.error(
                Self::name_or_last(line),
                PreprocessMessage::ExpectedMacroName {
                    directive: "macro".into(),
                },
            ));
        }
        let name = &line[2];
        self.check_macro_name(name, false)?;
        let mut definition = Macro::new(&name.text, MacroKind::Block, name.loc());
        let macro_name = definition.name.clone();
        // The parameter list: `#macro Name: param1, param2`.
        if line.len() > 3 {
            if !line[3].is(Punct::Colon) {
                return Err(self.error(
                    &line[3],
                    PreprocessMessage::BlockMacroWithoutColon { name: macro_name },
                ));
            }
            let mut expect_param = true;
            for token in &line[4..] {
                if token.is(Punct::Comma) {
                    if expect_param {
                        let message = match definition.params.last() {
                            None => PreprocessMessage::LostCommaBeforeFirst { name: macro_name },
                            Some(parameter) => PreprocessMessage::LostCommaAfter {
                                parameter: parameter.clone(),
                                name: macro_name,
                            },
                        };
                        return Err(self.error(token, message));
                    }
                    expect_param = true;
                    continue;
                }
                if !expect_param {
                    return Err(self.error(
                        token,
                        PreprocessMessage::MissingCommaAfter {
                            parameter: definition.params.last().cloned().unwrap_or_default(),
                            name: macro_name,
                        },
                    ));
                }
                if !token.is_identifier() {
                    return Err(self.error(
                        token,
                        PreprocessMessage::ExpectedParameterName { name: macro_name },
                    ));
                }
                if definition.params.contains(&token.text) {
                    return Err(self.error(
                        token,
                        PreprocessMessage::DuplicateParameter {
                            parameter: token.text.clone(),
                            name: macro_name,
                        },
                    ));
                }
                definition.params.push(token.text.clone());
                expect_param = false;
            }
            if expect_param && let Some(parameter) = definition.params.last() {
                return Err(self.error(
                    line.last().unwrap(),
                    PreprocessMessage::ExtraneousComma {
                        parameter: parameter.clone(),
                        name: macro_name,
                    },
                ));
            }
        }
        // The body: the lines up to #endmacro, as they are written.
        loop {
            let Some(body_line) = self.files.last_mut().and_then(Self::take_line) else {
                return Err(self.error(
                    name,
                    PreprocessMessage::UnterminatedMacro {
                        name: definition.name.clone(),
                    },
                ));
            };
            if body_line[0].is(Punct::Hash) {
                let directive = body_line
                    .get(1)
                    .map_or(String::new(), |token| token.text.clone());
                if directive == "endmacro" {
                    if body_line.len() > 2 {
                        return Err(self.error(&body_line[2], PreprocessMessage::TextAfterEndmacro));
                    }
                    break;
                }
                return Err(self.error(
                    &body_line[0],
                    PreprocessMessage::DirectiveInMacro {
                        directive,
                        name: definition.name.clone(),
                    },
                ));
            }
            definition.body.extend(body_line);
        }
        self.check_paste_operators(&definition)?;
        self.define_macro(definition, name);
        Ok(())
    }

    fn handle_vuprog(&mut self, line: &[Token]) -> Fallible<()> {
        let directive = &line[1];
        if let Some(first) = &self.vuprog_at {
            let note = self.note_at(Some(first.loc()), PreprocessMessage::FirstOneHere);
            return Err(self.error_with(directive, PreprocessMessage::VuprogTwice, vec![note]));
        }
        self.vuprog_at = Some(directive.clone());
        if line.len() > 2 {
            if !line[2].is_identifier() {
                return Err(self.error(&line[2], PreprocessMessage::ExpectedProgramName));
            }
            if line.len() > 3 {
                return Err(self.error(&line[3], PreprocessMessage::TextAfterProgramName));
            }
            self.program_name = line[2].text.clone();
        }
        Ok(())
    }

    fn handle_endvuprog(&mut self, line: &[Token]) -> Fallible<()> {
        let directive = &line[1];
        if self.vuprog_at.is_none() {
            return Err(self.error(directive, PreprocessMessage::EndvuprogWithoutVuprog));
        }
        if let Some(first) = &self.endvuprog_at {
            let note = self.note_at(Some(first.loc()), PreprocessMessage::FirstOneHere);
            return Err(self.error_with(directive, PreprocessMessage::EndvuprogTwice, vec![note]));
        }
        if line.len() > 2 {
            return Err(self.error(&line[2], PreprocessMessage::TextAfterEndvuprog));
        }
        self.endvuprog_at = Some(directive.clone());
        Ok(())
    }

    fn handle_pragma(&mut self, line: &[Token]) -> Fallible<()> {
        if line.len() == 3 && line[2].text == "once" {
            self.sources.mark_include_once(line[0].file);
            return Ok(());
        }
        if self.options.c_syntax {
            // The frontend reads the pragma: it goes out as a line of its
            // own, `#pragma` as written and the arguments expanded.
            let mut directive = line[..2].to_vec();
            directive[0].starts_line = true;
            let arguments = self.expand_tokens(line[2..].to_vec(), "a #pragma")?;
            self.record_undefined_names(&arguments);
            self.output.extend(directive);
            self.output.extend(arguments.into_iter().map(|mut token| {
                token.starts_line = false;
                token
            }));
            return Ok(());
        }
        let text = rest_of_line(line, 2);
        self.warn(
            &line[1],
            PreprocessMessage::UnknownPragma { text },
            Vec::new(),
        );
        Ok(())
    }

    fn handle_if(&mut self, line: &[Token]) -> Fallible<()> {
        // Inside code being skipped, no branch is taken and nothing is
        // evaluated.
        let parent_active = self.is_active();
        let taken = parent_active && self.evaluate_condition(line)?;
        self.conditionals.push(Conditional {
            directive: "if".into(),
            opened_at: line[1].clone(),
            parent_active,
            branch_taken: taken,
            active: taken,
            seen_else: false,
        });
        Ok(())
    }

    fn handle_ifdef(&mut self, line: &[Token], negated: bool) -> Fallible<()> {
        let directive = &line[1];
        let parent_active = self.is_active();
        let mut taken = false;
        if parent_active {
            if line.len() < 3 || !line[2].is_identifier() {
                return Err(self.error(
                    Self::name_or_last(line),
                    PreprocessMessage::ExpectedMacroName {
                        directive: directive.text.clone(),
                    },
                ));
            }
            self.warn_extra_tokens(line, 3);
            taken = self.macros.is_defined(&line[2].text) != negated;
        }
        self.conditionals.push(Conditional {
            directive: directive.text.clone(),
            opened_at: directive.clone(),
            parent_active,
            branch_taken: taken,
            active: taken,
            seen_else: false,
        });
        Ok(())
    }

    /// The innermost conditional, which must have been opened in this file.
    fn current_conditional(&self, directive: &Token) -> Fallible<()> {
        if self.conditionals.len() <= self.file_conditional_depth() {
            return Err(self.error(
                directive,
                PreprocessMessage::WithoutIf {
                    directive: directive.text.clone(),
                },
            ));
        }
        Ok(())
    }

    fn handle_elif(&mut self, line: &[Token]) -> Fallible<()> {
        self.current_conditional(&line[1])?;
        let open = self.conditionals.last().unwrap();
        if open.seen_else {
            return Err(self.error(&line[1], PreprocessMessage::ElifAfterElse));
        }
        if !open.parent_active || open.branch_taken {
            self.conditionals.last_mut().unwrap().active = false;
            return Ok(());
        }
        let active = self.evaluate_condition(line)?;
        let open = self.conditionals.last_mut().unwrap();
        open.active = active;
        open.branch_taken = active;
        Ok(())
    }

    fn handle_else(&mut self, line: &[Token]) -> Fallible<()> {
        self.current_conditional(&line[1])?;
        let open = self.conditionals.last().unwrap();
        if open.seen_else {
            return Err(self.error(&line[1], PreprocessMessage::ElseAfterElse));
        }
        if open.parent_active {
            self.warn_extra_tokens(line, 2);
        }
        let open = self.conditionals.last_mut().unwrap();
        open.seen_else = true;
        open.active = open.parent_active && !open.branch_taken;
        open.branch_taken = true;
        Ok(())
    }

    fn handle_endif(&mut self, line: &[Token]) -> Fallible<()> {
        self.current_conditional(&line[1])?;
        if self.conditionals.last().unwrap().parent_active {
            self.warn_extra_tokens(line, 2);
        }
        self.conditionals.pop();
        Ok(())
    }

    // ----------------------------------------------
    // Directive helpers
    // ----------------------------------------------

    fn evaluate_condition(&mut self, line: &[Token]) -> Fallible<bool> {
        let directive = &line[1];
        // `defined NAME` and `defined(NAME)` first, before NAME could be
        // expanded.
        let mut tokens = Vec::new();
        let mut index = 2;
        while index < line.len() {
            let token = &line[index];
            if !token.is_identifier() || token.text != "defined" {
                tokens.push(token.clone());
                index += 1;
                continue;
            }
            let mut name = index + 1;
            let parenthesized = line.get(name).is_some_and(|next| next.is(Punct::OpenParen));
            if parenthesized {
                name += 1;
            }
            if !line.get(name).is_some_and(Token::is_identifier) {
                return Err(self.error(token, PreprocessMessage::DefinedWithoutName));
            }
            let mut last = name;
            if parenthesized {
                last += 1;
                if !line
                    .get(last)
                    .is_some_and(|close| close.is(Punct::CloseParen))
                {
                    return Err(self.error(
                        token,
                        PreprocessMessage::DefinedMissingClose {
                            name: line[name].text.clone(),
                        },
                    ));
                }
            }
            let defined = self.macros.is_defined(&line[name].text);
            tokens.push(number_token(i64::from(defined), token));
            index = last + 1;
        }
        if tokens.is_empty() {
            return Err(self.error(
                directive,
                PreprocessMessage::NoExpression {
                    directive: directive.text.clone(),
                },
            ));
        }
        let context = format!("an #{} expression", directive.text);
        let mut expanded = self.expand_tokens(tokens, &context)?;
        // Names left after expansion are not macros. As in C, they count as
        // 0; as in C++, `true` and `false` count as 1 and 0.
        for token in &mut expanded {
            if !token.is_identifier() {
                continue;
            }
            if token.text == "true" || token.text == "false" {
                *token = number_token(i64::from(token.text == "true"), token);
                continue;
            }
            if self.options.warn_undef {
                let name = token.text.clone();
                self.warn(
                    token,
                    PreprocessMessage::NotDefinedIsZero { name },
                    Vec::new(),
                );
            }
            *token = number_token(0, token);
        }
        let value = self.evaluate(&expanded, directive, &format!("#{}", directive.text))?;
        Ok(value != 0)
    }

    /// The value of an integer expression. Errors are reported at `at`,
    /// prefixed with `context`.
    pub(super) fn evaluate(&self, tokens: &[Token], at: &Token, context: &str) -> Fallible<i64> {
        for token in tokens {
            if token.kind == TokenKind::Number && token.is_float {
                return Err(self.error(
                    token,
                    PreprocessMessage::FloatInExpression {
                        context: context.into(),
                        number: token.text.clone(),
                    },
                ));
            }
            if !matches!(token.kind, TokenKind::Number | TokenKind::Punctuation) {
                return Err(self.error(
                    token,
                    PreprocessMessage::NotInExpression {
                        context: context.into(),
                        token: token.text.clone(),
                    },
                ));
            }
        }
        expression::evaluate(tokens).map_err(|error: ExpressionError| {
            self.error(
                at,
                PreprocessMessage::Expression {
                    context: context.into(),
                    error,
                },
            )
        })
    }

    fn check_macro_name(&self, name: &Token, undefining: bool) -> Fallible<()> {
        if name.text == "defined" {
            return Err(self.error(name, PreprocessMessage::DefinedAsMacroName));
        }
        if MacroTable::is_builtin_name(&name.text) {
            return Err(self.error(
                name,
                PreprocessMessage::ChangeBuiltin {
                    name: name.text.clone(),
                    undefining,
                },
            ));
        }
        Ok(())
    }

    /// `##` needs a token on either side of it, on its own line.
    fn check_paste_operators(&self, definition: &Macro) -> Fallible<()> {
        let body = &definition.body;
        for (index, token) in body.iter().enumerate() {
            if !token.is(Punct::HashHash) {
                continue;
            }
            let at_start = index == 0 || token.starts_line;
            let at_end = body.get(index + 1).is_none_or(|next| next.starts_line);
            if at_start || at_end {
                return Err(self.error(
                    token,
                    PreprocessMessage::PasteAtEnd {
                        name: definition.name.clone(),
                        block: definition.kind == MacroKind::Block,
                    },
                ));
            }
        }
        Ok(())
    }

    fn define_macro(&mut self, definition: Macro, name: &Token) {
        // vclpp 1 expanded a #define anywhere in the file, even above it.
        if let Some(use_at) = self.used_undefined.remove(&definition.name) {
            let note = self.note_at(Some(use_at), PreprocessMessage::FirstUsedHere);
            self.warn(
                name,
                PreprocessMessage::DefinedAfterUse {
                    name: definition.name.clone(),
                },
                vec![note],
            );
        }
        let previous = self.macros.define(definition);
        let current = self.macros.find(&name.text).unwrap();
        if let Some(previous) = previous
            && !previous.same_definition(&current)
        {
            let note = self.note_at(
                Some(previous.defined_at),
                PreprocessMessage::PreviousDefinitionHere,
            );
            self.warn(
                name,
                PreprocessMessage::Redefined {
                    name: name.text.clone(),
                },
                vec![note],
            );
        }
    }

    fn define_command_line_macros(&mut self) -> Fallible<()> {
        for (name, value) in &self.options.defines {
            let file = self.sources.add_text("<command line>", value);
            let mut definition = Macro::new(
                name,
                MacroKind::ObjectLike,
                Loc {
                    file,
                    line: 0,
                    column: 0,
                },
            );
            definition.body = self.tokenize_file(file)?;
            for token in &mut definition.body {
                token.starts_line = false;
            }
            self.macros.define(definition);
        }
        Ok(())
    }

    fn warn_extra_tokens(&mut self, line: &[Token], first_extra: usize) {
        if let Some(extra) = line.get(first_extra) {
            let directive = line[1].text.clone();
            self.warn(
                extra,
                PreprocessMessage::ExtraTokens { directive },
                Vec::new(),
            );
        }
    }
}

/// A token left in place of one moved out of a file's list.
fn placeholder() -> Token {
    Token {
        kind: TokenKind::Punctuation,
        punct: Punct::None,
        is_float: false,
        starts_line: false,
        blank_before: false,
        blank_after: false,
        text: String::new(),
        leading: String::new(),
        file: 0,
        line: 0,
        column: 0,
        hidden: Vec::new(),
        origin: None,
    }
}

/// Shortens a long chain to its two ends, with `more` for what is left out.
fn shorten(notes: &mut Vec<Note>, more: impl Fn(usize) -> Note) {
    if notes.len() > 2 * CHAIN_ENDS_SHOWN {
        let hidden = notes.len() - 2 * CHAIN_ENDS_SHOWN;
        notes.drain(CHAIN_ENDS_SHOWN..notes.len() - CHAIN_ENDS_SHOWN);
        notes.insert(CHAIN_ENDS_SHOWN, more(hidden));
    }
}
