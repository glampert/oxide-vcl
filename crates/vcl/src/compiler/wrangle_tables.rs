//! Building the wrangler's work table for a span of a region's instructions.

use super::*;
use crate::scheduler::wrangle::{self, Direction as WDirection, Source as WSource};
use WrangleKey as Key;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum WrangleKey {
    Physical(u8, usize),
    Virtual(usize),
    Hardware(String),
}

/// A block scheduled by the ported linear optimizer, retained for the
/// post-allocation raw reduction.
#[derive(Clone, Debug)]
pub(super) struct WrangledTable {
    pub(super) live: Vec<usize>,
    pub(super) problem: crate::scheduler::wrangle::Problem,
    pub(super) keys: Vec<WrangleKey>,
    /// Program register of each wrangle operand.
    pub(super) operand_registers: Vec<Vec<usize>>,
    pub(super) statics: Vec<crate::scheduler::wrangle::Statics>,
    pub(super) lower_bound: i32,
    /// Table end before Tidy padding.
    pub(super) end: usize,
    /// Optimized rows of `live` indices.
    pub(super) rows: Vec<[Option<usize>; 2]>,
    /// Affine induction consumers: consumer, setter,
    /// before-phase, add (true) or subtract, stride expression, EA operand.
    pub(super) affine: Vec<AffineUse>,
    pub(super) pools: crate::scheduler::wrangle::Pools,
    /// Local allocation of table-local virtual registers: program register
    /// to physical index.
    pub(super) allocation: BTreeMap<usize, usize>,
    /// The linear strategy whose table won.
    pub(super) strategy: Option<crate::scheduler::schedule::Strategy>,
}

#[derive(Clone, Debug)]
pub(super) struct AffineUse {
    pub(super) consumer: usize,
    pub(super) setter: usize,
    pub(super) before: bool,
    pub(super) add: bool,
    pub(super) stride: String,
    pub(super) operand: usize,
}

/// Issue pipes after DetectMoves: an unscheduled vector copy may use either.
/// Whether `register` is the setter's first operand, written: a load's
/// destination, as opposed to an address increment. An early-sampled branch
/// reading an increment needs one more cycle (in the original).
pub(super) fn writes_first_operand(setter: &Located, register: usize) -> bool {
    setter
        .instruction
        .operands
        .first()
        .is_some_and(|operand| operand.direction == Direction::Write)
        && setter.operands.first().copied().flatten() == Some(register)
}

/// The `nop` left by a folded branch keeps the branch's lower pipe.
pub(super) fn lower_nop(
    nop: &'static instruction::InstructionDef,
) -> &'static instruction::InstructionDef {
    static LOWER: std::sync::OnceLock<instruction::InstructionDef> = std::sync::OnceLock::new();
    LOWER.get_or_init(|| instruction::InstructionDef {
        issue_mask: issue::LOWER,
        ..*nop
    })
}

pub(super) fn issue_pipes(located: &Located) -> u8 {
    let metadata = located.instruction.metadata;
    if metadata.index == id::MOVE && located.instruction.mode == ParseMode::Vcl {
        issue::EITHER
    } else {
        metadata.issue_mask
    }
}

/// Build the original work table for one ordinary linear
/// block, without external timing.
pub(super) fn build_wrangle_table(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
    block_id: usize,
    live: &[usize],
    timid: bool,
) -> Option<WrangledTable> {
    build_wrangle_table_span(program, region, analysis, block_id, block_id, live, timid)
}

/// Operand lists in descriptor order: register, direction, mask, latency and
/// whether the operand blocks.
type OperandList = Vec<(usize, WDirection, u8, i8, bool)>;

/// A memory instruction's address: base register, offset text, and kind
/// (plain, post-increment, pre-decrement).
type Address = (Option<usize>, String, u8);

/// The wrangle registers allocated so far, keyed by identity.
struct RegisterMap {
    keys: BTreeMap<WrangleKey, usize>,
    registers: Vec<wrangle::Register>,
}

impl RegisterMap {
    /// The wrangle register for program register `id`, allocating it on first
    /// use; `None` for registers outside the pools (VF00/VI00).
    fn register_of(&mut self, program: &Source, region: &Region, id: usize) -> Option<usize> {
        let register = program.registers.named(id);
        let (key, info) = match register.family {
            Family::Hardware => {
                let (global, parts, nonblocking, throughput) = hardware_register(&register.name)?;
                (
                    Key::Hardware(register.name.to_ascii_uppercase()),
                    wrangle::Register {
                        parts,
                        global_id: Some(global),
                        throughput_tracked: throughput,
                        nonblocking_id: nonblocking,
                        pool: 0,
                        physical: None,
                        through: 0,
                    },
                )
            }
            family => {
                let parts = if family == Family::Vector { 4 } else { 1 };
                let base = register
                    .physical
                    .or_else(|| region.forced.get(&id).copied());
                let key = match base {
                    Some(0) => return None,
                    Some(base) => Key::Physical(u8::from(family == Family::Vector), base),
                    None => Key::Virtual(id),
                };
                let physical = match key {
                    Key::Physical(_, n) => Some(n),
                    _ => None,
                };
                (
                    key,
                    wrangle::Register {
                        parts,
                        pool: if family == Family::Vector { 1 } else { 2 },
                        physical,
                        ..wrangle::Register::default()
                    },
                )
            }
        };
        let registers = &mut self.registers;
        Some(*self.keys.entry(key).or_insert_with(|| {
            registers.push(info);
            registers.len() - 1
        }))
    }
}

