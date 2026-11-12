//! Finishing scheduled tables into emitted rows: synchronization caps, row
//! timing and the generated-row bookkeeping each table carries.

use super::*;

pub(super) struct FinishedRows {
    pub(super) rows: Vec<TimedRow>,
    pub(super) origins: Vec<usize>,
    pub(super) replay: timing::Replay,
    pub(super) physical_names: BTreeMap<usize, String>,
}

impl FinishedRows {
    /// Adds an empty row (NOP, NOP) after the last one.
    pub(super) fn push_empty_row(&mut self) {
        let origin = self.origins.last().copied().unwrap_or(0);
        let issue = self.replay.issue_times.last().map_or(0, |time| time + 1);
        self.rows.push(TimedRow::new([None, None]));
        self.origins.push(origin);
        self.replay.issue_times.push(issue);
        self.replay.stalls.push(Default::default());
    }
}

/// Validate physical producer identities before reducing a straight-line block.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_rows(
    program: &Source,
    region: &Region,
    block: &Block,
    analysis: &flow::Analysis,
    rows: &[[Option<usize>; 2]],
    mappings: &BTreeMap<usize, String>,
    options: &Options,
) -> Result<FinishedRows, CompileError> {
    finish_rows_planned(
        program, region, block, analysis, rows, mappings, options, None,
    )
}

/// As `finish_rows`, optionally using an original-order cap/compression
/// plan for an optimized table instead of the replay-based reducer.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_rows_planned(
    program: &Source,
    region: &Region,
    block: &Block,
    analysis: &flow::Analysis,
    rows: &[[Option<usize>; 2]],
    mappings: &BTreeMap<usize, String>,
    options: &Options,
    plan: Option<&CompressionPlan>,
) -> Result<FinishedRows, CompileError> {
    let (ids, physical_names) = physical_identities(program, mappings);
    let mut positions = BTreeMap::new();
    for (row, pair) in rows.iter().enumerate() {
        for &operation in pair.iter().flatten() {
            positions.insert(operation, row);
        }
    }
    let context = RowContext {
        program,
        region,
        block,
        analysis,
        rows,
        mappings,
        ids,
        positions,
    };

    let operations = context.timing_operations();
    let raw = context.is_raw_table();
    let branch = matches!(block.end, End::Branch { .. });
    let timed = context.protected_rows(raw, branch);

    if let Some(plan) = plan.filter(|plan| plan.rows.len() == rows.len()) {
        return Ok(context.planned_rows(plan, &operations, physical_names));
    }
    if options.disable_size_reduction || raw || branch {
        return context.fixed_rows(timed, &operations, physical_names);
    }

    let compressed =
        timing::compress(&timed, &operations, &[], true).map_err(|e| internal(0, e))?;
    let mut origins = vec![0; compressed.rows.len()];
    for (old, new) in compressed.row_map.iter().enumerate() {
        if let Some(new) = new {
            origins[*new] = old;
        }
    }
    Ok(FinishedRows {
        rows: compressed.rows,
        origins,
        replay: compressed.replay,
        physical_names,
    })
}

/// Maps each logical register to a physical identity (by family and name),
/// and each identity to the name it prints as.
fn physical_identities(
    program: &Source,
    mappings: &BTreeMap<usize, String>,
) -> (BTreeMap<usize, usize>, BTreeMap<usize, String>) {
    let mut physical_ids = BTreeMap::new();
    let mut ids = BTreeMap::new();
    let mut physical_names = BTreeMap::new();
    for (&logical, name) in mappings {
        let key = (
            program.registers.named(logical).family,
            name.to_ascii_lowercase(),
        );
        let next = physical_ids.len();
        let physical = *physical_ids.entry(key).or_insert(next);
        ids.insert(logical, physical);
        physical_names.insert(physical, name.clone());
    }
    (ids, physical_names)
}

/// The inputs shared by the helpers that finish one table's rows.
struct RowContext<'a> {
    program: &'a Source,
    region: &'a Region,
    block: &'a Block,
    analysis: &'a flow::Analysis,
    rows: &'a [[Option<usize>; 2]],
    mappings: &'a BTreeMap<usize, String>,
    /// Logical register to physical identity.
    ids: BTreeMap<usize, usize>,
    /// Row of each scheduled operation.
    positions: BTreeMap<usize, usize>,
}

