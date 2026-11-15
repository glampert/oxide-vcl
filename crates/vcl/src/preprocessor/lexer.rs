//! The preprocessor's tokens: source text split into identifiers, numbers,
//! strings, character literals and punctuation, each keeping its exact
//! spelling and the whitespace before it, so that the output can be written
//! back as it was laid out.
//!
//! The rules are vclpp's (observed from vclpp 2, run on single tokens):
//!
//! - `;`, `//` and `/* */` start comments. A comment is dropped: between two
//!   tokens on a line it counts as one space, and before the first token of a
//!   line it turns into spaces, so the token keeps its column. A `/* */`
//!   comment that spans lines does not end the line it is in.
//! - Strings (`"..."`) and character literals (`'.'`) are kept as written:
//!   a backslash is an ordinary character, so `"a\"` is a whole string. A
//!   character literal holds at most one character.
//! - Numbers:
//!   - `0x` is followed by hex digits and `0b` by binary digits, either with
//!     none at all.
//!   - A `0` followed by anything but `.` is octal: the `0` and the decimal
//!     digits after it. So `0e5` is `0` followed by `e5`, and `00.5` is `00`
//!     followed by `.5`.
//!   - Otherwise a number is digits with at most one `.` (it may start with
//!     the `.`), then an optional exponent: `e`, an optional sign, and digits,
//!     which may be absent. A dot or an exponent makes it a float.
//!   - A float takes one `f`/`l` suffix and an integer any run of `u`/`l`, in
//!     either case.
//!   - A `#` straight after a float's fraction is a floating-point exception
//!     (`1.#INF`), which is an error.
//! - Punctuation is the longest of the C operators, plus `#`, `##`, `$`, `\`
//!   and `...`. A character no token starts with, such as `@` or a backtick,
//!   is a token of its own.
//! - A `\` that ends a line joins the next line to it.

use std::fmt;
use std::rc::Rc;

/// What a token is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind {
    Identifier,
    Number,
    String,
    CharLiteral,
    Punctuation,
}

/// A punctuation token's meaning; `None` for any other token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Punct {
    None,
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    ShiftRight,
    ShiftLeft,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    ShiftRightAssign,
    ShiftLeftAssign,
    LogicAnd,
    LogicOr,
    LogicNot,
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
    PlusPlus,
    MinusMinus,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    Dot,
    Arrow,
    ColonColon,
    DotStar,
    Comma,
    Semicolon,
    Colon,
    Question,
    Ellipsis,
    Backslash,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    Hash,
    HashHash,
    Dollar,
    /// A character no other token starts with, kept as written.
    Other,
}

/// Every punctuation spelling, longest first, so the first that matches is
/// the longest.
const PUNCTUATION: [(&str, Punct); 52] = [
    (">>=", Punct::ShiftRightAssign),
    ("<<=", Punct::ShiftLeftAssign),
    ("...", Punct::Ellipsis),
    (">>", Punct::ShiftRight),
    ("<<", Punct::ShiftLeft),
    ("+=", Punct::AddAssign),
    ("-=", Punct::SubAssign),
    ("*=", Punct::MulAssign),
    ("/=", Punct::DivAssign),
    ("%=", Punct::ModAssign),
    ("&&", Punct::LogicAnd),
    ("||", Punct::LogicOr),
    ("==", Punct::Equal),
    ("!=", Punct::NotEqual),
    (">=", Punct::GreaterEqual),
    ("<=", Punct::LessEqual),
    ("++", Punct::PlusPlus),
    ("--", Punct::MinusMinus),
    ("&=", Punct::BitAndAssign),
    ("|=", Punct::BitOrAssign),
    ("^=", Punct::BitXorAssign),
    ("->", Punct::Arrow),
    ("::", Punct::ColonColon),
    (".*", Punct::DotStar),
    ("##", Punct::HashHash),
    ("=", Punct::Assign),
    ("+", Punct::Add),
    ("-", Punct::Sub),
    ("*", Punct::Mul),
    ("/", Punct::Div),
    ("%", Punct::Mod),
    ("!", Punct::LogicNot),
    (">", Punct::Greater),
    ("<", Punct::Less),
    ("&", Punct::BitAnd),
    ("|", Punct::BitOr),
    ("^", Punct::BitXor),
    ("~", Punct::BitNot),
    (".", Punct::Dot),
    (",", Punct::Comma),
    (";", Punct::Semicolon),
    (":", Punct::Colon),
    ("?", Punct::Question),
    ("\\", Punct::Backslash),
    ("(", Punct::OpenParen),
    (")", Punct::CloseParen),
    ("[", Punct::OpenBracket),
    ("]", Punct::CloseBracket),
    ("{", Punct::OpenBrace),
    ("}", Punct::CloseBrace),
    ("#", Punct::Hash),
    ("$", Punct::Dollar),
];

