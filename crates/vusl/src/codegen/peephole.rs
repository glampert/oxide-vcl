//! Passes over the finished instructions, for what lowering one
//! expression at a time leaves behind. VCL schedules and allocates what it
//! is given but propagates no copies, so a move left in the source costs a
//! slot.
//!
//! - A temporary written by one instruction and then moved whole into a
//!   register is written there directly: `mul.xyz t, a, b` and
//!   `move.xyz r, t` become `mul.xyz r, a, b`.
//! - A copy of Q into a lane that nothing reads is dropped: lowering makes
//!   one after every division, and readers that take Q directly leave it
//!   unused.

use std::collections::HashMap;

use super::emit::{Inst, Line};
use crate::syntax::Pos;

pub fn run(lines: &mut Vec<(Line, Option<Pos>)>) {
    let mut mentions = count_mentions(lines);
    coalesce_moves(lines, &mut mentions);
    drop_unread_q_copies(lines, &mentions);
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
        ("iaddiu", [to, from, zero]) if zero == "0" && is_var(to) && is_var(from) => {
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
