//! VUSL's tokens, read from the preprocessed program. The preprocessor has
//! removed comments and directives, except the `#pragma` lines it keeps for
//! the frontend: each becomes a [`TokenKind::PragmaStart`], the pragma's own
//! tokens, and a [`TokenKind::PragmaEnd`] where its line ends.
//!
//! Numbers:
//!
//! - integers are decimal (`0` or a digit 1-9 and more digits), hexadecimal
//!   (`0x`) or binary (`0b`); a leading `0` before more digits is rejected,
//!   since C would read it as octal;
//! - a float has a `.` or an exponent (`1.0`, `.5`, `2.`, `1e-20`, `3.0e5`)
//!   and may end in `f`; its value is the nearest `f32`.

use std::fmt;

/// A place in the preprocessed text: its line and column, both from 1.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Pos {
    pub line: u32,
    pub column: u32,
}

/// Punctuation and operators, longest first in [`PUNCTUATION`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Punct {
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    Comma,
    Semicolon,
    Dot,
    /// `::`, which names an enum's value: `ClipPlane::Near`.
    Scope,
    Colon,
    Question,
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    AndAssign,
    OrAssign,
    XorAssign,
    ShlAssign,
    ShrAssign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    Not,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    LogicAnd,
    LogicOr,
    Increment,
    Decrement,
    Shl,
    Shr,
}

const PUNCTUATION: [(&str, Punct); 45] = [
    ("<<=", Punct::ShlAssign),
    (">>=", Punct::ShrAssign),
    ("+=", Punct::AddAssign),
    ("-=", Punct::SubAssign),
    ("*=", Punct::MulAssign),
    ("/=", Punct::DivAssign),
    ("%=", Punct::ModAssign),
    ("&=", Punct::AndAssign),
    ("|=", Punct::OrAssign),
    ("^=", Punct::XorAssign),
    ("<=", Punct::LessEqual),
    (">=", Punct::GreaterEqual),
    ("==", Punct::Equal),
    ("!=", Punct::NotEqual),
    ("&&", Punct::LogicAnd),
    ("||", Punct::LogicOr),
    ("++", Punct::Increment),
    ("--", Punct::Decrement),
    ("<<", Punct::Shl),
    (">>", Punct::Shr),
    ("::", Punct::Scope),
    ("(", Punct::OpenParen),
    (")", Punct::CloseParen),
    ("[", Punct::OpenBracket),
    ("]", Punct::CloseBracket),
    ("{", Punct::OpenBrace),
    ("}", Punct::CloseBrace),
    (",", Punct::Comma),
    (";", Punct::Semicolon),
    (".", Punct::Dot),
    (":", Punct::Colon),
    ("?", Punct::Question),
    ("=", Punct::Assign),
    ("+", Punct::Add),
    ("-", Punct::Sub),
    ("*", Punct::Mul),
    ("/", Punct::Div),
    ("%", Punct::Mod),
    ("&", Punct::BitAnd),
    ("|", Punct::BitOr),
    ("^", Punct::BitXor),
    ("~", Punct::BitNot),
    ("!", Punct::Not),
    ("<", Punct::Less),
    (">", Punct::Greater),
];

impl Punct {
    /// How the punctuation is written.
    pub fn spelling(self) -> &'static str {
        PUNCTUATION
            .iter()
            .find(|(_, punct)| *punct == self)
            .map_or("?", |(text, _)| text)
    }
}

impl fmt::Display for Punct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.spelling())
    }
}

/// What a token is.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Ident,
    Int(i64),
    Float(f32),
    Punct(Punct),
    /// `#pragma` at the start of a line.
    PragmaStart,
    /// The end of a `#pragma`'s line.
    PragmaEnd,
    /// The end of the text.
    End,
}

/// One token: what it is, as written, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub pos: Pos,
}

impl Token {
    pub fn is(&self, punct: Punct) -> bool {
        self.kind == TokenKind::Punct(punct)
    }

    pub fn is_ident(&self, name: &str) -> bool {
        self.kind == TokenKind::Ident && self.text == name
    }
}

/// Why the text is not a list of tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LexErrorKind {
    /// A character no token starts with.
    UnknownCharacter(char),
    /// A malformed number, as written.
    BadNumber(String),
    /// An integer with a leading zero, which C reads as octal.
    OctalNumber(String),
    /// An integer too large for any VUSL type.
    NumberTooLarge(String),
    /// A `#` that does not start a `#pragma` line.
    StrayHash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexError {
    pub kind: LexErrorKind,
    pub pos: Pos,
}

