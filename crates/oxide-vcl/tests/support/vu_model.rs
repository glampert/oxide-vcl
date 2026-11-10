//! Bounded, test-only VU microassembly model derived from SCE's VU User's Manual
//! version 6.0. This is an interlocked logical model, not a cycle-accurate VU.
//! Floating arithmetic is restricted to exactly representable finite results;
//! hardware rounding, flags, exceptional values and EFU approximations are absent.

use std::collections::{HashMap, HashSet};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub vf: [[u32; 4]; 32],
    pub vi: [u16; 16],
    pub acc: [u32; 4],
    pub q: u32,
    pub p: u32,
    pub i: u32,
    /// VU1 data memory, addressed in 16-byte qwords. Addresses outside this
    /// explicitly modeled region are errors, rather than guessed aliases.
    pub memory: Vec<[u32; 4]>,
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
            memory: vec![[0; 4]; 1024],
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

    pub fn execute(&self, mut state: State, max_steps: usize) -> Result<Execution> {
        if state.vf[0] != [0, 0, 0, 1.0_f32.to_bits()] || state.vi[0] != 0 {
            return Err("invalid constant register state".into());
        }
        let mut pc = 0;
        let mut delayed_branch = None;
        let mut end_delay = false;
        for step in 0..max_steps {
            let (upper, lower) = self
                .pairs
                .get(pc)
                .ok_or_else(|| format!("PC {pc} left program before E termination"))?;
            // SCE §3.4.2: both slots issue together. All reads use preissue
            // values, including a lower MTIR/SQ reading an upper destination.
            let high = self.evaluate(upper, &state, pc, true)?;
            let low = self.evaluate(lower, &state, pc, false)?;
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
            low.commit(&mut state, &high_writes);
            high.commit(&mut state, &HashSet::new());
            if end_delay {
                return Ok(Execution {
                    state,
                    steps: step + 1,
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
    ) -> Result<Effect> {
        let mut effect = Effect::default();
        let name = inst.name.as_str();
        let op = |n| inst.operand(n);
        if name == "nop" {
            return Ok(effect);
        }
        if upper {
            let (base, broadcast) = arithmetic_name(name);
            if matches!(
                base,
                "abs"
                    | "add"
                    | "sub"
                    | "mul"
                    | "max"
                    | "mini"
                    | "adda"
                    | "suba"
                    | "mula"
                    | "madd"
                    | "msub"
                    | "madda"
                    | "msuba"
            ) {
                let source = vector(state, op(1)?)?;
                let other = if base == "abs" {
                    [0; 4]
                } else {
                    vector_operand(state, op(2)?, broadcast)?
                };
                let mut result = [0; 4];
                for lane in 0..4 {
                    if inst.mask & (1 << lane) == 0 {
                        continue;
                    }
                    let a = normal(source[lane])?;
                    let b = normal(other[lane])?;
                    let value = match base {
                        "abs" => a.abs() as f64,
                        "add" | "adda" => a as f64 + b as f64,
                        "sub" | "suba" => a as f64 - b as f64,
                        "mul" | "mula" => a as f64 * b as f64,
                        "max" => a.max(b) as f64,
                        "mini" => a.min(b) as f64,
                        _ => {
                            let product = exact(a as f64 * b as f64)?;
                            let accumulator = normal(state.acc[lane])?;
                            if matches!(base, "madd" | "madda") {
                                accumulator as f64 + f32::from_bits(product) as f64
                            } else {
                                accumulator as f64 - f32::from_bits(product) as f64
                            }
                        }
                    };
                    result[lane] = exact(value)?;
                }
                if op(0)? == "acc" {
                    effect.acc = Some((inst.mask, result));
                } else {
                    effect
                        .vf
                        .push((register(op(0)?, "vf", 32)?, inst.mask, result));
                }
                return Ok(effect);
            }
            return Err(format!("unsupported upper instruction {name}"));
        }
        if inst.end {
            return Err("E modifier in lower slot".into());
        }
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
            "b" | "ibeq" | "ibne" | "ibltz" | "ibgtz" | "iblez" | "ibgez" => {
                let (taken, target_operand) = match name {
                    "b" => (true, 0),
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
                // Keep even untaken branches pending to reject a branch in
                // its delay slot and execute precisely one physical next pair.
                effect.branch = Some(if taken {
                    *self.labels.get(op(target_operand)?).ok_or_else(|| {
                        format!("unknown branch label {}", op(target_operand).unwrap_or("?"))
                    })?
                } else {
                    pc + 2
                });
            }
            "lq" | "sq" | "lqi" | "lqd" | "sqi" | "sqd" | "ilw" | "isw" => {
                effect.memory_access = true;
                let (address, update) = address(state, op(1)?, name)?;
                let memory = *state
                    .memory
                    .get(address)
                    .ok_or_else(|| format!("unmodeled memory qword {address:#x}"))?;
                if let Some(update) = update {
                    effect.vi.push(update);
                }
                if name.starts_with('l') {
                    effect
                        .vf
                        .push((register(op(0)?, "vf", 32)?, inst.mask, memory));
                } else if name.starts_with('s') {
                    effect
                        .memory
                        .push((address, inst.mask, vector(state, op(0)?)?));
                } else if name == "ilw" {
                    if inst.mask.count_ones() != 1 {
                        return Err("ILW needs exactly one field".into());
                    }
                    let lane = inst.mask.trailing_zeros() as usize;
                    effect
                        .vi
                        .push((register(op(0)?, "vi", 16)?, memory[lane] as u16));
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
                let a = normal(component(state, op(1)?)?)? as f64;
                let value = if name == "sqrt" {
                    if a < 0.0 {
                        return Err("negative SQRT is outside model".into());
                    }
                    a.sqrt()
                } else {
                    let b = normal(component(state, op(2)?)?)? as f64;
                    if b <= 0.0 {
                        return Err("nonpositive divisor is outside model".into());
                    }
                    a / if name == "rsqrt" { b.sqrt() } else { b }
                };
                effect.q = Some(exact(value)?);
            }
            "mfp" => effect
                .vf
                .push((register(op(0)?, "vf", 32)?, inst.mask, [state.p; 4])),
            "loi" => {
                let value = op(0)?.parse::<f32>().map_err(|_| "invalid LOI literal")?;
                normal(value.to_bits())?;
                effect.i = Some(value.to_bits());
            }
            "waitq" | "waitp" => {}
            _ => return Err(format!("unsupported lower instruction {name}")),
        }
        Ok(effect)
    }
}

#[derive(Default)]
struct Effect {
    vf: Vec<(usize, u8, [u32; 4])>,
    vi: Vec<(usize, u16)>,
    memory: Vec<(usize, u8, [u32; 4])>,
    acc: Option<(u8, [u32; 4])>,
    q: Option<u32>,
    i: Option<u32>,
    branch: Option<usize>,
    memory_access: bool,
}

impl Effect {
    fn commit(&self, state: &mut State, discarded_vf: &HashSet<usize>) {
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
        if let Some(q) = self.q {
            state.q = q;
        }
        if let Some(i) = self.i {
            state.i = i;
        }
    }
}

fn masked(destination: &mut [u32; 4], mask: u8, source: [u32; 4]) {
    for lane in 0..4 {
        if mask & (1 << lane) != 0 {
            destination[lane] = source[lane];
        }
    }
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

fn register(text: &str, prefix: &str, limit: usize) -> Result<usize> {
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
        let p = Program::parse(&assembly(&[("clipw.xyz VF01,VF02", "NOP")])).unwrap();
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
}
