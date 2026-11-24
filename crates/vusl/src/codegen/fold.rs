//! Algebraic folds over the checked program, before lowering. Each removes
//! an instruction the source asks for without changing any non-zero value:
//!
//! - `0.0 + x` and `x + 0.0` are `x`: the `add` from VF00 is gone, and a
//!   scalar keeps the lane it was read from.
//! - `a + (-b)` is `a - b` and `a - (-b)` is `a + b`, where `-b` is a
//!   negation in the expression or a local set to one (`jn = -jp`, then
//!   `d + jn.z` is `d - jp.z`). The negation's `sub` from VF00 is then
//!   unread, and vcl drops it.
//!
//! - `d = e; d = f(d);` is `d = f(e);` when `e` is a read of locals,
//!   memory or their components, and `f` reads `d` once: the copy into `d` is gone (`float d = pos.w;
//!   d = d * scale;` is one multiply, its result in w).
//!
//! None of these changes a non-zero result. Only the sign of a zero may
//! differ: `0.0 + x` may be +0 where `x` is -0 (or a denormal, which the
//! VU reads as zero), and `a + (-b)` may be +0 where `a - b` is -0. Only a
//! store of that zero, or its bits read through `floatBitsTo*`, can tell
//! them apart; locals whose bits are read are left as written.
//!
//! A local's negation is known from the `SetLocal` that sets it to `-e`,
//! where `e` reads only locals and constants, until the local or anything
//! `e` reads is set again. Branches keep what holds on both paths, a loop
//! forgets what it sets, and a loop with `break` or `continue` forgets what
//! its body learned before its step and condition. A library call forgets
//! everything.

use std::collections::HashMap;

use crate::sema::hir::{BinOp, Const, Expr, ExprKind, Local, LocalId, LoopKind, Program, Stmt};
use crate::sema::types::Type;

/// `program` with the folds applied; `bits_read` marks the locals whose
/// bits are read.
pub fn run(program: &Program, bits_read: &[bool]) -> Program {
    let mut program = program.clone();
    let mut folder = Folder {
        bits_read,
        negations: HashMap::new(),
    };
    folder.stmts(&mut program.body);
    let locals = program.locals.clone();
    merge_copies(&mut program.body, &locals, bits_read);
    program
}

struct Folder<'a> {
    bits_read: &'a [bool],
    /// Locals known to hold the negation of an expression: `local = -e`.
    negations: HashMap<LocalId, Expr>,
}

