//! Ordinary external-reference preparation before local scheduling.
//!
//! Builds descriptor-order references; the original assigns
//! external virtuals before local packing. This bridge preserves logical web
//! identities and adds physical pins. Raw/selected tables and partial VF pool
//! policies retain their existing path until their earlier lifecycle is ported.

use super::*;
use crate::allocator::external_allocation::{self, Masks, Mode, Reference, Table};

fn eligible_register(program: &Source, graph: &flow::Graph, family: Family, id: usize) -> bool {
    program.registers.named(id).family == family && !graph.constants.contains(&id)
}

fn prepare_tables(
    program: &Source,
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    family: Family,
) -> Vec<Table> {
    let owners: BTreeMap<_, _> = region
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(block, value)| value.operations.iter().map(move |&id| (id, block)))
        .collect();
    let mut masks = vec![BTreeMap::<usize, Masks>::new(); region.blocks.len()];
    for (block_id, block) in region.blocks.iter().enumerate() {
        if !analysis.reachable.contains(&block_id) {
            continue;
        }
        for &id in &block.operations {
            let op = &region.instructions[id];
            for access in &op.effects.reads {
                if eligible_register(program, graph, family, access.register) {
                    masks[block_id].entry(access.register).or_default().input |= access.mask;
                }
            }
            for access in &op.effects.writes {
                if eligible_register(program, graph, family, access.register) {
                    masks[block_id].entry(access.register).or_default().output |= access.mask;
                }
            }
        }
    }
    for read in &analysis.reads {
        if !eligible_register(program, graph, family, read.register) {
            continue;
        }
        let Some(&consumer) = owners.get(&read.operation) else {
            continue;
        };
        if !analysis.reachable.contains(&consumer) {
            continue;
        }
        let component = 1 << read.component;
        if read.definitions.iter().any(|definition| match definition {
            Definition::Input => true,
            Definition::Operation(writer) => owners.get(writer) != Some(&consumer),
            Definition::Uninitialized => false, // strict flow validation precedes this stage
        }) {
            masks[consumer]
                .entry(read.register)
                .or_default()
                .external_input |= component;
        }
        for definition in &read.definitions {
            let Definition::Operation(writer) = definition else {
                continue;
            };
            if let Some(&producer) = owners.get(writer)
                && producer != consumer
            {
                masks[producer]
                    .entry(read.register)
                    .or_default()
                    .external_output |= component;
            }
        }
    }
    // An EXIT observation is external even when its last setter is in the
    // same compiler block: the original represents that interface separately.
    for output in &analysis.outputs {
        if !eligible_register(program, graph, family, output.register) {
            continue;
        }
        for definition in &output.definitions {
            let Definition::Operation(writer) = definition else {
                continue;
            };
            if let Some(&producer) = owners.get(writer) {
                masks[producer]
                    .entry(output.register)
                    .or_default()
                    .external_output |= 1 << output.component;
            }
        }
    }
    for (block_id, block_masks) in masks.iter_mut().enumerate() {
        if !analysis.reachable.contains(&block_id) {
            continue;
        }
        let mut input = BTreeMap::<usize, u8>::new();
        let mut output = BTreeMap::<usize, u8>::new();
        for access in &analysis.live_in[block_id] {
            if eligible_register(program, graph, family, access.register) {
                *input.entry(access.register).or_default() |= access.mask;
            }
        }
        for access in &analysis.live_out[block_id] {
            if eligible_register(program, graph, family, access.register) {
                *output.entry(access.register).or_default() |= access.mask;
            }
        }
        for (register, live_in) in input {
            let written = block_masks.get(&register).map_or(0, |masks| masks.output);
            let through = live_in & output.get(&register).copied().unwrap_or(0) & !written;
            if through != 0 {
                block_masks.entry(register).or_default().through |= through;
            }
        }
    }
    region
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(block_id, block)| {
            if !analysis.reachable.contains(&block_id) {
                return None;
            }
            let mut order = Vec::new();
            let mut seen = BTreeSet::new();
            // Parsed operands retain the original descriptor order, unlike the
            // read-first effective operand view used by the web helper.
            for &id in &block.operations {
                for register in region.instructions[id].operands.iter().flatten() {
                    if masks[block_id].contains_key(register) && seen.insert(*register) {
                        order.push(*register);
                    }
                }
            }
            // Live-through references need not have a concrete operand here.
            // Their deterministic suffix does not reorder discovered operands.
            order.extend(
                masks[block_id]
                    .keys()
                    .copied()
                    .filter(|id| !seen.contains(id)),
            );
            Some(Table {
                mode: Mode::Unscheduled,
                references: order
                    .into_iter()
                    .map(|register| Reference {
                        register,
                        masks: masks[block_id][&register],
                        ranges: Vec::new(),
                        write_rows: BTreeSet::new(),
                    })
                    .collect(),
            })
        })
        .collect()
}

