//! Bounded construction of a single-stream modulo scheduling problem.
//!
//! Eligibility follows the original, branch placement is the
//! scheduler's `period - 2` rule, and side-effect age checks follow the original. This helper does not replace register-lock move repair,
//! allocation, prologue/epilogue construction, or original strategy selection.

use crate::analysis::flow::{self, Definition};
use crate::isa::instruction::issue;
use crate::loops::pipeline;
use crate::scheduler::schedule::{self, Dependency, SchedulingProblem};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Effect {
    #[default]
    Pure,
    /// A load participates in conservative memory order but has original
    /// side-effect category zero.
    MemoryRead,
    /// Original category one; speculation is bounded by the known-safe count.
    Store,
    /// Original category two; cannot precede the conditional's iteration.
    Critical,
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub id: usize,
    pub scheduling: schedule::Operation,
    pub write_latencies: BTreeMap<usize, u16>,
    /// Additional timing per input register, recovered separately from
    /// conditional-input marking. An absent entry means zero;
    /// a marked IALU branch input adds one to the producer latency.
    pub read_delays: BTreeMap<usize, u16>,
    /// Address reads proven affine by `loop_ir::annotate_sources`. Their
    /// producer/overwrite edges are replaced by clone-epoch immediate repair;
    /// the original reads and reaching definitions are still validated.
    pub relaxed_reads: BTreeSet<usize>,
    pub effect: Effect,
    pub is_nop: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Control {
    pub block: usize,
    pub conditional: usize,
    /// Actual selected outgoing edge, not a textual backward label guess.
    pub backedge: usize,
    pub is_conditional: bool,
    pub enabled: bool,
    pub known_minimum: u32,
    pub known_safe: u32,
}

#[derive(Clone, Debug)]
pub struct Problem {
    pub scheduling: SchedulingProblem,
    /// Local scheduler index -> original flow operation identity.
    pub ids: Vec<usize>,
    pub lower_bound: u16,
    pub conditional: usize,
    pub known_minimum: u32,
    pub known_safe: u32,
    effects: Vec<Effect>,
    nops: Vec<bool>,
}

#[derive(Clone, Copy, Debug)]
pub struct SearchOptions {
    pub maximum_period: u16,
    pub extra: u16,
    pub absolute: Option<u16>,
    pub scheduling: schedule::ScheduleOptions,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            maximum_period: 256,
            extra: 0,
            absolute: None,
            scheduling: Default::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Scheduled {
    pub schedule: schedule::Schedule,
    pub operations: Vec<pipeline::LoopOperation>,
    pub conditional: usize,
    pub known_minimum: u32,
    pub known_safe: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopError {
    Disabled,
    InvalidControlFlow,
    InvalidKnownMinimum,
    InvalidConditional,
    OperationSetMismatch,
    DuplicateOperation(usize),
    EffectMismatch(usize),
    MissingLatency {
        operation: usize,
        register: usize,
    },
    MissingRead {
        operation: usize,
        register: usize,
        component: u8,
    },
    UndefinedRead {
        operation: usize,
        register: usize,
        component: u8,
    },
    AmbiguousDefinitions {
        operation: usize,
        register: usize,
        component: u8,
    },
    InvalidHazard,
    PeriodOverflow,
    InvalidPeriodRange,
    UnsafeEffect {
        operation: usize,
        stage: u16,
        required_stage: u16,
    },
    Scheduling(schedule::ScheduleError),
}

impl fmt::Display for LoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "normal loop scheduling unavailable: {self:?}")
    }
}

impl std::error::Error for LoopError {}

fn components(mask: u8) -> impl Iterator<Item = u8> {
    (0..8).filter(move |component| mask & (1 << component) != 0)
}

fn masks(accesses: impl Iterator<Item = (usize, u8)>) -> BTreeMap<usize, u8> {
    let mut result = BTreeMap::new();
    for (register, mask) in accesses {
        if mask != 0 {
            *result.entry(register).or_default() |= mask;
        }
    }
    result
}

