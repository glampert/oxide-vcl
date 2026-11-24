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
    /// The library the program calls, or whose entry it is.
    pub library: Option<Library>,
    /// For a library's entry: where its parameters arrive and its results
    /// leave.
    pub entry: Option<Entry>,
}

/// `#pragma library(name, address = A, vf = F, vi = I, saved_vf = SF,
/// saved_vi = SI, save = memory)`: a library resident in micro memory at
/// `address`, whose calls pass values in VF`vf`-VF31 and VI`vi`-VI14 and
/// may change all of them. The `saved_*` registers below each bank the
/// library also uses, saving them at an entry and restoring them before it
/// returns, in its memory `save`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Library {
    pub name: String,
    pub address: u16,
    pub vf: u8,
    pub vi: u8,
    pub saved_vf: u8,
    pub saved_vi: u8,
    /// The library memory the saved registers go to: its index among the
    /// parameters, once bound, and its name.
    pub save: Option<String>,
    pub pos: Pos,
}

impl Library {
    /// The micro address of export `index`: one entry-table pair each.
    pub fn entry_address(&self, index: usize) -> u16 {
        self.address + 2 * index as u16
    }
}

/// A physical register a library call passes a value in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Register {
    Vf(u8),
    Vi(u8),
}

impl Register {
    pub fn name(self) -> String {
        match self {
            Register::Vf(n) => format!("vf{n:02}"),
            Register::Vi(n) => format!("vi{n:02}"),
        }
    }
}

/// One value of a library call: the register it travels in, the value
/// passed in (`in`, `inout`) and the local it comes back into (`out`,
/// `inout`, a return value).
#[derive(Clone, Debug)]
pub struct CallArg {
    pub register: Register,
    pub ty: Type,
    pub placement: Option<Placement>,
    pub input: Option<Expr>,
    pub output: Option<LocalId>,
}

/// A library entry's interface: each parameter's local and register.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The export's name, and its index in the library's entry table.
    pub export: String,
    pub index: usize,
    pub params: Vec<EntryParam>,
    /// Where the library's saved registers go: the qword address of its
    /// `save` memory.
    pub save: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct EntryParam {
    pub local: LocalId,
    pub register: Register,
    pub placement: Option<Placement>,
    /// Arrives in its register (`in`, `inout`).
    pub input: bool,
    /// Leaves in its register (`out`, `inout`, the return value).
    pub output: bool,
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
    /// Memory the library the program calls binds at file scope.
    pub library: bool,
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

impl Uses {
    /// What `other` uses too.
    pub fn merge(&mut self, other: &Uses) {
        self.xtop |= other.xtop;
        self.xitop |= other.xitop;
        self.clip |= other.clip;
        self.efu |= other.efu;
        self.xgkick |= other.xgkick;
        self.yield_ |= other.yield_;
    }
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
    /// A call of a library's export, at micro address `entry`: `setup` sets
    /// the parameters' locals, `args` passes them, and `body` is the
    /// export inlined, which is what the call means (the interpreter runs
    /// it; code generation calls the library instead). `inlined` is false
    /// when the program sees only the export's prototype: `body` is then
    /// empty, and the interpreter cannot run the call.
    Call {
        export: String,
        entry: u16,
        setup: Vec<Stmt>,
        args: Vec<CallArg>,
        body: Vec<Stmt>,
        inlined: bool,
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
    /// The lanes rotated: x takes y's, y z's, z w's, w x's (MR32).
    Mr32,
    ClipW,
    Any,
    Adc,
    /// 1 when any of the judgements is outside, else 0: FCAND's VI01.
    Clipped,
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
    /// A matrix from its stored qwords, each a vector of the matrix's
    /// size (`mat4(c0, c1, c2, c3)`, `mat3(m)`).
    Columns(Vec<Expr>),
}

impl Expr {
    /// Calls `visit` on each operand.
    pub fn each_child(&self, visit: &mut dyn FnMut(&Expr)) {
        match &self.kind {
            ExprKind::Const(_) | ExprKind::Local(_) | ExprKind::Load(_) | ExprKind::Address(_) => {}
            ExprKind::Swizzle { base: one, .. }
            | ExprKind::Neg(one)
            | ExprKind::Not(one)
            | ExprKind::Convert { operand: one, .. }
            | ExprKind::Column { matrix: one, .. }
            | ExprKind::RefTo { address: one, .. } => visit(one),
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Compare { lhs, rhs, .. }
            | ExprKind::And(lhs, rhs)
            | ExprKind::Or(lhs, rhs) => {
                visit(lhs);
                visit(rhs);
            }
            ExprKind::Call { args: parts, .. }
            | ExprKind::Construct(parts)
            | ExprKind::Columns(parts) => {
                parts.iter().for_each(visit);
            }
        }
    }

    /// Calls `visit` on each operand, which it may change.
    pub fn each_child_mut(&mut self, visit: &mut dyn FnMut(&mut Expr)) {
        match &mut self.kind {
            ExprKind::Const(_) | ExprKind::Local(_) | ExprKind::Load(_) | ExprKind::Address(_) => {}
            ExprKind::Swizzle { base: one, .. }
            | ExprKind::Neg(one)
            | ExprKind::Not(one)
            | ExprKind::Convert { operand: one, .. }
            | ExprKind::Column { matrix: one, .. }
            | ExprKind::RefTo { address: one, .. } => visit(one),
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Compare { lhs, rhs, .. }
            | ExprKind::And(lhs, rhs)
            | ExprKind::Or(lhs, rhs) => {
                visit(lhs);
                visit(rhs);
            }
            ExprKind::Call { args: parts, .. }
            | ExprKind::Construct(parts)
            | ExprKind::Columns(parts) => {
                parts.iter_mut().for_each(visit);
            }
        }
    }

    /// Whether the expression reads `local`, at any depth.
    pub fn reads_local(&self, local: LocalId) -> bool {
        match &self.kind {
            ExprKind::Local(id) => return *id == local,
            ExprKind::Load(place) | ExprKind::Address(place) => {
                return place.base == Base::Ref(local)
                    || place
                        .offset
                        .indices
                        .iter()
                        .any(|(index, _)| index.reads_local(local));
            }
            _ => {}
        }
        let mut found = false;
        self.each_child(&mut |child| found |= child.reads_local(local));
        found
    }

    /// Whether the expression reads memory, at any depth.
    pub fn loads(&self) -> bool {
        if matches!(self.kind, ExprKind::Load(_)) {
            return true;
        }
        let mut found = false;
        self.each_child(&mut |child| found |= child.loads());
        found
    }
}
