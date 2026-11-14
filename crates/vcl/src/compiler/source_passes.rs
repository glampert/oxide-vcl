//! Source-level passes before scheduling: copy normalization, immediate
//! coalescing, redundant-branch removal, pruning and register webs.

use super::*;

/// Copy detection skips fixed raw tables, which keep their encodings.
fn copies_allowed(source: &Region, block: &Block) -> bool {
    block.selected_table.as_ref().map_or_else(
        || {
            block.operations.iter().all(|&id| {
                source.instructions[id].instruction.mode == ParseMode::Vcl
                    || source.instructions[id].instruction.raw_reorder
            })
        },
        |table| table.mode != TableMode::Fixed,
    )
}

/// Detect copies after pruning, when secondary flag-output observations are
/// known. Fixed raw tables retain their original instruction encodings.
pub(super) fn normalize_copies(
    program: &mut Source,
    source: &Region,
) -> Result<Region, CompileError> {
    use crate::analysis::copies::{self, Kind};
    let graph = build_flow_graph(program, source)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    let mut result = source.clone();
    for block in &source.blocks {
        if !copies_allowed(source, block) {
            continue;
        }
        for &id in &block.operations {
            let located = &source.instructions[id];
            if located.instruction.generated_copy.is_some() {
                continue;
            }
            let third_is_pool_zero = located
                .operands
                .get(2)
                .copied()
                .flatten()
                .is_some_and(|register| program.registers.named(register).physical == Some(0));
            let numeric_immediate = located
                .instruction
                .operands
                .get(2)
                .and_then(|operand| operand.immediate.as_deref())
                .and_then(|text| crate::parser::expression::evaluate(text).ok())
                .and_then(|value| value.number())
                .map(|value| value.to_i32());
            let ordinary = located.operands.first().copied().flatten();
            let secondary_outputs_live = located
                .effects
                .writes
                .iter()
                .filter(|write| Some(write.register) != ordinary)
                .any(|write| {
                    analysis.reads.iter().any(|read| {
                        read.register == write.register
                            && write.mask & (1 << read.component) != 0
                            && read.definitions.contains(&Definition::Operation(id))
                    }) || analysis.outputs.iter().any(|output| {
                        output.register == write.register
                            && output.definitions.contains(&Definition::Operation(id))
                    })
                });
            let Some(kind) = copies::detect(copies::Facts {
                instruction: located.instruction.metadata.index,
                mask: located.instruction.mask.0,
                third_is_pool_zero,
                numeric_immediate,
                secondary_outputs_live,
            }) else {
                continue;
            };
            let Some(destination) = ordinary else {
                return Err(error(located.line, CompileMessage::MoveDestinationUnbound));
            };
            if program.registers.named(destination).physical == Some(0) {
                return Err(CompileError {
                    line: located.line,
                    message: CompileMessage::ZeroCopyDestination.into(),
                    kind: CompileErrorKind::ZeroCopyDestination,
                });
            }
            let mut instruction = located.instruction.clone();
            let descriptor = match kind {
                Kind::Integer => id::IMOVE,
                Kind::Vector => id::VMOVE,
            };
            instruction.metadata = &instruction::INSTRUCTIONS[usize::from(descriptor)];
            instruction.mnemonic = instruction.metadata.name.into();
            instruction.operands.truncate(2);
            instruction.generated_copy = Some(kind);
            // DetectMoves constructs a fresh instruction without source M/D/T.
            // The retained copy-iaddiu-debug probe confirms this for D.
            instruction.modifiers = 0;
            result.instructions[id] = program.lower(instruction, located.line)?;
        }
    }
    Ok(result)
}

pub(super) fn coalesce_immediates(
    program: &mut Source,
    source: &Region,
) -> Result<Region, CompileError> {
    use crate::analysis::induction;
    let mut result = source.clone();
    let order: Vec<_> = result
        .blocks
        .iter()
        .flat_map(|block| block.operations.iter().copied())
        .collect();
    let owners: BTreeMap<_, _> = result
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(block, value)| value.operations.iter().map(move |&id| (id, block)))
        .collect();
    for &id in &order {
        let original = induction_view(id, &result.instructions[id]);
        let normalized = induction::normalize_iaddi(original);
        if original != normalized {
            rewrite_induction(
                program,
                &mut result,
                &induction::Rewrite {
                    before: original,
                    after: normalized,
                    immediate_operand: 2,
                },
            )?;
        }
    }

    // All supported rewrites retain register operands, masks and latencies.
    // Reaching-definition identities therefore remain valid while numeric
    // constants move. Rebuild flow later when copy normalization changes IR.
    let graph = build_flow_graph(program, &result)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    let sites = immediate_consumer_sites(program, &result, &order, &owners, &analysis);
    let escaping: BTreeSet<_> = analysis
        .outputs
        .iter()
        .flat_map(|output| &output.definitions)
        .filter_map(|definition| match definition {
            Definition::Operation(id) => Some(*id),
            _ => None,
        })
        .collect();
    let limit = order.len().saturating_mul(order.len()).saturating_add(1);
    for _ in 0..limit {
        let mut changed = false;
        for &id in &order {
            changed |= coalesce_producer(program, &mut result, id, &sites, &escaping)?;
        }
        if !changed {
            return Ok(result);
        }
    }

    Err(internal(
        0,
        "VI immediate coalescing exceeded its transformation budget",
    ))
}

