//! The math functions of openvcl's `loi` expressions, in oxide mode:
//! `loi sin(45 * (pi()/1.8e2))`.
//!
//! vcl 1.4beta7 has no function calls: `loi sin(1)` fails with "failed to
//! parse immediate 'sin(1)'", and `pow(2, 10)` is split at its comma
//! (observed from vcl 1.4beta7). With `--mode=oxide` the parser keeps a
//! parenthesized group in one operand (`ParserOptions::grouped_parentheses`),
//! and each float immediate that calls a function is evaluated here
//! (`expression::FUNCTIONS`) and replaced by its float bits, so the rest of
//! the compiler sees an ordinary `loi`. Expressions without a call are left
//! as they are.

use super::*;
use crate::isa::instruction::ImmediateType;
use crate::parser::expression::{self, Evaluation, ExpressionOptions};

/// Replaces each float immediate with a function call by its value.
pub(super) fn fold(program: &mut parser::Program) -> Result<(), CompileError> {
    for statement in &mut program.statements {
        let StatementKind::Instruction(instruction) = &mut statement.kind else {
            continue;
        };
        for operand in &mut instruction.operands {
            if operand.immediate_kind.value_type() != Some(ImmediateType::Float) {
                continue;
            }
            let Some(text) = operand.immediate.as_deref() else {
                continue;
            };
            let Ok(parsed) = expression::parse_with_functions(text) else {
                continue;
            };
            if !parsed.has_call() {
                continue;
            }
            let options = ExpressionOptions {
                hex_is_float: true,
                functions: true,
            };
            let failed = |detail: String| {
                error(
                    statement.line,
                    CompileMessage::LoiFunction {
                        expression: text.to_string(),
                        detail,
                    },
                )
            };
            let value = match parsed.evaluate(options) {
                Ok(Evaluation::Number(number)) => number.to_f32(),
                Ok(Evaluation::Unresolved { symbols, .. }) => {
                    return Err(failed(format!(
                        "a function's arguments must be numbers, and '{}' is not",
                        symbols.join("', '")
                    )));
                }
                Err(failure) => return Err(failed(failure.message)),
            };
            // `0z` literals are float bits in every immediate.
            operand.immediate = Some(format!("0z{:08x}", value.to_bits()));
        }
    }
    Ok(())
}
