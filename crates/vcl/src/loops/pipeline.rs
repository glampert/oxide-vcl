//! Software-loop expansion plan inferred from vcl 1.4beta7,
//! using its clone-window primitive.
//!
//! This module expands an already legal, allocated modulo schedule. It keeps
//! source operation identities and iteration biases so the caller can clone
//! typed instructions, repair immediates and register webs, and rerun flow.
//! It deliberately does not render text or pretend those later passes are
//! unnecessary. Entry/exit rows remain empty here, as in the original builder;
//! ordinary exit padding belongs to the later Tidy pass.

use crate::scheduler::schedule::Pipe;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

// The optimizer itself searches stages through 16. The expansion primitive
// also accepts user raw-loop IDs 1..=32, which become stages 0..=31.
const MAX_STAGE: u8 = 31;
const MAX_PROLOGUES: usize = 16;
const POSITION_SENTINEL: u32 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopOperation {
    pub id: usize,
    pub row: u16,
    pub stage: u8,
    pub pipe: Pipe,
    pub is_nop: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopSchedule {
    pub period: u16,
    pub operations: Vec<LoopOperation>,
    /// Source operation ID of the loop's terminating conditional branch.
    pub conditional: usize,
    pub known_minimum: u32,
    /// Already enforced by the scheduler's side-effect eligibility checks.
    pub known_safe: u32,
    pub unique: String,
    /// Original stream name; source spelling is retained in generated names.
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpansionLimits {
    /// Owned-Rust resource bound, not an inferred original global-array limit.
    pub max_rows: usize,
}

impl Default for ExpansionLimits {
    fn default() -> Self {
        Self {
            max_rows: 1_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Entry,
    Prologue { index: usize },
    Kernel,
    Epilogue { index: usize },
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstructionClone {
    pub source_id: usize,
    pub original_stage: u8,
    /// Original clone iteration bias: window maximum minus source stage.
    pub iteration_bias: i16,
    /// Prologue tests branch to a drain when the original loop would stop.
    pub reverse_branch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageWindow {
    pub min_iteration: u16,
    pub max_iteration: u16,
    pub include_conditional: bool,
    pub keep_nops: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Fallthrough,
    Taken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    /// Index in [`Expansion::parts`], not a source operation ID.
    pub target: usize,
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    /// `None` means an inserted unconditional epilogue-to-exit branch.
    pub source_id: Option<usize>,
    pub reverse: bool,
    pub target: usize,
    pub row: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub kind: PartKind,
    pub rows: Vec<[Option<InstructionClone>; 2]>,
    /// Exact source-stage windows appended to this linear table.
    pub windows: Vec<StageWindow>,
    pub outgoing: Vec<Edge>,
    pub branch: Option<Branch>,
}

impl Part {
    fn new(name: String, kind: PartKind) -> Self {
        Self {
            name,
            kind,
            rows: Vec::new(),
            windows: Vec::new(),
            outgoing: Vec::new(),
            branch: None,
        }
    }

    fn source_position(&self, id: usize) -> Option<(usize, usize)> {
        self.rows.iter().enumerate().find_map(|(row, pair)| {
            pair.iter()
                .position(|entry| entry.is_some_and(|copy| copy.source_id == id))
                .map(|pipe| (row, pipe))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    /// Physical layout: entry, prologues, kernel, epilogues, exit.
    pub parts: Vec<Part>,
    pub entry: usize,
    pub kernel: usize,
    pub exit: usize,
    pub max_stage: u8,
    pub conditional_stage: u8,
    pub known_safe: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineError {
    InvalidPeriod(u16),
    InvalidKnownMinimum(u32),
    InvalidKnownSafe(u32),
    EmptyName,
    DuplicateOperation(usize),
    DuplicateSlot {
        row: u16,
        pipe: Pipe,
    },
    InvalidPosition {
        operation: usize,
        row: u16,
        stage: u8,
    },
    MissingConditional(usize),
    ConditionalIsNop,
    ConditionalInWrongSlot,
    MissingEpilogue {
        stage: u16,
    },
    OccupiedSyntheticBranchSlot,
    TooManyPrologues(usize),
    ResourceLimit {
        requested_rows: usize,
        max_rows: usize,
    },
}

impl fmt::Display for PipelineError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "invalid software-loop expansion: {self:?}")
    }
}

impl std::error::Error for PipelineError {}

/// Construct the original stage-window and stream-link plan. Register moves,
/// immediate repair and final reduction remain caller responsibilities.
pub fn expand(schedule: &LoopSchedule) -> Result<Expansion, PipelineError> {
    expand_with_limits(schedule, ExpansionLimits::default())
}

pub fn expand_with_limits(
    schedule: &LoopSchedule,
    limits: ExpansionLimits,
) -> Result<Expansion, PipelineError> {
    validate(schedule)?;
    let conditional = schedule
        .operations
        .iter()
        .find(|op| op.id == schedule.conditional)
        .ok_or(PipelineError::MissingConditional(schedule.conditional))?;
    let max_stage = schedule
        .operations
        .iter()
        .map(|op| op.stage)
        .max()
        .unwrap_or(0);
    let conditional_stage = conditional.stage;
    let max = u16::from(max_stage);
    let condition = u16::from(conditional_stage);
    let base = format!("EXPL_{}_{}_", schedule.unique, schedule.name);
    let mut budget = RowBudget {
        used: 0,
        limit: limits.max_rows,
    };

    // Records are the meaningful fields set in loopDebris prologue entries:
    // record[3]=last stage; record[1]=number of completed conditional stages.
    let mut prologues: Vec<(Part, u16, u16)> = Vec::new();
    let mut active: Option<Part> = None;
    let mut boundary =
        (u64::from(schedule.known_minimum) - 1 + u64::from(condition)).min(u64::from(max)) as u16;
    for stage in 0..max {
        let index = prologues.len();
        let prologue = active.get_or_insert_with(|| {
            Part::new(format!("{base}_PRO{index}"), PartKind::Prologue { index })
        });
        append_window(
            prologue,
            schedule,
            StageWindow {
                min_iteration: 0,
                max_iteration: stage,
                include_conditional: stage >= boundary,
                keep_nops: false,
            },
            &mut budget,
        )?;
        if stage == boundary || stage + 1 == max {
            let completed_conditions = stage + 1 - condition;
            let mut prologue = active.take().expect("active prologue was appended");
            if let Some((row, pipe)) = prologue.source_position(schedule.conditional) {
                prologue.rows[row][pipe].as_mut().unwrap().reverse_branch = true;
            }
            prologues.push((prologue, stage, completed_conditions));
            if prologues.len() > MAX_PROLOGUES {
                return Err(PipelineError::TooManyPrologues(prologues.len()));
            }
            boundary = stage + 1;
        }
    }

    let mut kernel = Part::new(format!("{base}_MAIN_LOOP"), PartKind::Kernel);
    append_window(
        &mut kernel,
        schedule,
        StageWindow {
            min_iteration: 0,
            max_iteration: max,
            include_conditional: true,
            keep_nops: true,
        },
        &mut budget,
    )?;

    // Drain tables are allocated in reverse prologue order. This is why the
    // kernel's fallthrough selects EPI0 and early exits can select later EPIs.
    let mut epilogues: Vec<(Part, u16)> = Vec::new();
    if max > condition {
        for (_, last_stage, completed_conditions) in prologues.iter().rev() {
            let index = epilogues.len();
            let start_stage = last_stage + 1;
            let mut epilogue =
                Part::new(format!("{base}_EPI{index}"), PartKind::Epilogue { index });
            for offset in 0..max - condition {
                let high = start_stage + offset;
                let low = start_stage - completed_conditions + offset + 1;
                append_window(
                    &mut epilogue,
                    schedule,
                    StageWindow {
                        min_iteration: low,
                        max_iteration: high,
                        include_conditional: false,
                        keep_nops: false,
                    },
                    &mut budget,
                )?;
            }
            epilogues.push((epilogue, start_stage));
        }
    }

    let prologue_count = prologues.len();
    let epilogue_count = epilogues.len();
    let kernel_index = prologue_count + 1;
    let epilogue_start = kernel_index + 1;
    let exit_index = epilogue_start + epilogue_count;
    let epilogue_targets: BTreeMap<_, _> = epilogues
        .iter()
        .enumerate()
        .map(|(index, (_, stage))| (*stage, epilogue_start + index))
        .collect();
    let mut parts = Vec::with_capacity(exit_index + 1);
    let mut entry = Part::new(format!("{base}_ENTRY_POINT"), PartKind::Entry);
    entry.outgoing.push(Edge {
        target: if prologue_count == 0 { kernel_index } else { 1 },
        kind: EdgeKind::Fallthrough,
    });
    parts.push(entry);

    for (index, (mut prologue, last_stage, _)) in prologues.into_iter().enumerate() {
        let next = if index + 1 == prologue_count {
            kernel_index
        } else {
            index + 2
        };
        prologue.outgoing.push(Edge {
            target: next,
            kind: EdgeKind::Fallthrough,
        });
        if let Some((row, _)) = prologue.source_position(schedule.conditional) {
            let stage = last_stage + 1;
            let target = *epilogue_targets
                .get(&stage)
                .ok_or(PipelineError::MissingEpilogue { stage })?;
            prologue.branch = Some(Branch {
                source_id: Some(schedule.conditional),
                reverse: true,
                target,
                row,
            });
            prologue.outgoing.push(Edge {
                target,
                kind: EdgeKind::Taken,
            });
        }
        parts.push(prologue);
    }

    let (branch_row, _) = kernel
        .source_position(schedule.conditional)
        .ok_or(PipelineError::MissingConditional(schedule.conditional))?;
    kernel.branch = Some(Branch {
        source_id: Some(schedule.conditional),
        reverse: false,
        target: kernel_index,
        row: branch_row,
    });
    kernel.outgoing.push(Edge {
        target: kernel_index,
        kind: EdgeKind::Taken,
    });
    kernel.outgoing.push(Edge {
        target: if epilogue_count == 0 {
            exit_index
        } else {
            epilogue_start
        },
        kind: EdgeKind::Fallthrough,
    });
    parts.push(kernel);

    for (index, (mut epilogue, _)) in epilogues.into_iter().enumerate() {
        let kind = if index + 1 == epilogue_count {
            EdgeKind::Fallthrough
        } else {
            let row = epilogue.rows.len() - 2;
            if epilogue.rows[row][Pipe::Lower.index()].is_some() {
                return Err(PipelineError::OccupiedSyntheticBranchSlot);
            }
            epilogue.branch = Some(Branch {
                source_id: None,
                reverse: false,
                target: exit_index,
                row,
            });
            EdgeKind::Taken
        };
        epilogue.outgoing.push(Edge {
            target: exit_index,
            kind,
        });
        parts.push(epilogue);
    }
    parts.push(Part::new(format!("{base}_EXIT_POINT"), PartKind::Exit));
    Ok(Expansion {
        parts,
        entry: 0,
        kernel: kernel_index,
        exit: exit_index,
        max_stage,
        conditional_stage,
        known_safe: schedule.known_safe,
    })
}

fn validate(schedule: &LoopSchedule) -> Result<(), PipelineError> {
    if schedule.period < 2 || u32::from(schedule.period) >= POSITION_SENTINEL {
        return Err(PipelineError::InvalidPeriod(schedule.period));
    }
    if schedule.known_minimum == 0 || schedule.known_minimum > i32::MAX as u32 {
        return Err(PipelineError::InvalidKnownMinimum(schedule.known_minimum));
    }
    if schedule.known_safe > i32::MAX as u32 {
        return Err(PipelineError::InvalidKnownSafe(schedule.known_safe));
    }
    if schedule.unique.is_empty()
        || schedule.name.is_empty()
        || schedule.unique.contains('\0')
        || schedule.name.contains('\0')
    {
        return Err(PipelineError::EmptyName);
    }
    let mut ids = BTreeSet::new();
    let mut slots = BTreeSet::new();
    for op in &schedule.operations {
        if !ids.insert(op.id) {
            return Err(PipelineError::DuplicateOperation(op.id));
        }
        if op.row >= schedule.period
            || op.stage > MAX_STAGE
            || u32::from(op.row) + u32::from(op.stage) * u32::from(schedule.period)
                >= POSITION_SENTINEL
        {
            return Err(PipelineError::InvalidPosition {
                operation: op.id,
                row: op.row,
                stage: op.stage,
            });
        }
        if !slots.insert((op.row, op.pipe.index())) {
            return Err(PipelineError::DuplicateSlot {
                row: op.row,
                pipe: op.pipe,
            });
        }
    }
    let conditional = schedule
        .operations
        .iter()
        .find(|op| op.id == schedule.conditional)
        .ok_or(PipelineError::MissingConditional(schedule.conditional))?;
    if conditional.is_nop {
        return Err(PipelineError::ConditionalIsNop);
    }
    if conditional.pipe != Pipe::Lower || conditional.row + 2 != schedule.period {
        return Err(PipelineError::ConditionalInWrongSlot);
    }
    Ok(())
}

struct RowBudget {
    used: usize,
    limit: usize,
}

fn append_window(
    part: &mut Part,
    schedule: &LoopSchedule,
    window: StageWindow,
    budget: &mut RowBudget,
) -> Result<(), PipelineError> {
    let rows = usize::from(schedule.period);
    let requested = budget.used.saturating_add(rows);
    if requested > budget.limit {
        return Err(PipelineError::ResourceLimit {
            requested_rows: requested,
            max_rows: budget.limit,
        });
    }
    budget.used = requested;
    let start = part.rows.len();
    part.rows.resize(start + rows, [None, None]);
    for op in &schedule.operations {
        let stage = u16::from(op.stage);
        if stage < window.min_iteration
            || stage > window.max_iteration
            || (op.id == schedule.conditional && !window.include_conditional)
            || (op.is_nop && !window.keep_nops)
        {
            continue;
        }
        part.rows[start + usize::from(op.row)][op.pipe.index()] = Some(InstructionClone {
            source_id: op.id,
            original_stage: op.stage,
            iteration_bias: (window.max_iteration - stage) as i16,
            reverse_branch: false,
        });
    }
    part.windows.push(window);
    Ok(())
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(id: usize, row: u16, stage: u8, pipe: Pipe) -> LoopOperation {
        LoopOperation {
            id,
            row,
            stage,
            pipe,
            is_nop: false,
        }
    }

    fn schedule(max_stage: u8, condition_stage: u8) -> LoopSchedule {
        LoopSchedule {
            period: 4,
            operations: vec![
                operation(10, 0, max_stage, Pipe::Upper),
                operation(20, 2, condition_stage, Pipe::Lower),
            ],
            conditional: 20,
            known_minimum: 1,
            known_safe: 0,
            unique: "ORACLE".into(),
            name: "loop".into(),
        }
    }

    fn ids(part: &Part) -> Vec<usize> {
        part.rows
            .iter()
            .flat_map(|row| row.iter())
            .flatten()
            .map(|copy| copy.source_id)
            .collect()
    }

    #[test]
    fn stage_zero_has_only_entry_kernel_and_exit() {
        let result = expand(&schedule(0, 0)).unwrap();
        assert_eq!(result.parts.len(), 3);
        assert_eq!((result.entry, result.kernel, result.exit), (0, 1, 2));
        assert!(result.parts[0].rows.is_empty());
        assert!(result.parts[2].rows.is_empty());
        assert_eq!(result.parts[1].rows.len(), 4);
        assert_eq!(result.parts[1].name, "EXPL_ORACLE_loop__MAIN_LOOP");
        assert_eq!(result.parts[1].branch.unwrap().target, 1);
        assert_eq!(result.parts[1].branch.unwrap().row, 2);
        assert!(!result.parts[1].branch.unwrap().reverse);
    }

    #[test]
    fn two_stage_early_exit_plan_routes_each_prologue_to_its_drain() {
        let result = expand(&schedule(2, 0)).unwrap();
        assert_eq!(result.parts.len(), 7);
        assert_eq!((result.kernel, result.exit), (3, 6));
        assert_eq!(result.parts[1].rows.len(), 4);
        assert_eq!(result.parts[2].rows.len(), 4);
        assert_eq!(result.parts[1].branch.unwrap().target, 5);
        assert_eq!(result.parts[2].branch.unwrap().target, 4);
        assert!(result.parts[1].branch.unwrap().reverse);
        assert_eq!(result.parts[3].outgoing[1].target, 4);
        assert_eq!(result.parts[4].rows.len(), 8);
        assert_eq!(
            result.parts[4].branch.unwrap(),
            Branch {
                source_id: None,
                reverse: false,
                target: 6,
                row: 6
            }
        );
        assert!(result.parts[5].branch.is_none());
    }

    #[test]
    fn known_minimum_groups_warmup_and_removes_unneeded_tests() {
        let mut source = schedule(3, 0);
        source.known_minimum = 4;
        let result = expand(&source).unwrap();
        assert_eq!(result.parts.len(), 5);
        let prologue = &result.parts[1];
        assert_eq!(prologue.windows.len(), 3);
        assert_eq!(prologue.rows.len(), 12);
        assert!(prologue.branch.is_none());
        assert_eq!(
            prologue
                .windows
                .iter()
                .map(|w| w.max_iteration)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(result.parts[3].rows.len(), 12);
    }

    #[test]
    fn conditional_stage_shortens_drain() {
        let result = expand(&schedule(3, 2)).unwrap();
        assert_eq!(result.parts.len(), 5);
        assert_eq!(result.parts[1].rows.len(), 12);
        assert_eq!(result.parts[3].rows.len(), 4);
        assert_eq!(result.parts[3].windows[0].min_iteration, 3);
        assert_eq!(result.parts[3].windows[0].max_iteration, 3);
    }

    #[test]
    fn branch_at_last_stage_needs_no_epilogues() {
        let result = expand(&schedule(2, 2)).unwrap();
        assert_eq!(result.parts.len(), 4);
        assert_eq!(result.parts[1].rows.len(), 8);
        assert!(result.parts[1].branch.is_none());
        assert_eq!(result.parts[result.kernel].outgoing[1].target, result.exit);
    }

    #[test]
    fn cloned_bias_is_relative_to_each_appended_window() {
        let mut source = schedule(2, 0);
        source.operations.push(operation(30, 1, 1, Pipe::Upper));
        let result = expand(&source).unwrap();
        let kernel = &result.parts[result.kernel];
        assert_eq!(kernel.rows[0][0].unwrap().iteration_bias, 0);
        assert_eq!(kernel.rows[1][0].unwrap().iteration_bias, 1);
        assert_eq!(kernel.rows[2][1].unwrap().iteration_bias, 2);
        let epi = &result.parts[result.kernel + 1];
        assert_eq!(epi.rows[0][0].unwrap().iteration_bias, 0);
        assert_eq!(epi.rows[1][0].unwrap().iteration_bias, 1);
        assert_eq!(epi.rows[4][0].unwrap().iteration_bias, 1);
    }

    #[test]
    fn explicit_nops_are_retained_only_in_kernel_window() {
        let mut source = schedule(1, 0);
        source.operations.push(LoopOperation {
            is_nop: true,
            ..operation(30, 1, 0, Pipe::Upper)
        });
        let result = expand(&source).unwrap();
        assert!(!ids(&result.parts[1]).contains(&30));
        assert!(ids(&result.parts[result.kernel]).contains(&30));
        assert!(!ids(&result.parts[result.kernel + 1]).contains(&30));
    }

    #[test]
    fn greatest_original_iteration_is_bounded_but_sixteen_is_allowed() {
        let result = expand(&schedule(16, 0)).unwrap();
        assert_eq!(
            result
                .parts
                .iter()
                .filter(|p| matches!(p.kind, PartKind::Prologue { .. }))
                .count(),
            16
        );
        assert_eq!(
            result
                .parts
                .iter()
                .filter(|p| matches!(p.kind, PartKind::Epilogue { .. }))
                .count(),
            16
        );
        assert!(matches!(
            expand(&schedule(17, 0)),
            Err(PipelineError::TooManyPrologues(17))
        ));
        let mut raw = schedule(17, 0);
        raw.known_minimum = 18;
        assert!(expand(&raw).is_ok());
        assert!(matches!(
            expand(&schedule(32, 0)),
            Err(PipelineError::InvalidPosition { .. })
        ));
    }

    #[test]
    fn malformed_schedules_are_explicit_errors() {
        let mut source = schedule(1, 0);
        source.period = 1;
        assert!(matches!(
            expand(&source),
            Err(PipelineError::InvalidPeriod(1))
        ));
        source = schedule(1, 0);
        source.known_minimum = 0;
        assert!(matches!(
            expand(&source),
            Err(PipelineError::InvalidKnownMinimum(0))
        ));
        source = schedule(1, 0);
        source.conditional = 99;
        assert!(matches!(
            expand(&source),
            Err(PipelineError::MissingConditional(99))
        ));
        source = schedule(1, 0);
        source.operations[1].row = 3;
        assert!(matches!(
            expand(&source),
            Err(PipelineError::ConditionalInWrongSlot)
        ));
        source = schedule(1, 0);
        source.operations.push(operation(30, 0, 0, Pipe::Upper));
        assert!(matches!(
            expand(&source),
            Err(PipelineError::DuplicateSlot { .. })
        ));
    }

    #[test]
    fn resource_limit_fails_before_allocating_an_unbounded_plan() {
        let error =
            expand_with_limits(&schedule(3, 0), ExpansionLimits { max_rows: 5 }).unwrap_err();
        assert!(matches!(
            error,
            PipelineError::ResourceLimit { max_rows: 5, .. }
        ));
    }

    #[test]
    fn original_raw_stage_probes_match_part_sizes_and_early_exit_targets() {
        // Independently authored input, executed by the original binary:
        // local/evidence/oracle/pipeline-raw-stages-min{1,4}/. Later passes may prune
        // individual instructions and rename registers; expansion row counts
        // and early-exit routing remain directly observable in the output.
        let mut source = LoopSchedule {
            period: 5,
            operations: vec![
                operation(0, 0, 0, Pipe::Upper),
                operation(1, 1, 1, Pipe::Upper),
                operation(2, 1, 0, Pipe::Lower),
                operation(3, 2, 2, Pipe::Upper),
                operation(4, 3, 0, Pipe::Lower),
            ],
            conditional: 4,
            known_minimum: 1,
            known_safe: 0,
            unique: "PIPE".into(),
            name: "loop".into(),
        };
        let result = expand(&source).unwrap();
        assert_eq!(
            result
                .parts
                .iter()
                .map(|p| p.rows.len())
                .collect::<Vec<_>>(),
            vec![0, 5, 5, 5, 10, 10, 0]
        );
        assert_eq!(result.parts[1].branch.unwrap().target, 5);
        assert_eq!(result.parts[2].branch.unwrap().target, 4);
        assert_eq!(
            result.parts.iter().map(|p| p.rows.len()).sum::<usize>() + 2,
            37
        );
        source.known_minimum = 4;
        let result = expand(&source).unwrap();
        assert_eq!(
            result
                .parts
                .iter()
                .map(|p| p.rows.len())
                .collect::<Vec<_>>(),
            vec![0, 10, 5, 10, 0]
        );
        assert!(result.parts[1].branch.is_none());
        assert_eq!(
            result.parts.iter().map(|p| p.rows.len()).sum::<usize>() + 2,
            27
        );
    }
}
