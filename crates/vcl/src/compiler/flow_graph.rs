//! The program's stream-level flow graph, including returns inferred for
//! untagged `jr` instructions.

use super::*;

pub(super) fn build_flow_graph(
    program: &Source,
    region: &Region,
) -> Result<flow::Graph, CompileError> {
    let labels = stream_labels(region)?;
    let operations = flow_operations(program, region)?;

    let mut successors_of: Vec<Vec<usize>> = Vec::new();
    let mut call_returns: Vec<Option<usize>> = Vec::new();
    // Indirect branches whose targets are inferred: (block, register, tagged).
    let mut indirect = Vec::new();
    for (id, block) in region.blocks.iter().enumerate() {
        let next = (id + 1 < region.blocks.len()).then_some(id + 1);
        let (successors, call_return) =
            block_edges(program, region, &labels, (id, block), next, &mut indirect)?;
        successors_of.push(successors);
        call_returns.push(call_return);
    }
    let returns = infer_returns(region, &mut successors_of, &call_returns, &indirect)?;

    let mut blocks = Vec::new();
    for (id, block) in region.blocks.iter().enumerate() {
        blocks.push(flow::Block {
            operations: block.operations.clone(),
            successors: successors_of[id].clone(),
            outputs: block.outputs.clone(),
            timing: block
                .selected_table
                .as_ref()
                .and_then(|table| table_timing(program, region, block, &table.rows)),
            call_return: call_returns[id],
            returns: returns[id].clone(),
        });
    }

    let constants = program
        .registers
        .values
        .iter()
        .enumerate()
        .filter_map(|(id, r)| (r.family != Family::Hardware && r.physical == Some(0)).then_some(id))
        .collect();
    Ok(flow::Graph {
        operations,
        blocks,
        entry: 0,
        inputs: region.inputs.clone(),
        constants,
    })
}

/// Map each stream label (lowercased) to its block, rejecting duplicates.
fn stream_labels(region: &Region) -> Result<BTreeMap<String, usize>, CompileError> {
    let mut labels = BTreeMap::new();
    for (id, block) in region.blocks.iter().enumerate() {
        for label in &block.labels {
            if labels.insert(label.to_ascii_lowercase(), id).is_some() {
                return Err(error(
                    0,
                    CompileMessage::DuplicateStreamLabel {
                        label: label.to_string(),
                    },
                ));
            }
        }
    }
    Ok(labels)
}

/// The flow operation of every instruction, with raw issue bundles merged.
fn flow_operations(
    program: &Source,
    region: &Region,
) -> Result<Vec<flow::Operation>, CompileError> {
    let mut operations: Vec<flow::Operation> = region
        .instructions
        .iter()
        .map(|located| flow::Operation {
            reads: located.effects.reads.clone(),
            writes: located.effects.writes.clone(),
            side_effect: located.effects.side_effect,
        })
        .collect();

    // One raw issue bundle fetches both halves' sources before either half
    // writes. Move lower reads to the first transfer event for flow analysis;
    // retain the two original operations for scheduling and text emission.
    for block in &region.blocks {
        let pairs: Vec<_> = if let Some(rows) = &block.selected_table {
            rows.rows
                .iter()
                .map(|row| {
                    row.iter()
                        .flatten()
                        .copied()
                        .filter(|id| block.operations.contains(id))
                        .collect::<Vec<_>>()
                })
                .collect()
        } else {
            block.operations.chunks(2).map(<[usize]>::to_vec).collect()
        };
        for pair in &pairs {
            if let [upper, lower] = pair.as_slice()
                && (block.selected_table.is_some()
                    || (region.instructions[*upper].instruction.mode == ParseMode::Raw
                        && region.instructions[*upper].line == region.instructions[*lower].line))
            {
                merge_issue_pair(program, region, &mut operations, *upper, *lower)?;
            }
        }
    }
    Ok(operations)
}

/// Move the lower half's reads onto the upper one; both writing the same
/// register is an error.
fn merge_issue_pair(
    program: &Source,
    region: &Region,
    operations: &mut [flow::Operation],
    upper: usize,
    lower: usize,
) -> Result<(), CompileError> {
    let lower_reads = std::mem::take(&mut operations[lower].reads);
    operations[upper].reads.extend(lower_reads);
    if operations[upper].writes.iter().any(|a| {
        operations[lower].writes.iter().any(|b| {
            a.register == b.register && program.registers.named(a.register).physical != Some(0)
        })
    }) {
        return Err(error(
            region.instructions[upper].line,
            CompileMessage::PipesWriteSameRegister,
        ));
    }
    Ok(())
}

