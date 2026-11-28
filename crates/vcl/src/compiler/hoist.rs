//! The `hoist` optimization (docs/OPTIMIZATION_PLAN.md, O5 step 1): work
//! a loop does on every path through an iteration moves up into the earliest
//! stream that runs exactly when it does.
//!
//! Each stream is scheduled on its own, and VU latency overlaps only with
//! independent instructions issued after it. A loop with branches inside
//! often ends its header with a long dependency chain and a flag test, while
//! the streams after the branches start with short, independent work: the
//! next test's subtraction, the latch's counter and pointer steps. Moved up
//! into the header, that work fills the chain's stall cycles, and the later
//! streams shrink to their branches.
//!
//! The rewrite works on the parsed program, before anything is compiled:
//!
//! - A loop is a label and the last conditional branch back to it, with only
//!   ordinary code between them (no barriers, raw code, regions or register
//!   branches), no other loop inside and no branch from outside into it past
//!   its first labels. Its blocks start at labels and end after branches.
//! - Within one iteration (a branch back to the header, or out of the loop,
//!   ends it), block `b` runs exactly when an earlier block `a` does when `a`
//!   dominates `b` and `b` post-dominates `a`. An instruction of `b` moves to
//!   the end of the earliest such `a`, before its branch, when nothing it
//!   would move above conflicts with it: `a`'s branch, every block on the
//!   paths from `a` to `b`, and what stays above it in `b`.
//! - Conflicts are register dependencies (explicit operands and the implicit
//!   ACC, Q, P, I, R and MAC), memory (a load does not move above a store), and
//!   flags. Every FMAC instruction writes the MAC and status flags, so an
//!   instruction that reads or writes the status and MAC flags does not move
//!   above another that does; the clip flags are kept the same way. A flag
//!   read then sees the same writer: the scheduler times each read against
//!   the last writer before it in the stream.
//! - Stores, branches, `xgkick` and the other external and random-unit
//!   instructions stay where they are.
//!
//! The moved program has the same instructions on every path, in an order
//! with the same dependencies, so it computes the same values. It is kept
//! for each loop whose streams that take part get shorter in all: a stream
//! that takes more work can grow, and the scheduler may not find the stall
//! cycles the work was meant to fill (`compile_parsed`).

use super::*;
use crate::isa::instruction::subtype::branch::{LINK, REGISTER};

/// The parsed program with work moved up, and for each loop that changed,
/// the streams to compare.
pub(super) struct Hoisted {
    pub(super) program: parser::Program,
    pub(super) loops: Vec<HoistedLoop>,
}

/// A loop the rewrite changed.
pub(super) struct HoistedLoop {
    /// The loop's header label, lowercase: `hoist` can be told to leave the
    /// others alone.
    pub(super) header: String,
    /// The first label of each stream that gave or took work, lowercase.
    /// They run equally often, so the sum of their cycles is what one
    /// iteration's path through them costs.
    pub(super) streams: Vec<String>,
}

/// `parsed` with work moved up in its loops, or `None` when nothing moves.
/// With `only`, just the loops with those headers change.
pub(super) fn hoist(parsed: &parser::Program, only: Option<&BTreeSet<String>>) -> Option<Hoisted> {
    let statements = &parsed.statements;
    let labels = label_indices(statements);
    let mut order: Vec<usize> = (0..statements.len()).collect();
    let mut changed = Vec::new();
    for (header, latch) in innermost_loops(statements, &labels) {
        let name = label_name(statements, header);
        if only.is_some_and(|only| !only.contains(&name)) {
            continue;
        }
        let Some((arranged, streams)) = hoist_loop(statements, &labels, header, latch) else {
            continue;
        };
        order.splice(position(&order, header)..=position(&order, latch), arranged);
        changed.push(HoistedLoop {
            header: name,
            streams,
        });
    }
    if changed.is_empty() {
        return None;
    }
    let program = parser::Program {
        statements: order
            .into_iter()
            .map(|index| statements[index].clone())
            .collect(),
    };
    Some(Hoisted {
        program,
        loops: changed,
    })
}

