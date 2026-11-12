//! The `imm` fix (docs/VCL_BUGS.md, F1): numeric immediates that do not
//! fit their encoding field.
//!
//! vcl 1.4beta7 copies numeric immediates to its output without checking
//! them against their field, and the assembler keeps only the field's low
//! bits without a diagnostic (observed from vcl 1.4beta7,
//! `checker-imm-*-source`, `openvcl__repros_immediates_out_of_range`). Its
//! displacement repair can also push an in-range memory offset out of its
//! field (`checker-imm-repair-*`). The field widths come from the VU User's
//! Manual's instruction reference.
//!
//! With the fix:
//!
//! - IADDIU and ISUBIU are rewritten exactly: VI registers hold 16 bits, so
//!   the addition is modulo 65536, and every 16-bit step but one has an
//!   encoding as one of the two (`iaddiu …,40000` is `isubiu …,25536`).
//!   Adding 0x8000 has none, so it is an error.
//! - A memory offset outside its field in the source is an error.
//! - A flag-test mask with bits past the flag's width (FCAND, FCOR, FSAND,
//!   FSOR) is a warning: those bits do not exist, so the test is the same.
//!   The listing carries the bits that do, as the assembler would.
//!   A value outside the field of FCEQ, FCSET, FSEQ or FSSET is an error.
//! - A symbolic immediate in a narrow field (IADDI, memory offsets) is a
//!   warning: only the assembler can check it.
//! - A displacement repair that would leave the field is not made: the
//!   memory instruction keeps its offset and stays on its side of the base
//!   register's update (`Instruction::fixed_offset`), and the program is
//!   compiled again.

use super::*;
use crate::parser::expression::{self, Evaluation, Number};

/// IADDIU and ISUBIU: 15 bits, unsigned.
const VI_STEP: std::ops::RangeInclusive<i64> = 0..=0x7fff;
/// LQ, SQ, ILW and ISW: 11 bits, signed.
const MEMORY_OFFSET: std::ops::RangeInclusive<i64> = -1024..=1023;
/// IADDI: 5 bits, signed.
const IADDI_STEP: std::ops::RangeInclusive<i64> = -16..=15;
const CLIP_FLAG: std::ops::RangeInclusive<i64> = 0..=0xff_ffff;
const STATUS_FLAG: std::ops::RangeInclusive<i64> = 0..=0xfff;

/// What the fix does with a numeric value outside an instruction's field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// The value means something else once truncated: an error.
    Reject,
    /// A mask whose extra bits do not exist: a warning.
    Mask,
    /// A VI step, rewritten as it is emitted; an error only for 0x8000.
    Step,
    /// Nothing: numeric IADDI is always rewritten to IADDIU or ISUBIU
    /// (observed from vcl 1.4beta7, `checker-imm-iaddi-*`).
    Normalized,
}

/// An instruction's immediate field.
struct Field {
    range: std::ops::RangeInclusive<i64>,
    rule: Rule,
    /// A narrow field, where a symbolic value is worth a warning.
    narrow: bool,
}

fn field(instruction: &Instruction) -> Option<Field> {
    let (range, rule, narrow) = match instruction.metadata.name {
        "lq" | "sq" | "ilw" | "isw" => (MEMORY_OFFSET, Rule::Reject, true),
        "iaddi" => (IADDI_STEP, Rule::Normalized, true),
        "iaddiu" | "isubiu" => (VI_STEP, Rule::Step, false),
        "fcand" | "fcor" => (CLIP_FLAG, Rule::Mask, false),
        "fceq" | "fcset" => (CLIP_FLAG, Rule::Reject, false),
        "fsand" | "fsor" => (STATUS_FLAG, Rule::Mask, false),
        "fseq" | "fsset" => (STATUS_FLAG, Rule::Reject, false),
        _ => return None,
    };
    Some(Field {
        range,
        rule,
        narrow,
    })
}

/// The immediate's source text: the first operand that takes one. A memory
/// operand without a displacement is offset 0.
fn immediate_text(instruction: &Instruction) -> Option<&str> {
    let operand = instruction
        .operands
        .iter()
        .find(|operand| operand.immediate_kind.value_type().is_some())?;
    match operand.immediate.as_deref() {
        Some(text) => Some(text),
        None if class::is_address(operand.class) => Some("0"),
        None => None,
    }
}

/// An immediate's value: a number, a symbolic expression, or neither.
enum Value {
    Number(i64),
    Symbolic,
}

fn value(text: &str) -> Option<Value> {
    match expression::evaluate(text).ok()? {
        Evaluation::Number(Number::Unsigned(value)) => Some(Value::Number(i64::from(value))),
        Evaluation::Number(Number::Signed(value)) => Some(Value::Number(i64::from(value))),
        Evaluation::Number(Number::Float(_)) => None,
        Evaluation::Unresolved { .. } => Some(Value::Symbolic),
    }
}

/// Checks every source immediate before compiling: the warnings to report,
/// or the first error.
pub(super) fn check_source(parsed: &parser::Program) -> Result<Vec<Diagnostic>, CompileError> {
    let mut warnings = Vec::new();
    for statement in &parsed.statements {
        let StatementKind::Instruction(instruction) = &statement.kind else {
            continue;
        };
        let (Some(field), Some(text)) = (field(instruction), immediate_text(instruction)) else {
            continue;
        };
        let name = instruction.metadata.name.to_string();
        let bits = (field.range.end() - field.range.start() + 1).ilog2() as u8;
        let number = match value(text) {
            Some(Value::Number(number)) => number,
            Some(Value::Symbolic) if field.narrow => {
                warnings.push(
                    Diagnostic::warning(CompileMessage::ImmediateUnchecked {
                        instruction: name,
                        value: text.to_string(),
                        bits,
                    })
                    .at(statement.line),
                );
                continue;
            }
            _ => continue,
        };
        if field.range.contains(&number) {
            continue;
        }
        match field.rule {
            Rule::Mask => warnings.push(
                Diagnostic::warning(CompileMessage::FlagMaskTruncated {
                    instruction: name,
                    value: text.to_string(),
                    bits,
                })
                .at(statement.line),
            ),
            Rule::Reject => return Err(out_of_field(statement.line, &name, text, &field.range)),
            Rule::Step if vi_step(signed_step(instruction, number)).is_none() => {
                return Err(out_of_field(statement.line, &name, text, &field.range));
            }
            Rule::Step | Rule::Normalized => {}
        }
    }
    Ok(warnings)
}

