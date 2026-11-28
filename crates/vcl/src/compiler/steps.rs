//! The `steps` optimization (docs/OPTIMIZATION_PLAN.md, O2 and O4): a run
//! of steps of one pointer within a block becomes one step.
//!
//! Hand-written VU code often steps a pointer once per element it handles:
//! a loop over four vertices of 3 qwords each ends with four
//! `iaddiu next_input, next_input, 3`. Each step is a lower-pipe
//! instruction, and in a loop the lower pipe bounds, each one is a cycle of
//! the period. One `iaddiu next_input, next_input, 12` does the same.
//!
//! The rewrite works on the parsed program, before `pointers`:
//!
//! - A block is a run of ordinary instructions between labels, branches,
//!   barriers, raw code and directives.
//! - In a block, steps of one register by an immediate (`iaddiu r, r, k`,
//!   `isubiu r, r, k` or `iaddi r, r, k`, all of one of those) make a run as
//!   long as nothing between them reads or writes the register, except as
//!   the base of an `lq`, `sq`, `ilw` or `isw` with an offset.
//! - The run becomes its last step, by the sum of the steps, when that sum
//!   fits the instruction's immediate field. The accesses between its steps
//!   saw the register already stepped by the steps before them; their
//!   offsets take those steps instead, and must stay within the signed
//!   11-bit field.
//!
//! The register holds what it held after each run, and every access
//! between reads the address it read: the program computes the same values,
//! with fewer instructions.

use super::*;
use crate::isa::instruction::subtype::branch::{LINK, REGISTER};

/// The operand class of an `offset(base)` memory operand.
const MEMORY_OFFSET: u8 = 29;

/// `parsed` with its runs of steps folded, or `None` when it has none.
pub(super) fn fold_steps(parsed: &parser::Program) -> Option<parser::Program> {
    let statements = &parsed.statements;
    let mut rewritten = parsed.clone();
    let mut removed = BTreeSet::new();
    let mut start = 0;
    while start < statements.len() {
        let mut end = start;
        while end < statements.len() && in_block(&statements[end]) {
            end += 1;
            if instruction_of(&statements[end - 1]).is_some_and(|i| i.metadata.is_branch()) {
                break;
            }
        }
        if end > start {
            fold_block(statements, start..end, &mut rewritten, &mut removed);
            start = end;
        } else {
            start += 1;
        }
    }
    if removed.is_empty() {
        return None;
    }
    rewritten.statements = rewritten
        .statements
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !removed.contains(index))
        .map(|(_, statement)| statement)
        .collect();
    Some(rewritten)
}

/// Whether a statement belongs inside a block: an ordinary instruction, or
/// a loop hint, which neither reads nor writes registers. A label, a
/// directive, raw code, any other synthetic statement and a branch that
/// reads a register or links end it (a branch is the block's last).
fn in_block(statement: &parser::Statement) -> bool {
    match &statement.kind {
        StatementKind::Instruction(instruction) if instruction.mode == ParseMode::Vcl => {
            match instruction.synthetic() {
                None => {
                    instruction.metadata.subtype & (REGISTER | LINK) == 0
                        || !instruction.metadata.is_branch()
                }
                Some(Synthetic::LoopCount | Synthetic::LoopExtra | Synthetic::LoopAbsolute) => true,
                Some(_) => false,
            }
        }
        _ => false,
    }
}

/// A run of steps being gathered for one register.
#[derive(Default)]
struct Run {
    /// The instruction its steps share.
    kind: Option<Kind>,
    /// The steps, by statement, with their amounts.
    steps: Vec<(usize, i64)>,
    /// The accesses after the first step, by statement, with what the steps
    /// before each had added.
    accesses: Vec<(usize, i64)>,
}