/// A place in one of the preprocessor's files: an index into its file table.
/// Lines and columns count from 1; 0 means unknown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Loc {
    pub file: usize,
    pub line: usize,
    pub column: usize,
}

/// The macro invocation a token came out of, so that diagnostics can show
/// the chain of expansions behind it. All the tokens of one expansion share
/// one.
#[derive(Debug, Eq, PartialEq)]
pub struct Origin {
    pub macro_name: String,
    pub invoked_at: Loc,
    /// The expansion the invocation itself came out of, if any.
    pub parent: Option<Rc<Origin>>,
}

/// One token, as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// For [`TokenKind::Punctuation`].
    pub punct: Punct,
    /// A floating-point number.
    pub is_float: bool,
    /// The first token of a line.
    pub starts_line: bool,
    /// Set off from the line before by a blank line.
    pub blank_before: bool,
    /// Set off from the line after by a blank line.
    pub blank_after: bool,
    /// The exact spelling.
    pub text: String,
    /// The whitespace before it: its indentation when it starts a line.
    pub leading: String,
    /// Where it is: its file, and its line and column (in bytes) from 1.
    pub file: usize,
    pub line: usize,
    pub column: usize,
    /// The macros it must not be expanded as again (C's hide set), as
    /// interned name ids: sorted, without duplicates.
    pub hidden: Vec<u32>,
    /// The expansion it came out of; `None` for a token read from a file.
    pub origin: Option<Rc<Origin>>,
}

impl Token {
    pub fn is(&self, punct: Punct) -> bool {
        self.kind == TokenKind::Punctuation && self.punct == punct
    }

    pub fn is_identifier(&self) -> bool {
        self.kind == TokenKind::Identifier
    }

    /// An integer number.
    pub fn is_integer(&self) -> bool {
        self.kind == TokenKind::Number && !self.is_float
    }

    pub fn loc(&self) -> Loc {
        Loc {
            file: self.file,
            line: self.line,
            column: self.column,
        }
    }

    /// Where the token was used: its own place, or for a token out of a
    /// macro expansion, where the outermost macro was invoked.
    pub fn use_loc(&self) -> Loc {
        let mut origin = match &self.origin {
            Some(origin) => origin,
            None => return self.loc(),
        };
        while let Some(parent) = &origin.parent {
            origin = parent;
        }
        origin.invoked_at
    }
}

/// Why text does not split into tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LexErrorKind {
    /// A string or character literal that a line ends inside.
    NewlineInString,
    /// A string or character literal that the text ends inside.
    MissingClosingQuote,
    /// A `/* */` comment that the text ends inside.
    UnterminatedComment,
    /// A character literal of more than one character.
    LongCharLiteral,
    /// A number with a second `.` before its exponent.
    TwoDots,
    /// `1.#INF`, `1.#IND`, `1.#NAN`, `1.#QNAN` or `1.#SNAN`.
    FloatException { number: String },
    /// A `#` after a float's fraction that names no exception: `number` is
    /// the float before it.
    UnknownFloatException { number: String },
}

