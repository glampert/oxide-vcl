//! Port of the original linear table optimizer core.
//!
//! The original builds a work table per strategy, queues every instruction
//! with per-component register locks, and runs one of two "wranglers": B or
//! A. Each wrangler repeatedly rebuilds a candidate list from dynamic
//! statistics, ranks it with a strategy callback, searches a legal line with
//! the unified checker and its register-lock checks, and inserts or
//! backtracks. Extents are then reduced.
//!
//! Register values are modelled exactly as the original lock records: each
//! register component has a lock list, each write is an output lock with its
//! users, and lifetime windows forbid overlapping values of the
//! same register. There are no separate anti-dependence edges.
//!
//! Linear tables have period zero. A loop table (`Table::new_loop`) folds
//! absolute lines into `period` dispatch rows; see
//! `docs/LPGEN_PORT.md` for the loop-mode branches ported so far. The
//! wall-clock watchdog is replaced by a deterministic step budget.
#![allow(clippy::needless_range_loop)]

use crate::isa::instruction::subtype;
use crate::scheduler::schedule::{self, Candidate, PriorityMetrics, Strategy};
use std::collections::BTreeMap;

pub const IMPOSSIBLE: i32 = 30000;

const SLOT_UNAVAILABLE: u32 = 0x40;
const PAIRING: u32 = 0x80;
const MOVE_NEEDED: u32 = 0x100;
const HORIZON: u32 = 0x8000;
const POSITION: u32 = 0x10000;
const LIST_MINIMUM: u32 = 0x80000;
const FORCED: u32 = 0x100000;

#[derive(Clone, Debug, Default)]
pub struct Register {
    /// Number of components.
    pub parts: u8,
    /// Global hardware ID for ACC/I/Q/P/flags; `None` for VF/VI.
    pub global_id: Option<u8>,
    /// Throughput-tracked resource, such as Q and P.
    pub throughput_tracked: bool,
    /// Nonblocking resource ID; zero for interlocked registers.
    pub nonblocking_id: u8,
    /// Register pool: 1 VF, 2 VI, 0 hardware.
    pub pool: u8,
    /// Physical register when already mapped (forced, external or source).
    pub physical: Option<usize>,
    /// Components live through the table unwritten (regref `t` bits); the
    /// incoming value is an external output.
    pub through: u8,
}

impl Register {
    /// A VF/VI pool register, pinned or not, may be renamed
    /// by a loop move (its regref pool is 8 or 9).
    pub fn movable(&self) -> bool {
        self.pool != 0 && self.global_id.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Local { instruction: usize, operand: usize },
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Read,
    Write,
}

#[derive(Clone, Debug)]
pub struct Operand {
    pub register: usize,
    pub direction: Direction,
    pub mask: u8,
    /// Output latency.
    pub latency: i8,
    /// Descriptor blocking byte.
    pub blocking: bool,
    /// Read offset, one for early-sampled branch sources.
    pub input_offset: i8,
    /// Per-component producer of a read.
    pub sources: [Option<Source>; 4],
    /// Write components whose value is observed outside the table.
    pub escapes: u8,
    /// A written value has users anywhere (its user list is nonempty).
    pub observed: bool,
}

/// First descriptor operand when it is an output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstOutput {
    pub vector: bool,
    /// Problem register, or `None` for the pool zero register.
    pub register: Option<usize>,
    pub mask: u8,
    pub latency: i8,
}

