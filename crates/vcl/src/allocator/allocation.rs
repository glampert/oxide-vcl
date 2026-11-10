//! Component-aware occupancy packing inferred from vcl 1.4beta7.
//!
//! Low seven bits are component availability; bit seven is an additional
//! exclusivity constraint. The original uses fixed global arrays. This port
//! owns and bounds its tables and returns errors instead of exiting.

use std::fmt;
use std::ops::Range;

const COMPONENTS: u8 = 0x7f;
pub const EXCLUSIVE: u8 = 0x80;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllocationError {
    InvalidComponentCount(u8),
    InvalidRange {
        start: usize,
        end: usize,
        rows: usize,
    },
    InvalidMask(u8),
    InvalidPhysical(usize),
    WrongRowCount {
        expected: usize,
        actual: usize,
    },
    NoFit {
        virtual_id: usize,
    },
    ForcedConflict {
        virtual_id: usize,
        physical: usize,
    },
    DuplicateVirtual(usize),
    InvalidOperation(usize),
}

impl fmt::Display for AllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "register allocation failed: {self:?}")
    }
}

impl std::error::Error for AllocationError {}

/// One logical register's liveness raster. Rows are scheduling positions, not
/// necessarily processor cycles (the scheduler accounts for hardware stalls).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occupancy {
    rows: Vec<u8>,
}

impl Occupancy {
    pub fn new(rows: usize) -> Self {
        Self {
            rows: vec![0; rows],
        }
    }

    pub fn rows(&self) -> &[u8] {
        &self.rows
    }

    /// Add a half-open lifetime. A modulo table folds each time into its period.
    /// This models the original after the original lock sentinels are resolved.
    pub fn add(
        &mut self,
        range: Range<usize>,
        mask: u8,
        modulo: bool,
    ) -> Result<(), AllocationError> {
        if range.start > range.end
            || (!modulo && range.end > self.rows.len())
            || (modulo && self.rows.is_empty() && !range.is_empty())
        {
            return Err(AllocationError::InvalidRange {
                start: range.start,
                end: range.end,
                rows: self.rows.len(),
            });
        }
        for time in range {
            let row = if modulo { time % self.rows.len() } else { time };
            self.rows[row] |= mask;
        }
        Ok(())
    }

    pub fn occupied_rows(&self) -> usize {
        self.rows.iter().filter(|&&mask| mask != 0).count()
    }
}

#[derive(Clone, Debug)]
pub struct VirtualRegister {
    pub id: usize,
    pub occupancy: Occupancy,
    pub forced: Option<usize>,
}

/// Per-pool state. Allowed masks represent `.init_*` access, including partial
/// VF permissions; occupancy includes physical and forced-register lifetimes.
#[derive(Clone, Debug)]
pub struct Allocator {
    components: u8,
    allowed: Vec<u8>,
    occupied: Vec<Occupancy>,
    rows: usize,
}

impl Allocator {
    pub fn new(components: u8, allowed: Vec<u8>, rows: usize) -> Result<Self, AllocationError> {
        if components == 0 || components > 7 {
            return Err(AllocationError::InvalidComponentCount(components));
        }
        let valid = (1u8 << components) - 1;
        if let Some(&mask) = allowed.iter().find(|&&mask| mask & !valid != 0) {
            return Err(AllocationError::InvalidMask(mask));
        }
        Ok(Self {
            components,
            occupied: vec![Occupancy::new(rows); allowed.len()],
            allowed,
            rows,
        })
    }

    pub fn occupancy(&self, physical: usize) -> Option<&Occupancy> {
        self.occupied.get(physical)
    }

    fn validate(&self, candidate: &Occupancy) -> Result<(), AllocationError> {
        if candidate.rows.len() != self.rows {
            return Err(AllocationError::WrongRowCount {
                expected: self.rows,
                actual: candidate.rows.len(),
            });
        }
        let valid = ((1u8 << self.components) - 1) | EXCLUSIVE;
        if let Some(&mask) = candidate.rows.iter().find(|&&mask| mask & !valid != 0) {
            return Err(AllocationError::InvalidMask(mask));
        }
        Ok(())
    }

    /// Semantic port of the candidate check and cost.
    pub fn packing_cost(
        &self,
        physical: usize,
        candidate: &Occupancy,
    ) -> Result<Option<u32>, AllocationError> {
        self.validate(candidate)?;
        let Some(occupied) = self.occupied.get(physical) else {
            return Err(AllocationError::InvalidPhysical(physical));
        };
        let mut cost = 0;
        for (&want, &used) in candidate.rows.iter().zip(&occupied.rows) {
            if self.allowed[physical] & want & COMPONENTS != want & COMPONENTS || want & used != 0 {
                return Ok(None);
            }
            cost += u32::from(self.components) - ((want | used) & COMPONENTS).count_ones();
        }
        Ok(Some(cost))
    }

