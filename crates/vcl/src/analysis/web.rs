//! Operand-level register webs inferred from vcl 1.4beta7.
//!
//! A source register name is an analysis identity, not an allocation lifetime.
//! The original joins each input operand to all its component definitions. An
//! operand that reads several components joins their strands, including joins
//! and loop recurrences. Unconnected definitions of the same name stay apart.
//! It coalesces explicit physical bindings and makes every unpinned
//! terminal web virtual. Hardware resources and pool zero registers bypass webs.

use crate::analysis::flow::{Analysis, Definition};
use crate::isa::instruction::Direction;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub type WebId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operand {
    pub register: usize,
    pub mask: u8,
    pub direction: Direction,
    /// True for a nonzero register in a forwardable VF/VI pool. This does not
    /// depend on whether its source spelling looks physical or virtual.
    pub forwardable: bool,
    /// Explicit physical binding, represented in the caller's register ID
    /// namespace. VF01 and VI01 must have different IDs.
    pub forced_base: Option<usize>,
    /// Same-instruction operand to which an implicit update is tied (for
    /// example an incremented load address). Indices include expanded operands.
    pub linked_operand: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub register: usize,
    pub mask: u8,
    pub forced_base: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitBinding {
    pub block: usize,
    pub register: usize,
    pub mask: u8,
    pub forced_base: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Web {
    /// Source identities joined by this web. Different identities can meet
    /// through explicit bindings to the same physical base.
    pub registers: BTreeSet<usize>,
    pub mask: u8,
    pub forced_base: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Effective operand indices, not parsed operand indices. A None entry
    /// leaves the original hardware/constant operand unchanged.
    pub operands: Vec<Vec<Option<WebId>>>,
    pub inputs: Vec<Option<WebId>>,
    pub outputs: Vec<Option<WebId>>,
    pub webs: Vec<Web>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebError {
    MissingRead {
        operation: usize,
        register: usize,
        component: u8,
    },
    Undefined {
        operation: Option<usize>,
        register: usize,
        component: u8,
    },
    MissingInput {
        register: usize,
        component: u8,
    },
    MissingProducer {
        operation: usize,
        register: usize,
        component: u8,
    },
    MissingOutput {
        block: usize,
        register: usize,
        component: u8,
    },
    InvalidLink {
        operation: usize,
        operand: usize,
        linked_operand: usize,
    },
    ConflictingBases {
        first: usize,
        second: usize,
    },
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "register web construction failed: {self:?}")
    }
}

impl std::error::Error for WebError {}

#[derive(Default)]
struct Strands {
    parent: Vec<usize>,
    forced: Vec<Option<usize>>,
    register: Vec<usize>,
    mask: Vec<u8>,
}

impl Strands {
    fn add(&mut self, register: usize, mask: u8, forced: Option<usize>) -> Option<usize> {
        if mask == 0 {
            return None;
        }
        let id = self.parent.len();
        self.parent.push(id);
        self.forced.push(forced);
        self.register.push(register);
        self.mask.push(mask);
        Some(id)
    }

    fn root(&self, mut id: usize) -> usize {
        while self.parent[id] != id {
            id = self.parent[id];
        }
        id
    }

    fn merge(&mut self, first: usize, second: usize) -> Result<(), WebError> {
        let a = self.root(first);
        let b = self.root(second);
        if a == b {
            return Ok(());
        }
        if let (Some(first), Some(second)) = (self.forced[a], self.forced[b])
            && first != second
        {
            return Err(WebError::ConflictingBases { first, second });
        }
        // Stable first-appearance order keeps plans reproducible; original
        // intrusive lists need not have this internal representative order.
        let (keep, lose) = if a < b { (a, b) } else { (b, a) };
        self.parent[lose] = keep;
        self.forced[keep] = self.forced[keep].or(self.forced[lose]);
        Ok(())
    }
}

fn components(mask: u8) -> impl Iterator<Item = u8> {
    (0..8).filter(move |component| mask & (1 << component) != 0)
}

/// Reconstruct allocation identities from already validated, pruned data flow.
///
/// `operations` and `analysis` must describe the same operation indices and
/// masks. Include only forwardable interface bindings: hardware resources and
/// zero constants remain represented by their original register identities.
/// A complete input/output operand is one strand, so a read combining X from
/// one definition and Y from another forces those definitions into one web.
/// Separate source occurrences of a read/write register remain separate unless
/// reaching definitions or `linked_operand` require them to share a location.
///
/// Entry bindings and EXIT outputs preserve their entire declared mask. In
/// particular, interfaces are not independently split into component registers.
/// All strands pinned to the same physical base coalesce, as Gather does before
/// creating a virtual register for every remaining unpinned strand.
pub fn plan(
    operations: &[Vec<Operand>],
    inputs: &[Binding],
    outputs: &[ExitBinding],
    analysis: &Analysis,
) -> Result<Plan, WebError> {
    let mut strands = Strands::default();
    let operand_nodes: Vec<Vec<_>> = operations
        .iter()
        .map(|operation| {
            operation
                .iter()
                .map(|operand| {
                    if operand.forwardable {
                        strands.add(operand.register, operand.mask, operand.forced_base)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();
    let input_nodes: Vec<_> = inputs
        .iter()
        .map(|input| strands.add(input.register, input.mask, input.forced_base))
        .collect();
    let output_nodes: Vec<_> = outputs
        .iter()
        .map(|output| strands.add(output.register, output.mask, output.forced_base))
        .collect();
    let visited: std::collections::BTreeSet<usize> =
        analysis.reads.iter().map(|read| read.operation).collect();
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
    let output_defs: BTreeMap<_, _> = analysis
        .outputs
        .iter()
        .map(|output| {
            (
                (output.block, output.register, output.component),
                &output.definitions,
            )
        })
        .collect();

    struct Definitions<'a> {
        operations: &'a [Vec<Operand>],
        operands: &'a [Vec<Option<usize>>],
        inputs: &'a [Binding],
        input_nodes: &'a [Option<usize>],
    }
    impl Definitions<'_> {
        fn connect(
            &self,
            strands: &mut Strands,
            node: usize,
            register: usize,
            component: u8,
            definitions: &BTreeSet<Definition>,
            consumer: Option<usize>,
        ) -> Result<(), WebError> {
            let mask = 1 << component;
            if definitions.is_empty() || definitions.contains(&Definition::Uninitialized) {
                return Err(WebError::Undefined {
                    operation: consumer,
                    register,
                    component,
                });
            }
            for definition in definitions {
                let producers: Vec<usize> = match *definition {
                    Definition::Uninitialized => unreachable!(),
                    Definition::Input => self
                        .inputs
                        .iter()
                        .zip(self.input_nodes)
                        .filter_map(|(input, &id)| {
                            (input.register == register && input.mask & mask != 0)
                                .then_some(id)
                                .flatten()
                        })
                        .collect(),
                    Definition::Operation(operation) => self
                        .operations
                        .get(operation)
                        .zip(self.operands.get(operation))
                        .into_iter()
                        .flat_map(|(operands, ids)| operands.iter().zip(ids))
                        .filter_map(|(output, &id)| {
                            (output.direction == Direction::Write
                                && output.register == register
                                && output.mask & mask != 0)
                                .then_some(id)
                                .flatten()
                        })
                        .collect(),
                };
                if producers.is_empty() {
                    return Err(match *definition {
                        Definition::Input => WebError::MissingInput {
                            register,
                            component,
                        },
                        Definition::Operation(operation) => WebError::MissingProducer {
                            operation,
                            register,
                            component,
                        },
                        Definition::Uninitialized => unreachable!(),
                    });
                }
                for producer in producers {
                    strands.merge(node, producer)?;
                }
            }
            Ok(())
        }
    }
    let definitions = Definitions {
        operations,
        operands: &operand_nodes,
        inputs,
        input_nodes: &input_nodes,
    };
    for (operation, operands) in operations.iter().enumerate() {
        for (index, operand) in operands.iter().enumerate() {
            let Some(node) = operand_nodes[operation][index] else {
                continue;
            };
            if let Some(linked_operand) = operand.linked_operand {
                let target = operand_nodes[operation]
                    .get(linked_operand)
                    .copied()
                    .flatten()
                    .ok_or(WebError::InvalidLink {
                        operation,
                        operand: index,
                        linked_operand,
                    })?;
                strands.merge(node, target)?;
            }
            if operand.direction != Direction::Read {
                continue;
            }
            // Flow never visits unreachable code (after a `jr` whose tags do
            // not name it, for example); the original drops it, and its
            // reads join no web.
            if !visited.contains(&operation) {
                continue;
            }
            for component in components(operand.mask) {
                let definitions_for_read = read_defs
                    .get(&(operation, operand.register, component))
                    .ok_or(WebError::MissingRead {
                        operation,
                        register: operand.register,
                        component,
                    })?;
                definitions.connect(
                    &mut strands,
                    node,
                    operand.register,
                    component,
                    definitions_for_read,
                    Some(operation),
                )?;
            }
        }
    }
    for (output, &node) in outputs.iter().zip(&output_nodes) {
        let Some(node) = node else {
            continue;
        };
        for component in components(output.mask) {
            let definitions_for_output = output_defs
                .get(&(output.block, output.register, component))
                .ok_or(WebError::MissingOutput {
                    block: output.block,
                    register: output.register,
                    component,
                })?;
            definitions.connect(
                &mut strands,
                node,
                output.register,
                component,
                definitions_for_output,
                None,
            )?;
        }
    }
    // Forced bases are whole-register identities, even when the uses have
    // disjoint masks or source names.
    let mut forced_nodes = BTreeMap::new();
    for node in 0..strands.parent.len() {
        if let Some(base) = strands.forced[strands.root(node)] {
            if let Some(&other) = forced_nodes.get(&base) {
                strands.merge(node, other)?;
            } else {
                forced_nodes.insert(base, node);
            }
        }
    }
    let mut roots = BTreeMap::new();
    let mut webs: Vec<Web> = Vec::new();
    for node in 0..strands.parent.len() {
        let root = strands.root(node);
        let id = *roots.entry(root).or_insert_with(|| {
            let id = webs.len();
            webs.push(Web {
                registers: BTreeSet::new(),
                mask: 0,
                forced_base: strands.forced[root],
            });
            id
        });
        webs[id].registers.insert(strands.register[node]);
        webs[id].mask |= strands.mask[node];
    }
    let web_for = |node: Option<usize>| node.map(|node| roots[&strands.root(node)]);
    Ok(Plan {
        operands: operand_nodes
            .into_iter()
            .map(|operation| operation.into_iter().map(web_for).collect())
            .collect(),
        inputs: input_nodes.into_iter().map(web_for).collect(),
        outputs: output_nodes.into_iter().map(web_for).collect(),
        webs,
    })
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::flow::{Access, Block, Graph, Operation};

    fn operand(register: usize, mask: u8, direction: Direction) -> Operand {
        Operand {
            register,
            mask,
            direction,
            forwardable: true,
            forced_base: None,
            linked_operand: None,
        }
    }

    fn read(register: usize, mask: u8) -> Operand {
        operand(register, mask, Direction::Read)
    }

    fn write(register: usize, mask: u8) -> Operand {
        operand(register, mask, Direction::Write)
    }

    fn binding(register: usize, mask: u8, base: usize) -> Binding {
        Binding {
            register,
            mask,
            forced_base: Some(base),
        }
    }

    fn exit(block: usize, register: usize, mask: u8, base: usize) -> ExitBinding {
        ExitBinding {
            block,
            register,
            mask,
            forced_base: Some(base),
        }
    }

    fn analyze(
        operands: &[Vec<Operand>],
        mut blocks: Vec<Block>,
        inputs: &[Binding],
        outputs: &[ExitBinding],
    ) -> Analysis {
        for output in outputs {
            blocks[output.block].outputs.push(Access {
                register: output.register,
                mask: output.mask,
            });
        }
        Graph {
            operations: operands
                .iter()
                .map(|operands| Operation {
                    reads: operands
                        .iter()
                        .filter(|operand| operand.direction == Direction::Read)
                        .map(|operand| Access {
                            register: operand.register,
                            mask: operand.mask,
                        })
                        .collect(),
                    writes: operands
                        .iter()
                        .filter(|operand| operand.direction == Direction::Write)
                        .map(|operand| Access {
                            register: operand.register,
                            mask: operand.mask,
                        })
                        .collect(),
                    side_effect: false,
                })
                .collect(),
            blocks,
            entry: 0,
            inputs: inputs
                .iter()
                .map(|binding| Access {
                    register: binding.register,
                    mask: binding.mask,
                })
                .collect(),
            constants: BTreeSet::new(),
        }
        .analyze()
        .unwrap()
    }

    fn straight(operations: &[Vec<Operand>], inputs: &[Binding], outputs: &[ExitBinding]) -> Plan {
        let blocks = vec![Block {
            operations: (0..operations.len()).collect(),
            ..Block::default()
        }];
        plan(
            operations,
            inputs,
            outputs,
            &analyze(operations, blocks, inputs, outputs),
        )
        .unwrap()
    }

    #[test]
    fn independent_definitions_of_one_name_are_separate_webs() {
        let operations = vec![
            vec![write(1, 15), read(0, 15)],
            vec![write(2, 15), read(1, 15)],
            vec![write(1, 15), read(0, 15)],
            vec![write(3, 15), read(1, 15)],
        ];
        let result = straight(
            &operations,
            &[binding(0, 15, 100)],
            &[exit(0, 2, 15, 102), exit(0, 3, 15, 103)],
        );
        assert_eq!(result.operands[0][0], result.operands[1][1]);
        assert_eq!(result.operands[2][0], result.operands[3][1]);
        assert_ne!(result.operands[0][0], result.operands[2][0]);
        assert_eq!(
            result.webs[result.operands[0][0].unwrap()].forced_base,
            None
        );
    }

    #[test]
    fn overwriting_an_input_spelling_does_not_inherit_its_entry_pin() {
        let operations = vec![
            vec![write(1, 15), read(1, 15)],
            vec![write(2, 15), read(1, 15)],
        ];
        let result = straight(&operations, &[binding(1, 15, 101)], &[exit(0, 2, 15, 102)]);
        let old = result.operands[0][1].unwrap();
        let new = result.operands[0][0].unwrap();
        assert_ne!(old, new);
        assert_eq!(result.webs[old].forced_base, Some(101));
        assert_eq!(result.webs[new].forced_base, None);
    }

    #[test]
    fn one_multicomponent_read_unites_partial_definition_strands() {
        let operations = vec![vec![write(1, 1)], vec![write(1, 2)], vec![read(1, 3)]];
        let result = straight(&operations, &[], &[]);
        assert_eq!(result.operands[0][0], result.operands[1][0]);
        assert_eq!(result.operands[0][0], result.operands[2][0]);
        assert_eq!(result.webs[0].mask, 3);
    }

    #[test]
    fn separate_component_reads_leave_independent_strands_apart() {
        let operations = vec![
            vec![write(1, 1)],
            vec![write(1, 2)],
            vec![read(1, 1), read(1, 2)],
        ];
        let result = straight(&operations, &[], &[]);
        assert_ne!(result.operands[0][0], result.operands[1][0]);
        assert_eq!(result.operands[0][0], result.operands[2][0]);
        assert_eq!(result.operands[1][0], result.operands[2][1]);
    }

    #[test]
    fn an_exit_operand_unites_its_partial_definition_strands() {
        let operations = vec![vec![write(1, 1)], vec![write(1, 2)]];
        let result = straight(&operations, &[], &[exit(0, 1, 3, 101)]);
        assert_eq!(result.operands[0][0], result.operands[1][0]);
        assert_eq!(result.operands[0][0], result.outputs[0]);
        assert_eq!(result.webs[0].forced_base, Some(101));
    }

    #[test]
    fn reaching_definitions_at_branch_join_share_one_web() {
        let operations = vec![vec![write(1, 15)], vec![write(1, 15)], vec![read(1, 15)]];
        let blocks = vec![
            Block {
                successors: vec![1, 2],
                ..Block::default()
            },
            Block {
                operations: vec![0],
                successors: vec![3],
                ..Block::default()
            },
            Block {
                operations: vec![1],
                successors: vec![3],
                ..Block::default()
            },
            Block {
                operations: vec![2],
                ..Block::default()
            },
        ];
        let result = plan(
            &operations,
            &[],
            &[],
            &analyze(&operations, blocks, &[], &[]),
        )
        .unwrap();
        assert_eq!(result.operands[0][0], result.operands[1][0]);
        assert_eq!(result.operands[1][0], result.operands[2][0]);
    }

    #[test]
    fn loop_recurrence_unites_entry_value_read_and_backedge_definition() {
        let operations = vec![vec![write(1, 15)], vec![write(1, 15), read(1, 15)]];
        let blocks = vec![
            Block {
                operations: vec![0],
                successors: vec![1],
                ..Block::default()
            },
            Block {
                operations: vec![1],
                successors: vec![1, 2],
                ..Block::default()
            },
            Block::default(),
        ];
        let outputs = [exit(2, 1, 15, 101)];
        let result = plan(
            &operations,
            &[],
            &outputs,
            &analyze(&operations, blocks, &[], &outputs),
        )
        .unwrap();
        assert_eq!(result.operands[0][0], result.operands[1][0]);
        assert_eq!(result.operands[1][0], result.operands[1][1]);
        assert_eq!(result.operands[1][0], result.outputs[0]);
    }

    #[test]
    fn implicit_address_writeback_preserves_input_identity() {
        let mut updated = write(1, 1);
        updated.linked_operand = Some(1);
        let operations = vec![vec![updated, read(1, 1)]];
        let result = straight(&operations, &[binding(1, 1, 101)], &[]);
        assert_eq!(result.operands[0][0], result.operands[0][1]);
        assert_eq!(result.webs[0].forced_base, Some(101));
    }

    #[test]
    fn all_strands_for_one_forced_base_merge_even_with_disjoint_names_masks() {
        let mut first = write(1, 1);
        first.forced_base = Some(101);
        let mut second = write(2, 2);
        second.forced_base = Some(101);
        let operations = vec![vec![first], vec![second]];
        let result = straight(&operations, &[], &[]);
        assert_eq!(result.operands[0][0], result.operands[1][0]);
        assert_eq!(result.webs[0].registers, BTreeSet::from([1, 2]));
        assert_eq!(result.webs[0].mask, 3);
    }

    #[test]
    fn operand_cannot_combine_values_pinned_to_different_bases() {
        let operations = vec![vec![read(1, 3)]];
        let inputs = [binding(1, 1, 101), binding(1, 2, 102)];
        let blocks = vec![Block {
            operations: vec![0],
            ..Block::default()
        }];
        let result = plan(
            &operations,
            &inputs,
            &[],
            &analyze(&operations, blocks, &inputs, &[]),
        );
        assert_eq!(
            result,
            Err(WebError::ConflictingBases {
                first: 101,
                second: 102
            })
        );
    }

    #[test]
    fn hardware_and_zero_registers_bypass_webs() {
        let mut hardware = read(100, 1);
        hardware.forwardable = false;
        let mut zero = write(0, 15);
        zero.forwardable = false;
        let operations = vec![vec![hardware, zero, write(1, 0)]];
        let result = straight(&operations, &[], &[]);
        assert_eq!(result.operands, vec![vec![None, None, None]]);
        assert!(result.webs.is_empty());
    }

    #[test]
    fn undefined_input_is_reported_without_inventing_a_web_definition() {
        let operations = vec![vec![read(1, 1)]];
        let blocks = vec![Block {
            operations: vec![0],
            ..Block::default()
        }];
        let result = plan(
            &operations,
            &[],
            &[],
            &analyze(&operations, blocks, &[], &[]),
        );
        assert_eq!(
            result,
            Err(WebError::Undefined {
                operation: Some(0),
                register: 1,
                component: 0
            })
        );
    }
}