#[derive(Clone, Debug, Default)]
pub struct Instruction {
    /// Original sequence number.
    pub order: i32,
    /// Allowed pipes: 1 upper, 2 lower, 3 either.
    pub pipes: u8,
    /// Issue throughput.
    pub throughput: u8,
    /// Functional-unit bitset.
    pub units: u16,
    pub branch: bool,
    /// Memory kind: 0 none, else `subtype::memory::LOAD` or `STORE`.
    pub memory: u8,
    /// Larger of the two minimum-position bounds.
    pub minimum_position: i32,
    /// Larger of the two end-distance bounds.
    pub end_distance: i32,
    /// A protected read operand: reduction never moves the
    /// instruction later.
    pub protected_read: bool,
    /// A protected write operand: reduction never moves it earlier.
    pub protected_write: bool,
    pub forced_predecessors: Vec<usize>,
    pub forced_successors: Vec<usize>,
    /// ACC special case: forced predecessors are not required.
    pub acc_special: bool,
    /// Loop side-effect category: 0 none, 1 store,
    /// 2 critical (an escaping or irreversible effect).
    pub side_effect: u8,
    /// A move synthesized by the loop optimizer.
    pub is_move: bool,
    /// A copy found by move detection (dual-issue `vmove` or
    /// integer `imove`). Like a synthesized move it has a move ID,
    /// so a loop gives it a pipe before placement, and a copy
    /// preferring the upper pipe while the lower-only count fills the period
    /// restarts the search.
    pub dual_copy: bool,
    /// Move chain depth.
    pub move_depth: i32,
    /// Latest useful issue line of a move.
    pub move_target: i32,
    /// A move whose every reader was retargeted may be dragged a full
    /// period.
    pub full_period: bool,
    pub first_output: Option<FirstOutput>,
    pub operands: Vec<Operand>,
    /// Affine base reads of an induction setter's new value (mode 2
    /// locks): no timing constraint, but the original's static chain
    /// statistics count them as (producer, operand) links.
    pub stat_links: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct Problem {
    pub registers: Vec<Register>,
    pub instructions: Vec<Instruction>,
    /// Table rows.
    pub horizon: i32,
    /// External minimum length.
    pub minimum_length: i32,
    /// Staggered memory access option (`-S`).
    pub stagger: bool,
    /// Mnemonics for diagnostics only.
    pub names: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Statics {
    pub height: i32,
    pub height_links: i32,
    pub depth: i32,
    pub depth_links: i32,
    pub fan_out: i32,
    pub weight: i32,
    pub chain: i32,
    pub depth_sum: i32,
}

/// Static chain statistics over source order.
pub fn statics(problem: &Problem) -> Vec<Statics> {
    let n = problem.instructions.len();
    let mut stats = vec![Statics::default(); n];
    let local_reads = |insn: &Instruction| {
        insn.operands
            .iter()
            .filter(|o| o.direction == Direction::Read)
            .flat_map(|o| {
                (0..4).filter_map(move |c| match o.sources[c] {
                    Some(Source::Local {
                        instruction,
                        operand,
                    }) if o.mask & (1 << c) != 0 => Some((instruction, operand, o.input_offset)),
                    _ => None,
                })
            })
            .chain(insn.stat_links.iter().map(|&(p, o)| (p, o, 0)))
            .collect::<Vec<_>>()
    };
    for i in 0..n {
        for (producer, operand, offset) in local_reads(&problem.instructions[i]) {
            let latency = i32::from(problem.instructions[producer].operands[operand].latency);
            let depth = stats[producer].depth + latency + i32::from(offset);
            let links = stats[producer].depth_links + 1;
            stats[i].depth = stats[i].depth.max(depth);
            stats[i].depth_links = stats[i].depth_links.max(links);
        }
        for (producer, operand, offset) in local_reads(&problem.instructions[i]) {
            let latency = i32::from(problem.instructions[producer].operands[operand].latency);
            let sum = latency + stats[producer].depth_sum + i32::from(offset);
            stats[i].depth_sum = stats[i].depth_sum.max(sum);
        }
    }
    for i in (0..n).rev() {
        for (producer, operand, offset) in local_reads(&problem.instructions[i]) {
            let latency = i32::from(problem.instructions[producer].operands[operand].latency);
            let height = stats[i].height + latency + i32::from(offset);
            let links = stats[i].height_links + 1;
            stats[producer].height = stats[producer].height.max(height);
            stats[producer].height_links = stats[producer].height_links.max(links);
        }
    }
    let references = |consumer: usize, producer: usize| {
        local_reads(&problem.instructions[consumer])
            .iter()
            .any(|&(p, _, _)| p == producer)
    };
    for consumer in (0..n).rev() {
        for producer in (0..consumer).rev() {
            if references(consumer, producer) {
                stats[producer].fan_out += 1;
                stats[producer].weight += stats[consumer].weight + 1;
                stats[producer].chain = stats[consumer].chain + 1;
            }
        }
    }
    stats
}

/// Linear lower bound and table horizon.
pub fn bounds(problem: &Problem, statics: &[Statics]) -> (i32, i32) {
    let (mut upper, mut lower, mut flexible) = (0, 0, 0);
    let mut units = [0i32; 9];
    for insn in &problem.instructions {
        for (bit, count) in units.iter_mut().enumerate() {
            if insn.units >> bit & 1 != 0 {
                *count += 2;
            }
        }
        match insn.pipes {
            3 => flexible += 1,
            p if p & 2 == 0 => upper += 1,
            _ => lower += 1,
        }
    }
    let mut dependency = 0;
    let mut users: Vec<Vec<[Vec<usize>; 4]>> = problem
        .instructions
        .iter()
        .map(|insn| vec![[vec![], vec![], vec![], vec![]]; insn.operands.len()])
        .collect();
    for (index, insn) in problem.instructions.iter().enumerate() {
        for operand in insn
            .operands
            .iter()
            .filter(|o| o.direction == Direction::Read)
        {
            for part in 0..4 {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                if let Some(Source::Local {
                    instruction,
                    operand: setter,
                }) = operand.sources[part]
                {
                    let latency = problem.instructions[instruction].operands[setter].latency;
                    dependency =
                        dependency.max(i32::from(operand.input_offset) + i32::from(latency));
                    users[instruction][setter][part].push(index);
                }
            }
        }
    }
    let mut chain = 0;
    for (index, insn) in problem.instructions.iter().enumerate() {
        for (o, operand) in insn.operands.iter().enumerate() {
            if operand.direction != Direction::Write {
                continue;
            }
            for (part, readers) in users[index][o].iter().enumerate() {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                let (mut flex, mut up, mut lo) = (0, 0, 0);
                for &reader in readers {
                    match problem.instructions[reader].pipes {
                        3 => flex += 1,
                        1 => up += 1,
                        _ => lo += 1,
                    }
                }
                let (mut high, low) = if up < lo { (lo, up) } else { (up, lo) };
                flex -= high - low;
                if flex > 0 {
                    high += (flex + 1) >> 1;
                }
                if high != 0 {
                    high -= 1;
                }
                chain = chain.max(high + i32::from(operand.latency));
            }
        }
    }
    for _ in 0..flexible {
        if upper < lower {
            upper += 1;
        } else {
            lower += 1;
        }
    }
    let mut bound = 0;
    if upper > 0 {
        bound = upper;
    }
    bound = bound.max(lower).max(chain).max(dependency);
    for count in units {
        bound = bound.max((count + 1) >> 1);
    }
    let horizon = (bound * 4).clamp(64, 0x4000);
    let max_depth = statics.iter().map(|s| s.depth).max().unwrap_or(0);
    let max_height = statics.iter().map(|s| s.height).max().unwrap_or(0);
    if bound <= max_depth {
        bound = max_depth + 1;
    }
    if bound <= max_height {
        bound = max_height + 1;
    }
    if let Some(last) = problem.instructions.len().checked_sub(1)
        && problem.instructions[last].branch
    {
        bound = bound.max(statics[last].depth + 2);
    }
    (bound, horizon)
}

/// Loop lower bound: the linear terms
/// without depth/height/branch depth, where a read with no earlier local
/// writer takes the body's last writer as its (previous-iteration) producer,
/// plus the throughput of each throughput-tracked register. At least two.
pub fn loop_bound(problem: &Problem) -> i32 {
    let (mut upper, mut lower, mut flexible) = (0, 0, 0);
    let mut units = [0i32; 9];
    let mut throughput: BTreeMap<(usize, usize), i32> = BTreeMap::new();
    for insn in &problem.instructions {
        for (bit, count) in units.iter_mut().enumerate() {
            if insn.units >> bit & 1 != 0 {
                *count += 2;
            }
        }
        match insn.pipes {
            3 => flexible += 1,
            p if p & 2 == 0 => upper += 1,
            _ => lower += 1,
        }
        for operand in insn.operands.iter().filter(|o| {
            o.direction == Direction::Write && problem.registers[o.register].throughput_tracked
        }) {
            for part in (0..4).filter(|part| operand.mask & (1 << part) != 0) {
                *throughput.entry((operand.register, part)).or_default() +=
                    i32::from(insn.throughput);
            }
        }
    }
    // Last writer of each register component.
    let mut last: BTreeMap<(usize, usize), (usize, usize)> = BTreeMap::new();
    for (index, insn) in problem.instructions.iter().enumerate() {
        for (o, operand) in insn.operands.iter().enumerate() {
            if operand.direction == Direction::Write {
                for part in (0..4).filter(|part| operand.mask & (1 << part) != 0) {
                    last.insert((operand.register, part), (index, o));
                }
            }
        }
    }
    let mut dependency = 0;
    let mut users: Vec<Vec<[Vec<usize>; 4]>> = problem
        .instructions
        .iter()
        .map(|insn| vec![[vec![], vec![], vec![], vec![]]; insn.operands.len()])
        .collect();
    for (index, insn) in problem.instructions.iter().enumerate() {
        for operand in insn
            .operands
            .iter()
            .filter(|o| o.direction == Direction::Read)
        {
            for part in (0..4).filter(|part| operand.mask & (1 << part) != 0) {
                let setter = match operand.sources[part] {
                    Some(Source::Local {
                        instruction,
                        operand,
                    }) => Some((instruction, operand)),
                    _ => last.get(&(operand.register, part)).copied(),
                };
                if let Some((instruction, setter)) = setter {
                    let latency = problem.instructions[instruction].operands[setter].latency;
                    dependency =
                        dependency.max(i32::from(operand.input_offset) + i32::from(latency));
                    users[instruction][setter][part].push(index);
                }
            }
        }
    }
    let mut chain = 0;
    for (index, insn) in problem.instructions.iter().enumerate() {
        for (o, operand) in insn.operands.iter().enumerate() {
            if operand.direction != Direction::Write {
                continue;
            }
            for (part, readers) in users[index][o].iter().enumerate() {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                let (mut flex, mut up, mut lo) = (0, 0, 0);
                for &reader in readers {
                    match problem.instructions[reader].pipes {
                        3 => flex += 1,
                        1 => up += 1,
                        _ => lo += 1,
                    }
                }
                let (mut high, low) = if up < lo { (lo, up) } else { (up, lo) };
                flex -= high - low;
                if flex > 0 {
                    high += (flex + 1) >> 1;
                }
                if high != 0 {
                    high -= 1;
                }
                chain = chain.max(high + i32::from(operand.latency));
            }
        }
    }
    for _ in 0..flexible {
        if upper < lower {
            upper += 1;
        } else {
            lower += 1;
        }
    }
    let mut bound = upper.max(0).max(lower).max(chain);
    bound = bound.max(throughput.values().copied().max().unwrap_or(0));
    bound = bound.max(dependency);
    for count in units {
        bound = bound.max((count + 1) >> 1);
    }
    bound.max(2)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Instruction(usize, usize),
    External,
}

#[derive(Clone, Debug)]
struct Lock {
    output: bool,
    list: usize,
    /// Why a movable input needs a move: 1 carried overrun,
    /// 2 lifetime overlap, 3 longer than a period.
    reason: u8,
    owner: Owner,
    latency: i8,
    blocking: bool,
    throughput: i8,
    external_output: u8,
    mode: u8,
    inserted: bool,
    line: i32,
    folded: i32,
    parent: Option<usize>,
    inout: Option<usize>,
    users: Vec<usize>,
    total_users: i32,
    pending_users: i32,
    user_count: i32,
    write: i32,
    ready: i32,
    first_use: i32,
    last_use: i32,
    end: i32,
    write_f: i32,
    end_f: i32,
    /// Reader pipe counts of an output lock's users: dual,
    /// upper only, lower only.
    sum46: i32,
    sum48: i32,
    sum4a: i32,
    /// An input lock's reader pipe:
    /// 0 lower only, 1 upper only, 2 both; `None` for a part the same
    /// instruction already reads. Linking adds it to the producer's sums.
    reader_pipes: Option<u8>,
    retry: i32,
    retry_flag: bool,
}

/// Link a reader to its producer.
fn link_input(locks: &mut [Lock], input: usize, producer: usize) {
    let pipes = locks[input].reader_pipes;
    let p = &mut locks[producer];
    p.total_users += 1;
    p.pending_users += 1;
    match pipes {
        Some(0) => p.sum4a += 1,
        Some(1) => p.sum48 += 1,
        Some(2) => p.sum46 += 1,
        _ => {}
    }
    locks[input].parent = Some(producer);
}

/// Unlink a reader from its producer.
fn unlink_input(locks: &mut [Lock], input: usize) {
    let Some(producer) = locks[input].parent.take() else {
        return;
    };
    let pipes = locks[input].reader_pipes;
    let p = &mut locks[producer];
    p.total_users -= 1;
    p.pending_users -= 1;
    match pipes {
        Some(0) => p.sum4a -= 1,
        Some(1) => p.sum48 -= 1,
        Some(2) => p.sum46 -= 1,
        _ => {}
    }
}

/// The first read of each register part in an instruction
/// counts the instruction's issue pipes.
fn reader_pipes(pipes: u8) -> u8 {
    match pipes {
        1 => 1,
        2 => 0,
        _ => 2,
    }
}

impl Lock {
    fn new(output: bool, list: usize, owner: Owner) -> Self {
        Self {
            output,
            list,
            reason: 0,
            owner,
            latency: 0,
            blocking: false,
            throughput: 0,
            external_output: 0,
            mode: 0,
            inserted: false,
            line: -1,
            folded: -1,
            parent: None,
            inout: None,
            users: Vec::new(),
            total_users: 0,
            pending_users: 0,
            user_count: 0,
            write: -1,
            ready: -1,
            first_use: -1,
            last_use: -1,
            end: -1,
            write_f: -1,
            end_f: -1,
            sum46: 0,
            sum48: 0,
            sum4a: 0,
            reader_pipes: None,
            retry: 0,
            retry_flag: false,
        }
    }

    fn instruction(&self) -> Option<usize> {
        match self.owner {
            Owner::Instruction(i, _) => Some(i),
            Owner::External => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LockList {
    minimum: i32,
    /// Inserted output locks, most recent first.
    outputs: Vec<usize>,
    /// Inserted output locks sorted by line.
    sorted: Vec<usize>,
    pending_outputs: i32,
    constant: bool,
    external_owner: bool,
    external_outputs: u8,
    /// First external output lock.
    first_external_output: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub line: i32,
    pub pipe: u8,
}

#[derive(Clone, Debug, Default)]
struct Slot {
    line: i32,
    pipe: u8,
    serial: i32,
    penalty: i32,
    threshold: i32,
    delay: i32,
    earliest: i32,
    asap: i32,
    latest: i32,
    min_latest: i32,
    max_asap: i32,
    height: i32,
    depth: i32,
    /// Lock index per operand component.
    locks: Vec<[Option<usize>; 4]>,
}

/// Work counters of one search: outer steps (the current budget unit),
/// placements, undos, position checks and to-do entries
/// ranked.
#[derive(Clone, Copy, Debug, Default)]
pub struct Work {
    pub steps: usize,
    pub inserts: usize,
    pub undos: usize,
    pub checks: usize,
    pub ranked: usize,
}

#[derive(Clone, Debug)]
pub struct Table<'a> {
    /// Copied on the first synthesized loop move.
    problem: std::borrow::Cow<'a, Problem>,
    statics: Vec<Statics>,
    locks: Vec<Lock>,
    lists: Vec<LockList>,
    /// `(register, part) -> list`.
    list_of: Vec<[Option<usize>; 4]>,
    slots: Vec<Slot>,
    rows: Vec<[Option<usize>; 2]>,
    todo: Vec<usize>,
    done: Vec<usize>,
    candidates: Vec<usize>,
    serial: i32,
    max_asap: i32,
    cursor: i32,
    end: i32,
    first_failure: Option<usize>,
    /// Reader component statistics per register part.
    reader_min: Vec<[i32; 4]>,
    reader_max: Vec<[i32; 4]>,
    /// Effective table end during extent reduction.
    end_limit: i32,
    /// Table start.
    start: i32,
    /// Undone instructions in order, for diagnostics.
    undo_log: Vec<usize>,
    /// Inserted synchronization instructions.
    caps: Vec<Cap>,
    /// Dispatch-entry timestamps and skip bytes.
    stamps: Vec<i32>,
    skipped: Vec<bool>,
    /// Loop period; zero for a linear table.
    period: i32,
    /// Known-safe iteration count.
    known_safe: i32,
    /// The inserted conditional branch of a loop.
    branch_placed: Option<usize>,
    /// Steps of a search that ran out of budget (diagnostics).
    failed_trace: Vec<(usize, i32, u8)>,
    /// Work done by the current search, for watchdog calibration.
    pub work: Work,
    /// To-do length when the search started.
    pub todo_at_start: usize,
    /// Side-effect line floors without a placed branch.
    effect_floors: [i32; 2],
    /// A loop branch's reserved lower slot at row `period - 2`: only that branch may use it, and the
    /// branch may use no other slot.
    reserved: Option<(i32, usize)>,
    /// Live synthesized moves in creation order.
    moves: Vec<usize>,
    /// Instructions issuable in either, upper only, lower only pipe.
    issue_counts: [i32; 3],
    /// Move pipe flags.
    move_flag_ac: bool,
    move_flag_ad: bool,
    /// Wrangler A allows a move for an overlapping input.
    overlap_moves: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapKind {
    WaitQ,
    WaitP,
    Abs,
}

/// A synchronization instruction placed by the cap pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    pub row: i32,
    pub slot: u8,
    pub kind: CapKind,
    /// Producer instruction and the operand whose value the cap waits on.
    pub producer: usize,
    pub operand: Option<usize>,
    pub mask: u8,
    /// Problem register read by an ABS cap; `None` for WAITQ/WAITP.
    pub source: Option<usize>,
}

/// Compression result: kept physical rows map to their original indices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compressed {
    pub rows: Vec<[Option<usize>; 2]>,
    pub caps: Vec<Option<Cap>>,
    pub skipped: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub strategy: Strategy,
    pub rows: Vec<[Option<usize>; 2]>,
    pub placements: Vec<Position>,
    /// Ordered `(instruction, line, pipe)` insertions, including undone ones.
    pub trace: Vec<(usize, i32, u8)>,
    /// Local allocation `(register, physical)` for VF then VI.
    pub allocation: Vec<(usize, usize)>,
}

/// Pool inputs for local allocation.
#[derive(Clone, Debug, Default)]
pub struct Pools {
    pub vf_allowed: Vec<u8>,
    pub vi_allowed: Vec<u8>,
    /// Unreferenced physical registers live through the table, with the
    /// components they carry.
    pub vf_through: Vec<(usize, u8)>,
    pub vi_through: Vec<(usize, u8)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    NoSchedule,
    Budget,
}

/// A write still in flight when an empty row is considered for removal.
struct Flight {
    index: usize,
    operand: usize,
    part: usize,
    remaining: i32,
}

/// An empty row being considered for removal, with the writes in flight and
/// the row where the rows below stop shifting.
struct EmptyRow {
    row: i32,
    now: i32,
    recovery: i32,
    flights: Vec<Flight>,
}

/// State of one move synthesis while the pending operands are retargeted.
struct MoveRetarget {
    mv: usize,
    old: usize,
    new: usize,
    parts: usize,
    latency: i8,
    /// The old register is also an output of the table.
    table_output: bool,
    /// Components fed by the move.
    moved: u8,
    /// Components of non-movable writes seen so far.
    fixed_writes: u8,
    full_period: bool,
}

/// How a placement attempt of the search ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// An instruction was placed.
    Inserted,
    /// A move was queued and the step starts over.
    Restart,
    /// The step failed and the search backtracks.
    Failed,
    /// The candidate is dropped and the next one is ranked.
    Discard,
}

fn interval_overlap(a1: i32, a2: i32, b1: i32, b2: i32) -> bool {
    if b1 < a1 { a1 <= b2 } else { b1 <= a2 }
}

impl<'a> Table<'a> {
    /// Queue every instruction.
    pub fn new(problem: &'a Problem, statics: &[Statics]) -> Self {
        Self::build(problem, statics, 0, 0)
    }

    /// A loop work table (built with the loop argument): `period`
    /// folded rows, start 0 and end `period`.
    pub fn new_loop(
        problem: &'a Problem,
        statics: &[Statics],
        period: i32,
        known_safe: i32,
    ) -> Self {
        Self::build(problem, statics, period, known_safe)
    }

    fn build(problem: &'a Problem, statics: &[Statics], period: i32, known_safe: i32) -> Self {
        let mut table = Table {
            problem: std::borrow::Cow::Borrowed(problem),
            statics: statics.to_vec(),
            locks: Vec::new(),
            lists: Vec::new(),
            list_of: vec![[None; 4]; problem.registers.len()],
            slots: vec![Slot::default(); problem.instructions.len()],
            rows: vec![[None, None]; problem.horizon.max(1) as usize],
            todo: Vec::new(),
            done: Vec::new(),
            candidates: Vec::new(),
            serial: 0,
            max_asap: -1,
            cursor: 0,
            end: -1,
            first_failure: None,
            reader_min: vec![[-1; 4]; problem.registers.len()],
            reader_max: vec![[-1; 4]; problem.registers.len()],
            end_limit: -1,
            start: 0,
            undo_log: Vec::new(),
            caps: Vec::new(),
            stamps: Vec::new(),
            skipped: Vec::new(),
            period,
            known_safe,
            branch_placed: None,
            failed_trace: Vec::new(),
            work: Work::default(),
            todo_at_start: 0,
            effect_floors: [0, 0],
            reserved: None,
            moves: Vec::new(),
            issue_counts: [0; 3],
            move_flag_ac: false,
            move_flag_ad: false,
            overlap_moves: false,
        };
        if period > 0 {
            table.rows = vec![[None, None]; period as usize];
            table.end = period;
            table.end_limit = period;
            table.reserved = problem
                .instructions
                .iter()
                .position(|insn| insn.branch)
                .map(|branch| (period - 2, branch));
        }

        // Register parts referenced by the table get lock lists. A part
        // written in the table is not constant.
        let mut written = vec![0u8; problem.registers.len()];
        let mut external_reads = vec![0u8; problem.registers.len()];
        for insn in &problem.instructions {
            for operand in &insn.operands {
                match operand.direction {
                    Direction::Write => written[operand.register] |= operand.mask,
                    Direction::Read => {
                        for part in 0..4 {
                            if operand.mask & (1 << part) != 0
                                && operand.sources[part] == Some(Source::External)
                            {
                                external_reads[operand.register] |= 1 << part;
                            }
                        }
                    }
                }
            }
        }
        table.add_lock_lists(problem, &written, &external_reads);

        for (index, insn) in problem.instructions.iter().enumerate() {
            table.slots[index].line = -1;
            table.slots[index].locks = vec![[None; 4]; insn.operands.len()];
            table.add_input_locks(problem, index);
            table.add_output_locks(problem, index);
            table.link_producers(problem, index);
            table.link_inout(problem, index);
            table.todo.push(index);
            if period > 0 {
                table.count_issue(index, 1);
            }
        }
        table
    }

    /// Give every referenced register part a lock list, plus an entry lock
    /// for external reads, throughput resources and registers live through.
    fn add_lock_lists(&mut self, problem: &Problem, written: &[u8], external_reads: &[u8]) {
        for (register, info) in problem.registers.iter().enumerate() {
            let mut referenced = written[register] | external_reads[register];
            for insn in &problem.instructions {
                for operand in &insn.operands {
                    if operand.register == register {
                        referenced |= operand.mask;
                    }
                }
            }

            for part in 0..usize::from(info.parts.min(4)) {
                if referenced & (1 << part) == 0 {
                    continue;
                }
                let list = self.lists.len();
                self.lists.push(LockList {
                    constant: written[register] & (1 << part) == 0,
                    ..LockList::default()
                });
                self.list_of[register][part] = Some(list);
                let through = info.through & (1 << part) != 0;
                // A throughput resource gets an entry lock only
                // in a linear table.
                let tracked = info.throughput_tracked && self.period == 0;
                if external_reads[register] & (1 << part) != 0 || tracked || through {
                    self.add_entry_lock(list, info.nonblocking_id == 0, through);
                }
            }
        }
    }

    /// Add the external entry output lock of a register part's list.
    fn add_entry_lock(&mut self, list: usize, blocking: bool, through: bool) {
        let lock = self.locks.len();
        let mut external = Lock::new(true, list, Owner::External);
        external.latency = 1;
        external.blocking = blocking;
        if through {
            let count = self.lists[list].external_outputs;
            self.lists[list].external_outputs = count + 1;
            external.external_output = count + 1;
            self.lists[list].first_external_output.get_or_insert(lock);
        }
        self.locks.push(external);
        self.lists[list].external_owner = true;
        self.lists[list].pending_outputs += 1;
        self.add_output_lock(lock);
    }

    /// Create the input locks of an instruction's read components.
    fn add_input_locks(&mut self, problem: &Problem, index: usize) {
        let insn = &problem.instructions[index];
        let mut counted: Vec<(usize, usize)> = Vec::new();

        for (o, operand) in insn.operands.iter().enumerate() {
            if operand.direction != Direction::Read {
                continue;
            }
            for part in 0..4 {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                let Some(list) = self.list_of[operand.register][part] else {
                    continue;
                };
                let lock = self.locks.len();
                let mut input = Lock::new(false, list, Owner::Instruction(index, o));
                input.latency = operand.input_offset;
                if !counted.contains(&(operand.register, part)) {
                    counted.push((operand.register, part));
                    input.reader_pipes = Some(reader_pipes(insn.pipes));
                }
                // A loop input of a pool register may be moved.
                if self.period > 0 && problem.registers[operand.register].movable() {
                    input.mode = 1;
                }
                self.locks.push(input);
                self.slots[index].locks[o][part] = Some(lock);
            }
        }
    }

    /// Create the output locks of an instruction's written components.
    fn add_output_locks(&mut self, problem: &Problem, index: usize) {
        let insn = &problem.instructions[index];

        for (o, operand) in insn.operands.iter().enumerate() {
            if operand.direction != Direction::Write {
                continue;
            }
            let register = &problem.registers[operand.register];
            for part in 0..4 {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                let Some(list) = self.list_of[operand.register][part] else {
                    continue;
                };
                let lock = self.locks.len();
                let mut output = Lock::new(true, list, Owner::Instruction(index, o));
                output.throughput = if register.throughput_tracked {
                    insn.throughput as i8
                } else {
                    1
                };
                output.latency = operand.latency;
                output.blocking = operand.blocking;
                self.locks.push(output);
                self.lists[list].pending_outputs += 1;
                if operand.escapes & (1 << part) != 0 {
                    let count = self.lists[list].external_outputs;
                    self.lists[list].external_outputs = count + 1;
                    self.locks[lock].external_output = count + 1;
                    self.lists[list].first_external_output.get_or_insert(lock);
                }
                self.slots[index].locks[o][part] = Some(lock);
            }
        }
    }

    /// Register reads with their producers, then in/out links.
    fn link_producers(&mut self, problem: &Problem, index: usize) {
        let insn = &problem.instructions[index];

        for (o, operand) in insn.operands.iter().enumerate() {
            if operand.direction != Direction::Read {
                continue;
            }
            for part in 0..4 {
                let Some(input) = self.slots[index].locks[o][part] else {
                    continue;
                };
                let producer = match operand.sources[part] {
                    Some(Source::Local {
                        instruction,
                        operand,
                    }) => self.slots[instruction].locks[operand][part],
                    _ => {
                        let list = self.locks[input].list;
                        self.lists[list]
                            .outputs
                            .iter()
                            .copied()
                            .find(|&l| self.locks[l].owner == Owner::External)
                    }
                };
                let Some(producer) = producer else {
                    continue;
                };
                self.locks[producer].blocking = operand.blocking;
                link_input(&mut self.locks, input, producer);
            }
        }
    }

    /// Link each read component to the output of the same register part that
    /// the same instruction writes.
    fn link_inout(&mut self, problem: &Problem, index: usize) {
        let insn = &problem.instructions[index];

        for (o, operand) in insn.operands.iter().enumerate() {
            if operand.direction != Direction::Read {
                continue;
            }
            for (w, write) in insn.operands.iter().enumerate() {
                if write.direction != Direction::Write || write.register != operand.register {
                    continue;
                }
                for part in 0..4 {
                    if operand.mask & write.mask & (1 << part) == 0 {
                        continue;
                    }
                    if let (Some(input), Some(output)) = (
                        self.slots[index].locks[o][part],
                        self.slots[index].locks[w][part],
                    ) {
                        self.locks[input].inout = Some(output);
                    }
                }
            }
        }
    }

    /// Per-pipe instruction counters.
    fn count_issue(&mut self, index: usize, delta: i32) {
        let pipes = self.insn(index).pipes;
        let class = if pipes == 3 {
            0
        } else if pipes & 1 == 0 {
            2
        } else {
            1
        };
        self.issue_counts[class] += delta;
    }

    fn insn(&self, index: usize) -> &Instruction {
        &self.problem.instructions[index]
    }

    /// Folded dispatch row and iteration ID of an absolute line.
    fn fold(&self, line: i32) -> (i32, i32) {
        if self.period == 0 || line < self.period {
            (line, 0)
        } else {
            (line % self.period, line / self.period)
        }
    }

    /// Inclusive interval overlap, cyclic in a loop table.
    fn overlap(&self, mut a1: i32, mut a2: i32, mut b1: i32, mut b2: i32) -> bool {
        let period = self.period;
        if period == 0 {
            return interval_overlap(a1, a2, b1, b2);
        }
        let (a_length, b_length) = (a2 - a1, b2 - b1);
        while period <= a1 {
            a1 -= period;
            a2 -= period;
        }
        while period <= b1 {
            b1 -= period;
            b2 -= period;
        }
        if period < a_length + b_length {
            return true;
        }
        if b1 < a1 {
            if b2 < a1 {
                return a2 - period >= b1;
            }
        } else if a2 < b1 {
            return b2 - period >= a1;
        }
        true
    }

    /// Next absolute line at or after `after`'s iteration whose folded row is
    /// `line`'s folded row (the loop branches of the output lock check and the unified checker).
    fn next_in_period(&self, line: i32, after: i32) -> i32 {
        let period = self.period;
        if period == 0 {
            return line;
        }
        let mut iteration = after / period;
        if line % period <= after % period {
            iteration += 1;
        }
        line % period + iteration * period
    }

    /// Lock statistics for a linear table.
    fn calc_stats(&mut self, lock: usize, ignore: Option<usize>) {
        let mut first_adjusted = IMPOSSIBLE;
        let mut first = IMPOSSIBLE;
        let mut last = -IMPOSSIBLE;
        let mut count: i32 = 0;
        for &user in &self.locks[lock].users {
            let u = &self.locks[user];
            if ignore.is_some() && u.instruction() == ignore {
                continue;
            }
            if u.mode == 2 {
                continue;
            }
            last = last.max(u.line);
            first = first.min(u.line);
            first_adjusted = first_adjusted.min(u.line - i32::from(u.latency));
            count += 1;
        }
        let constant = self.lists[self.locks[lock].list].constant;
        let period = self.period;
        // A constant list in a loop has no user window.
        if constant && period > 0 {
            count = 0;
        }
        let l = &mut self.locks[lock];
        l.user_count = count;
        let line = l.line;
        let latency = i32::from(l.latency);
        let external = l.owner == Owner::External;
        let (write, ready) = if external && period > 0 && count != 0 {
            // A loop-carried entry value is written when first needed.
            (first_adjusted, first_adjusted)
        } else if external {
            if latency != 0 {
                (if l.blocking { 0 } else { latency + line }, latency + line)
            } else {
                (0, 0)
            }
        } else if l.blocking {
            (line + 1, latency + line)
        } else {
            (latency + line, latency + line)
        };
        let _ = constant;
        let (first_use, last_use);
        if count == 0 {
            l.write = write;
            l.ready = ready;
            first_use = write;
            last_use = write;
        } else {
            l.write = write;
            l.ready = ready;
            first_use = first_adjusted;
            last_use = last;
        }
        l.first_use = first_use;
        l.last_use = last_use;
        let _ = first;
        let delta = l.folded - l.line;
        l.write_f = l.write + delta;
        let adjusted_end = {
            let (high, low) = if l.sum48 < l.sum4a {
                (l.sum4a, l.sum48)
            } else {
                (l.sum48, l.sum4a)
            };
            let mut high = high;
            let spare = l.sum46 - (high - low);
            if spare > 0 {
                high += (spare + 1) >> 1;
            }
            if high > 0 {
                high -= 1;
            }
            high + l.ready
        };
        let has_users =
            l.total_users != 0 || (l.external_output != 0 && self.lists[l.list].external_owner);
        l.end = if has_users {
            if (external && period > 0) || adjusted_end <= last_use {
                last_use
            } else {
                adjusted_end
            }
        } else {
            l.write
        };
        l.end_f = l.end + delta;
    }

    fn add_output_lock(&mut self, lock: usize) {
        let list = self.locks[lock].list;
        self.lists[list].pending_outputs -= 1;
        self.lists[list].outputs.insert(0, lock);
        self.calc_stats(lock, None);
        self.locks[lock].inserted = true;
        let line = self.locks[lock].line;
        let position = self.lists[list]
            .sorted
            .iter()
            .position(|&other| line < self.locks[other].line)
            .unwrap_or(self.lists[list].sorted.len());
        self.lists[list].sorted.insert(position, lock);
    }

    fn undo_output_lock(&mut self, lock: usize) {
        let list = self.locks[lock].list;
        self.lists[list].pending_outputs += 1;
        self.lists[list].outputs.retain(|&l| l != lock);
        self.calc_stats(lock, None);
        self.locks[lock].inserted = false;
        self.lists[list].sorted.retain(|&l| l != lock);
        self.locks[lock].line = -1;
        self.locks[lock].folded = -1;
    }

    fn add_input_lock(&mut self, lock: usize) {
        let Some(parent) = self.locks[lock].parent else {
            return;
        };
        self.locks[parent].pending_users -= 1;
        self.locks[parent].users.insert(0, lock);
        self.calc_stats(parent, None);
        self.locks[lock].inserted = true;
    }

    fn undo_input_lock(&mut self, lock: usize) {
        let Some(parent) = self.locks[lock].parent else {
            return;
        };
        self.locks[parent].pending_users += 1;
        self.locks[parent].users.retain(|&l| l != lock);
        self.calc_stats(parent, None);
        self.locks[lock].inserted = false;
        self.locks[lock].line = -1;
        self.locks[lock].folded = -1;
    }

    /// Outputs, then inputs, in operand order.
    fn set_locks(&mut self, index: usize) {
        let line = self.slots[index].line;
        let row = self.fold(line).0;
        for o in 0..self.insn(index).operands.len() {
            let operand = &self.problem.instructions[index].operands[o];
            if operand.direction == Direction::Write {
                for part in 0..4 {
                    if let Some(lock) = self.slots[index].locks[o][part] {
                        self.locks[lock].line = line;
                        self.locks[lock].folded = row;
                        self.add_output_lock(lock);
                    }
                }
            }
            let operand = &self.problem.instructions[index].operands[o];
            if operand.direction == Direction::Read {
                for part in 0..4 {
                    if let Some(lock) = self.slots[index].locks[o][part] {
                        self.locks[lock].line = line;
                        self.locks[lock].folded = row;
                        self.add_input_lock(lock);
                    }
                }
            }
        }
    }

    /// Reverse operand order.
    fn undo_locks(&mut self, index: usize) {
        for o in (0..self.insn(index).operands.len()).rev() {
            let operand = &self.problem.instructions[index].operands[o];
            if operand.direction == Direction::Write {
                for part in 0..4 {
                    if let Some(lock) = self.slots[index].locks[o][part] {
                        self.locks[lock].retry_flag = false;
                        self.undo_output_lock(lock);
                    }
                }
            }
            let operand = &self.problem.instructions[index].operands[o];
            if operand.direction == Direction::Read {
                for part in 0..4 {
                    if let Some(lock) = self.slots[index].locks[o][part] {
                        self.undo_input_lock(lock);
                    }
                }
            }
        }
    }

    /// Input lock check, linear.
    fn check_input(&mut self, input: usize, line: i32, next: &mut i32) -> u32 {
        let list = self.locks[input].list;
        if self.lists[list].constant {
            return 0;
        }
        let Some(producer) = self.locks[input].parent else {
            return 0;
        };
        let period = self.period;
        let movable = self.locks[input].mode == 1;
        self.locks[input].reason = 0;
        let p = &self.locks[producer];
        if period > 0 && p.owner == Owner::External {
            // A loop-carried entry value: its readers must fit in the period
            // before the first external output (the next value) lands.
            if p.user_count == 0 || !(p.end < line || line < p.write) {
                return 0;
            }
            let low = line.min(p.write);
            let high = p.end.max(line);
            let Some(first) = self.lists[list].first_external_output else {
                return 0;
            };
            let window = period - i32::from(self.locks[first].latency);
            if window >= high - low {
                return 0;
            }
            let proposal = if line < p.first_use {
                p.end - window
            } else if movable {
                self.locks[input].reason = 1;
                return MOVE_NEEDED;
            } else {
                IMPOSSIBLE
            };
            *next = (*next).max(proposal);
            return 0x2000;
        }
        let ready = i32::from(self.locks[input].latency) + p.ready;
        if period > 0
            && crate::debug::switches()
                .input_trace
                .is_some_and(|traced| Some(traced) == self.locks[input].instruction())
        {
            crate::trace!(
                "    check input {input} at {line}: producer {producer} {:?} write {} ready {} end {} write_f {} end_f {} users {} pending {} mode {}",
                p.instruction(),
                p.write,
                p.ready,
                p.end,
                p.write_f,
                p.end_f,
                p.user_count,
                p.pending_users,
                self.locks[input].mode
            );
        }
        if line < ready {
            *next = (*next).max(ready);
            return 8;
        }
        if p.end < line {
            if period > 0 && period <= line - p.write {
                if movable {
                    self.locks[input].reason = 3;
                    return MOVE_NEEDED;
                }
                *next = IMPOSSIBLE;
                return 0x400;
            }
            let mut start = p.write_f;
            let mut new_end = line - p.write + p.write_f;
            if period > 0 {
                while period <= start {
                    start -= period;
                    new_end -= period;
                }
            }
            let skip_external =
                period > 0 && self.lists[list].first_external_output == Some(producer);
            let mut result = 0;
            for other in self.lists[list].outputs.clone() {
                if other == producer {
                    continue;
                }
                let v = &self.locks[other];
                if skip_external && v.owner == Owner::External {
                    continue;
                }
                if v.pending_users == 0 {
                    if self.overlap(start, new_end, v.write_f, v.end_f) {
                        // Wrangler A may resolve the overlap with a move.
                        if !self.overlap_moves || period == 0 || !movable {
                            if crate::debug::switches().input_trace.is_some_and(|traced| {
                                Some(traced) == self.locks[input].instruction()
                            }) {
                                crate::trace!(
                                    "    overlap input {input} range {start}..{new_end} with {other} {:?} {}..{} ext {:?} movable {movable}",
                                    v.instruction(),
                                    v.write_f,
                                    v.end_f,
                                    v.owner
                                );
                            }
                            *next = IMPOSSIBLE;
                            return result | 0x20;
                        }
                        self.locks[input].reason = 2;
                        result = MOVE_NEEDED;
                    }
                } else if start < v.end_f && v.write_f <= new_end {
                    *next = IMPOSSIBLE;
                    return result | 0x10;
                }
            }
            return result;
        }
        0
    }

    /// Issue-throughput check.
    fn check_throughput(&self, output: usize, line: i32, next: &mut i32) -> u32 {
        let throughput = i32::from(self.locks[output].throughput);
        if throughput <= 1 {
            return 0;
        }
        let list = self.locks[output].list;
        let row = self.fold(line).0;
        let mut result = 0;
        for &other in &self.lists[list].outputs {
            if other == output {
                continue;
            }
            let v = &self.locks[other];
            let finish = i32::from(v.throughput) + v.folded;
            if finish > 0 && self.overlap(row, throughput - 1 + row, v.folded, finish - 1) {
                result = 1;
                *next = (*next).max(self.next_in_period(finish, line));
            }
        }
        result
    }

    /// Output lock check.
    fn check_output(&self, output: usize, line: i32, next: &mut i32) -> u32 {
        let mut result = self.check_throughput(output, line, next);
        let l = &self.locks[output];
        if l.owner == Owner::External {
            return 0;
        }
        let period = self.period;
        let latency = i32::from(l.latency);
        let write = if l.blocking { line + 1 } else { latency + line };
        let list = l.list;
        if l.external_output != 0 {
            if self.lists[list].pending_outputs > 1 {
                *next = IMPOSSIBLE;
                return result | 0x800;
            }
            for &other in &self.lists[list].outputs {
                let end = self.locks[other].end;
                if write <= end {
                    *next = (*next).max(end - (write - line) + 1);
                    result |= 0x1000;
                }
            }
            if result != 0 {
                return result;
            }
            result = 0;
        }
        let mut end = write;
        if l.total_users != 0 {
            let (high, low) = if l.sum48 < l.sum4a {
                (l.sum4a, l.sum48)
            } else {
                (l.sum48, l.sum4a)
            };
            let mut high = high;
            let spare = l.sum46 - (high - low);
            if spare > 0 {
                high += (spare + 1) >> 1;
            }
            if high > 0 {
                high -= 1;
            }
            end = latency + line + high;
        }
        // In a loop the entry value of an externally owned list is the
        // previous iteration's: this write must follow its readers and land
        // before its next first use.
        // The entry value is skipped below only when it has no users or this
        // write is the external output; otherwise it takes part in the
        // overlap check (the original's output lock check jumps past setting the flag).
        let mut skip_external = false;
        if period > 0 && self.lists[list].external_owner {
            skip_external = true;
            if let Some(&entry) = self.lists[list].sorted.first() {
                let e = &self.locks[entry];
                if e.pending_users != 0 {
                    *next = IMPOSSIBLE;
                    return result | 4;
                }
                if e.user_count != 0 {
                    if write <= e.end {
                        *next = (*next).max(e.end - (write - line) + 1);
                        result |= 0x4000;
                    }
                    let carried_end = e.end + period;
                    if e.first_use + period < latency + line {
                        *next = IMPOSSIBLE;
                        return 0x20000;
                    }
                    if l.external_output != 0 {
                        if period <= carried_end - write {
                            *next = IMPOSSIBLE;
                            return 0x2000;
                        }
                        end = end.max(carried_end);
                    } else {
                        skip_external = false;
                    }
                }
            }
        }
        let row = self.fold(line).0;
        let mut low = write - line + row;
        let mut high = end - line + row;
        if period > 0 {
            while period <= low {
                low -= period;
                high -= period;
            }
        }
        for &other in &self.lists[list].outputs {
            let v = &self.locks[other];
            if skip_external && v.owner == Owner::External {
                continue;
            }
            let (mut v_low, mut v_high) = (v.write_f, v.end_f);
            if period > 0 {
                while period <= v_low {
                    v_low -= period;
                    v_high -= period;
                }
            }
            if v.pending_users == 0 {
                if self.overlap(low, high, v_low, v_high) {
                    let retry = v.end - (write - line) + 1;
                    *next = (*next).max(if period > 0 {
                        self.next_in_period(retry, line)
                    } else {
                        retry
                    });
                    return result | 2;
                }
            } else if period > 0 || v_low <= high {
                *next = IMPOSSIBLE;
                return result | 4;
            }
        }
        result
    }

    /// Register lock driver.
    fn check_locks(&mut self, index: usize, line: i32, next: &mut i32) -> u32 {
        let mut result = 0;
        let mut adjusted: Vec<usize> = Vec::new();
        let operands = self.insn(index).operands.len();
        'inputs: for o in 0..operands {
            if self.problem.instructions[index].operands[o].direction != Direction::Read {
                continue;
            }
            for part in 0..4 {
                let Some(input) = self.slots[index].locks[o][part] else {
                    continue;
                };
                let minimum = self.lists[self.locks[input].list].minimum;
                if *next < minimum {
                    result |= LIST_MINIMUM;
                    *next = minimum;
                    break 'inputs;
                }
                let r = self.check_input(input, line, next);
                result |= r;
                if r & 0x430 != 0 {
                    if let Some(producer) = self.locks[input].parent {
                        if !self.locks[producer].retry_flag {
                            self.locks[producer].retry_flag = true;
                            self.locks[producer].retry += 1;
                        }
                        if let Some(owner) = self.locks[producer].instruction() {
                            self.mark_failure(owner);
                        }
                    }
                    *next = IMPOSSIBLE;
                    for &p in &adjusted {
                        self.locks[p].pending_users += 1;
                    }
                    return result;
                }
                if self.locks[input].inout.is_some()
                    && let Some(producer) = self.locks[input].parent
                {
                    self.locks[producer].pending_users -= 1;
                    adjusted.push(producer);
                }
            }
        }
        if result & LIST_MINIMUM == 0 {
            'outputs: for o in 0..operands {
                if self.problem.instructions[index].operands[o].direction != Direction::Write {
                    continue;
                }
                for part in 0..4 {
                    let Some(output) = self.slots[index].locks[o][part] else {
                        continue;
                    };
                    let retry = self.locks[output].retry;
                    if retry != 0 {
                        let position = self.retry_position(self.locks[output].list, retry);
                        if *next < position {
                            result |= LIST_MINIMUM;
                            *next = position;
                            break 'outputs;
                        }
                    }
                    let r = self.check_output(output, line, next);
                    result |= r;
                    if r & 0x22000 != 0 && self.period > 0 {
                        // Loop overrun of a carried value: raise the list
                        // minimum and retry from its earliest entry reader.
                        let minimum = line + i32::from(self.locks[output].latency) - self.period;
                        if minimum > 0 {
                            let list = self.locks[output].list;
                            self.lists[list].minimum = minimum;
                            if let Some(reader) = self.earliest_entry_reader(list) {
                                self.mark_failure(reader);
                            }
                            *next = IMPOSSIBLE;
                            break 'outputs;
                        }
                    }
                }
            }
        }
        for &p in &adjusted {
            self.locks[p].pending_users += 1;
        }
        result
    }