    /// Physical IDs are visited in order; equal costs retain the earlier ID.
    pub fn best_fit(&self, candidate: &Occupancy) -> Result<Option<usize>, AllocationError> {
        self.validate(candidate)?;
        let mut best = None;
        for physical in 0..self.allowed.len() {
            if let Some(cost) = self.packing_cost(physical, candidate)? {
                if cost == 0 {
                    return Ok(Some(physical));
                }
                if best.is_none_or(|(_, previous)| cost < previous) {
                    best = Some((physical, cost));
                }
            }
        }
        Ok(best.map(|(physical, _)| physical))
    }

    /// Check every row before committing any, so a failed reservation is atomic.
    pub fn reserve(
        &mut self,
        physical: usize,
        candidate: &Occupancy,
    ) -> Result<bool, AllocationError> {
        if self.packing_cost(physical, candidate)?.is_none() {
            return Ok(false);
        }
        for (used, &want) in self.occupied[physical].rows.iter_mut().zip(&candidate.rows) {
            *used |= want;
        }
        Ok(true)
    }

    /// Install a register fixed by the source interface. Such a register need
    /// not be in the user's free `.init_*` pool, but still must not overlap.
    pub fn reserve_fixed(
        &mut self,
        physical: usize,
        candidate: &Occupancy,
    ) -> Result<bool, AllocationError> {
        self.validate(candidate)?;
        let occupied = self
            .occupied
            .get_mut(physical)
            .ok_or(AllocationError::InvalidPhysical(physical))?;
        if occupied
            .rows
            .iter()
            .zip(&candidate.rows)
            .any(|(&a, &b)| a & b != 0)
        {
            return Ok(false);
        }
        for (used, &want) in occupied.rows.iter_mut().zip(&candidate.rows) {
            *used |= want;
        }
        Ok(true)
    }