/// Folds the runs in the block `range` of `statements` into `rewritten`,
/// adding the statements that go to `removed`.
fn fold_block(
    statements: &[parser::Statement],
    range: std::ops::Range<usize>,
    rewritten: &mut parser::Program,
    removed: &mut BTreeSet<usize>,
) {
    let mut runs: BTreeMap<String, Run> = BTreeMap::new();
    for index in range {
        let Some(instruction) = instruction_of(&statements[index]) else {
            continue;
        };
        if instruction.synthetic().is_some() {
            continue;
        }
        let step = step_of(instruction);
        let mentioned: BTreeSet<String> = instruction
            .operands
            .iter()
            .flat_map(|operand| [&operand.register, &operand.forced_register])
            .flatten()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        for register in mentioned {
            if let Some((kind, stepped, amount)) = &step
                && *stepped == register
            {
                let run = runs.entry(register.clone()).or_default();
                if run.kind.is_some_and(|other| other != *kind) {
                    let done = std::mem::take(run);
                    fold_run(statements, &done, rewritten, removed);
                }
                run.kind = Some(*kind);
                run.steps.push((index, *amount));
                continue;
            }
            let Some(run) = runs.get_mut(&register) else {
                continue;
            };
            if based_access(instruction, &register) {
                let added = run.steps.iter().map(|&(_, amount)| amount).sum();
                run.accesses.push((index, added));
            } else {
                let done = runs.remove(&register).unwrap();
                fold_run(statements, &done, rewritten, removed);
            }
        }
    }
    for run in runs.values() {
        fold_run(statements, run, rewritten, removed);
    }
}

/// Folds one run, when it has two steps or more, the sum fits its
/// instruction and every access's offset still fits: the last step takes
/// the sum, the others go, and each access between takes the steps before
/// it into its offset.
fn fold_run(
    statements: &[parser::Statement],
    run: &Run,
    rewritten: &mut parser::Program,
    removed: &mut BTreeSet<usize>,
) {
    let (Some(&(last, _)), Some(kind)) = (run.steps.last(), run.kind) else {
        return;
    };
    if run.steps.len() < 2 {
        return;
    }
    let (_, register, _) = step_of(instruction_of(&statements[last]).unwrap()).unwrap();
    let total: i64 = run.steps.iter().map(|&(_, amount)| amount).sum();
    let fits = match kind {
        Kind::Add => (0..=0x7fff).contains(&total),
        Kind::Subtract => (0..=0x7fff).contains(&-total),
        Kind::Small => (-16..=15).contains(&total),
    };
    if !fits {
        return;
    }
    // Only the accesses before the last step move past steps.
    let accesses: Vec<(usize, i64)> = run
        .accesses
        .iter()
        .copied()
        .filter(|&(index, _)| index < last)
        .collect();
    let mut shifted = Vec::new();
    for &(index, added) in &accesses {
        let instruction = instruction_of(&statements[index]).unwrap();
        let mut operands = instruction.operands.clone();
        for operand in operands
            .iter_mut()
            .filter(|operand| names(operand, &register))
        {
            let Some(offset) = evaluate(operand.immediate.as_deref().unwrap_or("0")) else {
                return;
            };
            let offset = offset + added;
            if !(-1024..=1023).contains(&offset) {
                return;
            }
            let base = operand.register.clone().unwrap_or_default();
            operand.text = format!("{offset}({base})");
            operand.immediate = Some(offset.to_string());
        }
        shifted.push((index, operands));
    }
    for (index, operands) in shifted {
        if let StatementKind::Instruction(instruction) = &mut rewritten.statements[index].kind {
            instruction.operands = operands;
        }
    }
    let magnitude = match kind {
        Kind::Subtract => -total,
        _ => total,
    };
    if let StatementKind::Instruction(instruction) = &mut rewritten.statements[last].kind {
        let operand = &mut instruction.operands[2];
        operand.text = magnitude.to_string();
        operand.immediate = Some(magnitude.to_string());
    }
    for &(index, _) in &run.steps[..run.steps.len() - 1] {
        removed.insert(index);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `iaddiu`.
    Add,
    /// `isubiu`.
    Subtract,
    /// `iaddi`, with a 5-bit signed immediate.
    Small,
}

/// `register = register + amount` for `iaddiu`, `isubiu` and `iaddi` on one
/// register, with its lowercase name and the signed amount.
fn step_of(instruction: &Instruction) -> Option<(Kind, String, i64)> {
    let (kind, sign) = match instruction.mnemonic.to_ascii_lowercase().as_str() {
        "iaddiu" => (Kind::Add, 1),
        "isubiu" => (Kind::Subtract, -1),
        "iaddi" => (Kind::Small, 1),
        _ => return None,
    };
    let [destination, source, amount] = &instruction.operands[..] else {
        return None;
    };
    if destination.forced_register.is_some() || source.forced_register.is_some() {
        return None;
    }
    let destination = destination.register.as_deref()?.to_ascii_lowercase();
    let source = source.register.as_deref()?.to_ascii_lowercase();
    if destination != source || destination == "vi00" {
        return None;
    }
    let amount = evaluate(amount.immediate.as_deref()?)?;
    Some((kind, destination, sign * amount))
}

/// Whether `instruction` is an `lq`, `sq`, `ilw` or `isw` that names
/// `register` only as the base of its `offset(base)` address, with an
/// offset it may change.
fn based_access(instruction: &Instruction, register: &str) -> bool {
    let name = instruction.mnemonic.to_ascii_lowercase();
    let base = name.split('.').next().unwrap_or(&name);
    if !matches!(base, "lq" | "sq" | "ilw" | "isw") || instruction.fixed_offset {
        return false;
    }
    instruction.operands.iter().all(|operand| {
        let named = names(operand, register)
            || operand
                .forced_register
                .as_deref()
                .is_some_and(|forced| forced.eq_ignore_ascii_case(register));
        !named
            || (operand.class == MEMORY_OFFSET
                && operand.direction == Direction::Read
                && operand.forced_register.is_none()
                && operand
                    .immediate
                    .as_deref()
                    .is_none_or(|text| evaluate(text).is_some()))
    })
}

/// Whether an operand names `register` (lowercase).
fn names(operand: &parser::Operand, register: &str) -> bool {
    operand
        .register
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(register))
}