/// The loops `hoist` and `speculate` work on, as their header's and latch's
/// statement indices: a label and the last conditional branch back to it,
/// with no other loop inside, and none that overlaps them without nesting
/// either way.
pub(super) fn innermost_loops(
    statements: &[parser::Statement],
    labels: &BTreeMap<String, usize>,
) -> Vec<(usize, usize)> {
    let mut latches: BTreeMap<usize, usize> = BTreeMap::new();
    for (index, statement) in statements.iter().enumerate() {
        if let Some(instruction) = instruction_of(statement)
            && is_conditional_branch(instruction)
            && let Some(&header) = branch_target(instruction).and_then(|t| labels.get(&t))
            && header < index
        {
            latches.insert(header, index);
        }
    }
    latches
        .iter()
        .filter(|&(&header, &latch)| {
            !latches.iter().any(|(&other, &end)| {
                (other, end) != (header, latch)
                    && other <= latch
                    && header <= end
                    && !(other < header && latch < end)
            })
        })
        .map(|(&header, &latch)| (header, latch))
        .collect()
}

/// Where statement `index` sits in `order`. The loops rewritten do not
/// overlap, so each loop's statements are still in their own order when it
/// is rewritten.
pub(super) fn position(order: &[usize], index: usize) -> usize {
    order.iter().position(|&at| at == index).unwrap()
}

/// A block of a loop: its statements, its instructions other than the
/// branch that ends it, and that branch.
pub(super) struct Block {
    pub(super) start: usize,
    /// One past the last statement.
    pub(super) end: usize,
    /// The first label, lowercase, when the block starts with one.
    pub(super) label: Option<String>,
    pub(super) body: Vec<usize>,
    pub(super) branch: Option<usize>,
}

/// The loop's statements in their new order, and the first labels of the
/// streams that changed, or `None` when nothing moves.
fn hoist_loop(
    statements: &[parser::Statement],
    labels: &BTreeMap<String, usize>,
    header: usize,
    latch: usize,
) -> Option<(Vec<usize>, Vec<String>)> {
    let inside = header..=latch;
    let body = &statements[inside.clone()];
    if !ordinary_body(body) || entered_past_header(statements, labels, header, &inside) {
        return None;
    }
    let blocks = blocks(statements, header, latch);
    if blocks.len() < 2 {
        return None;
    }
    let successors = successors(statements, labels, &blocks, header)?;
    let exit = blocks.len();
    let dominators = dominators(&blocks, &successors);
    let post_dominators = post_dominators(&successors, exit);

    // Each block's instructions as the rewrite goes, and what it took.
    let mut bodies: Vec<Vec<usize>> = blocks.iter().map(|block| block.body.clone()).collect();
    let mut taken: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    let mut streams = BTreeSet::new();
    for to in 1..blocks.len() {
        // The earliest blocks first: work goes as far up as it can.
        let receivers: Vec<usize> = (0..to)
            .filter(|&from| dominators[to][from] && post_dominators[from][to])
            .collect();
        if receivers.is_empty() || blocks[to].label.is_none() {
            continue;
        }
        let mut at = 0;
        while at < bodies[to].len() {
            let index = bodies[to][at];
            let instruction = instruction_of(&statements[index]).unwrap();
            let effects = Effects::of(instruction);
            let receiver = movable(instruction).then(|| {
                receivers.iter().copied().find(|&from| {
                    blocks[from].label.is_some()
                        && between(&successors, from, to)
                            .into_iter()
                            .flat_map(|block| bodies[block].iter().chain(&blocks[block].branch))
                            .chain(&blocks[from].branch)
                            .chain(&bodies[to][..at])
                            .all(|&other| {
                                !effects.conflicts(&Effects::of(
                                    instruction_of(&statements[other]).unwrap(),
                                ))
                            })
                })
            });
            match receiver.flatten() {
                Some(from) => {
                    bodies[to].remove(at);
                    bodies[from].push(index);
                    taken[from].push(index);
                    streams.insert(blocks[from].label.clone().unwrap());
                    streams.insert(blocks[to].label.clone().unwrap());
                }
                None => at += 1,
            }
        }
    }
    if streams.is_empty() {
        return None;
    }
    let moved: BTreeSet<usize> = taken.iter().flatten().copied().collect();
    let mut arranged = Vec::new();
    for (block, info) in blocks.iter().enumerate() {
        let insert = info.branch.unwrap_or(info.end);
        for index in info.start..info.end {
            if index == insert {
                arranged.extend(&taken[block]);
            }
            if !moved.contains(&index) {
                arranged.push(index);
            }
        }
        if insert == info.end {
            arranged.extend(&taken[block]);
        }
    }
    Some((arranged, streams.into_iter().collect()))
}

