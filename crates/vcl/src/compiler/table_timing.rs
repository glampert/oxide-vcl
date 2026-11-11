//! Replay timing of wrangled tables and the positions their rows take.

use super::*;

/// Cross-stream timing facts shared by the optimization order
/// and the external-information pass.
pub(super) struct TimingContext<'a> {
    pub(super) program: &'a Source,
    pub(super) region: &'a Region,
    pub(super) successors: Vec<Vec<usize>>,
    /// Successors for a path passing through a stream: `--cont` halts the
    /// VU, so a path continues past one only where it starts.
    pub(super) through: Vec<Vec<usize>>,
    pub(super) reads_by: BTreeMap<(usize, usize, u8), &'a flow::Read>,
    pub(super) users: BTreeMap<(usize, usize, u8), Vec<usize>>,
    pub(super) exit_users: BTreeSet<(usize, usize, u8)>,
    pub(super) owner: BTreeMap<usize, usize>,
    pub(super) hazard_reads: &'a BTreeSet<(usize, usize, u8)>,
    pub(super) hazard_writes: &'a BTreeSet<(usize, usize, u8)>,
    /// Flow block of each operation; a merged table spans several.
    pub(super) flow_block: BTreeMap<usize, usize>,
}

impl<'a> TimingContext<'a> {
    pub(super) fn new(
        program: &'a Source,
        region: &'a Region,
        analysis: &'a flow::Analysis,
        graph: &flow::Graph,
        live_of: &[Vec<usize>],
    ) -> Self {
        let mut reads_by = BTreeMap::new();
        let mut users: BTreeMap<_, Vec<usize>> = BTreeMap::new();
        for read in &analysis.reads {
            reads_by.insert((read.operation, read.register, read.component), read);
            for definition in &read.definitions {
                if let Definition::Operation(writer) = definition {
                    users
                        .entry((*writer, read.register, read.component))
                        .or_default()
                        .push(read.operation);
                }
            }
        }
        // A fixed table's lower half reads with its upper partner.
        for block in region.blocks.iter().filter(|b| b.selected_table.is_some()) {
            for &id in &block.operations {
                let owner = flow_owner(region, block, id);
                if owner == id {
                    continue;
                }
                for access in &region.instructions[id].effects.reads {
                    for read in analysis.reads.iter().filter(|r| {
                        r.operation == owner
                            && r.register == access.register
                            && access.mask & (1 << r.component) != 0
                    }) {
                        reads_by.insert((id, read.register, read.component), read);
                    }
                }
            }
        }
        let mut exit_users = BTreeSet::new();
        for output in &analysis.outputs {
            for definition in &output.definitions {
                if let Definition::Operation(writer) = definition {
                    exit_users.insert((*writer, output.register, output.component));
                }
            }
        }
        let mut owner = BTreeMap::new();
        for (block, live) in live_of.iter().enumerate() {
            for &id in live {
                owner.insert(id, block);
            }
        }
        Self {
            program,
            region,
            successors: graph.blocks.iter().map(|b| b.successors.clone()).collect(),
            through: graph
                .blocks
                .iter()
                .enumerate()
                .map(|(id, b)| {
                    if matches!(region.blocks[id].end, End::Continue) {
                        Vec::new()
                    } else {
                        b.successors.clone()
                    }
                })
                .collect(),
            reads_by,
            users,
            exit_users,
            owner,
            hazard_reads: {
                if std::env::var_os("VCL_HAZARD").is_some() {
                    for (kind, set) in [
                        ("read", &analysis.hazard_reads),
                        ("write", &analysis.hazard_writes),
                    ] {
                        for &(block, register, part) in set {
                            crate::trace!(
                                "hazard {kind} block {block} {:?} {} part {part}",
                                region.blocks[block].labels,
                                program.registers.named(register).name
                            );
                        }
                    }
                }
                &analysis.hazard_reads
            },
            hazard_writes: &analysis.hazard_writes,
            flow_block: graph
                .blocks
                .iter()
                .enumerate()
                .flat_map(|(block, b)| b.operations.iter().map(move |&op| (op, block)))
                .collect(),
        }
    }

    /// Rebuilt dependencies after an affine consumer crossed
    /// its induction update: the consumer's base read now sees what the
    /// update read (moved above it) or the update itself (moved below it).
    pub(super) fn retarget(&mut self, reads: &'a [flow::Read]) {
        for read in reads {
            let key = (read.operation, read.register, read.component);
            if let Some(old) = self.reads_by.insert(key, read) {
                for definition in &old.definitions {
                    if let Definition::Operation(writer) = definition
                        && let Some(users) =
                            self.users
                                .get_mut(&(*writer, read.register, read.component))
                    {
                        users.retain(|&user| user != read.operation);
                    }
                }
            }
            for definition in &read.definitions {
                if let Definition::Operation(writer) = definition {
                    self.users
                        .entry((*writer, read.register, read.component))
                        .or_default()
                        .push(read.operation);
                }
            }
        }
    }

