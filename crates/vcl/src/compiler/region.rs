//! Compiling one region: the scheduling candidates tried for it and the
//! emission of the chosen result.

use super::*;

pub(super) fn compile_region(
    program: &Source,
    region: &Region,
    unique: &str,
    options: &Options,
    output: &mut Compiled,
) -> Result<(), CompileError> {
    if !options.disable_loops && region.blocks.iter().any(|block| {
        block.loop_hints.minimum.is_some()
            && matches!(&block.end, End::Branch { target: Some(target), conditional: true, call: false, .. }
                if block.labels.iter().any(|label| label.eq_ignore_ascii_case(target)))
            && block.operations.iter().any(|&id| region.instructions[id].instruction.mode == ParseMode::Raw)
            && block.operations.iter().all(|&id| region.instructions[id].instruction.loop_id == 0)
    }) {
        // An unprefixed .vsmloop remains a raw linear table. With LoopCS,
        // the original tries its loop-work builder, whose clone guard requires UTA.
        // This is an original rejection, not an optimization fallback.
        return Err(internal(0, "normal loop optimization requires an unscheduled source table"));
    }
    let normal = !options.disable_loops
        && region.blocks.iter().any(|block| {
            block.loop_hints.minimum.is_some()
                && block
                    .operations
                    .iter()
                    .all(|&id| region.instructions[id].instruction.mode == ParseMode::Vcl)
        });
    let initial = output.clone();
    match compile_region_round(program, region, unique, options, normal, output) {
        Ok(()) => Ok(()),
        Err(failure) if normal => {
            if std::env::var_os("VCL_LPGEN_TRACE").is_some() {
                crate::trace!("normal loop round failed: {failure:?}");
            }
            *output = initial;
            compile_region_round(program, region, unique, options, false, output)
        }
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy)]
pub(super) struct CandidateOptions {
    pub(super) strategy: Option<schedule::Strategy>,
    pub(super) normal_loops: bool,
}

pub(super) fn compile_region_round(
    program: &Source,
    region: &Region,
    unique: &str,
    options: &Options,
    normal_loops: bool,
    output: &mut Compiled,
) -> Result<(), CompileError> {
    let initial = output.clone();
    let failure = match compile_region_candidate(
        program,
        region,
        unique,
        options,
        CandidateOptions {
            strategy: None,
            normal_loops,
        },
        output,
    ) {
        Ok(()) => return Ok(()),
        Err(failure) if failure.kind == CompileErrorKind::Allocation && !options.dumb => {
            if std::env::var_os("VCL_RETRY_TRACE").is_some() {
                crate::trace!("retry after allocation failure: {failure:?}");
            }
            failure
        }
        Err(failure) => return Err(failure),
    };
    // Allocation is part of candidate acceptance in the original optimizer.
    // Rejecting its first shortest schedule must not reject a
    // source that fits under another recovered scheduling priority.
    let mut best: Option<Compiled> = None;
    for strategy in schedule::Strategy::ALL
        .into_iter()
        .rev()
        .filter(|s| s.allows_linear())
    {
        let mut candidate = initial.clone();
        if compile_region_candidate(
            program,
            region,
            unique,
            options,
            CandidateOptions {
                strategy: Some(strategy),
                normal_loops,
            },
            &mut candidate,
        )
        .is_ok()
            && best
                .as_ref()
                .is_none_or(|previous| candidate.instruction_pairs < previous.instruction_pairs)
        {
            best = Some(candidate);
        }
    }
    if let Some(best) = best {
        *output = best;
        Ok(())
    } else {
        *output = initial;
        Err(failure)
    }
}

/// A schedule: one `[upper, lower]` pair of operation ids per row.
type Rows = Vec<[Option<usize>; 2]>;

/// The scratch destinations picked for uppers that would write VF00:
/// operation to (operand index, original register, scratch register).
type Scratched = BTreeMap<usize, (usize, usize, usize)>;

/// The region after the source passes, with its flow graph and analysis.
struct PreparedRegion {
    program: Source,
    region: Region,
    graph: flow::Graph,
    analysis: flow::Analysis,
}

/// Compiles one region with one set of candidate options, appending its
/// text to `output`.
pub(super) fn compile_region_candidate(
    program: &Source,
    region: &Region,
    unique: &str,
    options: &Options,
    candidate: CandidateOptions,
    output: &mut Compiled,
) -> Result<(), CompileError> {
    let prepared = prepare_region(program, region, unique, candidate)?;
    let program = &prepared.program;
    let region = &prepared.region;
    let graph = &prepared.graph;
    let analysis = &prepared.analysis;
    // External choices are fixed before ordinary local scheduling, rather
    // than being reconsidered by the final lifetime packing score.
    let externally_bound = if options.symbolic {
        region.clone()
    } else {
        external_assignment::assign(program, region, graph, analysis)?
    };
    let region = &externally_bound;

    let EmissionFolds {
        mut emission_blocks,
        to_nop,
        hidden,
    } = fold_exit_streams(program, region, graph, analysis);
    let Scheduled {
        mut schedules,
        notes,
        mut wrangled_tables,
        block_ranges,
        total_rows,
    } = ScheduleContext::new(
        program,
        region,
        options,
        candidate,
        graph,
        analysis,
        &emission_blocks,
        &hidden,
    )
    .run()?;

    let mappings = allocate_region(
        program,
        region,
        options,
        graph,
        analysis,
        &schedules,
        &block_ranges,
        &wrangled_tables,
        total_rows,
    )?;
    // Caps are materialized after allocation. If pruning sent an unused VF
    // result to VF00, reserve an allowed component set unused by this table,
    // redirect that result, and let the ABS cap interlock on it. A fixed
    // kernel gets no cap and keeps VF00.
    let (mut allocated_region, mut scratched) =
        assign_scratch_destinations(program, region, options, analysis, &schedules, &mappings);
    nop_folded_branches(&mut allocated_region, &to_nop)?;
    let region = &allocated_region;
    raw_reduction::reduce_tables(
        program,
        region,
        &mut emission_blocks,
        graph,
        analysis,
        &mut schedules,
        &mappings,
        options,
    )?;
    let last_member = merge_adjacent_streams(
        program,
        region,
        graph,
        analysis,
        &mut emission_blocks,
        &mut schedules,
    );
    // Flag caps run on merged streams: a scratch destination must be free in
    // the whole merged table, not only in its own block.
    let rechosen = rechoose_scratches(
        program,
        region,
        analysis,
        &emission_blocks,
        &schedules,
        &last_member,
        &mappings,
        &mut scratched,
    );
    let region = rechosen.as_ref().unwrap_or(region);
    rewrangle_merged_tables(
        program,
        region,
        options,
        analysis,
        &emission_blocks,
        &schedules,
        &last_member,
        &mut wrangled_tables,
    );
    let mut plans = raw_reduce_tables(
        program,
        region,
        options,
        graph,
        analysis,
        &mappings,
        &last_member,
        &emission_blocks,
        &mut wrangled_tables,
        &mut schedules,
    );
    // A scratch destination exists only for the ABS cap: a producer that got
    // no cap writes VF00 again (the original redirects it while inserting
    // the cap).
    let capped = capped_producers(&plans);
    let reverted = revert_uncapped_scratches(region, &scratched, &capped);
    let region = reverted.as_ref().unwrap_or(region);
    // The original picks the scratch while inserting the cap, so a producer
    // that got no cap reserves nothing: vu_3323147251's uncapped `madd.xyz`
    // held VF16 in the port's first pass, and the capped `sub.x` after it
    // took VF17 where the original takes VF16.
    let repicked = repick_scratches(
        program,
        region,
        analysis,
        &emission_blocks,
        &schedules,
        &last_member,
        &mappings,
        &capped,
        &mut scratched,
        &mut plans,
    );
    let region = repicked.as_ref().unwrap_or(region);
    let repairs = displacement_repairs(region, &schedules, &wrangled_tables);

    let emitter = Emitter {
        program,
        region,
        options,
        analysis,
        mappings: &mappings,
        repairs: &repairs,
        plans: &plans,
        unique,
        referenced: referenced_labels(program, analysis, &emission_blocks, &schedules),
    };
    emitter.emit_blocks(&emission_blocks, &schedules, &notes, output)?;
    emitter.emit_raw_text(output);
    Ok(())
}

/// Runs the source passes and loop expansion, ending with the register webs
/// and the analysis the scheduler works from.
fn prepare_region(
    program: &Source,
    region: &Region,
    unique: &str,
    candidate: CandidateOptions,
) -> Result<PreparedRegion, CompileError> {
    let (expanded_program, source_pruned) = prune_source_stage(program, region, unique)?;
    // Copies present before loop expansion; only these meet the original's pruning
    // identity check (a zero-delta `iaddiu VI03,VI03,0` that coalescing
    // leaves in a ps2gl fast epilogue stays).
    let source_copy_lines: BTreeSet<usize> = source_pruned
        .blocks
        .iter()
        .flat_map(|block| block.operations.iter())
        .map(|&id| &source_pruned.instructions[id])
        .filter(|located| located.instruction.generated_copy.is_some())
        .map(|located| located.line)
        .collect();

    let (copy_program, pruned_region, analysis) =
        expand_and_prune(expanded_program, &source_pruned, unique, candidate)?;
    build_webs(copy_program, &pruned_region, &analysis, &source_copy_lines)
}

/// Rejects a read of a source value that has no definition.
fn check_source_dependencies(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
) -> Result<(), CompileError> {
    // The `clip` fix sets aside CLIPW's reads of clip history that nothing
    // observes, and has the region declare the history instead.
    let unobserved = if program.fixes.contains(Fix::Clip) {
        clip_history::unobserved_reads(program, region, analysis)
    } else {
        BTreeSet::new()
    };
    let set_aside: BTreeSet<(usize, usize)> = unobserved
        .iter()
        .map(|&index| {
            (
                analysis.reads[index].operation,
                analysis.reads[index].register,
            )
        })
        .collect();
    let used_before_set = |operation: Option<usize>, register: usize, component: u8| {
        let line = operation.map_or(0, |id| region.instructions[id].line);
        let component = ["x", "y", "z", "w", "4", "5", "6", "7"][usize::from(component)];
        error(
            line,
            CompileMessage::UsedBeforeSet {
                register: program.registers.named(register).name.clone(),
                component: component.into(),
            },
        )
    };

    if let Some(read) = analysis.reads.iter().find(|read| {
        read.definitions.contains(&Definition::Uninitialized)
            && !set_aside.contains(&(read.operation, read.register))
    }) {
        return Err(used_before_set(
            Some(read.operation),
            read.register,
            read.component,
        ));
    }
    if let Some(undefined) = analysis.undefined.iter().find(|undefined| {
        undefined
            .operation
            .is_none_or(|operation| !set_aside.contains(&(operation, undefined.register)))
    }) {
        return Err(used_before_set(
            undefined.operation,
            undefined.register,
            undefined.component,
        ));
    }

    if let Some(&first) = unobserved.first() {
        let read = &analysis.reads[first];
        let mut error = used_before_set(Some(read.operation), read.register, read.component);
        error.kind = CompileErrorKind::UndeclaredClipHistory;
        return Err(error);
    }
    Ok(())
}

