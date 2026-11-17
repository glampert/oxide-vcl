//! Code generation: the checked program lowered to VCL source for
//! `vcl::compile` to schedule, pair and allocate. Lowering is
//! destination-driven: an expression computes into the register its
//! consumer wants where it can, vectors stay in the lanes the checker
//! placed them in, and a scalar sits in whichever lane its instruction
//! leaves it.
//!
//! - Memory places are a base register and an immediate: VI00 and the
//!   address for a fixed parameter, the XTOP register for the double
//!   buffer, a pointer for a range element. Lengths read from memory and
//!   run-time indices add to the base.
//! - A range-for counts down from its length; each element's pointer steps
//!   by its stride, so the body addresses elements with immediates.
//! - Matrix products are MULA/MADDA/MADD chains on the vector's components
//!   as broadcast operands; a component that is the constant 1 is VF00's w
//!   and a 0 drops its term.
//! - Comparisons of floats subtract and test the status flags (FSAND);
//!   comparisons of `int16` values branch on them or on their difference.

mod emit;

pub use emit::{Vf, Vi};

use emit::{Output, mask_of};

use crate::diagnostics::VuslMessage;
use crate::sema::hir::*;
use crate::sema::layout::StructDef;
use crate::sema::types::{Lane, Placement, Type};
use crate::sema::{Problem, problem};
use crate::syntax::Pos;

/// Generated VCL source, and for each of its lines the place in the
/// preprocessed program it came from.
pub struct Generated {
    pub text: String,
    pub origins: Vec<Option<Pos>>,
}

type Lowered<T> = Result<T, Box<Problem>>;

/// A VF value: the register holding it and the lanes of its components.
#[derive(Clone, Copy, Debug)]
struct Fv {
    reg: Vf,
    placement: Placement,
}

impl Fv {
    fn scalar(reg: Vf, lane: Lane) -> Self {
        Self {
            reg,
            placement: Placement::starting_at(lane, 1).expect("one lane"),
        }
    }

    fn lane(&self) -> Lane {
        self.placement.lane(0)
    }

    fn is_scalar(&self) -> bool {
        self.placement.len() == 1
    }
}

/// One component of a vector operand of a matrix product or constructor.
#[derive(Clone, Copy, Debug)]
enum Component {
    Zero,
    One,
    Lane(Vf, Lane),
}

/// The registers of a local.
#[derive(Clone, Debug)]
enum LocalReg {
    Unset,
    Vf(Vf),
    Vi(Vi),
    Mat(Vec<Vf>),
    /// A clip judgement: its number among the CLIPWs emitted.
    Clip(usize),
}

/// Lowers `program` to VCL source; `source` is the file it came from.
pub fn generate(program: &Program, source: &str) -> Lowered<Generated> {
    let mut lowering = Lowering {
        program,
        out: Output::default(),
        locals: vec![LocalReg::Unset; program.locals.len()],
        placements: vec![None; program.locals.len()],
        elements: vec![None; program.elements.len()],
        xtop: None,
        loops: Vec::new(),
        exit: String::new(),
        clips: 0,
        clips_reachable_from: 0,
    };
    lowering.program_text(source)?;
    let (text, origins) = lowering.out.finish();
    Ok(Generated { text, origins })
}

struct Lowering<'p> {
    program: &'p Program,
    out: Output,
    locals: Vec<LocalReg>,
    /// Where each local's value is now, as the code goes.
    placements: Vec<Option<Placement>>,
    /// Each range element's pointer, inside its loop.
    elements: Vec<Option<Vi>>,
    xtop: Option<Vi>,
    /// The `break` and `continue` labels of the loops being lowered.
    loops: Vec<(String, String)>,
    exit: String,
    /// The CLIPW judgements emitted so far, and the first one still
    /// readable: a branch or a loop boundary loses track of the flags.
    clips: usize,
    clips_reachable_from: usize,
}

fn float_text(value: f32) -> String {
    let text = format!("{value:?}");
    if text.contains(['.', 'e', 'E']) {
        text
    } else {
        format!("{text}.0")
    }
}

/// Whether `expr` reads local `id`.
fn reads_local(expr: &Expr, id: LocalId) -> bool {
    match &expr.kind {
        ExprKind::Local(local) => *local == id,
        ExprKind::Const(_) => false,
        ExprKind::Load(place) => place
            .offset
            .indices
            .iter()
            .any(|(index, _)| reads_local(index, id)),
        ExprKind::Swizzle { base, .. }
        | ExprKind::Neg(base)
        | ExprKind::Not(base)
        | ExprKind::Convert { operand: base, .. }
        | ExprKind::Column { matrix: base, .. } => reads_local(base, id),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Compare { lhs, rhs, .. }
        | ExprKind::And(lhs, rhs)
        | ExprKind::Or(lhs, rhs) => reads_local(lhs, id) || reads_local(rhs, id),
        ExprKind::Call { args, .. } | ExprKind::Construct(args) => {
            args.iter().any(|arg| reads_local(arg, id))
        }
    }
}

/// The lanes of a value of `ty` in memory at `lane`.
fn memory_lanes(ty: &Type, lane: Lane) -> Option<Placement> {
    let lanes = match ty {
        Type::Qword => 4,
        ty => ty.vf_lanes()?,
    };
    Placement::starting_at(lane, lanes)
}

