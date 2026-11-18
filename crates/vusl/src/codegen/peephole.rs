//! Passes over the finished instructions, for what lowering one
//! expression at a time leaves behind. VCL schedules and allocates what it
//! is given but propagates no copies, so a move left in the source costs a
//! slot.
//!
//! - A temporary written by one instruction and then moved whole into a
//!   register is written there directly: `mul.xyz t, a, b` and
//!   `move.xyz r, t` become `mul.xyz r, a, b`.
//! - A copy whose destination is read only later in its block, while its
//!   source keeps its value, is gone: those readers read the source
//!   (inlining copies a function's `in` arguments).
//! - An address made as a base plus a constant and only used to address
//!   memory later in its block is gone: those accesses take the base and
//!   the offset (`iaddiu t, b, 3` then `sq x, 1(t)` is `sq x, 4(b)`).
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
    let mentions = count_mentions(lines);
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
        let end = (index + 1..lines.len())
            .find(|&at| !matches!(lines[at].0, Line::Inst(_)))
            .unwrap_or(lines.len());
        let block = &lines[index + 1..end];
        // Every use is a memory operand, with an offset that still fits.
        let mut uses = 0;
        let mut last = None;
        let mut foldable = true;
        for (at, line) in block.iter().enumerate() {
            let Line::Inst(inst) = &line.0 else {
                continue;
            };
            if !inst_names(inst).contains(&to) {
                continue;
            }
            uses += 1;
            last = Some(at);
            let every = inst.operands.iter().all(|operand| {
                !names(operand).any(|name| name == to)
                    || memory_offset(operand, &to)
                        .is_some_and(|offset| (offset + amount).abs() <= MAX_OFFSET)
            });
            if !every || writes(inst, &to) {
                foldable = false;
            }
        }
        let rewritten = |name: &str, upto: usize| {
            block[..upto].iter().any(|line| match &line.0 {
                Line::Inst(inst) => writes(inst, name),
                _ => false,
            })
        };
        let folds = foldable
            && mentions.get(&to) == Some(&(uses + 1))
            && last.is_some_and(|last| !rewritten(&from, last + 1));
        if !folds {
            index += 1;
            continue;
        }
        for line in &mut lines[index + 1..end] {
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
    let mentions = count_mentions(lines);
    let mut index = 0;
    while index < lines.len() {
        let Some((to, from)) = (match &lines[index].0 {
            Line::Inst(inst) => copy_of(inst).map(|(to, from)| (to.to_string(), from.to_string())),
            _ => None,
        }) else {
            index += 1;
            continue;
        };
        // The block after the copy: up to the next label or directive.
        let end = (index + 1..lines.len())
            .find(|&at| !matches!(lines[at].0, Line::Inst(_)))
            .unwrap_or(lines.len());
        let block = &lines[index + 1..end];
        let named = |line: &(Line, Option<Pos>), name: &str| match &line.0 {
            Line::Inst(inst) => inst_names(inst).iter().any(|seen| seen == name),
            _ => false,
        };
        let uses = block.iter().filter(|line| named(line, &to)).count();
        let last_use = block.iter().rposition(|line| named(line, &to));
        let rewritten = |name: &str, upto: usize| {
            block[..upto].iter().any(|line| match &line.0 {
                Line::Inst(inst) => writes(inst, name),
                _ => false,
            })
        };
        let propagates = to != from
            && mentions.get(&to) == Some(&(uses + 1))
            && last_use
                .is_some_and(|last| !rewritten(&to, block.len()) && !rewritten(&from, last + 1));
        if !propagates {
            index += 1;
            continue;
        }
        for line in &mut lines[index + 1..end] {
            if let Line::Inst(inst) = &mut line.0 {
                for operand in &mut inst.operands {
                    *operand = renamed(operand, &to, &from);
                }
            }
        }
        lines.remove(index);
    }
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