/// Expands explicit raw stages, validates and prunes the source region, and
/// coalesces and normalizes it. Returns the program that holds the
/// registers those passes added.
fn prune_source_stage(
    program: &Source,
    region: &Region,
    unique: &str,
) -> Result<(Source, Region), CompileError> {
    // Original main expands explicit raw stages before its
    // first dependency check. A textual forward read can already
    // have a valid warmup definition in the expanded execution graph.
    let mut raw_program = program.clone();
    let raw_region = expand_software_loops(&mut raw_program, region, unique, false)?;
    let program = &raw_program;
    let region = &raw_region;
    let graph = build_flow_graph(program, region)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    // The original validates source dependencies before its dead-code pass.
    // Unused instructions therefore cannot conceal an undeclared source value.
    check_source_dependencies(program, region, &analysis)?;

    let source_pruned = prune_source_region(program, region, &analysis)?;
    // Deletes a branch whose every target is the next reachable
    // stream, and pruning then drops the code that only fed it
    // (vu_1401563139's `iaddiu vi04, vi00, 1; iand vi04, vi02, vi04` before
    // `ibeq vi04, vi00, L_0130` at L_0130's doorstep).
    let source_pruned = match drop_redundant_branches(program, &source_pruned)? {
        Some(dropped) => {
            let graph = build_flow_graph(program, &dropped)?;
            let analysis = graph.analyze().map_err(|e| internal(0, e))?;
            prune_source_region(program, &dropped, &analysis)?
        }
        None => source_pruned,
    };

    let mut expanded_program = program.clone();
    let source_pruned = coalesce_immediates(&mut expanded_program, &source_pruned)?;
    let source_pruned = normalize_copies(&mut expanded_program, &source_pruned)?;
    Ok((expanded_program, source_pruned))
}

/// Expands the software loops, then prunes, normalizes copies and spells the
/// zero webs. Returns the resulting program, region and analysis.
fn expand_and_prune(
    mut expanded_program: Source,
    source_pruned: &Region,
    unique: &str,
    candidate: CandidateOptions,
) -> Result<(Source, Region, flow::Analysis), CompileError> {
    let expanded_region = expand_software_loops(
        &mut expanded_program,
        source_pruned,
        unique,
        candidate.normal_loops,
    )?;
    let expanded_region = coalesce_immediates(&mut expanded_program, &expanded_region)?;
    let program = &expanded_program;
    let region = &expanded_region;
    let graph = build_flow_graph(program, region)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    if !analysis.undefined.is_empty() {
        trace_undefined(program, region, &analysis);
        return Err(internal(
            0,
            format!(
                "expanded loop has undefined dependencies: {:?}",
                analysis.undefined
            ),
        ));
    }

    let pruned_region = prune_source_region(program, region, &analysis)?;
    let mut copy_program = program.clone();
    let pruned_region = normalize_copies(&mut copy_program, &pruned_region)?;
    let graph = build_flow_graph(&copy_program, &pruned_region)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    let (pruned_region, analysis) =
        match spell_zero_webs(&mut copy_program, &pruned_region, &analysis)? {
            Some(spelled) => {
                let graph = build_flow_graph(&copy_program, &spelled)?;
                let analysis = graph.analyze().map_err(|e| internal(0, e))?;
                let pruned = prune_source_region(&copy_program, &spelled, &analysis)?;
                let graph = build_flow_graph(&copy_program, &pruned)?;
                let analysis = graph.analyze().map_err(|e| internal(0, e))?;
                (pruned, analysis)
            }
            None => (pruned_region, analysis),
        };
    Ok((copy_program, pruned_region, analysis))
}

/// Traces each undefined dependency of an expanded loop (`VCL_LPGEN_TRACE`).
fn trace_undefined(program: &Source, region: &Region, analysis: &flow::Analysis) {
    if std::env::var_os("VCL_LPGEN_TRACE").is_none() {
        return;
    }

    for undefined in &analysis.undefined {
        let located = undefined.operation.map(|id| &region.instructions[id]);
        crate::trace!(
            "undefined {} in {:?} line {:?} block {}",
            program.registers.named(undefined.register).name,
            located.map(|l| &l.instruction.mnemonic),
            located.map(|l| l.line),
            undefined.block
        );
    }
}

/// Splits the registers into webs and drops the copies whose destination and
/// source became one web.
fn build_webs(
    copy_program: Source,
    pruned_region: &Region,
    analysis: &flow::Analysis,
    source_copy_lines: &BTreeSet<usize>,
) -> Result<PreparedRegion, CompileError> {
    let program = &copy_program;
    let mut web_program = program.clone();
    let mut web_region = construct_register_webs(&mut web_program, pruned_region, analysis)?;
    // The original's pruning removes a copy (a move with a move ID) whose
    // destination and source are one register object, i.e. one web: a
    // partial `mulw.y vf03, vf03, vf00w` whose result is read together with
    // the untouched components (vu_268277683). A full overwrite such as a
    // coalesced `iaddiu p, p, 0` starts a web of its own and stays.
    let identities: BTreeSet<usize> = web_region
        .blocks
        .iter()
        .flat_map(|block| block.operations.iter().copied())
        .filter(|&id| {
            let located = &web_region.instructions[id];
            located.instruction.generated_copy.is_some()
                && source_copy_lines.contains(&located.line)
                && located.operands.first().copied().flatten().is_some()
                && located.operands.first() == located.operands.get(1)
        })
        .collect();
    for block in &mut web_region.blocks {
        block.operations.retain(|id| !identities.contains(id));
    }

    let graph = build_flow_graph(&web_program, &web_region)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    if !analysis.undefined.is_empty() {
        return Err(internal(0, "pruning introduced an undefined dependency"));
    }
    Ok(PreparedRegion {
        program: web_program,
        region: web_region,
        graph,
        analysis,
    })
}

/// The blocks as they are emitted, once branches into the exit are folded.
struct EmissionFolds {
    emission_blocks: Vec<Block>,
    /// Folded branches that print as a lower `nop`.
    to_nop: Vec<usize>,
    /// Empty exit streams that print nothing.
    hidden: BTreeSet<usize>,
}

/// Folds branches whose every target is an empty exit stream into exits, and
/// finds the streams that disappear with them.
fn fold_exit_streams(
    program: &Source,
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
) -> EmissionFolds {
    // Emission-edge visibility does not remove data-flow predecessor links.
    let mut emission_blocks = region.blocks.clone();
    let exit_like = exit_like_blocks(region, graph, analysis);
    let mut to_nop = Vec::new();
    let folded = fold_branches_into_exits(
        region,
        graph,
        analysis,
        &exit_like,
        &mut emission_blocks,
        &mut to_nop,
    );
    let hidden: BTreeSet<_> = analysis
        .reachable
        .iter()
        .copied()
        .filter(|&id| {
            id != 0
                && region.blocks[id].operations.is_empty()
                && exit_like.contains(&id)
                && !region.blocks[id]
                    .labels
                    .iter()
                    .any(|label| program.globals.contains(label))
                && analysis
                    .reachable
                    .iter()
                    .filter(|&&pred| graph.blocks[pred].successors.contains(&id))
                    .all(|pred| folded.contains(pred))
        })
        .collect();
    // An installed loop part ends at the exit point itself when that point
    // stays (other epilogues branch to it): it falls through instead.
    for &id in &folded {
        let installed = region.blocks[id]
            .selected_table
            .as_ref()
            .is_some_and(|table| table.installed);
        if installed
            && graph.blocks[id]
                .successors
                .iter()
                .any(|next| !hidden.contains(next))
        {
            emission_blocks[id] = region.blocks[id].clone();
            to_nop.retain(|op| !emission_blocks[id].operations.contains(op));
        }
    }
    EmissionFolds {
        emission_blocks,
        to_nop,
        hidden,
    }
}

/// The reachable blocks that end the program, or only fall into blocks that do.
fn exit_like_blocks(
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
) -> BTreeSet<usize> {
    let mut exit_like: BTreeSet<_> = analysis
        .reachable
        .iter()
        .copied()
        .filter(|&id| matches!(region.blocks[id].end, End::Exit { macro_text: None }))
        .collect();
    loop {
        let before = exit_like.len();
        for &id in &analysis.reachable {
            let block = &region.blocks[id];
            if movable_table(block)
                && block.operations.is_empty()
                && matches!(block.end, End::Fallthrough)
                && !graph.blocks[id].successors.is_empty()
                && graph.blocks[id]
                    .successors
                    .iter()
                    .all(|next| exit_like.contains(next))
            {
                exit_like.insert(id);
            }
        }
        if before == exit_like.len() {
            break;
        }
    }
    exit_like
}

/// Whether the block ends in a branch that stays as written.
fn keeps_raw_branch(region: &Region, id: usize) -> bool {
    // An installed loop part keeps its branch to the exit point.
    matches!(region.blocks[id].end, End::Branch { .. })
        && (region.blocks[id]
            .selected_table
            .as_ref()
            .is_some_and(|table| table.installed || !movable_table(&region.blocks[id]))
            || region.blocks[id].operations.iter().any(|&operation| {
                region.instructions[operation].instruction.mode == ParseMode::Raw
            }))
}

/// Turns the branches into exit streams into exits (or fallthroughs when
/// they only reach the next stream). Returns the folded blocks.
fn fold_branches_into_exits(
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    exit_like: &BTreeSet<usize>,
    emission_blocks: &mut [Block],
    to_nop: &mut Vec<usize>,
) -> BTreeSet<usize> {
    let mut folded = BTreeSet::new();
    for &id in &analysis.reachable {
        let successors = &graph.blocks[id].successors;
        let raw_branch = keeps_raw_branch(region, id);
        let ordinary = !raw_branch
            && matches!(
                region.blocks[id].end,
                End::Fallthrough | End::Branch { call: false, .. }
            );
        if ordinary
            && !successors.is_empty()
            && successors
                .iter()
                .all(|&next| region.blocks[next].operations.is_empty() && exit_like.contains(&next))
        {
            emission_blocks[id].end = End::Exit { macro_text: None };
            // A branch to the physically next stream goes; any other folded
            // branch stays in its slot as a lower `nop`.
            let next = analysis.reachable.range((id + 1)..).next().copied();
            if matches!(region.blocks[id].end, End::Fallthrough)
                || successors.iter().all(|&target| Some(target) == next)
            {
                emission_blocks[id]
                    .operations
                    .retain(|&op| !region.instructions[op].instruction.metadata.is_branch());
            } else {
                to_nop.extend(
                    emission_blocks[id]
                        .operations
                        .iter()
                        .copied()
                        .filter(|&op| region.instructions[op].instruction.metadata.is_branch()),
                );
            }
            folded.insert(id);
        } else if !raw_branch && matches!(region.blocks[id].end, End::Branch { call: false, .. }) {
            let next = analysis.reachable.range((id + 1)..).next().copied();
            if next.is_some() && successors.iter().all(|&target| Some(target) == next) {
                emission_blocks[id].end = End::Fallthrough;
                emission_blocks[id]
                    .operations
                    .retain(|&op| !region.instructions[op].instruction.metadata.is_branch());
            }
        }
    }
    folded
}

/// The scheduled tables of a region.
struct Scheduled {
    /// The rows of each block; none for blocks that print nothing.
    schedules: Vec<Option<Rows>>,
    /// What vcl's optimizer comment reports for each block's table.
    notes: Vec<TableNote>,
    wrangled_tables: BTreeMap<usize, WrangledTable>,
    /// The span of each scheduled block in the program's rows.
    block_ranges: Vec<Option<std::ops::Range<usize>>>,
    total_rows: usize,
}

