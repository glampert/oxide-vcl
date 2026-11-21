//! Oxide mode's register spilling to a `.vu_work_area`.
//!
//! vcl 1.4beta7 accepts the directive but never installs the memory pool it
//! needs, so every work area is an error there (observed, `spill-vf-area`).
//! In oxide mode a work area is data memory the program gives the compiler.
//! When a region's VF or VI registers do not fit, one symbolic name is
//! spilled: every write of it is followed by a store of the fields written,
//! and every read is preceded by a load into a fresh name, so the value lives
//! in its own qword of the area and only short ranges need a register. The
//! program is then compiled again, adding spills one at a time until it fits
//! or the area is full.
//!
//! In a program that calls a library (`--call`), a value is first saved
//! across the calls only: every write is still followed by a store, but the
//! value keeps its register and its reads, and is loaded back into the same
//! name right after each call's JALR. A call takes the registers it declares,
//! so what crosses one has the fewest left; saved this way, the value no
//! longer crosses any call, and the code between calls keeps it in a
//! register. When the same name is chosen again, saving it across calls did
//! not make the region fit, and it is spilled everywhere.
//!
//! The rewrite works on the parsed statements, before lowering, so the
//! stores and loads are scheduled and allocated like the program's own
//! instructions. Memory dependencies order a load after the store to the same
//! address (same base, VI00, and offset); the area must not be used by the
//! program itself.

use super::*;
use crate::isa::instruction::{Mask, class};
use crate::parser::{Operand, ParseMessage, ParserOptions, Program, Statement};
use std::cmp::Reverse;

/// A `.vu_work_area` request, read the oxide way: decimal or `0x`
/// hexadecimal qwords.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkArea {
    device: String,
    address: u32,
    size: u32,
}

impl WorkArea {
    /// Reads a work area's tokens. The parser has already checked the device
    /// and vcl's decimal-prefix reading of the numbers.
    pub(super) fn parse(device: &str, address: &str, size: &str) -> Result<Self, Message> {
        let capacity = if device == "vu0" { 0x100 } else { 0x400 };
        let address = number(address)?;
        let size = number(size)?;

        if size == 0 || u64::from(address) + u64::from(size) > capacity {
            return Err(ParseMessage::WorkAreaOutsideMemory.into());
        }

        Ok(Self {
            device: device.to_string(),
            address,
            size,
        })
    }
}

/// A decimal or `0x` hexadecimal number, with nothing after it.
fn number(token: &str) -> Result<u32, Message> {
    let token = token.trim();
    let value = match token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        Some(digits) => u32::from_str_radix(digits, 16),
        None => token.parse(),
    };

    value.map_err(|_| {
        CompileMessage::WorkAreaNumber {
            token: token.to_string(),
        }
        .into()
    })
}

/// A symbolic name kept in the work area.
#[derive(Clone, Debug)]
struct Spilled {
    name: String,
    vector: bool,
    device: String,
    address: u32,
    /// The first line that names it, for the note.
    line: usize,
    /// Saved across library calls only: loaded back after each call, not
    /// before each read.
    across_calls: bool,
}

/// The names spilled so far.
#[derive(Default)]
pub(super) struct Spills {
    spilled: Vec<Spilled>,
}

impl Spills {
    /// The notes that report each spill.
    pub(super) fn notes(&self) -> Vec<Diagnostic> {
        self.spilled
            .iter()
            .map(|spill| {
                let message = if spill.across_calls {
                    CompileMessage::RegisterSavedAcrossCalls {
                        name: spill.name.clone(),
                        vector: spill.vector,
                        device: spill.device.clone(),
                        address: spill.address,
                    }
                } else {
                    CompileMessage::RegisterSpilled {
                        name: spill.name.clone(),
                        vector: spill.vector,
                        device: spill.device.clone(),
                        address: spill.address,
                    }
                };
                Diagnostic::new(Severity::Info, spill.line, message)
            })
            .collect()
    }

