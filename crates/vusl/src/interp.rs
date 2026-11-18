//! A reference interpreter of the checked program: what a program means,
//! computed from its checked tree rather than its code, so the code
//! generated for it can be tested against a VU model.
//!
//! Values are their components, whatever lanes the code puts them in.
//! Arithmetic is `f32`, in the order the code computes it (a vector over a
//! scalar is the vector times the scalar's reciprocal, as the FDIV unit and
//! a Q multiply do), so the two agree exactly on values both represent
//! exactly. `int16` values wrap at 16 bits, a comparison of two is the sign
//! of their difference, and a float comparison reads the sign and zero of
//! the difference as the status flags do.

use crate::sema::hir::*;
use crate::sema::types::{Lane, Type};

/// VU1 data memory and the registers a program reads from the host.
#[derive(Clone, Debug)]
pub struct Machine {
    pub memory: Vec<[u32; 4]>,
    /// What XTOP and XITOP read.
    pub top: u16,
    pub itop: u16,
    /// Each XGKICK's address.
    pub kicks: Vec<u16>,
    /// The clip flags: the last four judgements, 6 bits each, newest
    /// lowest.
    clip: u32,
    judgements: usize,
}

impl Machine {
    /// A machine with `memory`, and `top` and `itop` for XTOP and XITOP.
    pub fn new(memory: Vec<[u32; 4]>, top: u16, itop: u16) -> Self {
        Self {
            memory,
            top,
            itop,
            ..Self::default()
        }
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            memory: vec![[0; 4]; 1024],
            top: 0,
            itop: 0,
            kicks: Vec::new(),
            clip: 0,
            judgements: 0,
        }
    }
}

/// A value: a VF value's components as bits, an `int16`, a `bool`, a
/// matrix's stored qwords, or a clip judgement's number.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Vf(Vec<u32>),
    Int(u16),
    Bool(bool),
    Mat(Vec<Vec<u32>>),
    Clip(usize),
}

/// How a statement ended.
enum Flow {
    Normal,
    Break,
    Continue,
    Exit,
}

type Run<T> = Result<T, String>;

/// Runs `program` on `machine`, for at most `max_steps` statements, until
/// it ends or yields.
pub fn run(program: &Program, machine: &mut Machine, max_steps: usize) -> Run<()> {
    run_with_host(program, machine, max_steps, &mut |_| false)
}

/// Runs `program` on `machine` as [`run`] does, with `host` called at each
/// `yield`: it may change the memory and what XTOP and XITOP read, as the
/// host does before MSCNT, and the program goes on when it returns true.
pub fn run_with_host(
    program: &Program,
    machine: &mut Machine,
    max_steps: usize,
    host: &mut dyn FnMut(&mut Machine) -> bool,
) -> Run<()> {
    let mut interpreter = Interpreter {
        program,
        machine,
        host,
        locals: vec![None; program.locals.len()],
        elements: vec![None; program.elements.len()],
        steps: 0,
        max_steps,
    };
    interpreter.stmts(&program.body).map(|_| ())
}

struct Interpreter<'p, 'm> {
    program: &'p Program,
    machine: &'m mut Machine,
    host: &'m mut dyn FnMut(&mut Machine) -> bool,
    locals: Vec<Option<Value>>,
    /// Each range element's address, inside its loop.
    elements: Vec<Option<u16>>,
    steps: usize,
    max_steps: usize,
}

fn float(bits: u32) -> f32 {
    f32::from_bits(bits)
}

fn bits(value: f32) -> u32 {
    value.to_bits()
}

/// The lanes a value of `ty` takes in memory from `lane`.
fn lanes(ty: &Type, lane: Lane) -> Vec<usize> {
    let count = match ty {
        Type::Qword => 4,
        ty => ty.vf_lanes().unwrap_or(1),
    };
    (lane.index()..lane.index() + count).collect()
}