/// State shared by the per-block scheduling steps.
struct ScheduleContext<'a> {
    program: &'a Source,
    region: &'a Region,
    options: &'a Options,
    candidate: CandidateOptions,
    analysis: &'a flow::Analysis,
    emission_blocks: &'a [Block],
    hidden: &'a BTreeSet<usize>,
    planned: BTreeMap<usize, WrangledTable>,
    successors: Vec<Vec<usize>>,
    owners: BTreeMap<usize, usize>,
    schedules: Vec<Option<Rows>>,
    notes: Vec<TableNote>,
    wrangled_tables: BTreeMap<usize, WrangledTable>,
    block_ranges: Vec<Option<std::ops::Range<usize>>>,
    total_rows: usize,
}

impl<'a> ScheduleContext<'a> {
    /// Plans the wrangled tables and collects what every block needs.
    #[allow(clippy::too_many_arguments)]
    fn new(
        program: &'a Source,
        region: &'a Region,
        options: &'a Options,
        candidate: CandidateOptions,
        graph: &'a flow::Graph,
        analysis: &'a flow::Analysis,
        emission_blocks: &'a [Block],
        hidden: &'a BTreeSet<usize>,
    ) -> Self {
        let planned = if !options.dumb
            && candidate.strategy.is_none()
            && std::env::var_os("VCL_OLD_SCHEDULER").is_none()
        {
            plan_wrangled_tables(
                program,
                region,
                analysis,
                graph,
                emission_blocks,
                hidden,
                options.timid_memory,
            )
        } else {
            BTreeMap::new()
        };
        let successors: Vec<_> = graph
            .blocks
            .iter()
            .map(|block| block.successors.clone())
            .collect();
        let owners: BTreeMap<_, _> = region
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(block, value)| value.operations.iter().map(move |&id| (id, block)))
            .collect();
        ScheduleContext {
            program,
            region,
            options,
            candidate,
            analysis,
            emission_blocks,
            hidden,
            planned,
            successors,
            owners,
            schedules: Vec::new(),
            notes: Vec::new(),
            wrangled_tables: BTreeMap::new(),
            block_ranges: Vec::new(),
            total_rows: 0,
        }
    }

    /// Schedules every reachable, visible block in order.
    fn run(mut self) -> Result<Scheduled, CompileError> {
        let emission_blocks = self.emission_blocks;
        for (block_id, block) in emission_blocks.iter().enumerate() {
            if !self.analysis.reachable.contains(&block_id) || self.hidden.contains(&block_id) {
                self.schedules.push(None);
                self.notes.push(TableNote::default());
                self.block_ranges.push(None);
                continue;
            }
            self.schedule_block(block_id, block)?;
        }
        Ok(Scheduled {
            schedules: self.schedules,
            notes: self.notes,
            wrangled_tables: self.wrangled_tables,
            block_ranges: self.block_ranges,
            total_rows: self.total_rows,
        })
    }

    /// Builds the dependencies of one block, schedules its rows and records
    /// the result.
    fn schedule_block(&mut self, block_id: usize, block: &'a Block) -> Result<(), CompileError> {
        let region = self.region;
        let live: Vec<_> = block
            .operations
            .iter()
            .copied()
            .filter(|id| self.analysis.live_operations.contains(id))
            .collect();
        let local: BTreeMap<_, _> = live.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let mut ops = build_operations(region, &live);
        let mut edges = self.dataflow_edges(block_id, block, &live, &local, &mut ops);
        // Preserve anti/output hazards and conservative memory aliases. Distinct
        // virtual definitions may later be renamed by a fuller web pass.
        add_hazard_edges(region, &live, &mut edges);
        // Retain real hazards separately from the scheduler's source-list
        // branch barrier. Only the final delay-slot exchange may cross it.
        let placement_edges = edges.clone();
        add_branch_barrier(&ops, &mut edges);
        if self.options.dumb
            || live
                .iter()
                .any(|&id| region.instructions[id].instruction.mode == ParseMode::Raw)
        {
            add_source_order_edges(region, &live, &mut edges);
        }
        let mut problem = SchedulingProblem {
            operations: ops,
            dependencies: edges_to_dependencies(edges),
            period: None,
        };
        let raw = !live.is_empty()
            && live
                .iter()
                .all(|&id| region.instructions[id].instruction.mode == ParseMode::Raw);
        let mut note = TableNote::default();
        let mut rows = if let Some(fixed) = &block.selected_table {
            note.kernel_strategy = fixed.kernel_strategy;
            Self::fixed_table_rows(&fixed.rows, &local)
        } else if raw {
            self.raw_table_rows(&live)?
        } else {
            self.optimized_rows(block_id, &live, &mut problem, &mut note)?
        };

        self.reduce_or_install(block_id, block, &live, &problem, &mut rows)?;
        self.pad_block_end(block_id, block, raw, &mut rows);
        if !self.options.dumb
            && !raw
            && block.selected_table.is_none()
            && !self.wrangled_tables.contains_key(&block_id)
            && matches!(block.end, End::Branch { .. })
        {
            self.fill_branch_delay(
                block_id,
                block,
                &live,
                &local,
                placement_edges,
                &problem,
                &mut rows,
            );
        }

        self.block_ranges
            .push(Some(self.total_rows..self.total_rows + rows.len()));
        self.total_rows += rows.len();
        self.schedules.push(Some(rows));
        self.notes.push(note);
        Ok(())
    }

    /// The dependencies between the block's operations that data flow
    /// implies. Reads whose writer is outside the block raise the
    /// operation's earliest cycle instead.
    fn dataflow_edges(
        &self,
        block_id: usize,
        block: &Block,
        live: &[usize],
        local: &BTreeMap<usize, usize>,
        ops: &mut [Operation],
    ) -> BTreeMap<(usize, usize), u16> {
        let region = self.region;
        let mut edges: BTreeMap<(usize, usize), u16> = BTreeMap::new();
        let paired;
        let reads = if block.selected_table.is_some() {
            paired = paired_block_reads(region, self.analysis, block, live);
            &paired
        } else {
            &self.analysis.reads
        };
        for read in reads {
            let Some(&to) = local.get(&read.operation) else {
                continue;
            };
            for definition in &read.definitions {
                let Definition::Operation(writer) = definition else {
                    continue;
                };
                let Some(&from) = local.get(writer) else {
                    let latency =
                        self.incoming_latency(block_id, block, ops[to].is_branch, read, *writer);
                    ops[to].earliest = ops[to].earliest.max(latency);
                    continue;
                };
                // A later setter reaching through a back edge belongs
                // to the previous iteration, not this linear block.
                if from >= to {
                    continue;
                }
                let mut latency = producer_latency(region, *writer, read.register);
                if region.instructions[read.operation]
                    .instruction
                    .metadata
                    .is_branch()
                    && self.program.registers.named(read.register).family == Family::Integer
                {
                    latency = latency.max(2);
                }
                edges
                    .entry((from, to))
                    .and_modify(|old| *old = (*old).max(latency))
                    .or_insert(latency);
            }
        }
        edges
    }

    /// The cycle an operation must wait for a result produced outside its
    /// block.
    fn incoming_latency(
        &self,
        block_id: usize,
        block: &Block,
        to_is_branch: bool,
        read: &flow::Read,
        writer: usize,
    ) -> u16 {
        let region = self.region;
        // A predecessor's slow result must be ready even when
        // its producer belongs to another scheduled block.
        let mut latency = producer_latency(region, writer, read.register);
        // Incoming debt is measured from the predecessor's
        // end, not from its producer's issue cycle. Use the
        // shortest CFG path through guaranteed issue spans;
        // unknown or still-movable intermediates contribute
        // only a conservative duration lower bound.
        let Some(&pred) = self.owners.get(&writer) else {
            return latency;
        };
        let ordinary = |b: &Block| {
            b.selected_table.is_none()
                && b.operations
                    .iter()
                    .all(|&id| region.instructions[id].instruction.mode == ParseMode::Vcl)
        };
        if ordinary(block)
            // Slow VI-to-branch biases need the complete
            // external tracker; retain their old bound.
            && (!to_is_branch || latency <= 1)
            && let Some(Some(rows)) = self.schedules.get(pred)
            && let Some(row) = rows.iter().position(|pair| pair.contains(&Some(writer)))
        {
            if to_is_branch && self.program.registers.named(read.register).family == Family::Integer
            {
                latency = latency.max(2);
            }
            let lengths: Vec<_> = region
                .blocks
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    self.schedules
                        .get(index)
                        .and_then(Option::as_ref)
                        .map_or(0, |table| {
                            guaranteed_rows(region, &self.emission_blocks[index], table, 0)
                        })
                })
                .collect();
            if let Some(path) = crate::scheduler::boundary::minimum_path_length(
                &self.successors,
                &lengths,
                pred,
                block_id,
            ) {
                let elapsed = guaranteed_rows(region, &self.emission_blocks[pred], rows, row)
                    .saturating_add(path);
                latency = latency.saturating_sub(elapsed.min(usize::from(u16::MAX)) as u16);
            }
        }
        latency
    }

    /// The rows of a table the loop optimizer already fixed.
    fn fixed_table_rows(fixed: &[[Option<usize>; 2]], local: &BTreeMap<usize, usize>) -> Rows {
        fixed
            .iter()
            .map(|pair| pair.map(|id| id.filter(|id| local.contains_key(id))))
            .collect()
    }

    /// The rows of raw code: one row per source line, in the pipes written.
    fn raw_table_rows(&mut self, live: &[usize]) -> Result<Rows, CompileError> {
        let region = self.region;
        let mut rows = Vec::new();
        let mut line = None;
        let mut slot = 0;
        for &id in live {
            let located = &region.instructions[id];
            if line != Some(located.line) {
                rows.push([None, None]);
                line = Some(located.line);
                slot = 0;
            }
            if slot > 1 || located.instruction.metadata.issue_mask & (1 << slot) == 0 {
                return Err(error(located.line, CompileMessage::WrongRawPipe));
            }
            if located.instruction.metadata.name != "nop" || located.instruction.modifiers != 0 {
                rows.last_mut().unwrap()[slot] = Some(id);
            }
            slot += 1;
        }
        Ok(rows)
    }

    /// The rows the scheduler picks for an ordinary block: the planned
    /// wrangled table when there is one, otherwise a fresh schedule.
    fn optimized_rows(
        &mut self,
        block_id: usize,
        live: &[usize],
        problem: &mut SchedulingProblem,
        note: &mut TableNote,
    ) -> Result<Rows, CompileError> {
        let region = self.region;
        let mut metrics = schedule::graph_metrics(problem).map_err(|e| internal(0, e))?;
        for (id, metric) in metrics.iter_mut().enumerate() {
            metric.status_inputs = 0;
            metric.mac_inputs = 0;
            for access in &region.instructions[live[id]].effects.reads {
                match self.program.registers.named(access.register).name.as_str() {
                    "STAT_ALU" | "STAT_DIV" => metric.status_inputs += 1,
                    "MAC" => {
                        metric.status_inputs += 1;
                        metric.mac_inputs += 1;
                    }
                    _ => {}
                }
            }
            problem.operations[id].metrics = Some(*metric);
        }

        let config = ScheduleOptions::default();
        let wrangled = self
            .planned
            .remove(&block_id)
            .filter(|table| table.live == live)
            .map(|table| (table.rows.clone(), table));
        if let Some((rows, table)) = wrangled {
            note.strategy = table.strategy;
            note.lower_bound = Some(table.lower_bound);
            self.wrangled_tables.insert(block_id, table);
            return Ok(rows
                .iter()
                .map(|row| row.map(|op| op.map(|id| live[id])))
                .collect::<Vec<_>>());
        }

        let schedule = if self.options.dumb {
            schedule::schedule(
                problem,
                &ScheduleOptions {
                    strategy: schedule::Strategy::Dumb,
                    ..config
                },
            )
        } else if let Some(strategy) = self.candidate.strategy {
            schedule::schedule(problem, &ScheduleOptions { strategy, ..config })
        } else {
            schedule::schedule_best(problem, &config)
        }
        .map_err(|e| internal(0, e))?;
        note.strategy = Some(schedule.strategy);
        Ok(schedule
            .rows
            .iter()
            .map(|row| row.map(|op| op.map(|id| live[id])))
            .collect::<Vec<_>>())
    }

    /// Keeps an installed loop part's planned table, or reduces a movable
    /// table's generated rows.
    fn reduce_or_install(
        &mut self,
        block_id: usize,
        block: &Block,
        live: &[usize],
        problem: &SchedulingProblem,
        rows: &mut Rows,
    ) -> Result<(), CompileError> {
        let installed = block.selected_table.as_ref().is_some_and(|t| t.installed)
            && self
                .planned
                .get(&block_id)
                .is_some_and(|table| table.live == live);
        if installed {
            self.wrangled_tables
                .insert(block_id, self.planned.remove(&block_id).unwrap());
        } else if movable_table(block) {
            *rows = reduce_generated_rows(
                self.program,
                self.region,
                block,
                self.analysis,
                live,
                problem,
                rows,
            )?;
        }
        Ok(())
    }

    /// Adds the padding rows an exit needs, or the branch's empty delay row.
    fn pad_block_end(&self, block_id: usize, block: &Block, raw: bool, rows: &mut Rows) {
        match block.end {
            End::Exit { macro_text: None } | End::Continue => {
                let lower: Vec<_> = rows
                    .iter()
                    .map(|pair| {
                        pair[1].map(|id| {
                            let metadata = self.region.instructions[id].instruction.metadata;
                            crate::analysis::control::LowerInstruction {
                                descriptor_index: metadata.index,
                                memory: metadata.is_memory(),
                                branch: metadata.is_branch(),
                            }
                        })
                    })
                    .collect();
                rows.extend(std::iter::repeat_n(
                    [None, None],
                    crate::analysis::control::required_exit_padding(&lower),
                ));
            }
            End::Branch { .. }
                if !raw
                    && block.selected_table.is_none()
                    && !self.wrangled_tables.contains_key(&block_id) =>
            {
                rows.push([None, None])
            }
            _ => {}
        }
    }

    /// Moves operations into the branch's delay slot where the hazards allow.
    #[allow(clippy::too_many_arguments)]
    fn fill_branch_delay(
        &mut self,
        block_id: usize,
        block: &Block,
        live: &[usize],
        local: &BTreeMap<usize, usize>,
        placement_edges: BTreeMap<(usize, usize), u16>,
        problem: &SchedulingProblem,
        rows: &mut Rows,
    ) {
        let region = self.region;
        let zero_tail =
            zero_tail_operations(self.program, region, block, self.analysis, block_id, live);
        let mut placement_problem = problem.clone();
        placement_problem.dependencies = edges_to_dependencies(placement_edges);
        let local_rows: Vec<_> = rows
            .iter()
            .map(|pair| pair.map(|id| id.map(|id| local[&id])))
            .collect();
        let minimum_rows =
            minimum_tail_rows(self.program, region, block, self.analysis, block_id, rows);
        *rows = crate::scheduler::linear_cleanup::adjust_branch_delay(
            &placement_problem,
            &local_rows,
            &zero_tail,
            minimum_rows,
        )
        .iter()
        .map(|pair| pair.map(|id| id.map(|id| live[id])))
        .collect();
    }
}