/// Splits the preprocessed program into tokens, ending with
/// [`TokenKind::End`].
pub fn tokenize(text: &str) -> Result<Vec<Token>, LexError> {
    let mut lexer = Lexer {
        bytes: text.as_bytes(),
        text,
        at: 0,
        line: 1,
        line_start: 0,
    };
    let mut tokens = Vec::new();
    let mut in_pragma = false;
    loop {
        // Whitespace, and the end of a pragma's line.
        while lexer.at < lexer.bytes.len() && lexer.bytes[lexer.at].is_ascii_whitespace() {
            if lexer.bytes[lexer.at] == b'\n' {
                if in_pragma {
                    tokens.push(lexer.token(TokenKind::PragmaEnd, lexer.at, lexer.at));
                    in_pragma = false;
                }
                lexer.line += 1;
                lexer.line_start = lexer.at + 1;
            }
            lexer.at += 1;
        }
        if lexer.at >= lexer.bytes.len() {
            if in_pragma {
                tokens.push(lexer.token(TokenKind::PragmaEnd, lexer.at, lexer.at));
            }
            tokens.push(lexer.token(TokenKind::End, lexer.at, lexer.at));
            return Ok(tokens);
        }
        let start = lexer.at;
        let c = lexer.bytes[start];
        if c == b'#' {
            // Only a pragma line reaches the frontend with a `#`.
            let first_on_line = lexer.text[lexer.line_start..start].trim().is_empty();
            lexer.at += 1;
            while lexer.at < lexer.bytes.len() && matches!(lexer.bytes[lexer.at], b' ' | b'\t') {
                lexer.at += 1;
            }
            let word_start = lexer.at;
            while lexer.at < lexer.bytes.len() && lexer.bytes[lexer.at].is_ascii_alphanumeric() {
                lexer.at += 1;
            }
            if !first_on_line || in_pragma || &lexer.text[word_start..lexer.at] != "pragma" {
                return Err(LexError {
                    kind: LexErrorKind::StrayHash,
                    pos: lexer.pos(start),
                });
            }
            tokens.push(lexer.token(TokenKind::PragmaStart, start, lexer.at));
            in_pragma = true;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while lexer.at < lexer.bytes.len()
                && (lexer.bytes[lexer.at].is_ascii_alphanumeric() || lexer.bytes[lexer.at] == b'_')
            {
                lexer.at += 1;
            }
            tokens.push(lexer.token(TokenKind::Ident, start, lexer.at));
        } else if c.is_ascii_digit()
            || (c == b'.' && lexer.bytes.get(start + 1).is_some_and(u8::is_ascii_digit))
        {
            let kind = lexer.number(start)?;
            tokens.push(lexer.token(kind, start, lexer.at));
        } else if let Some((spelling, punct)) = PUNCTUATION
            .iter()
            .find(|(spelling, _)| lexer.text[start..].starts_with(spelling))
        {
            lexer.at += spelling.len();
            tokens.push(lexer.token(TokenKind::Punct(*punct), start, lexer.at));
        } else {
            let character = lexer.text[start..].chars().next().unwrap_or('\0');
            return Err(LexError {
                kind: LexErrorKind::UnknownCharacter(character),
                pos: lexer.pos(start),
            });
        }
    }
}

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    line: u32,
    line_start: usize,
}

