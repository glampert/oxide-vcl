//! The `speculate` optimization (docs/OPTIMIZATION_PLAN.md, O5 step 1):
//! the first work of a stream that runs on some iterations of a loop starts
//! early, in a stream that runs on all of them, where its result is not
//! needed on the paths that skip it.
//!
//! A loop with branches inside often has a conditional stream that waits on
//! its own first instructions: an intersection that subtracts two corners'
//! distances, divides by the difference and then waits for Q. Each stream is
//! scheduled on its own, so that wait cannot overlap anything. Started in the
//! loop's header, which ends with a long dependency chain and its stall
//! cycles, the same instructions cost the header nothing, and their latency
//! overlaps the streams between.
//!
//! The rewrite works on the parsed program, after `hoist`, on the loops
//! `hoist` takes (`hoist::innermost_loops`, with the same blocks):
//!
//! - A block runs on every iteration when it post-dominates the header. Work
//!   moves from a block that does not into the earliest such block that
//!   dominates it, the receiver.
//! - An instruction moves when no other instruction of its block that stays
//!   produces what it reads, and either nothing in its block produces what it
//!   reads or it is a division or EFU instruction (it writes Q or P): the
//!   first independent work and the long-latency work it feeds. What reads Q
//!   or P stays, so the latency is spent in the streams between.
//! - It goes as far up into the receiver as its registers and memory allow,
//!   with the same conflicts as `hoist` (a load does not move above a store),
//!   and no conflict with the paths between the two blocks.
//! - On the paths that skip its block, its results must be dead: a search
//!   from where it lands finds no read of a register it writes before that
//!   register is written again, through the rest of the program, to the
//!   program's exits (where the declared outputs are read). It writes no
//!   hardware register by name and no declared one.
//! - Flags: every FMAC instruction writes the ALU status and MAC flags, and a
//!   division the divide status. Where a flag is read anywhere in the
//!   program, the moved instruction's write of it must be one nothing reads:
//!   in its block, the next instruction that touches that flag writes it,
//!   and in the receiver, the next one after where it lands does too. Flag
//!   tests, clip-flag instructions and anything writing the sticky flags do
//!   not move, and nothing that writes status moves when a test reads the
//!   sticky flags, which the moved work would add to on the other paths.
//! - Stores, branches and external and random-unit instructions stay.
//!
//! On the paths through the moved work's block the program computes what it
//! did; on the others, the moved work's results are never read. The program
//! is compiled once more and kept for each loop whose blocks that gave work
//! take fewer cycles in all, with none of the others taking more
//! (`compile_parsed`): the receiver is on every path, so the usual path
//! costs no more.

use super::hoist::{
    Block, between, blocks, branch_target, dominators, entered_past_header, innermost_loops,
    instruction_of, is_conditional_branch, label_indices, label_name, movable, ordinary_body,
    position, post_dominators, successors,
};
use super::*;
use crate::isa::instruction::subtype::branch::{LINK, REGISTER};

/// The parsed program with work started early, and for each loop that
/// changed, what to compare.
pub(super) struct Speculated {
    pub(super) program: parser::Program,
    pub(super) loops: Vec<SpeculatedLoop>,
}

/// A loop the rewrite changed.
pub(super) struct SpeculatedLoop {
    /// The loop's header label, lowercase: `speculate` can be told to leave
    /// the others alone.
    pub(super) header: String,
    /// Each block's first label, lowercase, in order: a block is one table
    /// of the listing, named by its label or, without one, by a generated
    /// name.
    blocks: Vec<Option<String>>,
    /// The blocks that gave work.
    donors: Vec<bool>,
}

/// The flags the status classes expand into (`analysis::prune::expand`),
/// lowercase.
const ALU: &str = "stat_alu";
const DIVIDE: &str = "stat_div";
const MAC: &str = "mac";
const STICKY_ALU: &str = "stat_alu_s";
const STICKY_DIVIDE: &str = "stat_div_s";

fn is_flag(name: &str) -> bool {
    [ALU, DIVIDE, MAC, STICKY_ALU, STICKY_DIVIDE].contains(&name)
}

/// The flags whose writes a later test can read.
fn is_tested_flag(name: &str) -> bool {
    [ALU, DIVIDE, MAC].contains(&name)
}