/// The induction analysis' view of an instruction: its opcode and the value
/// of its immediate operand.
fn induction_view(id: usize, located: &Located) -> crate::analysis::induction::Operation {
    let operand = match located.instruction.metadata.index {
        id::IADDI | id::IADDIU | id::ISUBIU => 2,
        _ => 1,
    };
    let immediate = located
        .instruction
        .operands
        .get(operand)
        .and_then(|operand| operand.immediate.as_deref())
        .and_then(|source| crate::parser::expression::evaluate(source).ok())
        .and_then(|value| value.number())
        .map(|number| number.to_i32());
    crate::analysis::induction::Operation {
        id,
        instruction: located.instruction.metadata.index,
        immediate,
    }
}

/// Apply an induction rewrite to its instruction, lowering it again.
fn rewrite_induction(
    program: &mut Source,
    region: &mut Region,
    change: &crate::analysis::induction::Rewrite,
) -> Result<(), CompileError> {
    let id = change.after.id;
    let mut instruction = region.instructions[id].instruction.clone();
    instruction.metadata = &instruction::INSTRUCTIONS[usize::from(change.after.instruction)];
    instruction.mnemonic = instruction.metadata.name.into();
    let operand = &mut instruction.operands[usize::from(change.immediate_operand)];
    operand.immediate = change.after.immediate.map(|value| value.to_string());
    if operand.class == class::IMMEDIATE {
        operand.text = operand.immediate.clone().unwrap_or_default();
    }
    region.instructions[id] = program.lower(instruction, region.instructions[id].line)?;
    Ok(())
}

/// Per producer, its VI readers: (consumer, operand, reaching setters).
type ConsumerSites = BTreeMap<usize, Vec<(usize, u8, Vec<crate::analysis::induction::Definition>)>>;

/// Where each producer's value is read as a VI operand.
fn immediate_consumer_sites(
    program: &Source,
    result: &Region,
    order: &[usize],
    owners: &BTreeMap<usize, usize>,
    analysis: &flow::Analysis,
) -> ConsumerSites {
    use crate::analysis::induction::Definition as Setter;
    let reads: BTreeMap<_, _> = analysis
        .reads
        .iter()
        .map(|read| {
            (
                (read.operation, read.register, read.component),
                &read.definitions,
            )
        })
        .collect();
    let mut sites = ConsumerSites::new();
    for &consumer in order {
        // A dead clone (an expanded part's unread copy) is pruned before
        // this cleanup in the original and is no user.
        if !analysis.live_operations.contains(&consumer) {
            continue;
        }
        let block = &result.blocks[owners[&consumer]];
        let flow_id = flow_owner(result, block, consumer);
        for (operand_id, operand) in result.instructions[consumer]
            .instruction
            .operands
            .iter()
            .enumerate()
        {
            if operand.direction != Direction::Read {
                continue;
            }
            let Some(register) = result.instructions[consumer].operands[operand_id] else {
                continue;
            };
            if program.registers.named(register).family != Family::Integer {
                continue;
            }
            let Some(definitions) = reads.get(&(flow_id, register, 0)) else {
                continue;
            };
            let reaching: Vec<_> = definitions
                .iter()
                .map(|definition| match definition {
                    Definition::Operation(id) => Setter::Operation(*id),
                    _ => Setter::Input,
                })
                .collect();
            for definition in *definitions {
                if let Definition::Operation(producer) = definition {
                    sites.entry(*producer).or_default().push((
                        consumer,
                        operand_id as u8,
                        reaching.clone(),
                    ));
                }
            }
        }
    }
    sites
}

/// Plan and apply the coalescing of one producer's immediate into its
/// consumers; whether anything changed.
fn coalesce_producer(
    program: &mut Source,
    result: &mut Region,
    id: usize,
    sites: &ConsumerSites,
    escaping: &BTreeSet<usize>,
) -> Result<bool, CompileError> {
    use crate::analysis::induction::{self, Consumer, User};
    let producer = induction_view(id, &result.instructions[id]);
    if !matches!(producer.instruction, 110 | 112)
        || producer.immediate.is_none_or(|value| value == 0)
    {
        return Ok(false);
    }
    let mut users: Vec<_> = sites
        .get(&id)
        .into_iter()
        .flatten()
        .map(|(consumer, operand, reaching)| {
            User::Instruction(Consumer {
                operation: induction_view(*consumer, &result.instructions[*consumer]),
                read_operand: *operand,
                reaching: reaching.clone(),
            })
        })
        .collect();
    if escaping.contains(&id) {
        users.push(User::EscapingOutput);
    }
    crate::trace_if!(
        coalesce_trace,
        "coalesce {id} {} line {} users {users:?}",
        result.instructions[id].instruction.mnemonic,
        result.instructions[id].line
    );

    let Some(plan) = induction::plan(producer, &users).map_err(|e| internal(0, e))? else {
        return Ok(false);
    };
    for change in std::iter::once(&plan.producer).chain(&plan.consumers) {
        rewrite_induction(program, result, change)?;
    }
    Ok(true)
}

