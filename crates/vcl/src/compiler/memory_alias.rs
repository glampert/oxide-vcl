//! The `alias` fix (docs/VCL_BUGS.md, F2): loads and stores whose addresses
//! are provably the same location.
//!
//! vcl 1.4beta7 orders two memory accesses with intersecting fields only
//! under `-M`, with a shared tag, or when their address operands match: the
//! same base register with the same reaching definitions, and the same
//! offset. Two registers that hold the same address are assumed not to
//! alias, so a load can move above a store to the same qword (observed from
//! vcl 1.4beta7: `openvcl__repros_store_load_alias`,
//! `openvcl__repros_store_store_alias`, `checker-mem-copied-base`).
//!
//! With the fix, each address is also written as a root plus a constant,
//! following the base register back through the steps that set it:
//! IADDIU, ISUBIU and IADDI by a number, IADD and ISUB with VI00, VI moves,
//! and the steps of LQI, SQI, LQD and SQD. The root is VI00 (an absolute
//! address) or a register at its reaching definitions, where the steps stop.
//! Two accesses with the same root and constant are the same location and
//! keep their order. Anything else keeps vcl's assumption, as before.

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

/// The location a memory instruction addresses, or `None` when its offset
/// is not a number.
pub(super) fn location(
    program: &Source,
    region: &Region,
    reads: &Reads,
    id: usize,
) -> Option<Location> {
    let located = &region.instructions[id];
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
    let base = located.operands[index];
    let at = match base {
        Some(register) => value(program, region, reads, register, id, &mut Vec::new())?,
        None => Location {
            root: Root::Absolute,
            offset: 0,
        },
    };
    Some(at.plus(offset + step))
}

fn number(text: &str) -> Option<i64> {
    crate::parser::expression::evaluate(text)
        .ok()?
        .number()
        .map(|value| i64::from(value.to_i32()))
}

/// The value of VI `register` as `operation` reads it, or `None` when the
/// read is not known.
fn value(
    program: &Source,
    region: &Region,
    reads: &Reads,
    register: usize,
    operation: usize,
    path: &mut Vec<usize>,
) -> Option<Location> {
    let named = program.registers.named(register);
    if named
        .physical
        .or_else(|| region.forced.get(&register).copied())
        == Some(0)
    {
        return Some(Location {
            root: Root::Absolute,
            offset: 0,
        });
    }
    let read = reads.get(&(operation, register, 0))?;
    let opaque = Location {
        root: Root::Value(register, read.definitions.clone()),
        offset: 0,
    };
    if path.len() >= DEPTH {
        return Some(opaque);
    }
    // Every reaching definition must give the same location.
    let mut agreed: Option<Location> = None;
    for definition in &read.definitions {
        let Definition::Operation(setter) = *definition else {
            return Some(opaque);
        };
        if path.contains(&setter) {
            return Some(opaque);
        }
        path.push(setter);
        let set = written(program, region, reads, register, setter, path);
        path.pop();
        match (set, &agreed) {
            (None, _) => return Some(opaque),
            (Some(location), Some(previous)) if location != *previous => return Some(opaque),
            (Some(location), _) => agreed = Some(location),
        }
    }
    Some(agreed.unwrap_or(opaque))
}

/// The value `setter` writes to VI `register`, when it is a step from
/// another value.
fn written(
    program: &Source,
    region: &Region,
    reads: &Reads,
    register: usize,
    setter: usize,
    path: &mut Vec<usize>,
) -> Option<Location> {
    let located = &region.instructions[setter];
    let operand = |index: usize| located.operands.get(index).copied().flatten();
    let is_zero = |index: usize| {
        operand(index).is_some_and(|r| {
            program
                .registers
                .named(r)
                .physical
                .or_else(|| region.forced.get(&r).copied())
                == Some(0)
        })
    };
    let mut from = |source: usize| value(program, region, reads, source, setter, path);
    match located.instruction.metadata.index {
        id::IADDIU | id::ISUBIU | id::IADDI if operand(0) == Some(register) => {
            let step = number(located.instruction.operands[2].immediate.as_deref()?)?;
            let step = if located.instruction.metadata.index == id::ISUBIU {
                -step
            } else {
                step
            };
            Some(from(operand(1)?)?.plus(step))
        }
        id::IADD | id::ISUB if operand(0) == Some(register) && is_zero(2) => from(operand(1)?),
        id::IADD if operand(0) == Some(register) && is_zero(1) => from(operand(2)?),
        id::IMOVE | id::LIMOVE if operand(0) == Some(register) => from(operand(1)?),
        id::LQI | id::SQI | id::LQD | id::SQD => {
            let index = located
                .instruction
                .operands
                .iter()
                .position(|operand| class::is_address(operand.class))?;
            (operand(index) == Some(register)).then_some(())?;
            let step = if matches!(located.instruction.metadata.index, id::LQI | id::SQI) {
                1
            } else {
                -1
            };
            Some(from(register)?.plus(step))
        }
        _ => None,
    }
}
