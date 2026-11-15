//! Macro expansion: `#define` constants and function-like macros, and
//! `#macro` blocks.
//!
//! Expansion works as the C preprocessor's does: a replacement is rescanned
//! for more macros, and a token that came out of a macro's expansion is
//! never expanded as that macro again (the "hide set" algorithm GCC and
//! Clang document), so a self-referential macro stops rather than recursing.
//! A `#macro` block invoked as `Name{ args }` follows the same rules, but its
//! body is lines: the invocation is replaced by them, and a block that leads
//! back to itself is an error, since leaving it unexpanded is no use to VCL.

use std::collections::VecDeque;
use std::rc::Rc;

use super::engine::{Engine, Fallible};
use super::lexer::{self, Origin, Punct, Token, TokenKind};
use super::macros::{Macro, MacroKind};
use crate::diagnostics::{Note, PreprocessMessage};

type TokenList = VecDeque<Token>;

/// A macro invocation's arguments and the `)` or `}` that ended them.
struct Arguments {
    values: Vec<Vec<Token>>,
    close: Token,
}

fn hides(hidden: &[u32], id: u32) -> bool {
    hidden.binary_search(&id).is_ok()
}

fn with_id(mut hidden: Vec<u32>, id: u32) -> Vec<u32> {
    if let Err(at) = hidden.binary_search(&id) {
        hidden.insert(at, id);
    }
    hidden
}

fn intersection(a: &[u32], b: &[u32]) -> Vec<u32> {
    a.iter().copied().filter(|id| hides(b, *id)).collect()
}

fn merged(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut result: Vec<u32> = a.iter().chain(b).copied().collect();
    result.sort_unstable();
    result.dedup();
    result
}

/// Whitespace inside a single-line expansion is just "some" or "none", as
/// in C.
fn normalize_spacing(token: &mut Token) {
    token.starts_line = false;
    token.blank_before = false;
    token.blank_after = false;
    if !token.leading.is_empty() {
        token.leading = " ".to_string();
    }
}

/// Puts a macro's replacement back on the input to be rescanned, in the
/// place of the invocation that ran from `first` to `last`.
fn push_replacement(
    input: &mut TokenList,
    mut replacement: Vec<Token>,
    first: &Token,
    last: &Token,
) {
    if let Some(head) = replacement.first_mut() {
        head.starts_line = first.starts_line;
        head.leading = first.leading.clone();
        head.blank_before = first.blank_before;
        replacement.last_mut().unwrap().blank_after = last.blank_after;
    } else if first.starts_line
        && let Some(next) = input.front_mut()
        && !next.starts_line
    {
        // Nothing replaces it and its line goes on: what follows starts the
        // line now.
        next.starts_line = true;
        next.leading = first.leading.clone();
        next.blank_before = first.blank_before;
    }
    for token in replacement.into_iter().rev() {
        input.push_front(token);
    }
}

fn closer_of(token: &Token) -> Option<Punct> {
    match token.punct {
        _ if token.kind != TokenKind::Punctuation => None,
        Punct::OpenParen => Some(Punct::CloseParen),
        Punct::OpenBracket => Some(Punct::CloseBracket),
        Punct::OpenBrace => Some(Punct::CloseBrace),
        _ => None,
    }
}

fn opens_nesting(token: &Token, all_brackets: bool) -> bool {
    token.is(Punct::OpenParen)
        || (all_brackets && (token.is(Punct::OpenBracket) || token.is(Punct::OpenBrace)))
}

fn closes_nesting(token: &Token, all_brackets: bool) -> bool {
    token.is(Punct::CloseParen)
        || (all_brackets && (token.is(Punct::CloseBracket) || token.is(Punct::CloseBrace)))
}

/// `text` as a C string literal: in quotes, with `"` and `\` escaped.
fn quoted(text: &str) -> String {
    let mut result = String::from("\"");
    for c in text.chars() {
        if c == '"' || c == '\\' {
            result.push('\\');
        }
        result.push(c);
    }
    result.push('"');
    result
}