    /// Answers a failed compile with one more spill, or returns the error
    /// when spilling cannot help: the failure was not an allocation, there
    /// is no work area, the area is full, or nothing left can be spilled.
    pub(super) fn add(&mut self, parsed: &Program, failure: Failure) -> Result<(), CompileError> {
        let Failure {
            error: failed,
            lines,
            work_areas,
        } = failure;

        let allocation = matches!(
            failed.kind,
            CompileErrorKind::Allocation | CompileErrorKind::ExternalAllocation
        );
        let crossing = match &failed.message {
            Message::Compile(CompileMessage::AllocationPoolFull { crossing, .. }) => {
                crossing.clone()
            }
            _ => Vec::new(),
        };
        if !allocation || work_areas.is_empty() {
            return Err(failed);
        }

        let (vector, register) = match &failed.message {
            Message::Compile(CompileMessage::AllocationPoolFull {
                vector, register, ..
            }) => (Some(*vector), register.clone()),
            // No register at all: a spilled value still needs one.
            Message::Compile(CompileMessage::AllocationPoolEmpty { .. }) => return Err(failed),
            _ => (None, String::new()),
        };

        // A value that meets a library call failed: one of the values that
        // cross calls is saved across them. Otherwise a value is spilled
        // everywhere; when every candidate already is, saving one across
        // calls was not enough, and the longest saved is spilled everywhere,
        // in the same slot.
        let across_calls = !crossing.is_empty()
            && choose(
                parsed,
                &self.spilled,
                lines.clone(),
                vector,
                Some(&crossing),
            )
            .is_some();
        if !across_calls && choose(parsed, &self.spilled, lines.clone(), vector, None).is_none() {
            let saved = choose_saved(parsed, &self.spilled, lines.clone(), vector);
            if let Some(name) = saved
                && let Some(spill) = self.spilled.iter_mut().find(|spill| spill.name == name)
            {
                spill.across_calls = false;
                return Ok(());
            }
        }

        let Some((device, address)) = self.next_slot(&work_areas) else {
            let size = work_areas.iter().map(|area| area.size).sum();
            return Err(error(
                failed.line,
                CompileMessage::WorkAreaFull {
                    vector: vector.unwrap_or(true),
                    register,
                    size,
                },
            ));
        };

        let among = across_calls.then_some(crossing.as_slice());
        let Some(candidate) = choose(parsed, &self.spilled, lines, vector, among) else {
            return Err(failed);
        };

        self.spilled.push(Spilled {
            name: candidate.name,
            vector: candidate.vector,
            device,
            address,
            line: candidate.line,
            across_calls,
        });
        Ok(())
    }

    /// The next free qword of the work areas, in source order.
    fn next_slot(&self, areas: &[WorkArea]) -> Option<(String, u32)> {
        let mut index = self.spilled.len() as u32;
        for area in areas {
            if index < area.size {
                return Some((area.device.clone(), area.address + index));
            }
            index -= area.size;
        }
        None
    }

    /// The parsed program with a store after every write of a spilled name
    /// and a load into a fresh name before every read of one.
    pub(super) fn apply(&self, parsed: &Program) -> Program {
        if self.spilled.is_empty() {
            return parsed.clone();
        }

        let mut temporaries = Temporaries::new(parsed);
        let mut statements = Vec::with_capacity(parsed.statements.len());
        // The `--LoopCS` hint of the block being rewritten, until its branch.
        let mut loop_hint: Option<usize> = None;
        // After `--endcall`, until the call's JALR.
        let mut calling = false;

        for statement in &parsed.statements {
            let StatementKind::Instruction(instruction) = &statement.kind else {
                statements.push(statement.clone());
                continue;
            };
            if instruction.synthetic() == Some(Synthetic::LoopCount) {
                loop_hint = Some(statements.len());
            }
            if instruction.synthetic() == Some(Synthetic::EndCall) {
                calling = true;
            }
            if calling && instruction.synthetic().is_none() {
                // The call's JALR, then the values saved across calls, back
                // into their own names where the callee returns.
                calling = false;
                statements.push(statement.clone());
                statements.extend(
                    self.spilled
                        .iter()
                        .filter(|spill| spill.across_calls)
                        .map(|spill| memory_access(spill, &spill.name, None, statement.line)),
                );
                continue;
            }
            if !rewritable(instruction) {
                statements.push(statement.clone());
                continue;
            }

            let mut instruction = instruction.clone();
            let loads = self.load_reads(&mut instruction, statement.line, &mut temporaries);
            let stores = self.store_writes(&instruction, statement.line);

            if (!loads.is_empty() || !stores.is_empty())
                && let Some(hint) = loop_hint
            {
                forbid_extra_stores(&mut statements[hint]);
            }
            if instruction.metadata.is_branch() {
                loop_hint = None;
            }

            statements.extend(loads);
            statements.push(Statement {
                line: statement.line,
                kind: StatementKind::Instruction(instruction),
            });
            statements.extend(stores);
        }

        Program { statements }
    }

