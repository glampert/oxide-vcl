//! What lowering knows about the registers it has filled: which hold a
//! qword read from memory, an `int16` read from a lane, a sum of two VI
//! registers, or a pure expression's value. A second use of any of them
//! takes the register instead of new instructions (common subexpressions),
//! and a loop's invariants are lowered once before it and found there
//! (loop-invariant code motion).
//!
//! An entry lasts until something may change what it holds:
//!
//! - a store to memory it was read from;
//! - a write to a local its expression reads, or to the register holding
//!   it, or to a register its key names;
//! - a join of paths it was not computed on (a label), which lowering
//!   handles by going back to the entries every path has;
//! - a `yield`, after which the host may have changed memory.

use super::Fv;
use super::emit::{Vf, Vi};
use crate::sema::hir::*;
use crate::sema::layout::{Extent, StructDef};
use crate::sema::types::{Lane, Type};
use crate::syntax::ast::Qualifier;

/// A value lowering can find again.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Key {
    /// A whole qword, loaded with LQ from a base register and offset.
    Qword { base: Vi, offset: i64 },
    /// One lane's low 16 bits, loaded with ILW.
    Int16 { base: Vi, offset: i64, lane: Lane },
    /// The sum of two VI registers, the smaller first.
    Sum(Vi, Vi),
    /// A pure expression, by its text.
    Expr(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Value {
    Vf(Fv),
    Vi(Vi),
}

/// A value read from memory: where, for stores that may change it.
#[derive(Clone, Debug)]
pub(super) struct Read {
    pub region: Region,
    /// The base register and qword offset, when lowering knows them.
    pub at: Option<(Vi, i64)>,
}

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub key: Key,
    pub value: Value,
    pub reads: Vec<Read>,
    /// The locals the key's expression reads.
    pub locals: Vec<LocalId>,
    /// The range elements its expression reads or addresses: a key names
    /// an element, not where its pointer is.
    pub elements: Vec<ElementId>,
    /// The VF registers its key names for the locals they hold.
    pub regs: Vec<Vf>,
}

/// The entries valid at the point being lowered, each numbered in the
/// order it was made.
#[derive(Clone, Debug, Default)]
pub(super) struct Cache {
    entries: Vec<(u64, Entry)>,
    made: u64,
    /// How many times each VF register has been written, for copies that
    /// name their source while it is unchanged.
    writes: std::collections::HashMap<Vf, u64>,
}

impl Cache {
    pub fn find(&self, key: &Key) -> Option<Value> {
        self.entries
            .iter()
            .rev()
            .find(|(_, entry)| entry.key == *key)
            .map(|(_, entry)| entry.value.clone())
    }

    pub fn insert(&mut self, entry: Entry) {
        self.entries.retain(|(_, old)| old.key != entry.key);
        self.entries.push((self.made, entry));
        self.made += 1;
    }

    /// How many times `reg` has been written.
    pub fn writes(&self, reg: Vf) -> u64 {
        self.writes.get(&reg).copied().unwrap_or(0)
    }

    /// The number the next entry gets.
    pub fn mark(&self) -> u64 {
        self.made
    }

    /// A store of `qwords` qwords to `region`, at a base register and
    /// offset when known.
    pub fn stored(&mut self, program: &Program, region: &Region, at: Option<(Vi, i64, i64)>) {
        self.entries.retain(|(_, entry)| {
            !entry
                .reads
                .iter()
                .any(|read| may_alias(program, region, at, read))
        });
    }

    /// The VF register `reg` written: values it holds from entries made
    /// before `since`, and entries whose key names it, are gone.
    pub fn clobber_vf(&mut self, reg: Vf, since: u64) {
        *self.writes.entry(reg).or_default() += 1;
        self.entries.retain(|(made, entry)| {
            let held = *made < since && matches!(entry.value, Value::Vf(fv) if fv.reg == reg);
            !held && !entry.regs.contains(&reg)
        });
    }

    /// The VI register `reg` written: values it holds from entries made
    /// before `since`, and entries whose key names it, are gone.
    pub fn clobber_vi(&mut self, reg: Vi, since: u64) {
        self.entries.retain(|(made, entry)| {
            let held = *made < since && entry.value == Value::Vi(reg);
            let named = match entry.key {
                Key::Qword { base, .. } | Key::Int16 { base, .. } => base == reg,
                Key::Sum(a, b) => a == reg || b == reg,
                Key::Expr(_) => false,
            };
            !held && !named
        });
    }

