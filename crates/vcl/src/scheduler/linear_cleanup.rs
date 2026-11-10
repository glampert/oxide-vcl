//! Conservative extent reduction for movable generated linear tables.
//!
//! The original gates linear reduction on table submode,
//! pulls the end inward and sinks eligible operations within
//! their user/output bounds. This adapter keeps
//! leading readiness rows and branch positions, and moves only caller-proven
//! operations. It does not replace the original lock/move engine.

use crate::scheduler::schedule::{self, Pipe, Placement, SchedulingProblem};
use std::collections::BTreeMap;
use std::fmt;

/// Additional issue separation required before the next stream can read an
/// escaping result. One permits the final issue row, two reserves one more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sink {
    pub operation: usize,
    pub exit_separation: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupError {
    ModularTable,
    InvalidOperation(usize),
    DuplicateOperation(usize),
    MissingOperation(usize),
    InvalidDependency,
    InvalidSink(usize),
    InvalidPipe(usize),
    TooManyRows,
    MissingBranchDelay,
}

impl fmt::Display for CleanupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "linear extent reduction failed: {self:?}")
    }
}

impl std::error::Error for CleanupError {}

/// Preserve the selected pipe and all proven placement edges. The caller must
/// supply actual RAW/WAR/WAW/memory edges rather than source-list branch order;
/// an unrelated instruction may legally issue in the branch's delay pair.
/// `minimum_rows` preserves externally required noninterlocked output debt.
/// Empty leading rows are deliberately retained as incoming readiness debt.
pub fn reduce(
    problem: &SchedulingProblem,
    rows: &[[Option<usize>; 2]],
    sinks: &[Sink],
    minimum_rows: usize,
) -> Result<Vec<[Option<usize>; 2]>, CleanupError> {
    if problem.period.is_some() {
        return Err(CleanupError::ModularTable);
    }
    if rows.len() >= 30000 {
        return Err(CleanupError::TooManyRows);
    }
    let mut positions = BTreeMap::new();
    let mut end = minimum_rows.min(rows.len());
    for (row, pair) in rows.iter().enumerate() {
        for (pipe, &operation) in pair.iter().enumerate() {
            let Some(id) = operation else { continue };
            let op = problem
                .operations
                .get(id)
                .ok_or(CleanupError::InvalidOperation(id))?;
            if op.issue_mask & (1 << pipe) == 0 {
                return Err(CleanupError::InvalidPipe(id));
            }
            if positions
                .insert(
                    id,
                    Placement {
                        cycle: row as u16,
                        pipe: if pipe == 0 { Pipe::Upper } else { Pipe::Lower },
                    },
                )
                .is_some()
            {
                return Err(CleanupError::DuplicateOperation(id));
            }
            end = end.max(row + if op.is_branch { 2 } else { 1 });
        }
    }
    for id in 0..problem.operations.len() {
        if !positions.contains_key(&id) {
            return Err(CleanupError::MissingOperation(id));
        }
    }
    for edge in &problem.dependencies {
        if edge.from >= problem.operations.len()
            || edge.to >= problem.operations.len()
            || edge.distance != 0
        {
            return Err(CleanupError::InvalidDependency);
        }
    }
    let mut movable = BTreeMap::new();
    for sink in sinks {
        if sink.operation >= problem.operations.len()
            || sink.exit_separation == 0
            || problem.operations[sink.operation].is_branch
            || movable
                .insert(sink.operation, sink.exit_separation)
                .is_some()
        {
            return Err(CleanupError::InvalidSink(sink.operation));
        }
    }
    // A source branch always keeps its explicit next row, including when that
    // row is empty. Reduction can shrink other trailing holes, never grow.
    if end > rows.len() {
        return Err(CleanupError::MissingBranchDelay);
    }
    let mut result = rows[..end].to_vec();
    let mut order: Vec<_> = movable.keys().copied().collect();
    order.sort_by_key(|id| std::cmp::Reverse((positions[id].cycle, positions[id].pipe.index())));
    for id in order {
        let old = positions[&id];
        let Some(latest) = end.checked_sub(usize::from(movable[&id])) else {
            continue;
        };
        for row in (usize::from(old.cycle) + 1..=latest).rev() {
            let placement = Placement {
                cycle: row as u16,
                pipe: old.pipe,
            };
            if fits(problem, &result, &positions, id, placement) {
                result[usize::from(old.cycle)][old.pipe.index()] = None;
                result[row][old.pipe.index()] = Some(id);
                positions.insert(id, placement);
                break;
            }
        }
    }
    Ok(result)
}