    /// Renames the instruction's reads of spilled names to fresh names, and
    /// returns the loads that fill them. Two reads of one name share a load.
    fn load_reads(
        &self,
        instruction: &mut Instruction,
        line: usize,
        temporaries: &mut Temporaries,
    ) -> Vec<Statement> {
        let mut loaded: Vec<(usize, String)> = Vec::new();
        let mut loads = Vec::new();

        for operand in &mut instruction.operands {
            if operand.implicit || operand.direction != Direction::Read {
                continue;
            }
            let Some(index) = self.spill_of(operand) else {
                continue;
            };
            if self.spilled[index].across_calls {
                continue;
            }

            let temporary = match loaded.iter().find(|(spill, _)| *spill == index) {
                Some((_, temporary)) => temporary.clone(),
                None => {
                    let spill = &self.spilled[index];
                    let temporary = temporaries.fresh(&spill.name);
                    loads.push(memory_access(spill, &temporary, None, line));
                    loaded.push((index, temporary.clone()));
                    temporary
                }
            };

            rename(operand, &self.spilled[index].name, &temporary);
        }

        loads
    }

    /// The stores that follow the instruction's writes of spilled names,
    /// each storing only the fields written.
    fn store_writes(&self, instruction: &Instruction, line: usize) -> Vec<Statement> {
        instruction
            .operands
            .iter()
            .filter(|operand| !operand.implicit && operand.direction == Direction::Write)
            .filter_map(|operand| {
                let spill = &self.spilled[self.spill_of(operand)?];
                Some(memory_access(spill, &spill.name, Some(operand.mask), line))
            })
            .collect()
    }

    /// The spill that an operand names, if any.
    fn spill_of(&self, operand: &Operand) -> Option<usize> {
        let name = operand.register.as_deref()?;
        let vector = register_vector(operand.class)?;
        self.spilled
            .iter()
            .position(|spill| spill.name == name && spill.vector == vector)
    }
}

/// Sets a `--LoopCS count, safe` hint's safe count to zero. The program's
/// own stores may be harmless past the last iteration, but a spill store
/// overwrites a value still to be read, so a pipelined loop that spills must
/// not start the next iteration's stores before its exit test.
fn forbid_extra_stores(hint: &mut Statement) {
    let StatementKind::Instruction(instruction) = &mut hint.kind else {
        return;
    };
    if let Some(safe) = instruction.operands.get_mut(1) {
        safe.text = "0".into();
        safe.immediate = Some("0".into());
    }
}

/// Whether the rewrite may add loads and stores around an instruction:
/// ordinary source code, not directives, declarations or raw code.
fn rewritable(instruction: &Instruction) -> bool {
    instruction.synthetic().is_none() && instruction.mode == ParseMode::Vcl
}

/// Whether an operand class holds a VF (true) or VI (false) register.
fn register_vector(operand_class: u8) -> Option<bool> {
    match instruction::register_family(operand_class) {
        RegisterFamily::Vector => Some(true),
        RegisterFamily::Integer => Some(false),
        _ => None,
    }
}

/// Points a read at a fresh name, keeping any field suffix it was written
/// with.
fn rename(operand: &mut Operand, name: &str, temporary: &str) {
    operand.register = Some(temporary.to_string());
    operand.text = match operand.text.strip_prefix(name) {
        Some(rest) => format!("{temporary}{rest}"),
        None => temporary.to_string(),
    };
}

