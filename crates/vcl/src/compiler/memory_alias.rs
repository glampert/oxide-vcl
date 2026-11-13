//! The `alias` fix (docs/VCL_BUGS.md, F2): loads and stores whose addresses
//! are provably the same location.
//!
//! vcl 1.4beta7 orders two memory accesses with intersecting fields only
//! under `-M`, with a shared tag, or when their address operands match: the
//! same base register with the same reaching definitions, and the same
//! offset. Two registers that hold the same address are assumed not to
//! alias, so a load can move above a store to the same qword (observed from
//! vcl 1.4beta7: `openvcl__repros_store_load_alias`,
//! `openvcl__repros_store_store_alias`, `checker-mem-copied-base`). A
//! software-pipelined loop keeps only the order of tagged accesses: one qword
//! is stored and loaded out of order even through one base register, with
//! or without `-M` (`mem-loop-same-base`, `mem-loop-same-base-timid`,
//! `mem-loop-copied-base`, `mem-loop-accumulator`, `mem-loop-store-store`,
//! `mem-loop-carried-raw`).
//!
//! With the fix, each address is also written as a root plus a constant,
//! following the base register back through the steps that set it:
//! IADDIU, ISUBIU and IADDI by a number, IADD and ISUB with VI00, VI moves,
//! and the steps of LQI, SQI, LQD and SQD. The root is VI00 (an absolute
//! address), a loop's induction register (`Resolver::induction`), or a
//! register at its reaching definitions, where the steps stop.
//! Two accesses with the same root and constant are the same location and
//! keep their order. In a pipelined loop, accesses from one induction
//! register a whole number of strides apart meet in different iterations;
//! the pairs that the schedule runs out of order (`broken_pairs`) become
//! resources of the loop table (`add_alias_resources`) and the loop is
//! searched again. Anything else keeps vcl's assumption, as before.

use super::*;
use crate::analysis::flow::Read;

/// How many steps back an address is followed.
const DEPTH: usize = 16;

/// Where an address starts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Root {
    /// VI00: the constant is the address.
    Absolute,
    /// A register's value at the definitions reaching a read of it.
    Value(usize, BTreeSet<Definition>),
    /// An induction register of the self-loop `block`: `init` plus `stride`
    /// for each pass through the block since it was entered.
    Induction {
        init: Box<Root>,
        stride: i64,
        block: usize,
    },
}

/// An address: a root plus a constant, in qwords, modulo the 16 bits of a
/// VI register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Location {
    root: Root,
    offset: i64,
}

impl Location {
    fn plus(self, step: i64) -> Self {
        Location {
            root: self.root,
            offset: (self.offset + step).rem_euclid(0x1_0000),
        }
    }
}

/// The reads of the region by (operation, register, component).
pub(super) type Reads<'a> = BTreeMap<(usize, usize, u8), &'a Read>;

/// Follows addresses back through the region's data flow. With the
/// region's flow graph it also recognizes induction registers.
pub(super) struct Resolver<'a> {
    pub(super) program: &'a Source,
    pub(super) region: &'a Region,
    pub(super) reads: &'a Reads<'a>,
    pub(super) graph: Option<&'a flow::Graph>,
}

fn number(text: &str) -> Option<i64> {
    crate::parser::expression::evaluate(text)
        .ok()?
        .number()
        .map(|value| i64::from(value.to_i32()))
}