impl Interpreter<'_, '_> {
    fn step(&mut self) -> Run<()> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return Err(format!("more than {} steps", self.max_steps));
        }
        Ok(())
    }

    fn stmts(&mut self, stmts: &[Stmt]) -> Run<Flow> {
        for stmt in stmts {
            match self.stmt(stmt)? {
                Flow::Normal => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }

    fn stmt(&mut self, stmt: &Stmt) -> Run<Flow> {
        self.step()?;
        match stmt {
            Stmt::SetLocal {
                local,
                column,
                components,
                value,
                ..
            } => {
                let value = self.eval(value)?;
                self.set_local(*local, *column, components.as_deref(), value)?;
            }
            Stmt::Store {
                place,
                components,
                value,
                ..
            } => {
                let value = self.eval(value)?;
                self.store(place, components.as_deref(), value)?;
            }
            Stmt::Copy {
                to, from, qwords, ..
            } => {
                let from = self.address(from)?;
                let to = self.address(to)?;
                for qword in 0..*qwords as usize {
                    let value = *self.memory(from as usize + qword)?;
                    *self.memory(to as usize + qword)? = value;
                }
            }
            Stmt::Kick { place, .. } => {
                let address = self.address(place)?;
                self.machine.kicks.push(address);
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let branch = if self.condition(cond)? {
                    then
                } else {
                    otherwise
                };
                return self.stmts(branch);
            }
            Stmt::Loop { kind, body, .. } => return self.run_loop(kind, body),
            Stmt::Break { .. } => return Ok(Flow::Break),
            Stmt::Continue { .. } => return Ok(Flow::Continue),
            Stmt::Exit { .. } => return Ok(Flow::Exit),
            // A yield ends the run until the next MSCNT, which the host
            // gives or not.
            Stmt::Yield { .. } => {
                if !(self.host)(self.machine) {
                    return Ok(Flow::Exit);
                }
            }
            Stmt::Barrier { .. } => {}
        }
        Ok(Flow::Normal)
    }

    fn run_loop(&mut self, kind: &LoopKind, body: &[Stmt]) -> Run<Flow> {
        match kind {
            LoopKind::Condition {
                cond_stmts,
                cond,
                test_first,
                step,
            } => {
                if *test_first {
                    if let Flow::Exit = self.stmts(cond_stmts)? {
                        return Ok(Flow::Exit);
                    }
                    if !self.condition(cond)? {
                        return Ok(Flow::Normal);
                    }
                }
                loop {
                    self.step()?;
                    match self.stmts(body)? {
                        Flow::Break => return Ok(Flow::Normal),
                        Flow::Exit => return Ok(Flow::Exit),
                        Flow::Normal | Flow::Continue => {}
                    }
                    if let Flow::Exit = self.stmts(step)? {
                        return Ok(Flow::Exit);
                    }
                    if let Flow::Exit = self.stmts(cond_stmts)? {
                        return Ok(Flow::Exit);
                    }
                    if !self.condition(cond)? {
                        return Ok(Flow::Normal);
                    }
                }
            }
            LoopKind::Range { elements, count } => {
                // The count is unsigned: the code steps a pointer until it
                // reaches the end of the array.
                let count = match count {
                    Count::Fixed(n) => *n as u16,
                    Count::Member(place) => {
                        let address = self.address(place)?;
                        (self.memory(address as usize)?[place.lane.index()] & 0xffff) as u16
                    }
                    Count::Xitop => self.machine.itop,
                };
                if count == 0 {
                    return Ok(Flow::Normal);
                }
                for &element in elements {
                    let array = &self.program.elements[element].array;
                    let address = self.address(array)?;
                    self.elements[element] = Some(address);
                }
                let mut done: u16 = 0;
                loop {
                    self.step()?;
                    match self.stmts(body)? {
                        Flow::Break => return Ok(Flow::Normal),
                        Flow::Exit => return Ok(Flow::Exit),
                        Flow::Normal | Flow::Continue => {}
                    }
                    for &element in elements {
                        let stride = self.program.elements[element].stride as u16;
                        let address = self.elements[element].expect("set before the loop");
                        self.elements[element] = Some(address.wrapping_add(stride));
                    }
                    done += 1;
                    if done == count {
                        return Ok(Flow::Normal);
                    }
                }
            }
        }
    }

    // ----------------------------------------------
    // Memory
    // ----------------------------------------------

    fn memory(&mut self, address: usize) -> Run<&mut [u32; 4]> {
        self.machine
            .memory
            .get_mut(address)
            .ok_or_else(|| format!("address {address} is outside data memory"))
    }

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

    /// The address a place's base is at.
    fn base_address(&self, base: Base) -> Run<u16> {
        Ok(match base {
            Base::Param(index) => match self.program.params[index].placement {
                ParamPlacement::Address(address) => address as u16,
                ParamPlacement::Xtop { .. } => self.machine.top,
                ParamPlacement::Xitop => return Err("the address of an xitop count".into()),
            },
            Base::Element(element) => {
                self.elements[element].ok_or("an element outside its loop")?
            }
            Base::Ref(local) => match self.locals[local] {
                Some(Value::Int(address)) => address,
                _ => return Err("a ref that is not set".into()),
            },
        })
    }

    /// A place's qword address.
    fn address(&mut self, place: &Place) -> Run<u16> {
        let mut address = self.base_address(place.base)?;
        address = address.wrapping_add(place.offset.extent.fixed as u16);
        for term in &place.offset.extent.terms {
            if term.is_xitop() {
                let count = self.machine.itop;
                address = address.wrapping_add(count.wrapping_mul(term.stride as u16));
                continue;
            }
            // The length's member, at its fixed place from the base.
            let mut id = self
                .base_struct(place.base)
                .ok_or("a length outside a struct")?;
            let mut count_address = self.base_address(place.base)?;
            let mut lane = 0;
            for &index in &term.count {
                let member = &self.program.structs[id].members[index];
                count_address = count_address.wrapping_add(member.offset.fixed as u16);
                lane = member.lane.index();
                if let Type::Struct(next) = member.ty {
                    id = next;
                }
            }
            let count = (self.memory(count_address as usize)?[lane] & 0xffff) as u16;
            address = address.wrapping_add(count.wrapping_mul(term.stride as u16));
        }
        for (index, stride) in &place.offset.indices {
            let Value::Int(index) = self.eval(index)? else {
                return Err("an index that is not an int16".into());
            };
            address = address.wrapping_add(index.wrapping_mul(*stride as u16));
        }
        Ok(address)
    }

    fn load(&mut self, place: &Place) -> Run<Value> {
        let address = self.address(place)? as usize;
        match &place.ty {
            Type::Int16 => Ok(Value::Int(
                (self.memory(address)?[place.lane.index()] & 0xffff) as u16,
            )),
            Type::Mat { size, .. } => {
                let mut qwords = Vec::new();
                for qword in 0..usize::from(*size) {
                    let value = *self.memory(address + qword)?;
                    qwords.push(value[..usize::from(*size).min(4)].to_vec());
                }
                Ok(Value::Mat(qwords))
            }
            ty => {
                let value = *self.memory(address)?;
                Ok(Value::Vf(
                    lanes(ty, place.lane)
                        .iter()
                        .map(|&lane| value[lane])
                        .collect(),
                ))
            }
        }
    }

    fn store(&mut self, place: &Place, components: Option<&[usize]>, value: Value) -> Run<()> {
        let address = self.address(place)? as usize;
        match (&place.ty, value) {
            (Type::Int16, Value::Int(value)) => {
                // ISW stores the 16 bits zero-extended.
                self.memory(address)?[place.lane.index()] = u32::from(value);
            }
            (Type::Mat { size, .. }, Value::Mat(qwords)) => {
                for (qword, components) in qwords.iter().enumerate() {
                    let memory = self.memory(address + qword)?;
                    for (lane, bits) in components
                        .iter()
                        .enumerate()
                        .take(usize::from(*size).min(4))
                    {
                        memory[lane] = *bits;
                    }
                }
            }
            (ty, Value::Vf(values)) => {
                let lanes = lanes(ty, place.lane);
                let lanes: Vec<usize> = match components {
                    Some(components) => components.iter().map(|&c| lanes[c]).collect(),
                    None => lanes,
                };
                let memory = self.memory(address)?;
                for (lane, bits) in lanes.iter().zip(values) {
                    memory[*lane] = bits;
                }
            }
            (_, value) => return Err(format!("storing {value:?}")),
        }
        Ok(())
    }

    fn set_local(
        &mut self,
        id: LocalId,
        column: Option<usize>,
        components: Option<&[usize]>,
        value: Value,
    ) -> Run<()> {
        match (column, components) {
            (None, None) => {
                self.locals[id] = Some(value);
                Ok(())
            }
            (column, components) => {
                let size = match &self.program.locals[id].ty {
                    Type::Mat { size, .. } => usize::from(*size).min(4),
                    ty => ty.vf_lanes().unwrap_or(1),
                };
                let Value::Vf(parts) = value else {
                    return Err("a part that is not a VF value".into());
                };
                let target: &mut Vec<u32> = match (&mut self.locals[id], column) {
                    (Some(Value::Mat(qwords)), Some(column)) => &mut qwords[column],
                    (slot @ None, None) => {
                        *slot = Some(Value::Vf(vec![0; size]));
                        let Some(Value::Vf(vector)) = slot else {
                            unreachable!()
                        };
                        vector
                    }
                    (Some(Value::Vf(vector)), None) => vector,
                    _ => return Err("a part of a local that is not set".into()),
                };
                match components {
                    Some(components) => {
                        for (&component, bits) in components.iter().zip(parts) {
                            target[component] = bits;
                        }
                    }
                    None => *target = parts,
                }
                Ok(())
            }
        }
    }

    // ----------------------------------------------
    // Expressions
    // ----------------------------------------------

    fn vf(&mut self, expr: &Expr) -> Run<Vec<u32>> {
        match self.eval(expr)? {
            Value::Vf(components) => Ok(components),
            other => Err(format!("{other:?} where a VF value is expected")),
        }
    }

    fn int(&mut self, expr: &Expr) -> Run<u16> {
        match self.eval(expr)? {
            Value::Int(value) => Ok(value),
            other => Err(format!("{other:?} where an int16 is expected")),
        }
    }

    fn mat(&mut self, expr: &Expr) -> Run<Vec<Vec<u32>>> {
        match self.eval(expr)? {
            Value::Mat(qwords) => Ok(qwords),
            other => Err(format!("{other:?} where a matrix is expected")),
        }
    }

    fn condition(&mut self, expr: &Expr) -> Run<bool> {
        match self.eval(expr)? {
            Value::Bool(value) => Ok(value),
            other => Err(format!("{other:?} where a bool is expected")),
        }
    }

    /// Two VF values element by element; a scalar broadcasts.
    fn zip(a: &[u32], b: &[u32], op: impl Fn(f32, f32) -> f32) -> Vec<u32> {
        let count = a.len().max(b.len());
        (0..count)
            .map(|index| {
                let x = float(if a.len() == 1 { a[0] } else { a[index] });
                let y = float(if b.len() == 1 { b[0] } else { b[index] });
                bits(op(x, y))
            })
            .collect()
    }

    /// The sum of each qword times its weight, as the multiply-add chain
    /// computes it.
    fn weighted_sum(qwords: &[Vec<u32>], weights: &[u32]) -> Vec<u32> {
        let size = qwords[0].len();
        let mut sum = vec![0.0_f32; size];
        for (qword, weight) in qwords.iter().zip(weights) {
            for (lane, value) in sum.iter_mut().enumerate() {
                *value += float(qword[lane]) * float(*weight);
            }
        }
        sum.into_iter().map(bits).collect()
    }

    fn dot(a: &[u32], b: &[u32]) -> f32 {
        let products: Vec<f32> = a
            .iter()
            .zip(b)
            .map(|(x, y)| float(*x) * float(*y))
            .collect();
        products
            .iter()
            .skip(1)
            .fold(products[0], |sum, value| sum + value)
    }

    fn judgement_mask(&self, args: &[Value]) -> Run<u32> {
        let mut mask = 0;
        for arg in args {
            let Value::Clip(judgement) = arg else {
                return Err("clipflags that are not a judgement".into());
            };
            let age = self.machine.judgements - 1 - judgement;
            mask |= 0x3f << (6 * age);
        }
        Ok(mask)
    }

    fn eval(&mut self, expr: &Expr) -> Run<Value> {
        Ok(match &expr.kind {
            ExprKind::Const(Const::Float(value)) => Value::Vf(vec![bits(*value)]),
            ExprKind::Const(Const::Int(value)) => match expr.ty {
                Type::Float => Value::Vf(vec![bits(*value as f32)]),
                _ => Value::Int(*value as u16),
            },
            ExprKind::Const(Const::Bool(value)) => Value::Bool(*value),
            ExprKind::Address(place) => Value::Int(self.address(place)?),
            ExprKind::RefTo { address, .. } => self.eval(address)?,
            ExprKind::Local(id) => self.locals[*id]
                .clone()
                .ok_or_else(|| format!("local '{}' is not set", self.program.locals[*id].name))?,
            ExprKind::Load(place) => self.load(place)?,
            ExprKind::Swizzle { base, components } => {
                let base = self.vf(base)?;
                Value::Vf(components.iter().map(|&c| base[c]).collect())
            }
            ExprKind::Neg(operand) => match self.eval(operand)? {
                // 0 - x, as the code computes it, so -0 does not appear.
                Value::Vf(components) => Value::Vf(
                    components
                        .into_iter()
                        .map(|c| bits(0.0 - float(c)))
                        .collect(),
                ),
                Value::Int(value) => Value::Int(value.wrapping_neg()),
                other => return Err(format!("negating {other:?}")),
            },
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs)?,
            ExprKind::Compare { op, lhs, rhs } => {
                let (lhs, rhs) = (self.eval(lhs)?, self.eval(rhs)?);
                let (sign, zero) = match (lhs, rhs) {
                    (Value::Int(a), Value::Int(b)) => {
                        let difference = a.wrapping_sub(b) as i16;
                        (difference < 0, difference == 0)
                    }
                    (Value::Vf(a), Value::Vf(b)) => {
                        let difference = bits(float(a[0]) - float(b[0]));
                        (difference >> 31 != 0, difference & 0x7fff_ffff == 0)
                    }
                    (a, b) => return Err(format!("comparing {a:?} with {b:?}")),
                };
                Value::Bool(match op {
                    CompareOp::Less => sign,
                    CompareOp::LessEqual => sign || zero,
                    CompareOp::Greater => !(sign || zero),
                    CompareOp::GreaterEqual => !sign,
                    CompareOp::Equal => zero,
                    CompareOp::NotEqual => !zero,
                })
            }
            ExprKind::Not(operand) => Value::Bool(!self.condition(operand)?),
            ExprKind::And(lhs, rhs) => Value::Bool(self.condition(lhs)? && self.condition(rhs)?),
            ExprKind::Or(lhs, rhs) => Value::Bool(self.condition(lhs)? || self.condition(rhs)?),
            ExprKind::Call { builtin, args } => self.call(*builtin, args)?,
            ExprKind::Construct(args) => {
                let mut components = Vec::new();
                for arg in args {
                    components.extend(self.vf(arg)?);
                }
                let count = expr.ty.vf_lanes().unwrap_or(1);
                if components.len() == 1 {
                    components = vec![components[0]; count];
                }
                Value::Vf(components)
            }
            ExprKind::Convert {
                conversion,
                operand,
            } => self.convert(*conversion, operand)?,
            ExprKind::Column { matrix, index } => Value::Vf(self.mat(matrix)?[*index].clone()),
        })
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Run<Value> {
        Ok(match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Min | BinOp::Max => {
                let (a, b) = (self.vf(lhs)?, self.vf(rhs)?);
                Value::Vf(match op {
                    BinOp::Add => Self::zip(&a, &b, |x, y| x + y),
                    BinOp::Sub => Self::zip(&a, &b, |x, y| x - y),
                    BinOp::Mul => Self::zip(&a, &b, |x, y| x * y),
                    BinOp::Min => Self::zip(&a, &b, f32::min),
                    _ => Self::zip(&a, &b, f32::max),
                })
            }
            BinOp::Div => {
                let (a, b) = (self.vf(lhs)?, self.vf(rhs)?);
                if a.len() == 1 {
                    Value::Vf(vec![bits(float(a[0]) / float(b[0]))])
                } else {
                    let reciprocal = 1.0 / float(b[0]);
                    Value::Vf(a.iter().map(|&c| bits(float(c) * reciprocal)).collect())
                }
            }
            BinOp::IntAdd | BinOp::IntSub | BinOp::IntAnd | BinOp::IntOr => {
                let (a, b) = (self.int(lhs)?, self.int(rhs)?);
                Value::Int(match op {
                    BinOp::IntAdd => a.wrapping_add(b),
                    BinOp::IntSub => a.wrapping_sub(b),
                    BinOp::IntAnd => a & b,
                    _ => a | b,
                })
            }
            BinOp::MatVec => {
                let qwords = self.mat(lhs)?;
                let weights = self.vf(rhs)?;
                Value::Vf(Self::weighted_sum(&qwords, &weights))
            }
            BinOp::VecMat => {
                let weights = self.vf(lhs)?;
                let qwords = self.mat(rhs)?;
                Value::Vf(Self::weighted_sum(&qwords, &weights))
            }
            BinOp::MatMat => {
                let (a, b) = (self.mat(lhs)?, self.mat(rhs)?);
                let row_major = matches!(
                    lhs.ty,
                    Type::Mat {
                        row_major: true,
                        ..
                    }
                );
                let (sums, weights) = if row_major { (b, a) } else { (a, b) };
                Value::Mat(
                    weights
                        .iter()
                        .map(|w| Self::weighted_sum(&sums, w))
                        .collect(),
                )
            }
            BinOp::ScaleMat => {
                let (scalar, matrix) = if lhs.ty == Type::Float {
                    (lhs, rhs)
                } else {
                    (rhs, lhs)
                };
                let scale = float(self.vf(scalar)?[0]);
                let qwords = self.mat(matrix)?;
                Value::Mat(
                    qwords
                        .iter()
                        .map(|qword| qword.iter().map(|&c| bits(float(c) * scale)).collect())
                        .collect(),
                )
            }
        })
    }

    fn call(&mut self, builtin: Builtin, args: &[Expr]) -> Run<Value> {
        let scalar = |value: f32| Value::Vf(vec![bits(value)]);
        Ok(match builtin {
            Builtin::Dot => {
                let (a, b) = (self.vf(&args[0])?, self.vf(&args[1])?);
                scalar(Self::dot(&a, &b))
            }
            Builtin::Cross => {
                let (a, b) = (self.vf(&args[0])?, self.vf(&args[1])?);
                let (a, b) = (
                    a.iter().map(|&c| float(c)).collect::<Vec<_>>(),
                    b.iter().map(|&c| float(c)).collect::<Vec<_>>(),
                );
                Value::Vf(
                    [
                        a[1] * b[2] - b[1] * a[2],
                        a[2] * b[0] - b[2] * a[0],
                        a[0] * b[1] - b[0] * a[1],
                    ]
                    .into_iter()
                    .map(bits)
                    .collect(),
                )
            }
            Builtin::Normalize => {
                let value = self.vf(&args[0])?;
                let scale = 1.0 / Self::dot(&value, &value).sqrt();
                Value::Vf(value.iter().map(|&c| bits(float(c) * scale)).collect())
            }
            Builtin::Length => {
                let value = self.vf(&args[0])?;
                scalar(Self::dot(&value, &value).sqrt())
            }
            Builtin::Sqrt => scalar(float(self.vf(&args[0])?[0]).sqrt()),
            Builtin::Rsqrt => scalar(1.0 / float(self.vf(&args[0])?[0]).sqrt()),
            Builtin::Abs => Value::Vf(
                self.vf(&args[0])?
                    .iter()
                    .map(|&c| bits(float(c).abs()))
                    .collect(),
            ),
            Builtin::Clamp => {
                let value = self.vf(&args[0])?;
                let low = self.vf(&args[1])?;
                let high = self.vf(&args[2])?;
                let raised = Self::zip(&value, &low, f32::max);
                Value::Vf(Self::zip(&raised, &high, f32::min))
            }
            Builtin::ClipW => {
                let judge = self.vf(&args[0])?;
                let limit = float(self.vf(&args[1])?[0]).abs();
                let mut flags = 0;
                for (lane, &component) in judge.iter().take(3).enumerate() {
                    if float(component) > limit {
                        flags |= 1 << (2 * lane);
                    }
                    if float(component) < -limit {
                        flags |= 2 << (2 * lane);
                    }
                }
                self.machine.clip = ((self.machine.clip << 6) | flags) & 0xff_ffff;
                self.machine.judgements += 1;
                Value::Clip(self.machine.judgements - 1)
            }
            Builtin::Any | Builtin::Adc => {
                let mut judgements = Vec::new();
                for arg in args {
                    judgements.push(self.eval(arg)?);
                }
                let mask = self.judgement_mask(&judgements)?;
                // FCAND sets VI01 to 1 when any flag in the mask is set.
                let any = self.machine.clip & mask != 0;
                if builtin == Builtin::Any {
                    Value::Bool(any)
                } else {
                    Value::Int(0x7fff + u16::from(any))
                }
            }
            Builtin::Esadd | Builtin::Ersadd | Builtin::Eleng | Builtin::Erleng => {
                let value = self.vf(&args[0])?;
                let squares = Self::dot(&value, &value);
                scalar(match builtin {
                    Builtin::Esadd => squares,
                    Builtin::Ersadd => 1.0 / squares,
                    Builtin::Eleng => squares.sqrt(),
                    _ => 1.0 / squares.sqrt(),
                })
            }
            Builtin::Esum => {
                let value = self.vf(&args[0])?;
                scalar(value.iter().map(|&c| float(c)).sum())
            }
            Builtin::Ercpr => scalar(1.0 / float(self.vf(&args[0])?[0])),
            Builtin::Esqrt => scalar(float(self.vf(&args[0])?[0]).sqrt()),
            Builtin::Ersqrt => scalar(1.0 / float(self.vf(&args[0])?[0]).sqrt()),
            Builtin::Esin => scalar(float(self.vf(&args[0])?[0]).sin()),
            Builtin::Eatan => scalar(float(self.vf(&args[0])?[0]).atan()),
            Builtin::Eexp => scalar((-float(self.vf(&args[0])?[0])).exp()),
            Builtin::Xitop => Value::Int(self.machine.itop),
        })
    }

    fn convert(&mut self, conversion: Conversion, operand: &Expr) -> Run<Value> {
        Ok(match conversion {
            Conversion::FloatToFixed(fraction) => {
                let scale = f64::from(1_u32 << fraction);
                Value::Vf(
                    self.vf(operand)?
                        .iter()
                        .map(|&c| {
                            (f64::from(float(c)) * scale)
                                .trunc()
                                .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                                as i32 as u32
                        })
                        .collect(),
                )
            }
            Conversion::FixedToFloat(fraction) => {
                let scale = f64::from(1_u32 << fraction);
                Value::Vf(
                    self.vf(operand)?
                        .iter()
                        .map(|&c| bits((f64::from(c as i32) / scale) as f32))
                        .collect(),
                )
            }
            Conversion::IntToLane => Value::Vf(vec![self.int(operand)? as i16 as i32 as u32]),
            Conversion::IntToFloat => Value::Vf(vec![bits(f32::from(self.int(operand)? as i16))]),
            Conversion::LaneToInt => Value::Int((self.vf(operand)?[0] & 0xffff) as u16),
            Conversion::FloatToInt => {
                let value = float(self.vf(operand)?[0]).trunc() as i32;
                Value::Int((value as u32 & 0xffff) as u16)
            }
            Conversion::Bits => self.eval(operand)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sema::types::Length;

    #[test]
    fn lanes_follow_memory() {
        assert_eq!(lanes(&Type::Vec(3), Lane::Y), [1, 2, 3]);
        assert_eq!(lanes(&Type::Qword, Lane::X), [0, 1, 2, 3]);
        assert_eq!(
            lanes(
                &Type::Array(Box::new(Type::Float), Length::Fixed(2)),
                Lane::X
            ),
            [0]
        );
    }
}