impl Engine<'_> {
    /// Expands a line of code. Block macros in it expand to their lines, so
    /// the result can span several lines.
    pub(super) fn expand_line(&mut self, line: Vec<Token>) -> Fallible<Vec<Token>> {
        let mut input: TokenList = line.into();
        self.expand(&mut input, true, "")
    }

    /// Expands tokens on their own, such as those of an `#if` expression. A
    /// block macro in them is an error, said to be invoked in `context`.
    pub(super) fn expand_tokens(
        &mut self,
        tokens: Vec<Token>,
        context: &str,
    ) -> Fallible<Vec<Token>> {
        let mut input: TokenList = tokens.into();
        self.expand(&mut input, false, context)
    }

    fn expand(
        &mut self,
        input: &mut TokenList,
        more_lines: bool,
        block_context: &str,
    ) -> Fallible<Vec<Token>> {
        let mut output = Vec::new();
        // The indentation of the line being output.
        let mut line_indent = String::new();
        let emit = |token: Token, output: &mut Vec<Token>, line_indent: &mut String| {
            if token.starts_line {
                *line_indent = token.leading.clone();
            }
            output.push(token);
        };
        while let Some(name) = input.pop_front() {
            // An argument list has to open on the same line as the name.
            let followed_by = |input: &TokenList, punct: Punct| {
                input
                    .front()
                    .is_some_and(|next| !next.starts_line && next.is(punct))
            };
            let found = if name.is_identifier() {
                self.macros.find(&name.text)
            } else {
                None
            };
            let Some(definition) = found else {
                // Name{ } in code can only be a block macro invocation. Left
                // as it is, it would only puzzle VCL.
                if !self.options.masp
                    && name.is_identifier()
                    && block_context.is_empty()
                    && followed_by(input, Punct::OpenBrace)
                {
                    return Err(self.error(
                        &name,
                        PreprocessMessage::NotABlockMacro {
                            name: name.text.clone(),
                        },
                    ));
                }
                emit(name, &mut output, &mut line_indent);
                continue;
            };
            let hidden = hides(&name.hidden, definition.id);
            match definition.kind {
                MacroKind::Builtin => {
                    let token = self.expand_builtin(&definition, &name);
                    emit(token, &mut output, &mut line_indent);
                }
                MacroKind::ObjectLike => {
                    if hidden {
                        emit(name, &mut output, &mut line_indent);
                        continue;
                    }
                    let replacement = self.substitute(
                        &definition,
                        &name,
                        &[],
                        &with_id(name.hidden.clone(), definition.id),
                    )?;
                    push_replacement(input, replacement, &name, &name);
                }
                MacroKind::FunctionLike => {
                    // Without an argument list the name is just a name, as in C.
                    if hidden || !followed_by(input, Punct::OpenParen) {
                        emit(name, &mut output, &mut line_indent);
                        continue;
                    }
                    let mut args = self.collect_arguments(input, more_lines, &definition, &name)?;
                    self.check_arguments(&definition, &name, &mut args)?;
                    let expansion_hidden = with_id(
                        intersection(&name.hidden, &args.close.hidden),
                        definition.id,
                    );
                    let replacement =
                        self.substitute(&definition, &name, &args.values, &expansion_hidden)?;
                    push_replacement(input, replacement, &name, &args.close);
                }
                MacroKind::Block => {
                    // As in vclpp 1, the name without `{` after it is not an
                    // invocation.
                    if !followed_by(input, Punct::OpenBrace) {
                        emit(name, &mut output, &mut line_indent);
                        continue;
                    }
                    if hidden {
                        return Err(self.error(
                            &name,
                            PreprocessMessage::RecursiveBlockMacro {
                                name: definition.name.clone(),
                            },
                        ));
                    }
                    if !block_context.is_empty() {
                        return Err(self.error(
                            &name,
                            PreprocessMessage::BlockMacroInContext {
                                name: definition.name.clone(),
                                context: block_context.into(),
                            },
                        ));
                    }
                    let indent = if name.starts_line {
                        name.leading.clone()
                    } else {
                        line_indent.clone()
                    };
                    let mut args = self.collect_arguments(input, more_lines, &definition, &name)?;
                    self.check_arguments(&definition, &name, &mut args)?;
                    let mut body = self.substitute(
                        &definition,
                        &name,
                        &args.values,
                        &with_id(name.hidden.clone(), definition.id),
                    )?;
                    // The body's lines replace the invocation, and whatever
                    // followed it on its line moves to a line of its own.
                    if let Some(next) = input.front_mut()
                        && !next.starts_line
                    {
                        next.starts_line = true;
                        next.leading = indent;
                    }
                    if !body.is_empty() {
                        // An invocation written in the source, not one inside
                        // another macro, is set off by blank lines; one that
                        // starts or ends the body of another passes that
                        // one's blank lines on.
                        let in_source = name.origin.is_none();
                        let first = body.first_mut().unwrap();
                        first.starts_line = true;
                        first.blank_before = first.blank_before || name.blank_before || in_source;
                        let last = body.last_mut().unwrap();
                        last.blank_after = last.blank_after || args.close.blank_after || in_source;
                    }
                    for token in body.into_iter().rev() {
                        input.push_front(token);
                    }
                }
            }
        }
        Ok(output)
    }

    fn collect_arguments(
        &mut self,
        input: &mut TokenList,
        more_lines: bool,
        definition: &Macro,
        name: &Token,
    ) -> Fallible<Arguments> {
        // A block macro's arguments can hold any brackets, which group commas
        // as well. In a function-like macro's, only parentheses do, as in C.
        let block = definition.kind == MacroKind::Block;
        let close = if block {
            Punct::CloseBrace
        } else {
            Punct::CloseParen
        };
        // A variadic macro's trailing arguments, commas and all, make up
        // __VA_ARGS__.
        let max_values = if definition.variadic {
            definition.params.len() + 1
        } else {
            usize::MAX
        };
        input.pop_front(); // The opening `(` or `{`.
        let mut values: Vec<Vec<Token>> = vec![Vec::new()];
        // The closers of the brackets open inside the arguments.
        let mut nesting: Vec<Punct> = Vec::new();
        loop {
            let Some(token) = input.pop_front() else {
                let line = if more_lines { self.more_line()? } else { None };
                let Some(line) = line else {
                    return Err(self.error(
                        name,
                        PreprocessMessage::UnterminatedArguments {
                            name: definition.name.clone(),
                        },
                    ));
                };
                input.extend(line);
                continue;
            };
            if nesting.is_empty() && token.is(close) {
                return Ok(Arguments {
                    values,
                    close: token,
                });
            }
            if opens_nesting(&token, block) {
                nesting.push(closer_of(&token).unwrap());
            } else if closes_nesting(&token, block) {
                if nesting.last().is_none_or(|&expected| !token.is(expected)) {
                    return Err(self.error(
                        &token,
                        PreprocessMessage::UnbalancedArgument {
                            token: token.text.clone(),
                            name: definition.name.clone(),
                        },
                    ));
                }
                nesting.pop();
            } else if nesting.is_empty() && token.is(Punct::Comma) && values.len() < max_values {
                values.push(Vec::new());
                continue;
            }
            values.last_mut().unwrap().push(token);
        }
    }

    fn check_arguments(
        &self,
        definition: &Macro,
        name: &Token,
        args: &mut Arguments,
    ) -> Fallible<()> {
        let values = &mut args.values;
        let defined_here = || {
            vec![Note::new(
                Some(self.location(definition.defined_at)),
                PreprocessMessage::DefinedHere {
                    name: definition.name.clone(),
                },
            )]
        };
        // `NAME()` and `NAME{ }` pass no arguments, rather than one that is
        // empty - unless the macro takes one, which a function-like macro can
        // be given empty.
        let takes_one = definition.kind == MacroKind::FunctionLike
            && definition.params.len() + usize::from(definition.variadic) == 1;
        if values.len() == 1 && values[0].is_empty() && !takes_one {
            values.clear();
        }
        if definition.variadic {
            if values.len() < definition.params.len() {
                return Err(self.error_with(
                    name,
                    PreprocessMessage::TooFewArguments {
                        name: definition.name.clone(),
                        at_least: definition.params.len(),
                        given: values.len(),
                    },
                    defined_here(),
                ));
            }
            if values.len() == definition.params.len() {
                // No trailing arguments: __VA_ARGS__ is empty.
                values.push(Vec::new());
            }
            return Ok(());
        }
        if values.len() != definition.params.len() {
            return Err(self.error_with(
                name,
                PreprocessMessage::WrongArgumentCount {
                    name: definition.name.clone(),
                    takes: definition.params.len(),
                    given: values.len(),
                },
                defined_here(),
            ));
        }
        // A block macro's arguments are operands, and an empty operand is a
        // mistake.
        if definition.kind == MacroKind::Block
            && let Some(index) = values.iter().position(Vec::is_empty)
        {
            return Err(self.error_with(
                name,
                PreprocessMessage::EmptyBlockArgument {
                    index: index + 1,
                    parameter: definition.params[index].clone(),
                    name: definition.name.clone(),
                },
                defined_here(),
            ));
        }
        Ok(())
    }

    fn substitute(
        &mut self,
        definition: &Macro,
        name: &Token,
        args: &[Vec<Token>],
        hidden: &[u32],
    ) -> Fallible<Vec<Token>> {
        // Each argument is fully expanded on its own before it replaces its
        // parameter, but only once, and only if a parameter needs it: the
        // operands of # and ## are not.
        let mut expanded_args: Vec<Option<Vec<Token>>> = vec![None; args.len()];
        let origin = Rc::new(Origin {
            macro_name: definition.name.clone(),
            invoked_at: name.loc(),
            parent: name.origin.clone(),
        });
        let body = &definition.body;
        let mut out: Vec<Token> = Vec::new();
        // The `##` waiting for its right operand.
        let mut paste_op: Option<&Token> = None;
        // The last thing added was an empty argument.
        let mut last_piece_empty = false;
        let mut index = 0;
        while index < body.len() {
            let token = &body[index];
            if token.is(Punct::HashHash) {
                paste_op = Some(token);
                index += 1;
                continue;
            }
            let mut piece: Vec<Token>;
            let mut piece_is_argument = false;
            if definition.kind == MacroKind::FunctionLike && token.is(Punct::Hash) {
                // `#param`: the argument's spelling as a string literal. The
                // definition was checked to have a parameter after every `#`.
                let param = definition.param_index(&body[index + 1]).unwrap();
                piece = vec![stringize(&args[param], token)];
                index += 1;
            } else if let Some(param) = definition.param_index(token) {
                let pasted = paste_op.is_some()
                    || body
                        .get(index + 1)
                        .is_some_and(|next| next.is(Punct::HashHash));
                piece = if pasted {
                    args[param].clone()
                } else {
                    if expanded_args[param].is_none() {
                        let mut input: TokenList = args[param].iter().cloned().collect();
                        expanded_args[param] =
                            Some(self.expand(&mut input, false, "a macro argument")?);
                    }
                    expanded_args[param].clone().unwrap()
                };
                piece_is_argument = true;
                // The argument takes the parameter's place on its line.
                for (k, argument) in piece.iter_mut().enumerate() {
                    normalize_spacing(argument);
                    if k == 0 {
                        argument.starts_line = token.starts_line;
                        argument.leading = token.leading.clone();
                    }
                }
            } else {
                piece = vec![token.clone()];
            }
            if let Some(op) = paste_op.take() {
                // An empty operand leaves the other one as it is.
                if !last_piece_empty
                    && let Some(left) = out.last()
                    && !piece.is_empty()
                {
                    let mut op = op.clone();
                    op.origin = Some(origin.clone());
                    let pasted = self.paste(left, &piece[0], &op)?;
                    *out.last_mut().unwrap() = pasted;
                    piece.remove(0);
                }
            }
            last_piece_empty = piece_is_argument && piece.is_empty();
            out.extend(piece);
            index += 1;
        }
        // Every token knows the expansion it came out of, and must not be
        // expanded as this macro again. Only a block macro's body is lines;
        // any other replacement goes in the invocation's place on its line.
        for (index, token) in out.iter_mut().enumerate() {
            token.hidden = merged(&token.hidden, hidden);
            token.origin = Some(origin.clone());
            if definition.kind != MacroKind::Block && index != 0 {
                normalize_spacing(token);
            }
        }
        Ok(out)
    }

    fn expand_builtin(&mut self, definition: &Macro, name: &Token) -> Token {
        // __FILE__ and __LINE__ say where the outermost macro was invoked, as
        // in C.
        let at = name.use_loc();
        let mut result = name.clone();
        result.kind = TokenKind::Number;
        result.punct = Punct::None;
        result.is_float = false;
        result.text = match definition.name.as_str() {
            "__FILE__" => {
                result.kind = TokenKind::String;
                quoted(&self.sources.file(at.file).name)
            }
            "__LINE__" => at.line.to_string(),
            "__COUNTER__" => {
                self.counter += 1;
                (self.counter - 1).to_string()
            }
            // __VCLPP__: vclpp's major version.
            _ => "2".to_string(),
        };
        result
    }

    fn paste(&self, left: &Token, right: &Token, op: &Token) -> Fallible<Token> {
        let text = format!("{}{}", left.text, right.text);
        let Some(lexed) = lexer::lex_single_token(&text) else {
            return Err(self.error(
                op,
                PreprocessMessage::InvalidPaste {
                    left: left.text.clone(),
                    right: right.text.clone(),
                },
            ));
        };
        let mut result = left.clone();
        result.kind = lexed.kind;
        result.punct = lexed.punct;
        result.is_float = lexed.is_float;
        result.text = lexed.text;
        result.hidden = merged(&left.hidden, &right.hidden);
        Ok(result)
    }
}

fn stringize(arg: &[Token], hash: &Token) -> Token {
    let mut text = String::new();
    for (index, token) in arg.iter().enumerate() {
        if index != 0 && !token.leading.is_empty() {
            text.push(' ');
        }
        if matches!(token.kind, TokenKind::String | TokenKind::CharLiteral) {
            // Escaped, without the new quotes.
            let escaped = quoted(&token.text);
            text.push_str(&escaped[1..escaped.len() - 1]);
        } else {
            text.push_str(&token.text);
        }
    }
    let mut result = hash.clone();
    result.kind = TokenKind::String;
    result.punct = Punct::None;
    result.text = format!("\"{text}\"");
    result
}