impl fmt::Display for LexErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexErrorKind::NewlineInString => f.write_str("newline inside string"),
            LexErrorKind::MissingClosingQuote => f.write_str("missing trailing quote"),
            LexErrorKind::UnterminatedComment => {
                f.write_str("end of file inside a C-style multi-line comment")
            }
            LexErrorKind::LongCharLiteral => f.write_str("char literal is not one character long"),
            LexErrorKind::TwoDots => f.write_str("more than one dot in number"),
            LexErrorKind::FloatException { number } => {
                write!(f, "floating-point exception scanned: {number}")
            }
            LexErrorKind::UnknownFloatException { number } => write!(
                f,
                "unknown floating-point exception after '{number}'; expected #INF, #IND, \
                 #NAN, #QNAN or #SNAN"
            ),
        }
    }
}

/// A lexical error and the line it is on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexError {
    pub line: usize,
    pub kind: LexErrorKind,
}

/// A lexical warning and the line it is on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexWarning {
    pub line: usize,
    pub kind: LexWarningKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LexWarningKind {
    /// A `/*` inside a `/* */` comment.
    NestedComment,
}

impl fmt::Display for LexWarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexWarningKind::NestedComment => f.write_str("nested C-style multi-line comment"),
        }
    }
}

/// The tokens of a text and the warnings found while splitting it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub warnings: Vec<LexWarning>,
}

/// Splits `text`, from file `file`, into tokens. Line numbers count from
/// `first_line`, the line of the file the text starts on. A `\` that ends a
/// line joins it to the next.
pub fn tokenize(text: &str, file: usize, first_line: usize) -> Result<Lexed, LexError> {
    let mut lexer = Lexer::new(text, first_line);
    lexer.file = file;
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token()? {
        tokens.push(token);
    }
    Ok(Lexed {
        tokens: join_spliced_lines(tokens),
        warnings: lexer.warnings,
    })
}

