//! The preprocessed tokens back as text: one line of code per line, as it was
//! indented, with the optional VCL boilerplate around it.

use super::engine::{Engine, Fallible};
use super::lexer::{Loc, Punct, Token, TokenKind};

/// `--vcl-boilerplate`'s prologue. vclpp lists VI02 to VI14 rather than
/// `.init_vi_all`, because openvcl gives VI01 to variables that FCAND,
/// FCEQ and FCOR overwrite. vcl 1.4beta7 keeps VI01 free across them
/// (observed, `vi01-n15-fc`), and so does the port, so all of VI01 to VI15
/// are allocable here.
const VCL_PROLOGUE: &str =
    "\n.init_vi_all\n.init_vf_all\n.syntax new\n.vu\n\n--enter\n--endenter\n\n";
const VCL_EPILOGUE: &str = "\n--exit\n--endexit\n\n";

fn ends_with_blank_line(text: &str) -> bool {
    text.ends_with("\n\n") || text == "\n"
}

impl Engine<'_> {
    /// Writes the code out as text, with the place each line came from. Each
    /// line keeps the indentation and spacing it was written with (a
    /// `#macro`'s body lines, those of its definition); comments and blank
    /// lines are gone, except for a blank line around each block macro
    /// expansion. A `#vuprog` name becomes VCL's `.name` directive.
    pub(super) fn write_output(&mut self) -> Fallible<(String, Vec<Option<Loc>>)> {
        let mut text = String::new();
        let mut origins: Vec<Option<Loc>> = Vec::new();
        let push_lines = |text: &mut String, origins: &mut Vec<Option<Loc>>, lines: &str| {
            text.push_str(lines);
            origins.resize(text.matches('\n').count(), None);
        };
        if !self.program_name.is_empty() {
            let name = format!("\n.name {}\n", self.program_name);
            push_lines(&mut text, &mut origins, &name);
        }
        if self.options.vcl_boilerplate {
            push_lines(&mut text, &mut origins, VCL_PROLOGUE);
        }
        let tokens = std::mem::take(&mut self.output);
        let mut tokens = tokens.into_iter().peekable();
        let mut blank_line_pending = false;
        while let Some(first) = tokens.next() {
            let mut line = vec![first];
            while let Some(next) = tokens.next_if(|next| !next.starts_line) {
                line.push(next);
            }
            if self.options.fold_constants {
                self.fold_constant_expressions(&mut line)?;
            }
            if self.options.flatten_subscripts {
                flatten_subscripts(&mut line);
            }
            if (blank_line_pending || line[0].blank_before)
                && !text.is_empty()
                && !ends_with_blank_line(&text)
            {
                push_lines(&mut text, &mut origins, "\n");
            }
            blank_line_pending = false;
            // The first token's leading whitespace is the line's indentation.
            for token in &line {
                text.push_str(&token.leading);
                text.push_str(&token.text);
                blank_line_pending = blank_line_pending || token.blank_after;
            }
            text.push('\n');
            origins.push(Some(line_origin(&line)));
        }
        if self.options.vcl_boilerplate {
            push_lines(&mut text, &mut origins, VCL_EPILOGUE);
        }
        Ok((text, origins))
    }

    /// Replaces the integer constant expressions on a line by their values,
    /// in decimal: `1010 + 0(vi00)` becomes `1010(vi00)`. An expression is
    /// only folded where it stands on its own: not when an operator touches
    /// it from either side (`a - 1 - 2` and `1 + 2 * a` are left alone), and
    /// it must apply at least one binary operator.
    fn fold_constant_expressions(&self, line: &mut Vec<Token>) -> Fallible<()> {
        let mut start = 0;
        while start < line.len() {
            let first = &line[start];
            let operator_first = is_unary_operator(first) || first.is(Punct::OpenParen);
            if !first.is_integer() && !operator_first {
                start += 1;
                continue;
            }
            if start > 0 {
                // An operator before it would take part, and so would a value
                // before a leading operator or parenthesis (`a -1 + 2` is
                // `(a - 1) + 2`).
                let before = &line[start - 1];
                if (before.kind == TokenKind::Punctuation && !is_separator(before))
                    || (operator_first && is_value(before))
                {
                    start += 1;
                    continue;
                }
            }
            let mut parser = ConstantParser {
                line,
                applies_binary_operator: false,
            };
            let end = parser.binary(start, 1);
            let Some(end) = end.filter(|_| parser.applies_binary_operator) else {
                start += 1;
                continue;
            };
            if let Some(after) = line.get(end)
                && after.kind == TokenKind::Punctuation
                && !is_separator(after)
            {
                start += 1;
                continue;
            }
            let value = self.evaluate(&line[start..end], &line[start], "constant expression")?;
            let mut folded = line[start].clone();
            folded.kind = TokenKind::Number;
            folded.punct = Punct::None;
            folded.is_float = false;
            folded.text = value.to_string();
            folded.blank_after = line[end - 1].blank_after;
            line.splice(start..end, [folded]);
            start += 1;
        }
        Ok(())
    }
}