/// An immediate's value, when it is a constant expression.
fn evaluate(text: &str) -> Option<i64> {
    let evaluation = crate::parser::expression::evaluate(text).ok()?;
    Some(i64::from(evaluation.number()?.to_i32()))
}

fn instruction_of(statement: &parser::Statement) -> Option<&Instruction> {
    match &statement.kind {
        StatementKind::Instruction(instruction) => Some(instruction),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The program's instructions as `mnemonic operand,operand`, lowercase.
    fn folded(source: &str) -> Option<Vec<String>> {
        let source = format!("--enter\n--endenter\n{source}--exit\n--endexit\n");
        let parsed = parser::parse(&source).unwrap();
        fold_steps(&parsed).map(|program| {
            program
                .statements
                .iter()
                .filter_map(instruction_of)
                .filter(|instruction| instruction.synthetic().is_none())
                .map(|instruction| {
                    let operands: Vec<&str> = instruction
                        .operands
                        .iter()
                        .filter(|operand| !operand.implicit)
                        .map(|operand| operand.text.as_str())
                        .collect();
                    format!("{} {}", instruction.mnemonic, operands.join(",")).to_ascii_lowercase()
                })
                .collect()
        })
    }

    #[test]
    fn a_run_becomes_its_last_step_and_the_accesses_between_shift() {
        let order = folded(
            "lq va,0(ptr)\n\
             iaddiu ptr,ptr,3\n\
             lq vb,0(ptr)\n\
             isubiu cnt,cnt,2\n\
             iaddiu ptr,ptr,3\n\
             sq vb,-1(ptr)\n\
             isubiu cnt,cnt,2\n\
             iaddiu ptr,ptr,3\n\
             ibne ptr,vi00,away\n\
             iaddiu ptr,ptr,3\n\
             away:\n",
        )
        .expect("a fold");
        assert_eq!(
            order,
            [
                "lq va,0(ptr)",
                "lq vb,3(ptr)",
                "sq vb,5(ptr)",
                "isubiu cnt,cnt,4",
                "iaddiu ptr,ptr,9",
                "ibne ptr,vi00,away",
                "iaddiu ptr,ptr,3",
            ]
        );
    }

    #[test]
    fn a_run_stays_when_an_offset_or_the_sum_would_not_fit() {
        assert!(folded("iaddiu ptr,ptr,1\nlq va,1023(ptr)\niaddiu ptr,ptr,1\n").is_none());
        assert!(folded("iaddiu ptr,ptr,0x7000\niaddiu ptr,ptr,0x1000\n").is_none());
        assert!(folded("iaddi ptr,ptr,10\niaddi ptr,ptr,10\n").is_none());
        assert!(folded("iaddiu ptr,ptr,1\nisubiu ptr,ptr,1\n").is_none());
    }
}
