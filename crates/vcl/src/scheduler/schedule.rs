//! Scheduling primitives reconstructed from the original VCL ELF.
//!
//! Strategy records, ranking callbacks, pipe pairing and cyclic inclusive
//! interval overlap follow the original. The owned graph search below is a bounded
//! reconstruction, not a port of VCL's full register-lock/move-repair engine.
//! See `docs/SCHEDULING.md` for the exact evidence and compatibility boundary.

use crate::isa::instruction::issue;
use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipe {
    Upper,
    Lower,
}

impl Pipe {
    pub const fn index(self) -> usize {
        match self {
            Self::Upper => 0,
            Self::Lower => 1,
        }
    }

    const fn bit(self) -> u8 {
        1 << self.index()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    Dumb,
    LinearDumb,
    Normal1,
    HeightDown,
    Vuta,
    DepthUp,
    Normal,
    Another,
    Vuta1,
    Normal2,
    Dumb2,
}

impl Strategy {
    /// Original table order is ascending; optimization tries this in reverse.
    pub const ALL: [Self; 11] = [
        Self::Dumb,
        Self::LinearDumb,
        Self::Normal1,
        Self::HeightDown,
        Self::Vuta,
        Self::DepthUp,
        Self::Normal,
        Self::Another,
        Self::Vuta1,
        Self::Normal2,
        Self::Dumb2,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Dumb => "dumb",
            Self::LinearDumb => "ldumb",
            Self::Normal1 => "normal1",
            Self::HeightDown => "hDown",
            Self::Vuta => "vuta",
            Self::DepthUp => "dUp",
            Self::Normal => "normal",
            Self::Another => "another",
            Self::Vuta1 => "vuta1",
            Self::Normal2 => "normal2",
            Self::Dumb2 => "dumb2",
        }
    }

    pub const fn allows_linear(self) -> bool {
        matches!(
            self,
            Self::Dumb
                | Self::LinearDumb
                | Self::Vuta
                | Self::Normal
                | Self::Another
                | Self::Vuta1
                | Self::Normal2
        )
    }

    pub const fn wrangler_a(self) -> bool {
        matches!(self, Self::Vuta1 | Self::Normal2 | Self::Dumb2)
    }
}

/// Values corresponding to instruction fields `0x13f0..0x13fc`.
///
/// The binary dynamically updates weighted height and depth. A caller with the
/// recovered lock graph can provide those values directly; the standalone
/// scheduler computes graph-only estimates if `Operation::metrics` is absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PriorityMetrics {
    pub height: i32,
    pub height_links: i32,
    pub depth: i32,
    pub depth_links: i32,
    pub fan_out: i32,
    pub consumer_weight: i32,
    /// Number of STAT_ALU, STAT_DIV, or MAC input operands, not component bits.
    pub status_inputs: u16,
    pub mac_inputs: u16,
}

/// One member of the original linked candidate list, in traversal order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub operation: usize,
    pub source_order: usize,
    pub metrics: PriorityMetrics,
    pub penalty: u16,
    pub delay: u16,
    pub earliest: u16,
    pub generated_move: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub operation: usize,
    pub cycle: u16,
    pub preferred_pipe: Pipe,
}

#[derive(Clone, Copy)]
enum FinalTie {
    LinksRememberLinks,
    LinksRememberHeight,
    Height,
}

fn proposal_line(candidate: &Candidate, strategy: Strategy, cursor: u16) -> i64 {
    let retry = i64::from(candidate.penalty);
    let extra = match strategy {
        Strategy::Dumb2 => retry >> 1,
        Strategy::Normal2 | Strategy::Vuta1 => retry >> 2,
        _ => i64::from(candidate.delay),
    };
    let mut line = i64::from(candidate.earliest) + extra;
    if !candidate.generated_move {
        match strategy {
            Strategy::Dumb | Strategy::Dumb2 => line = line.max(i64::from(cursor)),
            Strategy::LinearDumb => line = line.max(i64::from(cursor) - 64).max(0),
            _ => {}
        }
    }
    line
}

fn scores(candidate: &Candidate, strategy: Strategy) -> (i64, i64, FinalTie) {
    let m = candidate.metrics;
    let h = i64::from(m.height);
    let d = i64::from(m.depth);
    let nd = i64::from(m.depth_links);
    let f = i64::from(m.fan_out);
    let w = i64::from(m.consumer_weight);
    let p = i64::from(candidate.penalty);
    let bonus = i64::from(m.status_inputs) * 400;
    match strategy {
        Strategy::Normal | Strategy::Normal2 => {
            // Both callbacks call a flag-bonus helper and discard its result.
            (
                h - p,
                -(candidate.source_order as i64),
                FinalTie::LinksRememberLinks,
            )
        }
        Strategy::Normal1 => (d + h - p, w - p, FinalTie::LinksRememberLinks),
        Strategy::HeightDown => (
            w - p + h + bonus,
            f - p + (nd >> 2) + bonus,
            FinalTie::LinksRememberHeight,
        ),
        Strategy::Another => (
            w - p + h + bonus,
            f - p + (nd >> 2) + bonus,
            FinalTie::LinksRememberLinks,
        ),
        Strategy::DepthUp => (w - p, d + f - p, FinalTie::LinksRememberHeight),
        Strategy::Vuta => (w - p + 32 * d + bonus, f - p + nd + bonus, FinalTie::Height),
        Strategy::Vuta1 => {
            let bonus = i64::from(m.mac_inputs) * 400;
            (w - p + 32 * d + bonus, f - p + nd + bonus, FinalTie::Height)
        }
        _ => unreachable!("source-order strategies do not use weighted scoring"),
    }
}