/// Build a loop problem from matching, validated, expanded flow operands.
///
/// Only one reachable block with a conditional self-edge and one external exit
/// is supported. `body` must cover precisely that block. Other reachable blocks
/// may supply entry definitions, which belong to later pipeline expansion.
/// Additional proven hazard edges use local body indices in source order and
/// iteration distances zero or one. Memory operations are conservatively ordered
/// both within the body and across the backedge; no alias independence is guessed.
pub fn build(
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    body: &[Operation],
    control: Control,
    hazards: &[Dependency],
) -> Result<Problem, LoopError> {
    if !control.enabled {
        return Err(LoopError::Disabled);
    }
    if control.known_minimum == 0 {
        return Err(LoopError::InvalidKnownMinimum);
    }
    let block = graph
        .blocks
        .get(control.block)
        .ok_or(LoopError::InvalidControlFlow)?;
    let successors: BTreeSet<_> = block.successors.iter().copied().collect();
    if !control.is_conditional
        || control.backedge != control.block
        || !analysis.reachable.contains(&control.block)
        || successors.len() != 2
        || !successors.contains(&control.block)
        || successors.iter().any(|&block| block >= graph.blocks.len())
    {
        return Err(LoopError::InvalidControlFlow);
    }
    let mut supplied = BTreeMap::new();
    for operation in body {
        if supplied.insert(operation.id, operation).is_some() {
            return Err(LoopError::DuplicateOperation(operation.id));
        }
    }
    if supplied.len() != block.operations.len()
        || block.operations.iter().any(|id| !supplied.contains_key(id))
    {
        return Err(LoopError::OperationSetMismatch);
    }
    let ordered: Vec<_> = block.operations.iter().map(|id| supplied[id]).collect();
    let local: BTreeMap<_, _> = block
        .operations
        .iter()
        .enumerate()
        .map(|(index, &id)| (id, index))
        .collect();
    let conditional = *local
        .get(&control.conditional)
        .ok_or(LoopError::InvalidConditional)?;
    if conditional + 1 != ordered.len()
        || !ordered[conditional].scheduling.is_branch
        || ordered[conditional].scheduling.issue_mask != issue::LOWER
        || ordered.iter().filter(|op| op.scheduling.is_branch).count() != 1
    {
        return Err(LoopError::InvalidConditional);
    }
    for operation in &ordered {
        let flow = graph
            .operations
            .get(operation.id)
            .ok_or(LoopError::OperationSetMismatch)?;
        let reads = masks(flow.reads.iter().map(|a| (a.register, a.mask)));
        let writes = masks(flow.writes.iter().map(|a| (a.register, a.mask)));
        let schedule_reads = masks(
            operation
                .scheduling
                .reads
                .iter()
                .map(|a| (a.register, a.components)),
        );
        let schedule_writes = masks(
            operation
                .scheduling
                .writes
                .iter()
                .map(|a| (a.register, a.components)),
        );
        if reads != schedule_reads || writes != schedule_writes {
            return Err(LoopError::EffectMismatch(operation.id));
        }
        for &register in writes
            .keys()
            .filter(|register| !graph.constants.contains(register))
        {
            if !operation.write_latencies.contains_key(&register) {
                return Err(LoopError::MissingLatency {
                    operation: operation.id,
                    register,
                });
            }
        }
    }
    let mut edges = BTreeMap::<(usize, usize, u16), u16>::new();
    let mut add = |from, to, distance, latency| {
        edges
            .entry((from, to, distance))
            .and_modify(|value| *value = (*value).max(latency))
            .or_insert(latency);
    };
    let mut writers = BTreeMap::<(usize, u8), Vec<usize>>::new();
    for (index, operation) in ordered.iter().enumerate() {
        for write in &operation.scheduling.writes {
            if graph.constants.contains(&write.register) {
                continue;
            }
            for component in components(write.components) {
                let list = writers.entry((write.register, component)).or_default();
                if list.last() != Some(&index) {
                    list.push(index);
                }
            }
        }
    }
    let read_defs: BTreeMap<_, _> = analysis
        .reads
        .iter()
        .map(|read| {
            (
                (read.operation, read.register, read.component),
                &read.definitions,
            )
        })
        .collect();
    for (to, operation) in ordered.iter().enumerate() {
        for read in &operation.scheduling.reads {
            if graph.constants.contains(&read.register) {
                continue;
            }
            for component in components(read.components) {
                let definitions = read_defs
                    .get(&(operation.id, read.register, component))
                    .ok_or(LoopError::MissingRead {
                        operation: operation.id,
                        register: read.register,
                        component,
                    })?;
                if definitions.is_empty() || definitions.contains(&Definition::Uninitialized) {
                    return Err(LoopError::UndefinedRead {
                        operation: operation.id,
                        register: read.register,
                        component,
                    });
                }
                let local_definitions: BTreeSet<_> = definitions
                    .iter()
                    .filter_map(|definition| match definition {
                        Definition::Operation(id) => local.get(id).copied(),
                        _ => None,
                    })
                    .collect();
                let writes = writers.get(&(read.register, component));
                let before =
                    writes.and_then(|writes| writes.iter().copied().rfind(|&from| from < to));
                let expected = before.or_else(|| writes.and_then(|writes| writes.last().copied()));
                if local_definitions != expected.into_iter().collect() {
                    return Err(LoopError::AmbiguousDefinitions {
                        operation: operation.id,
                        register: read.register,
                        component,
                    });
                }
                if operation.relaxed_reads.contains(&read.register) {
                    continue;
                }
                if let Some(from) = expected {
                    let distance = u16::from(before.is_none());
                    let latency = ordered[from].write_latencies[&read.register]
                        .checked_add(
                            operation
                                .read_delays
                                .get(&read.register)
                                .copied()
                                .unwrap_or(0),
                        )
                        .ok_or(LoopError::PeriodOverflow)?;
                    add(from, to, distance, latency);
                }
                // Preserve the read before the next overwriting issue. A
                // read/write operation reads before its own write, so an
                // immediate self update needs no ordinary self edge.
                if let Some(writes) = writes {
                    let after = writes.iter().copied().find(|&writer| writer >= to);
                    let next = after.or_else(|| writes.first().copied()).unwrap();
                    if next != to {
                        add(to, next, u16::from(after.is_none()), 0);
                    }
                }
            }
        }
    }
    // Preserve output completion order, including last-to-first recurrence.
    // This is conservative for values later separated by move repair, and
    // must not be mistaken for a complete original lock transformation.
    for ((register, _), writes) in &writers {
        for (index, &from) in writes.iter().enumerate() {
            let to = writes[(index + 1) % writes.len()];
            let distance = u16::from(index + 1 == writes.len());
            let latency = ordered[from].write_latencies[register]
                .saturating_add(1)
                .saturating_sub(ordered[to].write_latencies[register]);
            add(from, to, distance, latency);
        }
    }
    let memory: Vec<_> = ordered
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| {
            matches!(operation.effect, Effect::MemoryRead | Effect::Store).then_some(index)
        })
        .collect();
    for (index, &from) in memory.iter().enumerate() {
        let to = memory[(index + 1) % memory.len()];
        add(from, to, u16::from(index + 1 == memory.len()), 1);
    }
    for hazard in hazards {
        if hazard.from >= ordered.len()
            || hazard.to >= ordered.len()
            || hazard.distance > 1
            || (hazard.from == hazard.to && hazard.distance == 0)
        {
            return Err(LoopError::InvalidHazard);
        }
        add(hazard.from, hazard.to, hazard.distance, hazard.latency);
    }
    let scheduling = SchedulingProblem {
        operations: ordered
            .iter()
            .enumerate()
            .map(|(index, op)| {
                let mut operation = op.scheduling.clone();
                operation.source_order = index;
                operation
            })
            .collect(),
        dependencies: edges
            .into_iter()
            .map(|((from, to, distance), latency)| Dependency {
                from,
                to,
                distance,
                latency,
            })
            .collect(),
        period: None,
    };
    let lower_bound = resource_bound(&scheduling, &ordered)?;
    Ok(Problem {
        scheduling,
        ids: block.operations.clone(),
        lower_bound,
        conditional,
        known_minimum: control.known_minimum,
        known_safe: control.known_safe,
        effects: ordered.iter().map(|op| op.effect).collect(),
        nops: ordered.iter().map(|op| op.is_nop).collect(),
    })
}

