//! Instruction formatting inferred from VCL's generic emitter, operand emitter,
//! component modifier emitter and the special `cliplw` emitter.
//!
//! This layer formats an already allocated instruction. It does not choose
//! issue slots, reverse branch senses, or synthesize allocator-internal moves.

use crate::isa::instruction::{ImmediateKind, ImmediateType, Mask, MaskMode, class, id};
use crate::parser::expression::{self, Evaluation, ExpressionOptions, Number};
use crate::parser::{Instruction, Operand};
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmitError(pub String);

impl fmt::Display for EmitError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.0)
    }
}

impl std::error::Error for EmitError {}

/// Render one instruction, without surrounding pair padding or a newline.
/// Replacements are indexed by all operands, including implicit operands.
/// A missing replacement preserves the operand's original register or text.
/// Numeric immediates use the descriptor's original conversion and formatting;
/// unresolved assembler expressions retain their complete source spelling.
pub fn instruction(
    inst: &Instruction,
    registers: &[Option<String>],
    extra_modifiers: u8,
) -> Result<String, EmitError> {
    if inst.synthetic().is_some() {
        return Err(EmitError(
            "synthetic instruction has no machine encoding".into(),
        ));
    }
    if matches!(
        inst.metadata.name,
        "imove" | "limove" | "vmove" | "lvmove" | "uvmove"
    ) {
        // The original's custom move hook requires baseMuid/outputMuid state
        // created by allocation. Source-written abstract moves lack that state
        // and fail with this exact diagnostic (local/evidence/oracle/emit-*move).
        if inst.generated_copy.is_none() {
            return Err(EmitError("baseMuid not set in vmove_Emit".into()));
        }
        let mut resolved = inst.clone();
        let mut registers = registers.to_vec();
        match (inst.generated_copy, inst.metadata.index) {
            (Some(crate::analysis::copies::Kind::Integer), id::LIMOVE) => {
                resolved.metadata =
                    crate::isa::instruction::lookup("iaddiu", crate::isa::instruction::Syntax::Old)
                        .unwrap();
                let mut zero = inst.operands[1].clone();
                zero.class = class::IMMEDIATE;
                zero.register = None;
                zero.forced_register = None;
                zero.text = "0".into();
                zero.immediate = Some("0".into());
                // The move hook writes a literal decimal zero.
                zero.immediate_kind = ImmediateKind::signed(5);
                zero.tags.clear();
                resolved.operands.push(zero);
                registers.push(None);
            }
            (Some(crate::analysis::copies::Kind::Vector), id::LVMOVE) => {
                resolved.metadata =
                    crate::isa::instruction::lookup("move", crate::isa::instruction::Syntax::Old)
                        .unwrap();
            }
            (Some(crate::analysis::copies::Kind::Vector), id::UVMOVE) => {
                resolved.metadata =
                    crate::isa::instruction::lookup("max", crate::isa::instruction::Syntax::Old)
                        .unwrap();
                resolved.operands.push(inst.operands[1].clone());
                registers.push(registers.get(1).cloned().flatten());
            }
            _ => return Err(EmitError("trying to emit an abstract move".into())),
        }
        resolved.generated_copy = None;
        resolved.mnemonic = resolved.metadata.name.into();
        return instruction(&resolved, &registers, extra_modifiers);
    }

    let modifiers = inst.modifiers | extra_modifiers;
    if modifiers & !15 != 0 {
        return Err(EmitError("unknown instruction modifier bits".into()));
    }

    // The original's collapse of IADDI preserves all register effects and
    // the operand's signed selector while replacing opcode and magnitude.
    let collapsed_iaddi = if inst.metadata.index == id::IADDI {
        inst.operands
            .get(2)
            .and_then(|operand| operand.immediate.as_deref())
            .map(expression::evaluate)
            .transpose()
            .map_err(|error| EmitError(error.to_string()))?
            .and_then(|evaluation| evaluation.number())
            .map(Number::to_i32)
    } else {
        None
    };

    let mut mnemonic = if let Some(value) = collapsed_iaddi {
        if value < 0 { "isubiu" } else { "iaddiu" }.to_owned()
    } else if inst.metadata.name == "cliplw" {
        "clipw".to_owned()
    } else {
        inst.metadata.name.to_owned()
    };

    mnemonic.push_str(&modifier_suffix(modifiers));

    if inst.metadata.name == "cliplw" {
        mnemonic.push_str(".xyz");
    } else {
        match inst.metadata.mask_mode {
            MaskMode::Unmasked => {}
            MaskMode::Any if inst.mask.0 == 0 || inst.mask.0 & !15 != 0 => {
                return Err(EmitError("invalid instruction component mask".into()));
            }
            MaskMode::Any => append_mask(&mut mnemonic, inst.mask.0),
            MaskMode::Xyz if inst.mask == Mask::XYZ => mnemonic.push_str(".xyz"),
            MaskMode::OneLane if inst.mask.is_scalar() => append_mask(&mut mnemonic, inst.mask.0),
            _ => {
                return Err(EmitError(
                    "instruction component mask does not match descriptor".into(),
                ));
            }
        }
    }

    let mut operands = Vec::new();

    for (index, operand) in inst.operands.iter().enumerate() {
        if !operand.implicit {
            let replacement = registers.get(index).and_then(Option::as_deref);
            let selector = operand.immediate_kind;
            if index == 2
                && let Some(value) = collapsed_iaddi
            {
                operands.push(
                    if value < 0 {
                        value.wrapping_neg()
                    } else {
                        value
                    }
                    .to_string(),
                );
                continue;
            }
            operands.push(emit_operand(operand, replacement, selector)?);
        }
    }

    if operands.is_empty() {
        return Ok(mnemonic);
    }

    // Original operands begin 14 bytes after the instruction's first byte.
    Ok(format!("{mnemonic:<14}{}", operands.join(",")))
}

