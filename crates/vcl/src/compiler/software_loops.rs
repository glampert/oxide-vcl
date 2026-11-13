//! Loop code generation: choosing loop plans, the lpgen modulo schedule and
//! expansion of the software pipeline back into straight-line streams.

use super::*;
use crate::loops::loop_ir;
use crate::loops::pipeline::{LoopOperation, LoopSchedule, PartKind};
use crate::scheduler::schedule::Pipe;

pub(super) fn flow_owner(region: &Region, block: &Block, id: usize) -> usize {
    if let Some(rows) = &block.selected_table
        && let Some(row) = rows.rows.iter().find(|row| row.contains(&Some(id)))
    {
        return row
            .iter()
            .flatten()
            .copied()
            .find(|other| block.operations.contains(other))
            .unwrap_or(id);
    }
    if region.instructions[id].instruction.mode == ParseMode::Raw {
        return block
            .operations
            .iter()
            .copied()
            .find(|&other| region.instructions[other].line == region.instructions[id].line)
            .unwrap_or(id);
    }
    id
}

pub(super) fn pipeline_source(
    program: &Source,
    located: &Located,
    id: usize,
) -> crate::loops::loop_ir::SourceOperation {
    let mut instruction = located.instruction.clone();
    for (operand, register) in instruction.operands.iter_mut().zip(&located.operands) {
        if let Some(register) = register {
            operand.register = Some(program.registers.named(*register).name.clone());
        }
    }
    crate::loops::loop_ir::SourceOperation {
        id,
        instruction,
        fixups: Vec::new(),
    }
}

pub(super) fn normal_loop_plan(
    program: &Source,
    source: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    block_id: usize,
    unique: &str,
    lpgen: Option<&LpgenContext>,
) -> Option<NormalLoopPlan> {
    use crate::loops::loop_schedule::{self, Control};
    let block = &source.blocks[block_id];
    let minimum = block.loop_hints.minimum?;
    let target = self_loop_target(block)?;
    let conditional = *block.operations.last()?;
    let annotated = annotate_loop_block(program, source, graph, analysis, block_id)?;
    if let Some(context) = lpgen {
        return lpgen_normal_plan(
            context,
            block_id,
            unique,
            target,
            conditional,
            minimum,
            annotated.sources,
        );
    }

    let mut body = Vec::new();
    for &id in &block.operations {
        body.push(loop_body_operation(
            program, source, analysis, block_id, id, &annotated,
        )?);
    }
    let hazards = memory_hazards(source, block);
    let problem = loop_schedule::build(
        graph,
        analysis,
        &body,
        Control {
            block: block_id,
            conditional,
            backedge: block_id,
            is_conditional: true,
            enabled: true,
            known_minimum: minimum,
            known_safe: block.loop_hints.safe,
        },
        &hazards,
    )
    .ok()?;
    let floor = problem
        .lower_bound
        .saturating_add(block.loop_hints.extra)
        .max(block.loop_hints.absolute.unwrap_or(0));
    if floor > 256 {
        return None;
    }

    let scheduled = loop_schedule::schedule(
        &problem,
        &loop_schedule::SearchOptions {
            maximum_period: floor.saturating_add(8).min(256),
            extra: block.loop_hints.extra,
            absolute: block.loop_hints.absolute,
            scheduling: ScheduleOptions {
                max_nodes: 4096,
                ..ScheduleOptions::default()
            },
        },
    )
    .ok()?;
    Some((
        crate::loops::pipeline::LoopSchedule {
            period: scheduled.schedule.period?,
            operations: scheduled.operations,
            conditional: scheduled.conditional,
            known_minimum: scheduled.known_minimum,
            known_safe: scheduled.known_safe,
            unique: unique.into(),
            name: target.clone(),
        },
        annotated.sources,
        KernelPins::new(),
        Some(scheduled.schedule.strategy),
    ))
}

/// The branch target of a block that ends in a conditional branch to one of
/// its own labels (a self loop), if it is one.
fn self_loop_target(block: &Block) -> Option<&String> {
    let End::Branch {
        target: Some(target),
        conditional: true,
        call: false,
        ..
    } = &block.end
    else {
        return None;
    };
    if !block
        .labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(target))
    {
        return None;
    }
    Some(target)
}

/// The block's source operations with their induction annotations.
fn annotate_loop_block(
    program: &Source,
    source: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    block_id: usize,
) -> Option<crate::loops::loop_ir::AnnotatedSources> {
    let block = &source.blocks[block_id];
    // Mark address operands before scheduling: these proven affine uses may
    // cross their VI setter, then recover the source address from clone epochs.
    let sources: Vec<_> = block
        .operations
        .iter()
        .map(|&id| pipeline_source(program, &source.instructions[id], id))
        .collect();
    let bindings: Vec<_> = block
        .operations
        .iter()
        .flat_map(|&id| {
            source.instructions[id]
                .operands
                .iter()
                .enumerate()
                .filter_map(move |(operand, register)| {
                    Some(crate::loops::loop_ir::OperandFlow {
                        instruction: id,
                        operand,
                        operation: id,
                        register: (*register)?,
                    })
                })
        })
        .collect();
    crate::loops::loop_ir::annotate_sources(&sources, graph, analysis, block_id, &bindings).ok()
}

/// Plan a normal loop with the lpgen schedule on the webbed region.
fn lpgen_normal_plan(
    context: &LpgenContext,
    block_id: usize,
    unique: &str,
    target: &String,
    conditional: usize,
    minimum: u32,
    mut sources: Vec<crate::loops::loop_ir::SourceOperation>,
) -> Option<NormalLoopPlan> {
    let source = &context.region;
    let block = &source.blocks[block_id];
    // Random-unit operations and external transfers keep their linear
    // schedule (expansion does not yet model their effects).
    if block.operations.iter().any(|&id| {
        let instruction = &source.instructions[id].instruction;
        instruction.mode != ParseMode::Vcl
            || instruction.modifiers != 0
            || instruction.metadata.unit_mask & (unit::RANDOM | unit::EXTERNAL) != 0
    }) {
        return None;
    }

    let (period, operations, pins, strategy) =
        lpgen_schedule(context, block_id, &mut sources, source.instructions.len())?;
    Some((
        crate::loops::pipeline::LoopSchedule {
            period,
            operations,
            conditional,
            known_minimum: minimum,
            known_safe: block.loop_hints.safe,
            unique: unique.into(),
            // The original names the parts after the stream (its first
            // label), not the branch target.
            name: block.labels.first().unwrap_or(target).clone(),
        },
        sources,
        pins,
        Some(strategy),
    ))
}