/// The token `text` is, if it is exactly one valid token with nothing
/// around it: the result of a `##` paste.
pub fn lex_single_token(text: &str) -> Option<Token> {
    if text.is_empty() {
        return None;
    }
    let mut lexer = Lexer::new(text, 1);
    let token = lexer.next_token().ok()??;
    let alone = token.leading.is_empty()
        && lexer.position == text.len()
        && lexer.warnings.is_empty()
        && !(token.column != 1 || token.line != 1);
    alone.then_some(token)
}

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    position: usize,
    line: usize,
    /// Where the current line starts, for columns.
    line_start: usize,
    tokens: usize,
    file: usize,
    warnings: Vec<LexWarning>,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, first_line: usize) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            position: 0,
            line: first_line,
            line_start: 0,
            tokens: 0,
            file: 0,
            warnings: Vec::new(),
        }
    }

    fn peek(&self, offset: usize) -> u8 {
        self.bytes.get(self.position + offset).copied().unwrap_or(0)
    }

    fn error(&self, kind: LexErrorKind) -> LexError {
        LexError {
            line: self.line,
            kind,
        }
    }

    /// Moves past one byte, counting lines.
    fn advance(&mut self) {
        if self.bytes[self.position] == b'\n' {
            self.line += 1;
            self.line_start = self.position + 1;
        }
        self.position += 1;
    }

    /// Skips whitespace and comments.
    fn skip_gap(&mut self) -> Result<(), LexError> {
        while self.position < self.bytes.len() {
            let c = self.peek(0);
            if c <= b' ' {
                self.advance();
            } else if c == b';' || (c == b'/' && self.peek(1) == b'/') {
                while self.position < self.bytes.len() && self.peek(0) != b'\n' {
                    self.advance();
                }
            } else if c == b'/' && self.peek(1) == b'*' {
                self.position += 2;
                loop {
                    if self.position >= self.bytes.len() {
                        return Err(self.error(LexErrorKind::UnterminatedComment));
                    }
                    if self.peek(0) == b'*' && self.peek(1) == b'/' {
                        self.position += 2;
                        break;
                    }
                    if self.peek(0) == b'/' && self.peek(1) == b'*' {
                        self.warnings.push(LexWarning {
                            line: self.line,
                            kind: LexWarningKind::NestedComment,
                        });
                    }
                    self.advance();
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    fn next_token(&mut self) -> Result<Option<Token>, LexError> {
        let gap_start = self.position;
        self.skip_gap()?;
        if self.position >= self.bytes.len() {
            return Ok(None);
        }
        let gap = read_gap(&self.text[gap_start..self.position]);
        let start = self.position;
        let line = self.line;
        let column = start - self.line_start + 1;
        let (kind, punct, is_float) = self.scan()?;
        let token = Token {
            kind,
            punct,
            is_float,
            starts_line: self.tokens == 0 || gap.line_break,
            blank_before: false,
            blank_after: false,
            text: self.text[start..self.position].to_string(),
            leading: gap.leading,
            file: self.file,
            line,
            column,
            hidden: Vec::new(),
            origin: None,
        };
        self.tokens += 1;
        Ok(Some(token))
    }

    /// Moves past one token, returning what it is.
    fn scan(&mut self) -> Result<(TokenKind, Punct, bool), LexError> {
        let c = self.peek(0);
        if c.is_ascii_alphabetic() || c == b'_' {
            while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                self.position += 1;
            }
            return Ok((TokenKind::Identifier, Punct::None, false));
        }
        if c.is_ascii_digit() || (c == b'.' && self.peek(1).is_ascii_digit()) {
            let is_float = self.scan_number()?;
            return Ok((TokenKind::Number, Punct::None, is_float));
        }
        if c == b'"' || c == b'\'' {
            self.scan_quoted(c)?;
            let kind = if c == b'"' {
                TokenKind::String
            } else {
                TokenKind::CharLiteral
            };
            return Ok((kind, Punct::None, false));
        }
        let rest = &self.text[self.position..];
        if let Some((spelling, punct)) = PUNCTUATION
            .iter()
            .find(|(spelling, _)| rest.starts_with(spelling))
        {
            self.position += spelling.len();
            return Ok((TokenKind::Punctuation, *punct, false));
        }
        // Any other character, whole even when it takes several bytes.
        let width = rest.chars().next().map_or(1, char::len_utf8);
        self.position += width;
        Ok((TokenKind::Punctuation, Punct::Other, false))
    }

    /// Moves past a number, returning whether it is a float.
    fn scan_number(&mut self) -> Result<bool, LexError> {
        let start = self.position;
        let prefix = (self.peek(0), self.peek(1).to_ascii_lowercase());
        if prefix == (b'0', b'x') {
            self.position += 2;
            while self.peek(0).is_ascii_hexdigit() {
                self.position += 1;
            }
            self.integer_suffix();
            return Ok(false);
        }
        if prefix == (b'0', b'b') {
            self.position += 2;
            while matches!(self.peek(0), b'0' | b'1') {
                self.position += 1;
            }
            self.integer_suffix();
            return Ok(false);
        }
        if prefix.0 == b'0' && prefix.1 != b'.' {
            self.position += 1;
            while self.peek(0).is_ascii_digit() {
                self.position += 1;
            }
            self.integer_suffix();
            return Ok(false);
        }
        let mut is_float = false;
        loop {
            let c = self.peek(0);
            if c.is_ascii_digit() {
                self.position += 1;
            } else if c == b'.' {
                if is_float {
                    return Err(self.error(LexErrorKind::TwoDots));
                }
                is_float = true;
                self.position += 1;
            } else {
                break;
            }
        }
        if is_float && self.peek(0) == b'#' {
            let number = self.text[start..self.position].to_string();
            let rest = &self.text[self.position + 1..];
            let named = ["INF", "IND", "NAN", "QNAN", "SNAN"]
                .iter()
                .find(|name| rest.starts_with(*name));
            return Err(self.error(match named {
                Some(name) => LexErrorKind::FloatException {
                    number: format!("{number}#{name}"),
                },
                None => LexErrorKind::UnknownFloatException { number },
            }));
        }
        if matches!(self.peek(0), b'e' | b'E') {
            is_float = true;
            self.position += 1;
            if matches!(self.peek(0), b'+' | b'-') {
                self.position += 1;
            }
            while self.peek(0).is_ascii_digit() {
                self.position += 1;
            }
        }
        if is_float {
            if matches!(self.peek(0), b'f' | b'F' | b'l' | b'L') {
                self.position += 1;
            }
        } else {
            self.integer_suffix();
        }
        Ok(is_float)
    }

    fn integer_suffix(&mut self) {
        while matches!(self.peek(0), b'u' | b'U' | b'l' | b'L') {
            self.position += 1;
        }
    }

    /// Moves past a string or character literal, quotes included.
    fn scan_quoted(&mut self, quote: u8) -> Result<(), LexError> {
        let content_start = self.position + 1;
        self.position += 1;
        loop {
            match self.bytes.get(self.position) {
                None => return Err(self.error(LexErrorKind::MissingClosingQuote)),
                Some(b'\n') => return Err(self.error(LexErrorKind::NewlineInString)),
                Some(&c) if c == quote => break,
                Some(_) => self.position += 1,
            }
        }
        let content = &self.text[content_start..self.position];
        self.position += 1;
        if quote == b'\'' && content.chars().count() > 1 {
            return Err(self.error(LexErrorKind::LongCharLiteral));
        }
        Ok(())
    }
}

/// The text between two tokens: whitespace, maybe with comments in it.
struct GapLayout {
    /// The next token starts a new line.
    line_break: bool,
    /// The whitespace the next token keeps.
    leading: String,
}

fn read_gap(gap: &str) -> GapLayout {
    // A newline only ends the line outside a comment: a `/* */` comment that
    // spans lines does not split the line it is in. Line comments stop at
    // their newline.
    let bytes = gap.as_bytes();
    let mut line_break = false;
    let mut last_line_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if gap[i..].starts_with("/*") {
            i = gap[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |end| i + 2 + end + 2);
        } else if gap[i..].starts_with("//") || bytes[i] == b';' {
            i = gap[i..].find('\n').map_or(bytes.len(), |end| i + end);
        } else {
            if bytes[i] == b'\n' {
                line_break = true;
                last_line_start = i + 1;
            }
            i += 1;
        }
    }
    let last_line = &gap[last_line_start..];
    let leading = if line_break {
        // A comment in front of the token turns into spaces, so the token
        // stays in its column.
        last_line
            .chars()
            .map(|c| if c == ' ' || c == '\t' { c } else { ' ' })
            .collect()
    } else if last_line.chars().all(|c| c == ' ' || c == '\t') {
        last_line.to_string()
    } else {
        // A comment between two tokens on a line separates them like a space.
        " ".to_string()
    };
    GapLayout {
        line_break,
        leading,
    }
}

/// A `\` that ends a line joins the next line to it.
fn join_spliced_lines(tokens: Vec<Token>) -> Vec<Token> {
    let mut joined: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut tokens = tokens.into_iter().peekable();
    while let Some(token) = tokens.next() {
        let ends_line = tokens.peek().is_none_or(|next| next.starts_line);
        if token.is(Punct::Backslash) && ends_line {
            if let Some(next) = tokens.peek_mut() {
                next.starts_line = token.starts_line;
                next.leading = if token.starts_line {
                    token.leading.clone()
                } else {
                    " ".to_string()
                };
            }
            continue;
        }
        joined.push(token);
    }
    joined
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<String> {
        tokenize(text, 0, 1)
            .unwrap()
            .tokens
            .into_iter()
            .map(|token| token.text)
            .collect()
    }

    fn error(text: &str) -> LexErrorKind {
        tokenize(text, 0, 1).unwrap_err().kind
    }

    #[test]
    fn numbers_end_where_their_form_does() {
        // Observed from vclpp 2: a macro named by the letters after each
        // number shows where the number ends.
        for (text, split) in [
            ("0x1fg", &["0x1f", "g"][..]),
            ("0x1Fu", &["0x1Fu"]),
            ("0x1ful", &["0x1ful"]),
            ("0x", &["0x"]),
            ("0xg", &["0x", "g"]),
            ("0b101b", &["0b101", "b"]),
            ("0b1u", &["0b1u"]),
            ("07x", &["07", "x"]),
            ("0e5", &["0", "e5"]),
            ("00.5", &["00", ".5"]),
            ("08", &["08"]),
            ("019", &["019"]),
            ("0_a", &["0", "_a"]),
            ("07u", &["07u"]),
            ("0.5", &["0.5"]),
            ("0.e5", &["0.e5"]),
            ("12f", &["12", "f"]),
            ("12g", &["12", "g"]),
            ("3ul", &["3ul"]),
            ("3uu", &["3uu"]),
            ("2l", &["2l"]),
            ("1.5f", &["1.5f"]),
            ("1.5ff", &["1.5f", "f"]),
            ("1.5fl", &["1.5f", "l"]),
            ("1.5lf", &["1.5l", "f"]),
            ("1e5", &["1e5"]),
            ("1e5f", &["1e5f"]),
            ("1E5", &["1E5"]),
            ("1e", &["1e"]),
            ("7ex", &["7e", "x"]),
            ("1.5e+3x", &["1.5e+3", "x"]),
            ("1.5e-x", &["1.5e-", "x"]),
            ("1.e5", &["1.e5"]),
            ("5.", &["5."]),
            (".5", &[".5"]),
            (".5g", &[".5", "g"]),
            ("..5", &[".", ".5"]),
            ("1.5e5.5", &["1.5e5", ".5"]),
            ("1.5e#", &["1.5e", "#"]),
            ("5#", &["5", "#"]),
            ("9a_", &["9", "a_"]),
        ] {
            assert_eq!(texts(text), split, "{text}");
        }
    }

    #[test]
    fn a_dot_or_an_exponent_makes_a_float() {
        let kinds: Vec<bool> = tokenize("1 0x10 07 1.5 5. .5 1e5 0b1 3u", 0, 1)
            .unwrap()
            .tokens
            .iter()
            .map(|token| token.is_float)
            .collect();
        assert_eq!(
            kinds,
            [false, false, false, true, true, true, true, false, false]
        );
    }

    #[test]
    fn numbers_with_two_dots_or_an_exception_are_errors() {
        for text in ["1.0.0", "5..", ".5.5", "1..5"] {
            assert_eq!(error(text), LexErrorKind::TwoDots, "{text}");
        }
        assert_eq!(
            error("5.5#INF"),
            LexErrorKind::FloatException {
                number: "5.5#INF".into()
            }
        );
        assert_eq!(
            error("1.#QNAN"),
            LexErrorKind::FloatException {
                number: "1.#QNAN".into()
            }
        );
        assert_eq!(
            error("5.#"),
            LexErrorKind::UnknownFloatException {
                number: "5.".into()
            }
        );
    }

    #[test]
    fn strings_keep_backslashes_and_end_at_their_quote() {
        assert_eq!(texts(r#""a\\" "x;y""#), [r#""a\\""#, r#""x;y""#]);
        assert_eq!(texts(r#""a""b""#), [r#""a""#, r#""b""#]);
        assert_eq!(texts("'a'b ''"), ["'a'", "b", "''"]);
        let kinds: Vec<TokenKind> = tokenize("\"s\" 'c'", 0, 1)
            .unwrap()
            .tokens
            .iter()
            .map(|token| token.kind)
            .collect();
        assert_eq!(kinds, [TokenKind::String, TokenKind::CharLiteral]);
    }

    #[test]
    fn unfinished_strings_and_long_literals_are_errors() {
        // `"a\"b"`: the backslash does not escape, so `b"` opens a string the
        // line ends inside.
        assert_eq!(error("\"a\\\"b\"\nx"), LexErrorKind::NewlineInString);
        assert_eq!(error("'a\n"), LexErrorKind::NewlineInString);
        assert_eq!(error("x\n  \"abc"), LexErrorKind::MissingClosingQuote);
        assert_eq!(tokenize("x\n  \"abc", 0, 1).unwrap_err().line, 2);
        assert_eq!(error("'ab'"), LexErrorKind::LongCharLiteral);
        assert_eq!(error("'\\n'"), LexErrorKind::LongCharLiteral);
    }

    #[test]
    fn punctuation_is_the_longest_match() {
        assert_eq!(
            texts("a<<=b>>c...d..e::f.*g->h##i#j$k"),
            [
                "a", "<<=", "b", ">>", "c", "...", "d", ".", ".", "e", "::", "f", ".*", "g", "->",
                "h", "##", "i", "#", "j", "$", "k"
            ]
        );
        let tokens = tokenize("x@y `z", 0, 1).unwrap().tokens;
        assert_eq!(tokens[1].text, "@");
        assert_eq!(tokens[1].punct, Punct::Other);
        assert_eq!(tokens[3].text, "`");
        assert!(tokens[0].is_identifier());
        assert!(tokens[1].is(Punct::Other));
    }

    #[test]
    fn comments_are_dropped_and_keep_columns() {
        let tokens = tokenize("a ;c\n/* x */ b // d\n\tc /* y\n z */ d", 0, 1)
            .unwrap()
            .tokens;
        let layout: Vec<(&str, &str, bool, usize, usize)> = tokens
            .iter()
            .map(|token| {
                (
                    token.text.as_str(),
                    token.leading.as_str(),
                    token.starts_line,
                    token.line,
                    token.column,
                )
            })
            .collect();
        assert_eq!(
            layout,
            [
                ("a", "", true, 1, 1),
                ("b", "        ", true, 2, 9),
                ("c", "\t", true, 3, 2),
                // The comment spans lines but does not end the line.
                ("d", " ", false, 4, 7),
            ]
        );
    }

    #[test]
    fn a_comment_between_tokens_is_one_space() {
        let tokens = tokenize("a/*x*/b  c", 0, 1).unwrap().tokens;
        assert_eq!(tokens[1].leading, " ");
        assert_eq!(tokens[2].leading, "  ");
    }

    #[test]
    fn unterminated_and_nested_comments() {
        assert_eq!(error("a /* c"), LexErrorKind::UnterminatedComment);
        let lexed = tokenize("a /* /* */ b", 0, 1).unwrap();
        assert_eq!(lexed.tokens.len(), 2);
        assert_eq!(
            lexed.warnings,
            [LexWarning {
                line: 1,
                kind: LexWarningKind::NestedComment
            }]
        );
    }

    #[test]
    fn a_backslash_at_the_end_of_a_line_joins_the_next() {
        let tokens = tokenize("#define A 1 \\\n   + 2\nb\n  \\\n  c\nd \\", 0, 1)
            .unwrap()
            .tokens;
        let layout: Vec<(&str, &str, bool)> = tokens
            .iter()
            .map(|token| {
                (
                    token.text.as_str(),
                    token.leading.as_str(),
                    token.starts_line,
                )
            })
            .collect();
        assert_eq!(
            layout,
            [
                ("#", "", true),
                ("define", "", false),
                ("A", " ", false),
                ("1", " ", false),
                ("+", " ", false),
                ("2", " ", false),
                ("b", "", true),
                ("c", "  ", true),
                ("d", "", true),
            ]
        );
        // A backslash elsewhere is a token.
        assert_eq!(texts("a\\b"), ["a", "\\", "b"]);
    }

    #[test]
    fn lines_count_from_the_first_line_given() {
        let tokens = tokenize("a\r\n\r\nb", 0, 10).unwrap().tokens;
        assert_eq!((tokens[0].line, tokens[1].line), (10, 12));
        assert!(tokens[1].starts_line);
        assert_eq!(tokens[1].leading, "");
    }

    #[test]
    fn single_tokens_are_whole_tokens() {
        assert_eq!(
            lex_single_token("ab12").unwrap().kind,
            TokenKind::Identifier
        );
        assert_eq!(lex_single_token("+=").unwrap().punct, Punct::AddAssign);
        assert!(lex_single_token("1.5").unwrap().is_float);
        for text in ["", "+-", " a", "a b", "1.0.0", "a;"] {
            assert!(lex_single_token(text).is_none(), "{text}");
        }
    }
}