/// Redundant-branch removal of the original: an ordinary (non-raw,
/// non-call) branch whose every outgoing reference is the next reachable
/// stream becomes a fallthrough. `None` when nothing changed.
pub(super) fn drop_redundant_branches(
    program: &Source,
    region: &Region,
) -> Result<Option<Region>, CompileError> {
    let graph = build_flow_graph(program, region)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    let mut result = region.clone();
    let mut changed = false;
    for &id in &analysis.reachable {
        let block = &region.blocks[id];
        let End::Branch { call, .. } = block.end else {
            continue;
        };
        // A call into the next block goes too when its link register is not
        // read afterwards (observed from vcl 1.4beta7 for a tagged JALR,
        // `audit-indirect-jalr-explicit`; a live link value is not observed).
        if call {
            let Some(&branch) = block.operations.last() else {
                continue;
            };
            let link_live = region.instructions[branch]
                .effects
                .writes
                .iter()
                .any(|write| {
                    analysis.live_out[id]
                        .iter()
                        .any(|live| live.register == write.register && live.mask & write.mask != 0)
                });
            if link_live {
                continue;
            }
        }
        let raw = block
            .selected_table
            .as_ref()
            .is_some_and(|table| table.installed || !movable_table(block))
            || block.operations.iter().any(|&operation| {
                region.instructions[operation].instruction.mode == ParseMode::Raw
            });
        if raw {
            continue;
        }
        // A fallthrough here reaches the physically next block, so an
        // unreachable block in between (vu_3323147251's dead entry after
        // `b L_03e0`) leaves the jump to the emission-stage deletion.
        let next = analysis.reachable.range((id + 1)..).next().copied();
        let successors = &graph.blocks[id].successors;
        if next != Some(id + 1)
            || successors.is_empty()
            || !successors.iter().all(|&t| Some(t) == next)
        {
            continue;
        }
        result.blocks[id].end = End::Fallthrough;
        result.blocks[id]
            .operations
            .retain(|&op| !region.instructions[op].instruction.metadata.is_branch());
        changed = true;
    }
    Ok(changed.then_some(result))
}

pub(super) fn prune_source_region(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
) -> Result<Region, CompileError> {
    use crate::analysis::prune;
    let expanded = expand_for_pruning(region);
    let ids = expanded_register_ids(program, &expanded);
    let edges = use_edges(region, analysis, &expanded, &ids);
    let roots = live_roots(analysis, &expanded, &ids);
    let keep = region
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(id, located)| located.effects.side_effect.then_some(id))
        .collect();
    let liveness =
        prune::propagate(&expanded, &edges, &roots, &keep).map_err(|e| internal(0, e))?;

    let mut result = region.clone();
    for (id, pruned) in liveness.pruned.iter().enumerate() {
        apply_pruned(program, &mut result.instructions[id], pruned, &ids[id]);
    }
    for block in &mut result.blocks {
        block.operations.retain(|&id| liveness.retained[id]);
    }
    Ok(result)
}

/// Every instruction of the region in the pruning analysis' operand form.
fn expand_for_pruning(region: &Region) -> Vec<crate::analysis::prune::ExpandedInstruction> {
    use crate::analysis::prune::{self, ExpandedOperand};
    let mut expanded: Vec<_> = region
        .instructions
        .iter()
        .map(|located| prune::expand(&located.instruction))
        .collect();
    // Address updates are hidden outputs synthesized by the original hooks.
    for instruction in &mut expanded {
        let updates: Vec<_> = instruction
            .operands
            .iter()
            .filter(|operand| {
                matches!(
                    operand.class,
                    class::ADDRESS_INCREMENT | class::ADDRESS_DECREMENT
                )
            })
            .map(|operand| ExpandedOperand {
                class: class::INTEGER,
                direction: Direction::Write,
                latency: 1,
                implicit: true,
                ..operand.clone()
            })
            .collect();
        instruction.operands.extend(updates);
    }
    expanded
}