/// The loop scheduler's description of one source operation of a loop body.
fn loop_body_operation(
    program: &Source,
    source: &Region,
    analysis: &flow::Analysis,
    block_id: usize,
    id: usize,
    annotated: &crate::loops::loop_ir::AnnotatedSources,
) -> Option<crate::loops::loop_schedule::Operation> {
    use crate::loops::loop_schedule::{self, Effect};
    let located = &source.instructions[id];
    if located.instruction.mode != ParseMode::Vcl || located.instruction.modifiers != 0 {
        return None;
    }
    let metadata = located.instruction.metadata;
    // External transfers and volatile random state need additional original
    // effect/lock rules. Retain their legal linear schedule for now.
    if metadata.unit_mask & (unit::DIVIDE | unit::EXTERNAL) != 0 {
        return None;
    }

    let mut scheduling = Operation::new(id, metadata.issue_mask);
    scheduling.unit_mask = metadata.unit_mask;
    scheduling.throughput = metadata.throughput.max(1);
    scheduling.reads = located
        .effects
        .reads
        .iter()
        .map(|access| RegisterAccess {
            register: access.register,
            components: access.mask,
        })
        .collect();
    scheduling.writes = located
        .effects
        .writes
        .iter()
        .map(|access| RegisterAccess {
            register: access.register,
            components: access.mask,
        })
        .collect();
    scheduling.is_branch = metadata.is_branch();
    let read_delays = if scheduling.is_branch {
        branch_read_delays(program, source, analysis, id)
    } else {
        BTreeMap::new()
    };

    let escaping_write = located.effects.writes.iter().any(|write| {
        analysis.live_out[block_id]
            .iter()
            .any(|live| live.register == write.register && live.mask & write.mask != 0)
    });
    let effect = if escaping_write {
        // Loop-carried/escaping state must not advance an unobserved
        // iteration beyond an early exit (notably VI induction outputs).
        Effect::Critical
    } else if metadata.is_memory() {
        if metadata.is_store() {
            Effect::Store
        } else {
            Effect::MemoryRead
        }
    } else {
        Effect::Pure
    };
    Some(loop_schedule::Operation {
        id,
        scheduling,
        write_latencies: located.effects.latencies.clone(),
        read_delays,
        relaxed_reads: annotated
            .sources
            .iter()
            .find(|source| source.id == id)?
            .fixups
            .iter()
            .filter_map(|fixup| located.operands[usize::from(fixup.operand)])
            .collect(),
        effect,
        is_nop: metadata.name == "nop",
    })
}

/// Integer registers a branch reads one cycle late: those whose writers are
/// neither memory operations nor readers of the status flags.
fn branch_read_delays(
    program: &Source,
    source: &Region,
    analysis: &flow::Analysis,
    id: usize,
) -> BTreeMap<usize, u16> {
    let located = &source.instructions[id];
    let mut read_delays = BTreeMap::new();
    for access in &located.effects.reads {
        if program.registers.named(access.register).family != Family::Integer {
            continue;
        }
        let writers: Vec<_> = analysis
            .reads
            .iter()
            .filter(|read| read.operation == id && read.register == access.register)
            .flat_map(|read| &read.definitions)
            .filter_map(|definition| match definition {
                Definition::Operation(writer) => Some(&source.instructions[*writer]),
                _ => None,
            })
            .collect();
        if writers.iter().all(|writer| {
            !writer.instruction.metadata.is_memory()
                && !writer.effects.reads.iter().any(|read| {
                    matches!(
                        program.registers.named(read.register).name.as_str(),
                        "STAT_ALU"
                            | "STAT_DIV"
                            | "STAT_ALU_S"
                            | "STAT_DIV_S"
                            | "MAC"
                            | "CLIP"
                            | "CLIPA"
                    )
                })
        }) {
            read_delays.insert(access.register, 1);
        }
    }
    read_delays
}

/// Dependencies that keep the block's memory operations in source order,
/// including the wrap to the next iteration.
fn memory_hazards(source: &Region, block: &Block) -> Vec<Dependency> {
    // Memory order is independent of escaping-state classification: an
    // autoincrement load may also be Critical because it updates a live VI.
    let memory: Vec<_> = block
        .operations
        .iter()
        .enumerate()
        .filter_map(|(index, &id)| {
            source.instructions[id]
                .instruction
                .metadata
                .is_memory()
                .then_some(index)
        })
        .collect();
    memory
        .iter()
        .enumerate()
        .map(|(index, &from)| Dependency {
            from,
            to: memory[(index + 1) % memory.len()],
            distance: u16::from(index + 1 == memory.len()),
            latency: 1,
        })
        .collect()
}

/// The region as the original loop optimizer sees it: after pruning,
/// register webs and external assignment.
pub(super) struct LpgenContext {
    pub(super) program: Source,
    pub(super) region: Region,
    pub(super) graph: flow::Graph,
    pub(super) analysis: flow::Analysis,
    /// Move registers named so far (`move__mN`), across the region's loops.
    pub(super) moves: std::cell::Cell<usize>,
}

pub(super) fn lpgen_context(
    program: &Source,
    region: &Region,
) -> Result<LpgenContext, CompileError> {
    let graph = build_flow_graph(program, region)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    let mut web_program = program.clone();
    let webs = construct_register_webs(&mut web_program, region, &analysis)?;
    let graph = build_flow_graph(&web_program, &webs)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    let mut bound = external_assignment::assign(&web_program, &webs, &graph, &analysis)?;
    // Webs rename operand IDs; later passes re-lower from spellings.
    for located in &mut bound.instructions {
        for (operand, register) in located
            .instruction
            .operands
            .iter_mut()
            .zip(&located.operands)
        {
            if let Some(register) = register {
                operand.register = Some(web_program.registers.named(*register).name.clone());
            }
        }
    }
    let graph = build_flow_graph(&web_program, &bound)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    Ok(LpgenContext {
        program: web_program,
        region: bound,
        graph,
        analysis,
        moves: std::cell::Cell::new(0),
    })
}

/// Loop-allocated physical register name per kernel access: (source
/// operation ID, write, register name).
pub(super) type KernelPins = BTreeMap<(usize, bool, String), String>;

/// A normal loop's schedule, its source operations, the loop allocation's
/// pins, and the loop optimizer strategy that won (none for loops scheduled
/// without the loop optimizer).
pub(super) type NormalLoopPlan = (
    crate::loops::pipeline::LoopSchedule,
    Vec<crate::loops::loop_ir::SourceOperation>,
    KernelPins,
    Option<crate::scheduler::schedule::Strategy>,
);