/// Where a line of output came from: the line a token on it was written on,
/// or for a line that is all a block macro's body, the body's line.
fn line_origin(line: &[Token]) -> Loc {
    line.iter()
        .find(|token| token.origin.is_none())
        .unwrap_or(&line[0])
        .loc()
}

fn is_value(token: &Token) -> bool {
    matches!(token.kind, TokenKind::Identifier | TokenKind::Number)
        || token.is(Punct::CloseParen)
        || token.is(Punct::CloseBracket)
}

/// Punctuation an expression can stand next to without becoming part of
/// something larger.
fn is_separator(token: &Token) -> bool {
    [
        Punct::Comma,
        Punct::Colon,
        Punct::OpenParen,
        Punct::CloseParen,
        Punct::OpenBracket,
        Punct::CloseBracket,
        Punct::OpenBrace,
        Punct::CloseBrace,
    ]
    .iter()
    .any(|&punct| token.is(punct))
}

fn is_unary_operator(token: &Token) -> bool {
    token.is(Punct::Add) || token.is(Punct::Sub) || token.is(Punct::BitNot)
}

/// C's precedence for the binary operators `-x` folds; 0 for anything else.
fn binary_precedence(token: &Token) -> u8 {
    if token.kind != TokenKind::Punctuation {
        return 0;
    }
    match token.punct {
        Punct::Mul | Punct::Div | Punct::Mod => 6,
        Punct::Add | Punct::Sub => 5,
        Punct::ShiftLeft | Punct::ShiftRight => 4,
        Punct::BitAnd => 3,
        Punct::BitXor => 2,
        Punct::BitOr => 1,
        _ => 0,
    }
}

/// Finds the longest integer constant expression starting at a position of
/// a line.
struct ConstantParser<'a> {
    line: &'a [Token],
    applies_binary_operator: bool,
}

impl ConstantParser<'_> {
    fn unary(&mut self, position: usize) -> Option<usize> {
        let token = self.line.get(position)?;
        if token.is_integer() {
            return Some(position + 1);
        }
        if is_unary_operator(token) {
            return self.unary(position + 1);
        }
        if token.is(Punct::OpenParen) {
            let end = self.binary(position + 1, 1)?;
            if self
                .line
                .get(end)
                .is_some_and(|close| close.is(Punct::CloseParen))
            {
                return Some(end + 1);
            }
        }
        None
    }

    fn binary(&mut self, position: usize, min_precedence: u8) -> Option<usize> {
        let mut end = self.unary(position)?;
        while let Some(operator) = self.line.get(end) {
            let precedence = binary_precedence(operator);
            if precedence == 0 || precedence < min_precedence {
                break;
            }
            // An operator without an operand after it is not part of the
            // expression.
            let applied = self.applies_binary_operator;
            let Some(right_end) = self.binary(end + 1, precedence + 1) else {
                self.applies_binary_operator = applied;
                break;
            };
            end = right_end;
            self.applies_binary_operator = true;
        }
        Some(end)
    }
}

/// `--flatten-subscripts`: each subscript of one digit or one of w, x, y and
/// z, written with no spaces inside the brackets, becomes a suffix. `name[0]`
/// turns into `name_0` and `name[x]` into `namex`, as the sed that ps2stuff's
/// build ran after masp did: `s/\[\([0-9]\)\]/_\1/g` and
/// `s/\[\([w-zW-Z]\)\]/\1/g`.
fn flatten_subscripts(line: &mut Vec<Token>) {
    let mut flat = Vec::with_capacity(line.len());
    let mut index = 0;
    while index < line.len() {
        if index + 2 < line.len()
            && line[index].is(Punct::OpenBracket)
            && line[index + 1].leading.is_empty()
            && line[index + 2].leading.is_empty()
            && line[index + 2].is(Punct::CloseBracket)
            && line[index + 1].text.len() == 1
        {
            let inside = &line[index + 1];
            let c = inside.text.chars().next().unwrap();
            let suffix = if inside.kind == TokenKind::Number && c.is_ascii_digit() {
                Some(format!("_{c}"))
            } else if inside.is_identifier() && "wxyzWXYZ".contains(c) {
                Some(c.to_string())
            } else {
                None
            };
            if let Some(suffix) = suffix {
                // The suffix keeps the `[`'s place, and the `]`'s blank line
                // after.
                let mut token = line[index].clone();
                token.kind = TokenKind::Identifier;
                token.punct = Punct::None;
                token.text = suffix;
                token.blank_after = token.blank_after || line[index + 2].blank_after;
                flat.push(token);
                index += 3;
                continue;
            }
        }
        flat.push(line[index].clone());
        index += 1;
    }
    *line = flat;
}
