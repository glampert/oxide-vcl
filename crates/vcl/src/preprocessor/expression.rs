//! Integer constant expressions, as `#if`, masp's `.aif`, `.awhile`,
//! `.assigna`, `.equ` and `.arepeat`, and `--fold-constants` read them:
//! numbers, the C operators and parentheses, worked out in 64-bit integers.
//!
//! The operators work as in C:
//!
//! - **Operators and precedence.** The binary operators are C's, at C's
//!   precedence: `* / %`, then `+ -`, `<< >>`, `< <= > >=`, `== !=`, `&`,
//!   `^`, `|`, `&&` and `||`, all grouping to the left. The unary operators
//!   are `+ - ~ !`.
//! - **`?:`.** C's conditional operator, below `||` and grouping to the
//!   right: `a ? b : c ? d : e` is `a ? b : (c ? d : e)`. Either branch can be
//!   any expression, another `?:` included. A `?` needs its `:`.
//! - **Unevaluated operands.** The right side of `&&` when the left is 0, of
//!   `||` when it is not, and the branch of `?:` not taken are checked but not
//!   evaluated, so `0 && 1 / 0` is 0, not a division by zero.
//! - **Arithmetic.** Overflow wraps, and so do literals too large for 64
//!   bits. A shift amount counts modulo 64, so `1 << 64` is 1 and `1 >> -1`
//!   is 0. Division and remainder truncate toward zero.
//! - **Numbers.** `0x` is hex, `0b` binary, and a leading `0` octal; `0x` and
//!   `0b` with no digits are 0. An octal number with an 8 or 9 in it is a
//!   syntax error.
//!
//! vclpp 2 evaluates `?` and `:` as two left-grouping operators that share
//! one pending condition, and evaluates every operand; the port follows C
//! instead (docs/PREPROCESSOR.md, "Differences from vclpp"). The error
//! messages are vclpp's.

use super::lexer::{Punct, Token, TokenKind};

/// Why an expression has no value. The texts are vclpp's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpressionError {
    /// No tokens at all.
    Empty,
    /// A `(` without its `)`.
    UnclosedParenthesis,
    /// A `)` without its `(`.
    UnopenedParenthesis,
    /// Two values in a row, or a value right before `(`.
    Syntax,
    /// A binary operator where a value should be.
    OperatorAfterOperator {
        operator: String,
    },
    /// `!` or `~` right after a value.
    NotAfterValue,
    /// Punctuation that is no operator here, like `=` or `,`.
    InvalidOperator {
        operator: String,
    },
    /// An operator or `(` with nothing after it.
    TrailingOperator,
    DivisionByZero,
    /// A `:` that belongs to no `?`.
    ColonWithoutQuestion,
    /// A `?` without its `:`.
    QuestionWithoutColon,
    /// A token that is not a number or punctuation, or a float.
    NotAnInteger {
        token: String,
    },
}

impl std::fmt::Display for ExpressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ExpressionError::*;
        match self {
            Empty => f.write_str("missing expression"),
            UnclosedParenthesis => f.write_str("too many '(' in preprocessor directive"),
            UnopenedParenthesis => f.write_str("too many ')' in preprocessor directive"),
            Syntax => f.write_str("syntax error in preprocessor expression"),
            OperatorAfterOperator { operator } => write!(
                f,
                "invalid operator '{operator}' after operator in preprocessor expression"
            ),
            NotAfterValue => f.write_str(
                "invalid logic not or two's complement after value in preprocessor expression",
            ),
            InvalidOperator { operator } => {
                write!(
                    f,
                    "invalid operator '{operator}' in preprocessor expression"
                )
            }
            TrailingOperator => f.write_str("trailing operator in preprocessor expression"),
            DivisionByZero => f.write_str("integer division by zero in preprocessor expression"),
            ColonWithoutQuestion => f.write_str("':' without '?' in preprocessor directive"),
            QuestionWithoutColon => f.write_str("'?' without ':' in preprocessor directive"),
            NotAnInteger { token } => write!(f, "'{token}' cannot appear in an integer expression"),
        }
    }
}

