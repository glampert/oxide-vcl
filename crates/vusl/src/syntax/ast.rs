//! The syntax tree the parser builds: declarations, statements and
//! expressions as written, each with the place it starts. Nothing here is
//! checked yet: names, types and the strict rules are the checker's
//! (`sema`).

use super::lexer::Pos;

/// A name as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub pos: Pos,
}

/// A whole program.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub items: Vec<Item>,
}

/// What a file declares.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Pragma(Pragma),
    Struct(StructDecl),
    Const(ConstDecl),
    Function(Function),
    /// `layout(...) qualifier Type name;`: memory a library binds at file
    /// scope, as `main` binds its parameters.
    Memory(Param),
}

/// `#pragma name` or `#pragma name(arguments)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pragma {
    pub name: Ident,
    /// `None` without parentheses.
    pub args: Option<Vec<Arg>>,
    pub pos: Pos,
}

/// An argument of a pragma or a layout: `name = value`, or a value alone
/// (a name alone is an [`ExprKind::Name`] value).
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
    pub pos: Pos,
}

impl Arg {
    /// The argument's name: its own, or that of a value that is a name
    /// alone (`xtop` in `layout(xtop)`).
    pub fn key(&self) -> Option<&str> {
        match (&self.name, &self.value.kind) {
            (Some(name), _) => Some(&name.name),
            (None, ExprKind::Name(name)) => Some(&name.name),
            _ => None,
        }
    }

    /// Whether the argument is a name alone: a flag, such as `xtop`.
    pub fn is_flag(&self) -> bool {
        self.name.is_none() && matches!(self.value.kind, ExprKind::Name(_))
    }
}

/// `layout(...)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub args: Vec<Arg>,
    pub pos: Pos,
}

/// A type as written: a name, or `ref<T>`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeName {
    pub name: Ident,
    /// `T` in `ref<T>`.
    pub argument: Option<Box<TypeName>>,
    pub pos: Pos,
}

/// An array's length as written.
#[derive(Clone, Debug, PartialEq)]
pub enum ArrayLen {
    /// `[expression]`: a constant, or the name of a length.
    Expr(Expr),
    /// `[]`: open.
    Open,
}

/// A declared name, with its array length and initial value.
#[derive(Clone, Debug, PartialEq)]
pub struct Declarator {
    pub name: Ident,
    pub array: Option<ArrayLen>,
    pub init: Option<Expr>,
}

/// `struct Name { members };`.
#[derive(Clone, Debug, PartialEq)]
pub struct StructDecl {
    pub name: Ident,
    pub members: Vec<Member>,
    pub pos: Pos,
}

/// One member line: `layout(...) type name, name[len];`.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub layout: Option<Layout>,
    pub ty: TypeName,
    pub names: Vec<Declarator>,
    pub pos: Pos,
}

/// `const type name = value;` at file scope.
#[derive(Clone, Debug, PartialEq)]
pub struct ConstDecl {
    pub ty: TypeName,
    pub name: Ident,
    pub value: Expr,
    pub pos: Pos,
}

/// A parameter's access qualifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Qualifier {
    In,
    Out,
    Inout,
    Uniform,
}

impl Qualifier {
    pub fn spelling(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Inout => "inout",
            Self::Uniform => "uniform",
        }
    }
}

/// A function parameter: `layout(...) qualifier type name`.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub layout: Option<Layout>,
    pub qualifier: Option<(Qualifier, Pos)>,
    pub ty: TypeName,
    pub name: Ident,
    pub pos: Pos,
}

/// `type name(parameters) { body }`, or with `export` before it, a
/// library's entry. A prototype (`export type name(parameters);`) has an
/// empty body.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub ret: TypeName,
    pub name: Ident,
    pub params: Vec<Param>,
    pub body: Block,
    pub export: bool,
    pub prototype: bool,
    pub pos: Pos,
}

/// `{ statements }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub pos: Pos,
}

/// A local declaration: `layout(...) const precise type name = value, ...;`.
#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    pub layout: Option<Layout>,
    pub constant: bool,
    pub precise: bool,
    pub ty: TypeName,
    pub declarators: Vec<Declarator>,
    pub pos: Pos,
}

/// One range of a range-for: `qualifier type name : source`.
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub qualifier: Option<(Qualifier, Pos)>,
    pub ty: TypeName,
    pub name: Ident,
    pub source: Expr,
    pub pos: Pos,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub pos: Pos,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Block(Block),
    /// A pragma between statements: it applies to the next one, or is a
    /// statement itself (`#pragma barrier`).
    Pragma(Pragma),
    Decl(Decl),
    Expr(Expr),
    If {
        cond: Expr,
        then: Box<Stmt>,
        otherwise: Option<Box<Stmt>>,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
    },
    RangeFor {
        ranges: Vec<Range>,
        body: Box<Stmt>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Yield,
    Empty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    BitNot,
    PreIncrement,
    PreDecrement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostfixOp {
    Increment,
    Decrement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    LogicAnd,
    LogicOr,
}

impl BinaryOp {
    pub fn spelling(self) -> &'static str {
        use BinaryOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Mod => "%",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
            Less => "<",
            Greater => ">",
            LessEqual => "<=",
            GreaterEqual => ">=",
            Equal => "==",
            NotEqual => "!=",
            LogicAnd => "&&",
            LogicOr => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        use BinaryOp::*;
        matches!(
            self,
            Less | Greater | LessEqual | GreaterEqual | Equal | NotEqual
        )
    }
}

/// `=` or a compound assignment, which applies its operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssignOp {
    Assign,
    Compound(BinaryOp),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub pos: Pos,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f32),
    Bool(bool),
    Name(Ident),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Postfix {
        op: PostfixOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        /// Where the operator is, for messages about the operation.
        op_pos: Pos,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Assign {
        op: AssignOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `?:`, parsed so the checker can reject it with a reason.
    Conditional {
        cond: Box<Expr>,
        /// Where the `?` is.
        question: Pos,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// A call of a function, a built-in or a constructor (`vec4(...)`,
    /// `ref<T>(...)`).
    Call {
        callee: TypeName,
        args: Vec<Expr>,
    },
    /// `.name`: a member or a swizzle.
    Field {
        base: Box<Expr>,
        field: Ident,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
}