/// The scheduler's view of each live operation.
fn build_operations(region: &Region, live: &[usize]) -> Vec<Operation> {
    let mut ops = Vec::new();
    for &id in live {
        let located = &region.instructions[id];
        let mut op = Operation::new(id, located.instruction.metadata.issue_mask);
        // DetectMoves turns an unscheduled vector copy into a two-pipe
        // move. Its upper form is MAX source,source (no flag side effects).
        if located.instruction.metadata.index == id::MOVE
            && located.instruction.mode == ParseMode::Vcl
        {
            op.issue_mask = issue::EITHER;
        }
        op.unit_mask = located.instruction.metadata.unit_mask;
        op.throughput = located.instruction.metadata.throughput.max(1);
        op.reads = located
            .effects
            .reads
            .iter()
            .map(|a| RegisterAccess {
                register: a.register,
                components: a.mask,
            })
            .collect();
        op.writes = located
            .effects
            .writes
            .iter()
            .map(|a| RegisterAccess {
                register: a.register,
                components: a.mask,
            })
            .collect();
        op.is_branch = located.instruction.metadata.is_branch();
        ops.push(op);
    }
    ops
}

/// The block's reads of a fixed table, re-attributed from the flow owner of
/// each operation to the operation itself.
fn paired_block_reads(
    region: &Region,
    analysis: &flow::Analysis,
    block: &Block,
    live: &[usize],
) -> Vec<flow::Read> {
    let mut paired_reads = Vec::new();
    for &id in live {
        let owner = flow_owner(region, block, id);
        for access in &region.instructions[id].effects.reads {
            paired_reads.extend(
                analysis
                    .reads
                    .iter()
                    .filter(|read| {
                        read.operation == owner
                            && read.register == access.register
                            && access.mask & (1 << read.component) != 0
                    })
                    .map(|read| flow::Read {
                        operation: id,
                        ..read.clone()
                    }),
            );
        }
    }
    paired_reads
}

/// The cycles before the writer's result of `register` is available.
fn producer_latency(region: &Region, writer: usize, register: usize) -> u16 {
    region.instructions[writer]
        .effects
        .latencies
        .get(&register)
        .copied()
        .unwrap_or(1)
}

/// Whether any access of `x` touches the same components as one of `y`.
fn accesses_overlap(x: &[Access], y: &[Access]) -> bool {
    x.iter().any(|a| {
        y.iter()
            .any(|b| a.register == b.register && a.mask & b.mask != 0)
    })
}

/// The latency between two writes of the same components.
fn output_hazard_latency(a: &Located, b: &Located) -> u16 {
    a.effects
        .writes
        .iter()
        .filter(|write| {
            b.effects
                .writes
                .iter()
                .any(|other| write.register == other.register && write.mask & other.mask != 0)
        })
        .map(|write| {
            a.effects
                .latencies
                .get(&write.register)
                .copied()
                .unwrap_or(1)
                .saturating_add(1)
                .saturating_sub(
                    b.effects
                        .latencies
                        .get(&write.register)
                        .copied()
                        .unwrap_or(1),
                )
        })
        .max()
        .unwrap_or(1)
}

/// Adds the anti/output hazards and memory-alias edges between the live
/// operations.
fn add_hazard_edges(region: &Region, live: &[usize], edges: &mut BTreeMap<(usize, usize), u16>) {
    for to in 0..live.len() {
        for from in 0..to {
            let a = &region.instructions[live[from]];
            let b = &region.instructions[live[to]];
            if accesses_overlap(&a.effects.reads, &b.effects.writes) {
                edges.entry((from, to)).or_insert(0);
            }
            if accesses_overlap(&a.effects.writes, &b.effects.writes) {
                let latency = output_hazard_latency(a, b);
                edges
                    .entry((from, to))
                    .and_modify(|old| *old = (*old).max(latency))
                    .or_insert(latency);
            }
            if a.instruction.metadata.is_memory() && b.instruction.metadata.is_memory() {
                edges.entry((from, to)).or_insert(1);
            }
        }
    }
}

/// Orders every operation before each branch of the block.
fn add_branch_barrier(ops: &[Operation], edges: &mut BTreeMap<(usize, usize), u16>) {
    for (to, operation) in ops.iter().enumerate() {
        if operation.is_branch {
            for from in 0..to {
                edges.entry((from, to)).or_insert(0);
            }
        }
    }
}

/// Keeps source order between consecutive operations (raw code, `-d`).
fn add_source_order_edges(
    region: &Region,
    live: &[usize],
    edges: &mut BTreeMap<(usize, usize), u16>,
) {
    for to in 1..live.len() {
        let a = &region.instructions[live[to - 1]];
        let b = &region.instructions[live[to]];
        let latency = if a.instruction.mode == ParseMode::Raw && a.line == b.line {
            0
        } else {
            1
        };
        edges
            .entry((to - 1, to))
            .and_modify(|old| *old = (*old).max(latency))
            .or_insert(latency);
    }
}

/// The scheduler's dependency list for an edge map.
fn edges_to_dependencies(edges: BTreeMap<(usize, usize), u16>) -> Vec<Dependency> {
    edges
        .into_iter()
        .map(|((from, to), latency)| Dependency {
            from,
            to,
            latency,
            distance: 0,
        })
        .collect()
}

/// The local indices of the operations a branch delay slot may take from the
/// table's tail.
fn zero_tail_operations(
    program: &Source,
    region: &Region,
    block: &Block,
    analysis: &flow::Analysis,
    block_id: usize,
    live: &[usize],
) -> Vec<usize> {
    live.iter()
        .enumerate()
        .filter_map(|(local, &id)| {
            let op = &region.instructions[id];
            // VI-only loads have no P/Q through-lock obligation.
            // Their future UTA users establish incoming debt later;
            // latency alone does not imply a nonzero outgoing tail.
            let vi_load = matches!(op.instruction.metadata.name, "ilw" | "ilwr");
            (!op.effects.side_effect
                && op.instruction.modifiers == 0
                && (!op.instruction.metadata.is_memory() || vi_load)
                && !op.instruction.metadata.is_branch()
                && op
                    .effects
                    .reads
                    .iter()
                    .chain(&op.effects.writes)
                    .all(|a| program.registers.named(a.register).family == Family::Integer)
                && (vi_load || op.effects.latencies.values().all(|&latency| latency <= 1))
                && op.effects.writes.iter().all(|&write| {
                    ordinary_tail_separation(program, region, block, analysis, block_id, id, write)
                        <= 1
                }))
            .then_some(local)
        })
        .collect()
}

/// The fewest rows the table may have so that every non-vector result written
/// near its end is ready.
fn minimum_tail_rows(
    program: &Source,
    region: &Region,
    block: &Block,
    analysis: &flow::Analysis,
    block_id: usize,
    rows: &Rows,
) -> usize {
    let mut minimum_rows = 0;
    for (row, pair) in rows.iter().enumerate() {
        for &id in pair.iter().flatten() {
            for &write in &region.instructions[id].effects.writes {
                if program.registers.named(write.register).family != Family::Vector {
                    let separation = ordinary_tail_separation(
                        program, region, block, analysis, block_id, id, write,
                    );
                    minimum_rows = minimum_rows.max(row + usize::from(separation));
                }
            }
        }
    }
    minimum_rows
}