/// The successors and call-return block of one block, from how it ends.
/// Indirect branches needing inference are appended to `indirect`.
fn block_edges(
    program: &Source,
    region: &Region,
    labels: &BTreeMap<String, usize>,
    (id, block): (usize, &Block),
    next: Option<usize>,
    indirect: &mut Vec<(usize, usize, bool)>,
) -> Result<(Vec<usize>, Option<usize>), CompileError> {
    let mut successors = Vec::new();
    let mut call_return = None;
    match &block.end {
        End::Fallthrough | End::Continue => {
            if let Some(next) = next {
                successors.push(next);
            }
        }
        End::Exit { .. } => {}
        End::Branch {
            target,
            tags,
            conditional,
            call,
        } => {
            if let Some(target) = target {
                let Some(&target) = labels.get(&target.to_ascii_lowercase()) else {
                    return Err(error(
                        0,
                        CompileMessage::UnknownBranchTarget {
                            target: target.to_string(),
                        },
                    ));
                };
                successors.push(target);
            } else {
                successors =
                    indirect_targets(program, region, labels, (id, block), tags, *call, indirect)?;
            }
            // A call reaches only its subroutine; the subroutine's
            // return edge leads back to the stream after it.
            if *call {
                call_return = next;
            } else if *conditional && let Some(next) = next {
                successors.push(next);
            }
        }
    }
    Ok((successors, call_return))
}

/// The tagged targets of an indirect branch; records the branch for return
/// inference when it jumps through an integer register.
fn indirect_targets(
    program: &Source,
    region: &Region,
    labels: &BTreeMap<String, usize>,
    (id, block): (usize, &Block),
    tags: &[String],
    call: bool,
    indirect: &mut Vec<(usize, usize, bool)>,
) -> Result<Vec<usize>, CompileError> {
    let mut successors = Vec::new();
    let mut tagged = BTreeSet::new();
    for tag in tags {
        let target = *labels.get(&tag.to_ascii_lowercase()).ok_or_else(|| {
            error(
                0,
                CompileMessage::UnknownBranchTag {
                    tag: tag.to_string(),
                },
            )
        })?;
        // A second link to the same stream fails (observed
        // from vcl 1.4beta7, `audit-indirect-duplicate`).
        if !tagged.insert(target) {
            return Err(error(0, CompileMessage::DuplicateBranchTag));
        }
        successors.push(target);
    }

    let branch = block.operations.last().map(|&id| &region.instructions[id]);
    let register = branch.and_then(|located| {
        located
            .effects
            .reads
            .iter()
            .map(|r| r.register)
            .find(|&r| program.registers.named(r).family == Family::Integer)
    });
    match register {
        Some(register) if !call => indirect.push((id, register, !tags.is_empty())),
        _ if tags.is_empty() => {
            return Err(error(0, CompileMessage::UntaggedIndirectBranch));
        }
        _ => {}
    }
    Ok(successors)
}

/// Return edges of indirect branches: each setter of the
/// branch register reaching it backward must be a call's link write, and the
/// branch returns to the stream after that call. Any other setter, including
/// an entry input, needs explicit tags; a path without a setter adds nothing.
/// Return edges add paths, so this runs to a fixpoint before judging.
pub(super) fn infer_returns(
    region: &Region,
    successors: &mut [Vec<usize>],
    call_returns: &[Option<usize>],
    indirect: &[(usize, usize, bool)],
) -> Result<Vec<Vec<usize>>, CompileError> {
    let mut returns = vec![Vec::new(); successors.len()];
    let inputs: BTreeSet<usize> = region.inputs.iter().map(|a| a.register).collect();
    let writes = |id: usize, register: usize| {
        region.instructions[id]
            .effects
            .writes
            .iter()
            .any(|w| w.register == register)
    };
    loop {
        let mut predecessors = vec![Vec::new(); successors.len()];
        for (block, next) in successors.iter().enumerate() {
            for &successor in next {
                predecessors[successor].push(block);
            }
        }
        let mut changed = false;
        let mut failed = false;
        for &(branch, register, tagged) in indirect {
            let mut found = BTreeSet::new();
            let mut ordinary = false;
            let mut seen = BTreeSet::new();
            let operations = &region.blocks[branch].operations;
            let own = operations[..operations.len().saturating_sub(1)]
                .iter()
                .rev()
                .find(|&&id| writes(id, register));
            let mut queue: VecDeque<usize> = VecDeque::new();
            if own.is_some() {
                ordinary = true;
            } else {
                queue.extend(&predecessors[branch]);
            }
            let mut entry = branch == 0 && own.is_none();
            while let Some(block) = queue.pop_front() {
                if !seen.insert(block) {
                    continue;
                }
                let setter = region.blocks[block]
                    .operations
                    .iter()
                    .rev()
                    .find(|&&id| writes(id, register));
                match setter {
                    Some(&id) => {
                        let link = region.blocks[block].operations.last() == Some(&id)
                            && call_returns[block].is_some();
                        if link {
                            found.insert(call_returns[block].unwrap());
                        } else {
                            ordinary = true;
                        }
                    }
                    None => {
                        entry |= block == 0;
                        queue.extend(&predecessors[block]);
                    }
                }
            }
            if !tagged && (ordinary || (entry && inputs.contains(&register))) {
                failed = true;
            }
            for back in found {
                if !returns[branch].contains(&back) {
                    returns[branch].push(back);
                    if !successors[branch].contains(&back) {
                        successors[branch].push(back);
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            if failed {
                return Err(error(0, CompileMessage::UntaggedIndirectBranch));
            }
            return Ok(returns);
        }
    }
}
