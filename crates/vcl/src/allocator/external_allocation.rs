//! External register assignment before local scheduling.
//!
//! The original's generic allocator walks stream references in list order,
//! excludes conflicts across every stream, and selects the first
//! wholly clear physical exclusion byte. This is a separate
//! policy from the local time-raster packing allocator.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Masks {
    pub external_input: u8,
    pub external_output: u8,
    pub input: u8,
    pub output: u8,
    pub through: u8,
}

impl Masks {
    pub const fn is_external(self) -> bool {
        self.external_input | self.external_output | self.through != 0
    }

    fn activity(self) -> u8 {
        self.input | self.output | if self.output != 0 { 0x80 } else { 0 }
    }

    fn valid(self, full: u8) -> bool {
        (self.external_input | self.external_output | self.input | self.output | self.through)
            & !full
            == 0
            && self.external_input & !self.input == 0
            && self.external_output & !self.output == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentRange {
    pub start: i32,
    pub end: i32,
    /// The component passes through without a local read or write. Two such
    /// references alone do not conflict in this stream; other streams decide.
    pub pure_through: bool,
}

impl ComponentRange {
    pub const ABSENT: Self = Self {
        start: -4,
        end: -3,
        pure_through: false,
    };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComponentTimes {
    pub first_read: Option<i32>,
    pub last_read: Option<i32>,
    pub first_write: Option<i32>,
    pub last_write: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub register: usize,
    pub masks: Masks,
    /// One range per pool component in selected mode; unused in UTA mode.
    pub ranges: Vec<ComponentRange>,
    /// Dispatch rows containing any write to this reference. The same-row
    /// exclusion is whole-register even for disjoint written components.
    pub write_rows: BTreeSet<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Original UTA/synthetic table modes 1 and 4.
    Unscheduled,
    /// Original selected VSM table mode 3, including fixed raw layouts.
    Selected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub mode: Mode,
    pub references: Vec<Reference>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalAllocationError {
    InvalidComponentCount(u8),
    TooManyPhysicalRegisters(usize),
    InvalidMasks,
    InvalidTimes {
        component: u8,
    },
    InvalidPhysical {
        register: usize,
        physical: usize,
    },
    DuplicateReference {
        table: usize,
        register: usize,
    },
    InvalidRangeCount {
        register: usize,
        expected: usize,
        actual: usize,
    },
    InvalidRange {
        register: usize,
        component: usize,
    },
    NoFit {
        register: usize,
    },
}

impl fmt::Display for ExternalAllocationError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "external register assignment failed: {self:?}")
    }
}
impl std::error::Error for ExternalAllocationError {}

/// Reconstruct the original's component interval from selected dispatch-row
/// statistics, not execution-clock latency. Last-read endpoints are exclusive;
/// last writes reserve their own row. `30000` denotes an external end.
pub fn component_range(
    masks: Masks,
    component: u8,
    times: ComponentTimes,
) -> Result<ComponentRange, ExternalAllocationError> {
    if !masks.valid(0x7f) {
        return Err(ExternalAllocationError::InvalidMasks);
    }
    if component >= 7 {
        return Err(ExternalAllocationError::InvalidTimes { component });
    }
    let bit = 1 << component;
    let input = masks.input & bit != 0;
    let output = masks.output & bit != 0;
    let ordered = |first: Option<i32>, last: Option<i32>| matches!((first, last), (Some(first), Some(last)) if first >= 0 && first <= last && last < 30000);
    if (input && !ordered(times.first_read, times.last_read))
        || (output && !ordered(times.first_write, times.last_write))
    {
        return Err(ExternalAllocationError::InvalidTimes { component });
    }
    if masks.through & !(masks.input | masks.output) & bit != 0 {
        return Ok(ComponentRange {
            start: -1,
            end: 30000,
            pure_through: true,
        });
    }
    let mut range = ComponentRange::ABSENT;
    if masks.external_input & bit != 0 {
        range.start = -1;
    } else {
        if input && times.first_read.unwrap() <= times.first_write.unwrap_or(-1) {
            return Err(ExternalAllocationError::InvalidTimes { component });
        }
        if output {
            range.start = times.first_write.unwrap();
        }
    }
    if (masks.through | masks.external_output) & bit != 0 {
        range.end = 30000;
    } else {
        if output {
            range.end = times.last_write.unwrap() + 1;
        }
        if input {
            range.end = range.end.max(times.last_read.unwrap());
        }
    }
    if range.end <= range.start {
        return Err(ExternalAllocationError::InvalidTimes { component });
    }
    Ok(range)
}

fn conflict(mode: Mode, target: &Reference, other: &Reference) -> u8 {
    match mode {
        Mode::Unscheduled => {
            let a = target.masks.activity();
            let b = other.masks.activity();
            (a & b) | (a & other.masks.through) | (b & target.masks.through)
        }
        Mode::Selected => {
            let mut result = 0;
            for (component, (a, b)) in target.ranges.iter().zip(&other.ranges).enumerate() {
                if !(a.pure_through && b.pure_through)
                    && (a.end >= 0 || b.end >= 0)
                    && a.start < b.end
                    && b.start < a.end
                {
                    result |= 1 << component;
                }
            }
            if target.masks.output != 0
                && other.masks.output != 0
                && target
                    .write_rows
                    .intersection(&other.write_rows)
                    .next()
                    .is_some()
            {
                result |= 0x80;
            }
            result
        }
    }
}

/// Assign external identities in stream/reference discovery order. Existing
/// bindings include forced physical references even when that physical register
/// is unavailable for new choices. The initial map is never mutated on failure.
pub fn assign(
    components: u8,
    available: &[bool],
    tables: &[Table],
    initial: &BTreeMap<usize, usize>,
) -> Result<BTreeMap<usize, usize>, ExternalAllocationError> {
    if components == 0 || components > 7 {
        return Err(ExternalAllocationError::InvalidComponentCount(components));
    }
    if available.len() > 32 {
        return Err(ExternalAllocationError::TooManyPhysicalRegisters(
            available.len(),
        ));
    }

    let full = (1 << components) - 1;
    for (&register, &physical) in initial {
        if physical >= available.len() {
            return Err(ExternalAllocationError::InvalidPhysical { register, physical });
        }
    }

    for (table_id, table) in tables.iter().enumerate() {
        let mut seen = BTreeSet::new();
        for reference in &table.references {
            if !seen.insert(reference.register) {
                return Err(ExternalAllocationError::DuplicateReference {
                    table: table_id,
                    register: reference.register,
                });
            }
            if !reference.masks.valid(full) {
                return Err(ExternalAllocationError::InvalidMasks);
            }
            if table.mode == Mode::Selected {
                if reference.ranges.len() != usize::from(components) {
                    return Err(ExternalAllocationError::InvalidRangeCount {
                        register: reference.register,
                        expected: usize::from(components),
                        actual: reference.ranges.len(),
                    });
                }
                for (component, range) in reference.ranges.iter().enumerate() {
                    if range.end <= range.start
                        || range.start < -4
                        || range.end > 30000
                        || (range.end < 0 && *range != ComponentRange::ABSENT)
                        || (range.pure_through && (range.start != -1 || range.end != 30000))
                    {
                        return Err(ExternalAllocationError::InvalidRange {
                            register: reference.register,
                            component,
                        });
                    }
                }
            }
        }
    }

    let mut assigned = initial.clone();
    for table in tables {
        for target in &table.references {
            if !target.masks.is_external() || assigned.contains_key(&target.register) {
                continue;
            }
            // Any bit blocks the entire candidate; the external chooser does
            // not apply the local allocator's component packing score.
            let mut excluded: Vec<_> = available
                .iter()
                .map(|&enabled| if enabled { 0 } else { full })
                .collect();
            for stream in tables {
                let Some(target_view) = stream
                    .references
                    .iter()
                    .find(|view| view.register == target.register)
                else {
                    continue;
                };
                // Original flow requires every matching target reference to
                // be external. A local view alone cannot supply this contract.
                if !target_view.masks.is_external() {
                    return Err(ExternalAllocationError::InvalidMasks);
                }
                for other in &stream.references {
                    if other.register == target.register {
                        continue;
                    }
                    if let Some(&physical) = assigned.get(&other.register) {
                        excluded[physical] |= conflict(stream.mode, target_view, other);
                    }
                }
            }
            let physical = excluded.iter().position(|&mask| mask == 0).ok_or(
                ExternalAllocationError::NoFit {
                    register: target.register,
                },
            )?;
            assigned.insert(target.register, physical);
        }
    }
    Ok(assigned)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn view(register: usize, input: u8, output: u8, external: bool) -> Reference {
        Reference {
            register,
            masks: Masks {
                input,
                output,
                external_input: if external { input } else { 0 },
                external_output: if external { output } else { 0 },
                through: 0,
            },
            ranges: Vec::new(),
            write_rows: BTreeSet::new(),
        }
    }

    fn uta(references: Vec<Reference>) -> Table {
        Table {
            mode: Mode::Unscheduled,
            references,
        }
    }

    fn available() -> Vec<bool> {
        vec![false, true, true, true]
    }

    fn range(start: i32, end: i32) -> ComponentRange {
        ComponentRange {
            start,
            end,
            pure_through: false,
        }
    }

    #[test]
    fn external_output_cannot_reuse_its_forced_last_read_input() {
        let table = uta(vec![view(10, 0, 1, true), view(20, 1, 0, false)]);
        let result = assign(1, &available(), &[table], &BTreeMap::from([(20, 1)])).unwrap();
        assert_eq!(result[&10], 2);
    }

    #[test]
    fn forced_flag_input_excludes_another_candidate_before_branch_scheduling() {
        let table = uta(vec![
            view(10, 0, 1, true),
            view(20, 1, 0, false),
            view(30, 1, 0, false),
        ]);
        let result = assign(
            1,
            &available(),
            &[table],
            &BTreeMap::from([(20, 1), (30, 2)]),
        )
        .unwrap();
        assert_eq!(result[&10], 3);
    }

    #[test]
    fn disjoint_outputs_still_exclude_a_whole_physical_register() {
        let table = uta(vec![view(10, 0, 1, true), view(20, 0, 2, false)]);
        let result = assign(4, &available(), &[table], &BTreeMap::from([(20, 1)])).unwrap();
        assert_eq!(result[&10], 2);
    }

    #[test]
    fn disjoint_input_only_components_can_share() {
        let table = uta(vec![view(10, 1, 0, true), view(20, 2, 0, false)]);
        let result = assign(4, &available(), &[table], &BTreeMap::from([(20, 1)])).unwrap();
        assert_eq!(result[&10], 1);
    }

    #[test]
    fn reference_discovery_order_selects_the_first_clear_register() {
        let first = assign(
            1,
            &available(),
            &[uta(vec![view(10, 0, 1, true), view(20, 0, 1, true)])],
            &BTreeMap::new(),
        )
        .unwrap();
        let second = assign(
            1,
            &available(),
            &[uta(vec![view(20, 0, 1, true), view(10, 0, 1, true)])],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!((first[&10], first[&20]), (1, 2));
        assert_eq!((second[&10], second[&20]), (2, 1));
    }

    #[test]
    fn separately_referenced_values_reuse_physical_locations() {
        let tables = [
            uta(vec![view(10, 0, 1, true)]),
            uta(vec![view(20, 0, 1, true)]),
        ];
        let result = assign(1, &available(), &tables, &BTreeMap::new()).unwrap();
        assert_eq!((result[&10], result[&20]), (1, 1));
    }

    #[test]
    fn through_values_exclude_local_activity_in_an_intervening_table() {
        let mut carried = view(10, 0, 0, false);
        carried.masks.through = 1;
        let tables = [
            uta(vec![view(10, 0, 1, true)]),
            uta(vec![view(20, 0, 1, true), carried]),
        ];
        let result = assign(1, &available(), &tables, &BTreeMap::new()).unwrap();
        assert_eq!((result[&10], result[&20]), (1, 2));
    }

    #[test]
    fn all_streams_contribute_even_before_the_target_is_visited_there() {
        let tables = [
            uta(vec![view(10, 0, 1, true)]),
            uta(vec![view(10, 1, 0, true), view(20, 1, 0, false)]),
        ];
        let result = assign(1, &available(), &tables, &BTreeMap::from([(20, 1)])).unwrap();
        assert_eq!(result[&10], 2);
    }

    #[test]
    fn disabled_choices_exhaustion_and_forced_bindings_remain_separate() {
        let tables = [uta(vec![view(10, 0, 1, true)])];
        let forced = BTreeMap::from([(20, 0)]);
        assert_eq!(
            assign(1, &[false, false, true], &tables, &forced).unwrap()[&10],
            2
        );
        assert_eq!(
            assign(1, &[false], &tables, &forced),
            Err(ExternalAllocationError::NoFit { register: 10 })
        );
        assert_eq!(forced, BTreeMap::from([(20, 0)]));
    }

    #[test]
    fn local_unassigned_virtuals_do_not_preempt_external_assignment() {
        let result = assign(
            1,
            &available(),
            &[uta(vec![view(20, 1, 1, false), view(10, 0, 1, true)])],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(result[&10], 1);
        assert!(!result.contains_key(&20));
    }

    #[test]
    fn selected_read_endpoint_is_exclusive() {
        let mut target = view(10, 0, 1, true);
        target.ranges = vec![range(1, 30000)];
        target.write_rows.insert(1);
        let mut source = view(20, 1, 0, false);
        source.ranges = vec![range(-1, 1)];
        let result = assign(
            1,
            &available(),
            &[Table {
                mode: Mode::Selected,
                references: vec![target, source],
            }],
            &BTreeMap::from([(20, 1)]),
        )
        .unwrap();
        assert_eq!(result[&10], 1);
    }

    #[test]
    fn selected_same_pair_writes_conflict_even_with_disjoint_components() {
        let mut target = view(10, 0, 1, true);
        target.ranges = vec![
            range(1, 30000),
            ComponentRange::ABSENT,
            ComponentRange::ABSENT,
            ComponentRange::ABSENT,
        ];
        target.write_rows.insert(1);
        let mut other = view(20, 0, 2, false);
        other.ranges = vec![
            ComponentRange::ABSENT,
            range(1, 2),
            ComponentRange::ABSENT,
            ComponentRange::ABSENT,
        ];
        other.write_rows.insert(1);
        let result = assign(
            4,
            &available(),
            &[Table {
                mode: Mode::Selected,
                references: vec![target, other],
            }],
            &BTreeMap::from([(20, 1)]),
        )
        .unwrap();
        assert_eq!(result[&10], 2);
    }

    #[test]
    fn pure_through_pairs_do_not_conflict_until_local_activity_occurs() {
        let mut a = view(10, 0, 0, false);
        a.masks.through = 1;
        a.ranges = vec![ComponentRange {
            start: -1,
            end: 30000,
            pure_through: true,
        }];
        let mut b = a.clone();
        b.register = 20;
        let table = Table {
            mode: Mode::Selected,
            references: vec![a.clone(), b.clone()],
        };
        assert_eq!(
            assign(1, &available(), &[table], &BTreeMap::from([(20, 1)])).unwrap()[&10],
            1
        );
        b.masks.input = 1;
        b.masks.external_input = 1;
        b.ranges[0].pure_through = false;
        let table = Table {
            mode: Mode::Selected,
            references: vec![a, b],
        };
        assert_eq!(
            assign(1, &available(), &[table], &BTreeMap::from([(20, 1)])).unwrap()[&10],
            2
        );
    }

    #[test]
    fn absent_negative_ranges_do_not_create_a_collision() {
        let mut a = view(10, 0, 1, true);
        a.ranges = vec![
            range(0, 30000),
            ComponentRange::ABSENT,
            ComponentRange::ABSENT,
            ComponentRange::ABSENT,
        ];
        let mut b = view(20, 2, 0, false);
        b.ranges = vec![
            ComponentRange::ABSENT,
            range(-1, 3),
            ComponentRange::ABSENT,
            ComponentRange::ABSENT,
        ];
        let result = assign(
            4,
            &available(),
            &[Table {
                mode: Mode::Selected,
                references: vec![a, b],
            }],
            &BTreeMap::from([(20, 1)]),
        )
        .unwrap();
        assert_eq!(result[&10], 1);
    }

    #[test]
    fn component_interval_reconstructs_selected_statistics() {
        assert_eq!(
            component_range(Masks::default(), 0, ComponentTimes::default()).unwrap(),
            ComponentRange::ABSENT
        );
        let input = Masks {
            external_input: 1,
            input: 1,
            ..Masks::default()
        };
        assert_eq!(
            component_range(
                input,
                0,
                ComponentTimes {
                    first_read: Some(0),
                    last_read: Some(4),
                    ..ComponentTimes::default()
                }
            )
            .unwrap(),
            range(-1, 4)
        );
        let output = Masks {
            external_output: 1,
            output: 1,
            ..Masks::default()
        };
        assert_eq!(
            component_range(
                output,
                0,
                ComponentTimes {
                    first_write: Some(2),
                    last_write: Some(3),
                    ..ComponentTimes::default()
                }
            )
            .unwrap(),
            range(2, 30000)
        );
        let local = Masks {
            input: 1,
            output: 1,
            ..Masks::default()
        };
        assert_eq!(
            component_range(
                local,
                0,
                ComponentTimes {
                    first_write: Some(1),
                    last_write: Some(3),
                    first_read: Some(2),
                    last_read: Some(6)
                }
            )
            .unwrap(),
            range(1, 6)
        );
        let through = Masks {
            through: 1,
            ..Masks::default()
        };
        assert_eq!(
            component_range(through, 0, ComponentTimes::default()).unwrap(),
            ComponentRange {
                start: -1,
                end: 30000,
                pure_through: true
            }
        );
    }

    #[test]
    fn malformed_masks_ranges_times_and_physical_ids_are_errors() {
        let invalid = view(10, 0, 0, true);
        let mut bad = invalid.clone();
        bad.masks.external_input = 1;
        assert_eq!(
            assign(1, &available(), &[uta(vec![bad])], &BTreeMap::new()),
            Err(ExternalAllocationError::InvalidMasks)
        );
        assert!(matches!(
            assign(
                1,
                &available(),
                &[uta(vec![invalid.clone(), invalid.clone()])],
                &BTreeMap::new()
            ),
            Err(ExternalAllocationError::DuplicateReference { .. })
        ));
        assert!(matches!(
            assign(
                1,
                &available(),
                &[Table {
                    mode: Mode::Selected,
                    references: vec![invalid]
                }],
                &BTreeMap::new()
            ),
            Err(ExternalAllocationError::InvalidRangeCount { .. })
        ));
        assert!(matches!(
            assign(1, &available(), &[], &BTreeMap::from([(10, 99)])),
            Err(ExternalAllocationError::InvalidPhysical { .. })
        ));
        assert!(
            component_range(
                Masks {
                    input: 1,
                    ..Masks::default()
                },
                0,
                ComponentTimes::default()
            )
            .is_err()
        );
        assert!(
            component_range(
                Masks {
                    input: 1,
                    output: 1,
                    ..Masks::default()
                },
                0,
                ComponentTimes {
                    first_read: Some(1),
                    last_read: Some(1),
                    first_write: Some(1),
                    last_write: Some(1)
                }
            )
            .is_err()
        );
    }
}
