//! Bounded bridge from allocated raw-linear frames to recovered raw motion.
//!
//! External operand bounds are prepared by the original's external-information step; it then
//! pulls raw operations upward and drags later instructions within their
//! user/output bounds. This adapter admits only ordinary VF/VI frames whose
//! initial physical replay has no stalls. It retains the original pair's old
//! read semantics while the pure reducer changes dispatch coordinates.

use super::*;

struct Adapter<'a> {
    program: &'a Source,
    region: &'a Region,
    graph: &'a flow::Graph,
    analysis: &'a flow::Analysis,
    physical: BTreeMap<usize, usize>,
    zero: BTreeSet<usize>,
    owners: BTreeMap<usize, usize>,
    reads: BTreeMap<usize, Vec<flow::Read>>,
    unobserved_flags: BTreeSet<usize>,
}

struct Tables<'a> {
    blocks: &'a [Block],
    rows: &'a [Option<Vec<[Option<usize>; 2]>>],
}

struct Prepared {
    ids: Vec<usize>,
    problem: SchedulingProblem,
    rows: Vec<[Option<usize>; 2]>,
    bounds: Vec<crate::scheduler::raw_cleanup::Bounds>,
}

impl<'a> Adapter<'a> {
    fn new(
        program: &'a Source,
        region: &'a Region,
        graph: &'a flow::Graph,
        analysis: &'a flow::Analysis,
        mappings: &BTreeMap<usize, String>,
    ) -> Self {
        let mut names = BTreeMap::new();
        let mut physical_ids = BTreeMap::new();
        let mut zero = BTreeSet::new();
        for (&logical, name) in mappings {
            let family = program.registers.named(logical).family;
            let next = names.len();
            let physical = *names
                .entry((family, name.to_ascii_lowercase()))
                .or_insert(next);
            physical_ids.insert(logical, physical);
            if family != Family::Hardware && super::physical(name, family) == Some(0) {
                zero.insert(logical);
            }
        }
        let mut owners = BTreeMap::new();
        let mut reads = BTreeMap::new();
        for (block_id, block) in region.blocks.iter().enumerate() {
            for &id in &block.operations {
                owners.insert(id, block_id);
                let owner = flow_owner(region, block, id);
                let mut actual = Vec::new();
                for access in &region.instructions[id].effects.reads {
                    actual.extend(
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
                reads.insert(id, actual);
            }
        }
        let unobserved_flags = owners
            .keys()
            .copied()
            .filter(|&id| Self::flags_unobserved(program, region, analysis, id))
            .collect();
        Self {
            program,
            region,
            graph,
            analysis,
            physical: physical_ids,
            zero,
            owners,
            reads,
            unobserved_flags,
        }
    }

    fn flags_unobserved(
        program: &Source,
        region: &Region,
        analysis: &flow::Analysis,
        id: usize,
    ) -> bool {
        let op = &region.instructions[id];
        if op.instruction.metadata.unit_mask != unit::FMAC {
            return false;
        }
        // Re-lowering can recreate implicit flag effects after pruning. Only
        // project them away when every affected status/MAC output is unused.
        // Explicit hardware operands and ACC/I/Q/P/etc. remain unsupported.
        if op.operands.iter().enumerate().any(|(index, register)| {
            register.is_some_and(|id| program.registers.named(id).family == Family::Hardware)
                && !op
                    .instruction
                    .metadata
                    .operands
                    .get(index)
                    .is_some_and(|spec| spec.implicit)
        }) {
            return false;
        }
        op.effects
            .reads
            .iter()
            .filter(|a| program.registers.named(a.register).family == Family::Hardware)
            .all(|a| program.registers.named(a.register).name == "STAT_ALU_S")
            && op
                .effects
                .writes
                .iter()
                .filter(|a| program.registers.named(a.register).family == Family::Hardware)
                .all(|write| {
                    matches!(
                        program.registers.named(write.register).name.as_str(),
                        "MAC" | "STAT_ALU"
                    ) && !analysis.reads.iter().any(|read| {
                        read.register == write.register
                            && write.mask & (1 << read.component) != 0
                            && read.definitions.contains(&Definition::Operation(id))
                    }) && !analysis.outputs.iter().any(|output| {
                        output.register == write.register
                            && write.mask & (1 << output.component) != 0
                            && output.definitions.contains(&Definition::Operation(id))
                    })
                })
    }

    fn eligible(&self, id: usize) -> bool {
        let op = &self.region.instructions[id];
        !op.instruction.metadata.is_memory()
            && op.instruction.modifiers == 0
            && (!op.effects.side_effect || op.instruction.metadata.is_branch())
            && op.effects.reads.iter().chain(&op.effects.writes).all(|a| {
                self.program.registers.named(a.register).family != Family::Hardware
                    || self.unobserved_flags.contains(&id)
            })
    }

    fn accesses(
        &self,
        id: usize,
        accesses: &[Access],
        writes: bool,
    ) -> Option<Vec<RegisterAccess>> {
        accesses
            .iter()
            .filter(|a| !writes || !self.zero.contains(&a.register))
            .filter(|a| {
                !self.unobserved_flags.contains(&id)
                    || self.program.registers.named(a.register).family != Family::Hardware
            })
            .map(|a| {
                Some(RegisterAccess {
                    register: *self.physical.get(&a.register)?,
                    components: a.mask,
                })
            })
            .collect()
    }

    fn latency(&self, read: &flow::Read, writer: usize) -> u16 {
        let setter = &self.region.instructions[writer];
        let latency = setter
            .effects
            .latencies
            .get(&read.register)
            .copied()
            .unwrap_or(1);
        let consumer = &self.region.instructions[read.operation];
        // Excludes loads and flag-test setters from the extra
        // conditional VI sampling bias. The mark belongs to the whole input,
        // so every possible operation setter must satisfy the predicate.
        let biased = consumer.instruction.metadata.is_branch()
            && self.program.registers.named(read.register).family == Family::Integer
            && read.definitions.iter().all(|definition| {
                let Definition::Operation(writer) = definition else {
                    return true;
                };
                let setter = &self.region.instructions[*writer];
                !setter.instruction.metadata.is_memory()
                    && !setter.effects.reads.iter().any(|access| {
                        matches!(
                            self.program.registers.named(access.register).name.as_str(),
                            "STAT_ALU"
                                | "STAT_DIV"
                                | "STAT_ALU_S"
                                | "STAT_DIV_S"
                                | "MAC"
                                | "CLIP"
                                | "CLIPA"
                        )
                    })
            });
        latency.saturating_add(u16::from(biased))
    }

    fn incoming(
        &self,
        writer: usize,
        block_id: usize,
        old_row: usize,
        latency: u16,
        tables: &Tables<'_>,
    ) -> u16 {
        let Some(&pred) = self.owners.get(&writer) else {
            return old_row as u16;
        };
        if pred >= block_id || !self.graph.blocks[pred].successors.contains(&block_id) {
            return old_row as u16;
        }
        let Some(rows) = tables.rows.get(pred).and_then(Option::as_ref) else {
            return old_row as u16;
        };
        let Some(row) = rows.iter().position(|pair| pair.contains(&Some(writer))) else {
            return old_row as u16;
        };
        let block = &tables.blocks[pred];
        let protected = matches!(block.end, End::Branch { .. })
            || block
                .selected_table
                .as_ref()
                .is_some_and(|table| table.mode == TableMode::Fixed)
            || block
                .operations
                .iter()
                .any(|&id| self.region.instructions[id].instruction.mode == ParseMode::Raw);
        // These are guaranteed issue pairs, not the original execution-time
        // getter's exact hardware clock. Final compression can remove empty
        // rows from a movable predecessor, but never its nonempty pairs.
        let elapsed = if protected {
            rows.len() - row
        } else {
            rows[row..]
                .iter()
                .filter(|pair| pair.iter().any(Option::is_some))
                .count()
        };
        latency.saturating_sub(elapsed.min(usize::from(u16::MAX)) as u16)
    }

    fn physical_write_latency(&self, id: usize, register: usize, components: u8) -> u16 {
        self.region.instructions[id]
            .effects
            .writes
            .iter()
            .filter(|access| {
                self.physical.get(&access.register) == Some(&register)
                    && access.mask & components != 0
            })
            .map(|access| {
                self.region.instructions[id]
                    .effects
                    .latencies
                    .get(&access.register)
                    .copied()
                    .unwrap_or(1)
            })
            .max()
            .unwrap_or(1)
    }

    fn prepare(
        &self,
        block_id: usize,
        rows: &[[Option<usize>; 2]],
        tables: &Tables<'_>,
    ) -> Option<Prepared> {
        if rows.len() >= 30000 {
            return None;
        }
        let ids: Vec<_> = rows.iter().flatten().flatten().copied().collect();
        if ids.is_empty() || ids.iter().any(|&id| !self.eligible(id)) {
            return None;
        }
        let local: BTreeMap<_, _> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        if local.len() != ids.len() {
            return None;
        }

        let (positions, mut operations) = self.build_operations(rows, &local)?;
        let mut edges = BTreeMap::<(usize, usize), u16>::new();
        self.add_read_edges(
            block_id,
            tables,
            &ids,
            &local,
            &positions,
            &mut operations,
            &mut edges,
        )?;
        self.add_overwrite_edges(&ids, &positions, &operations, &mut edges);
        let bounds = self.output_bounds(tables, &ids, &local);

        Some(Prepared {
            rows: rows
                .iter()
                .map(|pair| pair.map(|id| id.map(|id| local[&id])))
                .collect(),
            ids,
            problem: SchedulingProblem {
                operations,
                dependencies: edges
                    .into_iter()
                    .map(|((from, to), latency)| Dependency {
                        from,
                        to,
                        latency,
                        distance: 0,
                    })
                    .collect(),
                period: None,
            },
            bounds,
        })
    }

    /// The scheduling operation of every placed instruction, with the row
    /// each one sits in.
    fn build_operations(
        &self,
        rows: &[[Option<usize>; 2]],
        local: &BTreeMap<usize, usize>,
    ) -> Option<(BTreeMap<usize, usize>, Vec<Operation>)> {
        let mut positions = BTreeMap::new();
        let mut operations = Vec::new();

        for (row, pair) in rows.iter().enumerate() {
            for (pipe, &id) in pair.iter().enumerate() {
                let Some(id) = id else { continue };
                positions.insert(id, row);
                let located = &self.region.instructions[id];
                let metadata = located.instruction.generated_copy.map_or(
                    located.instruction.metadata,
                    |kind| {
                        &instruction::INSTRUCTIONS[usize::from(kind.resolved_descriptor(
                            if pipe == 0 {
                                schedule::Pipe::Upper
                            } else {
                                schedule::Pipe::Lower
                            },
                        ))]
                    },
                );
                let mut op = Operation::new(local[&id], 1 << pipe);
                op.unit_mask = metadata.unit_mask;
                op.throughput = metadata.throughput.max(1);
                op.reads = self.accesses(id, &located.effects.reads, false)?;
                op.writes = self.accesses(id, &located.effects.writes, true)?;
                op.is_branch = metadata.is_branch();
                operations.push(op);
            }
        }
        Some((positions, operations))
    }

    /// Add the dependency edges from each read's local writer, and raise the
    /// earliest row of operations whose writer is in a predecessor block.
    /// `None` when a read is outside the straight-line proof.
    #[allow(clippy::too_many_arguments)]
    fn add_read_edges(
        &self,
        block_id: usize,
        tables: &Tables<'_>,
        ids: &[usize],
        local: &BTreeMap<usize, usize>,
        positions: &BTreeMap<usize, usize>,
        operations: &mut [Operation],
        edges: &mut BTreeMap<(usize, usize), u16>,
    ) -> Option<()> {
        for &id in ids {
            let to = local[&id];
            let old_row = positions[&id];
            for read in self.reads.get(&id)? {
                if self.unobserved_flags.contains(&id)
                    && self.program.registers.named(read.register).family == Family::Hardware
                {
                    continue;
                }
                if self.graph.constants.contains(&read.register) {
                    continue;
                }
                let local_definitions: Vec<_> = read
                    .definitions
                    .iter()
                    .filter_map(|definition| match definition {
                        Definition::Operation(writer) if local.contains_key(writer) => {
                            Some(*writer)
                        }
                        _ => None,
                    })
                    .collect();
                // A local merge/backedge is outside this straight-line proof.
                if local_definitions.len() > 1
                    || (!local_definitions.is_empty() && read.definitions.len() > 1)
                    || read.definitions.is_empty()
                {
                    return None;
                }
                for definition in &read.definitions {
                    match definition {
                        Definition::Input => {}
                        Definition::Uninitialized => return None,
                        Definition::Operation(writer) => {
                            let latency = self.latency(read, *writer);
                            if let Some(&from) = local.get(writer) {
                                let writer_row = positions[writer];
                                if *writer == id || writer_row > old_row {
                                    return None;
                                }
                                // Both sides of a raw bundle read their old
                                // operands. Physical WAR below preserves that
                                // ordering when the two instructions separate.
                                if writer_row != old_row {
                                    edges
                                        .entry((from, to))
                                        .and_modify(|old| *old = (*old).max(latency))
                                        .or_insert(latency);
                                }
                            } else {
                                operations[to].earliest = operations[to].earliest.max(
                                    self.incoming(*writer, block_id, old_row, latency, tables),
                                );
                            }
                        }
                    }
                }
            }
        }
        Some(())
    }

    /// Add the edges that keep physical overwrite order between instructions.
    fn add_overwrite_edges(
        &self,
        ids: &[usize],
        positions: &BTreeMap<usize, usize>,
        operations: &[Operation],
        edges: &mut BTreeMap<(usize, usize), u16>,
    ) {
        // Preserve physical overwrite order, including allocator aliases that
        // have different logical register IDs. A paired read precedes either
        // paired write; in particular the lower read may constrain the upper
        // write with a reverse, zero-latency WAR edge.
        for (a, &a_id) in ids.iter().enumerate() {
            for (b, &b_id) in ids.iter().enumerate() {
                if a == b || positions[&a_id] > positions[&b_id] {
                    continue;
                }
                let overlap = |left: &[RegisterAccess], right: &[RegisterAccess]| {
                    left.iter().any(|a| {
                        right
                            .iter()
                            .any(|b| a.register == b.register && a.components & b.components != 0)
                    })
                };
                if overlap(&operations[a].reads, &operations[b].writes) {
                    edges.entry((a, b)).or_insert(0);
                }
                if positions[&a_id] == positions[&b_id] {
                    continue;
                }
                for write in &operations[a].writes {
                    for other in &operations[b].writes {
                        if write.register == other.register
                            && write.components & other.components != 0
                        {
                            let mask = write.components & other.components;
                            let latency = self
                                .physical_write_latency(a_id, write.register, mask)
                                .saturating_add(1)
                                .saturating_sub(self.physical_write_latency(
                                    b_id,
                                    write.register,
                                    mask,
                                ));
                            edges
                                .entry((a, b))
                                .and_modify(|old| *old = (*old).max(latency))
                                .or_insert(latency);
                        }
                    }
                }
            }
        }
    }

    /// Per-instruction tail: the longest wait of any live reader outside the
    /// block that is scheduled in its own block.
    fn output_bounds(
        &self,
        tables: &Tables<'_>,
        ids: &[usize],
        local: &BTreeMap<usize, usize>,
    ) -> Vec<crate::scheduler::raw_cleanup::Bounds> {
        let mut bounds = vec![
            crate::scheduler::raw_cleanup::Bounds {
                tail: 0,
                movable: true,
            };
            ids.len()
        ];

        for &writer in ids {
            for read in self.reads.values().flatten() {
                if local.contains_key(&read.operation)
                    || !read.definitions.contains(&Definition::Operation(writer))
                    || !self.analysis.live_operations.contains(&read.operation)
                {
                    continue;
                }
                let Some(&owner) = self.owners.get(&read.operation) else {
                    continue;
                };
                // Absent/hidden consumers and synthetic EXIT observations
                // impose no operation-user tail. For a scheduled operation,
                // zero from-start distance remains safe if later compression
                // or raw movement advances that consumer.
                if tables.rows[owner].as_ref().is_some_and(|rows| {
                    rows.iter().any(|pair| pair.contains(&Some(read.operation)))
                }) {
                    bounds[local[&writer]].tail = bounds[local[&writer]]
                        .tail
                        .max(self.latency(read, writer).saturating_sub(1));
                }
            }
        }
        bounds
    }
}

fn freeze(block: &mut Block) {
    if let Some(table) = &mut block.selected_table {
        table.mode = TableMode::Fixed;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reduce_tables(
    program: &Source,
    region: &Region,
    emission_blocks: &mut [Block],
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    schedules: &mut [Option<Vec<[Option<usize>; 2]>>],
    mappings: &BTreeMap<usize, String>,
    options: &Options,
) -> Result<(), CompileError> {
    if !emission_blocks.iter().any(|block| {
        block
            .selected_table
            .as_ref()
            .is_some_and(|table| table.mode == TableMode::RawLinear)
    }) {
        return Ok(());
    }
    let adapter = Adapter::new(program, region, graph, analysis, mappings);
    let mut replay_options = options.clone();
    replay_options.disable_size_reduction = true;
    for block_id in 0..emission_blocks.len() {
        if !emission_blocks[block_id]
            .selected_table
            .as_ref()
            .is_some_and(|table| table.mode == TableMode::RawLinear)
        {
            continue;
        }
        let Some(rows) = schedules[block_id].as_ref() else {
            freeze(&mut emission_blocks[block_id]);
            continue;
        };
        let initial = finish_rows(
            program,
            region,
            &emission_blocks[block_id],
            analysis,
            rows,
            mappings,
            &replay_options,
        );
        let stalled = initial.as_ref().map_or(true, |finished| {
            finished.replay.stalls.iter().any(|stall| {
                stall.latency != 0 || stall.throughput != 0 || stall.synchronization != 0
            })
        });
        if stalled {
            freeze(&mut emission_blocks[block_id]);
            continue;
        }
        let Some(prepared) = adapter.prepare(
            block_id,
            rows,
            &Tables {
                blocks: emission_blocks,
                rows: schedules,
            },
        ) else {
            freeze(&mut emission_blocks[block_id]);
            continue;
        };
        let Ok(candidate) = crate::scheduler::raw_cleanup::reduce(
            &prepared.problem,
            &prepared.rows,
            &prepared.bounds,
            0,
        ) else {
            freeze(&mut emission_blocks[block_id]);
            continue;
        };
        let candidate: Vec<_> = candidate
            .iter()
            .map(|pair| pair.map(|id| id.map(|id| prepared.ids[id])))
            .collect();
        if finish_rows(
            program,
            region,
            &emission_blocks[block_id],
            analysis,
            &candidate,
            mappings,
            &replay_options,
        )
        .is_err()
        {
            freeze(&mut emission_blocks[block_id]);
            continue;
        }
        schedules[block_id] = Some(candidate);
    }
    Ok(())
}
