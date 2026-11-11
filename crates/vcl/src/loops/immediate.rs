//! Affine VI-induction displacement repair recovered from the supplied VCL ELF.
//!
//! Tables name eligible updates and memory consumers; annotation, epoch
//! resolution and application are separate steps. This is a typed semantic port
//! of those primitives. The caller must supply proven component reaching
//! setters from its rebuilt flow graph; source order alone is insufficient.

use crate::isa::instruction::id;
use crate::parser::expression::{self, Evaluation};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AffineOperator {
    Add,
    Subtract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrideSource {
    Operand(u8),
    Constant(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AffineSpec {
    pub instruction: u16,
    pub operator: AffineOperator,
    pub input_operand: u8,
    pub output_operand: u8,
    pub stride: StrideSource,
}

/// Original seven records, in original table order. Autoincrement memory
/// syntax is an output operand with an implicit read of the same VI register.
pub const AFFINE_SPECS: [AffineSpec; 7] = [
    AffineSpec {
        instruction: id::IADDI,
        operator: AffineOperator::Add,
        input_operand: 1,
        output_operand: 0,
        stride: StrideSource::Operand(2),
    },
    AffineSpec {
        instruction: id::IADDIU,
        operator: AffineOperator::Add,
        input_operand: 1,
        output_operand: 0,
        stride: StrideSource::Operand(2),
    },
    AffineSpec {
        instruction: id::ISUBIU,
        operator: AffineOperator::Subtract,
        input_operand: 1,
        output_operand: 0,
        stride: StrideSource::Operand(2),
    },
    AffineSpec {
        instruction: id::LQI,
        operator: AffineOperator::Add,
        input_operand: 2,
        output_operand: 1,
        stride: StrideSource::Constant(1),
    },
    AffineSpec {
        instruction: id::SQI,
        operator: AffineOperator::Add,
        input_operand: 2,
        output_operand: 1,
        stride: StrideSource::Constant(1),
    },
    AffineSpec {
        instruction: id::LQD,
        operator: AffineOperator::Subtract,
        input_operand: 2,
        output_operand: 1,
        stride: StrideSource::Constant(1),
    },
    AffineSpec {
        instruction: id::SQD,
        operator: AffineOperator::Subtract,
        input_operand: 2,
        output_operand: 1,
        stride: StrideSource::Constant(1),
    },
];

pub fn affine_spec(instruction: u16) -> Option<AffineSpec> {
    AFFINE_SPECS
        .iter()
        .copied()
        .find(|spec| spec.instruction == instruction)
}

/// Source-flow requirements checked by the annotation pass before consulting
/// users of a VI induction value. `setter_count` is the number of scalar
/// setters in that stream's register reference, not a textual name count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InductionFacts {
    pub external_input_mask: u8,
    pub external_output_mask: u8,
    pub setter_count: usize,
    pub input_register: usize,
    pub output_register: usize,
}

pub fn eligible_induction(facts: InductionFacts) -> bool {
    facts.external_input_mask & 1 != 0
        && facts.external_output_mask & 1 != 0
        && facts.setter_count == 1
        && facts.input_register == facts.output_register
}

/// Only these four descriptor IDs participate as displacement consumers in
/// this pass; IADDIU immediates and branch-label immediates are not rewritten.
pub const fn consumer_operand(instruction: u16) -> Option<u8> {
    match instruction {
        id::LQ | id::SQ | id::ILW | id::ISW => Some(1),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImmediateValue {
    Integer(i32),
    /// Complete unresolved source expression, never a partially folded tree.
    Symbolic(String),
}

impl ImmediateValue {
    pub fn parse(source: &str) -> Result<Self, ImmediateError> {
        match expression::evaluate(source)
            .map_err(|error| ImmediateError::Expression(error.to_string()))?
        {
            Evaluation::Number(value) => Ok(Self::Integer(value.to_i32())),
            Evaluation::Unresolved { source, .. } => Ok(Self::Symbolic(source)),
        }
    }

    pub fn source(&self) -> String {
        match self {
            Self::Integer(value) => value.to_string(),
            Self::Symbolic(source) => source.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Consumer originally read the pre-update external value; stored mode 1.
    Before,
    /// Consumer originally read this iteration's update; stored mode 2.
    After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LoopPart {
    Outside = 0,
    Exit = 1,
    Prologue = 2,
    Epilogue = 3,
    Entry = 4,
    Kernel = 5,
}

impl From<crate::loops::pipeline::PartKind> for LoopPart {
    fn from(part: crate::loops::pipeline::PartKind) -> Self {
        match part {
            crate::loops::pipeline::PartKind::Entry => Self::Entry,
            crate::loops::pipeline::PartKind::Prologue { .. } => Self::Prologue,
            crate::loops::pipeline::PartKind::Kernel => Self::Kernel,
            crate::loops::pipeline::PartKind::Epilogue { .. } => Self::Epilogue,
            crate::loops::pipeline::PartKind::Exit => Self::Exit,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetterLocation {
    /// Original generated-loop identity of the stream.
    pub loop_id: u16,
    /// Stream construct kind.
    pub part: LoopPart,
    /// Setter instruction's cloned iteration bias.
    pub iteration_bias: i16,
}

impl SetterLocation {
    /// A declared external value reaches the expanded construct from outside
    /// its generated-loop group and therefore has source epoch -1.
    pub const fn external() -> Self {
        Self {
            loop_id: 0,
            part: LoopPart::Outside,
            iteration_bias: -1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixup {
    pub operator: AffineOperator,
    pub phase: Phase,
    pub operand: u8,
    pub stride: ImmediateValue,
}

impl Fixup {
    /// Construct annotation once the caller has established the unique scalar
    /// read/modify/write induction and selected a before/after memory user.
    pub fn from_update(
        update: AffineSpec,
        consumer: u16,
        phase: Phase,
        stride: Option<&str>,
    ) -> Result<Option<Self>, ImmediateError> {
        let Some(operand) = consumer_operand(consumer) else {
            return Ok(None);
        };
        let stride = match update.stride {
            StrideSource::Constant(value) => ImmediateValue::Integer(value),
            StrideSource::Operand(index) => {
                ImmediateValue::parse(stride.ok_or(ImmediateError::MissingStride(index))?)?
            }
        };
        Ok(Some(Self {
            operator: update.operator,
            phase,
            operand,
            stride,
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub loop_id: u16,
    pub part: LoopPart,
    /// `None` represents the original -1 non-expanded sentinel. Expanded
    /// consumers require a nonnegative bias copied from the pipeline plan.
    pub iteration_bias: Option<i16>,
    /// For non-expanded code, true means the operand retains external bits.
    pub external_input: bool,
    /// All actual component setters after expansion and rebuilt flow.
    pub setters: Vec<SetterLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    pub operand: u8,
    pub expected_epoch: i32,
    pub actual_epoch: i32,
    /// Signed multiplier of the stored unsigned-direction stride expression.
    pub factor: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub correction: Correction,
    pub value: ImmediateValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImmediateError {
    Expression(String),
    MissingStride(u8),
    NoSetters,
    TooManySetters(usize),
    DuplicateSetterPart(LoopPart),
    InvalidSourcePart { current: LoopPart, source: LoopPart },
    AmbiguousPrologue,
    EpochMismatch { first: i16, second: i16 },
    UnexpectedBiasOutsideLoop(i16),
    MissingExpandedBias,
    InvalidLoopId(u16),
}

impl fmt::Display for ImmediateError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "immediate fixup failed: {self:?}")
    }
}

impl std::error::Error for ImmediateError {}

/// Port of the original's source-epoch resolution. The one-setter and two-setter rules
/// are intentionally distinct: one local kernel setter retains its bias for a
/// kernel consumer, while a kernel/prologue join normalizes the kernel's bias
/// by minus one before requiring agreement.
pub fn source_epoch(
    loop_id: u16,
    current: LoopPart,
    setters: &[SetterLocation],
) -> Result<i16, ImmediateError> {
    if loop_id > i16::MAX as u16 {
        return Err(ImmediateError::InvalidLoopId(loop_id));
    }
    if setters.is_empty() {
        return Err(ImmediateError::NoSetters);
    }
    // The original returns external epoch -1 as soon as any reaching setter
    // belongs to another generated loop, before local-count validation.
    if setters.iter().any(|setter| setter.loop_id != loop_id) {
        return Ok(-1);
    }
    if setters.len() > 2 {
        return Err(ImmediateError::TooManySetters(setters.len()));
    }
    if setters.len() == 1 {
        let setter = setters[0];
        return match current {
            LoopPart::Prologue => Ok(setter.iteration_bias),
            LoopPart::Kernel if matches!(setter.part, LoopPart::Prologue | LoopPart::Kernel) => {
                Ok(setter.iteration_bias)
            }
            LoopPart::Epilogue
                if matches!(setter.part, LoopPart::Prologue | LoopPart::Epilogue) =>
            {
                Ok(setter.iteration_bias)
            }
            LoopPart::Epilogue if setter.part == LoopPart::Kernel => {
                Ok(setter.iteration_bias.wrapping_sub(1))
            }
            _ => Err(ImmediateError::InvalidSourcePart {
                current,
                source: setter.part,
            }),
        };
    }
    if setters[0].part == setters[1].part {
        return Err(ImmediateError::DuplicateSetterPart(setters[0].part));
    }
    if current == LoopPart::Prologue {
        return Err(ImmediateError::AmbiguousPrologue);
    }
    let normalize = |setter: SetterLocation| match current {
        LoopPart::Kernel | LoopPart::Epilogue => match setter.part {
            LoopPart::Prologue => Ok(setter.iteration_bias),
            LoopPart::Kernel => Ok(setter.iteration_bias.wrapping_sub(1)),
            _ => Err(ImmediateError::InvalidSourcePart {
                current,
                source: setter.part,
            }),
        },
        _ => Err(ImmediateError::InvalidSourcePart {
            current,
            source: setter.part,
        }),
    };
    let first = normalize(setters[0])?;
    let second = normalize(setters[1])?;
    if first != second {
        return Err(ImmediateError::EpochMismatch { first, second });
    }
    Ok(first)
}

pub fn plan(fixup: &Fixup, context: &Context) -> Result<Correction, ImmediateError> {
    let bias = if context.loop_id == 0 {
        if let Some(bias) = context.iteration_bias {
            return Err(ImmediateError::UnexpectedBiasOutsideLoop(bias));
        }
        0
    } else {
        context
            .iteration_bias
            .filter(|&bias| bias >= 0)
            .ok_or(ImmediateError::MissingExpandedBias)?
    };
    let expected_epoch = i32::from(bias) - i32::from(fixup.phase == Phase::Before);
    let actual_epoch = if context.loop_id == 0 {
        if context.external_input { -1 } else { 0 }
    } else {
        i32::from(source_epoch(
            context.loop_id,
            context.part,
            &context.setters,
        )?)
    };
    let delta = expected_epoch - actual_epoch;
    let factor = match fixup.operator {
        AffineOperator::Add => delta,
        AffineOperator::Subtract => -delta,
    };
    Ok(Correction {
        operand: fixup.operand,
        expected_epoch,
        actual_epoch,
        factor,
    })
}

/// Apply the affine offset repair as the original's memory-immediate rewrite does.
/// Both-numeric values use wrapping signed 32-bit multiply/add. If either
/// value is unresolved, the complete original expressions are retained.
pub fn apply(
    offset: &ImmediateValue,
    fixup: &Fixup,
    context: &Context,
) -> Result<Applied, ImmediateError> {
    let correction = plan(fixup, context)?;
    let value = if correction.factor == 0 {
        offset.clone()
    } else {
        match (offset, &fixup.stride) {
            (ImmediateValue::Integer(base), ImmediateValue::Integer(stride)) => {
                ImmediateValue::Integer(base.wrapping_add(stride.wrapping_mul(correction.factor)))
            }
            _ => ImmediateValue::Symbolic(format!(
                "({})+(({})*{})",
                offset.source(),
                fixup.stride.source(),
                correction.factor
            )),
        }
    };
    Ok(Applied { correction, value })
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    fn setter(part: LoopPart, bias: i16) -> SetterLocation {
        SetterLocation {
            loop_id: 1,
            part,
            iteration_bias: bias,
        }
    }

    fn context(part: LoopPart, bias: i16, setters: Vec<SetterLocation>) -> Context {
        Context {
            loop_id: 1,
            part,
            iteration_bias: Some(bias),
            external_input: false,
            setters,
        }
    }

    fn fixup(operator: AffineOperator, phase: Phase, stride: i32) -> Fixup {
        Fixup {
            operator,
            phase,
            operand: 1,
            stride: ImmediateValue::Integer(stride),
        }
    }

    #[test]
    fn original_tables_include_only_supported_updates_and_memory_consumers() {
        assert_eq!(AFFINE_SPECS.len(), 7);
        assert_eq!(affine_spec(109).unwrap().stride, StrideSource::Operand(2));
        assert_eq!(affine_spec(163).unwrap().input_operand, 2);
        assert_eq!(affine_spec(159).unwrap().operator, AffineOperator::Subtract);
        assert_eq!(consumer_operand(155), Some(1));
        assert_eq!(consumer_operand(110), None);
        assert_eq!(consumer_operand(172), None);
    }

    #[test]
    fn annotation_requires_one_same_register_scalar_update_visible_at_both_ends() {
        let yes = InductionFacts {
            external_input_mask: 1,
            external_output_mask: 1,
            setter_count: 1,
            input_register: 2,
            output_register: 2,
        };
        assert!(eligible_induction(yes));
        assert!(!eligible_induction(InductionFacts {
            setter_count: 2,
            ..yes
        }));
        assert!(!eligible_induction(InductionFacts {
            output_register: 3,
            ..yes
        }));
        assert!(!eligible_induction(InductionFacts {
            external_input_mask: 0,
            ..yes
        }));
        assert!(!eligible_induction(InductionFacts {
            external_output_mask: 0,
            ..yes
        }));
    }

    #[test]
    fn constructors_use_operand_stride_or_original_auto_update_constant() {
        let auto = Fixup::from_update(affine_spec(158).unwrap(), 155, Phase::Before, None)
            .unwrap()
            .unwrap();
        assert_eq!(auto.stride, ImmediateValue::Integer(1));
        assert!(matches!(
            Fixup::from_update(affine_spec(110).unwrap(), 155, Phase::After, None),
            Err(ImmediateError::MissingStride(2))
        ));
        assert!(
            Fixup::from_update(affine_spec(110).unwrap(), 172, Phase::After, Some("3"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn before_and_after_crossing_an_update_have_opposite_repairs() {
        let old = ImmediateValue::Integer(5);
        let before = fixup(AffineOperator::Add, Phase::Before, 3);
        let after = fixup(AffineOperator::Add, Phase::After, 3);
        let input = Context {
            loop_id: 0,
            part: LoopPart::Outside,
            iteration_bias: None,
            external_input: true,
            setters: vec![],
        };
        assert_eq!(apply(&old, &before, &input).unwrap().value, old);
        assert_eq!(
            apply(&old, &after, &input).unwrap().value,
            ImmediateValue::Integer(8)
        );
        let local = Context {
            external_input: false,
            ..input
        };
        assert_eq!(
            apply(&old, &before, &local).unwrap().value,
            ImmediateValue::Integer(2)
        );
        assert_eq!(apply(&old, &after, &local).unwrap().value, old);
    }

    #[test]
    fn subtracting_induction_reverses_offset_factor() {
        let repair = fixup(AffineOperator::Subtract, Phase::Before, 3);
        let ctx = context(
            LoopPart::Kernel,
            1,
            vec![setter(LoopPart::Prologue, 1), setter(LoopPart::Kernel, 2)],
        );
        assert_eq!(
            apply(&ImmediateValue::Integer(5), &repair, &ctx)
                .unwrap()
                .value,
            ImmediateValue::Integer(8)
        );
    }

    #[test]
    fn kernel_join_normalizes_previous_iteration_but_single_local_setter_does_not() {
        assert_eq!(
            source_epoch(1, LoopPart::Kernel, &[setter(LoopPart::Kernel, 2)]).unwrap(),
            2
        );
        assert_eq!(
            source_epoch(
                1,
                LoopPart::Kernel,
                &[setter(LoopPart::Prologue, 1), setter(LoopPart::Kernel, 2)]
            )
            .unwrap(),
            1
        );
        assert!(matches!(
            source_epoch(
                1,
                LoopPart::Kernel,
                &[setter(LoopPart::Prologue, 2), setter(LoopPart::Kernel, 2)]
            ),
            Err(ImmediateError::EpochMismatch { .. })
        ));
    }

    #[test]
    fn epilogue_treats_kernel_values_as_previous_iteration() {
        assert_eq!(
            source_epoch(1, LoopPart::Epilogue, &[setter(LoopPart::Kernel, 3)]).unwrap(),
            2
        );
        assert_eq!(
            source_epoch(1, LoopPart::Epilogue, &[setter(LoopPart::Prologue, 3)]).unwrap(),
            3
        );
        assert_eq!(
            source_epoch(1, LoopPart::Epilogue, &[setter(LoopPart::Epilogue, 3)]).unwrap(),
            3
        );
    }

    #[test]
    fn external_setter_wins_before_local_ambiguity_check() {
        let outside = SetterLocation {
            loop_id: 0,
            part: LoopPart::Outside,
            iteration_bias: -1,
        };
        assert_eq!(
            source_epoch(
                1,
                LoopPart::Kernel,
                &[
                    setter(LoopPart::Kernel, 1),
                    setter(LoopPart::Kernel, 2),
                    outside
                ]
            )
            .unwrap(),
            -1
        );
    }

    #[test]
    fn ambiguous_and_invalid_sources_are_explicit_errors() {
        assert!(matches!(
            source_epoch(1, LoopPart::Kernel, &[]),
            Err(ImmediateError::NoSetters)
        ));
        assert!(matches!(
            source_epoch(
                1,
                LoopPart::Kernel,
                &[setter(LoopPart::Kernel, 1), setter(LoopPart::Kernel, 2)]
            ),
            Err(ImmediateError::DuplicateSetterPart(_))
        ));
        assert!(matches!(
            source_epoch(
                1,
                LoopPart::Prologue,
                &[setter(LoopPart::Kernel, 1), setter(LoopPart::Prologue, 2)]
            ),
            Err(ImmediateError::AmbiguousPrologue)
        ));
        assert!(source_epoch(1, LoopPart::Exit, &[setter(LoopPart::Kernel, 1)]).is_err());
    }

    #[test]
    fn symbolic_affine_repair_preserves_original_complete_expressions() {
        let repair = Fixup {
            stride: ImmediateValue::parse("STRIDE+1").unwrap(),
            ..fixup(AffineOperator::Add, Phase::After, 0)
        };
        let ctx = context(LoopPart::Prologue, 2, vec![setter(LoopPart::Prologue, 0)]);
        assert_eq!(
            apply(&ImmediateValue::parse("BASE+4").unwrap(), &repair, &ctx)
                .unwrap()
                .value,
            ImmediateValue::Symbolic("(BASE+4)+((STRIDE+1)*2)".into())
        );
    }

    #[test]
    fn numeric_affine_repair_uses_original_32_bit_wrap() {
        let repair = fixup(AffineOperator::Add, Phase::After, i32::MAX);
        let ctx = context(LoopPart::Prologue, 2, vec![setter(LoopPart::Prologue, 0)]);
        assert_eq!(
            apply(&ImmediateValue::Integer(1), &repair, &ctx)
                .unwrap()
                .value,
            ImmediateValue::Integer(-1)
        );
    }

    #[test]
    fn expanded_and_unexpanded_bias_invariants_are_checked() {
        let repair = fixup(AffineOperator::Add, Phase::After, 1);
        let mut ctx = context(LoopPart::Kernel, 0, vec![setter(LoopPart::Kernel, 0)]);
        ctx.iteration_bias = None;
        assert!(matches!(
            plan(&repair, &ctx),
            Err(ImmediateError::MissingExpandedBias)
        ));
        ctx.loop_id = 0;
        ctx.iteration_bias = Some(0);
        assert!(matches!(
            plan(&repair, &ctx),
            Err(ImmediateError::UnexpectedBiasOutsideLoop(0))
        ));
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn scheduled_original_probes_confirm_opposite_displacement_repairs() {
        let add = Fixup::from_update(affine_spec(110).unwrap(), 155, Phase::After, Some("3"))
            .unwrap()
            .unwrap();
        let subtract = Fixup::from_update(affine_spec(112).unwrap(), 155, Phase::After, Some("3"))
            .unwrap()
            .unwrap();
        let prologue = context(LoopPart::Prologue, 0, vec![SetterLocation::external()]);
        let kernel = context(
            LoopPart::Kernel,
            1,
            vec![setter(LoopPart::Prologue, 0), setter(LoopPart::Kernel, 1)],
        );
        for ctx in [prologue, kernel] {
            assert_eq!(
                apply(&ImmediateValue::Integer(5), &add, &ctx)
                    .unwrap()
                    .value,
                ImmediateValue::Integer(8)
            );
            assert_eq!(
                apply(&ImmediateValue::Integer(5), &subtract, &ctx)
                    .unwrap()
                    .value,
                ImmediateValue::Integer(2)
            );
        }
        let original_add =
            &crate::test_evidence::text("oracle/immediate-scheduled-after-add/stdout.bin");
        let original_subtract =
            &crate::test_evidence::text("oracle/immediate-scheduled-after-subtract/stdout.bin");
        assert!(original_add.contains("8(VI02)"));
        assert!(original_subtract.contains("2(VI02)"));
        assert!(original_add.contains("VI02,VI02,0x00000003"));
        assert!(original_subtract.contains("VI02,VI02,0x00000003"));
    }
}