fn resource_bound(
    problem: &SchedulingProblem,
    operations: &[&Operation],
) -> Result<u16, LoopError> {
    let upper = problem
        .operations
        .iter()
        .filter(|op| op.issue_mask == issue::UPPER)
        .count() as u64;
    let lower = problem
        .operations
        .iter()
        .filter(|op| op.issue_mask == issue::LOWER)
        .count() as u64;
    let mut bound = 2u64
        .max(upper)
        .max(lower)
        .max((problem.operations.len() as u64).div_ceil(2));
    for unit in 0..16 {
        bound = bound.max(
            problem
                .operations
                .iter()
                .filter(|op| op.unit_mask & (1 << unit) != 0)
                .map(|op| u64::from(op.throughput))
                .sum(),
        );
    }
    for operation in operations {
        bound = bound.max(
            operation
                .write_latencies
                .values()
                .copied()
                .map(u64::from)
                .max()
                .unwrap_or(0),
        );
    }
    u16::try_from(bound).map_err(|_| LoopError::PeriodOverflow)
}

/// Find a bounded legal dependency/resource candidate. This does not certify
/// physical allocation or cloning. An unsafe side-effect candidate is rejected
/// explicitly; the caller can retain its linear schedule instead of guessing a
/// safe speculative transformation. At each period, all strategies compete by
/// maximum iteration ID; equal depths replace the previous candidate, following
/// the original selection rule. Physical allocation remains a later check.
pub fn schedule(problem: &Problem, options: &SearchOptions) -> Result<Scheduled, LoopError> {
    let minimum = problem
        .lower_bound
        .checked_add(options.extra)
        .ok_or(LoopError::PeriodOverflow)?;
    let minimum = match options.absolute {
        Some(value) if value < 2 => return Err(LoopError::InvalidPeriodRange),
        Some(value) => minimum.max(value),
        None => minimum,
    };
    if options.maximum_period < minimum {
        return Err(LoopError::InvalidPeriodRange);
    }
    let mut selected = None;
    let mut failure = LoopError::Scheduling(schedule::ScheduleError::NoSchedule);
    for period in minimum..=options.maximum_period {
        let mut trial = problem.scheduling.clone();
        trial.period = Some(period);
        let mut best: Option<(u16, schedule::Schedule)> = None;
        for strategy in schedule::Strategy::ALL.into_iter().rev() {
            let config = schedule::ScheduleOptions {
                strategy,
                ..options.scheduling
            };
            match schedule::schedule(&trial, &config) {
                Ok(candidate) => {
                    if let Err(error) = validate_ages(problem, &candidate) {
                        failure = error;
                        continue;
                    }
                    let stage = candidate
                        .placements
                        .iter()
                        .map(|placement| placement.cycle / period)
                        .max()
                        .unwrap_or(0);
                    // Original loop selection minimizes maxLid within the
                    // first feasible period; equal stages replace the prior
                    // strategy rather than comparing identical row counts.
                    if best.as_ref().is_none_or(|(previous, _)| stage <= *previous) {
                        best = Some((stage, candidate));
                    }
                }
                Err(error) => failure = LoopError::Scheduling(error),
            }
        }
        if let Some((_, best)) = best {
            selected = Some(best);
            break;
        }
    }
    let schedule = selected.ok_or(failure)?;
    let period = schedule.period.ok_or(LoopError::InvalidPeriodRange)?;
    let operations = schedule
        .placements
        .iter()
        .enumerate()
        .map(|(index, placement)| pipeline::LoopOperation {
            id: problem.ids[index],
            row: placement.cycle % period,
            stage: (placement.cycle / period) as u8,
            pipe: placement.pipe,
            is_nop: problem.nops[index],
        })
        .collect();
    Ok(Scheduled {
        schedule,
        operations,
        conditional: problem.ids[problem.conditional],
        known_minimum: problem.known_minimum,
        known_safe: problem.known_safe,
    })
}

