//! VI immediate coalescing, distinct from stage/epoch displacement repair.
//!
//! The original normalizes IADDI, then pushes a nonzero IADDIU/ISUBIU
//! constant into every compatible user and zeroes the producer. User arithmetic
//! and ranges are inferred from vcl 1.4beta7.
//! Callers supply complete operand-level flow, including escaping observations.
//! Equal register names, adjacent rows, or clone biases do not prove eligibility.

use crate::isa::instruction::id;
use std::collections::BTreeSet;
use std::fmt;

pub type OperationId = usize;

/// Minimal view of an operation. `None` means symbolic/unavailable immediate,
/// which the original coalescer skips rather than inventing an expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operation {
    pub id: OperationId,
    pub instruction: u16,
    pub immediate: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Definition {
    Input,
    Operation(OperationId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consumer {
    pub operation: Operation,
    /// The actual all-operand index of this producer's use. Must match the
    /// original instruction-specific input index, not merely any VI operand.
    pub read_operand: u8,
    pub reaching: Vec<Definition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum User {
    Instruction(Consumer),
    /// Observability at an EXIT or other boundary cannot be erased by changing
    /// the producer's value. Include this even without a normal instruction use.
    EscapingOutput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub before: Operation,
    pub after: Operation,
    pub immediate_operand: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub producer: Rewrite,
    pub consumers: Vec<Rewrite>,
    pub signed_delta: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InductionError {
    MissingReachingDefinitions(OperationId),
    DuplicateConsumer(OperationId),
    MissingOperation(OperationId),
    DuplicateOperation(OperationId),
    StaleOperation(OperationId),
}

impl fmt::Display for InductionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid VI immediate coalescing graph/plan: {self:?}")
    }
}

impl std::error::Error for InductionError {}

/// Original initial sweep: numeric IADDI (109) becomes unsigned add (110) or
/// subtract (112). The immediate retains its 32-bit bit pattern, including the
/// unencodable signed-minimum corner, which subsequent range checks skip.
/// This sweep applies even when coalescing is blocked by an ordinary user.
pub fn normalize_iaddi(operation: Operation) -> Operation {
    if operation.instruction != 109 {
        return operation;
    }
    let Some(value) = operation.immediate else {
        return operation;
    };
    Operation {
        instruction: if value < 0 { 112 } else { 110 },
        immediate: Some(if value < 0 {
            value.wrapping_neg()
        } else {
            value
        }),
        ..operation
    }
}

/// Return an atomic rewrite if *every* user permits pushing the producer's
/// constant. Rejection is `Ok(None)`, not a partially modified graph.
///
/// Normalize all numeric IADDI operations before constructing these views.
/// Rebuild users after transformations that alter flow. Repeat the original
/// instruction-order scan until no plan remains. This function itself neither
/// rebuilds flow nor prunes the zero-immediate copy left at the producer site.
pub fn plan(producer: Operation, users: &[User]) -> Result<Option<Plan>, InductionError> {
    let Some(magnitude) = producer.immediate else {
        return Ok(None);
    };
    if !matches!(producer.instruction, 110 | 112) || !(1..32768).contains(&magnitude) {
        return Ok(None);
    }
    let signed_delta = if producer.instruction == 112 {
        -magnitude
    } else {
        magnitude
    };
    let mut consumers = Vec::new();
    let mut seen = BTreeSet::new();
    for user in users {
        let User::Instruction(consumer) = user else {
            return Ok(None);
        };
        let operation = consumer.operation;
        if operation.id == producer.id {
            return Ok(None);
        }
        if !seen.insert(operation.id) {
            return Err(InductionError::DuplicateConsumer(operation.id));
        }
        let (read_operand, immediate_operand, subtract, memory) = match operation.instruction {
            id::IADDIU => (1, 2, false, false),
            id::ISUBIU => (1, 2, true, false),
            id::LQ | id::ILW | id::SQ | id::ISW => (1, 1, false, true),
            _ => return Ok(None),
        };
        if consumer.read_operand != read_operand {
            return Ok(None);
        }
        let Some(old) = operation.immediate else {
            return Ok(None);
        };
        if consumer.reaching.is_empty() {
            return Err(InductionError::MissingReachingDefinitions(operation.id));
        }
        if consumer
            .reaching
            .iter()
            .any(|definition| *definition != Definition::Operation(producer.id))
        {
            return Ok(None);
        }
        let result = old.wrapping_add(if subtract {
            -signed_delta
        } else {
            signed_delta
        });
        let (instruction, immediate) = if memory {
            if !(-1024..1024).contains(&result) {
                return Ok(None);
            }
            (operation.instruction, result)
        } else {
            if result.unsigned_abs() >= 32768 {
                return Ok(None);
            }
            let instruction = if result < 0 {
                if operation.instruction == 110 {
                    112
                } else {
                    110
                }
            } else {
                operation.instruction
            };
            (instruction, result.abs())
        };
        consumers.push(Rewrite {
            before: operation,
            after: Operation {
                instruction,
                immediate: Some(immediate),
                ..operation
            },
            immediate_operand,
        });
    }
    Ok(Some(Plan {
        producer: Rewrite {
            before: producer,
            after: Operation {
                immediate: Some(0),
                ..producer
            },
            immediate_operand: 2,
        },
        consumers,
        signed_delta,
    }))
}

/// Apply a plan to the same operation snapshots used to prove it. Validate all
/// identities and snapshots before mutating anything. Real compiler adapters
/// can instead use each `Rewrite` to change their own typed instruction IR.
pub fn apply(plan: &Plan, operations: &mut [Operation]) -> Result<(), InductionError> {
    let rewrites: Vec<_> = std::iter::once(&plan.producer)
        .chain(&plan.consumers)
        .collect();
    let mut positions = Vec::new();
    for rewrite in &rewrites {
        let mut found = operations
            .iter()
            .enumerate()
            .filter(|(_, op)| op.id == rewrite.before.id);
        let (index, operation) = found
            .next()
            .ok_or(InductionError::MissingOperation(rewrite.before.id))?;
        if found.next().is_some() {
            return Err(InductionError::DuplicateOperation(operation.id));
        }
        if *operation != rewrite.before {
            return Err(InductionError::StaleOperation(operation.id));
        }
        positions.push(index);
    }
    for (rewrite, index) in rewrites.into_iter().zip(positions) {
        operations[index] = rewrite.after;
    }
    Ok(())
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: usize, instruction: u16, immediate: i32) -> Operation {
        Operation {
            id,
            instruction,
            immediate: Some(immediate),
        }
    }

    fn user(operation: Operation, producer: usize) -> User {
        User::Instruction(Consumer {
            operation,
            read_operand: 1,
            reaching: vec![Definition::Operation(producer)],
        })
    }

    #[test]
    fn known_minimum_four_prologue_combines_two_decrements_without_stage_arithmetic() {
        let mut operations = [op(0, 112, 1), op(1, 112, 1)];
        let plan = plan(operations[0], &[user(operations[1], 0)])
            .unwrap()
            .unwrap();
        assert_eq!(plan.signed_delta, -1);
        apply(&plan, &mut operations).unwrap();
        assert_eq!(operations, [op(0, 112, 0), op(1, 112, 2)]);
    }

    #[test]
    fn early_exit_conditional_and_escaping_output_block_push() {
        let producer = op(0, 112, 1);
        assert!(
            plan(producer, &[user(op(1, 112, 1), 0), user(op(2, 125, 0), 0)])
                .unwrap()
                .is_none()
        );
        assert!(plan(producer, &[User::EscapingOutput]).unwrap().is_none());
    }

    #[test]
    fn signed_delta_preserves_or_flips_consumer_add_subtract() {
        let add = plan(op(0, 110, 5), &[user(op(1, 112, 2), 0)])
            .unwrap()
            .unwrap();
        assert_eq!(add.consumers[0].after, op(1, 110, 3));
        let sub = plan(op(0, 112, 5), &[user(op(1, 110, 2), 0)])
            .unwrap()
            .unwrap();
        assert_eq!(sub.consumers[0].after, op(1, 112, 3));
        let zero = plan(op(0, 110, 2), &[user(op(1, 112, 2), 0)])
            .unwrap()
            .unwrap();
        assert_eq!(zero.consumers[0].after, op(1, 112, 0));
    }

    #[test]
    fn direct_memory_consumers_take_signed_offset_and_memory_operand_index() {
        for opcode in [155, 156, 160, 161] {
            let update = plan(op(0, 110, 3), &[user(op(1, opcode, 5), 0)])
                .unwrap()
                .unwrap();
            assert_eq!(update.consumers[0].after.immediate, Some(8));
            assert_eq!(update.consumers[0].immediate_operand, 1);
            let negative = plan(op(0, 112, 3), &[user(op(1, opcode, 1), 0)])
                .unwrap()
                .unwrap();
            assert_eq!(negative.consumers[0].after.immediate, Some(-2));
        }
    }

    #[test]
    fn representability_is_unsigned_fifteen_or_signed_eleven_bits() {
        assert!(
            plan(op(0, 110, 32767), &[user(op(1, 110, 1), 0)])
                .unwrap()
                .is_none()
        );
        assert!(
            plan(op(0, 112, 32767), &[user(op(1, 110, 0), 0)])
                .unwrap()
                .is_some()
        );
        assert!(
            plan(op(0, 110, 3), &[user(op(1, 155, 1022), 0)])
                .unwrap()
                .is_none()
        );
        assert!(
            plan(op(0, 112, 1), &[user(op(1, 155, -1023), 0)])
                .unwrap()
                .is_some()
        );
        assert!(
            plan(op(0, 112, 1), &[user(op(1, 155, -1024), 0)])
                .unwrap()
                .is_none()
        );
        for magnitude in [-1, 0, 32768] {
            assert!(plan(op(0, 110, magnitude), &[]).unwrap().is_none());
        }
    }

    #[test]
    fn all_users_must_use_correct_input_and_have_only_this_reaching_producer() {
        let producer = op(0, 110, 3);
        let mut consumer = Consumer {
            operation: op(1, 110, 2),
            read_operand: 1,
            reaching: vec![Definition::Operation(0), Definition::Operation(2)],
        };
        assert!(
            plan(producer, &[User::Instruction(consumer.clone())])
                .unwrap()
                .is_none()
        );
        consumer.reaching = vec![Definition::Input];
        assert!(
            plan(producer, &[User::Instruction(consumer.clone())])
                .unwrap()
                .is_none()
        );
        consumer.reaching = vec![Definition::Operation(0)];
        consumer.read_operand = 0;
        assert!(
            plan(producer, &[User::Instruction(consumer.clone())])
                .unwrap()
                .is_none()
        );
        consumer.read_operand = 1;
        consumer.reaching.clear();
        assert_eq!(
            plan(producer, &[User::Instruction(consumer)]),
            Err(InductionError::MissingReachingDefinitions(1))
        );
        assert!(plan(producer, &[user(producer, 0)]).unwrap().is_none());
    }

    #[test]
    fn symbolic_immediates_and_autoincrement_consumers_are_not_guessed() {
        let symbolic = Operation {
            id: 0,
            instruction: 110,
            immediate: None,
        };
        assert!(plan(symbolic, &[user(op(1, 110, 2), 0)]).unwrap().is_none());
        assert!(
            plan(op(0, 110, 2), &[user(Operation { id: 1, ..symbolic }, 0)])
                .unwrap()
                .is_none()
        );
        assert!(
            plan(op(0, 110, 2), &[user(op(1, 158, 0), 0)])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn iaddi_normalization_is_separate_even_when_no_push_is_possible() {
        assert_eq!(normalize_iaddi(op(0, 109, -3)), op(0, 112, 3));
        assert_eq!(normalize_iaddi(op(0, 109, 3)), op(0, 110, 3));
        assert_eq!(normalize_iaddi(op(0, 109, i32::MIN)), op(0, 112, i32::MIN));
        let symbolic = Operation {
            id: 0,
            instruction: 109,
            immediate: None,
        };
        assert_eq!(normalize_iaddi(symbolic), symbolic);
    }

    #[test]
    fn apply_is_atomic_and_rejects_missing_duplicate_or_stale_views() {
        let producer = op(0, 110, 5);
        let consumer = op(1, 112, 2);
        let plan = plan(producer, &[user(consumer, 0)]).unwrap().unwrap();
        let mut operations = [producer, op(1, 112, 3)];
        let before = operations;
        assert_eq!(
            apply(&plan, &mut operations),
            Err(InductionError::StaleOperation(1))
        );
        assert_eq!(operations, before);
        assert_eq!(
            apply(&plan, &mut [producer]),
            Err(InductionError::MissingOperation(1))
        );
        assert_eq!(
            apply(&plan, &mut [producer, producer, consumer]),
            Err(InductionError::DuplicateOperation(0))
        );
    }

    #[test]
    fn producer_can_push_into_multiple_independent_users_atomically() {
        let producer = op(0, 110, 3);
        let plan = plan(producer, &[user(op(1, 155, 5), 0), user(op(2, 112, 2), 0)])
            .unwrap()
            .unwrap();
        assert_eq!(plan.consumers[0].after, op(1, 155, 8));
        assert_eq!(plan.consumers[1].after, op(2, 110, 1));
        assert_eq!(plan.producer.after.immediate, Some(0));
    }
}
