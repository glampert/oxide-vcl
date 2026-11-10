//! Finishing scheduled tables into emitted rows: synchronization caps, row
//! timing and the generated-row bookkeeping each table carries.

use super::*;

pub(super) struct FinishedRows {
    pub(super) rows: Vec<TimedRow>,
    pub(super) origins: Vec<usize>,
    pub(super) replay: timing::Replay,
    pub(super) physical_names: BTreeMap<usize, String>,
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
    let mut positions = BTreeMap::new();
    for (row, pair) in rows.iter().enumerate() {
        for &operation in pair.iter().flatten() {
            positions.insert(operation, row);
        }
    }
    let mut operations = vec![TimingOperation::default(); region.instructions.len()];
    for (&id, &row) in &positions {
        let located = &region.instructions[id];
        let timing_metadata =
            located
                .instruction
                .generated_copy
                .map_or(located.instruction.metadata, |kind| {
                    let pipe = if rows[row][0] == Some(id) {
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
        let home = region
            .blocks
            .iter()
            .find(|candidate| candidate.operations.contains(&id))
            .unwrap_or(block);
        let flow_id = flow_owner(region, home, id);
        for access in &located.effects.reads {
            for component in 0..7 {
                if access.mask & (1 << component) == 0 {
                    continue;
                }
                let producer = analysis
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
                            if positions.get(writer).is_some_and(|&time| time < row) =>
                        {
                            Some(*writer)
                        }
                        _ => None,
                    })
                    .max_by_key(|writer| positions[writer]);
                let register = program.registers.named(access.register);
                operation.reads.push(TimingRead {
                    register: ids[&access.register],
                    components: 1 << component,
                    producer,
                    interlocked: register.family != Family::Hardware,
                });
                if let Some(writer) = producer.filter(|_| register.family == Family::Hardware) {
                    match register.name.as_str() {
                        "Q" => operation.wait_reads.push(Synchronization::Q {
                            register: ids[&access.register],
                        }),
                        "P" => operation.wait_reads.push(Synchronization::P {
                            register: ids[&access.register],
                        }),
                        "STAT_ALU" | "STAT_ALU_S" | "MAC" => {
                            if let Some(vf) = region.instructions[writer]
                                .effects
                                .writes
                                .iter()
                                .find(|write| {
                                    let value = program.registers.named(write.register);
                                    value.family == Family::Vector
                                        && mappings[&write.register] != "VF00"
                                })
                            {
                                operation.wait_reads.push(Synchronization::Vector {
                                    register: ids[&vf.register],
                                    components: vf.mask,
                                    producer: writer,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for access in &located.effects.writes {
            let register = program.registers.named(access.register);
            if register.family != Family::Hardware && mappings[&access.register].ends_with("00") {
                continue;
            }
            if access.mask != 0 {
                operation.writes.push(TimingWrite {
                    register: ids[&access.register],
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
        operations[id] = operation;
    }
    let raw = block
        .selected_table
        .as_ref()
        .is_some_and(|table| table.mode == TableMode::Fixed)
        || (movable_table(block)
            && positions.keys().any(|&id| {
                region.instructions[id].effects.writes.iter().any(|&write| {
                    program.registers.named(write.register).family != Family::Vector
                        && escaping_delay(program, region, block, analysis, id, write)
                            .is_some_and(|delay| delay > 1)
                })
            }))
        || block
            .operations
            .iter()
            .any(|&id| region.instructions[id].instruction.mode == ParseMode::Raw);
    let branch = matches!(block.end, End::Branch { .. });
    let kick_padding: BTreeSet<_> = positions
        .iter()
        .filter(|(id, _)| region.instructions[**id].instruction.metadata.index == 165)
        .flat_map(|(_, &row)| [row + 1, row + 2])
        .collect();
    let mut started = false;
    let timed: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(index, &instructions)| {
            let mut row = TimedRow::new(instructions);
            row.protected = raw || branch || !started || kick_padding.contains(&index);
            started |= instructions.iter().any(Option::is_some);
            row
        })
        .collect();
    if let Some(plan) = plan.filter(|plan| plan.rows.len() == rows.len()) {
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
        match timing::replay(&kept, &operations, &[]) {
            Ok(replay) => {
                return Ok(FinishedRows {
                    rows: kept,
                    origins,
                    replay,
                    physical_names,
                });
            }
            Err(failure) => {
                // The replay is a conservative model (stall annotations and
                // validation only); the original layout stands without it.
                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                    crate::trace!("plan replay failed: {failure:?}");
                }
                let count = kept.len();
                return Ok(FinishedRows {
                    rows: kept,
                    origins,
                    replay: timing::Replay {
                        issue_times: (0..count as u32).collect(),
                        stalls: vec![timing::Stalls::default(); count],
                        elapsed: count as u32,
                        drain_cycles: 0,
                    },
                    physical_names,
                });
            }
        }
    }
    if options.disable_size_reduction || raw || branch {
        // A pipelined kernel reads non-interlocked Q/P values written by the
        // previous pass, which order-based flow attributes to this pass; its
        // fixed layout stands without the validation.
        let kernel = block
            .selected_table
            .as_ref()
            .is_some_and(|table| table.mode == TableMode::Fixed)
            && matches!(&block.end, End::Branch { target: Some(target), conditional: true, .. }
                if block.labels.iter().any(|label| label.eq_ignore_ascii_case(target)));
        let replay = match timing::replay(&timed, &operations, &[]) {
            Ok(replay) => replay,
            Err(failure) if kernel => {
                if std::env::var_os("VCL_TIMING_DEBUG").is_some() {
                    crate::trace!("kernel replay failed: {failure:?}");
                }
                let count = timed.len();
                timing::Replay {
                    issue_times: (0..count as u32).collect(),
                    stalls: vec![timing::Stalls::default(); count],
                    elapsed: count as u32,
                    drain_cycles: 0,
                }
            }
            Err(failure) => {
                if std::env::var_os("VCL_TIMING_DEBUG").is_some()
                    && let timing::TimingError::ReadMismatch {
                        operation,
                        expected,
                        observed,
                        ..
                    } = &failure
                {
                    let describe = |id: usize| {
                        let located = &region.instructions[id];
                        format!("{} line {}", located.instruction.mnemonic, located.line)
                    };
                    crate::trace!(
                        "replay mismatch: {} expected {:?} observed {:?}",
                        describe(*operation),
                        expected.map(describe),
                        observed.map(describe)
                    );
                }
                return Err(internal(0, failure));
            }
        };
        return Ok(FinishedRows {
            rows: timed,
            origins: (0..rows.len()).collect(),
            replay,
            physical_names,
        });
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