/// `parsed` with work started early in its loops, or `None` when nothing
/// moves. With `only`, just the loops with those headers change.
pub(super) fn speculate(
    parsed: &parser::Program,
    only: Option<&BTreeSet<String>>,
) -> Option<Speculated> {
    let statements = &parsed.statements;
    let labels = label_indices(statements);
    let flow = Flow::new(statements, &labels);
    let mut order: Vec<usize> = (0..statements.len()).collect();
    let mut changed = Vec::new();
    for (header, latch) in innermost_loops(statements, &labels) {
        let name = label_name(statements, header);
        if only.is_some_and(|only| !only.contains(&name)) {
            continue;
        }
        let Some((arranged, speculated)) =
            speculate_loop(statements, &labels, &flow, (header, latch), name)
        else {
            continue;
        };
        order.splice(position(&order, header)..=position(&order, latch), arranged);
        changed.push(speculated);
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
    Some(Speculated {
        program,
        loops: changed,
    })
}

/// Whether a loop gains in `after` against `before` (both from
/// [`hoist::tables`]): its blocks that gave work take fewer cycles in all,
/// and none of the others takes more. `false` when its tables cannot be
/// matched to its blocks in both listings.
pub(super) fn gains(
    changed: &SpeculatedLoop,
    before: &[(String, u32)],
    after: &[(String, u32)],
) -> bool {
    let (Some(old), Some(new)) = (loop_tables(changed, before), loop_tables(changed, after)) else {
        return false;
    };
    let mut given = (0, 0);
    for (block, (old, new)) in old.iter().zip(new).enumerate() {
        if old.0 != new.0 {
            return false;
        }
        if changed.donors[block] {
            given.0 += old.1;
            given.1 += new.1;
        } else if new.1 > old.1 {
            return false;
        }
    }
    given.1 < given.0
}

/// A loop's tables in a listing, one per block in order, or `None` when they
/// do not match its blocks: a block with a label is the table of that name,
/// one without is a table whose name no block of the loop has.
fn loop_tables<'a>(
    changed: &SpeculatedLoop,
    tables: &'a [(String, u32)],
) -> Option<&'a [(String, u32)]> {
    let first = tables
        .iter()
        .position(|(name, _)| *name == changed.header)?;
    let tables = tables.get(first..first + changed.blocks.len())?;
    let labels: BTreeSet<&String> = changed.blocks.iter().flatten().collect();
    tables
        .iter()
        .zip(&changed.blocks)
        .all(|((name, _), label)| match label {
            Some(label) => name == label,
            None => !labels.contains(name),
        })
        .then_some(tables)
}

