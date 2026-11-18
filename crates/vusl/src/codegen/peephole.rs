//! Passes over the finished instructions, for what lowering one
//! expression at a time leaves behind. VCL schedules and allocates what it
//! is given but propagates no copies, so a move left in the source costs a
//! slot.
//!
//! - A temporary written by one instruction and then moved whole into a
//!   register is written there directly: `mul.xyz t, a, b` and
//!   `move.xyz r, t` become `mul.xyz r, a, b`.
//! - A copy whose destination is only read after it, while its source
//!   keeps its value, is gone: those readers read the source. "While"
//!   covers the paths between: a loop around the readers that changes the
//!   source keeps the copy.
//! - An address made as a base plus a constant and only used to address
//!   memory, under the same conditions, is gone: those accesses take the
//!   base and the offset (`iaddiu t, b, 3` then `sq x, 1(t)` is
//!   `sq x, 4(b)`).
//! - A copy of Q into a lane that nothing reads is dropped: lowering makes
//!   one after every division, and readers that take Q directly leave it
//!   unused.

use std::collections::HashMap;

use super::emit::{Inst, Line};
use crate::syntax::Pos;

pub fn run(lines: &mut Vec<(Line, Option<Pos>)>) {
    let mut mentions = count_mentions(lines);
    coalesce_moves(lines, &mut mentions);
    propagate_copies(lines);
    fold_offsets(lines);
    exact_raw_moves(lines);
    let mentions = count_mentions(lines);
    drop_unread_q_copies(lines, &mentions);
}

/// Whether `inst` writes the register `name`: it is the destination of
/// an instruction that writes one.
fn writes(inst: &Inst, name: &str) -> bool {
    let writer = VF_WRITERS.contains(&inst.mnemonic.as_str())
        || VI_WRITERS.contains(&inst.mnemonic.as_str())
        || FLAG_WRITERS.contains(&inst.mnemonic.as_str());
    writer && inst.operands.first().map(String::as_str) == Some(name)
}

/// Instructions that write VI01, their first operand.
const FLAG_WRITERS: &[&str] = &["fcand", "fcor", "fceq"];