/// The program register of each expanded operand, if it has one.
fn expanded_register_ids(
    program: &Source,
    expanded: &[crate::analysis::prune::ExpandedInstruction],
) -> Vec<Vec<Option<usize>>> {
    expanded
        .iter()
        .map(|instruction| {
            instruction
                .operands
                .iter()
                .map(|operand| {
                    let family = family(operand.class)?;
                    let source = operand.register.as_ref()?;
                    let name = physical(source, family)
                        .filter(|_| family != Family::Hardware)
                        .map_or_else(
                            || source.to_ascii_lowercase(),
                            |id| physical_name(family, id).to_ascii_lowercase(),
                        );
                    program.registers.ids.get(&(family, name)).copied()
                })
                .collect()
        })
        .collect()
}

/// The producer-to-consumer edges of every register component read.
fn use_edges(
    region: &Region,
    analysis: &flow::Analysis,
    expanded: &[crate::analysis::prune::ExpandedInstruction],
    ids: &[Vec<Option<usize>>],
) -> Vec<crate::analysis::prune::UseEdge> {
    use crate::analysis::prune::UseEdge;
    let reads: BTreeMap<_, _> = analysis
        .reads
        .iter()
        .map(|read| {
            (
                (read.operation, read.register, read.component),
                &read.definitions,
            )
        })
        .collect();
    let owners: BTreeMap<_, _> = region
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .operations
                .iter()
                .map(move |&id| (id, flow_owner(region, block, id)))
        })
        .collect();
    let mut edges = Vec::new();
    for (consumer, instruction) in expanded.iter().enumerate() {
        let flow_id = owners.get(&consumer).copied().unwrap_or(consumer);
        for (input, operand) in instruction
            .operands
            .iter()
            .enumerate()
            .filter(|(_, o)| o.direction == Direction::Read)
        {
            let Some(register) = ids[consumer][input] else {
                continue;
            };
            for part in 0..7 {
                let components = 1 << part;
                if operand.mask & components == 0 {
                    continue;
                }
                let Some(definitions) = reads.get(&(flow_id, register, part)) else {
                    continue;
                };
                for definition in *definitions {
                    let Definition::Operation(producer) = definition else {
                        continue;
                    };
                    for (output, setter) in expanded[*producer].operands.iter().enumerate() {
                        if setter.direction == Direction::Write
                            && ids[*producer][output] == Some(register)
                            && setter.mask & components != 0
                        {
                            edges.push(UseEdge {
                                producer: *producer,
                                output_operand: output,
                                consumer,
                                input_operand: input,
                                components,
                            });
                        }
                    }
                }
            }
        }
    }
    edges
}

/// The writes that reach the region's outputs.
fn live_roots(
    analysis: &flow::Analysis,
    expanded: &[crate::analysis::prune::ExpandedInstruction],
    ids: &[Vec<Option<usize>>],
) -> Vec<crate::analysis::prune::LiveRoot> {
    use crate::analysis::prune::LiveRoot;
    let mut roots = Vec::new();
    for output in &analysis.outputs {
        for definition in &output.definitions {
            let Definition::Operation(id) = definition else {
                continue;
            };
            for (operand, setter) in expanded[*id].operands.iter().enumerate() {
                if setter.direction == Direction::Write
                    && ids[*id][operand] == Some(output.register)
                    && setter.mask & (1 << output.component) != 0
                {
                    roots.push(LiveRoot {
                        instruction: *id,
                        output_operand: operand,
                        components: 1 << output.component,
                    });
                }
            }
        }
    }
    roots
}

/// Replace an instruction with its pruned form and recompute its effects.
fn apply_pruned(
    program: &Source,
    located: &mut Located,
    pruned: &crate::analysis::prune::Pruned,
    ids: &[Option<usize>],
) {
    located.instruction = pruned.instruction.clone();
    let mut zero: BTreeSet<_> = pruned
        .zero_outputs
        .iter()
        .copied()
        .filter(|&operand| located.instruction.operands[operand].direction == Direction::Write)
        .collect();
    // A dead VF result loses its source binding when the original web is
    // pruned, including a source-spelled physical destination. The later
    // flag-cap pass can assign a fresh scratch destination if necessary.
    if !located.effects.side_effect {
        for (index, operand) in pruned.expanded.iter().enumerate() {
            // Class 11 is the xyz-only destination of OPMSUB.
            if matches!(operand.class, class::VECTOR | class::VECTOR_XYZ)
                && operand.direction == Direction::Write
                && pruned.live_output_masks[index] == 0
                && located.instruction.operands[operand.source_operand].direction
                    == Direction::Write
                && operand
                    .register
                    .as_deref()
                    .and_then(|name| physical(name, Family::Vector))
                    .is_none_or(|id| program.allowed_vf[id] & operand.mask == operand.mask)
            {
                zero.insert(operand.source_operand);
            }
        }
    }
    for &operand in &zero {
        let kind = family(located.instruction.operands[operand].class).unwrap_or(Family::Vector);
        let name = if kind == Family::Integer {
            "vi00"
        } else {
            "vf00"
        };
        located.operands[operand] = program
            .registers
            .ids
            .get(&(kind, name.to_string()))
            .copied();
    }

    let mut effects = Effects {
        side_effect: located.effects.side_effect,
        ..Effects::default()
    };
    for (index, operand) in pruned.expanded.iter().enumerate() {
        let register = if zero.contains(&operand.source_operand) {
            located.operands[operand.source_operand]
        } else {
            ids[index]
        };
        let Some(register) = register else { continue };
        let mask = if operand.direction == Direction::Read {
            pruned.input_masks[index]
        } else {
            operand.mask
        };
        if mask == 0 {
            continue;
        }
        let access = Access { register, mask };
        match operand.direction {
            Direction::Read => effects.reads.push(access),
            Direction::Write => {
                effects.writes.push(access);
                effects
                    .latencies
                    .insert(register, operand.latency.max(0) as u16);
            }
            Direction::Unknown => {}
        }
    }
    located.effects = effects;
}

