//! Extent reduction of generated tables: guaranteed rows, escaping delays
//! and tail separation between ordinary streams.

use super::*;

pub(super) fn movable_table(block: &Block) -> bool {
    block
        .selected_table
        .as_ref()
        .is_some_and(|table| table.mode == TableMode::Linear)
}

/// Lower-bound issue span that survives later motion and compression.
/// Raw debris can still co-pack and move its producer, so only one issue
/// pair is credited there, even for a currently longer branch frame.
pub(super) fn guaranteed_rows(
    region: &Region,
    block: &Block,
    rows: &[[Option<usize>; 2]],
    start: usize,
) -> usize {
    if block
        .selected_table
        .as_ref()
        .is_some_and(|table| table.mode == TableMode::RawLinear)
    {
        return usize::from(rows[start..].iter().flatten().any(Option::is_some));
    }
    let protected = matches!(block.end, End::Branch { .. })
        || block
            .selected_table
            .as_ref()
            .is_some_and(|table| table.mode == TableMode::Fixed)
        || block
            .operations
            .iter()
            .any(|&id| region.instructions[id].instruction.mode == ParseMode::Raw);
    if protected {
        rows.len() - start
    } else {
        rows[start..]
            .iter()
            .filter(|pair| pair.iter().any(Option::is_some))
            .count()
    }
}

/// Readiness required at an outgoing stream boundary. The original computes
/// per-component external constraints; this adapter uses a conservative earliest
/// successor read and retains extra separation for conditional VI operands.
pub(super) fn escaping_delay(
    program: &Source,
    region: &Region,
    block: &Block,
    analysis: &flow::Analysis,
    id: usize,
    write: Access,
) -> Option<u16> {
    let outside: Vec<_> = analysis
        .reads
        .iter()
        .filter(|read| {
            read.register == write.register
                && write.mask & (1 << read.component) != 0
                && read.definitions.contains(&Definition::Operation(id))
                && !block.operations.contains(&read.operation)
        })
        .collect();
    if outside.is_empty() {
        return None;
    }
    let mut separation = region.instructions[id]
        .effects
        .latencies
        .get(&write.register)
        .copied()
        .unwrap_or(1);
    if program.registers.named(write.register).family == Family::Integer
        && outside.iter().any(|read| {
            region.blocks.iter().any(|next| {
                next.operations.iter().any(|&consumer| {
                    region.instructions[consumer]
                        .instruction
                        .metadata
                        .is_branch()
                        && flow_owner(region, next, consumer) == read.operation
                        && region.instructions[consumer]
                            .effects
                            .reads
                            .iter()
                            .any(|access| {
                                access.register == read.register
                                    && access.mask & (1 << read.component) != 0
                            })
                })
            })
        })
    {
        separation = separation.max(2);
    }
    Some(separation)
}

/// The original's external-information step only gets outgoing debt from consumers
/// with selected images/VSM tables. Future ordinary UTA
/// users and synthetic EXIT observations leave an ordinary VI tail zero.
/// Keep the fuller conservative bound for already scheduled/raw users and
/// other resource families, whose through locks need additional tracking.
pub(super) fn ordinary_tail_separation(
    program: &Source,
    region: &Region,
    block: &Block,
    analysis: &flow::Analysis,
    scheduled_blocks: usize,
    id: usize,
    write: Access,
) -> u16 {
    let future_ordinary_users = analysis
        .reads
        .iter()
        .filter(|read| {
            read.register == write.register
                && write.mask & (1 << read.component) != 0
                && read.definitions.contains(&Definition::Operation(id))
                && !block.operations.contains(&read.operation)
        })
        .all(|read| {
            region
                .blocks
                .iter()
                .enumerate()
                .find(|(_, consumer)| consumer.operations.contains(&read.operation))
                .is_some_and(|(index, consumer)| {
                    index >= scheduled_blocks
                        && consumer.selected_table.is_none()
                        && consumer
                            .operations
                            .iter()
                            .all(|&op| region.instructions[op].instruction.mode == ParseMode::Vcl)
                })
        });
    if program.registers.named(write.register).family == Family::Integer && future_ordinary_users {
        1 // zero extra tail, while preserving the instruction's own issue row
    } else {
        escaping_delay(program, region, block, analysis, id, write).unwrap_or(1)
    }
}

pub(super) fn reduce_generated_rows(
    program: &Source,
    region: &Region,
    block: &Block,
    analysis: &flow::Analysis,
    live: &[usize],
    problem: &SchedulingProblem,
    rows: &[[Option<usize>; 2]],
) -> Result<Vec<[Option<usize>; 2]>, CompileError> {
    let local: BTreeMap<_, _> = live.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let mut cleanup = problem.clone();
    // Existing selection barriers put unrelated operations before a
    // source branch. Late reduction may use its physical delay pair;
    // actual branch-input edges have positive VI latency and remain.
    let sinks: Vec<_> = live
        .iter()
        .enumerate()
        .filter_map(|(local_id, &id)| {
            let located = &region.instructions[id];
            (!located.instruction.metadata.is_memory()
                && !located.effects.side_effect
                && !located.instruction.metadata.is_branch()
                && located.instruction.modifiers == 0
                && !located.effects.writes.is_empty()
                && located
                    .effects
                    .reads
                    .iter()
                    .chain(&located.effects.writes)
                    .all(|access| {
                        program.registers.named(access.register).family == Family::Integer
                    })
                && located
                    .effects
                    .latencies
                    .values()
                    .all(|&latency| latency <= 1))
            .then_some(crate::scheduler::linear_cleanup::Sink {
                operation: local_id,
                exit_separation: 1,
            })
        })
        .collect();
    cleanup.dependencies.retain(|edge| {
        !(edge.latency == 0
            && cleanup.operations[edge.to].is_branch
            && sinks.iter().any(|sink| sink.operation == edge.from))
    });
    // Marked noninterlocked external results retain their selected
    // output debt. Ordinary VF values use the hardware interlock.
    let mut minimum_rows = 0;
    let mut sinks = sinks;
    for (row, pair) in rows.iter().enumerate() {
        for &id in pair.iter().flatten() {
            for write in &region.instructions[id].effects.writes {
                let Some(separation) = escaping_delay(program, region, block, analysis, id, *write)
                else {
                    continue;
                };
                let family = program.registers.named(write.register).family;
                if let Some(sink) = sinks.iter_mut().find(|sink| live[sink.operation] == id) {
                    sink.exit_separation = sink.exit_separation.max(separation);
                }
                if family != Family::Vector {
                    minimum_rows = minimum_rows.max(row + usize::from(separation));
                }
            }
        }
    }
    let local_rows: Vec<_> = rows
        .iter()
        .map(|pair| pair.map(|id| id.map(|id| local[&id])))
        .collect();
    let reduced =
        crate::scheduler::linear_cleanup::reduce(&cleanup, &local_rows, &sinks, minimum_rows)
            .map_err(|e| error(0, e.to_string()))?
            .iter()
            .map(|pair| pair.map(|id| id.map(|id| live[id])))
            .collect();
    Ok(reduced)
}