/// Schedule a normal loop body with the lpgen port on the
/// webbed region. Retargeted operands are rewritten in `sources` (by operand
/// index) and synthesized moves appended with IDs from `next_id` on.
pub(super) fn lpgen_schedule(
    context: &LpgenContext,
    block_id: usize,
    sources: &mut Vec<crate::loops::loop_ir::SourceOperation>,
    next_id: usize,
) -> Option<(
    u16,
    Vec<crate::loops::pipeline::LoopOperation>,
    KernelPins,
    crate::scheduler::schedule::Strategy,
)> {
    let source = &context.region;
    let analysis = &context.analysis;
    let block = &source.blocks[block_id];
    let live: Vec<_> = block
        .operations
        .iter()
        .copied()
        .filter(|id| analysis.live_operations.contains(id))
        .collect();
    if live.len() != block.operations.len() || live.len() != sources.len() {
        return None;
    }

    let mut table = lpgen_wrangle_table(context, block_id, &live)?;
    let mut outcome = lpgen_optimize_loop(&table, block, block_id)?;
    if context.program.fixes.contains(Fix::Alias) {
        (table, outcome) = order_same_locations(context, block_id, &live, table, outcome)?;
    }
    let period = outcome.period;
    let base = live.len();
    let mut rewrite = LpgenRewrite::new(context, &table, &outcome, base);
    rewrite.collapse_noop_moves();
    rewrite.retarget_sources(&live, sources)?;
    if std::env::var_os("VCL_LPGEN_BODY").is_some() {
        rewrite.trace_body(block_id, sources);
    }

    let mut operations = Vec::new();
    for (index, &id) in live.iter().enumerate() {
        if rewrite.dropped(index) {
            continue;
        }
        place_loop_operation(&outcome, period, index, id, &mut operations)?;
        operations.last_mut()?.is_nop = source.instructions[id].instruction.metadata.name == "nop";
    }
    rewrite.place_moves(next_id, period, sources, &mut operations)?;
    let pins = rewrite.kernel_pins(&live, next_id);

    // Dropped copies leave the expanded loop.
    let removed: Vec<usize> = (0..live.len())
        .filter(|&index| rewrite.dropped(index))
        .map(|index| live[index])
        .collect();
    sources.retain(|source| !removed.contains(&source.id));
    Some((
        u16::try_from(period).ok()?,
        operations,
        pins,
        outcome.strategy,
    ))
}

/// With the `alias` fix: when the loop schedule runs two accesses to the
/// same location out of order (`memory_alias::breaks_order`), the loop is
/// searched again with the locations as resources of its table. `None`
/// leaves the loop unpipelined, where linear tables keep the order.
fn order_same_locations(
    context: &LpgenContext,
    block_id: usize,
    live: &[usize],
    mut table: WrangledTable,
    outcome: crate::scheduler::wrangle::LoopOutcome,
) -> Option<(WrangledTable, crate::scheduler::wrangle::LoopOutcome)> {
    let source = &context.region;
    let block = &source.blocks[block_id];
    let groups = memory_alias::loop_groups(
        &context.program,
        source,
        &context.analysis,
        &block.operations,
        live,
    );
    let broken = |outcome: &crate::scheduler::wrangle::LoopOutcome| {
        let lines: Vec<i32> = outcome.placements.iter().map(|p| p.line).collect();
        memory_alias::breaks_order(&groups, source, live, &lines, outcome.period)
    };
    if !broken(&outcome) {
        return Some((table, outcome));
    }
    add_alias_resources(&mut table, source, &groups);
    let outcome = lpgen_optimize_loop(&table, block, block_id)?;
    (!broken(&outcome)).then_some((table, outcome))
}

/// The wrangle table of the loop block, with the loop's side-effect
/// categories and memory-tag resources set.
fn lpgen_wrangle_table(
    context: &LpgenContext,
    block_id: usize,
    live: &[usize],
) -> Option<WrangledTable> {
    let source = &context.region;
    let analysis = &context.analysis;
    let mut table = build_wrangle_table(&context.program, source, analysis, block_id, live, false)?;
    // The loop runs on a cloned table; cloning does not copy
    // the forced memory/order lists, so a loop store has
    // no forced predecessors (traced: pv_diff_quad's `sq tmp3` at period 51).
    for insn in &mut table.problem.instructions {
        insn.forced_predecessors.clear();
        insn.forced_successors.clear();
    }

    // Loop side-effect categories: stores, and writes of a
    // value live into an exit (critical).
    let exits: Vec<usize> = context.graph.blocks[block_id]
        .successors
        .iter()
        .copied()
        .filter(|&next| next != block_id)
        .collect();
    for (index, &id) in live.iter().enumerate() {
        let located = &source.instructions[id];
        let store = located.instruction.metadata.is_store();
        let escapes = located.effects.writes.iter().any(|write| {
            exits.iter().any(|&exit| {
                analysis.live_in[exit]
                    .iter()
                    .any(|live| live.register == write.register && live.mask & write.mask != 0)
            })
        });
        table.problem.instructions[index].side_effect = if store {
            1
        } else if escapes {
            2
        } else {
            0
        };
    }
    add_memory_tag_resources(&mut table, &context.program, source);
    Some(table)
}

/// Search the loop periods for the table, starting at the loop bound.
fn lpgen_optimize_loop(
    table: &WrangledTable,
    block: &Block,
    block_id: usize,
) -> Option<crate::scheduler::wrangle::LoopOutcome> {
    // First period: loop bound plus LoopExtra; a LoopAbs above one replaces
    // it unless smaller.
    let hints = &block.loop_hints;
    let mut first = crate::scheduler::wrangle::loop_bound(&table.problem) + i32::from(hints.extra);
    if let Some(absolute) = hints.absolute.map(i32::from).filter(|&a| a > 1) {
        first = first.max(absolute);
    }
    if first > 256 {
        return None;
    }

    crate::scheduler::wrangle::set_trace_block(block_id);
    let outcome = crate::scheduler::wrangle::optimize_loop(
        &table.problem,
        first,
        (first + 16).min(256),
        hints.safe as i32,
        std::env::var("VCL_LPGEN_BUDGET")
            .ok()
            .and_then(|budget| budget.parse().ok())
            .unwrap_or(20_000),
        Some(&table.pools),
    )?;
    if std::env::var_os("VCL_LPGEN_TRACE").is_some() {
        crate::trace!(
            "lpgen {block_id} result {:?} period {} max lid {}",
            outcome.strategy,
            outcome.period,
            outcome.max_iteration
        );
    }
    Some(outcome)
}