/// The loop's blocks, from `header` to `latch`: a label after code starts
/// one, and a branch ends one.
pub(super) fn blocks(statements: &[parser::Statement], header: usize, latch: usize) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut current = Block {
        start: header,
        end: header,
        label: None,
        body: Vec::new(),
        branch: None,
    };
    for (index, statement) in statements.iter().enumerate().take(latch + 1).skip(header) {
        let code = !current.body.is_empty() || current.branch.is_some();
        match &statement.kind {
            StatementKind::Label(label) => {
                if code {
                    current.end = index;
                    let next = index;
                    blocks.push(std::mem::replace(
                        &mut current,
                        Block {
                            start: next,
                            end: next,
                            label: None,
                            body: Vec::new(),
                            branch: None,
                        },
                    ));
                }
                if current.label.is_none() {
                    current.label = Some(label.name.to_ascii_lowercase());
                }
            }
            StatementKind::Instruction(instruction) => {
                if instruction.synthetic().is_some() {
                    continue;
                }
                if current.branch.is_some() {
                    current.end = index;
                    blocks.push(std::mem::replace(
                        &mut current,
                        Block {
                            start: index,
                            end: index,
                            label: None,
                            body: Vec::new(),
                            branch: None,
                        },
                    ));
                }
                if instruction.metadata.is_branch() {
                    current.branch = Some(index);
                } else {
                    current.body.push(index);
                }
            }
            _ => {}
        }
    }
    current.end = latch + 1;
    blocks.push(current);
    blocks
}

/// Each block's successors within one iteration, by block index, where
/// `blocks.len()` is the iteration's end: a branch back to the header or out
/// of the loop, or falling out of it. `None` when a branch goes back to
/// another block of the loop.
pub(super) fn successors(
    statements: &[parser::Statement],
    labels: &BTreeMap<String, usize>,
    blocks: &[Block],
    header: usize,
) -> Option<Vec<Vec<usize>>> {
    let exit = blocks.len();
    let block_of = |statement: usize| {
        blocks
            .iter()
            .position(|block| block.start <= statement && statement < block.end)
    };
    let mut successors = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let mut next = Vec::new();
        let mut falls = true;
        if let Some(branch) = block.branch {
            let instruction = instruction_of(&statements[branch]).unwrap();
            let target = branch_target(instruction)
                .and_then(|target| labels.get(&target).copied())
                .map(|at| block_of(at).filter(|_| at != header));
            match target {
                Some(Some(to)) if to > index => next.push(to),
                Some(Some(_)) => return None,
                // Back to the header, or out of the loop.
                Some(None) | None => next.push(exit),
            }
            falls = is_conditional_branch(instruction);
        }
        if falls {
            next.push(index + 1);
        }
        next.sort_unstable();
        next.dedup();
        successors.push(next);
    }
    Some(successors)
}

/// `dominators[b][a]`: block `a` is on every path from the header to `b`.
/// Every edge goes forward, so one pass in order settles them.
pub(super) fn dominators(blocks: &[Block], successors: &[Vec<usize>]) -> Vec<Vec<bool>> {
    let count = blocks.len();
    let mut predecessors = vec![Vec::new(); count];
    for (from, next) in successors.iter().enumerate() {
        for &to in next {
            if to < count {
                predecessors[to].push(from);
            }
        }
    }
    let mut dominators = vec![vec![false; count]; count];
    dominators[0][0] = true;
    for block in 1..count {
        let mut set: Option<Vec<bool>> = None;
        for &from in &predecessors[block] {
            set = Some(match set {
                None => dominators[from].clone(),
                Some(set) => set
                    .iter()
                    .zip(&dominators[from])
                    .map(|(a, b)| *a && *b)
                    .collect(),
            });
        }
        // A block nothing reaches is dominated by nothing but itself.
        let mut set = set.unwrap_or_else(|| vec![false; count]);
        set[block] = true;
        dominators[block] = set;
    }
    dominators
}

