//! `main`'s body, elaborated: names resolved, every call inlined, each
//! expression typed and placed, and the strict rules applied. No operation
//! may need an instruction the VU does not have (an `int16` multiply, a
//! division by a vector, a select), and an operation's vector operands must
//! be in the same lanes: lanes move only in assignments.
//!
//! Each local carries, as the checker walks the program, the placement of
//! the value it holds and which of its components are set. Two paths that
//! meet (after an `if`, around a loop) must agree on a local's placement,
//! unless it was declared with `layout(lane = ...)`, which makes every
//! assignment move into those lanes.

use std::collections::{HashMap, HashSet};

use super::check::{
    Declarations, STATEMENT_PRAGMAS, const_type_name, constant, int16_range, resolve_type,
};
use super::consteval;
use super::hir::*;
use super::layout::{Extent, Term};
use super::types::{Lane, Length, Placement, Type, builtin_type};
use super::{Checked, Problem, problem};
use crate::diagnostics::VuslMessage;
use crate::syntax::Pos;
use crate::syntax::ast::{self, AssignOp, BinaryOp, ExprKind as Ast, Qualifier, StmtKind, UnaryOp};

/// Elaborates the program.
pub fn program(
    decls: &Declarations,
    params: Vec<Param>,
    default_name: &str,
    warnings: &mut Vec<Problem>,
) -> Checked<Program> {
    let mut elaborator = Elaborator {
        decls,
        names: decls.struct_names(),
        locals: Vec::new(),
        state: Vec::new(),
        elements: Vec::new(),
        scopes: vec![HashMap::new()],
        pending: Vec::new(),
        calls: Vec::new(),
        loops: Vec::new(),
        uses: Uses::default(),
        accesses: Vec::new(),
        access_set: HashSet::new(),
        called: HashSet::new(),
        warnings,
        clips: 0,
        clip_block: 0,
        clip_judgements: HashMap::new(),
        xitop_locals: Vec::new(),
        ranges: 0,
        spills: Vec::new(),
    };
    let mut prologue = Vec::new();
    for (index, param) in params.iter().enumerate() {
        if param.spill {
            elaborator.spills.push(param.name.clone());
            continue;
        }
        let ident = ast::Ident {
            name: param.name.clone(),
            pos: param.pos,
        };
        let symbol = match param.placement {
            ParamPlacement::Xitop => {
                elaborator.uses.xitop = true;
                let local = elaborator.new_local(&param.name, Type::Int16, None, false, param.pos);
                prologue.push(Stmt::SetLocal {
                    local,
                    column: None,
                    components: None,
                    value: Expr {
                        kind: ExprKind::Call {
                            builtin: Builtin::Xitop,
                            args: Vec::new(),
                        },
                        ty: Type::Int16,
                        placement: None,
                        pos: param.pos,
                    },
                    pos: param.pos,
                });
                elaborator.state[local].set = 1;
                elaborator.xitop_locals.push(local);
                Symbol::Local(local)
            }
            placement => {
                if matches!(placement, ParamPlacement::Xtop { .. }) {
                    elaborator.uses.xtop = true;
                }
                Symbol::Memory(Memory {
                    place: Place {
                        base: Base::Param(index),
                        path: Vec::new(),
                        offset: Offset::default(),
                        ty: param.ty.clone(),
                        lane: Lane::X,
                        length: None,
                    },
                    access: param.qualifier,
                    name: param.name.clone(),
                    root: Some((index, Vec::new())),
                })
            }
        };
        elaborator.declare(&ident, symbol)?;
    }
    let mut body = prologue;
    body.extend(elaborator.block(&decls.main.body.stmts)?);
    let mut unused: Vec<&&ast::Function> = decls
        .functions
        .values()
        .filter(|function| !elaborator.called.contains(&function.name.name))
        .collect();
    unused.sort_by_key(|function| function.pos);
    for function in unused {
        elaborator.warnings.push(problem(
            VuslMessage::UnusedFunction {
                name: function.name.name.clone(),
            },
            function.name.pos,
        ));
    }
    Ok(Program {
        name: decls
            .program_name
            .clone()
            .unwrap_or_else(|| default_name.to_string()),
        structs: decls.structs.clone(),
        params,
        locals: elaborator.locals,
        elements: elaborator.elements,
        body,
        uses: elaborator.uses,
        accesses: elaborator.accesses,
    })
}

/// What a name means in a scope.
#[derive(Clone, Debug)]
enum Symbol {
    Local(LocalId),
    Const(Const, Type),
    Memory(Memory),
    /// An inlined function's `out` or `inout` register parameter: the
    /// caller's variable.
    Alias(Target),
}

/// Memory reached by a name: a place, what may be done with it, and the
/// parameter member it is part of, for the reflection.
#[derive(Clone, Debug)]
struct Memory {
    place: Place,
    access: Qualifier,
    name: String,
    root: Option<(usize, Vec<usize>)>,
}

impl Memory {
    fn readable(&self) -> bool {
        self.access != Qualifier::Out
    }

    fn writable(&self) -> bool {
        matches!(self.access, Qualifier::Out | Qualifier::Inout)
    }
}

/// What an assignment writes.
#[derive(Clone, Debug)]
enum Target {
    Local {
        id: LocalId,
        column: Option<usize>,
        components: Option<Vec<usize>>,
    },
    Memory {
        memory: Memory,
        components: Option<Vec<usize>>,
    },
}

/// An expression's result: a value in registers, or memory not read yet.
enum Value {
    Reg(Expr),
    Mem(Memory),
    /// Some components of a vector in memory.
    MemLanes(Memory, Vec<usize>),
}

/// A local's state where the checker is: the placement of the value it
/// holds and which components are set (bit per component, or per column
/// for a matrix).
#[derive(Clone, Copy, Debug, Default)]
struct LocalState {
    placement: Option<Placement>,
    set: u8,
}

/// A loop being checked: the locals' states on entry and wherever a
/// `break` or `continue` leaves.
struct LoopContext {
    entry: Vec<LocalState>,
    exits: Vec<(Pos, Vec<LocalState>)>,
}

struct Elaborator<'d, 'w> {
    decls: &'d Declarations<'d>,
    names: Vec<String>,
    locals: Vec<Local>,
    state: Vec<LocalState>,
    elements: Vec<Element>,
    scopes: Vec<HashMap<String, (Symbol, Pos)>>,
    /// Statements an inlined call produced, ahead of the statement being
    /// elaborated.
    pending: Vec<Stmt>,
    /// The inlined calls being elaborated, innermost last.
    calls: Vec<(String, Pos)>,
    loops: Vec<LoopContext>,
    uses: Uses,
    accesses: Vec<Access>,
    access_set: HashSet<Access>,
    called: HashSet<String>,
    warnings: &'w mut Vec<Problem>,
    /// CLIPW judgements so far, and the block they are in: a judgement can
    /// only be read in its own block, while the clip flags still hold it.
    clips: usize,
    clip_block: usize,
    clip_judgements: HashMap<LocalId, (usize, usize)>,
    /// The locals of `main`'s `layout(xitop)` parameters, read again after
    /// each `yield`.
    xitop_locals: Vec<LocalId>,
    /// The range-fors being elaborated.
    ranges: usize,
    /// `layout(spill)` parameters: oxide-vcl's work area, not the program's.
    spills: Vec<String>,
}

fn err<T>(message: VuslMessage, pos: Pos) -> Checked<T> {
    Err(Box::new(problem(message, pos)))
}

/// The bit mask of a local's components or columns.
fn full_mask(ty: &Type) -> u8 {
    let count = match ty {
        Type::Mat { size, .. } => usize::from(*size),
        ty => ty.vf_lanes().unwrap_or(1),
    };
    ((1u16 << count) - 1) as u8
}

fn mask_of(components: &[usize]) -> u8 {
    components.iter().fold(0, |mask, &c| mask | (1 << c))
}

fn swizzle_components(text: &str) -> Option<Vec<usize>> {
    text.chars()
        .map(|letter| Lane::from_letter(letter).map(Lane::index))
        .collect()
}

/// The placement a memory value of `ty` has at `lane`.
fn memory_placement(ty: &Type, lane: Lane) -> Option<Placement> {
    let lanes = ty.vf_lanes()?;
    Placement::starting_at(lane, lanes)
}

fn constant_expr(value: Const, pos: Pos) -> Expr {
    let ty = match value {
        Const::Int(_) => Type::Int16,
        Const::Float(_) => Type::Float,
        Const::Bool(_) => Type::Bool,
    };
    Expr {
        kind: ExprKind::Const(value),
        ty,
        placement: None,
        pos,
    }
}

fn as_const(expr: &Expr) -> Option<Const> {
    match expr.kind {
        ExprKind::Const(value) => Some(value),
        _ => None,
    }
}

/// The lanes letters of a value, for messages: `xyz`, or `a scalar`.
fn lanes_text(expr: &Expr) -> String {
    match expr.placement {
        Some(placement) => placement.letters(),
        None => "(a scalar)".into(),
    }
}

impl Elaborator<'_, '_> {
    fn type_name(&self, ty: &Type) -> String {
        ty.name(&self.names)
    }

    fn type_text(&self, expr: &Expr) -> String {
        match (&expr.ty, expr.placement) {
            (ty, Some(placement)) if ty.vf_lanes().is_some_and(|lanes| lanes > 1) => {
                format!("{} (lanes {})", self.type_name(ty), placement.letters())
            }
            (ty, _) => self.type_name(ty),
        }
    }

    fn new_local(
        &mut self,
        name: &str,
        ty: Type,
        fixed: Option<Placement>,
        precise: bool,
        pos: Pos,
    ) -> LocalId {
        self.locals.push(Local {
            name: name.to_string(),
            ty,
            fixed,
            precise,
            within: None,
            pos,
        });
        self.state.push(LocalState {
            placement: fixed,
            set: 0,
        });
        self.locals.len() - 1
    }

    fn declare(&mut self, name: &ast::Ident, symbol: Symbol) -> Checked<()> {
        let scope = self.scopes.last_mut().expect("a scope");
        if let Some((_, first)) = scope.get(&name.name) {
            return Err(Box::new(
                problem(
                    VuslMessage::Redeclared {
                        name: name.name.clone(),
                    },
                    name.pos,
                )
                .with_note(
                    *first,
                    VuslMessage::FirstDeclaredHere {
                        name: name.name.clone(),
                    },
                ),
            ));
        }
        scope.insert(name.name.clone(), (symbol, name.pos));
        Ok(())
    }

    fn lookup(&self, name: &ast::Ident) -> Checked<Symbol> {
        for scope in self.scopes.iter().rev() {
            if let Some((symbol, _)) = scope.get(&name.name) {
                return Ok(symbol.clone());
            }
        }
        if let Some((value, ty)) = self.decls.consts.get(&name.name) {
            return Ok(Symbol::Const(*value, ty.clone()));
        }
        if self.spills.contains(&name.name) {
            return err(
                VuslMessage::SpillUsed {
                    name: name.name.clone(),
                },
                name.pos,
            );
        }
        err(
            VuslMessage::UnknownName {
                name: name.name.clone(),
            },
            name.pos,
        )
    }

    fn record(&mut self, memory: &Memory, write: bool) {
        if let Some((param, path)) = &memory.root {
            let access = Access {
                param: *param,
                path: path.clone(),
                write,
            };
            if self.access_set.insert(access.clone()) {
                self.accesses.push(access);
            }
        }
    }

    // ----------------------------------------------
    // Statements
    // ----------------------------------------------

    fn block(&mut self, stmts: &[ast::Stmt]) -> Checked<Vec<Stmt>> {
        self.scopes.push(HashMap::new());
        let result = self.statements(stmts);
        self.scopes.pop();
        result
    }

    fn statements(&mut self, stmts: &[ast::Stmt]) -> Checked<Vec<Stmt>> {
        let mut out = Vec::new();
        let mut pragmas: Vec<&ast::Pragma> = Vec::new();
        for stmt in stmts {
            if let StmtKind::Pragma(pragma) = &stmt.kind {
                let name = pragma.name.name.as_str();
                if name == "barrier" {
                    if pragma.args.is_some() {
                        return err(
                            VuslMessage::BadPragma {
                                name: name.into(),
                                expected: "",
                            },
                            pragma.pos,
                        );
                    }
                    out.push(Stmt::Barrier { pos: pragma.pos });
                } else if STATEMENT_PRAGMAS.contains(&name) {
                    pragmas.push(pragma);
                } else if name == "program" {
                    return err(
                        VuslMessage::MisplacedPragma {
                            name: name.into(),
                            place: "names the program: it belongs at file scope",
                        },
                        pragma.pos,
                    );
                } else {
                    self.warnings.push(problem(
                        VuslMessage::UnknownPragma { name: name.into() },
                        pragma.pos,
                    ));
                }
                continue;
            }
            let hints = self.loop_hints(&pragmas, stmt)?;
            pragmas.clear();
            self.stmt(stmt, hints, &mut out)?;
        }
        if let Some(pragma) = pragmas.first() {
            return err(
                VuslMessage::MisplacedPragma {
                    name: pragma.name.name.clone(),
                    place: "must come right before a loop",
                },
                pragma.pos,
            );
        }
        Ok(out)
    }

    /// A pragma argument's value as a non-negative integer constant.
    fn pragma_int(&self, arg: &ast::Arg) -> Option<u32> {
        match constant(&arg.value, &self.decls.consts)? {
            Const::Int(value) => u32::try_from(value).ok(),
            _ => None,
        }
    }

