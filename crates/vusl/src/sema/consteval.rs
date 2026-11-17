//! Compile-time constants: literals, `const` declarations and any operator
//! on them. They cost no instructions, so every C operator applies, `*` and
//! `<<` on integers included. Integers are 64-bit here and are checked
//! against their type's range where they are used; floats are `f32`, in
//! IEEE single precision (the VU is not IEEE, which the docs note).

use super::hir::Const;
use crate::syntax::ast::{BinaryOp, UnaryOp};

/// Why two constants do not combine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FoldError {
    DivisionByZero,
    /// An operator that does not apply to these constants' types.
    Mismatch,
}

pub fn unary(op: UnaryOp, value: Const) -> Result<Const, FoldError> {
    match (op, value) {
        (UnaryOp::Plus, value) => Ok(value),
        (UnaryOp::Neg, Const::Int(n)) => Ok(Const::Int(n.wrapping_neg())),
        (UnaryOp::Neg, Const::Float(f)) => Ok(Const::Float(-f)),
        (UnaryOp::Not, Const::Bool(b)) => Ok(Const::Bool(!b)),
        (UnaryOp::BitNot, Const::Int(n)) => Ok(Const::Int(!n)),
        _ => Err(FoldError::Mismatch),
    }
}

/// An integer constant combined with a float one is a float.
fn as_float(value: Const) -> Option<f32> {
    match value {
        Const::Int(n) => Some(n as f32),
        Const::Float(f) => Some(f),
        Const::Bool(_) => None,
    }
}

pub fn binary(op: BinaryOp, lhs: Const, rhs: Const) -> Result<Const, FoldError> {
    use BinaryOp::*;
    match (lhs, rhs) {
        (Const::Int(a), Const::Int(b)) => {
            let value = match op {
                Add => a.wrapping_add(b),
                Sub => a.wrapping_sub(b),
                Mul => a.wrapping_mul(b),
                Div | Mod if b == 0 => return Err(FoldError::DivisionByZero),
                Div => a.wrapping_div(b),
                Mod => a.wrapping_rem(b),
                BitAnd => a & b,
                BitOr => a | b,
                BitXor => a ^ b,
                Shl => a.wrapping_shl((b & 63) as u32),
                Shr => a.wrapping_shr((b & 63) as u32),
                Less => return Ok(Const::Bool(a < b)),
                Greater => return Ok(Const::Bool(a > b)),
                LessEqual => return Ok(Const::Bool(a <= b)),
                GreaterEqual => return Ok(Const::Bool(a >= b)),
                Equal => return Ok(Const::Bool(a == b)),
                NotEqual => return Ok(Const::Bool(a != b)),
                LogicAnd | LogicOr => return Err(FoldError::Mismatch),
            };
            Ok(Const::Int(value))
        }
        (Const::Bool(a), Const::Bool(b)) => match op {
            LogicAnd => Ok(Const::Bool(a && b)),
            LogicOr => Ok(Const::Bool(a || b)),
            Equal => Ok(Const::Bool(a == b)),
            NotEqual => Ok(Const::Bool(a != b)),
            _ => Err(FoldError::Mismatch),
        },
        _ => {
            let (Some(a), Some(b)) = (as_float(lhs), as_float(rhs)) else {
                return Err(FoldError::Mismatch);
            };
            Ok(match op {
                Add => Const::Float(a + b),
                Sub => Const::Float(a - b),
                Mul => Const::Float(a * b),
                Div if b == 0.0 => return Err(FoldError::DivisionByZero),
                Div => Const::Float(a / b),
                Less => Const::Bool(a < b),
                Greater => Const::Bool(a > b),
                LessEqual => Const::Bool(a <= b),
                GreaterEqual => Const::Bool(a >= b),
                Equal => Const::Bool(a == b),
                NotEqual => Const::Bool(a != b),
                _ => return Err(FoldError::Mismatch),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_take_every_operator() {
        assert_eq!(
            binary(BinaryOp::Mul, Const::Int(6), Const::Int(7)),
            Ok(Const::Int(42))
        );
        assert_eq!(
            binary(BinaryOp::Shl, Const::Int(1), Const::Int(15)),
            Ok(Const::Int(0x8000))
        );
        assert_eq!(
            binary(BinaryOp::Div, Const::Float(1.0), Const::Int(4)),
            Ok(Const::Float(0.25))
        );
        assert_eq!(
            binary(BinaryOp::Less, Const::Int(1), Const::Int(2)),
            Ok(Const::Bool(true))
        );
        assert_eq!(
            binary(BinaryOp::Mod, Const::Int(1), Const::Int(0)),
            Err(FoldError::DivisionByZero)
        );
        assert_eq!(
            binary(BinaryOp::BitAnd, Const::Float(1.0), Const::Int(1)),
            Err(FoldError::Mismatch)
        );
        assert_eq!(
            unary(UnaryOp::Neg, Const::Int(32768)),
            Ok(Const::Int(-32768))
        );
        assert_eq!(unary(UnaryOp::Not, Const::Int(1)), Err(FoldError::Mismatch));
    }
}