impl Resolver<'_> {
    /// The location a memory instruction addresses, or `None` when its
    /// offset is not a number.
    pub(super) fn location(&self, id: usize) -> Option<Location> {
        let located = &self.region.instructions[id];
        let index = located
            .instruction
            .operands
            .iter()
            .position(|operand| class::is_address(operand.class))?;
        let operand = &located.instruction.operands[index];
        let offset = match operand.immediate.as_deref() {
            Some(text) => number(text)?,
            None => 0,
        };
        // `(--vi)` addresses the decremented value; `(vi++)` the value before.
        let step = if operand.class == class::ADDRESS_DECREMENT {
            -1
        } else {
            0
        };
        let at = match located.operands[index] {
            Some(register) => self.value(register, id, &mut Vec::new())?,
            None => Location {
                root: Root::Absolute,
                offset: 0,
            },
        };
        Some(at.plus(offset + step))
    }

    fn is_zero(&self, register: usize) -> bool {
        self.program
            .registers
            .named(register)
            .physical
            .or_else(|| self.region.forced.get(&register).copied())
            == Some(0)
    }

    /// The value of VI `register` as `operation` reads it, or `None` when
    /// the read is not known.
    fn value(&self, register: usize, operation: usize, path: &mut Vec<usize>) -> Option<Location> {
        if self.is_zero(register) {
            return Some(Location {
                root: Root::Absolute,
                offset: 0,
            });
        }
        let read = self.reads.get(&(operation, register, 0))?;
        let opaque = Location {
            root: Root::Value(register, read.definitions.clone()),
            offset: 0,
        };
        if path.len() >= DEPTH {
            return Some(opaque);
        }
        if let Some(induction) = self.induction(register, operation, read, path) {
            return Some(induction);
        }
        // Every reaching definition must give the same location.
        let mut agreed: Option<Location> = None;
        for definition in &read.definitions {
            let Definition::Operation(setter) = *definition else {
                return Some(opaque);
            };
            match (self.written_once(register, setter, path), &agreed) {
                (None, _) => return Some(opaque),
                (Some(location), Some(previous)) if location != *previous => return Some(opaque),
                (Some(location), _) => agreed = Some(location),
            }
        }
        Some(agreed.unwrap_or(opaque))
    }

    /// `written`, unless `setter` is already being followed.
    fn written_once(
        &self,
        register: usize,
        setter: usize,
        path: &mut Vec<usize>,
    ) -> Option<Location> {
        if path.contains(&setter) {
            return None;
        }
        path.push(setter);
        let set = self.written(register, setter, path);
        path.pop();
        set
    }

    /// The value `setter` writes to VI `register`, when it is a step from
    /// another value.
    fn written(&self, register: usize, setter: usize, path: &mut Vec<usize>) -> Option<Location> {
        let located = &self.region.instructions[setter];
        let operand = |index: usize| located.operands.get(index).copied().flatten();
        let zero = |index: usize| operand(index).is_some_and(|r| self.is_zero(r));
        let mut from = |source: usize| self.value(source, setter, path);
        match located.instruction.metadata.index {
            id::IADDIU | id::ISUBIU | id::IADDI if operand(0) == Some(register) => {
                let step = self.step(setter)?;
                Some(from(operand(1)?)?.plus(step))
            }
            id::IADD | id::ISUB if operand(0) == Some(register) && zero(2) => from(operand(1)?),
            id::IADD if operand(0) == Some(register) && zero(1) => from(operand(2)?),
            id::IMOVE | id::LIMOVE if operand(0) == Some(register) => from(operand(1)?),
            id::LQI | id::SQI | id::LQD | id::SQD
                if self.address_register(setter) == Some(register) =>
            {
                Some(from(register)?.plus(self.step(setter)?))
            }
            _ => None,
        }
    }

    fn address_register(&self, operation: usize) -> Option<usize> {
        let located = &self.region.instructions[operation];
        let index = located
            .instruction
            .operands
            .iter()
            .position(|operand| class::is_address(operand.class))?;
        located.operands[index]
    }

    /// What a step adds: the immediate of IADDIU, ISUBIU (negated) or
    /// IADDI, or one for LQI and SQI and minus one for LQD and SQD.
    fn step(&self, setter: usize) -> Option<i64> {
        let instruction = &self.region.instructions[setter].instruction;
        match instruction.metadata.index {
            id::IADDIU | id::IADDI => number(instruction.operands[2].immediate.as_deref()?),
            id::ISUBIU => number(instruction.operands[2].immediate.as_deref()?).map(|step| -step),
            id::LQI | id::SQI => Some(1),
            id::LQD | id::SQD => Some(-1),
            _ => None,
        }
    }

    /// The value of VI `register` at `operation` as an induction: the
    /// operation is in a block that branches to itself, the one definition
    /// from that block steps the register by itself (`iaddiu r, r, s` or a
    /// LQI, SQI, LQD or SQD on it) at or after the operation, and the other
    /// definitions, from outside the block, give the same value. That step
    /// must reach the block's top only through its own back edge, so that
    /// each entry into the block starts from that value.
    fn induction(
        &self,
        register: usize,
        operation: usize,
        read: &Read,
        path: &mut Vec<usize>,
    ) -> Option<Location> {
        let graph = self.graph?;
        let block = self
            .region
            .blocks
            .iter()
            .position(|block| block.operations.contains(&operation))?;
        let operations = &self.region.blocks[block].operations;
        let position = |id: usize| operations.iter().position(|&other| other == id);
        let here = position(operation)?;
        let mut step = None;
        let mut entries = Vec::new();
        for &definition in &read.definitions {
            match definition {
                Definition::Operation(setter) if position(setter).is_some() => {
                    if position(setter)? < here || step.replace(setter).is_some() {
                        return None;
                    }
                }
                Definition::Uninitialized => return None,
                other => entries.push(other),
            }
        }
        let step = step?;
        if entries.is_empty() || !graph.blocks.get(block)?.successors.contains(&block) {
            return None;
        }
        let self_step = match self.region.instructions[step].instruction.metadata.index {
            id::IADDIU | id::ISUBIU | id::IADDI => {
                let operands = &self.region.instructions[step].operands;
                operands.first().copied().flatten() == Some(register)
                    && operands.get(1).copied().flatten() == Some(register)
            }
            id::LQI | id::SQI | id::LQD | id::SQD => self.address_register(step) == Some(register),
            _ => false,
        };
        if !self_step || self.reads.get(&(step, register, 0))?.definitions != read.definitions {
            return None;
        }
        if self.reenters(graph, block, register) {
            return None;
        }
        let stride = self.step(step)?.rem_euclid(0x1_0000);
        let mut init: Option<Location> = None;
        for entry in entries {
            let location = match entry {
                Definition::Input => Location {
                    root: Root::Value(register, BTreeSet::from([Definition::Input])),
                    offset: 0,
                },
                Definition::Operation(setter) => self.written_once(register, setter, path)?,
                Definition::Uninitialized => return None,
            };
            if init.as_ref().is_some_and(|previous| *previous != location) {
                return None;
            }
            init = Some(location);
        }
        let init = init?;
        if stride == 0 {
            return Some(init);
        }
        Some(Location {
            root: Root::Induction {
                init: Box::new(init.root),
                stride,
                block,
            },
            offset: init.offset,
        })
    }

    /// Whether a value of `register` that leaves `block` can come back to
    /// its top other than through its own back edge: a path from the block
    /// to a predecessor of it, through blocks that do not write the
    /// register.
    fn reenters(&self, graph: &flow::Graph, block: usize, register: usize) -> bool {
        let writes = |id: usize| {
            self.region.blocks.get(id).is_some_and(|other| {
                other.operations.iter().any(|&operation| {
                    self.region.instructions[operation]
                        .effects
                        .writes
                        .iter()
                        .any(|write| write.register == register)
                })
            })
        };
        let mut seen = BTreeSet::new();
        let mut queue: Vec<usize> = graph.blocks[block]
            .successors
            .iter()
            .copied()
            .filter(|&next| next != block)
            .collect();
        while let Some(id) = queue.pop() {
            if !seen.insert(id) || writes(id) {
                continue;
            }
            let Some(other) = graph.blocks.get(id) else {
                continue;
            };
            if other.successors.contains(&block) {
                return true;
            }
            queue.extend(other.successors.iter().copied());
        }
        false
    }
}

