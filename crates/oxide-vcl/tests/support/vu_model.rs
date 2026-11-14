//! Bounded, test-only VU microassembly model derived from SCE's VU User's Manual
//! version 6.0 (docs/TESTING.md).
//!
//! It runs in one of two configurations:
//!
//! - [`Config::LOGICAL`], the default: an interlocked logical model whose
//!   arithmetic is restricted to exactly representable finite results.
//!   Every result is visible to the next pair.
//! - [`Config::TIMED`]: cycle by cycle, for comparing two listings of one
//!   program. Pairs stall on VF fields still in the FMAC pipeline, on VI
//!   values from ILW, and on a busy FDIV or EFU; Q, P and the MAC and clip
//!   flags are not interlocked and change when their latency has passed.
//!   Arithmetic is deterministic f32 clamped to the VU's range, not the
//!   VU's exact rounding, and addresses wrap around data memory.
// Several test crates include this file, and each uses part of it.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

pub type Result<T> = std::result::Result<T, String>;

/// How arithmetic is evaluated.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Arithmetic {
    /// Finite normal values from 2^-16 to 2^16, and results exactly
    /// representable in that range; anything else is an error.
    #[default]
    Exact,
    /// Any value: f32 arithmetic (each product rounded before an
    /// accumulation), with the VU's range: no infinity, NaN or denormal, so
    /// overflow saturates and underflow flushes to zero. Deterministic, but
    /// not the VU's rounding, so two listings compare only with each other.
    Deterministic,
}

/// What the model simulates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Config {
    pub arithmetic: Arithmetic,
    /// Count cycles, with the interlocks and landing times described in
    /// the module comment.
    pub timed: bool,
    /// Wrap addresses into data memory, as VU1 does, instead of rejecting
    /// addresses outside it.
    pub wrap_memory: bool,
    /// End a run that passes the last pair as if an E bit ended it, instead
    /// of failing: a listing whose E bit vcl 1.4beta7 deleted runs on into
    /// whatever follows it in micro memory.
    pub fall_off_ends: bool,
}

impl Config {
    pub const LOGICAL: Config = Config {
        arithmetic: Arithmetic::Exact,
        timed: false,
        wrap_memory: false,
        fall_off_ends: false,
    };
    pub const TIMED: Config = Config {
        arithmetic: Arithmetic::Deterministic,
        timed: true,
        wrap_memory: true,
        fall_off_ends: true,
    };
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub vf: [[u32; 4]; 32],
    pub vi: [u16; 16],
    pub acc: [u32; 4],
    pub q: u32,
    pub p: u32,
    pub i: u32,
    /// The MAC flag: overflow, underflow, sign and zero nibbles (bits 15-12,
    /// 11-8, 7-4, 3-0), x the highest bit of each.
    pub mac: u16,
    /// The status flag: Z, S, U, O, I and D (bits 0-5) and their sticky
    /// copies (bits 6-11).
    pub status: u16,
    /// The clip flag: the last four judgments, 6 bits each, newest lowest.
    pub clip: u32,
    /// The R register of RINIT, RGET, RNEXT and RXOR.
    pub r: u32,
    /// What XTOP and XITOP read.
    pub top: u16,
    pub itop: u16,
    /// VU1 data memory, addressed in 16-byte qwords. Addresses outside it
    /// are errors unless [`Config::wrap_memory`] wraps them.
    pub memory: Vec<[u32; 4]>,
    /// Each XGKICK: its address and the GIF packet it sends, read from
    /// memory when it issues.
    pub kicks: Vec<(u16, Vec<[u32; 4]>)>,
}

impl Default for State {
    fn default() -> Self {
        let mut state = Self {
            vf: [[0; 4]; 32],
            vi: [0; 16],
            acc: [0; 4],
            q: 0,
            p: 0,
            i: 0,
            mac: 0,
            status: 0,
            clip: 0,
            r: 0,
            top: 0,
            itop: 0,
            memory: vec![[0; 4]; 1024],
            kicks: Vec::new(),
        };
        state.vf[0][3] = 1.0_f32.to_bits();
        state
    }
}

impl State {
    pub fn set_vector(&mut self, register: usize, vector: [f32; 4]) {
        assert!(register > 0 && register < 32);
        self.vf[register] = vector.map(f32::to_bits);
    }
}

#[derive(Clone, Debug)]
struct Instruction {
    name: String,
    mask: u8,
    operands: Vec<String>,
    end: bool,
}

impl Instruction {
    fn parse(text: &str) -> Result<Self> {
        let mut split = text.splitn(2, char::is_whitespace);
        let raw = split.next().ok_or("missing opcode")?.to_ascii_lowercase();
        let mut spelling = String::new();
        let mut modifiers = false;
        let mut end = false;
        for c in raw.chars() {
            match c {
                '[' if !modifiers => modifiers = true,
                ']' if modifiers => modifiers = false,
                'e' if modifiers => end = true,
                // The D, T and M bits change nothing the model observes.
                'd' | 't' | 'm' if modifiers => {}
                _ if modifiers => return Err(format!("unsupported instruction modifier {raw}")),
                _ => spelling.push(c),
            }
        }
        if modifiers {
            return Err(format!("unterminated modifier {raw}"));
        }
        let (name, mask) = if let Some((name, mask)) = spelling.split_once('.') {
            (name.to_owned(), component_mask(mask)?)
        } else {
            (spelling, 15)
        };
        let operands = split
            .next()
            .unwrap_or("")
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split_whitespace()
                    .collect::<String>()
                    .to_ascii_lowercase()
            })
            .collect();
        Ok(Self {
            name,
            mask,
            operands,
            end,
        })
    }

    fn operand(&self, index: usize) -> Result<&str> {
        self.operands
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("{} missing operand {index}", self.name))
    }

    /// What the instruction waits for before it can issue: the VF fields
    /// and VI registers it reads (interlocked), the unit it occupies, and
    /// WAITQ/WAITP. Malformed operands are left to [`Program::evaluate`].
    fn needs(&self, upper: bool) -> Needs {
        let mut needs = Needs::default();
        let name = self.name.as_str();
        let op = |n: usize| self.operand(n).unwrap_or("");
        let mut vf = |text: &str, mask: u8| {
            if let Ok(register) = register(text, "vf", 32) {
                needs.vf.push((register, mask));
            }
        };
        if upper {
            let (base, broadcast) = arithmetic_name(name);
            if ARITHMETIC.contains(&base) {
                vf(op(1), self.mask);
                if base != "abs" {
                    match broadcast {
                        Some(field @ ('x' | 'y' | 'z' | 'w')) => vf(op(2), field_bit(field)),
                        Some(_) => {}
                        None => vf(op(2), self.mask),
                    }
                }
            } else if name == "clipw" {
                vf(op(0), 0b0111);
                vf(op(1), 0b1000);
            } else if matches!(name, "opmula" | "opmsub") {
                vf(op(1), 0b0111);
                vf(op(2), 0b0111);
            } else if conversion(name).is_some() {
                vf(op(1), self.mask);
            }
            return needs;
        }
        let mut vi = |text: &str| {
            if let Ok(register) = register(text, "vi", 16) {
                needs.vi.push(register);
            }
        };
        match name {
            "iadd" | "isub" | "iand" | "ior" => {
                vi(op(1));
                vi(op(2));
            }
            "iaddi" | "iaddiu" | "isubiu" | "mfir" | "fmand" | "fmor" | "fmeq" => vi(op(1)),
            "ibeq" | "ibne" => {
                vi(op(0));
                vi(op(1));
            }
            "ibltz" | "ibgtz" | "iblez" | "ibgez" | "jr" | "xgkick" => vi(op(0)),
            "jalr" => vi(op(1)),
            "lq" | "lqi" | "lqd" | "ilw" | "ilwr" => vi(&address_base(op(1))),
            "sq" | "sqi" | "sqd" => {
                vi(&address_base(op(1)));
                vf(op(0), self.mask);
            }
            "isw" | "iswr" => {
                vi(&address_base(op(1)));
                vi(op(0));
            }
            "move" => vf(op(1), self.mask),
            // MR32 writes field n from field n+1.
            "mr32" => vf(op(1), ((self.mask >> 1) | (self.mask << 3)) & 15),
            "mtir" => scalar_need(op(1), &mut vf),
            "div" | "rsqrt" => {
                scalar_need(op(1), &mut vf);
                scalar_need(op(2), &mut vf);
            }
            "sqrt" | "rinit" | "rxor" => scalar_need(op(1), &mut vf),
            "waitq" => needs.wait_q = true,
            "waitp" => needs.wait_p = true,
            _ => {}
        }
        if let Some((throughput, _)) = fdiv_timing(name) {
            needs.fdiv = Some(throughput);
        }
        if let Some((throughput, _)) = efu_timing(name) {
            needs.efu = Some(throughput);
            if matches!(name, "esadd" | "ersadd" | "eleng" | "erleng") {
                vf(op(1), 0b0111);
            } else if name == "esum" {
                vf(op(1), 0b1111);
            } else if name == "eatanxy" {
                vf(op(1), 0b0011);
            } else if name == "eatanxz" {
                vf(op(1), 0b0101);
            } else {
                scalar_need(op(1), &mut vf);
            }
        }
        needs
    }
}