/// Replace source-name identities with connected definition/use webs. A
/// physical spelling is pinned only when it is outside the allocatable pool;
/// explicit ENTER/EXIT bindings pin their connected web independently.
pub(super) fn construct_register_webs(
    program: &mut Source,
    region: &Region,
    analysis: &flow::Analysis,
) -> Result<Region, CompileError> {
    use crate::analysis::web;
    let context = WebContext { program, region };
    let active: BTreeSet<_> = region
        .blocks
        .iter()
        .flat_map(|b| &b.operations)
        .copied()
        .collect();
    let mut operations = Vec::new();
    for (id, located) in region.instructions.iter().enumerate() {
        let mut operands = Vec::new();
        if active.contains(&id) {
            for (direction, accesses) in [
                (Direction::Read, &located.effects.reads),
                (Direction::Write, &located.effects.writes),
            ] {
                for access in accesses {
                    let operand = context.web_operand(id, located, direction, access, &operands);
                    operands.push(operand);
                }
            }
        }
        operations.push(operands);
    }
    let (input_indices, inputs) = context.web_inputs();
    let (output_indices, outputs) = context.web_outputs();

    // Flow transfers a raw pair's reads at its first operation. The web builder
    // still needs the same old definitions under each operand's source ID.
    let mut web_analysis = analysis.clone();
    for block in &region.blocks {
        for &id in &block.operations {
            let owner = flow_owner(region, block, id);
            if owner != id {
                let copies: Vec<_> = analysis
                    .reads
                    .iter()
                    .filter(|read| read.operation == owner)
                    .cloned()
                    .map(|mut read| {
                        read.operation = id;
                        read
                    })
                    .collect();
                web_analysis.reads.extend(copies);
            }
        }
    }
    let plan = web::plan(&operations, &inputs, &outputs, &web_analysis);
    trace_missing_web_read(program, region, &web_analysis, &plan);
    let plan = plan.map_err(|e| match e {
        // One web reaches two interface bindings (`regweb`).
        web::WebError::ConflictingBases { first, second } => error(
            0,
            CompileMessage::ConflictingBases {
                first: program.registers.named(first).name.to_ascii_uppercase(),
                second: program.registers.named(second).name.to_ascii_uppercase(),
            },
        ),
        e => internal(0, e),
    })?;

    let mut result = region.clone();
    result.forced.clear();
    let web_registers = create_web_registers(program, region, &plan, &mut result);
    for (id, located) in result.instructions.iter_mut().enumerate() {
        if !active.contains(&id) {
            continue;
        }
        let mapped: Vec<_> = operations[id]
            .iter()
            .enumerate()
            .map(|(index, operand)| {
                plan.operands[id][index].map_or(operand.register, |web| web_registers[web])
            })
            .collect();
        remap_operands(program, located, &operations[id], &mapped);
        remap_effects(located, &mapped);
    }
    for (index, &source) in input_indices.iter().enumerate() {
        if let Some(web) = plan.inputs[index] {
            result.inputs[source].register = web_registers[web];
        }
    }
    for (index, &(block, source)) in output_indices.iter().enumerate() {
        if let Some(web) = plan.outputs[index] {
            result.blocks[block].outputs[source].register = web_registers[web];
        }
    }
    Ok(result)
}

/// The program and region a web plan is built from.
struct WebContext<'a> {
    program: &'a Source,
    region: &'a Region,
}

