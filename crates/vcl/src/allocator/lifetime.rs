//! Component occupancy from scheduled streams and CFG liveness.
//!
//! The original rasterizes individual output-lock
//! intervals, preserving holes. It records each output's folded
//! issue row and marks that row with the extra 0x80 lock. Read endpoints
//! are exclusive, allowing a destination to reuse its last source's location.

use crate::allocator::allocation::{AllocationError, EXCLUSIVE, Occupancy};
use crate::analysis::flow::{Access, Operation};
use std::collections::{BTreeMap, BTreeSet};

pub struct ScheduledBlock<'a> {
    pub start: usize,
    pub rows: &'a [[Option<usize>; 2]],
    pub live_out: &'a BTreeSet<Access>,
}

/// Walk both halves' writes before adding either half's reads to the backward
/// state. Forward execution therefore fetches both inputs before writing.
/// Live-out includes synthetic outputs and loop-through components.
pub fn build(
    operations: &[Operation],
    blocks: &[ScheduledBlock<'_>],
    total_rows: usize,
    constants: &BTreeSet<usize>,
) -> Result<BTreeMap<usize, Occupancy>, AllocationError> {
    let mut result = BTreeMap::new();
    for block in blocks {
        let end =
            block
                .start
                .checked_add(block.rows.len())
                .ok_or(AllocationError::InvalidRange {
                    start: block.start,
                    end: usize::MAX,
                    rows: total_rows,
                })?;
        if end > total_rows {
            return Err(AllocationError::InvalidRange {
                start: block.start,
                end,
                rows: total_rows,
            });
        }
        let mut live = BTreeMap::<usize, u8>::new();
        for access in block.live_out {
            if !constants.contains(&access.register) {
                *live.entry(access.register).or_default() |= access.mask;
            }
        }
        for (offset, pair) in block.rows.iter().enumerate().rev() {
            let row = block.start + offset;
            for (&register, &mask) in &live {
                mark(&mut result, total_rows, register, row, mask)?;
            }
            for &id in pair.iter().flatten() {
                let operation = operations
                    .get(id)
                    .ok_or(AllocationError::InvalidOperation(id))?;
                for write in &operation.writes {
                    if constants.contains(&write.register) || write.mask == 0 {
                        continue;
                    }
                    mark(
                        &mut result,
                        total_rows,
                        write.register,
                        row,
                        write.mask | EXCLUSIVE,
                    )?;
                    if let Some(mask) = live.get_mut(&write.register) {
                        *mask &= !write.mask;
                        if *mask == 0 {
                            live.remove(&write.register);
                        }
                    }
                }
            }
            for &id in pair.iter().flatten() {
                for read in &operations
                    .get(id)
                    .ok_or(AllocationError::InvalidOperation(id))?
                    .reads
                {
                    if constants.contains(&read.register) || read.mask == 0 {
                        continue;
                    }
                    result
                        .entry(read.register)
                        .or_insert_with(|| Occupancy::new(total_rows));
                    *live.entry(read.register).or_default() |= read.mask;
                }
            }
        }
    }
    Ok(result)
}

fn mark(
    result: &mut BTreeMap<usize, Occupancy>,
    total_rows: usize,
    register: usize,
    row: usize,
    mask: u8,
) -> Result<(), AllocationError> {
    result
        .entry(register)
        .or_insert_with(|| Occupancy::new(total_rows))
        .add(row..row + 1, mask, false)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocator::allocation::{Allocator, VirtualRegister};

    fn op(reads: &[(usize, u8)], writes: &[(usize, u8)]) -> Operation {
        Operation {
            reads: reads
                .iter()
                .map(|&(register, mask)| Access { register, mask })
                .collect(),
            writes: writes
                .iter()
                .map(|&(register, mask)| Access { register, mask })
                .collect(),
            side_effect: false,
        }
    }

    #[test]
    fn disconnected_streams_do_not_fill_a_global_hull() {
        let operations = vec![
            op(&[], &[(1, 1)]),
            op(&[(1, 1)], &[]),
            op(&[], &[(2, 1)]),
            op(&[(2, 1)], &[]),
        ];
        let none = BTreeSet::new();
        let a = [[Some(0), None], [Some(1), None]];
        let b = [[Some(2), None], [Some(3), None]];
        let raster = build(
            &operations,
            &[
                ScheduledBlock {
                    start: 0,
                    rows: &a,
                    live_out: &none,
                },
                ScheduledBlock {
                    start: 4,
                    rows: &b,
                    live_out: &none,
                },
            ],
            6,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(raster[&1].rows(), &[0x81, 0, 0, 0, 0, 0]);
        assert_eq!(raster[&2].rows(), &[0, 0, 0, 0, 0x81, 0]);
    }

    #[test]
    fn component_intervals_and_last_read_reuse_are_independent() {
        let operations = vec![
            op(&[], &[(1, 1)]),
            op(&[(1, 1)], &[(2, 1)]),
            op(&[], &[(1, 8)]),
            op(&[(1, 8), (2, 1)], &[]),
        ];
        let rows = [
            [Some(0), None],
            [Some(1), None],
            [Some(2), None],
            [Some(3), None],
        ];
        let none = BTreeSet::new();
        let raster = build(
            &operations,
            &[ScheduledBlock {
                start: 0,
                rows: &rows,
                live_out: &none,
            }],
            4,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(raster[&1].rows(), &[0x81, 0, 0x88, 0]);
        assert_eq!(raster[&2].rows(), &[0, 0x81, 1, 0]);
        let mut allocator = Allocator::new(4, vec![15], 4).unwrap();
        let assigned = allocator
            .allocate(&[
                VirtualRegister {
                    id: 1,
                    occupancy: raster[&1].clone(),
                    forced: None,
                },
                VirtualRegister {
                    id: 2,
                    occupancy: raster[&2].clone(),
                    forced: None,
                },
            ])
            .unwrap();
        assert_eq!(assigned.len(), 2); // one physical register suffices
    }

    #[test]
    fn disjoint_simultaneous_outputs_need_distinct_registers() {
        let operations = vec![op(&[], &[(1, 7)]), op(&[], &[(2, 8)])];
        let rows = [[Some(0), Some(1)]];
        let none = BTreeSet::new();
        let raster = build(
            &operations,
            &[ScheduledBlock {
                start: 0,
                rows: &rows,
                live_out: &none,
            }],
            1,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(raster[&1].rows(), &[0x87]);
        assert_eq!(raster[&2].rows(), &[0x88]);
        let mut allocator = Allocator::new(4, vec![15], 1).unwrap();
        assert!(
            allocator
                .allocate(&[
                    VirtualRegister {
                        id: 1,
                        occupancy: raster[&1].clone(),
                        forced: None
                    },
                    VirtualRegister {
                        id: 2,
                        occupancy: raster[&2].clone(),
                        forced: None
                    }
                ])
                .is_err()
        );
    }

    #[test]
    fn loop_invariant_and_output_components_survive_padding() {
        let operations = vec![op(&[(1, 7)], &[(2, 1)])];
        let rows = [[Some(0), None], [None, None], [None, None]];
        let out = BTreeSet::from([
            Access {
                register: 1,
                mask: 7,
            },
            Access {
                register: 2,
                mask: 1,
            },
        ]);
        let raster = build(
            &operations,
            &[ScheduledBlock {
                start: 0,
                rows: &rows,
                live_out: &out,
            }],
            3,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(raster[&1].rows(), &[7, 7, 7]);
        assert_eq!(raster[&2].rows(), &[0x81, 1, 1]);
    }

    #[test]
    fn constants_never_acquire_write_locks_or_component_reservations() {
        let operations = [op(&[(0, 15)], &[(0, 15)])];
        let rows = [[Some(0), None]];
        let out = BTreeSet::from([Access {
            register: 0,
            mask: 15,
        }]);
        let result = build(
            &operations,
            &[ScheduledBlock {
                start: 0,
                rows: &rows,
                live_out: &out,
            }],
            1,
            &BTreeSet::from([0]),
        )
        .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn malformed_schedule_indices_and_ranges_return_errors() {
        let rows = [[Some(7), None]];
        let out = BTreeSet::new();
        assert!(matches!(
            build(
                &[],
                &[ScheduledBlock {
                    start: 0,
                    rows: &rows,
                    live_out: &out
                }],
                1,
                &BTreeSet::new()
            ),
            Err(AllocationError::InvalidOperation(7))
        ));
        assert!(matches!(
            build(
                &[],
                &[ScheduledBlock {
                    start: 1,
                    rows: &rows,
                    live_out: &out
                }],
                1,
                &BTreeSet::new()
            ),
            Err(AllocationError::InvalidRange { .. })
        ));
    }
}