/// Add the placed instruction `index` of the outcome, as source operation
/// `id`, to the loop operations.
fn place_loop_operation(
    outcome: &crate::scheduler::wrangle::LoopOutcome,
    period: i32,
    index: usize,
    id: usize,
    operations: &mut Vec<crate::loops::pipeline::LoopOperation>,
) -> Option<()> {
    let position = outcome.placements[index];
    if position.line < 0 {
        return None;
    }
    operations.push(LoopOperation {
        id,
        row: (position.line % period) as u16,
        stage: u8::try_from(position.line / period).ok()?,
        pipe: if position.pipe == 0 {
            Pipe::Upper
        } else {
            Pipe::Lower
        },
        is_nop: false,
    });
    Some(())
}

/// The register renaming the loop search produced: moves it synthesized,
/// their names, the loop-local allocation and the moves that collapse.
struct LpgenRewrite<'a> {
    context: &'a LpgenContext,
    table: &'a WrangledTable,
    outcome: &'a crate::scheduler::wrangle::LoopOutcome,
    /// Program register spelling of each table register.
    spelling: BTreeMap<usize, String>,
    /// Moves named before this loop.
    used: usize,
    /// Number of source instructions; synthesized moves follow them.
    base: usize,
    base_registers: usize,
    /// Destination to source of the moves allocated to their source's
    /// register.
    collapsed: BTreeMap<usize, usize>,
}

impl<'a> LpgenRewrite<'a> {
    fn new(
        context: &'a LpgenContext,
        table: &'a WrangledTable,
        outcome: &'a crate::scheduler::wrangle::LoopOutcome,
        base: usize,
    ) -> Self {
        let base_registers = table.keys.len();
        // Program register spelling of each table register.
        let mut spelling: BTreeMap<usize, String> = BTreeMap::new();
        for (index, insn) in table.problem.instructions.iter().enumerate() {
            for (o, operand) in insn.operands.iter().enumerate() {
                let register = table.operand_registers[index][o];
                // The `alias` fix's resources have no source register, and
                // no move renames them.
                if register == memory_alias::NO_PROGRAM_REGISTER {
                    continue;
                }
                spelling
                    .entry(operand.register)
                    .or_insert_with(|| context.program.registers.named(register).name.clone());
            }
        }
        let used = context.moves.get();
        context
            .moves
            .set(used + outcome.problem.registers.len() - base_registers);
        LpgenRewrite {
            context,
            table,
            outcome,
            spelling,
            used,
            base,
            base_registers,
            collapsed: BTreeMap::new(),
        }
    }

    /// The program name of a table register (none for hardware registers).
    fn name_of(&self, register: usize) -> Option<String> {
        if register >= self.base_registers {
            return Some(format!(
                "move__m{}",
                self.used + register - self.base_registers
            ));
        }
        match &self.table.keys[register] {
            WrangleKey::Hardware(_) => None,
            _ => self.spelling.get(&register).cloned(),
        }
    }

    /// Loop local allocation of each table register.
    fn physical(&self, register: usize) -> Option<String> {
        let info = &self.outcome.problem.registers[register];
        let index = info.physical.or_else(|| {
            self.outcome
                .allocation
                .iter()
                .find(|&&(r, _)| r == register)
                .map(|&(_, p)| p)
        })?;
        Some(physical_name(
            if info.pool == 1 {
                Family::Vector
            } else {
                Family::Integer
            },
            index,
        ))
    }

    /// Whether the instruction is a move (synthesized or detected copy).
    fn copy(&self, index: usize) -> bool {
        index >= self.base || self.outcome.problem.instructions[index].dual_copy
    }

    /// Record the moves allocated to their source's register.
    ///
    /// A move allocated to its source's register is a no-op; the original
    /// drops it (normalize moves) and its readers read the source again.
    /// This holds for every instruction with a move ID: synthesized moves
    /// and the copies move detection found (quad's `next_output` chain).
    fn collapse_noop_moves(&mut self) {
        let mut collapsed: BTreeMap<usize, usize> = BTreeMap::new();
        for (index, mv) in self.outcome.problem.instructions.iter().enumerate() {
            if !self.copy(index) || self.outcome.placements[index].line < 0 {
                continue;
            }
            let (write, read) = (mv.operands[0].register, mv.operands[1].register);
            if self.physical(write).is_some() && self.physical(write) == self.physical(read) {
                collapsed.insert(write, read);
            }
        }
        self.collapsed = collapsed;
    }

    /// Follow collapsed moves back to the register they copy.
    fn resolve(&self, mut register: usize) -> usize {
        while let Some(&source) = self.collapsed.get(&register) {
            register = source;
        }
        register
    }

    /// Whether the instruction is a collapsed move that leaves the loop.
    fn dropped(&self, index: usize) -> bool {
        self.copy(index)
            && self
                .collapsed
                .contains_key(&self.outcome.problem.instructions[index].operands[0].register)
    }

    /// Retargeted operands of source instructions.
    fn retarget_sources(
        &self,
        live: &[usize],
        sources: &mut [crate::loops::loop_ir::SourceOperation],
    ) -> Option<()> {
        use crate::scheduler::wrangle::Direction as WDirection;
        let source = &self.context.region;
        for (index, &id) in live.iter().enumerate() {
            if self.dropped(index) {
                continue;
            }
            let before = &self.table.problem.instructions[index].operands;
            let after = &self.outcome.problem.instructions[index].operands;
            for (o, (old, new)) in before.iter().zip(after).enumerate() {
                let target = self.resolve(new.register);
                if old.register == target {
                    continue;
                }
                let name = self.name_of(target)?;
                let program_register = self.table.operand_registers[index][o];
                let direction = match old.direction {
                    WDirection::Read => Direction::Read,
                    WDirection::Write => Direction::Write,
                };
                // A move copies some components only: rename the operands it
                // covers (an instruction may read a register twice, as in
                // `mulw.xyz v, v, vw`).
                let covered = if target >= self.base_registers {
                    self.outcome.problem.instructions[self.base..]
                        .iter()
                        .find(|mv| mv.operands[0].register == target)
                        .map_or(0xff, |mv| mv.operands[0].mask)
                } else {
                    0xff
                };
                let located = &source.instructions[id];
                let mut found = false;
                for (i, operand) in located.instruction.operands.iter().enumerate() {
                    if operand.direction == direction
                        && located.operands.get(i).copied().flatten() == Some(program_register)
                    {
                        let mask = if operand.class == class::INTEGER {
                            1
                        } else {
                            operand.mask.0
                        };
                        if mask & !covered != 0 {
                            continue;
                        }
                        sources[index].instruction.operands[i].register = Some(name.clone());
                        found = true;
                    }
                }
                if !found {
                    return None;
                }
            }
        }
        Some(())
    }