/// The loop's statements in their new order and what changed, or `None`
/// when nothing moves. `name` is its header's label.
fn speculate_loop(
    statements: &[parser::Statement],
    labels: &BTreeMap<String, usize>,
    flow: &Flow,
    (header, latch): (usize, usize),
    name: String,
) -> Option<(Vec<usize>, SpeculatedLoop)> {
    let inside = header..=latch;
    if !ordinary_body(&statements[inside.clone()])
        || entered_past_header(statements, labels, header, &inside)
    {
        return None;
    }
    let blocks = blocks(statements, header, latch);
    if blocks.len() < 2 {
        return None;
    }
    let successors = successors(statements, labels, &blocks, header)?;
    let dominators = dominators(&blocks, &successors);
    let post_dominators = post_dominators(&successors, blocks.len());
    let every = |block: usize| post_dominators[0][block];
    let accesses = |index: usize| Access::of(instruction_of(&statements[index]).unwrap());

    let mut bodies: Vec<Vec<usize>> = blocks.iter().map(|block| block.body.clone()).collect();
    let mut donors = vec![false; blocks.len()];
    for from in 1..blocks.len() {
        let receivers: Vec<usize> = (0..from)
            .filter(|&to| every(to) && dominators[from][to])
            .collect();
        if every(from) || receivers.is_empty() {
            continue;
        }
        let original = &blocks[from].body;
        let mut moved: BTreeSet<usize> = BTreeSet::new();
        let mut at = 0;
        while at < bodies[from].len() {
            let index = bodies[from][at];
            let instruction = instruction_of(&statements[index]).unwrap();
            let access = accesses(index);
            let earlier = &original[..original.iter().position(|&i| i == index).unwrap()];
            let producers: Vec<usize> = earlier
                .iter()
                .copied()
                .filter(|&other| access.reads_what(&accesses(other)))
                .collect();
            let candidate = flow.may_speculate(instruction, &access)
                && producers.iter().all(|other| moved.contains(other))
                && (producers.is_empty() || access.has_long_latency())
                && flags_unread_after(
                    &access,
                    original[original.iter().position(|&i| i == index).unwrap() + 1..]
                        .iter()
                        .map(|&other| accesses(other)),
                    &flow.tested,
                );
            let landing = candidate
                .then(|| {
                    receivers.iter().find_map(|&to| {
                        let path_clear = between(&successors, to, from)
                            .into_iter()
                            .flat_map(|block| bodies[block].iter().chain(&blocks[block].branch))
                            .chain(&blocks[to].branch)
                            .chain(&bodies[from][..at])
                            .all(|&other| !access.conflicts(&accesses(other)));
                        if !path_clear {
                            return None;
                        }
                        let mut earliest = bodies[to].len();
                        while earliest > 0 && !access.conflicts(&accesses(bodies[to][earliest - 1]))
                        {
                            earliest -= 1;
                        }
                        (earliest..=bodies[to].len()).find_map(|place| {
                            let after = bodies[to][place..].iter().map(|&other| accesses(other));
                            if !flags_unread_after(&access, after, &flow.tested) {
                                return None;
                            }
                            // The statement the moved instruction precedes in
                            // the program as it was: the first of the rest of
                            // the receiver's body that was there, its branch,
                            // or its end.
                            let next = bodies[to][place..]
                                .iter()
                                .copied()
                                .find(|other| blocks[to].body.contains(other))
                                .or(blocks[to].branch)
                                .unwrap_or(blocks[to].end);
                            access
                                .writes
                                .iter()
                                .filter(|(name, _)| !is_flag(name))
                                .all(|(name, mask)| flow.dead(next, index, name, *mask))
                                .then_some((to, place))
                        })
                    })
                })
                .flatten();
            match landing {
                Some((to, place)) => {
                    bodies[from].remove(at);
                    bodies[to].insert(place, index);
                    moved.insert(index);
                    donors[from] = true;
                }
                None => at += 1,
            }
        }
    }
    if !donors.contains(&true) {
        return None;
    }
    Some((
        arrange(&blocks, &bodies),
        SpeculatedLoop {
            header: name,
            blocks: blocks.iter().map(|block| block.label.clone()).collect(),
            donors,
        },
    ))
}

/// The loop's statements with each block's instructions in their new order:
/// labels and the other statements that are not instructions stay where
/// they were, the block's instructions take the place of its first one (or
/// go before its branch when it had none), and the branch stays last.
fn arrange(blocks: &[Block], bodies: &[Vec<usize>]) -> Vec<usize> {
    let mut arranged = Vec::new();
    for (block, info) in blocks.iter().enumerate() {
        let first = info.body.first().copied();
        let insert = first.or(info.branch).unwrap_or(info.end);
        let original: BTreeSet<usize> = info.body.iter().copied().collect();
        for index in info.start..info.end {
            if index == insert {
                arranged.extend(&bodies[block]);
            }
            if !original.contains(&index) {
                arranged.push(index);
            }
        }
        if insert == info.end {
            arranged.extend(&bodies[block]);
        }
    }
    arranged
}

/// Whether nothing reads the tested flags `access` writes, in the
/// instructions that follow it, `after`: for each one, the first of them
/// that touches it writes it. A flag no test in the program reads (`tested`)
/// needs nothing.
fn flags_unread_after(
    access: &Access,
    after: impl Iterator<Item = Access>,
    tested: &BTreeSet<String>,
) -> bool {
    let mut open: BTreeSet<&str> = access
        .writes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| is_tested_flag(name) && tested.contains(*name))
        .collect();
    for other in after {
        if open.is_empty() {
            break;
        }
        if open
            .iter()
            .any(|flag| other.reads.iter().any(|(name, _)| name == flag))
        {
            return false;
        }
        open.retain(|flag| !other.writes.iter().any(|(name, _)| name == flag));
    }
    open.is_empty()
}

/// What an instruction reads and writes: registers by lowercase name with
/// the fields it touches, the implicit ones and the status flags included
/// (`analysis::prune::expand`), memory and the clip flags.
struct Access {
    reads: Vec<(String, u8)>,
    writes: Vec<(String, u8)>,
    loads: bool,
    stores: bool,
    clip: bool,
}

/// Every field of a register: an operand that names none touches all.
fn fields(mask: u8) -> u8 {
    if mask == 0 { 0xff } else { mask }
}