impl Folder<'_> {
    fn stmts(&mut self, stmts: &mut [Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::SetLocal {
                local,
                column,
                components,
                value,
                ..
            } => {
                let raw = self.bits_read.get(*local).copied().unwrap_or(true);
                if !raw {
                    self.expr(value);
                }
                self.forget(*local);
                if !raw
                    && column.is_none()
                    && components.is_none()
                    && let ExprKind::Neg(operand) = &value.kind
                    && only_locals(operand)
                    && !reads(operand, *local)
                {
                    self.negations.insert(*local, (**operand).clone());
                }
            }
            Stmt::Store { value, .. } => self.expr(value),
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.expr(cond);
                let before = self.negations.clone();
                self.stmts(then);
                let after_then = std::mem::replace(&mut self.negations, before);
                self.stmts(otherwise);
                self.negations
                    .retain(|local, e| after_then.get(local).is_some_and(|other| same(other, e)));
            }
            Stmt::Loop { kind, body, .. } => {
                let mut set = Vec::new();
                sets_in(body, &mut set);
                if let LoopKind::Condition {
                    cond_stmts, step, ..
                } = kind
                {
                    sets_in(cond_stmts, &mut set);
                    sets_in(step, &mut set);
                }
                for &local in &set {
                    self.forget(local);
                }
                if calls_in(body) {
                    self.negations.clear();
                }
                let head = self.negations.clone();
                let jumps = jumps_out(body);
                match kind {
                    LoopKind::Condition {
                        cond_stmts,
                        cond,
                        test_first,
                        step,
                    } => {
                        if *test_first {
                            self.stmts(cond_stmts);
                            self.expr(cond);
                            self.stmts(body);
                            if jumps {
                                self.negations = head.clone();
                            }
                            self.stmts(step);
                        } else {
                            self.stmts(body);
                            if jumps {
                                self.negations = head.clone();
                            }
                            self.stmts(step);
                            self.stmts(cond_stmts);
                            self.expr(cond);
                        }
                    }
                    LoopKind::Range { .. } => self.stmts(body),
                }
                self.negations = head;
            }
            Stmt::Call { .. } => self.negations.clear(),
            Stmt::Copy { .. }
            | Stmt::Kick { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Exit { .. }
            | Stmt::Yield { .. }
            | Stmt::Barrier { .. } => {}
        }
    }

    /// `local` is set: what was known of it, or of expressions reading it,
    /// no longer holds.
    fn forget(&mut self, local: LocalId) {
        self.negations
            .retain(|&known, e| known != local && !reads(e, local));
    }

    fn expr(&mut self, expr: &mut Expr) {
        match &mut expr.kind {
            ExprKind::Const(_) | ExprKind::Local(_) | ExprKind::Load(_) | ExprKind::Address(_) => {}
            ExprKind::Convert {
                conversion,
                operand,
            } => {
                // Raw bits keep every bit: nothing under them folds.
                if *conversion != crate::sema::hir::Conversion::Bits {
                    self.expr(operand);
                }
                return;
            }
            ExprKind::Swizzle { base, .. } => self.expr(base),
            ExprKind::Neg(operand) | ExprKind::Not(operand) => self.expr(operand),
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Compare { lhs, rhs, .. }
            | ExprKind::And(lhs, rhs)
            | ExprKind::Or(lhs, rhs) => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Call { args, .. } => {
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::Construct(parts) => {
                for part in parts {
                    self.expr(part);
                }
            }
            ExprKind::Column { matrix, .. } => self.expr(matrix),
            ExprKind::RefTo { address, .. } => self.expr(address),
        }
        if let Some(folded) = self.fold(expr) {
            *expr = folded;
        }
    }

    /// The fold of a sum or difference, when one applies.
    fn fold(&self, expr: &Expr) -> Option<Expr> {
        let ExprKind::Binary { op, lhs, rhs } = &expr.kind else {
            return None;
        };
        let keeps = |operand: &Expr| operand.ty == expr.ty;
        match op {
            BinOp::Add if is_zero(rhs) && keeps(lhs) => Some((**lhs).clone()),
            BinOp::Add if is_zero(lhs) && keeps(rhs) => Some((**rhs).clone()),
            // The first operand of a difference cannot be a scalar under a
            // vector: only the second broadcasts.
            BinOp::Add if keeps(lhs) && self.negated(rhs).is_some() => Some(binary(
                expr,
                BinOp::Sub,
                (**lhs).clone(),
                self.negated(rhs)?,
            )),
            BinOp::Add if keeps(rhs) && self.negated(lhs).is_some() => Some(binary(
                expr,
                BinOp::Sub,
                (**rhs).clone(),
                self.negated(lhs)?,
            )),
            BinOp::Sub if self.negated(rhs).is_some() => Some(binary(
                expr,
                BinOp::Add,
                (**lhs).clone(),
                self.negated(rhs)?,
            )),
            _ => None,
        }
    }

    /// `b` when `expr` is `-b`: a negation, or a local, or components of
    /// one, known to hold a negation.
    fn negated(&self, expr: &Expr) -> Option<Expr> {
        match &expr.kind {
            ExprKind::Neg(operand) => Some((**operand).clone()),
            ExprKind::Local(local) => self.negations.get(local).cloned(),
            ExprKind::Swizzle { base, components } => {
                let ExprKind::Local(local) = &base.kind else {
                    return None;
                };
                let operand = self.negations.get(local)?;
                Some(Expr {
                    kind: ExprKind::Swizzle {
                        base: Box::new(operand.clone()),
                        components: components.clone(),
                    },
                    ..expr.clone()
                })
            }
            _ => None,
        }
    }
}

fn binary(like: &Expr, op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    Expr {
        kind: ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        ..like.clone()
    }
}

fn is_zero(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Const(Const::Float(value)) if value == 0.0)
}

/// Whether `expr` is a read: a constant, a local, memory, or components of
/// one.
fn is_read(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Const(_) | ExprKind::Local(_) | ExprKind::Load(_) => true,
        ExprKind::Swizzle { base, .. } => is_read(base),
        _ => false,
    }
}

/// Whether `expr` reads only locals and constants, so it keeps its value
/// until one of its locals is set.
fn only_locals(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Const(_) | ExprKind::Local(_) => true,
        ExprKind::Swizzle { base, .. } => only_locals(base),
        _ => false,
    }
}

fn reads(expr: &Expr, local: LocalId) -> bool {
    match &expr.kind {
        ExprKind::Local(id) => *id == local,
        ExprKind::Swizzle { base, .. } => reads(base, local),
        _ => false,
    }
}

/// Whether two expressions that read only locals and constants are the
/// same.
fn same(a: &Expr, b: &Expr) -> bool {
    match (&a.kind, &b.kind) {
        (ExprKind::Local(x), ExprKind::Local(y)) => x == y,
        (ExprKind::Const(x), ExprKind::Const(y)) => x == y,
        (
            ExprKind::Swizzle {
                base: x,
                components: cx,
            },
            ExprKind::Swizzle {
                base: y,
                components: cy,
            },
        ) => cx == cy && same(x, y),
        _ => false,
    }
}