    fn loop_hints(&mut self, pragmas: &[&ast::Pragma], stmt: &ast::Stmt) -> Checked<LoopHints> {
        let mut hints = LoopHints::default();
        if pragmas.is_empty() {
            return Ok(hints);
        }
        let is_loop = matches!(
            stmt.kind,
            StmtKind::For { .. }
                | StmtKind::RangeFor { .. }
                | StmtKind::While { .. }
                | StmtKind::DoWhile { .. }
        );
        for pragma in pragmas {
            let name = pragma.name.name.clone();
            if !is_loop {
                return err(
                    VuslMessage::MisplacedPragma {
                        name,
                        place: "must come right before a loop",
                    },
                    pragma.pos,
                );
            }
            let bad = |expected| {
                err(
                    VuslMessage::BadPragma {
                        name: name.clone(),
                        expected,
                    },
                    pragma.pos,
                )
            };
            match (name.as_str(), pragma.args.as_deref()) {
                ("unroll", None) => {
                    if matches!(stmt.kind, StmtKind::While { .. } | StmtKind::DoWhile { .. }) {
                        return err(
                            VuslMessage::MisplacedPragma {
                                name,
                                place: "must come before a for loop with constant bounds or a range-for",
                            },
                            pragma.pos,
                        );
                    }
                    hints.unroll = Some(Unroll::Full);
                }
                ("unroll", Some([arg])) if arg.name.is_none() => {
                    if !matches!(stmt.kind, StmtKind::RangeFor { .. }) {
                        return err(
                            VuslMessage::MisplacedPragma {
                                name,
                                place: "with a count must come before a range-for (#pragma unroll alone unrolls a for loop with constant bounds)",
                            },
                            pragma.pos,
                        );
                    }
                    match self
                        .pragma_int(arg)
                        .filter(|count| (1..=MAX_UNROLL).contains(count))
                    {
                        Some(count) => hints.unroll = Some(Unroll::By(count)),
                        None => {
                            return bad(" or #pragma unroll(N), with N a constant from 1 to 16");
                        }
                    }
                }
                ("nounroll", None) => hints.unroll = Some(Unroll::Never),
                ("nopipeline", None) => hints.pipeline = Some(Pipeline::Off),
                ("pipeline", args) => {
                    let (mut min, mut safe, mut extra, mut abs) = (1, 0, None, None);
                    for arg in args.unwrap_or_default() {
                        let value = self.pragma_int(arg);
                        match (arg.name.as_ref().map(|name| name.name.as_str()), value) {
                            (Some("min"), Some(value)) if value >= 1 => min = value,
                            (Some("safe"), Some(value)) => safe = value,
                            (Some("extra"), Some(value)) => extra = Some(value),
                            (Some("abs"), Some(value)) => abs = Some(value),
                            _ => {
                                return bad(
                                    "(min = C, safe = S, extra = E, abs = A), each optional",
                                );
                            }
                        }
                    }
                    hints.pipeline = Some(Pipeline::Hint {
                        min,
                        safe,
                        extra,
                        abs,
                    });
                }
                ("unroll" | "nounroll" | "nopipeline", _) => return bad(""),
                _ => {}
            }
        }
        Ok(hints)
    }

    /// Statements inlined calls produced, then the statement itself.
    fn emit(&mut self, out: &mut Vec<Stmt>, stmt: Stmt) {
        out.append(&mut self.pending);
        out.push(stmt);
    }