/// The inputs shared by the steps that build one work table.
struct TableBuilder<'a> {
    program: &'a Source,
    region: &'a Region,
    analysis: &'a flow::Analysis,
    first: usize,
    block_id: usize,
    live: &'a [usize],
    timid: bool,
    /// Operation id to its index in `live`.
    local: BTreeMap<usize, usize>,
    /// Flow reads by (operation, register, component).
    reads_by: BTreeMap<(usize, usize, u8), &'a flow::Read>,
    /// Operand lists of `live`, in descriptor order.
    operand_lists: Vec<OperandList>,
    /// Operand indices after dropping pool-zero operands.
    mapped_index: Vec<Vec<Option<usize>>>,
}

/// As `build_wrangle_table`, for a merged table: it enters with the live
/// state of `first` and leaves with that of `last`.
pub(super) fn build_wrangle_table_span(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
    first: usize,
    block_id: usize,
    live: &[usize],
    timid: bool,
) -> Option<WrangledTable> {
    let mut builder = TableBuilder {
        program,
        region,
        analysis,
        first,
        block_id,
        live,
        timid,
        local: live.iter().enumerate().map(|(i, &id)| (id, i)).collect(),
        reads_by: BTreeMap::new(),
        operand_lists: Vec::new(),
        mapped_index: Vec::new(),
    };
    let mut map = RegisterMap {
        keys: BTreeMap::new(),
        registers: Vec::new(),
    };
    builder.reads_by = builder.collect_reads();

    // Affine induction annotation: a VI pointer with one
    // affine setter, live into and out of the table, frees the base reads of
    // its LQ/SQ/ILW/ISW users from lock constraints.
    let affine = builder.affine_uses();
    let affine_bases: BTreeMap<usize, usize> = affine
        .iter()
        .map(|a| {
            (
                a.consumer,
                region.instructions[live[a.consumer]].operands[a.operand].unwrap(),
            )
        })
        .collect();
    builder.operand_lists = builder.build_operand_lists(&affine_bases);

    // Operand indices after dropping pool-zero operands.
    builder.mapped_index = builder
        .operand_lists
        .iter()
        .map(|operands| {
            let mut next = 0;
            let mut indices = Vec::new();
            for &(register, ..) in operands {
                if map.register_of(program, region, register).is_some() {
                    indices.push(Some(next));
                    next += 1;
                } else {
                    indices.push(None);
                }
            }
            indices
        })
        .collect();

    let mut instructions = Vec::new();
    let mut operand_registers = Vec::new();
    for (index, &id) in live.iter().enumerate() {
        let (instruction, registers_here) = builder.wrangle_instruction(&mut map, index, id);
        operand_registers.push(registers_here);
        instructions.push(instruction);
    }
    builder.add_memory_dependencies(&mut instructions);
    builder.add_through_bits(&mut map, &instructions);

    let RegisterMap { keys, registers } = map;
    let mut problem = wrangle::Problem {
        registers,
        instructions,
        horizon: 64,
        minimum_length: 0,
        stagger: false,
        names: live
            .iter()
            .map(|&id| region.instructions[id].instruction.mnemonic.clone())
            .collect(),
    };
    // ACC/I producer links and ACC chain starts.
    link_accumulators(&mut problem);
    if std::env::var_os("VCL_WRANGLE_PROBLEM").is_some() {
        builder.trace_problem(&problem, &keys);
    }
    builder.add_affine_statistics(&mut problem, &affine);

    let statics = wrangle::statics(&problem);
    let (lower_bound, horizon) = wrangle::bounds(&problem, &statics);
    problem.horizon = horizon;
    let pools = builder.through_pools(&keys);

    let mut ordered: Vec<(WrangleKey, usize)> = keys.into_iter().collect();
    ordered.sort_by_key(|&(_, index)| index);
    Some(WrangledTable {
        live: live.to_vec(),
        problem,
        keys: ordered.into_iter().map(|(key, _)| key).collect(),
        operand_registers,
        statics,
        lower_bound,
        end: 0,
        rows: Vec::new(),
        affine,
        pools,
        allocation: BTreeMap::new(),
        strategy: None,
    })
}