/// The value of the integer expression `tokens` spell.
pub fn evaluate(tokens: &[Token]) -> Result<i64, ExpressionError> {
    if tokens.is_empty() {
        return Err(ExpressionError::Empty);
    }
    check(tokens)?;
    let mut evaluator = Evaluator { tokens, next: 0 };
    let value = evaluator.conditional(true)?;
    // What is left can only be a `:` that closes no `?`.
    if evaluator.next < tokens.len() {
        return Err(ExpressionError::ColonWithoutQuestion);
    }
    Ok(value)
}

/// The binary operators by precedence, lowest first. `?` and `:` are not
/// among them: [`Evaluator::conditional`] reads them.
fn precedence(punct: Punct) -> Option<u8> {
    use Punct::*;
    Some(match punct {
        LogicOr => 2,
        LogicAnd => 3,
        BitOr => 4,
        BitXor => 5,
        BitAnd => 6,
        Equal | NotEqual => 7,
        Less | LessEqual | Greater | GreaterEqual => 8,
        ShiftLeft | ShiftRight => 9,
        Add | Sub => 10,
        Mul | Div | Mod => 11,
        _ => return Option::None,
    })
}

/// The order of the tokens: values and operators where each belongs, and the
/// parentheses balanced.
fn check(tokens: &[Token]) -> Result<(), ExpressionError> {
    let mut want_value = true;
    let mut depth = 0usize;
    for token in tokens {
        match token.kind {
            TokenKind::Number if !token.is_float => {
                if !want_value {
                    return Err(ExpressionError::Syntax);
                }
                want_value = false;
            }
            TokenKind::Punctuation => match token.punct {
                Punct::OpenParen => {
                    if !want_value {
                        return Err(ExpressionError::Syntax);
                    }
                    depth += 1;
                }
                Punct::CloseParen => {
                    if depth == 0 {
                        return Err(ExpressionError::UnopenedParenthesis);
                    }
                    if want_value {
                        return Err(ExpressionError::TrailingOperator);
                    }
                    depth -= 1;
                }
                Punct::Add | Punct::Sub => want_value = true,
                Punct::LogicNot | Punct::BitNot => {
                    if !want_value {
                        return Err(ExpressionError::NotAfterValue);
                    }
                }
                punct
                    if precedence(punct).is_some()
                        || matches!(punct, Punct::Question | Punct::Colon) =>
                {
                    if want_value {
                        return Err(ExpressionError::OperatorAfterOperator {
                            operator: token.text.clone(),
                        });
                    }
                    want_value = true;
                }
                _ => {
                    return Err(ExpressionError::InvalidOperator {
                        operator: token.text.clone(),
                    });
                }
            },
            _ => {
                return Err(ExpressionError::NotAnInteger {
                    token: token.text.clone(),
                });
            }
        }
    }
    if want_value {
        return Err(ExpressionError::TrailingOperator);
    }
    if depth > 0 {
        return Err(ExpressionError::UnclosedParenthesis);
    }
    Ok(())
}

/// Works out checked tokens by recursive descent. `live` is false inside an
/// operand C does not evaluate: its syntax still counts, but it divides by
/// zero without error.
struct Evaluator<'a> {
    tokens: &'a [Token],
    next: usize,
}