impl WebContext<'_> {
    fn forwardable(&self, id: usize, mask: u8) -> bool {
        let program = self.program;
        let register = program.registers.named(id);
        match register.family {
            Family::Hardware => false,
            Family::Vector | Family::Integer => register.physical.is_none_or(|physical| {
                let pool = if register.family == Family::Vector {
                    &program.allowed_vf
                } else {
                    &program.allowed_vi
                };
                physical != 0 && pool[physical] & mask == mask
            }),
        }
    }

    fn base_id(&self, id: usize, base: usize) -> Option<usize> {
        let family = self.program.registers.named(id).family;
        self.program
            .registers
            .ids
            .get(&(family, physical_name(family, base).to_ascii_lowercase()))
            .copied()
    }

    fn interface_base(&self, id: usize) -> Option<usize> {
        let register = self.program.registers.named(id);
        register
            .physical
            .or_else(|| self.region.forced.get(&id).copied())
            .and_then(|base| self.base_id(id, base))
    }

    /// The web operand of one register access of an instruction;
    /// `operands` are those already built for the instruction.
    fn web_operand(
        &self,
        id: usize,
        located: &Located,
        direction: Direction,
        access: &Access,
        operands: &[crate::analysis::web::Operand],
    ) -> crate::analysis::web::Operand {
        let linked_operand = (direction == Direction::Write
            && located
                .instruction
                .operands
                .iter()
                .enumerate()
                .any(|(index, operand)| {
                    matches!(
                        operand.class,
                        class::ADDRESS_INCREMENT | class::ADDRESS_DECREMENT
                    ) && located.operands[index] == Some(access.register)
                }))
        .then(|| {
            operands.iter().position(|operand| {
                operand.direction == Direction::Read && operand.register == access.register
            })
        })
        .flatten();
        // FCAND/FCEQ/FCOR decode only `vi01` (operand class 34):
        // its web stays on VI01 although the spelling is pooled.
        let forced_base = located
            .instruction
            .operands
            .iter()
            .enumerate()
            .any(|(index, operand)| {
                operand.class == class::INTEGER_FLAG
                    && located.operands[index] == Some(access.register)
            })
            .then(|| self.interface_base(access.register))
            .flatten()
            .or_else(|| {
                self.region
                    .pinned
                    .get(&(id, direction == Direction::Write, access.register))
                    .copied()
            });
        crate::analysis::web::Operand {
            register: access.register,
            mask: access.mask,
            direction,
            forwardable: self.forwardable(access.register, access.mask),
            forced_base,
            linked_operand,
        }
    }

    /// The region's forwardable inputs, with their indices in the region.
    fn web_inputs(&self) -> (Vec<usize>, Vec<crate::analysis::web::Binding>) {
        use crate::analysis::web::Binding;
        let region = self.region;
        let input_indices: Vec<_> = region
            .inputs
            .iter()
            .enumerate()
            .filter(|(_, access)| self.forwardable(access.register, access.mask))
            .map(|(index, _)| index)
            .collect();
        let inputs: Vec<_> = input_indices
            .iter()
            .map(|&index| {
                let access = region.inputs[index];
                Binding {
                    register: access.register,
                    mask: access.mask,
                    forced_base: self.interface_base(access.register),
                }
            })
            .collect();
        (input_indices, inputs)
    }

    /// The forwardable block outputs, with their (block, index) positions.
    fn web_outputs(&self) -> (Vec<(usize, usize)>, Vec<crate::analysis::web::ExitBinding>) {
        use crate::analysis::web::ExitBinding;
        let region = self.region;
        let output_indices: Vec<_> = region
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(block, value)| {
                value
                    .outputs
                    .iter()
                    .enumerate()
                    .map(move |(index, access)| (block, index, access))
            })
            .filter(|(_, _, access)| self.forwardable(access.register, access.mask))
            .map(|(block, index, _)| (block, index))
            .collect();
        let outputs: Vec<_> = output_indices
            .iter()
            .map(|&(block, index)| {
                let access = region.blocks[block].outputs[index];
                let forced_base = match region.blocks[block].output_forced.get(&access.register) {
                    Some(&base)
                        if self
                            .program
                            .registers
                            .named(access.register)
                            .physical
                            .is_none() =>
                    {
                        self.base_id(access.register, base)
                    }
                    _ => self.interface_base(access.register),
                };
                ExitBinding {
                    block,
                    register: access.register,
                    mask: access.mask,
                    forced_base,
                }
            })
            .collect();
        (output_indices, outputs)
    }
}