fn out_of_field(
    line: usize,
    instruction: &str,
    value: &str,
    range: &std::ops::RangeInclusive<i64>,
) -> CompileError {
    error(
        line,
        CompileMessage::ImmediateOutOfField {
            instruction: instruction.to_string(),
            value: value.to_string(),
            low: *range.start(),
            high: *range.end(),
        },
    )
}

/// Makes an instruction's immediate fit its field as it is emitted: an
/// IADDIU or ISUBIU step outside 15 bits becomes the exact equivalent, a
/// flag mask loses the bits past the flag, and a memory offset outside 11
/// bits, which only a displacement repair can produce once the source is
/// checked, is a `RepairedOffset` failure.
pub(super) fn legalize(instruction: &mut Instruction, line: usize) -> Result<(), CompileError> {
    let index = instruction.metadata.index;
    if let Some(Field {
        range,
        rule: Rule::Mask,
        ..
    }) = field(instruction)
        && let Some(Value::Number(mask)) = immediate_text(instruction).and_then(value)
        && !range.contains(&mask)
        && let Some(operand) = instruction
            .operands
            .iter_mut()
            .find(|operand| operand.immediate.is_some())
    {
        operand.immediate = Some((mask & range.end()).to_string());
        return Ok(());
    }

    if matches!(index, id::IADDIU | id::ISUBIU) {
        let Some(Value::Number(step)) =
            instruction.operands[2].immediate.as_deref().and_then(value)
        else {
            return Ok(());
        };
        if !VI_STEP.contains(&step) {
            let Some((descriptor, step)) = vi_step(signed_step(instruction, step)) else {
                let text = instruction.operands[2]
                    .immediate
                    .clone()
                    .unwrap_or_default();
                return Err(out_of_field(
                    line,
                    instruction.metadata.name,
                    &text,
                    &VI_STEP,
                ));
            };
            instruction.metadata = &instruction::INSTRUCTIONS[usize::from(descriptor)];
            instruction.mnemonic = instruction.metadata.name.into();
            instruction.operands[2].immediate = Some(step.to_string());
        }
        return Ok(());
    }

    if matches!(index, id::LQ | id::SQ | id::ILW | id::ISW)
        && let Some(text) = immediate_text(instruction)
        && let Some(Value::Number(offset)) = value(text)
        && !MEMORY_OFFSET.contains(&offset)
    {
        let mut failure = out_of_field(line, instruction.metadata.name, text, &MEMORY_OFFSET);
        failure.kind = CompileErrorKind::RepairedOffset;
        return Err(failure);
    }
    Ok(())
}

/// What an IADDIU or ISUBIU with immediate `step` adds.
fn signed_step(instruction: &Instruction, step: i64) -> i64 {
    if instruction.metadata.index == id::ISUBIU {
        -step
    } else {
        step
    }
}

/// The instruction and 15-bit step that add `added` to a 16-bit VI value;
/// none for 0x8000, which neither can encode.
fn vi_step(added: i64) -> Option<(u16, i64)> {
    let step = added.rem_euclid(0x1_0000);
    if VI_STEP.contains(&step) {
        Some((id::IADDIU, step))
    } else if VI_STEP.contains(&(0x1_0000 - step)) {
        Some((id::ISUBIU, 0x1_0000 - step))
    } else {
        None
    }
}

/// `parsed` with the memory instructions on `line` keeping their offsets,
/// or `None` when they already do.
pub(super) fn fix_offsets(parsed: &parser::Program, line: usize) -> Option<parser::Program> {
    let mut program = parsed.clone();
    let mut changed = false;
    for statement in &mut program.statements {
        if statement.line != line {
            continue;
        }
        if let StatementKind::Instruction(instruction) = &mut statement.kind
            && instruction.metadata.is_memory()
            && !instruction.fixed_offset
        {
            instruction.fixed_offset = true;
            changed = true;
        }
    }
    changed.then_some(program)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_16_bit_step_has_an_exact_encoding() {
        // The examples of docs/VCL_BUGS.md, F1.
        assert_eq!(vi_step(0xffff), Some((id::ISUBIU, 1)));
        assert_eq!(vi_step(1), Some((id::IADDIU, 1)));
        assert_eq!(vi_step(40000), Some((id::ISUBIU, 25536)));
        assert_eq!(vi_step(-70000), Some((id::ISUBIU, 4464)));
        // Neither IADDIU nor ISUBIU can add 0x8000 in one step.
        assert_eq!(vi_step(0x8000), None);
        assert_eq!(vi_step(-0x8000), None);
        for added in -70_000..70_000 {
            let Some((descriptor, step)) = vi_step(added) else {
                assert_eq!(added.rem_euclid(0x1_0000), 0x8000);
                continue;
            };
            assert!(VI_STEP.contains(&step), "{added}");
            let sum = if descriptor == id::IADDIU {
                step
            } else {
                -step
            };
            assert_eq!(sum.rem_euclid(0x1_0000), added.rem_euclid(0x1_0000));
        }
    }
}
