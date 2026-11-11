//! Component-level output-to-input liveness inferred from vcl 1.4beta7.
//!
//! The original's transfer rule, propagation, physical
//! field narrowing and virtual status expansion are ported here. These are conservative compiler dependencies, not a proposed
//! mathematical simplifier. In particular, fixed XYZ operations retain all
//! their source components when any output is live.
//!
//! The original validates undefined reads before pruning. Narrowing a later
//! dead lane must not retroactively make an invalid original source valid.

use crate::isa::instruction::{Direction, Hook, InstructionKind, Mask, MaskMode, class};
use crate::parser::{Instruction, ParseMode};
use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedOperand {
    /// Index in the parsed instruction, including its implicit operands.
    /// Virtual status operands can expand into several members with one index.
    pub source_operand: usize,
    pub class: u8,
    pub direction: Direction,
    pub mask: u8,
    pub register: Option<String>,
    pub implicit: bool,
    pub latency: i16,
    pub blocking: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedInstruction {
    pub instruction: Instruction,
    pub operands: Vec<ExpandedOperand>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pruned {
    pub instruction: Instruction,
    pub expanded: Vec<ExpandedOperand>,
    /// Required read bits, indexed by expanded operand rather than source index.
    pub input_masks: Vec<u8>,
    /// Needed output bits, also indexed by expanded operand. Actual selected
    /// destination fields may be wider when another result keeps an operation.
    pub live_output_masks: Vec<u8>,
    /// Parsed virtual destination operands whose unused result can be sent to
    /// the register pool's zero register. Physical destinations stay physical.
    pub zero_outputs: Vec<usize>,
    pub retained: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UseEdge {
    pub producer: usize,
    pub output_operand: usize,
    pub consumer: usize,
    pub input_operand: usize,
    /// Same physical register components on the output and its reaching read.
    /// MR32/swizzle transfer occurs inside the consumer, before traversing edges.
    pub components: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveRoot {
    pub instruction: usize,
    pub output_operand: usize,
    pub components: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Liveness {
    pub live_output_masks: Vec<Vec<u8>>,
    pub required_input_masks: Vec<Vec<u8>>,
    pub retained: Vec<bool>,
    /// Final narrowed instructions/effective operands. Each entry also returns
    /// its source-index zero-destination candidates and expanded read masks.
    pub pruned: Vec<Pruned>,
    pub iterations: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    InvalidInstruction(usize),
    InvalidOperand {
        instruction: usize,
        operand: usize,
    },
    InvalidEdge {
        edge: usize,
        reason: &'static str,
    },
    Prune {
        instruction: usize,
        error: PruneError,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pruning graph failed: {self:?}")
    }
}

impl std::error::Error for GraphError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PruneError {
    MaskCount {
        expected: usize,
        actual: usize,
    },
    LiveInput {
        operand: usize,
    },
    OutputMask {
        operand: usize,
        requested: u8,
        available: u8,
    },
    InputMask {
        operand: usize,
        required: u8,
        available: u8,
    },
    ConflictingVectorOutputs,
    OutputExceedsInput {
        outputs: u8,
        inputs: u8,
    },
}

impl fmt::Display for PruneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "instruction pruning failed: {self:?}")
    }
}

impl std::error::Error for PruneError {}

fn rotate_left(mask: u8) -> u8 {
    (mask << 1) & 15 | (mask & 8) >> 3
}

fn rotate_right(mask: u8) -> u8 {
    (mask >> 1) & 7 | (mask & 1) << 3
}

fn lane_wise(operand_class: u8) -> bool {
    matches!(operand_class, class::VECTOR | class::ACC)
}

fn status_operand(
    source_operand: usize,
    class: u8,
    direction: Direction,
    register: &str,
    latency: i16,
    blocking: bool,
) -> ExpandedOperand {
    ExpandedOperand {
        source_operand,
        class,
        direction,
        mask: 1,
        register: Some(register.into()),
        implicit: true,
        latency,
        blocking,
    }
}

/// Expand the four virtual status classes as the original does and apply the
/// MR32 input-mask rotation. Parser indices remain available
/// for mapping effective operands back to source expressions and registers.
pub fn expand(instruction: &Instruction) -> ExpandedInstruction {
    let mut operands = Vec::new();
    for (index, source) in instruction.operands.iter().enumerate() {
        match source.class {
            class::STATUS_FROM_DIVIDE => {
                let latency = instruction
                    .metadata
                    .operands
                    .first()
                    .map_or(1, |o| i16::from(o.latency));
                operands.push(status_operand(
                    index,
                    class::STAT_DIV,
                    Direction::Write,
                    "STAT_DIV",
                    latency,
                    false,
                ));
                operands.push(status_operand(
                    index,
                    class::STAT_DIV_STICKY,
                    Direction::Read,
                    "STAT_DIV_S",
                    -1,
                    true,
                ));
            }
            class::STATUS_FROM_ALU => {
                operands.push(status_operand(
                    index,
                    class::STAT_ALU,
                    Direction::Write,
                    "STAT_ALU",
                    4,
                    false,
                ));
                operands.push(status_operand(
                    index,
                    class::STAT_ALU_STICKY,
                    Direction::Read,
                    "STAT_ALU_S",
                    -1,
                    true,
                ));
            }
            class::STATUS_TEST => {
                let mut bits = instruction
                    .operands
                    .iter()
                    .find_map(|operand| operand.immediate.as_deref())
                    .and_then(|source| crate::parser::expression::evaluate(source).ok())
                    .and_then(|value| value.number())
                    .map(|number| number.to_u32());
                if instruction.mnemonic.eq_ignore_ascii_case("fseq") {
                    bits = None;
                }
                if instruction.mnemonic.eq_ignore_ascii_case("fsor") {
                    bits = bits.map(|value| !value);
                }
                let bits = bits.unwrap_or(0xfff);
                let alu = bits & 0x00f != 0;
                let div = bits & 0x030 != 0;
                let sticky_alu = bits & 0xc00 != 0;
                let sticky_div = bits & 0x3c0 != 0;
                if alu || sticky_alu {
                    operands.push(status_operand(
                        index,
                        class::STAT_ALU,
                        Direction::Read,
                        "STAT_ALU",
                        -1,
                        false,
                    ));
                }
                if div || sticky_div {
                    operands.push(status_operand(
                        index,
                        class::STAT_DIV,
                        Direction::Read,
                        "STAT_DIV",
                        -1,
                        false,
                    ));
                }
                if sticky_alu {
                    operands.push(status_operand(
                        index,
                        class::STAT_ALU_STICKY,
                        Direction::Read,
                        "STAT_ALU_S",
                        -1,
                        true,
                    ));
                    operands.push(status_operand(
                        index,
                        class::STAT_ALU_STICKY,
                        Direction::Write,
                        "STAT_ALU_S",
                        1,
                        true,
                    ));
                }
                if sticky_div {
                    operands.push(status_operand(
                        index,
                        class::STAT_DIV_STICKY,
                        Direction::Read,
                        "STAT_DIV_S",
                        -1,
                        true,
                    ));
                    operands.push(status_operand(
                        index,
                        class::STAT_DIV_STICKY,
                        Direction::Write,
                        "STAT_DIV_S",
                        1,
                        true,
                    ));
                }
            }
            class::STATUS_SET => {
                operands.push(status_operand(
                    index,
                    class::STAT_ALU_STICKY,
                    Direction::Write,
                    "STAT_ALU_S",
                    1,
                    true,
                ));
                operands.push(status_operand(
                    index,
                    class::STAT_DIV_STICKY,
                    Direction::Write,
                    "STAT_DIV_S",
                    1,
                    true,
                ));
            }
            _ => {
                let spec = instruction.metadata.operands.get(index);
                // Memory field selectors belong to the transferred qword,
                // not to its scalar VI base. Keep the parsed mask for ILW/
                // ISW emission, but address dependencies always use bit 0.
                let mask = if class::is_address(source.class) {
                    Mask::X.0
                } else if instruction.metadata.hook == Hook::Mr32
                    && source.direction == Direction::Read
                    && lane_wise(source.class)
                {
                    rotate_left(instruction.mask.0)
                } else {
                    source.mask.0
                };
                operands.push(ExpandedOperand {
                    source_operand: index,
                    class: source.class,
                    direction: source.direction,
                    mask,
                    register: source.register.clone(),
                    implicit: source.implicit,
                    latency: spec.map_or(0, |spec| i16::from(spec.latency)),
                    blocking: spec.is_some_and(|spec| spec.uses_mask),
                });
            }
        }
    }
    ExpandedInstruction {
        instruction: instruction.clone(),
        operands,
    }
}

/// Whether the original's prune initialization can start with this instruction's inputs
/// unmarked. Stores, branches and structural synthetics are retained roots.
pub fn can_prune(instruction: &Instruction) -> bool {
    let info = instruction.metadata;
    info.order_flags == 0
        && !(info.kind == InstructionKind::Synthetic && info.subtype != 0)
        && !info.is_store()
        && !(info.kind == InstructionKind::Branch && info.subtype != 0)
}

fn normalize_outputs(operands: &[ExpandedOperand], live: &[u8]) -> Result<Vec<u8>, PruneError> {
    if operands.len() != live.len() {
        return Err(PruneError::MaskCount {
            expected: operands.len(),
            actual: live.len(),
        });
    }
    let mut output_masks = live.to_vec();
    for (index, (operand, live)) in operands.iter().zip(&mut output_masks).enumerate() {
        if *live == 0 {
            continue;
        }
        if operand.direction != Direction::Write {
            return Err(PruneError::LiveInput { operand: index });
        }
        if *live & !operand.mask != 0 {
            return Err(PruneError::OutputMask {
                operand: index,
                requested: *live,
                available: operand.mask,
            });
        }
        if matches!(operand.class, class::ACC_XYZ | class::VECTOR_XYZ) {
            *live = operand.mask;
        }
    }
    Ok(output_masks)
}

/// Literal output-to-input transfer, after status expansion.
/// Every required output contributes bits to every input register operand.
/// VF4/ACC→VF4/ACC is lane-wise; MR32 rotates those bits. All other class
/// combinations demand the complete existing input mask.
pub fn required_inputs_expanded(
    instruction: &ExpandedInstruction,
    live_outputs: &[u8],
) -> Result<Vec<u8>, PruneError> {
    let outputs = normalize_outputs(&instruction.operands, live_outputs)?;
    let mut inputs = vec![0; instruction.operands.len()];
    for (output, &live) in instruction.operands.iter().zip(&outputs) {
        if live == 0 {
            continue;
        }
        for (index, input) in instruction.operands.iter().enumerate() {
            if input.direction != Direction::Read || input.register.is_none() {
                continue;
            }
            let required = if lane_wise(input.class) && lane_wise(output.class) {
                if instruction.instruction.metadata.hook == Hook::Mr32 {
                    rotate_left(live)
                } else {
                    live
                }
            } else {
                input.mask
            };
            if required & !input.mask != 0 {
                return Err(PruneError::InputMask {
                    operand: index,
                    required,
                    available: input.mask,
                });
            }
            inputs[index] |= required;
        }
    }
    Ok(inputs)
}

/// Convenience adapter whose input/output slices use parsed operand indices.
/// A nonzero mask on a virtual status operand names all status outputs expanded
/// from that placeholder. Use `required_inputs_expanded` to select them separately.
pub fn required_inputs(
    instruction: &Instruction,
    live_outputs: &[u8],
) -> Result<Vec<u8>, PruneError> {
    if live_outputs.len() != instruction.operands.len() {
        return Err(PruneError::MaskCount {
            expected: instruction.operands.len(),
            actual: live_outputs.len(),
        });
    }
    let expanded = expand(instruction);
    let outputs = source_output_masks(&expanded, live_outputs)?;
    let required = required_inputs_expanded(&expanded, &outputs)?;
    let mut result = vec![0; instruction.operands.len()];
    for (operand, mask) in expanded.operands.iter().zip(required) {
        result[operand.source_operand] |= mask;
    }
    Ok(result)
}

fn source_output_masks(expanded: &ExpandedInstruction, live: &[u8]) -> Result<Vec<u8>, PruneError> {
    for (index, &mask) in live.iter().enumerate() {
        if mask != 0
            && !expanded.operands.iter().any(|operand| {
                operand.source_operand == index && operand.direction == Direction::Write
            })
        {
            return Err(PruneError::LiveInput { operand: index });
        }
    }
    Ok(expanded
        .operands
        .iter()
        .map(|operand| {
            if operand.direction == Direction::Write {
                live[operand.source_operand]
            } else {
                0
            }
        })
        .collect())
}

fn looks_physical(register: &str) -> bool {
    let lower = register.to_ascii_lowercase();
    lower
        .strip_prefix("vf")
        .or_else(|| lower.strip_prefix("vi"))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Narrow one instruction after graph propagation has supplied live bits for
/// its expanded outputs. Side-effect/RAW instructions keep their original
/// masks. This function does not perform fixed-point graph propagation itself.
pub fn prune_expanded(
    expanded: &ExpandedInstruction,
    live_outputs: &[u8],
    keep_side_effects: bool,
) -> Result<Pruned, PruneError> {
    let source = &expanded.instruction;
    let forced = keep_side_effects || !can_prune(source) || source.mode == ParseMode::Raw;
    let mut outputs = normalize_outputs(&expanded.operands, live_outputs)?;
    let mut inputs = required_inputs_expanded(expanded, &outputs)?;
    if forced {
        for (index, operand) in expanded.operands.iter().enumerate() {
            if operand.register.is_some() {
                match operand.direction {
                    Direction::Read => inputs[index] = operand.mask,
                    Direction::Write => outputs[index] = operand.mask,
                    Direction::Unknown => {}
                }
            }
        }
    }
    let retained = forced || outputs.iter().any(|&mask| mask != 0);
    let mut instruction = source.clone();
    let mut operands = expanded.operands.clone();
    if retained && !forced && source.metadata.mask_mode == MaskMode::Any {
        let mut read_mask = 0;
        let mut write_mask = None;
        let mut vector_inputs = 0;
        for (index, operand) in operands
            .iter()
            .enumerate()
            .filter(|(_, operand)| lane_wise(operand.class))
        {
            if operand.direction == Direction::Read {
                read_mask |= inputs[index];
                vector_inputs += 1;
            }
            if operand.direction == Direction::Write {
                if write_mask.is_some_and(|previous| previous != outputs[index]) {
                    return Err(PruneError::ConflictingVectorOutputs);
                }
                write_mask = Some(outputs[index]);
            }
        }
        if instruction.metadata.hook == Hook::Mr32 {
            read_mask = rotate_right(read_mask);
        }
        let mask = if vector_inputs != 0 {
            if write_mask.is_some_and(|writes| writes & !read_mask != 0) {
                return Err(PruneError::OutputExceedsInput {
                    outputs: write_mask.unwrap(),
                    inputs: read_mask,
                });
            }
            read_mask
        } else {
            write_mask
                .filter(|&mask| mask != 0)
                .unwrap_or(source.mask.0)
        };
        instruction.mask = Mask(mask);
        for operand in &mut instruction.operands {
            if lane_wise(operand.class) {
                operand.mask = Mask(mask);
            }
        }
        for operand in &mut operands {
            if lane_wise(operand.class) {
                operand.mask = if instruction.metadata.hook == Hook::Mr32
                    && operand.direction == Direction::Read
                {
                    rotate_left(mask)
                } else {
                    mask
                };
            }
        }
        if instruction.metadata.hook == Hook::Mr32
            && let Some(input) = instruction.operands.get_mut(1)
        {
            input.mask = Mask(rotate_left(mask));
        }
    }
    let mut zero_outputs = Vec::new();
    if retained && !forced {
        for (index, operand) in operands.iter().enumerate() {
            if outputs[index] == 0
                && operand.direction == Direction::Write
                && matches!(
                    operand.class,
                    class::VECTOR
                        | class::INTEGER
                        | class::INTEGER_FLAG
                        | class::DECLARED_VECTOR
                        | class::DECLARED_INTEGER
                )
                && operand
                    .register
                    .as_deref()
                    .is_some_and(|name| !looks_physical(name))
                && !source
                    .metadata
                    .operands
                    .iter()
                    .any(|spec| usize::from(spec.patch) == operand.source_operand + 1)
            {
                zero_outputs.push(operand.source_operand);
            }
        }
        zero_outputs.sort_unstable();
        zero_outputs.dedup();
    }
    Ok(Pruned {
        instruction,
        expanded: operands,
        input_masks: inputs,
        live_output_masks: outputs,
        zero_outputs,
        retained,
    })
}

/// Parsed-index convenience wrapper. See `prune_expanded` for exact status
/// output selection when a placeholder expands to multiple hardware resources.
pub fn prune(
    instruction: &Instruction,
    live_outputs: &[u8],
    keep_side_effects: bool,
) -> Result<Pruned, PruneError> {
    if live_outputs.len() != instruction.operands.len() {
        return Err(PruneError::MaskCount {
            expected: instruction.operands.len(),
            actual: live_outputs.len(),
        });
    }
    let expanded = expand(instruction);
    let live = source_output_masks(&expanded, live_outputs)?;
    prune_expanded(&expanded, &live, keep_side_effects)
}

/// Propagate required read components back through explicit reaching-definition
/// edges until no output bit changes, as in the original.
///
/// Roots represent externally visible values. Stores, branches, synthetics,
/// raw instructions and caller-provided keep roots independently retain all
/// their inputs. Multiple edges into one read represent control-flow joins;
/// cyclic edges represent loop-carried definitions. Every union is monotonic,
/// so cycles converge without a recursion-depth or iteration-budget shortcut.
/// External/constant/undefined definitions have no producer edge here: callers
/// must perform the original's initial full-source validity check separately.
pub fn propagate(
    instructions: &[ExpandedInstruction],
    edges: &[UseEdge],
    roots: &[LiveRoot],
    keep: &BTreeSet<usize>,
) -> Result<Liveness, GraphError> {
    let operand_at = |instruction: usize, operand: usize| -> Result<&ExpandedOperand, GraphError> {
        instructions
            .get(instruction)
            .ok_or(GraphError::InvalidInstruction(instruction))?
            .operands
            .get(operand)
            .ok_or(GraphError::InvalidOperand {
                instruction,
                operand,
            })
    };
    for &instruction in keep {
        if instruction >= instructions.len() {
            return Err(GraphError::InvalidInstruction(instruction));
        }
    }
    for (id, edge) in edges.iter().enumerate() {
        let output = operand_at(edge.producer, edge.output_operand)?;
        let input = operand_at(edge.consumer, edge.input_operand)?;
        let reason = if output.direction != Direction::Write || output.register.is_none() {
            Some("source is not a register output")
        } else if input.direction != Direction::Read || input.register.is_none() {
            Some("target is not a register input")
        } else if edge.components == 0
            || edge.components & !output.mask != 0
            || edge.components & !input.mask != 0
        {
            Some("component mask is outside the source or target operand")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(GraphError::InvalidEdge { edge: id, reason });
        }
    }
    let mut outputs: Vec<Vec<u8>> = instructions
        .iter()
        .map(|instruction| vec![0; instruction.operands.len()])
        .collect();
    for root in roots {
        let operand = operand_at(root.instruction, root.output_operand)?;
        if operand.direction != Direction::Write {
            return Err(GraphError::Prune {
                instruction: root.instruction,
                error: PruneError::LiveInput {
                    operand: root.output_operand,
                },
            });
        }
        if root.components & !operand.mask != 0 {
            return Err(GraphError::Prune {
                instruction: root.instruction,
                error: PruneError::OutputMask {
                    operand: root.output_operand,
                    requested: root.components,
                    available: operand.mask,
                },
            });
        }
        outputs[root.instruction][root.output_operand] |= root.components;
    }
    let mut iterations = 0;
    let inputs = loop {
        iterations += 1;
        let mut changed = false;
        let mut inputs = Vec::with_capacity(instructions.len());
        for (id, instruction) in instructions.iter().enumerate() {
            let normalized =
                normalize_outputs(&instruction.operands, &outputs[id]).map_err(|error| {
                    GraphError::Prune {
                        instruction: id,
                        error,
                    }
                })?;
            if normalized != outputs[id] {
                outputs[id] = normalized;
                changed = true;
            }
            let forced = keep.contains(&id)
                || !can_prune(&instruction.instruction)
                || instruction.instruction.mode == ParseMode::Raw;
            let input = if forced {
                instruction
                    .operands
                    .iter()
                    .map(|operand| {
                        if operand.direction == Direction::Read && operand.register.is_some() {
                            operand.mask
                        } else {
                            0
                        }
                    })
                    .collect()
            } else {
                required_inputs_expanded(instruction, &outputs[id]).map_err(|error| {
                    GraphError::Prune {
                        instruction: id,
                        error,
                    }
                })?
            };
            inputs.push(input);
        }
        for edge in edges {
            let needed = inputs[edge.consumer][edge.input_operand] & edge.components;
            let output = &mut outputs[edge.producer][edge.output_operand];
            if needed & !*output != 0 {
                *output |= needed;
                changed = true;
            }
        }
        if !changed {
            break inputs;
        }
    };
    let pruned: Vec<_> = instructions
        .iter()
        .enumerate()
        .map(|(id, instruction)| {
            prune_expanded(instruction, &outputs[id], keep.contains(&id)).map_err(|error| {
                GraphError::Prune {
                    instruction: id,
                    error,
                }
            })
        })
        .collect::<Result<_, _>>()?;
    let retained = pruned
        .iter()
        .map(|instruction| instruction.retained)
        .collect();
    Ok(Liveness {
        live_output_masks: outputs,
        required_input_masks: inputs,
        retained,
        pruned,
        iterations,
    })
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::instruction::Syntax;
    use crate::parser::{self, ParserOptions, StatementKind};

    fn instruction(source: &str) -> Instruction {
        parser::parse_with_options(
            source,
            ParserOptions {
                syntax: Syntax::Old,
                allow_incomplete: true,
            },
        )
        .unwrap()
        .statements
        .into_iter()
        .find_map(|statement| match statement.kind {
            StatementKind::Instruction(instruction) => Some(instruction),
            _ => None,
        })
        .unwrap()
    }

    fn live(source: &Instruction, index: usize, mask: u8) -> Vec<u8> {
        let mut live = vec![0; source.operands.len()];
        live[index] = mask;
        live
    }

    #[test]
    fn add_output_lane_narrows_both_vector_inputs() {
        let add = instruction("add b,a,c");
        let needed = live(&add, 0, 1);
        let inputs = required_inputs(&add, &needed).unwrap();
        assert_eq!(&inputs[..3], &[0, 1, 1]);
        let pruned = prune(&add, &needed, false).unwrap();
        assert!(pruned.retained);
        assert_eq!(pruned.instruction.mask, Mask::X);
        assert!(
            pruned.instruction.operands[..3]
                .iter()
                .all(|operand| operand.mask == Mask::X)
        );
    }

    #[test]
    fn broadcast_input_keeps_its_fixed_lane() {
        let add = instruction("addx b,a,c.x");
        let inputs = required_inputs(&add, &live(&add, 0, 2)).unwrap();
        assert_eq!(&inputs[..3], &[0, 2, 1]);
        let pruned = prune(&add, &live(&add, 0, 2), false).unwrap();
        assert_eq!(pruned.instruction.mask, Mask::Y);
        assert_eq!(pruned.instruction.operands[2].mask, Mask::X);
    }

    #[test]
    fn mr32_rotates_required_input_lanes_then_restores_destination_mask() {
        let mr32 = instruction("mr32 b,a");
        assert_eq!(
            required_inputs(&mr32, &live(&mr32, 0, 1)).unwrap(),
            vec![0, 2]
        );
        assert_eq!(
            required_inputs(&mr32, &live(&mr32, 0, 8)).unwrap(),
            vec![0, 1]
        );
        let pruned = prune(&mr32, &live(&mr32, 0, 1), false).unwrap();
        assert_eq!(pruned.instruction.mask, Mask::X);
        assert_eq!(pruned.instruction.operands[1].mask, Mask::Y);
    }

    #[test]
    fn cross_product_xyz_results_are_indivisible_to_original_pruner() {
        for source in ["opmula.xyz ACC,a,c", "opmsub.xyz b,a,c"] {
            let inst = instruction(source);
            let pruned = prune(&inst, &live(&inst, 0, 1), false).unwrap();
            assert_eq!(pruned.instruction.mask, Mask::XYZ);
            assert_eq!(pruned.live_output_masks[0], 7);
            assert_eq!(pruned.input_masks[1], 7);
            assert_eq!(pruned.input_masks[2], 7);
        }
    }

    #[test]
    fn a_live_mac_output_demands_all_selected_arithmetic_inputs() {
        let add = instruction("add b,a,c");
        let needed = live(&add, 3, 1);
        let pruned = prune(&add, &needed, false).unwrap();
        assert_eq!(pruned.input_masks[1], 15);
        assert_eq!(pruned.input_masks[2], 15);
        assert_eq!(pruned.instruction.mask, Mask::XYZW);
        assert_eq!(pruned.zero_outputs, vec![0]);
        let physical = instruction("add vf02,a,c");
        assert!(
            prune(&physical, &live(&physical, 3, 1), false)
                .unwrap()
                .zero_outputs
                .is_empty()
        );
    }

    #[test]
    fn status_expansion_separates_current_and_sticky_resources() {
        let add = expand(&instruction("add b,a,c"));
        let status: Vec<_> = add
            .operands
            .iter()
            .filter(|operand| operand.source_operand == 4)
            .map(|operand| {
                (
                    operand.register.as_deref().unwrap(),
                    operand.direction,
                    operand.latency,
                )
            })
            .collect();
        assert_eq!(
            status,
            vec![
                ("STAT_ALU", Direction::Write, 4),
                ("STAT_ALU_S", Direction::Read, -1)
            ]
        );
        let fsand = expand(&instruction("fsand vi01,0x400"));
        let status: Vec<_> = fsand
            .operands
            .iter()
            .filter(|operand| operand.source_operand == 2)
            .map(|operand| (operand.register.as_deref().unwrap(), operand.direction))
            .collect();
        assert_eq!(
            status,
            vec![
                ("STAT_ALU", Direction::Read),
                ("STAT_ALU_S", Direction::Read),
                ("STAT_ALU_S", Direction::Write)
            ]
        );
    }

    #[test]
    fn fs_test_flags_follow_immediate_masks_and_unresolved_values_are_conservative() {
        let reads = |source: &str| {
            expand(&instruction(source))
                .operands
                .into_iter()
                .filter(|operand| operand.implicit && operand.direction == Direction::Read)
                .filter_map(|operand| operand.register)
                .collect::<Vec<_>>()
        };
        assert_eq!(reads("fsand vi01,1"), vec!["STAT_ALU"]);
        assert_eq!(reads("fsand vi01,0x10"), vec!["STAT_DIV"]);
        assert_eq!(reads("fsand vi01,0"), Vec::<String>::new());
        assert_eq!(reads("fsor vi01,0xfff"), Vec::<String>::new());
        assert_eq!(
            reads("fseq vi01,0"),
            vec!["STAT_ALU", "STAT_DIV", "STAT_ALU_S", "STAT_DIV_S"]
        );
        assert_eq!(
            reads("fsand vi01,FLAGS"),
            vec!["STAT_ALU", "STAT_DIV", "STAT_ALU_S", "STAT_DIV_S"]
        );
    }

    #[test]
    fn stores_and_address_update_effects_keep_original_input_extent() {
        let store = instruction("sqi a,(p++)");
        assert!(!can_prune(&store));
        let result = prune(&store, &vec![0; store.operands.len()], false).unwrap();
        assert!(result.retained);
        assert_eq!(result.input_masks[0], 15);
        assert!(result.input_masks.iter().skip(1).any(|&mask| mask != 0));
        assert!(result.zero_outputs.is_empty());
        let load = instruction("lqi b,(p++)");
        let needed = live(&load, 1, 1);
        let result = prune(&load, &needed, false).unwrap();
        assert_eq!(result.zero_outputs, vec![0]);
        assert!(result.input_masks.iter().skip(1).any(|&mask| mask != 0));
    }

    #[test]
    fn raw_pairs_preserve_input_fields_even_when_outputs_are_unused() {
        let inst = instruction(
            ".vsm\n\
                                add b,a,c nop\n\
                                .endvsm",
        );
        let result = prune(&inst, &vec![0; inst.operands.len()], false).unwrap();
        assert!(result.retained);
        assert_eq!(result.instruction.mask, Mask::XYZW);
        assert_eq!(result.input_masks[1], 15);
        assert_eq!(result.input_masks[2], 15);
    }

    #[test]
    fn an_unused_pure_instruction_can_be_deleted() {
        let inst = instruction("add b,a,c");
        let result = prune(&inst, &vec![0; inst.operands.len()], false).unwrap();
        assert!(!result.retained);
        assert!(result.input_masks.iter().all(|&mask| mask == 0));
    }

    #[test]
    fn malformed_liveness_masks_are_explicit_errors() {
        let inst = instruction("add.xyz b,a,c");
        assert!(matches!(
            required_inputs(&inst, &[]),
            Err(PruneError::MaskCount { .. })
        ));
        assert!(matches!(
            required_inputs(&inst, &live(&inst, 0, 8)),
            Err(PruneError::OutputMask { .. })
        ));
        assert!(matches!(
            required_inputs(&inst, &live(&inst, 1, 1)),
            Err(PruneError::LiveInput { .. })
        ));
        let expanded = expand(&inst);
        let mut live = vec![0; expanded.operands.len()];
        live[1] = 1;
        assert!(matches!(
            required_inputs_expanded(&expanded, &live),
            Err(PruneError::LiveInput { .. })
        ));
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn recovered_narrowing_matches_saved_original_outputs() {
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        for (case, source, requested, expected) in [
            ("prune-add-output-x", "add b,a,a", 1, "add[e].x"),
            ("prune-broadcast-output-y", "addx b,a,c.x", 2, "addx[e].y"),
            ("prune-mr32-output-x", "mr32 b,a", 1, "mr32.x"),
            (
                "prune-opmula-acc-x",
                "opmula.xyz ACC,a,c",
                1,
                "opmula[e].xyz",
            ),
            (
                "prune-opmsub-output-x",
                "opmsub.xyz b,a,c",
                1,
                "opmsub[e].xyz",
            ),
        ] {
            let source = instruction(source);
            let result = prune(&source, &live(&source, 0, requested), false).unwrap();
            let original = std::fs::read_to_string(
                root.join("local/evidence/oracle")
                    .join(case)
                    .join("stdout.bin"),
            )
            .unwrap();
            let opcode = original
                .split_whitespace()
                .find(|token| token.eq_ignore_ascii_case(expected))
                .unwrap();
            let suffix = opcode.rsplit_once('.').unwrap().1;
            assert_eq!(
                result.instruction.mask,
                Mask::parse(suffix).unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn original_initial_flow_rejects_undefined_reads_even_in_dead_lanes_and_operations() {
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        for (case, register) in [
            ("prune-add-input-x", "a"),
            ("prune-mr32-input-y", "a"),
            ("prune-dead-undefined", "missing_value"),
            ("prune-dead-partial", "a"),
            ("prune-dead-hardware-q", "Q"),
        ] {
            let path = root.join("local/evidence/oracle").join(case);
            let result = std::fs::read_to_string(path.join("result.json")).unwrap();
            assert!(result.contains("\"exit_status\": 255"), "{case}");
            let diagnostic = std::fs::read_to_string(path.join("stderr.bin")).unwrap();
            assert!(
                diagnostic.contains(&format!("register {register} is used before set")),
                "{case}"
            );
        }
    }

    #[test]
    fn graph_fixed_point_carries_lane_requirements_through_a_chain() {
        let instructions = [
            expand(&instruction("add c,a,a")),
            expand(&instruction("mul b,c,a")),
        ];
        let edges = [UseEdge {
            producer: 0,
            output_operand: 0,
            consumer: 1,
            input_operand: 1,
            components: 15,
        }];
        let roots = [LiveRoot {
            instruction: 1,
            output_operand: 0,
            components: 1,
        }];
        let result = propagate(&instructions, &edges, &roots, &BTreeSet::new()).unwrap();
        assert_eq!(result.live_output_masks[0][0], 1);
        assert_eq!(result.required_input_masks[0][1], 1);
        assert_eq!(result.required_input_masks[1][1], 1);
        assert!(
            result
                .pruned
                .iter()
                .all(|instruction| instruction.instruction.mask == Mask::X)
        );
        assert_eq!(result.retained, vec![true, true]);
    }

    #[test]
    fn graph_joins_keep_each_reaching_definition_for_live_components() {
        let instructions = [
            expand(&instruction("add c,a,a")),
            expand(&instruction("mul c,a,a")),
            expand(&instruction("sub b,c,a")),
        ];
        let edges = [
            UseEdge {
                producer: 0,
                output_operand: 0,
                consumer: 2,
                input_operand: 1,
                components: 15,
            },
            UseEdge {
                producer: 1,
                output_operand: 0,
                consumer: 2,
                input_operand: 1,
                components: 15,
            },
        ];
        let result = propagate(
            &instructions,
            &edges,
            &[LiveRoot {
                instruction: 2,
                output_operand: 0,
                components: 2,
            }],
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(result.live_output_masks[0][0], 2);
        assert_eq!(result.live_output_masks[1][0], 2);
        assert_eq!(result.retained, vec![true, true, true]);
    }

    #[test]
    fn graph_discards_a_definition_of_a_different_dead_lane() {
        let instructions = [
            expand(&instruction("add.x c,a,a")),
            expand(&instruction("mul.y c,a,a")),
            expand(&instruction("sub b,c,a")),
        ];
        let edges = [
            UseEdge {
                producer: 0,
                output_operand: 0,
                consumer: 2,
                input_operand: 1,
                components: 1,
            },
            UseEdge {
                producer: 1,
                output_operand: 0,
                consumer: 2,
                input_operand: 1,
                components: 2,
            },
        ];
        let result = propagate(
            &instructions,
            &edges,
            &[LiveRoot {
                instruction: 2,
                output_operand: 0,
                components: 1,
            }],
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(result.retained, vec![true, false, true]);
        assert_eq!(result.live_output_masks[1][0], 0);
        assert!(result.required_input_masks[1].iter().all(|&mask| mask == 0));
    }

    #[test]
    fn graph_loop_edges_converge_monotonically() {
        let instructions = [
            expand(&instruction("add b,b,a")),
            expand(&instruction("mul c,b,a")),
        ];
        let edges = [
            UseEdge {
                producer: 0,
                output_operand: 0,
                consumer: 0,
                input_operand: 1,
                components: 15,
            },
            UseEdge {
                producer: 0,
                output_operand: 0,
                consumer: 1,
                input_operand: 1,
                components: 15,
            },
        ];
        let result = propagate(
            &instructions,
            &edges,
            &[LiveRoot {
                instruction: 1,
                output_operand: 0,
                components: 9,
            }],
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(result.live_output_masks[0][0], 9);
        assert_eq!(result.required_input_masks[0][1], 9);
        assert!(result.iterations < 10);
    }

    #[test]
    fn graph_mr32_rotates_into_its_reaching_vector_definition() {
        let instructions = [
            expand(&instruction("add c,a,a")),
            expand(&instruction("mr32 b,c")),
        ];
        let edges = [UseEdge {
            producer: 0,
            output_operand: 0,
            consumer: 1,
            input_operand: 1,
            components: 15,
        }];
        let result = propagate(
            &instructions,
            &edges,
            &[LiveRoot {
                instruction: 1,
                output_operand: 0,
                components: 1,
            }],
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(result.live_output_masks[0][0], 2);
        assert_eq!(result.pruned[0].instruction.mask, Mask::Y);
        assert_eq!(result.pruned[1].instruction.mask, Mask::X);
    }

    #[test]
    fn graph_side_effect_roots_keep_inputs_without_an_output_root() {
        let instructions = [
            expand(&instruction("add c,a,a")),
            expand(&instruction("sq c,0(p)")),
        ];
        let edges = [UseEdge {
            producer: 0,
            output_operand: 0,
            consumer: 1,
            input_operand: 0,
            components: 15,
        }];
        let result = propagate(&instructions, &edges, &[], &BTreeSet::new()).unwrap();
        assert_eq!(result.retained, vec![true, true]);
        assert_eq!(result.live_output_masks[0][0], 15);
    }

    #[test]
    fn malformed_graph_edges_and_roots_return_errors() {
        let instructions = [expand(&instruction("add c,a,a"))];
        let edge = UseEdge {
            producer: 0,
            output_operand: 0,
            consumer: 0,
            input_operand: 1,
            components: 0,
        };
        assert!(matches!(
            propagate(&instructions, &[edge], &[], &BTreeSet::new()),
            Err(GraphError::InvalidEdge { .. })
        ));
        let edge = UseEdge {
            input_operand: 0,
            components: 1,
            ..edge
        };
        assert!(matches!(
            propagate(&instructions, &[edge], &[], &BTreeSet::new()),
            Err(GraphError::InvalidEdge { .. })
        ));
        let root = LiveRoot {
            instruction: 1,
            output_operand: 0,
            components: 1,
        };
        assert_eq!(
            propagate(&instructions, &[], &[root], &BTreeSet::new()),
            Err(GraphError::InvalidInstruction(1))
        );
    }
}
