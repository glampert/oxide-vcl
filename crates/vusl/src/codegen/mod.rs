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
//! - A range-for steps a pointer per element stride by that stride, so the
//!   body addresses elements with immediates, and ends when the pointer
//!   with the smallest stride reaches the end of its array.
//! - What registers hold is recorded (`values`), so a value is computed
//!   once while it holds and a loop's invariants once before it. A
//!   division's readers take Q directly, sums of products are ACC chains,
//!   and stores to the lanes of one qword can be one store.
//! - `peephole` then writes a temporary's value straight into the register
//!   it was moved to.
//! - Matrix products are MULA/MADDA/MADD chains on the vector's components
//!   as broadcast operands; a component that is the constant 1 is VF00's w
//!   and a 0 drops its term.
//! - Comparisons of floats subtract and test the status flags (FSAND);
//!   comparisons of `int16` values branch on them or on their difference.

mod emit;
mod peephole;
mod values;

pub use emit::{Vf, Vi};

use emit::{Output, mask_of};
use values::{Cache, Effects, Entry, Key, Read, Value};

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
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// Choices lowering leaves to its caller.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// The most values a loop's invariants may keep in registers across
    /// it.
    pub hoist_limit: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { hoist_limit: 16 }
    }
}

/// Lowers `program` to VCL source; `source` is the file it came from.
pub fn generate(program: &Program, source: &str, settings: Settings) -> Lowered<Generated> {
    let mut lowering = Lowering {
        settings,
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
        cache: Cache::default(),
        q_holds: None,
    };
    lowering.program_text(source)?;
    let (text, origins) = lowering.out.finish();
    Ok(Generated { text, origins })
}