impl Lowering<'_> {
    fn not_yet<T>(&self, what: &'static str) -> Lowered<T> {
        Err(Box::new(problem(
            VuslMessage::NotYet { what },
            self.out.pos.unwrap_or_default(),
        )))
    }

    fn name(&self, reg: Vf) -> String {
        self.out.vf_name(reg)
    }

    fn iname(&self, reg: Vi) -> String {
        self.out.vi_name(reg)
    }

    fn field(&self, fv: Fv) -> String {
        self.out.field(fv.reg, fv.lane())
    }

    fn mask(placement: &Placement) -> String {
        mask_of(placement)
    }

    fn dest(&mut self, dest: Option<Vf>) -> Vf {
        dest.unwrap_or_else(|| self.out.vf("t"))
    }

    fn idest(&mut self, dest: Option<Vi>) -> Vi {
        dest.unwrap_or_else(|| self.out.vi("t"))
    }

    // ----------------------------------------------
    // The program
    // ----------------------------------------------

    fn program_text(&mut self, source: &str) -> Lowered<()> {
        let program = self.program;
        self.out.line(format!(
            "; generated by oxide-vusl {} from {source}",
            env!("CARGO_PKG_VERSION")
        ));
        for line in [
            ".syntax new".to_string(),
            format!(".name {}", program.name),
            ".init_vf_all".into(),
            ".init_vi_all".into(),
            ".vu".into(),
            "--enter".into(),
            "--endenter".into(),
        ] {
            self.out.line(line);
        }
        if program.uses.clip {
            // VCL wants the clip flags set before the first CLIPW reads them.
            self.out.inst("fcset", None, &["0x000000".into()]);
        }
        if program.uses.xtop {
            let base = self.out.vi("xtop");
            let name = self.iname(base);
            self.out.inst("xtop", None, &[name]);
            self.xtop = Some(base);
        }
        self.exit = self.out.label("exit");
        self.stmts(&program.body)?;
        let exit = self.exit.clone();
        self.out.pos = None;
        self.out.place_label(&exit);
        self.out.line("--exit");
        self.out.line("--endexit");
        Ok(())
    }

    fn stmts(&mut self, stmts: &[Stmt]) -> Lowered<()> {
        for stmt in stmts {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Lowered<()> {
        match stmt {
            Stmt::SetLocal {
                local,
                column,
                components,
                value,
                pos,
            } => {
                self.out.pos = Some(*pos);
                self.set_local(*local, *column, components.as_deref(), value)
            }
            Stmt::Store {
                place,
                components,
                value,
                pos,
            } => {
                self.out.pos = Some(*pos);
                self.store(place, components.as_deref(), value)
            }
            Stmt::Copy {
                to,
                from,
                qwords,
                pos,
            } => {
                self.out.pos = Some(*pos);
                let (from_base, from_offset) = self.address(from)?;
                let (to_base, to_offset) = self.address(to)?;
                for qword in 0..i64::from(*qwords) {
                    let value = self.out.vf("copy");
                    let (value_name, from_name, to_name) =
                        (self.name(value), self.iname(from_base), self.iname(to_base));
                    self.out.inst(
                        "lq",
                        None,
                        &[
                            value_name.clone(),
                            format!("{}({from_name})", from_offset + qword),
                        ],
                    );
                    self.out.inst(
                        "sq",
                        None,
                        &[value_name, format!("{}({to_name})", to_offset + qword)],
                    );
                }
                Ok(())
            }
            Stmt::Kick { place, pos } => {
                self.out.pos = Some(*pos);
                let (base, offset) = self.address(place)?;
                let address = if offset == 0 && base != Vi::Zero {
                    base
                } else {
                    let address = self.out.vi("kick");
                    let (address_name, base_name) = (self.iname(address), self.iname(base));
                    self.out.inst(
                        "iaddiu",
                        None,
                        &[address_name, base_name, offset.to_string()],
                    );
                    address
                };
                let name = self.iname(address);
                self.out.inst("xgkick", None, &[name]);
                Ok(())
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                pos,
            } => {
                self.out.pos = Some(*pos);
                let otherwise_label = self.out.label("else");
                let end = self.out.label("endif");
                self.branch(cond, false, &otherwise_label)?;
                self.clips_reachable_from = self.clips;
                self.stmts(then)?;
                if !otherwise.is_empty() {
                    self.out.pos = Some(*pos);
                    self.out.inst("b", None, std::slice::from_ref(&end));
                }
                self.out.place_label(&otherwise_label);
                self.clips_reachable_from = self.clips;
                self.stmts(otherwise)?;
                self.out.place_label(&end);
                self.clips_reachable_from = self.clips;
                Ok(())
            }
            Stmt::Loop {
                kind,
                body,
                hints,
                pos,
            } => {
                self.out.pos = Some(*pos);
                self.lower_loop(kind, body, hints, *pos)
            }
            Stmt::Break { pos } | Stmt::Continue { pos } => {
                self.out.pos = Some(*pos);
                let Some((exit, next)) = self.loops.last().cloned() else {
                    return self.not_yet("break or continue outside a loop");
                };
                let target = if matches!(stmt, Stmt::Break { .. }) {
                    exit
                } else {
                    next
                };
                self.out.inst("b", None, &[target]);
                Ok(())
            }
            Stmt::Exit { pos } => {
                self.out.pos = Some(*pos);
                let exit = self.exit.clone();
                self.out.inst("b", None, &[exit]);
                Ok(())
            }
            Stmt::Yield { pos } => {
                self.out.pos = Some(*pos);
                self.out.line("--cont");
                Ok(())
            }
            Stmt::Barrier { pos } => {
                self.out.pos = Some(*pos);
                self.out.line("--barrier");
                Ok(())
            }
        }
    }

    fn loop_hints(&mut self, hints: &LoopHints) {
        match hints.pipeline {
            Some(Pipeline::Off) => self.out.line("--LoopCS 0,0"),
            Some(Pipeline::Hint {
                min,
                safe,
                extra,
                abs,
            }) => {
                self.out.line(format!("--LoopCS {min},{safe}"));
                if let Some(extra) = extra {
                    self.out.line(format!("--LoopExtra {extra}"));
                }
                if let Some(abs) = abs {
                    self.out.line(format!("--LoopAbs {abs}"));
                }
            }
            None => {}
        }
    }

    fn lower_loop(
        &mut self,
        kind: &LoopKind,
        body: &[Stmt],
        hints: &LoopHints,
        pos: Pos,
    ) -> Lowered<()> {
        let top = self.out.label("loop");
        let next = self.out.label("next");
        let exit = self.out.label("done");
        self.clips_reachable_from = self.clips;
        match kind {
            LoopKind::Condition {
                cond_stmts,
                cond,
                test_first,
                step,
            } => {
                if *test_first {
                    self.stmts(cond_stmts)?;
                    self.out.pos = Some(pos);
                    self.branch(cond, false, &exit)?;
                }
                self.out.place_label(&top);
                self.loop_hints(hints);
                self.loops.push((exit.clone(), next.clone()));
                let result = self.stmts(body);
                self.loops.pop();
                result?;
                self.out.place_label(&next);
                self.stmts(step)?;
                self.stmts(cond_stmts)?;
                self.out.pos = Some(pos);
                self.branch(cond, true, &top)?;
            }
            LoopKind::Range { elements, count } => {
                let counter = self.out.vi("count");
                let counter_name = self.iname(counter);
                let guarded = match count {
                    Count::Fixed(n) => {
                        self.materialize(i64::from(*n), Some(counter));
                        false
                    }
                    Count::Member(place) => {
                        let (base, offset) = self.address(place)?;
                        let base_name = self.iname(base);
                        self.out.inst(
                            "ilw",
                            Some(&place.lane.letter().to_string()),
                            &[counter_name.clone(), format!("{offset}({base_name})")],
                        );
                        true
                    }
                    Count::Param(_) => return self.not_yet("a range counted by xitop"),
                };
                if guarded {
                    self.out.inst(
                        "ibeq",
                        None,
                        &[counter_name.clone(), "vi00".into(), exit.clone()],
                    );
                }
                let mut pointers = Vec::new();
                let program = self.program;
                for &element in elements {
                    let array = &program.elements[element].array;
                    let (base, offset) = self.address(array)?;
                    let pointer = self.out.vi(&program.elements[element].name);
                    let (pointer_name, base_name) = (self.iname(pointer), self.iname(base));
                    self.out.inst(
                        "iaddiu",
                        None,
                        &[pointer_name, base_name, offset.to_string()],
                    );
                    self.elements[element] = Some(pointer);
                    pointers.push((pointer, program.elements[element].stride));
                }
                self.out.place_label(&top);
                self.loop_hints(hints);
                self.loops.push((exit.clone(), next.clone()));
                let result = self.stmts(body);
                self.loops.pop();
                result?;
                self.out.place_label(&next);
                self.out.pos = Some(pos);
                for (pointer, stride) in pointers {
                    let name = self.iname(pointer);
                    self.out
                        .inst("iaddiu", None, &[name.clone(), name, stride.to_string()]);
                }
                self.out.inst(
                    "iaddi",
                    None,
                    &[counter_name.clone(), counter_name.clone(), "-1".into()],
                );
                self.out.inst("ibgtz", None, &[counter_name, top]);
            }
        }
        self.out.place_label(&exit);
        self.clips_reachable_from = self.clips;
        Ok(())
    }

    // ----------------------------------------------
    // Conditions
    // ----------------------------------------------

    /// Branches to `label` when `cond` is `when`.
    fn branch(&mut self, cond: &Expr, when: bool, label: &str) -> Lowered<()> {
        match &cond.kind {
            ExprKind::Const(Const::Bool(value)) => {
                if *value == when {
                    self.out.inst("b", None, &[label.into()]);
                }
                Ok(())
            }
            ExprKind::Not(inner) => self.branch(inner, !when, label),
            ExprKind::And(lhs, rhs) | ExprKind::Or(lhs, rhs) => {
                let and = matches!(cond.kind, ExprKind::And(..));
                if and != when {
                    // (a && b) false or (a || b) true: either one decides.
                    self.branch(lhs, when, label)?;
                    self.branch(rhs, when, label)
                } else {
                    let skip = self.out.label("skip");
                    self.branch(lhs, !when, &skip)?;
                    self.branch(rhs, when, label)?;
                    self.out.place_label(&skip);
                    Ok(())
                }
            }
            ExprKind::Compare { op, lhs, rhs } => {
                let op = if when { *op } else { negated(*op) };
                if lhs.ty == Type::Int16 {
                    self.int_branch(op, lhs, rhs, label)
                } else {
                    self.float_branch(op, lhs, rhs, label)
                }
            }
            ExprKind::Call {
                builtin: Builtin::Any,
                args,
            } => {
                let mask = self.clip_mask(args)?;
                self.out
                    .inst("fcand", None, &["vi01".into(), format!("0x{mask:06x}")]);
                let mnemonic = if when { "ibne" } else { "ibeq" };
                self.out.inst(
                    mnemonic,
                    None,
                    &["vi01".into(), "vi00".into(), label.into()],
                );
                Ok(())
            }
            _ => self.not_yet("a bool value other than a comparison or any()"),
        }
    }

    fn int_branch(&mut self, op: CompareOp, lhs: &Expr, rhs: &Expr, label: &str) -> Lowered<()> {
        let is_zero = |expr: &Expr| matches!(expr.kind, ExprKind::Const(Const::Int(0)));
        if matches!(op, CompareOp::Equal | CompareOp::NotEqual) {
            let (a, b) = (self.vi(lhs)?, self.vi(rhs)?);
            let mnemonic = if op == CompareOp::Equal {
                "ibeq"
            } else {
                "ibne"
            };
            let (a_name, b_name) = (self.iname(a), self.iname(b));
            self.out
                .inst(mnemonic, None, &[a_name, b_name, label.into()]);
            return Ok(());
        }
        // The sign of the difference, or of a value against 0.
        let (value, op) = if is_zero(rhs) {
            (self.vi(lhs)?, op)
        } else if is_zero(lhs) {
            (self.vi(rhs)?, swapped(op))
        } else {
            let (a, b) = (self.vi(lhs)?, self.vi(rhs)?);
            let difference = self.out.vi("difference");
            let names = [self.iname(difference), self.iname(a), self.iname(b)];
            self.out.inst("isub", None, &names);
            (difference, op)
        };
        let mnemonic = match op {
            CompareOp::Less => "ibltz",
            CompareOp::LessEqual => "iblez",
            CompareOp::Greater => "ibgtz",
            _ => "ibgez",
        };
        let name = self.iname(value);
        self.out.inst(mnemonic, None, &[name, label.into()]);
        Ok(())
    }

    fn float_branch(&mut self, op: CompareOp, lhs: &Expr, rhs: &Expr, label: &str) -> Lowered<()> {
        // The constant on the right, where the broadcast operand goes.
        let (lhs, rhs, op) = if matches!(lhs.kind, ExprKind::Const(_)) {
            (rhs, lhs, swapped(op))
        } else {
            (lhs, rhs, op)
        };
        let a = self.vf(lhs)?;
        let b = self.vf(rhs)?;
        let difference = self.out.vf("difference");
        let lane = a.lane().letter().to_string();
        let operands = [self.name(difference), self.name(a.reg), self.field(b)];
        self.out.inst("sub", Some(&lane), &operands);
        // The status flag's Z (bit 0) and S (bit 1) reflect that lane.
        let (mask, set) = match op {
            CompareOp::Less => (2, true),
            CompareOp::GreaterEqual => (2, false),
            CompareOp::LessEqual => (3, true),
            CompareOp::Greater => (3, false),
            CompareOp::Equal => (1, true),
            CompareOp::NotEqual => (1, false),
        };
        let flags = self.out.vi("flags");
        let flags_name = self.iname(flags);
        self.out
            .inst("fsand", None, &[flags_name.clone(), mask.to_string()]);
        let mnemonic = if set { "ibne" } else { "ibeq" };
        self.out
            .inst(mnemonic, None, &[flags_name, "vi00".into(), label.into()]);
        Ok(())
    }

    /// The FCAND mask of clip judgements: each judgement's 6 bits, the
    /// newest lowest.
    fn clip_mask(&mut self, args: &[Expr]) -> Lowered<u32> {
        let mut mask = 0;
        for arg in args {
            let judgement = match &arg.kind {
                ExprKind::Local(id) => match self.locals[*id] {
                    LocalReg::Clip(judgement) => judgement,
                    _ => return self.not_yet("clipflags that are not a clipw's"),
                },
                ExprKind::Call {
                    builtin: Builtin::ClipW,
                    args,
                } => self.clipw(args)?,
                _ => return self.not_yet("clipflags that are not a clipw's"),
            };
            if judgement < self.clips_reachable_from {
                return Err(Box::new(problem(
                    VuslMessage::ClipOutOfReach {
                        reason: "a branch or a loop boundary lies between",
                    },
                    arg.pos,
                )));
            }
            let age = self.clips - 1 - judgement;
            if age >= 4 {
                return Err(Box::new(problem(
                    VuslMessage::ClipOutOfReach {
                        reason: "four newer judgements pushed it out",
                    },
                    arg.pos,
                )));
            }
            mask |= 0x3f << (6 * age);
        }
        Ok(mask)
    }

    fn clipw(&mut self, args: &[Expr]) -> Lowered<usize> {
        let judge = self.vf(&args[0])?;
        let w = self.vf(&args[1])?;
        let operands = [self.name(judge.reg), self.field(w)];
        self.out.inst("clipw", Some("xyz"), &operands);
        self.clips += 1;
        Ok(self.clips - 1)
    }

    // ----------------------------------------------
    // Locals and stores
    // ----------------------------------------------

    fn local_vf(&mut self, id: LocalId) -> Vf {
        if let LocalReg::Vf(reg) = self.locals[id] {
            return reg;
        }
        let reg = self.out.vf(&self.program.locals[id].name);
        self.locals[id] = LocalReg::Vf(reg);
        reg
    }

    fn local_vi(&mut self, id: LocalId) -> Vi {
        if let LocalReg::Vi(reg) = self.locals[id] {
            return reg;
        }
        let reg = self.out.vi(&self.program.locals[id].name);
        self.locals[id] = LocalReg::Vi(reg);
        reg
    }

    fn local_mat(&mut self, id: LocalId, size: u8) -> Vec<Vf> {
        if let LocalReg::Mat(regs) = &self.locals[id] {
            return regs.clone();
        }
        let name = self.program.locals[id].name.clone();
        let regs: Vec<Vf> = (0..size).map(|_| self.out.vf(&name)).collect();
        self.locals[id] = LocalReg::Mat(regs.clone());
        regs
    }

    fn set_local(
        &mut self,
        id: LocalId,
        column: Option<usize>,
        components: Option<&[usize]>,
        value: &Expr,
    ) -> Lowered<()> {
        let program = self.program;
        let local = &program.locals[id];
        match &local.ty {
            Type::ClipFlags => {
                let ExprKind::Call {
                    builtin: Builtin::ClipW,
                    args,
                } = &value.kind
                else {
                    return self.not_yet("clipflags from anything but clipw");
                };
                let judgement = self.clipw(args)?;
                self.locals[id] = LocalReg::Clip(judgement);
                Ok(())
            }
            Type::Bool => self.not_yet("a bool variable"),
            Type::Int16 => {
                let reg = self.local_vi(id);
                let result = self.vi_into(value, Some(reg))?;
                if result != reg {
                    let names = [self.iname(reg), self.iname(result)];
                    self.out.inst(
                        "iaddiu",
                        None,
                        &[names[0].clone(), names[1].clone(), "0".into()],
                    );
                }
                Ok(())
            }
            Type::Mat { size, .. } => {
                let size = *size;
                let regs = self.local_mat(id, size);
                match column {
                    None => {
                        let values = self.mat_into(value, Some(&regs))?;
                        let lanes = Placement::identity(usize::from(size).min(4));
                        let mask = Self::mask(&lanes);
                        for (reg, value) in regs.iter().zip(values) {
                            if *reg != value {
                                let names = [self.name(*reg), self.name(value)];
                                self.out.inst("move", Some(&mask), &names);
                            }
                        }
                        Ok(())
                    }
                    Some(column) => {
                        let reg = regs[column];
                        let current = Placement::identity(usize::from(size));
                        self.write_vf(id, reg, Some(current), None, components, value, false)
                            .map(|_| ())
                    }
                }
            }
            ty => {
                let raw = ty.is_raw();
                let reg = self.local_vf(id);
                let current = self.placements[id];
                let fixed = local.fixed;
                let placement = self.write_vf(id, reg, current, fixed, components, value, raw)?;
                self.placements[id] = Some(placement);
                Ok(())
            }
        }
    }

    /// Writes `value` into the VF register `reg` of local `id`, whole or
    /// some components, and returns where the local's value is after.
    #[allow(clippy::too_many_arguments)]
    fn write_vf(
        &mut self,
        id: LocalId,
        reg: Vf,
        current: Option<Placement>,
        fixed: Option<Placement>,
        components: Option<&[usize]>,
        value: &Expr,
        raw: bool,
    ) -> Lowered<Placement> {
        let lanes = value.ty.vf_lanes().unwrap_or(1);
        match components {
            None => {
                if let Some(target) = fixed {
                    let value = self.vf(value)?;
                    self.write_lanes(value, reg, target, raw)?;
                    return Ok(target);
                }
                // A constructor writes its result a lane at a time, so it
                // must not read the register it writes.
                let unsafe_direct =
                    matches!(value.kind, ExprKind::Construct(_)) && reads_local(value, id);
                let result = if unsafe_direct {
                    self.vf(value)?
                } else {
                    self.vf_into(value, Some(reg))?
                };
                if result.reg != reg {
                    let names = [self.name(reg), self.name(result.reg)];
                    self.out
                        .inst("move", Some(&Self::mask(&result.placement)), &names);
                }
                Ok(result.placement)
            }
            Some(components) => {
                let base = current.or(fixed).unwrap_or_else(|| {
                    Placement::identity(self.program.locals[id].ty.vf_lanes().unwrap_or(lanes))
                });
                let Some(target) = base.select(components) else {
                    return self.not_yet("these components");
                };
                let value = self.vf(value)?;
                self.write_lanes(value, reg, target, raw)?;
                Ok(base)
            }
        }
    }

    /// Puts `value`'s components into `dest` at `lanes`: nothing when they
    /// are there already, a MOVE for the lanes already right, and MR32
    /// rotations or one broadcast instruction per lane for the rest. Raw
    /// bits only rotate.
    fn write_lanes(&mut self, value: Fv, dest: Vf, lanes: Placement, raw: bool) -> Lowered<()> {
        if value.reg == dest && value.placement == lanes {
            return Ok(());
        }
        let rotation = value.placement.rotation_to(&lanes);
        let differing = value
            .placement
            .lanes()
            .iter()
            .zip(lanes.lanes())
            .filter(|(from, to)| from != to)
            .count();
        if let Some(steps) = rotation.filter(|&steps| steps > 0 && (raw || steps < differing)) {
            let mut current = value;
            for step in 1..=steps {
                let target = if step == steps {
                    dest
                } else {
                    self.out.vf("rotate")
                };
                let landed: Vec<Lane> = current
                    .placement
                    .lanes()
                    .iter()
                    .map(|lane| Lane::from_index((lane.index() + 3) % 4).expect("a lane"))
                    .collect();
                let landed = Placement::from_lanes(&landed).expect("distinct lanes");
                let names = [self.name(target), self.name(current.reg)];
                self.out.inst("mr32", Some(&Self::mask(&landed)), &names);
                current = Fv {
                    reg: target,
                    placement: landed,
                };
            }
            return Ok(());
        }
        if raw && differing > 0 {
            return Err(Box::new(problem(
                VuslMessage::RawLaneMove {
                    from: value.placement.letters(),
                    to: lanes.letters(),
                },
                self.out.pos.unwrap_or_default(),
            )));
        }
        let mut same = Vec::new();
        for (&from, &to) in value.placement.lanes().iter().zip(lanes.lanes()) {
            if from == to {
                same.push(to);
            } else {
                self.put(dest, to, Component::Lane(value.reg, from));
            }
        }
        if !same.is_empty() && value.reg != dest {
            let same = Placement::from_lanes(&same).expect("distinct lanes");
            let names = [self.name(dest), self.name(value.reg)];
            self.out.inst("move", Some(&Self::mask(&same)), &names);
        }
        Ok(())
    }

    /// One component into lane `target` of `dest`, with one instruction:
    /// VF00 gives 0 in x, y and z and 1 in w, and a broadcast add of 0 (or
    /// multiply by 1, for w) moves a value between lanes.
    fn put(&mut self, dest: Vf, target: Lane, component: Component) {
        let lane = target.letter().to_string();
        let dest_name = self.name(dest);
        match component {
            Component::Zero if target == Lane::W => {
                self.out
                    .inst("sub", Some("w"), &[dest_name, "vf00".into(), "vf00".into()]);
            }
            Component::Zero => self
                .out
                .inst("move", Some(&lane), &[dest_name, "vf00".into()]),
            Component::One if target == Lane::W => {
                self.out
                    .inst("move", Some("w"), &[dest_name, "vf00".into()]);
            }
            Component::One => {
                self.out.inst(
                    "add",
                    Some(&lane),
                    &[dest_name, "vf00".into(), "vf00[w]".into()],
                );
            }
            Component::Lane(source, from) if from == target => {
                let source = self.name(source);
                self.out.inst("move", Some(&lane), &[dest_name, source]);
            }
            Component::Lane(source, from) => {
                let source = self.out.field(source, from);
                let mnemonic = if target == Lane::W { "mul" } else { "add" };
                self.out
                    .inst(mnemonic, Some(&lane), &[dest_name, "vf00".into(), source]);
            }
        }
    }

    fn store(&mut self, place: &Place, components: Option<&[usize]>, value: &Expr) -> Lowered<()> {
        match &place.ty {
            Type::Int16 => {
                let value = self.vi(value)?;
                let (base, offset) = self.address(place)?;
                let names = [self.iname(value), self.iname(base)];
                self.out.inst(
                    "isw",
                    Some(&place.lane.letter().to_string()),
                    &[names[0].clone(), format!("{offset}({})", names[1])],
                );
                Ok(())
            }
            Type::Mat { size, .. } => {
                let regs = self.mat(value)?;
                let (base, offset) = self.address(place)?;
                let mask = Self::mask(&Placement::identity(usize::from(*size).min(4)));
                for (qword, reg) in regs.iter().enumerate() {
                    let names = [self.name(*reg), self.iname(base)];
                    self.out.inst(
                        "sq",
                        Some(&mask),
                        &[
                            names[0].clone(),
                            format!("{}({})", offset + qword as i64, names[1]),
                        ],
                    );
                }
                Ok(())
            }
            ty => {
                let Some(whole) = memory_lanes(ty, place.lane) else {
                    return self.not_yet("storing a value of this type");
                };
                let target = match components {
                    Some(components) => whole.select(components).expect("checked components"),
                    None => whole,
                };
                let value = self.vf(value)?;
                let value = if value.placement == target {
                    value
                } else {
                    let moved = self.out.vf("store");
                    self.write_lanes(value, moved, target, ty.is_raw())?;
                    Fv {
                        reg: moved,
                        placement: target,
                    }
                };
                let (base, offset) = self.address(place)?;
                let names = [self.name(value.reg), self.iname(base)];
                self.out.inst(
                    "sq",
                    Some(&Self::mask(&target)),
                    &[names[0].clone(), format!("{offset}({})", names[1])],
                );
                Ok(())
            }
        }
    }

    // ----------------------------------------------
    // Addresses
    // ----------------------------------------------

    /// The struct a place's base holds.
    fn base_struct(&self, base: Base) -> Option<usize> {
        let ty = match base {
            Base::Param(index) => &self.program.params[index].ty,
            Base::Element(element) => &self.program.elements[element].ty,
        };
        match ty {
            Type::Struct(id) => Some(*id),
            _ => None,
        }
    }

    /// The place of a member path from a base, for a length read from
    /// memory.
    fn member_place(&self, base: Base, path: &[usize]) -> Option<Place> {
        let structs: &[StructDef] = &self.program.structs;
        let mut id = self.base_struct(base)?;
        let mut place = Place {
            base,
            path: Vec::new(),
            offset: Offset::default(),
            ty: Type::Struct(id),
            lane: Lane::X,
            length: None,
        };
        for &index in path {
            let member = structs.get(id)?.members.get(index)?;
            place.offset.extent.fixed += member.offset.fixed;
            place.path.push(index);
            place.ty = member.ty.clone();
            place.lane = member.lane;
            if let Type::Struct(next) = member.ty {
                id = next;
            }
        }
        Some(place)
    }

    /// A place's base register and immediate offset in qwords.
    fn address(&mut self, place: &Place) -> Lowered<(Vi, i64)> {
        let (mut base, mut offset) = match place.base {
            Base::Param(index) => match self.program.params[index].placement {
                ParamPlacement::Address(address) => (Vi::Zero, i64::from(address)),
                ParamPlacement::Xtop { .. } => (self.xtop.expect("XTOP read in the prologue"), 0),
                ParamPlacement::Xitop => return self.not_yet("the address of an xitop count"),
            },
            Base::Element(element) => match self.elements[element] {
                Some(pointer) => (pointer, 0),
                None => return self.not_yet("an element outside its loop"),
            },
        };
        offset += i64::from(place.offset.extent.fixed);
        for term in &place.offset.extent.terms {
            let Some(count_place) = self.member_place(place.base, &term.count) else {
                return self.not_yet("a length read through this place");
            };
            let (count_base, count_offset) = self.address(&count_place)?;
            let count = self.out.vi("length");
            let names = [self.iname(count), self.iname(count_base)];
            self.out.inst(
                "ilw",
                Some(&count_place.lane.letter().to_string()),
                &[names[0].clone(), format!("{count_offset}({})", names[1])],
            );
            let scaled = self.scale(count, term.stride);
            base = self.add_regs(base, scaled);
        }
        for (index, stride) in &place.offset.indices {
            let index = self.vi(index)?;
            let scaled = self.scale(index, *stride);
            base = self.add_regs(base, scaled);
        }
        Ok((base, offset))
    }

    /// `value * factor`, by doubling and adding: VU1 has no integer
    /// multiply.
    fn scale(&mut self, value: Vi, factor: u32) -> Vi {
        if factor == 1 {
            return value;
        }
        let mut result: Option<Vi> = None;
        let mut power = value;
        let mut remaining = factor;
        while remaining > 0 {
            if remaining & 1 != 0 {
                result = Some(match result {
                    None => power,
                    Some(sum) => self.add_regs(sum, power),
                });
            }
            remaining >>= 1;
            if remaining > 0 {
                power = self.add_regs(power, power);
            }
        }
        result.unwrap_or(Vi::Zero)
    }

    fn add_regs(&mut self, a: Vi, b: Vi) -> Vi {
        if a == Vi::Zero {
            return b;
        }
        if b == Vi::Zero {
            return a;
        }
        let sum = self.out.vi("address");
        let names = [self.iname(sum), self.iname(a), self.iname(b)];
        self.out.inst("iadd", None, &names);
        sum
    }

    // ----------------------------------------------
    // VI expressions
    // ----------------------------------------------

    fn vi(&mut self, expr: &Expr) -> Lowered<Vi> {
        self.vi_into(expr, None)
    }

    /// An `int16` constant in a VI register: IADDIU takes 15 bits, so
    /// 0x8000 takes two instructions.
    fn materialize(&mut self, value: i64, dest: Option<Vi>) -> Vi {
        let bits = (value & 0xffff) as u32;
        if bits == 0 && dest.is_none() {
            return Vi::Zero;
        }
        let reg = self.idest(dest);
        let name = self.iname(reg);
        if bits <= 0x7fff {
            self.out
                .inst("iaddiu", None, &[name, "vi00".into(), bits.to_string()]);
        } else if 0x1_0000 - bits <= 0x7fff {
            self.out.inst(
                "isubiu",
                None,
                &[name, "vi00".into(), (0x1_0000 - bits).to_string()],
            );
        } else {
            self.out.inst(
                "iaddiu",
                None,
                &[name.clone(), "vi00".into(), "0x7fff".into()],
            );
            self.out
                .inst("iaddiu", None, &[name.clone(), name, "1".into()]);
        }
        reg
    }

    /// `value + amount`.
    fn add_immediate(&mut self, value: Vi, amount: i64, dest: Option<Vi>) -> Vi {
        let amount = ((amount + 0x8000) & 0xffff) - 0x8000;
        if amount == 0 && dest.is_none() {
            return value;
        }
        let reg = self.idest(dest);
        let names = [self.iname(reg), self.iname(value)];
        if (0..=0x7fff).contains(&amount) {
            self.out.inst(
                "iaddiu",
                None,
                &[names[0].clone(), names[1].clone(), amount.to_string()],
            );
        } else if (-0x7fff..0).contains(&amount) {
            self.out.inst(
                "isubiu",
                None,
                &[names[0].clone(), names[1].clone(), (-amount).to_string()],
            );
        } else {
            let constant = self.materialize(amount, None);
            let constant = self.iname(constant);
            self.out.inst(
                "iadd",
                None,
                &[names[0].clone(), names[1].clone(), constant],
            );
        }
        reg
    }

    fn vi_into(&mut self, expr: &Expr, dest: Option<Vi>) -> Lowered<Vi> {
        match &expr.kind {
            ExprKind::Const(Const::Int(value)) => Ok(self.materialize(*value, dest)),
            ExprKind::Local(id) => match self.locals[*id] {
                LocalReg::Vi(reg) => Ok(reg),
                _ => self.not_yet("this local as an int16"),
            },
            ExprKind::Load(place) => {
                let (base, offset) = self.address(place)?;
                let reg = self.idest(dest);
                let names = [self.iname(reg), self.iname(base)];
                self.out.inst(
                    "ilw",
                    Some(&place.lane.letter().to_string()),
                    &[names[0].clone(), format!("{offset}({})", names[1])],
                );
                Ok(reg)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let constant = |expr: &Expr| match expr.kind {
                    ExprKind::Const(Const::Int(value)) => Some(value),
                    _ => None,
                };
                match (op, constant(lhs), constant(rhs)) {
                    (BinOp::IntAdd, _, Some(amount)) => {
                        let value = self.vi(lhs)?;
                        Ok(self.add_immediate(value, amount, dest))
                    }
                    (BinOp::IntAdd, Some(amount), _) => {
                        let value = self.vi(rhs)?;
                        Ok(self.add_immediate(value, amount, dest))
                    }
                    (BinOp::IntSub, _, Some(amount)) => {
                        let value = self.vi(lhs)?;
                        Ok(self.add_immediate(value, -amount, dest))
                    }
                    (BinOp::IntAdd | BinOp::IntSub | BinOp::IntAnd | BinOp::IntOr, _, _) => {
                        let a = self.vi(lhs)?;
                        let b = self.vi(rhs)?;
                        let reg = self.idest(dest);
                        let mnemonic = match op {
                            BinOp::IntAdd => "iadd",
                            BinOp::IntSub => "isub",
                            BinOp::IntAnd => "iand",
                            _ => "ior",
                        };
                        let names = [self.iname(reg), self.iname(a), self.iname(b)];
                        self.out.inst(mnemonic, None, &names);
                        Ok(reg)
                    }
                    _ => self.not_yet("this int16 operation"),
                }
            }
            ExprKind::Neg(operand) => {
                let value = self.vi(operand)?;
                let reg = self.idest(dest);
                let names = [self.iname(reg), "vi00".into(), self.iname(value)];
                self.out.inst("isub", None, &names);
                Ok(reg)
            }
            ExprKind::Convert {
                conversion: conversion @ (Conversion::LaneToInt | Conversion::FloatToInt),
                operand,
            } => {
                let mut value = self.vf(operand)?;
                if *conversion == Conversion::FloatToInt {
                    let fixed = self.out.vf("fixed");
                    let lane = value.lane().letter().to_string();
                    let names = [self.name(fixed), self.name(value.reg)];
                    self.out.inst("ftoi0", Some(&lane), &names);
                    value = Fv::scalar(fixed, value.lane());
                }
                let reg = self.idest(dest);
                let names = [self.iname(reg), self.field(value)];
                self.out.inst("mtir", None, &names);
                Ok(reg)
            }
            ExprKind::Call {
                builtin: Builtin::Adc,
                args,
            } => {
                let mask = self.clip_mask(args)?;
                self.out
                    .inst("fcand", None, &["vi01".into(), format!("0x{mask:06x}")]);
                let reg = self.idest(dest);
                let name = self.iname(reg);
                // Bit 15 is set exactly when any flag is.
                self.out
                    .inst("iaddiu", None, &[name, "vi01".into(), "0x7fff".into()]);
                Ok(reg)
            }
            ExprKind::Call {
                builtin: Builtin::Xitop,
                ..
            } => {
                let reg = self.idest(dest);
                let name = self.iname(reg);
                self.out.inst("xitop", None, &[name]);
                Ok(reg)
            }
            _ => self.not_yet("this int16 expression"),
        }
    }

    // ----------------------------------------------
    // VF expressions
    // ----------------------------------------------

    fn vf(&mut self, expr: &Expr) -> Lowered<Fv> {
        self.vf_into(expr, None)
    }

    /// A float constant as an operand: VF00's x for 0 and w for 1, else
    /// LOI into a lane.
    fn float_constant(&mut self, value: f32, dest: Option<Vf>) -> Fv {
        if dest.is_none() {
            if value == 0.0 && value.is_sign_positive() {
                return Fv::scalar(Vf::Zero, Lane::X);
            }
            if value == 1.0 {
                return Fv::scalar(Vf::Zero, Lane::W);
            }
        }
        let reg = self.dest(dest);
        self.out.inst("loi", None, &[float_text(value)]);
        let name = self.name(reg);
        self.out
            .inst("addi", Some("x"), &[name, "vf00".into(), "i".into()]);
        Fv::scalar(reg, Lane::X)
    }

    fn vf_into(&mut self, expr: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        match &expr.kind {
            ExprKind::Const(Const::Float(value)) => Ok(self.float_constant(*value, dest)),
            ExprKind::Const(Const::Int(value)) => Ok(self.float_constant(*value as f32, dest)),
            ExprKind::Local(id) => match self.locals[*id] {
                LocalReg::Vf(reg) => {
                    let placement = self.placements[*id]
                        .or(expr.placement)
                        .unwrap_or_else(|| Placement::identity(expr.ty.vf_lanes().unwrap_or(1)));
                    Ok(Fv { reg, placement })
                }
                _ => self.not_yet("this local as a VF value"),
            },
            ExprKind::Load(place) => {
                let Some(placement) = memory_lanes(&place.ty, place.lane) else {
                    return self.not_yet("loading a value of this type");
                };
                let (base, offset) = self.address(place)?;
                let reg = self.dest(dest);
                let names = [self.name(reg), self.iname(base)];
                self.out.inst(
                    "lq",
                    Some(&Self::mask(&placement)),
                    &[names[0].clone(), format!("{offset}({})", names[1])],
                );
                Ok(Fv { reg, placement })
            }
            ExprKind::Swizzle { base, components } => {
                let base = self.vf(base)?;
                let Some(placement) = base.placement.select(components) else {
                    return self.not_yet("this swizzle");
                };
                Ok(Fv {
                    reg: base.reg,
                    placement,
                })
            }
            ExprKind::Neg(operand) => self.negate(operand, dest),
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::Add => self.elementwise("add", lhs, rhs, dest, true),
                BinOp::Sub => self.elementwise("sub", lhs, rhs, dest, false),
                BinOp::Mul => self.elementwise("mul", lhs, rhs, dest, true),
                BinOp::Min => self.elementwise("mini", lhs, rhs, dest, true),
                BinOp::Max => self.elementwise("max", lhs, rhs, dest, true),
                BinOp::Div => self.divide(lhs, rhs, dest),
                BinOp::MatVec => self.transform(lhs, rhs, dest),
                BinOp::VecMat => self.transform(rhs, lhs, dest),
                _ => self.not_yet("this operation on VF values"),
            },
            ExprKind::Construct(_) => self.construct(expr, dest),
            ExprKind::Convert {
                conversion,
                operand,
            } => self.convert(*conversion, operand, dest),
            ExprKind::Column { matrix, index } => {
                let regs = self.mat(matrix)?;
                let size = regs.len();
                Ok(Fv {
                    reg: regs[*index],
                    placement: Placement::identity(size.min(4)),
                })
            }
            ExprKind::Call { builtin, args } => self.builtin(*builtin, args, dest),
            _ => self.not_yet("this expression as a VF value"),
        }
    }

    fn negate(&mut self, operand: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        let value = self.vf(operand)?;
        let reg = self.dest(dest);
        if value.is_scalar() {
            // 0 - x lands in lane x, whatever x's lane.
            let operands = [self.name(reg), "vf00".into(), self.field(value)];
            self.out.inst("sub", Some("x"), &operands);
            return Ok(Fv::scalar(reg, Lane::X));
        }
        let (xyz, w): (Vec<Lane>, Vec<Lane>) = value
            .placement
            .lanes()
            .iter()
            .partition(|lane| **lane != Lane::W);
        if !xyz.is_empty() {
            let mask = Self::mask(&Placement::from_lanes(&xyz).expect("distinct"));
            let operands = [self.name(reg), "vf00".into(), self.name(value.reg)];
            self.out.inst("sub", Some(&mask), &operands);
        }
        if !w.is_empty() {
            // VF00's w is 1: ACC = 1 * 0, then 0 - x * 1.
            self.out.inst(
                "mula",
                Some("w"),
                &["acc".into(), "vf00".into(), "vf00[x]".into()],
            );
            let operands = [self.name(reg), self.name(value.reg), "vf00[w]".into()];
            self.out.inst("msub", Some("w"), &operands);
        }
        Ok(Fv {
            reg,
            placement: value.placement,
        })
    }

    fn elementwise(
        &mut self,
        mnemonic: &str,
        lhs: &Expr,
        rhs: &Expr,
        dest: Option<Vf>,
        commutative: bool,
    ) -> Lowered<Fv> {
        let a = self.vf(lhs)?;
        let b = self.vf(rhs)?;
        self.elementwise_values(mnemonic, a, b, dest, commutative)
    }

    fn elementwise_values(
        &mut self,
        mnemonic: &str,
        a: Fv,
        b: Fv,
        dest: Option<Vf>,
        commutative: bool,
    ) -> Lowered<Fv> {
        let (a, b) = if a.is_scalar() && !b.is_scalar() {
            if !commutative {
                return self.not_yet("a scalar minus a vector");
            }
            (b, a)
        } else {
            (a, b)
        };
        let reg = self.dest(dest);
        let operand = if b.is_scalar() {
            self.field(b)
        } else {
            self.name(b.reg)
        };
        let operands = [self.name(reg), self.name(a.reg), operand];
        self.out
            .inst(mnemonic, Some(&Self::mask(&a.placement)), &operands);
        Ok(Fv {
            reg,
            placement: a.placement,
        })
    }

    fn divide(&mut self, lhs: &Expr, rhs: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        let divisor = self.vf(rhs)?;
        let dividend = self.vf(lhs)?;
        let reg = self.dest(dest);
        if dividend.is_scalar() {
            let operands = ["q".into(), self.field(dividend), self.field(divisor)];
            self.out.inst("div", None, &operands);
            let operands = [self.name(reg), "vf00".into(), "q".into()];
            self.out.inst("addq", Some("x"), &operands);
            return Ok(Fv::scalar(reg, Lane::X));
        }
        let operands = ["q".into(), "vf00[w]".into(), self.field(divisor)];
        self.out.inst("div", None, &operands);
        let operands = [self.name(reg), self.name(dividend.reg), "q".into()];
        self.out
            .inst("mulq", Some(&Self::mask(&dividend.placement)), &operands);
        Ok(Fv {
            reg,
            placement: dividend.placement,
        })
    }

    /// A vector's components for a matrix product or a constructor: a
    /// constructor's parts as they are, constants 0 and 1 as VF00's.
    fn components(&mut self, expr: &Expr) -> Lowered<Vec<Component>> {
        let constant = |expr: &Expr| match expr.kind {
            ExprKind::Const(Const::Float(value)) => Some(value),
            ExprKind::Const(Const::Int(value)) => Some(value as f32),
            _ => None,
        };
        if let ExprKind::Construct(args) = &expr.kind {
            let count = expr.ty.vf_lanes().unwrap_or(1);
            let mut components = Vec::new();
            for arg in args {
                match constant(arg) {
                    Some(0.0) => components.push(Component::Zero),
                    Some(1.0) => components.push(Component::One),
                    _ => {
                        let value = self.vf(arg)?;
                        for &lane in value.placement.lanes() {
                            components.push(Component::Lane(value.reg, lane));
                        }
                    }
                }
            }
            // One scalar fills every component.
            if args.len() == 1 && components.len() == 1 {
                components = vec![components[0]; count];
            }
            return Ok(components);
        }
        let value = self.vf(expr)?;
        Ok(value
            .placement
            .lanes()
            .iter()
            .map(|&lane| Component::Lane(value.reg, lane))
            .collect())
    }

    /// `matrix * vector` (columns stored) or `vector * matrix` (rows): the
    /// sum of each stored qword times its component.
    fn transform(&mut self, matrix: &Expr, vector: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        let regs = self.mat(matrix)?;
        let components = self.components(vector)?;
        let size = regs.len();
        let lanes = Placement::identity(size.min(4));
        let mask = Self::mask(&lanes);
        let reg = self.dest(dest);
        let terms: Vec<(Vf, String)> = regs
            .iter()
            .zip(&components)
            .filter_map(|(&qword, component)| match component {
                Component::Zero => None,
                Component::One => Some((qword, "vf00[w]".to_string())),
                Component::Lane(source, lane) => Some((qword, self.out.field(*source, *lane))),
            })
            .collect();
        let dest_name = self.name(reg);
        match terms.len() {
            0 => self.out.inst(
                "sub",
                Some(&mask),
                &[dest_name, "vf00".into(), "vf00".into()],
            ),
            1 => {
                let (qword, operand) = &terms[0];
                let operands = [dest_name, self.name(*qword), operand.clone()];
                self.out.inst("mul", Some(&mask), &operands);
            }
            count => {
                for (index, (qword, operand)) in terms.iter().enumerate() {
                    let (mnemonic, target) = match index {
                        0 => ("mul", "acc".to_string()),
                        index if index + 1 == count => ("madd", dest_name.clone()),
                        _ => ("madd", "acc".to_string()),
                    };
                    let operands = [target, self.name(*qword), operand.clone()];
                    self.out.inst(mnemonic, Some(&mask), &operands);
                }
            }
        }
        Ok(Fv {
            reg,
            placement: lanes,
        })
    }

    fn construct(&mut self, expr: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        let components = self.components(expr)?;
        let reg = self.dest(dest);
        let count = components.len();
        // Components already in their own lanes of one register move at
        // once.
        let mut moved: Vec<(Vf, Vec<Lane>)> = Vec::new();
        for (index, component) in components.into_iter().enumerate() {
            let target = Lane::from_index(index).expect("four components at most");
            match component {
                Component::Lane(source, lane) if lane == target && source != reg => {
                    match moved.iter_mut().find(|(register, _)| *register == source) {
                        Some((_, lanes)) => lanes.push(target),
                        None => moved.push((source, vec![target])),
                    }
                }
                Component::Lane(source, lane) if lane == target && source == reg => {}
                component => self.put(reg, target, component),
            }
        }
        for (source, lanes) in moved {
            let mask = Self::mask(&Placement::from_lanes(&lanes).expect("distinct"));
            let names = [self.name(reg), self.name(source)];
            self.out.inst("move", Some(&mask), &names);
        }
        Ok(Fv {
            reg,
            placement: Placement::identity(count),
        })
    }

    fn convert(&mut self, conversion: Conversion, operand: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        match conversion {
            Conversion::FloatToFixed(bits) | Conversion::FixedToFloat(bits) => {
                let value = self.vf(operand)?;
                let reg = self.dest(dest);
                let mnemonic = match conversion {
                    Conversion::FloatToFixed(_) => format!("ftoi{bits}"),
                    _ => format!("itof{bits}"),
                };
                let names = [self.name(reg), self.name(value.reg)];
                self.out
                    .inst(&mnemonic, Some(&Self::mask(&value.placement)), &names);
                Ok(Fv {
                    reg,
                    placement: value.placement,
                })
            }
            Conversion::IntToLane | Conversion::IntToFloat => {
                let value = self.vi(operand)?;
                let lane_reg = if conversion == Conversion::IntToLane {
                    self.dest(dest)
                } else {
                    self.out.vf("lane")
                };
                let names = [self.name(lane_reg), self.iname(value)];
                self.out.inst("mfir", Some("x"), &names);
                if conversion == Conversion::IntToLane {
                    return Ok(Fv::scalar(lane_reg, Lane::X));
                }
                let reg = self.dest(dest);
                let names = [self.name(reg), self.name(lane_reg)];
                self.out.inst("itof0", Some("x"), &names);
                Ok(Fv::scalar(reg, Lane::X))
            }
            Conversion::Bits => self.vf_into(operand, dest),
            Conversion::LaneToInt | Conversion::FloatToInt => {
                self.not_yet("an int16 as a VF value")
            }
        }
    }

    /// The sum of a vector's components, into the first one's lane.
    fn sum_components(&mut self, value: Fv, reg: Vf) -> Fv {
        let first = value.lane();
        let lane = first.letter().to_string();
        let lanes: Vec<Lane> = value.placement.lanes().to_vec();
        for (index, &next) in lanes.iter().enumerate().skip(1) {
            let source = if index == 1 { value.reg } else { reg };
            let operands = [
                self.name(reg),
                self.name(source),
                self.out.field(value.reg, next),
            ];
            self.out.inst("add", Some(&lane), &operands);
        }
        Fv::scalar(reg, first)
    }

    fn dot(&mut self, a: &Expr, b: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        let a = self.vf(a)?;
        let b = self.vf(b)?;
        let products = self.out.vf("products");
        let operands = [self.name(products), self.name(a.reg), self.name(b.reg)];
        self.out
            .inst("mul", Some(&Self::mask(&a.placement)), &operands);
        let reg = self.dest(dest);
        Ok(self.sum_components(
            Fv {
                reg: products,
                placement: a.placement,
            },
            reg,
        ))
    }

    /// Q into a VF register's x: ADDQ of VF00's 0.
    fn q_to_lane(&mut self, dest: Option<Vf>) -> Fv {
        let reg = self.dest(dest);
        let name = self.name(reg);
        self.out
            .inst("addq", Some("x"), &[name, "vf00".into(), "q".into()]);
        Fv::scalar(reg, Lane::X)
    }

    fn builtin(&mut self, builtin: Builtin, args: &[Expr], dest: Option<Vf>) -> Lowered<Fv> {
        match builtin {
            Builtin::Dot => self.dot(&args[0], &args[1], dest),
            Builtin::Cross => {
                let a = self.vf(&args[0])?;
                let b = self.vf(&args[1])?;
                let reg = self.dest(dest);
                let operands = ["acc".into(), self.name(a.reg), self.name(b.reg)];
                self.out.inst("opmula", Some("xyz"), &operands);
                let operands = [self.name(reg), self.name(b.reg), self.name(a.reg)];
                self.out.inst("opmsub", Some("xyz"), &operands);
                Ok(Fv {
                    reg,
                    placement: Placement::identity(3),
                })
            }
            Builtin::Normalize => {
                let value = self.vf(&args[0])?;
                let length = self.dot(&args[0], &args[0], None)?;
                let operands = ["q".into(), "vf00[w]".into(), self.field(length)];
                self.out.inst("rsqrt", None, &operands);
                let reg = self.dest(dest);
                let operands = [self.name(reg), self.name(value.reg), "q".into()];
                self.out
                    .inst("mulq", Some(&Self::mask(&value.placement)), &operands);
                Ok(Fv {
                    reg,
                    placement: value.placement,
                })
            }
            Builtin::Length => {
                let squared = self.dot(&args[0], &args[0], None)?;
                let operands = ["q".into(), self.field(squared)];
                self.out.inst("sqrt", None, &operands);
                Ok(self.q_to_lane(dest))
            }
            Builtin::Sqrt => {
                let value = self.vf(&args[0])?;
                let operands = ["q".into(), self.field(value)];
                self.out.inst("sqrt", None, &operands);
                Ok(self.q_to_lane(dest))
            }
            Builtin::Rsqrt => {
                let value = self.vf(&args[0])?;
                let operands = ["q".into(), "vf00[w]".into(), self.field(value)];
                self.out.inst("rsqrt", None, &operands);
                Ok(self.q_to_lane(dest))
            }
            Builtin::Abs => {
                let value = self.vf(&args[0])?;
                let reg = self.dest(dest);
                let names = [self.name(reg), self.name(value.reg)];
                self.out
                    .inst("abs", Some(&Self::mask(&value.placement)), &names);
                Ok(Fv {
                    reg,
                    placement: value.placement,
                })
            }
            Builtin::Clamp => {
                let value = self.vf(&args[0])?;
                let low = self.vf(&args[1])?;
                let high = self.vf(&args[2])?;
                let raised = self.elementwise_values("max", value, low, None, true)?;
                self.elementwise_values("mini", raised, high, dest, true)
            }
            Builtin::Esadd | Builtin::Ersadd | Builtin::Eleng | Builtin::Erleng | Builtin::Esum => {
                let value = self.vf(&args[0])?;
                let mnemonic = match builtin {
                    Builtin::Esadd => "esadd",
                    Builtin::Ersadd => "ersadd",
                    Builtin::Eleng => "eleng",
                    Builtin::Erleng => "erleng",
                    _ => "esum",
                };
                let name = self.name(value.reg);
                self.out.inst(mnemonic, None, &["p".into(), name]);
                Ok(self.p_to_lane(dest))
            }
            Builtin::Ercpr
            | Builtin::Esqrt
            | Builtin::Ersqrt
            | Builtin::Esin
            | Builtin::Eatan
            | Builtin::Eexp => {
                let value = self.vf(&args[0])?;
                let mnemonic = match builtin {
                    Builtin::Ercpr => "ercpr",
                    Builtin::Esqrt => "esqrt",
                    Builtin::Ersqrt => "ersqrt",
                    Builtin::Esin => "esin",
                    Builtin::Eatan => "eatan",
                    _ => "eexp",
                };
                let operand = self.field(value);
                self.out.inst(mnemonic, None, &["p".into(), operand]);
                Ok(self.p_to_lane(dest))
            }
            _ => self.not_yet("this built-in as a VF value"),
        }
    }

    fn p_to_lane(&mut self, dest: Option<Vf>) -> Fv {
        let reg = self.dest(dest);
        let name = self.name(reg);
        self.out.inst("mfp", Some("x"), &[name, "p".into()]);
        Fv::scalar(reg, Lane::X)
    }

    // ----------------------------------------------
    // Matrices
    // ----------------------------------------------

    fn mat(&mut self, expr: &Expr) -> Lowered<Vec<Vf>> {
        self.mat_into(expr, None)
    }

    /// A matrix's qwords in registers: `dest`'s for a load, when given.
    fn mat_into(&mut self, expr: &Expr, dest: Option<&[Vf]>) -> Lowered<Vec<Vf>> {
        let Type::Mat { size, row_major } = expr.ty else {
            return self.not_yet("a matrix value of this type");
        };
        let lanes = Placement::identity(usize::from(size).min(4));
        let mask = Self::mask(&lanes);
        match &expr.kind {
            ExprKind::Local(id) => match &self.locals[*id] {
                LocalReg::Mat(regs) => Ok(regs.clone()),
                _ => self.not_yet("this local as a matrix"),
            },
            ExprKind::Load(place) => {
                let (base, offset) = self.address(place)?;
                let mut regs = Vec::new();
                for qword in 0..i64::from(size) {
                    let reg = match dest {
                        Some(dest) => dest[qword as usize],
                        None => self.out.vf("matrix"),
                    };
                    let names = [self.name(reg), self.iname(base)];
                    self.out.inst(
                        "lq",
                        Some(&mask),
                        &[
                            names[0].clone(),
                            format!("{}({})", offset + qword, names[1]),
                        ],
                    );
                    regs.push(reg);
                }
                Ok(regs)
            }
            ExprKind::Binary {
                op: BinOp::ScaleMat,
                lhs,
                rhs,
            } => {
                let (scalar, matrix) = if lhs.ty == Type::Float {
                    (lhs, rhs)
                } else {
                    (rhs, lhs)
                };
                let scalar = self.vf(scalar)?;
                let matrix = self.mat(matrix)?;
                let mut regs = Vec::new();
                for qword in matrix {
                    let reg = self.out.vf("scaled");
                    let operands = [self.name(reg), self.name(qword), self.field(scalar)];
                    self.out.inst("mul", Some(&mask), &operands);
                    regs.push(reg);
                }
                Ok(regs)
            }
            ExprKind::Binary {
                op: BinOp::MatMat,
                lhs,
                rhs,
            } => {
                let a = self.mat(lhs)?;
                let b = self.mat(rhs)?;
                // Columns: (a * b)'s column i is a times b's column i. Rows:
                // (a * b)'s row i is a's row i times b.
                let (sums, weights) = if row_major { (b, a) } else { (a, b) };
                let mut regs = Vec::new();
                for &weight in &weights {
                    let reg = self.out.vf("product");
                    let count = sums.len();
                    for (index, &qword) in sums.iter().enumerate() {
                        let lane = Lane::from_index(index).expect("four qwords at most");
                        let (mnemonic, target) = match index {
                            0 => ("mul", "acc".to_string()),
                            index if index + 1 == count => ("madd", self.name(reg)),
                            _ => ("madd", "acc".to_string()),
                        };
                        let operands = [target, self.name(qword), self.out.field(weight, lane)];
                        self.out.inst(mnemonic, Some(&mask), &operands);
                    }
                    regs.push(reg);
                }
                Ok(regs)
            }
            _ => self.not_yet("this matrix expression"),
        }
    }
}

fn negated(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Less => CompareOp::GreaterEqual,
        CompareOp::LessEqual => CompareOp::Greater,
        CompareOp::Greater => CompareOp::LessEqual,
        CompareOp::GreaterEqual => CompareOp::Less,
        CompareOp::Equal => CompareOp::NotEqual,
        CompareOp::NotEqual => CompareOp::Equal,
    }
}

/// The comparison with its operands swapped: `a < b` is `b > a`.
fn swapped(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Less => CompareOp::Greater,
        CompareOp::LessEqual => CompareOp::GreaterEqual,
        CompareOp::Greater => CompareOp::Less,
        CompareOp::GreaterEqual => CompareOp::LessEqual,
        other => other,
    }
}
