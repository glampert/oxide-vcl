//! VCL immediate expressions recovered from the supplied ELF.
//!
//! Unresolved operands preserve the complete expression for the assembler.
//! Integer arithmetic uses the original 32-bit representation; division traps
//! become explicit errors instead of crashing the Rust process.

use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Number {
    Unsigned(u32),
    Signed(i32),
    Float(f32),
}

impl Number {
    /// Raw storage payload, including IEEE-754 bits for a floating value.
    pub fn bits(self) -> u32 {
        match self {
            Self::Unsigned(value) => value,
            Self::Signed(value) => value as u32,
            Self::Float(value) => value.to_bits(),
        }
    }

    fn type_id(self) -> u8 {
        match self {
            Self::Unsigned(_) => 1,
            Self::Signed(_) => 2,
            Self::Float(_) => 3,
        }
    }

    /// Signed conversion follows x87 truncation toward zero. Its indefinite
    /// result for NaN/out-of-range conversion is preserved as i32::MIN.
    pub fn to_i32(self) -> i32 {
        match self {
            Self::Unsigned(value) => value as i32,
            Self::Signed(value) => value,
            Self::Float(value) => {
                let value = f64::from(value).trunc();
                if value.is_finite() && (-2_147_483_648.0..2_147_483_648.0).contains(&value) {
                    value as i32
                } else {
                    i32::MIN
                }
            }
        }
    }

