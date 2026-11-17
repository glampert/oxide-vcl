//! A recursive-descent parser from the lexer's tokens to the syntax tree.
//! Operators take C's precedence. A statement that starts with two names
//! (`vec4 c`) or with `const`, `precise`, `layout` or `ref<` declares
//! variables; a `for` whose parentheses start `type name :` is a range-for.

use super::ast::*;
use super::lexer::{Pos, Punct, Token, TokenKind};

/// Words that cannot name anything.
pub const KEYWORDS: [&str; 20] = [
    "struct", "if", "else", "for", "while", "do", "break", "continue", "return", "yield", "in",
    "out", "inout", "uniform", "layout", "const", "precise", "true", "false", "ref",
];

/// Why the tokens are not a program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxErrorKind {
    /// `expected`, a description, where `found` is.
    Expected { expected: String, found: String },
    /// A keyword where a name belongs.
    KeywordAsName(String),
    /// A variable declared at file scope.
    GlobalVariable(String),
    /// A call of something that is not a name: `(f)(x)`, `a.b(c)`.
    NotCallable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxError {
    pub kind: SyntaxErrorKind,
    pub pos: Pos,
}

/// Parses a whole program from `tokens`, which end with
/// [`TokenKind::End`].
pub fn parse(tokens: &[Token]) -> Result<File, SyntaxError> {
    let mut parser = Parser { tokens, at: 0 };
    parser.file()
}

type Parsed<T> = Result<T, SyntaxError>;

/// Binary operators by C's precedence, loosest first.
const LEVELS: [&[(Punct, BinaryOp)]; 10] = [
    &[(Punct::LogicOr, BinaryOp::LogicOr)],
    &[(Punct::LogicAnd, BinaryOp::LogicAnd)],
    &[(Punct::BitOr, BinaryOp::BitOr)],
    &[(Punct::BitXor, BinaryOp::BitXor)],
    &[(Punct::BitAnd, BinaryOp::BitAnd)],
    &[
        (Punct::Equal, BinaryOp::Equal),
        (Punct::NotEqual, BinaryOp::NotEqual),
    ],
    &[
        (Punct::Less, BinaryOp::Less),
        (Punct::Greater, BinaryOp::Greater),
        (Punct::LessEqual, BinaryOp::LessEqual),
        (Punct::GreaterEqual, BinaryOp::GreaterEqual),
    ],
    &[(Punct::Shl, BinaryOp::Shl), (Punct::Shr, BinaryOp::Shr)],
    &[(Punct::Add, BinaryOp::Add), (Punct::Sub, BinaryOp::Sub)],
    &[
        (Punct::Mul, BinaryOp::Mul),
        (Punct::Div, BinaryOp::Div),
        (Punct::Mod, BinaryOp::Mod),
    ],
];