impl Access {
    fn of(instruction: &Instruction) -> Self {
        let mut access = Access {
            reads: Vec::new(),
            writes: Vec::new(),
            loads: false,
            stores: false,
            clip: false,
        };
        for operand in crate::analysis::prune::expand(instruction).operands {
            match operand.class {
                class::CLIP | class::CLIP_ALL => access.clip = true,
                class::MEMORY => {
                    access.loads = true;
                    access.stores = true;
                }
                _ => {}
            }
            if let Some(name) = operand.register {
                let entry = (name.to_ascii_lowercase(), fields(operand.mask));
                match operand.direction {
                    Direction::Write => access.writes.push(entry),
                    _ => access.reads.push(entry),
                }
            }
        }
        for operand in &instruction.operands {
            if let Some(name) = &operand.forced_register {
                let entry = (name.to_ascii_lowercase(), 0xff);
                match operand.direction {
                    Direction::Write => access.writes.push(entry),
                    _ => access.reads.push(entry),
                }
            }
        }
        let metadata = instruction.metadata;
        if metadata.is_memory() {
            if metadata.is_store() {
                access.stores = true;
            } else {
                access.loads = true;
            }
        }
        if metadata.order_flags & order::WRITES_MEMORY != 0 {
            access.stores = true;
        }
        if metadata.order_flags & order::READS_MEMORY != 0 {
            access.loads = true;
        }
        access
    }

    fn writes_name(&self, name: &str) -> bool {
        self.writes.iter().any(|(other, _)| other == name)
    }

    fn reads_name(&self, name: &str) -> bool {
        self.reads.iter().any(|(other, _)| other == name)
    }

    /// Whether `other` writes a register this instruction reads: a producer.
    /// The tested flags are left to [`flags_unread_after`].
    fn reads_what(&self, other: &Access) -> bool {
        self.reads
            .iter()
            .any(|(name, _)| !is_tested_flag(name) && other.writes_name(name))
    }

    /// Whether this instruction may not move above `other`: a register
    /// dependency either way (the sticky flags included, the tested flags
    /// left to [`flags_unread_after`]), or a load above a store.
    fn conflicts(&self, other: &Access) -> bool {
        self.writes
            .iter()
            .filter(|(name, _)| !is_tested_flag(name))
            .any(|(name, _)| other.reads_name(name) || other.writes_name(name))
            || self.reads_what(other)
            || (self.loads && other.stores)
            || (self.stores && (other.loads || other.stores))
            || (self.clip && other.clip)
    }

    /// A division or EFU instruction: its result lands in Q or P.
    fn has_long_latency(&self) -> bool {
        self.writes_name("q") || self.writes_name("p")
    }
}

/// The program's statements as a flow graph, for the search that proves a
/// register dead.
struct Flow<'a> {
    statements: &'a [parser::Statement],
    labels: &'a BTreeMap<String, usize>,
    /// The registers the exits declare as outputs, lowercase.
    outputs: BTreeSet<String>,
    /// Every register a declaration names, lowercase.
    declared: BTreeSet<String>,
    /// The tested flags (`is_tested_flag`) some instruction reads.
    tested: BTreeSet<String>,
    /// Whether a flag test reads the sticky flags.
    sticky_tested: bool,
}

impl<'a> Flow<'a> {
    fn new(statements: &'a [parser::Statement], labels: &'a BTreeMap<String, usize>) -> Self {
        let mut flow = Flow {
            statements,
            labels,
            outputs: BTreeSet::new(),
            declared: BTreeSet::new(),
            tested: BTreeSet::new(),
            sticky_tested: false,
        };
        for instruction in statements.iter().filter_map(instruction_of) {
            match instruction.synthetic() {
                Some(declaration @ (Synthetic::Input | Synthetic::Output)) => {
                    for operand in &instruction.operands {
                        for name in [&operand.register, &operand.forced_register]
                            .into_iter()
                            .flatten()
                        {
                            let name = name.to_ascii_lowercase();
                            if declaration == Synthetic::Output {
                                flow.outputs.insert(name.clone());
                            }
                            flow.declared.insert(name);
                        }
                    }
                }
                Some(_) => {}
                None => {
                    let access = Access::of(instruction);
                    for (name, _) in &access.reads {
                        if is_tested_flag(name) {
                            flow.tested.insert(name.clone());
                        }
                    }
                    let test = instruction
                        .operands
                        .iter()
                        .any(|operand| operand.class == class::STATUS_TEST);
                    if test && (access.reads_name(STICKY_ALU) || access.reads_name(STICKY_DIVIDE)) {
                        flow.sticky_tested = true;
                    }
                }
            }
        }
        flow
    }