/// `operand` with the register `from` named `to`.
fn renamed(operand: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token == from {
            out.push_str(to);
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for c in operand.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// The raw moves left become their exact instructions.
fn exact_raw_moves(lines: &mut Vec<(Line, Option<Pos>)>) {
    let mut index = 0;
    while index < lines.len() {
        let exact = match &mut lines[index].0 {
            Line::Inst(inst) => inst.exact.take(),
            _ => None,
        };
        match exact {
            Some(exact) => {
                let pos = lines[index].1;
                let count = exact.len();
                lines.splice(
                    index..=index,
                    exact.into_iter().map(|inst| (Line::Inst(inst), pos)),
                );
                index += count;
            }
            None => index += 1,
        }
    }
}

/// The largest qword offset LQ, SQ, ILW and ISW take either way.
const MAX_OFFSET: i64 = 1023;

/// `iaddiu t, b, k` with a variable `t`: (t, b, k).
fn offset_of(inst: &Inst) -> Option<(&str, &str, i64)> {
    match (inst.mnemonic.as_str(), inst.operands.as_slice()) {
        ("iaddiu", [to, from, amount]) if is_var(to) && (is_var(from) || from == "vi00") => {
            let amount = match amount.strip_prefix("0x") {
                Some(hex) => i64::from_str_radix(hex, 16).ok()?,
                None => amount.parse().ok()?,
            };
            Some((to, from, amount))
        }
        _ => None,
    }
}

/// A memory operand on register `name`: its offset.
fn memory_offset(operand: &str, name: &str) -> Option<i64> {
    let (offset, rest) = operand.split_once('(')?;
    (rest.strip_suffix(')')? == name)
        .then(|| offset.parse().ok())
        .flatten()
}

fn fold_offsets(lines: &mut Vec<(Line, Option<Pos>)>) {
    let mut index = 0;
    while index < lines.len() {
        let Some((to, from, amount)) = (match &lines[index].0 {
            Line::Inst(inst) => {
                offset_of(inst).map(|(to, from, k)| (to.to_string(), from.to_string(), k))
            }
            _ => None,
        }) else {
            index += 1;
            continue;
        };
        let Some(end) = live_range(lines, index, &to, &from) else {
            index += 1;
            continue;
        };
        // Every use is a memory operand, with an offset that still fits.
        let foldable = lines[index + 1..=end].iter().all(|line| match &line.0 {
            Line::Inst(inst) => inst.operands.iter().all(|operand| {
                !names(operand).any(|name| name == to)
                    || memory_offset(operand, &to)
                        .is_some_and(|offset| (offset + amount).abs() <= MAX_OFFSET)
            }),
            _ => true,
        });
        if !foldable {
            index += 1;
            continue;
        }
        for line in &mut lines[index + 1..=end] {
            if let Line::Inst(inst) = &mut line.0 {
                for operand in &mut inst.operands {
                    if let Some(offset) = memory_offset(operand, &to) {
                        *operand = format!("{}({from})", offset + amount);
                    }
                }
            }
        }
        lines.remove(index);
    }
}

fn propagate_copies(lines: &mut Vec<(Line, Option<Pos>)>) {
    let mut index = 0;
    while index < lines.len() {
        let Some((to, from)) = (match &lines[index].0 {
            Line::Inst(inst) => copy_of(inst).map(|(to, from)| (to.to_string(), from.to_string())),
            _ => None,
        }) else {
            index += 1;
            continue;
        };
        let Some(end) = live_range(lines, index, &to, &from) else {
            index += 1;
            continue;
        };
        // VI01 stays a block's: a label between may be reached from
        // where FCAND has rewritten it.
        let crosses = lines[index + 1..=end]
            .iter()
            .any(|line| !matches!(line.0, Line::Inst(_)));
        if from == "vi01" && crosses {
            index += 1;
            continue;
        }
        for line in &mut lines[index + 1..=end] {
            if let Line::Inst(inst) = &mut line.0 {
                for operand in &mut inst.operands {
                    *operand = renamed(operand, &to, &from);
                }
            }
        }
        lines.remove(index);
    }
}

/// For `to`, defined at `index` from `from`: the last line where `to` is
/// live, when the definition can go and `from` stand for `to` up to
/// there. That needs every other mention of `to` after the definition,
/// none writing it, and `from` unchanged in between. A branch from
/// further on back into that range (a loop around the uses) extends it to
/// the branch; a branch from before the definition into it, past the
/// definition, rules it out.
fn live_range(lines: &[(Line, Option<Pos>)], index: usize, to: &str, from: &str) -> Option<usize> {
    if to == from {
        return None;
    }
    let names_to = |line: &(Line, Option<Pos>)| match &line.0 {
        Line::Inst(inst) => inst_names(inst).iter().any(|name| name == to),
        _ => false,
    };
    if lines[..index].iter().any(names_to) {
        return None;
    }
    let last = (index + 1..lines.len())
        .rev()
        .find(|&at| names_to(&lines[at]))?;
    let labels: HashMap<&str, usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(at, line)| match &line.0 {
            Line::Label(label) => Some((label.as_str(), at)),
            _ => None,
        })
        .collect();
    let mut end = last;
    loop {
        let mut grown = end;
        for (at, line) in lines.iter().enumerate() {
            let Line::Inst(inst) = &line.0 else {
                continue;
            };
            if !is_branch(&inst.mnemonic) {
                continue;
            }
            let target = inst
                .operands
                .last()
                .and_then(|label| labels.get(label.as_str()));
            match target {
                Some(&target) if target > index && target <= end => {
                    if at < index {
                        return None;
                    }
                    grown = grown.max(at);
                }
                _ => {}
            }
        }
        if grown == end {
            break;
        }
        end = grown;
    }
    let written = lines[index + 1..=end].iter().any(|line| match &line.0 {
        Line::Inst(inst) => writes(inst, to) || writes(inst, from),
        _ => false,
    });
    (!written).then_some(end)
}

/// The register names in an operand: `ft_4[x]` names `ft_4`, `3(ixtop_0)`
/// names `ixtop_0`.
fn names(operand: &str) -> impl Iterator<Item = &str> {
    operand
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| token.starts_with(['f', 'i']) && token.contains('_'))
}

/// For each variable, the number of instructions that name it.
fn count_mentions(lines: &[(Line, Option<Pos>)]) -> HashMap<String, usize> {
    let mut mentions = HashMap::new();
    for (line, _) in lines {
        if let Line::Inst(inst) = line {
            for name in inst_names(inst) {
                *mentions.entry(name).or_insert(0) += 1;
            }
        }
    }
    mentions
}

/// The variables an instruction names, each once.
fn inst_names(inst: &Inst) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for operand in &inst.operands {
        for name in names(operand) {
            if !found.iter().any(|seen| seen == name) {
                found.push(name.to_string());
            }
        }
    }
    found
}

/// Instructions whose first operand is the VF register they write.
const VF_WRITERS: &[&str] = &[
    "add", "sub", "mul", "madd", "msub", "max", "mini", "abs", "addi", "subi", "muli", "maddi",
    "msubi", "addq", "subq", "mulq", "maddq", "msubq", "ftoi0", "ftoi4", "ftoi12", "ftoi15",
    "itof0", "itof4", "itof12", "itof15", "opmsub", "move", "mr32", "lq", "mfir", "mfp",
];