pub(super) fn assign(
    program: &Source,
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
) -> Result<Region, CompileError> {
    let mut result = region.clone();
    if region
        .blocks
        .iter()
        .any(|block| block.selected_table.is_some())
        || region
            .instructions
            .iter()
            .any(|op| op.instruction.mode == ParseMode::Raw)
        || program
            .allowed_vf
            .iter()
            .any(|&mask| mask != 0 && mask != 15)
    {
        return Ok(result);
    }
    // Original pool IDs 8 (VI) and 9 (VF) are visited in that order.
    for family in [Family::Integer, Family::Vector] {
        let tables = prepare_tables(program, region, graph, analysis, family);
        let mut initial = BTreeMap::new();
        for reference in tables.iter().flat_map(|table| &table.references) {
            let register = program.registers.named(reference.register);
            if let Some(base) = register
                .physical
                .or_else(|| region.forced.get(&reference.register).copied())
            {
                initial.insert(reference.register, base);
            }
        }
        let pool = if family == Family::Vector {
            &program.allowed_vf
        } else {
            &program.allowed_vi
        };
        let available: Vec<_> = pool
            .iter()
            .enumerate()
            .map(|(id, &mask)| id != 0 && mask != 0)
            .collect();
        let assignments = external_allocation::assign(
            if family == Family::Vector { 4 } else { 1 },
            &available,
            &tables,
            &initial,
        )
        .map_err(|failure| error(0, failure.to_string()))?;
        for (register, base) in assignments {
            if !initial.contains_key(&register) {
                result.forced.insert(register, base);
            }
        }
    }
    Ok(result)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> (Source, Region, flow::Graph, flow::Analysis) {
        let mut program = Source::new(&Options::default());
        program.parse_program(parser::parse(text).unwrap()).unwrap();
        let region = program.regions.remove(0);
        let graph = build_flow_graph(&program, &region).unwrap();
        let analysis = graph.analyze().unwrap();
        assert!(analysis.undefined.is_empty());
        (program, region, graph, analysis)
    }

    fn id(program: &Source, family: Family, name: &str) -> usize {
        program.registers.ids[&(family, name.into())]
    }

    #[test]
    fn descriptor_order_and_external_masks_survive_a_pure_through_stream() {
        let (program, region, graph, analysis) = source(
            ".init_vi_all\n\
             .init_vf_all\n\
             --enter\n\
             in_vi base(vi01)\n\
             in_vf vf01\n\
             --endenter\n\
             iaddiu pointer,base,3\n\
             b gap\n\
             gap:\n\
             add unused,vf01,vf01\n\
             b next\n\
             next:\n\
             lq result,0(pointer)\n\
             --exit\n\
             out_vf result(vf02)\n\
             --endexit\n",
        );
        let pointer = id(&program, Family::Integer, "pointer");
        let base = id(&program, Family::Integer, "base");
        let tables = prepare_tables(&program, &region, &graph, &analysis, Family::Integer);
        assert_eq!(tables[0].references[0].register, pointer);
        assert_eq!(tables[0].references[1].register, base);
        let reference = |block: usize| {
            tables[block]
                .references
                .iter()
                .find(|r| r.register == pointer)
                .unwrap()
        };
        assert_eq!(reference(0).masks.output, 1);
        assert_eq!(reference(0).masks.external_output, 1);
        assert_eq!(
            reference(1).masks,
            Masks {
                through: 1,
                ..Masks::default()
            }
        );
        assert_eq!(reference(2).masks.input, 1);
        assert_eq!(reference(2).masks.external_input, 1);
        let assigned = assign(&program, &region, &graph, &analysis).unwrap();
        assert_eq!(assigned.forced[&pointer], 2);
    }

    #[test]
    fn same_block_exit_is_external_but_a_local_temporary_is_not() {
        let (program, region, graph, analysis) = source(
            ".init_vf_all\n\
             --enter\n\
             in_vf a(vf01)\n\
             --endenter\n\
             mul scratch,a,a\n\
             add result,scratch,a\n\
             --exit\n\
             out_vf result(vf02)\n\
             --endexit\n",
        );
        let tables = prepare_tables(&program, &region, &graph, &analysis, Family::Vector);
        let temporary = id(&program, Family::Vector, "scratch");
        let result = id(&program, Family::Vector, "result");
        let temp_ref = tables[0]
            .references
            .iter()
            .find(|r| r.register == temporary)
            .unwrap();
        let output_ref = tables[0]
            .references
            .iter()
            .find(|r| r.register == result)
            .unwrap();
        assert!(!temp_ref.masks.is_external());
        assert_eq!(output_ref.masks.external_output, 15);
        let assigned = assign(&program, &region, &graph, &analysis).unwrap();
        assert!(!assigned.forced.contains_key(&temporary));
    }

    #[test]
    fn external_exhaustion_does_not_become_a_local_scheduling_retry() {
        let (program, region, graph, analysis) = source(
            ".init_vi vi01\n\
             .init_vf_all\n\
             --enter\n\
             in_vi base(vi01)\n\
             --endenter\n\
             iaddiu pointer,base,3\n\
             b next\n\
             next:\n\
             lq result,0(pointer)\n\
             --exit\n\
             out_vf result(vf02)\n\
             --endexit\n",
        );
        let failure = match assign(&program, &region, &graph, &analysis) {
            Err(failure) => failure,
            Ok(_) => panic!("external output must not reuse the stream's forced input"),
        };
        assert_eq!(failure.kind, CompileErrorKind::General);
        assert_eq!(failure.exit_status(), 255);
    }

    #[test]
    fn selected_raw_and_partial_pool_lifecycles_keep_the_existing_assignment_path() {
        let ordinary = ".init_vi_all\n\
                        .init_vf_all\n\
                        --enter\n\
                        in_vi base(vi01)\n\
                        --endenter\n\
            iaddiu pointer,base,3\n\
            b next\n\
            next:\n\
            lq result,0(pointer)\n\
            --exit\n\
            out_vf result(vf02)\n\
            --endexit\n";
        let (mut program, mut region, graph, analysis) = source(ordinary);
        let existing = region.forced.clone();
        // The ordinary source would gain a new external pointer pin. A
        // selected table instead belongs to the earlier allocation lifecycle.
        assert!(
            assign(&program, &region, &graph, &analysis)
                .unwrap()
                .forced
                .len()
                > existing.len()
        );
        region.blocks[0].selected_table = Some(SelectedTable {
            rows: Vec::new(),
            mode: TableMode::Linear,
            installed: false,
        });
        assert_eq!(
            assign(&program, &region, &graph, &analysis).unwrap().forced,
            existing
        );
        region.blocks[0].selected_table = None;
        program.allowed_vf[2] = 1;
        assert_eq!(
            assign(&program, &region, &graph, &analysis).unwrap().forced,
            existing
        );

        let raw = ordinary.replace(
            "iaddiu pointer,base,3\n",
            ".vsm\n\
             nop iaddiu pointer,base,3\n\
             .endvsm\n",
        );
        let (program, region, graph, analysis) = source(&raw);
        assert_eq!(
            assign(&program, &region, &graph, &analysis).unwrap().forced,
            region.forced
        );
    }
}