    fn stmt(&mut self, stmt: &ast::Stmt, hints: LoopHints, out: &mut Vec<Stmt>) -> Checked<()> {
        match &stmt.kind {
            StmtKind::Block(block) => {
                let inner = self.block(&block.stmts)?;
                out.extend(inner);
            }
            StmtKind::Pragma(_) | StmtKind::Empty => {}
            StmtKind::Decl(decl) => self.decl(decl, out)?,
            StmtKind::Expr(expr) => self.expr_stmt(expr, out)?,
            StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = self.condition(cond)?;
                out.append(&mut self.pending);
                let before = self.state.clone();
                let then = self.branch(then)?;
                let after_then = std::mem::replace(&mut self.state, before);
                let otherwise = match otherwise {
                    Some(otherwise) => self.branch(otherwise)?,
                    None => Vec::new(),
                };
                let after_else = std::mem::take(&mut self.state);
                self.state = self.merge(&after_then, &after_else, stmt.pos)?;
                out.push(Stmt::If {
                    cond,
                    then,
                    otherwise,
                    pos: stmt.pos,
                });
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.scopes.push(HashMap::new());
                let result = self.for_loop(
                    init.as_deref(),
                    cond.as_ref(),
                    step.as_ref(),
                    body,
                    hints,
                    stmt.pos,
                    out,
                );
                self.scopes.pop();
                result?;
            }
            StmtKind::RangeFor { ranges, body } => {
                self.range_for(ranges, body, hints, stmt.pos, out)?
            }
            StmtKind::While { cond, body } => {
                let looped =
                    self.condition_loop(Some(cond), body, Vec::new(), true, hints, stmt.pos)?;
                out.push(looped);
            }
            StmtKind::DoWhile { body, cond } => {
                let looped =
                    self.condition_loop(Some(cond), body, Vec::new(), false, hints, stmt.pos)?;
                out.push(looped);
            }
            StmtKind::Break | StmtKind::Continue => {
                let is_break = matches!(stmt.kind, StmtKind::Break);
                let state = self.state.clone();
                let Some(context) = self.loops.last_mut() else {
                    let message = if is_break {
                        VuslMessage::BreakOutsideLoop
                    } else {
                        VuslMessage::ContinueOutsideLoop
                    };
                    return err(message, stmt.pos);
                };
                context.exits.push((stmt.pos, state));
                out.push(if is_break {
                    Stmt::Break { pos: stmt.pos }
                } else {
                    Stmt::Continue { pos: stmt.pos }
                });
            }
            StmtKind::Return(value) => {
                if !self.calls.is_empty() {
                    // A function's last return is taken before its body is
                    // elaborated, so any other is too early.
                    return err(VuslMessage::ReturnNotLast, stmt.pos);
                }
                if value.is_some() {
                    return err(
                        VuslMessage::ReturnMismatch {
                            name: "main".into(),
                        },
                        stmt.pos,
                    );
                }
                out.push(Stmt::Exit { pos: stmt.pos });
            }
            StmtKind::Yield => {
                if !self.calls.is_empty() {
                    return err(VuslMessage::YieldOutsideMain, stmt.pos);
                }
                if self.ranges > 0 {
                    return err(VuslMessage::YieldInRange, stmt.pos);
                }
                self.uses.yield_ = true;
                out.push(Stmt::Yield { pos: stmt.pos });
                // MSCNT resumes with another batch, and XITOP its count.
                for &local in &self.xitop_locals {
                    out.push(Stmt::SetLocal {
                        local,
                        column: None,
                        components: None,
                        value: Expr {
                            kind: ExprKind::Call {
                                builtin: Builtin::Xitop,
                                args: Vec::new(),
                            },
                            ty: Type::Int16,
                            placement: None,
                            pos: stmt.pos,
                        },
                        pos: stmt.pos,
                    });
                }
            }
        }
        Ok(())
    }

    /// A branch of an `if`, in a scope of its own.
    fn branch(&mut self, stmt: &ast::Stmt) -> Checked<Vec<Stmt>> {
        // A branch or a loop body is a block of its own, for the clip flags.
        self.clip_block += 1;
        self.scopes.push(HashMap::new());
        let mut out = Vec::new();
        let result = self.stmt(stmt, LoopHints::default(), &mut out);
        self.scopes.pop();
        self.clip_block += 1;
        result.map(|()| out)
    }

    /// Two paths' local states where they meet.
    fn merge(&self, a: &[LocalState], b: &[LocalState], pos: Pos) -> Checked<Vec<LocalState>> {
        let mut merged = Vec::with_capacity(a.len().max(b.len()));
        for (index, local) in self.locals.iter().enumerate().take(a.len().max(b.len())) {
            let (first, second) = (a.get(index).copied(), b.get(index).copied());
            let state = match (first, second) {
                (Some(first), Some(second)) => {
                    if let (Some(p), Some(q)) = (first.placement, second.placement)
                        && p != q
                        && local.fixed.is_none()
                    {
                        return err(
                            VuslMessage::PlacementMerge {
                                name: local.name.clone(),
                                first: p.letters(),
                                second: q.letters(),
                            },
                            pos,
                        );
                    }
                    LocalState {
                        placement: first.placement.or(second.placement),
                        set: first.set & second.set,
                    }
                }
                (Some(only), None) | (None, Some(only)) => only,
                (None, None) => LocalState::default(),
            };
            merged.push(state);
        }
        Ok(merged)
    }

    /// Checks a loop's body ends (and its exits) against its entry, and
    /// leaves the state the loop's exit has: the entry's when the body may
    /// not run.
    fn finish_loop(&mut self, context: LoopContext, runs_once: bool, pos: Pos) -> Checked<()> {
        let end = std::mem::take(&mut self.state);
        let mut after = self.merge(&context.entry, &end, pos)?;
        for (exit_pos, exit) in &context.exits {
            after = self.merge(&after, exit, *exit_pos)?;
        }
        if runs_once {
            for (index, state) in after.iter_mut().enumerate() {
                state.set = end.get(index).map_or(0, |end| end.set);
                for (_, exit) in &context.exits {
                    state.set &= exit.get(index).map_or(0, |exit| exit.set);
                }
            }
        } else {
            for (index, state) in after.iter_mut().enumerate() {
                state.set = context.entry.get(index).map_or(0, |entry| entry.set);
            }
        }
        self.state = after;
        Ok(())
    }

    fn condition_loop(
        &mut self,
        cond: Option<&ast::Expr>,
        body: &ast::Stmt,
        step: Vec<Stmt>,
        test_first: bool,
        hints: LoopHints,
        pos: Pos,
    ) -> Checked<Stmt> {
        self.loops.push(LoopContext {
            entry: self.state.clone(),
            exits: Vec::new(),
        });
        let source = cond;
        let result = (|| -> Checked<(Vec<Stmt>, Expr, Vec<Stmt>)> {
            if test_first {
                let cond = self.optional_condition(source, pos)?;
                let cond_stmts = std::mem::take(&mut self.pending);
                let body = self.branch(body)?;
                Ok((cond_stmts, cond, body))
            } else {
                let mut body = self.branch(body)?;
                let cond = self.optional_condition(source, pos)?;
                body.append(&mut self.pending);
                Ok((Vec::new(), cond, body))
            }
        })();
        let context = self.loops.pop().expect("the loop's context");
        let (cond_stmts, cond, body) = result?;
        self.finish_loop(context, !test_first, pos)?;
        Ok(Stmt::Loop {
            kind: LoopKind::Condition {
                cond_stmts,
                cond,
                test_first,
                step,
            },
            body,
            hints,
            pos,
        })
    }

    fn optional_condition(&mut self, cond: Option<&ast::Expr>, pos: Pos) -> Checked<Expr> {
        match cond {
            Some(cond) => self.condition(cond),
            None => Ok(constant_expr(Const::Bool(true), pos)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn for_loop(
        &mut self,
        init: Option<&ast::Stmt>,
        cond: Option<&ast::Expr>,
        step: Option<&ast::Expr>,
        body: &ast::Stmt,
        hints: LoopHints,
        pos: Pos,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        if hints.unroll == Some(Unroll::Full) {
            return self.unrolled(init, cond, step, body, pos, out);
        }
        if let Some(init) = init {
            self.stmt(init, LoopHints::default(), out)?;
        }
        // The step is elaborated after the body, at its end.
        self.loops.push(LoopContext {
            entry: self.state.clone(),
            exits: Vec::new(),
        });
        type Parts = (Vec<Stmt>, Expr, Vec<Stmt>, Vec<Stmt>);
        let result = (|| -> Checked<Parts> {
            let cond = self.optional_condition(cond, pos)?;
            let cond_stmts = std::mem::take(&mut self.pending);
            let body = self.branch(body)?;
            let mut step_stmts = Vec::new();
            if let Some(step) = step {
                self.expr_stmt(step, &mut step_stmts)?;
            }
            Ok((cond_stmts, cond, body, step_stmts))
        })();
        let context = self.loops.pop().expect("the loop's context");
        let (cond_stmts, cond, body, step) = result?;
        self.finish_loop(context, false, pos)?;
        out.push(Stmt::Loop {
            kind: LoopKind::Condition {
                cond_stmts,
                cond,
                test_first: true,
                step,
            },
            body,
            hints,
            pos,
        });
        Ok(())
    }

    /// `#pragma unroll` on `for (int16 i = A; i < B; ++i)` with constants:
    /// the body once per value of `i`, with `i` a constant in each.
    fn unrolled(
        &mut self,
        init: Option<&ast::Stmt>,
        cond: Option<&ast::Expr>,
        step: Option<&ast::Expr>,
        body: &ast::Stmt,
        pos: Pos,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        let bad = || {
            err(
                VuslMessage::BadPragma {
                    name: "unroll".into(),
                    expected: " before for (int16 i = A; i < B; ++i), with A and B constants",
                },
                pos,
            )
        };
        let decls = self.decls;
        let consts = &decls.consts;
        let Some(ast::Stmt {
            kind: StmtKind::Decl(decl),
            ..
        }) = init
        else {
            return bad();
        };
        let [declarator] = decl.declarators.as_slice() else {
            return bad();
        };
        let (Some(start), "int16") = (
            declarator
                .init
                .as_ref()
                .and_then(|init| constant(init, consts)),
            decl.ty.name.name.as_str(),
        ) else {
            return bad();
        };
        let Const::Int(start) = start else {
            return bad();
        };
        let variable = &declarator.name.name;
        let Some(ast::Expr {
            kind: Ast::Binary { op, lhs, rhs, .. },
            ..
        }) = cond
        else {
            return bad();
        };
        let is_variable =
            |expr: &ast::Expr| matches!(&expr.kind, Ast::Name(name) if &name.name == variable);
        let Some(Const::Int(end)) = constant(rhs, consts) else {
            return bad();
        };
        if !is_variable(lhs) {
            return bad();
        }
        let delta = match step.map(|step| &step.kind) {
            Some(Ast::Unary {
                op: UnaryOp::PreIncrement,
                operand,
            })
            | Some(Ast::Postfix {
                op: ast::PostfixOp::Increment,
                operand,
            }) if is_variable(operand) => 1,
            Some(Ast::Unary {
                op: UnaryOp::PreDecrement,
                operand,
            })
            | Some(Ast::Postfix {
                op: ast::PostfixOp::Decrement,
                operand,
            }) if is_variable(operand) => -1,
            Some(Ast::Assign {
                op: AssignOp::Compound(direction @ (BinaryOp::Add | BinaryOp::Sub)),
                target,
                value,
            }) if is_variable(target) => match constant(value, consts) {
                Some(Const::Int(amount)) if amount != 0 => {
                    if *direction == BinaryOp::Add {
                        amount
                    } else {
                        -amount
                    }
                }
                _ => return bad(),
            },
            _ => return bad(),
        };
        let mut values = Vec::new();
        let mut value = start;
        loop {
            let going = match op {
                BinaryOp::Less => value < end,
                BinaryOp::LessEqual => value <= end,
                BinaryOp::Greater => value > end,
                BinaryOp::GreaterEqual => value >= end,
                BinaryOp::NotEqual => value != end,
                _ => return bad(),
            };
            if !going {
                break;
            }
            if values.len() == 1024 {
                return bad();
            }
            values.push(value);
            value += delta;
        }
        for value in values {
            self.scopes.push(HashMap::new());
            let result = self
                .declare(
                    &declarator.name,
                    Symbol::Const(Const::Int(value), Type::Int16),
                )
                .and_then(|()| self.branch(body));
            self.scopes.pop();
            out.extend(result?);
        }
        Ok(())
    }

    fn range_for(
        &mut self,
        ranges: &[ast::Range],
        body: &ast::Stmt,
        hints: LoopHints,
        pos: Pos,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        let mut elements = Vec::new();
        let mut count: Option<Count> = None;
        let mut first_length: Option<Length> = None;
        let mut bindings = Vec::new();
        for (position, range) in ranges.iter().enumerate() {
            let memory = match self.value(&range.source)? {
                Value::Mem(memory) => memory,
                Value::Reg(expr) => {
                    return err(
                        VuslMessage::RangeNotArray {
                            found: self.type_text(&expr),
                        },
                        range.source.pos,
                    );
                }
                Value::MemLanes(..) => {
                    return err(
                        VuslMessage::RangeNotArray {
                            found: "a swizzle".into(),
                        },
                        range.source.pos,
                    );
                }
            };
            let Type::Array(element_ty, length) = memory.place.ty.clone() else {
                return err(
                    VuslMessage::RangeNotArray {
                        found: self.type_name(&memory.place.ty),
                    },
                    range.source.pos,
                );
            };
            let declared = resolve_type(&range.ty, &self.decls.struct_ids)?;
            if declared != *element_ty {
                return err(
                    VuslMessage::RangeElementType {
                        expected: self.type_name(&element_ty),
                        found: self.type_name(&declared),
                    },
                    range.ty.pos,
                );
            }
            let access = range
                .qualifier
                .map_or(Qualifier::In, |(qualifier, _)| qualifier);
            let allowed = match access {
                Qualifier::In | Qualifier::Uniform => memory.readable(),
                Qualifier::Out => memory.writable(),
                Qualifier::Inout => memory.readable() && memory.writable(),
            };
            if !allowed {
                let message = if memory.writable() {
                    VuslMessage::WriteOnly {
                        name: memory.name.clone(),
                    }
                } else {
                    VuslMessage::ReadOnly {
                        name: memory.name.clone(),
                    }
                };
                return err(message, range.source.pos);
            }
            if position == 0 {
                count = Some(match &length {
                    Length::Fixed(n) => Count::Fixed(*n),
                    Length::Member(_) => {
                        let length_place = memory.place.length.clone().map(|place| *place);
                        let Some(length_place) = length_place else {
                            return err(VuslMessage::RangeOpen, range.source.pos);
                        };
                        // The loop reads its count: a member beside the
                        // array, in the struct that holds both.
                        if let (Some((param, array_path)), Length::Member(count)) =
                            (&memory.root, &length)
                        {
                            let holder = &array_path[..array_path.len().saturating_sub(1)];
                            let access = Access {
                                param: *param,
                                path: holder.iter().chain(count).copied().collect(),
                                write: false,
                            };
                            if self.access_set.insert(access.clone()) {
                                self.accesses.push(access);
                            }
                        }
                        Count::Member(length_place)
                    }
                    Length::Xitop => Count::Xitop,
                    Length::Open => return err(VuslMessage::RangeOpen, range.source.pos),
                });
                first_length = Some(length.clone());
            } else if length != Length::Open
                && (Some(&length) != first_length.as_ref()
                    || !same_length_place(&memory.place, &elements, &self.elements))
            {
                return err(VuslMessage::RangeLengths, range.source.pos);
            }
            let stride = element_qwords(&element_ty, &self.decls.structs);
            let id = self.elements.len();
            self.elements.push(Element {
                name: range.name.name.clone(),
                ty: (*element_ty).clone(),
                array: memory.place.clone(),
                access,
                stride,
                pos: range.pos,
            });
            elements.push(id);
            bindings.push((
                range.name.clone(),
                Memory {
                    place: Place {
                        base: Base::Element(id),
                        path: Vec::new(),
                        offset: Offset::default(),
                        ty: (*element_ty).clone(),
                        lane: Lane::X,
                        length: None,
                    },
                    access,
                    name: range.name.name.clone(),
                    root: memory.root.clone(),
                },
            ));
        }
        out.append(&mut self.pending);
        self.scopes.push(HashMap::new());
        let result = (|| -> Checked<Vec<Stmt>> {
            for (name, memory) in bindings {
                self.declare(&name, Symbol::Memory(memory))?;
            }
            self.loops.push(LoopContext {
                entry: self.state.clone(),
                exits: Vec::new(),
            });
            self.ranges += 1;
            let body = self.branch(body);
            self.ranges -= 1;
            let context = self.loops.pop().expect("the loop's context");
            let body = body?;
            self.finish_loop(context, false, pos)?;
            Ok(body)
        })();
        self.scopes.pop();
        let body = result?;
        let count = count.expect("a first range");
        // Unrolled, the body is repeated: a break or continue has nowhere
        // to go, and a count read at run time splits only by a power of
        // two.
        let unrolled = match hints.unroll {
            Some(Unroll::By(n)) if n > 1 => Some(n),
            Some(Unroll::Full) => match count {
                Count::Fixed(n) => Some(n),
                _ => {
                    return err(
                        VuslMessage::BadPragma {
                            name: "unroll".into(),
                            expected: "(N) before a range-for whose length is read at run time",
                        },
                        pos,
                    );
                }
            },
            _ => None,
        };
        if let Some(factor) = unrolled {
            if leaves_loop(&body) {
                return err(VuslMessage::UnrolledLoopLeaves, pos);
            }
            if !matches!(count, Count::Fixed(_)) && !factor.is_power_of_two() {
                return err(
                    VuslMessage::BadPragma {
                        name: "unroll".into(),
                        expected: "(N) with N a power of two, for a length read at run time",
                    },
                    pos,
                );
            }
        }
        out.push(Stmt::Loop {
            kind: LoopKind::Range { elements, count },
            body,
            hints,
            pos,
        });
        Ok(())
    }

    fn decl(&mut self, decl: &ast::Decl, out: &mut Vec<Stmt>) -> Checked<()> {
        let ty = resolve_type(&decl.ty, &self.decls.struct_ids)?;
        let mut fixed_lane = None;
        for arg in decl.layout.iter().flat_map(|layout| &layout.args) {
            let lane = match (
                arg.name.as_ref().map(|name| name.name.as_str()),
                &arg.value.kind,
            ) {
                (Some("lane"), Ast::Name(letter)) => {
                    let mut chars = letter.name.chars();
                    match (chars.next().and_then(Lane::from_letter), chars.next()) {
                        (Some(lane), None) => Some(lane),
                        _ => None,
                    }
                }
                _ => None,
            };
            match lane {
                Some(lane) => fixed_lane = Some(lane),
                None => {
                    return err(
                        VuslMessage::UnknownLayout {
                            name: crate::syntax::print::expr(&arg.value),
                            context: "a variable (only lane = x, y, z or w)",
                        },
                        arg.pos,
                    );
                }
            }
        }
        match &ty {
            Type::Struct(_) => {
                return err(
                    VuslMessage::NotYet {
                        what: "a struct variable in registers",
                    },
                    decl.ty.pos,
                );
            }
            Type::Void => {
                return err(
                    VuslMessage::TypeMismatch {
                        context: "a variable".into(),
                        expected: "a type with values".into(),
                        found: "void".into(),
                    },
                    decl.ty.pos,
                );
            }
            _ => {}
        }
        let fixed = match fixed_lane {
            None => None,
            Some(lane) => {
                let lanes = ty.vf_lanes().filter(|_| !matches!(ty, Type::Qword));
                let placement = lanes.and_then(|lanes| Placement::starting_at(lane, lanes));
                match placement {
                    Some(placement) => Some(placement),
                    None => {
                        return err(
                            VuslMessage::BadLayoutValue {
                                name: "lane".into(),
                                expected: "a lane the variable's components fit from",
                            },
                            decl.pos,
                        );
                    }
                }
            }
        };
        for declarator in &decl.declarators {
            if declarator.array.is_some() {
                return err(
                    VuslMessage::ArrayNotAllowed {
                        name: declarator.name.name.clone(),
                    },
                    declarator.name.pos,
                );
            }
            if decl.constant {
                let Some(init) = &declarator.init else {
                    return err(
                        VuslMessage::NotConstant {
                            name: declarator.name.name.clone(),
                        },
                        declarator.name.pos,
                    );
                };
                let value = self.rvalue(init)?;
                let value = self.coerce(value, &ty, "the constant")?;
                let Some(value) = as_const(&value) else {
                    return err(
                        VuslMessage::NotConstant {
                            name: declarator.name.name.clone(),
                        },
                        declarator.name.pos,
                    );
                };
                self.declare(&declarator.name, Symbol::Const(value, ty.clone()))?;
                continue;
            }
            // A matrix takes its initializer's storage, as a vector takes
            // its lanes: `mat4 m = frame.mvp;` holds the rows of a
            // row_major matrix.
            let value = match &declarator.init {
                Some(init) => Some(self.rvalue(init)?),
                None => None,
            };
            let local_ty = match (&ty, value.as_ref().map(|value| &value.ty)) {
                (
                    Type::Mat { size, .. },
                    Some(Type::Mat {
                        size: other,
                        row_major,
                    }),
                ) if size == other => Type::Mat {
                    size: *size,
                    row_major: *row_major,
                },
                _ => ty.clone(),
            };
            let local = self.new_local(
                &declarator.name.name,
                local_ty,
                fixed,
                decl.precise,
                declarator.name.pos,
            );
            if let (Some(init), Some(value)) = (&declarator.init, value) {
                let target = Target::Local {
                    id: local,
                    column: None,
                    components: None,
                };
                let stmt = self.set(target, value, init.pos)?;
                self.emit(out, stmt);
            }
            // The name is visible after its initializer, as in C.
            self.declare(&declarator.name, Symbol::Local(local))?;
        }
        Ok(())
    }

    fn expr_stmt(&mut self, expr: &ast::Expr, out: &mut Vec<Stmt>) -> Checked<()> {
        match &expr.kind {
            Ast::Assign { op, target, value } => {
                let stmt = self.assign(*op, target, value, expr.pos)?;
                if let Some(stmt) = stmt {
                    self.emit(out, stmt);
                }
            }
            Ast::Unary {
                op: op @ (UnaryOp::PreIncrement | UnaryOp::PreDecrement),
                operand,
            } => {
                let stmt = self.increment(operand, *op == UnaryOp::PreIncrement, expr.pos)?;
                self.emit(out, stmt);
            }
            Ast::Postfix { op, operand } => {
                let up = *op == ast::PostfixOp::Increment;
                let stmt = self.increment(operand, up, expr.pos)?;
                self.emit(out, stmt);
            }
            Ast::Call { callee, args }
                if callee.name.name == "xgkick" && callee.argument.is_none() =>
            {
                let [arg] = args.as_slice() else {
                    return err(
                        VuslMessage::WrongArgumentCount {
                            name: "xgkick".into(),
                            expected: "1 argument".into(),
                            found: args.len(),
                        },
                        expr.pos,
                    );
                };
                let value = self.value(arg)?;
                let memory = match self.deref(value, false)? {
                    Value::Mem(memory) => memory,
                    _ => {
                        return err(
                            VuslMessage::TypeMismatch {
                                context: "xgkick".into(),
                                expected: "a packet in memory".into(),
                                found: "a value in registers".into(),
                            },
                            arg.pos,
                        );
                    }
                };
                self.uses.xgkick = true;
                self.emit(
                    out,
                    Stmt::Kick {
                        place: memory.place,
                        pos: expr.pos,
                    },
                );
            }
            Ast::Call { callee, args }
                if callee.argument.is_none()
                    && self.decls.functions.contains_key(&callee.name.name) =>
            {
                let function = self.decls.functions[&callee.name.name];
                self.inline(function, args, expr.pos)?;
                out.append(&mut self.pending);
            }
            _ => {
                self.rvalue(expr)?;
                out.append(&mut self.pending);
            }
        }
        Ok(())
    }

    // ----------------------------------------------
    // Assignments
    // ----------------------------------------------

    fn target(&mut self, expr: &ast::Expr) -> Checked<Target> {
        match &expr.kind {
            Ast::Name(name) => match self.lookup(name)? {
                Symbol::Local(id) => Ok(Target::Local {
                    id,
                    column: None,
                    components: None,
                }),
                Symbol::Memory(memory) => Ok(Target::Memory {
                    memory,
                    components: None,
                }),
                Symbol::Alias(target) => Ok(target),
                Symbol::Const(..) => {
                    err(VuslMessage::NotAssignable { what: "a constant" }, expr.pos)
                }
            },
            Ast::Field { base, field } => {
                // A struct member in memory, or components of a vector; a
                // ref's members are memory.
                if let Ok(Target::Local {
                    id,
                    column,
                    components: None,
                }) = self.target_no_member(base)
                    && !matches!(self.locals[id].ty, Type::Ref(_))
                {
                    let ty = match column {
                        Some(_) => self.column_type(&self.locals[id].ty),
                        None => self.locals[id].ty.clone(),
                    };
                    let components = self.swizzle(&field.name, &ty, field.pos, false)?;
                    return Ok(Target::Local {
                        id,
                        column,
                        components: Some(components),
                    });
                }
                match self.value(expr)? {
                    Value::Mem(memory) => Ok(Target::Memory {
                        memory,
                        components: None,
                    }),
                    Value::MemLanes(memory, components) => Ok(Target::Memory {
                        memory,
                        components: Some(components),
                    }),
                    Value::Reg(_) => err(
                        VuslMessage::NotAssignable {
                            what: "a computed value",
                        },
                        expr.pos,
                    ),
                }
            }
            Ast::Index { base, index } => {
                if let Ok(Target::Local {
                    id,
                    column: None,
                    components: None,
                }) = self.target_no_member(base)
                    && !matches!(self.locals[id].ty, Type::Ref(_))
                {
                    let index_value = self.rvalue(index)?;
                    let Some(Const::Int(column)) = as_const(&index_value) else {
                        return err(
                            VuslMessage::NoInstruction {
                                what: "indexing registers with a value computed at run time".into(),
                                help: "use a constant index, or #pragma unroll the loop that computes it",
                            },
                            index.pos,
                        );
                    };
                    return match &self.locals[id].ty {
                        Type::Mat { size, .. } if (0..i64::from(*size)).contains(&column) => {
                            Ok(Target::Local {
                                id,
                                column: Some(column as usize),
                                components: None,
                            })
                        }
                        ty if ty
                            .vf_lanes()
                            .is_some_and(|lanes| (0..lanes as i64).contains(&column)) =>
                        {
                            Ok(Target::Local {
                                id,
                                column: None,
                                components: Some(vec![column as usize]),
                            })
                        }
                        _ => err(
                            VuslMessage::IndexOutOfRange {
                                index: column,
                                length: full_mask(&self.locals[id].ty).count_ones(),
                            },
                            index.pos,
                        ),
                    };
                }
                match self.value(expr)? {
                    Value::Mem(memory) => Ok(Target::Memory {
                        memory,
                        components: None,
                    }),
                    Value::MemLanes(memory, components) => Ok(Target::Memory {
                        memory,
                        components: Some(components),
                    }),
                    Value::Reg(_) => err(
                        VuslMessage::NotAssignable {
                            what: "a computed value",
                        },
                        expr.pos,
                    ),
                }
            }
            _ => err(
                VuslMessage::NotAssignable {
                    what: "a computed value",
                },
                expr.pos,
            ),
        }
    }

    /// A local, or a matrix local's column, when `expr` names one.
    fn target_no_member(&mut self, expr: &ast::Expr) -> Checked<Target> {
        match &expr.kind {
            Ast::Name(name) => match self.lookup(name)? {
                Symbol::Local(id) => Ok(Target::Local {
                    id,
                    column: None,
                    components: None,
                }),
                Symbol::Alias(target @ Target::Local { .. }) => Ok(target),
                _ => err(VuslMessage::NotAssignable { what: "memory" }, expr.pos),
            },
            Ast::Index { base, index } => {
                let base = self.target_no_member(base)?;
                let Target::Local {
                    id,
                    column: None,
                    components: None,
                } = base
                else {
                    return err(VuslMessage::NotAssignable { what: "memory" }, expr.pos);
                };
                if !matches!(self.locals[id].ty, Type::Mat { .. }) {
                    return err(VuslMessage::NotAssignable { what: "memory" }, expr.pos);
                }
                let index_value = self.rvalue(index)?;
                match as_const(&index_value) {
                    Some(Const::Int(column)) if column >= 0 => Ok(Target::Local {
                        id,
                        column: Some(column as usize),
                        components: None,
                    }),
                    _ => err(
                        VuslMessage::NoInstruction {
                            what: "indexing registers with a value computed at run time".into(),
                            help: "use a constant index, or #pragma unroll the loop that computes it",
                        },
                        index.pos,
                    ),
                }
            }
            _ => err(VuslMessage::NotAssignable { what: "memory" }, expr.pos),
        }
    }

    fn column_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Mat { size, .. } => Type::Vec(*size),
            ty => ty.clone(),
        }
    }

    /// The type and placement a target takes a value in.
    fn target_shape(&self, target: &Target) -> (Type, Option<Placement>) {
        match target {
            Target::Local {
                id,
                column,
                components,
            } => {
                let local = &self.locals[*id];
                let ty = match column {
                    Some(_) => self.column_type(&local.ty),
                    None => local.ty.clone(),
                };
                let placement = match column {
                    Some(_) => ty.vf_lanes().map(Placement::identity),
                    None => self.state[*id].placement.or(local.fixed).or_else(|| {
                        ty.vf_lanes()
                            .filter(|lanes| *lanes > 1)
                            .map(Placement::identity)
                    }),
                };
                match components {
                    None => (ty, placement),
                    Some(components) => (
                        ty.with_components(components.len()).unwrap_or(ty.clone()),
                        placement.and_then(|placement| placement.select(components)),
                    ),
                }
            }
            Target::Memory { memory, components } => {
                let ty = memory.place.ty.clone();
                let placement = memory_placement(&ty, memory.place.lane);
                match components {
                    None => (ty, placement),
                    Some(components) => (
                        ty.with_components(components.len()).unwrap_or(ty.clone()),
                        placement.and_then(|placement| placement.select(components)),
                    ),
                }
            }
        }
    }

    /// Reads a target's current value, for a compound assignment.
    fn read_target(&mut self, target: &Target, pos: Pos) -> Checked<Expr> {
        match target {
            Target::Local {
                id,
                column,
                components,
            } => {
                let mut value = self.read_local(*id, pos)?;
                if let Some(column) = column {
                    value = self.column(value, *column, pos)?;
                }
                match components {
                    Some(components) => self.apply_swizzle(value, components.clone(), pos),
                    None => Ok(value),
                }
            }
            Target::Memory { memory, components } => {
                let value = self.load(memory.clone(), pos)?;
                match components {
                    Some(components) => self.apply_swizzle(value, components.clone(), pos),
                    None => Ok(value),
                }
            }
        }
    }

    fn assign(
        &mut self,
        op: AssignOp,
        target_expr: &ast::Expr,
        value_expr: &ast::Expr,
        pos: Pos,
    ) -> Checked<Option<Stmt>> {
        let target = self.target(target_expr)?;
        // Structs, arrays and qwords copy between memory places whole.
        let mut assigned = None;
        if let (
            AssignOp::Assign,
            Target::Memory {
                memory: to,
                components: None,
            },
        ) = (op, &target)
            && matches!(
                to.place.ty,
                Type::Struct(_) | Type::Array(..) | Type::Mat { .. } | Type::Qword
            )
        {
            match self.value(value_expr)? {
                Value::Mem(from) => return self.copy(to.clone(), from, pos).map(Some),
                other => assigned = Some(self.read_value(other, value_expr.pos)?),
            }
        }
        let value = match op {
            AssignOp::Assign => match assigned {
                Some(value) => value,
                None => self.rvalue(value_expr)?,
            },
            AssignOp::Compound(binary) => {
                let current = self.read_target(&target, pos)?;
                let rhs = self.rvalue(value_expr)?;
                self.binary_values(binary, current, rhs, pos)?
            }
        };
        self.set(target, value, pos).map(Some)
    }

    fn copy(&mut self, to: Memory, from: Memory, pos: Pos) -> Checked<Stmt> {
        if to.place.ty != from.place.ty {
            return err(
                VuslMessage::TypeMismatch {
                    context: "the copy".into(),
                    expected: self.type_name(&to.place.ty),
                    found: self.type_name(&from.place.ty),
                },
                pos,
            );
        }
        if !to.writable() {
            return err(VuslMessage::ReadOnly { name: to.name }, pos);
        }
        if !from.readable() {
            return err(VuslMessage::WriteOnly { name: from.name }, pos);
        }
        let qwords = match &to.place.ty {
            Type::Qword => 1,
            Type::Mat { size, .. } => u32::from(*size),
            ty => {
                let qwords = whole_qwords(ty, &self.decls.structs);
                match qwords {
                    Some(qwords) => qwords,
                    None => {
                        return err(
                            VuslMessage::NotYet {
                                what: "copying memory whose size is read at run time",
                            },
                            pos,
                        );
                    }
                }
            }
        };
        self.record(&to, true);
        self.record(&from, false);
        Ok(Stmt::Copy {
            to: to.place,
            from: from.place,
            qwords,
            pos,
        })
    }

    /// `target = value`: the value converted to the target's type and, when
    /// its lanes differ, moved into the target's lanes.
    fn set(&mut self, target: Target, value: Expr, pos: Pos) -> Checked<Stmt> {
        let (ty, placement) = self.target_shape(&target);
        let value = self.coerce(value, &ty, "the assignment")?;
        if let Target::Local { id, .. } = &target
            && matches!(ty, Type::Ref(_))
        {
            // A ref stays within the memory it was first set from.
            let within = self.ref_within(&value);
            match (&self.locals[*id].within, within) {
                (_, None) => {
                    return err(
                        VuslMessage::RefWithin {
                            name: self.locals[*id].name.clone(),
                        },
                        pos,
                    );
                }
                (None, Some(within)) => self.locals[*id].within = Some(within),
                (Some(current), Some(within)) if *current == within => {}
                (Some(_), Some(_)) => {
                    return err(
                        VuslMessage::RefWithin {
                            name: self.locals[*id].name.clone(),
                        },
                        pos,
                    );
                }
            }
        }
        // A whole local without layout(lane) takes the value's lanes: only
        // a fixed local, a part of one, or memory moves lanes.
        let adopts = matches!(
            target,
            Target::Local { id, column: None, components: None } if self.locals[id].fixed.is_none()
        );
        if !adopts
            && let (Some(to), Some(from)) = (placement, value.placement)
            && to.len() > 1
            && to != from
            && value.ty.is_raw()
            && from.rotation_to(&to).is_none()
        {
            return err(
                VuslMessage::RawLaneMove {
                    from: from.letters(),
                    to: to.letters(),
                },
                pos,
            );
        }
        match target {
            Target::Local {
                id,
                column,
                components,
            } => {
                if self.locals[id].ty == Type::ClipFlags {
                    self.clip_judgements
                        .insert(id, (self.clip_block, self.clips.saturating_sub(1)));
                }
                let local = &self.locals[id];
                let whole = full_mask(&local.ty);
                match (column, &components) {
                    (None, None) => {
                        let placement = local.fixed.or(value.placement).or_else(|| {
                            local
                                .ty
                                .vf_lanes()
                                .filter(|lanes| *lanes > 1)
                                .map(Placement::identity)
                        });
                        self.state[id] = LocalState {
                            placement,
                            set: whole,
                        };
                    }
                    (None, Some(components)) => {
                        if self.state[id].placement.is_none() {
                            self.state[id].placement = local
                                .fixed
                                .or_else(|| local.ty.vf_lanes().map(Placement::identity));
                        }
                        self.state[id].set |= mask_of(components);
                    }
                    (Some(_), _) => {
                        // A matrix's column needs the matrix set first.
                        if self.state[id].set != whole {
                            return err(
                                VuslMessage::UseBeforeSet {
                                    name: local.name.clone(),
                                },
                                pos,
                            );
                        }
                    }
                }
                Ok(Stmt::SetLocal {
                    local: id,
                    column,
                    components,
                    value,
                    pos,
                })
            }
            Target::Memory { memory, components } => {
                if !memory.writable() {
                    return err(VuslMessage::ReadOnly { name: memory.name }, pos);
                }
                if !self.register_type(&memory.place.ty) {
                    return err(
                        VuslMessage::TypeMismatch {
                            context: "the assignment".into(),
                            expected: "a value in registers".into(),
                            found: self.type_name(&memory.place.ty),
                        },
                        pos,
                    );
                }
                self.record(&memory, true);
                Ok(Stmt::Store {
                    place: memory.place,
                    components,
                    value,
                    pos,
                })
            }
        }
    }

    fn increment(&mut self, operand: &ast::Expr, up: bool, pos: Pos) -> Checked<Stmt> {
        let target = self.target(operand)?;
        let (ty, _) = self.target_shape(&target);
        if ty != Type::Int16 {
            return err(
                VuslMessage::TypeMismatch {
                    context: if up { "'++'" } else { "'--'" }.into(),
                    expected: "an int16".into(),
                    found: self.type_name(&ty),
                },
                pos,
            );
        }
        let current = self.read_target(&target, pos)?;
        let one = constant_expr(Const::Int(1), pos);
        let op = if up { BinaryOp::Add } else { BinaryOp::Sub };
        let value = self.binary_values(op, current, one, pos)?;
        self.set(target, value, pos)
    }

    // ----------------------------------------------
    // Expressions
    // ----------------------------------------------

    fn condition(&mut self, expr: &ast::Expr) -> Checked<Expr> {
        let value = self.rvalue(expr)?;
        if value.ty != Type::Bool {
            return err(
                VuslMessage::NotBool {
                    found: self.type_text(&value),
                },
                expr.pos,
            );
        }
        Ok(value)
    }

    fn register_type(&self, ty: &Type) -> bool {
        matches!(
            ty,
            Type::Float
                | Type::Vec(_)
                | Type::Int16
                | Type::Int32
                | Type::Uint32
                | Type::IVec(_)
                | Type::UVec(_)
                | Type::Qword
                | Type::Mat { .. }
        )
    }

    fn rvalue(&mut self, expr: &ast::Expr) -> Checked<Expr> {
        let value = self.value(expr)?;
        self.read_value(value, expr.pos)
    }

    /// An expression's result in registers: memory read.
    fn read_value(&mut self, value: Value, pos: Pos) -> Checked<Expr> {
        match value {
            Value::Reg(value) => Ok(value),
            Value::Mem(memory) => self.load(memory, pos),
            Value::MemLanes(memory, components) => {
                let value = self.load(memory, pos)?;
                self.apply_swizzle(value, components, pos)
            }
        }
    }

    fn load(&mut self, memory: Memory, pos: Pos) -> Checked<Expr> {
        if !memory.readable() {
            return err(VuslMessage::WriteOnly { name: memory.name }, pos);
        }
        if !self.register_type(&memory.place.ty) {
            return err(
                VuslMessage::TypeMismatch {
                    context: format!("reading '{}'", memory.name),
                    expected: "a value that fits in registers".into(),
                    found: self.type_name(&memory.place.ty),
                },
                pos,
            );
        }
        self.record(&memory, false);
        let placement = match memory.place.ty {
            Type::Mat { .. } => None,
            ref ty => memory_placement(ty, memory.place.lane),
        };
        Ok(Expr {
            ty: memory.place.ty.clone(),
            placement,
            kind: ExprKind::Load(memory.place),
            pos,
        })
    }

    fn read_local(&self, id: LocalId, pos: Pos) -> Checked<Expr> {
        let local = &self.locals[id];
        if self.state[id].set != full_mask(&local.ty) {
            return err(
                VuslMessage::UseBeforeSet {
                    name: local.name.clone(),
                },
                pos,
            );
        }
        Ok(Expr {
            kind: ExprKind::Local(id),
            ty: local.ty.clone(),
            placement: self.state[id].placement,
            pos,
        })
    }

    fn value(&mut self, expr: &ast::Expr) -> Checked<Value> {
        let pos = expr.pos;
        match &expr.kind {
            Ast::Int(value) => Ok(Value::Reg(constant_expr(Const::Int(*value), pos))),
            Ast::Float(value) => Ok(Value::Reg(constant_expr(Const::Float(*value), pos))),
            Ast::Bool(value) => Ok(Value::Reg(constant_expr(Const::Bool(*value), pos))),
            Ast::Name(name) => match self.lookup(name)? {
                Symbol::Local(id) => Ok(Value::Reg(self.read_local(id, pos)?)),
                Symbol::Const(value, ty) => {
                    let mut constant = constant_expr(value, pos);
                    constant.ty = ty;
                    Ok(Value::Reg(constant))
                }
                Symbol::Memory(memory) => Ok(Value::Mem(memory)),
                Symbol::Alias(target) => match target {
                    Target::Memory {
                        memory,
                        components: None,
                    } => Ok(Value::Mem(memory)),
                    Target::Memory {
                        memory,
                        components: Some(components),
                    } => Ok(Value::MemLanes(memory, components)),
                    target => Ok(Value::Reg(self.read_target(&target, pos)?)),
                },
            },
            Ast::Field { base, field } => self.field(base, field),
            Ast::Index { base, index } => self.index(base, index, pos),
            Ast::Unary { op, operand } => {
                if matches!(op, UnaryOp::PreIncrement | UnaryOp::PreDecrement) {
                    return err(VuslMessage::NestedAssignment, pos);
                }
                let operand = self.rvalue(operand)?;
                self.unary(*op, operand, pos).map(Value::Reg)
            }
            Ast::Binary {
                op,
                op_pos,
                lhs,
                rhs,
            } => self.binary(*op, lhs, rhs, *op_pos).map(Value::Reg),
            Ast::Assign { .. } | Ast::Postfix { .. } => err(VuslMessage::NestedAssignment, pos),
            Ast::Conditional { question, .. } => err(
                VuslMessage::NoInstruction {
                    what: "'?:'".into(),
                    help: "the VU has no select: write an if statement",
                },
                *question,
            ),
            Ast::Call { callee, args } => self.call(callee, args, pos),
        }
    }

    /// A swizzle's components, checked against a value of `ty`.
    fn swizzle(&self, text: &str, ty: &Type, pos: Pos, reading: bool) -> Checked<Vec<usize>> {
        let lanes = ty.vf_lanes().unwrap_or(0);
        let components = swizzle_components(text)
            .filter(|components| components.len() <= 4 && components.iter().all(|c| *c < lanes));
        let Some(components) = components else {
            return err(
                VuslMessage::BadSwizzle {
                    swizzle: text.into(),
                    ty: self.type_name(ty),
                },
                pos,
            );
        };
        let mut seen = [false; 4];
        for &component in &components {
            if std::mem::replace(&mut seen[component], true) {
                return err(
                    if reading {
                        VuslMessage::RepeatedComponent {
                            swizzle: text.into(),
                        }
                    } else {
                        VuslMessage::BadSwizzle {
                            swizzle: text.into(),
                            ty: self.type_name(ty),
                        }
                    },
                    pos,
                );
            }
        }
        Ok(components)
    }

    fn apply_swizzle(&self, value: Expr, components: Vec<usize>, pos: Pos) -> Checked<Expr> {
        let Some(ty) = value.ty.with_components(components.len()) else {
            return err(
                VuslMessage::BadSwizzle {
                    swizzle: components
                        .iter()
                        .filter_map(|&c| Lane::from_index(c).map(Lane::letter))
                        .collect(),
                    ty: self.type_name(&value.ty),
                },
                pos,
            );
        };
        let placement = value
            .placement
            .and_then(|placement| placement.select(&components));
        Ok(Expr {
            kind: ExprKind::Swizzle {
                base: Box::new(value),
                components,
            },
            ty,
            placement,
            pos,
        })
    }

    /// The member of a struct in memory.
    fn member(&mut self, memory: Memory, id: usize, field: &ast::Ident) -> Checked<Memory> {
        let def = &self.decls.structs[id];
        let Some((index, member)) = def.member(&field.name) else {
            return err(
                VuslMessage::UnknownMember {
                    ty: def.name.clone(),
                    member: field.name.clone(),
                },
                field.pos,
            );
        };
        let mut offset = memory.place.offset.clone();
        let prefix = &memory.place.path;
        offset.extent.fixed += member.offset.fixed;
        for term in &member.offset.terms {
            offset.extent.add(&Extent {
                fixed: 0,
                terms: vec![Term {
                    // XITOP's count is the same from every struct.
                    count: if term.is_xitop() {
                        Vec::new()
                    } else {
                        prefix.iter().chain(&term.count).copied().collect()
                    },
                    stride: term.stride,
                }],
            });
        }
        let mut path = memory.place.path.clone();
        path.push(index);
        // A dependent array's length, read through the same struct.
        let length = match &member.ty {
            Type::Array(_, Length::Member(count)) => {
                let mut place = memory.place.clone();
                let mut struct_id = id;
                for (step, &member_index) in count.iter().enumerate() {
                    let member = &self.decls.structs[struct_id].members[member_index];
                    place.offset.extent.fixed += member.offset.fixed;
                    place.path.push(member_index);
                    place.ty = member.ty.clone();
                    place.lane = member.lane;
                    if step + 1 < count.len() {
                        let Type::Struct(next) = member.ty else {
                            break;
                        };
                        struct_id = next;
                    }
                }
                place.length = None;
                Some(Box::new(place))
            }
            _ => None,
        };
        let root = memory.root.map(|(param, mut root_path)| {
            root_path.push(index);
            (param, root_path)
        });
        Ok(Memory {
            place: Place {
                base: memory.place.base,
                path,
                offset,
                ty: member.ty.clone(),
                lane: member.lane,
                length,
            },
            access: memory.access,
            name: format!("{}.{}", memory.name, field.name),
            root,
        })
    }

    fn field(&mut self, base: &ast::Expr, field: &ast::Ident) -> Checked<Value> {
        let value = self.value(base)?;
        match self.deref(value, false)? {
            Value::Mem(memory) => match memory.place.ty.clone() {
                Type::Struct(id) => self.member(memory, id, field).map(Value::Mem),
                Type::Qword => {
                    // One lane of a qword in memory, as an int16 (ILW/ISW).
                    let lane = match field.name.as_str() {
                        "x" => Lane::X,
                        "y" => Lane::Y,
                        "z" => Lane::Z,
                        "w" => Lane::W,
                        _ => {
                            return err(
                                VuslMessage::BadSwizzle {
                                    swizzle: field.name.clone(),
                                    ty: "qword (one lane, as an int16)".into(),
                                },
                                field.pos,
                            );
                        }
                    };
                    let mut place = memory.place.clone();
                    place.ty = Type::Int16;
                    place.lane = lane;
                    Ok(Value::Mem(Memory {
                        place,
                        name: format!("{}.{}", memory.name, field.name),
                        ..memory
                    }))
                }
                ty if ty.vf_lanes().is_some() => {
                    let components = self.swizzle(&field.name, &ty, field.pos, false)?;
                    Ok(Value::MemLanes(memory, components))
                }
                ty => err(
                    VuslMessage::UnknownMember {
                        ty: self.type_name(&ty),
                        member: field.name.clone(),
                    },
                    field.pos,
                ),
            },
            Value::MemLanes(memory, components) => {
                let value = self.load(memory, base.pos)?;
                let value = self.apply_swizzle(value, components, base.pos)?;
                self.swizzle_value(value, field).map(Value::Reg)
            }
            Value::Reg(value) => self.swizzle_value(value, field).map(Value::Reg),
        }
    }

    fn swizzle_value(&self, value: Expr, field: &ast::Ident) -> Checked<Expr> {
        if value.ty.vf_lanes().is_none() || value.ty == Type::Qword {
            return err(
                VuslMessage::UnknownMember {
                    ty: self.type_name(&value.ty),
                    member: field.name.clone(),
                },
                field.pos,
            );
        }
        let components = self.swizzle(&field.name, &value.ty, field.pos, true)?;
        self.apply_swizzle(value, components, field.pos)
    }

    fn column(&self, matrix: Expr, index: usize, pos: Pos) -> Checked<Expr> {
        let Type::Mat { size, .. } = matrix.ty else {
            unreachable!("columns are of matrices");
        };
        if index >= usize::from(size) {
            return err(
                VuslMessage::IndexOutOfRange {
                    index: index as i64,
                    length: u32::from(size),
                },
                pos,
            );
        }
        Ok(Expr {
            kind: ExprKind::Column {
                matrix: Box::new(matrix),
                index,
            },
            ty: Type::Vec(size),
            placement: Some(Placement::identity(usize::from(size))),
            pos,
        })
    }

    fn index(&mut self, base: &ast::Expr, index: &ast::Expr, pos: Pos) -> Checked<Value> {
        let base_value = self.value(base)?;
        let base_value = self.deref(base_value, true)?;
        let index_value = self.rvalue(index)?;
        let index_const = match as_const(&index_value) {
            Some(Const::Int(value)) => Some(value),
            Some(_) => {
                return err(
                    VuslMessage::TypeMismatch {
                        context: "an index".into(),
                        expected: "an int16".into(),
                        found: self.type_text(&index_value),
                    },
                    index.pos,
                );
            }
            None => None,
        };
        let not_constant = || {
            err(
                VuslMessage::NoInstruction {
                    what: "indexing registers with a value computed at run time".into(),
                    help: "use a constant index, or #pragma unroll the loop that computes it",
                },
                index.pos,
            )
        };
        match base_value {
            Value::Mem(memory) => match memory.place.ty.clone() {
                Type::Array(element, length) => {
                    let stride = element_qwords(&element, &self.decls.structs);
                    let mut place = memory.place.clone();
                    match index_const {
                        Some(value) => {
                            let in_range = match length {
                                Length::Fixed(n) => (0..i64::from(n)).contains(&value),
                                _ => value >= 0,
                            };
                            if !in_range {
                                return err(
                                    VuslMessage::IndexOutOfRange {
                                        index: value,
                                        length: match length {
                                            Length::Fixed(n) => n,
                                            _ => 0,
                                        },
                                    },
                                    index.pos,
                                );
                            }
                            place.offset.extent.fixed += value as u32 * stride;
                        }
                        None => {
                            if index_value.ty != Type::Int16 {
                                return err(
                                    VuslMessage::TypeMismatch {
                                        context: "an index".into(),
                                        expected: "an int16".into(),
                                        found: self.type_text(&index_value),
                                    },
                                    index.pos,
                                );
                            }
                            place.offset.indices.push((index_value, stride));
                        }
                    }
                    place.ty = *element;
                    place.lane = Lane::X;
                    place.length = None;
                    Ok(Value::Mem(Memory {
                        place,
                        name: format!("{}[]", memory.name),
                        ..memory
                    }))
                }
                ty if ty.vf_lanes().is_some() && ty != Type::Qword => {
                    let Some(value) = index_const else {
                        return not_constant();
                    };
                    let lanes = ty.vf_lanes().unwrap_or(0) as i64;
                    if !(0..lanes).contains(&value) {
                        return err(
                            VuslMessage::IndexOutOfRange {
                                index: value,
                                length: lanes as u32,
                            },
                            index.pos,
                        );
                    }
                    Ok(Value::MemLanes(memory, vec![value as usize]))
                }
                Type::Mat { .. } => {
                    let Some(value) = index_const else {
                        return not_constant();
                    };
                    let matrix = self.load(memory, base.pos)?;
                    self.column(matrix, value.max(0) as usize, pos)
                        .map(Value::Reg)
                }
                ty => err(
                    VuslMessage::TypeMismatch {
                        context: "indexing".into(),
                        expected: "an array, a vector or a matrix".into(),
                        found: self.type_name(&ty),
                    },
                    pos,
                ),
            },
            Value::MemLanes(memory, components) => {
                let value = self.load(memory, base.pos)?;
                let value = self.apply_swizzle(value, components, base.pos)?;
                self.index_value(value, index_const, index.pos, pos)
            }
            Value::Reg(value) => self.index_value(value, index_const, index.pos, pos),
        }
    }

    fn index_value(
        &self,
        value: Expr,
        index: Option<i64>,
        index_pos: Pos,
        pos: Pos,
    ) -> Checked<Value> {
        let Some(index) = index else {
            return err(
                VuslMessage::NoInstruction {
                    what: "indexing registers with a value computed at run time".into(),
                    help: "use a constant index, or #pragma unroll the loop that computes it",
                },
                index_pos,
            );
        };
        match value.ty {
            Type::Mat { .. } => self
                .column(value, index.max(0) as usize, pos)
                .map(Value::Reg),
            ref ty
                if ty
                    .vf_lanes()
                    .is_some_and(|lanes| (0..lanes as i64).contains(&index)) =>
            {
                self.apply_swizzle(value, vec![index as usize], pos)
                    .map(Value::Reg)
            }
            _ => err(
                VuslMessage::IndexOutOfRange {
                    index,
                    length: value.ty.vf_lanes().unwrap_or(0) as u32,
                },
                index_pos,
            ),
        }
    }

    fn unary(&mut self, op: UnaryOp, operand: Expr, pos: Pos) -> Checked<Expr> {
        if let Some(value) = as_const(&operand) {
            return match consteval::unary(op, value) {
                Ok(value) => {
                    let mut folded = constant_expr(value, pos);
                    if folded.ty == Type::Int16 || folded.ty == Type::Float {
                        folded.ty = match (&operand.ty, value) {
                            (Type::Float, Const::Int(n)) => {
                                folded.kind = ExprKind::Const(Const::Float(n as f32));
                                Type::Float
                            }
                            _ => folded.ty,
                        };
                    }
                    Ok(folded)
                }
                Err(_) => err(
                    VuslMessage::TypeMismatch {
                        context: "the operator".into(),
                        expected: "an operand it applies to".into(),
                        found: const_type_name(value).into(),
                    },
                    pos,
                ),
            };
        }
        match op {
            UnaryOp::Plus => Ok(operand),
            UnaryOp::Neg if operand.ty.is_float() || operand.ty == Type::Int16 => Ok(Expr {
                ty: operand.ty.clone(),
                placement: operand.placement,
                kind: ExprKind::Neg(Box::new(operand)),
                pos,
            }),
            UnaryOp::Not if operand.ty == Type::Bool => Ok(Expr {
                kind: ExprKind::Not(Box::new(operand)),
                ty: Type::Bool,
                placement: None,
                pos,
            }),
            UnaryOp::BitNot if operand.ty == Type::Int16 => err(
                VuslMessage::NoInstruction {
                    what: "'~' on an int16".into(),
                    help: "subtract it from -1: -1 - x",
                },
                pos,
            ),
            _ => err(
                VuslMessage::TypeMismatch {
                    context: "the operator".into(),
                    expected: "an operand it applies to".into(),
                    found: self.type_text(&operand),
                },
                pos,
            ),
        }
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        pos: Pos,
    ) -> Checked<Expr> {
        if matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr) {
            let lhs = self.condition(lhs)?;
            let before = self.pending.len();
            let rhs = self.condition(rhs)?;
            if self.pending.len() != before {
                return err(
                    VuslMessage::NotYet {
                        what: "a call with statements on the right of && or ||",
                    },
                    pos,
                );
            }
            if let (Some(a), Some(b)) = (as_const(&lhs), as_const(&rhs))
                && let Ok(value) = consteval::binary(op, a, b)
            {
                return Ok(constant_expr(value, pos));
            }
            let kind = if op == BinaryOp::LogicAnd {
                ExprKind::And(Box::new(lhs), Box::new(rhs))
            } else {
                ExprKind::Or(Box::new(lhs), Box::new(rhs))
            };
            return Ok(Expr {
                kind,
                ty: Type::Bool,
                placement: None,
                pos,
            });
        }
        let lhs = self.rvalue(lhs)?;
        let rhs = self.rvalue(rhs)?;
        self.binary_values(op, lhs, rhs, pos)
    }

    /// `+` and `-` of a `ref<T>` and an `int16`, which move it by whole
    /// elements, a difference of two refs in elements, and comparisons.
    fn ref_arithmetic(&mut self, op: BinaryOp, lhs: Expr, rhs: Expr, pos: Pos) -> Checked<Expr> {
        if matches!(
            op,
            BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
                | BinaryOp::Equal
                | BinaryOp::NotEqual
        ) {
            return self.compare(op, lhs, rhs, pos);
        }
        let (reference, amount, op) = match (&lhs.ty, &rhs.ty, op) {
            (Type::Ref(_), Type::Int16, BinaryOp::Add | BinaryOp::Sub) => (lhs, rhs, op),
            (Type::Int16, Type::Ref(_), BinaryOp::Add) => (rhs, lhs, op),
            (Type::Ref(a), Type::Ref(b), BinaryOp::Sub) if a == b => {
                let stride = element_qwords(a, &self.decls.structs);
                if stride != 1 {
                    return err(
                        VuslMessage::NoInstruction {
                            what: format!("the distance between two refs to {}", self.type_name(a)),
                            help: "VU1 has no divide: compare refs instead, or count elements",
                        },
                        pos,
                    );
                }
                return Ok(Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::IntSub,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    ty: Type::Int16,
                    placement: None,
                    pos,
                });
            }
            _ => {
                return err(
                    VuslMessage::TypeMismatch {
                        context: format!("'{}'", op.spelling()),
                        expected: "a ref<T> plus or minus an int16".into(),
                        found: format!("{} and {}", self.type_text(&lhs), self.type_text(&rhs)),
                    },
                    pos,
                );
            }
        };
        let Type::Ref(target) = reference.ty.clone() else {
            unreachable!("a ref operand");
        };
        let stride = element_qwords(&target, &self.decls.structs);
        let amount = match as_const(&amount) {
            Some(Const::Int(n)) => {
                let qwords = n * i64::from(stride);
                if !int16_range(qwords) {
                    return err(
                        VuslMessage::ConstantRange {
                            value: qwords.to_string(),
                            ty: "int16",
                        },
                        amount.pos,
                    );
                }
                constant_expr(Const::Int(qwords), amount.pos)
            }
            _ if stride == 1 => amount,
            _ => {
                return err(
                    VuslMessage::NoInstruction {
                        what: format!(
                            "moving a ref to {} by a value computed at run time",
                            self.type_name(&target)
                        ),
                        help: "each element takes several qwords and VU1 has no multiply: move it by constants",
                    },
                    amount.pos,
                );
            }
        };
        let op = if op == BinaryOp::Add {
            BinOp::IntAdd
        } else {
            BinOp::IntSub
        };
        Ok(Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(reference),
                rhs: Box::new(amount),
            },
            ty: Type::Ref(target),
            placement: None,
            pos,
        })
    }

    /// The memory a ref value stays within.
    fn ref_within(&self, value: &Expr) -> Option<Within> {
        match &value.kind {
            ExprKind::RefTo { within, .. } => Some(within.clone()),
            ExprKind::Local(id) => self.locals[*id].within.clone(),
            ExprKind::Binary {
                op: BinOp::IntAdd | BinOp::IntSub,
                lhs,
                ..
            } => self.ref_within(lhs),
            _ => None,
        }
    }

    /// `ref<T>(memory)`, the address of memory of type T or of an array's
    /// first T, and `ref<T>(memory, address)`, an `int16` address within
    /// memory.
    fn make_ref(&mut self, callee: &ast::TypeName, args: &[ast::Expr], pos: Pos) -> Checked<Expr> {
        let ty = resolve_type(callee, &self.decls.struct_ids)?;
        let Type::Ref(target) = ty.clone() else {
            return err(
                VuslMessage::TypeMismatch {
                    context: "a call".into(),
                    expected: "a function or a type".into(),
                    found: self.type_name(&ty),
                },
                pos,
            );
        };
        let memory_of = |this: &mut Self, arg: &ast::Expr| -> Checked<Memory> {
            match this.value(arg)? {
                Value::Mem(memory) => Ok(memory),
                value => {
                    let value = this.deref(value, false)?;
                    match value {
                        Value::Mem(memory) => Ok(memory),
                        _ => err(
                            VuslMessage::TypeMismatch {
                                context: "ref<T>(...)".into(),
                                expected: "memory".into(),
                                found: "a value in registers".into(),
                            },
                            arg.pos,
                        ),
                    }
                }
            }
        };
        let (address, memory) = match args {
            [place] => {
                let memory = memory_of(self, place)?;
                let element = match &memory.place.ty {
                    Type::Array(element, _) => (**element).clone(),
                    ty => ty.clone(),
                };
                if element != *target {
                    return err(
                        VuslMessage::TypeMismatch {
                            context: "ref<T>(...)".into(),
                            expected: format!(
                                "a {} in memory, or an array of them",
                                self.type_name(&target)
                            ),
                            found: self.type_name(&memory.place.ty),
                        },
                        place.pos,
                    );
                }
                let address = Expr {
                    kind: ExprKind::Address(memory.place.clone()),
                    ty: Type::Int16,
                    placement: None,
                    pos,
                };
                (address, memory)
            }
            [place, address] => {
                let memory = memory_of(self, place)?;
                let address = self.rvalue(address)?;
                let address = self.coerce(address, &Type::Int16, "a ref's address")?;
                (address, memory)
            }
            _ => {
                return err(
                    VuslMessage::WrongArgumentCount {
                        name: "ref".into(),
                        expected: "memory, and an int16 address within it".into(),
                        found: args.len(),
                    },
                    pos,
                );
            }
        };
        let within = Within {
            base: memory.place.base,
            path: memory.place.path.clone(),
            access: memory.access,
            root: memory.root.clone(),
        };
        Ok(Expr {
            kind: ExprKind::RefTo {
                address: Box::new(address),
                within,
            },
            ty,
            placement: None,
            pos,
        })
    }

    /// The memory a `ref<T>` variable points at: a T, or with `as_array`
    /// the first of an array of them. Other values are as they are.
    fn deref(&self, value: Value, as_array: bool) -> Checked<Value> {
        let Value::Reg(expr) = &value else {
            return Ok(value);
        };
        let Type::Ref(target) = &expr.ty else {
            return Ok(value);
        };
        let ExprKind::Local(id) = expr.kind else {
            return err(VuslMessage::RefNotVariable, expr.pos);
        };
        let local = &self.locals[id];
        let Some(within) = &local.within else {
            return err(
                VuslMessage::UseBeforeSet {
                    name: local.name.clone(),
                },
                expr.pos,
            );
        };
        let ty = if as_array {
            Type::Array(target.clone(), Length::Open)
        } else {
            (**target).clone()
        };
        Ok(Value::Mem(Memory {
            place: Place {
                base: Base::Ref(id),
                path: Vec::new(),
                offset: Offset::default(),
                ty,
                lane: Lane::X,
                length: None,
            },
            access: within.access,
            name: local.name.clone(),
            root: within.root.clone(),
        }))
    }

    /// An integer constant where a float is wanted is that float.
    fn to_float(&self, value: Expr) -> Expr {
        match value.kind {
            ExprKind::Const(Const::Int(n)) => {
                let mut value = constant_expr(Const::Float(n as f32), value.pos);
                value.placement = None;
                value
            }
            _ => value,
        }
    }

    fn binary_values(&mut self, op: BinaryOp, lhs: Expr, rhs: Expr, pos: Pos) -> Checked<Expr> {
        if matches!(lhs.ty, Type::Ref(_)) || matches!(rhs.ty, Type::Ref(_)) {
            return self.ref_arithmetic(op, lhs, rhs, pos);
        }
        if let (Some(a), Some(b)) = (as_const(&lhs), as_const(&rhs)) {
            let both_int = matches!((a, b), (Const::Int(_), Const::Int(_)));
            let float_typed = lhs.ty == Type::Float || rhs.ty == Type::Float;
            let (a, b) = if both_int && float_typed {
                (
                    as_const(&self.to_float(lhs.clone())).unwrap_or(a),
                    as_const(&self.to_float(rhs.clone())).unwrap_or(b),
                )
            } else {
                (a, b)
            };
            return match consteval::binary(op, a, b) {
                Ok(value) => Ok(constant_expr(value, pos)),
                Err(consteval::FoldError::DivisionByZero) => err(
                    VuslMessage::NoInstruction {
                        what: "a division by zero".into(),
                        help: "the constant's divisor is 0",
                    },
                    pos,
                ),
                Err(consteval::FoldError::Mismatch) => err(
                    VuslMessage::TypeMismatch {
                        context: format!("'{}'", op.spelling()),
                        expected: "operands it applies to".into(),
                        found: format!("{} and {}", const_type_name(a), const_type_name(b)),
                    },
                    pos,
                ),
            };
        }
        // Integer constants next to floats are floats.
        let (lhs, rhs) = if lhs.ty.is_float() || matches!(lhs.ty, Type::Mat { .. }) {
            (lhs, self.to_float(rhs))
        } else if rhs.ty.is_float() || matches!(rhs.ty, Type::Mat { .. }) {
            (self.to_float(lhs), rhs)
        } else {
            (lhs, rhs)
        };
        if op.is_comparison() {
            return self.compare(op, lhs, rhs, pos);
        }
        let context = format!("'{}'", op.spelling());
        let int16 = lhs.ty == Type::Int16 && rhs.ty == Type::Int16;
        if int16 {
            let int_op = match op {
                BinaryOp::Add => BinOp::IntAdd,
                BinaryOp::Sub => BinOp::IntSub,
                BinaryOp::BitAnd => BinOp::IntAnd,
                BinaryOp::BitOr => BinOp::IntOr,
                BinaryOp::Mul => {
                    return err(
                        VuslMessage::NoInstruction {
                            what: "multiplying int16 values".into(),
                            help: "VU1 has no integer multiply: add the value to itself, or keep a running sum",
                        },
                        pos,
                    );
                }
                BinaryOp::Div | BinaryOp::Mod => {
                    return err(
                        VuslMessage::NoInstruction {
                            what: format!("'{}' on int16 values", op.spelling()),
                            help: "VU1 has no integer divide: keep a counter, or compute it in float",
                        },
                        pos,
                    );
                }
                BinaryOp::Shl | BinaryOp::Shr => {
                    return err(
                        VuslMessage::NoInstruction {
                            what: "shifting an int16".into(),
                            help: "VU1 has no integer shift: double a value by adding it to itself",
                        },
                        pos,
                    );
                }
                BinaryOp::BitXor => {
                    return err(
                        VuslMessage::NoInstruction {
                            what: "'^' on int16 values".into(),
                            help: "VU1 has IAND and IOR but no exclusive or",
                        },
                        pos,
                    );
                }
                _ => unreachable!("comparisons and logic are handled first"),
            };
            return Ok(Expr {
                kind: ExprKind::Binary {
                    op: int_op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                ty: Type::Int16,
                placement: None,
                pos,
            });
        }
        if lhs.ty.is_raw() || rhs.ty.is_raw() {
            return err(
                VuslMessage::NoInstruction {
                    what: "arithmetic on raw integer bits".into(),
                    help: "convert them with itof first: an FMAC operation would flush them as denormals",
                },
                pos,
            );
        }
        if matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr)
            && (lhs.ty == Type::Bool || rhs.ty == Type::Bool)
        {
            return err(
                VuslMessage::NoInstruction {
                    what: format!("'{}' on bools", op.spelling()),
                    help: "write && or ||",
                },
                pos,
            );
        }
        match (&lhs.ty, &rhs.ty, op) {
            (Type::Mat { size, row_major }, Type::Vec(n), BinaryOp::Mul) => {
                if *row_major {
                    return err(
                        VuslMessage::NoInstruction {
                            what: "a row_major matrix times a vector on its right".into(),
                            help: "write vector * matrix, or store the matrix column_major",
                        },
                        pos,
                    );
                }
                self.matrix_product(BinOp::MatVec, *size, *n, lhs, rhs, pos)
            }
            (Type::Vec(n), Type::Mat { size, row_major }, BinaryOp::Mul) => {
                if !*row_major {
                    return err(
                        VuslMessage::NoInstruction {
                            what: "a vector times a column_major matrix on its right".into(),
                            help: "write matrix * vector, or store the matrix row_major",
                        },
                        pos,
                    );
                }
                self.matrix_product(BinOp::VecMat, *size, *n, lhs, rhs, pos)
            }
            (
                Type::Mat {
                    size: a,
                    row_major: ra,
                },
                Type::Mat {
                    size: b,
                    row_major: rb,
                },
                BinaryOp::Mul,
            ) => {
                if a != b || ra != rb {
                    return err(
                        VuslMessage::NoInstruction {
                            what: "multiplying matrices of different sizes or storage".into(),
                            help: "store both matrices the same way, at the same size",
                        },
                        pos,
                    );
                }
                let ty = lhs.ty.clone();
                Ok(Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::MatMat,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    ty,
                    placement: None,
                    pos,
                })
            }
            (Type::Float, Type::Mat { .. }, BinaryOp::Mul)
            | (Type::Mat { .. }, Type::Float, BinaryOp::Mul) => {
                let ty = if lhs.ty == Type::Float {
                    rhs.ty.clone()
                } else {
                    lhs.ty.clone()
                };
                Ok(Expr {
                    kind: ExprKind::Binary {
                        op: BinOp::ScaleMat,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    ty,
                    placement: None,
                    pos,
                })
            }
            (_, Type::Vec(_), BinaryOp::Div) if lhs.ty.is_float() => err(
                VuslMessage::NoInstruction {
                    what: "dividing by a vector".into(),
                    help: "the FDIV unit takes one scalar at a time: divide by each component",
                },
                pos,
            ),
            (left, right, BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div)
                if left.is_float() && right.is_float() =>
            {
                let float_op = match op {
                    BinaryOp::Add => BinOp::Add,
                    BinaryOp::Sub => BinOp::Sub,
                    BinaryOp::Mul => BinOp::Mul,
                    _ => BinOp::Div,
                };
                self.elementwise(float_op, lhs, rhs, &context, pos)
            }
            (
                Type::Float | Type::Vec(_),
                _,
                BinaryOp::Mod
                | BinaryOp::Shl
                | BinaryOp::Shr
                | BinaryOp::BitXor
                | BinaryOp::BitAnd
                | BinaryOp::BitOr,
            ) => err(
                VuslMessage::NoInstruction {
                    what: format!("'{}' on floats", op.spelling()),
                    help: "FMAC operations add, subtract, multiply and take minimum and maximum",
                },
                pos,
            ),
            _ => err(
                VuslMessage::TypeMismatch {
                    context,
                    expected: "operands of matching types".into(),
                    found: format!("{} and {}", self.type_text(&lhs), self.type_text(&rhs)),
                },
                pos,
            ),
        }
    }

    fn matrix_product(
        &self,
        op: BinOp,
        size: u8,
        n: u8,
        lhs: Expr,
        rhs: Expr,
        pos: Pos,
    ) -> Checked<Expr> {
        if size != n {
            return err(
                VuslMessage::TypeMismatch {
                    context: "the matrix product".into(),
                    expected: format!("a vec{size}"),
                    found: format!("a vec{n}"),
                },
                pos,
            );
        }
        Ok(Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            ty: Type::Vec(size),
            placement: Some(Placement::identity(usize::from(size))),
            pos,
        })
    }

    /// An FMAC operation element by element: vectors in the same lanes, or
    /// a vector and a scalar, which broadcasts.
    fn elementwise(
        &self,
        op: BinOp,
        lhs: Expr,
        rhs: Expr,
        context: &str,
        pos: Pos,
    ) -> Checked<Expr> {
        let (ty, placement) = match (&lhs.ty, &rhs.ty) {
            (Type::Vec(a), Type::Vec(b)) => {
                if a != b {
                    return err(
                        VuslMessage::TypeMismatch {
                            context: context.into(),
                            expected: "vectors of one size, or a vector and a float".into(),
                            found: format!("{} and {}", self.type_text(&lhs), self.type_text(&rhs)),
                        },
                        pos,
                    );
                }
                if lhs.placement != rhs.placement {
                    return err(
                        VuslMessage::LaneMismatch {
                            context: context.into(),
                            left: lanes_text(&lhs),
                            right: lanes_text(&rhs),
                        },
                        pos,
                    );
                }
                (lhs.ty.clone(), lhs.placement)
            }
            (Type::Vec(_), Type::Float) => (lhs.ty.clone(), lhs.placement),
            (Type::Float, Type::Vec(_)) => (rhs.ty.clone(), rhs.placement),
            (Type::Float, Type::Float) => (Type::Float, None),
            _ => {
                return err(
                    VuslMessage::TypeMismatch {
                        context: context.into(),
                        expected: "float operands".into(),
                        found: format!("{} and {}", self.type_text(&lhs), self.type_text(&rhs)),
                    },
                    pos,
                );
            }
        };
        if op == BinOp::Sub
            && lhs.ty == Type::Float
            && rhs.ty.is_float()
            && matches!(rhs.ty, Type::Vec(_))
        {
            return err(
                VuslMessage::NoInstruction {
                    what: "a scalar minus a vector".into(),
                    help: "the scalar can only be the broadcast operand: negate the difference, -(v - s)",
                },
                pos,
            );
        }
        Ok(Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            ty,
            placement,
            pos,
        })
    }

    fn compare(&self, op: BinaryOp, lhs: Expr, rhs: Expr, pos: Pos) -> Checked<Expr> {
        let compare_op = match op {
            BinaryOp::Less => CompareOp::Less,
            BinaryOp::LessEqual => CompareOp::LessEqual,
            BinaryOp::Greater => CompareOp::Greater,
            BinaryOp::GreaterEqual => CompareOp::GreaterEqual,
            BinaryOp::Equal => CompareOp::Equal,
            _ => CompareOp::NotEqual,
        };
        let scalars = match (&lhs.ty, &rhs.ty) {
            (Type::Int16, Type::Int16) | (Type::Float, Type::Float) => true,
            (Type::Ref(a), Type::Ref(b)) => a == b,
            _ => false,
        };
        if !scalars {
            let vectors = lhs.ty.vf_lanes().is_some_and(|lanes| lanes > 1);
            return err(
                if vectors {
                    VuslMessage::NoInstruction {
                        what: "comparing vectors".into(),
                        help: "compare scalar components",
                    }
                } else {
                    VuslMessage::TypeMismatch {
                        context: format!("'{}'", op.spelling()),
                        expected: "two int16 or two float scalars".into(),
                        found: format!("{} and {}", self.type_text(&lhs), self.type_text(&rhs)),
                    }
                },
                pos,
            );
        }
        Ok(Expr {
            kind: ExprKind::Compare {
                op: compare_op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            ty: Type::Bool,
            placement: None,
            pos,
        })
    }

    /// `value` as `ty`: the same type, or an integer constant where a float
    /// or an `int16` is wanted.
    fn coerce(&self, value: Expr, ty: &Type, context: &str) -> Checked<Expr> {
        if &value.ty == ty {
            if let (Type::Int16, Some(Const::Int(n))) = (ty, as_const(&value))
                && !int16_range(n)
            {
                return err(
                    VuslMessage::ConstantRange {
                        value: n.to_string(),
                        ty: "int16",
                    },
                    value.pos,
                );
            }
            return Ok(value);
        }
        if let (Type::Float, Some(Const::Int(_))) = (ty, as_const(&value)) {
            return Ok(self.to_float(value));
        }
        err(
            VuslMessage::TypeMismatch {
                context: context.into(),
                expected: self.type_name(ty),
                found: self.type_text(&value),
            },
            value.pos,
        )
    }

    // ----------------------------------------------
    // Calls
    // ----------------------------------------------

    fn call(&mut self, callee: &ast::TypeName, args: &[ast::Expr], pos: Pos) -> Checked<Value> {
        if callee.argument.is_some() {
            return self.make_ref(callee, args, pos).map(Value::Reg);
        }
        let name = callee.name.name.as_str();
        if let Some(ty) = builtin_type(name) {
            return self.construct(ty, name, args, pos).map(Value::Reg);
        }
        if let Some(function) = self.decls.functions.get(name).copied() {
            return match self.inline(function, args, pos)? {
                Some(value) => Ok(Value::Reg(value)),
                None => err(
                    VuslMessage::TypeMismatch {
                        context: format!("the call to '{name}'"),
                        expected: "a value".into(),
                        found: "void".into(),
                    },
                    pos,
                ),
            };
        }
        if name == "xgkick" {
            return err(VuslMessage::NestedAssignment, pos);
        }
        if self.decls.struct_ids.contains_key(name) {
            return err(
                VuslMessage::NotYet {
                    what: "a struct constructor",
                },
                pos,
            );
        }
        let args: Vec<Expr> = args
            .iter()
            .map(|arg| self.rvalue(arg))
            .collect::<Checked<_>>()?;
        self.builtin(name, args, pos).map(Value::Reg)
    }

    fn count(
        &self,
        name: &str,
        expected: &str,
        args: &[Expr],
        range: std::ops::RangeInclusive<usize>,
        pos: Pos,
    ) -> Checked<()> {
        if range.contains(&args.len()) {
            return Ok(());
        }
        err(
            VuslMessage::WrongArgumentCount {
                name: name.into(),
                expected: expected.into(),
                found: args.len(),
            },
            pos,
        )
    }

    fn mismatch(&self, arg: &Expr, context: &str, expected: &str) -> Box<Problem> {
        Box::new(problem(
            VuslMessage::TypeMismatch {
                context: context.into(),
                expected: expected.into(),
                found: self.type_text(arg),
            },
            arg.pos,
        ))
    }

    fn expect_type(
        &self,
        arg: &Expr,
        accepted: bool,
        context: &str,
        expected: &str,
    ) -> Checked<()> {
        if accepted {
            return Ok(());
        }
        err(
            VuslMessage::TypeMismatch {
                context: context.into(),
                expected: expected.into(),
                found: self.type_text(arg),
            },
            arg.pos,
        )
    }

    /// An operand of an instruction that works on x, y and z in their own
    /// lanes (OPMULA, CLIPW, the EFU's vector operations).
    fn xyz(&self, arg: &Expr, context: &str) -> Checked<()> {
        if arg.placement == Some(Placement::identity(3)) {
            return Ok(());
        }
        err(
            VuslMessage::NeedsLanes {
                what: context.into(),
                lanes: "xyz",
                found: lanes_text(arg),
            },
            arg.pos,
        )
    }

    fn builtin(&mut self, name: &str, args: Vec<Expr>, pos: Pos) -> Checked<Expr> {
        let call = |builtin, args, ty, placement| {
            Ok(Expr {
                kind: ExprKind::Call { builtin, args },
                ty,
                placement,
                pos,
            })
        };
        let context = format!("'{name}'");
        match name {
            "dot" | "cross" => {
                self.count(name, "2 arguments", &args, 2..=2, pos)?;
                let cross = name == "cross";
                for arg in &args {
                    let accepted = if cross {
                        arg.ty == Type::Vec(3)
                    } else {
                        matches!(arg.ty, Type::Vec(_))
                    };
                    self.expect_type(
                        arg,
                        accepted,
                        &context,
                        if cross { "a vec3" } else { "vectors" },
                    )?;
                }
                if args[0].ty != args[1].ty || args[0].placement != args[1].placement {
                    return err(
                        VuslMessage::LaneMismatch {
                            context,
                            left: lanes_text(&args[0]),
                            right: lanes_text(&args[1]),
                        },
                        pos,
                    );
                }
                if cross {
                    // OPMULA and OPMSUB take x, y and z in their own lanes.
                    self.xyz(&args[0], "'cross'")?;
                    let placement = args[0].placement;
                    call(Builtin::Cross, args, Type::Vec(3), placement)
                } else {
                    call(Builtin::Dot, args, Type::Float, None)
                }
            }
            "normalize" | "length" => {
                self.count(name, "1 argument", &args, 1..=1, pos)?;
                self.expect_type(
                    &args[0],
                    matches!(args[0].ty, Type::Vec(_)),
                    &context,
                    "a vector",
                )?;
                if name == "normalize" {
                    let (ty, placement) = (args[0].ty.clone(), args[0].placement);
                    call(Builtin::Normalize, args, ty, placement)
                } else {
                    call(Builtin::Length, args, Type::Float, None)
                }
            }
            "sqrt" | "rsqrt" | "ercpr" | "esqrt" | "ersqrt" | "esin" | "eatan" | "eexp" => {
                self.count(name, "1 argument", &args, 1..=1, pos)?;
                let args: Vec<Expr> = args.into_iter().map(|arg| self.to_float(arg)).collect();
                self.expect_type(&args[0], args[0].ty == Type::Float, &context, "a float")?;
                let builtin = match name {
                    "sqrt" => Builtin::Sqrt,
                    "rsqrt" => Builtin::Rsqrt,
                    "ercpr" => Builtin::Ercpr,
                    "esqrt" => Builtin::Esqrt,
                    "ersqrt" => Builtin::Ersqrt,
                    "esin" => Builtin::Esin,
                    "eatan" => Builtin::Eatan,
                    _ => Builtin::Eexp,
                };
                if name.starts_with('e') {
                    self.uses.efu = true;
                }
                call(builtin, args, Type::Float, None)
            }
            "esadd" | "ersadd" | "eleng" | "erleng" | "esum" => {
                self.count(name, "1 argument", &args, 1..=1, pos)?;
                let wanted = if name == "esum" {
                    Type::Vec(4)
                } else {
                    Type::Vec(3)
                };
                let expected = if name == "esum" { "a vec4" } else { "a vec3" };
                self.expect_type(&args[0], args[0].ty == wanted, &context, expected)?;
                if name != "esum" {
                    self.xyz(&args[0], &context)?;
                }
                self.uses.efu = true;
                let builtin = match name {
                    "esadd" => Builtin::Esadd,
                    "ersadd" => Builtin::Ersadd,
                    "eleng" => Builtin::Eleng,
                    "erleng" => Builtin::Erleng,
                    _ => Builtin::Esum,
                };
                call(builtin, args, Type::Float, None)
            }
            "min" | "max" => {
                self.count(name, "2 arguments", &args, 2..=2, pos)?;
                let mut args = args.into_iter();
                let (a, b) = (args.next().unwrap(), args.next().unwrap());
                let (a, b) = if a.ty.is_float() {
                    (a, self.to_float(b))
                } else {
                    (self.to_float(a), b)
                };
                let op = if name == "min" {
                    BinOp::Min
                } else {
                    BinOp::Max
                };
                self.elementwise(op, a, b, &context, pos)
            }
            "abs" => {
                self.count(name, "1 argument", &args, 1..=1, pos)?;
                self.expect_type(
                    &args[0],
                    args[0].ty.is_float(),
                    &context,
                    "a float or a vector",
                )?;
                let (ty, placement) = (args[0].ty.clone(), args[0].placement);
                call(Builtin::Abs, args, ty, placement)
            }
            "clamp" => {
                self.count(name, "3 arguments", &args, 3..=3, pos)?;
                let mut args = args.into_iter();
                let value = args.next().unwrap();
                let low = self.to_float(args.next().unwrap());
                let high = self.to_float(args.next().unwrap());
                self.expect_type(&value, value.ty.is_float(), &context, "a float or a vector")?;
                // Each bound is MAX and MINI against the value.
                self.elementwise(BinOp::Max, value.clone(), low.clone(), &context, pos)?;
                self.elementwise(BinOp::Min, value.clone(), high.clone(), &context, pos)?;
                let (ty, placement) = (value.ty.clone(), value.placement);
                call(Builtin::Clamp, vec![value, low, high], ty, placement)
            }
            "clipw" => {
                self.count(name, "2 arguments", &args, 2..=2, pos)?;
                let mut args = args.into_iter();
                let judge = args.next().unwrap();
                let w = self.to_float(args.next().unwrap());
                self.expect_type(
                    &judge,
                    judge.ty == Type::Vec(3),
                    &context,
                    "a vec3 to judge",
                )?;
                self.xyz(&judge, "'clipw'")?;
                self.expect_type(&w, w.ty == Type::Float, &context, "a float w")?;
                self.uses.clip = true;
                self.clips += 1;
                call(Builtin::ClipW, vec![judge, w], Type::ClipFlags, None)
            }
            "any" | "adc" => {
                self.count(name, "1 to 3 judgements", &args, 1..=3, pos)?;
                for arg in &args {
                    self.expect_type(arg, arg.ty == Type::ClipFlags, &context, "clipflags")?;
                    // The clip flags hold the last four judgements, of this
                    // block (VCL does not carry them across a branch).
                    if let ExprKind::Local(id) = arg.kind
                        && let Some(&(block, index)) = self.clip_judgements.get(&id)
                    {
                        let reason = if block != self.clip_block {
                            Some("a branch or a loop boundary lies between")
                        } else if self.clips - 1 - index >= 4 {
                            Some("four newer judgements pushed it out")
                        } else {
                            None
                        };
                        if let Some(reason) = reason {
                            return err(VuslMessage::ClipOutOfReach { reason }, arg.pos);
                        }
                    }
                }
                if name == "any" {
                    call(Builtin::Any, args, Type::Bool, None)
                } else {
                    call(Builtin::Adc, args, Type::Int16, None)
                }
            }
            _ if fixed_point(name).is_some() => {
                let (to_fixed, bits) = fixed_point(name).expect("a conversion");
                self.count(name, "1 argument", &args, 1..=1, pos)?;
                let arg = args.into_iter().next().unwrap();
                let arg = if to_fixed { self.to_float(arg) } else { arg };
                let lanes = arg.ty.vf_lanes().unwrap_or(0);
                let (accepted, ty) = if to_fixed {
                    (
                        arg.ty.is_float(),
                        if lanes == 1 {
                            Type::Int32
                        } else {
                            Type::IVec(lanes as u8)
                        },
                    )
                } else {
                    (
                        matches!(
                            arg.ty,
                            Type::Int32 | Type::Uint32 | Type::IVec(_) | Type::UVec(_)
                        ),
                        if lanes == 1 {
                            Type::Float
                        } else {
                            Type::Vec(lanes as u8)
                        },
                    )
                };
                let expected = if to_fixed {
                    "a float or a vector"
                } else {
                    "raw integer lanes"
                };
                self.expect_type(&arg, accepted, &context, expected)?;
                let conversion = if to_fixed {
                    Conversion::FloatToFixed(bits)
                } else {
                    Conversion::FixedToFloat(bits)
                };
                let placement = arg.placement;
                Ok(Expr {
                    kind: ExprKind::Convert {
                        conversion,
                        operand: Box::new(arg),
                    },
                    ty,
                    placement,
                    pos,
                })
            }
            _ => err(VuslMessage::UnknownFunction { name: name.into() }, pos),
        }
    }

    /// A type's constructor or conversion: `vec4(...)`, `float(i)`,
    /// `int16(f)`, `int32(i)`, `ivec4(v)`.
    fn construct(&mut self, ty: Type, name: &str, args: &[ast::Expr], pos: Pos) -> Checked<Expr> {
        let args: Vec<Expr> = args
            .iter()
            .map(|arg| self.rvalue(arg))
            .collect::<Checked<_>>()?;
        let context = format!("'{name}(...)'");
        let convert = |conversion, operand: Expr, ty: Type| {
            let placement = operand.placement;
            Ok(Expr {
                kind: ExprKind::Convert {
                    conversion,
                    operand: Box::new(operand),
                },
                ty,
                placement,
                pos,
            })
        };
        let single = |args: Vec<Expr>| -> Checked<Expr> {
            match <[Expr; 1]>::try_from(args) {
                Ok([arg]) => Ok(arg),
                Err(args) => err(
                    VuslMessage::WrongArgumentCount {
                        name: name.into(),
                        expected: "1 argument".into(),
                        found: args.len(),
                    },
                    pos,
                ),
            }
        };
        match &ty {
            Type::Float => {
                let arg = single(args)?;
                match (&arg.ty, as_const(&arg)) {
                    (_, Some(Const::Int(n))) => Ok(constant_expr(Const::Float(n as f32), pos)),
                    (Type::Float, _) => Ok(arg),
                    (Type::Int16, _) => convert(Conversion::IntToFloat, arg, Type::Float),
                    _ => self
                        .expect_type(&arg, false, &context, "an int16 or a float")
                        .map(|()| arg),
                }
            }
            Type::Int16 => {
                let arg = single(args)?;
                match (&arg.ty, as_const(&arg)) {
                    (_, Some(Const::Float(f))) => Ok(constant_expr(Const::Int(f as i64), pos)),
                    (Type::Int16, _) => Ok(arg),
                    // A ref's address.
                    (Type::Ref(_), _) => convert(Conversion::Bits, arg, Type::Int16),
                    (Type::Float, _) => convert(Conversion::FloatToInt, arg, Type::Int16),
                    (Type::Int32 | Type::Uint32, _) => {
                        convert(Conversion::LaneToInt, arg, Type::Int16)
                    }
                    _ => self
                        .expect_type(&arg, false, &context, "a float or a raw lane")
                        .map(|()| arg),
                }
            }
            Type::Int32 | Type::Uint32 => {
                let arg = single(args)?;
                match &arg.ty {
                    Type::Int16 => convert(Conversion::IntToLane, arg, ty.clone()),
                    Type::Int32 | Type::Uint32 => convert(Conversion::Bits, arg, ty.clone()),
                    _ => self
                        .expect_type(&arg, false, &context, "an int16 or a raw lane")
                        .map(|()| arg),
                }
            }
            Type::IVec(n) | Type::UVec(n) => {
                let arg = single(args)?;
                let lanes = arg.ty.vf_lanes().unwrap_or(0);
                if lanes != usize::from(*n) {
                    return self
                        .expect_type(&arg, false, &context, &format!("{n} components"))
                        .map(|()| arg);
                }
                match &arg.ty {
                    Type::Vec(_) => convert(Conversion::FloatToFixed(0), arg, ty.clone()),
                    Type::IVec(_) | Type::UVec(_) => convert(Conversion::Bits, arg, ty.clone()),
                    _ => self
                        .expect_type(&arg, false, &context, "a vector")
                        .map(|()| arg),
                }
            }
            Type::Vec(n) => {
                let n = usize::from(*n);
                if let [arg] = args.as_slice()
                    && matches!(arg.ty, Type::IVec(_) | Type::UVec(_))
                {
                    let arg = args.into_iter().next().unwrap();
                    if arg.ty.vf_lanes() != Some(n) {
                        return self
                            .expect_type(&arg, false, &context, &format!("{n} components"))
                            .map(|()| arg);
                    }
                    return convert(Conversion::FixedToFloat(0), arg, ty.clone());
                }
                let args: Vec<Expr> = args.into_iter().map(|arg| self.to_float(arg)).collect();
                let mut components = 0;
                for arg in &args {
                    if !arg.ty.is_float() {
                        return Err(self.mismatch(arg, &context, "floats and vectors"));
                    }
                    components += arg.ty.vf_lanes().unwrap_or(1);
                }
                let splat = args.len() == 1 && args[0].ty == Type::Float;
                if !splat && components != n {
                    return err(
                        VuslMessage::WrongArgumentCount {
                            name: name.into(),
                            expected: format!("{n} components"),
                            found: components,
                        },
                        pos,
                    );
                }
                Ok(Expr {
                    kind: ExprKind::Construct(args),
                    ty: ty.clone(),
                    placement: Some(Placement::identity(n)),
                    pos,
                })
            }
            _ => err(
                VuslMessage::NotYet {
                    what: "this type's constructor",
                },
                pos,
            ),
        }
    }

    /// Inlines a call: the parameters bound (copies for `in` values, the
    /// caller's variables for `out` and `inout`, the memory itself for
    /// structs), the body's statements added to the pending ones, and the
    /// final `return`'s value as the call's.
    fn inline(
        &mut self,
        function: &ast::Function,
        args: &[ast::Expr],
        pos: Pos,
    ) -> Checked<Option<Expr>> {
        let name = function.name.name.clone();
        self.inline_inner(function, args, pos)
            .map_err(|mut problem| {
                // A call that fails itself (a recursion) is the error's own place.
                if problem.pos != pos {
                    problem
                        .notes
                        .push((pos, VuslMessage::CalledHere { name: name.clone() }));
                }
                problem
            })
    }

    fn inline_inner(
        &mut self,
        function: &ast::Function,
        args: &[ast::Expr],
        pos: Pos,
    ) -> Checked<Option<Expr>> {
        let name = function.name.name.clone();
        if self.calls.iter().any(|(called, _)| *called == name) {
            return err(VuslMessage::Recursion { name }, pos);
        }
        if args.len() != function.params.len() {
            return err(
                VuslMessage::WrongArgumentCount {
                    name,
                    expected: format!("{} arguments", function.params.len()),
                    found: args.len(),
                },
                pos,
            );
        }
        self.called.insert(name.clone());
        let ret = resolve_type(&function.ret, &self.decls.struct_ids)?;
        let mut scope: HashMap<String, (Symbol, Pos)> = HashMap::new();
        let mut setup = Vec::new();
        for (param, arg) in function.params.iter().zip(args) {
            if let Some(layout) = &param.layout {
                return err(
                    VuslMessage::UnknownLayout {
                        name: "layout".into(),
                        context: "a function's parameter (only main's take placements)",
                    },
                    layout.pos,
                );
            }
            let ty = resolve_type(&param.ty, &self.decls.struct_ids)?;
            let qualifier = param
                .qualifier
                .map_or(Qualifier::In, |(qualifier, _)| qualifier);
            if qualifier == Qualifier::Uniform {
                return err(
                    VuslMessage::UnknownLayout {
                        name: "uniform".into(),
                        context: "a function's parameter",
                    },
                    param.pos,
                );
            }
            let symbol = match &ty {
                Type::Struct(_) | Type::Array(..) => {
                    let value = self.value(arg)?;
                    let Value::Mem(memory) = self.deref(value, false)? else {
                        return err(
                            VuslMessage::TypeMismatch {
                                context: format!("parameter '{}'", param.name.name),
                                expected: "memory".into(),
                                found: "a value in registers".into(),
                            },
                            arg.pos,
                        );
                    };
                    if memory.place.ty != ty {
                        return err(
                            VuslMessage::TypeMismatch {
                                context: format!("parameter '{}'", param.name.name),
                                expected: self.type_name(&ty),
                                found: self.type_name(&memory.place.ty),
                            },
                            arg.pos,
                        );
                    }
                    let allowed = match qualifier {
                        Qualifier::Out => memory.writable(),
                        Qualifier::Inout => memory.readable() && memory.writable(),
                        _ => memory.readable(),
                    };
                    if !allowed {
                        let message = if memory.writable() {
                            VuslMessage::WriteOnly {
                                name: memory.name.clone(),
                            }
                        } else {
                            VuslMessage::ReadOnly {
                                name: memory.name.clone(),
                            }
                        };
                        return err(message, arg.pos);
                    }
                    Symbol::Memory(Memory {
                        access: qualifier,
                        ..memory
                    })
                }
                _ if qualifier == Qualifier::In => {
                    let value = self.rvalue(arg)?;
                    let value =
                        self.coerce(value, &ty, &format!("parameter '{}'", param.name.name))?;
                    let local =
                        self.new_local(&param.name.name, ty.clone(), None, false, param.name.pos);
                    let target = Target::Local {
                        id: local,
                        column: None,
                        components: None,
                    };
                    setup.push(self.set(target, value, arg.pos)?);
                    Symbol::Local(local)
                }
                _ => {
                    let target = self.target(arg)?;
                    let (target_ty, _) = self.target_shape(&target);
                    if target_ty != ty {
                        return err(
                            VuslMessage::TypeMismatch {
                                context: format!("parameter '{}'", param.name.name),
                                expected: self.type_name(&ty),
                                found: self.type_name(&target_ty),
                            },
                            arg.pos,
                        );
                    }
                    if qualifier == Qualifier::Inout {
                        self.read_target(&target, arg.pos)?;
                    }
                    Symbol::Alias(target)
                }
            };
            if scope.contains_key(&param.name.name) {
                return err(
                    VuslMessage::Redeclared {
                        name: param.name.name.clone(),
                    },
                    param.name.pos,
                );
            }
            scope.insert(param.name.name.clone(), (symbol, param.name.pos));
        }
        // The body: a value-returning function ends with `return value;`.
        let stmts = &function.body.stmts;
        let (stmts, returned) = match stmts.split_last() {
            Some((
                ast::Stmt {
                    kind: StmtKind::Return(value),
                    pos: return_pos,
                },
                rest,
            )) => (rest, Some((value.as_ref(), *return_pos))),
            _ => (stmts.as_slice(), None),
        };
        match (&ret, returned) {
            (Type::Void, Some((Some(_), return_pos))) => {
                return err(VuslMessage::ReturnMismatch { name }, return_pos);
            }
            (Type::Void, _) => {}
            (_, Some((Some(_), _))) => {}
            (_, Some((None, return_pos))) => {
                return err(VuslMessage::ReturnMismatch { name }, return_pos);
            }
            (_, None) => return err(VuslMessage::MissingReturn { name }, function.name.pos),
        }
        let saved_scopes = std::mem::replace(&mut self.scopes, vec![scope]);
        let saved_pending = std::mem::take(&mut self.pending);
        let saved_loops = std::mem::take(&mut self.loops);
        self.calls.push((name, pos));
        // The returned value sees the body's locals: one scope holds both.
        self.scopes.push(HashMap::new());
        let result = (|| -> Checked<(Vec<Stmt>, Option<Expr>)> {
            let mut body = self.statements(stmts)?;
            let value = match returned {
                Some((Some(value), _)) if ret != Type::Void => {
                    let value = self.rvalue(value)?;
                    let value = self.coerce(value, &ret, "the returned value")?;
                    body.append(&mut self.pending);
                    Some(value)
                }
                _ => None,
            };
            Ok((body, value))
        })();
        self.scopes.pop();
        self.calls.pop();
        self.loops = saved_loops;
        self.scopes = saved_scopes;
        self.pending = saved_pending;
        let (body, value) = result?;
        self.pending.extend(setup);
        self.pending.extend(body);
        Ok(value)
    }
}