    pub(super) fn latency(&self, writer: usize, register: usize) -> i32 {
        i32::from(
            self.region.instructions[writer]
                .effects
                .latencies
                .get(&register)
                .copied()
                .unwrap_or(1),
        )
    }

    /// Early-sampled conditional branch source.
    pub(super) fn offset(&self, reader: usize, register: usize) -> i32 {
        let metadata = self.region.instructions[reader].instruction.metadata;
        if !metadata.is_conditional_branch()
            || self.program.registers.named(register).family != Family::Integer
        {
            return 0;
        }
        let Some(read) = self.reads_by.get(&(reader, register, 0)) else {
            return 0;
        };
        for definition in &read.definitions {
            if *definition == Definition::Input {
                return 1;
            }
            let Definition::Operation(writer) = definition else {
                continue;
            };
            let setter = &self.region.instructions[*writer];
            let first_operand = writes_first_operand(setter, register);
            let flagged = setter.effects.reads.iter().any(|r| {
                let named = self.program.registers.named(r.register);
                named.family == Family::Hardware
                    && hardware_register(&named.name).is_some_and(|h| (0x36..=0x3b).contains(&h.0))
            });
            if (!setter.instruction.metadata.is_memory() || !first_operand) && !flagged {
                return 1;
            }
        }
        0
    }

    /// Intermediate stream lengths, excluding both endpoints.
    pub(super) fn distance(&self, lengths: &[usize], from: usize, to: usize) -> Option<i32> {
        let through = |a: usize, b: usize| {
            crate::scheduler::boundary::minimum_path_length(&self.through, lengths, a, b)
        };
        // The first step may leave a starting `--cont` stream.
        let path = |a: usize, b: usize, start: bool| {
            if start && self.through[a].len() != self.successors[a].len() {
                self.successors[a]
                    .iter()
                    .filter_map(|&next| {
                        if next == b {
                            Some(0)
                        } else {
                            through(next, b).map(|d| d + lengths[next])
                        }
                    })
                    .min()
            } else {
                through(a, b)
            }
        };
        let value = if from == to {
            self.successors[from]
                .iter()
                .filter_map(|&next| {
                    if next == from {
                        Some(0)
                    } else {
                        path(next, from, false).map(|d| d + lengths[next])
                    }
                })
                .min()?
        } else {
            path(from, to, true)?
        };
        Some(value as i32)
    }