/// A load of a spilled value into `register`, or (with the fields written)
/// a store of `register` to the spill's qword: LQ/SQ for VF, ILW/ISW (field
/// x) for VI, addressed from VI00.
fn memory_access(spill: &Spilled, register: &str, stored: Option<Mask>, line: usize) -> Statement {
    let address = spill.address;
    let text = match (spill.vector, stored) {
        (true, None) => format!("lq.xyzw spill, {address}(vi00)"),
        (true, Some(fields)) => format!("sq.{} spill, {address}(vi00)", fields.suffix()),
        (false, None) => format!("ilw.x spill, {address}(vi00)"),
        (false, Some(_)) => format!("isw.x spill, {address}(vi00)"),
    };

    let parsed = parser::parse_with_options(
        &text,
        ParserOptions {
            syntax: Syntax::Old,
            allow_incomplete: true,
            ..Default::default()
        },
    )
    .expect("spill loads and stores are valid VCL");
    let Some(StatementKind::Instruction(mut instruction)) =
        parsed.statements.into_iter().map(|s| s.kind).next()
    else {
        unreachable!("a spill load or store parses as one instruction");
    };

    let operand = &mut instruction.operands[0];
    operand.register = Some(register.to_string());
    operand.text = register.to_string();

    Statement {
        line,
        kind: StatementKind::Instruction(instruction),
    }
}

/// Fresh names for spill loads, distinct from every name in the program.
struct Temporaries {
    taken: BTreeSet<String>,
    next: usize,
}

impl Temporaries {
    fn new(parsed: &Program) -> Self {
        let taken = parsed
            .statements
            .iter()
            .filter_map(|statement| match &statement.kind {
                StatementKind::Instruction(instruction) => Some(instruction),
                _ => None,
            })
            .flat_map(|instruction| &instruction.operands)
            .filter_map(|operand| operand.register.clone())
            .collect();
        Self { taken, next: 0 }
    }

