//! The checked program: `main`'s parameters bound in memory, and its body
//! with every function inlined and every expression typed and placed. Code
//! generation starts from this.

use super::layout::{Extent, StructDef};
use super::types::{Lane, Placement, Type};
use crate::syntax::Pos;
use crate::syntax::ast::Qualifier;

pub type LocalId = usize;
pub type ElementId = usize;

/// A checked program.
#[derive(Clone, Debug)]
pub struct Program {
    /// `#pragma program(name)`, or the file's stem.
    pub name: String,
    pub structs: Vec<StructDef>,
    pub params: Vec<Param>,
    pub locals: Vec<Local>,
    pub elements: Vec<Element>,
    pub body: Vec<Stmt>,
    pub uses: Uses,
    /// What the program reads and writes in its parameters' memory.
    pub accesses: Vec<Access>,
}

/// One of `main`'s parameters: memory the program reads or writes, or the
/// count VIF ITOP sent.
#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub qualifier: Qualifier,
    pub ty: Type,
    pub placement: ParamPlacement,
    /// `layout(spill)`: the region takes oxide-vcl's spills.
    pub spill: bool,
    /// `layout(alias = other)`: may overlap that parameter.
    pub alias: Option<usize>,
    pub pos: Pos,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParamPlacement {
    /// A fixed qword address in data memory.
    Address(u32),
    /// The current double buffer, from XTOP, with the VIF1 BASE and OFFSET
    /// the host sets when the program says.
    Xtop { vif: Option<Vif> },
    /// The `int16` count from XITOP.
    Xitop,
}

/// VIF1 BASE and OFFSET: two buffers of `offset` qwords from `base`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Vif {
    pub base: u32,
    pub offset: u32,
}

/// A variable in VU registers.
#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Type,
    /// `layout(lane = L)`: every value assigned moves into these lanes.
    pub fixed: Option<Placement>,
    /// `precise`: no contraction into multiply-adds.
    pub precise: bool,
    /// For a `ref<T>`: the memory it stays within.
    pub within: Option<Within>,
    pub pos: Pos,
}

/// The memory a `ref<T>` stays within: a place's base and members, for
/// the stores that may change what is read through it, with what the
/// program may do there and the parameter member it is in.
#[derive(Clone, Debug, PartialEq)]
pub struct Within {
    pub base: Base,
    pub path: Vec<usize>,
    pub access: Qualifier,
    pub root: Option<(usize, Vec<usize>)>,
}

/// A range-for's element: a view of one element of an array per
/// iteration.
#[derive(Clone, Debug)]
pub struct Element {
    pub name: String,
    pub ty: Type,
    /// The array, as a place.
    pub array: Place,
    pub access: Qualifier,
    /// The qwords one element takes.
    pub stride: u32,
    pub pos: Pos,
}

/// The features a program uses, for the reflection and the code's
/// prologue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Uses {
    pub xtop: bool,
    pub xitop: bool,
    pub clip: bool,
    pub efu: bool,
    pub xgkick: bool,
    pub yield_: bool,
}

/// A read or write of a parameter's member, by member indices from the
/// parameter's struct (array elements count as their array).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Access {
    pub param: usize,
    pub path: Vec<usize>,
    pub write: bool,
}