/// Builds the occupancy of every virtual register and assigns the physical
/// registers. Returns the name each register prints as.
#[allow(clippy::too_many_arguments)]
fn allocate_region(
    program: &Source,
    region: &Region,
    options: &Options,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    schedules: &[Option<Rows>],
    block_ranges: &[Option<std::ops::Range<usize>>],
    wrangled_tables: &BTreeMap<usize, WrangledTable>,
    total_rows: usize,
) -> Result<BTreeMap<usize, String>, CompileError> {
    let operations: Vec<_> = region
        .instructions
        .iter()
        .map(|located| flow::Operation {
            reads: located.effects.reads.clone(),
            writes: located.effects.writes.clone(),
            side_effect: located.effects.side_effect,
        })
        .collect();
    let (tables, occupancy_rows) = scheduled_blocks(analysis, schedules, block_ranges, total_rows);
    let lifetimes =
        crate::allocator::lifetime::build(&operations, &tables, occupancy_rows, &graph.constants)
            .map_err(|e| internal(0, e))?;
    // Table-local virtual registers keep the original per-table allocation
    // made for the selected optimizer candidate.
    let local_allocation: BTreeMap<usize, usize> = wrangled_tables
        .values()
        .flat_map(|table| table.allocation.iter().map(|(&id, &p)| (id, p)))
        .collect();
    let mut mappings = BTreeMap::new();
    for family in [Family::Vector, Family::Integer] {
        allocate_family(
            program,
            region,
            options,
            family,
            &lifetimes,
            &local_allocation,
            occupancy_rows,
            &mut mappings,
        )?;
    }
    for (id, register) in program.registers.values.iter().enumerate() {
        mappings.entry(id).or_insert_with(|| register.name.clone());
    }
    Ok(mappings)
}

/// The scheduled tables as the lifetime builder sees them, and the number of
/// rows including the copies of shared subroutines.
fn scheduled_blocks<'a>(
    analysis: &'a flow::Analysis,
    schedules: &'a [Option<Rows>],
    block_ranges: &[Option<std::ops::Range<usize>>],
    total_rows: usize,
) -> (Vec<crate::allocator::lifetime::ScheduledBlock<'a>>, usize) {
    // A subroutine has one copy per call string; each copy occupies its own
    // rows past the program, so values passing through for one caller do not
    // interfere with another caller's.
    let mut tables: Vec<_> = Vec::new();
    let mut occupancy_rows = total_rows;
    for (block, rows) in schedules.iter().enumerate() {
        let (Some(range), Some(rows)) = (block_ranges[block].as_ref(), rows.as_deref()) else {
            continue;
        };
        let copies = &analysis.copy_live_out[block];
        if copies.len() <= 1 {
            tables.push(crate::allocator::lifetime::ScheduledBlock {
                start: range.start,
                rows,
                live_out: &analysis.live_out[block],
            });
            continue;
        }

        for (copy, live_out) in copies.iter().enumerate() {
            let start = if copy == 0 {
                range.start
            } else {
                occupancy_rows += rows.len();
                occupancy_rows - rows.len()
            };
            tables.push(crate::allocator::lifetime::ScheduledBlock {
                start,
                rows,
                live_out,
            });
        }
    }
    (tables, occupancy_rows)
}

/// Assigns the physical registers of one family to its virtual registers.
#[allow(clippy::too_many_arguments)]
fn allocate_family(
    program: &Source,
    region: &Region,
    options: &Options,
    family: Family,
    lifetimes: &BTreeMap<usize, crate::allocator::allocation::Occupancy>,
    local_allocation: &BTreeMap<usize, usize>,
    occupancy_rows: usize,
    mappings: &mut BTreeMap<usize, String>,
) -> Result<(), CompileError> {
    let allowed = if family == Family::Vector {
        program.allowed_vf.clone()
    } else {
        program.allowed_vi.clone()
    };
    let mut allocator = Allocator::new(
        if family == Family::Vector { 4 } else { 1 },
        allowed,
        occupancy_rows,
    )
    .map_err(|e| internal(0, e))?;
    let mut virtuals = Vec::new();
    for (&id, occupancy) in lifetimes {
        let register = program.registers.named(id);
        if register.family != family || register.physical == Some(0) {
            continue;
        }

        virtuals.push(VirtualRegister {
            id,
            occupancy: occupancy.clone(),
            forced: register
                .physical
                .or_else(|| region.forced.get(&id).copied())
                .or_else(|| local_allocation.get(&id).copied()),
        });
    }
    if options.symbolic {
        return Ok(());
    }

    if std::env::var_os("VCL_ALLOC_TRACE").is_some() {
        for v in &virtuals {
            crate::trace!(
                "global {} {} forced {:?} rows {}",
                v.id,
                program.registers.named(v.id).name,
                v.forced,
                v.occupancy.occupied_rows()
            );
        }
    }
    let allocated = allocator
        .allocate(&virtuals)
        .map_err(|failure| allocation_pool_error(program, region, family, failure))?;
    for (id, physical) in allocated {
        mappings.insert(id, physical_name(family, physical));
    }
    Ok(())
}

/// The error for a failed allocation, naming the pool that ran out.
fn allocation_pool_error(
    program: &Source,
    region: &Region,
    family: Family,
    failure: crate::allocator::allocation::AllocationError,
) -> CompileError {
    let crate::allocator::allocation::AllocationError::NoFit { virtual_id } = failure else {
        return allocation_error(failure);
    };
    let mut result = allocation_error(failure);
    let vector = family == Family::Vector;
    let allowed = if family == Family::Vector {
        &program.allowed_vf
    } else {
        &program.allowed_vi
    };
    result.message = if allowed.iter().all(|&mask| mask == 0) {
        CompileMessage::AllocationPoolEmpty { vector }
    } else {
        CompileMessage::AllocationPoolFull {
            vector,
            register: program
                .registers
                .named(
                    region
                        .web_sources
                        .get(&virtual_id)
                        .copied()
                        .unwrap_or(virtual_id),
                )
                .name
                .clone(),
        }
    }
    .into();
    result
}

/// Records the components an access uses in the physical VF it was given.
fn mark_used(
    program: &Source,
    mappings: &BTreeMap<usize, String>,
    used: &mut [u8; 32],
    access: &Access,
) {
    if program.registers.named(access.register).family == Family::Vector
        && let Some(id) = physical(&mappings[&access.register], Family::Vector)
    {
        used[id] |= access.mask;
    }
}

/// The first allowed VF (above VF00) with every component of `mask` unused.
fn pick_scratch(program: &Source, mask: u8, used: &[u8; 32]) -> Option<usize> {
    (1..32).find(|&candidate| {
        program.allowed_vf[candidate] & mask == mask && used[candidate] & mask == 0
    })
}

/// The register id of the physical VF `scratch`.
fn scratch_register(program: &Source, scratch: usize) -> usize {
    let name = physical_name(Family::Vector, scratch).to_ascii_lowercase();
    program.registers.ids[&(Family::Vector, name)]
}

/// Redirects an instruction's destination operand, its write and its latency
/// from register `from` to register `to`.
fn retarget_destination(located: &mut Located, destination: usize, from: usize, to: usize) {
    located.operands[destination] = Some(to);
    for write in &mut located.effects.writes {
        if write.register == from {
            write.register = to;
        }
    }
    if let Some(latency) = located.effects.latencies.remove(&from) {
        located.effects.latencies.insert(to, latency);
    }
}

/// The components of each physical VF a table uses.
fn table_register_usage(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
    schedules: &[Option<Rows>],
    mappings: &BTreeMap<usize, String>,
) -> Vec<[u8; 32]> {
    let mut table_used: Vec<[u8; 32]> = vec![[0u8; 32]; schedules.len()];
    for (block, rows) in schedules.iter().enumerate() {
        let Some(rows) = rows else { continue };
        let used = &mut table_used[block];
        for access in analysis.live_in[block]
            .iter()
            .chain(&analysis.live_out[block])
        {
            mark_used(program, mappings, used, access);
        }
        for access in region.inputs.iter().chain(&region.blocks[block].outputs) {
            mark_used(program, mappings, used, access);
        }
        for &id in rows.iter().flatten().flatten() {
            let effects = &region.instructions[id].effects;
            for access in effects.reads.iter().chain(&effects.writes) {
                mark_used(program, mappings, used, access);
            }
        }
    }
    table_used
}

/// The index of the instruction's vector destination operand.
fn vector_destination(located: &Located) -> Option<usize> {
    located
        .instruction
        .operands
        .iter()
        .enumerate()
        .find_map(|(index, operand)| {
            (matches!(operand.class, class::VECTOR | class::VECTOR_XYZ)
                && operand.direction == Direction::Write)
                .then_some(index)
        })
}

/// Gives an upper whose flag result a cap reads, but whose destination was
/// sent to VF00, a scratch VF of its own. Returns the region with those
/// destinations redirected and the redirections.
fn assign_scratch_destinations(
    program: &Source,
    region: &Region,
    options: &Options,
    analysis: &flow::Analysis,
    schedules: &[Option<Rows>],
    mappings: &BTreeMap<usize, String>,
) -> (Region, Scratched) {
    let mut allocated_region = region.clone();
    let mut scratched: Scratched = BTreeMap::new();
    if options.disable_size_reduction {
        return (allocated_region, scratched);
    }

    let mut table_used = table_register_usage(program, region, analysis, schedules, mappings);
    let fixed_kernel = |block: usize| {
        region.blocks[block]
            .selected_table
            .as_ref()
            .is_some_and(|table| table.mode == TableMode::Fixed)
    };
    let uppers: Vec<(usize, usize)> = schedules
        .iter()
        .enumerate()
        .filter(|&(block, _)| !fixed_kernel(block))
        .filter_map(|(block, rows)| Some((block, rows.as_ref()?)))
        .flat_map(|(block, rows)| rows.iter().filter_map(move |row| Some((block, row[0]?))))
        .collect();
    for (block, id) in uppers {
        let used = &mut table_used[block];
        let needed = analysis.reads.iter().any(|read| {
            matches!(
                program.registers.named(read.register).name.as_str(),
                "STAT_ALU" | "STAT_ALU_S" | "MAC"
            ) && read.definitions.contains(&Definition::Operation(id))
        });
        if !needed {
            continue;
        }

        let located = &mut allocated_region.instructions[id];
        let Some(destination) = vector_destination(located) else {
            continue;
        };
        let Some(old) = located.operands[destination] else {
            continue;
        };
        if mappings[&old] != "VF00" {
            continue;
        }

        let mask = located.instruction.mask.0;
        let Some(scratch) = pick_scratch(program, mask, used) else {
            continue;
        };
        let register = scratch_register(program, scratch);
        retarget_destination(located, destination, old, register);
        used[scratch] |= mask;
        scratched.insert(id, (destination, old, register));
    }
    (allocated_region, scratched)
}

/// Turns the folded branches into lower `nop`s. A folded branch is scheduled
/// as the branch it was; only its text becomes the lower `nop` that carries
/// the end bit.
fn nop_folded_branches(
    allocated_region: &mut Region,
    to_nop: &[usize],
) -> Result<(), CompileError> {
    if to_nop.is_empty() {
        return Ok(());
    }

    let parsed = parser::parse_with_options(
        ".raw\nnop nop\n",
        parser::ParserOptions {
            allow_incomplete: true,
            ..Default::default()
        },
    )
    .map_err(|errors| internal(0, format!("failed to construct nop: {errors:?}")))?;
    let nop = parsed
        .statements
        .iter()
        .rev()
        .find_map(|statement| match &statement.kind {
            StatementKind::Instruction(instruction) => Some(instruction.clone()),
            _ => None,
        })
        .ok_or_else(|| internal(0, "nop has no instruction"))?;
    for &op in to_nop {
        let located = &mut allocated_region.instructions[op];
        let mode = located.instruction.mode;
        located.instruction = nop.clone();
        located.instruction.mode = mode;
        located.instruction.metadata = lower_nop(nop.metadata);
        located.operands.clear();
        located.effects = Effects::default();
    }
    Ok(())
}