    /// Print the rewritten body and its moves (`VCL_LPGEN_BODY`).
    fn trace_body(&self, block_id: usize, sources: &[crate::loops::loop_ir::SourceOperation]) {
        let outcome = self.outcome;
        for (index, source) in sources.iter().enumerate() {
            crate::trace!(
                "body {block_id} {index} {} {:?} at {:?}",
                source.instruction.mnemonic,
                source
                    .instruction
                    .operands
                    .iter()
                    .map(|o| (o.register.clone(), o.mask.0, o.direction))
                    .collect::<Vec<_>>(),
                outcome.placements[index]
            );
        }
        for (k, mv) in outcome.problem.instructions[self.base..].iter().enumerate() {
            crate::trace!(
                "body {block_id} move {} {:?} -> {:?} at {:?} collapsed {}",
                self.base + k,
                self.name_of(mv.operands[1].register),
                self.name_of(mv.operands[0].register),
                outcome.placements[self.base + k],
                self.collapsed.contains_key(&mv.operands[0].register)
            );
        }
    }

    /// Synthesized moves (`vmove`/`imove`, generated copies): append them to
    /// `sources` with IDs from `next_id` on and place them.
    fn place_moves(
        &self,
        next_id: usize,
        period: i32,
        sources: &mut Vec<crate::loops::loop_ir::SourceOperation>,
        operations: &mut Vec<crate::loops::pipeline::LoopOperation>,
    ) -> Option<()> {
        let outcome = self.outcome;
        for (k, mv) in outcome.problem.instructions[self.base..].iter().enumerate() {
            let index = self.base + k;
            // A move destroyed by backtracking stays unplaced.
            if outcome.placements[index].line < 0
                || self.collapsed.contains_key(&mv.operands[0].register)
            {
                continue;
            }
            let instruction = self.move_instruction(mv)?;
            let id = next_id + k;
            sources.push(crate::loops::loop_ir::SourceOperation {
                id,
                instruction,
                fixups: Vec::new(),
            });
            place_loop_operation(outcome, period, index, id, operations)?;
        }
        Some(())
    }

    /// The `vmove`/`imove` instruction for a synthesized move.
    fn move_instruction(&self, mv: &crate::scheduler::wrangle::Instruction) -> Option<Instruction> {
        let (write, read) = (&mv.operands[0], &mv.operands[1]);
        let vector = self.outcome.problem.registers[write.register].pool == 1;
        let (kind, descriptor, operand_class) = if vector {
            (
                crate::analysis::copies::Kind::Vector,
                id::VMOVE,
                class::VECTOR,
            )
        } else {
            (
                crate::analysis::copies::Kind::Integer,
                id::IMOVE,
                class::INTEGER,
            )
        };
        // VI operands carry component x; an ordinary VI instruction's own
        // mask is the parser default.
        let (mask, instruction_mask) = if vector {
            (instruction::Mask(write.mask), instruction::Mask(write.mask))
        } else {
            (instruction::Mask::X, instruction::Mask::XYZW)
        };
        let operand = |name: String, direction: Direction| crate::parser::Operand {
            text: name.clone(),
            class: operand_class,
            direction,
            implicit: false,
            mask,
            register: Some(name),
            forced_register: None,
            immediate: None,
            immediate_kind: instruction::ImmediateKind::NONE,
            tags: Vec::new(),
            field_letters: None,
        };
        let metadata = &instruction::INSTRUCTIONS[usize::from(descriptor)];
        Some(Instruction {
            metadata,
            mnemonic: metadata.name.into(),
            mask: instruction_mask,
            modifiers: 0,
            operands: vec![
                operand(self.name_of(write.register)?, Direction::Write),
                operand(self.name_of(self.resolve(read.register))?, Direction::Read),
            ],
            mode: ParseMode::Vcl,
            raw_reorder: false,
            loop_id: 0,
            optional: false,
            generated_copy: Some(kind),
            fixed_offset: false,
        })
    }

    /// The loop-allocated physical register of each kernel access.
    fn kernel_pins(&self, live: &[usize], next_id: usize) -> KernelPins {
        use crate::scheduler::wrangle::Direction as WDirection;
        let outcome = self.outcome;
        let mut pins = KernelPins::new();
        let mut ids: Vec<usize> = live.to_vec();
        ids.extend((0..outcome.problem.instructions.len() - self.base).map(|k| next_id + k));
        for (index, insn) in outcome.problem.instructions.iter().enumerate() {
            if outcome.placements[index].line < 0 || self.dropped(index) {
                continue;
            }
            for operand in &insn.operands {
                if outcome.problem.registers[operand.register].pool == 0 {
                    continue;
                }
                let register = self.resolve(operand.register);
                let (Some(name), Some(physical)) =
                    (self.name_of(register), self.physical(register))
                else {
                    continue;
                };
                pins.insert(
                    (
                        ids[index],
                        operand.direction == WDirection::Write,
                        name.to_ascii_lowercase(),
                    ),
                    physical,
                );
            }
        }
        pins
    }
}

/// Whether the block is an explicitly staged raw loop.
fn is_staged_block(block: &Block, source: &Region) -> bool {
    block.operations.iter().any(|&id| {
        let instruction = &source.instructions[id].instruction;
        instruction.mode == ParseMode::Raw && instruction.loop_id != 0
    })
}