    /// Whether `instruction` may run on paths that did not run it: work that
    /// `hoist` could move, which tests no flag, touches no clip flag, writes
    /// no sticky flag, and writes registers it can be proven dead on: by
    /// name, not hardware registers, and none a declaration names.
    fn may_speculate(&self, instruction: &Instruction, access: &Access) -> bool {
        movable(instruction)
            && !access.clip
            && !access.reads.iter().any(|(name, _)| is_tested_flag(name))
            && !access.writes_name(STICKY_ALU)
            && !access.writes_name(STICKY_DIVIDE)
            && !(self.sticky_tested && (access.writes_name(ALU) || access.writes_name(DIVIDE)))
            && access
                .writes
                .iter()
                .filter(|(name, _)| !is_flag(name))
                .all(|(name, _)| !is_hardware(name) && !self.declared.contains(name))
            && instruction
                .operands
                .iter()
                .all(|operand| operand.forced_register.is_none())
    }

    /// Whether the fields `mask` of `name` are dead at statement `start` on
    /// every path that does not reach statement `old` first: no read of one
    /// before every one is written again. A path that comes back to `start`
    /// ends there, where the moved instruction writes them again. Unknown
    /// flow (raw code, register branches, calls) counts as a read, and so
    /// does an exit, of the outputs it declares.
    fn dead(&self, start: usize, old: usize, name: &str, mask: u8) -> bool {
        let mut seen = BTreeSet::new();
        let mut pending = vec![(start, mask, true)];
        while let Some((index, live, first)) = pending.pop() {
            if index == old || (index == start && !first) || !seen.insert((index, live)) {
                continue;
            }
            let Some(statement) = self.statements.get(index) else {
                if self.outputs.contains(name) {
                    return false;
                }
                continue;
            };
            let instruction = match &statement.kind {
                StatementKind::Label(_) => {
                    pending.push((index + 1, live, false));
                    continue;
                }
                StatementKind::Directive(
                    parser::Directive::Raw { .. }
                    | parser::Directive::EndRaw
                    | parser::Directive::RawLoop { .. }
                    | parser::Directive::EndRawLoop,
                )
                | StatementKind::RawText(_) => return false,
                StatementKind::Directive(_) => {
                    pending.push((index + 1, live, false));
                    continue;
                }
                StatementKind::Instruction(instruction) => instruction,
            };
            if instruction.mode != ParseMode::Vcl {
                return false;
            }
            match instruction.synthetic() {
                Some(Synthetic::Exit | Synthetic::ExitMacro | Synthetic::Return) => {
                    if self.outputs.contains(name) {
                        return false;
                    }
                    continue;
                }
                Some(Synthetic::Call | Synthetic::EndCall | Synthetic::Unknown) => return false,
                Some(_) => {
                    pending.push((index + 1, live, false));
                    continue;
                }
                None => {}
            }
            let access = Access::of(instruction);
            if access
                .reads
                .iter()
                .any(|(other, fields)| other == name && fields & live != 0)
            {
                return false;
            }
            let left = access
                .writes
                .iter()
                .filter(|(other, _)| other == name)
                .fold(live, |left, (_, fields)| left & !fields);
            if left == 0 {
                continue;
            }
            if instruction.metadata.is_branch() {
                if instruction.metadata.subtype & (REGISTER | LINK) != 0 {
                    return false;
                }
                let Some(&target) = branch_target(instruction).and_then(|t| self.labels.get(&t))
                else {
                    return false;
                };
                pending.push((target, left, false));
                if is_conditional_branch(instruction) {
                    pending.push((index + 1, left, false));
                }
            } else {
                pending.push((index + 1, left, false));
            }
        }
        true
    }
}