fn fits(
    problem: &SchedulingProblem,
    rows: &[[Option<usize>; 2]],
    positions: &BTreeMap<usize, Placement>,
    id: usize,
    placement: Placement,
) -> bool {
    let op = &problem.operations[id];
    if placement.cycle < op.earliest {
        return false;
    }
    let row = rows[usize::from(placement.cycle)];
    if row[placement.pipe.index()].is_some() {
        return false;
    }
    let other_pipe = if placement.pipe == Pipe::Upper {
        Pipe::Lower
    } else {
        Pipe::Upper
    };
    if row[other_pipe.index()].is_some_and(|other| {
        !schedule::pair_compatible(op, placement.pipe, &problem.operations[other], other_pipe)
    }) {
        return false;
    }
    for edge in &problem.dependencies {
        let from = if edge.from == id {
            placement
        } else {
            positions[&edge.from]
        };
        let to = if edge.to == id {
            placement
        } else {
            positions[&edge.to]
        };
        if u32::from(to.cycle) < u32::from(from.cycle) + u32::from(edge.latency) {
            return false;
        }
    }
    for (&other_id, other_position) in positions {
        if other_id == id {
            continue;
        }
        let other = &problem.operations[other_id];
        if op.unit_mask & other.unit_mask != 0
            && schedule::intervals_overlap(
                (
                    u32::from(placement.cycle),
                    u32::from(placement.cycle) + u32::from(op.throughput.max(1)) - 1,
                ),
                (
                    u32::from(other_position.cycle),
                    u32::from(other_position.cycle) + u32::from(other.throughput.max(1)) - 1,
                ),
                None,
            )
        {
            return false;
        }
    }
    true
}