/// The table key prefix of the fix's loop resources: no source tag can be
/// spelled with parentheses.
pub(super) const LOOP_RESOURCE: &str = ":(alias)";

/// The program register of the fix's loop resources in a table's
/// `operand_registers`: they have none. They are pool-0 registers, so no
/// move renames them.
pub(super) const NO_PROGRAM_REGISTER: usize = usize::MAX;

/// The accesses of a loop body that address the same qwords, in body
/// order, when one of them is a store: at one location, or, from an
/// induction register, a whole number of strides apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LoopGroup {
    /// Indices into the loop table's operations.
    pub(super) members: Vec<usize>,
    /// For each member, how many iterations ahead of the group's first
    /// member it addresses the same qword: member `a` in iteration `i` and
    /// member `b` in iteration `i + shifts[a] - shifts[b]` meet.
    pub(super) shifts: Vec<i64>,
    /// The location is the same in every iteration: its root is VI00, or a
    /// register no operation of the loop body defines.
    pub(super) invariant: bool,
}

/// The memory fields an instruction touches.
pub(super) fn fields(located: &Located) -> u8 {
    if located.instruction.metadata.mask_mode == MaskMode::Unmasked {
        0xf
    } else {
        located.instruction.mask.0
    }
}

/// A 16-bit difference as a signed number.
fn signed(value: i64) -> i64 {
    (value + 0x8000).rem_euclid(0x1_0000) - 0x8000
}

/// How many strides `location` is ahead of `reference`, when both step
/// from the same induction register and are a whole number of strides
/// apart; zero when they are the same location.
fn strides_apart(location: &Location, reference: &Location) -> Option<i64> {
    if location == reference {
        return Some(0);
    }
    let Root::Induction { stride, .. } = &reference.root else {
        return None;
    };
    if location.root != reference.root {
        return None;
    }
    let (difference, stride) = (signed(location.offset - reference.offset), signed(*stride));
    (difference % stride == 0).then(|| difference / stride)
}