    /// Entries whose expressions read these elements are gone: the next
    /// copy of an unrolled body reads other ones.
    pub fn forget_elements(&mut self, elements: &[ElementId]) {
        self.entries.retain(|(_, entry)| {
            !entry
                .elements
                .iter()
                .any(|element| elements.contains(element))
        });
    }

    /// Local `id` written: entries whose expression reads it are gone.
    pub fn clobber_local(&mut self, id: LocalId) {
        self.entries
            .retain(|(_, entry)| !entry.locals.contains(&id));
    }

    /// The entry for `key` is gone.
    pub fn forget_key(&mut self, key: &Key) {
        self.entries.retain(|(_, entry)| entry.key != *key);
    }

    /// Everything read from memory is gone.
    pub fn forget_memory(&mut self) {
        self.entries.retain(|(_, entry)| entry.reads.is_empty());
    }
}

/// Memory a place covers, for alias checks: a parameter and the members
/// from it, array elements as their array. A member that takes lanes
/// stands for the qword it shares with its neighbours.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Region {
    param: usize,
    path: Vec<Step>,
}

impl Region {
    /// Any memory: what a library call may store to.
    pub fn anywhere() -> Self {
        Region {
            param: usize::MAX,
            path: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Member(usize),
    Element,
    /// The qword at this offset in the struct so far.
    Qword(Extent),
}

/// Whether a member of type `ty` takes lanes of a qword rather than
/// whole qwords.
fn shares_qword(ty: &Type) -> bool {
    *ty == Type::Int16 || ty.vf_lanes().is_some_and(|lanes| lanes < 4)
}

/// The members of `path` from struct `id`, with an element step after
/// each array the path goes into, and a member taking lanes as its qword.
fn steps(structs: &[StructDef], mut id: Option<usize>, path: &[usize], out: &mut Vec<Step>) {
    for &index in path {
        let member = id
            .and_then(|id| structs.get(id))
            .and_then(|def| def.members.get(index));
        let Some(member) = member else {
            out.push(Step::Member(index));
            id = None;
            continue;
        };
        if shares_qword(&member.ty) {
            out.push(Step::Qword(member.offset.clone()));
            return;
        }
        out.push(Step::Member(index));
        let mut ty = &member.ty;
        if let Type::Array(element, _) = ty {
            out.push(Step::Element);
            ty = element;
        }
        id = match ty {
            Type::Struct(next) => Some(*next),
            _ => None,
        };
    }
}

fn base_struct(program: &Program, base: Base) -> Option<usize> {
    let ty = match base {
        // A library caller's memory, which the library knows nothing of.
        Base::Param(usize::MAX) => return None,
        Base::Param(index) => &program.params[index].ty,
        Base::Element(element) => &program.elements[element].ty,
        Base::Ref(local) => match &program.locals[local].ty {
            Type::Ref(target) => target,
            ty => ty,
        },
    };
    match ty {
        Type::Struct(id) => Some(*id),
        _ => None,
    }
}

pub(super) fn region(program: &Program, place: &Place) -> Region {
    base_region(program, place.base, &place.path)
}

/// The region of the members `path` from `base`.
fn base_region(program: &Program, base: Base, path: &[usize]) -> Region {
    let mut region = match base {
        Base::Param(usize::MAX) => return Region::anywhere(),
        Base::Param(param) => Region {
            param,
            path: Vec::new(),
        },
        Base::Element(element) => {
            let mut region = region(program, &program.elements[element].array);
            region.path.push(Step::Element);
            region
        }
        // A ref may move anywhere within its memory.
        Base::Ref(local) => {
            return match &program.locals[local].within {
                Some(within) => base_region(program, within.base, &within.path),
                None => Region {
                    param: usize::MAX,
                    path: Vec::new(),
                },
            };
        }
    };
    steps(
        &program.structs,
        base_struct(program, base),
        path,
        &mut region.path,
    );
    while region.path.last() == Some(&Step::Element) {
        region.path.pop();
    }
    region
}

/// Whether two parameters' memory may overlap; `usize::MAX` stands for
/// any memory.
fn params_overlap(program: &Program, a: usize, b: usize) -> bool {
    a == b
        || a == usize::MAX
        || b == usize::MAX
        || program.params[a].alias == Some(b)
        || program.params[b].alias == Some(a)
}

/// Whether a store of `qwords` qwords to `region` (at a base and offset
/// when known) may change a value read at `read`. Distinct members never
/// overlap, and the same base register with different offsets never
/// does.
fn may_alias(program: &Program, region: &Region, at: Option<(Vi, i64, i64)>, read: &Read) -> bool {
    if let (Some((base, offset, qwords)), Some((read_base, read_offset))) = (at, read.at)
        && base == read_base
    {
        return (offset..offset + qwords).contains(&read_offset);
    }
    // A store anywhere, through a ref from a library's caller or by a
    // library call, never writes `uniform` memory, which nothing writes.
    if region.param == usize::MAX
        && read.region.param != usize::MAX
        && program.params[read.region.param].qualifier == Qualifier::Uniform
    {
        return false;
    }
    params_overlap(program, region.param, read.region.param)
        && (region.path.starts_with(&read.region.path)
            || read.region.path.starts_with(&region.path))
}

/// What statements may change: the locals they write, the memory they
/// store to, and whether they yield.
#[derive(Clone, Debug, Default)]
pub(super) struct Effects {
    pub locals: Vec<LocalId>,
    pub stores: Vec<Region>,
    pub yields: bool,
}

impl Effects {
    pub fn of(program: &Program, stmts: &[Stmt]) -> Self {
        let mut effects = Self::default();
        effects.add(program, stmts);
        effects
    }

    pub fn add(&mut self, program: &Program, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::SetLocal { local, .. } => {
                    if !self.locals.contains(local) {
                        self.locals.push(*local);
                    }
                }
                Stmt::Store { place, .. } => self.stores.push(region(program, place)),
                Stmt::Copy { to, .. } => self.stores.push(region(program, to)),
                Stmt::If {
                    then, otherwise, ..
                } => {
                    self.add(program, then);
                    self.add(program, otherwise);
                }
                Stmt::Loop { kind, body, .. } => {
                    if let LoopKind::Condition {
                        cond_stmts, step, ..
                    } = kind
                    {
                        self.add(program, cond_stmts);
                        self.add(program, step);
                    }
                    self.add(program, body);
                }
                Stmt::Yield { .. } => self.yields = true,
                // A library call sets its parameters' locals and results,
                // and may store anywhere.
                Stmt::Call { setup, args, .. } => {
                    self.add(program, setup);
                    for local in args.iter().filter_map(|arg| arg.output) {
                        if !self.locals.contains(&local) {
                            self.locals.push(local);
                        }
                    }
                    self.stores.push(Region::anywhere());
                }
                Stmt::Kick { .. }
                | Stmt::Break { .. }
                | Stmt::Continue { .. }
                | Stmt::Exit { .. }
                | Stmt::Barrier { .. } => {}
            }
        }
    }