/// `ftoi4` is (to fixed, 4); `itof12` (to float, 12).
fn fixed_point(name: &str) -> Option<(bool, u8)> {
    let (to_fixed, bits) = if let Some(bits) = name.strip_prefix("ftoi") {
        (true, bits)
    } else {
        (false, name.strip_prefix("itof")?)
    };
    match bits {
        "0" => Some((to_fixed, 0)),
        "4" => Some((to_fixed, 4)),
        "12" => Some((to_fixed, 12)),
        "15" => Some((to_fixed, 15)),
        _ => None,
    }
}

/// The qwords an array element takes.
fn element_qwords(element: &Type, structs: &[super::layout::StructDef]) -> u32 {
    whole_qwords(element, structs).unwrap_or(1)
}

/// The qwords a type takes when its size is fixed.
fn whole_qwords(ty: &Type, structs: &[super::layout::StructDef]) -> Option<u32> {
    match ty {
        Type::Struct(id) => {
            let def = &structs[*id];
            (def.size.is_fixed() && def.open.is_none()).then_some(def.size.fixed)
        }
        Type::Mat { size, .. } => Some(u32::from(*size)),
        Type::Array(element, Length::Fixed(n)) => {
            Some(n * whole_qwords(element, structs).unwrap_or(1))
        }
        Type::Array(..) => None,
        _ => Some(1),
    }
}

/// Whether a later range's dependent length is read from the same place
/// as the first range's.
/// The most copies `#pragma unroll(N)` makes of a body.
const MAX_UNROLL: u32 = 16;

/// Whether `body` has a `break` or `continue` of its own loop.
fn leaves_loop(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::If {
            then, otherwise, ..
        } => leaves_loop(then) || leaves_loop(otherwise),
        _ => false,
    })
}

fn same_length_place(place: &Place, elements: &[ElementId], all: &[Element]) -> bool {
    let Some(first) = elements.first().map(|&id| &all[id]) else {
        return true;
    };
    match (&first.array.length, &place.length) {
        (Some(a), Some(b)) => a.base == b.base && a.path == b.path,
        (None, None) => true,
        _ => false,
    }
}