    /// Helper of the register lock checks.
    fn retry_position(&self, list: usize, mut count: i32) -> i32 {
        let mut position = 0;
        for &lock in &self.lists[list].sorted {
            count -= 1;
            position = position.max(self.locks[lock].last_use);
            if self.locks[lock].pending_users != 0 {
                return IMPOSSIBLE;
            }
            if count < 1 {
                return position;
            }
        }
        position
    }

    /// The reader of a list's entry value to retry from. The
    /// original compares each reader's line against the previous winner's
    /// insertion serial; that quirk is kept.
    fn earliest_entry_reader(&self, list: usize) -> Option<usize> {
        let &entry = self.lists[list].sorted.first()?;
        if self.locks[entry].owner != Owner::External {
            return None;
        }
        let mut bound = IMPOSSIBLE;
        let mut found = None;
        for &user in &self.locks[entry].users {
            if self.locks[user].mode == 2 {
                continue;
            }
            let Some(index) = self.locks[user].instruction() else {
                continue;
            };
            if self.slots[index].line < bound {
                bound = self.slots[index].serial;
                found = Some(index);
            }
        }
        found
    }

    fn mark_failure(&mut self, index: usize) {
        if self.slots[index].line < 0 {
            return;
        }
        if self
            .first_failure
            .is_none_or(|current| self.slots[index].serial < self.slots[current].serial)
        {
            self.first_failure = Some(index);
        }
    }

    fn forced_ready(&self, index: usize) -> i32 {
        let mut line = 0;
        for &p in &self.insn(index).forced_predecessors {
            let placed = self.slots[p].line;
            if placed < 0 {
                return -1;
            }
            line = line.max(placed + 1);
        }
        line
    }