/// What [`Instruction::needs`] finds.
#[derive(Default)]
struct Needs {
    vf: Vec<(usize, u8)>,
    vi: Vec<usize>,
    fdiv: Option<u64>,
    efu: Option<u64>,
    wait_q: bool,
    wait_p: bool,
}

fn scalar_need(text: &str, vf: &mut impl FnMut(&str, u8)) {
    if let Some(field) = text.chars().last().filter(|c| "xyzw".contains(*c)) {
        vf(text, field_bit(field));
    }
}

const ARITHMETIC: [&str; 13] = [
    "abs", "add", "sub", "mul", "max", "mini", "adda", "suba", "mula", "madd", "msub", "madda",
    "msuba",
];

/// FDIV throughput and latency (VU User's Manual 6.0, section 7.4).
fn fdiv_timing(name: &str) -> Option<(u64, u64)> {
    match name {
        "div" | "sqrt" => Some((7, 7)),
        "rsqrt" => Some((13, 13)),
        _ => None,
    }
}

/// EFU throughput and latency; the latency is one more.
fn efu_timing(name: &str) -> Option<(u64, u64)> {
    let throughput = match name {
        "eatan" | "eatanxy" | "eatanxz" => 53,
        "eexp" => 43,
        "eleng" | "ersadd" | "ersqrt" => 17,
        "ercpr" | "esqrt" | "esum" => 11,
        "erleng" => 23,
        "esadd" => 10,
        "esin" => 28,
        _ => return None,
    };
    Some((throughput, throughput + 1))
}