/// Expand explicitly staged raw self loops before rebuilding source flow.
/// Their source rows are already the selected modulo placement; -L controls
/// optimizer search and does not erase user-provided stage information.
pub(super) fn expand_software_loops(
    program: &mut Source,
    source: &Region,
    unique: &str,
    normal_loops: bool,
) -> Result<Region, CompileError> {
    // The original optimizes normal loops after webs and external
    // assignment, and expands that region.
    // When the lpgen port schedules a loop, expand the webbed region.
    let lpgen = normal_loops && std::env::var_os("VCL_OLD_LOOPS").is_none();
    let mut plans = BTreeMap::new();
    let mut webbed = None;
    if lpgen && source.blocks.iter().any(|b| b.loop_hints.minimum.is_some()) {
        (plans, webbed) = lpgen_loop_plans(program, source, unique)?;
    }

    let source = webbed.as_ref().unwrap_or(source);
    let mut expansion = Expansion {
        result: source.clone(),
        generated_loop: 0,
        repairs: Vec::new(),
        kernel_blocks: BTreeSet::new(),
        part_blocks: BTreeSet::new(),
        retarget: BTreeMap::new(),
    };
    expansion.result.blocks.clear();
    let graph = build_flow_graph(program, source)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    for (block_id, block) in source.blocks.iter().enumerate() {
        let staged = is_staged_block(block, source);
        let normal = if staged || !normal_loops {
            None
        } else if lpgen {
            plans.remove(&block_id)
        } else {
            normal_loop_plan(program, source, &graph, &analysis, block_id, unique, None)
        };
        if (!staged && normal.is_none()) || block.selected_table.is_some() {
            expansion.result.blocks.push(block.clone());
            continue;
        }
        expansion.expand_block(program, source, block, normal, unique, lpgen)?;
    }

    let Expansion {
        mut result,
        repairs,
        kernel_blocks,
        part_blocks,
        retarget,
        ..
    } = expansion;
    let graph = build_flow_graph(program, &result)?;
    let analysis = graph.analyze().map_err(|e| internal(0, e))?;
    let blocks: BTreeMap<_, _> = result
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(block, value)| value.operations.iter().map(move |&id| (id, block)))
        .collect();
    retarget_loop_branches(&mut result, &part_blocks, &retarget);
    if webbed.is_some() {
        pin_forced_registers(program, source, &mut result);
    }
    release_part_registers(
        &mut result,
        &blocks,
        &analysis,
        &part_blocks,
        &kernel_blocks,
    );
    repair_expanded_loops(program, &mut result, repairs, &blocks, &analysis)?;
    Ok(result)
}

/// Plan every normal loop of the region with the lpgen port, on the webbed
/// region; the webbed region is returned when any loop was planned (the
/// program then takes over its registers).
fn lpgen_loop_plans(
    program: &mut Source,
    source: &Region,
    unique: &str,
) -> Result<(BTreeMap<usize, NormalLoopPlan>, Option<Region>), CompileError> {
    let mut plans = BTreeMap::new();
    let mut webbed = None;
    let context = lpgen_context(program, source)?;
    for (block_id, block) in context.region.blocks.iter().enumerate() {
        if is_staged_block(block, &context.region) {
            continue;
        }
        if let Some(plan) = normal_loop_plan(
            &context.program,
            &context.region,
            &context.graph,
            &context.analysis,
            block_id,
            unique,
            Some(&context),
        ) {
            plans.insert(block_id, plan);
        }
    }
    if !plans.is_empty() {
        *program = context.program;
        webbed = Some(context.region);
    }
    Ok((plans, webbed))
}

/// The output region and bookkeeping while loops are expanded block by block.
struct Expansion {
    result: Region,
    generated_loop: u16,
    /// Expanded loops with the global instruction ID of each clone.
    repairs: Vec<(loop_ir::ExpandedLoop, Vec<usize>)>,
    kernel_blocks: BTreeSet<usize>,
    part_blocks: BTreeSet<usize>,
    /// Other streams' branch targets and the loop entry part they now name.
    retarget: BTreeMap<String, String>,
}

/// What the parts of one expanded loop are built from.
struct ExpandedBlock<'a> {
    source: &'a Region,
    block: &'a Block,
    expanded: &'a loop_ir::ExpandedLoop,
    /// Global instruction ID of each clone.
    ids: &'a [usize],
    pins: &'a KernelPins,
    /// Parts that are the target of a branch.
    targets: BTreeSet<usize>,
    staged: bool,
    lpgen: bool,
    loop_strategy: Option<crate::scheduler::schedule::Strategy>,
}

impl Expansion {
    /// Expand one staged raw loop or planned normal loop into its parts.
    fn expand_block(
        &mut self,
        program: &mut Source,
        source: &Region,
        block: &Block,
        normal: Option<NormalLoopPlan>,
        unique: &str,
        lpgen: bool,
    ) -> Result<(), CompileError> {
        let staged = is_staged_block(block, source);
        let End::Branch {
            target: Some(target),
            conditional: true,
            call: false,
            ..
        } = &block.end
        else {
            return Err(error(0, CompileMessage::StagedRawLoopBranch));
        };
        if !block
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(target))
        {
            return Err(error(0, CompileMessage::StagedRawLoopTarget));
        }

        let (mut schedule, sources, pins, loop_strategy) = if let Some(normal) = normal {
            normal
        } else {
            let (schedule, sources) = staged_raw_schedule(program, source, block, target, unique)?;
            (schedule, sources, KernelPins::new(), None)
        };
        if block.loop_hints.minimum.is_none() {
            schedule.known_safe = 10000;
        }
        self.generated_loop = self
            .generated_loop
            .checked_add(1)
            .ok_or_else(|| error(0, CompileMessage::TooManyLoops))?;

