//! Building the wrangler's work table for a span of a region's instructions.

use super::*;

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
        issue_mask: 2,
        ..*nop
    })
}

pub(super) fn issue_pipes(located: &Located) -> u8 {
    let metadata = located.instruction.metadata;
    if metadata.index == 153 && located.instruction.mode == ParseMode::Vcl {
        3
    } else {
        metadata.issue_mask as u8
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
    use crate::scheduler::wrangle::{self, Direction as WDirection, Source as WSource};
    use WrangleKey as Key;
    let local: BTreeMap<_, _> = live.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let mut keys: BTreeMap<Key, usize> = BTreeMap::new();
    let mut registers: Vec<wrangle::Register> = Vec::new();
    let mut register_of = |id: usize| -> Option<usize> {
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
        Some(*keys.entry(key).or_insert_with(|| {
            registers.push(info);
            registers.len() - 1
        }))
    };
    let mut reads_by: BTreeMap<(usize, usize, u8), &flow::Read> = BTreeMap::new();
    for read in &analysis.reads {
        reads_by.insert((read.operation, read.register, read.component), read);
    }
    // A fixed table's lower half reads with its upper partner (flow moves
    // them to the pair's first operation).
    for &id in live {
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
    let hardware_id = |id: usize| -> Option<u8> {
        let register = program.registers.named(id);
        (register.family == Family::Hardware)
            .then(|| hardware_register(&register.name).map(|h| h.0))
            .flatten()
    };
    // Affine induction annotation: a VI pointer with one
    // affine setter, live into and out of the table, frees the base reads of
    // its LQ/SQ/ILW/ISW users from lock constraints.
    let mut affine: Vec<AffineUse> = Vec::new();
    {
        let local: BTreeMap<_, _> = live.iter().enumerate().map(|(i, &id)| (id, i)).collect();
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
            let [setter] = setters[..] else { continue };
            let located = &region.instructions[live[setter]];
            let index = located.instruction.metadata.index;
            let (add, input, output, stride) = match index {
                109 | 110 => (true, 1, 0, None),
                112 => (false, 1, 0, None),
                158 | 163 => (true, 1, 1, Some("1".to_string())),
                159 | 164 => (false, 1, 1, Some("1".to_string())),
                _ => continue,
            };
            if located.operands.get(input).copied().flatten() != Some(register)
                || located.operands.get(output).copied().flatten() != Some(register)
            {
                continue;
            }
            let stride = match stride {
                Some(stride) => stride,
                None => match located
                    .instruction
                    .operands
                    .get(2)
                    .and_then(|o| o.immediate.clone())
                {
                    Some(stride) => stride,
                    None => continue,
                },
            };
            let external_input = live.iter().any(|&id| {
                reads_by.get(&(id, register, 0)).is_some_and(|read| {
                    read.definitions.iter().any(|d| match d {
                        Definition::Operation(w) => local.get(w).is_none_or(|&w| w >= local[&id]),
                        _ => true,
                    })
                })
            });
            let external_output = analysis.live_out[block_id]
                .iter()
                .any(|a| a.register == register && a.mask & 1 != 0);
            if !external_input || !external_output {
                continue;
            }
            for (consumer, &id) in live.iter().enumerate() {
                let user = &region.instructions[id];
                if !matches!(user.instruction.metadata.index, 155 | 156 | 160 | 161) {
                    continue;
                }
                let Some(ea) = user
                    .instruction
                    .operands
                    .iter()
                    .position(|o| (28..=33).contains(&o.class))
                else {
                    continue;
                };
                if ea != 1 || user.operands.get(ea).copied().flatten() != Some(register) {
                    continue;
                }
                let Some(read) = reads_by.get(&(id, register, 0)) else {
                    continue;
                };
                let before = read.definitions.iter().all(|d| match d {
                    Definition::Operation(w) => local.get(w).is_none_or(|&w| w >= consumer),
                    _ => true,
                });
                let after = read.definitions.len() == 1
                    && read
                        .definitions
                        .contains(&Definition::Operation(live[setter]));
                if !(before || after) || !annotated.insert(consumer) {
                    continue;
                }
                affine.push(AffineUse {
                    consumer,
                    setter,
                    before,
                    add,
                    stride: stride.clone(),
                    operand: ea,
                });
            }
        }
    }
    let affine_bases: BTreeMap<usize, usize> = affine
        .iter()
        .map(|a| {
            (
                a.consumer,
                region.instructions[live[a.consumer]].operands[a.operand].unwrap(),
            )
        })
        .collect();
    // Operand lists in descriptor order, with effective masks from effects.
    type OperandList = Vec<(usize, WDirection, u8, i8, bool)>;
    let mut operand_lists: Vec<OperandList> = Vec::new();
    for (operands_index, &id) in live.iter().enumerate() {
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
                && (28..=33).contains(&operand.class)
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
        operand_lists.push(operands);
    }
    // Operand indices after dropping pool-zero operands.
    let mut mapped_index: Vec<Vec<Option<usize>>> = Vec::new();
    for operands in &operand_lists {
        let mut next = 0;
        let mut indices = Vec::new();
        for &(register, ..) in operands {
            if register_of(register).is_some() {
                indices.push(Some(next));
                next += 1;
            } else {
                indices.push(None);
            }
        }
        mapped_index.push(indices);
    }
    let mut instructions = Vec::new();
    let mut operand_registers = Vec::new();
    for (index, &id) in live.iter().enumerate() {
        let located = &region.instructions[id];
        let metadata = located.instruction.metadata;
        let conditional = metadata.is_branch() && metadata.subtype & 4 != 0;
        let mut operands = Vec::new();
        let mut registers_here = Vec::new();
        for &(register, direction, mask, latency, blocking) in &operand_lists[index] {
            let Some(mapped) = register_of(register) else {
                continue;
            };
            registers_here.push(register);
            let mut sources = [None; 4];
            let mut input_offset = 0;
            let mut escapes = 0;
            for part in 0..4u8 {
                if mask & (1 << part) == 0 {
                    continue;
                }
                match direction {
                    WDirection::Read => {
                        let definitions = reads_by
                            .get(&(id, register, part))
                            .map(|read| &read.definitions);
                        let mut source = WSource::External;
                        if let Some(definitions) = definitions {
                            if definitions.len() == 1
                                && let Some(Definition::Operation(writer)) =
                                    definitions.iter().next()
                                && let Some(&w) = local.get(writer)
                                && w < index
                                && let Some(o) = operand_lists[w]
                                    .iter()
                                    .position(|&(r, d, m, _, _)| {
                                        r == register
                                            && d == WDirection::Write
                                            && m & (1 << part) != 0
                                    })
                                    .and_then(|o| mapped_index[w][o])
                            {
                                source = WSource::Local {
                                    instruction: w,
                                    operand: o,
                                };
                            }
                            if conditional
                                && program.registers.named(register).family == Family::Integer
                            {
                                for definition in definitions {
                                    // An `--enter` input's definer is no
                                    // load and reads no flags.
                                    if *definition == Definition::Input {
                                        input_offset = 1;
                                    }
                                    let Definition::Operation(writer) = definition else {
                                        continue;
                                    };
                                    let setter = &region.instructions[*writer];
                                    let first_operand = writes_first_operand(setter, register);
                                    let flagged = setter.effects.reads.iter().any(|r| {
                                        hardware_id(r.register)
                                            .is_some_and(|g| (0x36..=0x3b).contains(&g))
                                    });
                                    if (!setter.instruction.metadata.is_memory() || !first_operand)
                                        && !flagged
                                    {
                                        input_offset = 1;
                                    }
                                }
                            }
                        }
                        sources[usize::from(part)] = Some(source);
                    }
                    WDirection::Write => {
                        let later = live[index + 1..].iter().any(|&other| {
                            region.instructions[other]
                                .effects
                                .writes
                                .iter()
                                .any(|w| w.register == register && w.mask & (1 << part) != 0)
                        });
                        if !later
                            && analysis.live_out[block_id]
                                .iter()
                                .any(|a| a.register == register && a.mask & (1 << part) != 0)
                        {
                            escapes |= 1 << part;
                        }
                    }
                }
            }
            let observed = direction == WDirection::Write
                && (0..4u8).any(|part| {
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
                });
            operands.push(wrangle::Operand {
                register: mapped,
                direction,
                mask,
                latency,
                blocking,
                input_offset,
                sources,
                escapes,
                observed,
            });
        }
        operand_registers.push(registers_here);
        instructions.push(wrangle::Instruction {
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
                        register: register_of(register),
                        mask: located.instruction.mask.0,
                        latency: metadata.operands.first().map_or(1, |o| o.latency),
                    })
                }),
            operands,
        });
    }
    // Memory forced dependencies.
    let address = |id: usize| -> Option<(Option<usize>, String, u8)> {
        let located = &region.instructions[id];
        let index = located
            .instruction
            .operands
            .iter()
            .position(|o| (28..=33).contains(&o.class))?;
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
        // not separate `disp(reg)` from `disp(reg)mask` (class 29 vs 31): an
        // `sq ...,3(vi04)` aliases `ilw.w ...,3(vi04)` (vu_1518195603). Only
        // the post-increment and pre-decrement forms differ.
        let kind = match operand.class {
            32 => 1,
            33 => 2,
            _ => 0,
        };
        Some((base, offset, kind))
    };
    // Memory tags (`sq v,0(p):frame`) are resources the store writes and the
    // load reads over the fields it touches, so a shared tag orders a load
    // against a store but not two stores (observed from vcl 1.4beta7:
    // `memory-tag-shared` keeps a store and a later load in order, and
    // `memory-tag-store-store` lets two stores swap, as do
    // `memory-tag-none` and `memory-tag-disjoint`).
    let tags = |id: usize| -> &[String] {
        region.instructions[id]
            .instruction
            .operands
            .iter()
            .find(|operand| (28..=33).contains(&operand.class))
            .map_or(&[], |operand| operand.tags.as_slice())
    };
    let shares_tag = |a: usize, b: usize| tags(a).iter().any(|tag| tags(b).contains(tag));
    let ram_bits = |id: usize| -> u8 {
        let instruction = &region.instructions[id].instruction;
        if instruction.metadata.mask_mode == 0 {
            0xf
        } else {
            instruction.mask.0
        }
    };
    // Order classes (descriptor flags): 0x10 and
    // 0x20 compare like a store and a load against earlier loads and stores,
    // 0x40 links to the nearest earlier instruction of that class. Only
    // XGKICK (0x60) carries any: it follows every earlier store (it has no
    // address to compare) and the previous XGKICK, and a later store
    // follows it.
    let order_flags = |id: usize| region.instructions[id].instruction.metadata.flags;
    for i in 0..live.len() {
        let kind = instructions[i].memory;
        let flags = order_flags(live[i]);
        let after_loads = kind == 2 || flags & 0x10 != 0;
        let after_stores = kind != 0 || flags & 0x20 != 0;
        let mut nearest_class = flags & 0x40 != 0;
        if !after_loads && !after_stores && !nearest_class {
            continue;
        }
        let ea = address(live[i]);
        for j in (0..i).rev() {
            let other = instructions[j].memory;
            let ordered = (after_stores && other == 2) || (after_loads && other == 1);
            let other_flags = order_flags(live[j]);
            let mut flagged = false;
            if nearest_class && other_flags & 0x40 != 0 {
                nearest_class = false;
                flagged = true;
            }
            if (kind == 2 && other_flags & 0x20 != 0) || (kind == 1 && other_flags & 0x10 != 0) {
                flagged = true;
            }
            if !ordered && !flagged {
                continue;
            }
            let alias = flagged
                || ea.is_none()
                || (ram_bits(live[i]) & ram_bits(live[j]) != 0
                    && (timid
                        || (shares_tag(live[i], live[j]) && !(kind == 2 && other == 2))
                        || {
                            let other_ea = address(live[j]);
                            match (&ea, &other_ea) {
                                (Some((a, x, c)), Some((b, y, d))) => {
                                    let same_base = match (a, b) {
                                        (None, None) => true,
                                        (Some(a), Some(b)) => {
                                            a == b && {
                                                let defs = |id: usize, r: usize| {
                                                    reads_by
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
                        }));
            if std::env::var_os("VCL_ALIAS_TRACE").is_some() && kind == 2 {
                eprintln!(
                    "alias {i} {:?} vs {j} {:?} -> {alias} timid {timid}",
                    ea,
                    address(live[j])
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
    // Regref through bits: physical components live into and out of the
    // table without a local write.
    for (key, &index) in &keys {
        let WrangleKey::Physical(vector, base) = *key else {
            continue;
        };
        let family = if vector == 1 {
            Family::Vector
        } else {
            Family::Integer
        };
        let mut written = 0u8;
        for insn in &instructions {
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
        let live_in: u8 = analysis.live_in[first]
            .iter()
            .filter(|a| physical_of(a))
            .fold(0, |m, a| m | a.mask);
        let live_out: u8 = analysis.live_out[block_id]
            .iter()
            .filter(|a| physical_of(a))
            .fold(0, |m, a| m | a.mask);
        registers[index].through = live_in & live_out & !written;
    }
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
        eprintln!("problem block {block_id}");
        for (index, register) in problem.registers.iter().enumerate() {
            eprintln!(
                "  reg {index} {:?} through {} phys {:?}",
                keys.iter().find(|&(_, &v)| v == index).map(|(k, _)| k),
                register.through,
                register.physical
            );
        }
        for (index, insn) in problem.instructions.iter().enumerate() {
            eprintln!(
                "insn {index} {} pipes {} min {} end {} forced {:?}",
                region.instructions[live[index]].instruction.mnemonic,
                insn.pipes,
                insn.minimum_position,
                insn.end_distance,
                insn.forced_predecessors
            );
            for operand in &insn.operands {
                eprintln!(
                    "    {:?} {:?}",
                    operand,
                    keys.iter()
                        .find(|&(_, &v)| v == operand.register)
                        .map(|(k, _)| k)
                );
            }
        }
    }
    // An affine consumer after its setter reads the new value through a
    // mode-2 lock the port leaves out; the statistics still count it
    // (vu_2250509715: `iaddiu vi11` has fan-out 4 from the stores after it).
    for use_ in affine.iter().filter(|a| !a.before) {
        let register = region.instructions[live[use_.consumer]].operands[use_.operand].unwrap();
        let Some(o) = operand_lists[use_.setter]
            .iter()
            .position(|&(r, d, ..)| r == register && d == WDirection::Write)
            .and_then(|o| mapped_index[use_.setter][o])
        else {
            continue;
        };
        problem.instructions[use_.consumer]
            .stat_links
            .push((use_.setter, o));
    }
    let statics = wrangle::statics(&problem);
    let (lower_bound, horizon) = wrangle::bounds(&problem, &statics);
    problem.horizon = horizon;
    // Physical registers live through the table without a reference keep
    // their through components for the whole extent; referenced pinned
    // registers come from their locks.
    let mut pools = wrangle::Pools {
        vf_allowed: program.allowed_vf.clone(),
        vi_allowed: program.allowed_vi.clone(),
        ..wrangle::Pools::default()
    };
    for access in &analysis.live_in[first] {
        let through = analysis.live_out[block_id]
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
    })
}

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
    use crate::scheduler::wrangle::{self, Direction as WDirection, Source as WSource};
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
            .find(|operand| (28..=33).contains(&operand.class))
        else {
            continue;
        };
        let store = instruction.metadata.subtype == 2;
        let mask = if instruction.metadata.mask_mode == 0 {
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