/// FTOIn and ITOFn: whether it is FTOI, and its fraction bits.
fn conversion(name: &str) -> Option<(bool, u32)> {
    let (to_integer, bits) = if let Some(bits) = name.strip_prefix("ftoi") {
        (true, bits)
    } else {
        (false, name.strip_prefix("itof")?)
    };
    match bits {
        "0" => Some((to_integer, 0)),
        "4" => Some((to_integer, 4)),
        "12" => Some((to_integer, 12)),
        "15" => Some((to_integer, 15)),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Program {
    pairs: Vec<(Instruction, Instruction)>,
    labels: HashMap<String, usize>,
}

#[derive(Debug)]
pub struct Execution {
    pub state: State,
    pub steps: usize,
    /// Cycles from the first issue to the end of the E delay slot,
    /// stalls included ([`Config::timed`]); the pair count otherwise.
    pub cycles: u64,
    /// For each pair of the program: how often it issued, and the stall
    /// cycles before those issues.
    pub profile: Vec<(u64, u64)>,
}

/// A non-interlocked result that changes its register or flag later.
#[derive(Clone, Copy, Debug)]
enum Late {
    Q(u32),
    P(u32),
    Mac(u16),
    ClipJudgment(u32),
    ClipSet(u32),
    /// New status bits under a mask; their sticky copies are set too.
    Status {
        mask: u16,
        bits: u16,
    },
    /// FSSET: the sticky bits.
    StatusSticky(u16),
}

/// Timing state of [`Config::timed`].
#[derive(Default)]
struct Pipeline {
    vf_ready: [[u64; 4]; 32],
    vi_ready: [u64; 16],
    fdiv_free: u64,
    efu_free: u64,
    q_landing: u64,
    p_landing: u64,
    /// Landing cycle, issue order, change.
    pending: Vec<(u64, usize, Late)>,
    issued: usize,
}

impl Pipeline {
    /// The cycle at which a pair can issue, at the earliest `earliest`.
    fn issue_cycle(&self, needs: [&Needs; 2], earliest: u64) -> u64 {
        let mut cycle = earliest;
        for needs in needs {
            for &(register, mask) in &needs.vf {
                if register == 0 {
                    continue;
                }
                for (lane, ready) in self.vf_ready[register].iter().enumerate() {
                    if mask & (1 << lane) != 0 {
                        cycle = cycle.max(*ready);
                    }
                }
            }
            for &register in &needs.vi {
                cycle = cycle.max(self.vi_ready[register]);
            }
            if needs.fdiv.is_some() {
                cycle = cycle.max(self.fdiv_free);
            }
            if needs.efu.is_some() {
                cycle = cycle.max(self.efu_free);
            }
            if needs.wait_q {
                cycle = cycle.max(self.q_landing);
            }
            if needs.wait_p {
                cycle = cycle.max(self.p_landing);
            }
        }
        cycle
    }

    fn defer(&mut self, cycle: u64, late: Late) {
        self.pending.push((cycle, self.issued, late));
        self.issued += 1;
    }

    /// Applies every change that has landed by `cycle`, in landing order.
    fn land(&mut self, cycle: u64, state: &mut State) {
        let mut landed: Vec<_> = self
            .pending
            .iter()
            .copied()
            .filter(|(at, _, _)| *at <= cycle)
            .collect();
        if landed.is_empty() {
            return;
        }
        self.pending.retain(|(at, _, _)| *at > cycle);
        landed.sort_by_key(|(at, order, _)| (*at, *order));
        for (_, _, late) in landed {
            late.apply(state);
        }
    }
}

impl Late {
    fn apply(self, state: &mut State) {
        match self {
            Late::Q(value) => state.q = value,
            Late::P(value) => state.p = value,
            Late::Mac(value) => state.mac = value,
            Late::ClipJudgment(judgment) => {
                state.clip = ((state.clip << 6) | judgment) & 0xff_ffff;
            }
            Late::ClipSet(value) => state.clip = value & 0xff_ffff,
            Late::Status { mask, bits } => {
                state.status = (state.status & !mask) | bits | (bits << 6);
            }
            Late::StatusSticky(value) => state.status = (state.status & 0x3f) | (value & 0xfc0),
        }
    }
}

impl Program {
    /// Parse the fixed-width pair format emitted by both original VCL and the
    /// reconstruction. Labels resolve case-insensitively; directives/comments
    /// are presentation only. Unknown non-pair text is rejected.
    pub fn parse(assembly: &str) -> Result<Self> {
        let mut pairs = Vec::new();
        let mut labels = HashMap::new();
        for (line_index, line) in assembly.lines().enumerate() {
            let line = line.split(';').next().unwrap_or("").trim_end();
            let text = line.trim();
            if text.is_empty() || text.starts_with('.') {
                continue;
            }
            // `-m`'s DMA and VIF wrapping around the program.
            let first = text.split_whitespace().next().unwrap_or("");
            if ["dmaret", "dmacnt", "dmaend", "mpg", "vifnop", "mscal"]
                .iter()
                .any(|word| first.eq_ignore_ascii_case(word))
            {
                continue;
            }
            if let Some(label) = text.strip_suffix(':') {
                if labels
                    .insert(label.to_ascii_lowercase(), pairs.len())
                    .is_some()
                {
                    return Err(format!("duplicate label {label}"));
                }
                continue;
            }
            let leading = line.len() - line.trim_start().len();
            let lower_column = leading + 59;
            if leading < 1 || line.len() <= lower_column || !line.is_ascii() {
                return Err(format!(
                    "unrecognized assembly line {}: {text}",
                    line_index + 1
                ));
            }
            pairs.push((
                Instruction::parse(line[..lower_column].trim())?,
                Instruction::parse(line[lower_column..].trim())?,
            ));
        }
        if pairs.is_empty() {
            return Err("no instruction pairs".into());
        }
        Ok(Self { pairs, labels })
    }

    /// The labels at each pair, in pair order.
    pub fn labels(&self) -> Vec<(usize, String)> {
        let mut labels: Vec<_> = self
            .labels
            .iter()
            .map(|(name, &pc)| (pc, name.clone()))
            .collect();
        labels.sort();
        labels
    }

    /// Runs in [`Config::LOGICAL`] from the first pair.
    pub fn execute(&self, state: State, max_steps: usize) -> Result<Execution> {
        self.run(state, Config::LOGICAL, max_steps)
    }

    /// Runs from the first pair until the pair after an E bit, for at most
    /// `max_steps` pairs.
    pub fn run(&self, mut state: State, config: Config, max_steps: usize) -> Result<Execution> {
        if state.vf[0] != [0, 0, 0, 1.0_f32.to_bits()] || state.vi[0] != 0 {
            return Err("invalid constant register state".into());
        }
        let mut pipeline = Pipeline::default();
        let mut profile = vec![(0, 0); self.pairs.len()];
        let mut next_cycle = 0;
        let mut pc = 0;
        let mut delayed_branch = None;
        let mut end_delay = false;
        for step in 0..max_steps {
            let Some((upper, lower)) = self.pairs.get(pc) else {
                if config.fall_off_ends && pc == self.pairs.len() && delayed_branch.is_none() {
                    pipeline.land(u64::MAX, &mut state);
                    return Ok(Execution {
                        state,
                        steps: step,
                        cycles: next_cycle,
                        profile,
                    });
                }
                return Err(format!("PC {pc} left program before E termination"));
            };
            let cycle = if config.timed {
                let cycle =
                    pipeline.issue_cycle([&upper.needs(true), &lower.needs(false)], next_cycle);
                pipeline.land(cycle, &mut state);
                cycle
            } else {
                next_cycle
            };
            profile[pc].0 += 1;
            profile[pc].1 += cycle - next_cycle;
            // SCE §3.4.2: both slots issue together. All reads use preissue
            // values, including a lower MTIR/SQ reading an upper destination.
            let high = self.evaluate(upper, &state, pc, true, config)?;
            let low = self.evaluate(lower, &state, pc, false, config)?;
            if upper.end && (end_delay || low.branch.is_some()) {
                return Err("E in E delay slot or on a branch pair".into());
            }
            if delayed_branch.is_some() && low.branch.is_some() {
                return Err("branch in branch delay slot".into());
            }
            if end_delay && (low.branch.is_some() || low.memory_access) {
                return Err(
                    "unsupported/forbidden branch or memory instruction in E delay slot".into(),
                );
            }
            // SCE §3.4.3: upper wins for the entire VF register, even when
            // upper/lower destination masks are disjoint.
            let high_writes: HashSet<_> = high.vf.iter().map(|(r, _, _)| *r).collect();
            let timing = config.timed.then_some((&mut pipeline, cycle));
            low.commit(&mut state, &high_writes, timing);
            let timing = config.timed.then_some((&mut pipeline, cycle));
            high.commit(&mut state, &HashSet::new(), timing);
            next_cycle = cycle + 1;
            if end_delay {
                // Results in flight still complete after termination.
                pipeline.land(u64::MAX, &mut state);
                return Ok(Execution {
                    state,
                    steps: step + 1,
                    cycles: next_cycle,
                    profile,
                });
            }
            let next = delayed_branch.take().unwrap_or(pc + 1);
            delayed_branch = low.branch;
            end_delay = upper.end;
            pc = next;
        }
        Err(format!("execution exceeded {max_steps} pairs"))
    }

    fn evaluate(
        &self,
        inst: &Instruction,
        state: &State,
        pc: usize,
        upper: bool,
        config: Config,
    ) -> Result<Effect> {
        let mut effect = Effect::default();
        let name = inst.name.as_str();
        let op = |n| inst.operand(n);
        let arithmetic = config.arithmetic;
        if name == "nop" {
            return Ok(effect);
        }
        if upper {
            let (base, broadcast) = arithmetic_name(name);
            if ARITHMETIC.contains(&base) {
                let source = vector(state, op(1)?)?;
                let other = if base == "abs" {
                    [0; 4]
                } else {
                    vector_operand(state, op(2)?, broadcast)?
                };
                let mut result = [0; 4];
                let mut flags = [0; 4];
                for lane in 0..4 {
                    if inst.mask & (1 << lane) == 0 {
                        continue;
                    }
                    let a = input(source[lane], arithmetic)?;
                    let b = input(other[lane], arithmetic)?;
                    let value = match base {
                        "abs" => a.abs() as f64,
                        "add" | "adda" => a as f64 + b as f64,
                        "sub" | "suba" => a as f64 - b as f64,
                        "mul" | "mula" => a as f64 * b as f64,
                        "max" => a.max(b) as f64,
                        "mini" => a.min(b) as f64,
                        _ => {
                            let product = output(a as f64 * b as f64, arithmetic)?.0;
                            let accumulator = input(state.acc[lane], arithmetic)? as f64;
                            let product = f32::from_bits(product) as f64;
                            if matches!(base, "madd" | "madda") {
                                accumulator + product
                            } else {
                                accumulator - product
                            }
                        }
                    };
                    (result[lane], flags[lane]) = output(value, arithmetic)?;
                }
                // MAX, MINI and ABS leave the MAC flag alone (§3.3.5).
                if !matches!(base, "abs" | "max" | "mini") {
                    effect.mac = Some(mac_flag(inst.mask, result, flags));
                }
                effect.write_vector(op(0)?, inst.mask, result)?;
                return Ok(effect);
            }
            if matches!(name, "opmula" | "opmsub") {
                let a = vector(state, op(1)?)?;
                let b = vector(state, op(2)?)?;
                let mut result = [0; 4];
                let mut flags = [0; 4];
                for lane in 0..3 {
                    let (first, second) = ((lane + 1) % 3, (lane + 2) % 3);
                    let product =
                        input(a[first], arithmetic)? as f64 * input(b[second], arithmetic)? as f64;
                    let value = if name == "opmula" {
                        product
                    } else {
                        let product = f32::from_bits(output(product, arithmetic)?.0) as f64;
                        input(state.acc[lane], arithmetic)? as f64 - product
                    };
                    (result[lane], flags[lane]) = output(value, arithmetic)?;
                }
                effect.mac = Some(mac_flag(0b0111, result, flags));
                effect.write_vector(op(0)?, 0b0111, result)?;
                return Ok(effect);
            }
            if name == "clipw" {
                let value = vector(state, op(0)?)?;
                let limit = input(component(state, op(1)?)?, arithmetic)?.abs();
                let mut judgment = 0;
                for (lane, &bits) in value.iter().take(3).enumerate() {
                    let field = input(bits, arithmetic)?;
                    if field > limit {
                        judgment |= 1 << (2 * lane);
                    }
                    if field < -limit {
                        judgment |= 2 << (2 * lane);
                    }
                }
                effect.clip = Some(Late::ClipJudgment(judgment));
                return Ok(effect);
            }
            if let Some((to_integer, fraction)) = conversion(name) {
                let source = vector(state, op(1)?)?;
                let scale = (1_u32 << fraction) as f64;
                let mut result = [0; 4];
                for lane in 0..4 {
                    if inst.mask & (1 << lane) == 0 {
                        continue;
                    }
                    result[lane] = if to_integer {
                        let value = input(source[lane], arithmetic)? as f64 * scale;
                        if arithmetic == Arithmetic::Exact && value.fract() != 0.0 {
                            return Err(format!("inexact {name} outside exact model"));
                        }
                        value.trunc().clamp(i32::MIN as f64, i32::MAX as f64) as i32 as u32
                    } else {
                        output(source[lane] as i32 as f64 / scale, arithmetic)?.0
                    };
                }
                effect
                    .vf
                    .push((register(op(0)?, "vf", 32)?, inst.mask, result));
                return Ok(effect);
            }
            return Err(format!("unsupported upper instruction {name}"));
        }
        if inst.end {
            return Err("E modifier in lower slot".into());
        }
        let label = |text: &str| {
            self.labels
                .get(text)
                .copied()
                .ok_or_else(|| format!("unknown branch label {text}"))
        };
        match name {
            "iadd" | "isub" | "iand" | "ior" | "iaddi" | "iaddiu" | "isubiu" => {
                let destination = register(op(0)?, "vi", 16)?;
                let a = state.vi[register(op(1)?, "vi", 16)?];
                let b = if matches!(name, "iaddi" | "iaddiu" | "isubiu") {
                    integer(op(2)?)? as u16
                } else {
                    state.vi[register(op(2)?, "vi", 16)?]
                };
                let value = match name {
                    "isub" | "isubiu" => a.wrapping_sub(b),
                    "iand" => a & b,
                    "ior" => a | b,
                    _ => a.wrapping_add(b),
                };
                effect.vi.push((destination, value));
            }
            "b" | "bal" | "ibeq" | "ibne" | "ibltz" | "ibgtz" | "iblez" | "ibgez" => {
                let (taken, target_operand) = match name {
                    "b" => (true, 0),
                    "bal" => (true, 1),
                    "ibeq" | "ibne" => {
                        let equal = state.vi[register(op(0)?, "vi", 16)?]
                            == state.vi[register(op(1)?, "vi", 16)?];
                        (equal == (name == "ibeq"), 2)
                    }
                    _ => {
                        let a = state.vi[register(op(0)?, "vi", 16)?] as i16;
                        (
                            match name {
                                "ibltz" => a < 0,
                                "ibgtz" => a > 0,
                                "iblez" => a <= 0,
                                _ => a >= 0,
                            },
                            1,
                        )
                    }
                };
                if name == "bal" {
                    // The link is the address after the delay slot, in
                    // 8-byte instruction pairs: the model's pair index.
                    effect
                        .vi
                        .push((register(op(0)?, "vi", 16)?, (pc + 2) as u16));
                }
                // Keep even untaken branches pending to reject a branch in
                // its delay slot and execute precisely one physical next pair.
                effect.branch = Some(if taken {
                    label(op(target_operand)?)?
                } else {
                    pc + 2
                });
            }
            "jr" | "jalr" => {
                let target = if name == "jr" { op(0)? } else { op(1)? };
                let target = usize::from(state.vi[register(target, "vi", 16)?]);
                if name == "jalr" {
                    effect
                        .vi
                        .push((register(op(0)?, "vi", 16)?, (pc + 2) as u16));
                }
                effect.branch = Some(target);
            }
            "lq" | "sq" | "lqi" | "lqd" | "sqi" | "sqd" | "ilw" | "isw" | "ilwr" | "iswr" => {
                effect.memory_access = true;
                let (address, update) = address(state, op(1)?, name)?;
                let address = memory_index(state, address, config)?;
                let memory = state.memory[address];
                if let Some(update) = update {
                    effect.vi.push(update);
                }
                if name.starts_with("lq") {
                    effect
                        .vf
                        .push((register(op(0)?, "vf", 32)?, inst.mask, memory));
                } else if name.starts_with("sq") {
                    effect
                        .memory
                        .push((address, inst.mask, vector(state, op(0)?)?));
                } else if name.starts_with("ilw") {
                    if inst.mask.count_ones() != 1 {
                        return Err("ILW needs exactly one field".into());
                    }
                    let lane = inst.mask.trailing_zeros() as usize;
                    effect
                        .vi
                        .push((register(op(0)?, "vi", 16)?, memory[lane] as u16));
                    effect.vi_latency = 4;
                } else {
                    let value = state.vi[register(op(0)?, "vi", 16)?] as u32;
                    effect.memory.push((address, inst.mask, [value; 4]));
                }
            }
            "move" | "mr32" => {
                let value = vector(state, op(1)?)?;
                let value = if name == "mr32" {
                    [value[1], value[2], value[3], value[0]]
                } else {
                    value
                };
                effect
                    .vf
                    .push((register(op(0)?, "vf", 32)?, inst.mask, value));
            }
            "mtir" => effect.vi.push((
                register(op(0)?, "vi", 16)?,
                component(state, op(1)?)? as u16,
            )),
            "mfir" => {
                let value = state.vi[register(op(1)?, "vi", 16)?] as i16 as i32 as u32;
                effect
                    .vf
                    .push((register(op(0)?, "vf", 32)?, inst.mask, [value; 4]));
            }
            "div" | "sqrt" | "rsqrt" => {
                if op(0)? != "q" {
                    return Err("divider destination must be Q".into());
                }
                let a = input(component(state, op(1)?)?, arithmetic)? as f64;
                // Status I: 0/0 or the root of a negative number; D: x/0.
                let (value, invalid, by_zero) = if name == "sqrt" {
                    (root(a, arithmetic)?, a < 0.0, false)
                } else {
                    let b = input(component(state, op(2)?)?, arithmetic)? as f64;
                    let divisor = if name == "rsqrt" {
                        root(b, arithmetic)?
                    } else {
                        b
                    };
                    let zero = divisor == 0.0;
                    let invalid = (zero && a == 0.0) || (name == "rsqrt" && b < 0.0);
                    (divide(a, divisor, arithmetic)?, invalid, zero && a != 0.0)
                };
                effect.divide_status = Some(u16::from(invalid) << 4 | u16::from(by_zero) << 5);
                effect.q = Some(output(value, arithmetic)?.0);
                effect.unit_latency = fdiv_timing(name).map_or(0, |(_, latency)| latency);
            }
            _ if efu_timing(name).is_some() => {
                if op(0)? != "p" {
                    return Err("EFU destination must be P".into());
                }
                let source = op(1)?;
                let lanes = |count: usize| -> Result<Vec<f64>> {
                    let value = vector(state, source)?;
                    (0..count)
                        .map(|lane| input(value[lane], arithmetic).map(f64::from))
                        .collect()
                };
                let squares = || -> Result<f64> { Ok(lanes(3)?.iter().map(|v| v * v).sum()) };
                let value = match name {
                    "esadd" => squares()?,
                    "ersadd" => divide(1.0, squares()?, arithmetic)?,
                    "eleng" => root(squares()?, arithmetic)?,
                    "erleng" => divide(1.0, root(squares()?, arithmetic)?, arithmetic)?,
                    "esum" => lanes(4)?.iter().sum(),
                    "eatanxy" | "eatanxz" => {
                        let value = lanes(3)?;
                        let other = if name == "eatanxy" {
                            value[1]
                        } else {
                            value[2]
                        };
                        other.atan2(value[0])
                    }
                    _ => {
                        let scalar = input(component(state, source)?, arithmetic)? as f64;
                        match name {
                            "esqrt" => root(scalar, arithmetic)?,
                            "ersqrt" => divide(1.0, root(scalar, arithmetic)?, arithmetic)?,
                            "ercpr" => divide(1.0, scalar, arithmetic)?,
                            "eatan" => scalar.atan(),
                            "eexp" => (-scalar).exp(),
                            _ => scalar.sin(),
                        }
                    }
                };
                effect.p = Some(output(value, arithmetic)?.0);
                effect.unit_latency = efu_timing(name).map_or(0, |(_, latency)| latency);
            }
            "mfp" => effect
                .vf
                .push((register(op(0)?, "vf", 32)?, inst.mask, [state.p; 4])),
            "loi" => {
                let text = op(0)?;
                let bits = if let Some(hex) = text.strip_prefix("0x") {
                    u32::from_str_radix(hex, 16).map_err(|_| "invalid LOI literal")?
                } else {
                    text.parse::<f32>()
                        .map_err(|_| "invalid LOI literal")?
                        .to_bits()
                };
                input(bits, arithmetic)?;
                effect.i = Some(bits);
            }
            "fsand" | "fsor" | "fseq" => {
                let value = integer(op(1)?)? as u16 & 0xfff;
                let result = match name {
                    "fsand" => state.status & value,
                    "fsor" => state.status | value,
                    _ => u16::from(state.status == value),
                };
                effect.vi.push((register(op(0)?, "vi", 16)?, result));
            }
            "fsset" => effect.status_sticky = Some(integer(op(0)?)? as u16),
            "rinit" | "rxor" => {
                let bits = component(state, op(1)?)? & 0x7f_ffff;
                effect.r = Some(if name == "rinit" {
                    0x3f80_0000 | bits
                } else {
                    state.r ^ bits
                });
            }
            "rget" | "rnext" => {
                // RNEXT steps a 23-bit shift register; the model's feedback
                // is a deterministic stand-in for the hardware's.
                let r = if name == "rnext" {
                    let mantissa = state.r & 0x7f_ffff;
                    let feedback = ((mantissa >> 22) ^ (mantissa >> 4)) & 1;
                    0x3f80_0000 | (((mantissa << 1) | feedback) & 0x7f_ffff)
                } else {
                    state.r
                };
                if name == "rnext" {
                    effect.r = Some(r);
                }
                effect
                    .vf
                    .push((register(op(0)?, "vf", 32)?, inst.mask, [r; 4]));
            }
            "fcand" | "fcor" | "fceq" => {
                if op(0)? != "vi01" {
                    return Err(format!("{name} writes VI01"));
                }
                let value = integer(op(1)?)? as u32 & 0xff_ffff;
                let clip = state.clip & 0xff_ffff;
                let result = match name {
                    "fcand" => clip & value != 0,
                    "fcor" => clip | value == 0xff_ffff,
                    _ => clip == value,
                };
                effect.vi.push((1, u16::from(result)));
            }
            "fcget" => effect
                .vi
                .push((register(op(0)?, "vi", 16)?, (state.clip & 0xfff) as u16)),
            "fcset" => effect.clip = Some(Late::ClipSet(integer(op(0)?)? as u32)),
            "fmand" | "fmor" | "fmeq" => {
                let source = state.vi[register(op(1)?, "vi", 16)?];
                let value = match name {
                    "fmand" => source & state.mac,
                    "fmor" => source | state.mac,
                    _ => u16::from(source == state.mac),
                };
                effect.vi.push((register(op(0)?, "vi", 16)?, value));
            }
            "xtop" => effect.vi.push((register(op(0)?, "vi", 16)?, state.top)),
            "xitop" => effect.vi.push((register(op(0)?, "vi", 16)?, state.itop)),
            "xgkick" => {
                effect.memory_access = true;
                let start = state.vi[register(op(0)?, "vi", 16)?];
                effect.kick = Some((start, gif_packet(state, start, config)?));
            }
            "waitq" | "waitp" => {}
            _ => return Err(format!("unsupported lower instruction {name}")),
        }
        Ok(effect)
    }
}

/// The GIF packet at `start`: tags and their data up to the tag with EOP
/// (GS User's Manual, GIFtag: NLOOP bits 0-14, EOP bit 15, FLG bits 58-59,
/// NREG bits 60-63), at most the whole memory.
fn gif_packet(state: &State, start: u16, config: Config) -> Result<Vec<[u32; 4]>> {
    let limit = state.memory.len();
    let mut packet = Vec::new();
    let mut address = usize::from(start);
    while packet.len() < limit {
        let tag = state.memory[memory_index(state, address, config)?];
        packet.push(tag);
        address += 1;
        let loops = (tag[0] & 0x7fff) as usize;
        let registers = match (tag[1] >> 28) as usize {
            0 => 16,
            count => count,
        };
        let data = match (tag[1] >> 26) & 3 {
            0 => loops * registers,
            1 => (loops * registers).div_ceil(2),
            _ => loops,
        };
        for _ in 0..data.min(limit - packet.len()) {
            packet.push(state.memory[memory_index(state, address, config)?]);
            address += 1;
        }
        if tag[0] & 0x8000 != 0 {
            break;
        }
    }
    Ok(packet)
}

fn memory_index(state: &State, address: usize, config: Config) -> Result<usize> {
    if config.wrap_memory {
        Ok(address % state.memory.len())
    } else if address < state.memory.len() {
        Ok(address)
    } else {
        Err(format!("unmodeled memory qword {address:#x}"))
    }
}

#[derive(Default)]
struct Effect {
    vf: Vec<(usize, u8, [u32; 4])>,
    vi: Vec<(usize, u16)>,
    /// The cycles before a VI result can be read: 4 for ILW, 1 otherwise.
    vi_latency: u64,
    memory: Vec<(usize, u8, [u32; 4])>,
    acc: Option<(u8, [u32; 4])>,
    q: Option<u32>,
    p: Option<u32>,
    /// The latency of the Q or P result.
    unit_latency: u64,
    i: Option<u32>,
    r: Option<u32>,
    mac: Option<u16>,
    /// I and D from FDIV, landing with Q.
    divide_status: Option<u16>,
    clip: Option<Late>,
    status_sticky: Option<u16>,
    kick: Option<(u16, Vec<[u32; 4]>)>,
    branch: Option<usize>,
    memory_access: bool,
}

/// FMAC results, and the MAC and clip flags, land after 4 cycles.
const FMAC_LATENCY: u64 = 4;

impl Effect {
    fn write_vector(&mut self, destination: &str, mask: u8, value: [u32; 4]) -> Result<()> {
        if destination.trim_end_matches(['x', 'y', 'z', 'w']) == "acc" {
            self.acc = Some((mask, value));
        } else {
            self.vf
                .push((register(destination, "vf", 32)?, mask, value));
        }
        Ok(())
    }

    /// Commits the effect: at once, or with `timing` the issue cycle from
    /// which non-interlocked results land and interlocked ones are ready.
    fn commit(
        &self,
        state: &mut State,
        discarded_vf: &HashSet<usize>,
        timing: Option<(&mut Pipeline, u64)>,
    ) {
        for (register, mask, value) in &self.vf {
            if *register != 0 && !discarded_vf.contains(register) {
                masked(&mut state.vf[*register], *mask, *value);
            }
        }
        for (register, value) in &self.vi {
            if *register != 0 {
                state.vi[*register] = *value;
            }
        }
        for (address, mask, value) in &self.memory {
            masked(&mut state.memory[*address], *mask, *value);
        }
        if let Some((mask, value)) = self.acc {
            masked(&mut state.acc, mask, value);
        }
        if let Some(i) = self.i {
            state.i = i;
        }
        if let Some(r) = self.r {
            state.r = r;
        }
        if let Some(kick) = &self.kick {
            state.kicks.push(kick.clone());
        }
        let late = [
            self.q.map(|value| (Late::Q(value), self.unit_latency)),
            self.p.map(|value| (Late::P(value), self.unit_latency)),
            self.mac.map(|value| (Late::Mac(value), FMAC_LATENCY)),
            self.mac.map(|value| (mac_status(value), FMAC_LATENCY)),
            self.divide_status.map(|bits| {
                let change = Late::Status {
                    mask: 0b11_0000,
                    bits,
                };
                (change, self.unit_latency)
            }),
            self.clip.map(|change| (change, FMAC_LATENCY)),
            self.status_sticky
                .map(|value| (Late::StatusSticky(value), FMAC_LATENCY)),
        ];
        let Some((pipeline, cycle)) = timing else {
            for (change, _) in late.into_iter().flatten() {
                change.apply(state);
            }
            return;
        };
        for (register, mask, _) in &self.vf {
            for (lane, ready) in pipeline.vf_ready[*register].iter_mut().enumerate() {
                if mask & (1 << lane) != 0 {
                    *ready = cycle + FMAC_LATENCY;
                }
            }
        }
        for (register, _) in &self.vi {
            pipeline.vi_ready[*register] = cycle + self.vi_latency.max(1);
        }
        for (change, latency) in late.into_iter().flatten() {
            match change {
                Late::Q(_) => {
                    pipeline.fdiv_free = cycle + fdiv_throughput(latency);
                    pipeline.q_landing = cycle + latency;
                }
                Late::P(_) => {
                    pipeline.efu_free = cycle + latency - 1;
                    pipeline.p_landing = cycle + latency;
                }
                _ => {}
            }
            pipeline.defer(cycle + latency, change);
        }
    }
}

/// FDIV throughput equals its latency (7 or 13).
fn fdiv_throughput(latency: u64) -> u64 {
    latency
}

/// The status bits an FMAC result sets: Z, S, U and O when any field has
/// them in the MAC flag.
fn mac_status(mac: u16) -> Late {
    let mut bits = 0;
    for (index, nibble) in [0, 4, 8, 12].into_iter().enumerate() {
        if (mac >> nibble) & 0xf != 0 {
            bits |= 1 << index;
        }
    }
    Late::Status { mask: 0b1111, bits }
}

/// The MAC flag for a result: zero and sign from the bits, underflow and
/// overflow from [`output`]'s flags, for the fields in `mask`.
fn mac_flag(mask: u8, result: [u32; 4], flags: [u8; 4]) -> u16 {
    let mut mac = 0;
    for lane in 0..4 {
        if mask & (1 << lane) == 0 {
            continue;
        }
        let bit = 3 - lane;
        if result[lane] & 0x7fff_ffff == 0 {
            mac |= 1 << bit;
        }
        if result[lane] >> 31 != 0 {
            mac |= 1 << (bit + 4);
        }
        if flags[lane] & UNDERFLOW != 0 {
            mac |= 1 << (bit + 8);
        }
        if flags[lane] & OVERFLOW != 0 {
            mac |= 1 << (bit + 12);
        }
    }
    mac
}

fn masked(destination: &mut [u32; 4], mask: u8, source: [u32; 4]) {
    for lane in 0..4 {
        if mask & (1 << lane) != 0 {
            destination[lane] = source[lane];
        }
    }
}

fn field_bit(field: char) -> u8 {
    1 << "xyzw".find(field).unwrap_or(0)
}

fn component_mask(text: &str) -> Result<u8> {
    let mut mask = 0;
    for c in text.chars() {
        let lane = "xyzw"
            .find(c)
            .ok_or_else(|| format!("invalid component mask {text}"))?;
        if mask & (1 << lane) != 0 {
            return Err(format!("duplicate component {text}"));
        }
        mask |= 1 << lane;
    }
    if mask == 0 {
        return Err("empty component mask".into());
    }
    Ok(mask)
}

/// A register operand; a VF operand may carry field letters (`vf16xyz`).
fn register(text: &str, prefix: &str, limit: usize) -> Result<usize> {
    let text = if prefix == "vf" {
        text.trim_end_matches(['x', 'y', 'z', 'w'])
    } else {
        text
    };
    text.strip_prefix(prefix)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|r| *r < limit)
        .ok_or_else(|| format!("invalid {prefix} register {text}"))
}

fn vector(state: &State, text: &str) -> Result<[u32; 4]> {
    Ok(state.vf[register(text, "vf", 32)?])
}

fn component(state: &State, text: &str) -> Result<u32> {
    let text = text.replace('.', "");
    let (register_text, field) = text.split_at(text.len().checked_sub(1).ok_or("empty component")?);
    let lane = "xyzw"
        .find(field)
        .ok_or_else(|| format!("expected scalar VF component {text}"))?;
    Ok(state.vf[register(register_text, "vf", 32)?][lane])
}

fn vector_operand(state: &State, text: &str, broadcast: Option<char>) -> Result<[u32; 4]> {
    let scalar = match broadcast {
        Some('i') if text == "i" => Some(state.i),
        Some('q') if text == "q" => Some(state.q),
        Some(field) if "xyzw".contains(field) => {
            let value = if text.ends_with(field) {
                component(state, text)?
            } else {
                vector(state, text)?["xyzw".find(field).unwrap()]
            };
            Some(value)
        }
        None => None,
        _ => return Err(format!("invalid scalar operand {text}")),
    };
    if let Some(value) = scalar {
        Ok([value; 4])
    } else {
        vector(state, text)
    }
}

fn arithmetic_name(name: &str) -> (&str, Option<char>) {
    for base in [
        "madda", "msuba", "adda", "suba", "mula", "madd", "msub", "mini", "add", "sub", "mul",
        "max", "abs",
    ] {
        if name == base {
            return (base, None);
        }
        if let Some(suffix) = name.strip_prefix(base)
            && suffix.len() == 1
            && "xyzwqi".contains(suffix)
        {
            return (base, suffix.chars().next());
        }
    }
    (name, None)
}

fn integer(text: &str) -> Result<i64> {
    if let Some(hex) = text.strip_prefix("0x") {
        i64::from_str_radix(hex, 16).map_err(|_| format!("invalid integer {text}"))
    } else if let Some(hex) = text.strip_prefix("-0x") {
        i64::from_str_radix(hex, 16)
            .map(|n| -n)
            .map_err(|_| format!("invalid integer {text}"))
    } else {
        text.parse()
            .map_err(|_| format!("unresolved integer expression {text}"))
    }
}

/// The base register of a memory operand: `3(vi04)`, `(vi02++)`, `(--vi01)`.
fn address_base(text: &str) -> String {
    text.split_once('(')
        .map(|(_, rest)| {
            rest.trim_end_matches(')')
                .replace("++", "")
                .replace("--", "")
        })
        .unwrap_or_default()
}

fn address(state: &State, text: &str, opcode: &str) -> Result<(usize, Option<(usize, u16)>)> {
    let (offset, rest) = text
        .split_once('(')
        .ok_or_else(|| format!("invalid address {text}"))?;
    let base = rest
        .strip_suffix(')')
        .ok_or_else(|| format!("invalid address {text}"))?;
    if matches!(opcode, "lqi" | "sqi" | "lqd" | "sqd") {
        let decrement = opcode.ends_with('d');
        let register_text = if decrement {
            base.strip_prefix("--")
        } else {
            base.strip_suffix("++")
        }
        .ok_or_else(|| format!("invalid updated address {text}"))?;
        if !offset.is_empty() {
            return Err(format!("offset on updated address {text}"));
        }
        let register = register(register_text, "vi", 16)?;
        let old = state.vi[register];
        let next = if decrement {
            old.wrapping_sub(1)
        } else {
            old.wrapping_add(1)
        };
        Ok((
            if decrement { next } else { old } as usize,
            Some((register, next)),
        ))
    } else {
        let base = state.vi[register(base, "vi", 16)?] as i64;
        let offset = if offset.is_empty() {
            0
        } else {
            integer(offset)?
        };
        Ok(((base + offset) as u16 as usize, None))
    }
}

/// [`output`]'s flags.
const UNDERFLOW: u8 = 1;
const OVERFLOW: u8 = 2;

/// An operand's value. [`Arithmetic::Deterministic`] reads the VU's way:
/// a zero exponent is zero, and the all-ones exponent, which the VU reads
/// as a large number, saturates.
fn input(bits: u32, arithmetic: Arithmetic) -> Result<f32> {
    match arithmetic {
        Arithmetic::Exact => normal(bits),
        Arithmetic::Deterministic => {
            let sign = bits & 0x8000_0000;
            Ok(match (bits >> 23) & 0xff {
                0 => f32::from_bits(sign),
                0xff => f32::from_bits(sign | f32::MAX.to_bits()),
                _ => f32::from_bits(bits),
            })
        }
    }
}

/// A result's bits and its underflow and overflow flags.
fn output(value: f64, arithmetic: Arithmetic) -> Result<(u32, u8)> {
    match arithmetic {
        Arithmetic::Exact => Ok((exact(value)?, 0)),
        Arithmetic::Deterministic => {
            let rounded = value as f32;
            let sign = if value.is_sign_negative() {
                0x8000_0000
            } else {
                0
            };
            Ok(if value.is_nan() || !rounded.is_finite() {
                (sign | f32::MAX.to_bits(), OVERFLOW)
            } else if value != 0.0 && (rounded == 0.0 || rounded.is_subnormal()) {
                (sign, UNDERFLOW)
            } else {
                (rounded.to_bits(), 0)
            })
        }
    }
}

/// A square root: the VU takes the root of the magnitude.
fn root(value: f64, arithmetic: Arithmetic) -> Result<f64> {
    if value < 0.0 && arithmetic == Arithmetic::Exact {
        return Err("negative SQRT is outside model".into());
    }
    Ok(value.abs().sqrt())
}

/// A quotient: division by zero saturates.
fn divide(numerator: f64, divisor: f64, arithmetic: Arithmetic) -> Result<f64> {
    if divisor <= 0.0 && arithmetic == Arithmetic::Exact {
        return Err("nonpositive divisor is outside model".into());
    }
    Ok(if divisor == 0.0 {
        f64::from(f32::MAX).copysign(numerator)
    } else {
        numerator / divisor
    })
}

fn normal(bits: u32) -> Result<f32> {
    let value = f32::from_bits(bits);
    if value == 0.0 || (value.is_normal() && (1.0 / 65536.0..=65536.0).contains(&value.abs())) {
        Ok(value)
    } else {
        Err(format!(
            "floating value outside small-normal exact model {bits:#010x}"
        ))
    }
}

fn exact(value: f64) -> Result<u32> {
    let rounded = value as f32;
    normal(rounded.to_bits())?;
    if rounded as f64 != value {
        return Err(format!("floating rounding outside exact model: {value}"));
    }
    Ok(rounded.to_bits())
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn assembly(pairs: &[(&str, &str)]) -> String {
        pairs
            .iter()
            .map(|(u, l)| format!("         {u:59}{l}\n"))
            .collect()
    }

    #[test]
    fn paired_reads_precede_writes_and_upper_register_priority_is_whole_register() {
        let mut state = State::default();
        state.set_vector(1, [2.0; 4]);
        state.set_vector(2, [1.0; 4]);
        let p = Program::parse(&assembly(&[
            ("add[E].x VF02,VF01,VF01", "move.y VF02,VF01"),
            ("NOP", "mtir VI01,VF02y"),
        ]))
        .unwrap();
        let final_state = p.execute(state, 10).unwrap().state;
        assert_eq!(
            final_state.vf[2],
            [
                4.0_f32.to_bits(),
                1.0_f32.to_bits(),
                1.0_f32.to_bits(),
                1.0_f32.to_bits()
            ]
        );
        assert_eq!(final_state.vi[1], 0);
        let mut state = State::default();
        state.vf[2] = [0x1234; 4];
        let p = Program::parse(&assembly(&[
            ("add[E] VF02,VF00,VF00", "mtir VI01,VF02x"),
            ("NOP", "NOP"),
        ]))
        .unwrap();
        assert_eq!(p.execute(state, 10).unwrap().state.vi[1], 0x1234);
    }

    #[test]
    fn taken_and_untaken_branches_execute_one_delay_pair() {
        let text = format!(
            "{}target:\n\
             {}",
            assembly(&[
                ("NOP", "ibeq VI01,VI00,target"),
                ("NOP", "iaddiu VI02,VI02,1"),
                ("NOP", "iaddiu VI02,VI02,8")
            ]),
            assembly(&[
                ("NOP[E]", "iaddiu VI02,VI02,2"),
                ("NOP", "iaddiu VI02,VI02,4")
            ])
        );
        let p = Program::parse(&text).unwrap();
        let mut state = State::default();
        assert_eq!(p.execute(state.clone(), 20).unwrap().state.vi[2], 7);
        state.vi[1] = 1;
        assert_eq!(p.execute(state, 20).unwrap().state.vi[2], 15);
    }

    #[test]
    fn integer_and_constant_register_writes_wrap_and_are_ignored_respectively() {
        let mut state = State::default();
        state.vi[1] = 65535;
        let p = Program::parse(&assembly(&[
            ("add[E] VF00,VF00,VF00", "iaddiu VI01,VI01,2"),
            ("NOP", "isubiu VI00,VI00,1"),
        ]))
        .unwrap();
        let final_state = p.execute(state, 10).unwrap().state;
        assert_eq!(final_state.vi[1], 1);
        assert_eq!(final_state.vi[0], 0);
        assert_eq!(final_state.vf[0], [0, 0, 0, 1.0_f32.to_bits()]);
    }

    #[test]
    fn predecrement_and_postincrement_use_correct_qword_and_mask() {
        let mut state = State::default();
        state.vi[1] = 6;
        state.memory[5] = [1, 2, 3, 4];
        state.vf[1] = [10, 11, 12, 13];
        let p = Program::parse(&assembly(&[
            ("NOP", "lqd.xy VF02,(--VI01)"),
            ("NOP", "sqi.zw VF01,(VI01++)"),
            ("NOP[E]", "NOP"),
            ("NOP", "NOP"),
        ]))
        .unwrap();
        let final_state = p.execute(state, 10).unwrap().state;
        assert_eq!(final_state.vf[2], [1, 2, 0, 0]);
        assert_eq!(final_state.memory[5], [1, 2, 12, 13]);
        assert_eq!(final_state.vi[1], 6);
    }

    #[test]
    fn scalar_q_p_i_and_broadcast_preserve_same_pair_old_value_reads() {
        let mut state = State::default();
        state.set_vector(1, [4.0; 4]);
        state.set_vector(2, [2.0; 4]);
        state.q = 0.5_f32.to_bits();
        state.p = 2.0_f32.to_bits();
        state.i = 1.0_f32.to_bits();
        let p = Program::parse(&assembly(&[
            ("mulq VF03,VF01,Q", "div Q,VF01x,VF02y"),
            ("addi VF07,VF01,I", "loi 2.0"),
            ("addi VF08,VF01,I", "waitq"),
            ("mulx VF09,VF01,VF02x", "NOP"),
            ("mulq[E] VF04,VF01,Q", "mfp VF05,P"),
            ("abs VF06,VF05", "NOP"),
        ]))
        .unwrap();
        let output = p.execute(state, 20).unwrap().state;
        for (register, expected) in [
            (3, 2.0_f32),
            (4, 8.0),
            (5, 2.0),
            (6, 2.0),
            (7, 5.0),
            (8, 6.0),
            (9, 8.0),
        ] {
            assert_eq!(output.vf[register], [expected.to_bits(); 4]);
        }
        assert_eq!(output.q, 2.0_f32.to_bits());
    }

    #[test]
    fn rounding_exceptions_unsupported_ops_and_unbounded_execution_are_errors() {
        assert!(exact(1.0 + 2.0_f64.powi(-25)).is_err());
        assert!(normal(f32::NAN.to_bits()).is_err());
        assert!(normal(1).is_err());
        let p = Program::parse(&assembly(&[("ftoi7 VF01,VF02", "NOP")])).unwrap();
        assert!(
            p.execute(State::default(), 4)
                .unwrap_err()
                .contains("unsupported upper")
        );
        let p = Program::parse(&format!(
            "loop:\n\
             {}",
            assembly(&[("NOP", "b loop"), ("NOP", "NOP")])
        ))
        .unwrap();
        assert!(
            p.execute(State::default(), 4)
                .unwrap_err()
                .contains("exceeded")
        );
    }

    fn timed(pairs: &[(&str, &str)], state: State) -> Execution {
        Program::parse(&assembly(pairs))
            .unwrap()
            .run(state, Config::TIMED, 64)
            .unwrap()
    }

    #[test]
    fn timed_fmac_results_stall_their_readers_field_by_field() {
        let mut state = State::default();
        state.set_vector(1, [2.0; 4]);
        // The second ADD reads VF03x, three cycles away: it waits until
        // cycle 4. The MUL reads only VF04y, which nothing writes.
        let execution = timed(
            &[
                ("add.x VF03,VF01,VF01", "NOP"),
                ("add.x VF04,VF03,VF03", "NOP"),
                ("mul[E].y VF05,VF04,VF01", "NOP"),
                ("NOP", "NOP"),
            ],
            state,
        );
        assert_eq!(execution.state.vf[4][0], 8.0_f32.to_bits());
        assert_eq!(execution.cycles, 7);
        let untimed = Program::parse(&assembly(&[
            ("add.x VF03,VF01,VF01", "NOP"),
            ("add.x[E] VF04,VF03,VF03", "NOP"),
            ("NOP", "NOP"),
        ]))
        .unwrap();
        let mut state = State::default();
        state.set_vector(1, [2.0; 4]);
        assert_eq!(untimed.execute(state, 8).unwrap().cycles, 3);
    }

    #[test]
    fn timed_q_lands_after_its_latency_and_waitq_waits_for_it() {
        let mut state = State::default();
        state.set_vector(1, [6.0; 4]);
        state.set_vector(2, [3.0; 4]);
        state.q = 0.5_f32.to_bits();
        let nop = ("NOP", "NOP");
        // DIV issues at cycle 0, so Q changes at cycle 7: the MULQ at
        // cycle 6 reads the old Q, the one at cycle 7 the quotient.
        let execution = timed(
            &[
                ("NOP", "div Q,VF01x,VF02x"),
                nop,
                nop,
                nop,
                nop,
                nop,
                ("mulq VF03,VF01,Q", "NOP"),
                ("mulq[E] VF04,VF01,Q", "NOP"),
                nop,
            ],
            state.clone(),
        );
        assert_eq!(execution.state.vf[3], [3.0_f32.to_bits(); 4]);
        assert_eq!(execution.state.vf[4], [12.0_f32.to_bits(); 4]);
        // WAITQ stalls until cycle 7; a second DIV waits for the unit.
        let execution = timed(
            &[
                ("NOP", "div Q,VF01x,VF02x"),
                ("NOP", "waitq"),
                ("mulq VF03,VF01,Q", "div Q,VF02x,VF01x"),
                ("NOP[E]", "div Q,VF01x,VF01x"),
                nop,
            ],
            state,
        );
        assert_eq!(execution.state.vf[3], [12.0_f32.to_bits(); 4]);
        // The third DIV issues at cycle 15, its delay slot at 16.
        assert_eq!(execution.cycles, 17);
        assert_eq!(execution.state.q, 1.0_f32.to_bits());
    }

    #[test]
    fn timed_flags_change_four_cycles_after_issue() {
        let mut state = State::default();
        state.set_vector(1, [1.0, -2.0, 3.0, 0.5]);
        state.vi[2] = 0xffff;
        let nop = ("NOP", "NOP");
        let execution = timed(
            &[
                ("sub.x VF03,VF01,VF01", "NOP"),
                ("clipw.xyz VF01xyz,VF01w", "fmand VI03,VI02"),
                nop,
                nop,
                ("NOP", "fmand VI04,VI02"),
                ("NOP", "fcand VI01,0x3f"),
                ("NOP[E]", "fcget VI05"),
                nop,
            ],
            state,
        );
        // Old MAC at cycle 1; Zx (bit 3) from cycle 4.
        assert_eq!(execution.state.vi[3], 0);
        assert_eq!(execution.state.vi[4], 0x8);
        // x = 1 > 0.5, y = -2 < -0.5, z = 3 > 0.5: bits 0, 3 and 4.
        assert_eq!(execution.state.vi[1], 1);
        assert_eq!(execution.state.vi[5], 0b01_1001);
        assert_eq!(execution.state.mac, 0x8);
    }

    #[test]
    fn timed_ilw_results_are_interlocked() {
        let mut state = State::default();
        state.memory[3] = [5, 0, 0, 0];
        let execution = timed(
            &[
                ("NOP", "ilw.x VI01,3(VI00)"),
                ("NOP", "iaddiu VI02,VI01,1"),
                ("NOP[E]", "NOP"),
                ("NOP", "NOP"),
            ],
            state,
        );
        assert_eq!(execution.state.vi[2], 6);
        assert_eq!(execution.cycles, 7);
    }

    #[test]
    fn xgkick_records_the_packet_up_to_eop() {
        let mut state = State::default();
        state.vi[1] = 1022;
        // PACKED, NLOOP 1, NREG 2, no EOP; then IMAGE, NLOOP 1, EOP.
        state.memory[1022] = [1, 2 << 28, 0, 0];
        state.memory[1023] = [10, 0, 0, 0];
        state.memory[0] = [11, 0, 0, 0];
        state.memory[1] = [0x8001, 2 << 26, 0, 0];
        state.memory[2] = [12, 0, 0, 0];
        state.memory[3] = [13, 0, 0, 0];
        let execution = timed(
            &[("NOP", "xgkick VI01"), ("NOP[E]", "NOP"), ("NOP", "NOP")],
            state,
        );
        let (address, packet) = &execution.state.kicks[0];
        assert_eq!(*address, 1022);
        assert_eq!(
            packet.iter().map(|qword| qword[0]).collect::<Vec<_>>(),
            [1, 10, 11, 0x8001, 12]
        );
    }

    #[test]
    fn deterministic_arithmetic_saturates_and_converts() {
        let mut state = State::default();
        state.set_vector(1, [f32::MAX, 1.5, -2.75, 1e-30]);
        state.set_vector(2, [2.0, 3.0, 4.0, 1e-30]);
        let execution = timed(
            &[
                ("mul VF03,VF01,VF02", "NOP"),
                ("ftoi4 VF04,VF01", "NOP"),
                ("itof4[E] VF05,VF04", "NOP"),
                ("NOP", "NOP"),
            ],
            state,
        );
        let product = execution.state.vf[3].map(f32::from_bits);
        assert_eq!(product, [f32::MAX, 4.5, -11.0, 0.0]);
        assert_eq!(execution.state.vf[4][1..3], [24, (-44_i32) as u32]);
        assert_eq!(execution.state.vf[4][0], i32::MAX as u32);
        assert_eq!(
            execution.state.vf[5][1..3],
            [1.5_f32.to_bits(), (-2.75_f32).to_bits()]
        );
        // Overflow in x, sign in z, underflow and zero in w.
        assert_eq!(execution.state.mac, 1 << 15 | 1 << 8 | 1 << 5 | 1);
    }
}