    /// Whether `place` may be stored to.
    pub fn stores_to(&self, program: &Program, place: &Place) -> bool {
        let read = Read {
            region: region(program, place),
            at: None,
        };
        self.stores
            .iter()
            .any(|store| may_alias(program, store, None, &read))
    }
}

/// The text of a pure expression, for finding it again; `None` for one
/// with effects (clip judgements, flags) or no value in a register.
pub(super) fn expr_key(expr: &Expr) -> Option<String> {
    expr_key_with(expr, &|_| None)
}

/// A local as it is named in a key: `local`'s name for it (its register,
/// while lowering), or else its number.
pub(super) type LocalName<'a> = &'a dyn Fn(LocalId) -> Option<String>;

/// [`expr_key`], with `local` naming the locals it can: two locals in one
/// register give one key.
pub(super) fn expr_key_with(expr: &Expr, local: LocalName) -> Option<String> {
    let mut text = String::new();
    write_key(expr, &mut text, local).then_some(text)
}

fn write_place(place: &Place, out: &mut String, local: LocalName) -> bool {
    match place.base {
        Base::Param(index) => out.push_str(&format!("P{index}")),
        Base::Element(element) => out.push_str(&format!("E{element}")),
        Base::Ref(local) => out.push_str(&format!("R{local}")),
    }
    out.push_str(&format!(
        "{:?}+{}.{}",
        place.path,
        place.offset.extent.fixed,
        place.lane.letter()
    ));
    for term in &place.offset.extent.terms {
        out.push_str(&format!("+{:?}*{}", term.count, term.stride));
    }
    for (index, stride) in &place.offset.indices {
        out.push_str("+[");
        if !write_key(index, out, local) {
            return false;
        }
        out.push_str(&format!("]*{stride}"));
    }
    out.push_str(&format!(":{:?}", place.ty));
    true
}