/// `post_dominators[a][b]`: block `b` is on every path from `a` to the
/// iteration's end (`exit`). One pass in reverse order settles them.
pub(super) fn post_dominators(successors: &[Vec<usize>], exit: usize) -> Vec<Vec<bool>> {
    let count = successors.len();
    let mut post = vec![vec![false; count + 1]; count + 1];
    post[exit][exit] = true;
    for block in (0..count).rev() {
        let mut set: Option<Vec<bool>> = None;
        for &to in &successors[block] {
            set = Some(match set {
                None => post[to].clone(),
                Some(set) => set.iter().zip(&post[to]).map(|(a, b)| *a && *b).collect(),
            });
        }
        let mut set = set.unwrap_or_else(|| vec![false; count + 1]);
        set[block] = true;
        post[block] = set;
    }
    post
}

/// The blocks strictly between `from` and `to`: reachable from `from`, and
/// reaching `to`.
pub(super) fn between(successors: &[Vec<usize>], from: usize, to: usize) -> Vec<usize> {
    let count = successors.len();
    let mut reached = vec![false; count + 1];
    reached[from] = true;
    for block in from..count {
        if reached[block] {
            for &next in &successors[block] {
                reached[next] = true;
            }
        }
    }
    let mut reaches = vec![false; count + 1];
    reaches[to] = true;
    for block in (from..to).rev() {
        reaches[block] = successors[block].iter().any(|&next| reaches[next]);
    }
    (from + 1..to)
        .filter(|&block| reached[block] && reaches[block])
        .collect()
}

/// What an instruction reads and writes, for the conflict test.
struct Effects {
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
    /// The status and MAC flags.
    status: bool,
    /// The clip flags.
    clip: bool,
    loads: bool,
    stores: bool,
}

impl Effects {
    fn of(instruction: &Instruction) -> Self {
        let mut effects = Effects {
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            status: false,
            clip: false,
            loads: false,
            stores: false,
        };
        for operand in &instruction.operands {
            match operand.class {
                class::STAT_ALU_STICKY
                | class::STAT_DIV_STICKY
                | class::STAT_DIV
                | class::STAT_ALU
                | class::STATUS_FROM_DIVIDE
                | class::STATUS_FROM_ALU
                | class::STATUS_TEST
                | class::STATUS_SET
                | class::MAC => effects.status = true,
                class::CLIP | class::CLIP_ALL => effects.clip = true,
                class::MEMORY => {
                    effects.loads = true;
                    effects.stores = true;
                }
                _ => {}
            }
            let names = [&operand.register, &operand.forced_register];
            for name in names.into_iter().flatten() {
                let name = name.to_ascii_lowercase();
                match operand.direction {
                    Direction::Write => effects.writes.insert(name),
                    _ => effects.reads.insert(name),
                };
            }
        }
        let metadata = instruction.metadata;
        if metadata.is_memory() {
            if metadata.is_store() {
                effects.stores = true;
            } else {
                effects.loads = true;
            }
        }
        if metadata.order_flags & order::WRITES_MEMORY != 0 {
            effects.stores = true;
        }
        if metadata.order_flags & order::READS_MEMORY != 0 {
            effects.loads = true;
        }
        effects
    }

    /// Whether this instruction may not move above `other`.
    fn conflicts(&self, other: &Effects) -> bool {
        self.writes
            .iter()
            .any(|name| other.reads.contains(name) || other.writes.contains(name))
            || self.reads.iter().any(|name| other.writes.contains(name))
            || (self.status && other.status)
            || (self.clip && other.clip)
            || (self.loads && other.stores)
            || (self.stores && (other.loads || other.stores))
    }
}

/// Whether an instruction may move at all: ordinary code or a load, with no
/// external, random-unit or memory-ordering effect.
pub(super) fn movable(instruction: &Instruction) -> bool {
    let metadata = instruction.metadata;
    let ordinary = match metadata.kind {
        instruction::InstructionKind::Ordinary => true,
        instruction::InstructionKind::Memory => !metadata.is_store(),
        _ => false,
    };
    ordinary
        && instruction.mode == ParseMode::Vcl
        && instruction.synthetic().is_none()
        && metadata.order_flags == order::NONE
        && metadata.unit_mask & (unit::EXTERNAL | unit::RANDOM | unit::BRANCH) == 0
        && !instruction
            .operands
            .iter()
            .any(|operand| operand.class == class::MEMORY || operand.class == class::EXTERNAL)
}