    /// Port the recovered local priority policy. Input order is discovery order;
    /// equal lifetimes are processed in reverse discovery order, as in the
    /// original prepended pending list. The entire allocation rolls back on error.
    pub fn allocate(
        &mut self,
        registers: &[VirtualRegister],
    ) -> Result<Vec<(usize, usize)>, AllocationError> {
        let mut pending: Vec<_> = registers.iter().enumerate().collect();
        let mut seen = std::collections::BTreeSet::new();
        for register in registers {
            self.validate(&register.occupancy)?;
            if !seen.insert(register.id) {
                return Err(AllocationError::DuplicateVirtual(register.id));
            }
        }
        pending.sort_by_key(|(discovery, register)| {
            (
                std::cmp::Reverse(register.occupancy.occupied_rows()),
                std::cmp::Reverse(*discovery),
            )
        });
        let mut trial = self.clone();
        let mut assignments = Vec::with_capacity(registers.len());
        for register in registers
            .iter()
            .filter(|register| register.forced.is_some())
        {
            let physical = register.forced.expect("filtered forced mapping");
            if !trial.reserve_fixed(physical, &register.occupancy)? {
                return Err(AllocationError::ForcedConflict {
                    virtual_id: register.id,
                    physical,
                });
            }
            assignments.push((register.id, physical));
        }
        for (_, register) in pending
            .into_iter()
            .filter(|(_, register)| register.forced.is_none())
        {
            let physical = trial
                .best_fit(&register.occupancy)?
                .ok_or(AllocationError::NoFit {
                    virtual_id: register.id,
                })?;
            trial.reserve(physical, &register.occupancy)?;
            assignments.push((register.id, physical));
        }
        *self = trial;
        Ok(assignments)
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(rows: usize, range: Range<usize>, mask: u8) -> Occupancy {
        let mut result = Occupancy::new(rows);
        result.add(range, mask, false).unwrap();
        result
    }

    #[test]
    fn half_open_lifetimes_reuse_a_register() {
        let mut alloc = Allocator::new(4, vec![15], 4).unwrap();
        assert!(alloc.reserve(0, &raster(4, 0..2, 15)).unwrap());
        assert!(alloc.reserve(0, &raster(4, 2..4, 15)).unwrap());
    }

    #[test]
    fn components_pack_but_overlap_rejects() {
        let mut alloc = Allocator::new(4, vec![15], 4).unwrap();
        assert!(alloc.reserve(0, &raster(4, 0..4, 1)).unwrap());
        assert!(alloc.reserve(0, &raster(4, 0..4, 2)).unwrap());
        assert!(!alloc.reserve(0, &raster(4, 1..2, 1)).unwrap());
    }

    #[test]
    fn partial_user_permission_is_enforced() {
        let alloc = Allocator::new(4, vec![1, 3], 2).unwrap();
        assert_eq!(alloc.best_fit(&raster(2, 0..2, 2)).unwrap(), Some(1));
        assert_eq!(alloc.best_fit(&raster(2, 0..2, 4)).unwrap(), None);
    }

    #[test]
    fn packing_prefers_existing_complementary_components() {
        let mut alloc = Allocator::new(4, vec![15; 2], 2).unwrap();
        alloc.reserve(1, &raster(2, 0..2, 7)).unwrap();
        assert_eq!(alloc.best_fit(&raster(2, 0..2, 8)).unwrap(), Some(1));
    }

    #[test]
    fn physical_ties_keep_first_id() {
        let alloc = Allocator::new(4, vec![15; 3], 2).unwrap();
        assert_eq!(alloc.best_fit(&raster(2, 0..2, 1)).unwrap(), Some(0));
    }

    #[test]
    fn modulo_intervals_wrap() {
        let mut value = Occupancy::new(4);
        value.add(3..6, 1, true).unwrap();
        assert_eq!(value.rows(), &[1, 1, 0, 1]);
        value.add(0..10, 2, true).unwrap();
        assert_eq!(value.rows(), &[3, 3, 2, 3]);
    }

    #[test]
    fn extra_lock_collides_but_does_not_need_user_component_permission() {
        let mut alloc = Allocator::new(4, vec![0], 1).unwrap();
        assert_eq!(
            alloc.packing_cost(0, &raster(1, 0..1, EXCLUSIVE)).unwrap(),
            Some(4)
        );
        assert!(alloc.reserve(0, &raster(1, 0..1, EXCLUSIVE)).unwrap());
        assert!(!alloc.reserve(0, &raster(1, 0..1, EXCLUSIVE)).unwrap());
    }

    #[test]
    fn longest_lifetime_then_reverse_discovery_priority() {
        let registers = vec![
            VirtualRegister {
                id: 1,
                occupancy: raster(4, 0..2, 15),
                forced: None,
            },
            VirtualRegister {
                id: 2,
                occupancy: raster(4, 0..4, 15),
                forced: None,
            },
            VirtualRegister {
                id: 3,
                occupancy: raster(4, 0..2, 15),
                forced: None,
            },
        ];
        let mut alloc = Allocator::new(4, vec![15; 3], 4).unwrap();
        assert_eq!(
            alloc.allocate(&registers).unwrap(),
            vec![(2, 0), (3, 1), (1, 2)]
        );
    }

    #[test]
    fn forced_reservations_precede_unforced_values() {
        let registers = vec![
            VirtualRegister {
                id: 1,
                occupancy: raster(2, 0..2, 15),
                forced: None,
            },
            VirtualRegister {
                id: 2,
                occupancy: raster(2, 0..2, 15),
                forced: Some(0),
            },
        ];
        let mut alloc = Allocator::new(4, vec![15; 2], 2).unwrap();
        assert_eq!(alloc.allocate(&registers).unwrap(), vec![(2, 0), (1, 1)]);
    }

    #[test]
    fn failed_allocation_rolls_back_all_rows() {
        let registers = vec![
            VirtualRegister {
                id: 1,
                occupancy: raster(2, 0..2, 15),
                forced: Some(0),
            },
            VirtualRegister {
                id: 2,
                occupancy: raster(2, 0..2, 15),
                forced: None,
            },
        ];
        let mut alloc = Allocator::new(4, vec![15], 2).unwrap();
        assert!(matches!(
            alloc.allocate(&registers),
            Err(AllocationError::NoFit { .. })
        ));
        assert_eq!(alloc.occupancy(0).unwrap().rows(), &[0, 0]);
    }

    #[test]
    fn malformed_tables_return_errors() {
        assert!(Allocator::new(0, vec![], 1).is_err());
        assert!(Allocator::new(4, vec![16], 1).is_err());
        let alloc = Allocator::new(4, vec![15], 2).unwrap();
        assert!(alloc.packing_cost(4, &raster(2, 0..1, 1)).is_err());
        assert!(alloc.best_fit(&Occupancy::new(3)).is_err());
        assert!(alloc.best_fit(&raster(2, 0..1, 16)).is_err());
        assert!(Occupancy::new(0).add(0..1, 1, true).is_err());
        assert!(Occupancy::new(2).add(0..3, 1, false).is_err());
    }
}