/// Instructions whose first operand is the VI register they write.
const VI_WRITERS: &[&str] = &[
    "iadd", "iaddi", "iaddiu", "isub", "isubiu", "iand", "ior", "ilw", "mtir", "fsand", "fsor",
    "fmand", "fmor", "fmeq", "fcget", "xtop", "xitop",
];

fn is_branch(mnemonic: &str) -> bool {
    matches!(
        mnemonic,
        "b" | "bal" | "jr" | "jalr" | "ibeq" | "ibne" | "ibltz" | "iblez" | "ibgtz" | "ibgez"
    )
}

/// A whole-register copy: `move.M r, t` or `iaddiu r, t, 0`, as (r, t).
fn copy_of(inst: &Inst) -> Option<(&str, &str)> {
    match (inst.mnemonic.as_str(), inst.operands.as_slice()) {
        ("move", [to, from]) if is_var(to) && is_var(from) => Some((to, from)),
        ("iaddiu", [to, from, zero])
            if zero == "0" && is_var(to) && (is_var(from) || from == "vi01") =>
        {
            Some((to, from))
        }
        _ => None,
    }
}

/// A variable's name, as lowering makes them: `f` or `i`, a hint, `_`, a
/// number.
fn is_var(operand: &str) -> bool {
    operand.starts_with(['f', 'i'])
        && operand.contains('_')
        && operand
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn coalesce_moves(lines: &mut Vec<(Line, Option<Pos>)>, mentions: &mut HashMap<String, usize>) {
    let mut index = 0;
    while index < lines.len() {
        if let Some(def) = coalescible(lines, index, mentions) {
            let Line::Inst(copy) = &lines[index].0 else {
                unreachable!("a copy is an instruction");
            };
            let (to, from) = copy_of(copy).expect("a copy");
            let (to, from) = (to.to_string(), from.to_string());
            let mask = copy.mask.clone();
            if let Line::Inst(def) = &mut lines[def].0 {
                // The destination loses the copy's mention, and gains the
                // definition's unless it had one.
                if inst_names(def).contains(&to)
                    && let Some(count) = mentions.get_mut(&to)
                {
                    *count -= 1;
                }
                def.operands[0] = to;
                if def.mnemonic == "lq" {
                    def.mask = mask;
                }
            }
            mentions.remove(&from);
            lines.remove(index);
            continue;
        }
        index += 1;
    }
}

/// For the copy at `index`, the instruction in the same block that writes
/// its source, when that can write the destination instead: it writes
/// the same lanes, nothing else names the source, and nothing between
/// names the destination.
fn coalescible(
    lines: &[(Line, Option<Pos>)],
    index: usize,
    mentions: &HashMap<String, usize>,
) -> Option<usize> {
    let Line::Inst(copy) = &lines[index].0 else {
        return None;
    };
    let (to, from) = copy_of(copy)?;
    if to == from || mentions.get(from) != Some(&2) {
        return None;
    }
    let vf = copy.mnemonic == "move";
    for def in (0..index).rev() {
        let Line::Inst(inst) = &lines[def].0 else {
            return None;
        };
        if is_branch(&inst.mnemonic) {
            return None;
        }
        let named = inst_names(inst);
        if named.iter().any(|name| name == to) && !named.iter().any(|name| name == from) {
            return None;
        }
        if !named.iter().any(|name| name == from) {
            continue;
        }
        let writers = if vf { VF_WRITERS } else { VI_WRITERS };
        let writes = writers.contains(&inst.mnemonic.as_str())
            && inst.operands.first().map(String::as_str) == Some(from)
            && !inst.operands[1..]
                .iter()
                .any(|operand| names(operand).any(|name| name == from));
        let lanes = !vf
            || inst.mask == copy.mask
            || (inst.mnemonic == "lq" && covers(inst.mask.as_deref(), copy.mask.as_deref()));
        return (writes && lanes).then_some(def);
    }
    None
}

/// Whether mask `outer` has every lane of `inner`.
fn covers(outer: Option<&str>, inner: Option<&str>) -> bool {
    let outer = outer.unwrap_or("xyzw");
    inner
        .unwrap_or("xyzw")
        .chars()
        .all(|lane| outer.contains(lane))
}

fn drop_unread_q_copies(lines: &mut Vec<(Line, Option<Pos>)>, mentions: &HashMap<String, usize>) {
    lines.retain(|(line, _)| {
        let Line::Inst(inst) = line else {
            return true;
        };
        let unread = inst.mnemonic == "addq"
            && matches!(inst.operands.as_slice(), [to, zero, q] if zero == "vf00" && q == "q"
                && mentions.get(to.as_str()) == Some(&1));
        !unread
    });
}