/// The same-qword groups of a loop body's operations `live`.
pub(super) fn loop_groups(
    program: &Source,
    region: &Region,
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    body: &[usize],
    live: &[usize],
) -> Vec<LoopGroup> {
    let reads: Reads = analysis
        .reads
        .iter()
        .map(|read| ((read.operation, read.register, read.component), read))
        .collect();
    let resolver = Resolver {
        program,
        region,
        reads: &reads,
        graph: Some(graph),
    };
    let mut groups: Vec<(Location, LoopGroup)> = Vec::new();
    for (index, &id) in live.iter().enumerate() {
        if !region.instructions[id].instruction.metadata.is_memory() {
            continue;
        }
        let Some(location) = resolver.location(id) else {
            continue;
        };
        if let Some((group, shift)) = groups.iter_mut().find_map(|(reference, group)| {
            strides_apart(&location, reference).map(|shift| (group, shift))
        }) {
            group.members.push(index);
            group.shifts.push(shift);
            continue;
        }
        let invariant = match &location.root {
            Root::Absolute => true,
            Root::Value(_, definitions) => definitions.iter().all(|definition| {
                !matches!(definition, Definition::Operation(setter) if body.contains(setter))
            }),
            Root::Induction { .. } => false,
        };
        groups.push((
            location,
            LoopGroup {
                members: vec![index],
                shifts: vec![0],
                invariant,
            },
        ));
    }
    groups
        .into_iter()
        .map(|(_, group)| group)
        .filter(|group| {
            group.members.len() > 1
                && group.members.iter().any(|&index| {
                    region.instructions[live[index]]
                        .instruction
                        .metadata
                        .is_store()
                })
        })
        .collect()
}

/// The pairs of accesses (indices into the loop table's operations, in
/// body order) that a modulo schedule runs out of their sequential order:
/// each operation's absolute line is `lines`, and iteration `i` of it runs
/// `i * period` lines after its first. Two loads never conflict, nor do
/// accesses to different fields.
///
/// Members `a` before `b` in the body meet with `b` `m = shifts[a] -
/// shifts[b]` iterations later (`b` a stride behind reads what `a` wrote
/// before it). With `m = 0`, `a` must run first; with `m > 0`, `a` of
/// iteration `i` before `b` of `i + m`; with `m < 0`, `b` of `i + m`
/// before `a` of `i`. At an invariant location every iteration meets
/// every other: `a` before `b`, and `b` before the next iteration's `a`.
pub(super) fn broken_pairs(
    groups: &[LoopGroup],
    region: &Region,
    live: &[usize],
    lines: &[i32],
    period: i32,
) -> Vec<(usize, usize)> {
    let located = |index: usize| &region.instructions[live[index]];
    let period = i64::from(period);
    let mut broken = Vec::new();
    for group in groups {
        for (at, (&first, &first_shift)) in group.members.iter().zip(&group.shifts).enumerate() {
            for (&later, &later_shift) in
                group.members[at + 1..].iter().zip(&group.shifts[at + 1..])
            {
                let conflict = (located(first).instruction.metadata.is_store()
                    || located(later).instruction.metadata.is_store())
                    && fields(located(first)) & fields(located(later)) != 0;
                let (a, b) = (i64::from(lines[first]), i64::from(lines[later]));
                let lag = first_shift - later_shift;
                let out_of_order = if group.invariant {
                    b <= a || a + period <= b
                } else if lag >= 0 {
                    b + lag * period <= a
                } else {
                    a - lag * period <= b
                };
                if conflict && out_of_order {
                    broken.push((first, later));
                }
            }
        }
    }
    broken
}

/// The accesses joined by `pairs`, as groups in body order: two pairs that
/// share an access are one group.
pub(super) fn joined(pairs: &BTreeSet<(usize, usize)>) -> Vec<LoopGroup> {
    let mut sets: Vec<BTreeSet<usize>> = Vec::new();
    for &(first, later) in pairs {
        let touching: Vec<usize> = (0..sets.len())
            .filter(|&at| sets[at].contains(&first) || sets[at].contains(&later))
            .collect();
        let mut set = BTreeSet::from([first, later]);
        for &at in touching.iter().rev() {
            set.extend(sets.remove(at));
        }
        sets.push(set);
    }
    sets.into_iter()
        .map(|set| LoopGroup {
            shifts: vec![0; set.len()],
            members: set.into_iter().collect(),
            invariant: false,
        })
        .collect()
}