    /// The unsigned converter uses FISTP to an i64 temporary and takes its low
    /// 32 bits. This differs from Rust's saturating float-to-u32 conversion.
    pub fn to_u32(self) -> u32 {
        match self {
            Self::Unsigned(value) => value,
            Self::Signed(value) => value as u32,
            Self::Float(value) => {
                let value = f64::from(value).trunc();
                if value.is_finite()
                    && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value)
                {
                    value as i64 as u32
                } else {
                    0
                }
            }
        }
    }

    pub fn to_f32(self) -> f32 {
        match self {
            Self::Unsigned(value) => value as f32,
            Self::Signed(value) => value as f32,
            Self::Float(value) => value,
        }
    }

    fn to_f64(self) -> f64 {
        match self {
            Self::Unsigned(value) => f64::from(value),
            Self::Signed(value) => f64::from(value),
            Self::Float(value) => f64::from(value),
        }
    }

    fn from_bits(type_id: u8, value: u32) -> Self {
        match type_id {
            1 => Self::Unsigned(value),
            2 => Self::Signed(value as i32),
            _ => Self::Float(f32::from_bits(value)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Evaluation {
    Number(Number),
    /// All arithmetic is deferred when any leaf is unresolved. The original
    /// does not partially fold `label + 2 * 3` into `label + 6`.
    Unresolved {
        source: String,
        symbols: Vec<String>,
    },
}

impl Evaluation {
    pub fn number(&self) -> Option<Number> {
        match self {
            Self::Number(number) => Some(*number),
            Self::Unresolved { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpressionOptions {
    /// LOI's immediate selector (0x40) interprets 0x literals as float bits.
    /// Ordinary integer immediates keep the default unsigned interpretation.
    pub hex_is_float: bool,
    /// Function calls (`sin(x)`, `pow(x, y)`, `pi()`): the math functions of
    /// openvcl's `loi` expressions, which vcl 1.4beta7 does not have.
    pub functions: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Plus,
    Minus,
    Complement,
    LogicalNot,
    Absolute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    ShiftLeft,
    ShiftRight,
    BitAnd,
    BitXor,
    BitOr,
    LogicalAnd,
    /// Present in the token table with opcode 14, absent from binary eval.
    LogicalOr,
    /// Present in the token table with opcode 20, absent from binary eval.
    Power,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Literal(String),
    /// A function call, with `ExpressionOptions::functions`.
    Call {
        name: String,
        arguments: Vec<Expr>,
    },
    Unary {
        operation: Unary,
        operand: Box<Expr>,
    },
    Binary {
        operation: Binary,
        left: Box<Expr>,
        right: Box<Expr>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expression {
    pub source: String,
    pub tree: Expr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpressionError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for ExpressionError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "expression offset {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ExpressionError {}

fn failure(offset: usize, message: impl Into<String>) -> ExpressionError {
    ExpressionError {
        offset,
        message: message.into(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Literal(String),
    Operator(String),
    Open,
    Close,
    /// An argument separator, with `ExpressionOptions::functions`.
    Comma,
}

/// Parse the executable's precedence table. Equal-precedence binary operators
/// associate to the left, including the recognized but unevaluable power op.
pub fn parse(source: &str) -> Result<Expression, ExpressionError> {
    parse_with(source, false)
}

/// `parse`, also reading function calls (`ExpressionOptions::functions`).
pub fn parse_with_functions(source: &str) -> Result<Expression, ExpressionError> {
    parse_with(source, true)
}

fn parse_with(source: &str, functions: bool) -> Result<Expression, ExpressionError> {
    if !source.is_ascii() {
        return Err(failure(0, "non-ASCII expression"));
    }
    let source: String = source
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let tokens = lex(&source, functions);
    if tokens.is_empty() {
        return Err(failure(0, "empty expression"));
    }
    let mut cursor = 0;
    let tree = parse_binary(&tokens, &mut cursor, 0, 0, functions)?;
    if cursor != tokens.len() {
        return Err(failure(
            cursor,
            "unexpected token or unbalanced parentheses",
        ));
    }
    Ok(Expression { source, tree })
}

pub fn evaluate(source: &str) -> Result<Evaluation, ExpressionError> {
    evaluate_with_options(source, ExpressionOptions::default())
}

pub fn evaluate_with_options(
    source: &str,
    options: ExpressionOptions,
) -> Result<Evaluation, ExpressionError> {
    parse_with(source, options.functions)?.evaluate(options)
}

impl Expression {
    pub fn evaluate(&self, options: ExpressionOptions) -> Result<Evaluation, ExpressionError> {
        let mut symbols = BTreeSet::new();
        unknowns(&self.tree, options, &mut symbols);
        if !symbols.is_empty() {
            return Ok(Evaluation::Unresolved {
                source: self.source.clone(),
                symbols: symbols.into_iter().collect(),
            });
        }
        numeric(&self.tree, options).map(Evaluation::Number)
    }

    /// Whether the expression calls a function.
    pub fn has_call(&self) -> bool {
        fn calls(tree: &Expr) -> bool {
            match tree {
                Expr::Literal(_) => false,
                Expr::Call { .. } => true,
                Expr::Unary { operand, .. } => calls(operand),
                Expr::Binary { left, right, .. } => calls(left) || calls(right),
            }
        }
        calls(&self.tree)
    }
}

fn lex(source: &str, functions: bool) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < source.len() {
        let tail = &source[position..];
        if tail.starts_with('(') {
            tokens.push(Token::Open);
            position += 1;
            continue;
        }
        if tail.starts_with(')') {
            tokens.push(Token::Close);
            position += 1;
            continue;
        }
        if functions && tail.starts_with(',') {
            tokens.push(Token::Comma);
            position += 1;
            continue;
        }
        if let Some(operator) = operator_at(tail) {
            tokens.push(Token::Operator(operator.into()));
            position += operator.len();
            continue;
        }
        let start = position;
        position += 1;
        while position < source.len() {
            let byte = source.as_bytes()[position];
            if matches!(byte, b'(' | b')') || (functions && byte == b',') {
                break;
            }
            let exponent_sign = matches!(byte, b'+' | b'-')
                && source.as_bytes()[position - 1] == b'e'
                && source[start..position - 1]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'_'));
            if !exponent_sign && operator_at(&source[position..]).is_some() {
                break;
            }
            position += 1;
        }
        tokens.push(Token::Literal(source[start..position].into()));
    }
    tokens
}

fn operator_at(source: &str) -> Option<&'static str> {
    // Ordering is the literal table's longest-match ordering.
    [
        "**", "&&", "||", "<<", ">>", "~", "!", "+", "-", "*", "/", "%", "#", "&", "^", "|",
    ]
    .into_iter()
    .find(|operator| source.starts_with(operator))
}

fn binary(operator: &str) -> Option<(Binary, u8)> {
    Some(match operator {
        "**" => (Binary::Power, 26),
        "*" => (Binary::Multiply, 25),
        "/" => (Binary::Divide, 25),
        "%" => (Binary::Remainder, 25),
        "+" => (Binary::Add, 20),
        "-" => (Binary::Subtract, 20),
        "<<" => (Binary::ShiftLeft, 19),
        ">>" => (Binary::ShiftRight, 19),
        "&" => (Binary::BitAnd, 18),
        "^" => (Binary::BitXor, 17),
        "|" => (Binary::BitOr, 16),
        "&&" => (Binary::LogicalAnd, 15),
        "||" => (Binary::LogicalOr, 14),
        _ => return None,
    })
}

fn parse_binary(
    tokens: &[Token],
    cursor: &mut usize,
    minimum: u8,
    depth: u16,
    functions: bool,
) -> Result<Expr, ExpressionError> {
    if depth >= 255 {
        return Err(failure(
            *cursor,
            "expression nesting exceeds original byte counter",
        ));
    }
    let mut left = match tokens.get(*cursor) {
        Some(Token::Literal(name))
            if functions
                && tokens.get(*cursor + 1) == Some(&Token::Open)
                && is_function_name(name) =>
        {
            *cursor += 2;
            let mut arguments = Vec::new();
            if tokens.get(*cursor) == Some(&Token::Close) {
                *cursor += 1;
            } else {
                loop {
                    arguments.push(parse_binary(tokens, cursor, 0, depth + 1, functions)?);
                    match tokens.get(*cursor) {
                        Some(Token::Comma) => *cursor += 1,
                        Some(Token::Close) => {
                            *cursor += 1;
                            break;
                        }
                        _ => return Err(failure(*cursor, "missing closing parenthesis")),
                    }
                }
            }
            Expr::Call {
                name: name.clone(),
                arguments,
            }
        }
        Some(Token::Literal(literal)) => {
            *cursor += 1;
            Expr::Literal(literal.clone())
        }
        Some(Token::Open) => {
            *cursor += 1;
            let expression = parse_binary(tokens, cursor, 0, depth + 1, functions)?;
            if tokens.get(*cursor) != Some(&Token::Close) {
                return Err(failure(*cursor, "missing closing parenthesis"));
            }
            *cursor += 1;
            expression
        }
        Some(Token::Operator(operator)) => {
            let operation = match operator.as_str() {
                "+" => Unary::Plus,
                "-" => Unary::Minus,
                "~" => Unary::Complement,
                "!" => Unary::LogicalNot,
                "#" => Unary::Absolute,
                _ => return Err(failure(*cursor, "binary operator has no left operand")),
            };
            *cursor += 1;
            Expr::Unary {
                operation,
                operand: Box::new(parse_binary(tokens, cursor, 30, depth + 1, functions)?),
            }
        }
        _ => return Err(failure(*cursor, "expected an expression operand")),
    };
    while let Some(Token::Operator(operator)) = tokens.get(*cursor) {
        let Some((operation, precedence)) = binary(operator) else {
            break;
        };
        if precedence < minimum {
            break;
        }
        *cursor += 1;
        let right = parse_binary(tokens, cursor, precedence + 1, depth + 1, functions)?;
        left = Expr::Binary {
            operation,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

/// A name a call can have: a letter or `_`, then letters, digits and `_`.
fn is_function_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// The functions of openvcl's `loi` expressions: name, argument count and
/// the function, in double precision. `atan2(y, x)` takes its arguments in
/// C's order (observed from openvcl: `atan2(1, 2)` is `atan(1 / 2)`).
/// A function's name, argument count and definition.
type Function = (&'static str, usize, fn(&[f64]) -> f64);

const FUNCTIONS: [Function; 17] = [
    ("abs", 1, |x| x[0].abs()),
    ("exp", 1, |x| x[0].exp()),
    ("sin", 1, |x| x[0].sin()),
    ("cos", 1, |x| x[0].cos()),
    ("tan", 1, |x| x[0].tan()),
    ("sinh", 1, |x| x[0].sinh()),
    ("cosh", 1, |x| x[0].cosh()),
    ("tanh", 1, |x| x[0].tanh()),
    ("asin", 1, |x| x[0].asin()),
    ("acos", 1, |x| x[0].acos()),
    ("atan", 1, |x| x[0].atan()),
    ("atan2", 2, |x| x[0].atan2(x[1])),
    ("pow", 2, |x| x[0].powf(x[1])),
    ("log", 1, |x| x[0].ln()),
    ("log10", 1, |x| x[0].log10()),
    ("sqrt", 1, |x| x[0].sqrt()),
    ("pi", 0, |_| std::f64::consts::PI),
];

/// The names of the functions, for messages.
pub fn function_names() -> impl Iterator<Item = &'static str> {
    FUNCTIONS.iter().map(|(name, ..)| *name)
}

fn call(name: &str, arguments: &[Number]) -> Result<Number, ExpressionError> {
    let Some((_, count, function)) = FUNCTIONS.iter().find(|(known, ..)| *known == name) else {
        return Err(failure(0, format!("unknown function '{name}'")));
    };
    if arguments.len() != *count {
        let plural = if *count == 1 { "" } else { "s" };
        return Err(failure(
            0,
            format!(
                "{name} takes {count} argument{plural}, not {}",
                arguments.len()
            ),
        ));
    }
    let values: Vec<f64> = arguments.iter().map(|argument| argument.to_f64()).collect();
    Ok(Number::Float(function(&values) as f32))
}

fn literal(source: &str, options: ExpressionOptions) -> Option<Number> {
    let lower = source.to_ascii_lowercase();
    let (digits, radix, kind) = if let Some(digits) = lower.strip_prefix("0x") {
        (digits, 16, if options.hex_is_float { 3 } else { 1 })
    } else if let Some(digits) = lower.strip_prefix("0z") {
        (digits, 16, 3)
    } else if let Some(digits) = lower.strip_prefix("0b") {
        (digits, 2, 2)
    } else {
        (lower.as_str(), 10, 2)
    };
    if digits.is_empty() {
        return None;
    }
    if radix != 10 {
        let mut value = 0_u32;
        let mut count = 0;
        for digit in digits.chars() {
            if digit == '_' {
                continue;
            }
            value = value
                .wrapping_mul(radix)
                .wrapping_add(digit.to_digit(radix)?);
            count += 1;
        }
        return (count != 0).then_some(Number::from_bits(kind, value));
    }
    let clean: String = digits
        .chars()
        .filter(|&character| character != '_')
        .collect();
    if clean.is_empty() || clean.ends_with('.') {
        return None;
    }
    let mut exponent = false;
    let mut decimal = false;
    let mut position = 0;
    while position < clean.len() {
        match clean.as_bytes()[position] {
            b'.' if !decimal && !exponent => decimal = true,
            b'e' if !exponent
                && clean
                    .as_bytes()
                    .get(position + 1)
                    .is_some_and(|byte| matches!(byte, b'+' | b'-')) =>
            {
                exponent = true;
                position += 1;
            }
            // openvcl's `loi` expressions also take `1.8e2`; vcl 1.4beta7
            // reads it as a symbol.
            b'e' if !exponent
                && options.functions
                && clean
                    .as_bytes()
                    .get(position + 1)
                    .is_some_and(u8::is_ascii_digit) =>
            {
                exponent = true;
            }
            byte if byte.is_ascii_digit() => {}
            _ => return None,
        }
        position += 1;
    }
    if !clean.as_bytes().last()?.is_ascii_digit() {
        return None;
    }
    if decimal || exponent {
        // Validation ignores underscores, but strtod consumes the original
        // literal. Retain that surprising truncation behavior.
        let prefix = source.split('_').next()?;
        return c_float_prefix(prefix).map(Number::Float);
    }
    let value = clean.bytes().fold(0_u32, |value, byte| {
        value.wrapping_mul(10).wrapping_add(u32::from(byte - b'0'))
    });
    Some(Number::Signed(value as i32))
}

fn c_float_prefix(source: &str) -> Option<f32> {
    if source.is_empty() {
        return Some(0.0);
    }
    for end in (1..=source.len()).rev() {
        if let Ok(value) = source[..end].parse::<f64>() {
            return Some(value as f32);
        }
    }
    // strtod has no conversion for an initial underscore or an empty prefix.
    Some(0.0)
}

fn unknowns(tree: &Expr, options: ExpressionOptions, symbols: &mut BTreeSet<String>) {
    match tree {
        Expr::Literal(source) => {
            if literal(source, options).is_none() {
                symbols.insert(source.clone());
            }
        }
        Expr::Unary { operand, .. } => unknowns(operand, options, symbols),
        Expr::Binary { left, right, .. } => {
            unknowns(left, options, symbols);
            unknowns(right, options, symbols);
        }
        Expr::Call { arguments, .. } => {
            for argument in arguments {
                unknowns(argument, options, symbols);
            }
        }
    }
}

fn numeric(tree: &Expr, options: ExpressionOptions) -> Result<Number, ExpressionError> {
    match tree {
        Expr::Literal(source) => literal(source, options)
            .ok_or_else(|| failure(0, "unresolved literal reached numeric evaluation")),
        Expr::Unary { operation, operand } => unary(*operation, numeric(operand, options)?),
        Expr::Call { name, arguments } => {
            let values = arguments
                .iter()
                .map(|argument| numeric(argument, options))
                .collect::<Result<Vec<_>, _>>()?;
            call(name, &values)
        }
        Expr::Binary {
            operation,
            left,
            right,
        } => binary_value(
            *operation,
            numeric(left, options)?,
            numeric(right, options)?,
        ),
    }
}

fn unary(operation: Unary, operand: Number) -> Result<Number, ExpressionError> {
    Ok(match operation {
        Unary::Plus => operand,
        Unary::Minus => match operand {
            Number::Unsigned(value) => Number::Unsigned(value.wrapping_neg()),
            Number::Signed(value) => Number::Signed(value.wrapping_neg()),
            Number::Float(value) => Number::Float(f32::from_bits(value.to_bits() ^ 0x8000_0000)),
        },
        Unary::Absolute => match operand {
            Number::Unsigned(_) => operand,
            Number::Signed(value) => Number::Signed(if value < 0 {
                value.wrapping_neg()
            } else {
                value
            }),
            Number::Float(value) => Number::Float(if value < 0.0 { -value } else { value }),
        },
        Unary::Complement | Unary::LogicalNot if matches!(operand, Number::Float(_)) => {
            return Err(failure(0, "unary integer operation applied to a float"));
        }
        Unary::Complement => Number::from_bits(operand.type_id(), !operand.bits()),
        Unary::LogicalNot => Number::from_bits(operand.type_id(), u32::from(operand.bits() == 0)),
    })
}

fn binary_value(operation: Binary, left: Number, right: Number) -> Result<Number, ExpressionError> {
    let kind = left.type_id().max(right.type_id());
    if matches!(operation, Binary::Power | Binary::LogicalOr) {
        return Err(failure(
            0,
            "operator is tokenized but has no binary evaluator in the original executable",
        ));
    }
    if matches!(operation, Binary::BitAnd | Binary::BitOr | Binary::BitXor) {
        let bits = match operation {
            Binary::BitAnd => left.bits() & right.bits(),
            Binary::BitOr => left.bits() | right.bits(),
            Binary::BitXor => left.bits() ^ right.bits(),
            _ => unreachable!(),
        };
        return Ok(Number::from_bits(kind, bits));
    }
    if kind == 3 {
        let result = match operation {
            Binary::Add => left.to_f64() + right.to_f64(),
            Binary::Subtract => left.to_f64() - right.to_f64(),
            Binary::Multiply => left.to_f64() * right.to_f64(),
            Binary::Divide => left.to_f64() / right.to_f64(),
            _ => {
                return Err(failure(
                    0,
                    "integer-only binary operation applied to a float",
                ));
            }
        };
        return Ok(Number::Float(result as f32));
    }
    let lhs = left.bits();
    let rhs = right.bits();
    let bits = match operation {
        Binary::Add => lhs.wrapping_add(rhs),
        Binary::Subtract => lhs.wrapping_sub(rhs),
        Binary::Multiply => lhs.wrapping_mul(rhs),
        Binary::Divide | Binary::Remainder => {
            if rhs == 0 {
                return Err(failure(
                    0,
                    "integer division by zero would trap in the original executable",
                ));
            }
            if kind == 2 {
                let lhs = lhs as i32;
                let rhs = rhs as i32;
                if lhs == i32::MIN && rhs == -1 {
                    return Err(failure(
                        0,
                        "signed division overflow would trap in the original executable",
                    ));
                }
                if operation == Binary::Divide {
                    (lhs / rhs) as u32
                } else {
                    (lhs % rhs) as u32
                }
            } else if operation == Binary::Divide {
                lhs / rhs
            } else {
                lhs % rhs
            }
        }
        Binary::ShiftLeft => lhs.wrapping_shl(rhs & 31),
        Binary::ShiftRight if kind == 2 => ((lhs as i32) >> (rhs & 31)) as u32,
        Binary::ShiftRight => lhs >> (rhs & 31),
        Binary::LogicalAnd => u32::from(lhs != 0 && rhs != 0),
        _ => unreachable!(),
    };
    Ok(Number::from_bits(kind, bits))
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn number(source: &str) -> Number {
        evaluate(source).unwrap().number().unwrap()
    }

    #[test]
    fn literal_types_and_wrapping_come_from_binary_converter() {
        assert_eq!(number("123"), Number::Signed(123));
        assert_eq!(number("4294967295"), Number::Signed(-1));
        assert_eq!(number("0xFFFFFFFF"), Number::Unsigned(u32::MAX));
        assert_eq!(number("0b1010_0010"), Number::Signed(162));
        assert_eq!(number("1_000"), Number::Signed(1000));
        assert_eq!(number("0x1_0000_0000"), Number::Unsigned(0));
    }

    #[test]
    fn original_precedence_and_left_associativity_are_preserved() {
        assert_eq!(number("2+3*4"), Number::Signed(14));
        assert_eq!(number("20-3-2"), Number::Signed(15));
        assert_eq!(number("1+2<<3"), Number::Signed(24));
        assert_eq!(number("1|2^3&4"), Number::Signed(3));
        assert_eq!(number("(1+2)*(3+4)"), Number::Signed(21));
        assert_eq!(number("1+-2"), Number::Signed(-1));
    }

    #[test]
    fn arithmetic_promotes_by_original_type_tag_order() {
        assert_eq!(number("0xffffffff+1"), Number::Signed(0));
        assert_eq!(number("0xffffffff+0x1"), Number::Unsigned(0));
        assert_eq!(number("-0x1"), Number::Unsigned(u32::MAX));
        assert_eq!(number("3+0.5"), Number::Float(3.5));
        assert_eq!(number("0xffffffff+0.0"), Number::Float(u32::MAX as f32));
    }

    #[test]
    fn shifts_use_x86_count_mask_and_signedness() {
        assert_eq!(number("1<<32"), Number::Signed(1));
        assert_eq!(number("-4>>1"), Number::Signed(-2));
        assert_eq!(number("0xfffffffc>>0x1"), Number::Unsigned(0x7fff_fffe));
        assert_eq!(number("1<<-1"), Number::Signed(i32::MIN));
    }

    #[test]
    fn abs_not_and_complement_have_distinct_float_rules() {
        assert_eq!(number("#(-3)"), Number::Signed(3));
        assert_eq!(number("#(-2.5)"), Number::Float(2.5));
        assert_eq!(number("!0"), Number::Signed(1));
        assert_eq!(number("~0x0"), Number::Unsigned(u32::MAX));
        assert_eq!(number("#(-0.0)").bits(), 0x8000_0000);
        assert!(evaluate("!0.0").is_err());
        assert!(evaluate("~0.0").is_err());
    }

    #[test]
    fn float_bit_literals_and_loi_mode_are_distinct() {
        assert_eq!(number("0z3f800000"), Number::Float(1.0));
        assert_eq!(number("0x3f800000"), Number::Unsigned(0x3f80_0000));
        assert_eq!(
            evaluate_with_options(
                "0x3f800000",
                ExpressionOptions {
                    hex_is_float: true,
                    ..ExpressionOptions::default()
                }
            )
            .unwrap(),
            Evaluation::Number(Number::Float(1.0))
        );
        assert_eq!(number("1.0&0x7fffffff").bits(), 0x3f80_0000);
    }

    #[test]
    fn decimal_scientific_literals_keep_original_exponent_rule() {
        assert_eq!(number("1e-3"), Number::Float(0.001));
        assert_eq!(number("1.25e+2"), Number::Float(125.0));
        assert!(matches!(
            evaluate("1e3").unwrap(),
            Evaluation::Unresolved { .. }
        ));
        assert!(matches!(
            evaluate("1E-3").unwrap(),
            Evaluation::Unresolved { .. }
        ));
        assert!(matches!(
            evaluate("1.").unwrap(),
            Evaluation::Unresolved { .. }
        ));
        assert_eq!(number("1_2.5"), Number::Float(1.0));
    }

    #[test]
    fn symbolic_expressions_defer_all_arithmetic_and_keep_full_source() {
        assert_eq!(
            evaluate("label + 2 * 3").unwrap(),
            Evaluation::Unresolved {
                source: "label+2*3".into(),
                symbols: vec!["label".into()]
            }
        );
        assert!(matches!(
            evaluate("label/0").unwrap(),
            Evaluation::Unresolved { .. }
        ));
        assert!(matches!(
            evaluate("a||b").unwrap(),
            Evaluation::Unresolved { .. }
        ));
        assert!(matches!(
            evaluate("0x_invalid").unwrap(),
            Evaluation::Unresolved { .. }
        ));
    }

    #[test]
    fn tokenized_but_unimplemented_binary_operators_fail_numeric_evaluation() {
        assert!(evaluate("2**3").is_err());
        assert!(evaluate("1||0").is_err());
        assert_eq!(number("2&&3"), Number::Signed(1));
        assert_eq!(number("0&&3"), Number::Signed(0));
        // Evaluation does not short circuit either operand.
        assert!(evaluate("0&&(1/0)").is_err());
    }

    #[test]
    fn arithmetic_traps_are_explicit_errors() {
        assert!(evaluate("3/0").is_err());
        assert!(evaluate("3%0").is_err());
        assert!(evaluate("-2147483648/-1").is_err());
        assert_eq!(number("-7%3"), Number::Signed(-1));
        assert!(matches!(number("1.0/0.0"), Number::Float(value) if value.is_infinite()));
    }

    #[test]
    fn conversion_uses_truncation_and_payload_reinterpretation() {
        assert_eq!(Number::Float(-2.9).to_i32(), -2);
        assert_eq!(Number::Float(-2.9).to_u32(), 0xffff_fffe);
        assert_eq!(Number::Unsigned(u32::MAX).to_i32(), -1);
        assert_eq!(Number::Signed(-1).to_u32(), u32::MAX);
        assert_eq!(Number::Float(f32::NAN).to_i32(), i32::MIN);
        assert_eq!(Number::Float(f32::NAN).to_u32(), 0);
    }

    #[test]
    fn malformed_trees_are_rejected_without_panicking() {
        for source in ["", "(", ")", "1+", "*1", "1(2)", "(1+2", "1+2)", "1!2"] {
            assert!(parse(source).is_err(), "{source:?}");
        }
        let nested = format!("{}1{}", "(".repeat(260), ")".repeat(260));
        assert!(parse(&nested).is_err());
    }

    fn loi_bits(source: &str) -> Result<u32, ExpressionError> {
        let options = ExpressionOptions {
            hex_is_float: true,
            functions: true,
        };
        Ok(evaluate_with_options(source, options)?
            .number()
            .expect("a number")
            .to_f32()
            .to_bits())
    }

    #[test]
    fn functions_give_openvcls_values() {
        // Observed from openvcl: the float bits each `loi` line emits.
        for (source, bits) in [
            ("sin(45*(pi()/1.8e2))", 0x3f35_04f3),
            ("pi()", 0x4049_0fdb),
            ("abs(-2.5)", 0x4020_0000),
            ("abs(-2)", 0x4000_0000),
            ("exp(1)", 0x402d_f854),
            ("cos(0)", 0x3f80_0000),
            ("tan(1)", 0x3fc7_5923),
            ("sinh(1)", 0x3f96_6cfe),
            ("cosh(1)", 0x3fc5_83ab),
            ("asin(1)", 0x3fc9_0fdb),
            ("acos(0)", 0x3fc9_0fdb),
            ("atan(1)", 0x3f49_0fdb),
            ("atan2(1,2)", 0x3eed_6338),
            ("atan2(2,1)", 0x3f8d_b70d),
            ("pow(2,10)", 0x4480_0000),
            ("log(10)", 0x4013_5d8e),
            ("log10(1000)", 0x4040_0000),
            ("sqrt(2)", 0x3fb5_04f3),
            ("sqrt(pow(3,2)+pow(4,2))", 0x40a0_0000),
            ("sqrt(16)", 0x4080_0000),
            ("sin (1)", 0x3f57_6aa4),
        ] {
            assert_eq!(loi_bits(source).unwrap(), bits, "{source}");
        }
        // openvcl lists tanh but leaves it unevaluated; here it is tanh.
        assert_eq!(
            loi_bits("tanh(1)").unwrap(),
            (1.0_f64.tanh() as f32).to_bits()
        );
        // LOI reads 0x literals as float bits, in arguments too.
        assert_eq!(loi_bits("sqrt(0x40800000)").unwrap(), 2.0_f32.to_bits());
    }

    #[test]
    fn function_calls_need_the_option_and_a_known_name() {
        // Without the option a call is what it was: no expression.
        assert!(evaluate("sin(1)").is_err());
        assert!(evaluate("pi()").is_err());
        for (source, message) in [
            ("foo(1)", "unknown function 'foo'"),
            ("SIN(1)", "unknown function 'SIN'"),
            ("sin(1,2)", "sin takes 1 argument, not 2"),
            ("sin()", "sin takes 1 argument, not 0"),
            ("pi(1)", "pi takes 0 arguments, not 1"),
            ("pow(2)", "pow takes 2 arguments, not 1"),
        ] {
            assert_eq!(loi_bits(source).unwrap_err().message, message, "{source}");
        }
        assert!(loi_bits("sin(1").is_err());
        // A symbolic argument leaves the expression unresolved.
        let options = ExpressionOptions {
            hex_is_float: true,
            functions: true,
        };
        assert!(matches!(
            evaluate_with_options("sin(angle)", options).unwrap(),
            Evaluation::Unresolved { symbols, .. } if symbols == ["angle"]
        ));
        // Without the option, `1.8e2` is a symbol, as in vcl 1.4beta7.
        assert!(matches!(
            evaluate("1.8e2").unwrap(),
            Evaluation::Unresolved { .. }
        ));
        assert_eq!(loi_bits("1.8e2").unwrap(), 180.0_f32.to_bits());
        assert!(parse_with_functions("2*sin(1)").unwrap().has_call());
        assert!(!parse_with_functions("2*(1)").unwrap().has_call());
    }
}