/// The original's shared flag-string builder uses D, E, T, M order.
pub(crate) fn modifier_suffix(modifiers: u8) -> String {
    if modifiers == 0 {
        return String::new();
    }
    let mut text = String::from("[");
    for (bit, flag) in [(4, 'D'), (2, 'E'), (8, 'T'), (1, 'M')] {
        if modifiers & bit != 0 {
            text.push(flag);
        }
    }
    text.push(']');
    text
}

fn append_mask(output: &mut String, mask: u8) {
    if mask != 15 {
        output.push('.');
        for (bit, character) in [(1, 'x'), (2, 'y'), (4, 'z'), (8, 'w')] {
            if mask & bit != 0 {
                output.push(character);
            }
        }
    }
}

fn register_name(operand: &Operand, replacement: Option<&str>) -> Result<String, EmitError> {
    let name = replacement
        .or(operand.register.as_deref())
        .ok_or_else(|| EmitError(format!("operand '{}' has no register", operand.text)))?;
    if name.len() >= 3 {
        let prefix = &name.as_bytes()[..2];
        if (prefix.eq_ignore_ascii_case(b"vf") || prefix.eq_ignore_ascii_case(b"vi"))
            && let Ok(number) = name[2..].parse::<u8>()
        {
            return Ok(format!("{}{:02}", name[..2].to_ascii_uppercase(), number));
        }
    }
    Ok(name.to_owned())
}

fn emit_operand(
    operand: &Operand,
    replacement: Option<&str>,
    selector: ImmediateKind,
) -> Result<String, EmitError> {
    use class::*;
    let register = || register_name(operand, replacement);
    let lane = |suffix: &str| register().map(|name| name + suffix);
    match operand.class {
        VECTOR | INTEGER | INTEGER_FLAG | CORE_GPR32..=CORE_FPR_SCALAR | CORE_RETURN_ADDRESS => {
            register()
        }
        SCALAR => {
            if !operand.mask.is_scalar() {
                return Err(EmitError(
                    "scalar operand does not select one component".into(),
                ));
            }
            lane(&operand.mask.suffix())
        }
        I => Ok("I".into()),
        P => Ok("P".into()),
        NEXT_P => Ok("nP".into()),
        Q => Ok("Q".into()),
        R => Ok("R".into()),
        ACC => Ok("ACC".into()),
        ACC_XYZ => Ok("ACCxyz".into()),
        VECTOR_XYZ => lane("xyz"),
        FIELD_X => lane("x"),
        FIELD_Y => lane("y"),
        FIELD_Z => lane("z"),
        FIELD_W => lane("w"),
        IMMEDIATE => immediate(
            operand.immediate.as_deref().unwrap_or(&operand.text),
            selector,
            false,
        ),
        ADDRESS | ADDRESS_REGISTER => register().map(|name| format!("({name})")),
        ADDRESS_OFFSET | ADDRESS_REGISTER_OFFSET | CORE_ADDRESS_OFFSET => {
            let name = register()?;
            let offset = immediate(operand.immediate.as_deref().unwrap_or("0"), selector, true)?;
            Ok(format!("{offset}({name})"))
        }
        ADDRESS_INCREMENT => register().map(|name| format!("({name}++)")),
        ADDRESS_DECREMENT => register().map(|name| format!("(--{name})")),
        MEMORY => register().map(|name| format!("ramreg-[{name}]")),
        _ => Err(EmitError(format!(
            "cannot emit operand class {}",
            operand.class
        ))),
    }
}