fn write_key(expr: &Expr, out: &mut String, local: LocalName) -> bool {
    let args = |out: &mut String, args: &[&Expr]| {
        out.push('(');
        for (index, arg) in args.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            if !write_key(arg, out, local) {
                return false;
            }
        }
        out.push(')');
        true
    };
    match &expr.kind {
        ExprKind::Const(Const::Float(value)) => {
            out.push_str(&format!("f{:08x}", value.to_bits()));
            true
        }
        ExprKind::Const(Const::Int(value)) => {
            out.push_str(&format!("i{value}"));
            true
        }
        ExprKind::Const(Const::Bool(_)) => false,
        ExprKind::Local(id) => {
            match local(*id) {
                Some(name) => out.push_str(&name),
                None => out.push_str(&format!("L{id}")),
            }
            true
        }
        ExprKind::Load(place) => {
            out.push_str("load ");
            write_place(place, out, local)
        }
        ExprKind::Swizzle { base, components } => {
            out.push_str(&format!("swizzle{components:?}"));
            args(out, &[base])
        }
        ExprKind::Neg(operand) => {
            out.push_str("neg");
            args(out, &[operand])
        }
        ExprKind::Binary { op, lhs, rhs } => {
            out.push_str(&format!("{op:?}"));
            args(out, &[lhs, rhs])
        }
        ExprKind::Compare { .. } | ExprKind::Not(_) | ExprKind::And(..) | ExprKind::Or(..) => false,
        ExprKind::Call {
            builtin,
            args: call_args,
        } => {
            if matches!(
                builtin,
                Builtin::ClipW | Builtin::Any | Builtin::Adc | Builtin::Clipped
            ) {
                return false;
            }
            out.push_str(&format!("{builtin:?}"));
            let refs: Vec<&Expr> = call_args.iter().collect();
            args(out, &refs)
        }
        // A matrix is several registers: it is never shared.
        ExprKind::Columns(_) => false,
        ExprKind::Construct(parts) => {
            out.push_str(&format!("construct{:?}", expr.ty));
            let refs: Vec<&Expr> = parts.iter().collect();
            args(out, &refs)
        }
        ExprKind::Convert {
            conversion,
            operand,
        } => {
            out.push_str(&format!("{conversion:?}"));
            args(out, &[operand])
        }
        ExprKind::Column { matrix, index } => {
            out.push_str(&format!("column{index}"));
            args(out, &[matrix])
        }
        ExprKind::Address(place) => {
            out.push_str("address ");
            write_place(place, out, local)
        }
        // The memory a ref stays within does not change its value.
        ExprKind::RefTo { address, .. } => write_key(address, out, local),
    }
}

/// The range elements whose places an expression reads or addresses.
pub(super) fn element_bases(expr: &Expr, out: &mut Vec<ElementId>) {
    let place_bases = |place: &Place, out: &mut Vec<ElementId>| {
        if let Base::Element(element) = place.base
            && !out.contains(&element)
        {
            out.push(element);
        }
        for (index, _) in &place.offset.indices {
            element_bases(index, out);
        }
    };
    match &expr.kind {
        ExprKind::Const(_) | ExprKind::Local(_) => {}
        ExprKind::Load(place) | ExprKind::Address(place) => place_bases(place, out),
        ExprKind::RefTo { address, .. } => element_bases(address, out),
        ExprKind::Swizzle { base: operand, .. }
        | ExprKind::Neg(operand)
        | ExprKind::Not(operand)
        | ExprKind::Convert { operand, .. }
        | ExprKind::Column {
            matrix: operand, ..
        } => element_bases(operand, out),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Compare { lhs, rhs, .. }
        | ExprKind::And(lhs, rhs)
        | ExprKind::Or(lhs, rhs) => {
            element_bases(lhs, out);
            element_bases(rhs, out);
        }
        ExprKind::Call { args, .. } | ExprKind::Construct(args) | ExprKind::Columns(args) => {
            for arg in args {
                element_bases(arg, out);
            }
        }
    }
}

/// The locals a place's address reads: a ref's, and its indices'.
fn place_inputs<'e>(place: &'e Place, locals: &mut Vec<LocalId>, places: &mut Vec<&'e Place>) {
    if let Base::Ref(local) = place.base
        && !locals.contains(&local)
    {
        locals.push(local);
    }
    for (index, _) in &place.offset.indices {
        inputs(index, locals, places);
    }
}

