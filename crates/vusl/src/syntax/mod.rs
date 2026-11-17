//! Reading a preprocessed VUSL program: its tokens, then its syntax tree.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod print;

pub use lexer::Pos;

/// Why the text is not a program: a token the lexer cannot read, or tokens
/// the parser cannot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxFailure {
    Lex(lexer::LexError),
    Parse(parser::SyntaxError),
}

impl SyntaxFailure {
    pub fn pos(&self) -> Pos {
        match self {
            Self::Lex(error) => error.pos,
            Self::Parse(error) => error.pos,
        }
    }
}

/// Parses the preprocessed program `text`.
pub fn parse(text: &str) -> Result<ast::File, SyntaxFailure> {
    let tokens = lexer::tokenize(text).map_err(SyntaxFailure::Lex)?;
    parser::parse(&tokens).map_err(SyntaxFailure::Parse)
}