/// Semantic port of the eleven priority callbacks, including stateful tie rules.
///
/// `candidates` must preserve the candidate list's traversal order. VCL pushes
/// candidates to the head, reversing the to-do scan. Primary improvements do
/// not reset its tertiary earliest-line accumulator; that quirk is preserved.
pub fn select_candidate(
    candidates: &[Candidate],
    strategy: Strategy,
    cursor: u16,
) -> Option<Proposal> {
    let selected = if matches!(
        strategy,
        Strategy::Dumb | Strategy::LinearDumb | Strategy::Dumb2
    ) {
        // Iterator::min_by_key chooses the first minimum, matching strict <.
        candidates
            .iter()
            .min_by_key(|candidate| candidate.source_order)
    } else {
        let mut selected = None;
        let mut primary = i64::MIN;
        let mut secondary = i64::MIN;
        let mut earliest = i64::MAX;
        let mut remembered_height = i64::MIN;
        for candidate in candidates {
            let (a, b, tie) = scores(candidate, strategy);
            let line = proposal_line(candidate, strategy, cursor);
            let height = i64::from(candidate.metrics.height);
            let links = i64::from(candidate.metrics.height_links);
            if a > primary {
                selected = Some(candidate);
                primary = a;
                secondary = b;
                remembered_height = height;
            } else if a == primary {
                if b > secondary {
                    selected = Some(candidate);
                    secondary = b;
                    remembered_height = height;
                } else if b == secondary {
                    let final_value = match tie {
                        FinalTie::Height => height,
                        _ => links,
                    };
                    if line < earliest {
                        selected = Some(candidate);
                        earliest = line;
                        remembered_height = height;
                    } else if line == earliest && final_value > remembered_height {
                        selected = Some(candidate);
                        remembered_height = match tie {
                            FinalTie::LinksRememberLinks => links,
                            FinalTie::LinksRememberHeight | FinalTie::Height => height,
                        };
                    }
                }
            }
        }
        selected
    }?;
    let pipe_bit = match strategy {
        Strategy::Normal2 | Strategy::Vuta1 => (selected.penalty >> 3) & 1,
        _ => selected.penalty & 1,
    };
    Some(Proposal {
        operation: selected.operation,
        cycle: proposal_line(selected, strategy, cursor).min(i64::from(u16::MAX)) as u16,
        preferred_pipe: if pipe_bit == 0 {
            Pipe::Upper
        } else {
            Pipe::Lower
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisterAccess {
    pub register: usize,
    pub components: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    pub source_order: usize,
    /// Bit zero permits upper, bit one lower, exactly as the original.
    pub issue_mask: u8,
    /// Independent execution resources: FMAC, IALU, LS, DIV, EFU, etc.
    pub unit_mask: u16,
    pub throughput: u16,
    pub reads: Vec<RegisterAccess>,
    pub writes: Vec<RegisterAccess>,
    pub earliest: u16,
    pub is_branch: bool,
    pub metrics: Option<PriorityMetrics>,
}

impl Operation {
    pub fn new(source_order: usize, issue_mask: u8) -> Self {
        Self {
            source_order,
            issue_mask,
            unit_mask: 0,
            throughput: 1,
            reads: Vec::new(),
            writes: Vec::new(),
            earliest: 0,
            is_branch: false,
            metrics: None,
        }
    }
}

/// `to.cycle + distance * period >= from.cycle + latency`.
///
/// Source and target are indices in `SchedulingProblem::operations`.
/// Dataflow construction, memory aliasing, and implicit register operands belong
/// to the caller. This avoids inventing dependencies from operand spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub from: usize,
    pub to: usize,
    pub latency: u16,
    pub distance: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchedulingProblem {
    pub operations: Vec<Operation>,
    pub dependencies: Vec<Dependency>,
    pub period: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleOptions {
    pub strategy: Strategy,
    /// Inclusive absolute time horizon. Original placement sentinel is 30000.
    pub max_cycle: u16,
    /// Deterministic work budget, replacing VCL's wall-clock watchdog.
    pub max_nodes: usize,
}

impl Default for ScheduleOptions {
    fn default() -> Self {
        Self {
            strategy: Strategy::Normal2,
            max_cycle: 29999,
            max_nodes: 100_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub cycle: u16,
    pub pipe: Pipe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub placements: Vec<Placement>,
    /// A period's folded rows for modulo, absolute cycle rows for linear.
    /// Empty rows are retained; a later hardware-stall pass may compress them.
    pub rows: Vec<[Option<usize>; 2]>,
    pub period: Option<u16>,
    pub search_nodes: usize,
    /// The strategy that produced the schedule.
    pub strategy: Strategy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    InvalidIssueMask { operation: usize, mask: u8 },
    InvalidComponents { operation: usize, mask: u8 },
    InvalidDependency { from: usize, to: usize },
    LoopDistanceWithoutPeriod,
    ZeroPeriod,
    ZeroThroughput(usize),
    InvalidHorizon(u16),
    CyclicOrdinaryDependencies,
    NoSchedule,
    SearchBudgetExceeded { nodes: usize },
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "instruction scheduling failed: {self:?}")
    }
}

impl std::error::Error for ScheduleError {}

/// Inclusive interval overlap as the original, with explicit cyclic context.
/// Inputs must be ordered; invalid intervals are empty. A cyclic range whose
/// length spans the entire period conflicts with every nonempty range.
pub fn intervals_overlap(a: (u32, u32), b: (u32, u32), period: Option<u16>) -> bool {
    if a.0 > a.1 || b.0 > b.1 {
        return false;
    }
    let Some(period) = period.filter(|&period| period != 0) else {
        return a.0 <= b.1 && b.0 <= a.1;
    };
    let p = u64::from(period);
    let alen = u64::from(a.1 - a.0);
    let blen = u64::from(b.1 - b.0);
    if alen + blen >= p {
        return true;
    }
    let ast = u64::from(a.0) % p;
    let bst = u64::from(b.0) % p;
    // Compare one interval against this iteration and the adjacent one. At
    // least one pair of starts lies within one period of the other.
    (ast <= bst + blen && bst <= ast + alen)
        || (ast + p <= bst + blen && bst <= ast + p + alen)
        || (ast <= bst + p + blen && bst + p <= ast + alen)
}

/// VCL rejects simultaneous writes to one register even for disjoint lanes.
/// Zero-register outputs must be removed by the flow-building caller.
pub fn pair_compatible(a: &Operation, a_pipe: Pipe, b: &Operation, b_pipe: Pipe) -> bool {
    a_pipe != b_pipe
        && a.issue_mask & a_pipe.bit() != 0
        && b.issue_mask & b_pipe.bit() != 0
        && !a
            .writes
            .iter()
            .any(|left| b.writes.iter().any(|right| left.register == right.register))
}

fn validate(problem: &SchedulingProblem, options: &ScheduleOptions) -> Result<(), ScheduleError> {
    if problem.period == Some(0) {
        return Err(ScheduleError::ZeroPeriod);
    }
    if options.max_cycle >= 30000 {
        return Err(ScheduleError::InvalidHorizon(options.max_cycle));
    }
    for (index, operation) in problem.operations.iter().enumerate() {
        if operation.issue_mask == 0 || operation.issue_mask & !issue::EITHER != 0 {
            return Err(ScheduleError::InvalidIssueMask {
                operation: index,
                mask: operation.issue_mask,
            });
        }
        if operation.throughput == 0 {
            return Err(ScheduleError::ZeroThroughput(index));
        }
        for access in operation.reads.iter().chain(&operation.writes) {
            if access.components == 0 || access.components & 0x80 != 0 {
                return Err(ScheduleError::InvalidComponents {
                    operation: index,
                    mask: access.components,
                });
            }
        }
    }
    for edge in &problem.dependencies {
        if edge.from >= problem.operations.len() || edge.to >= problem.operations.len() {
            return Err(ScheduleError::InvalidDependency {
                from: edge.from,
                to: edge.to,
            });
        }
        if edge.distance != 0 && problem.period.is_none() {
            return Err(ScheduleError::LoopDistanceWithoutPeriod);
        }
    }
    Ok(())
}

fn topological_order(problem: &SchedulingProblem) -> Result<Vec<usize>, ScheduleError> {
    let mut incoming = vec![BTreeSet::new(); problem.operations.len()];
    let mut outgoing = vec![BTreeSet::new(); problem.operations.len()];
    for edge in problem
        .dependencies
        .iter()
        .filter(|edge| edge.distance == 0)
    {
        incoming[edge.to].insert(edge.from);
        outgoing[edge.from].insert(edge.to);
    }
    let mut available: BTreeSet<_> = incoming
        .iter()
        .enumerate()
        .filter_map(|(index, set)| set.is_empty().then_some(index))
        .collect();
    let mut order = Vec::new();
    while let Some(index) = available.pop_first() {
        order.push(index);
        for &next in &outgoing[index] {
            incoming[next].remove(&index);
            if incoming[next].is_empty() {
                available.insert(next);
            }
        }
    }
    if order.len() != problem.operations.len() {
        return Err(ScheduleError::CyclicOrdinaryDependencies);
    }
    Ok(order)
}

/// Graph-only metric reconstruction. The source binary's lock-aware dynamic
/// metrics include additional physical value-lifetime constraints.
pub fn graph_metrics(problem: &SchedulingProblem) -> Result<Vec<PriorityMetrics>, ScheduleError> {
    validate(problem, &ScheduleOptions::default())?;
    let order = topological_order(problem)?;
    let mut result = vec![PriorityMetrics::default(); problem.operations.len()];
    for &index in &order {
        for edge in problem
            .dependencies
            .iter()
            .filter(|edge| edge.from == index && edge.distance == 0)
        {
            result[edge.to].depth = result[edge.to]
                .depth
                .max(result[index].depth.saturating_add(i32::from(edge.latency)));
            result[edge.to].depth_links = result[edge.to]
                .depth_links
                .max(result[index].depth_links.saturating_add(1));
        }
    }
    for &index in order.iter().rev() {
        let mut consumers = BTreeSet::new();
        for edge in problem
            .dependencies
            .iter()
            .filter(|edge| edge.from == index && edge.distance == 0)
        {
            result[index].height = result[index].height.max(
                result[edge.to]
                    .height
                    .saturating_add(i32::from(edge.latency)),
            );
            result[index].height_links = result[index]
                .height_links
                .max(result[edge.to].height_links.saturating_add(1));
            consumers.insert(edge.to);
        }
        result[index].fan_out = consumers.len().min(i32::MAX as usize) as i32;
        result[index].consumer_weight = consumers.iter().fold(0i32, |sum, &next| {
            sum.saturating_add(result[next].consumer_weight.saturating_add(1))
        });
    }
    for (operation, metrics) in problem.operations.iter().zip(&mut result) {
        if let Some(explicit) = operation.metrics {
            *metrics = explicit;
        } else {
            metrics.status_inputs = operation
                .reads
                .iter()
                .filter(|access| matches!(access.register, 0x37 | 0x38 | 0x3b))
                .count()
                .min(u16::MAX as usize) as u16;
            metrics.mac_inputs = operation
                .reads
                .iter()
                .filter(|access| access.register == 0x3b)
                .count()
                .min(u16::MAX as usize) as u16;
        }
    }
    Ok(result)
}

fn modulo_bounds_fit(problem: &SchedulingProblem) -> bool {
    let Some(period) = problem.period else {
        return true;
    };
    let p = usize::from(period);
    if problem.operations.len() > p * 2
        || problem
            .operations
            .iter()
            .filter(|op| op.issue_mask == issue::UPPER)
            .count()
            > p
        || problem
            .operations
            .iter()
            .filter(|op| op.issue_mask == issue::LOWER)
            .count()
            > p
    {
        return false;
    }
    for unit in 0..16 {
        let occupancy: u64 = problem
            .operations
            .iter()
            .filter(|op| op.unit_mask & (1 << unit) != 0)
            .map(|op| u64::from(op.throughput))
            .sum();
        if occupancy > u64::from(period) {
            return false;
        }
    }
    // Difference constraints detect positive-latency recurrence cycles before
    // placement search. A cycle's total latency must fit its iteration distance
    // times the period. Repeated relaxation is independent of graph direction.
    let mut earliest = vec![0i64; problem.operations.len()];
    for pass in 0..problem.operations.len() {
        let mut changed = false;
        for edge in &problem.dependencies {
            let weight = i64::from(edge.latency) - i64::from(edge.distance) * i64::from(period);
            if earliest[edge.to] < earliest[edge.from] + weight {
                earliest[edge.to] = earliest[edge.from] + weight;
                changed = true;
            }
        }
        if !changed {
            return true;
        }
        if pass + 1 == problem.operations.len() {
            return false;
        }
    }
    true
}

struct Search<'a> {
    problem: &'a SchedulingProblem,
    options: &'a ScheduleOptions,
    metrics: Vec<PriorityMetrics>,
    placements: Vec<Option<Placement>>,
    occupied: Vec<[Option<usize>; 2]>,
    nodes: usize,
    horizon: u16,
}

impl Search<'_> {
    fn row(&self, cycle: u16) -> u16 {
        self.problem.period.map_or(cycle, |period| cycle % period)
    }

    fn earliest(&self, index: usize) -> Option<u16> {
        let mut time = u32::from(self.problem.operations[index].earliest);
        for edge in self
            .problem
            .dependencies
            .iter()
            .filter(|edge| edge.to == index)
        {
            let Some(placed) = self.placements[edge.from] else {
                if edge.distance == 0 {
                    return None;
                }
                continue;
            };
            let offset = u32::from(edge.distance) * u32::from(self.problem.period.unwrap_or(0));
            time = time
                .max((u32::from(placed.cycle) + u32::from(edge.latency)).saturating_sub(offset));
        }
        (time <= u32::from(self.horizon)).then_some(time as u16)
    }

    fn fits(&self, index: usize, placement: Placement) -> bool {
        let operation = &self.problem.operations[index];
        if operation.issue_mask & placement.pipe.bit() == 0 {
            return false;
        }
        let row = self.occupied[usize::from(self.row(placement.cycle))];
        if row[placement.pipe.index()].is_some() {
            return false;
        }
        let opposite = if placement.pipe == Pipe::Upper {
            Pipe::Lower
        } else {
            Pipe::Upper
        };
        if let Some(other) = row[opposite.index()]
            && !pair_compatible(
                operation,
                placement.pipe,
                &self.problem.operations[other],
                opposite,
            )
        {
            return false;
        }
        if operation.is_branch
            && let Some(period) = self.problem.period
            && (period < 2 || self.row(placement.cycle) != period - 2)
        {
            return false;
        }
        let period = i64::from(self.problem.period.unwrap_or(0));
        for edge in &self.problem.dependencies {
            let source = if edge.from == index {
                Some(placement)
            } else {
                self.placements[edge.from]
            };
            let target = if edge.to == index {
                Some(placement)
            } else {
                self.placements[edge.to]
            };
            if let (Some(source), Some(target)) = (source, target)
                && i64::from(target.cycle) + i64::from(edge.distance) * period
                    < i64::from(source.cycle) + i64::from(edge.latency)
            {
                return false;
            }
        }
        if operation.unit_mask != 0
            && self
                .problem
                .period
                .is_some_and(|p| operation.throughput > p)
        {
            return false;
        }
        for (other_index, other_placement) in self.placements.iter().enumerate() {
            let Some(other_placement) = other_placement else {
                continue;
            };
            let other = &self.problem.operations[other_index];
            if operation.unit_mask & other.unit_mask != 0
                && intervals_overlap(
                    (
                        u32::from(placement.cycle),
                        u32::from(placement.cycle) + u32::from(operation.throughput) - 1,
                    ),
                    (
                        u32::from(other_placement.cycle),
                        u32::from(other_placement.cycle) + u32::from(other.throughput) - 1,
                    ),
                    self.problem.period,
                )
            {
                return false;
            }
        }
        true
    }

    fn visit(&mut self, placed_count: usize) -> Result<bool, ScheduleError> {
        if placed_count == self.placements.len() {
            return Ok(true);
        }
        let remaining = self.placements.len() - placed_count;
        let cursor = self
            .placements
            .iter()
            .flatten()
            .map(|placement| placement.cycle)
            .max()
            .unwrap_or(0);
        let mut candidates = Vec::new();
        for (index, operation) in self.problem.operations.iter().enumerate() {
            if self.placements[index].is_some()
                || (operation.is_branch && remaining > 1 && self.problem.period.is_none())
            {
                continue;
            }
            if let Some(mut earliest) = self.earliest(index) {
                if operation.is_branch {
                    earliest = earliest.max(
                        self.problem
                            .period
                            .map_or(cursor.saturating_sub(1), |period| period.saturating_sub(2)),
                    );
                }
                candidates.push(Candidate {
                    operation: index,
                    source_order: operation.source_order,
                    metrics: self.metrics[index],
                    penalty: 0,
                    delay: 0,
                    earliest,
                    generated_move: false,
                });
                // The C loops break after adding the 5th or 17th candidate.
                let limit = if self.options.strategy.wrangler_a() {
                    5
                } else {
                    17
                };
                if self.problem.period.is_none() && candidates.len() >= limit {
                    break;
                }
            }
        }
        candidates.reverse();
        while let Some(proposal) = select_candidate(&candidates, self.options.strategy, cursor) {
            let index = proposal.operation;
            let pipes = match proposal.preferred_pipe {
                Pipe::Upper => [Pipe::Upper, Pipe::Lower],
                Pipe::Lower => [Pipe::Lower, Pipe::Upper],
            };
            for cycle in proposal.cycle..=self.horizon {
                for pipe in pipes {
                    let row = usize::from(self.row(cycle));
                    // Slot conflicts are constant-time; they do not consume
                    // decision nodes or rescan all already placed operations.
                    if self.problem.operations[index].issue_mask & pipe.bit() == 0
                        || self.occupied[row][pipe.index()].is_some()
                    {
                        continue;
                    }
                    if self.nodes >= self.options.max_nodes {
                        return Err(ScheduleError::SearchBudgetExceeded { nodes: self.nodes });
                    }
                    self.nodes += 1;
                    let placement = Placement { cycle, pipe };
                    if !self.fits(index, placement) {
                        continue;
                    }
                    self.placements[index] = Some(placement);
                    self.occupied[row][pipe.index()] = Some(index);
                    if self.visit(placed_count + 1)? {
                        return Ok(true);
                    }
                    self.occupied[row][pipe.index()] = None;
                    self.placements[index] = None;
                }
            }
            // Try the next ranked candidate after rolling back every placement
            // of this candidate. No failed partial assignment escapes the call.
            candidates.retain(|candidate| candidate.operation != index);
        }
        Ok(false)
    }
}

/// Schedule explicit dependencies with two issue pipes and bounded rollback.
/// This does not synthesize VCL's register moves or compress hardware stalls.
pub fn schedule(
    problem: &SchedulingProblem,
    options: &ScheduleOptions,
) -> Result<Schedule, ScheduleError> {
    validate(problem, options)?;
    let metrics = graph_metrics(problem)?;
    if !modulo_bounds_fit(problem) {
        return Err(ScheduleError::NoSchedule);
    }
    let horizon = problem.period.map_or(options.max_cycle, |period| {
        // Original test rejects absolute_iteration > 16, admitting 0..=16.
        options
            .max_cycle
            .min((u32::from(period) * 17 - 1).min(29999) as u16)
    });
    let mut search = Search {
        problem,
        options,
        metrics,
        placements: vec![None; problem.operations.len()],
        occupied: vec![[None; 2]; problem.period.map_or(usize::from(horizon) + 1, usize::from)],
        nodes: 0,
        horizon,
    };
    if !search.visit(0)? {
        return Err(ScheduleError::NoSchedule);
    }
    let placements: Vec<_> = search
        .placements
        .iter()
        .map(|placement| placement.expect("complete schedule"))
        .collect();
    let rows_len = problem.period.map_or_else(
        || {
            placements
                .iter()
                .map(|placement| usize::from(placement.cycle) + 1)
                .max()
                .unwrap_or(0)
        },
        usize::from,
    );
    let mut rows = vec![[None; 2]; rows_len];
    for (index, placement) in placements.iter().enumerate() {
        let row = usize::from(search.row(placement.cycle));
        rows[row][placement.pipe.index()] = Some(index);
    }
    Ok(Schedule {
        placements,
        rows,
        period: problem.period,
        search_nodes: search.nodes,
        strategy: options.strategy,
    })
}

/// Try original strategy table order, retaining a strictly shorter linear result.
/// Register allocation is a separate stage; callers may reject a candidate that
/// cannot allocate, as VCL does.
pub fn schedule_best(
    problem: &SchedulingProblem,
    options: &ScheduleOptions,
) -> Result<Schedule, ScheduleError> {
    let mut best: Option<Schedule> = None;
    let mut error = ScheduleError::NoSchedule;
    for strategy in Strategy::ALL.into_iter().rev() {
        if problem.period.is_none() && !strategy.allows_linear() {
            continue;
        }
        let trial = ScheduleOptions {
            strategy,
            ..*options
        };
        match schedule(problem, &trial) {
            Ok(candidate) => {
                if best
                    .as_ref()
                    .is_none_or(|previous| candidate.rows.len() < previous.rows.len())
                {
                    best = Some(candidate);
                }
            }
            Err(failure) => error = failure,
        }
    }
    best.ok_or(error)
}

/// Increase the modulo initiation interval until the explicit graph fits.
/// VCL additionally validates side effects, allocates registers, and builds
/// prologue/epilogue streams before accepting a modulo result.
pub fn schedule_modulo(
    problem: &SchedulingProblem,
    minimum: u16,
    maximum: u16,
    options: &ScheduleOptions,
) -> Result<Schedule, ScheduleError> {
    if minimum == 0 {
        return Err(ScheduleError::ZeroPeriod);
    }
    let mut trial = problem.clone();
    let mut error = ScheduleError::NoSchedule;
    for period in minimum..=maximum {
        trial.period = Some(period);
        match schedule_best(&trial, options) {
            Ok(result) => return Ok(result),
            Err(failure) => error = failure,
        }
    }
    Err(error)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::instruction::unit;

    fn access(register: usize, components: u8) -> RegisterAccess {
        RegisterAccess {
            register,
            components,
        }
    }

    fn candidate(index: usize, height: i32, depth: i32) -> Candidate {
        Candidate {
            operation: index,
            source_order: index,
            metrics: PriorityMetrics {
                height,
                depth,
                ..Default::default()
            },
            penalty: 0,
            delay: 0,
            earliest: 0,
            generated_move: false,
        }
    }

    #[test]
    fn original_strategy_inventory_and_linear_flags() {
        assert_eq!(
            Strategy::ALL
                .iter()
                .map(|strategy| strategy.name())
                .collect::<Vec<_>>(),
            [
                "dumb", "ldumb", "normal1", "hDown", "vuta", "dUp", "normal", "another", "vuta1",
                "normal2", "dumb2"
            ]
        );
        assert_eq!(
            Strategy::ALL
                .iter()
                .filter(|strategy| strategy.allows_linear())
                .count(),
            7
        );
    }

    #[test]
    fn source_order_strategy_retains_first_exact_tie() {
        let mut a = candidate(8, 0, 0);
        a.source_order = 3;
        let mut b = candidate(9, 0, 0);
        b.source_order = 3;
        assert_eq!(
            select_candidate(&[a, b], Strategy::Dumb, 0)
                .unwrap()
                .operation,
            8
        );
    }

    #[test]
    fn normal_discards_the_status_bonus() {
        let mut a = candidate(0, 1, 0);
        a.metrics.status_inputs = 10;
        let b = candidate(1, 2, 0);
        assert_eq!(
            select_candidate(&[a, b], Strategy::Normal, 0)
                .unwrap()
                .operation,
            1
        );
        assert_eq!(
            select_candidate(&[a, b], Strategy::HeightDown, 0)
                .unwrap()
                .operation,
            0
        );
    }

    #[test]
    fn vuta1_rewards_only_mac() {
        let mut a = candidate(0, 0, 0);
        a.metrics.status_inputs = 2;
        let mut b = candidate(1, 0, 0);
        b.metrics.mac_inputs = 1;
        assert_eq!(
            select_candidate(&[a, b], Strategy::Vuta1, 0)
                .unwrap()
                .operation,
            1
        );
        assert_eq!(
            select_candidate(&[a, b], Strategy::Vuta, 0)
                .unwrap()
                .operation,
            0
        );
    }

    #[test]
    fn retry_penalty_encodes_delay_and_pipe() {
        let mut a = candidate(0, 0, 0);
        a.earliest = 4;
        a.penalty = 11;
        a.delay = 7;
        assert_eq!(
            select_candidate(&[a], Strategy::Normal2, 0),
            Some(Proposal {
                operation: 0,
                cycle: 6,
                preferred_pipe: Pipe::Lower
            })
        );
        assert_eq!(select_candidate(&[a], Strategy::Dumb2, 0).unwrap().cycle, 9);
        assert_eq!(
            select_candidate(&[a], Strategy::Normal, 0).unwrap().cycle,
            11
        );
    }

    #[test]
    fn generated_moves_can_precede_the_cursor() {
        let mut a = candidate(0, 0, 0);
        assert_eq!(
            select_candidate(&[a], Strategy::Dumb, 80).unwrap().cycle,
            80
        );
        assert_eq!(
            select_candidate(&[a], Strategy::LinearDumb, 80)
                .unwrap()
                .cycle,
            16
        );
        a.generated_move = true;
        assert_eq!(select_candidate(&[a], Strategy::Dumb, 80).unwrap().cycle, 0);
    }

    #[test]
    fn primary_improvement_preserves_original_tertiary_state() {
        let mut a = candidate(0, 0, 0);
        a.source_order = 0;
        a.earliest = 10;
        let mut b = candidate(1, 0, 0);
        b.source_order = 0;
        b.earliest = 1;
        let mut c = candidate(2, 2, 0);
        c.source_order = 0;
        c.earliest = 20;
        let mut d = candidate(3, 2, 0);
        d.source_order = 0;
        d.earliest = 15;
        // A normalized lexicographic sort picks d. VCL retained earliest=1
        // from b when c improved the primary, so its actual callback keeps c.
        assert_eq!(
            select_candidate(&[a, b, c, d], Strategy::Normal, 0)
                .unwrap()
                .operation,
            2
        );
    }

    #[test]
    fn cyclic_interval_endpoints_and_wraparound() {
        assert!(intervals_overlap((2, 4), (4, 7), None));
        assert!(!intervals_overlap((2, 4), (5, 7), None));
        assert!(intervals_overlap((7, 9), (0, 0), Some(8)));
        assert!(!intervals_overlap((7, 9), (2, 6), Some(8)));
        assert!(intervals_overlap((7, 10), (2, 6), Some(8)));
        assert!(intervals_overlap((0, 8), (3, 3), Some(8)));
    }

    #[test]
    fn cyclic_overlap_matches_an_exhaustive_residue_oracle() {
        for period in 1..10u16 {
            for start_a in 0..20u32 {
                for start_b in 0..20u32 {
                    for len_a in 0..10u32 {
                        for len_b in 0..10u32 {
                            let a = (start_a, start_a + len_a);
                            let b = (start_b, start_b + len_b);
                            let expected = (a.0..=a.1).any(|x| {
                                (b.0..=b.1).any(|y| x % u32::from(period) == y % u32::from(period))
                            });
                            assert_eq!(
                                intervals_overlap(a, b, Some(period)),
                                expected,
                                "period={period} a={a:?} b={b:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn disjoint_same_register_writes_cannot_pair() {
        let mut a = Operation::new(0, 1);
        a.writes.push(access(4, 1));
        let mut b = Operation::new(1, 2);
        b.writes.push(access(4, 2));
        assert!(!pair_compatible(&a, Pipe::Upper, &b, Pipe::Lower));
        b.writes[0].register = 5;
        assert!(pair_compatible(&a, Pipe::Upper, &b, Pipe::Lower));
    }

    #[test]
    fn independent_upper_and_lower_pack_together() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1), Operation::new(1, 2)],
            ..Default::default()
        };
        let result = schedule(&problem, &Default::default()).unwrap();
        assert_eq!(result.rows, vec![[Some(0), Some(1)]]);
    }

    #[test]
    fn failed_partial_placement_rolls_back_before_another_pipe() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 3), Operation::new(1, 1)],
            ..Default::default()
        };
        let options = ScheduleOptions {
            max_cycle: 0,
            ..Default::default()
        };
        // The flexible first operation initially occupies upper. The second
        // cannot fit, forcing a rollback and retry of the first in lower.
        let result = schedule(&problem, &options).unwrap();
        assert_eq!(result.rows, vec![[Some(1), Some(0)]]);
        assert!(result.search_nodes > 2);
    }

    #[test]
    fn data_latency_preserves_empty_cycle_rows() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1), Operation::new(1, 2)],
            dependencies: vec![Dependency {
                from: 0,
                to: 1,
                latency: 4,
                distance: 0,
            }],
            period: None,
        };
        let result = schedule(&problem, &Default::default()).unwrap();
        assert_eq!(result.placements[1].cycle, 4);
        assert_eq!(result.rows.len(), 5);
    }

    #[test]
    fn division_throughput_blocks_its_resource() {
        let mut a = Operation::new(0, 2);
        a.unit_mask = unit::DIVIDE;
        a.throughput = 7;
        let mut b = Operation::new(1, 2);
        b.unit_mask = unit::DIVIDE;
        b.throughput = 7;
        let problem = SchedulingProblem {
            operations: vec![a, b],
            ..Default::default()
        };
        let result = schedule(&problem, &Default::default()).unwrap();
        assert_eq!(result.placements[1].cycle, 7);
    }

    #[test]
    fn modulo_pipeline_folds_stages() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1), Operation::new(1, 2)],
            dependencies: vec![
                Dependency {
                    from: 0,
                    to: 1,
                    latency: 3,
                    distance: 0,
                },
                Dependency {
                    from: 1,
                    to: 0,
                    latency: 1,
                    distance: 2,
                },
            ],
            period: Some(2),
        };
        let result = schedule(&problem, &Default::default()).unwrap();
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.placements[1].cycle, 3);
        assert_eq!(result.rows[1][1], Some(1));
    }

    #[test]
    fn self_recurrence_rejects_a_period_below_its_latency() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1)],
            dependencies: vec![Dependency {
                from: 0,
                to: 0,
                latency: 5,
                distance: 1,
            }],
            period: Some(2),
        };
        let opts = ScheduleOptions {
            max_cycle: 10,
            ..Default::default()
        };
        assert_eq!(schedule(&problem, &opts), Err(ScheduleError::NoSchedule));
        assert_eq!(
            schedule_modulo(&problem, 2, 6, &opts).unwrap().period,
            Some(5)
        );
    }

    #[test]
    fn modulo_resource_pressure_is_proved_before_search() {
        let mut operation = Operation::new(0, 2);
        operation.unit_mask = unit::EFU;
        operation.throughput = 8;
        let problem = SchedulingProblem {
            operations: vec![operation],
            period: Some(4),
            ..Default::default()
        };
        let options = ScheduleOptions {
            max_nodes: 0,
            ..Default::default()
        };
        assert_eq!(schedule(&problem, &options), Err(ScheduleError::NoSchedule));
    }

    #[test]
    fn modulo_recurrence_cycle_sums_all_edge_latencies() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1), Operation::new(1, 2)],
            dependencies: vec![
                Dependency {
                    from: 0,
                    to: 1,
                    latency: 4,
                    distance: 0,
                },
                Dependency {
                    from: 1,
                    to: 0,
                    latency: 3,
                    distance: 1,
                },
            ],
            period: Some(6),
        };
        let options = ScheduleOptions {
            max_nodes: 0,
            ..Default::default()
        };
        assert_eq!(schedule(&problem, &options), Err(ScheduleError::NoSchedule));
        let fit = schedule_modulo(&problem, 6, 8, &ScheduleOptions::default()).unwrap();
        assert_eq!(fit.period, Some(7));
    }

    #[test]
    fn branch_waits_for_other_linear_operations() {
        let mut branch = Operation::new(0, 2);
        branch.is_branch = true;
        let problem = SchedulingProblem {
            operations: vec![branch, Operation::new(1, 1)],
            ..Default::default()
        };
        let result = schedule(&problem, &Default::default()).unwrap();
        assert_eq!(result.rows[0], [Some(1), Some(0)]);
    }

    #[test]
    fn branch_uses_the_penultimate_modulo_row() {
        let mut branch = Operation::new(0, 2);
        branch.is_branch = true;
        let problem = SchedulingProblem {
            operations: vec![branch],
            period: Some(5),
            ..Default::default()
        };
        assert_eq!(
            schedule(&problem, &Default::default()).unwrap().placements[0].cycle,
            3
        );
    }

    #[test]
    fn graph_metrics_count_distinct_consumers_but_sum_paths() {
        let problem = SchedulingProblem {
            operations: (0..4).map(|i| Operation::new(i, 3)).collect(),
            dependencies: vec![
                Dependency {
                    from: 0,
                    to: 1,
                    latency: 4,
                    distance: 0,
                },
                Dependency {
                    from: 0,
                    to: 1,
                    latency: 3,
                    distance: 0,
                },
                Dependency {
                    from: 0,
                    to: 2,
                    latency: 1,
                    distance: 0,
                },
                Dependency {
                    from: 1,
                    to: 3,
                    latency: 2,
                    distance: 0,
                },
                Dependency {
                    from: 2,
                    to: 3,
                    latency: 1,
                    distance: 0,
                },
            ],
            period: None,
        };
        let m = graph_metrics(&problem).unwrap();
        assert_eq!(m[0].height, 6);
        assert_eq!(m[3].depth, 6);
        assert_eq!(m[0].fan_out, 2);
        assert_eq!(m[0].consumer_weight, 4);
        assert_eq!(m[0].height_links, 2);
    }

    #[test]
    fn budget_exhaustion_is_not_reported_as_infeasibility() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1)],
            ..Default::default()
        };
        let options = ScheduleOptions {
            max_nodes: 0,
            ..Default::default()
        };
        assert_eq!(
            schedule(&problem, &options),
            Err(ScheduleError::SearchBudgetExceeded { nodes: 0 })
        );
    }

    #[test]
    fn validation_reports_bad_input_without_panicking() {
        let mut problem = SchedulingProblem {
            operations: vec![Operation::new(0, 0)],
            ..Default::default()
        };
        assert!(matches!(
            schedule(&problem, &Default::default()),
            Err(ScheduleError::InvalidIssueMask { .. })
        ));
        problem.operations[0].issue_mask = issue::UPPER;
        problem.dependencies.push(Dependency {
            from: 0,
            to: 1,
            latency: 1,
            distance: 0,
        });
        assert!(matches!(
            schedule(&problem, &Default::default()),
            Err(ScheduleError::InvalidDependency { .. })
        ));
    }

    #[test]
    fn ordinary_dependency_cycle_is_a_distinct_error() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1), Operation::new(1, 2)],
            dependencies: vec![
                Dependency {
                    from: 0,
                    to: 1,
                    latency: 1,
                    distance: 0,
                },
                Dependency {
                    from: 1,
                    to: 0,
                    latency: 1,
                    distance: 0,
                },
            ],
            period: None,
        };
        assert_eq!(
            schedule(&problem, &Default::default()),
            Err(ScheduleError::CyclicOrdinaryDependencies)
        );
    }

    #[test]
    fn empty_problem_has_no_linear_rows() {
        assert!(
            schedule(&SchedulingProblem::default(), &Default::default())
                .unwrap()
                .rows
                .is_empty()
        );
    }
}