struct Lowering<'p> {
    program: &'p Program,
    settings: Settings,
    out: Output,
    locals: Vec<LocalReg>,
    /// Where each local's value is now, as the code goes.
    placements: Vec<Option<Placement>>,
    /// Each range element's pointer and its offset from it, inside its
    /// loop.
    elements: Vec<Option<(Vi, i64)>>,
    xtop: Option<Vi>,
    /// The `break` and `continue` labels of the loops being lowered.
    loops: Vec<(String, String)>,
    exit: String,
    /// The CLIPW judgements emitted so far, and the first one still
    /// readable: a branch or a loop boundary loses track of the flags.
    clips: usize,
    clips_reachable_from: usize,
    /// The values registers hold that the code can use again.
    cache: Cache,
    /// The register lane whose value Q holds too, while no FDIV operation
    /// or label has come since: its readers take Q directly.
    q_holds: Option<(Vf, Lane)>,
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
        ExprKind::Load(place) | ExprKind::Address(place) => {
            place.base == Base::Ref(id)
                || place
                    .offset
                    .indices
                    .iter()
                    .any(|(index, _)| reads_local(index, id))
        }
        ExprKind::RefTo { address, .. } => reads_local(address, id),
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

    /// A label here. Q may differ on the paths that meet at it.
    fn label_here(&mut self, label: &str) {
        self.q_holds = None;
        self.out.place_label(label);
    }

    /// An FDIV unit operation: Q changes.
    fn fdiv(&mut self, mnemonic: &str, operands: &[String]) {
        self.q_holds = None;
        self.out.inst(mnemonic, None, operands);
    }

    /// Whether `value` is the one Q holds too.
    fn is_q(&self, value: Fv) -> bool {
        value.is_scalar() && self.q_holds == Some((value.reg, value.lane()))
    }

    /// Local `id`'s registers, VF and VI.
    fn local_regs(&self, id: LocalId) -> (Vec<Vf>, Vec<Vi>) {
        match &self.locals[id] {
            LocalReg::Vf(reg) => (vec![*reg], Vec::new()),
            LocalReg::Mat(regs) => (regs.clone(), Vec::new()),
            LocalReg::Vi(reg) => (Vec::new(), vec![*reg]),
            LocalReg::Unset | LocalReg::Clip(_) => (Vec::new(), Vec::new()),
        }
    }

    /// VF register `reg` written: values it held from before cache entry
    /// `since` are gone, and so is Q's copy unless it came since.
    fn clobber_vf(&mut self, reg: Vf, since: u64, q_before: Option<(Vf, Lane)>) {
        self.cache.clobber_vf(reg, since);
        if self.q_holds.is_some_and(|(q_reg, _)| q_reg == reg) && self.q_holds == q_before {
            self.q_holds = None;
        }
    }

    /// After a `yield`: the host may have written memory and sent another
    /// batch, so what came from memory, XTOP and XITOP is gone, and so is Q
    /// (another program may have run).
    fn resumed(&mut self) {
        self.cache.forget_memory();
        self.cache.forget_key(&Key::Expr(XITOP_KEY.into()));
        if let Some(xtop) = self.xtop {
            self.cache.clobber_vi(xtop, u64::MAX);
        }
        self.q_holds = None;
    }

    /// XITOP's count: the register that holds it, or XITOP.
    fn xitop_count(&mut self) -> Vi {
        let key = Key::Expr(XITOP_KEY.into());
        if let Some(Value::Vi(reg)) = self.cache.find(&key) {
            return reg;
        }
        let reg = self.out.vi("xitop");
        let name = self.iname(reg);
        self.out.inst("xitop", None, &[name]);
        self.cache.insert(Entry {
            key,
            value: Value::Vi(reg),
            reads: Vec::new(),
            locals: Vec::new(),
            elements: Vec::new(),
        });
        reg
    }

    /// Cache entries that `effects` may change are gone.
    fn forget(&mut self, effects: &Effects) {
        if effects.yields {
            self.resumed();
        }
        for region in &effects.stores {
            self.cache.stored(self.program, region, None);
        }
        for &id in &effects.locals {
            let (vf, vi) = self.local_regs(id);
            for reg in vf {
                self.clobber_vf(reg, u64::MAX, self.q_holds);
            }
            for reg in vi {
                self.cache.clobber_vi(reg, u64::MAX);
            }
            self.cache.clobber_local(id);
        }
    }

    /// A store of `qwords` qwords to `place` at `base` and `offset`.
    fn stored(&mut self, place: &Place, base: Vi, offset: i64, qwords: i64) {
        let region = values::region(self.program, place);
        self.cache
            .stored(self.program, &region, Some((base, offset, qwords)));
    }

    /// Lowers the invariants of a loop with `effects` before it, so the
    /// loop finds them in registers.
    fn hoist(&mut self, stmts: &[&[Stmt]], conds: &[&Expr], effects: &Effects) -> Lowered<()> {
        let program = self.program;
        let mut found = Vec::new();
        {
            let elements = &self.elements;
            let bound = |element: ElementId| elements[element].is_some();
            for stmts in stmts {
                values::invariants(program, stmts, &[], effects, &bound, &mut found);
            }
            values::invariants(program, &[], conds, effects, &bound, &mut found);
        }
        let mut kept = 0;
        for expr in found {
            if kept >= self.settings.hoist_limit {
                break;
            }
            let before = self.out.instructions();
            self.out.pos = Some(expr.pos);
            match expr.ty {
                Type::Int16 | Type::Ref(_) => {
                    self.vi(expr)?;
                }
                Type::Mat { size, .. } => {
                    self.mat(expr)?;
                    kept += usize::from(size) - 1;
                }
                _ => {
                    self.vf(expr)?;
                }
            }
            if self.out.instructions() > before {
                kept += 1;
            }
        }
        Ok(())
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
        ] {
            self.out.line(line);
        }
        // A layout(spill) parameter's qwords take the values that do not
        // fit in registers.
        for param in &program.params {
            if let (true, ParamPlacement::Address(address), Type::Struct(id)) =
                (param.spill, param.placement, &param.ty)
            {
                let qwords = program.structs[*id].size.fixed;
                self.out
                    .line(format!(".vu_work_area vu1, {address}, {qwords}"));
            }
        }
        for line in [".vu", "--enter", "--endenter"] {
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
        self.label_here(&exit);
        self.out.line("--exit");
        self.out.line("--endexit");
        Ok(())
    }

    fn stmts(&mut self, stmts: &[Stmt]) -> Lowered<()> {
        let mut index = 0;
        while index < stmts.len() {
            let parts = self.qword_run(&stmts[index..]);
            if parts > 1 {
                self.assemble_qword(&stmts[index..index + parts])?;
                index += parts;
            } else {
                self.stmt(&stmts[index])?;
                index += 1;
            }
        }
        Ok(())
    }

    /// How many stores from the first of `stmts` go to lanes of one qword
    /// and are worth one store: VF values in distinct lanes, none reading
    /// the qword, needing fewer extra lane moves in one register than the
    /// stores they save.
    fn qword_run(&self, stmts: &[Stmt]) -> usize {
        let mut lanes = 0u8;
        let mut extra = 0;
        let mut count = 0;
        let mut first: Option<&Place> = None;
        for stmt in stmts {
            let Stmt::Store {
                place,
                components,
                value,
                ..
            } = stmt
            else {
                break;
            };
            let Some(target) = store_lanes(place, components.as_deref()) else {
                break;
            };
            if let Some(first) = first
                && !same_qword(first, place)
            {
                break;
            }
            let mask = target
                .lanes()
                .iter()
                .fold(0, |mask, lane| mask | 1 << lane.index());
            if lanes & mask != 0 || self.reads_qword(value, place) {
                break;
            }
            // A part that a store of its own would take straight from its
            // register costs a move here.
            let in_place = (target.len() == 1 && any_lane(value))
                || (target.len() > 1 && value.placement == Some(target) && !passes_through(value));
            let direct = self.predicted(value) == Some(target);
            if !in_place && direct {
                extra += 1;
            }
            first = Some(first.unwrap_or(place));
            lanes |= mask;
            count += 1;
        }
        if count > 1 && extra < count - 1 {
            count
        } else {
            1
        }
    }

    /// Where `value`'s lanes are before lowering, when known.
    fn predicted(&self, value: &Expr) -> Option<Placement> {
        match &value.kind {
            ExprKind::Local(id) => self.placements[*id].or(value.placement),
            _ => value.placement,
        }
    }

    /// Whether `value` loads from memory that may be `place`'s qword.
    fn reads_qword(&self, value: &Expr, place: &Place) -> bool {
        let mut locals = Vec::new();
        let mut places = Vec::new();
        values::inputs(value, &mut locals, &mut places);
        let effects = Effects {
            stores: vec![values::region(self.program, place)],
            ..Effects::default()
        };
        places
            .iter()
            .any(|read| effects.stores_to(self.program, read))
    }

    /// Stores to lanes of one qword, as one: each part built in one
    /// register, then one SQ of their lanes.
    fn assemble_qword(&mut self, stores: &[Stmt]) -> Lowered<()> {
        let qword = self.out.vf("qword");
        let mut lanes: Vec<Lane> = Vec::new();
        let mut first = None;
        for stmt in stores {
            let Stmt::Store {
                place,
                components,
                value,
                pos,
            } = stmt
            else {
                unreachable!("a run of stores");
            };
            self.out.pos = Some(*pos);
            first.get_or_insert((place, *pos));
            let target = store_lanes(place, components.as_deref()).expect("a store to lanes");
            let into = if target.len() == 1 {
                self.vf_in_lane(value, Some(qword), target.lane(0))?
            } else if value.placement == Some(target) && !passes_through(value) {
                self.vf_into(value, Some(qword))?
            } else {
                self.vf(value)?
            };
            if into.reg == qword && into.placement != target {
                return self.not_yet("a part of a qword computed in other lanes");
            }
            self.write_lanes(into, qword, target, place.ty.is_raw())?;
            lanes.extend(target.lanes());
        }
        let (place, pos) = first.expect("stores");
        self.out.pos = Some(pos);
        let (base, offset) = self.address(place)?;
        let mask = Self::mask(&Placement::from_lanes(&lanes).expect("distinct lanes"));
        let names = [self.name(qword), self.iname(base)];
        self.out.inst(
            "sq",
            Some(&mask),
            &[names[0].clone(), format!("{offset}({})", names[1])],
        );
        self.stored(place, base, offset, 1);
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
                let since = self.cache.mark();
                let q_before = self.q_holds;
                self.set_local(*local, *column, components.as_deref(), value)?;
                // What the local's registers held before is gone; what the
                // value left in them holds.
                let (vf, vi) = self.local_regs(*local);
                let vf = match column {
                    Some(column) => vec![vf[*column]],
                    None => vf,
                };
                for reg in vf {
                    self.clobber_vf(reg, since, q_before);
                }
                for reg in vi {
                    self.cache.clobber_vi(reg, since);
                }
                self.cache.clobber_local(*local);
                Ok(())
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
                    let value = self.load_qword(from, from_base, from_offset + qword, None);
                    let (value_name, to_name) = (self.name(value), self.iname(to_base));
                    self.out.inst(
                        "sq",
                        None,
                        &[value_name, format!("{}({to_name})", to_offset + qword)],
                    );
                    self.stored(to, to_base, to_offset + qword, 1);
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
                // Values from the condition hold on both paths and after,
                // unless `&&` or `||` skips part of it on some path.
                let mut before = self.cache.clone();
                self.branch(cond, false, &otherwise_label)?;
                if !short_circuits(cond) {
                    before = self.cache.clone();
                }
                self.clips_reachable_from = self.clips;
                self.stmts(then)?;
                if !otherwise.is_empty() {
                    self.out.pos = Some(*pos);
                    self.out.inst("b", None, std::slice::from_ref(&end));
                }
                self.cache = before.clone();
                self.label_here(&otherwise_label);
                self.clips_reachable_from = self.clips;
                self.stmts(otherwise)?;
                self.label_here(&end);
                self.clips_reachable_from = self.clips;
                self.cache = before;
                let mut effects = Effects::of(self.program, then);
                effects.add(self.program, otherwise);
                self.forget(&effects);
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
                self.resumed();
                // MSCNT resumes with the batch the host sent last.
                if let Some(xtop) = self.xtop {
                    let name = self.iname(xtop);
                    self.out.inst("xtop", None, &[name]);
                }
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
        let program = self.program;
        let top = self.out.label("loop");
        let next = self.out.label("next");
        let exit = self.out.label("done");
        self.clips_reachable_from = self.clips;
        let mut effects = Effects::of(program, body);
        if let LoopKind::Condition {
            cond_stmts, step, ..
        } = kind
        {
            effects.add(program, cond_stmts);
            effects.add(program, step);
        }
        // What the loop changes is not known at its top; its invariants
        // are computed once, before it.
        self.forget(&effects);
        match kind {
            LoopKind::Condition {
                cond_stmts,
                cond,
                step,
                ..
            } => self.hoist(&[cond_stmts, body, step], &[cond], &effects)?,
            LoopKind::Range { .. } => self.hoist(&[body], &[], &effects)?,
        }
        self.out.pos = Some(pos);
        // The values every path to the exit has.
        let at_exit;
        match kind {
            LoopKind::Condition {
                cond_stmts,
                cond,
                test_first,
                step,
            } => {
                at_exit = self.cache.clone();
                if *test_first {
                    self.stmts(cond_stmts)?;
                    self.out.pos = Some(pos);
                    self.branch(cond, false, &exit)?;
                }
                self.forget(&effects);
                let at_top = self.cache.clone();
                self.label_here(&top);
                self.loop_hints(hints);
                self.loops.push((exit.clone(), next.clone()));
                let result = self.stmts(body);
                self.loops.pop();
                result?;
                self.reach_next(body, &next, &at_top);
                self.stmts(step)?;
                self.stmts(cond_stmts)?;
                self.out.pos = Some(pos);
                self.branch(cond, true, &top)?;
            }
            LoopKind::Range { elements, count } => {
                let trips = match count {
                    // No iterations: no loop.
                    Count::Fixed(0) => return Ok(()),
                    Count::Fixed(n) => Trips::Fixed(i64::from(*n)),
                    Count::Member(place) => {
                        let (base, offset) = self.address(place)?;
                        Trips::Counted(self.load_int16(place, base, offset, None))
                    }
                    Count::Xitop => Trips::Counted(self.xitop_count()),
                };
                // A pointer per element stride and base, stepping by the
                // stride; elements whose arrays differ by a constant share
                // one. The loop ends when the pointer with the smallest
                // stride reaches its end, so no counter steps too.
                let mut pointers: Vec<(Vi, u32)> = Vec::new();
                let mut starts: Vec<(Vi, i64, Vi, u32)> = Vec::new();
                for &element in elements {
                    let array = &program.elements[element].array;
                    let stride = program.elements[element].stride;
                    let (base, offset) = self.address(array)?;
                    let shared = starts.iter().find(|(start_base, _, _, start_stride)| {
                        *start_base == base && *start_stride == stride
                    });
                    if let Some(&(_, start, pointer, _)) = shared {
                        self.elements[element] = Some((pointer, offset - start));
                        continue;
                    }
                    let pointer = self.out.vi(&program.elements[element].name);
                    let (pointer_name, base_name) = (self.iname(pointer), self.iname(base));
                    self.out.inst(
                        "iaddiu",
                        None,
                        &[pointer_name, base_name, offset.to_string()],
                    );
                    self.elements[element] = Some((pointer, 0));
                    starts.push((base, offset, pointer, stride));
                    pointers.push((pointer, stride));
                }
                let &(lead, stride) = pointers
                    .iter()
                    .min_by_key(|(_, stride)| *stride)
                    .expect("a range has an element");
                let offsets: Vec<(ElementId, Vi, i64)> = elements
                    .iter()
                    .map(|&element| {
                        let (pointer, delta) = self.elements[element].expect("a pointer");
                        (element, pointer, delta)
                    })
                    .collect();
                let factor = match (hints.unroll, trips) {
                    (Some(Unroll::By(n)), _) => i64::from(n),
                    (Some(Unroll::Full), Trips::Fixed(n)) => n,
                    _ => 1,
                };
                let range = RangeLoop {
                    body,
                    hints,
                    pos,
                    pointers: &pointers,
                    offsets: &offsets,
                    lead,
                    stride,
                };
                at_exit = if factor > 1 {
                    self.unrolled_range(&range, trips, factor, &effects, &exit)?
                } else {
                    let end = self.out.vi("end");
                    match trips {
                        Trips::Fixed(n) => {
                            self.add_immediate(lead, n * i64::from(stride), Some(end));
                        }
                        Trips::Counted(count) => {
                            let scaled = self.scale(count, stride);
                            let names = [self.iname(end), self.iname(lead), self.iname(scaled)];
                            self.out.inst("iadd", None, &names);
                            let count_name = self.iname(count);
                            self.out
                                .inst("ibeq", None, &[count_name, "vi00".into(), exit.clone()]);
                        }
                    }
                    self.forget(&effects);
                    let at_exit = self.cache.clone();
                    self.range_loop(&range, 1, end, &top, &next, &exit, true)?;
                    at_exit
                };
                // The elements exist only inside their loop.
                for &element in elements {
                    self.elements[element] = None;
                }
            }
        }
        self.label_here(&exit);
        self.cache = at_exit;
        self.clips_reachable_from = self.clips;
        Ok(())
    }

    /// A range loop's body `copies` times from `top`, the elements of copy
    /// k k strides on, then the pointers stepped and the branch back while
    /// the lead pointer is not at `end`. `hinted` loops get the pragmas'
    /// pipelining hints.
    #[allow(clippy::too_many_arguments)]
    fn range_loop(
        &mut self,
        range: &RangeLoop,
        copies: i64,
        end: Vi,
        top: &str,
        next: &str,
        exit: &str,
        hinted: bool,
    ) -> Lowered<()> {
        let at_top = self.cache.clone();
        self.label_here(top);
        if hinted {
            self.loop_hints(range.hints);
        }
        self.loops.push((exit.to_string(), next.to_string()));
        let result = self.range_copies(range, copies);
        self.loops.pop();
        result?;
        self.reach_next(range.body, next, &at_top);
        self.out.pos = Some(range.pos);
        for &(pointer, stride) in range.pointers {
            let name = self.iname(pointer);
            let step = i64::from(stride) * copies;
            self.out
                .inst("iaddiu", None, &[name.clone(), name, step.to_string()]);
        }
        let (lead_name, end_name) = (self.iname(range.lead), self.iname(end));
        self.out
            .inst("ibne", None, &[lead_name, end_name, top.to_string()]);
        Ok(())
    }

    /// The body `copies` times, copy k addressing the elements k strides
    /// on.
    fn range_copies(&mut self, range: &RangeLoop, copies: i64) -> Lowered<()> {
        let elements: Vec<ElementId> = range.offsets.iter().map(|&(element, ..)| element).collect();
        for copy in 0..copies {
            self.cache.forget_elements(&elements);
            for &(element, pointer, delta) in range.offsets {
                let stride = i64::from(self.program.elements[element].stride);
                self.elements[element] = Some((pointer, delta + copy * stride));
            }
            self.stmts(range.body)?;
        }
        for &(element, pointer, delta) in range.offsets {
            self.elements[element] = Some((pointer, delta));
        }
        Ok(())
    }

    /// `#pragma unroll(N)`: a loop over groups of `factor` elements, then
    /// the rest one at a time. A count read at run time splits with IAND,
    /// `factor` being a power of two. Returns the values every path to
    /// `exit` has.
    fn unrolled_range(
        &mut self,
        range: &RangeLoop,
        trips: Trips,
        factor: i64,
        effects: &Effects,
        exit: &str,
    ) -> Lowered<Cache> {
        match trips {
            Trips::Fixed(n) => {
                let stride = i64::from(range.stride);
                let groups = n / factor;
                let rest = n % factor;
                self.forget(effects);
                let at_exit = self.cache.clone();
                if groups == 1 {
                    // One group: the copies without a loop.
                    self.range_copies(range, factor)?;
                    self.step_pointers(range, factor);
                } else if groups > 1 {
                    let end = self.out.vi("end");
                    self.add_immediate(range.lead, groups * factor * stride, Some(end));
                    let (top, next) = (self.out.label("loop"), self.out.label("next"));
                    self.range_loop(range, factor, end, &top, &next, exit, true)?;
                    self.cache = at_exit.clone();
                }
                if rest > 0 {
                    self.range_copies(range, rest)?;
                }
                Ok(at_exit)
            }
            Trips::Counted(count) => {
                // count & -factor elements in groups, count & (factor - 1)
                // after.
                let mask = self.materialize(-factor, None);
                let grouped = self.out.vi("grouped");
                let names = [self.iname(grouped), self.iname(count), self.iname(mask)];
                self.out.inst("iand", None, &names);
                let low = self.materialize(factor - 1, None);
                let rest = self.out.vi("rest");
                let names = [self.iname(rest), self.iname(count), self.iname(low)];
                self.out.inst("iand", None, &names);
                let groups_end = self.out.vi("end");
                let scaled = self.scale(grouped, range.stride);
                let names = [
                    self.iname(groups_end),
                    self.iname(range.lead),
                    self.iname(scaled),
                ];
                self.out.inst("iadd", None, &names);
                let end = self.out.vi("end");
                let scaled = self.scale(count, range.stride);
                let names = [self.iname(end), self.iname(range.lead), self.iname(scaled)];
                self.out.inst("iadd", None, &names);
                self.forget(effects);
                let at_exit = self.cache.clone();
                let groups_done = self.out.label("grouped");
                let grouped_name = self.iname(grouped);
                self.out.inst(
                    "ibeq",
                    None,
                    &[grouped_name, "vi00".into(), groups_done.clone()],
                );
                let (top, next) = (self.out.label("loop"), self.out.label("next"));
                self.range_loop(range, factor, groups_end, &top, &next, exit, true)?;
                self.label_here(&groups_done);
                self.cache = at_exit.clone();
                let rest_name = self.iname(rest);
                self.out
                    .inst("ibeq", None, &[rest_name, "vi00".into(), exit.to_string()]);
                let (top, next) = (self.out.label("rest"), self.out.label("next"));
                self.range_loop(range, 1, end, &top, &next, exit, false)?;
                Ok(at_exit)
            }
        }
    }

    /// The pointers stepped past `copies` elements.
    fn step_pointers(&mut self, range: &RangeLoop, copies: i64) {
        for &(pointer, stride) in range.pointers {
            let name = self.iname(pointer);
            let step = i64::from(stride) * copies;
            self.out
                .inst("iaddiu", None, &[name.clone(), name, step.to_string()]);
        }
    }

    /// The `next` label after a loop's body: a `continue` reaches it with
    /// only the values the top has.
    fn reach_next(&mut self, body: &[Stmt], next: &str, at_top: &Cache) {
        if continues(body) {
            self.cache = at_top.clone();
        }
        self.label_here(next);
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
                    // The path through `skip` may not evaluate `rhs`.
                    let before = self.cache.clone();
                    self.branch(lhs, !when, &skip)?;
                    self.branch(rhs, when, label)?;
                    self.label_here(&skip);
                    self.cache = before;
                    Ok(())
                }
            }
            ExprKind::Compare { op, lhs, rhs } => {
                let op = if when { *op } else { negated(*op) };
                if matches!(lhs.ty, Type::Int16 | Type::Ref(_)) {
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
            ExprKind::Local(_) => {
                let value = self.bool_value(cond, None)?;
                let mnemonic = if when { "ibne" } else { "ibeq" };
                let name = self.iname(value);
                self.out
                    .inst(mnemonic, None, &[name, "vi00".into(), label.into()]);
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

    /// A `bool` as a value: a VI register, true when it is not 0.
    /// Comparisons of floats are the status flags of a subtraction (FSAND),
    /// `any()` FCAND's VI01, `||` IOR and `!=` of `int16` values ISUB; the
    /// others are set by a branch.
    fn bool_value(&mut self, expr: &Expr, dest: Option<Vi>) -> Lowered<Vi> {
        match &expr.kind {
            ExprKind::Const(Const::Bool(value)) => {
                let reg = self.idest(dest);
                Ok(self.materialize(i64::from(*value), Some(reg)))
            }
            ExprKind::Local(id) => match self.locals[*id] {
                LocalReg::Vi(reg) => Ok(reg),
                _ => self.not_yet("this local as a bool"),
            },
            ExprKind::Call {
                builtin: Builtin::Any,
                args,
            } => {
                let mask = self.clip_mask(args)?;
                self.out
                    .inst("fcand", None, &["vi01".into(), format!("0x{mask:06x}")]);
                let reg = self.idest(dest);
                let name = self.iname(reg);
                self.out
                    .inst("iaddiu", None, &[name, "vi01".into(), "0".into()]);
                Ok(reg)
            }
            ExprKind::Or(lhs, rhs) => {
                // An any() last is read where FCAND leaves it.
                let is_any = |expr: &Expr| {
                    matches!(
                        expr.kind,
                        ExprKind::Call {
                            builtin: Builtin::Any,
                            ..
                        }
                    )
                };
                let (first, last) = if is_any(lhs) && !is_any(rhs) {
                    (rhs, lhs)
                } else {
                    (lhs, rhs)
                };
                let a = self.bool_value(first, None)?;
                let b = match &last.kind {
                    ExprKind::Call {
                        builtin: Builtin::Any,
                        args,
                    } => {
                        let mask = self.clip_mask(args)?;
                        self.out
                            .inst("fcand", None, &["vi01".into(), format!("0x{mask:06x}")]);
                        Vi::Flags
                    }
                    _ => self.bool_value(last, None)?,
                };
                let reg = self.idest(dest);
                let names = [self.iname(reg), self.iname(a), self.iname(b)];
                self.out.inst("ior", None, &names);
                Ok(reg)
            }
            ExprKind::Compare { op, lhs, rhs } if !matches!(lhs.ty, Type::Int16 | Type::Ref(_)) => {
                // a > b is b < a, and a >= b is b <= a: the flags set, never
                // clear, say true.
                let (a, b, op) = match op {
                    CompareOp::Greater => (rhs, lhs, CompareOp::Less),
                    CompareOp::GreaterEqual => (rhs, lhs, CompareOp::LessEqual),
                    op => (lhs, rhs, *op),
                };
                let a = self.vf(a)?;
                let b = self.vf(b)?;
                let difference = self.out.vf("difference");
                let lane = a.lane().letter().to_string();
                let operands = [self.name(difference), self.name(a.reg), self.field(b)];
                self.out.inst("sub", Some(&lane), &operands);
                let mask = match op {
                    CompareOp::Less => 2,
                    CompareOp::LessEqual => 3,
                    _ => 1,
                };
                if op == CompareOp::NotEqual {
                    // Z is 1 when equal: 1 - 1 is 0, and 0 - 1 is not.
                    let flags = self.out.vi("flags");
                    let flags_name = self.iname(flags);
                    self.out
                        .inst("fsand", None, &[flags_name.clone(), mask.to_string()]);
                    let reg = self.idest(dest);
                    let name = self.iname(reg);
                    self.out
                        .inst("iaddi", None, &[name, flags_name, "-1".into()]);
                    return Ok(reg);
                }
                let reg = self.idest(dest);
                let name = self.iname(reg);
                self.out.inst("fsand", None, &[name, mask.to_string()]);
                Ok(reg)
            }
            ExprKind::Compare {
                op: CompareOp::NotEqual,
                lhs,
                rhs,
            } => {
                let a = self.vi(lhs)?;
                let b = self.vi(rhs)?;
                let reg = self.idest(dest);
                let names = [self.iname(reg), self.iname(a), self.iname(b)];
                self.out.inst("isub", None, &names);
                Ok(reg)
            }
            _ => {
                // 0, then 1 unless the condition is false; in a register of
                // its own, since the condition may read the destination.
                let reg = self.out.vi("t");
                let name = self.iname(reg);
                let skip = self.out.label("false");
                self.out
                    .inst("iaddiu", None, &[name.clone(), "vi00".into(), "0".into()]);
                let before = self.cache.clone();
                self.branch(expr, false, &skip)?;
                self.out
                    .inst("iaddiu", None, &[name, "vi00".into(), "1".into()]);
                self.label_here(&skip);
                self.cache = before;
                Ok(reg)
            }
        }
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
            Type::Bool => {
                let reg = self.local_vi(id);
                let result = self.bool_value(value, Some(reg))?;
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
            Type::Int16 | Type::Ref(_) => {
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
                    let value = self.vf_toward(value, reg, target)?;
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
                let value = self.vf_toward(value, reg, target)?;
                self.write_lanes(value, reg, target, raw)?;
                Ok(base)
            }
        }
    }

    /// `value`, for `reg`'s lanes `target`: a scalar that can go in any
    /// lane goes there.
    fn vf_toward(&mut self, value: &Expr, reg: Vf, target: Placement) -> Lowered<Fv> {
        match target.len() {
            1 => self.vf_in_lane(value, Some(reg), target.lane(0)),
            _ => self.vf(value),
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
            Component::Lane(source, from) if self.q_holds == Some((source, from)) => {
                // From Q: 0 + Q, or 1 * Q in w.
                let mnemonic = if target == Lane::W { "mulq" } else { "addq" };
                self.out.inst(
                    mnemonic,
                    Some(&lane),
                    &[dest_name, "vf00".into(), "q".into()],
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
                self.stored(place, base, offset, 1);
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
                self.stored(place, base, offset, i64::from(*size));
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
                let value = match target.len() {
                    1 => self.vf_in_lane(value, None, target.lane(0))?,
                    _ => self.vf(value)?,
                };
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
                self.stored(place, base, offset, 1);
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
            Base::Ref(local) => match &self.program.locals[local].ty {
                Type::Ref(target) => target,
                ty => ty,
            },
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
                Some(at) => at,
                None => return self.not_yet("an element outside its loop"),
            },
            Base::Ref(local) => match self.locals[local] {
                LocalReg::Vi(reg) => (reg, 0),
                _ => return self.not_yet("a ref that is not set"),
            },
        };
        offset += i64::from(place.offset.extent.fixed);
        for term in &place.offset.extent.terms {
            if term.is_xitop() {
                let count = self.xitop_count();
                let scaled = self.scale(count, term.stride);
                base = self.add_regs(base, scaled);
                continue;
            }
            let Some(count_place) = self.member_place(place.base, &term.count) else {
                return self.not_yet("a length read through this place");
            };
            let (count_base, count_offset) = self.address(&count_place)?;
            let count = self.load_int16(&count_place, count_base, count_offset, None);
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
        let key = Key::Sum(a.min(b), a.max(b));
        if let Some(Value::Vi(sum)) = self.cache.find(&key) {
            return sum;
        }
        let sum = self.out.vi("address");
        let names = [self.iname(sum), self.iname(a), self.iname(b)];
        self.out.inst("iadd", None, &names);
        self.cache.insert(Entry {
            key,
            value: Value::Vi(sum),
            reads: Vec::new(),
            locals: Vec::new(),
            elements: Vec::new(),
        });
        sum
    }

    /// The `int16` at `place`, at `base` and `offset`: ILW, or the
    /// register that already holds it.
    fn load_int16(&mut self, place: &Place, base: Vi, offset: i64, dest: Option<Vi>) -> Vi {
        let key = Key::Int16 {
            base,
            offset,
            lane: place.lane,
        };
        if let Some(Value::Vi(reg)) = self.cache.find(&key) {
            return reg;
        }
        let reg = self.idest(dest);
        let names = [self.iname(reg), self.iname(base)];
        self.out.inst(
            "ilw",
            Some(&place.lane.letter().to_string()),
            &[names[0].clone(), format!("{offset}({})", names[1])],
        );
        self.cache.insert(Entry {
            key,
            value: Value::Vi(reg),
            reads: vec![Read {
                region: values::region(self.program, place),
                at: Some((base, offset)),
            }],
            locals: Vec::new(),
            elements: Vec::new(),
        });
        reg
    }

    /// The qword at `base` and `offset`, read through `place`: LQ of all
    /// four lanes, or the register that already holds them. VCL narrows
    /// the lanes no one reads.
    fn load_qword(&mut self, place: &Place, base: Vi, offset: i64, dest: Option<Vf>) -> Vf {
        let key = Key::Qword { base, offset };
        if let Some(Value::Vf(value)) = self.cache.find(&key) {
            return value.reg;
        }
        let reg = self.dest(dest);
        let names = [self.name(reg), self.iname(base)];
        self.out.inst(
            "lq",
            Some("xyzw"),
            &[names[0].clone(), format!("{offset}({})", names[1])],
        );
        self.cache.insert(Entry {
            key,
            value: Value::Vf(Fv {
                reg,
                placement: Placement::identity(4),
            }),
            reads: vec![Read {
                region: values::region(self.program, place),
                at: Some((base, offset)),
            }],
            locals: Vec::new(),
            elements: Vec::new(),
        });
        reg
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

    /// Puts a pure expression's value in the cache under `key`.
    fn remember(&mut self, expr: &Expr, key: String, value: Value) {
        let mut locals = Vec::new();
        let mut places = Vec::new();
        values::inputs(expr, &mut locals, &mut places);
        let reads = places
            .into_iter()
            .map(|place| Read {
                region: values::region(self.program, place),
                at: None,
            })
            .collect();
        let mut elements = Vec::new();
        values::element_bases(expr, &mut elements);
        self.cache.insert(Entry {
            key: Key::Expr(key),
            value,
            reads,
            locals,
            elements,
        });
    }

    fn vi_into(&mut self, expr: &Expr, dest: Option<Vi>) -> Lowered<Vi> {
        let cacheable = match &expr.kind {
            ExprKind::Const(Const::Int(value)) => value & 0xffff != 0,
            ExprKind::Binary { .. } | ExprKind::Neg(_) | ExprKind::Convert { .. } => true,
            ExprKind::Call {
                builtin: Builtin::Xitop,
                ..
            }
            | ExprKind::Address(_)
            | ExprKind::RefTo { .. } => true,
            _ => false,
        };
        let key = if cacheable {
            values::expr_key(expr)
        } else {
            None
        };
        if let Some(key) = &key
            && let Some(Value::Vi(reg)) = self.cache.find(&Key::Expr(key.clone()))
        {
            return Ok(reg);
        }
        let reg = self.vi_lower(expr, dest)?;
        if let Some(key) = key {
            self.remember(expr, key, Value::Vi(reg));
        }
        Ok(reg)
    }

    fn vi_lower(&mut self, expr: &Expr, dest: Option<Vi>) -> Lowered<Vi> {
        match &expr.kind {
            ExprKind::Const(Const::Int(value)) => Ok(self.materialize(*value, dest)),
            ExprKind::Local(id) => match self.locals[*id] {
                LocalReg::Vi(reg) => Ok(reg),
                _ => self.not_yet("this local as an int16"),
            },
            ExprKind::Load(place) => {
                let (base, offset) = self.address(place)?;
                Ok(self.load_int16(place, base, offset, dest))
            }
            ExprKind::Address(place) => {
                let (base, offset) = self.address(place)?;
                Ok(self.add_immediate(base, offset, dest))
            }
            ExprKind::RefTo { address, .. }
            | ExprKind::Convert {
                conversion: Conversion::Bits,
                operand: address,
            } => self.vi_into(address, dest),
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
    fn float_constant(&mut self, value: f32, dest: Option<Vf>, lane: Lane) -> Fv {
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
        let field = lane.letter().to_string();
        // VF00's w is 1, not 0: I times 1 there.
        let mnemonic = if lane == Lane::W { "muli" } else { "addi" };
        self.out
            .inst(mnemonic, Some(&field), &[name, "vf00".into(), "i".into()]);
        Fv::scalar(reg, lane)
    }

    fn vf_into(&mut self, expr: &Expr, dest: Option<Vf>) -> Lowered<Fv> {
        self.vf_full(expr, dest, Lane::X)
    }

    /// A scalar in `lane`, when its instruction can put it in any lane;
    /// in `dest` then too, which keeps its other lanes.
    fn vf_in_lane(&mut self, expr: &Expr, dest: Option<Vf>, lane: Lane) -> Lowered<Fv> {
        let dest = dest.filter(|_| any_lane(expr));
        self.vf_full(expr, dest, lane)
    }

    /// `expr`'s value, in `dest` when it can, and a scalar that can go in
    /// any lane in `lane`.
    fn vf_full(&mut self, expr: &Expr, dest: Option<Vf>, lane: Lane) -> Lowered<Fv> {
        let cacheable = match &expr.kind {
            ExprKind::Const(Const::Float(value)) => {
                !(*value == 0.0 && value.is_sign_positive()) && *value != 1.0
            }
            ExprKind::Const(Const::Int(value)) => *value != 0 && *value != 1,
            ExprKind::Local(_)
            | ExprKind::Load(_)
            | ExprKind::Swizzle { .. }
            | ExprKind::Column { .. }
            | ExprKind::Convert {
                conversion: Conversion::Bits,
                ..
            } => false,
            _ => !matches!(expr.ty, Type::Mat { .. }),
        };
        let key = if cacheable {
            values::expr_key(expr)
        } else {
            None
        };
        if let Some(key) = &key
            && let Some(Value::Vf(value)) = self.cache.find(&Key::Expr(key.clone()))
        {
            return Ok(value);
        }
        let value = self.vf_lower(expr, dest, lane)?;
        if let Some(key) = key {
            self.remember(expr, key, Value::Vf(value));
        }
        Ok(value)
    }

    fn vf_lower(&mut self, expr: &Expr, dest: Option<Vf>, lane: Lane) -> Lowered<Fv> {
        if matches!(
            expr.kind,
            ExprKind::Binary {
                op: BinOp::Add | BinOp::Sub,
                ..
            }
        ) && let Some(value) = self.contract(expr, dest)?
        {
            return Ok(value);
        }
        match &expr.kind {
            ExprKind::Const(Const::Float(value)) => Ok(self.float_constant(*value, dest, lane)),
            ExprKind::Const(Const::Int(value)) => {
                Ok(self.float_constant(*value as f32, dest, lane))
            }
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
                let reg = self.load_qword(place, base, offset, dest);
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
            ExprKind::Neg(operand) => self.negate(operand, dest, lane),
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::Add => self.elementwise("add", lhs, rhs, dest, true),
                BinOp::Sub => self.elementwise("sub", lhs, rhs, dest, false),
                BinOp::Mul => self.elementwise("mul", lhs, rhs, dest, true),
                BinOp::Min => self.elementwise("mini", lhs, rhs, dest, true),
                BinOp::Max => self.elementwise("max", lhs, rhs, dest, true),
                BinOp::Div => self.divide(lhs, rhs, dest, lane),
                BinOp::MatVec => self.transform(lhs, rhs, dest),
                BinOp::VecMat => self.transform(rhs, lhs, dest),
                _ => self.not_yet("this operation on VF values"),
            },
            ExprKind::Construct(_) => self.construct(expr, dest),
            ExprKind::Convert {
                conversion,
                operand,
            } => self.convert(*conversion, operand, dest, lane),
            ExprKind::Column { matrix, index } => {
                let Type::Mat { size, .. } = matrix.ty else {
                    return self.not_yet("a column of this type");
                };
                let placement = Placement::identity(usize::from(size).min(4));
                if let ExprKind::Load(place) = &matrix.kind {
                    // One qword of a matrix in memory.
                    let (base, offset) = self.address(place)?;
                    let reg = self.load_qword(place, base, offset + *index as i64, dest);
                    return Ok(Fv { reg, placement });
                }
                let regs = self.mat(matrix)?;
                Ok(Fv {
                    reg: regs[*index],
                    placement,
                })
            }
            ExprKind::Call { builtin, args } => self.builtin(*builtin, args, dest, lane),
            _ => self.not_yet("this expression as a VF value"),
        }
    }

    fn negate(&mut self, operand: &Expr, dest: Option<Vf>, lane: Lane) -> Lowered<Fv> {
        let value = self.vf(operand)?;
        let reg = self.dest(dest);
        if value.is_scalar() {
            // 0 - x, in any lane but w (VF00's w is 1): 0 - x * 1 there.
            if lane == Lane::W {
                self.out.inst(
                    "mula",
                    Some("w"),
                    &["acc".into(), "vf00".into(), "vf00[x]".into()],
                );
                let operands = [self.name(reg), "vf00".into(), self.field(value)];
                self.out.inst("msub", Some("w"), &operands);
            } else {
                let operands = [self.name(reg), "vf00".into(), self.field(value)];
                self.out
                    .inst("sub", Some(&lane.letter().to_string()), &operands);
            }
            return Ok(Fv::scalar(reg, lane));
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
        } else if commutative && self.is_q(a) && !self.is_q(b) {
            // Two scalars: Q as the broadcast operand.
            (b, a)
        } else {
            (a, b)
        };
        let reg = self.dest(dest);
        let q_form = self.is_q(b) && matches!(mnemonic, "add" | "sub" | "mul");
        let (mnemonic, operand) = if q_form {
            (format!("{mnemonic}q"), "q".to_string())
        } else if b.is_scalar() {
            (mnemonic.to_string(), self.field(b))
        } else {
            (mnemonic.to_string(), self.name(b.reg))
        };
        let operands = [self.name(reg), self.name(a.reg), operand];
        self.out
            .inst(&mnemonic, Some(&Self::mask(&a.placement)), &operands);
        Ok(Fv {
            reg,
            placement: a.placement,
        })
    }

    /// A sum of two or more products, and any other terms, as one
    /// MULA/MADDA/MSUBA chain: n products and k other terms take n + k
    /// instructions instead of 2n + k - 1. VU1 rounds each product before
    /// adding it, so the chain's value is the separate operations' exactly.
    /// `None` when `expr` is not such a sum.
    fn contract(&mut self, expr: &Expr, dest: Option<Vf>) -> Lowered<Option<Fv>> {
        let mut terms = Vec::new();
        flatten_sum(expr, false, &mut terms);
        let is_product = |term: &Expr| matches!(term.kind, ExprKind::Binary { op: BinOp::Mul, .. });
        let products = terms.iter().filter(|(_, term)| is_product(term)).count();
        if products < 2 || terms[0].0 {
            return Ok(None);
        }
        // Each term as a lane-wise operand and a broadcast or lane-wise
        // one; a term that is not a product is times VF00's w.
        let mut operands = Vec::new();
        for &(negative, term) in &terms {
            let (a, b) = match &term.kind {
                ExprKind::Binary {
                    op: BinOp::Mul,
                    lhs,
                    rhs,
                } => {
                    let (a, b) = (self.vf(lhs)?, self.vf(rhs)?);
                    if a.is_scalar() && !b.is_scalar() {
                        (b, a)
                    } else {
                        (a, b)
                    }
                }
                _ => (self.vf(term)?, Fv::scalar(Vf::Zero, Lane::W)),
            };
            operands.push((negative, a, b));
        }
        let placement = operands[0].1.placement;
        let fits = operands.iter().all(|(_, a, b)| {
            a.placement == placement
                && (b.is_scalar() || b.placement == placement)
                && !self.is_q(*a)
                && !self.is_q(*b)
        });
        if !fits {
            return Ok(None);
        }
        let reg = self.dest(dest);
        let mask = Self::mask(&placement);
        let last = operands.len() - 1;
        for (index, (negative, a, b)) in operands.into_iter().enumerate() {
            let mnemonic = match (index, negative) {
                (0, _) => "mula",
                (index, false) if index == last => "madd",
                (index, true) if index == last => "msub",
                (_, false) => "madda",
                (_, true) => "msuba",
            };
            let target = if index == last {
                self.name(reg)
            } else {
                "acc".to_string()
            };
            let operand = if b.is_scalar() {
                self.field(b)
            } else {
                self.name(b.reg)
            };
            let operands = [target, self.name(a.reg), operand];
            self.out.inst(mnemonic, Some(&mask), &operands);
        }
        Ok(Some(Fv { reg, placement }))
    }

    fn divide(&mut self, lhs: &Expr, rhs: &Expr, dest: Option<Vf>, lane: Lane) -> Lowered<Fv> {
        let divisor = self.vf(rhs)?;
        let dividend = self.vf(lhs)?;
        let reg = self.dest(dest);
        if dividend.is_scalar() {
            let operands = ["q".into(), self.field(dividend), self.field(divisor)];
            self.fdiv("div", &operands);
            return Ok(self.q_to_lane(Some(reg), lane));
        }
        let operands = ["q".into(), "vf00[w]".into(), self.field(divisor)];
        self.fdiv("div", &operands);
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

    fn convert(
        &mut self,
        conversion: Conversion,
        operand: &Expr,
        dest: Option<Vf>,
        lane: Lane,
    ) -> Lowered<Fv> {
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
                let field = lane.letter().to_string();
                let names = [self.name(lane_reg), self.iname(value)];
                self.out.inst("mfir", Some(&field), &names);
                if conversion == Conversion::IntToLane {
                    return Ok(Fv::scalar(lane_reg, lane));
                }
                let reg = self.dest(dest);
                let names = [self.name(reg), self.name(lane_reg)];
                self.out.inst("itof0", Some(&field), &names);
                Ok(Fv::scalar(reg, lane))
            }
            Conversion::Bits => self.vf_full(operand, dest, lane),
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

    /// Q into a VF register's x: ADDQ of VF00's 0. Readers that take Q
    /// read it there instead while it lasts, and the copy is dead.
    fn q_to_lane(&mut self, dest: Option<Vf>, lane: Lane) -> Fv {
        let reg = self.dest(dest);
        let name = self.name(reg);
        // 0 + Q, or 1 * Q in w.
        let mnemonic = if lane == Lane::W { "mulq" } else { "addq" };
        self.out.inst(
            mnemonic,
            Some(&lane.letter().to_string()),
            &[name, "vf00".into(), "q".into()],
        );
        self.q_holds = Some((reg, lane));
        Fv::scalar(reg, lane)
    }

    fn builtin(
        &mut self,
        builtin: Builtin,
        args: &[Expr],
        dest: Option<Vf>,
        lane: Lane,
    ) -> Lowered<Fv> {
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
                self.fdiv("rsqrt", &operands);
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
                self.fdiv("sqrt", &operands);
                Ok(self.q_to_lane(dest, lane))
            }
            Builtin::Sqrt => {
                let value = self.vf(&args[0])?;
                let operands = ["q".into(), self.field(value)];
                self.fdiv("sqrt", &operands);
                Ok(self.q_to_lane(dest, lane))
            }
            Builtin::Rsqrt => {
                let value = self.vf(&args[0])?;
                let operands = ["q".into(), "vf00[w]".into(), self.field(value)];
                self.fdiv("rsqrt", &operands);
                Ok(self.q_to_lane(dest, lane))
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
                Ok(self.p_to_lane(dest, lane))
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
                Ok(self.p_to_lane(dest, lane))
            }
            _ => self.not_yet("this built-in as a VF value"),
        }
    }

    fn p_to_lane(&mut self, dest: Option<Vf>, lane: Lane) -> Fv {
        let reg = self.dest(dest);
        let name = self.name(reg);
        self.out
            .inst("mfp", Some(&lane.letter().to_string()), &[name, "p".into()]);
        Fv::scalar(reg, lane)
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
                    let reg = dest.map(|dest| dest[qword as usize]);
                    regs.push(self.load_qword(place, base, offset + qword, reg));
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

/// The cache key of XITOP's count: `xitop()`'s.
const XITOP_KEY: &str = "Xitop()";

/// A range-for being lowered: its body and pragmas, its pointers and
/// strides, each element's pointer and offset, and the pointer whose end
/// ends it.
struct RangeLoop<'a> {
    body: &'a [Stmt],
    hints: &'a LoopHints,
    pos: Pos,
    pointers: &'a [(Vi, u32)],
    offsets: &'a [(ElementId, Vi, i64)],
    lead: Vi,
    stride: u32,
}

/// A range-for's trip count.
#[derive(Clone, Copy)]
enum Trips {
    Fixed(i64),
    /// Read from memory, possibly 0.
    Counted(Vi),
}

/// The lanes a store of VF lanes writes in its qword; `None` for other
/// stores.
fn store_lanes(place: &Place, components: Option<&[usize]>) -> Option<Placement> {
    if matches!(place.ty, Type::Qword | Type::Int16 | Type::Mat { .. }) {
        return None;
    }
    let whole = memory_lanes(&place.ty, place.lane)?;
    match components {
        Some(components) => whole.select(components),
        None => Some(whole),
    }
}

/// Whether two places are in the same qword.
fn same_qword(a: &Place, b: &Place) -> bool {
    let indices = |place: &Place| -> Option<Vec<String>> {
        place
            .offset
            .indices
            .iter()
            .map(|(index, stride)| values::expr_key(index).map(|key| format!("{key}*{stride}")))
            .collect()
    };
    a.base == b.base
        && a.offset.extent == b.offset.extent
        && indices(a).is_some()
        && indices(a) == indices(b)
}

/// Whether lowering `value` costs nothing: it names a register that
/// already holds it.
fn passes_through(value: &Expr) -> bool {
    match &value.kind {
        ExprKind::Local(_) | ExprKind::Load(_) => true,
        ExprKind::Swizzle { base, .. }
        | ExprKind::Column { matrix: base, .. }
        | ExprKind::Convert {
            conversion: Conversion::Bits,
            operand: base,
        } => passes_through(base),
        _ => false,
    }
}

/// Whether a scalar expression's last instruction can put it in any lane
/// and writes only that lane.
fn any_lane(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Const(Const::Float(_) | Const::Int(_)) => true,
        ExprKind::Neg(operand) => operand.ty.vf_lanes() == Some(1),
        ExprKind::Binary {
            op: BinOp::Div,
            lhs,
            ..
        } => lhs.ty.vf_lanes() == Some(1),
        ExprKind::Call { builtin, .. } => matches!(
            builtin,
            Builtin::Length
                | Builtin::Sqrt
                | Builtin::Rsqrt
                | Builtin::Esadd
                | Builtin::Ersadd
                | Builtin::Eleng
                | Builtin::Erleng
                | Builtin::Esum
                | Builtin::Ercpr
                | Builtin::Esqrt
                | Builtin::Ersqrt
                | Builtin::Esin
                | Builtin::Eatan
                | Builtin::Eexp
        ),
        ExprKind::Convert {
            conversion: Conversion::IntToLane | Conversion::IntToFloat,
            ..
        } => true,
        ExprKind::Convert {
            conversion: Conversion::Bits,
            operand,
        } => any_lane(operand),
        _ => false,
    }
}

/// The terms of a sum of floats, each with whether it is subtracted:
/// through additions and subtractions whose operands have the sum's type.
fn flatten_sum<'e>(expr: &'e Expr, negative: bool, terms: &mut Vec<(bool, &'e Expr)>) {
    if let ExprKind::Binary {
        op: op @ (BinOp::Add | BinOp::Sub),
        lhs,
        rhs,
    } = &expr.kind
        && lhs.ty == expr.ty
        && rhs.ty == expr.ty
        && expr.ty.is_float()
    {
        flatten_sum(lhs, negative, terms);
        flatten_sum(rhs, negative != (*op == BinOp::Sub), terms);
        return;
    }
    terms.push((negative, expr));
}

/// Whether a condition has parts that some paths skip.
fn short_circuits(cond: &Expr) -> bool {
    match &cond.kind {
        ExprKind::And(..) | ExprKind::Or(..) => true,
        ExprKind::Not(inner) => short_circuits(inner),
        _ => false,
    }
}

/// Whether `body` has a `continue` of its own loop.
fn continues(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Continue { .. } => true,
        Stmt::If {
            then, otherwise, ..
        } => continues(then) || continues(otherwise),
        _ => false,
    })
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
