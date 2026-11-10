//! Bounded linear raw-table reduction inferred from vcl 1.4beta7.
//!
//! Pull-up and drag-down alternate while the
//! extent shrinks. The caller supplies actual placement edges and external
//! bounds after checking mode, through values and initial stalls. This helper
//! retains selected pipes; the original also tries alternate pipes and checks
//! dynamic physical locks. It is not a replacement for that complete engine.

use crate::scheduler::schedule::{self, Pipe, SchedulingProblem};
use std::fmt;

/// Per-operation proof supplied by the compiler's external-flow adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    /// Extra issue rows required after this operation before the table ends.
    pub tail: u16,
    /// False pins the original issue row and prevents rebasing the table start.
    pub movable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawCleanupError {
    ModularTable,
    TooManyRows,
    InvalidMinimum,
    InvalidBounds { expected: usize, actual: usize },
    InvalidOperation(usize),
    DuplicateOperation(usize),
    MissingOperation(usize),
    InvalidPipe(usize),
    InvalidDependency { from: usize, to: usize },
    MultipleBranches,
    InvalidBranchLayout,
    InvalidPlacement(usize),
}

impl fmt::Display for RawCleanupError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "linear raw reduction failed: {self:?}")
    }
}

impl std::error::Error for RawCleanupError {}

struct Table<'a> {
    problem: &'a SchedulingProblem,
    bounds: &'a [Bounds],
    original: Vec<usize>,
    positions: Vec<usize>,
    pipes: Vec<Pipe>,
    branch: Option<usize>,
    extent: usize,
    minimum: usize,
}