/// Whether the table may be concatenated with a neighbour: installed loop
/// parts are linear vsm tables too; the kernel is not.
fn mergeable_table(region: &Region, b: &Block) -> bool {
    b.selected_table
        .as_ref()
        .is_none_or(|table| table.installed)
        && b.operations
            .iter()
            .all(|&id| region.instructions[id].instruction.mode == ParseMode::Vcl)
}

/// Whether stream `next` may join the merged stream that starts at `current`
/// and ends with `last`.
#[allow(clippy::too_many_arguments)]
fn can_merge_streams(
    program: &Source,
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    a: &Block,
    b: &Block,
    last: usize,
    next: usize,
) -> bool {
    let end = match a.end {
        End::Fallthrough | End::Branch { .. } => crate::analysis::control::StreamEnd::Ordinary,
        End::Continue => crate::analysis::control::StreamEnd::Continue,
        End::Exit { macro_text: None } => crate::analysis::control::StreamEnd::Exit,
        End::Exit {
            macro_text: Some(_),
        } => crate::analysis::control::StreamEnd::ExitMacro,
    };
    crate::analysis::control::can_merge_adjacent(crate::analysis::control::MergeFacts {
        current_is_emittable_linear_vsm: mergeable_table(region, a),
        next_is_emittable_linear_vsm: mergeable_table(region, b),
        current_end: end,
        next_has_ordinary_start: true,
        current_successors: graph.blocks[last].successors.len(),
        next_predecessors: analysis
            .reachable
            .iter()
            .filter(|&&pred| graph.blocks[pred].successors.contains(&next))
            .count(),
        current_zero_edge_targets_next: matches!(a.end, End::Fallthrough)
            && graph.blocks[last].successors == [next],
        current_has_branch: a
            .operations
            .iter()
            .any(|&id| region.instructions[id].instruction.metadata.is_branch()),
        current_end_fence: a.end_fence,
        next_start_fence: b.start_fence,
        current_debug_end: a.debug_end != 0,
        next_debug_top: b.debug_top != 0,
        next_has_exported_label: b.labels.iter().any(|label| {
            program
                .globals
                .iter()
                .any(|export| crate::analysis::control::same_label(export, label))
        }),
    })
}

/// `mergeStreams` runs after scheduling/allocation in the binary. Keep the
/// analysis graph and its lifetimes intact, then concatenate only unique
/// ordinary fallthrough dispatch tables. No rescheduling or renaming here.
/// Returns the last block of the stream each block starts.
fn merge_adjacent_streams(
    program: &Source,
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    emission_blocks: &mut [Block],
    schedules: &mut [Option<Rows>],
) -> Vec<usize> {
    let mut last_member: Vec<_> = (0..emission_blocks.len()).collect();
    for current in 0..emission_blocks.len() {
        if schedules[current].is_none() {
            continue;
        }

        while let Some(next) =
            (last_member[current] + 1..schedules.len()).find(|&id| schedules[id].is_some())
        {
            let last = last_member[current];
            let a = &emission_blocks[current];
            let b = &emission_blocks[next];
            if !can_merge_streams(program, region, graph, analysis, a, b, last, next) {
                break;
            }

            let next_block = emission_blocks[next].clone();
            let current_block = &mut emission_blocks[current];
            current_block.operations.extend(next_block.operations);
            current_block.outputs = next_block.outputs;
            current_block.end = next_block.end;
            current_block.end_fence = next_block.end_fence;
            current_block.debug_end = next_block.debug_end;
            let next_rows = schedules[next].take().unwrap();
            schedules[current].as_mut().unwrap().extend(next_rows);
            last_member[current] = last_member[next];
        }
    }
    last_member
}

/// Re-picks the scratch destinations on the merged streams: a scratch
/// destination must be free in the whole merged table, not only in its own
/// block. Returns the region with the new destinations, if any were picked.
#[allow(clippy::too_many_arguments)]
fn rechoose_scratches(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
    emission_blocks: &[Block],
    schedules: &[Option<Rows>],
    last_member: &[usize],
    mappings: &BTreeMap<usize, String>,
    scratched: &mut Scratched,
) -> Option<Region> {
    if scratched.is_empty() {
        return None;
    }

    let mut copy = region.clone();
    let entries: Vec<usize> = scratched.keys().copied().collect();
    for id in entries {
        rechoose_scratch(
            program,
            &mut copy,
            analysis,
            emission_blocks,
            schedules,
            last_member,
            mappings,
            scratched,
            id,
        );
    }
    Some(copy)
}

/// Moves one scratch destination off a VF its merged table uses.
#[allow(clippy::too_many_arguments)]
fn rechoose_scratch(
    program: &Source,
    copy: &mut Region,
    analysis: &flow::Analysis,
    emission_blocks: &[Block],
    schedules: &[Option<Rows>],
    last_member: &[usize],
    mappings: &BTreeMap<usize, String>,
    scratched: &mut Scratched,
    id: usize,
) {
    let Some(head) = (0..schedules.len()).find(|&b| {
        schedules[b]
            .as_ref()
            .is_some_and(|rows| rows.iter().flatten().flatten().any(|&x| x == id))
    }) else {
        return;
    };

    let mut used = [0u8; 32];
    for access in analysis.live_in[head]
        .iter()
        .chain(&analysis.live_out[last_member[head]])
        .chain(&copy.inputs)
        .chain(&emission_blocks[head].outputs)
    {
        mark_used(program, mappings, &mut used, access);
    }
    for &other in schedules[head].as_ref().unwrap().iter().flatten().flatten() {
        if other == id {
            continue;
        }
        let effects = &copy.instructions[other].effects;
        for access in effects.reads.iter().chain(&effects.writes) {
            mark_used(program, mappings, &mut used, access);
        }
    }

    let (destination, old, register) = scratched[&id];
    let mask = copy.instructions[id].instruction.mask.0;
    let current = physical(&mappings[&register], Family::Vector).unwrap_or(0);
    if used[current] & mask == 0 {
        return;
    }

    let Some(scratch) = pick_scratch(program, mask, &used) else {
        return;
    };
    let fresh = scratch_register(program, scratch);
    retarget_destination(&mut copy.instructions[id], destination, register, fresh);
    scratched.insert(id, (destination, old, fresh));
}

/// A merged table is reduced as one table (the original reduces
/// main_loop_lid together with its loop's PRO0, and a stream with the one
/// its deleted jump reached: vu_3323147251).
#[allow(clippy::too_many_arguments)]
fn rewrangle_merged_tables(
    program: &Source,
    region: &Region,
    options: &Options,
    analysis: &flow::Analysis,
    emission_blocks: &[Block],
    schedules: &[Option<Rows>],
    last_member: &[usize],
    wrangled_tables: &mut BTreeMap<usize, WrangledTable>,
) {
    for current in 0..emission_blocks.len() {
        let last = last_member[current];
        if last == current || schedules[current].is_none() {
            continue;
        }

        let members = current..=last;
        let live: Vec<_> = emission_blocks[current]
            .operations
            .iter()
            .copied()
            .filter(|id| analysis.live_operations.contains(id))
            .collect();
        let rows = schedules[current].clone().unwrap();
        let Some(mut table) = build_wrangle_table_span(
            program,
            region,
            analysis,
            current,
            last,
            &live,
            options.timid_memory,
        ) else {
            continue;
        };

        let local: BTreeMap<_, _> = live.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        table.rows = rows
            .iter()
            .map(|pair| pair.map(|id| id.and_then(|id| local.get(&id).copied())))
            .collect();
        table.end = table.rows.len();
        table.problem.horizon = table.problem.horizon.max(table.end as i32 + 1);
        for block in members {
            wrangled_tables.remove(&block);
        }
        wrangled_tables.insert(current, table);
    }
}

/// The reads whose definitions an affine consumer crossed when its table was
/// scheduled, for the timing context to retarget.
fn crossed_reads(
    region: &Region,
    analysis: &flow::Analysis,
    wrangled_tables: &BTreeMap<usize, WrangledTable>,
    holders: &[Vec<usize>],
    installed: &[Option<(BTreeMap<usize, usize>, usize)>],
) -> Vec<flow::Read> {
    let mut crossed = Vec::new();
    for table in wrangled_tables.values() {
        for affine in &table.affine {
            let consumer = table.live[affine.consumer];
            let setter = table.live[affine.setter];
            let row = |id: usize| {
                let &block = holders.iter().position(|h| h.contains(&id)).as_ref()?;
                installed[block].as_ref()?.0.get(&id).copied()
            };
            let (Some(consumer_row), Some(setter_row)) = (row(consumer), row(setter)) else {
                continue;
            };
            let Some(register) = region.instructions[consumer].operands[affine.operand] else {
                continue;
            };
            let definitions = if !affine.before && consumer_row < setter_row {
                analysis
                    .reads
                    .iter()
                    .find(|read| {
                        read.operation == setter && read.register == register && read.component == 0
                    })
                    .map(|read| read.definitions.clone())
            } else if affine.before && consumer_row > setter_row {
                Some(BTreeSet::from([Definition::Operation(setter)]))
            } else {
                None
            };
            if let Some(definitions) = definitions {
                crossed.push(flow::Read {
                    operation: consumer,
                    register,
                    component: 0,
                    definitions,
                });
            }
        }
    }
    crossed
}

/// Raw linear reduction of optimized tables after allocation.
/// Streams are visited in order: each one's external
/// timing sees the tables reduced before it.
#[allow(clippy::too_many_arguments)]
fn raw_reduce_tables(
    program: &Source,
    region: &Region,
    options: &Options,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    mappings: &BTreeMap<usize, String>,
    last_member: &[usize],
    emission_blocks: &[Block],
    wrangled_tables: &mut BTreeMap<usize, WrangledTable>,
    schedules: &mut [Option<Rows>],
) -> BTreeMap<usize, CompressionPlan> {
    let mut installed: Vec<Option<(BTreeMap<usize, usize>, usize)>> = schedules
        .iter()
        .enumerate()
        .map(|(block, rows)| {
            rows.as_ref().map(|rows| {
                (
                    table_positions(region, &emission_blocks[block].operations, rows),
                    rows.len(),
                )
            })
        })
        .collect();
    let holders: Vec<Vec<usize>> = installed
        .iter()
        .map(|table| {
            table
                .as_ref()
                .map(|(positions, _)| positions.keys().copied().collect())
                .unwrap_or_default()
        })
        .collect();
    let crossed = crossed_reads(region, analysis, wrangled_tables, &holders, &installed);
    let mut timing = TimingContext::new(program, region, analysis, graph, &holders);
    timing.retarget(&crossed);
    let mut lengths: Vec<usize> = schedules
        .iter()
        .map(|rows| rows.as_ref().map_or(0, Vec::len))
        .collect();
    let mut plans: BTreeMap<usize, CompressionPlan> = BTreeMap::new();
    let blocks: Vec<usize> = wrangled_tables.keys().copied().collect();
    for block_id in blocks {
        let Some(rows) = schedules[block_id].clone() else {
            continue;
        };
        let placed = |id: usize| -> Option<(usize, usize)> {
            let &block = timing.owner.get(&id)?;
            let (positions, length) = installed[block].as_ref()?;
            positions.get(&id).map(|&row| (row, *length))
        };
        let table = wrangled_tables.get_mut(&block_id).unwrap();
        timing.apply(table, block_id, &lengths, &placed);
        let table = &wrangled_tables[&block_id];
        let end = table.end.min(rows.len());
        let padding = rows.len() - end;
        if rows[end..]
            .iter()
            .any(|row| row.iter().any(Option::is_some))
        {
            continue;
        }

        let stalled = table_stalls(region, &timing, &lengths, &placed, block_id, analysis);
        if std::env::var_os("VCL_MOVE_TRACE").is_some() {
            crate::trace!("raw reduce block {block_id} stalled {stalled}");
        }
        if let Some(plan) = raw_reduce_wrangled(
            program,
            region,
            table,
            &rows[..end],
            mappings,
            padding,
            &emission_blocks[block_id].end,
            // An installed loop part is raw-reduced even under -C.
            !stalled
                && region.blocks[block_id..=last_member[block_id]]
                    .iter()
                    .any(|block| block.selected_table.as_ref().is_some_and(|t| t.installed)),
            !stalled && !options.disable_size_reduction,
        ) {
            installed[block_id] = Some((
                table_positions(region, &emission_blocks[block_id].operations, &plan.rows),
                plan.end,
            ));
            lengths[block_id] = plan.end;
            schedules[block_id] = Some(plan.rows.clone());
            plans.insert(block_id, plan);
        }
    }
    plans
}