        // Explicit raw tables expand before Init_immFixup in the original
        // driver. Their authored addresses already
        // describe staged execution. Only normal-loop motion has annotations.
        let expanded = loop_ir::expand(&schedule, &sources, self.generated_loop)
            .map_err(|e| internal(0, e))?;
        let ids = self.lower_clones(program, source, &expanded)?;
        let targets: BTreeSet<_> = expanded
            .plan
            .parts
            .iter()
            .filter_map(|part| part.branch.map(|branch| branch.target))
            .collect();
        let context = ExpandedBlock {
            source,
            block,
            expanded: &expanded,
            ids: &ids,
            pins: &pins,
            targets,
            staged,
            lpgen,
            loop_strategy,
        };
        for part_id in 0..expanded.plan.parts.len() {
            self.emit_part(program, &context, part_id);
        }
        self.repairs.push((expanded, ids));
        Ok(())
    }

    /// Lower the expanded clones into the result; their global IDs.
    fn lower_clones(
        &mut self,
        program: &mut Source,
        source: &Region,
        expanded: &loop_ir::ExpandedLoop,
    ) -> Result<Vec<usize>, CompileError> {
        let mut ids = Vec::new();
        for clone in &expanded.operations {
            let mut instruction = clone.instruction.clone();
            instruction.mode = ParseMode::Vcl;
            instruction.loop_id = 0;
            let source_line = match clone.origin {
                loop_ir::Origin::Source { id, .. } => source
                    .instructions
                    .get(id)
                    .map_or(0, |located| located.line),
                loop_ir::Origin::DrainBranch => 0,
            };
            let id = self.result.instructions.len();
            self.result
                .instructions
                .push(program.lower(instruction, source_line)?);
            ids.push(id);
        }
        Ok(ids)
    }

    /// Append the block of one part of an expanded loop to the result.
    fn emit_part(&mut self, program: &mut Source, context: &ExpandedBlock, part_id: usize) {
        let ExpandedBlock {
            source,
            block,
            expanded,
            ids,
            pins,
            ..
        } = *context;
        let part = &expanded.plan.parts[part_id];
        // Every clone carries the loop allocation's registers; see the
        // split of part-local registers below.
        if matches!(part.kind, PartKind::Kernel) {
            self.kernel_blocks.insert(self.result.blocks.len());
        }
        self.part_blocks.insert(self.result.blocks.len());
        if !pins.is_empty() {
            self.pin_part_registers(program, context, part_id);
        }

        let fixed: Vec<_> = expanded.rows[part_id]
            .iter()
            .map(|row| row.map(|id| id.map(|id| ids[id])))
            .collect();
        let mut labels =
            if context.targets.contains(&part_id) || matches!(part.kind, PartKind::Entry) {
                vec![part.name.clone()]
            } else {
                Vec::new()
            };
        if matches!(part.kind, PartKind::Entry) {
            labels.extend(block.labels.clone());
            // Other streams' branches to the loop now name its entry.
            if !pins.is_empty() {
                for label in &block.labels {
                    self.retarget
                        .insert(label.to_ascii_lowercase(), part.name.clone());
                }
            }
        }
        let end = part.branch.map_or(End::Fallthrough, |branch| End::Branch {
            target: Some(expanded.plan.parts[branch.target].name.clone()),
            tags: Vec::new(),
            conditional: branch.source_id.is_some(),
            call: false,
        });
        let last_part = part_id + 1 == expanded.plan.parts.len();
        self.result.blocks.push(Block {
            labels,
            start_fence: matches!(part.kind, PartKind::Entry) && block.start_fence,
            debug_top: if matches!(part.kind, PartKind::Entry) {
                block.debug_top
            } else {
                0
            },
            end_fence: last_part && block.end_fence,
            debug_end: if last_part { block.debug_end } else { 0 },
            operations: fixed.iter().flatten().flatten().copied().collect(),
            end,
            selected_table: Some(SelectedTable {
                rows: fixed,
                mode: part_table_mode(source, context, &part.kind),
                installed: !context.staged
                    && context.lpgen
                    && !matches!(part.kind, PartKind::Kernel),
                kernel_strategy: if matches!(part.kind, PartKind::Kernel) {
                    context.loop_strategy
                } else {
                    None
                },
            }),
            part_name: Some(part.name.clone()),
            ..Block::default()
        });
    }

    /// Pin the registers the loop allocation gave to the part's clones.
    fn pin_part_registers(
        &mut self,
        program: &mut Source,
        context: &ExpandedBlock,
        part_id: usize,
    ) {
        for clone in context.expanded.rows[part_id].iter().flatten().flatten() {
            let loop_ir::Origin::Source { id: origin, .. } =
                context.expanded.operations[*clone].origin
            else {
                continue;
            };
            let global = context.ids[*clone];
            let located = self.result.instructions[global].clone();
            for (write, accesses) in [
                (false, &located.effects.reads),
                (true, &located.effects.writes),
            ] {
                for access in accesses {
                    let register = program.registers.named(access.register);
                    let key = (origin, write, register.name.to_ascii_lowercase());
                    if let Some(physical) = context.pins.get(&key) {
                        let family = register.family;
                        let id = program.registers.get(physical, family);
                        self.result
                            .pinned
                            .insert((global, write, access.register), id);
                    }
                }
            }
        }
    }
}

/// The table mode of a part of an expanded loop.
fn part_table_mode(source: &Region, context: &ExpandedBlock, kind: &PartKind) -> TableMode {
    if context.staged
        && matches!(kind, PartKind::Prologue { .. } | PartKind::Epilogue { .. })
        && context
            .block
            .operations
            .iter()
            .all(|&id| source.instructions[id].instruction.raw_reorder)
    {
        TableMode::RawLinear
    } else if matches!(kind, PartKind::Kernel) || context.staged {
        TableMode::Fixed
    } else {
        TableMode::Linear
    }
}

/// The loop schedule and source operations of an explicitly staged raw loop.
fn staged_raw_schedule(
    program: &Source,
    source: &Region,
    block: &Block,
    target: &str,
    unique: &str,
) -> Result<(LoopSchedule, Vec<loop_ir::SourceOperation>), CompileError> {
    let rows = staged_raw_rows(source, block)?;
    let period =
        u16::try_from(rows.len()).map_err(|_| error(0, CompileMessage::RawLoopPeriodRange))?;
    let mut operations = Vec::new();
    let mut sources = Vec::new();
    let mut conditional = None;
    for (row, pair) in rows.iter().enumerate() {
        for (pipe, id) in pair
            .iter()
            .enumerate()
            .filter_map(|(pipe, id)| id.map(|id| (pipe, id)))
        {
            let instruction = &source.instructions[id].instruction;
            if instruction.metadata.name == "nop" {
                if instruction.loop_id != 0 {
                    return Err(error(
                        source.instructions[id].line,
                        CompileMessage::LoopIdOnNop,
                    ));
                }
                continue;
            }
            if instruction.metadata.issue_mask & (1 << pipe) == 0 {
                return Err(error(
                    source.instructions[id].line,
                    CompileMessage::WrongRawPipe,
                ));
            }
            if instruction.metadata.is_branch() {
                conditional = Some(id);
            }
            let stage = instruction
                .loop_id
                .checked_sub(1)
                .filter(|&id| id <= 31)
                .ok_or_else(|| {
                    error(
                        source.instructions[id].line,
                        CompileMessage::InvalidRawLoopStage,
                    )
                })?;
            operations.push(LoopOperation {
                id,
                row: row as u16,
                stage: stage as u8,
                pipe: if pipe == 0 { Pipe::Upper } else { Pipe::Lower },
                is_nop: false,
            });
            sources.push(pipeline_source(program, &source.instructions[id], id));
        }
    }

    // The original's raw-table builder rejects a loop-marked table whose
    // largest decoded stage is zero, before later flow/pruning.
    if operations.iter().all(|operation| operation.stage == 0) {
        return Err(error(0, CompileMessage::MaxLoopIdZero));
    }
    let schedule = LoopSchedule {
        period,
        operations,
        conditional: conditional.ok_or_else(|| error(0, CompileMessage::RawLoopWithoutBranch))?,
        known_minimum: block.loop_hints.minimum.unwrap_or(1),
        known_safe: block.loop_hints.safe,
        unique: unique.into(),
        name: target.to_owned(),
    };
    Ok((schedule, sources))
}