/// Where in memory a value is: a base address and qwords from it.
#[derive(Clone, Debug)]
pub struct Place {
    pub base: Base,
    /// The struct members from the base to here, for lengths read from
    /// memory and for the reflection.
    pub path: Vec<usize>,
    pub offset: Offset,
    pub ty: Type,
    /// The first lane, for a value that takes lanes rather than qwords.
    pub lane: Lane,
    /// For an array whose length is read from memory: the `int16` it is
    /// read from.
    pub length: Option<Box<Place>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Base {
    Param(usize),
    Element(ElementId),
    /// The address a `ref<T>` local holds.
    Ref(LocalId),
}

/// Qwords from a place's base: a fixed part, lengths read from `int16`
/// members of the base (by member path), and run-time indices.
#[derive(Clone, Debug, Default)]
pub struct Offset {
    pub extent: Extent,
    /// `(index, stride)`: `index * stride` qwords.
    pub indices: Vec<(Expr, u32)>,
}

/// How a range-for counts its iterations.
#[derive(Clone, Debug)]
pub enum Count {
    Fixed(u32),
    /// An `int16` member in memory.
    Member(Place),
    /// The count XITOP reads: a `layout(xitop)` parameter.
    Xitop,
}

/// A pragma's loop hints.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LoopHints {
    pub unroll: Option<Unroll>,
    pub pipeline: Option<Pipeline>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unroll {
    /// `#pragma unroll`: all iterations, for a constant trip count.
    Full,
    By(u32),
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pipeline {
    /// `#pragma nopipeline`.
    Off,
    /// `#pragma pipeline(min, safe, extra, abs)`: `--LoopCS min,safe`,
    /// `--LoopExtra`, `--LoopAbs`.
    Hint {
        min: u32,
        safe: u32,
        extra: Option<u32>,
        abs: Option<u32>,
    },
}

#[derive(Clone, Debug)]
pub enum LoopKind {
    /// `while`, `for` and `do`: the condition's statements and value,
    /// tested before the body unless `do`, and `for`'s step after it.
    Condition {
        cond_stmts: Vec<Stmt>,
        cond: Expr,
        test_first: bool,
        step: Vec<Stmt>,
    },
    /// A range-for: `count` iterations over its elements.
    Range {
        elements: Vec<ElementId>,
        count: Count,
    },
}

#[derive(Clone, Debug)]
pub enum Stmt {
    /// `local = value`, `local.components = value`, or for a matrix
    /// `local[column] = value` and `local[column].components = value`.
    SetLocal {
        local: LocalId,
        column: Option<usize>,
        components: Option<Vec<usize>>,
        value: Expr,
        pos: Pos,
    },
    /// A store: `place = value`, or `place.components = value`.
    Store {
        place: Place,
        components: Option<Vec<usize>>,
        value: Expr,
        pos: Pos,
    },
    /// Whole qwords copied from one place to another.
    Copy {
        to: Place,
        from: Place,
        qwords: u32,
        pos: Pos,
    },
    /// XGKICK of the packet at a place.
    Kick {
        place: Place,
        pos: Pos,
    },
    If {
        cond: Expr,
        then: Vec<Stmt>,
        otherwise: Vec<Stmt>,
        pos: Pos,
    },
    Loop {
        kind: LoopKind,
        body: Vec<Stmt>,
        hints: LoopHints,
        pos: Pos,
    },
    Break {
        pos: Pos,
    },
    Continue {
        pos: Pos,
    },
    /// `return` in `main`: the program ends.
    Exit {
        pos: Pos,
    },
    /// `yield`: the program stops until MSCNT.
    Yield {
        pos: Pos,
    },
    /// `#pragma barrier`.
    Barrier {
        pos: Pos,
    },
}

/// A typed expression. VF values have a placement once their lanes are
/// known (vectors always; scalars when read from a lane).
#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub placement: Option<Placement>,
    pub pos: Pos,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f32),
    Bool(bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinOp {
    /// Floating-point, element by element (one operand may be a scalar).
    Add,
    Sub,
    Mul,
    /// By a scalar: through the FDIV unit and a Q multiply.
    Div,
    Min,
    Max,
    /// `int16`.
    IntAdd,
    IntSub,
    IntAnd,
    IntOr,
    /// `matrix * vector`, columns stored.
    MatVec,
    /// `vector * matrix`, rows stored.
    VecMat,
    /// `matrix * matrix`, in the matrices' order.
    MatMat,
    /// `scalar * matrix`.
    ScaleMat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareOp {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Builtin {
    Dot,
    Cross,
    Normalize,
    Length,
    Sqrt,
    Rsqrt,
    Abs,
    Clamp,
    ClipW,
    Any,
    Adc,
    Esadd,
    Ersadd,
    Eleng,
    Erleng,
    Esum,
    Ercpr,
    Esqrt,
    Ersqrt,
    Esin,
    Eatan,
    Eexp,
    /// The count VIF ITOP sent (XITOP).
    Xitop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Conversion {
    /// FTOI0/4/12/15: float to fixed-point bits.
    FloatToFixed(u8),
    /// ITOF0/4/12/15.
    FixedToFloat(u8),
    /// `int16` to a 32-bit lane, sign-extended (MFIR).
    IntToLane,
    /// A 32-bit lane's low 16 bits to `int16` (MTIR).
    LaneToInt,
    /// `float(i)`: MFIR then ITOF0.
    IntToFloat,
    /// `int16(f)`: FTOI0 then MTIR.
    FloatToInt,
    /// Raw bits under another raw type (`uint32(int32)`): no instruction.
    Bits,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Const(Const),
    Local(LocalId),
    Load(Place),
    Swizzle {
        base: Box<Expr>,
        components: Vec<usize>,
    },
    /// `-x`.
    Neg(Box<Expr>),
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Compare {
        op: CompareOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Call {
        builtin: Builtin,
        args: Vec<Expr>,
    },
    /// A vector from parts, component by component (`vec4(v.xyz, 1.0)`),
    /// or one scalar in every component.
    Construct(Vec<Expr>),
    Convert {
        conversion: Conversion,
        operand: Box<Expr>,
    },
    /// A matrix's stored qword `index`.
    Column {
        matrix: Box<Expr>,
        index: usize,
    },
    /// A place's qword address, as an `int16`.
    Address(Place),
    /// A `ref<T>`: an `int16` address, and the memory it stays within.
    RefTo {
        address: Box<Expr>,
        within: Within,
    },
}