    /// A name for one more load of `name`.
    fn fresh(&mut self, name: &str) -> String {
        loop {
            self.next += 1;
            let candidate = format!("{name}__spill{}", self.next);
            if self.taken.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

/// A name the rewrite could spill.
struct Candidate {
    name: String,
    vector: bool,
    line: usize,
}

/// How a symbolic name is used across the program.
#[derive(Default)]
struct Usage {
    vector: Option<bool>,
    excluded: bool,
    written: bool,
    /// Statement indices of its first and last mention in the failed
    /// region, and the first line naming it anywhere.
    first: Option<usize>,
    last: usize,
    line: usize,
}

/// Picks the next name to spill: of the failed family, mentioned in the
/// failed region, among `among` when given, and with the longest stretch
/// of statements between its first and last mention there (the earliest on
/// a tie), since spilling it frees a register over the most code.
fn choose(
    parsed: &Program,
    spilled: &[Spilled],
    lines: Option<std::ops::RangeInclusive<usize>>,
    vector: Option<bool>,
    among: Option<&[String]>,
) -> Option<Candidate> {
    let usage = usages(parsed, lines);

    usage
        .into_iter()
        .filter(|(name, usage)| {
            !usage.excluded
                && usage.written
                && usage.first.is_some()
                && (vector.is_none() || usage.vector == vector)
                && among.is_none_or(|among| among.contains(name))
                && !spilled.iter().any(|spill| &spill.name == name)
        })
        .max_by_key(|(_, usage)| {
            let first = usage.first.unwrap_or_default();
            (usage.last - first, Reverse(first))
        })
        .map(|(name, usage)| Candidate {
            name,
            vector: usage.vector.unwrap_or(true),
            line: usage.line,
        })
}

/// The name saved across calls that [`choose`] would pick were it not
/// spilled at all.
fn choose_saved(
    parsed: &Program,
    spilled: &[Spilled],
    lines: Option<std::ops::RangeInclusive<usize>>,
    vector: Option<bool>,
) -> Option<String> {
    let saved: Vec<Spilled> = spilled
        .iter()
        .filter(|spill| !spill.across_calls)
        .cloned()
        .collect();
    let names: Vec<String> = spilled
        .iter()
        .filter(|spill| spill.across_calls)
        .map(|spill| spill.name.clone())
        .collect();
    choose(parsed, &saved, lines, vector, Some(&names)).map(|candidate| candidate.name)
}

/// Every symbolic VF and VI name's usage. Names that the rewrite cannot
/// handle are excluded: interface and directive operands, raw code, names
/// inside visibility scopes (the same spelling can be two values there),
/// implicit operands (the register an LQI or SQD steps), auto-increment
/// addresses, branch link registers, and names used as both VF and VI.
fn usages(
    parsed: &Program,
    lines: Option<std::ops::RangeInclusive<usize>>,
) -> BTreeMap<String, Usage> {
    let mut usage: BTreeMap<String, Usage> = BTreeMap::new();
    let mut scopes = 0usize;

    for (index, statement) in parsed.statements.iter().enumerate() {
        let instruction = match &statement.kind {
            StatementKind::Directive(Directive::Visibility { .. }) => {
                scopes += 1;
                continue;
            }
            StatementKind::Directive(Directive::EndVisibility { .. }) => {
                scopes = scopes.saturating_sub(1);
                continue;
            }
            StatementKind::Instruction(instruction) => instruction,
            _ => continue,
        };
        let in_region = lines
            .as_ref()
            .is_none_or(|lines| lines.contains(&statement.line));

        for operand in &instruction.operands {
            let Some(name) = operand.register.as_deref() else {
                continue;
            };
            let Some(vector) = register_vector(operand.class) else {
                continue;
            };
            if parser::physical_register(name, vector).is_some() {
                continue;
            }

            let entry = usage.entry(name.to_string()).or_default();
            if entry.vector.is_none() {
                entry.line = statement.line;
            }
            entry.excluded |= entry.vector.is_some_and(|known| known != vector)
                || scopes > 0
                || !rewritable(instruction)
                || operand.implicit
                || matches!(
                    operand.class,
                    class::ADDRESS_INCREMENT | class::ADDRESS_DECREMENT
                )
                || (instruction.metadata.is_branch() && operand.direction == Direction::Write);
            entry.vector = Some(vector);
            entry.written |= operand.direction == Direction::Write;

            if in_region {
                entry.first.get_or_insert(index);
                entry.last = index;
            }
        }
    }

    usage
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_area_numbers_are_decimal_or_hexadecimal() {
        assert_eq!(number("1000"), Ok(1000));
        assert_eq!(number("0x3e8"), Ok(1000));
        assert_eq!(number("0X3E8"), Ok(1000));
        for token in ["12tail", "-1", "BASE", "0x", ""] {
            assert_eq!(
                number(token),
                Err(CompileMessage::WorkAreaNumber {
                    token: token.into()
                }
                .into()),
                "{token}"
            );
        }
    }

    #[test]
    fn work_areas_must_fit_their_device() {
        assert!(WorkArea::parse("vu1", "1016", "8").is_ok());
        assert!(WorkArea::parse("vu0", "0xf8", "8").is_ok());
        for (device, address, size) in [
            ("vu0", "0x100", "1"),
            ("vu1", "1020", "8"),
            ("vu1", "0", "0"),
        ] {
            assert_eq!(
                WorkArea::parse(device, address, size),
                Err(ParseMessage::WorkAreaOutsideMemory.into()),
                "{device},{address},{size}"
            );
        }
    }

    #[test]
    fn slots_fill_each_area_in_turn() {
        let areas = [
            WorkArea::parse("vu1", "500", "1").unwrap(),
            WorkArea::parse("vu1", "900", "2").unwrap(),
        ];
        let mut spills = Spills::default();
        let mut addresses = Vec::new();
        while let Some((_, address)) = spills.next_slot(&areas) {
            addresses.push(address);
            spills.spilled.push(Spilled {
                name: format!("value{address}"),
                vector: true,
                device: "vu1".into(),
                address,
                line: 1,
                across_calls: false,
            });
        }
        assert_eq!(addresses, [500, 900, 901]);
    }
}