/// The selector's value type picks the conversion. Its field width is
/// consumed by validation and fixups, not by this final formatting layer.
fn immediate(source: &str, selector: ImmediateKind, memory: bool) -> Result<String, EmitError> {
    let value_type = selector.value_type();
    if value_type == Some(ImmediateType::Literal) {
        return Ok(source.to_owned());
    }
    let evaluation = expression::evaluate_with_options(
        source,
        ExpressionOptions {
            hex_is_float: value_type == Some(ImmediateType::Float),
            ..ExpressionOptions::default()
        },
    )
    .map_err(|error| EmitError(error.to_string()))?;
    let number = match evaluation {
        Evaluation::Unresolved { source, .. } => return Ok(source),
        Evaluation::Number(number) => number,
    };
    let converted = match value_type {
        Some(ImmediateType::Unsigned) => Number::Unsigned(number.to_u32()),
        Some(ImmediateType::Signed) => Number::Signed(number.to_i32()),
        Some(ImmediateType::Float) => Number::Float(number.to_f32()),
        _ => number,
    };
    if memory {
        // Effective-address emission uses the immediate's own character formatter, rather
        // than the standalone immediate's hexadecimal formatting branch.
        return Ok(match converted {
            Number::Unsigned(value) => value.to_string(),
            Number::Signed(value) => value.to_string(),
            Number::Float(value) => scientific(value),
        });
    }
    Ok(if value_type == Some(ImmediateType::Signed) {
        converted.to_i32().to_string()
    } else {
        format!("0x{:08x}", converted.bits())
    })
}

fn scientific(value: f32) -> String {
    let formatted = format!("{:.9e}", f64::from(value));
    if let Some((significand, exponent)) = formatted.split_once('e')
        && let Ok(exponent) = exponent.parse::<i32>()
    {
        return format!("{significand}e{exponent:+03}");
    }
    formatted.to_ascii_lowercase()
}