/// Print the details of a missing read in the web plan (`VCL_WEB_DEBUG`).
fn trace_missing_web_read(
    program: &Source,
    region: &Region,
    web_analysis: &flow::Analysis,
    plan: &Result<crate::analysis::web::Plan, crate::analysis::web::WebError>,
) {
    use crate::analysis::web;
    if let Err(web::WebError::MissingRead {
        operation,
        register,
        component,
    }) = plan
        && crate::debug::switches().web_debug
    {
        let located = &region.instructions[*operation];
        crate::trace!(
            "missing read: op {operation} line {} {:?} reg {} ({}) c {component}",
            located.line,
            located.instruction.mnemonic,
            register,
            program.registers.named(*register).name
        );
        crate::trace!(
            "  operands {:?}",
            located
                .instruction
                .operands
                .iter()
                .map(|o| (o.text.clone(), o.mask.0, o.direction))
                .collect::<Vec<_>>()
        );
        crate::trace!(
            "  reads {:?}",
            web_analysis
                .reads
                .iter()
                .filter(|r| r.operation == *operation)
                .map(|r| (
                    program.registers.named(r.register).name.clone(),
                    r.component
                ))
                .collect::<Vec<_>>()
        );
        crate::trace!(
            "  effects {:?}",
            located
                .effects
                .reads
                .iter()
                .map(|a| (program.registers.named(a.register).name.clone(), a.mask))
                .collect::<Vec<_>>()
        );
        let block = region
            .blocks
            .iter()
            .position(|b| b.operations.contains(operation));
        crate::trace!(
            "  block {block:?} reachable {:?} owner {}",
            block.map(|b| web_analysis.reachable.contains(&b)),
            block.map_or(0, |b| flow_owner(region, &region.blocks[b], *operation))
        );
        if let Some(b) = block {
            let ops = &region.blocks[b].operations;
            crate::trace!(
                "  block ops {} with reads {}",
                ops.len(),
                ops.iter()
                    .filter(|&&o| web_analysis.reads.iter().any(|r| r.operation == o))
                    .count()
            );
            crate::trace!(
                "  block mode {:?} raw lines {:?}",
                region.instructions[*operation].instruction.mode,
                region.blocks[b].selected_table.is_some()
            );
        }
    }
}

/// Create a virtual register for every web of the plan; their IDs. Records
/// each web's source register and forced base in `result`.
fn create_web_registers(
    program: &mut Source,
    region: &Region,
    plan: &crate::analysis::web::Plan,
    result: &mut Region,
) -> Vec<usize> {
    let mut web_registers = Vec::new();
    for (index, web) in plan.webs.iter().enumerate() {
        let original = *web.registers.first().expect("web has a source identity");
        let family = program.registers.named(original).family;
        // A later pass (after loop expansion) builds webs from the previous
        // pass's `__web_N` registers, whose names it reuses: keep the source
        // register the earlier web came from.
        let original = region
            .web_sources
            .get(&original)
            .copied()
            .unwrap_or(original);
        let id = program.registers.get(&format!("__web_{index}"), family);
        result.web_sources.insert(id, original);
        if let Some(base) = web.forced_base {
            result.forced.insert(
                id,
                program
                    .registers
                    .named(base)
                    .physical
                    .expect("physical binding"),
            );
        }
        web_registers.push(id);
    }
    web_registers
}

/// Point an instruction's operands at the webs of its effective operands.
fn remap_operands(
    program: &Source,
    located: &mut Located,
    effective_operands: &[crate::analysis::web::Operand],
    mapped: &[usize],
) {
    for (index, operand) in located.instruction.operands.iter().enumerate() {
        let Some(old) = located.operands[index] else {
            continue;
        };
        // An instruction may read one register in two operands from
        // different webs (`mulw.xyz v, v, vw`): match the components.
        let same = |effective: &&crate::analysis::web::Operand| {
            effective.register == old && effective.direction == operand.direction
        };
        let found = effective_operands
            .iter()
            .enumerate()
            .find(|(_, effective)| same(effective) && effective.mask == operand.mask.0)
            .or_else(|| effective_operands.iter().enumerate().find(|(_, e)| same(e)));
        if let Some((effective, _)) = found {
            located.operands[index] = Some(mapped[effective]);
        } else if program.registers.named(old).physical.is_none()
            && program.registers.named(old).family != Family::Hardware
        {
            // A pruned virtual operand has no strand. Original late field
            // materialization substitutes its pool's zero register.
            let kind = program.registers.named(old).family;
            located.operands[index] = program
                .registers
                .ids
                .get(&(kind, physical_name(kind, 0).to_ascii_lowercase()))
                .copied();
        }
    }
}

/// Point an instruction's register accesses at the webs, keeping the write
/// latencies.
fn remap_effects(located: &mut Located, mapped: &[usize]) {
    let read_count = located.effects.reads.len();
    let old_latencies = std::mem::take(&mut located.effects.latencies);
    for (index, access) in located.effects.reads.iter_mut().enumerate() {
        access.register = mapped[index];
    }
    for (index, access) in located.effects.writes.iter_mut().enumerate() {
        let latency = old_latencies.get(&access.register).copied().unwrap_or(1);
        access.register = mapped[read_count + index];
        located.effects.latencies.insert(access.register, latency);
    }
}