impl RowContext<'_> {
    /// The timing model of every scheduled operation, indexed by operation.
    fn timing_operations(&self) -> Vec<TimingOperation> {
        let mut operations = vec![TimingOperation::default(); self.region.instructions.len()];
        for (&id, &row) in &self.positions {
            operations[id] = self.timing_operation(id, row);
        }
        operations
    }

    /// The timing model of one operation issued in `row`.
    fn timing_operation(&self, id: usize, row: usize) -> TimingOperation {
        let located = &self.region.instructions[id];
        let timing_metadata =
            located
                .instruction
                .generated_copy
                .map_or(located.instruction.metadata, |kind| {
                    let pipe = if self.rows[row][0] == Some(id) {
                        schedule::Pipe::Upper
                    } else {
                        schedule::Pipe::Lower
                    };
                    &instruction::INSTRUCTIONS[usize::from(kind.resolved_descriptor(pipe))]
                });
        let mut operation = TimingOperation {
            unit_mask: timing_metadata.unit_mask,
            throughput: timing_metadata.throughput.max(1),
            ..TimingOperation::default()
        };

        // A merged table keeps each member's pairing (its own block).
        let home = self
            .region
            .blocks
            .iter()
            .find(|candidate| candidate.operations.contains(&id))
            .unwrap_or(self.block);
        let flow_id = flow_owner(self.region, home, id);
        for access in &located.effects.reads {
            for component in 0..7 {
                if access.mask & (1 << component) == 0 {
                    continue;
                }
                self.add_read(&mut operation, flow_id, row, access, component);
            }
        }

        for access in &located.effects.writes {
            let register = self.program.registers.named(access.register);
            if register.family != Family::Hardware
                && self.mappings[&access.register].ends_with("00")
            {
                continue;
            }
            if access.mask != 0 {
                operation.writes.push(TimingWrite {
                    register: self.ids[&access.register],
                    components: access.mask,
                    latency: located
                        .effects
                        .latencies
                        .get(&access.register)
                        .copied()
                        .unwrap_or(1),
                });
            }
        }
        operation
    }

    /// Records one component read of `access` (and its synchronization
    /// wait) on `operation`, issued in `row`.
    fn add_read(
        &self,
        operation: &mut TimingOperation,
        flow_id: usize,
        row: usize,
        access: &Access,
        component: u8,
    ) {
        let producer = self
            .analysis
            .reads
            .iter()
            .find(|read| {
                read.operation == flow_id
                    && read.register == access.register
                    && read.component == component
            })
            .into_iter()
            .flat_map(|read| &read.definitions)
            .filter_map(|definition| match definition {
                Definition::Operation(writer)
                    if self.positions.get(writer).is_some_and(|&time| time < row) =>
                {
                    Some(*writer)
                }
                _ => None,
            })
            .max_by_key(|writer| self.positions[writer]);
        let register = self.program.registers.named(access.register);
        operation.reads.push(TimingRead {
            register: self.ids[&access.register],
            components: 1 << component,
            producer,
            interlocked: register.family != Family::Hardware,
        });
        if let Some(writer) = producer.filter(|_| register.family == Family::Hardware) {
            operation
                .wait_reads
                .extend(self.wait_synchronization(access, &register.name, writer));
        }
    }

    /// The synchronization a read of hardware register `name`, produced by
    /// `writer`, must wait for.
    fn wait_synchronization(
        &self,
        access: &Access,
        name: &str,
        writer: usize,
    ) -> Option<Synchronization> {
        match name {
            "Q" => Some(Synchronization::Q {
                register: self.ids[&access.register],
            }),
            "P" => Some(Synchronization::P {
                register: self.ids[&access.register],
            }),
            "STAT_ALU" | "STAT_ALU_S" | "MAC" => {
                let vf = self.region.instructions[writer]
                    .effects
                    .writes
                    .iter()
                    .find(|write| {
                        let value = self.program.registers.named(write.register);
                        value.family == Family::Vector && self.mappings[&write.register] != "VF00"
                    })?;
                Some(Synchronization::Vector {
                    register: self.ids[&vf.register],
                    components: vf.mask,
                    producer: writer,
                })
            }
            _ => None,
        }
    }

    /// Whether the table keeps its layout as scheduled (fixed, raw, or with
    /// a write that escapes with a delay).
    fn is_raw_table(&self) -> bool {
        let (program, region, block, analysis) =
            (self.program, self.region, self.block, self.analysis);
        block
            .selected_table
            .as_ref()
            .is_some_and(|table| table.mode == TableMode::Fixed)
            || (movable_table(block)
                && self.positions.keys().any(|&id| {
                    region.instructions[id].effects.writes.iter().any(|&write| {
                        program.registers.named(write.register).family != Family::Vector
                            && escaping_delay(program, region, block, analysis, id, write)
                                .is_some_and(|delay| delay > 1)
                    })
                }))
            || block
                .operations
                .iter()
                .any(|&id| region.instructions[id].instruction.mode == ParseMode::Raw)
    }

    /// The scheduled rows as timed rows, marking those the reducer must keep.
    fn protected_rows(&self, raw: bool, branch: bool) -> Vec<TimedRow> {
        let kick_padding: BTreeSet<_> = self
            .positions
            .iter()
            .filter(|(id, _)| {
                self.region.instructions[**id].instruction.metadata.index == id::XGKICK
            })
            .flat_map(|(_, &row)| [row + 1, row + 2])
            .collect();
        let mut started = false;
        self.rows
            .iter()
            .enumerate()
            .map(|(index, &instructions)| {
                let mut row = TimedRow::new(instructions);
                row.protected = raw || branch || !started || kick_padding.contains(&index);
                started |= instructions.iter().any(Option::is_some);
                row
            })
            .collect()
    }

    /// The rows of an original-order cap/compression plan, replayed for
    /// annotations.
    fn planned_rows(
        &self,
        plan: &CompressionPlan,
        operations: &[TimingOperation],
        physical_names: BTreeMap<usize, String>,
    ) -> FinishedRows {
        let ids = &self.ids;
        let convert = |sync: Synchronization| -> Option<Synchronization> {
            Some(match sync {
                Synchronization::Q { register } => Synchronization::Q {
                    register: *ids.get(&register)?,
                },
                Synchronization::P { register } => Synchronization::P {
                    register: *ids.get(&register)?,
                },
                Synchronization::Vector {
                    register,
                    components,
                    producer,
                } => Synchronization::Vector {
                    register: *ids.get(&register)?,
                    components,
                    producer,
                },
            })
        };
        let mut kept = Vec::new();
        let mut origins = Vec::new();
        for (index, row) in plan.rows.iter().enumerate() {
            if plan.skipped[index] {
                continue;
            }
            let mut timed = TimedRow::new(*row);
            timed.synchronization = plan.sync[index].and_then(convert);
            kept.push(timed);
            origins.push(index);
        }

        match timing::replay(&kept, operations, &[]) {
            Ok(replay) => FinishedRows {
                rows: kept,
                origins,
                replay,
                physical_names,
            },
            Err(failure) => {
                // The replay is a conservative model (stall annotations and
                // validation only); the original layout stands without it.
                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                    crate::trace!("plan replay failed: {failure:?}");
                }
                let count = kept.len();
                FinishedRows {
                    rows: kept,
                    origins,
                    replay: identity_replay(count),
                    physical_names,
                }
            }
        }
    }

    /// The rows kept as scheduled, validated by a replay.
    fn fixed_rows(
        &self,
        timed: Vec<TimedRow>,
        operations: &[TimingOperation],
        physical_names: BTreeMap<usize, String>,
    ) -> Result<FinishedRows, CompileError> {
        let block = self.block;
        // A pipelined kernel reads non-interlocked Q/P values written by the
        // previous pass, which order-based flow attributes to this pass; its
        // fixed layout stands without the validation.
        let kernel = block
            .selected_table
            .as_ref()
            .is_some_and(|table| table.mode == TableMode::Fixed)
            && matches!(&block.end, End::Branch { target: Some(target), conditional: true, .. }
                if block.labels.iter().any(|label| label.eq_ignore_ascii_case(target)));
        let replay = match timing::replay(&timed, operations, &[]) {
            Ok(replay) => replay,
            Err(failure) if kernel => {
                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                    crate::trace!("kernel replay failed: {failure:?}");
                }
                identity_replay(timed.len())
            }
            Err(failure) => {
                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                    self.trace_read_mismatch(&failure);
                }
                return Err(internal(0, failure));
            }
        };
        Ok(FinishedRows {
            rows: timed,
            origins: (0..self.rows.len()).collect(),
            replay,
            physical_names,
        })
    }

    /// Traces a replay read mismatch, naming the operations involved.
    fn trace_read_mismatch(&self, failure: &timing::TimingError) {
        if let timing::TimingError::ReadMismatch {
            operation,
            expected,
            observed,
            ..
        } = failure
        {
            let describe = |id: usize| {
                let located = &self.region.instructions[id];
                format!("{} line {}", located.instruction.mnemonic, located.line)
            };
            crate::trace!(
                "replay mismatch: {} expected {:?} observed {:?}",
                describe(*operation),
                expected.map(describe),
                observed.map(describe)
            );
        }
    }
}