fn validate_ages(problem: &Problem, schedule: &schedule::Schedule) -> Result<(), LoopError> {
    let period = schedule.period.ok_or(LoopError::InvalidPeriodRange)?;
    let conditional_stage = schedule.placements[problem.conditional].cycle / period;
    for (index, effect) in problem.effects.iter().enumerate() {
        let required = match effect {
            Effect::Store => u32::from(conditional_stage).saturating_sub(problem.known_safe) as u16,
            Effect::Critical => conditional_stage,
            Effect::Pure | Effect::MemoryRead => continue,
        };
        let stage = schedule.placements[index].cycle / period;
        if stage < required {
            return Err(LoopError::UnsafeEffect {
                operation: problem.ids[index],
                stage,
                required_stage: required,
            });
        }
    }
    Ok(())
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::flow::{Access, Block, Graph};
    use crate::scheduler::schedule::{Pipe, Placement, RegisterAccess};

    fn op(id: usize, issue: u8, reads: &[(usize, u8)], writes: &[(usize, u8, u16)]) -> Operation {
        let mut scheduling = schedule::Operation::new(id, issue);
        scheduling.reads = reads
            .iter()
            .map(|&(register, components)| RegisterAccess {
                register,
                components,
            })
            .collect();
        scheduling.writes = writes
            .iter()
            .map(|&(register, components, _)| RegisterAccess {
                register,
                components,
            })
            .collect();
        Operation {
            id,
            scheduling,
            write_latencies: writes
                .iter()
                .map(|&(register, _, latency)| (register, latency))
                .collect(),
            read_delays: BTreeMap::new(),
            relaxed_reads: BTreeSet::new(),
            effect: Effect::Pure,
            is_nop: false,
        }
    }

    fn integer() -> Vec<Operation> {
        let update = op(0, 2, &[(1, 1)], &[(1, 1, 1)]);
        let mut branch = op(1, 2, &[(1, 1), (0, 1)], &[]);
        branch.scheduling.is_branch = true;
        branch.read_delays.insert(1, 1);
        vec![update, branch]
    }

    fn graph(body: &[Operation]) -> Graph {
        Graph {
            operations: body
                .iter()
                .map(|op| flow::Operation {
                    reads: op
                        .scheduling
                        .reads
                        .iter()
                        .map(|a| Access {
                            register: a.register,
                            mask: a.components,
                        })
                        .collect(),
                    writes: op
                        .scheduling
                        .writes
                        .iter()
                        .map(|a| Access {
                            register: a.register,
                            mask: a.components,
                        })
                        .collect(),
                    side_effect: matches!(op.effect, Effect::Store | Effect::Critical),
                })
                .collect(),
            blocks: vec![
                Block {
                    successors: vec![1],
                    ..Block::default()
                },
                Block {
                    operations: (0..body.len()).collect(),
                    successors: vec![1, 2],
                    ..Block::default()
                },
                Block::default(),
            ],
            entry: 0,
            inputs: vec![
                Access {
                    register: 1,
                    mask: 1,
                },
                Access {
                    register: 2,
                    mask: 15,
                },
                Access {
                    register: 3,
                    mask: 15,
                },
            ],
            constants: BTreeSet::from([0]),
        }
    }

    fn control(conditional: usize) -> Control {
        Control {
            block: 1,
            conditional,
            backedge: 1,
            is_conditional: true,
            enabled: true,
            known_minimum: 1,
            known_safe: 0,
        }
    }

    fn problem(body: &[Operation]) -> Problem {
        let graph = graph(body);
        let analysis = graph.analyze().unwrap();
        build(&graph, &analysis, body, control(body.len() - 1), &[]).unwrap()
    }

    fn edge(problem: &Problem, from: usize, to: usize, distance: u16) -> u16 {
        problem
            .scheduling
            .dependencies
            .iter()
            .find(|e| e.from == from && e.to == to && e.distance == distance)
            .unwrap()
            .latency
    }

    #[test]
    fn proven_affine_read_relaxes_only_address_edges_and_still_validates_flow() {
        let update = op(0, 2, &[(1, 1)], &[(1, 1, 1)]);
        let mut load = op(1, 2, &[(1, 1)], &[(2, 15, 4)]);
        load.effect = Effect::MemoryRead;
        let mut branch = op(2, 2, &[(0, 1)], &[]);
        branch.scheduling.is_branch = true;
        let mut body = vec![update, load, branch];
        let ordinary = problem(&body);
        assert_eq!(edge(&ordinary, 0, 1, 0), 1);
        assert_eq!(edge(&ordinary, 1, 0, 1), 0);
        body[1].relaxed_reads.insert(1);
        let relaxed = problem(&body);
        assert!(
            !relaxed
                .scheduling
                .dependencies
                .iter()
                .any(|edge| (edge.from, edge.to) == (0, 1) || (edge.from, edge.to) == (1, 0))
        );
        assert_eq!(edge(&relaxed, 0, 0, 1), 1); // setter recurrence remains
        assert_eq!(edge(&relaxed, 1, 1, 1), 1); // memory ordering remains
        let graph = graph(&body);
        let mut analysis = graph.analyze().unwrap();
        analysis
            .reads
            .iter_mut()
            .find(|read| read.operation == 1)
            .unwrap()
            .definitions = BTreeSet::from([Definition::Uninitialized]);
        assert!(matches!(
            build(&graph, &analysis, &body, control(2), &[]),
            Err(LoopError::UndefinedRead { operation: 1, .. })
        ));
    }

    #[test]
    fn integer_recurrence_and_branch_delay_are_explicit() {
        let problem = problem(&integer());
        assert_eq!(edge(&problem, 0, 0, 1), 1);
        assert_eq!(edge(&problem, 0, 1, 0), 2);
        assert_eq!(edge(&problem, 1, 0, 1), 0);
        let result = schedule(
            &problem,
            &SearchOptions {
                maximum_period: 6,
                ..Default::default()
            },
        )
        .unwrap();
        let period = result.schedule.period.unwrap();
        assert!(period >= 3);
        assert_eq!(result.operations[1].row, period - 2);
        assert_eq!(result.operations[1].pipe, Pipe::Lower);
        for dependency in &problem.scheduling.dependencies {
            let a = result.schedule.placements[dependency.from].cycle;
            let b = result.schedule.placements[dependency.to].cycle;
            assert!(
                u32::from(b) + u32::from(dependency.distance) * u32::from(period)
                    >= u32::from(a) + u32::from(dependency.latency)
            );
        }
    }

    #[test]
    fn original_counted_vector_fixture_has_four_row_recurrence_bound() {
        // loop-vector-counted: ADD b,b,a; ISUBIU c,c,1; IBNE c,VI00,loop.
        let mut vector = op(0, 1, &[(2, 15), (3, 15)], &[(2, 15, 4)]);
        vector.scheduling.unit_mask = crate::isa::instruction::unit::FMAC;
        let mut update = integer().remove(0);
        update.id = 1;
        let mut branch = integer().remove(1);
        branch.id = 2;
        let problem = problem(&[vector, update, branch]);
        assert_eq!(problem.lower_bound, 4);
        assert_eq!(edge(&problem, 0, 0, 1), 4);
        let result = schedule(
            &problem,
            &SearchOptions {
                maximum_period: 4,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.schedule.period, Some(4));
        assert_eq!(result.operations[2].row, 2);
        assert_eq!(result.operations.iter().map(|op| op.stage).max(), Some(0));
    }

    #[test]
    fn a_read_before_a_later_body_writer_is_previous_iteration_data() {
        let body = vec![
            op(0, 1, &[(2, 1)], &[(4, 1, 4)]),
            op(1, 1, &[(3, 1)], &[(2, 1, 4)]),
            {
                let mut branch = op(2, 2, &[(1, 1)], &[]);
                branch.scheduling.is_branch = true;
                branch
            },
        ];
        let problem = problem(&body);
        assert_eq!(edge(&problem, 1, 0, 1), 4);
        assert_eq!(edge(&problem, 0, 1, 0), 0);
    }

    #[test]
    fn disjoint_components_do_not_create_false_recurrence_edges() {
        let body = vec![op(0, 1, &[(2, 1)], &[(2, 2, 4)]), {
            let mut branch = op(1, 2, &[(1, 1)], &[]);
            branch.scheduling.is_branch = true;
            branch
        }];
        let problem = problem(&body);
        assert!(
            !problem
                .scheduling
                .dependencies
                .iter()
                .any(|edge| edge.from == 0 && edge.to == 0 && edge.latency == 4)
        );
        // WAW protection remains, but is issue/completion order latency one.
        assert_eq!(edge(&problem, 0, 0, 1), 1);
    }

    #[test]
    fn memory_order_includes_the_last_to_first_backedge() {
        let mut load = op(0, 2, &[(1, 1)], &[(4, 15, 4)]);
        load.effect = Effect::MemoryRead;
        let mut store = op(1, 2, &[(4, 15)], &[]);
        store.effect = Effect::Store;
        let mut branch = op(2, 2, &[(1, 1)], &[]);
        branch.scheduling.is_branch = true;
        let problem = problem(&[load, store, branch]);
        assert_eq!(edge(&problem, 0, 1, 0), 4);
        assert_eq!(edge(&problem, 1, 0, 1), 1);
    }

    #[test]
    fn hints_raise_period_without_overriding_a_lower_bound() {
        let problem = problem(&integer());
        let result = schedule(
            &problem,
            &SearchOptions {
                extra: 2,
                absolute: Some(3),
                maximum_period: 6,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.schedule.period.unwrap() >= problem.lower_bound + 2);
        assert_eq!(
            schedule(
                &problem,
                &SearchOptions {
                    maximum_period: 1,
                    ..Default::default()
                }
            )
            .unwrap_err(),
            LoopError::InvalidPeriodRange
        );
    }

    #[test]
    fn unsupported_cfg_enable_and_control_forms_are_explicit() {
        let body = integer();
        let mut graph = graph(&body);
        let analysis = graph.analyze().unwrap();
        let mut config = control(1);
        config.enabled = false;
        assert_eq!(
            build(&graph, &analysis, &body, config, &[]).unwrap_err(),
            LoopError::Disabled
        );
        config.enabled = true;
        config.backedge = 0;
        assert_eq!(
            build(&graph, &analysis, &body, config, &[]).unwrap_err(),
            LoopError::InvalidControlFlow
        );
        config.backedge = 1;
        config.is_conditional = false;
        assert_eq!(
            build(&graph, &analysis, &body, config, &[]).unwrap_err(),
            LoopError::InvalidControlFlow
        );
        config.is_conditional = true;
        graph.blocks[1].successors = vec![2];
        assert_eq!(
            build(&graph, &analysis, &body, config, &[]).unwrap_err(),
            LoopError::InvalidControlFlow
        );
    }

    #[test]
    fn stale_or_undefined_reaching_definitions_are_not_guessed() {
        let body = integer();
        let graph = graph(&body);
        let mut analysis = graph.analyze().unwrap();
        let read = analysis
            .reads
            .iter_mut()
            .find(|read| read.operation == 0)
            .unwrap();
        read.definitions = BTreeSet::from([Definition::Input]);
        assert!(matches!(
            build(&graph, &analysis, &body, control(1), &[]),
            Err(LoopError::AmbiguousDefinitions { .. })
        ));
        analysis.reads[0].definitions = BTreeSet::from([Definition::Uninitialized]);
        assert!(matches!(
            build(&graph, &analysis, &body, control(1), &[]),
            Err(LoopError::UndefinedRead { .. })
        ));
    }

    #[test]
    fn critical_effects_and_stores_obey_conditional_age() {
        let mut body = integer();
        body[0].effect = Effect::Store;
        let mut problem = problem(&body);
        let schedule = schedule::Schedule {
            placements: vec![
                Placement {
                    cycle: 0,
                    pipe: Pipe::Lower,
                },
                Placement {
                    cycle: 6,
                    pipe: Pipe::Lower,
                },
            ],
            rows: vec![[None; 2]; 4],
            period: Some(4),
            search_nodes: 0,
            strategy: schedule::Strategy::Normal2,
        };
        assert_eq!(
            validate_ages(&problem, &schedule),
            Err(LoopError::UnsafeEffect {
                operation: 0,
                stage: 0,
                required_stage: 1
            })
        );
        problem.known_safe = 1;
        assert!(validate_ages(&problem, &schedule).is_ok());
        problem.effects[0] = Effect::Critical;
        assert_eq!(
            validate_ages(&problem, &schedule),
            Err(LoopError::UnsafeEffect {
                operation: 0,
                stage: 0,
                required_stage: 1
            })
        );
    }

    #[test]
    fn execution_resource_occupancy_raises_period_bound() {
        let mut body = integer();
        body[0].scheduling.unit_mask = crate::isa::instruction::unit::EFU;
        body[0].scheduling.throughput = 8;
        let problem = problem(&body);
        assert_eq!(problem.lower_bound, 8);
    }

    #[test]
    fn input_delay_marks_are_per_register_and_additive() {
        let mut load = op(0, 2, &[(1, 1)], &[(4, 1, 4)]);
        load.effect = Effect::MemoryRead;
        let mut branch = op(1, 2, &[(4, 1), (1, 1)], &[]);
        branch.scheduling.is_branch = true;
        // A delay on another input does not change this load's RAW edge.
        branch.read_delays.insert(1, 1);
        let plain = problem(&[load.clone(), branch.clone()]);
        assert_eq!(edge(&plain, 0, 1, 0), 4);
        branch.read_delays.insert(4, 1);
        let marked = problem(&[load, branch]);
        assert_eq!(edge(&marked, 0, 1, 0), 5);
    }

    #[test]
    fn missing_writer_latency_and_invalid_hazard_are_explicit() {
        let mut body = integer();
        let graph = graph(&body);
        let analysis = graph.analyze().unwrap();
        body[0].write_latencies.clear();
        assert_eq!(
            build(&graph, &analysis, &body, control(1), &[]).unwrap_err(),
            LoopError::MissingLatency {
                operation: 0,
                register: 1
            }
        );
        let body = integer();
        let hazard = Dependency {
            from: 0,
            to: 1,
            latency: 0,
            distance: 2,
        };
        assert_eq!(
            build(&graph, &analysis, &body, control(1), &[hazard]).unwrap_err(),
            LoopError::InvalidHazard
        );
    }
}