    fn pairs(&self, index: usize, line: i32, pipe: u8) -> bool {
        let row = self.rows[self.fold(line).0 as usize];
        let mut slots = row;
        for slot in &mut slots {
            if *slot == Some(index) {
                *slot = None;
            }
        }
        slots[usize::from(pipe)] = Some(index);
        for (slot, entry) in slots.iter().enumerate() {
            if let Some(i) = entry
                && (self.insn(*i).pipes >> slot) & 1 == 0
            {
                return false;
            }
        }
        if let (Some(a), Some(b)) = (slots[0], slots[1]) {
            for wa in self
                .insn(a)
                .operands
                .iter()
                .filter(|o| o.direction == Direction::Write)
            {
                for wb in self
                    .insn(b)
                    .operands
                    .iter()
                    .filter(|o| o.direction == Direction::Write)
                {
                    if wa.register == wb.register {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Unified checker, linear.
    fn check(&mut self, index: usize, position: Position, next: &mut i32) -> u32 {
        let line = position.line;
        let row = self.fold(line).0;
        *next = (*next).max(line);
        let insn = self.insn(index);
        let minimum = insn.minimum_position + self.start;
        if row < minimum {
            *next = if self.period > 0 {
                self.next_in_period(minimum, *next)
            } else {
                minimum
            };
            return POSITION;
        }
        let distance = insn.end_distance;
        if distance != 0 && self.end_limit >= 0 && self.end_limit - row <= distance {
            *next = IMPOSSIBLE;
            return POSITION;
        }
        if self.problem.stagger && insn.memory != 0 && row & 1 != 0 {
            *next = line + 1;
            return POSITION;
        }
        // A loop places at most iteration ID 16; a linear table its horizon.
        if (self.period == 0 && self.problem.horizon <= line)
            || (self.period > 0 && line / self.period > 16)
        {
            *next = IMPOSSIBLE;
            return HORIZON;
        }
        if !insn.acc_special {
            let ready = self.forced_ready(index);
            if ready < 0 {
                *next = IMPOSSIBLE;
                return FORCED;
            }
            if ready > *next {
                *next = ready;
                return FORCED;
            }
        }
        if row < 0 || row as usize >= self.rows.len() {
            *next = IMPOSSIBLE;
            return HORIZON;
        }
        let reserved = self
            .reserved
            .filter(|&(r, _)| r == row && position.pipe == 1)
            .map(|(_, branch)| branch);
        let token_matches = match self.reserved {
            Some((_, branch)) if branch == index => reserved == Some(index),
            _ => reserved.is_none(),
        };
        if self.rows[row as usize][usize::from(position.pipe)].is_some() || !token_matches {
            *next = line + 1;
            return SLOT_UNAVAILABLE;
        }
        if !self.pairs(index, line, position.pipe) {
            *next += 1;
            return PAIRING;
        }
        self.check_locks(index, line, next)
    }

    /// Placement search.
    fn find_line(&mut self, index: usize, line: &mut i32, pipe: &mut u8) -> bool {
        let pipes = self.insn(index).pipes;
        if pipes == 3 {
            loop {
                let mut minimum = IMPOSSIBLE;
                let mut p = *pipe;
                while p < 2 {
                    let mut next = *line;
                    let result = self.check(
                        index,
                        Position {
                            line: *line,
                            pipe: p,
                        },
                        &mut next,
                    );
                    if result & !MOVE_NEEDED == 0 {
                        *line = next;
                        *pipe = p;
                        return true;
                    }
                    minimum = minimum.min(next);
                    p += 1;
                }
                if minimum >= IMPOSSIBLE {
                    return false;
                }
                if minimum == *line {
                    // The original reports an internal error here.
                    return false;
                }
                *line = minimum;
                *pipe = 0;
            }
        } else {
            *pipe = u8::from(pipes != 1);
            loop {
                let mut next = *line;
                let result = self.check(
                    index,
                    Position {
                        line: *line,
                        pipe: *pipe,
                    },
                    &mut next,
                );
                if self.period > 0 && crate::debug::switches().line_trace == Some(index) {
                    crate::trace!(
                        "  line {index} at {} pipe {} -> {result:#x} next {next}",
                        *line,
                        *pipe
                    );
                }
                *line = next;
                if result & !MOVE_NEEDED == 0 {
                    return *line < IMPOSSIBLE;
                }
                if *line >= IMPOSSIBLE {
                    return false;
                }
            }
        }
    }

    /// Dynamic statistics, linear.
    fn dynamic_stats(&mut self) {
        self.max_asap = -1;
        for index in self
            .done
            .iter()
            .chain(&self.todo)
            .copied()
            .collect::<Vec<_>>()
        {
            let s = &mut self.slots[index];
            s.min_latest = IMPOSSIBLE;
            s.asap = -1;
            s.latest = 0;
            s.max_asap = 0;
        }
        for register in 0..self.reader_min.len() {
            self.reader_min[register] = [-1; 4];
            self.reader_max[register] = [-1; 4];
        }
        for &index in &self.done {
            let line = self.slots[index].line;
            self.slots[index].latest = line;
            self.slots[index].asap = line;
            self.max_asap = self.max_asap.max(line);
        }
        let todo = self.todo.clone();
        for &index in &todo {
            let mut asap = 0;
            let insn = &self.problem.instructions[index];
            for (o, operand) in insn.operands.iter().enumerate() {
                if operand.direction != Direction::Read {
                    continue;
                }
                for part in 0..4 {
                    let Some(input) = self.slots[index].locks[o][part] else {
                        continue;
                    };
                    if self.locks[input].mode == 2 {
                        continue;
                    }
                    let Some(producer) = self.locks[input].parent else {
                        continue;
                    };
                    let mut value = 0;
                    if let Some(owner) = self.locks[producer].instruction() {
                        value = self.slots[owner].asap
                            + i32::from(self.locks[input].latency)
                            + i32::from(self.locks[producer].latency);
                    }
                    asap = asap.max(value);
                }
            }
            for operand in &insn.operands {
                if operand.direction != Direction::Write {
                    continue;
                }
                let reads: u8 = insn
                    .operands
                    .iter()
                    .filter(|r| r.direction == Direction::Read && r.register == operand.register)
                    .fold(0, |mask, r| mask | r.mask);
                for part in 0..usize::from(self.problem.registers[operand.register].parts.min(4)) {
                    if reads & (1 << part) == 0 {
                        continue;
                    }
                    let last = self.reader_max[operand.register][part];
                    if last == -1 {
                        continue;
                    }
                    let value = if operand.blocking {
                        last - 1
                    } else {
                        last - i32::from(operand.latency)
                    };
                    if asap <= value {
                        asap = value + 1;
                    }
                }
            }
            self.slots[index].asap = asap;
            self.max_asap = self.max_asap.max(asap);
            for (o, operand) in insn.operands.iter().enumerate() {
                if operand.direction != Direction::Read {
                    continue;
                }
                for part in 0..4 {
                    if operand.mask & (1 << part) == 0 {
                        continue;
                    }
                    if self.slots[index].locks[o][part].is_some_and(|l| self.locks[l].mode == 2) {
                        continue;
                    }
                    let (min, max) = (
                        &mut self.reader_min[operand.register][part],
                        &mut self.reader_max[operand.register][part],
                    );
                    if *min == -1 {
                        *min = asap;
                        *max = asap;
                    } else {
                        *min = (*min).min(asap);
                        *max = (*max).max(asap);
                    }
                }
            }
            for operand in &insn.operands {
                if operand.direction != Direction::Write {
                    continue;
                }
                for part in 0..4 {
                    if operand.mask & (1 << part) != 0 {
                        self.reader_min[operand.register][part] = -1;
                        self.reader_max[operand.register][part] = -1;
                    }
                }
            }
        }
        for &index in &todo {
            self.slots[index].latest = self.max_asap;
        }
        for &index in todo.iter().rev() {
            let insn = &self.problem.instructions[index];
            for (o, operand) in insn.operands.iter().enumerate() {
                if operand.direction != Direction::Read {
                    continue;
                }
                for part in 0..4 {
                    let Some(input) = self.slots[index].locks[o][part] else {
                        continue;
                    };
                    if self.locks[input].mode == 2 {
                        continue;
                    }
                    let Some(producer) = self.locks[input].parent else {
                        continue;
                    };
                    let Some(owner) = self.locks[producer].instruction() else {
                        continue;
                    };
                    // The bound uses the consumer's ASAP, not its
                    // own latest time: this is one level of slack, not ALAP.
                    let value = self.slots[index].asap
                        - (i32::from(self.locks[input].latency)
                            + i32::from(self.locks[producer].latency));
                    if value < self.slots[owner].latest {
                        self.slots[owner].latest = value;
                    }
                }
            }
        }
        for &index in &todo {
            let s = &mut self.slots[index];
            s.depth = s.asap;
            s.height = self.max_asap - s.latest;
        }
    }

    fn done_size(&self) -> i32 {
        let mut size = 0;
        for &index in &self.done {
            let mut distance = self.insn(index).end_distance;
            if self.insn(index).branch && distance < 1 {
                distance = 1;
            }
            size = size.max(distance + 1 + self.slots[index].line);
        }
        size.max(self.problem.minimum_length)
    }

    /// Candidate list.
    fn build_candidates(&mut self, limit: usize) {
        self.work.ranked += self.todo.len();
        self.candidates.clear();
        self.dynamic_stats();
        let todo = self.todo.clone();
        for &index in &todo {
            let insn = &self.problem.instructions[index];
            let mut line = insn.minimum_position;
            let forced = self.forced_ready(index);
            if forced < 0 {
                if crate::debug::switches().forced_trace == Some(index) {
                    crate::trace!(
                        "forced {index} waits on {:?}",
                        self.insn(index)
                            .forced_predecessors
                            .iter()
                            .filter(|&&p| self.slots[p].line < 0)
                            .collect::<Vec<_>>()
                    );
                }
                continue;
            }
            if !insn.acc_special && line < forced {
                line = forced;
            }
            if insn.branch {
                if self.period > 0 {
                    // A loop's conditional always proposes `period - 2`.
                    line = self.period - 2;
                } else {
                    if self.todo.len() > 1 {
                        continue;
                    }
                    line = line.max(self.done_size() - 2).max(0);
                }
            }
            let mut ready = true;
            for (o, operand) in insn.operands.iter().enumerate() {
                if operand.direction != Direction::Read {
                    continue;
                }
                for part in 0..4 {
                    let Some(input) = self.slots[index].locks[o][part] else {
                        continue;
                    };
                    let Some(producer) = self.locks[input].parent else {
                        continue;
                    };
                    if !self.locks[producer].inserted {
                        ready = false;
                        break;
                    }
                    if self.locks[input].mode != 2 {
                        let value = self.locks[producer].line
                            + i32::from(self.locks[producer].latency)
                            + i32::from(self.locks[input].latency);
                        line = line.max(value);
                    }
                }
                if !ready {
                    break;
                }
            }
            if !ready {
                continue;
            }
            if self.period > 0 {
                // Stores may not precede the branch iteration minus the
                // known-safe count, critical effects the branch iteration.
                let branch_iteration = self.branch_placed.map(|b| self.fold(self.slots[b].line).1);
                let floor = match self.insn(index).side_effect {
                    1 => branch_iteration.map_or(self.effect_floors[0], |iteration| {
                        (iteration - self.known_safe) * self.period
                    }),
                    2 => branch_iteration
                        .map_or(self.effect_floors[1], |iteration| iteration * self.period),
                    _ => 0,
                };
                line = line.max(floor);
                // Wrangler A also bounds by the dynamic latest time.
                if limit == 4 && self.slots[index].height != 0 {
                    line = line.max(self.slots[index].latest - 2 * self.period);
                }
            }
            let mut found = line;
            let mut pipe = 0;
            let fits = self.find_line(index, &mut found, &mut pipe);
            if crate::debug::switches().cand_trace && self.period > 0 {
                crate::trace!("cand {index} from {line} -> {fits} at {found}");
            }
            if fits {
                self.slots[index].earliest = found;
                self.candidates.insert(0, index);
            }
            if self.period == 0 && self.candidates.len() > limit {
                break;
            }
        }
    }

    fn rank(&self, strategy: Strategy) -> Option<(usize, i32, u8)> {
        let candidates: Vec<_> = self
            .candidates
            .iter()
            .map(|&index| {
                let s = &self.slots[index];
                let st = &self.statics[index];
                let mut status_inputs = 0;
                let mut mac_inputs = 0;
                for operand in self.insn(index).operands.iter() {
                    if operand.direction != Direction::Read {
                        continue;
                    }
                    match self.problem.registers[operand.register].global_id {
                        Some(0x37 | 0x38) => status_inputs += 1,
                        Some(0x3b) => {
                            status_inputs += 1;
                            mac_inputs += 1;
                        }
                        _ => {}
                    }
                }
                Candidate {
                    operation: index,
                    source_order: self.insn(index).order as usize,
                    metrics: PriorityMetrics {
                        height: s.height,
                        height_links: st.height_links,
                        depth: s.depth,
                        depth_links: st.depth_links,
                        fan_out: st.fan_out,
                        consumer_weight: st.weight,
                        status_inputs,
                        mac_inputs,
                    },
                    penalty: s.penalty.max(0) as u16,
                    delay: s.delay.max(0) as u16,
                    earliest: s.earliest.max(0) as u16,
                    generated_move: self.insn(index).is_move,
                }
            })
            .collect();
        crate::trace_if!(
            wrangle_candidates,
            "rank block {} {:?} cursor {} candidates {:?}",
            trace_block(),
            strategy,
            self.cursor,
            candidates
                .iter()
                .map(|c| (
                    c.operation,
                    c.metrics.height,
                    c.metrics.height_links,
                    c.metrics.depth,
                    c.metrics.depth_links,
                    c.metrics.fan_out,
                    c.metrics.consumer_weight,
                    c.penalty,
                    c.delay,
                    c.earliest
                ))
                .collect::<Vec<_>>()
        );
        let proposal =
            schedule::select_candidate(&candidates, strategy, self.cursor.max(0) as u16)?;
        let mut line = i32::from(proposal.cycle);
        if self.insn(proposal.operation).branch {
            line = self.slots[proposal.operation].earliest;
        }
        Some((
            proposal.operation,
            line,
            match proposal.preferred_pipe {
                schedule::Pipe::Upper => 0,
                schedule::Pipe::Lower => 1,
            },
        ))
    }

    fn insert(&mut self, index: usize, position: Position) {
        self.work.inserts += 1;
        self.slots[index].line = position.line;
        self.slots[index].pipe = position.pipe;
        let row = self.fold(position.line).0;
        if self.period > 0 && crate::debug::switches().lpgen_steps {
            crate::trace!(
                "  insert {index} {} at {} pipe {}",
                self.problem.names.get(index).map_or("?", String::as_str),
                position.line,
                position.pipe
            );
        }
        self.rows[row as usize][usize::from(position.pipe)] = Some(index);
        if self.period > 0 && self.insn(index).branch {
            self.branch_placed = Some(index);
        }
        self.set_locks(index);
        self.slots[index].serial = self.serial;
        self.serial += 1;
        self.todo.retain(|&i| i != index);
        self.done.push(index);
    }

    fn undo(&mut self, index: usize) {
        self.work.undos += 1;
        if self.period > 0 && crate::debug::switches().lpgen_steps {
            crate::trace!("  undo {index}");
        }
        self.undo_log.push(index);
        if self.insn(index).branch {
            self.branch_placed = None;
        }
        let s = &self.slots[index];
        let row = self.fold(s.line).0;
        self.rows[row as usize][usize::from(s.pipe)] = None;
        if self.insn(index).is_move || (self.period > 0 && self.insn(index).dual_copy) {
            self.unresolve_move(index);
        }
        self.undo_locks(index);
        self.slots[index].line = -1;
        self.serial -= 1;
        self.slots[index].serial = -1;
        self.done.retain(|&i| i != index);
        self.todo.insert(0, index);
    }

    fn reset_penalties(&mut self, index: usize) {
        self.slots[index].penalty = 0;
        self.slots[index].delay = 0;
        for o in 0..self.slots[index].locks.len() {
            for part in 0..4 {
                if let Some(lock) = self.slots[index].locks[o][part]
                    && self.locks[lock].output
                {
                    self.locks[lock].retry = 0;
                }
            }
        }
    }

    /// Backtracking shared by both wranglers.
    fn backtrack(&mut self, wrangler_b: bool) {
        let first = self.first_failure.or(self.done.last().copied());
        if let Some(first) = first {
            while self.done.last().copied() != Some(first) && !self.done.is_empty() {
                let index = *self.done.last().unwrap();
                self.reset_penalties(index);
                self.undo(index);
                if self.insn(index).is_move {
                    self.undo_moves_from(index);
                }
            }
        }
        while let Some(&index) = self.done.last() {
            self.undo(index);
            self.slots[index].penalty += 1;
            if wrangler_b {
                self.slots[index].delay += 1;
            }
            if self.slots[index].penalty <= self.slots[index].threshold {
                break;
            }
            self.reset_penalties(index);
            // Wrangler B drops an exhausted move at once;
            // wrangler A keeps it pending with a raised threshold until
            // that passes 16 periods.
            if self.insn(index).is_move && wrangler_b {
                self.undo_moves_from(index);
                continue;
            }
            self.slots[index].penalty = 0;
            self.slots[index].threshold += 1;
            let threshold = self.slots[index].threshold;
            if self.period > 0 && threshold > self.period * 16 {
                if self.insn(index).is_move {
                    self.undo_moves_from(index);
                    continue;
                }
                self.slots[index].threshold = self.period * 16;
            } else if !wrangler_b && threshold > self.max_asap * 8 {
                self.slots[index].threshold = self.max_asap * 8;
            }
        }
        self.first_failure = None;
    }

    /// Give an unresolved VF move its upper (`uvmove`) or
    /// lower (`lvmove`) form.
    fn resolve_move(&mut self, index: usize, pipe: u8) {
        self.count_issue(index, -1);
        self.problem.to_mut().instructions[index].pipes = if pipe == 0 { 1 } else { 2 };
        self.count_issue(index, 1);
    }

    /// Back to the dual-issue `vmove` when uninserted.
    fn unresolve_move(&mut self, index: usize) {
        let vector = self.problem.registers[self.insn(index).operands[0].register].pool == 1;
        if vector && self.insn(index).pipes != 3 {
            self.count_issue(index, -1);
            self.problem.to_mut().instructions[index].pipes = 3;
            self.count_issue(index, 1);
        }
    }

    /// Pipe choice for a ranked move in wrangler B/A before placement:
    /// returns `false` when the lower pipe is full and the search must
    /// restart from the first placed instruction.
    fn choose_move_pipe(&mut self, index: usize, preferred: u8) -> bool {
        let period = self.period;
        let mut pipe = preferred;
        crate::trace_if!(
            place_trace,
            "  movepipe {index} pipes {} preferred {preferred} counts {:?} ac {}",
            self.insn(index).pipes,
            self.issue_counts,
            self.move_flag_ac
        );
        if self.insn(index).pipes == 3 {
            let [_, upper, lower] = self.issue_counts;
            pipe = if upper == period {
                1
            } else if lower == period {
                0
            } else if !self.move_flag_ac {
                u8::from(lower < upper)
            } else {
                0
            };
        }
        if pipe == 1 && self.issue_counts[2] == period && !self.move_flag_ac {
            self.first_failure = self.done.first().copied();
            self.move_flag_ac = true;
            self.move_flag_ad = false;
            return false;
        }
        if self.insn(index).pipes == 3 {
            self.resolve_move(index, pipe);
        }
        true
    }

    /// Move one pending input lock to another list and producer.
    fn relink_input(&mut self, input: usize, list: usize, producer: usize) {
        unlink_input(&mut self.locks, input);
        self.locks[input].list = list;
        link_input(&mut self.locks, input, producer);
    }

    /// Move one pending output lock to another register's list.
    fn relink_output(&mut self, output: usize, list: usize) {
        let old = self.locks[output].list;
        self.lists[old].pending_outputs -= 1;
        self.lists[list].pending_outputs += 1;
        if self.locks[output].external_output != 0 {
            self.lists[old].external_outputs -= 1;
            if self.lists[old].first_external_output == Some(output) {
                self.lists[old].first_external_output = None;
            }
            self.lists[list].external_outputs += 1;
            self.lists[list].first_external_output.get_or_insert(output);
        }
        self.locks[output].list = list;
    }

    /// Synthesize a move for an input of `index` that cannot
    /// reach its producer at `line`. Returns 0 when a move was queued, 1
    /// when the search must backtrack (the failing setter is marked), 2 for
    /// a move instruction itself and 4 when no input is movable.
    fn generate_move(&mut self, index: usize, line: i32) -> u8 {
        if self.insn(index).is_move {
            return 2;
        }
        // The input register whose producer is farthest (or whose reason
        // is a carried overrun or overlap) is renamed.
        let mut best: Option<(usize, usize)> = None;
        let mut best_score = -1;
        for (o, operand) in self.insn(index).operands.iter().enumerate() {
            if operand.direction != Direction::Read
                || !self.problem.registers[operand.register].movable()
            {
                continue;
            }
            let mut score = -1;
            let mut last = None;
            for part in 0..4 {
                let Some(input) = self.slots[index].locks[o][part] else {
                    continue;
                };
                match self.locks[input].reason {
                    2 => score = score.max(10_000),
                    1 => score = score.max(20_000),
                    _ => {}
                }
                last = Some(input);
                let Some(producer) = self.locks[input].parent else {
                    continue;
                };
                if self.locks[producer].owner == Owner::External {
                    continue;
                }
                score = score.max(line - (self.locks[producer].line + 1));
            }
            if let Some(last) = last
                && best_score < score
            {
                best_score = score;
                best = Some((operand.register, last));
            }
        }
        let Some((old, sample)) = best else {
            return 4;
        };
        let setter = self.locks[sample]
            .parent
            .and_then(|producer| self.locks[producer].instruction());
        let parts = usize::from(self.problem.registers[old].parts.min(4));
        let mut pending = 0u8;
        for part in 0..parts {
            if let Some(list) = self.list_of[old][part]
                && let Some(&head) = self.lists[list].outputs.first()
                && self.locks[head].pending_users != 0
            {
                pending |= 1 << part;
            }
        }
        if let Some(created) = self.create_move(index, old, setter, line) {
            return if created {
                0
            } else {
                self.move_failure(old, pending)
            };
        }
        self.move_failure(old, pending)
    }

    /// Failure tail: retry from the earliest setter of the
    /// register's still-read values, else from the first placed instruction.
    fn move_failure(&mut self, register: usize, pending: u8) -> u8 {
        let mut earliest: Option<usize> = None;
        for part in 0..usize::from(self.problem.registers[register].parts.min(4)) {
            if pending & (1 << part) == 0 {
                continue;
            }
            let Some(list) = self.list_of[register][part] else {
                continue;
            };
            let Some(&head) = self.lists[list].outputs.first() else {
                continue;
            };
            if self.locks[head].pending_users == 0 {
                continue;
            }
            if let Some(owner) = self.locks[head].instruction()
                && earliest.is_none_or(|e| self.slots[owner].line < self.slots[e].line)
            {
                earliest = Some(owner);
            }
        }
        if let Some(first) = earliest.or(self.done.first().copied()) {
            self.mark_failure(first);
        }
        1
    }

    /// The retargeting of the move synthesis. `None` when no
    /// move may be made (issue slots exhausted); `Some(false)` when no
    /// reader could be retargeted.
    fn create_move(
        &mut self,
        index: usize,
        old: usize,
        setter: Option<usize>,
        line: i32,
    ) -> Option<bool> {
        let period = self.period;
        if self.issue_counts.iter().sum::<i32>() >= period * 2 {
            return None;
        }
        let vector = self.problem.registers[old].pool == 1;
        if !vector && self.issue_counts[2] >= period {
            self.move_flag_ad = true;
            return None;
        }
        let parts = usize::from(self.problem.registers[old].parts.min(4));
        let latency: i8 = if vector { 4 } else { 1 };
        let (mv, new) = self.add_move_slot(index, old, setter, line, vector, latency);

        // Retarget pending readers and later writers of the old register.
        let table_output = self.problem.instructions.iter().any(|insn| {
            insn.operands
                .iter()
                .any(|o| o.direction == Direction::Write && o.register == old && o.escapes != 0)
        });
        let mut state = MoveRetarget {
            mv,
            old,
            new,
            parts,
            latency,
            table_output,
            moved: 0,
            fixed_writes: 0,
            full_period: true,
        };
        for t in self.todo.clone() {
            self.retarget_readers(t, &mut state);
            self.retarget_writers(t, &mut state);
        }

        if state.moved == 0 {
            self.discard_move(mv, old, new);
            return Some(false);
        }
        {
            let problem = self.problem.to_mut();
            problem.instructions[mv].operands[0].mask = state.moved;
            problem.instructions[mv].operands[1].mask = state.moved;
            problem.instructions[mv].full_period = state.full_period;
        }
        self.moves.push(mv);
        self.todo.insert(0, mv);
        self.count_issue(mv, 1);
        Some(true)
    }

    /// Append the new move instruction, its fresh register, lock lists,
    /// statics and slot; returns the move and the new register.
    fn add_move_slot(
        &mut self,
        index: usize,
        old: usize,
        setter: Option<usize>,
        line: i32,
        vector: bool,
        latency: i8,
    ) -> (usize, usize) {
        let parts = usize::from(self.problem.registers[old].parts.min(4));
        // The new register and its lock lists.
        let mut register = self.problem.registers[old].clone();
        register.through = 0;
        // The move's register is a fresh virtual one (`move__mN`) even when
        // the moved value lives in a fixed register: general_tri's
        // `move__m11 <- VI04` is allocated VI09.
        register.physical = None;
        let new = self.problem.registers.len();
        let base = setter.unwrap_or(index);
        let mv = self.problem.instructions.len();
        let instruction = Instruction {
            order: match setter {
                Some(s) => self.insn(s).order + 1,
                None => self.insn(index).order - 1,
            },
            pipes: if vector { 3 } else { 2 },
            throughput: 1,
            units: 0,
            branch: false,
            memory: 0,
            minimum_position: 0,
            end_distance: 0,
            protected_read: false,
            protected_write: false,
            forced_predecessors: Vec::new(),
            forced_successors: Vec::new(),
            acc_special: false,
            side_effect: 0,
            is_move: true,
            dual_copy: false,
            move_depth: setter.map_or(1, |s| self.insn(s).move_depth + 1),
            move_target: line - i32::from(latency),
            full_period: true,
            first_output: None,
            operands: vec![
                Operand {
                    register: new,
                    direction: Direction::Write,
                    mask: 0,
                    latency,
                    blocking: true,
                    input_offset: 0,
                    sources: [None; 4],
                    escapes: 0,
                    observed: true,
                },
                Operand {
                    register: old,
                    direction: Direction::Read,
                    mask: 0,
                    latency: -1,
                    blocking: true,
                    input_offset: 0,
                    sources: [None; 4],
                    escapes: 0,
                    observed: false,
                },
            ],
            stat_links: Vec::new(),
        };
        {
            let problem = self.problem.to_mut();
            problem.registers.push(register);
            problem.instructions.push(instruction);
            let name = format!("move_m{}", self.moves.len());
            problem.names.push(name);
        }
        self.list_of.push([None; 4]);
        self.reader_min.push([-1; 4]);
        self.reader_max.push([-1; 4]);
        for part in 0..parts {
            let list = self.lists.len();
            self.lists.push(LockList::default());
            self.list_of[new][part] = Some(list);
        }
        let statics = self.statics[base];
        self.statics.push(statics);
        let mut slot = Slot {
            line: -1,
            serial: -1,
            threshold: self.slots[base].threshold,
            ..Slot::default()
        };
        slot.locks = vec![[None; 4]; 2];
        self.slots.push(slot);
        (mv, new)
    }

    /// Point the pending reads of the old register in `t` at the move's
    /// register, feeding them from the move where their producer is placed.
    fn retarget_readers(&mut self, t: usize, state: &mut MoveRetarget) {
        let (mv, old, new, parts) = (state.mv, state.old, state.new, state.parts);

        for o in 0..self.insn(t).operands.len() {
            let operand = self.insn(t).operands[o].clone();
            if operand.direction != Direction::Read || operand.register != old {
                continue;
            }
            let movable = self.slots[t].locks[o]
                .iter()
                .flatten()
                .all(|&l| self.locks[l].mode == 1);
            if !movable || state.fixed_writes & operand.mask != 0 {
                state.full_period = false;
                continue;
            }

            for part in 0..parts {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                let Some(input) = self.slots[t].locks[o][part] else {
                    continue;
                };
                let Some(producer) = self.locks[input].parent else {
                    continue;
                };
                let new_list = self.list_of[new][part].unwrap();
                if !self.locks[producer].inserted {
                    if self.locks[producer].list == new_list {
                        self.relink_input(input, new_list, producer);
                    }
                    continue;
                }
                if state.moved & (1 << part) == 0 {
                    self.add_move_locks(mv, old, new, part, state.latency);
                    state.moved |= 1 << part;
                }
                let output = self.slots[mv].locks[0][part].unwrap();
                self.relink_input(input, new_list, output);
            }
            self.problem.to_mut().instructions[t].operands[o].register = new;
        }
    }

    /// Point the writes of the old register in `t` at the move's register
    /// when the register is movable; otherwise record them as fixed writes.
    fn retarget_writers(&mut self, t: usize, state: &mut MoveRetarget) {
        let (old, new, parts) = (state.old, state.new, state.parts);

        for o in 0..self.insn(t).operands.len() {
            let operand = self.insn(t).operands[o].clone();
            if operand.direction != Direction::Write || operand.register != old {
                continue;
            }
            // Marks a loop write movable only when the
            // register is also not a table output.
            if !self.problem.registers[old].movable() || state.table_output {
                state.full_period = false;
                state.fixed_writes |= operand.mask;
                continue;
            }
            for part in 0..parts {
                if let Some(output) = self.slots[t].locks[o][part] {
                    let list = self.list_of[new][part].unwrap();
                    self.relink_output(output, list);
                }
            }
            self.problem.to_mut().instructions[t].operands[o].register = new;
        }
    }

    /// Input lock of a move on the old register's current value and its
    /// output lock in the new register.
    fn add_move_locks(&mut self, mv: usize, old: usize, new: usize, part: usize, latency: i8) {
        let old_list = self.list_of[old][part].unwrap();
        let new_list = self.list_of[new][part].unwrap();
        let input = self.locks.len();
        let mut lock = Lock::new(false, old_list, Owner::Instruction(mv, 1));
        lock.mode = 1;
        lock.reader_pipes = Some(reader_pipes(self.insn(mv).pipes));
        self.locks.push(lock);
        if let Some(&head) = self.lists[old_list].outputs.first() {
            link_input(&mut self.locks, input, head);
        }
        self.slots[mv].locks[1][part] = Some(input);
        let output = self.locks.len();
        let mut lock = Lock::new(true, new_list, Owner::Instruction(mv, 0));
        lock.latency = latency;
        lock.blocking = true;
        lock.throughput = 1;
        lock.mode = 1;
        self.locks.push(lock);
        self.lists[new_list].pending_outputs += 1;
        self.slots[mv].locks[0][part] = Some(output);
    }

    /// Release a move that was never queued.
    fn discard_move(&mut self, mv: usize, old: usize, new: usize) {
        self.restore_move_targets(mv, old, new);
        for part in 0..4 {
            if let Some(input) = self.slots[mv].locks[1][part] {
                unlink_input(&mut self.locks, input);
            }
        }
        self.slots[mv].locks = vec![[None; 4]; 2];
    }

    /// Give every pending reference to the new register back
    /// to the old one; a reader fed by the move reads its source again.
    fn restore_move_targets(&mut self, mv: usize, old: usize, new: usize) {
        for t in self.todo.clone() {
            if t == mv {
                continue;
            }
            for o in 0..self.insn(t).operands.len() {
                let operand = self.insn(t).operands[o].clone();
                if operand.register != new {
                    continue;
                }
                for part in 0..4 {
                    let Some(lock) = self.slots[t].locks[o][part] else {
                        continue;
                    };
                    let Some(old_list) = self.list_of[old][part] else {
                        continue;
                    };
                    match operand.direction {
                        Direction::Read => {
                            let Some(mut producer) = self.locks[lock].parent else {
                                continue;
                            };
                            if self.slots[mv].locks[0][part] == Some(producer)
                                && let Some(source) =
                                    self.slots[mv].locks[1][part].and_then(|i| self.locks[i].parent)
                            {
                                producer = source;
                            }
                            self.relink_input(lock, old_list, producer);
                        }
                        Direction::Write => self.relink_output(lock, old_list),
                    }
                }
                self.problem.to_mut().instructions[t].operands[o].register = old;
            }
        }
    }

    /// Walk the to-do list from its tail to `mv` (just put
    /// back at the head) and undo every pending move on the way.
    /// Placed moves are untouched.
    fn undo_moves_from(&mut self, mv: usize) {
        for last in self.todo.clone().into_iter().rev() {
            if self.insn(last).is_move {
                let old = self.insn(last).operands[1].register;
                let new = self.insn(last).operands[0].register;
                self.unresolve_move(last);
                self.todo.retain(|&i| i != last);
                self.restore_move_targets(last, old, new);
                for part in 0..4 {
                    if let Some(input) = self.slots[last].locks[1][part] {
                        unlink_input(&mut self.locks, input);
                    }
                }
                self.slots[last].locks = vec![[None; 4]; 2];
                self.count_issue(last, -1);
                self.moves.retain(|&m| m != last);
            }
            if last == mv {
                break;
            }
        }
    }

    /// Loop side-effect ages after a complete placement: a
    /// critical effect before the branch iteration, or a store before the
    /// branch iteration minus the known-safe count, is retried from the
    /// earliest such instruction inserted.
    fn unsafe_effect(&self) -> Option<usize> {
        let mut branch = -1;
        let mut store = IMPOSSIBLE;
        let mut critical = IMPOSSIBLE;
        for &index in &self.done {
            let iteration = self.fold(self.slots[index].line).1;
            match self.insn(index).side_effect {
                1 => store = store.min(iteration),
                2 => critical = critical.min(iteration),
                _ => {}
            }
            if self.insn(index).branch {
                branch = branch.max(iteration);
            }
        }
        let store_bad = store < IMPOSSIBLE && store < branch - self.known_safe;
        let critical_bad = critical < IMPOSSIBLE && critical < branch;
        if !store_bad && !critical_bad {
            return None;
        }
        let mut first: Option<usize> = None;
        for &index in &self.done {
            let line = self.slots[index].line;
            let offending = match self.insn(index).side_effect {
                1 => line < self.period * (branch - self.known_safe),
                2 => line < self.period * branch,
                _ => false,
            };
            if offending
                && first.is_none_or(|current| self.slots[index].serial < self.slots[current].serial)
            {
                first = Some(index);
            }
        }
        first
    }

    /// Run one strategy. `budget` bounds the number of placements made.
    pub fn wrangle(
        &mut self,
        strategy: Strategy,
        budget: usize,
    ) -> Result<Vec<(usize, i32, u8)>, Failure> {
        let wrangler_b = !strategy.wrangler_a();
        let limit = if wrangler_b { 16 } else { 4 };
        let mut trace = Vec::new();
        let mut steps = 0usize;
        self.todo_at_start = self.todo.len();
        self.overlap_moves = !wrangler_b;
        self.move_flag_ac = false;
        self.move_flag_ad = false;

        loop {
            steps += 1;
            self.work.steps = steps;
            if steps > budget {
                self.failed_trace = trace;
                return Err(Failure::Budget);
            }
            self.first_failure = None;
            self.build_candidates(limit);
            self.cursor = self.done.last().map_or(0, |&i| self.slots[i].line);

            if self.candidates.is_empty() {
                match self.finish_or_backtrack(wrangler_b, &mut trace) {
                    Some(result) => return result.map(|()| trace),
                    None => continue,
                }
            }

            let attempt = self.place_a_candidate(strategy, budget, &mut steps, &mut trace)?;
            match attempt {
                Placement::Inserted | Placement::Restart => continue,
                Placement::Failed => {
                    if self.done.is_empty() {
                        return Err(Failure::NoSchedule);
                    }
                    self.backtrack(wrangler_b);
                    trace.extend(self.undo_log.drain(..).map(|i| (i, -1, 9)));
                }
                Placement::Discard => unreachable!("the candidate loop resolves a discard"),
            }
        }
    }

    /// Step of `wrangle` with no candidates: `None` to go on with the next
    /// step (after a backtrack), `Some` to end the run with that result.
    fn finish_or_backtrack(
        &mut self,
        wrangler_b: bool,
        trace: &mut Vec<(usize, i32, u8)>,
    ) -> Option<Result<(), Failure>> {
        if !self.todo.is_empty() {
            if self.done.is_empty() {
                return Some(Err(Failure::NoSchedule));
            }
            self.backtrack(wrangler_b);
            trace.extend(self.undo_log.drain(..).map(|i| (i, -1, 9)));
            return None;
        }

        if self.period > 0 {
            if let Some(first) = self.unsafe_effect() {
                self.first_failure = Some(first);
                self.backtrack(wrangler_b);
                trace.extend(self.undo_log.drain(..).map(|i| (i, -1, 9)));
                return None;
            }
            return Some(Ok(()));
        }

        self.end = self.done_size();
        if self.rows.len() < self.end as usize + 1 {
            self.rows.resize(self.end as usize + 1, [None, None]);
        }
        Some(Ok(()))
    }

    /// Rank candidates and try them until one is placed or the step fails.
    /// Returns `Inserted`, `Restart` or `Failed`; a ran-out budget is an error.
    fn place_a_candidate(
        &mut self,
        strategy: Strategy,
        budget: usize,
        steps: &mut usize,
        trace: &mut Vec<(usize, i32, u8)>,
    ) -> Result<Placement, Failure> {
        let wrangler_b = !strategy.wrangler_a();

        loop {
            let Some((index, proposal, preferred)) = self.rank(strategy) else {
                return Ok(Placement::Failed);
            };
            // A loop move takes its pipe before placement.
            if self.period > 0
                && (self.insn(index).is_move || self.insn(index).dual_copy)
                && !self.choose_move_pipe(index, preferred)
            {
                return Ok(Placement::Failed);
            }

            let placement = match self.locate_line(index, proposal, preferred, wrangler_b) {
                Some((line, pipe)) => self.place_at_line(index, line, pipe, wrangler_b, trace),
                None if wrangler_b => Placement::Failed,
                None => Placement::Discard,
            };
            if placement != Placement::Discard {
                return Ok(placement);
            }

            // Wrangler A discards a failed candidate and ranks again.
            // In a loop this inner search also spends the step budget.
            if self.period > 0 {
                *steps += 1;
                self.work.steps = *steps;
                if *steps > budget {
                    self.failed_trace = std::mem::take(trace);
                    return Err(Failure::Budget);
                }
            }
            self.candidates.retain(|&i| i != index);
            if self.candidates.is_empty() {
                return Ok(Placement::Failed);
            }
        }
    }

    /// The first line and pipe where a ranked candidate can be tried;
    /// wrangler B searches again from its earliest line.
    fn locate_line(
        &mut self,
        index: usize,
        proposal: i32,
        preferred: u8,
        wrangler_b: bool,
    ) -> Option<(i32, u8)> {
        let pipes = self.insn(index).pipes;
        let mut line = proposal;
        let mut pipe = if pipes == 3 {
            preferred & 1
        } else {
            u8::from(pipes != 1)
        };
        let mut found = self.find_line(index, &mut line, &mut pipe);
        if !found && wrangler_b {
            line = self.slots[index].earliest;
            pipe = if pipes == 3 {
                preferred & 1
            } else {
                u8::from(pipes != 1)
            };
            found = self.find_line(index, &mut line, &mut pipe);
        }
        found.then_some((line, pipe))
    }

    /// Check and insert a candidate at a found line, or synthesize a move
    /// for it; `Discard` leaves a failed wrangler A candidate to be dropped.
    fn place_at_line(
        &mut self,
        index: usize,
        line: i32,
        pipe: u8,
        wrangler_b: bool,
        trace: &mut Vec<(usize, i32, u8)>,
    ) -> Placement {
        self.first_failure = None;
        if self.period > 0 {
            let window = if self.insn(index).full_period {
                self.period
            } else {
                self.period >> 1
            };
            self.drag_setters(index, line, window, 0);
        }

        let mut next = line;
        let mut result = self.check(index, Position { line, pipe }, &mut next);
        if self.period > 0
            && self.first_failure.is_none()
            && result & MOVE_NEEDED != 0
            && self.drag_setters(index, line, self.period, 4) != 0
        {
            let mut next = line;
            result = self.check(index, Position { line, pipe }, &mut next);
        }
        if self.period > 0 && crate::debug::switches().place_trace {
            crate::trace!(
                "  place step {} {index} at {line} pipe {pipe} check {result:#x} abort {:?}",
                trace.len(),
                self.first_failure
            );
        }

        if self.first_failure.is_none() && result == 0 {
            self.insert(index, Position { line, pipe });
            trace.push((index, line, pipe));
            return Placement::Inserted;
        }
        if self.period > 0 && self.first_failure.is_none() && result & MOVE_NEEDED != 0 {
            return self.placement_after_move(index, line);
        }
        if wrangler_b || self.first_failure.is_some() {
            return Placement::Failed;
        }
        Placement::Discard
    }

    /// Synthesize a move for a candidate whose inputs cannot reach their
    /// producers: `Restart` when one was queued, otherwise `Failed`.
    fn placement_after_move(&mut self, index: usize, line: i32) -> Placement {
        let generated = self.generate_move(index, line);
        crate::trace_if!(
            place_trace,
            "  genmove {index} -> {generated} ac {} ad {}",
            self.move_flag_ac,
            self.move_flag_ad
        );

        match generated {
            0 => return Placement::Restart,
            1 if self.move_flag_ad && !self.move_flag_ac => {
                self.first_failure = self.done.first().copied();
                self.move_flag_ac = true;
                self.move_flag_ad = false;
            }
            _ => {}
        }
        Placement::Failed
    }

    /// Move check for an already placed instruction, linear.
    fn can_move(&mut self, index: usize, position: Position) -> u32 {
        self.work.checks += 1;
        let insn = self.insn(index);
        let line = position.line;
        let row = self.fold(line).0;
        if row < insn.minimum_position + self.start {
            return POSITION;
        }
        let distance = insn.end_distance;
        if distance != 0 && self.end_limit >= 0 && self.end_limit - row <= distance {
            return POSITION;
        }
        if self.problem.stagger && insn.memory != 0 && row & 1 != 0 {
            return POSITION;
        }
        if (self.period == 0 && self.problem.horizon <= line)
            || (self.period > 0 && line / self.period > 16)
        {
            return HORIZON;
        }
        if !insn.acc_special {
            let ready = self.forced_ready(index);
            if ready < 0 || line < ready {
                return FORCED;
            }
        }
        let mut successor = IMPOSSIBLE;
        for &s in &insn.forced_successors {
            let placed = self.slots[s].line;
            if placed >= 0 && placed < successor {
                successor = placed;
            }
        }
        if successor <= line {
            return FORCED;
        }
        if row < 0 || row as usize >= self.rows.len() {
            return HORIZON;
        }
        let occupant = self.rows[row as usize][usize::from(position.pipe)];
        if occupant.is_some() && occupant != Some(index) {
            return SLOT_UNAVAILABLE;
        }
        let reserved = self
            .reserved
            .filter(|&(r, _)| r == row && position.pipe == 1)
            .map(|(_, branch)| branch);
        if reserved.is_some_and(|branch| branch != index) {
            return SLOT_UNAVAILABLE;
        }
        if !self.pairs(index, line, position.pipe) {
            return PAIRING;
        }
        self.can_move_locks(index, line)
    }

    /// Lock checks for moving a placed instruction, linear.
    fn can_move_locks(&mut self, index: usize, line: i32) -> u32 {
        let operands = self.insn(index).operands.len();
        let mut recalculated = Vec::new();
        for o in 0..operands {
            if self.problem.instructions[index].operands[o].direction != Direction::Read {
                continue;
            }
            for part in 0..4 {
                if let Some(input) = self.slots[index].locks[o][part]
                    && let Some(producer) = self.locks[input].parent
                {
                    self.calc_stats(producer, Some(index));
                    recalculated.push(producer);
                }
            }
        }
        let mut result = 0;
        'all: for o in 0..operands {
            let direction = self.problem.instructions[index].operands[o].direction;
            for part in 0..4 {
                let Some(lock) = self.slots[index].locks[o][part] else {
                    continue;
                };
                result |= match direction {
                    Direction::Read => self.can_move_input(lock, index, line),
                    Direction::Write => self.can_move_output(lock, line),
                };
                if result != 0 {
                    break 'all;
                }
            }
        }
        for &producer in recalculated.iter().rev() {
            self.calc_stats(producer, None);
        }
        result
    }

    /// Input lock check for moving a placed instruction.
    fn can_move_input(&self, input: usize, index: usize, line: i32) -> u32 {
        let list = self.locks[input].list;
        if self.lists[list].constant {
            return 0;
        }
        let Some(mut producer) = self.locks[input].parent else {
            return 0;
        };
        let period = self.period;
        let latency = i32::from(self.locks[input].latency);
        let mut line = line;
        let p = &self.locks[producer];
        let (mut write, mut ready, mut end) = (p.write, p.ready, p.end);
        let mut skip_external = false;
        if period > 0 && p.owner == Owner::External {
            let first = self.lists[list].first_external_output;
            if first.is_none_or(|f| !self.locks[f].inserted) {
                if p.user_count == 0 {
                    write = line - latency;
                    ready = line;
                    end = line;
                } else {
                    write = p.write.min(line - latency);
                    ready = p.ready.min(line - latency);
                    end = p.end.max(line);
                    if period <= end - write {
                        return 0x2000;
                    }
                }
            } else {
                let first = first.unwrap();
                if self.locks[first].instruction() == Some(index) {
                    return 0;
                }
                line += period;
                (write, ready, end) = self.concatenate(producer, first);
                skip_external = true;
                producer = first;
            }
        } else if period > 0
            && self.lists[list].external_owner
            && p.external_output != 0
            && let Some(&entry) = self.lists[list].sorted.first()
        {
            (write, ready, end) = self.concatenate(entry, producer);
            skip_external = true;
        }
        if period > 0 && crate::debug::switches().input_trace == Some(index) {
            crate::trace!(
                "    input {input} of {index} at {line}: producer {producer} {:?} write {write} ready {ready} end {end} latency {latency} users {} external {}",
                self.locks[producer].instruction(),
                self.locks[producer].user_count,
                skip_external
            );
        }
        if line - latency < ready {
            return 8;
        }
        if end < line {
            if period > 0 && period <= line - write {
                return 0x40000;
            }
            let (mut start, mut stop) = (write, line);
            if period > 0 {
                while period <= start {
                    start -= period;
                    stop -= period;
                }
            }
            for &other in &self.lists[list].outputs {
                let v = &self.locks[other];
                if other == producer || v.instruction() == Some(index) {
                    continue;
                }
                if skip_external && v.owner == Owner::External {
                    continue;
                }
                let (mut low, mut high) = (v.write_f, v.end_f);
                if period > 0 {
                    while period <= low {
                        low -= period;
                        high -= period;
                    }
                }
                if v.pending_users == 0 {
                    if self.overlap(start, stop, low, high) {
                        return 0x20;
                    }
                } else if start < high && low <= stop {
                    return 0x10;
                }
            }
        }
        0
    }

    /// Windows of a carried entry value followed by the
    /// value written for the next iteration.
    fn concatenate(&self, entry: usize, next: usize) -> (i32, i32, i32) {
        let e = &self.locks[entry];
        let n = &self.locks[next];
        if e.user_count != 0 {
            let end = (e.end + self.period).max(n.end);
            (n.write, n.ready, end)
        } else {
            (n.write, n.ready, n.end)
        }
    }

    fn can_move_output(&self, output: usize, line: i32) -> u32 {
        let mut next = line;
        let throughput = self.check_throughput(output, line, &mut next);
        if throughput != 0 {
            return throughput;
        }
        let l = &self.locks[output];
        let latency = i32::from(l.latency);
        let write = if l.blocking { line + 1 } else { latency + line };
        let ready = latency + line;
        let mut end = write;
        if l.total_users != 0 {
            let (high, low) = if l.sum48 < l.sum4a {
                (l.sum4a, l.sum48)
            } else {
                (l.sum48, l.sum4a)
            };
            let mut high = high;
            let spare = l.sum46 - (high - low);
            if spare > 0 {
                high += (spare + 1) >> 1;
            }
            if high > 0 {
                high -= 1;
            }
            end = ready + high;
        }
        if l.user_count != 0 {
            if end < l.last_use {
                end = l.last_use;
            }
            if l.first_use < ready {
                return 2;
            }
        }
        let list = l.list;
        if self.period > 0 {
            return self.can_move_output_loop(output, line, write, ready, end);
        }
        if l.external_output != 0 {
            for &other in &self.lists[list].outputs {
                if other != output && write <= self.locks[other].end {
                    return 0x1000;
                }
            }
        }
        for &other in &self.lists[list].outputs {
            if other == output {
                continue;
            }
            let v = &self.locks[other];
            if interval_overlap(write, end, v.write_f, v.end_f) {
                return 2;
            }
        }
        if self.lists[list].external_outputs != 0
            && !l.blocking
            && let Some(external) = self.lists[list].first_external_output
            && self.locks[external].inserted
        {
            if l.line == line {
                return 0;
            }
            if self.locks[external].line < line {
                crate::trace_if!(
                    move_trace,
                    "    external {:?} line {} self {}",
                    self.locks[external].owner,
                    self.locks[external].line,
                    external == output
                );
                return 0x200000;
            }
        }
        0
    }

    /// Loop branches of the original's output-move check.
    fn can_move_output_loop(
        &self,
        output: usize,
        line: i32,
        write: i32,
        ready: i32,
        mut end: i32,
    ) -> u32 {
        let period = self.period;
        let l = &self.locks[output];
        let list = l.list;
        let mut skip_external = false;
        let entry = self.lists[list].sorted.first().copied();
        if l.external_output == 0 {
            if self.lists[list].external_owner
                && let Some(entry) = entry
            {
                let e = &self.locks[entry];
                if e.user_count == 0 {
                    skip_external = true;
                } else if write < e.end {
                    return 0x4000;
                }
            }
        } else {
            for &other in &self.lists[list].outputs {
                if other != output && write <= self.locks[other].end {
                    return 0x1000;
                }
            }
            if self.lists[list].external_owner {
                skip_external = true;
                if let Some(entry) = entry {
                    let e = &self.locks[entry];
                    if e.user_count != 0 {
                        if e.write + period < ready {
                            return 2;
                        }
                        end = end.max(e.end + period);
                    }
                }
            }
        }
        if period < end - write {
            return 0x2000;
        }
        let row = self.fold(line).0;
        let (mut low, mut high) = (write + row - line, end + row - line);
        while period <= low {
            low -= period;
            high -= period;
        }
        for &other in &self.lists[list].outputs {
            if other == output {
                continue;
            }
            let v = &self.locks[other];
            if skip_external && v.owner == Owner::External {
                continue;
            }
            let (mut v_low, mut v_high) = (v.write_f, v.end_f);
            while period <= v_low {
                v_low -= period;
                v_high -= period;
            }
            if self.overlap(low, high, v_low, v_high) {
                return 2;
            }
        }
        0
    }

    /// Try `line` in each allowed pipe; lower first unless
    /// `upper_first`.
    fn try_move(&mut self, index: usize, line: i32, upper_first: bool) -> Option<Position> {
        let order: Vec<u8> = if self.insn(index).pipes == 3 {
            if upper_first { vec![0, 1] } else { vec![1, 0] }
        } else {
            vec![self.slots[index].pipe]
        };
        order
            .into_iter()
            .map(|pipe| Position { line, pipe })
            .find(|&position| self.can_move(index, position) == 0)
    }

    /// Sink a load (or a producer with dynamic depth zero)
    /// whose readers are all placed to the latest line that still feeds
    /// `index` in time. Uses the reader's placed line, so an unplaced reader
    /// sinks nothing.
    fn sink_loads(&mut self, index: usize) {
        let line = self.slots[index].line;
        for o in 0..self.insn(index).operands.len() {
            let operand = self.insn(index).operands[o].clone();
            if operand.direction != Direction::Read {
                continue;
            }
            for part in 0..4 {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                let Some(input) = self.slots[index].locks[o][part] else {
                    continue;
                };
                let Some(producer) = self.locks[input].parent else {
                    continue;
                };
                if self.locks[producer].pending_users != 0 {
                    continue;
                }
                let Some(setter) = self.locks[producer].instruction() else {
                    continue;
                };
                if self.insn(setter).memory != subtype::memory::LOAD
                    && self.slots[setter].depth != 0
                {
                    continue;
                }
                let at = self.slots[setter].line;
                if at < 0 {
                    continue;
                }
                let mut target = line
                    - i32::from(self.locks[producer].latency)
                    - i32::from(self.locks[input].latency);
                while at < target {
                    if let Some(position) = self.try_move(setter, target, true) {
                        self.relocate(setter, position);
                        break;
                    }
                    target -= 1;
                }
            }
        }
    }

    /// Sink ACC/I/MAC setters as late as their reader allows.
    /// The bound uses the reader's placed line, so an unplaced
    /// reader drags nothing.
    fn drag_accumulators(&mut self, index: usize) -> i32 {
        let line = self.slots[index].line;
        let mut moved = 0;
        for o in 0..self.insn(index).operands.len() {
            let operand = self.insn(index).operands[o].clone();
            if operand.direction != Direction::Read {
                continue;
            }
            let global = self.problem.registers[operand.register].global_id;
            if !matches!(global, Some(0x30 | 0x31 | 0x3b)) {
                continue;
            }
            for part in 0..4 {
                let Some(input) = self.slots[index].locks[o][part] else {
                    continue;
                };
                let Some(producer) = self.locks[input].parent else {
                    continue;
                };
                let Some(setter) = self.locks[producer].instruction() else {
                    continue;
                };
                let mut target = line
                    - i32::from(self.locks[producer].latency)
                    - i32::from(self.locks[input].latency);
                let from = self.slots[setter].line;
                while from < target {
                    if let Some(position) = self.try_move(setter, target, true) {
                        self.relocate(setter, position);
                        moved += 1;
                        break;
                    }
                    target -= 1;
                }
            }
        }
        moved
    }

    /// Drag placed setters of `index`'s inputs later so the
    /// value fits the period, recursively up to depth 4.
    fn drag_setters(&mut self, index: usize, line: i32, window: i32, depth: i32) -> i32 {
        if self.period == 0 {
            return 0;
        }
        self.drag_accumulators(index);
        self.sink_loads(index);
        let period = self.period;
        let mut total = 0;
        loop {
            let mut moved = 0;
            for o in 0..self.insn(index).operands.len() {
                let operand = self.insn(index).operands[o].clone();
                if operand.direction != Direction::Read
                    || self.problem.registers[operand.register].pool == 0
                        && self.problem.registers[operand.register].global_id.is_none()
                {
                    continue;
                }
                for part in 0..4 {
                    let Some(input) = self.slots[index].locks[o][part] else {
                        continue;
                    };
                    let Some(producer) = self.locks[input].parent else {
                        continue;
                    };
                    let Some(setter) = self.locks[producer].instruction() else {
                        continue;
                    };
                    // Only a placed setter can be dragged.
                    if !self.locks[producer].inserted {
                        continue;
                    }
                    let p_latency = i32::from(self.locks[producer].latency);
                    let mut latest = line - p_latency - i32::from(self.locks[input].latency);
                    let at = self.slots[setter].line;
                    let mut start = if window == 0 { at + 1 } else { line - window };
                    if !self.locks[producer].blocking {
                        start -= p_latency;
                    }
                    let mut first_fit = true;
                    if self.insn(setter).full_period {
                        start = at + 1;
                        latest = latest.min(at + period).min(self.insn(setter).move_target);
                        first_fit = false;
                    }
                    crate::trace_if!(
                        drag_trace,
                        "  drag-setters {index} line {line} window {window} depth {depth}: setter {setter} at {at} start {start} latest {latest}"
                    );
                    if at >= start || latest < start {
                        continue;
                    }
                    if depth < 4 {
                        let inner = if self.insn(setter).full_period {
                            period
                        } else {
                            window
                        };
                        moved += self.drag_setters(setter, start, inner, depth + 1);
                    }
                    let mut found = None;
                    for candidate in start..=latest {
                        if let Some(position) = self.try_move(setter, candidate, false) {
                            found = Some(position);
                            if first_fit {
                                break;
                            }
                        }
                    }
                    if let Some(position) = found {
                        self.relocate(setter, position);
                        moved += 1;
                    }
                }
            }
            total += moved;
            if moved == 0 {
                return total;
            }
        }
    }

    fn relocate(&mut self, index: usize, position: Position) {
        crate::trace_if!(
            move_trace,
            "move {index} {} {} -> {} {}",
            self.problem.names.get(index).map_or("?", String::as_str),
            self.slots[index].line,
            position.line,
            position.pipe
        );
        let old = &self.slots[index];
        let old_row = self.fold(old.line).0;
        self.rows[old_row as usize][usize::from(old.pipe)] = None;
        self.slots[index].line = position.line;
        self.slots[index].pipe = position.pipe;
        let row = self.fold(position.line).0;
        for o in 0..self.slots[index].locks.len() {
            let direction = self.problem.instructions[index].operands[o].direction;
            for part in 0..4 {
                let Some(lock) = self.slots[index].locks[o][part] else {
                    continue;
                };
                self.locks[lock].line = position.line;
                self.locks[lock].folded = row;
                match direction {
                    Direction::Write => {
                        self.calc_stats(lock, None);
                        let list = self.locks[lock].list;
                        self.lists[list].sorted.retain(|&l| l != lock);
                        let at = self.lists[list]
                            .sorted
                            .iter()
                            .position(|&other| position.line < self.locks[other].line)
                            .unwrap_or(self.lists[list].sorted.len());
                        self.lists[list].sorted.insert(at, lock);
                    }
                    Direction::Read => {
                        if let Some(parent) = self.locks[lock].parent {
                            self.calc_stats(parent, None);
                        }
                    }
                }
            }
        }
        self.rows[row as usize][usize::from(position.pipe)] = Some(index);
    }

    /// Sink producers toward their first use.
    pub fn reduce_extents(&mut self) {
        self.end_limit = self.end;
        loop {
            let mut moved = 0;

            for row in 0..self.end.max(0) {
                for slot in 0..2 {
                    let Some(index) = self.rows[row as usize][slot] else {
                        continue;
                    };
                    if self.sink_to_first_use(index) {
                        moved += 1;
                    }
                }
            }

            if moved == 0 {
                break;
            }
        }
    }

    /// Move one placed producer down toward its first use; true when it moved.
    fn sink_to_first_use(&mut self, index: usize) -> bool {
        let line = self.slots[index].line;
        let Some(first_use) = self.extent_first_use(index) else {
            return false;
        };

        let Some(position) = self.sink_target(index, first_use) else {
            return false;
        };

        crate::trace_if!(
            extent_trace,
            "extent {index} {line} -> {} first_use {first_use}",
            position.line
        );
        self.relocate(index, position);
        true
    }

    /// Earliest first use among an instruction's written components, or `None`
    /// when it has no used output or reads more components than it writes.
    fn extent_first_use(&self, index: usize) -> Option<i32> {
        let mut balance = 0;
        let mut first_use = IMPOSSIBLE;

        for (o, operand) in self.insn(index).operands.iter().enumerate() {
            match operand.direction {
                Direction::Write => {
                    for part in 0..4 {
                        if operand.mask & (1 << part) == 0 {
                            continue;
                        }
                        balance += 1;
                        if let Some(lock) = self.slots[index].locks[o][part] {
                            let l = &self.locks[lock];
                            if l.total_users != 0 && l.first_use < first_use {
                                first_use = l.first_use;
                            }
                        }
                    }
                }
                Direction::Read => balance -= operand.mask.count_ones() as i32,
            }
        }

        if first_use >= IMPOSSIBLE || balance < 0 {
            return None;
        }
        Some(first_use)
    }

    /// The latest line at or before `first_use` (and after the current one)
    /// where the instruction can move, in its own pipe.
    fn sink_target(&mut self, index: usize, first_use: i32) -> Option<Position> {
        let line = self.slots[index].line;
        let mut candidate = first_use;
        // Placement already specialized a dual-issue move to
        // its upper or lower form, so it sinks within that pipe.
        let pipe = self.slots[index].pipe;

        while line < candidate {
            if self.can_move(
                index,
                Position {
                    line: candidate,
                    pipe,
                },
            ) == 0
            {
                return Some(Position {
                    line: candidate,
                    pipe,
                });
            }
            candidate -= 1;
        }
        None
    }

    pub fn rows(&self) -> Vec<[Option<usize>; 2]> {
        self.rows[..self.end.max(0) as usize].to_vec()
    }

    pub fn placements(&self) -> Vec<Position> {
        self.slots
            .iter()
            .map(|s| Position {
                line: s.line,
                pipe: s.pipe,
            })
            .collect()
    }

    pub fn length(&self) -> i32 {
        self.end
    }

    pub fn start(&self) -> i32 {
        self.start
    }

    /// Install a fixed raw table in to-do
    /// (source) order: inputs, then outputs, at the given rows.
    pub fn from_rows(
        problem: &'a Problem,
        statics: &[Statics],
        rows: &[[Option<usize>; 2]],
        start: i32,
        end: i32,
    ) -> Self {
        let mut table = Table::new(problem, statics);
        table.start = start;
        table.end = end;
        let mut placed = vec![None; problem.instructions.len()];
        for (line, row) in rows.iter().enumerate() {
            for (pipe, entry) in row.iter().enumerate() {
                if let Some(index) = entry {
                    placed[*index] = Some(Position {
                        line: line as i32,
                        pipe: pipe as u8,
                    });
                }
            }
        }
        for index in table.todo.clone() {
            let Some(position) = placed[index] else {
                continue;
            };
            table.slots[index].line = position.line;
            table.slots[index].pipe = position.pipe;
            table.rows[position.line as usize][usize::from(position.pipe)] = Some(index);
            for direction in [Direction::Read, Direction::Write] {
                for o in 0..problem.instructions[index].operands.len() {
                    if problem.instructions[index].operands[o].direction != direction {
                        continue;
                    }
                    for part in 0..4 {
                        let Some(lock) = table.slots[index].locks[o][part] else {
                            continue;
                        };
                        table.locks[lock].line = position.line;
                        table.locks[lock].folded = position.line;
                        match direction {
                            Direction::Read => table.add_input_lock(lock),
                            Direction::Write => table.add_output_lock(lock),
                        }
                    }
                }
            }
            table.slots[index].serial = table.serial;
            table.serial += 1;
            table.done.push(index);
        }
        table.todo.retain(|&index| placed[index].is_none());
        table
    }

    /// Earliest line for a placed instruction.
    fn earliest_line(&self, index: usize) -> i32 {
        self.earliest_interlocked(index).0
    }

    /// Its flag: the bound came from an interlocked
    /// (blocking, offset-free) producer or a throughput predecessor.
    fn earliest_interlocked(&self, index: usize) -> (i32, bool) {
        let insn = self.insn(index);
        let mut earliest = insn.minimum_position + self.start;
        let mut interlock = -1;
        let line = self.slots[index].line;

        for (o, operand) in insn.operands.iter().enumerate() {
            match operand.direction {
                Direction::Read => {
                    self.earliest_after_producers(index, o, &mut earliest, &mut interlock);
                }
                Direction::Write => {
                    if !self.problem.registers[operand.register].throughput_tracked {
                        continue;
                    }
                    self.earliest_after_throughput(index, o, &mut earliest, &mut interlock);
                }
            }
        }

        if insn.branch || insn.protected_write {
            earliest = line;
        }
        (earliest, interlock == earliest)
    }

    /// Raise `earliest` past the placed producers of a read operand's
    /// components; an interlocked producer also sets `interlock`.
    fn earliest_after_producers(
        &self,
        index: usize,
        o: usize,
        earliest: &mut i32,
        interlock: &mut i32,
    ) {
        let operand = &self.insn(index).operands[o];

        for part in 0..4 {
            let Some(input) = self.slots[index].locks[o][part] else {
                continue;
            };
            if self.locks[input].mode == 2 {
                continue;
            }
            let Some(producer) = self.locks[input].parent else {
                continue;
            };
            let p = &self.locks[producer];
            if p.line >= 0 {
                let value = i32::from(p.latency) + p.line + i32::from(self.locks[input].latency);
                if *earliest <= value {
                    *earliest = value;
                    if operand.blocking && operand.input_offset == 0 {
                        *interlock = value;
                    }
                }
            }
        }
    }

    /// Raise `earliest` past the throughput predecessors of a written
    /// operand's components, setting `interlock` for each one.
    fn earliest_after_throughput(
        &self,
        index: usize,
        o: usize,
        earliest: &mut i32,
        interlock: &mut i32,
    ) {
        for part in 0..4 {
            let Some(output) = self.slots[index].locks[o][part] else {
                continue;
            };
            let list = self.locks[output].list;
            for &other in &self.lists[list].sorted {
                if other == output {
                    break;
                }
                let v = &self.locks[other];
                if v.line >= 0 {
                    let value = i32::from(v.throughput) + v.line;
                    if *earliest <= value {
                        *interlock = value;
                        *earliest = value;
                    }
                }
            }
        }
    }

    /// Latest line: branches stay; others may reach the end
    /// less their external tail.
    fn latest_line(&self, index: usize) -> i32 {
        let insn = self.insn(index);
        if insn.branch || insn.protected_read {
            return self.slots[index].line;
        }
        self.end - 1 - insn.end_distance
    }

    fn occupancy(&self, line: i32) -> usize {
        self.rows[line as usize]
            .iter()
            .filter(|e| e.is_some())
            .count()
    }

    fn single(&self, line: i32) -> Option<usize> {
        let row = self.rows[line as usize];
        match row {
            [Some(i), None] | [None, Some(i)] => Some(i),
            _ => None,
        }
    }

    fn try_place(&mut self, index: usize, line: i32, pipe_order_upper_first: bool) -> bool {
        let pipes = self.insn(index).pipes;
        let order: Vec<u8> = if pipes == 3 {
            if pipe_order_upper_first {
                vec![0, 1]
            } else {
                vec![1, 0]
            }
        } else {
            vec![self.slots[index].pipe]
        };
        for pipe in order {
            if self.can_move(index, Position { line, pipe }) == 0 {
                self.relocate(index, Position { line, pipe });
                return true;
            }
        }
        false
    }

    /// Move a lone instruction into a row with one occupant.
    fn move_single(&mut self, index: usize, upward: bool) {
        let line = self.slots[index].line;
        let (mut target, interlock, step) = if upward {
            let (earliest, flag) = self.earliest_interlocked(index);
            (earliest, flag, 1)
        } else {
            (self.latest_line(index), false, -1)
        };
        if target == line || (upward && target > line) || (!upward && target < line) {
            return;
        }
        if interlock {
            if self.try_place(index, target, upward) {
                return;
            }
            target += step;
        }
        while target != line {
            if self.occupancy(target) == 1 && self.try_place(index, target, upward) {
                return;
            }
            target += step;
        }
    }

    /// Size-reduction preparation.
    pub fn prepare_size_reduction(&mut self) {
        if self.start == self.end {
            return;
        }
        self.end_limit = self.end;
        for line in self.start..self.end {
            if let Some(index) = self.single(line) {
                self.move_single(index, true);
            }
        }
        for line in (self.start..self.end).rev() {
            if let Some(index) = self.single(line) {
                let (earliest, flag) = self.earliest_interlocked(index);
                if flag && earliest == self.slots[index].line {
                    continue;
                }
                self.move_single(index, false);
            }
        }
    }

    /// Move to the first fitting line from
    /// `from` toward (excluding) `to`.
    fn local_move(&mut self, index: usize, from: i32, to: i32, step: i32) -> bool {
        let pipes = self.insn(index).pipes;
        let order: [u8; 2] = if step < 0 { [0, 1] } else { [1, 0] };
        let mut line = from;
        while line != to {
            let candidates: &[u8] = if pipes == 3 {
                &order
            } else {
                std::slice::from_ref(&self.slots[index].pipe)
            };
            let candidates = candidates.to_vec();
            for pipe in candidates {
                let result = self.can_move(index, Position { line, pipe });
                if result == 0 {
                    self.relocate(index, Position { line, pipe });
                    return true;
                }
                crate::trace_if!(
                    move_trace,
                    "  try {index} {} at {line} {pipe}: {result:#x}",
                    self.problem.names.get(index).map_or("?", String::as_str)
                );
            }
            line += step;
        }
        false
    }

    fn branch_layout(&self) -> (Option<usize>, Option<usize>) {
        if self.end - 2 < self.start || self.end < 2 {
            return (None, None);
        }
        let branch = self.rows[(self.end - 2) as usize][1].filter(|&i| self.insn(i).branch);
        let Some(branch) = branch else {
            return (None, None);
        };
        let delay = self.rows[(self.end - 1) as usize][1].filter(|&d| {
            // The delay instruction stays attached only when the
            // branch reads a component that it writes.
            self.insn(branch).operands.iter().any(|read| {
                read.direction == Direction::Read
                    && self.insn(d).operands.iter().any(|write| {
                        write.direction == Direction::Write
                            && write.register == read.register
                            && write.mask & read.mask != 0
                    })
            })
        });
        (Some(branch), delay)
    }

    /// Pull-up: returns the number of rows removed at the end.
    fn pull_up(&mut self) -> Option<i32> {
        if self.end == self.start {
            return Some(0);
        }
        self.end_limit = self.end;
        let (branch, delay) = self.branch_layout();
        let mut last;
        loop {
            let mut moved = 0;
            last = self.start;
            for row in self.start..self.end {
                for slot in 0..2 {
                    let Some(index) = self.rows[row as usize][slot] else {
                        continue;
                    };
                    if Some(index) == branch || Some(index) == delay {
                        continue;
                    }
                    let line = self.slots[index].line;
                    let earliest = self.earliest_line(index);
                    if earliest < line && self.local_move(index, earliest, line, 1) {
                        crate::trace_if!(
                            raw_moves,
                            "  pull {index} {line} -> {}",
                            self.slots[index].line
                        );
                        moved += 1;
                    }
                    let reach = self.insn(index).end_distance + self.slots[index].line;
                    last = last.max(reach);
                    if self.end <= last {
                        crate::trace_if!(
                            wrangle_problem,
                            "  pull-up reach {index} {reach} end {}",
                            self.end
                        );
                        return None;
                    }
                }
            }
            if moved == 0 {
                break;
            }
        }
        if last - self.start + 1 < self.problem.minimum_length {
            last = self.problem.minimum_length - 1 + self.start;
        }
        let old_end = self.end;
        if let Some(branch) = branch {
            let mut line = (last - 1).max(self.start);
            let mut placed = false;
            while line < self.end {
                let pipe = self.slots[branch].pipe;
                let mut ok = false;
                crate::trace_if!(
                    wrangle_problem,
                    "  pull-up branch {branch} at {line}: {:#x}",
                    self.can_move(branch, Position { line, pipe })
                );
                if self.can_move(branch, Position { line, pipe }) == 0 {
                    self.relocate(branch, Position { line, pipe });
                    let next = line + 1;
                    last = last.max(next);
                    ok = true;
                    if let Some(delay) = delay {
                        if self.end <= next {
                            return None;
                        }
                        let pipe = self.slots[delay].pipe;
                        if self.can_move(delay, Position { line: next, pipe }) == 0 {
                            self.relocate(delay, Position { line: next, pipe });
                            last = last.max(next);
                        } else {
                            ok = false;
                        }
                    }
                }
                line += 1;
                if ok {
                    placed = true;
                    break;
                }
            }
            if !placed {
                crate::trace_if!(wrangle_problem, "  pull-up branch not placed");
                return None;
            }
        }
        if old_end < last + 1 {
            crate::trace_if!(wrangle_problem, "  pull-up end {old_end} < {}", last + 1);
            return None;
        }
        self.end = last + 1;
        self.end_limit = self.end;
        Some(old_end - last - 1)
    }

    /// Drag-down: returns how far the start advanced.
    fn drag_down(&mut self) -> Option<i32> {
        if self.end == self.start {
            return Some(0);
        }
        self.end_limit = self.end;
        let old_start = self.start;
        let mut new_start;

        loop {
            let mut moved = 0;
            new_start = self.end;

            for row in (self.start..self.end).rev() {
                for slot in 0..2 {
                    let Some(index) = self.rows[row as usize][slot] else {
                        continue;
                    };
                    if !self.insn(index).branch && self.drag_one(index) {
                        moved += 1;
                    }
                    let value = self.slots[index].line - self.insn(index).minimum_position;
                    new_start = new_start.min(value);
                    if new_start < old_start {
                        new_start = old_start;
                    }
                }
            }

            if self.end <= new_start {
                return Some(0);
            }
            if moved == 0 {
                break;
            }
        }

        if self.end - new_start < self.problem.minimum_length {
            new_start = self.end - self.problem.minimum_length;
        }
        self.start = new_start;
        self.end_limit = self.end;
        Some(new_start - old_start)
    }

    /// Drag one non-branch instruction down toward its bound; true when it moved.
    fn drag_one(&mut self, index: usize) -> bool {
        let line = self.slots[index].line;
        let bound = self.drag_bound(index, line);
        crate::trace_if!(
            drag_trace,
            "  drag {index} {} line {line} bound {bound}",
            self.problem.names.get(index).map_or("?", String::as_str)
        );

        if line < bound && self.local_move(index, bound, line, -1) {
            crate::trace_if!(
                raw_moves,
                "  drag {index} {line} -> {}",
                self.slots[index].line
            );
            return true;
        }
        false
    }

    /// The latest line a drag-down may place an instruction at, from its end
    /// distance and the first use of each written component.
    fn drag_bound(&self, index: usize, line: i32) -> i32 {
        let mut bound = self.end - 1 - self.insn(index).end_distance;

        for (o, operand) in self.insn(index).operands.iter().enumerate() {
            if operand.direction != Direction::Write {
                continue;
            }
            for part in 0..4 {
                if operand.mask & (1 << part) == 0 {
                    continue;
                }
                let mut b = self.end - 1;
                if let Some(lock) = self.slots[index].locks[o][part] {
                    let l = &self.locks[lock];
                    if l.user_count != 0 {
                        b = l.first_use - i32::from(l.latency);
                    }
                    crate::trace_if!(
                        drag_trace,
                        "    out reg {} part {part} users {} first {} latency {} users {:?}",
                        operand.register,
                        l.user_count,
                        l.first_use,
                        l.latency,
                        l.users
                            .iter()
                            .map(|&u| (self.locks[u].owner, self.locks[u].line))
                            .collect::<Vec<_>>()
                    );
                }
                bound = bound.min(b);
            }
        }

        if self.insn(index).protected_read {
            bound = line;
        }
        bound
    }

    /// Linear raw reduction.
    pub fn raw_reduce(&mut self, pull: bool, drag: bool) -> bool {
        let rounds = u32::from(pull) + u32::from(drag);
        let mut remaining = rounds;
        while remaining != 0 {
            let mut next = remaining - 1;
            if pull {
                match self.pull_up() {
                    None => return false,
                    Some(0) => {}
                    Some(_) => next = rounds,
                }
            }
            remaining = next;
            if drag {
                match self.drag_down() {
                    None => return false,
                    Some(0) => {}
                    Some(_) => remaining = rounds,
                }
            }
        }
        true
    }

    /// Does `reader` read a component that `writer` writes?
    fn depends(&self, writer: usize, reader: usize) -> bool {
        self.insn(reader).operands.iter().any(|read| {
            read.direction == Direction::Read
                && self.insn(writer).operands.iter().any(|write| {
                    write.direction == Direction::Write
                        && write.register == read.register
                        && write.mask & read.mask != 0
                })
        })
    }

    /// Branch-delay exchange: lift the branch over the lower
    /// instruction before it when the final row is empty.
    pub fn fiddle_branch_delay(&mut self) {
        self.end_limit = self.end;
        let length = self.end - self.start;
        if length <= self.problem.minimum_length || length < 3 {
            return;
        }
        let Some(branch) = self.rows[(self.end - 2) as usize][1].filter(|&i| self.insn(i).branch)
        else {
            return;
        };
        if length - 3 < self.insn(branch).minimum_position {
            return;
        }
        if self.occupancy(self.end - 1) != 0 {
            return;
        }
        let branch_line = self.end - 3;
        let prev = self.rows[branch_line as usize][1];
        if let Some(prev) = prev
            && self.depends(prev, branch)
        {
            return;
        }
        // Take out the previous instruction while checking the branch.
        if prev.is_some() {
            self.rows[branch_line as usize][1] = None;
        }
        let branch_ok = self.can_move(
            branch,
            Position {
                line: branch_line,
                pipe: 1,
            },
        ) == 0;
        if let Some(prev) = prev {
            self.rows[branch_line as usize][1] = Some(prev);
        }
        if !branch_ok {
            return;
        }
        let mut swap = true;
        if let Some(prev) = prev {
            self.rows[(self.end - 2) as usize][1] = None;
            let ok = self.can_move(
                prev,
                Position {
                    line: self.end - 2,
                    pipe: 1,
                },
            ) == 0;
            self.rows[(self.end - 2) as usize][1] = Some(branch);
            swap = self.insn(prev).end_distance == 0 && ok;
        }
        if !swap {
            return;
        }
        self.rows[(self.end - 2) as usize][1] = None;
        if prev.is_some() {
            self.rows[branch_line as usize][1] = None;
        }
        self.slots[branch].line = -1;
        self.place_at(branch, branch_line, 1);
        if let Some(prev) = prev {
            self.place_at(prev, self.end - 2, 1);
        }
        self.end -= 1;
        self.end_limit = self.end;
    }

    /// Move an instruction whose slot was already vacated.
    fn place_at(&mut self, index: usize, line: i32, pipe: u8) {
        let current = self.slots[index].line;
        if current >= 0
            && self.rows[current as usize][usize::from(self.slots[index].pipe)] == Some(index)
        {
            self.rows[current as usize][usize::from(self.slots[index].pipe)] = None;
        }
        self.slots[index].line = line;
        self.slots[index].pipe = pipe;
        for o in 0..self.slots[index].locks.len() {
            let direction = self.problem.instructions[index].operands[o].direction;
            for part in 0..4 {
                let Some(lock) = self.slots[index].locks[o][part] else {
                    continue;
                };
                self.locks[lock].line = line;
                self.locks[lock].folded = line;
                match direction {
                    Direction::Write => {
                        self.calc_stats(lock, None);
                        let list = self.locks[lock].list;
                        self.lists[list].sorted.retain(|&l| l != lock);
                        let at = self.lists[list]
                            .sorted
                            .iter()
                            .position(|&other| line < self.locks[other].line)
                            .unwrap_or(self.lists[list].sorted.len());
                        self.lists[list].sorted.insert(at, lock);
                    }
                    Direction::Read => {
                        if let Some(parent) = self.locks[lock].parent {
                            self.calc_stats(parent, None);
                        }
                    }
                }
            }
        }
        self.rows[line as usize][usize::from(pipe)] = Some(index);
    }

    fn cap_at(&self, row: i32, slot: u8) -> bool {
        self.caps.iter().any(|c| c.row == row && c.slot == slot)
    }

    fn slot_used(&self, row: i32, slot: u8) -> bool {
        self.rows[row as usize][usize::from(slot)].is_some() || self.cap_at(row, slot)
    }

    fn entries(&self, row: i32) -> usize {
        (0..2).filter(|&slot| self.slot_used(row, slot)).count()
    }

    fn row_reads(&self, row: i32, register: usize) -> bool {
        if self.skipped.get(row as usize).copied().unwrap_or(false) {
            return false;
        }
        let instructions = self.rows[row as usize].iter().flatten().any(|&i| {
            self.insn(i)
                .operands
                .iter()
                .any(|o| o.direction == Direction::Read && o.register == register)
        });
        instructions
            || self
                .caps
                .iter()
                .any(|c| c.row == row && c.source == Some(register))
    }

    fn row_write_mask(&self, row: i32, register: usize) -> u8 {
        if self.skipped.get(row as usize).copied().unwrap_or(false) {
            return 0;
        }
        self.rows[row as usize]
            .iter()
            .flatten()
            .flat_map(|&i| self.insn(i).operands.iter())
            .filter(|o| o.direction == Direction::Write && o.register == register)
            .fold(0, |mask, o| mask | o.mask)
    }

    fn global(&self, id: u8) -> Option<usize> {
        self.problem
            .registers
            .iter()
            .position(|r| r.global_id == Some(id))
    }

    /// Cap eligibility: returns allowed pipes, source and mask.
    fn can_cap(
        &self,
        index: usize,
        operand: usize,
        scratch: &dyn Fn(usize) -> Option<usize>,
    ) -> Option<(u8, CapKind, Option<usize>, u8)> {
        let register = self.insn(index).operands[operand].register;
        match self.problem.registers[register].global_id {
            Some(0x32) => Some((2, CapKind::WaitQ, Some(register), 1)),
            Some(0x34) => Some((2, CapKind::WaitP, self.global(0x33), 1)),
            Some(0x37 | 0x39 | 0x3b) => {
                if self.slots[index].pipe != 0 {
                    return None;
                }
                let first = self.insn(index).first_output?;
                if !first.vector {
                    return None;
                }
                match first.register {
                    Some(source) => Some((1, CapKind::Abs, Some(source), first.mask)),
                    None => scratch(index).map(|s| (1, CapKind::Abs, Some(s), first.mask)),
                }
            }
            _ => None,
        }
    }

    /// Cap insertion scan.
    pub fn insert_caps(&mut self, scratch: &dyn Fn(usize) -> Option<usize>) -> usize {
        if self.start == self.end {
            return 0;
        }
        self.stamps = (0..self.rows.len() as i32)
            .map(|r| r - self.start)
            .collect();
        self.skipped = vec![false; self.rows.len()];
        let mut inserted = 0;

        for row in self.start..self.end {
            let now = self.stamps[row as usize];
            for slot in 0..2 {
                let Some(index) = self.rows[row as usize][slot] else {
                    continue;
                };
                for o in 0..self.insn(index).operands.len() {
                    if self.insert_cap(row, now, index, o, scratch) {
                        inserted += 1;
                    }
                }
            }
        }
        inserted
    }

    /// Try to add a cap after one written operand of the instruction at
    /// `row`; true when one was added.
    fn insert_cap(
        &mut self,
        row: i32,
        now: i32,
        index: usize,
        o: usize,
        scratch: &dyn Fn(usize) -> Option<usize>,
    ) -> bool {
        let operand = &self.insn(index).operands[o];
        if operand.direction != Direction::Write || operand.blocking || !operand.observed {
            return false;
        }
        let Some((pipes, kind, source, mask)) = self.can_cap(index, o, scratch) else {
            return false;
        };
        let written = operand.register;
        let mut latency = i32::from(operand.latency);
        if self.problem.registers[written].global_id == Some(0x34) && latency > 1 {
            latency -= 1;
        }

        let candidate = self.cap_candidate(row, now, written, latency, pipes, source, mask);
        let Some((line, p, mask, busy)) = candidate else {
            return false;
        };
        if latency <= busy {
            return false;
        }

        self.caps.push(Cap {
            row: line,
            slot: p,
            kind,
            producer: index,
            operand: match kind {
                CapKind::Abs => self
                    .insn(index)
                    .operands
                    .iter()
                    .position(|w| w.direction == Direction::Write && Some(w.register) == source),
                CapKind::WaitQ => Some(o),
                CapKind::WaitP => self
                    .insn(index)
                    .operands
                    .iter()
                    .position(|w| w.direction == Direction::Write && Some(w.register) == source),
            },
            mask,
            source,
        });
        true
    }

    /// Scan the rows after `row` for the last free slot a cap could take:
    /// returns (line, pipe, mask, busy rows passed).
    #[allow(clippy::too_many_arguments)]
    fn cap_candidate(
        &self,
        row: i32,
        now: i32,
        written: usize,
        latency: i32,
        pipes: u8,
        source: Option<usize>,
        mut mask: u8,
    ) -> Option<(i32, u8, u8, i32)> {
        let mut candidate: Option<(i32, u8, u8, i32)> = None;
        let mut busy = 0;
        let mut line = row;

        loop {
            line += 1;
            if line >= self.end || mask == 0 {
                break;
            }
            if latency + now <= self.stamps[line as usize] {
                for p in 0..2u8 {
                    if pipes >> p & 1 == 0 || self.slot_used(line, p) {
                        continue;
                    }
                    if self.entries(line) == 0 && candidate.is_some() {
                        break;
                    }
                    candidate = Some((line, p, mask, busy));
                    break;
                }
            }
            if self.row_reads(line, written) {
                break;
            }
            if self.entries(line) != 0 {
                busy += 1;
            }
            if let Some(source) = source {
                mask &= !self.row_write_mask(line, source);
            }
        }
        candidate
    }

    /// Users of one written component: placed lock users and caps, in row
    /// order, with each user's blocking flag.
    fn component_users(&self, index: usize, operand: usize, part: usize) -> Vec<(i32, bool)> {
        let mut users = Vec::new();
        if let Some(lock) = self.slots[index].locks[operand][part] {
            for &user in &self.locks[lock].users {
                if let Owner::Instruction(u, o) = self.locks[user].owner {
                    users.push((self.slots[u].line, self.insn(u).operands[o].blocking));
                }
            }
        }
        for cap in &self.caps {
            if cap.producer == index && cap.operand == Some(operand) && cap.mask >> part & 1 != 0 {
                users.push((cap.row, true));
            }
        }
        users.sort_by_key(|&(row, _)| row);
        users
    }

    /// Empty-row compression. `last_row_ok` is end kind 1 with
    /// no branch; `ordinary_end` is end kind 1.
    pub fn compress(&mut self, last_row_ok: bool, ordinary_end: bool) {
        if self.start == self.end || self.end - self.start < self.problem.minimum_length {
            return;
        }
        if self.stamps.len() != self.rows.len() {
            self.stamps = (0..self.rows.len() as i32)
                .map(|r| r - self.start)
                .collect();
            self.skipped = vec![false; self.rows.len()];
        }
        let mut length = self.end - self.start;

        for row in self.start..self.end {
            if self.entries(row) != 0 {
                continue;
            }
            let now = self.stamps[row as usize];
            let flights = self.in_flight_writes(row, now);
            let mut empty = EmptyRow {
                row,
                now,
                recovery: self.end,
                flights,
            };
            empty.recovery = self.recovery_row(&empty);

            if self.can_skip_row(&empty, length, last_row_ok, ordinary_end) {
                let recovery = empty.recovery;
                self.skipped[row as usize] = true;
                for line in row..recovery.min(self.end) {
                    self.stamps[line as usize] -= 1;
                }
                if recovery >= self.end {
                    length -= 1;
                }
            }
        }
    }

    /// In-flight writes issued by rows before `row`, still pending at `now`.
    fn in_flight_writes(&self, row: i32, now: i32) -> Vec<Flight> {
        let mut flights = Vec::new();

        for earlier in self.start..row {
            if self.skipped[earlier as usize] {
                continue;
            }
            for &index in self.rows[earlier as usize].iter().flatten() {
                for (o, operand) in self.insn(index).operands.iter().enumerate() {
                    if operand.direction != Direction::Write {
                        continue;
                    }
                    let ready = self.stamps[earlier as usize] + i32::from(operand.latency);
                    if ready <= now {
                        continue;
                    }
                    for part in 0..4 {
                        if operand.mask >> part & 1 != 0 {
                            flights.push(Flight {
                                index,
                                operand: o,
                                part,
                                remaining: ready - now,
                            });
                        }
                    }
                }
            }
        }
        flights
    }

    /// The first row after an empty row where pending writes land and the
    /// rows below could shift up (the end when none does).
    fn recovery_row(&self, empty: &EmptyRow) -> i32 {
        let (row, now) = (empty.row, empty.now);
        let mut recovery = self.end;

        for cap in &self.caps {
            if cap.operand.is_some() || cap.kind != CapKind::Abs {
                continue;
            }
            let line = self.slots[cap.producer].line;
            if line < self.start || line >= row || self.skipped[line as usize] {
                continue;
            }
            let latency = self
                .insn(cap.producer)
                .first_output
                .map_or(1, |f| i32::from(f.latency));
            let ready = self.stamps[line as usize] + latency;
            if latency > 1 && ready > now && self.stamps[cap.row as usize] == ready {
                recovery = recovery.min(cap.row);
            }
        }

        for flight in &empty.flights {
            let operand = &self.insn(flight.index).operands[flight.operand];
            let register = operand.register;
            if self.problem.registers[register].throughput_tracked {
                let limit = if self.problem.registers[register].global_id == Some(0x34) {
                    flight.remaining - 1
                } else {
                    flight.remaining
                };
                let mut line = row;
                loop {
                    line += 1;
                    if line >= self.end || now + limit < self.stamps[line as usize] {
                        break;
                    }
                    if self.row_write_mask(line, register) != 0 {
                        if self.stamps[line as usize] == now + limit {
                            recovery = recovery.min(line);
                        }
                        break;
                    }
                }
            }
            if operand.latency > 1 {
                for (user_row, blocking) in
                    self.component_users(flight.index, flight.operand, flight.part)
                {
                    if !blocking {
                        continue;
                    }
                    let tick = self.stamps[user_row as usize];
                    if tick == now + flight.remaining && user_row < recovery {
                        recovery = user_row;
                    }
                }
            }
        }
        recovery
    }

    /// Whether removing the empty row keeps every timing constraint.
    fn can_skip_row(
        &self,
        empty: &EmptyRow,
        length: i32,
        last_row_ok: bool,
        ordinary_end: bool,
    ) -> bool {
        let (row, now, recovery) = (empty.row, empty.now, empty.recovery);

        if row == self.end - 1 && !last_row_ok {
            return false;
        }
        if self.unblocked_user_too_close(empty) {
            return false;
        }
        if row < self.end - 1 && row + 1 < recovery && self.early_branch_slack(row + 1, now) == 1 {
            return false;
        }
        if recovery < self.end && self.shift_breaks_read(empty) {
            return false;
        }

        for line in row + 1..recovery.min(self.end) {
            let tick = self.stamps[line as usize];
            if self.rows[line as usize]
                .iter()
                .flatten()
                .any(|&i| tick - 1 < self.insn(i).minimum_position)
            {
                return false;
            }
        }

        if recovery >= self.end {
            let distance = length - 1 - now;
            for line in self.start..recovery.min(self.end) {
                if self.rows[line as usize]
                    .iter()
                    .flatten()
                    .any(|&i| self.insn(i).end_distance < distance)
                {
                    return false;
                }
            }
            return self.problem.minimum_length < length && (ordinary_end || length > 2);
        }
        true
    }

    /// True when a pending non-blocking write has a non-blocking first user
    /// that would then read it before it lands.
    fn unblocked_user_too_close(&self, empty: &EmptyRow) -> bool {
        for flight in &empty.flights {
            let operand = &self.insn(flight.index).operands[flight.operand];
            let Some(&(user_row, user_blocking)) = self
                .component_users(flight.index, flight.operand, flight.part)
                .first()
            else {
                continue;
            };
            if !operand.blocking
                && !user_blocking
                && user_row < empty.recovery
                && self.stamps[user_row as usize] - 1 < empty.now + flight.remaining
            {
                return true;
            }
        }
        false
    }

    /// True when shifting the rows between the empty row and its recovery row
    /// up would land a non-blocking write on a row that reads the register.
    fn shift_breaks_read(&self, empty: &EmptyRow) -> bool {
        let recovery = empty.recovery;
        let target = self.stamps[recovery as usize];

        for line in empty.row + 1..recovery {
            let tick = self.stamps[line as usize];
            for &index in self.rows[line as usize].iter().flatten() {
                for operand in &self.insn(index).operands {
                    if operand.direction != Direction::Write || operand.blocking {
                        continue;
                    }
                    let ready = i32::from(operand.latency) + tick - 1;
                    if ready < target {
                        let mut k = recovery;
                        while k < self.end && self.stamps[k as usize] < ready {
                            k += 1;
                        }
                        if k < self.end
                            && self.stamps[k as usize] == ready
                            && self.row_reads(k, operand.register)
                        {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Remaining issue lock of an early-sampled lower read.
    fn early_branch_slack(&self, row: i32, now: i32) -> i32 {
        let Some(index) = self.rows[row as usize][1] else {
            return 0;
        };
        let mut slack = 0;
        for (o, operand) in self.insn(index).operands.iter().enumerate() {
            if operand.direction != Direction::Read || operand.input_offset == 0 {
                continue;
            }
            let Some(input) = self.slots[index].locks[o][0] else {
                continue;
            };
            let Some(producer) = self.locks[input].parent else {
                continue;
            };
            // The original reads the register's countdown; a value
            // entering the table counts from the first row, so a branch on an
            // `--enter` input keeps one row after it (an `add`/`ibeq` probe).
            let stamp = match self.locks[producer].instruction() {
                Some(owner) => {
                    let line = self.slots[owner].line;
                    if line < self.start || self.skipped[line as usize] {
                        continue;
                    }
                    self.stamps[line as usize]
                }
                None => 0,
            };
            let value =
                stamp + i32::from(self.locks[producer].latency) + i32::from(operand.input_offset)
                    - now;
            if value > 0 && (slack == 0 || value < slack) {
                slack = value;
            }
        }
        slack
    }

    /// Append Tidy padding rows.
    pub fn pad(&mut self, rows: usize) {
        for _ in 0..rows {
            if self.rows.len() <= self.end as usize {
                self.rows.push([None, None]);
            }
            self.end += 1;
        }
    }

    /// Physical rows with caps and skip flags.
    pub fn compressed(&self) -> Compressed {
        let window = self.start.max(0) as usize..self.end.max(0) as usize;
        Compressed {
            rows: self.rows[window.clone()].to_vec(),
            caps: window
                .clone()
                .map(|r| self.caps.iter().find(|c| c.row == r as i32).copied())
                .collect(),
            skipped: window
                .map(|r| self.skipped.get(r).copied().unwrap_or(false))
                .collect(),
        }
    }

    /// Rows occupied by a
    /// register's values, component bits plus `0x80` at each write row.
    fn raster(&self, register: usize) -> Vec<u8> {
        let length = self.end.max(0) as usize;
        let mut rows = vec![0u8; length];
        let parts = usize::from(self.problem.registers[register].parts.min(4));
        for part in 0..parts {
            let Some(list) = self.list_of[register][part] else {
                continue;
            };
            for &value in &self.lists[list].outputs {
                let lock = &self.locks[value];
                // Live range; a loop value that escapes ends
                // with its iteration block.
                let period = self.period;
                let mut start = match lock.owner {
                    Owner::External => -1,
                    Owner::Instruction(i, _) => self.slots[i].line,
                };
                let mut end = if lock.external_output == 0 {
                    -1
                } else if period == 0 || start < 0 {
                    IMPOSSIBLE
                } else {
                    start + period - start % period
                };
                for &user in &lock.users {
                    if self.locks[user].mode == 2 {
                        continue;
                    }
                    if let Owner::Instruction(u, _) = self.locks[user].owner {
                        end = end.max(self.slots[u].line);
                    }
                }
                if end < 0 {
                    end = start + 1;
                }
                // An incoming loop value starts with the
                // iteration block of its last use; rows fold by period.
                if start < 0 {
                    start = if period == 0 || end == IMPOSSIBLE {
                        0
                    } else {
                        end / period * period
                    };
                }
                if end == IMPOSSIBLE {
                    end = if period == 0 {
                        self.end
                    } else {
                        period + start - start % period
                    };
                }
                if period == 0 {
                    for row in start.max(0)..end.min(self.end) {
                        rows[row as usize] |= 1 << part;
                    }
                } else {
                    for line in start..end {
                        rows[(line % period) as usize] |= 1 << part;
                    }
                }
                if lock.folded >= 0 && (lock.folded as usize) < length {
                    rows[lock.folded as usize] |= 0x80;
                }
            }
        }
        rows
    }

    /// Local allocation for one pool: forced registers first,
    /// then virtual registers by largest occupied-row count, each placed by
    /// best packing cost. `allowed[physical]`
    /// holds the usable components; `through` lists physical registers live
    /// across the table without a reference.
    pub fn allocate(
        &self,
        pool: u8,
        allowed: &[u8],
        through: &[(usize, u8)],
    ) -> Option<Vec<(usize, usize)>> {
        let length = self.end.max(0) as usize;
        let count = allowed.len();
        let parts = if pool == 1 { 4 } else { 1 };
        let mut occupied = vec![vec![0u8; count]; length];
        for (register, info) in self.problem.registers.iter().enumerate() {
            if info.pool != pool {
                continue;
            }
            if let Some(physical) = info.physical
                && physical < count
            {
                for (row, bits) in self.raster(register).into_iter().enumerate() {
                    occupied[row][physical] |= bits;
                }
                // A through part without a reference has no lock list;
                // its value still occupies the whole table.
                let through = (0..usize::from(info.parts.min(4)))
                    .filter(|&part| {
                        info.through & (1 << part) != 0 && self.list_of[register][part].is_none()
                    })
                    .fold(0u8, |mask, part| mask | 1 << part);
                for row in occupied.iter_mut() {
                    row[physical] |= through;
                }
            }
        }
        for &(physical, mask) in through {
            if physical < count {
                for row in occupied.iter_mut() {
                    row[physical] |= mask & ((1u8 << parts) - 1);
                }
            }
        }
        // Only values confined to this table are local; a virtual register
        // reaching another table belongs to external allocation.
        // A reader retargeted to a synthesized move keeps the source it had
        // (an outside value); the move's register is still local.
        let move_registers: std::collections::BTreeSet<usize> = self
            .problem
            .instructions
            .iter()
            .filter(|insn| insn.is_move)
            .map(|insn| insn.operands[0].register)
            .collect();
        let mut external = vec![false; self.problem.registers.len()];
        for insn in &self.problem.instructions {
            for operand in &insn.operands {
                let outside = operand.sources.contains(&Some(Source::External))
                    && !move_registers.contains(&operand.register);
                if operand.escapes != 0 || outside {
                    external[operand.register] = true;
                }
            }
        }
        let mut pending: Vec<(usize, usize)> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        // A raw table's instruction list is linked in dispatch-row order.
        let order: Vec<usize> = (self.start.max(0)..self.end.max(0))
            .flat_map(|row| self.rows[row as usize].into_iter().flatten())
            .collect();
        for &index in &order {
            let insn = &self.problem.instructions[index];
            for operand in &insn.operands {
                let info = &self.problem.registers[operand.register];
                if info.pool != pool
                    || info.physical.is_some()
                    || external[operand.register]
                    || !seen.insert(operand.register)
                {
                    continue;
                }
                let priority = self
                    .raster(operand.register)
                    .iter()
                    .filter(|&&bits| bits != 0)
                    .count();
                pending.insert(0, (operand.register, priority));
            }
        }
        let mut result = Vec::new();
        while !pending.is_empty() {
            let mut best = 0;
            let mut best_priority = -1i64;
            for (index, &(_, priority)) in pending.iter().enumerate() {
                if best_priority < priority as i64 {
                    best_priority = priority as i64;
                    best = index;
                }
            }
            let (register, _) = pending.remove(best);
            let want = self.raster(register);
            let mut choice = None;
            let mut lowest = i64::MAX;
            for physical in 0..count {
                let mut cost = 0i64;
                let mut fits = true;
                for row in 0..length {
                    let w = want[row];
                    let used = occupied[row][physical];
                    if allowed[physical] & w & 0x7f != w & 0x7f || used & w != 0 {
                        fits = false;
                        break;
                    }
                    cost += parts as i64 - i64::from(((w | used) & 0x7f).count_ones());
                }
                if crate::debug::switches().alloc_costs {
                    let used: Vec<String> = (0..length)
                        .filter(|&r| occupied[r][physical] != 0)
                        .map(|r| format!("{r}:{:x}", occupied[r][physical]))
                        .collect();
                    crate::trace!(
                        "  cost reg {register} phys {physical} fits {fits} cost {cost} used {}",
                        used.join(",")
                    );
                }
                if !fits {
                    continue;
                }
                if cost == 0 {
                    choice = Some(physical);
                    break;
                }
                if cost < lowest {
                    lowest = cost;
                    choice = Some(physical);
                }
            }
            let physical = choice?;
            if crate::debug::switches().alloc_trace {
                let rows: Vec<String> = want
                    .iter()
                    .enumerate()
                    .filter(|&(_, &b)| b != 0)
                    .map(|(r, b)| format!("{r}:{b:x}"))
                    .collect();
                crate::trace!(
                    "alloc pool {pool} register {register} priority {best_priority} -> {physical} cost {lowest} rows {}",
                    rows.join(",")
                );
            }
            for row in 0..length {
                occupied[row][physical] |= want[row];
            }
            result.push((register, physical));
        }
        Some(result)
    }

    /// Rows in `[start, end)`.
    pub fn window(&self) -> Vec<[Option<usize>; 2]> {
        self.rows[self.start.max(0) as usize..self.end.max(0) as usize].to_vec()
    }
}

thread_local! {
    static TRACE_BLOCK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Label for `VCL_WRANGLE_TRACE` diagnostics.
pub fn set_trace_block(block: usize) {
    TRACE_BLOCK.with(|b| b.set(block));
}

fn trace_block() -> usize {
    TRACE_BLOCK.with(std::cell::Cell::get)
}

/// One loop candidate kept by the loop optimizer.
#[derive(Clone, Debug)]
pub struct LoopOutcome {
    pub strategy: Strategy,
    pub period: i32,
    /// Folded rows (`period` entries).
    pub rows: Vec<[Option<usize>; 2]>,
    /// Absolute placement per instruction.
    pub placements: Vec<Position>,
    /// Maximum iteration ID.
    pub max_iteration: i32,
    /// The problem after the search: synthesized moves are appended after
    /// the source instructions, and retargeted operands name their move
    /// registers (appended after the source registers).
    pub problem: Problem,
    /// Loop local allocation: problem register to physical index.
    pub allocation: Vec<(usize, usize)>,
}

/// The loop optimizer: from `first_period` upward, try every
/// strategy (index 10 down to 0) and keep the successful table with the
/// smallest maximum iteration ID, a later tie replacing an earlier one. The
/// original's per-strategy watchdog is `budget` wrangler steps.
///
/// Not yet ported: loop local allocation (every wrangled candidate is
/// accepted; see `docs/LPGEN_PORT.md`).
pub fn optimize_loop(
    problem: &Problem,
    first_period: i32,
    last_period: i32,
    known_safe: i32,
    budget: usize,
    pools: Option<&Pools>,
) -> Option<LoopOutcome> {
    let statics = statics(problem);
    for period in first_period.max(2)..=last_period {
        let mut kept: Option<LoopOutcome> = None;
        for strategy in Strategy::ALL.into_iter().rev() {
            let mut table = Table::new_loop(problem, &statics, period, known_safe);
            let result = table.wrangle(strategy, budget);
            if crate::debug::switches().lpgen_trace {
                let (status, trace) = match &result {
                    Ok(trace) => ("ok", trace.clone()),
                    Err(failure) => (
                        if *failure == Failure::Budget {
                            "budget"
                        } else {
                            "failed"
                        },
                        table.failed_trace.clone(),
                    ),
                };
                crate::trace!(
                    "lpgen {} period {period} {:?} {status} base {} trace {:?}",
                    trace_block(),
                    strategy,
                    problem.instructions.len(),
                    trace
                );
                let w = table.work;
                crate::trace!(
                    "lpwork {} period {period} {:?} {status} todo {} steps {} inserts {} undos {} checks {} ranked {}",
                    trace_block(),
                    strategy,
                    table.todo_at_start,
                    w.steps,
                    w.inserts,
                    w.undos,
                    w.checks,
                    w.ranked
                );
            }
            if result.is_err() {
                continue;
            }
            // Reduce extents over the folded rows.
            table.reduce_extents();
            // Loop local allocation rejects a candidate.
            let mut allocation = Vec::new();
            if let Some(pools) = pools {
                let vf = table.allocate(1, &pools.vf_allowed, &pools.vf_through);
                let vi = table.allocate(2, &pools.vi_allowed, &pools.vi_through);
                let (Some(vf), Some(vi)) = (vf, vi) else {
                    crate::trace_if!(
                        lpgen_trace,
                        "lpgen {} period {period} {strategy:?} allocation failed",
                        trace_block()
                    );
                    continue;
                };
                allocation.extend(vf);
                allocation.extend(vi);
                if crate::debug::switches().lpgen_trace {
                    let physical = |register: usize| {
                        let info = &table.problem.registers[register];
                        info.physical
                            .or_else(|| {
                                allocation
                                    .iter()
                                    .find(|&&(r, _)| r == register)
                                    .map(|&(_, p)| p)
                            })
                            .map(|p| format!("{}{p:02}", if info.pool == 1 { "VF" } else { "VI" }))
                    };
                    let regs: Vec<Vec<String>> = table
                        .problem
                        .instructions
                        .iter()
                        .enumerate()
                        .map(|(index, insn)| {
                            insn.operands
                                .iter()
                                .filter(|_| table.slots[index].line >= 0)
                                .filter(|o| table.problem.registers[o.register].pool != 0)
                                .map(|o| physical(o.register).unwrap_or_else(|| "?".into()))
                                .collect()
                        })
                        .collect();
                    crate::trace!(
                        "lpgen {} period {period} {strategy:?} regs {regs:?}",
                        trace_block()
                    );
                    let placed: Vec<(i32, u8)> =
                        table.slots.iter().map(|s| (s.line, s.pipe)).collect();
                    crate::trace!(
                        "lpgen {} period {period} {strategy:?} placed {placed:?}",
                        trace_block()
                    );
                }
            }
            let max_iteration = table
                .done
                .iter()
                .map(|&i| table.fold(table.slots[i].line).1)
                .max()
                .unwrap_or(0);
            if kept
                .as_ref()
                .is_some_and(|k| k.max_iteration < max_iteration)
            {
                continue;
            }
            kept = Some(LoopOutcome {
                strategy,
                period,
                rows: table.rows.clone(),
                placements: table.placements(),
                max_iteration,
                problem: table.problem.clone().into_owned(),
                allocation,
            });
        }
        if kept.is_some() {
            return kept;
        }
    }
    None
}

/// Run the linear optimizer strategies, keeping strictly
/// shorter tables and stopping at the lower bound.
pub fn optimize(
    problem: &Problem,
    lower_bound: i32,
    pools: Option<&Pools>,
    accept: &mut dyn FnMut(&Outcome) -> bool,
) -> Option<Outcome> {
    let statics = statics(problem);
    let mut best: Option<Outcome> = None;
    let lower_bound = if lower_bound < 0 {
        bounds(problem, &statics).0
    } else {
        lower_bound
    };
    for strategy in Strategy::ALL.into_iter().rev() {
        if !strategy.allows_linear() {
            continue;
        }
        if strategy == Strategy::Dumb && best.is_some() {
            break;
        }
        let mut table = Table::new(problem, &statics);
        let budget = 20_000 + problem.instructions.len() * 2_000;
        let trace = match table.wrangle(strategy, budget) {
            Ok(trace) => trace,
            Err(failure) => {
                crate::trace_if!(
                    wrangle_trace,
                    "wrangle strategy {} {:?} failed {failure:?} done {:?} todo {:?}",
                    trace_block(),
                    strategy,
                    table.done,
                    table.todo
                );
                continue;
            }
        };
        table.reduce_extents();
        table.fiddle_branch_delay();
        if crate::debug::switches().wrangle_trace {
            crate::trace!(
                "wrangle strategy {} {:?} length {} trace {:?}",
                trace_block(),
                strategy,
                table.length(),
                trace
            );
            crate::trace!("wrangle reduced {:?}", table.rows());
        }
        let mut allocation = Vec::new();
        if let Some(pools) = pools {
            let vf = table.allocate(1, &pools.vf_allowed, &pools.vf_through);
            let vi = table.allocate(2, &pools.vi_allowed, &pools.vi_through);
            match (vf, vi) {
                (Some(vf), Some(vi)) => {
                    allocation.extend(vf);
                    allocation.extend(vi);
                }
                _ => {
                    crate::trace_if!(
                        wrangle_trace,
                        "wrangle strategy {} {:?} allocation failed",
                        trace_block(),
                        strategy
                    );
                    continue;
                }
            }
        }
        let outcome = Outcome {
            strategy,
            rows: table.rows(),
            placements: table.placements(),
            trace,
            allocation,
        };
        if !accept(&outcome) {
            continue;
        }
        let length = table.length();
        if best
            .as_ref()
            .is_none_or(|previous| (length as usize) < previous.rows.len())
        {
            best = Some(outcome);
        }
        if best
            .as_ref()
            .is_some_and(|b| b.rows.len() as i32 <= lower_bound.max(0))
        {
            break;
        }
    }
    best
}