/// A web bound to register 0 (an ENTER or EXIT mapping to VF00 or VI00,
/// given or inherited) is that constant register, observed from vcl
/// 1.4beta7 (`dead-write-zero-*` probes):
///
/// - A copy into it, or an IADDI, IADDIU or ISUBIU with a numeric immediate
///   (any value, not only zero), fails the copy detector
///   (`copy-output-zero-binding`, `dead-write-zero-iaddiu`).
/// - Any other write into it is removed like a write spelled VF00, so its
///   readers read VF00 and what only fed it is pruned in turn
///   (`dead-write-zero-read`, `dead-write-zero-chain`). A write whose flags
///   are read stays, as one spelled VF00 does (`dead-write-zero-flags`).
///
/// The binding is the web's, not the name's: one exit of several can bind
/// its web to VF00 (`dead-write-zero-exits`). vcl gathers webs before it
/// detects copies, so a web bound to two bases fails first.
///
/// Returns the region with such operands spelled as the zero register and
/// their ENTER/EXIT entries dropped, for another pruning pass, or `None`
/// when no web is bound to register 0.
pub(super) fn spell_zero_webs(
    program: &mut Source,
    region: &Region,
    analysis: &flow::Analysis,
) -> Result<Option<Region>, CompileError> {
    let webs = construct_register_webs(&mut program.clone(), region, analysis)?;
    let zero: BTreeSet<usize> = webs
        .forced
        .iter()
        .filter(|&(_, &base)| base == 0)
        .map(|(&web, _)| web)
        .collect();
    if zero.is_empty() {
        return Ok(None);
    }
    for block in &region.blocks {
        let allowed = copies_allowed(region, block);
        for &id in &block.operations {
            let located = &webs.instructions[id];
            if !located
                .operands
                .first()
                .copied()
                .flatten()
                .is_some_and(|destination| zero.contains(&destination))
            {
                continue;
            }
            let numeric_immediate = matches!(
                located.instruction.metadata.index,
                id::IADDI | id::IADDIU | id::ISUBIU
            ) && located
                .instruction
                .operands
                .get(2)
                .and_then(|operand| operand.immediate.as_deref())
                .and_then(|text| crate::parser::expression::evaluate(text).ok())
                .and_then(|value| value.number())
                .is_some();
            if located.instruction.generated_copy.is_some() || (allowed && numeric_immediate) {
                return Err(CompileError {
                    line: located.line,
                    message: CompileMessage::ZeroCopyDestination.into(),
                    kind: CompileErrorKind::ZeroCopyDestination,
                });
            }
        }
    }
    let mut result = region.clone();
    for (id, located) in webs.instructions.iter().enumerate() {
        let spelled: Vec<_> = located
            .operands
            .iter()
            .enumerate()
            .filter(|(_, register)| register.is_some_and(|register| zero.contains(&register)))
            .map(|(index, _)| index)
            .collect();
        if spelled.is_empty() {
            continue;
        }
        let mut instruction = region.instructions[id].instruction.clone();
        for index in spelled {
            let operand = &mut instruction.operands[index];
            let kind = family(operand.class).unwrap_or(Family::Vector);
            operand.register = Some(physical_name(kind, 0).to_ascii_lowercase());
        }
        result.instructions[id] = program.lower(instruction, located.line)?;
    }
    let kept = |access: &Access| !zero.contains(&access.register);
    result.inputs = region
        .inputs
        .iter()
        .zip(&webs.inputs)
        .filter(|(_, web)| kept(web))
        .map(|(access, _)| *access)
        .collect();
    result.forced.retain(|register, _| {
        result
            .inputs
            .iter()
            .any(|access| access.register == *register)
            || !region
                .inputs
                .iter()
                .any(|access| access.register == *register)
    });
    for (block, web_block) in result.blocks.iter_mut().zip(&webs.blocks) {
        block.outputs = block
            .outputs
            .iter()
            .zip(&web_block.outputs)
            .filter(|(_, web)| kept(web))
            .map(|(access, _)| *access)
            .collect();
        let outputs = &block.outputs;
        block
            .output_forced
            .retain(|register, _| outputs.iter().any(|access| access.register == *register));
    }
    Ok(Some(result))
}

/// Hardware register identity from the original register table
///: global ID, parts, nonblocking ID and throughput tracking.
pub(super) fn hardware_register(name: &str) -> Option<(u8, u8, u8, bool)> {
    Some(match name.to_ascii_uppercase().as_str() {
        "ACC" => (0x30, 4, 0, false),
        "I" => (0x31, 1, 0, false),
        "Q" => (0x32, 1, 2, true),
        "TT_NP" => (0x33, 1, 0, false),
        "P" => (0x34, 1, 1, true),
        "R" => (0x35, 1, 3, false),
        "CLIP" => (0x36, 1, 4, false),
        "STAT_ALU" => (0x37, 1, 6, false),
        "STAT_DIV" => (0x38, 1, 7, false),
        "STAT_ALU_S" => (0x39, 1, 0, false),
        "STAT_DIV_S" => (0x3a, 1, 0, false),
        "MAC" => (0x3b, 1, 10, false),
        "CLIPA" => (0x3c, 1, 5, false),
        "EXT" => (0x3d, 1, 0, false),
        _ => return None,
    })
}