/// The producers a compression plan gave a cap.
fn capped_producers(plans: &BTreeMap<usize, CompressionPlan>) -> BTreeSet<usize> {
    plans
        .values()
        .flat_map(|plan| plan.sync.iter().flatten())
        .filter_map(|sync| match sync {
            Synchronization::Vector { producer, .. } => Some(*producer),
            _ => None,
        })
        .collect()
}

/// Sends the producers that got no cap back to their original destination.
/// Returns the region with those restored, if any were.
fn revert_uncapped_scratches(
    region: &Region,
    scratched: &Scratched,
    capped: &BTreeSet<usize>,
) -> Option<Region> {
    if !scratched.keys().any(|id| !capped.contains(id)) {
        return None;
    }

    let mut copy = region.clone();
    for (&id, &(destination, old, register)) in scratched {
        if capped.contains(&id) {
            continue;
        }
        retarget_destination(&mut copy.instructions[id], destination, register, old);
    }
    Some(copy)
}

/// Re-picks the scratch of each capped producer, now that the uncapped ones
/// reserve nothing. Returns the region with the new destinations, if any
/// producer went uncapped.
#[allow(clippy::too_many_arguments)]
fn repick_scratches(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
    emission_blocks: &[Block],
    schedules: &[Option<Rows>],
    last_member: &[usize],
    mappings: &BTreeMap<usize, String>,
    capped: &BTreeSet<usize>,
    scratched: &mut Scratched,
    plans: &mut BTreeMap<usize, CompressionPlan>,
) -> Option<Region> {
    if !scratched.keys().any(|id| !capped.contains(id)) {
        return None;
    }

    let mut copy = region.clone();
    let mut renamed: BTreeMap<usize, usize> = BTreeMap::new();
    for head in 0..schedules.len() {
        let Some(rows) = schedules[head].as_ref() else {
            continue;
        };
        let order: Vec<usize> = rows
            .iter()
            .flatten()
            .flatten()
            .copied()
            .filter(|id| capped.contains(id) && scratched.contains_key(id))
            .collect();
        for id in order {
            let mut used = [0u8; 32];
            for access in analysis.live_in[head]
                .iter()
                .chain(&analysis.live_out[last_member[head]])
                .chain(&emission_blocks[head].outputs)
            {
                mark_used(program, mappings, &mut used, access);
            }
            for &other in rows.iter().flatten().flatten() {
                if other == id {
                    continue;
                }
                let effects = &copy.instructions[other].effects;
                for access in effects.reads.iter().chain(&effects.writes) {
                    mark_used(program, mappings, &mut used, access);
                }
            }

            let (destination, old, register) = scratched[&id];
            let mask = copy.instructions[id].instruction.mask.0;
            let Some(scratch) = pick_scratch(program, mask, &used) else {
                continue;
            };
            let fresh = scratch_register(program, scratch);
            if fresh == register {
                continue;
            }

            retarget_destination(&mut copy.instructions[id], destination, register, fresh);
            scratched.insert(id, (destination, old, fresh));
            renamed.insert(id, fresh);
        }
    }

    for plan in plans.values_mut() {
        for sync in plan.sync.iter_mut().flatten() {
            if let Synchronization::Vector {
                register, producer, ..
            } = sync
                && let Some(&fresh) = renamed.get(producer)
            {
                *register = fresh;
            }
        }
    }
    Some(copy)
}

/// Displacement repair: a consumer that crossed its induction update absorbs
/// the stride. Returns the replacement immediate for each repaired operand.
fn displacement_repairs(
    region: &Region,
    schedules: &[Option<Rows>],
    wrangled_tables: &BTreeMap<usize, WrangledTable>,
) -> BTreeMap<usize, (usize, String)> {
    let mut final_rows = BTreeMap::new();
    for rows in schedules.iter().flatten() {
        for (row, pair) in rows.iter().enumerate() {
            for &id in pair.iter().flatten() {
                final_rows.insert(id, row);
            }
        }
    }

    let mut repairs: BTreeMap<usize, (usize, String)> = BTreeMap::new();
    for table in wrangled_tables.values() {
        for affine in &table.affine {
            let consumer = table.live[affine.consumer];
            let setter = table.live[affine.setter];
            let (Some(&cr), Some(&sr)) = (final_rows.get(&consumer), final_rows.get(&setter))
            else {
                continue;
            };
            let reads_new = cr > sr;
            let mut factor = match (affine.before, reads_new) {
                (true, true) => -1,
                (false, false) => 1,
                _ => 0,
            };
            if !affine.add {
                factor = -factor;
            }
            if factor == 0 {
                continue;
            }

            let old = region.instructions[consumer].instruction.operands[affine.operand]
                .immediate
                .clone()
                .unwrap_or_else(|| "0".into());
            repairs.insert(
                consumer,
                (
                    affine.operand,
                    format!("({old})+(({})*{factor})", affine.stride),
                ),
            );
        }
    }
    repairs
}

/// The labels (lowercased) that print: those an emitted branch, indirect tag
/// or export names. A deleted jump to the next stream does not name its
/// target. Unreachable code still names its targets (vu_411781243's last
/// `jr` tags a label the original prints).
fn referenced_labels(
    program: &Source,
    analysis: &flow::Analysis,
    emission_blocks: &[Block],
    schedules: &[Option<Rows>],
) -> BTreeSet<String> {
    emission_blocks
        .iter()
        .zip(schedules)
        .enumerate()
        .filter(|(id, (_, rows))| rows.is_some() || !analysis.reachable.contains(id))
        .map(|(_, pair)| pair)
        .flat_map(|(block, _)| match &block.end {
            End::Branch { target, tags, .. } => target.iter().chain(tags).cloned().collect(),
            _ => Vec::new(),
        })
        .chain(program.globals.iter().cloned())
        .map(|label| label.to_ascii_lowercase())
        .collect()
}

/// What the text of a region's tables is printed from.
struct Emitter<'a> {
    program: &'a Source,
    region: &'a Region,
    options: &'a Options,
    analysis: &'a flow::Analysis,
    mappings: &'a BTreeMap<usize, String>,
    repairs: &'a BTreeMap<usize, (usize, String)>,
    plans: &'a BTreeMap<usize, CompressionPlan>,
    unique: &'a str,
    /// Labels that print away from the region start.
    referenced: BTreeSet<String>,
}