/// The single tail exchange in the original's branch-delay fix-up. An empty final
/// delay row is removed by advancing the lower branch one row and placing
/// the previous lower instruction in its delay slot. Upper slots stay put.
///
/// Supply actual placement dependencies, excluding artificial source-order
/// barriers to the branch. `zero_tail` proves which operations can occupy the
/// last row without leaving noninterlocked output debt. Fixed/modular tables
/// and externally required extents must be excluded by the caller.
pub fn adjust_branch_delay(
    problem: &SchedulingProblem,
    rows: &[[Option<usize>; 2]],
    zero_tail: &[usize],
    minimum_rows: usize,
) -> Vec<[Option<usize>; 2]> {
    let unchanged = || rows.to_vec();
    let n = rows.len();
    if problem.period.is_some()
        || n < 3
        || n <= minimum_rows
        || n >= 30000
        || rows[n - 1] != [None, None]
    {
        return unchanged();
    }
    let Some(branch) = rows[n - 2][1] else {
        return unchanged();
    };
    let Some(op) = problem.operations.get(branch) else {
        return unchanged();
    };
    if !op.is_branch || usize::from(op.earliest) > n - 3 {
        return unchanged();
    }

    let previous = rows[n - 3][1];
    if let Some(prev) = previous {
        let Some(prev_op) = problem.operations.get(prev) else {
            return unchanged();
        };
        if !zero_tail.contains(&prev)
            || prev_op.is_branch
            || prev_op.writes.iter().any(|write| {
                op.reads.iter().any(|read| {
                    write.register == read.register && write.components & read.components != 0
                })
            })
        {
            return unchanged();
        }
    }

    let mut result = rows[..n - 1].to_vec();
    result[n - 3][1] = Some(branch);
    result[n - 2][1] = previous;
    let mut positions = BTreeMap::new();
    for (row, pair) in result.iter().enumerate() {
        for (pipe, &id) in pair.iter().enumerate() {
            if let Some(id) = id {
                let Some(op) = problem.operations.get(id) else {
                    return unchanged();
                };
                if op.issue_mask & (1 << pipe) == 0
                    || positions
                        .insert(
                            id,
                            Placement {
                                cycle: row as u16,
                                pipe: if pipe == 0 { Pipe::Upper } else { Pipe::Lower },
                            },
                        )
                        .is_some()
                {
                    return unchanged();
                }
            }
        }
    }

    if positions.len() != problem.operations.len()
        || problem.dependencies.iter().any(|edge| {
            edge.distance != 0
                || !positions.contains_key(&edge.from)
                || !positions.contains_key(&edge.to)
        })
    {
        return unchanged();
    }

    for id in [Some(branch), previous].into_iter().flatten() {
        let at = positions[&id];
        result[usize::from(at.cycle)][at.pipe.index()] = None;
        let valid = fits(problem, &result, &positions, id, at);
        result[usize::from(at.cycle)][at.pipe.index()] = Some(id);
        if !valid {
            return unchanged();
        }
    }
    result
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::schedule::{Dependency, Operation, RegisterAccess};

    fn tail_problem() -> SchedulingProblem {
        let mut branch = Operation::new(1, 2);
        branch.is_branch = true;
        SchedulingProblem {
            operations: vec![Operation::new(0, 2), branch, Operation::new(2, 1)],
            ..Default::default()
        }
    }

    const TAIL: [[Option<usize>; 2]; 3] = [[Some(2), Some(0)], [None, Some(1)], [None, None]];

    #[test]
    fn delay_exchange_preserves_upper_slot_and_executes_displaced_lower_on_both_paths() {
        assert_eq!(
            adjust_branch_delay(&tail_problem(), &TAIL, &[0], 0),
            [[Some(2), Some(1)], [None, Some(0)],]
        );
    }

    #[test]
    fn delay_exchange_rejects_component_dependency_even_without_a_placement_edge() {
        let mut p = tail_problem();
        p.operations[0].writes.push(RegisterAccess {
            register: 7,
            components: 2,
        });
        p.operations[1].reads.push(RegisterAccess {
            register: 7,
            components: 2,
        });
        assert_eq!(adjust_branch_delay(&p, &TAIL, &[0], 0), TAIL);
        p.operations[1].reads[0].components = 1;
        assert_eq!(adjust_branch_delay(&p, &TAIL, &[0], 0).len(), 2);
    }

    #[test]
    fn delay_exchange_respects_readiness_real_edges_and_output_debt() {
        let mut p = tail_problem();
        p.operations[1].earliest = 1;
        assert_eq!(adjust_branch_delay(&p, &TAIL, &[0], 0), TAIL);
        p.operations[1].earliest = 0;
        p.dependencies.push(Dependency {
            from: 0,
            to: 1,
            latency: 0,
            distance: 0,
        });
        assert_eq!(adjust_branch_delay(&p, &TAIL, &[0], 0), TAIL);
        assert_eq!(adjust_branch_delay(&tail_problem(), &TAIL, &[], 0), TAIL);
        assert_eq!(adjust_branch_delay(&tail_problem(), &TAIL, &[0], 3), TAIL);
    }

    #[test]
    fn delay_exchange_checks_upper_pair_and_execution_unit_conflicts() {
        let mut p = tail_problem();
        p.operations[1].unit_mask = 1;
        p.operations[2].unit_mask = 1;
        assert_eq!(adjust_branch_delay(&p, &TAIL, &[0], 0), TAIL);
        p.operations[2].unit_mask = 0;
        p.operations[1].throughput = 2;
        p.operations[0].unit_mask = 1;
        assert_eq!(adjust_branch_delay(&p, &TAIL, &[0], 0), TAIL);
    }

    #[test]
    fn delay_exchange_requires_an_empty_last_pair_and_a_linear_table() {
        let mut rows = TAIL;
        rows[2][0] = Some(2);
        assert_eq!(adjust_branch_delay(&tail_problem(), &rows, &[0], 0), rows);
        let mut p = tail_problem();
        p.period = Some(3);
        assert_eq!(adjust_branch_delay(&p, &TAIL, &[0], 0), TAIL);
    }

    #[test]
    fn delay_exchange_can_advance_a_branch_into_an_empty_lower_slot() {
        let mut p = tail_problem();
        p.operations.remove(0);
        let rows = [[Some(1), None], [None, Some(0)], [None, None]];
        assert_eq!(
            adjust_branch_delay(&p, &rows, &[], 0),
            [[Some(1), Some(0)], [None, None]]
        );
    }
    fn problem() -> SchedulingProblem {
        let mut branch = Operation::new(2, 2);
        branch.is_branch = true;
        SchedulingProblem {
            operations: vec![
                Operation::new(0, 2),
                Operation::new(1, 2),
                branch,
                Operation::new(3, 1),
            ],
            dependencies: vec![Dependency {
                from: 1,
                to: 2,
                latency: 2,
                distance: 0,
            }],
            period: None,
        }
    }
    #[test]
    fn sink_an_unobserved_update_into_branch_delay_but_keep_branch_input_latency() {
        let p = problem();
        let rows = [
            [None, None],
            [None, Some(1)],
            [None, Some(0)],
            [None, Some(2)],
            [Some(3), None],
        ];
        let reduced = reduce(
            &p,
            &rows,
            &[
                Sink {
                    operation: 0,
                    exit_separation: 1,
                },
                Sink {
                    operation: 1,
                    exit_separation: 1,
                },
            ],
            0,
        )
        .unwrap();
        assert_eq!(reduced[1][1], Some(1));
        assert_eq!(reduced[2][1], None);
        assert_eq!(reduced[4], [Some(3), Some(0)]);
        assert_eq!(reduced[3][1], Some(2));
    }

    #[test]
    fn trim_tail_preserves_leading_readiness_rows_and_branch_delay() {
        let p = SchedulingProblem {
            operations: vec![Operation::new(0, 1)],
            ..Default::default()
        };
        let rows = [
            [None, None],
            [None, None],
            [None, None],
            [Some(0), None],
            [None, None],
        ];
        assert_eq!(reduce(&p, &rows, &[], 0).unwrap(), rows[..4]);
        assert_eq!(reduce(&p, &rows, &[], 5).unwrap(), rows);
        let mut b = Operation::new(0, 2);
        b.is_branch = true;
        let p = SchedulingProblem {
            operations: vec![b],
            ..Default::default()
        };
        assert_eq!(
            reduce(&p, &[[None, Some(0)], [None, None], [None, None]], &[], 0)
                .unwrap()
                .len(),
            2
        );
        assert!(
            reduce(&SchedulingProblem::default(), &[[None, None]; 3], &[], 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn escaping_branch_reader_reserves_extra_separation() {
        let p = problem();
        let rows = [
            [None, Some(0)],
            [None, Some(1)],
            [None, None],
            [None, Some(2)],
            [Some(3), None],
        ];
        let reduced = reduce(
            &p,
            &rows,
            &[Sink {
                operation: 0,
                exit_separation: 2,
            }],
            0,
        )
        .unwrap();
        assert_eq!(reduced[2][1], Some(0));
        assert_eq!(reduced[4][1], None);
    }

    #[test]
    fn anti_dependencies_and_whole_register_pair_writes_remain_constraints() {
        let mut p = problem();
        p.dependencies.push(Dependency {
            from: 0,
            to: 3,
            latency: 2,
            distance: 0,
        });
        let rows = [
            [None, Some(0)],
            [None, Some(1)],
            [None, None],
            [None, Some(2)],
            [Some(3), None],
        ];
        assert_eq!(
            reduce(
                &p,
                &rows,
                &[Sink {
                    operation: 0,
                    exit_separation: 1
                }],
                0
            )
            .unwrap()[2][1],
            Some(0)
        );
        p.dependencies.pop();
        p.operations[0].writes.push(RegisterAccess {
            register: 1,
            components: 1,
        });
        p.operations[3].writes.push(RegisterAccess {
            register: 1,
            components: 2,
        });
        assert_eq!(
            reduce(
                &p,
                &rows,
                &[Sink {
                    operation: 0,
                    exit_separation: 1
                }],
                0
            )
            .unwrap()[2][1],
            Some(0)
        );
    }

    #[test]
    fn throughput_prevents_an_otherwise_available_final_slot() {
        let mut p = problem();
        p.operations[0].unit_mask = 4;
        p.operations[3].unit_mask = 4;
        p.operations[3].throughput = 2;
        let rows = [
            [None, Some(0)],
            [None, Some(1)],
            [None, None],
            [None, Some(2)],
            [Some(3), None],
        ];
        assert_eq!(
            reduce(
                &p,
                &rows,
                &[Sink {
                    operation: 0,
                    exit_separation: 1
                }],
                0
            )
            .unwrap()[2][1],
            Some(0)
        );
    }

    #[test]
    fn invalid_table_views_are_errors() {
        let p = problem();
        assert!(matches!(
            reduce(&p, &[], &[], 0),
            Err(CleanupError::MissingOperation(0))
        ));
        let mut p = SchedulingProblem {
            operations: vec![Operation::new(0, 1)],
            ..Default::default()
        };
        assert!(matches!(
            reduce(&p, &[[None, Some(0)]], &[], 0),
            Err(CleanupError::InvalidPipe(0))
        ));
        assert!(matches!(
            reduce(&p, &[[Some(0), None]; 2], &[], 0),
            Err(CleanupError::DuplicateOperation(0))
        ));
        let mut branch = Operation::new(0, 2);
        branch.is_branch = true;
        let no_delay = SchedulingProblem {
            operations: vec![branch],
            ..Default::default()
        };
        assert!(matches!(
            reduce(&no_delay, &[[None, Some(0)]], &[], 0),
            Err(CleanupError::MissingBranchDelay)
        ));
        assert!(matches!(
            reduce(
                &p,
                &[[Some(0), None]],
                &[Sink {
                    operation: 1,
                    exit_separation: 1
                }],
                0
            ),
            Err(CleanupError::InvalidSink(1))
        ));
        p.dependencies.push(Dependency {
            from: 1,
            to: 0,
            distance: 0,
            latency: 1,
        });
        assert!(matches!(
            reduce(&p, &[[Some(0), None]], &[], 0),
            Err(CleanupError::InvalidDependency)
        ));
        p.period = Some(5);
        assert!(matches!(
            reduce(&p, &[], &[], 0),
            Err(CleanupError::ModularTable)
        ));
    }
}