impl Evaluator<'_> {
    fn is_next(&self, punct: Punct) -> bool {
        self.tokens
            .get(self.next)
            .is_some_and(|token| token.is(punct))
    }

    /// `condition ? then : else`, or a binary expression on its own.
    fn conditional(&mut self, live: bool) -> Result<i64, ExpressionError> {
        let condition = self.binary(0, live)?;
        if !self.is_next(Punct::Question) {
            return Ok(condition);
        }
        self.next += 1;
        let then = self.conditional(live && condition != 0)?;
        if !self.is_next(Punct::Colon) {
            return Err(ExpressionError::QuestionWithoutColon);
        }
        self.next += 1;
        let otherwise = self.conditional(live && condition == 0)?;
        Ok(if condition != 0 { then } else { otherwise })
    }

    fn binary(&mut self, min_precedence: u8, live: bool) -> Result<i64, ExpressionError> {
        let mut left = self.unary(live)?;
        while let Some(token) = self.tokens.get(self.next) {
            let Some(precedence) = precedence(token.punct).filter(|&p| p > min_precedence) else {
                break;
            };
            let punct = token.punct;
            self.next += 1;
            // `&&` and `||` evaluate their right side only when the left one
            // does not decide.
            let right_live = match punct {
                Punct::LogicAnd => live && left != 0,
                Punct::LogicOr => live && left == 0,
                _ => live,
            };
            let right = self.binary(precedence, right_live)?;
            left = apply(punct, left, right, live)?;
        }
        Ok(left)
    }

    fn unary(&mut self, live: bool) -> Result<i64, ExpressionError> {
        let token = &self.tokens[self.next];
        self.next += 1;
        match token.punct {
            Punct::Add => self.unary(live),
            Punct::Sub => Ok(self.unary(live)?.wrapping_neg()),
            Punct::BitNot => Ok(!self.unary(live)?),
            Punct::LogicNot => Ok(i64::from(self.unary(live)? == 0)),
            Punct::OpenParen => {
                let value = self.conditional(live)?;
                if self.is_next(Punct::Colon) {
                    return Err(ExpressionError::ColonWithoutQuestion);
                }
                // check() has balanced the parentheses.
                self.next += 1;
                Ok(value)
            }
            _ => number_value(&token.text),
        }
    }
}

/// A binary operator's value. Dividing by zero is an error only where the
/// operand is evaluated (`live`).
fn apply(punct: Punct, left: i64, right: i64, live: bool) -> Result<i64, ExpressionError> {
    use Punct::*;
    let truth = |value: bool| i64::from(value);
    Ok(match punct {
        LogicOr => truth(left != 0 || right != 0),
        LogicAnd => truth(left != 0 && right != 0),
        BitOr => left | right,
        BitXor => left ^ right,
        BitAnd => left & right,
        Equal => truth(left == right),
        NotEqual => truth(left != right),
        Less => truth(left < right),
        LessEqual => truth(left <= right),
        Greater => truth(left > right),
        GreaterEqual => truth(left >= right),
        // The amount counts modulo 64, as wrapping shifts do.
        ShiftLeft => left.wrapping_shl(right as u32),
        ShiftRight => left.wrapping_shr(right as u32),
        Add => left.wrapping_add(right),
        Sub => left.wrapping_sub(right),
        Mul => left.wrapping_mul(right),
        Div | Mod if right == 0 && live => return Err(ExpressionError::DivisionByZero),
        Div | Mod if right == 0 => 0,
        Div => left.wrapping_div(right),
        Mod => left.wrapping_rem(right),
        _ => unreachable!("check() lets only binary operators through"),
    })
}