impl Emitter<'_> {
    /// Prints each scheduled block: its labels, comment, rows and macro text.
    fn emit_blocks(
        &self,
        emission_blocks: &[Block],
        schedules: &[Option<Rows>],
        notes: &[TableNote],
        output: &mut Compiled,
    ) -> Result<(), CompileError> {
        let mut offset = output.instruction_pairs;
        let mut last_stream: Option<u16> = None;
        for (block_id, block) in emission_blocks.iter().enumerate() {
            let Some(rows) = &schedules[block_id] else {
                continue;
            };
            self.emit_labels(block_id, block, offset, output);
            let mut finished = finish_rows_planned(
                self.program,
                self.region,
                block,
                self.analysis,
                rows,
                self.mappings,
                self.options,
                self.plans.get(&block_id),
            )?;
            let end_row = self.end_row(block, &mut finished, rows.len());
            // An empty stream (after `--cont`, say) takes the uid after the
            // previous stream's.
            let stream = block
                .stream
                .or_else(|| last_stream.map(|uid| uid + 1))
                .or(self.region.stream);
            last_stream = stream;
            // An empty stream that ends at an exit has no table of its own: its
            // rows are the exit's padding, printed without a comment (observed
            // from vcl 1.4beta7, `branch-forward-conditional`).
            let commented = !(block_id != 0
                && block.part_name.is_none()
                && block.operations.is_empty()
                && matches!(block.end, End::Exit { .. }));
            if self.options.mode == crate::Mode::Vcl && commented {
                self.emit_table_comment(
                    block,
                    stream,
                    notes[block_id],
                    finished.rows.len(),
                    rows.len(),
                    output,
                );
            }
            // vcl's `-c` layout (observed from vcl 1.4beta7, `comments-*`): each
            // row as a comment with source register names, the row itself one
            // space further in, and a blank line; each walked row that is not
            // printed is a bare `; ` line. An exit's padding stream keeps the
            // plain layout. Both modes use it.
            let source_comments = self.options.comments && commented;
            self.emit_rows(block, &finished, end_row, source_comments, output)?;
            offset += finished.rows.len();
            self.emit_exit_macro(block, output);
        }
        Ok(())
    }

    /// The name of a stream that has no label.
    fn generated_name(&self, stream: u16) -> String {
        let name = format!("__v_{}_{stream}", self.unique);
        if self.program.unique_labels {
            format!("{}_{name}", self.unique)
        } else {
            name
        }
    }

    /// Prints the labels that start the block, and records the exported ones.
    fn emit_labels(&self, block_id: usize, block: &Block, offset: usize, output: &mut Compiled) {
        let visible = block_id == 0
            || block
                .labels
                .iter()
                .any(|label| self.referenced.contains(&label.to_ascii_lowercase()));
        if block.labels.is_empty() {
            if let (0, Some(stream)) = (block_id, self.region.stream) {
                let name = self.generated_name(stream);
                output.assembly.push_str(&format!("{name}:\n"));
            }
            return;
        }

        if !visible {
            return;
        }
        // A pipelined loop's entry keeps the old stream labels only at
        // the region start or where something still names them.
        // Away from the region start a label prints only when something
        // names it (an unreferenced label starts no stream).
        for label in &block.labels {
            if block_id != 0 && !self.referenced.contains(&label.to_ascii_lowercase()) {
                continue;
            }
            output.assembly.push_str(&format!("{label}:\n"));
            if !label.starts_with("__v_") {
                output.header.push_str(&format!("{label:<40} = {offset}\n"));
            }
        }
    }

    /// Prints the optimizer comment line that precedes a table (vcl mode).
    fn emit_table_comment(
        &self,
        block: &Block,
        stream: Option<u16>,
        mut note: TableNote,
        printed: usize,
        walked: usize,
        output: &mut Compiled,
    ) {
        // A stream is named after its first label, printed or not (the
        // labels kept on a block are those that start its stream), or
        // after its uid.
        let name = block
            .part_name
            .clone()
            .or_else(|| block.labels.first().cloned())
            .or_else(|| stream.map(|stream| self.generated_name(stream)))
            .unwrap_or_default();
        if block.operations.is_empty() {
            note.lower_bound = Some(0);
        }
        output
            .assembly
            .push_str(&note.comment(printed, walked, &name));
    }

    /// The row of a table that carries the E bit, when the block ends the
    /// program or a `--cont` stream.
    ///
    /// vcl 1.4beta7 sets it on the row walked second to last, the row before
    /// the delay slot. Its final compression can delete that row, leaving the
    /// exit without an E bit (`external-vf-two`), or delete the delay slot,
    /// so the next table's first row runs in it. With the `end` fix the E row
    /// and its delay slot always end the table. Where either is missing, or
    /// the delay slot holds an instruction the hardware forbids there, the E
    /// bit goes on the table's last row and an empty delay row follows it
    /// (an empty table gets both rows).
    fn end_row(&self, block: &Block, finished: &mut FinishedRows, walked: usize) -> Option<usize> {
        let ends = self.program.end_bit
            && matches!(block.end, End::Exit { macro_text: None } | End::Continue);
        if !ends {
            return None;
        }
        let original = finished
            .origins
            .iter()
            .position(|&origin| origin + 2 == walked);
        // `-e` asks for no E bit, which the fix respects.
        if !self.options.fixes(Fix::End) || self.options.disable_end_bit {
            return original;
        }

        let length = finished.rows.len();
        if let Some(row) = original
            && row + 2 == length
            && self.allowed_in_end_delay(&finished.rows[row + 1])
        {
            return Some(row);
        }
        if length == 0 {
            finished.push_empty_row();
        }
        finished.push_empty_row();
        Some(finished.rows.len() - 2)
    }

    /// Whether a row may run in the E bit's delay slot. The VU User's Manual
    /// (section 3.5.3) forbids branches, external synchronization (XTOP,
    /// XITOP, XGKICK) and VU memory loads and stores there.
    fn allowed_in_end_delay(&self, row: &TimedRow) -> bool {
        row.instructions.iter().flatten().all(|&id| {
            let metadata = self.region.instructions[id].instruction.metadata;
            !(metadata.is_branch()
                || metadata.is_memory()
                || matches!(metadata.index, id::XGKICK | id::XTOP | id::XITOP))
        })
    }

    /// Prints the rows of one table, with the E bit on `end_row`.
    fn emit_rows(
        &self,
        block: &Block,
        finished: &FinishedRows,
        end_row: Option<usize>,
        source_comments: bool,
        output: &mut Compiled,
    ) -> Result<(), CompileError> {
        let mut next_origin = 0;
        for (row_id, timed) in finished.rows.iter().enumerate() {
            if source_comments {
                for _ in next_origin..finished.origins[row_id] {
                    output.assembly.push_str("; \n");
                }
                next_origin = finished.origins[row_id] + 1;
            }
            let end_bit = end_row == Some(row_id);
            // Stream label flags are carried onto the first emitted upper
            // instruction, including an otherwise empty NOP.
            let extra = if row_id == 0 { block.debug_top } else { 0 }
                | if end_bit { 2 | block.debug_end } else { 0 };
            let (pair, comment) = self.row_text(timed, finished, extra, source_comments)?;
            let stalls = finished.replay.stalls[row_id];
            let count = stalls.latency + stalls.throughput + stalls.synchronization;
            let stall = if count != 0 {
                format!("       ;\tSTALL_LATENCY ?{count}")
            } else {
                String::new()
            };
            if let Some(comment) = comment {
                output
                    .assembly
                    .push_str(&format!(";{comment}{stall}\n {pair}{stall}\n\n"));
            } else {
                output.assembly.push_str(&format!("{pair}{stall}\n"));
            }
            output.instruction_pairs += 1;
        }
        Ok(())
    }

    /// The text of one row, and with source comments its symbolic form.
    fn row_text(
        &self,
        timed: &TimedRow,
        finished: &FinishedRows,
        extra: u8,
        source_comments: bool,
    ) -> Result<(String, Option<String>), CompileError> {
        let row = &timed.instructions;
        let mut text = ["NOP".to_string(), "NOP".to_string()];
        // The original's -e mask is in the empty-NOP hook,
        // not the occupied-instruction hook.
        let nop_extra = if self.options.disable_end_bit {
            extra & !2
        } else {
            extra
        };
        text[0].push_str(&crate::emit::modifier_suffix(nop_extra));
        let mut symbolic = text.clone();
        for pipe in 0..2 {
            if let Some(id) = row[pipe] {
                let (printed, spelled) = self.instruction_text(id, pipe, extra, source_comments)?;
                text[pipe] = printed;
                if let Some(spelled) = spelled {
                    symbolic[pipe] = spelled;
                }
            }
        }
        if let Some(sync) = timed.synchronization {
            text[sync.issue_slot()] = render_synchronization(
                sync,
                &finished.physical_names,
                if sync.issue_slot() == 0 { extra } else { 0 },
            )?;
            symbolic[sync.issue_slot()] = text[sync.issue_slot()].clone();
        }
        let pair = crate::emit::pair(&text[0], &text[1]);
        let comment = source_comments.then(|| crate::emit::pair(&symbolic[0], &symbolic[1]));
        Ok((pair, comment))
    }

    /// The text of one instruction in the given pipe, and with source
    /// comments the same spelled with the source's register names.
    fn instruction_text(
        &self,
        id: usize,
        pipe: usize,
        extra: u8,
        source_comments: bool,
    ) -> Result<(String, Option<String>), CompileError> {
        let program = self.program;
        let region = self.region;
        let located = &region.instructions[id];
        let replacements: Vec<_> = located
            .operands
            .iter()
            .map(|id| id.and_then(|id| self.mappings.get(&id).cloned()))
            .collect();
        let mut instruction = located.instruction.clone();
        if let Some((operand, text)) = self.repairs.get(&id) {
            instruction.operands[*operand].immediate = Some(text.clone());
        }
        let mut replacements = replacements;
        let mut sources: Vec<_> = located
            .operands
            .iter()
            .map(|id| id.map(|id| source_name(program, region, id)))
            .collect();
        if let Some(kind) = instruction.generated_copy {
            let descriptor = kind.resolved_descriptor(if pipe == 0 {
                schedule::Pipe::Upper
            } else {
                schedule::Pipe::Lower
            });
            instruction.metadata = &instruction::INSTRUCTIONS[usize::from(descriptor)];
            instruction.mnemonic = instruction.metadata.name.into();
        }
        if pipe == 0 && instruction.metadata.index == id::MOVE && instruction.mode == ParseMode::Vcl
        {
            instruction.metadata =
                instruction::lookup("max", Syntax::Old).expect("original MAX descriptor");
            instruction.mnemonic = "max".into();
            instruction.operands.push(instruction.operands[1].clone());
            replacements.push(replacements[1].clone());
            sources.push(sources[1].clone());
        }
        if program.fixes.contains(Fix::Imm) {
            immediate_fields::legalize(&mut instruction, located.line)?;
        }

        let printed = crate::emit::instruction(
            &instruction,
            &replacements,
            if pipe == 0 { extra } else { 0 },
        )
        .map_err(|e| internal(located.line, e))?;
        let spelled = if source_comments {
            Some(
                crate::emit::instruction(&instruction, &sources, if pipe == 0 { extra } else { 0 })
                    .map_err(|e| internal(located.line, e))?,
            )
        } else {
            None
        };
        Ok((printed, spelled))
    }

    /// Prints the macro text an `exitm` stream ends with.
    fn emit_exit_macro(&self, block: &Block, output: &mut Compiled) {
        let End::Exit {
            macro_text: Some(text),
        } = &block.end
        else {
            return;
        };

        if self.options.mode == crate::Mode::Vcl {
            // A leading newline, then the macro line indented by ten
            // spaces (observed from vcl 1.4beta7, `exitm-replaces-exit`,
            // `label-exitm`).
            output.assembly.push_str(&format!("\n          {text}\n"));
        } else {
            output.assembly.push_str(text);
            if !text.ends_with('\n') {
                output.assembly.push('\n');
            }
        }
    }

    /// Prints the pass-through text of the region. vcl writes a newline
    /// before and after a pass-through block (observed from vcl 1.4beta7,
    /// `label-passthrough`).
    fn emit_raw_text(&self, output: &mut Compiled) {
        let vcl = self.options.mode == crate::Mode::Vcl && !self.region.raw_text.is_empty();
        if vcl {
            output.assembly.push('\n');
        }
        for text in &self.region.raw_text {
            output.assembly.push_str(text);
            output.assembly.push('\n');
        }
        if vcl {
            output.assembly.push('\n');
        }
    }
}

/// A register's name as the source spells it, for vcl's `-c` comments: a
/// web split from a source register takes that register's name, and a
/// physical register name prints in capitals (`VI00`, raw code's `VF04`).
fn source_name(program: &Source, region: &Region, id: usize) -> String {
    let id = region.web_sources.get(&id).copied().unwrap_or(id);
    let name = &program.registers.named(id).name;
    let lower = name.to_ascii_lowercase();
    let physical = (lower.starts_with("vf") || lower.starts_with("vi"))
        && lower.len() == 4
        && lower[2..].bytes().all(|byte| byte.is_ascii_digit());
    if physical {
        name.to_ascii_uppercase()
    } else {
        name.clone()
    }
}

/// What vcl's optimizer comment (`; _LNOPT_w=...`) reports for one table.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TableNote {
    /// The linear strategy whose table won; none for tables kept as they
    /// are (raw code, installed loop parts).
    pub(super) strategy: Option<schedule::Strategy>,
    /// A loop kernel's winning loop strategy (an `_LPOPT_` table).
    pub(super) kernel_strategy: Option<schedule::Strategy>,
    /// The wrangled table's lower bound.
    pub(super) lower_bound: Option<i32>,
}

impl TableNote {
    /// The comment line: `; _LNOPT_w=[ strategy ] printed [a b] walked   [name]`
    /// (observed from vcl 1.4beta7; `docs/OUTPUT_FORMAT.md`). `printed` is
    /// the rows printed and `walked` the rows walked, skipped ones included.
    /// The bracketed pair is reproduced only in part: `a` is the wrangled
    /// table's lower bound, or `walked` for a table without one, and `b` is
    /// zero (a loop kernel repeats `a`).
    pub(super) fn comment(&self, printed: usize, walked: usize, name: &str) -> String {
        let (kind, strategy) = match self.kernel_strategy {
            Some(strategy) => ('P', Some(strategy)),
            None => ('N', self.strategy),
        };
        let strategy = strategy.map_or(String::new(), |s| format!("{} ", s.name()));
        let first = if walked == 0 {
            0
        } else {
            self.lower_bound.map_or(walked as i64, i64::from)
        };
        let second = if kind == 'P' { first } else { 0 };
        format!("; _L{kind}OPT_w=[ {strategy}] {printed} [{first} {second}] {walked}   [{name}]\n")
    }
}