/// A hardware register named directly (`vf05`, `vi01`).
fn is_hardware(name: &str) -> bool {
    let digits = name.strip_prefix("vf").or_else(|| name.strip_prefix("vi"));
    digits.is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clipping pass as `hoist` leaves it: the second test's subtraction
    /// and FSAND and the latch's steps at the end of the header, and the
    /// intersection after the second test's branch.
    const PASS: &str = ".syntax new\n\
                        .init_vf_all\n\
                        .init_vi_all\n\
                        --enter\n\
                        in_vi src(vi01)\n\
                        in_vi count(vi02)\n\
                        in_vi dst(vi03)\n\
                        in_vf limit(vf02)\n\
                        --endenter\n\
                        lpass:\n\
                        lq.xyzw cur,0(src)\n\
                        lq.xyzw nxt,1(src)\n\
                        mul.w dcur,cur,limit[w]\n\
                        mul.w dnxt,nxt,limit[w]\n\
                        mul.w cut,dcur,dnxt[w]\n\
                        sub.w diff,dcur,vf00[x]\n\
                        fsand flagA,2\n\
                        sub.w later,cut,vf00[x]\n\
                        fsand flagB,2\n\
                        iaddiu src,src,1\n\
                        isubiu count,count,1\n\
                        ibne flagA,vi00,lskip\n\
                        sq.xyzw cur,0(dst)\n\
                        iaddiu dst,dst,1\n\
                        lskip:\n\
                        ibeq flagB,vi00,lnext\n\
                        sub.w den,dcur,dnxt[w]\n\
                        div q,dcur[w],den[w]\n\
                        sub.xyzw edge,nxt,cur\n\
                        mulaq.xyzw acc,edge,q\n\
                        madd.xyzw mid,cur,vf00[w]\n\
                        sq.xyzw mid,0(dst)\n\
                        iaddiu dst,dst,1\n\
                        lnext:\n\
                        ibgtz count,lpass\n\
                        --exit\n\
                        --endexit\n";

    /// Each instruction of `program`, by its first operand, in order.
    fn order(program: &parser::Program) -> Vec<String> {
        program
            .statements
            .iter()
            .filter_map(instruction_of)
            .filter(|instruction| instruction.synthetic().is_none())
            .map(|instruction| {
                let first = instruction
                    .operands
                    .iter()
                    .find(|operand| !operand.implicit)
                    .map_or("", |operand| operand.text.as_str());
                format!(
                    "{} {}",
                    instruction.mnemonic.to_ascii_lowercase(),
                    first.to_ascii_lowercase()
                )
            })
            .collect()
    }

    fn at(order: &[String], instruction: &str) -> usize {
        order
            .iter()
            .position(|line| line == instruction)
            .unwrap_or_else(|| panic!("no {instruction} in {order:?}"))
    }

    fn speculated(source: &str) -> Option<Vec<String>> {
        let parsed = parser::parse(source).unwrap();
        speculate(&parsed, None).map(|speculated| order(&speculated.program))
    }

    #[test]
    fn the_division_and_its_first_work_move_into_the_header() {
        let order = speculated(PASS).expect("a move");
        let branch = at(&order, "ibne flaga");
        for moved in ["subw den", "div q", "sub edge"] {
            assert!(at(&order, moved) < branch, "{moved}: {order:?}");
        }
        // What reads Q stays, and so does the rest of the intersection.
        assert!(
            at(&order, "mulaq acc") > at(&order, "ibeq flagb"),
            "{order:?}"
        );
        // The difference lands after what it reads, and its flags before
        // the next FMAC instruction's.
        assert!(
            at(&order, "subw den") > at(&order, "mulw dnxt"),
            "{order:?}"
        );
        assert!(at(&order, "subw den") < at(&order, "mulw cut"), "{order:?}");
    }

    #[test]
    fn a_flag_writer_does_not_land_where_a_test_would_read_it() {
        // Its subtraction reads the second test's difference, the header's
        // last flag writer: below that, the second test's FSAND would read
        // it, and below the FSAND nothing writes the flags again.
        let source = PASS.replace(
            "ibeq flagB,vi00,lnext\n",
            "ibeq flagB,vi00,lnext\nsub.w scaled,vf00,later\nsq.w scaled,50(dst)\n",
        );
        let order = speculated(&source).expect("a move");
        assert!(
            at(&order, "sub scaled") > at(&order, "ibeq flagb"),
            "{order:?}"
        );
    }

    #[test]
    fn a_division_stays_when_q_is_read_after_the_loop() {
        let source = PASS.replace(
            "--exit\n",
            "mulq.xyzw scale,limit,q\nsq.xyzw scale,300(vi00)\n--exit\n",
        );
        let order = speculated(&source).expect("a move");
        assert!(at(&order, "div q") > at(&order, "ibeq flagb"), "{order:?}");
        assert!(
            at(&order, "subw den") < at(&order, "ibne flaga"),
            "{order:?}"
        );
    }
}