impl Table<'_> {
    /// Check the one operation whose position changed. Other operations remain
    /// valid; edges involving either changed end are checked against the new
    /// complete position vector. Pair reads are simultaneous: a reverse zero
    /// WAR edge permits the original pair, but prevents an earlier write.
    fn fits(&self, id: usize) -> bool {
        let row = self.positions[id];
        let op = &self.problem.operations[id];
        if row < usize::from(op.earliest)
            || row + usize::from(self.bounds[id].tail) >= self.extent
            || (!self.bounds[id].movable && row != self.original[id])
        {
            return false;
        }
        for edge in &self.problem.dependencies {
            if (edge.from == id || edge.to == id)
                && self.positions[edge.to] < self.positions[edge.from] + usize::from(edge.latency)
            {
                return false;
            }
        }
        for (other, &other_row) in self.positions.iter().enumerate() {
            if other == id {
                continue;
            }
            let other_op = &self.problem.operations[other];
            if row == other_row
                && (self.pipes[id] == self.pipes[other]
                    || !schedule::pair_compatible(op, self.pipes[id], other_op, self.pipes[other]))
            {
                return false;
            }
            if op.unit_mask & other_op.unit_mask != 0
                && schedule::intervals_overlap(
                    (row as u32, row as u32 + u32::from(op.throughput.max(1)) - 1),
                    (
                        other_row as u32,
                        other_row as u32 + u32::from(other_op.throughput.max(1)) - 1,
                    ),
                    None,
                )
            {
                return false;
            }
        }
        true
    }

    fn move_to(&mut self, id: usize, row: usize) -> bool {
        let old = self.positions[id];
        self.positions[id] = row;
        if self.fits(id) {
            true
        } else {
            self.positions[id] = old;
            false
        }
    }

    fn dependent_delay(&self) -> Option<usize> {
        let branch = self.branch?;
        self.positions.iter().enumerate().find_map(|(id, &row)| {
            (self.pipes[id] == Pipe::Lower
                && row == self.positions[branch] + 1
                && self.problem.operations[id].writes.iter().any(|write| {
                    self.problem.operations[branch].reads.iter().any(|read| {
                        write.register == read.register && write.components & read.components != 0
                    })
                }))
            .then_some(id)
        })
    }

    fn move_branch(&mut self, row: usize, delay: Option<usize>) -> bool {
        let branch = self.branch.unwrap();
        let old_branch = self.positions[branch];
        let old_delay = delay.map(|id| self.positions[id]);
        self.positions[branch] = row;
        if let Some(id) = delay {
            self.positions[id] = row + 1;
        }
        if self.fits(branch) && delay.is_none_or(|id| self.fits(id)) {
            return true;
        }
        self.positions[branch] = old_branch;
        if let Some(id) = delay {
            self.positions[id] = old_delay.unwrap();
        }
        false
    }

    fn pull_up(&mut self) {
        let delay = self.dependent_delay();
        loop {
            let mut changed = false;
            let mut order: Vec<_> = (0..self.positions.len()).collect();
            order.sort_by_key(|&id| (self.positions[id], self.pipes[id].index()));
            for id in order {
                if Some(id) == self.branch || Some(id) == delay || !self.bounds[id].movable {
                    continue;
                }
                let old = self.positions[id];
                for row in usize::from(self.problem.operations[id].earliest)..old {
                    if self.move_to(id, row) {
                        changed = true;
                        break;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let ordinary_extent = self
            .positions
            .iter()
            .enumerate()
            .filter(|&(id, _)| Some(id) != self.branch && Some(id) != delay)
            .map(|(id, &row)| row + usize::from(self.bounds[id].tail) + 1)
            .max()
            .unwrap_or(0)
            .max(self.minimum);
        if let Some(branch) = self.branch {
            // The original searches from max(start, last-required-row - 1)
            // for a branch and leaves one complete row after it.
            for row in ordinary_extent.saturating_sub(2)..self.extent - 1 {
                if self.move_branch(row, delay) {
                    break;
                }
            }
            self.extent = ordinary_extent.max(self.positions[branch] + 2);
        } else {
            self.extent = ordinary_extent;
        }
    }

    fn drag_down(&mut self) {
        loop {
            let mut changed = false;
            let mut order: Vec<_> = (0..self.positions.len()).collect();
            order.sort_by_key(|&id| {
                (
                    std::cmp::Reverse(self.positions[id]),
                    self.pipes[id].index(),
                )
            });
            for id in order {
                if Some(id) == self.branch || !self.bounds[id].movable {
                    continue;
                }
                let old = self.positions[id];
                let latest = self.extent - usize::from(self.bounds[id].tail) - 1;
                for row in (old + 1..=latest).rev() {
                    if self.move_to(id, row) {
                        changed = true;
                        break;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        if self.bounds.iter().any(|bound| !bound.movable) {
            return;
        }
        let start = self
            .positions
            .iter()
            .enumerate()
            .map(|(id, &row)| row - usize::from(self.problem.operations[id].earliest))
            .min()
            .unwrap_or(0)
            .min(self.extent - self.minimum);
        for row in &mut self.positions {
            *row -= start;
        }
        self.extent -= start;
    }
}

/// Reduce a caller-proven movable raw table without changing selected pipes.
///
/// `rows` contain operation indices, not `Operation::source_order` values.
/// Actual RAW/WAR/WAW/resource constraints must hold initially. In particular,
/// paired old-value reads may require reverse zero-latency WAR edges. Artificial
/// source-order branch barriers must not be mistaken for actual dependencies.
/// A branch occupies the lower penultimate slot with one explicit delay pair.
pub fn reduce(
    problem: &SchedulingProblem,
    rows: &[[Option<usize>; 2]],
    bounds: &[Bounds],
    minimum_rows: usize,
) -> Result<Vec<[Option<usize>; 2]>, RawCleanupError> {
    if problem.period.is_some() {
        return Err(RawCleanupError::ModularTable);
    }
    if rows.len() >= 30000 {
        return Err(RawCleanupError::TooManyRows);
    }
    if minimum_rows > rows.len() {
        return Err(RawCleanupError::InvalidMinimum);
    }
    let count = problem.operations.len();
    if bounds.len() != count {
        return Err(RawCleanupError::InvalidBounds {
            expected: count,
            actual: bounds.len(),
        });
    }
    let mut positions = vec![usize::MAX; count];
    let mut pipes = vec![Pipe::Upper; count];
    for (row, pair) in rows.iter().enumerate() {
        for (pipe, &id) in pair.iter().enumerate() {
            let Some(id) = id else { continue };
            let op = problem
                .operations
                .get(id)
                .ok_or(RawCleanupError::InvalidOperation(id))?;
            if positions[id] != usize::MAX {
                return Err(RawCleanupError::DuplicateOperation(id));
            }
            if op.issue_mask & (1 << pipe) == 0 {
                return Err(RawCleanupError::InvalidPipe(id));
            }
            positions[id] = row;
            pipes[id] = if pipe == 0 { Pipe::Upper } else { Pipe::Lower };
        }
    }
    if let Some(id) = positions.iter().position(|&row| row == usize::MAX) {
        return Err(RawCleanupError::MissingOperation(id));
    }
    for edge in &problem.dependencies {
        if edge.from >= count || edge.to >= count || edge.distance != 0 {
            return Err(RawCleanupError::InvalidDependency {
                from: edge.from,
                to: edge.to,
            });
        }
    }
    let branches: Vec<_> = problem
        .operations
        .iter()
        .enumerate()
        .filter_map(|(id, op)| op.is_branch.then_some(id))
        .collect();
    if branches.len() > 1 {
        return Err(RawCleanupError::MultipleBranches);
    }
    let branch = branches.first().copied();
    if branch.is_some_and(|id| pipes[id] != Pipe::Lower || positions[id] + 2 != rows.len()) {
        return Err(RawCleanupError::InvalidBranchLayout);
    }
    let mut table = Table {
        problem,
        bounds,
        original: positions.clone(),
        positions,
        pipes,
        branch,
        extent: rows.len(),
        minimum: minimum_rows,
    };
    for id in 0..count {
        if !table.fits(id) {
            return Err(RawCleanupError::InvalidPlacement(id));
        }
    }
    loop {
        let old = table.extent;
        table.pull_up();
        table.drag_down();
        if table.extent == old {
            break;
        }
    }
    let mut result = vec![[None, None]; table.extent];
    for (id, &row) in table.positions.iter().enumerate() {
        result[row][table.pipes[id].index()] = Some(id);
    }
    Ok(result)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::schedule::{Dependency, Operation, RegisterAccess};

    fn movable(count: usize) -> Vec<Bounds> {
        vec![
            Bounds {
                tail: 0,
                movable: true
            };
            count
        ]
    }

    fn edge(from: usize, to: usize, latency: u16) -> Dependency {
        Dependency {
            from,
            to,
            latency,
            distance: 0,
        }
    }

    fn position(rows: &[[Option<usize>; 2]], id: usize) -> usize {
        rows.iter()
            .position(|pair| pair.contains(&Some(id)))
            .unwrap()
    }

    fn branch(id: usize) -> Operation {
        let mut op = Operation::new(id, 2);
        op.is_branch = true;
        op
    }

    #[test]
    fn combine_independent_selected_pipes_and_remove_empty_extent() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1), Operation::new(1, 2)],
            ..Default::default()
        };
        let rows = [
            [None, None],
            [None, None],
            [None, None],
            [Some(0), None],
            [None, Some(1)],
        ];
        assert_eq!(
            reduce(&problem, &rows, &movable(2), 0).unwrap(),
            [[Some(0), Some(1)]]
        );
    }

    #[test]
    fn keep_true_latency_rows_until_the_callers_physical_replay() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1), Operation::new(1, 1)],
            dependencies: vec![edge(0, 1, 4)],
            ..Default::default()
        };
        let mut rows = vec![[None, None]; 10];
        rows[1][0] = Some(0);
        rows[7][0] = Some(1);
        let result = reduce(&problem, &rows, &movable(2), 0).unwrap();
        assert_eq!(result.len(), 5);
        assert_eq!((position(&result, 0), position(&result, 1)), (0, 4));
    }

    #[test]
    fn reverse_war_preserves_a_paired_lower_read_of_the_old_upper_destination() {
        let mut upper = Operation::new(0, 1);
        upper.writes.push(RegisterAccess {
            register: 7,
            components: 1,
        });
        let mut lower = Operation::new(1, 2);
        lower.reads.push(RegisterAccess {
            register: 7,
            components: 1,
        });
        lower.earliest = 2;
        let problem = SchedulingProblem {
            operations: vec![upper, lower],
            dependencies: vec![edge(1, 0, 0)],
            ..Default::default()
        };
        let mut rows = vec![[None, None]; 6];
        rows[4] = [Some(0), Some(1)];
        let result = reduce(&problem, &rows, &movable(2), 0).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[2], [Some(0), Some(1)]);
    }

    #[test]
    fn advance_branch_after_its_real_input_latency_and_keep_the_delay_pair() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 2), branch(1)],
            dependencies: vec![edge(0, 1, 2)],
            ..Default::default()
        };
        let mut rows = vec![[None, None]; 6];
        rows[1][1] = Some(0);
        rows[4][1] = Some(1);
        let result = reduce(&problem, &rows, &movable(2), 0).unwrap();
        assert_eq!(
            result,
            [[None, Some(0)], [None, None], [None, Some(1)], [None, None]]
        );
    }

    #[test]
    fn drag_independent_lower_into_delay_then_rebase_the_leading_extent() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 2), branch(1)],
            ..Default::default()
        };
        let mut rows = vec![[None, None]; 5];
        rows[0][1] = Some(0);
        rows[3][1] = Some(1);
        assert_eq!(
            reduce(&problem, &rows, &movable(2), 0).unwrap(),
            [[None, Some(1)], [None, Some(0)]]
        );
    }

    #[test]
    fn preserve_a_delay_write_to_the_branchs_old_input() {
        let mut branch = branch(0);
        branch.reads.push(RegisterAccess {
            register: 7,
            components: 1,
        });
        let mut delay = Operation::new(1, 2);
        delay.writes.push(RegisterAccess {
            register: 7,
            components: 1,
        });
        let problem = SchedulingProblem {
            operations: vec![branch, delay],
            dependencies: vec![edge(0, 1, 0)],
            ..Default::default()
        };
        let mut rows = vec![[None, None]; 5];
        rows[3][1] = Some(0);
        rows[4][1] = Some(1);
        assert_eq!(
            reduce(&problem, &rows, &movable(2), 0).unwrap(),
            [[None, Some(0)], [None, Some(1)]]
        );
    }

    #[test]
    fn preserve_external_tail_and_minimum_extent() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1)],
            ..Default::default()
        };
        let mut rows = vec![[None, None]; 5];
        rows[2][0] = Some(0);
        let bounds = [Bounds {
            tail: 2,
            movable: true,
        }];
        let result = reduce(&problem, &rows, &bounds, 0).unwrap();
        assert_eq!(result, [[Some(0), None], [None, None], [None, None]]);
        assert_eq!(reduce(&problem, &rows, &bounds, 5).unwrap().len(), 5);
    }

    #[test]
    fn earliest_and_immovable_rows_survive_leading_reduction() {
        let mut op = Operation::new(0, 1);
        op.earliest = 2;
        let problem = SchedulingProblem {
            operations: vec![op],
            ..Default::default()
        };
        let mut rows = vec![[None, None]; 6];
        rows[3][0] = Some(0);
        let result = reduce(&problem, &rows, &movable(1), 0).unwrap();
        assert_eq!(result, [[None, None], [None, None], [Some(0), None]]);
        let pinned = [Bounds {
            tail: 0,
            movable: false,
        }];
        let result = reduce(&problem, &rows, &pinned, 0).unwrap();
        assert_eq!(position(&result, 0), 3);
    }

    #[test]
    fn retain_execution_unit_throughput_and_whole_register_pair_exclusion() {
        let mut a = Operation::new(0, 1);
        a.unit_mask = 1;
        a.throughput = 2;
        let mut b = Operation::new(1, 2);
        b.unit_mask = 1;
        let problem = SchedulingProblem {
            operations: vec![a, b],
            ..Default::default()
        };
        let rows = [[Some(0), None], [None, None], [None, None], [None, Some(1)]];
        let result = reduce(&problem, &rows, &movable(2), 0).unwrap();
        assert_eq!((position(&result, 0), position(&result, 1)), (0, 2));
        let mut problem = problem;
        for op in &mut problem.operations {
            op.unit_mask = 0;
        }
        problem.operations[0].writes.push(RegisterAccess {
            register: 4,
            components: 1,
        });
        problem.operations[1].writes.push(RegisterAccess {
            register: 4,
            components: 2,
        });
        assert_eq!(reduce(&problem, &rows, &movable(2), 0).unwrap().len(), 2);
    }

    #[test]
    fn flexible_operations_keep_their_selected_pipe_in_this_adapter() {
        let problem = SchedulingProblem {
            operations: vec![Operation::new(0, 3), Operation::new(1, 3)],
            ..Default::default()
        };
        let rows = [[Some(0), None], [Some(1), None]];
        assert_eq!(reduce(&problem, &rows, &movable(2), 0).unwrap(), rows);
    }

    #[test]
    fn reject_malformed_identities_bounds_dependencies_and_initial_placement() {
        let mut problem = SchedulingProblem {
            operations: vec![Operation::new(0, 1)],
            ..Default::default()
        };
        assert_eq!(
            reduce(&problem, &[[Some(1), None]], &movable(1), 0),
            Err(RawCleanupError::InvalidOperation(1))
        );
        assert_eq!(
            reduce(
                &problem,
                &[[Some(0), None], [Some(0), None]],
                &movable(1),
                0
            ),
            Err(RawCleanupError::DuplicateOperation(0))
        );
        assert_eq!(
            reduce(&problem, &[], &movable(1), 0),
            Err(RawCleanupError::MissingOperation(0))
        );
        assert_eq!(
            reduce(&problem, &[[None, Some(0)]], &movable(1), 0),
            Err(RawCleanupError::InvalidPipe(0))
        );
        assert_eq!(
            reduce(&problem, &[[Some(0), None]], &[], 0),
            Err(RawCleanupError::InvalidBounds {
                expected: 1,
                actual: 0
            })
        );
        assert_eq!(
            reduce(&problem, &[[Some(0), None]], &movable(1), 2),
            Err(RawCleanupError::InvalidMinimum)
        );
        problem.dependencies.push(edge(0, 1, 1));
        assert_eq!(
            reduce(&problem, &[[Some(0), None]], &movable(1), 0),
            Err(RawCleanupError::InvalidDependency { from: 0, to: 1 })
        );
        problem.dependencies.clear();
        problem.operations[0].earliest = 1;
        assert_eq!(
            reduce(&problem, &[[Some(0), None]], &movable(1), 0),
            Err(RawCleanupError::InvalidPlacement(0))
        );
    }

    #[test]
    fn reject_modulo_tables_and_malformed_branch_frames() {
        let mut problem = SchedulingProblem {
            period: Some(2),
            ..Default::default()
        };
        assert_eq!(
            reduce(&problem, &[], &[], 0),
            Err(RawCleanupError::ModularTable)
        );
        problem.period = None;
        problem.operations.push(branch(0));
        assert_eq!(
            reduce(&problem, &[[None, Some(0)]], &movable(1), 0),
            Err(RawCleanupError::InvalidBranchLayout)
        );
        problem.operations.push(branch(1));
        assert_eq!(
            reduce(
                &problem,
                &[[None, Some(0)], [None, Some(1)]],
                &movable(2),
                0
            ),
            Err(RawCleanupError::MultipleBranches)
        );
    }

    #[test]
    fn reduction_preserves_constraints_across_a_matrix_of_linear_frames() {
        for seed in 0usize..128 {
            let count = 3 + seed % 3;
            let has_branch = seed & 1 != 0;
            let mut problem = SchedulingProblem::default();
            let mut bounds = Vec::new();
            let mut input = vec![[None, None]; 2 * count + 8];
            for id in 0..count {
                let pipe = (seed >> id) & 1;
                let mut op = Operation::new(id, 1 << pipe);
                op.unit_mask = 1 << ((seed + id) % 3);
                op.throughput = (1 + ((seed >> id) & 1)) as u16;
                op.earliest = ((seed + id) % 4) as u16;
                input[2 * id + 4][pipe] = Some(id);
                problem.operations.push(op);
                bounds.push(Bounds {
                    tail: ((seed >> id) % 3) as u16,
                    movable: (seed + id) % 5 != 0,
                });
                if id > 0 {
                    problem
                        .dependencies
                        .push(edge(id - 1, id, (1 + seed % 2) as u16));
                }
            }
            if has_branch {
                let row = input.len() - 2;
                input[row][1] = Some(count);
                problem.operations.push(branch(count));
                problem.dependencies.push(edge(count - 1, count, 2));
                bounds.push(Bounds {
                    tail: 0,
                    movable: true,
                });
            }
            let minimum = seed % 4;
            let output = reduce(&problem, &input, &bounds, minimum).unwrap();
            assert!((minimum..=input.len()).contains(&output.len()));
            let mut placements = vec![(usize::MAX, 0); problem.operations.len()];
            for (row, pair) in output.iter().enumerate() {
                for (pipe, &id) in pair.iter().enumerate() {
                    let Some(id) = id else { continue };
                    assert_eq!(placements[id].0, usize::MAX, "duplicate seed {seed}");
                    placements[id] = (row, pipe);
                }
            }
            for (id, &(row, pipe)) in placements.iter().enumerate() {
                let op = &problem.operations[id];
                assert_ne!(row, usize::MAX);
                assert!(row >= usize::from(op.earliest));
                assert!(row + usize::from(bounds[id].tail) < output.len());
                assert_ne!(op.issue_mask & (1 << pipe), 0);
                if !bounds[id].movable {
                    assert_eq!(row, position(&input, id));
                }
                if op.is_branch {
                    assert_eq!((row + 2, pipe), (output.len(), 1));
                }
                for (other, &(other_row, _)) in placements.iter().enumerate().take(id) {
                    let other_op = &problem.operations[other];
                    if op.unit_mask & other_op.unit_mask != 0 {
                        let overlap = row < other_row + usize::from(other_op.throughput)
                            && other_row < row + usize::from(op.throughput);
                        assert!(!overlap, "unit overlap seed {seed}");
                    }
                }
            }
            for edge in &problem.dependencies {
                assert!(
                    placements[edge.to].0 >= placements[edge.from].0 + usize::from(edge.latency)
                );
            }
        }
    }
}
