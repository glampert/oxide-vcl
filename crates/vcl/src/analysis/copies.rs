//! Copy recognition inferred from vcl 1.4beta7.
//!
//! A recognized instruction becomes an internal IMOVE/VMOVE, not a deleted
//! operation. The shipped move-pruning pass is a successful no-op.
//! Callers prove pool-zero identity and whether secondary outputs are used.

use crate::isa::instruction::id;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Integer,
    Vector,
}

impl Kind {
    /// Resolve the original output MUID after a pipe has been selected.
    /// Integer moves only admit the lower pipe in scheduling.
    pub const fn resolved_descriptor(self, pipe: crate::scheduler::schedule::Pipe) -> u16 {
        match self {
            Self::Integer => id::LIMOVE,
            Self::Vector => match pipe {
                crate::scheduler::schedule::Pipe::Lower => id::LVMOVE,
                crate::scheduler::schedule::Pipe::Upper => id::UVMOVE,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub instruction: u16,
    pub mask: u8,
    pub third_is_pool_zero: bool,
    pub numeric_immediate: Option<i32>,
    /// MAC/status or another output beyond the ordinary destination is read
    /// or escapes. Copy normalization would erase that observation.
    pub secondary_outputs_live: bool,
}

pub fn detect(facts: Facts) -> Option<Kind> {
    use Kind::*;
    match facts.instruction {
        id::IADD | id::ISUB if facts.third_is_pool_zero => Some(Integer),
        id::IADDI | id::IADDIU | id::ISUBIU if facts.numeric_immediate == Some(0) => Some(Integer),
        id::MOVE => Some(Vector),
        id::ADD | id::SUB if facts.mask & 8 != 0 => None, // VF00.w is one, not zero.
        id::ADD | id::ADDX..=id::ADDZ | id::SUB | id::MULW
            if facts.third_is_pool_zero && !facts.secondary_outputs_live =>
        {
            Some(Vector)
        }
        _ => None,
    }
    .filter(|_| facts.mask != 0)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(instruction: u16) -> Facts {
        Facts {
            instruction,
            mask: 15,
            third_is_pool_zero: true,
            numeric_immediate: None,
            secondary_outputs_live: false,
        }
    }

    #[test]
    fn integer_zero_sources_and_numeric_immediates_have_distinct_rules() {
        for id in [108, 111] {
            assert_eq!(detect(facts(id)), Some(Kind::Integer));
            assert_eq!(
                detect(Facts {
                    third_is_pool_zero: false,
                    ..facts(id)
                }),
                None
            );
        }
        for id in [109, 110, 112] {
            assert_eq!(
                detect(Facts {
                    numeric_immediate: Some(0),
                    ..facts(id)
                }),
                Some(Kind::Integer)
            );
            assert_eq!(
                detect(Facts {
                    numeric_immediate: Some(1),
                    ..facts(id)
                }),
                None
            );
            assert_eq!(detect(facts(id)), None); // unresolved expression
        }
    }

    #[test]
    fn vector_zero_and_one_fields_do_not_generalize_to_other_arithmetic() {
        for id in [34, 48] {
            assert_eq!(detect(facts(id)), None);
            assert_eq!(
                detect(Facts {
                    mask: 7,
                    ..facts(id)
                }),
                Some(Kind::Vector)
            );
        }
        for id in [37, 38, 39, 68, 153] {
            assert_eq!(detect(facts(id)), Some(Kind::Vector));
        }
        for id in [40, 62, 64, 65, 66, 67, 69, 76, 154] {
            assert_eq!(detect(facts(id)), None);
        }
    }

    #[test]
    fn observed_flags_block_arithmetic_copies_and_empty_masks_do_not_create_moves() {
        for id in [34, 37, 38, 39, 48, 68] {
            assert_eq!(
                detect(Facts {
                    mask: 7,
                    secondary_outputs_live: true,
                    ..facts(id)
                }),
                None
            );
        }
        assert_eq!(
            detect(Facts {
                mask: 0,
                ..facts(153)
            }),
            None
        );
        assert_eq!(
            detect(Facts {
                secondary_outputs_live: true,
                ..facts(153)
            }),
            Some(Kind::Vector)
        );
    }
}