/// An integer literal's value, wrapping to 64 bits; its suffix is ignored.
fn number_value(text: &str) -> Result<i64, ExpressionError> {
    let digits = text.trim_end_matches(['u', 'U', 'l', 'L']);
    let lower = digits.to_ascii_lowercase();
    let (radix, digits) = if let Some(hex) = lower.strip_prefix("0x") {
        (16, hex)
    } else if let Some(binary) = lower.strip_prefix("0b") {
        (2, binary)
    } else if lower.len() > 1 && lower.starts_with('0') {
        (8, &lower[1..])
    } else {
        (10, lower.as_str())
    };
    let mut value = 0u64;
    for c in digits.chars() {
        let digit = c.to_digit(radix).ok_or(ExpressionError::Syntax)?;
        value = value
            .wrapping_mul(u64::from(radix))
            .wrapping_add(u64::from(digit));
    }
    Ok(value as i64)
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocessor::lexer::tokenize;

    fn eval(text: &str) -> Result<i64, ExpressionError> {
        evaluate(&tokenize(text, 0, 1).unwrap().tokens)
    }

    #[test]
    fn operators_follow_cs_precedence() {
        for (text, value) in [
            ("2 + 3 * 4", 14),
            ("10 - 4 - 3", 3),
            ("4 / 2 / 2", 1),
            ("2 * 3 % 4", 2),
            ("(2 + 3) * 4", 20),
            ("5 & 3 ^ 6 | 8", 15),
            ("1 << 4 | 1", 17),
            ("3 > 2 > 1", 0),
            ("1 == 1 == 1", 1),
            ("1 < 2 == 1", 1),
            ("1 & 1 == 1", 1),
            ("- ~ ! 0", 2),
            ("- - 3", 3),
            ("+3", 3),
            ("1 + + 2", 3),
            ("1 - -2", 3),
            ("1 * -2", -2),
            ("!!1", 1),
            ("!5", 0),
            ("~0", -1),
            ("1 || 0 && 0", 1),
            ("-1 >> 1", -1),
            ("-7 / 2", -3),
            ("-7 % 2", -1),
            ("6 % -4", 2),
            ("-6 % 4", -2),
        ] {
            assert_eq!(eval(text), Ok(value), "{text}");
        }
    }

    #[test]
    fn arithmetic_wraps_and_shifts_count_modulo_64() {
        for (text, value) in [
            ("9223372036854775807 + 1", i64::MIN),
            ("(-9223372036854775807 - 1) / -1", i64::MIN),
            ("1 << 63", i64::MIN),
            ("1 << 64", 1),
            ("1 << 65", 2),
            ("1 >> -1", 0),
            ("1 << -1", i64::MIN),
            ("18446744073709551615", -1),
            ("0xFFFFFFFFFFFFFFFF", -1),
        ] {
            assert_eq!(eval(text), Ok(value), "{text}");
        }
        assert!(eval("99999999999999999999").unwrap() > 0);
    }

    #[test]
    fn numbers_read_in_their_base() {
        for (text, value) in [
            ("010", 8),
            ("0x10", 16),
            ("0b11", 3),
            ("3u", 3),
            ("0x1ful", 31),
            ("2l", 2),
            ("0b", 0),
            ("0x", 0),
            ("0", 0),
            ("07", 7),
        ] {
            assert_eq!(eval(text), Ok(value), "{text}");
        }
        for text in ["08", "019"] {
            assert_eq!(eval(text), Err(ExpressionError::Syntax), "{text}");
        }
    }

    #[test]
    fn conditionals_work_as_in_c() {
        for (text, value) in [
            ("1 ? 2 : 3", 2),
            ("0 ? 2 : 3", 3),
            // They group to the right.
            ("1 ? 0 : 1 ? 1 : 1", 0),
            ("1 ? 1 : 0 ? 0 : 0", 1),
            ("0 ? 2 : 3 ? 0 : 5", 0),
            ("0 ? 1 : 0 ? 1 : 0 ? 1 : 1", 1),
            ("0 ? 1 : 0 ? 2 : 3", 3),
            // Either branch can be another one.
            ("2 ? 3 ? 4 : 5 : 6", 4),
            ("2 ? 0 ? 4 : 5 : 6", 5),
            ("1 ? 2 : (3 ? 4 : 5)", 2),
            ("0 ? 2 : (3 ? 4 : 5)", 4),
            ("1 ? 2 : 3 + (4 ? 5 : 6)", 2),
            ("0 ? 2 : 3 + (4 ? 5 : 6)", 8),
            ("1 ? (2 ? 3 : 4) : (5 ? 6 : 7)", 3),
            ("1 ? (2 ? 3 : 4) : 5", 3),
            ("(1 ? 2 : 3) + (4 ? 5 : 6)", 7),
            ("(1 ? 0 : 3) ? 4 : 0", 0),
            ("1 ? ((0 ? 1 : 0)) : 1", 0),
            ("(2 ? 3 : 4) * (0 ? 5 : 6)", 18),
            // Below `||`, above nothing.
            ("1 || 0 ? 0 : 6", 0),
            ("0 ? 1 : 0 || 1", 1),
            ("1 ? 2 : 3 == 3", 2),
        ] {
            assert_eq!(eval(text), Ok(value), "{text}");
        }
        for text in ["1 ? 2", "0 ? 2", "1 ? 2 ? 3 : 4", "(1 ? 2) : 3"] {
            assert_eq!(
                eval(text),
                Err(ExpressionError::QuestionWithoutColon),
                "{text}"
            );
        }
        for text in ["1 ? 2 : 3 : 4", "1 : 2", "(1 : 2)"] {
            assert_eq!(
                eval(text),
                Err(ExpressionError::ColonWithoutQuestion),
                "{text}"
            );
        }
    }

    #[test]
    fn operands_c_does_not_evaluate_are_only_checked() {
        for (text, value) in [
            ("1 || 1 / 0", 1),
            ("0 && 1 / 0", 0),
            ("0 && (1 % 0 || 1 / 0)", 0),
            ("0 ? 1 / 0 : 2", 2),
            ("1 ? 2 : 1 / 0", 2),
            ("1 ? 2 : 0 ? 1 / 0 : 3", 2),
            ("0 || 0 && 1 / 0", 0),
        ] {
            assert_eq!(eval(text), Ok(value), "{text}");
        }
        for text in [
            "1 / 0 * 0",
            "1 % 0",
            "0 || 1 / 0",
            "1 && 1 % 0",
            "1 ? 1 / 0 : 2",
        ] {
            assert_eq!(eval(text), Err(ExpressionError::DivisionByZero), "{text}");
        }
        // Their syntax still counts.
        assert_eq!(eval("0 && (1 +)"), Err(ExpressionError::TrailingOperator));
        assert_eq!(eval("1 || 2 3"), Err(ExpressionError::Syntax));
    }

    #[test]
    fn misplaced_tokens_are_errors() {
        use ExpressionError::*;
        let operator = |text: &str| text.to_string();
        for (text, error) in [
            ("(1", UnclosedParenthesis),
            ("((1)", UnclosedParenthesis),
            ("1 + (2", UnclosedParenthesis),
            ("1)", UnopenedParenthesis),
            (")", UnopenedParenthesis),
            ("1 2", Syntax),
            ("1 2 3", Syntax),
            ("(1)(2)", Syntax),
            ("1 (2)", Syntax),
            (
                "* 1",
                OperatorAfterOperator {
                    operator: operator("*"),
                },
            ),
            (
                "?",
                OperatorAfterOperator {
                    operator: operator("?"),
                },
            ),
            (
                "1 + * 2",
                OperatorAfterOperator {
                    operator: operator("*"),
                },
            ),
            (
                "1 ? : 2",
                OperatorAfterOperator {
                    operator: operator(":"),
                },
            ),
            ("1 ~ 2", NotAfterValue),
            ("1 ! 2", NotAfterValue),
            (
                "1 = 2",
                InvalidOperator {
                    operator: operator("="),
                },
            ),
            (
                "1 ++ 2",
                InvalidOperator {
                    operator: operator("++"),
                },
            ),
            (
                "1 += 2",
                InvalidOperator {
                    operator: operator("+="),
                },
            ),
            (
                "1 , 2",
                InvalidOperator {
                    operator: operator(","),
                },
            ),
            ("1 +", TrailingOperator),
            ("1 ?", TrailingOperator),
            ("1 ? 2 :", TrailingOperator),
            ("~", TrailingOperator),
            ("!", TrailingOperator),
            ("(", TrailingOperator),
            ("()", TrailingOperator),
            (
                "1.5",
                NotAnInteger {
                    token: operator("1.5"),
                },
            ),
            (
                "a",
                NotAnInteger {
                    token: operator("a"),
                },
            ),
            ("", Empty),
        ] {
            assert_eq!(eval(text), Err(error), "{text}");
        }
    }

    #[test]
    fn errors_read_as_vclpps() {
        assert_eq!(
            ExpressionError::DivisionByZero.to_string(),
            "integer division by zero in preprocessor expression"
        );
        assert_eq!(
            ExpressionError::TrailingOperator.to_string(),
            "trailing operator in preprocessor expression"
        );
    }
}
