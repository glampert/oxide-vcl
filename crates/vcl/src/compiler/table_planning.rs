//! Planning, running and raw-reducing the wrangled tables of a region.

use super::*;

/// Original linear optimization order and external timing for ordinary tables.
#[allow(clippy::too_many_arguments)]
pub(super) fn plan_wrangled_tables(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
    graph: &flow::Graph,
    emission_blocks: &[Block],
    hidden: &BTreeSet<usize>,
    timid: bool,
) -> BTreeMap<usize, WrangledTable> {
    use crate::scheduler::wrangle::{Direction as WDirection, Source as WSource};
    let n = emission_blocks.len();
    let mut live_of: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut placed_rows: Vec<Option<Vec<[Option<usize>; 2]>>> = vec![None; n];
    let mut tables: BTreeMap<usize, WrangledTable> = BTreeMap::new();
    let mut installed: BTreeMap<usize, WrangledTable> = BTreeMap::new();
    for (b, block) in emission_blocks.iter().enumerate() {
        if !analysis.reachable.contains(&b) || hidden.contains(&b) {
            continue;
        }
        let live: Vec<_> = block
            .operations
            .iter()
            .copied()
            .filter(|id| analysis.live_operations.contains(id))
            .collect();
        live_of[b] = live.clone();
        if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
            eprintln!(
                "plan block {b} live {live:?} selected {} modes {:?}",
                block.selected_table.is_some(),
                live.iter()
                    .map(|&id| (
                        region.instructions[id].instruction.mnemonic.clone(),
                        region.instructions[id].line,
                        region.instructions[id].instruction.mode
                    ))
                    .collect::<Vec<_>>()
            );
        }
        let raw = !live.is_empty()
            && live
                .iter()
                .all(|&id| region.instructions[id].instruction.mode == ParseMode::Raw);
        if let Some(fixed) = &block.selected_table {
            let rows: Vec<[Option<usize>; 2]> = fixed
                .rows
                .iter()
                .map(|pair| pair.map(|id| id.filter(|id| live.contains(id))))
                .collect();
            // An installed loop part keeps its expanded rows as its table.
            if fixed.installed
                && let Some(mut table) =
                    build_wrangle_table(program, region, analysis, b, &live, timid)
            {
                let local: BTreeMap<_, _> =
                    live.iter().enumerate().map(|(i, &id)| (id, i)).collect();
                table.rows = rows
                    .iter()
                    .map(|pair| pair.map(|id| id.map(|id| local[&id])))
                    .collect();
                table.end = table.rows.len();
                table.problem.horizon = table.problem.horizon.max(table.end as i32 + 1);
                // Local allocation of the installed rows.
                let placed = crate::scheduler::wrangle::Table::from_rows(
                    &table.problem,
                    &table.statics,
                    &table.rows,
                    0,
                    table.end as i32,
                );
                let vf = placed.allocate(1, &table.pools.vf_allowed, &table.pools.vf_through);
                let vi = placed.allocate(2, &table.pools.vi_allowed, &table.pools.vi_through);
                let allocation: Vec<_> = vf.into_iter().chain(vi).flatten().collect();
                for (register, physical) in allocation {
                    if let WrangleKey::Virtual(id) = table.keys[register] {
                        table.allocation.insert(id, physical);
                    }
                }
                installed.insert(b, table);
            }
            placed_rows[b] = Some(rows);
        } else if raw {
            let mut rows: Vec<[Option<usize>; 2]> = Vec::new();
            let mut line = None;
            let mut slot = 0;
            for &id in &live {
                let located = &region.instructions[id];
                if line != Some(located.line) {
                    rows.push([None, None]);
                    line = Some(located.line);
                    slot = 0;
                }
                if slot < 2
                    && (located.instruction.metadata.name != "nop"
                        || located.instruction.modifiers != 0)
                {
                    rows.last_mut().unwrap()[slot] = Some(id);
                }
                slot += 1;
            }
            placed_rows[b] = Some(rows);
        } else if let Some(table) = build_wrangle_table(program, region, analysis, b, &live, timid)
        {
            tables.insert(b, table);
        }
    }
    let context = TimingContext::new(program, region, analysis, graph, &live_of);
    let local_of: BTreeMap<usize, (usize, usize)> = live_of
        .iter()
        .enumerate()
        .flat_map(|(b, live)| live.iter().enumerate().map(move |(i, &id)| (id, (b, i))))
        .collect();
    let mut optimized = BTreeSet::new();
    let mut pending: Vec<usize> = tables.keys().copied().collect();
    while !pending.is_empty() {
        // Unscheduled or fixed instructions have no optimized image.
        let row_in = |rows: &[Vec<[Option<usize>; 2]>], id: usize| -> Option<(usize, usize)> {
            let &(b, _) = local_of.get(&id)?;
            let table = rows.get(b)?;
            table_positions(region, &live_of[b], table)
                .get(&id)
                .map(|&row| (row, table.len()))
        };
        let fixed: Vec<Vec<[Option<usize>; 2]>> = placed_rows
            .iter()
            .enumerate()
            .map(|(b, rows)| {
                if optimized.contains(&b) {
                    Vec::new()
                } else {
                    rows.clone().unwrap_or_default()
                }
            })
            .collect();
        let statics_of = |id: usize| -> Option<crate::scheduler::wrangle::Statics> {
            let &(b, i) = local_of.get(&id)?;
            tables
                .get(&b)
                .filter(|_| !optimized.contains(&b))
                .map(|t| t.statics[i])
        };
        let parts = |register: usize| {
            if program.registers.named(register).family == Family::Vector {
                4
            } else {
                1
            }
        };
        let mut best: Option<(usize, i32, i32)> = None;
        for &b in &pending {
            let table = &tables[&b];
            let max_height = table.statics.iter().map(|s| s.height).max().unwrap_or(0);
            let max_depth = table.statics.iter().map(|s| s.depth).max().unwrap_or(0);
            let (mut inputs, mut outputs) = (0, 0);
            for (index, &id) in table.live.iter().enumerate() {
                let st = table.statics[index];
                for (o, operand) in table.problem.instructions[index]
                    .operands
                    .iter()
                    .enumerate()
                {
                    let register = table.operand_registers[index][o];
                    let weight = if parts(register) == 1 { 4 } else { 1 };
                    for part in 0..4u8 {
                        if operand.mask & (1 << part) == 0 {
                            continue;
                        }
                        match operand.direction {
                            WDirection::Read => {
                                if operand.sources[usize::from(part)] != Some(WSource::External) {
                                    continue;
                                }
                                let Some(read) = context.reads_by.get(&(id, register, part)) else {
                                    continue;
                                };
                                for definition in &read.definitions {
                                    let (to_end, latency) = match definition {
                                        Definition::Operation(setter) => {
                                            if local_of.get(setter).is_some_and(|&(sb, _)| {
                                                sb == b || optimized.contains(&sb)
                                            }) {
                                                continue;
                                            }
                                            let to_end = if let Some(s) = statics_of(*setter) {
                                                s.height
                                            } else if let Some((row, len)) = row_in(&fixed, *setter)
                                            {
                                                (len - row - 1) as i32
                                            } else {
                                                0
                                            };
                                            (to_end, context.latency(*setter, register))
                                        }
                                        Definition::Input => (0, 1),
                                        Definition::Uninitialized => continue,
                                    };
                                    if 1 + st.depth + (max_height - st.height) + to_end
                                        < latency + context.offset(id, register)
                                    {
                                        inputs += weight;
                                    }
                                }
                            }
                            WDirection::Write => {
                                let latency = context.latency(id, register);
                                let mut candidates: Vec<i32> = Vec::new();
                                for &user in context
                                    .users
                                    .get(&(id, register, part))
                                    .into_iter()
                                    .flatten()
                                {
                                    if local_of
                                        .get(&user)
                                        .is_some_and(|&(ub, _)| ub == b || optimized.contains(&ub))
                                    {
                                        continue;
                                    }
                                    let from_start = if let Some(s) = statics_of(user) {
                                        s.depth
                                    } else if let Some((row, _)) = row_in(&fixed, user) {
                                        row as i32
                                    } else {
                                        0
                                    };
                                    candidates.push(from_start - context.offset(user, register));
                                }
                                if context.exit_users.contains(&(id, register, part)) {
                                    candidates.push(0);
                                }
                                for value in candidates {
                                    if 1 + st.height + (max_depth - st.depth) + value < latency {
                                        outputs += weight;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                eprintln!("select candidate {b} inputs {inputs} outputs {outputs}");
            }
            if best.is_none_or(|(_, bi, bo)| inputs < bi || (inputs == bi && bo < outputs)) {
                best = Some((b, inputs, outputs));
            }
        }
        let (b, _, _) = best.unwrap();
        pending.retain(|&p| p != b);
        let lengths: Vec<usize> = (0..n)
            .map(|block| {
                if let Some(rows) = &placed_rows[block] {
                    rows.len()
                } else if let Some(t) = tables.get(&block) {
                    t.lower_bound.max(0) as usize
                } else {
                    0
                }
            })
            .collect();
        let placed_positions: Vec<Option<(BTreeMap<usize, usize>, usize)>> = placed_rows
            .iter()
            .enumerate()
            .map(|(block, rows)| {
                rows.as_ref()
                    .map(|rows| (table_positions(region, &live_of[block], rows), rows.len()))
            })
            .collect();
        let placed = |id: usize| -> Option<(usize, usize)> {
            let &(block, _) = local_of.get(&id)?;
            let (positions, length) = placed_positions[block].as_ref()?;
            positions.get(&id).map(|&row| (row, *length))
        };
        let mut table = tables.remove(&b).unwrap();
        context.apply(&mut table, b, &lengths, &placed);
        if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
            eprintln!(
                "apply block {b} minimum {:?} end {:?}",
                table
                    .problem
                    .instructions
                    .iter()
                    .map(|i| i.minimum_position)
                    .collect::<Vec<_>>(),
                table
                    .problem
                    .instructions
                    .iter()
                    .map(|i| i.end_distance)
                    .collect::<Vec<_>>()
            );
            eprintln!(
                "statics block {b} {:?}",
                table
                    .statics
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (
                        i,
                        region.instructions[table.live[i]]
                            .instruction
                            .mnemonic
                            .clone(),
                        s.height,
                        s.depth
                    ))
                    .collect::<Vec<_>>()
            );
        }
        if run_wrangle(&mut table, b).is_none() {
            continue;
        }
        placed_rows[b] = Some(
            table
                .rows
                .iter()
                .map(|row| row.map(|entry| entry.map(|i| table.live[i])))
                .collect(),
        );
        optimized.insert(b);
        tables.insert(b, table);
    }
    tables.retain(|_, table| table.end != 0 || table.live.is_empty());
    tables.extend(installed);
    tables
}

/// Run the linear optimizer strategies on a prepared table.
pub(super) fn run_wrangle(table: &mut WrangledTable, block_id: usize) -> Option<()> {
    crate::scheduler::wrangle::set_trace_block(block_id);
    let outcome = crate::scheduler::wrangle::optimize(
        &table.problem,
        table.lower_bound,
        Some(&table.pools),
        &mut |_| true,
    )?;
    if std::env::var_os("VCL_WRANGLE_TRACE").is_some() {
        eprintln!(
            "wrangle block {block_id}: {:?} lb {} rows {} trace {:?}",
            outcome.strategy,
            table.lower_bound,
            outcome.rows.len(),
            outcome.trace
        );
    }
    table.end = outcome.rows.len();
    table.rows = outcome.rows;
    table.allocation.clear();
    for &(register, physical) in &outcome.allocation {
        if let WrangleKey::Virtual(id) = table.keys[register] {
            table.allocation.insert(id, physical);
        }
    }
    Some(())
}

/// Cross-stream latency stalls recorded by the dependency emulation
///: a reader issues before an external setter's result.
pub(super) fn table_stalls(
    region: &Region,
    timing: &TimingContext,
    lengths: &[usize],
    placed: &dyn Fn(usize) -> Option<(usize, usize)>,
    block: usize,
    analysis: &flow::Analysis,
) -> bool {
    for read in &analysis.reads {
        for definition in &read.definitions {
            let Definition::Operation(setter) = definition else {
                continue;
            };
            let (Some(&reader_block), Some(&setter_block)) =
                (timing.owner.get(&read.operation), timing.owner.get(setter))
            else {
                continue;
            };
            if reader_block == setter_block || (reader_block != block && setter_block != block) {
                continue;
            }
            let (Some((user_row, _)), Some((setter_row, setter_len))) =
                (placed(read.operation), placed(*setter))
            else {
                continue;
            };
            let Some(distance) = timing.distance(lengths, setter_block, reader_block) else {
                continue;
            };
            let available = (setter_len - setter_row) as i32 + distance + user_row as i32;
            let needed = timing.latency(*setter, read.register)
                + timing.offset(read.operation, read.register);
            let _ = region;
            if available < needed {
                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                    eprintln!(
                        "stall block {block}: setter {setter} ({setter_block} row {setter_row}/{setter_len}) reader {} ({reader_block} row {user_row}) distance {distance} needed {needed}",
                        read.operation
                    );
                }
                return true;
            }
        }
    }
    false
}

/// Post-allocation raw reduction of one table
/// built by the linear optimizer, on physical registers.
/// Final layout of an optimized table: physical rows with their skip bytes
/// and synchronization caps.
#[derive(Clone, Debug)]
pub(super) struct CompressionPlan {
    pub(super) rows: Vec<[Option<usize>; 2]>,
    /// Rows before the exit padding: the table end later tables' external
    /// timing measures to (tidy pads after every table is reduced).
    pub(super) end: usize,
    pub(super) sync: Vec<Option<Synchronization>>,
    pub(super) skipped: Vec<bool>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn raw_reduce_wrangled(
    program: &Source,
    region: &Region,
    table: &WrangledTable,
    rows: &[[Option<usize>; 2]],
    mappings: &BTreeMap<usize, String>,
    padding: usize,
    end: &End,
    raw: bool,
    reduce: bool,
) -> Option<CompressionPlan> {
    use crate::scheduler::wrangle;
    let mut keys: BTreeMap<WrangleKey, usize> = BTreeMap::new();
    let mut registers = Vec::new();
    let mut remap = Vec::new();
    for (index, key) in table.keys.iter().enumerate() {
        let physical = match key {
            WrangleKey::Virtual(id) => {
                let name = mappings.get(id)?.to_ascii_lowercase();
                let family = program.registers.named(*id).family;
                match physical(&name, family) {
                    Some(0) => None,
                    Some(n) => Some(WrangleKey::Physical(u8::from(family == Family::Vector), n)),
                    None => Some(key.clone()),
                }
            }
            other => Some(other.clone()),
        };
        remap.push(physical.map(|key| {
            *keys.entry(key).or_insert_with(|| {
                registers.push(table.problem.registers[index].clone());
                registers.len() - 1
            })
        }));
    }
    let mut problem = table.problem.clone();
    problem.registers = registers;
    for insn in &mut problem.instructions {
        insn.operands.retain(|o| remap[o.register].is_some());
        // The first output names a table register too; VF00 maps to none.
        if let Some(first) = &mut insn.first_output {
            first.register = first.register.and_then(|r| remap[r]);
        }
        for operand in &mut insn.operands {
            operand.register = remap[operand.register].unwrap();
        }
    }
    let local: BTreeMap<_, _> = table
        .live
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();
    let mut local_rows = Vec::new();
    for row in rows {
        let mut mapped = [None, None];
        for (slot, entry) in row.iter().enumerate() {
            if let Some(id) = entry {
                mapped[slot] = Some(*local.get(id)?);
            }
        }
        local_rows.push(mapped);
    }
    if local_rows.iter().flatten().flatten().count() != table.live.len() {
        return None;
    }
    // Placement specializes a dual-issue move into its upper or lower form
    // (`uvmove`/`lvmove`); the rebuilt table keeps that pipe.
    for row in &local_rows {
        for (slot, entry) in row.iter().enumerate() {
            if let Some(index) = *entry
                && problem.instructions[index].pipes == 3
            {
                problem.instructions[index].pipes = 1 << slot;
            }
        }
    }
    let statics = vec![wrangle::Statics::default(); problem.instructions.len()];
    problem.horizon = problem.horizon.max(local_rows.len() as i32 + 1);
    if std::env::var_os("VCL_WRANGLE_PROBLEM").is_some() {
        eprintln!("raw reduce rows {local_rows:?}");
        for (index, insn) in problem.instructions.iter().enumerate() {
            eprintln!(
                "  raw insn {index} {} min {} end {} protected {} {} escapes {:?}",
                region.instructions[table.live[index]].instruction.mnemonic,
                insn.minimum_position,
                insn.end_distance,
                insn.protected_read,
                insn.protected_write,
                insn.operands.iter().map(|o| o.escapes).collect::<Vec<_>>()
            );
        }
    }
    // Scratch destinations chosen for flag caps of VF00 producers.
    let mut scratch: BTreeMap<usize, usize> = BTreeMap::new();
    for (index, &id) in table.live.iter().enumerate() {
        let Some(first) = problem.instructions[index].first_output else {
            continue;
        };
        if !first.vector || first.register.is_some() {
            continue;
        }
        let Some(register) = region.instructions[id].operands.first().copied().flatten() else {
            continue;
        };
        let Some(n) = mappings
            .get(&register)
            .and_then(|name| physical(&name.to_ascii_lowercase(), Family::Vector))
            .filter(|&n| n != 0)
        else {
            continue;
        };
        let at = *keys.entry(WrangleKey::Physical(1, n)).or_insert_with(|| {
            problem.registers.push(wrangle::Register {
                parts: 4,
                pool: 1,
                physical: Some(n),
                ..wrangle::Register::default()
            });
            problem.registers.len() - 1
        });
        scratch.insert(index, at);
    }
    // Tidy may need up to two E-bit rows once reduction has moved the tail.
    let horizon = local_rows.len() + padding.max(2) + 1;
    problem.horizon = problem.horizon.max(horizon as i32);
    let mut reduced =
        wrangle::Table::from_rows(&problem, &statics, &local_rows, 0, local_rows.len() as i32);
    if raw || reduce {
        if !reduced.raw_reduce(true, true) {
            return None;
        }
        if std::env::var_os("VCL_WRANGLE_PROBLEM").is_some() {
            eprintln!("  after raw {} {:?}", reduced.start(), reduced.window());
        }
    }
    if reduce {
        reduced.prepare_size_reduction();
        if std::env::var_os("VCL_WRANGLE_PROBLEM").is_some() {
            eprintln!("  after sred {:?}", reduced.window());
        }
    }
    // Tidy runs after reduction, so the E-bit padding follows
    // the reduced tail rather than the optimizer's table.
    let padding =
        if (raw || reduce) && matches!(end, End::Exit { macro_text: None } | End::Continue) {
            let lower: Vec<_> = reduced
                .window()
                .iter()
                .map(|pair| {
                    pair[1].map(|index| {
                        let metadata = region.instructions[table.live[index]].instruction.metadata;
                        crate::analysis::control::LowerInstruction {
                            descriptor_index: metadata.index,
                            memory: metadata.is_memory(),
                            branch: metadata.is_branch(),
                        }
                    })
                })
                .collect();
            crate::analysis::control::required_exit_padding(&lower)
        } else {
            padding
        };
    let before_padding = reduced.window().len();
    reduced.pad(padding);
    let padded = reduced.window().len() - before_padding;
    if reduce {
        reduced.insert_caps(&|index| scratch.get(&index).copied());
        let ordinary = matches!(end, End::Fallthrough | End::Branch { .. });
        reduced.compress(matches!(end, End::Fallthrough), ordinary);
    }
    let compressed = reduced.compressed();
    let rows: Vec<[Option<usize>; 2]> = compressed
        .rows
        .iter()
        .map(|row| row.map(|entry| entry.map(|i| table.live[i])))
        .collect();
    let sync = compressed
        .caps
        .iter()
        .map(|cap| {
            let cap = (*cap)?;
            let producer = table.live[cap.producer];
            let writes = &region.instructions[producer].effects.writes;
            let find = |name: &str| {
                writes.iter().find(|w| {
                    let named = program.registers.named(w.register);
                    named.family == Family::Hardware && named.name.eq_ignore_ascii_case(name)
                })
            };
            Some(match cap.kind {
                wrangle::CapKind::WaitQ => Synchronization::Q {
                    register: find("Q")?.register,
                },
                wrangle::CapKind::WaitP => Synchronization::P {
                    register: find("P")?.register,
                },
                wrangle::CapKind::Abs => {
                    let vf = writes.iter().find(|w| {
                        program.registers.named(w.register).family == Family::Vector
                            && mappings.get(&w.register).is_some_and(|m| m != "VF00")
                    })?;
                    Synchronization::Vector {
                        register: vf.register,
                        components: cap.mask,
                        producer,
                    }
                }
            })
        })
        .collect();
    Some(CompressionPlan {
        end: rows.len().saturating_sub(padded),
        rows,
        sync,
        skipped: compressed.skipped,
    })
}

/// ACC/I forced links: a writer of ACC or I
/// must follow earlier setters of its users' other inputs, and ACC chain
/// starts inherit their members' earlier forced predecessors.
pub(super) fn link_accumulators(problem: &mut crate::scheduler::wrangle::Problem) {
    use crate::scheduler::wrangle::{Direction as WDirection, Source as WSource};
    let n = problem.instructions.len();
    let is_acc_or_i = |problem: &crate::scheduler::wrangle::Problem, r: usize| {
        matches!(problem.registers[r].global_id, Some(0x30 | 0x31))
    };
    let is_acc = |problem: &crate::scheduler::wrangle::Problem, r: usize| {
        problem.registers[r].global_id == Some(0x30)
    };
    for x in 0..n {
        let writes: Vec<(usize, usize, u8)> = problem.instructions[x]
            .operands
            .iter()
            .enumerate()
            .filter(|(_, o)| o.direction == WDirection::Write && is_acc_or_i(problem, o.register))
            .map(|(i, o)| (i, o.register, o.mask))
            .collect();
        for (operand, _, mask) in writes {
            for user in x + 1..n {
                let reads_x = problem.instructions[user].operands.iter().any(|o| {
                    o.direction == WDirection::Read
                        && (0..4).any(|c| {
                            mask & (1 << c) != 0
                                && o.sources[c]
                                    == Some(WSource::Local {
                                        instruction: x,
                                        operand,
                                    })
                        })
                });
                if !reads_x {
                    continue;
                }
                let setters: Vec<usize> = problem.instructions[user]
                    .operands
                    .iter()
                    .filter(|o| o.direction == WDirection::Read)
                    .flat_map(|o| {
                        (0..4).filter_map(move |c| match o.sources[c] {
                            Some(WSource::Local { instruction, .. }) if o.mask & (1 << c) != 0 => {
                                Some(instruction)
                            }
                            _ => None,
                        })
                    })
                    .collect();
                // Links only the writer's predecessor list;
                // the original keeps entries unique.
                for setter in setters {
                    if setter != x && setter < x {
                        problem.instructions[x].acc_special = true;
                        if !problem.instructions[x]
                            .forced_predecessors
                            .contains(&setter)
                        {
                            problem.instructions[x].forced_predecessors.push(setter);
                        }
                    }
                }
            }
        }
    }
    let mut starts: [Option<usize>; 4] = [None; 4];
    for index in 0..n {
        let written: u8 = problem.instructions[index]
            .operands
            .iter()
            .filter(|o| o.direction == WDirection::Write && is_acc(problem, o.register))
            .fold(0, |m, o| m | o.mask);
        let read: u8 = problem.instructions[index]
            .operands
            .iter()
            .filter(|o| o.direction == WDirection::Read && is_acc(problem, o.register))
            .fold(0, |m, o| m | o.mask);
        for (part, start) in starts.iter_mut().enumerate() {
            if written & (1 << part) != 0 && read & (1 << part) == 0 {
                *start = Some(index);
            }
        }
        for (part, start) in starts.iter().enumerate() {
            if read & (1 << part) == 0 {
                continue;
            }
            let Some(start) = *start else { continue };
            let predecessors = problem.instructions[index].forced_predecessors.clone();
            for p in predecessors {
                if p < start {
                    problem.instructions[start].acc_special = true;
                    if !problem.instructions[start].forced_predecessors.contains(&p) {
                        problem.instructions[start].forced_predecessors.push(p);
                    }
                }
            }
        }
    }
}