/// The locals an expression reads, and the places it loads.
pub(super) fn inputs<'e>(expr: &'e Expr, locals: &mut Vec<LocalId>, places: &mut Vec<&'e Place>) {
    match &expr.kind {
        ExprKind::Const(_) => {}
        ExprKind::Local(id) => {
            if !locals.contains(id) {
                locals.push(*id);
            }
        }
        ExprKind::Load(place) => {
            places.push(place);
            place_inputs(place, locals, places);
        }
        ExprKind::Address(place) => place_inputs(place, locals, places),
        ExprKind::RefTo { address, .. } => inputs(address, locals, places),
        ExprKind::Swizzle { base: operand, .. }
        | ExprKind::Neg(operand)
        | ExprKind::Not(operand)
        | ExprKind::Convert { operand, .. }
        | ExprKind::Column {
            matrix: operand, ..
        } => inputs(operand, locals, places),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Compare { lhs, rhs, .. }
        | ExprKind::And(lhs, rhs)
        | ExprKind::Or(lhs, rhs) => {
            inputs(lhs, locals, places);
            inputs(rhs, locals, places);
        }
        ExprKind::Call { args, .. } | ExprKind::Construct(args) | ExprKind::Columns(args) => {
            for arg in args {
                inputs(arg, locals, places);
            }
        }
    }
}

/// The expressions in `stmts` worth computing once before a loop whose
/// effects are `effects`: the largest pure parts that read nothing the
/// loop changes and cost an instruction, outside its branches. `bound` says whether a range
/// element's pointer exists outside the loop.
pub(super) fn invariants<'s>(
    program: &Program,
    stmts: &'s [Stmt],
    conds: &[&'s Expr],
    effects: &Effects,
    bound: &dyn Fn(ElementId) -> bool,
    out: &mut Vec<&'s Expr>,
) {
    let finder = Invariants {
        program,
        effects,
        bound,
    };
    finder.stmts(stmts, out);
    for cond in conds {
        finder.expr(cond, out);
    }
}

struct Invariants<'a> {
    program: &'a Program,
    effects: &'a Effects,
    bound: &'a dyn Fn(ElementId) -> bool,
}