impl Lexer<'_> {
    fn pos(&self, at: usize) -> Pos {
        Pos {
            line: self.line,
            column: u32::try_from(self.text[self.line_start..at].chars().count() + 1)
                .unwrap_or(u32::MAX),
        }
    }

    fn token(&self, kind: TokenKind, start: usize, end: usize) -> Token {
        Token {
            kind,
            text: self.text[start..end].to_string(),
            pos: self.pos(start),
        }
    }

    /// Moves past a number starting at `start`.
    fn number(&mut self, start: usize) -> Result<TokenKind, LexError> {
        let bytes = self.bytes;
        let digits = |at: &mut usize, accept: fn(u8) -> bool| {
            while *at < bytes.len() && accept(bytes[*at]) {
                *at += 1;
            }
        };
        let mut at = start;
        let based =
            bytes[start] == b'0' && matches!(bytes.get(start + 1), Some(b'x' | b'X' | b'b' | b'B'));
        let kind = if based {
            let hex = matches!(bytes[start + 1], b'x' | b'X');
            at += 2;
            let digits_start = at;
            if hex {
                digits(&mut at, |c| c.is_ascii_hexdigit());
            } else {
                digits(&mut at, |c| c == b'0' || c == b'1');
            }
            let radix = if hex { 16 } else { 2 };
            let value = i64::from_str_radix(&self.text[digits_start..at], radix);
            self.at = at;
            self.check_end(start)?;
            match value {
                Ok(value) => TokenKind::Int(value),
                Err(_) if at == digits_start => return Err(self.bad_number(start)),
                Err(_) => {
                    return Err(LexError {
                        kind: LexErrorKind::NumberTooLarge(self.text[start..at].into()),
                        pos: self.pos(start),
                    });
                }
            }
        } else {
            digits(&mut at, |c| c.is_ascii_digit());
            let mut float = false;
            if bytes.get(at) == Some(&b'.') {
                float = true;
                at += 1;
                digits(&mut at, |c| c.is_ascii_digit());
            }
            if matches!(bytes.get(at), Some(b'e' | b'E')) {
                float = true;
                at += 1;
                if matches!(bytes.get(at), Some(b'+' | b'-')) {
                    at += 1;
                }
                let exponent = at;
                digits(&mut at, |c| c.is_ascii_digit());
                if at == exponent {
                    self.at = at;
                    return Err(self.bad_number(start));
                }
            }
            let text = &self.text[start..at];
            if float && matches!(bytes.get(at), Some(b'f' | b'F')) {
                at += 1;
            }
            self.at = at;
            self.check_end(start)?;
            if float {
                match text.parse::<f32>() {
                    Ok(value) if value.is_finite() => TokenKind::Float(value),
                    _ => return Err(self.bad_number(start)),
                }
            } else if text.len() > 1 && text.starts_with('0') {
                return Err(LexError {
                    kind: LexErrorKind::OctalNumber(text.into()),
                    pos: self.pos(start),
                });
            } else {
                match text.parse::<i64>() {
                    Ok(value) => TokenKind::Int(value),
                    Err(_) => {
                        return Err(LexError {
                            kind: LexErrorKind::NumberTooLarge(text.into()),
                            pos: self.pos(start),
                        });
                    }
                }
            }
        };
        Ok(kind)
    }

    /// A number runs into a letter, digit or `.` only when it is malformed:
    /// `1x`, `0x1g`, `1.2.3`. `start` is where the number starts.
    fn check_end(&mut self, start: usize) -> Result<(), LexError> {
        let mut at = self.at;
        while at < self.bytes.len()
            && (self.bytes[at].is_ascii_alphanumeric() || matches!(self.bytes[at], b'_' | b'.'))
        {
            at += 1;
        }
        if at == self.at {
            return Ok(());
        }
        self.at = at;
        Err(self.bad_number(start))
    }

    fn bad_number(&self, start: usize) -> LexError {
        LexError {
            kind: LexErrorKind::BadNumber(self.text[start..self.at.max(start + 1)].into()),
            pos: self.pos(start),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        tokenize(text)
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    fn error(text: &str) -> LexErrorKind {
        tokenize(text).unwrap_err().kind
    }

    #[test]
    fn names_numbers_and_operators() {
        use TokenKind::*;
        assert_eq!(
            kinds("vec4 c = a.xyz * 2.0 + 0x10;"),
            [
                Ident,
                Ident,
                Punct(super::Punct::Assign),
                Ident,
                Punct(super::Punct::Dot),
                Ident,
                Punct(super::Punct::Mul),
                Float(2.0),
                Punct(super::Punct::Add),
                Int(16),
                Punct(super::Punct::Semicolon),
                End
            ]
        );
        assert_eq!(kinds("a<<=b")[1], Punct(super::Punct::ShlAssign));
        assert_eq!(kinds("++a")[0], Punct(super::Punct::Increment));
    }

    #[test]
    fn number_forms() {
        use TokenKind::*;
        for (text, kind) in [
            ("0", Int(0)),
            ("32767", Int(32767)),
            ("0x7FFF", Int(0x7fff)),
            ("0b101", Int(5)),
            ("1.0", Float(1.0)),
            (".5", Float(0.5)),
            ("2.", Float(2.0)),
            ("1e-20", Float(1e-20)),
            ("3.0e5f", Float(3.0e5)),
        ] {
            assert_eq!(kinds(text)[0], kind, "{text}");
        }
        assert_eq!(error("017"), LexErrorKind::OctalNumber("017".into()));
        assert_eq!(error("1x"), LexErrorKind::BadNumber("1x".into()));
        assert_eq!(error("1.2.3"), LexErrorKind::BadNumber("1.2.3".into()));
        assert_eq!(error("1e"), LexErrorKind::BadNumber("1e".into()));
        assert_eq!(error("0x"), LexErrorKind::BadNumber("0x".into()));
        assert_eq!(error("1e99"), LexErrorKind::BadNumber("1e99".into()));
        assert_eq!(
            error("99999999999999999999"),
            LexErrorKind::NumberTooLarge("99999999999999999999".into())
        );
    }

    #[test]
    fn pragma_lines_are_bracketed() {
        use TokenKind::*;
        let tokens = tokenize("a;\n    #pragma unroll(4)\nb;\n#pragma barrier").unwrap();
        let kinds: Vec<&TokenKind> = tokens.iter().map(|token| &token.kind).collect();
        assert_eq!(
            kinds,
            [
                &Ident,
                &Punct(super::Punct::Semicolon),
                &PragmaStart,
                &Ident,
                &Punct(super::Punct::OpenParen),
                &Int(4),
                &Punct(super::Punct::CloseParen),
                &PragmaEnd,
                &Ident,
                &Punct(super::Punct::Semicolon),
                &PragmaStart,
                &Ident,
                &PragmaEnd,
                &End
            ]
        );
        assert_eq!(tokens[2].pos, Pos { line: 2, column: 5 });
        assert_eq!(tokens[3].text, "unroll");
        assert_eq!(error("a # b"), LexErrorKind::StrayHash);
        assert_eq!(error("#define A"), LexErrorKind::StrayHash);
        assert_eq!(error("a $"), LexErrorKind::UnknownCharacter('$'));
    }
}