    /// External min positions and end distances.
    /// `placed(id)` gives `(row, table length)` for an instruction in an
    /// installed (raw) table. A same-stream setter of an unplaced table uses
    /// its static height as time to end.
    pub(super) fn apply(
        &self,
        table: &mut WrangledTable,
        block: usize,
        lengths: &[usize],
        placed: &dyn Fn(usize) -> Option<(usize, usize)>,
    ) {
        use crate::scheduler::wrangle::{Direction as WDirection, Source as WSource};
        let local: BTreeMap<_, _> = table
            .live
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();
        for index in 0..table.live.len() {
            let id = table.live[index];
            let mut minimum = 0;
            let mut end = 0;
            for (o, operand) in table.problem.instructions[index]
                .operands
                .iter()
                .enumerate()
            {
                let register = table.operand_registers[index][o];
                for part in 0..4u8 {
                    if operand.mask & (1 << part) == 0 {
                        continue;
                    }
                    match operand.direction {
                        WDirection::Read => {
                            if operand.sources[usize::from(part)] != Some(WSource::External) {
                                continue;
                            }
                            let Some(read) = self.reads_by.get(&(id, register, part)) else {
                                continue;
                            };
                            for definition in &read.definitions {
                                let Definition::Operation(setter) = definition else {
                                    continue;
                                };
                                let Some(&setter_block) = self.owner.get(setter) else {
                                    continue;
                                };
                                let to_end = if let Some((row, length)) = placed(*setter) {
                                    (length - row - 1) as i32
                                } else if setter_block == block {
                                    local.get(setter).map_or(0, |&s| table.statics[s].height)
                                } else {
                                    continue;
                                };
                                let Some(distance) = self.distance(lengths, setter_block, block)
                                else {
                                    continue;
                                };
                                let value = self.latency(*setter, register)
                                    + self.offset(id, register)
                                    - (distance + to_end)
                                    - 1;
                                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                                    crate::trace!(
                                        "  read {id} {} in {block} setter {setter} {} block {setter_block} to_end {to_end} distance {distance} value {value}",
                                        self.region.instructions[id].instruction.mnemonic,
                                        self.region.instructions[*setter].instruction.mnemonic
                                    );
                                }
                                minimum = minimum.max(value);
                            }
                        }
                        WDirection::Write => {
                            if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                                crate::trace!(
                                    "timing write {id} reg {register} part {part} users {:?}",
                                    self.users.get(&(id, register, part))
                                );
                            }
                            let Some(users) = self.users.get(&(id, register, part)) else {
                                continue;
                            };
                            for &user in users {
                                let Some(&user_block) = self.owner.get(&user) else {
                                    continue;
                                };
                                if user_block == block
                                    && local.get(&user).is_some_and(|&u| u > index)
                                {
                                    continue;
                                }
                                let Some((row, _)) = placed(user) else {
                                    if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                                        crate::trace!(
                                            "  user {user} block {user_block} not placed"
                                        );
                                    }
                                    continue;
                                };
                                let Some(distance) = self.distance(lengths, block, user_block)
                                else {
                                    continue;
                                };
                                let value = self.latency(id, register)
                                    + self.offset(user, register)
                                    - (distance + row as i32)
                                    - 1;
                                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                                    crate::trace!(
                                        "  user {user} block {user_block} row {row} distance {distance} value {value}"
                                    );
                                }
                                end = end.max(value);
                            }
                        }
                    }
                }
            }
            // Throughput-tracked registers (Q, P): a writer in another table
            // keeps its unit busy.
            for (o, operand) in table.problem.instructions[index]
                .operands
                .iter()
                .enumerate()
            {
                if operand.direction != WDirection::Write
                    || !table.problem.registers[operand.register].throughput_tracked
                {
                    continue;
                }
                let register = table.operand_registers[index][o];
                let throughput = |id: usize| {
                    i32::from(
                        self.region.instructions[id]
                            .instruction
                            .metadata
                            .throughput
                            .max(1),
                    )
                };
                for (&other, &other_block) in &self.owner {
                    if other_block == block
                        || !self.region.instructions[other]
                            .effects
                            .writes
                            .iter()
                            .any(|w| w.register == register)
                    {
                        continue;
                    }
                    let Some((row, length)) = placed(other) else {
                        continue;
                    };
                    if let Some(distance) = self.distance(lengths, other_block, block) {
                        let to_end = (length - row - 1) as i32;
                        minimum = minimum.max(throughput(other) - (distance + to_end) - 1);
                    }
                    if let Some(distance) = self.distance(lengths, block, other_block) {
                        end = end.max(throughput(id) - (distance + row as i32 + 1));
                    }
                }
            }
            // A `--cont` stream (end mode 2; 3 is the synthetic exit
            // itself) keeps XGKICK out of its last two rows already while
            // scheduling, so the
            // strategies compare that length. Code running into `--exit` is
            // still an ordinary stream then (Tidy folds it into E later).
            if matches!(self.region.blocks[block].end, End::Continue)
                && self.region.instructions[id].instruction.metadata.index == id::XGKICK
            {
                end = end.max(2);
            }
            table.problem.instructions[index].minimum_position = minimum;
            table.problem.instructions[index].end_distance = end;
            // Protected operands: a hazard
            // read stays put, as do the writes of the value it misses.
            let home = self.flow_block.get(&id).copied().unwrap_or(block);
            let (mut read, mut write) = (false, false);
            for (o, operand) in table.problem.instructions[index]
                .operands
                .iter()
                .enumerate()
            {
                let register = table.operand_registers[index][o];
                for part in (0..4u8).filter(|part| operand.mask & (1 << part) != 0) {
                    // Operand mask AND regref bits: only a value
                    // that crosses the table boundary is protected (a read
                    // of an outside producer, a write read outside).
                    match operand.direction {
                        WDirection::Read => {
                            read |= operand.sources[usize::from(part)] == Some(WSource::External)
                                && self.hazard_reads.contains(&(home, register, part));
                        }
                        WDirection::Write => {
                            // Readers by timed flow: a non-interlocked value
                            // overwritten later in the table can still be the
                            // one another table reads.
                            let outside =
                                self.users.get(&(id, register, part)).is_some_and(|users| {
                                    users.iter().any(|user| {
                                        self.owner.get(user).is_some_and(|&b| b != block)
                                    })
                                });
                            write |=
                                outside && self.hazard_writes.contains(&(home, register, part));
                        }
                    }
                }
            }
            table.problem.instructions[index].protected_read = read;
            table.problem.instructions[index].protected_write = write;
        }
    }
}

/// Row of every live instruction in an installed table. A raw pair owner
/// that emits as an empty NOP shares the row of its source line.
pub(super) fn table_positions(
    region: &Region,
    live: &[usize],
    rows: &[[Option<usize>; 2]],
) -> BTreeMap<usize, usize> {
    let mut positions = BTreeMap::new();
    let mut by_line = BTreeMap::new();
    for (row, pair) in rows.iter().enumerate() {
        for &id in pair.iter().flatten() {
            positions.insert(id, row);
            by_line.entry(region.instructions[id].line).or_insert(row);
        }
    }
    for &id in live {
        if let Some(&row) = by_line.get(&region.instructions[id].line) {
            positions.entry(id).or_insert(row);
        }
    }
    positions
}