impl Invariants<'_> {
    fn stmts<'s>(&self, stmts: &'s [Stmt], out: &mut Vec<&'s Expr>) {
        for stmt in stmts {
            match stmt {
                Stmt::SetLocal { value, .. } | Stmt::Store { value, .. } => self.expr(value, out),
                // What only one branch reads would hold a register on every
                // path for some of them: it stays where it is. What both
                // read is read on every path.
                Stmt::If {
                    cond,
                    then,
                    otherwise,
                    ..
                } => {
                    self.expr(cond, out);
                    let mut first = Vec::new();
                    let mut second = Vec::new();
                    self.stmts(then, &mut first);
                    self.stmts(otherwise, &mut second);
                    let keys: Vec<Option<String>> =
                        second.iter().map(|expr| expr_key(expr)).collect();
                    for expr in first {
                        let key = expr_key(expr);
                        if key.is_some() && keys.contains(&key) {
                            out.push(expr);
                        }
                    }
                }
                Stmt::Loop { kind, body, .. } => {
                    if let LoopKind::Condition {
                        cond_stmts,
                        cond,
                        step,
                        ..
                    } = kind
                    {
                        self.stmts(cond_stmts, out);
                        self.expr(cond, out);
                        self.stmts(step, out);
                    }
                    self.stmts(body, out);
                }
                Stmt::Call { setup, .. } => self.stmts(setup, out),
                Stmt::Copy { .. }
                | Stmt::Kick { .. }
                | Stmt::Break { .. }
                | Stmt::Continue { .. }
                | Stmt::Exit { .. }
                | Stmt::Yield { .. }
                | Stmt::Barrier { .. } => {}
            }
        }
    }

    /// The largest invariant parts of `expr`.
    fn expr<'s>(&self, expr: &'s Expr, out: &mut Vec<&'s Expr>) {
        if self.invariant(expr) {
            if self.worth(expr) {
                out.push(expr);
            }
            return;
        }
        match &expr.kind {
            ExprKind::Const(_) | ExprKind::Local(_) | ExprKind::Load(_) => {}
            ExprKind::Address(place) => {
                for (index, _) in &place.offset.indices {
                    self.expr(index, out);
                }
            }
            ExprKind::RefTo { address, .. } => self.expr(address, out),
            ExprKind::Swizzle { base: operand, .. }
            | ExprKind::Neg(operand)
            | ExprKind::Not(operand)
            | ExprKind::Convert { operand, .. }
            | ExprKind::Column {
                matrix: operand, ..
            } => self.expr(operand, out),
            ExprKind::Binary { op, lhs, rhs } => {
                // An int16 constant added is an immediate, not a register.
                let immediate = |expr: &Expr| {
                    matches!(op, BinOp::IntAdd | BinOp::IntSub)
                        && matches!(expr.kind, ExprKind::Const(Const::Int(_)))
                };
                for operand in [lhs, rhs] {
                    if immediate(operand) {
                        continue;
                    }
                    // A transformed constructor's parts are the operands.
                    match &operand.kind {
                        ExprKind::Construct(parts)
                            if matches!(op, BinOp::MatVec | BinOp::VecMat) =>
                        {
                            for part in parts {
                                self.expr(part, out);
                            }
                        }
                        _ => self.expr(operand, out),
                    }
                }
            }
            ExprKind::Compare { lhs, rhs, .. }
            | ExprKind::And(lhs, rhs)
            | ExprKind::Or(lhs, rhs) => {
                self.expr(lhs, out);
                self.expr(rhs, out);
            }
            ExprKind::Call { args, .. } | ExprKind::Construct(args) | ExprKind::Columns(args) => {
                for arg in args {
                    self.expr(arg, out);
                }
            }
        }
    }

    /// Whether a place's address is the same in every iteration.
    fn address_invariant(&self, place: &Place) -> bool {
        let base = match place.base {
            Base::Param(index) => {
                !matches!(program_param(self.program, index), ParamPlacement::Xitop)
            }
            Base::Element(element) => (self.bound)(element),
            Base::Ref(local) => !self.effects.locals.contains(&local),
        };
        // After a yield, XTOP and the lengths may be another batch's.
        !self.effects.yields
            && base
            && place
                .offset
                .indices
                .iter()
                .all(|(index, _)| self.invariant(index))
    }

    fn place_invariant(&self, place: &Place) -> bool {
        self.address_invariant(place) && !self.effects.stores_to(self.program, place)
    }

    fn invariant(&self, expr: &Expr) -> bool {
        if matches!(expr.ty, Type::Bool | Type::ClipFlags | Type::Void) {
            return false;
        }
        match &expr.kind {
            ExprKind::Const(_) => true,
            ExprKind::Local(id) => !self.effects.locals.contains(id),
            ExprKind::Load(place) => self.place_invariant(place),
            ExprKind::Address(place) => self.address_invariant(place),
            ExprKind::RefTo { address, .. } => self.invariant(address),
            ExprKind::Swizzle { base: operand, .. }
            | ExprKind::Neg(operand)
            | ExprKind::Convert { operand, .. }
            | ExprKind::Column {
                matrix: operand, ..
            } => self.invariant(operand),
            ExprKind::Binary { lhs, rhs, .. } => self.invariant(lhs) && self.invariant(rhs),
            ExprKind::Call { builtin, args } => {
                !matches!(
                    builtin,
                    Builtin::ClipW
                        | Builtin::Any
                        | Builtin::Adc
                        | Builtin::Clipped
                        | Builtin::Xitop
                ) && args.iter().all(|arg| self.invariant(arg))
            }
            ExprKind::Construct(args) => args.iter().all(|arg| self.invariant(arg)),
            // A matrix is not kept in registers across a loop.
            ExprKind::Columns(_) => false,
            ExprKind::Compare { .. } | ExprKind::Not(_) | ExprKind::And(..) | ExprKind::Or(..) => {
                false
            }
        }
    }

    /// Whether lowering `expr` costs an instruction that hoisting saves.
    fn worth(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Const(Const::Float(value)) => *value != 0.0 && *value != 1.0,
            ExprKind::Const(Const::Int(value)) => *value != 0,
            ExprKind::Const(Const::Bool(_)) | ExprKind::Local(_) => false,
            ExprKind::Swizzle { base, .. } => self.worth(base),
            ExprKind::Column { matrix, .. } => self.worth(matrix),
            ExprKind::RefTo { address, .. } => self.worth(address),
            ExprKind::Convert {
                conversion: Conversion::Bits,
                operand,
            } => self.worth(operand),
            // Matrices other than loads are values of several registers.
            _ if matches!(expr.ty, Type::Mat { .. }) => matches!(expr.kind, ExprKind::Load(_)),
            _ => true,
        }
    }
}

fn program_param(program: &Program, index: usize) -> ParamPlacement {
    program.params[index].placement
}