/// The locals `stmts` set, at any depth.
fn sets_in(stmts: &[Stmt], set: &mut Vec<LocalId>) {
    for stmt in stmts {
        match stmt {
            Stmt::SetLocal { local, .. } => set.push(*local),
            Stmt::If {
                then, otherwise, ..
            } => {
                sets_in(then, set);
                sets_in(otherwise, set);
            }
            Stmt::Loop { kind, body, .. } => {
                sets_in(body, set);
                if let LoopKind::Condition {
                    cond_stmts, step, ..
                } = kind
                {
                    sets_in(cond_stmts, set);
                    sets_in(step, set);
                }
            }
            Stmt::Call { setup, body, .. } => {
                sets_in(setup, set);
                sets_in(body, set);
            }
            _ => {}
        }
    }
}

/// `d = e; d = f(d);` merged into `d = f(e);`, at every depth, for a VF
/// local `d` with no fixed lanes whose bits are not read, an `e` that is a
/// read of locals, memory or their components, and an `f` that reads `d`
/// once. The statements are adjacent, so `e` reads the same values in
/// either.
fn merge_copies(stmts: &mut Vec<Stmt>, locals: &[Local], bits_read: &[bool]) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::If {
                then, otherwise, ..
            } => {
                merge_copies(then, locals, bits_read);
                merge_copies(otherwise, locals, bits_read);
            }
            Stmt::Loop { kind, body, .. } => {
                merge_copies(body, locals, bits_read);
                if let LoopKind::Condition {
                    cond_stmts, step, ..
                } = kind
                {
                    merge_copies(cond_stmts, locals, bits_read);
                    merge_copies(step, locals, bits_read);
                }
            }
            _ => {}
        }
    }
    let mut index = 0;
    while index + 1 < stmts.len() {
        if let Some(merged) = merged_copy(&stmts[index], &stmts[index + 1], locals, bits_read) {
            stmts[index + 1] = merged;
            stmts.remove(index);
        } else {
            index += 1;
        }
    }
}

fn merged_copy(first: &Stmt, second: &Stmt, locals: &[Local], bits_read: &[bool]) -> Option<Stmt> {
    let Stmt::SetLocal {
        local,
        column: None,
        components: None,
        value: copied,
        ..
    } = first
    else {
        return None;
    };
    let Stmt::SetLocal {
        local: again,
        column: None,
        components: None,
        value,
        pos,
    } = second
    else {
        return None;
    };
    let info = locals.get(*local)?;
    let vf = matches!(info.ty, Type::Float | Type::Vec(_));
    if again != local
        || !vf
        || info.fixed.is_some()
        || bits_read.get(*local).copied().unwrap_or(true)
        || !is_read(copied)
        || mentions(value, *local) != 1
        || under_swizzle(value, *local)
    {
        return None;
    }
    let mut value = value.clone();
    substitute(&mut value, *local, copied);
    Some(Stmt::SetLocal {
        local: *local,
        column: None,
        components: None,
        value,
        pos: *pos,
    })
}

/// How many times `expr` reads `local`.
fn mentions(expr: &Expr, local: LocalId) -> usize {
    let mut count = usize::from(matches!(expr.kind, ExprKind::Local(id) if id == local));
    each_child(expr, &mut |child| count += mentions(child, local));
    count
}

/// Whether `expr` reads components of `local` (`d.xy`): substituting
/// those would swizzle a swizzle, so only whole reads are merged.
fn under_swizzle(expr: &Expr, local: LocalId) -> bool {
    if let ExprKind::Swizzle { base, .. } = &expr.kind
        && matches!(base.kind, ExprKind::Local(id) if id == local)
    {
        return true;
    }
    let mut found = false;
    each_child(expr, &mut |child| found |= under_swizzle(child, local));
    found
}

/// `expr` with its read of `local` replaced by `value`.
fn substitute(expr: &mut Expr, local: LocalId, value: &Expr) {
    if matches!(expr.kind, ExprKind::Local(id) if id == local) {
        *expr = value.clone();
        return;
    }
    each_child_mut(expr, &mut |child| substitute(child, local, value));
}

fn each_child(expr: &Expr, visit: &mut dyn FnMut(&Expr)) {
    match &expr.kind {
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
        ExprKind::Call { args: parts, .. } | ExprKind::Construct(parts) => {
            parts.iter().for_each(visit);
        }
    }
}

fn each_child_mut(expr: &mut Expr, visit: &mut dyn FnMut(&mut Expr)) {
    match &mut expr.kind {
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
        ExprKind::Call { args: parts, .. } | ExprKind::Construct(parts) => {
            parts.iter_mut().for_each(visit);
        }
    }
}

/// Whether `stmts` call a library, at any depth: a call may set locals
/// that no `SetLocal` names.
fn calls_in(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::Call { .. } => true,
        Stmt::If {
            then, otherwise, ..
        } => calls_in(then) || calls_in(otherwise),
        Stmt::Loop { kind, body, .. } => {
            calls_in(body)
                || matches!(kind, LoopKind::Condition { cond_stmts, step, .. }
                    if calls_in(cond_stmts) || calls_in(step))
        }
        _ => false,
    })
}

/// Whether `stmts` hold a `break` or `continue` of the loop around them.
fn jumps_out(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::If {
            then, otherwise, ..
        } => jumps_out(then) || jumps_out(otherwise),
        _ => false,
    })
}