/// A replay that issues one row per cycle with no stalls.
fn identity_replay(count: usize) -> timing::Replay {
    timing::Replay {
        issue_times: (0..count as u32).collect(),
        stalls: vec![timing::Stalls::default(); count],
        elapsed: count as u32,
        drain_cycles: 0,
    }
}

pub(super) fn render_synchronization(
    sync: Synchronization,
    names: &BTreeMap<usize, String>,
    modifiers: u8,
) -> Result<String, CompileError> {
    let Synchronization::Vector {
        register,
        components,
        ..
    } = sync
    else {
        return Ok(sync.mnemonic().to_ascii_lowercase());
    };
    let suffix = if components == 15 {
        String::new()
    } else {
        format!(".{}", instruction::Mask(components).suffix())
    };
    let source = format!("abs{suffix} VF00,{}", names[&register]);
    let parsed = parser::parse_with_options(
        &source,
        parser::ParserOptions {
            allow_incomplete: true,
            ..Default::default()
        },
    )
    .map_err(|errors| internal(0, format!("failed to construct flag interlock: {errors:?}")))?;
    let cap = parsed
        .statements
        .iter()
        .find_map(|statement| match &statement.kind {
            StatementKind::Instruction(instruction) => Some(instruction),
            _ => None,
        })
        .ok_or_else(|| internal(0, "flag interlock has no instruction"))?;
    crate::emit::instruction(cap, &vec![None; cap.operands.len()], modifiers)
        .map_err(|e| internal(0, e))
}