/// The block's operations grouped into one pair of pipe slots per source line.
fn staged_raw_rows(
    source: &Region,
    block: &Block,
) -> Result<Vec<[Option<usize>; 2]>, CompileError> {
    let mut rows: Vec<[Option<usize>; 2]> = Vec::new();
    let mut line = None;
    let mut slot = 0;
    for &id in &block.operations {
        let located = &source.instructions[id];
        if line != Some(located.line) {
            rows.push([None, None]);
            line = Some(located.line);
            slot = 0;
        }
        if slot >= 2 || located.instruction.mode != ParseMode::Raw {
            return Err(error(located.line, CompileMessage::InvalidStagedRawPair));
        }
        rows.last_mut().unwrap()[slot] = Some(id);
        slot += 1;
    }
    Ok(rows)
}

/// Other streams' branches to a pipelined loop name its entry part.
fn retarget_loop_branches(
    result: &mut Region,
    part_blocks: &BTreeSet<usize>,
    retarget: &BTreeMap<String, String>,
) {
    for (id, block) in result.blocks.iter_mut().enumerate() {
        if part_blocks.contains(&id) {
            continue;
        }
        let End::Branch {
            target: Some(target),
            ..
        } = &mut block.end
        else {
            continue;
        };
        let Some(name) = retarget.get(&target.to_ascii_lowercase()) else {
            continue;
        };
        for &operation in &block.operations {
            let instruction = &mut result.instructions[operation].instruction;
            if !instruction.metadata.is_branch() {
                continue;
            }
            for operand in &mut instruction.operands {
                if operand.class == class::IMMEDIATE
                    && operand
                        .immediate
                        .as_deref()
                        .is_some_and(|label| label.eq_ignore_ascii_case(target))
                {
                    operand.immediate = Some(name.clone());
                    operand.text = name.clone();
                }
            }
        }
        *target = name.clone();
    }
}

/// Registers bound by the external assignment before loop optimization
/// stay bound after expansion (the original keeps their mappings).
fn pin_forced_registers(program: &mut Source, source: &Region, result: &mut Region) {
    let forced: Vec<(usize, usize)> = source.forced.iter().map(|(&r, &p)| (r, p)).collect();
    let mut physical_ids = BTreeMap::new();
    for (register, base) in forced {
        let family = program.registers.named(register).family;
        let id = program.registers.get(&physical_name(family, base), family);
        physical_ids.insert(register, id);
    }
    for block in &result.blocks {
        for &id in &block.operations {
            let effects = &result.instructions[id].effects;
            for (write, accesses) in [(false, &effects.reads), (true, &effects.writes)] {
                for access in accesses {
                    if let Some(&physical) = physical_ids.get(&access.register) {
                        result
                            .pinned
                            .entry((id, write, access.register))
                            .or_insert(physical);
                    }
                }
            }
        }
    }
}

/// After expansion the original renames, in each prologue/epilogue part,
/// the webs of a loop-allocated register that is neither live into nor
/// out of that part (`VF04__0`, `VF04__1`); local allocation assigns them.
/// A register crossing the part's boundary keeps its kernel register.
fn release_part_registers(
    result: &mut Region,
    blocks: &BTreeMap<usize, usize>,
    analysis: &flow::Analysis,
    part_blocks: &BTreeSet<usize>,
    kernel_blocks: &BTreeSet<usize>,
) {
    if result.pinned.is_empty() {
        return;
    }
    let mut crossing: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (&(id, write, register), &physical) in &result.pinned {
        let Some(&block) = blocks.get(&id) else {
            continue;
        };
        let outside = |operation: usize| blocks.get(&operation) != Some(&block);
        let crosses = if write {
            analysis.reads.iter().any(|read| {
                read.register == register
                    && outside(read.operation)
                    && read.definitions.contains(&Definition::Operation(id))
            }) || analysis.outputs.iter().any(|output| {
                output.register == register
                    && output.definitions.contains(&Definition::Operation(id))
            })
        } else {
            let owner = flow_owner(result, &result.blocks[block], id);
            analysis.reads.iter().any(|read| {
                read.operation == owner
                    && read.register == register
                    && read.definitions.iter().any(|definition| match definition {
                        Definition::Operation(writer) => outside(*writer),
                        _ => true,
                    })
            })
        };
        if crosses {
            crossing.insert((block, physical));
        }
    }

    // A value passing through a part without a reference there also
    // holds its register (a loop-invariant in VF07.xyz keeps VF07).
    let mut physical_of: BTreeMap<usize, usize> = BTreeMap::new();
    for (&(_, _, register), &physical) in &result.pinned {
        physical_of.entry(register).or_insert(physical);
    }
    for &block in part_blocks {
        for access in analysis.live_in[block]
            .iter()
            .chain(&analysis.live_out[block])
        {
            if let Some(&physical) = physical_of.get(&access.register) {
                crossing.insert((block, physical));
            }
        }
    }
    result.pinned.retain(|(id, _, _), physical| {
        blocks.get(id).is_none_or(|&block| {
            !part_blocks.contains(&block)
                || kernel_blocks.contains(&block)
                || crossing.contains(&(block, *physical))
        })
    });
}

/// Re-lower the clones of expanded loops whose address operands the flow
/// of the expanded region corrects.
fn repair_expanded_loops(
    program: &mut Source,
    result: &mut Region,
    repairs: Vec<(loop_ir::ExpandedLoop, Vec<usize>)>,
    blocks: &BTreeMap<usize, usize>,
    analysis: &flow::Analysis,
) -> Result<(), CompileError> {
    for (expanded, ids) in repairs {
        if !expanded
            .operations
            .iter()
            .any(|operation| !operation.fixups.is_empty())
        {
            continue;
        }
        let bindings: Vec<_> = expanded
            .operations
            .iter()
            .flat_map(|operation| {
                let global = ids[operation.id];
                let located = &result.instructions[global];
                let owner = flow_owner(result, &result.blocks[blocks[&global]], global);
                located
                    .operands
                    .iter()
                    .enumerate()
                    .filter_map(move |(operand, register)| {
                        Some(loop_ir::OperandFlow {
                            instruction: operation.id,
                            operand,
                            operation: if located.instruction.operands[operand].direction
                                == Direction::Read
                            {
                                owner
                            } else {
                                global
                            },
                            register: (*register)?,
                        })
                    })
            })
            .collect();
        let repaired = expanded
            .repair_from_flow(&[], analysis, &bindings)
            .map_err(|e| internal(0, e))?;
        for operation in repaired.expanded.operations {
            let global = ids[operation.id];
            let mut instruction = operation.instruction;
            instruction.mode = ParseMode::Vcl;
            instruction.loop_id = 0;
            result.instructions[global] =
                program.lower(instruction, result.instructions[global].line)?;
        }
    }
    Ok(())
}