/// Exact original pair layout: upper starts at column 9, lower at column 68,
/// and the lower field is padded to 50 bytes. No newline is appended.
pub fn pair(upper: &str, lower: &str) -> String {
    format!("         {upper:<59}{lower:<50}")
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        isa::instruction::Syntax,
        parser::{ParserOptions, StatementKind, parse_with_options},
    };

    fn parsed(source: &str) -> Instruction {
        let program = parse_with_options(
            source,
            ParserOptions {
                allow_incomplete: true,
                ..ParserOptions::default()
            },
        )
        .unwrap();
        program
            .statements
            .into_iter()
            .find_map(|statement| match statement.kind {
                StatementKind::Instruction(inst) => Some(inst),
                _ => None,
            })
            .unwrap()
    }

    fn physical(names: &[&str]) -> Vec<Option<String>> {
        names.iter().map(|name| Some((*name).to_owned())).collect()
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn mapped_add_omits_implicit_flags_and_has_exact_columns() {
        let inst = parsed("add b,a,a");
        let upper = instruction(&inst, &physical(&["VF02", "VF01", "VF01"]), 2).unwrap();
        assert_eq!(upper, "add[E]        VF02,VF01,VF01");
        let output = pair(&upper, "NOP");
        assert_eq!(output.as_bytes()[9..15], *b"add[E]");
        assert_eq!(&output[68..71], "NOP");
        assert_eq!(output.len(), 118);
        assert_eq!(
            output,
            crate::test_evidence::text("oracle/sched-add-symbol/stdout.bin")
                .as_str()
                .lines()
                .find(|line| line.contains("add[E]"))
                .unwrap()
        );
    }

    #[test]
    fn destination_mask_follows_modifiers() {
        let inst = parsed("add.xyz b,a,a");
        assert_eq!(
            instruction(&inst, &physical(&["VF02", "VF01", "VF01"]), 2).unwrap(),
            "add[E].xyz    VF02,VF01,VF01"
        );
    }

    #[test]
    fn combined_stream_flags_match_the_original_string_builder() {
        let inst = parsed("mul b,a,a");
        let emitted = instruction(&inst, &physical(&["VF02", "VF01", "VF01"]), 15).unwrap();
        assert!(emitted.starts_with("mul[DETM]"));
        assert_eq!(modifier_suffix(0), "");
    }

    #[test]
    fn scalar_division_uses_register_lane_suffixes_without_dots() {
        let inst = parsed("div Q,a.x,b.y");
        assert_eq!(
            instruction(&inst, &[None, Some("VF01".into()), Some("VF02".into())], 0).unwrap(),
            "div           Q,VF01x,VF02y"
        );
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn cliplw_uses_special_mnemonic_and_fixed_operand_lanes() {
        let inst = parsed("cliplw.xyz a,b");
        let upper = instruction(&inst, &physical(&["VF01", "VF02"]), 2).unwrap();
        assert_eq!(upper, "clipw[E].xyz  VF01xyz,VF02w");
        assert_eq!(
            pair(&upper, "NOP"),
            crate::test_evidence::text("oracle/emit-cliplw/stdout.bin")
                .as_str()
                .lines()
                .find(|line| line.contains("clipw[E]"))
                .unwrap()
        );
    }

    #[test]
    fn memory_operand_replaces_base_register_preserving_expression() {
        let inst = parsed("lq vf2,label+16(base)");
        assert_eq!(
            instruction(&inst, &[None, Some("VI03".into())], 0).unwrap(),
            "lq            VF02,label+16(VI03)"
        );
        assert_eq!(
            instruction(&parsed("lqi vf2,(vi3++)"), &[], 0).unwrap(),
            "lqi           VF02,(VI03++)"
        );
        assert_eq!(
            instruction(&parsed("sqd vf2,(--vi3)"), &[], 0).unwrap(),
            "sqd           VF02,(--VI03)"
        );
    }

    #[test]
    fn integer_and_branch_immediates_keep_their_source_expression() {
        assert_eq!(
            instruction(&parsed("iaddiu vi2,vi1,0x1+SYMBOL"), &[], 0).unwrap(),
            "iaddiu        VI02,VI01,0x1+SYMBOL"
        );
        assert_eq!(
            instruction(&parsed("ibeq vi1,vi2,Done"), &[], 0).unwrap(),
            "ibeq          VI01,VI02,Done"
        );
    }

    fn oracle_lower(name: &str, mnemonic: &str) -> String {
        let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(name)
            .join("stdout.bin");
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .find(|line| line.len() > 68 && line[68..].starts_with(mnemonic))
            .unwrap()
            .get(68..)
            .unwrap()
            .trim_end()
            .to_string()
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn numeric_immediates_match_saved_original_expression_probes() {
        for (name, expression) in [
            ("expr-precedence", "2+3*4"),
            ("expr-absolute", "#(-3)"),
            ("expr-binary", "0b1010"),
            ("expr-shift-mask", "1<<32"),
            ("expr-float-truncation", "2.9"),
            ("expr-scientific", "1e+2"),
            ("expr-float-underscore", "1_2.5"),
            ("expr-unresolved", "LABEL+2*3"),
            ("expr-fix-iaddiu-32768", "32768"),
            ("expr-fix-iaddiu-neg1", "-1"),
        ] {
            let inst = parsed(&format!("iaddiu vi2,vi1,{expression}"));
            assert_eq!(
                instruction(&inst, &[], 0).unwrap(),
                oracle_lower(name, "iaddiu"),
                "{name}"
            );
        }
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn loi_keeps_float_payloads_and_arithmetic() {
        for (name, expression) in [
            ("expr-loi-hex-float", "0x3f800000"),
            ("expr-float-bits", "0z40000000"),
            ("expr-emit-loi-arithmetic", "2+0.5"),
        ] {
            let inst = parsed(&format!("loi {expression}"));
            assert_eq!(
                instruction(&inst, &[], 0).unwrap(),
                oracle_lower(name, "loi")
            );
        }
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn signed_immediates_and_numeric_memory_use_decimal_rendering() {
        assert_eq!(
            instruction(&parsed("fcand vi1,2+3"), &[], 0).unwrap(),
            oracle_lower("expr-emit-fcand", "fcand")
        );
        assert_eq!(
            instruction(&parsed("lq vf2,0x10+2(vi1)"), &[], 0).unwrap(),
            oracle_lower("expr-emit-memory", "lq")
        );
        assert_eq!(scientific(1.0), "1.000000000e+00");
        assert_eq!(scientific(-0.0), "-0.000000000e+00");
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn normalized_opcode_keeps_the_original_operand_conversion_selector() {
        let mut inst = parsed("iaddi vi2,vi1,3");
        inst.metadata =
            crate::isa::instruction::lookup("iaddiu", crate::isa::instruction::Syntax::Old)
                .unwrap();
        assert_eq!(inst.operands[2].immediate_kind, ImmediateKind::signed(5));
        assert_eq!(
            inst.metadata.operands[2].immediate_kind,
            ImmediateKind::unsigned(15)
        );
        assert_eq!(
            instruction(&inst, &[], 0).unwrap(),
            oracle_lower("expr-fix-iaddi-3", "iaddiu")
        );
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn generated_copies_require_both_provenance_and_a_resolved_output_kind() {
        let mut inst = parsed("imove vi3,vi1");
        inst.generated_copy = Some(crate::analysis::copies::Kind::Integer);
        assert_eq!(
            instruction(&inst, &[], 0).unwrap_err().0,
            "trying to emit an abstract move"
        );
        inst.metadata = &crate::isa::instruction::INSTRUCTIONS[usize::from(id::LIMOVE)];
        assert_eq!(
            instruction(&inst, &[], 0).unwrap(),
            oracle_lower("copy-iadd-zero", "iaddiu")
        );
        inst.generated_copy = None;
        assert_eq!(
            instruction(&inst, &[], 0).unwrap_err().0,
            "baseMuid not set in vmove_Emit"
        );
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn iaddi_collapse_preserves_original_signed_immediate_type() {
        for (name, expression, mnemonic) in [
            ("expr-fix-iaddi-3", "3", "iaddiu"),
            ("expr-fix-iaddi-neg16", "-16", "isubiu"),
            ("expr-emit-iaddi", "-(2+3)", "isubiu"),
        ] {
            let inst = parsed(&format!("iaddi vi2,vi1,{expression}"));
            assert_eq!(
                instruction(&inst, &[], 0).unwrap(),
                oracle_lower(name, mnemonic)
            );
        }
        assert_eq!(
            instruction(&parsed("iaddi vi2,vi1,SYMBOL+1"), &[], 0).unwrap(),
            "iaddi         VI02,VI01,SYMBOL+1"
        );
    }

    #[test]
    fn original_unsupported_numeric_operators_fail_without_losing_symbolic_literals() {
        // The parser rejects them (`ParseMessage::BadImmediate`); emission
        // still refuses one that reaches it.
        for expression in ["2**3", "1||0"] {
            let source = format!("iaddiu vi2,vi1,{expression}");
            let options = ParserOptions {
                allow_incomplete: true,
                ..ParserOptions::default()
            };
            assert!(matches!(
                parse_with_options(&source, options).unwrap_err()[0].message,
                crate::diagnostics::ParseMessage::BadImmediate { .. }
            ));
            let mut inst = parsed("iaddiu vi2,vi1,1");
            inst.operands[2].immediate = Some(expression.into());
            assert!(instruction(&inst, &[], 0).is_err());
        }
        assert_eq!(
            instruction(&parsed("iaddiu vi2,vi1,SYMBOL||0"), &[], 0).unwrap(),
            "iaddiu        VI02,VI01,SYMBOL||0"
        );
    }

    #[test]
    fn generalized_new_syntax_is_already_lowered_by_parser() {
        let program = parse_with_options(
            "add b,a,c[y]",
            ParserOptions {
                syntax: Syntax::New,
                allow_incomplete: true,
                ..Default::default()
            },
        )
        .unwrap();
        let inst = program
            .statements
            .iter()
            .find_map(|statement| match &statement.kind {
                StatementKind::Instruction(inst) => Some(inst),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            instruction(inst, &physical(&["VF02", "VF01", "VF03"]), 0).unwrap(),
            "addy          VF02,VF01,VF03y"
        );
    }

    #[test]
    fn synthetics_and_unlowered_abstract_moves_fail_explicitly() {
        assert!(instruction(&parsed("--enter"), &[], 0).is_err());
        assert_eq!(
            instruction(&parsed("imove vi2,vi1"), &[], 0).unwrap_err().0,
            "baseMuid not set in vmove_Emit"
        );
    }
}