/// Each table's cycles in a listing, by its stream's name, lowercase: the
/// rows it walks, stall rows included (`printed [a b] walked [name]`), which
/// is what the stream takes when it is entered with nothing in flight.
pub(super) fn table_cycles(assembly: &str) -> BTreeMap<String, u32> {
    tables(assembly).into_iter().collect()
}

/// Each table's stream name, lowercase, and cycles (as [`table_cycles`]
/// counts them), in the listing's order.
pub(super) fn tables(assembly: &str) -> Vec<(String, u32)> {
    assembly
        .lines()
        .filter_map(|line| {
            let rest = line
                .strip_prefix("; _LNOPT_w=[")
                .or_else(|| line.strip_prefix("; _LPOPT_w=["))?;
            let (_, rest) = rest.split_once(']')?;
            let (_, rest) = rest.split_once(']')?;
            let cycles = rest.split_whitespace().next()?.parse().ok()?;
            let name = rest.rsplit_once('[')?.1.trim_end().strip_suffix(']')?;
            Some((name.to_ascii_lowercase(), cycles))
        })
        .collect()
}

/// The cycles a loop's moved streams take in `before` and `after`, or
/// `None` when one of them is not a table in both.
pub(super) fn stream_cycles(
    changed: &HoistedLoop,
    before: &BTreeMap<String, u32>,
    after: &BTreeMap<String, u32>,
) -> Option<(u32, u32)> {
    let mut sums = (0, 0);
    for stream in &changed.streams {
        sums.0 += before.get(stream)?;
        sums.1 += after.get(stream)?;
    }
    Some(sums)
}

pub(super) fn label_name(statements: &[parser::Statement], index: usize) -> String {
    match &statements[index].kind {
        StatementKind::Label(label) => label.name.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Only labels and ordinary instructions, with no barrier: no regions, raw
/// code, directives, calls or register branches.
pub(super) fn ordinary_body(statements: &[parser::Statement]) -> bool {
    statements.iter().all(|statement| match &statement.kind {
        StatementKind::Label(_) => true,
        StatementKind::Instruction(instruction) if instruction.mode == ParseMode::Vcl => {
            match instruction.synthetic() {
                None => {
                    !instruction.metadata.is_branch()
                        || instruction.metadata.subtype & (REGISTER | LINK) == 0
                }
                Some(Synthetic::LoopCount | Synthetic::LoopExtra | Synthetic::LoopAbsolute) => true,
                Some(_) => false,
            }
        }
        _ => false,
    })
}

/// Whether a branch from outside the loop names one of its labels past the
/// header.
pub(super) fn entered_past_header(
    statements: &[parser::Statement],
    labels: &BTreeMap<String, usize>,
    header: usize,
    inside: &std::ops::RangeInclusive<usize>,
) -> bool {
    statements.iter().enumerate().any(|(index, statement)| {
        !inside.contains(&index)
            && instruction_of(statement)
                .and_then(branch_target)
                .and_then(|target| labels.get(&target))
                .is_some_and(|&at| at > header && inside.contains(&at))
    })
}

/// Each label's statement index, by lowercase name.
pub(super) fn label_indices(statements: &[parser::Statement]) -> BTreeMap<String, usize> {
    statements
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| match &statement.kind {
            StatementKind::Label(label) => Some((label.name.to_ascii_lowercase(), index)),
            _ => None,
        })
        .collect()
}

pub(super) fn instruction_of(statement: &parser::Statement) -> Option<&Instruction> {
    match &statement.kind {
        StatementKind::Instruction(instruction) => Some(instruction),
        _ => None,
    }
}

/// A branch that reads a register and may fall through.
pub(super) fn is_conditional_branch(instruction: &Instruction) -> bool {
    instruction.synthetic().is_none()
        && instruction.metadata.is_branch()
        && instruction.metadata.subtype & (REGISTER | LINK) == 0
        && instruction
            .operands
            .iter()
            .any(|operand| operand.register.is_some() && operand.direction == Direction::Read)
}

/// A branch's target label, lowercase.
pub(super) fn branch_target(instruction: &Instruction) -> Option<String> {
    if instruction.synthetic().is_some() || !instruction.metadata.is_branch() {
        return None;
    }
    instruction
        .operands
        .iter()
        .rev()
        .find(|operand| operand.register.is_none())
        .and_then(|operand| operand.immediate.as_deref())
        .map(str::to_ascii_lowercase)
}