/// Issue layout of a fixed table for flow analysis. Q, P and the flags are
/// not interlocked: a result is visible only after its latency, which in a
/// pipelined loop is often in the next part or kernel pass.
pub(super) fn table_timing(
    program: &Source,
    region: &Region,
    block: &Block,
    rows: &[[Option<usize>; 2]],
) -> Option<flow::Timing> {
    let mut row_of = BTreeMap::new();
    for (row, pair) in rows.iter().enumerate() {
        for &id in pair.iter().flatten() {
            row_of.insert(id, row as u32);
        }
    }
    let issue = block
        .operations
        .iter()
        .map(|id| row_of.get(id).copied())
        .collect::<Option<Vec<_>>>()?;
    let mut delays = BTreeMap::new();
    for &id in &block.operations {
        let effects = &region.instructions[id].effects;
        for write in &effects.writes {
            let register = program.registers.named(write.register);
            // Q/P results and the MAC/status/clip flags are not interlocked;
            // ACC is forwarded between FMAC operations and stays immediate.
            if register.family == Family::Hardware
                && matches!(
                    register.name.as_str(),
                    "Q" | "P" | "MAC" | "STAT_ALU" | "STAT_DIV" | "CLIP"
                )
            {
                let latency = effects.latencies.get(&write.register).copied().unwrap_or(0);
                if latency > 0 {
                    delays.insert((id, write.register), u32::from(latency));
                }
            }
        }
    }
    Some(flow::Timing {
        issue,
        length: rows.len() as u32,
        delays,
    })
}