const QUALIFIERS: [(&str, Qualifier); 4] = [
    ("in", Qualifier::In),
    ("out", Qualifier::Out),
    ("inout", Qualifier::Inout),
    ("uniform", Qualifier::Uniform),
];

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        self.peek_at(0)
    }

    fn peek_at(&self, offset: usize) -> &Token {
        let last = self.tokens.len() - 1;
        &self.tokens[(self.at + offset).min(last)]
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.at < self.tokens.len() - 1 {
            self.at += 1;
        }
        token
    }

    fn at_punct(&self, punct: Punct) -> bool {
        self.peek().is(punct)
    }

    fn eat(&mut self, punct: Punct) -> bool {
        let found = self.at_punct(punct);
        if found {
            self.advance();
        }
        found
    }

    fn at_keyword(&self, word: &str) -> bool {
        self.peek().is_ident(word)
    }

    fn eat_keyword(&mut self, word: &str) -> bool {
        let found = self.at_keyword(word);
        if found {
            self.advance();
        }
        found
    }

    /// A name token: an identifier that is not a keyword.
    fn is_name(token: &Token) -> bool {
        token.kind == TokenKind::Ident && !KEYWORDS.contains(&token.text.as_str())
    }

    /// How the token is described in a message.
    fn describe(token: &Token) -> String {
        match token.kind {
            TokenKind::End => "the end of the program".into(),
            TokenKind::PragmaEnd => "the end of the pragma's line".into(),
            _ => format!("'{}'", token.text),
        }
    }

    fn expected<T>(&self, expected: &str) -> Parsed<T> {
        Err(SyntaxError {
            kind: SyntaxErrorKind::Expected {
                expected: expected.into(),
                found: Self::describe(self.peek()),
            },
            pos: self.peek().pos,
        })
    }

    fn expect(&mut self, punct: Punct) -> Parsed<Pos> {
        if self.at_punct(punct) {
            Ok(self.advance().pos)
        } else {
            self.expected(&format!("'{punct}'"))
        }
    }

    fn ident(&mut self, what: &str) -> Parsed<Ident> {
        let token = self.peek();
        if token.kind != TokenKind::Ident {
            return self.expected(what);
        }
        if KEYWORDS.contains(&token.text.as_str()) {
            return Err(SyntaxError {
                kind: SyntaxErrorKind::KeywordAsName(token.text.clone()),
                pos: token.pos,
            });
        }
        let token = self.advance();
        Ok(Ident {
            name: token.text,
            pos: token.pos,
        })
    }

    // ----------------------------------------------
    // Declarations
    // ----------------------------------------------

    fn file(&mut self) -> Parsed<File> {
        let mut items = Vec::new();
        while self.peek().kind != TokenKind::End {
            items.push(self.item()?);
        }
        Ok(File { items })
    }

    fn item(&mut self) -> Parsed<Item> {
        if self.peek().kind == TokenKind::PragmaStart {
            return Ok(Item::Pragma(self.pragma()?));
        }
        if self.at_keyword("struct") {
            return Ok(Item::Struct(self.struct_decl()?));
        }
        if self.at_keyword("const") {
            return Ok(Item::Const(self.const_decl()?));
        }
        let pos = self.peek().pos;
        let ret = self.type_name("a declaration")?;
        let name = self.ident("a function's name")?;
        if !self.at_punct(Punct::OpenParen) {
            return Err(SyntaxError {
                kind: SyntaxErrorKind::GlobalVariable(name.name),
                pos: name.pos,
            });
        }
        let params = self.params()?;
        let body = self.block()?;
        Ok(Item::Function(Function {
            ret,
            name,
            params,
            body,
            pos,
        }))
    }

    fn pragma(&mut self) -> Parsed<Pragma> {
        let pos = self.advance().pos;
        let name = self.ident("the pragma's name")?;
        let args = if self.at_punct(Punct::OpenParen) {
            Some(self.args()?)
        } else {
            None
        };
        if self.peek().kind != TokenKind::PragmaEnd {
            return self.expected("the end of the pragma's line");
        }
        self.advance();
        Ok(Pragma { name, args, pos })
    }

    /// `(arg, ...)` of a pragma or a layout: `name = value` or a value.
    fn args(&mut self) -> Parsed<Vec<Arg>> {
        self.expect(Punct::OpenParen)?;
        let mut args = Vec::new();
        if self.eat(Punct::CloseParen) {
            return Ok(args);
        }
        loop {
            let pos = self.peek().pos;
            let name = if Self::is_name(self.peek()) && self.peek_at(1).is(Punct::Assign) {
                let name = self.ident("a name")?;
                self.advance();
                Some(name)
            } else {
                None
            };
            let value = self.conditional()?;
            args.push(Arg { name, value, pos });
            if self.eat(Punct::CloseParen) {
                return Ok(args);
            }
            self.expect(Punct::Comma)?;
        }
    }

    fn layout(&mut self) -> Parsed<Option<Layout>> {
        if !self.at_keyword("layout") {
            return Ok(None);
        }
        let pos = self.advance().pos;
        let args = self.args()?;
        Ok(Some(Layout { args, pos }))
    }

    /// A type: a name, or `ref<T>`.
    fn type_name(&mut self, what: &str) -> Parsed<TypeName> {
        let pos = self.peek().pos;
        if self.at_keyword("ref") {
            let token = self.advance();
            self.expect(Punct::Less)?;
            let argument = self.type_name("the referenced type")?;
            self.expect(Punct::Greater)?;
            return Ok(TypeName {
                name: Ident {
                    name: token.text,
                    pos,
                },
                argument: Some(Box::new(argument)),
                pos,
            });
        }
        if self.peek().kind != TokenKind::Ident {
            return self.expected(what);
        }
        let name = self.ident(what)?;
        Ok(TypeName {
            name,
            argument: None,
            pos,
        })
    }

    fn array_len(&mut self) -> Parsed<Option<ArrayLen>> {
        if !self.eat(Punct::OpenBracket) {
            return Ok(None);
        }
        if self.eat(Punct::CloseBracket) {
            return Ok(Some(ArrayLen::Open));
        }
        let length = self.expr()?;
        self.expect(Punct::CloseBracket)?;
        Ok(Some(ArrayLen::Expr(length)))
    }

    fn struct_decl(&mut self) -> Parsed<StructDecl> {
        let pos = self.advance().pos;
        let name = self.ident("the struct's name")?;
        self.expect(Punct::OpenBrace)?;
        let mut members = Vec::new();
        while !self.eat(Punct::CloseBrace) {
            let pos = self.peek().pos;
            let layout = self.layout()?;
            let ty = self.type_name("a member's type")?;
            let mut names = Vec::new();
            loop {
                let name = self.ident("a member's name")?;
                let array = self.array_len()?;
                names.push(Declarator {
                    name,
                    array,
                    init: None,
                });
                if !self.eat(Punct::Comma) {
                    break;
                }
            }
            self.expect(Punct::Semicolon)?;
            members.push(Member {
                layout,
                ty,
                names,
                pos,
            });
        }
        self.expect(Punct::Semicolon)?;
        Ok(StructDecl { name, members, pos })
    }

    fn const_decl(&mut self) -> Parsed<ConstDecl> {
        let pos = self.advance().pos;
        let ty = self.type_name("the constant's type")?;
        let name = self.ident("the constant's name")?;
        self.expect(Punct::Assign)?;
        let value = self.assignment()?;
        self.expect(Punct::Semicolon)?;
        Ok(ConstDecl {
            ty,
            name,
            value,
            pos,
        })
    }

    fn qualifier(&mut self) -> Option<(Qualifier, Pos)> {
        let (_, qualifier) = QUALIFIERS.iter().find(|(word, _)| self.at_keyword(word))?;
        Some((*qualifier, self.advance().pos))
    }

    fn params(&mut self) -> Parsed<Vec<Param>> {
        self.expect(Punct::OpenParen)?;
        let mut params = Vec::new();
        if self.eat(Punct::CloseParen) {
            return Ok(params);
        }
        if self.at_keyword("void") && self.peek_at(1).is(Punct::CloseParen) {
            self.advance();
            self.advance();
            return Ok(params);
        }
        loop {
            let pos = self.peek().pos;
            let layout = self.layout()?;
            let qualifier = self.qualifier();
            let ty = self.type_name("a parameter's type")?;
            let name = self.ident("a parameter's name")?;
            params.push(Param {
                layout,
                qualifier,
                ty,
                name,
                pos,
            });
            if self.eat(Punct::CloseParen) {
                return Ok(params);
            }
            self.expect(Punct::Comma)?;
        }
    }

    // ----------------------------------------------
    // Statements
    // ----------------------------------------------

    fn block(&mut self) -> Parsed<Block> {
        let pos = self.expect(Punct::OpenBrace)?;
        let mut stmts = Vec::new();
        while !self.eat(Punct::CloseBrace) {
            if self.peek().kind == TokenKind::End {
                return self.expected("'}'");
            }
            stmts.push(self.stmt()?);
        }
        Ok(Block { stmts, pos })
    }

    /// Whether the statement here declares variables.
    fn starts_decl(&self) -> bool {
        let token = self.peek();
        if ["const", "precise", "layout"]
            .iter()
            .any(|word| token.is_ident(word))
        {
            return true;
        }
        if token.is_ident("ref") {
            return self.peek_at(1).is(Punct::Less);
        }
        // A keyword after the type is a misnamed variable, which `decl`
        // reports as such.
        Self::is_name(token) && self.peek_at(1).kind == TokenKind::Ident
    }

    fn stmt(&mut self) -> Parsed<Stmt> {
        let token = self.peek().clone();
        let pos = token.pos;
        let kind = match &token.kind {
            TokenKind::PragmaStart => StmtKind::Pragma(self.pragma()?),
            TokenKind::Punct(Punct::OpenBrace) => StmtKind::Block(self.block()?),
            TokenKind::Punct(Punct::Semicolon) => {
                self.advance();
                StmtKind::Empty
            }
            TokenKind::Ident => match token.text.as_str() {
                "if" => self.if_stmt()?,
                "for" => self.for_stmt()?,
                "while" => {
                    self.advance();
                    self.expect(Punct::OpenParen)?;
                    let cond = self.expr()?;
                    self.expect(Punct::CloseParen)?;
                    let body = Box::new(self.stmt()?);
                    StmtKind::While { cond, body }
                }
                "do" => {
                    self.advance();
                    let body = Box::new(self.stmt()?);
                    if !self.eat_keyword("while") {
                        return self.expected("'while'");
                    }
                    self.expect(Punct::OpenParen)?;
                    let cond = self.expr()?;
                    self.expect(Punct::CloseParen)?;
                    self.expect(Punct::Semicolon)?;
                    StmtKind::DoWhile { body, cond }
                }
                "break" | "continue" | "yield" => {
                    self.advance();
                    self.expect(Punct::Semicolon)?;
                    match token.text.as_str() {
                        "break" => StmtKind::Break,
                        "continue" => StmtKind::Continue,
                        _ => StmtKind::Yield,
                    }
                }
                "return" => {
                    self.advance();
                    let value = if self.at_punct(Punct::Semicolon) {
                        None
                    } else {
                        Some(self.expr()?)
                    };
                    self.expect(Punct::Semicolon)?;
                    StmtKind::Return(value)
                }
                _ if self.starts_decl() => {
                    let decl = self.decl()?;
                    self.expect(Punct::Semicolon)?;
                    StmtKind::Decl(decl)
                }
                _ => self.expr_stmt()?,
            },
            _ => self.expr_stmt()?,
        };
        Ok(Stmt { kind, pos })
    }

    fn expr_stmt(&mut self) -> Parsed<StmtKind> {
        let expr = self.expr()?;
        self.expect(Punct::Semicolon)?;
        Ok(StmtKind::Expr(expr))
    }

    fn decl(&mut self) -> Parsed<Decl> {
        let pos = self.peek().pos;
        let layout = self.layout()?;
        let mut constant = false;
        let mut precise = false;
        loop {
            if self.eat_keyword("const") {
                constant = true;
            } else if self.eat_keyword("precise") {
                precise = true;
            } else {
                break;
            }
        }
        let ty = self.type_name("a type")?;
        let mut declarators = Vec::new();
        loop {
            let name = self.ident("a variable's name")?;
            let array = self.array_len()?;
            let init = if self.eat(Punct::Assign) {
                Some(self.assignment()?)
            } else {
                None
            };
            declarators.push(Declarator { name, array, init });
            if !self.eat(Punct::Comma) {
                break;
            }
        }
        Ok(Decl {
            layout,
            constant,
            precise,
            ty,
            declarators,
            pos,
        })
    }

    fn if_stmt(&mut self) -> Parsed<StmtKind> {
        self.advance();
        self.expect(Punct::OpenParen)?;
        let cond = self.expr()?;
        self.expect(Punct::CloseParen)?;
        let then = Box::new(self.stmt()?);
        let otherwise = if self.eat_keyword("else") {
            Some(Box::new(self.stmt()?))
        } else {
            None
        };
        Ok(StmtKind::If {
            cond,
            then,
            otherwise,
        })
    }

    /// Whether a range-for's `qualifier type name :` comes next.
    fn range_ahead(&self) -> bool {
        let mut at = 0;
        if QUALIFIERS
            .iter()
            .any(|(word, _)| self.peek_at(at).is_ident(word))
        {
            at += 1;
        }
        if self.peek_at(at).is_ident("ref") {
            let closed = self.peek_at(at + 1).is(Punct::Less)
                && Self::is_name(self.peek_at(at + 2))
                && self.peek_at(at + 3).is(Punct::Greater);
            if !closed {
                return false;
            }
            at += 4;
        } else if Self::is_name(self.peek_at(at)) {
            at += 1;
        } else {
            return false;
        }
        Self::is_name(self.peek_at(at)) && self.peek_at(at + 1).is(Punct::Colon)
    }

    fn for_stmt(&mut self) -> Parsed<StmtKind> {
        self.advance();
        self.expect(Punct::OpenParen)?;
        if self.range_ahead() {
            let mut ranges = Vec::new();
            loop {
                let pos = self.peek().pos;
                let qualifier = self.qualifier();
                let ty = self.type_name("the element's type")?;
                let name = self.ident("the element's name")?;
                self.expect(Punct::Colon)?;
                let source = self.expr()?;
                ranges.push(Range {
                    qualifier,
                    ty,
                    name,
                    source,
                    pos,
                });
                if self.eat(Punct::CloseParen) {
                    break;
                }
                self.expect(Punct::Comma)?;
                if !self.range_ahead() {
                    return self.expected("another range: 'type name : array'");
                }
            }
            let body = Box::new(self.stmt()?);
            return Ok(StmtKind::RangeFor { ranges, body });
        }
        let init = if self.eat(Punct::Semicolon) {
            None
        } else {
            let pos = self.peek().pos;
            let kind = if self.starts_decl() {
                StmtKind::Decl(self.decl()?)
            } else {
                StmtKind::Expr(self.expr()?)
            };
            self.expect(Punct::Semicolon)?;
            Some(Box::new(Stmt { kind, pos }))
        };
        let cond = if self.at_punct(Punct::Semicolon) {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect(Punct::Semicolon)?;
        let step = if self.at_punct(Punct::CloseParen) {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect(Punct::CloseParen)?;
        let body = Box::new(self.stmt()?);
        Ok(StmtKind::For {
            init,
            cond,
            step,
            body,
        })
    }

    // ----------------------------------------------
    // Expressions
    // ----------------------------------------------

    fn expr(&mut self) -> Parsed<Expr> {
        self.assignment()
    }

    fn assignment(&mut self) -> Parsed<Expr> {
        let target = self.conditional()?;
        let compound = |op| AssignOp::Compound(op);
        let op = match self.peek().kind {
            TokenKind::Punct(Punct::Assign) => AssignOp::Assign,
            TokenKind::Punct(Punct::AddAssign) => compound(BinaryOp::Add),
            TokenKind::Punct(Punct::SubAssign) => compound(BinaryOp::Sub),
            TokenKind::Punct(Punct::MulAssign) => compound(BinaryOp::Mul),
            TokenKind::Punct(Punct::DivAssign) => compound(BinaryOp::Div),
            TokenKind::Punct(Punct::ModAssign) => compound(BinaryOp::Mod),
            TokenKind::Punct(Punct::AndAssign) => compound(BinaryOp::BitAnd),
            TokenKind::Punct(Punct::OrAssign) => compound(BinaryOp::BitOr),
            TokenKind::Punct(Punct::XorAssign) => compound(BinaryOp::BitXor),
            TokenKind::Punct(Punct::ShlAssign) => compound(BinaryOp::Shl),
            TokenKind::Punct(Punct::ShrAssign) => compound(BinaryOp::Shr),
            _ => return Ok(target),
        };
        self.advance();
        let value = self.assignment()?;
        Ok(Expr {
            pos: target.pos,
            kind: ExprKind::Assign {
                op,
                target: Box::new(target),
                value: Box::new(value),
            },
        })
    }

    fn conditional(&mut self) -> Parsed<Expr> {
        let cond = self.binary(0)?;
        if !self.at_punct(Punct::Question) {
            return Ok(cond);
        }
        let question = self.advance().pos;
        let then = self.expr()?;
        self.expect(Punct::Colon)?;
        let otherwise = self.conditional()?;
        Ok(Expr {
            pos: cond.pos,
            kind: ExprKind::Conditional {
                cond: Box::new(cond),
                question,
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            },
        })
    }

    fn binary(&mut self, level: usize) -> Parsed<Expr> {
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        while let Some(&(_, op)) = LEVELS[level]
            .iter()
            .find(|(punct, _)| self.at_punct(*punct))
        {
            let op_pos = self.advance().pos;
            let rhs = self.binary(level + 1)?;
            lhs = Expr {
                pos: lhs.pos,
                kind: ExprKind::Binary {
                    op,
                    op_pos,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Parsed<Expr> {
        let token = self.peek().clone();
        let op = match token.kind {
            TokenKind::Punct(Punct::Sub) => UnaryOp::Neg,
            TokenKind::Punct(Punct::Add) => UnaryOp::Plus,
            TokenKind::Punct(Punct::Not) => UnaryOp::Not,
            TokenKind::Punct(Punct::BitNot) => UnaryOp::BitNot,
            TokenKind::Punct(Punct::Increment) => UnaryOp::PreIncrement,
            TokenKind::Punct(Punct::Decrement) => UnaryOp::PreDecrement,
            _ => return self.postfix(),
        };
        self.advance();
        let operand = self.unary()?;
        Ok(Expr {
            pos: token.pos,
            kind: ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
        })
    }

    fn postfix(&mut self) -> Parsed<Expr> {
        let mut expr = self.primary()?;
        loop {
            let kind = if self.eat(Punct::Dot) {
                let field = self.ident("a member or a swizzle")?;
                ExprKind::Field {
                    base: Box::new(expr),
                    field,
                }
            } else if self.eat(Punct::OpenBracket) {
                let index = self.expr()?;
                self.expect(Punct::CloseBracket)?;
                ExprKind::Index {
                    base: Box::new(expr),
                    index: Box::new(index),
                }
            } else if self.at_punct(Punct::Increment) || self.at_punct(Punct::Decrement) {
                let op = if self.advance().is(Punct::Increment) {
                    PostfixOp::Increment
                } else {
                    PostfixOp::Decrement
                };
                ExprKind::Postfix {
                    op,
                    operand: Box::new(expr),
                }
            } else if self.at_punct(Punct::OpenParen) {
                return Err(SyntaxError {
                    kind: SyntaxErrorKind::NotCallable,
                    pos: self.peek().pos,
                });
            } else {
                return Ok(expr);
            };
            let pos = match &kind {
                ExprKind::Field { base, .. }
                | ExprKind::Index { base, .. }
                | ExprKind::Postfix { operand: base, .. } => base.pos,
                _ => unreachable!("only postfix forms are built here"),
            };
            expr = Expr { kind, pos };
        }
    }

    fn call_args(&mut self) -> Parsed<Vec<Expr>> {
        self.expect(Punct::OpenParen)?;
        let mut args = Vec::new();
        if self.eat(Punct::CloseParen) {
            return Ok(args);
        }
        loop {
            args.push(self.assignment()?);
            if self.eat(Punct::CloseParen) {
                return Ok(args);
            }
            self.expect(Punct::Comma)?;
        }
    }

    fn primary(&mut self) -> Parsed<Expr> {
        let token = self.peek().clone();
        let pos = token.pos;
        let kind = match &token.kind {
            TokenKind::Int(value) => {
                self.advance();
                ExprKind::Int(*value)
            }
            TokenKind::Float(value) => {
                self.advance();
                ExprKind::Float(*value)
            }
            TokenKind::Punct(Punct::OpenParen) => {
                self.advance();
                let inner = self.expr()?;
                self.expect(Punct::CloseParen)?;
                return Ok(inner);
            }
            TokenKind::Ident if token.text == "true" || token.text == "false" => {
                self.advance();
                ExprKind::Bool(token.text == "true")
            }
            TokenKind::Ident if token.text == "ref" => {
                let callee = self.type_name("a type")?;
                let args = self.call_args()?;
                ExprKind::Call { callee, args }
            }
            TokenKind::Ident if Self::is_name(&token) => {
                let name = self.ident("an expression")?;
                if self.at_punct(Punct::OpenParen) {
                    let args = self.call_args()?;
                    let callee = TypeName {
                        pos: name.pos,
                        name,
                        argument: None,
                    };
                    ExprKind::Call { callee, args }
                } else {
                    ExprKind::Name(name)
                }
            }
            _ => return self.expected("an expression"),
        };
        Ok(Expr { kind, pos })
    }
}