impl<'a> TableBuilder<'a> {
    /// The flow reads of the table by (operation, register, component).
    fn collect_reads(&self) -> BTreeMap<(usize, usize, u8), &'a flow::Read> {
        let (region, analysis) = (self.region, self.analysis);
        let mut reads_by: BTreeMap<(usize, usize, u8), &flow::Read> = BTreeMap::new();
        for read in &analysis.reads {
            reads_by.insert((read.operation, read.register, read.component), read);
        }
        // A fixed table's lower half reads with its upper partner (flow moves
        // them to the pair's first operation).
        for &id in self.live {
            let Some(home) = region
                .blocks
                .iter()
                .find(|candidate| candidate.operations.contains(&id))
            else {
                continue;
            };
            let owner = flow_owner(region, home, id);
            if owner == id || home.selected_table.is_none() {
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
        reads_by
    }

    /// The hardware register's global id, for a hardware register.
    fn hardware_id(&self, id: usize) -> Option<u8> {
        let register = self.program.registers.named(id);
        (register.family == Family::Hardware)
            .then(|| hardware_register(&register.name).map(|h| h.0))
            .flatten()
    }

    /// The affine induction consumers of the table's VI pointers.
    fn affine_uses(&self) -> Vec<AffineUse> {
        let (program, region, live) = (self.program, self.region, self.live);
        let mut affine: Vec<AffineUse> = Vec::new();
        let mut vi_registers = BTreeSet::new();
        for &id in live {
            for access in region.instructions[id]
                .effects
                .reads
                .iter()
                .chain(&region.instructions[id].effects.writes)
            {
                if program.registers.named(access.register).family == Family::Integer {
                    vi_registers.insert(access.register);
                }
            }
        }

        let mut annotated = BTreeSet::new();
        for register in vi_registers {
            let Some((setter, add, stride)) = self.affine_setter(register) else {
                continue;
            };
            for (consumer, &id) in live.iter().enumerate() {
                let Some((before, operand)) = self.affine_consumer(register, setter, consumer, id)
                else {
                    continue;
                };
                if !annotated.insert(consumer) {
                    continue;
                }
                affine.push(AffineUse {
                    consumer,
                    setter,
                    before,
                    add,
                    stride: stride.clone(),
                    operand,
                });
            }
        }
        affine
    }

    /// The single affine setter of VI `register` live into and out of the
    /// table: its index in `live`, whether it adds, and its stride.
    fn affine_setter(&self, register: usize) -> Option<(usize, bool, String)> {
        let (region, live) = (self.region, self.live);
        let setters: Vec<usize> = live
            .iter()
            .enumerate()
            .filter(|&(_, &id)| {
                region.instructions[id]
                    .effects
                    .writes
                    .iter()
                    .any(|w| w.register == register)
            })
            .map(|(i, _)| i)
            .collect();
        let [setter] = setters[..] else { return None };
        let located = &region.instructions[live[setter]];
        let index = located.instruction.metadata.index;
        let (add, input, output, stride) = match index {
            109 | 110 => (true, 1, 0, None),
            112 => (false, 1, 0, None),
            158 | 163 => (true, 1, 1, Some("1".to_string())),
            159 | 164 => (false, 1, 1, Some("1".to_string())),
            _ => return None,
        };
        if located.operands.get(input).copied().flatten() != Some(register)
            || located.operands.get(output).copied().flatten() != Some(register)
        {
            return None;
        }
        let stride = match stride {
            Some(stride) => stride,
            None => located
                .instruction
                .operands
                .get(2)
                .and_then(|o| o.immediate.clone())?,
        };
        let local = &self.local;
        let external_input = live.iter().any(|&id| {
            self.reads_by.get(&(id, register, 0)).is_some_and(|read| {
                read.definitions.iter().any(|d| match d {
                    Definition::Operation(w) => local.get(w).is_none_or(|&w| w >= local[&id]),
                    _ => true,
                })
            })
        });
        let external_output = self.analysis.live_out[self.block_id]
            .iter()
            .any(|a| a.register == register && a.mask & 1 != 0);
        if !external_input || !external_output {
            return None;
        }
        Some((setter, add, stride))
    }

    /// Whether `id` (at `consumer` in `live`) is an LQ/SQ/ILW/ISW user of
    /// `register` that the affine `setter` frees from lock constraints: the
    /// before-phase flag and the address operand index.
    fn affine_consumer(
        &self,
        register: usize,
        setter: usize,
        consumer: usize,
        id: usize,
    ) -> Option<(bool, usize)> {
        let user = &self.region.instructions[id];
        if !matches!(
            user.instruction.metadata.index,
            id::LQ | id::ILW | id::SQ | id::ISW
        ) || user.instruction.fixed_offset
        {
            return None;
        }
        let ea = user
            .instruction
            .operands
            .iter()
            .position(|o| class::is_address(o.class))?;
        if ea != 1 || user.operands.get(ea).copied().flatten() != Some(register) {
            return None;
        }
        let read = self.reads_by.get(&(id, register, 0))?;
        let before = read.definitions.iter().all(|d| match d {
            Definition::Operation(w) => self.local.get(w).is_none_or(|&w| w >= consumer),
            _ => true,
        });
        let after = read.definitions.len() == 1
            && read
                .definitions
                .contains(&Definition::Operation(self.live[setter]));
        (before || after).then_some((before, ea))
    }

    /// Operand lists in descriptor order, with effective masks from effects.
    fn build_operand_lists(&self, affine_bases: &BTreeMap<usize, usize>) -> Vec<OperandList> {
        self.live
            .iter()
            .enumerate()
            .map(|(operands_index, &id)| self.operand_list(operands_index, id, affine_bases))
            .collect()
    }

    /// The operand list of one instruction.
    fn operand_list(
        &self,
        operands_index: usize,
        id: usize,
        affine_bases: &BTreeMap<usize, usize>,
    ) -> OperandList {
        let (program, region) = (self.program, self.region);
        let located = &region.instructions[id];
        let expanded = crate::analysis::prune::expand(&located.instruction);
        let mut reads_left: Vec<Option<Access>> =
            located.effects.reads.iter().copied().map(Some).collect();
        let mut writes_left: Vec<Option<Access>> =
            located.effects.writes.iter().copied().map(Some).collect();
        let mut operands = Vec::new();
        for operand in &expanded.operands {
            let Some(name) = &operand.register else {
                continue;
            };
            let Some(kind) = family(operand.class) else {
                continue;
            };
            let register = if kind == Family::Hardware {
                program
                    .registers
                    .ids
                    .get(&(Family::Hardware, name.to_ascii_lowercase()))
                    .copied()
            } else {
                located
                    .operands
                    .get(operand.source_operand)
                    .copied()
                    .flatten()
            };
            let Some(register) = register else { continue };
            let (list, direction) = match operand.direction {
                Direction::Read => (&mut reads_left, WDirection::Read),
                Direction::Write => (&mut writes_left, WDirection::Write),
                Direction::Unknown => continue,
            };
            let Some(slot) = list
                .iter_mut()
                .find(|access| access.is_some_and(|a| a.register == register))
            else {
                continue;
            };
            let access = slot.take().unwrap();
            if direction == WDirection::Read
                && class::is_address(operand.class)
                && affine_bases.get(&operands_index) == Some(&register)
            {
                continue;
            }
            let latency = if direction == WDirection::Write {
                located
                    .effects
                    .latencies
                    .get(&register)
                    .copied()
                    .unwrap_or(1) as i8
            } else {
                -1
            };
            operands.push((register, direction, access.mask, latency, operand.blocking));
        }
        for access in reads_left.into_iter().flatten() {
            operands.push((access.register, WDirection::Read, access.mask, -1, true));
        }

        // Lowering adds a second write for an incrementing/decrementing EA
        // operand; the descriptor has one write and an implicit patched read.
        let written: BTreeSet<usize> = operands
            .iter()
            .filter(|&&(_, d, ..)| d == WDirection::Write)
            .map(|&(r, ..)| r)
            .collect();
        for access in writes_left
            .into_iter()
            .flatten()
            .filter(|access| !written.contains(&access.register))
        {
            let latency = located
                .effects
                .latencies
                .get(&access.register)
                .copied()
                .unwrap_or(1) as i8;
            operands.push((
                access.register,
                WDirection::Write,
                access.mask,
                latency,
                true,
            ));
        }
        operands
    }

    /// The wrangle instruction for `id` (at `index` in `live`), and the
    /// program registers of its operands.
    fn wrangle_instruction(
        &self,
        map: &mut RegisterMap,
        index: usize,
        id: usize,
    ) -> (wrangle::Instruction, Vec<usize>) {
        let (program, region) = (self.program, self.region);
        let located = &region.instructions[id];
        let metadata = located.instruction.metadata;
        let mut operands = Vec::new();
        let mut registers_here = Vec::new();
        for &(register, direction, mask, latency, blocking) in &self.operand_lists[index] {
            let Some(mapped) = map.register_of(program, region, register) else {
                continue;
            };
            registers_here.push(register);
            operands.push(self.wrangle_operand(
                index, id, mapped, register, direction, mask, latency, blocking,
            ));
        }

        let instruction = wrangle::Instruction {
            // Spaced like the original's sequence numbers so a
            // synthesized move can sort between its setter and the next.
            order: (index as i32 + 1) * 8,
            pipes: issue_pipes(located),
            throughput: metadata.throughput.clamp(1, 127) as u8,
            units: metadata.unit_mask,
            branch: metadata.is_branch(),
            memory: if metadata.is_memory() {
                metadata.subtype
            } else {
                0
            },
            minimum_position: 0,
            end_distance: 0,
            protected_read: false,
            protected_write: false,
            forced_predecessors: Vec::new(),
            forced_successors: Vec::new(),
            acc_special: false,
            side_effect: 0,
            is_move: false,
            dual_copy: issue_pipes(located) == 3 || located.instruction.generated_copy.is_some(),
            move_depth: 0,
            move_target: 0,
            full_period: false,
            stat_links: Vec::new(),
            first_output: located
                .instruction
                .operands
                .first()
                .filter(|operand| operand.direction == Direction::Write)
                .and_then(|_| {
                    let register = located.operands.first().copied().flatten()?;
                    let vector = program.registers.named(register).family == Family::Vector;
                    Some(wrangle::FirstOutput {
                        vector,
                        register: map.register_of(program, region, register),
                        mask: located.instruction.mask.0,
                        latency: metadata.operands.first().map_or(1, |o| o.latency),
                    })
                }),
            operands,
        };
        (instruction, registers_here)
    }

    /// One wrangle operand of instruction `id` (at `index` in `live`): its
    /// per-component sources, input offset, escapes and observed flag.
    #[allow(clippy::too_many_arguments)]
    fn wrangle_operand(
        &self,
        index: usize,
        id: usize,
        mapped: usize,
        register: usize,
        direction: WDirection,
        mask: u8,
        latency: i8,
        blocking: bool,
    ) -> wrangle::Operand {
        let conditional = self.region.instructions[id]
            .instruction
            .metadata
            .is_conditional_branch();
        let mut sources = [None; 4];
        let mut input_offset = 0;
        let mut escapes = 0;
        for part in 0..4u8 {
            if mask & (1 << part) == 0 {
                continue;
            }
            match direction {
                WDirection::Read => {
                    let definitions = self
                        .reads_by
                        .get(&(id, register, part))
                        .map(|read| &read.definitions);
                    let mut source = WSource::External;
                    if let Some(definitions) = definitions {
                        if let Some(local) = self.local_source(index, register, part, definitions) {
                            source = local;
                        }
                        if conditional
                            && self.program.registers.named(register).family == Family::Integer
                            && self.flags_after_setter(register, definitions)
                        {
                            input_offset = 1;
                        }
                    }
                    sources[usize::from(part)] = Some(source);
                }
                WDirection::Write => {
                    if self.escapes_table(index, register, part) {
                        escapes |= 1 << part;
                    }
                }
            }
        }
        let observed = direction == WDirection::Write && self.is_observed(id, register, mask);
        wrangle::Operand {
            register: mapped,
            direction,
            mask,
            latency,
            blocking,
            input_offset,
            sources,
            escapes,
            observed,
        }
    }

    /// The local source of a read when a single earlier instruction of the
    /// table defines it.
    fn local_source(
        &self,
        index: usize,
        register: usize,
        part: u8,
        definitions: &BTreeSet<Definition>,
    ) -> Option<WSource> {
        if definitions.len() != 1 {
            return None;
        }
        let Some(Definition::Operation(writer)) = definitions.iter().next() else {
            return None;
        };
        let &w = self.local.get(writer)?;
        if w >= index {
            return None;
        }
        let o = self.operand_lists[w]
            .iter()
            .position(|&(r, d, m, _, _)| {
                r == register && d == WDirection::Write && m & (1 << part) != 0
            })
            .and_then(|o| self.mapped_index[w][o])?;
        Some(WSource::Local {
            instruction: w,
            operand: o,
        })
    }

    /// Whether a conditional branch reading VI `register` needs the extra
    /// cycle: some definition is an input or a setter that is not a load
    /// of it and reads no flags.
    fn flags_after_setter(&self, register: usize, definitions: &BTreeSet<Definition>) -> bool {
        let mut input_offset = false;
        for definition in definitions {
            // An `--enter` input's definer is no
            // load and reads no flags.
            if *definition == Definition::Input {
                input_offset = true;
            }
            let Definition::Operation(writer) = definition else {
                continue;
            };
            let setter = &self.region.instructions[*writer];
            let first_operand = writes_first_operand(setter, register);
            let flagged = setter.effects.reads.iter().any(|r| {
                self.hardware_id(r.register)
                    .is_some_and(|g| (0x36..=0x3b).contains(&g))
            });
            if (!setter.instruction.metadata.is_memory() || !first_operand) && !flagged {
                input_offset = true;
            }
        }
        input_offset
    }

    /// Whether the write of `register` component `part` by the instruction at
    /// `index` is the last in the table and live out.
    fn escapes_table(&self, index: usize, register: usize, part: u8) -> bool {
        let later = self.live[index + 1..].iter().any(|&other| {
            self.region.instructions[other]
                .effects
                .writes
                .iter()
                .any(|w| w.register == register && w.mask & (1 << part) != 0)
        });
        !later
            && self.analysis.live_out[self.block_id]
                .iter()
                .any(|a| a.register == register && a.mask & (1 << part) != 0)
    }

    /// Whether some component of `register` written by `id` is read, or
    /// reaches an output.
    fn is_observed(&self, id: usize, register: usize, mask: u8) -> bool {
        let analysis = self.analysis;
        (0..4u8).any(|part| {
            mask & (1 << part) != 0
                && (analysis.reads.iter().any(|read| {
                    read.register == register
                        && read.component == part
                        && read.definitions.contains(&Definition::Operation(id))
                }) || analysis.outputs.iter().any(|output| {
                    output.register == register
                        && output.component == part
                        && output.definitions.contains(&Definition::Operation(id))
                }))
        })
    }

    /// The address of a memory instruction.
    fn address(&self, id: usize) -> Option<Address> {
        let (program, region) = (self.program, self.region);
        let located = &region.instructions[id];
        let index = located
            .instruction
            .operands
            .iter()
            .position(|o| class::is_address(o.class))?;
        let operand = &located.instruction.operands[index];
        let base = located.operands[index].and_then(|r| {
            let register = program.registers.named(r);
            let physical = register.physical.or_else(|| region.forced.get(&r).copied());
            (physical != Some(0)).then_some(r)
        });
        let offset = operand
            .immediate
            .as_deref()
            .map(|text| {
                crate::parser::expression::evaluate(text)
                    .ok()
                    .and_then(|v| v.number())
                    .map_or_else(|| text.replace(' ', ""), |n| format!("{}", n.to_u32()))
            })
            .unwrap_or_else(|| "0".into());
        // Compares the EA operands' mask byte, which does
        // not separate `disp(reg)` from `disp(reg)mask` (ADDRESS_OFFSET vs
        // ADDRESS_REGISTER_OFFSET): an
        // `sq ...,3(vi04)` aliases `ilw.w ...,3(vi04)` (vu_1518195603). Only
        // the post-increment and pre-decrement forms differ.
        let kind = match operand.class {
            class::ADDRESS_INCREMENT => 1,
            class::ADDRESS_DECREMENT => 2,
            _ => 0,
        };
        Some((base, offset, kind))
    }

    /// The memory tags of an instruction's address operand.
    fn tags(&self, id: usize) -> &[String] {
        self.region.instructions[id]
            .instruction
            .operands
            .iter()
            .find(|operand| class::is_address(operand.class))
            .map_or(&[], |operand| operand.tags.as_slice())
    }

    /// Memory tags (`sq v,0(p):frame`) are resources the store writes and the
    /// load reads over the fields it touches, so a shared tag orders a load
    /// against a store but not two stores (observed from vcl 1.4beta7:
    /// `memory-tag-shared` keeps a store and a later load in order, and
    /// `memory-tag-store-store` lets two stores swap, as do
    /// `memory-tag-none` and `memory-tag-disjoint`).
    fn shares_tag(&self, a: usize, b: usize) -> bool {
        self.tags(a).iter().any(|tag| self.tags(b).contains(tag))
    }

    /// The memory fields an instruction touches.
    fn ram_bits(&self, id: usize) -> u8 {
        let instruction = &self.region.instructions[id].instruction;
        if instruction.metadata.mask_mode == MaskMode::Unmasked {
            0xf
        } else {
            instruction.mask.0
        }
    }

    /// The descriptor's order flags of an instruction.
    fn order_flags(&self, id: usize) -> u8 {
        self.region.instructions[id]
            .instruction
            .metadata
            .order_flags
    }

    /// Memory forced dependencies.
    fn add_memory_dependencies(&self, instructions: &mut [wrangle::Instruction]) {
        // Order classes (the descriptor's order flags): WRITES_MEMORY and
        // READS_MEMORY compare like a store and a load against earlier loads and
        // stores, CHAINED links to the nearest earlier instruction of that class.
        // Only XGKICK (READS_MEMORY | CHAINED) carries any: it follows every
        // earlier store (it has no address to compare) and the previous XGKICK,
        // and a later store follows it.
        let live = self.live;
        for i in 0..live.len() {
            let kind = instructions[i].memory;
            let flags = self.order_flags(live[i]);
            let after_loads = kind == STORE || flags & order::WRITES_MEMORY != 0;
            let after_stores = kind != 0 || flags & order::READS_MEMORY != 0;
            let mut nearest_class = flags & order::CHAINED != 0;
            if !after_loads && !after_stores && !nearest_class {
                continue;
            }
            let ea = self.address(live[i]);
            for j in (0..i).rev() {
                let other = instructions[j].memory;
                let ordered = (after_stores && other == STORE) || (after_loads && other == LOAD);
                let other_flags = self.order_flags(live[j]);
                let mut flagged = false;
                if nearest_class && other_flags & order::CHAINED != 0 {
                    nearest_class = false;
                    flagged = true;
                }
                if (kind == STORE && other_flags & order::READS_MEMORY != 0)
                    || (kind == LOAD && other_flags & order::WRITES_MEMORY != 0)
                {
                    flagged = true;
                }
                if !ordered && !flagged {
                    continue;
                }
                let alias = flagged || self.memory_alias(i, j, kind, other, &ea);
                if std::env::var_os("VCL_ALIAS_TRACE").is_some() && kind == STORE {
                    crate::trace!(
                        "alias {i} {:?} vs {j} {:?} -> {alias} timid {}",
                        ea,
                        self.address(live[j]),
                        self.timid
                    );
                }
                if alias {
                    if !instructions[i].forced_predecessors.contains(&j) {
                        instructions[i].forced_predecessors.push(j);
                    }
                    if !instructions[j].forced_successors.contains(&i) {
                        instructions[j].forced_successors.push(i);
                    }
                }
            }
        }
    }

    /// Whether the memory instruction `i` (address `ea`, kind `kind`) may
    /// alias the earlier `j` (kind `other`).
    fn memory_alias(&self, i: usize, j: usize, kind: u8, other: u8, ea: &Option<Address>) -> bool {
        let live = self.live;
        ea.is_none()
            || (self.ram_bits(live[i]) & self.ram_bits(live[j]) != 0
                && (self.timid
                    || (self.shares_tag(live[i], live[j]) && !(kind == STORE && other == STORE))
                    || self.same_address(i, j, ea)
                    || self.same_location(i, j)))
    }

    /// With the `alias` fix, whether `i` and `j` provably address the same
    /// location (`memory_alias`).
    fn same_location(&self, i: usize, j: usize) -> bool {
        if !self.program.fixes.contains(Fix::Alias) {
            return false;
        }
        let at = |index: usize| {
            memory_alias::location(self.program, self.region, &self.reads_by, self.live[index])
        };
        at(i).is_some_and(|location| at(j) == Some(location))
    }

    /// Whether the instructions `i` and `j` address the same location: same
    /// kind, base value and offset.
    fn same_address(&self, i: usize, j: usize, ea: &Option<Address>) -> bool {
        let live = self.live;
        let other_ea = self.address(live[j]);
        match (ea, &other_ea) {
            (Some((a, x, c)), Some((b, y, d))) => {
                let same_base = match (a, b) {
                    (None, None) => true,
                    (Some(a), Some(b)) => {
                        a == b && {
                            let defs = |id: usize, r: usize| {
                                self.reads_by
                                    .get(&(id, r, 0))
                                    .map(|read| read.definitions.clone())
                            };
                            defs(live[i], *a) == defs(live[j], *b)
                        }
                    }
                    _ => false,
                };
                c == d && same_base && x == y
            }
            _ => false,
        }
    }

    /// Regref through bits: physical components live into and out of the
    /// table without a local write.
    fn add_through_bits(&self, map: &mut RegisterMap, instructions: &[wrangle::Instruction]) {
        let (program, region, analysis) = (self.program, self.region, self.analysis);
        for (key, &index) in &map.keys {
            let WrangleKey::Physical(vector, base) = *key else {
                continue;
            };
            let family = if vector == 1 {
                Family::Vector
            } else {
                Family::Integer
            };
            let mut written = 0u8;
            for insn in instructions {
                for operand in &insn.operands {
                    if operand.register == index && operand.direction == WDirection::Write {
                        written |= operand.mask;
                    }
                }
            }
            let physical_of = |access: &Access| {
                let named = program.registers.named(access.register);
                named.family == family
                    && named
                        .physical
                        .or_else(|| region.forced.get(&access.register).copied())
                        == Some(base)
            };
            let live_in: u8 = analysis.live_in[self.first]
                .iter()
                .filter(|a| physical_of(a))
                .fold(0, |m, a| m | a.mask);
            let live_out: u8 = analysis.live_out[self.block_id]
                .iter()
                .filter(|a| physical_of(a))
                .fold(0, |m, a| m | a.mask);
            map.registers[index].through = live_in & live_out & !written;
        }
    }

    /// Traces the assembled problem (`VCL_WRANGLE_PROBLEM`).
    fn trace_problem(&self, problem: &wrangle::Problem, keys: &BTreeMap<WrangleKey, usize>) {
        crate::trace!("problem block {}", self.block_id);
        for (index, register) in problem.registers.iter().enumerate() {
            crate::trace!(
                "  reg {index} {:?} through {} phys {:?}",
                keys.iter().find(|&(_, &v)| v == index).map(|(k, _)| k),
                register.through,
                register.physical
            );
        }
        for (index, insn) in problem.instructions.iter().enumerate() {
            crate::trace!(
                "insn {index} {} pipes {} min {} end {} forced {:?}",
                self.region.instructions[self.live[index]]
                    .instruction
                    .mnemonic,
                insn.pipes,
                insn.minimum_position,
                insn.end_distance,
                insn.forced_predecessors
            );
            for operand in &insn.operands {
                crate::trace!(
                    "    {:?} {:?}",
                    operand,
                    keys.iter()
                        .find(|&(_, &v)| v == operand.register)
                        .map(|(k, _)| k)
                );
            }
        }
    }

    /// An affine consumer after its setter reads the new value through a
    /// mode-2 lock the port leaves out; the statistics still count it
    /// (vu_2250509715: `iaddiu vi11` has fan-out 4 from the stores after it).
    fn add_affine_statistics(&self, problem: &mut wrangle::Problem, affine: &[AffineUse]) {
        for use_ in affine.iter().filter(|a| !a.before) {
            let register =
                self.region.instructions[self.live[use_.consumer]].operands[use_.operand].unwrap();
            let Some(o) = self.operand_lists[use_.setter]
                .iter()
                .position(|&(r, d, ..)| r == register && d == WDirection::Write)
                .and_then(|o| self.mapped_index[use_.setter][o])
            else {
                continue;
            };
            problem.instructions[use_.consumer]
                .stat_links
                .push((use_.setter, o));
        }
    }

    /// Physical registers live through the table without a reference keep
    /// their through components for the whole extent; referenced pinned
    /// registers come from their locks.
    fn through_pools(&self, keys: &BTreeMap<WrangleKey, usize>) -> wrangle::Pools {
        let (program, region, analysis) = (self.program, self.region, self.analysis);
        let mut pools = wrangle::Pools {
            vf_allowed: program.allowed_vf.clone(),
            vi_allowed: program.allowed_vi.clone(),
            ..wrangle::Pools::default()
        };
        for access in &analysis.live_in[self.first] {
            let through = analysis.live_out[self.block_id]
                .iter()
                .filter(|out| out.register == access.register)
                .fold(0, |mask, out| mask | out.mask)
                & access.mask;
            if through == 0 {
                continue;
            }
            let register = program.registers.named(access.register);
            let Some(base) = register
                .physical
                .or_else(|| region.forced.get(&access.register).copied())
            else {
                continue;
            };
            let (family_key, list) = match register.family {
                Family::Vector => (1, &mut pools.vf_through),
                Family::Integer => (0, &mut pools.vi_through),
                Family::Hardware => continue,
            };
            if base != 0 && !keys.contains_key(&Key::Physical(family_key, base)) {
                match list.iter_mut().find(|(physical, _)| *physical == base) {
                    Some((_, mask)) => *mask |= through,
                    None => list.push((base, through)),
                }
            }
        }
        pools
    }
}

const LOAD: u8 = subtype::memory::LOAD;
const STORE: u8 = subtype::memory::STORE;

/// Memory tags as resources in a loop table, as vcl 1.4beta7 binds them: a
/// store writes its tags (latency 1) and a load reads them, over the memory
/// fields it touches. In a software-pipelined loop the forced memory-order
/// links are gone (the loop runs on a cloned table that drops them), but these
/// operands remain, so a load stays after the previous iteration's store with
/// the same tag, and a store after the loads before it. The tag registers
/// are pool 0: no move can rename them, so an overlap the period cannot fit is
/// a hard conflict and the search moves on to a longer period
/// (`memory-tag-loop`, period 9 against a bound of 6).
pub(super) fn add_memory_tag_resources(
    table: &mut WrangledTable,
    program: &Source,
    region: &Region,
) {
    let mut registers: BTreeMap<String, usize> = BTreeMap::new();
    // Latest store of each tag register so far: (instruction, operand).
    let mut last_store: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for (index, &id) in table.live.iter().enumerate() {
        let instruction = &region.instructions[id].instruction;
        if !instruction.metadata.is_memory() {
            continue;
        }
        let Some(memory) = instruction
            .operands
            .iter()
            .find(|operand| class::is_address(operand.class))
        else {
            continue;
        };
        let store = instruction.metadata.is_store();
        let mask = if instruction.metadata.mask_mode == MaskMode::Unmasked {
            0xf
        } else {
            instruction.mask.0
        };
        for tag in &memory.tags {
            let register = *registers.entry(tag.clone()).or_insert_with(|| {
                table.problem.registers.push(wrangle::Register {
                    parts: 4,
                    ..wrangle::Register::default()
                });
                table.keys.push(WrangleKey::Hardware(format!(":{tag}")));
                table.problem.registers.len() - 1
            });
            let Some(&program_register) = program
                .registers
                .ids
                .get(&(Family::Hardware, format!(":{tag}").to_ascii_lowercase()))
            else {
                continue;
            };
            let mut sources = [None; 4];
            if !store {
                let source = last_store.get(&register).map_or(
                    WSource::External,
                    |&(instruction, operand)| WSource::Local {
                        instruction,
                        operand,
                    },
                );
                for (part, slot) in sources.iter_mut().enumerate() {
                    if mask & (1 << part) != 0 {
                        *slot = Some(source);
                    }
                }
            }
            let operands = &mut table.problem.instructions[index].operands;
            operands.push(wrangle::Operand {
                register,
                direction: if store {
                    WDirection::Write
                } else {
                    WDirection::Read
                },
                mask,
                latency: if store { 1 } else { -1 },
                blocking: true,
                input_offset: 0,
                sources,
                // The value written reaches the next iteration's loads.
                escapes: if store { mask } else { 0 },
                observed: store,
            });
            table.operand_registers[index].push(program_register);
            if store {
                last_store.insert(register, (index, operands.len() - 1));
            }
        }
    }
    if !registers.is_empty() {
        table.statics = wrangle::statics(&table.problem);
    }
}

/// With the `alias` fix, the same-location groups of a loop body as
/// resources of its table, like memory tags (`add_memory_tag_resources`):
/// one register per group, which each access reads and each store also
/// writes (latency 1). A load stays after the stores before it and a store
/// after every access before it, in the body and, through the value the
/// last store leaves, from one iteration to the next.
pub(super) fn add_alias_resources(
    table: &mut WrangledTable,
    region: &Region,
    groups: &[memory_alias::LoopGroup],
) {
    let program_register = memory_alias::NO_PROGRAM_REGISTER;
    for (number, group) in groups.iter().enumerate() {
        table.problem.registers.push(wrangle::Register {
            parts: 4,
            ..wrangle::Register::default()
        });
        table.keys.push(WrangleKey::Hardware(format!(
            "{}{number}",
            memory_alias::LOOP_RESOURCE
        )));
        let register = table.problem.registers.len() - 1;
        // The latest store of each field so far: (instruction, operand).
        let mut last_store: [Option<(usize, usize)>; 4] = [None; 4];
        let final_store = group.members.iter().rev().copied().find(|&index| {
            region.instructions[table.live[index]]
                .instruction
                .metadata
                .is_store()
        });
        for &index in &group.members {
            let located = &region.instructions[table.live[index]];
            let store = located.instruction.metadata.is_store();
            let mask = memory_alias::fields(located);
            let mut sources = [None; 4];
            for (part, slot) in sources.iter_mut().enumerate() {
                if mask & (1 << part) != 0 {
                    *slot = Some(last_store[part].map_or(
                        WSource::External,
                        |(instruction, operand)| WSource::Local {
                            instruction,
                            operand,
                        },
                    ));
                }
            }
            let operands = &mut table.problem.instructions[index].operands;
            operands.push(wrangle::Operand {
                register,
                direction: WDirection::Read,
                mask,
                latency: -1,
                blocking: true,
                input_offset: 0,
                sources,
                escapes: 0,
                observed: false,
            });
            table.operand_registers[index].push(program_register);
            if store {
                operands.push(wrangle::Operand {
                    register,
                    direction: WDirection::Write,
                    mask,
                    latency: 1,
                    blocking: true,
                    input_offset: 0,
                    sources: [None; 4],
                    // Only the last store's value reaches the next iteration.
                    escapes: if Some(index) == final_store { mask } else { 0 },
                    observed: true,
                });
                table.operand_registers[index].push(program_register);
                let operand = operands.len() - 1;
                for (part, slot) in last_store.iter_mut().enumerate() {
                    if mask & (1 << part) != 0 {
                        *slot = Some((index, operand));
                    }
                }
            }
        }
    }
    if !groups.is_empty() {
        table.statics = wrangle::statics(&table.problem);
    }
}
