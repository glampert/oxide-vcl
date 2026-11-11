//! Typed bridge from allocated modulo placements to expanded instructions.
//!
//! Clone identity/bias, branch routing and operand copying follow the original.
//! Affine repair delegates to the recovered primitives in `immediate`.
//! Expansion does not infer register webs from equal iteration biases. The
//! caller rebuilds component flow and supplies actual reaching definitions.

use crate::analysis::flow::{self, Definition};
use crate::isa::instruction::{self, Direction, Mask, Syntax, class, subtype};
use crate::loops::immediate::{self, Applied, Fixup, ImmediateValue, SetterLocation};
use crate::loops::pipeline::{self, Expansion, LoopSchedule};
use crate::parser::{Instruction, Operand, ParseMode};
use crate::scheduler::schedule::Pipe;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub type CloneId = usize;

#[derive(Clone, Debug)]
pub struct SourceOperation {
    /// The same identity used by `LoopSchedule::operations`.
    pub id: usize,
    pub instruction: Instruction,
    /// Annotations already justified by source induction analysis.
    pub fixups: Vec<Fixup>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site {
    pub part: usize,
    pub row: usize,
    pub pipe: Pipe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Source {
        id: usize,
        stage: u8,
        iteration_bias: i16,
        reverse_branch: bool,
    },
    /// The unconditional branch from a nonfinal drain to the common exit.
    DrainBranch,
}

#[derive(Clone, Debug)]
pub struct CloneOperation {
    pub id: CloneId,
    pub site: Site,
    pub origin: Origin,
    pub instruction: Instruction,
    pub fixups: Vec<Fixup>,
}

#[derive(Clone, Debug)]
pub struct ExpandedLoop {
    /// Generated-loop identity, distinct from source one-based stage prefixes.
    pub loop_id: u16,
    pub plan: Expansion,
    /// Dense IDs index this vector and remain stable through operand repair.
    pub operations: Vec<CloneOperation>,
    /// Same dimensions as `plan.parts[*].rows`, with dense clone identities.
    pub rows: Vec<Vec<[Option<CloneId>; 2]>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperandBinding {
    pub clone: CloneId,
    /// All-operand index, including implicit operands.
    pub operand: usize,
    pub register: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Setter {
    External,
    Operation { clone: CloneId, operand: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachingSetters {
    pub clone: CloneId,
    /// The scalar address operand annotated by `Fixup::operand`.
    pub operand: usize,
    /// Proven component reaching definitions after expansion and rebinding.
    pub setters: Vec<Setter>,
}

/// Identity bridge between a typed instruction operand and component flow.
/// `instruction` is a source ID during annotation and a clone ID after
/// expansion. `operation` is the caller's actual flow operation ID; both
/// issue slots in a raw pair may intentionally share it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperandFlow {
    pub instruction: usize,
    pub operand: usize,
    pub operation: flow::OperationId,
    pub register: flow::RegisterId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InductionAnnotation {
    pub consumer: usize,
    pub operand: usize,
    pub update: usize,
    pub register: flow::RegisterId,
    pub phase: immediate::Phase,
}

#[derive(Clone, Debug)]
pub struct AnnotatedSources {
    pub sources: Vec<SourceOperation>,
    pub annotations: Vec<InductionAnnotation>,
}

#[derive(Clone, Debug)]
pub struct RepairedLoop {
    pub expanded: ExpandedLoop,
    pub corrections: Vec<(CloneId, Applied)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopIrError {
    Pipeline(pipeline::PipelineError),
    Flow(flow::FlowError),
    Immediate(immediate::ImmediateError),
    InvalidLoopId(u16),
    DuplicateSource(usize),
    MissingSource(usize),
    ExtraSource(usize),
    InvalidSlot { source: usize, pipe: Pipe },
    NopMismatch(usize),
    InvalidConditional(usize),
    InvalidBranchTarget,
    UnsupportedBranchSense(u16),
    InvalidClone(CloneId),
    InvalidOperand { clone: CloneId, operand: usize },
    InvalidBinding { clone: CloneId, operand: usize },
    DuplicateBinding { clone: CloneId, operand: usize },
    InvalidFixup { source: usize, operand: usize },
    DuplicateFixup { source: usize, operand: usize },
    MissingSetters { clone: CloneId, operand: usize },
    UnexpectedSetters { clone: CloneId, operand: usize },
    DuplicateSetters { clone: CloneId, operand: usize },
    InvalidSetter { clone: CloneId, operand: usize },
    InvalidFlowBlock(usize),
    DuplicateOperandFlow { instruction: usize, operand: usize },
    MissingOperandFlow { instruction: usize, operand: usize },
    MissingFlowRead { operation: usize, register: usize },
    UndefinedAddress { instruction: usize, operand: usize },
    AmbiguousFlowSetter { operation: usize, register: usize },
}

impl fmt::Display for LoopIrError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "expanded loop repair failed: {self:?}")
    }
}

impl std::error::Error for LoopIrError {}

impl From<pipeline::PipelineError> for LoopIrError {
    fn from(error: pipeline::PipelineError) -> Self {
        Self::Pipeline(error)
    }
}

impl From<immediate::ImmediateError> for LoopIrError {
    fn from(error: immediate::ImmediateError) -> Self {
        Self::Immediate(error)
    }
}

impl From<flow::FlowError> for LoopIrError {
    fn from(error: flow::FlowError) -> Self {
        Self::Flow(error)
    }
}

/// Annotate affine memory users before modulo scheduling.
///
/// Scalar boundary masks come from component liveness. The source stream is
/// then analyzed in isolation with its live-ins as external seeds: a use before
/// its sole local setter is `Before`, even when the whole-loop analysis also
/// sees that setter through a backedge. This preserves the original distinction
/// between the register reference's visible inputs and the setter's local users.
/// Source/flow identities are explicit, including raw paired-read grouping.
pub fn annotate_sources(
    sources: &[SourceOperation],
    graph: &flow::Graph,
    analysis: &flow::Analysis,
    block: usize,
    operands: &[OperandFlow],
) -> Result<AnnotatedSources, LoopIrError> {
    let stream = graph
        .blocks
        .get(block)
        .ok_or(LoopIrError::InvalidFlowBlock(block))?;
    let inputs = analysis
        .live_in
        .get(block)
        .ok_or(LoopIrError::InvalidFlowBlock(block))?;
    let outputs = analysis
        .live_out
        .get(block)
        .ok_or(LoopIrError::InvalidFlowBlock(block))?;
    let bindings = operand_map(operands)?;
    let mut result = AnnotatedSources {
        sources: sources.to_vec(),
        annotations: Vec::new(),
    };
    let local = flow::Graph {
        operations: graph.operations.clone(),
        blocks: vec![flow::Block {
            operations: stream.operations.clone(),
            successors: Vec::new(),
            outputs: outputs.iter().copied().collect(),
            timing: None,
            ..flow::Block::default()
        }],
        entry: 0,
        inputs: inputs.iter().copied().collect(),
        constants: graph.constants.clone(),
    }
    .analyze()?;

    for update in sources {
        let Some(spec) = immediate::affine_spec(update.instruction.metadata.index) else {
            continue;
        };
        let output = required_operand_flow(&bindings, update.id, usize::from(spec.output_operand))?;
        let input = required_operand_flow(&bindings, update.id, usize::from(spec.input_operand))?;
        if !stream.operations.contains(&output.operation)
            || !stream.operations.contains(&input.operation)
        {
            return Err(LoopIrError::InvalidFlowBlock(block));
        }
        let setter_count = count_local_setters(graph, stream, output.register);
        if graph.constants.contains(&output.register)
            || !immediate::eligible_induction(immediate::InductionFacts {
                external_input_mask: boundary_mask(inputs, output.register),
                external_output_mask: boundary_mask(outputs, output.register),
                setter_count,
                input_register: input.register,
                output_register: output.register,
            })
        {
            continue;
        }
        let stride = match spec.stride {
            immediate::StrideSource::Operand(index) => update
                .instruction
                .operands
                .get(usize::from(index))
                .and_then(|value| value.immediate.as_deref()),
            immediate::StrideSource::Constant(_) => None,
        };
        let site = UpdateSite {
            update,
            spec,
            output,
            stride,
        };
        annotate_consumers(sources, &bindings, &local, &site, &mut result)?;
    }
    Ok(result)
}

/// An eligible induction update and the facts its consumers need.
struct UpdateSite<'a> {
    update: &'a SourceOperation,
    spec: immediate::AffineSpec,
    output: OperandFlow,
    stride: Option<&'a str>,
}

/// The union of the component masks of `register` in a boundary set.
fn boundary_mask(values: &BTreeSet<flow::Access>, register: flow::RegisterId) -> u8 {
    values
        .iter()
        .filter(|access| access.register == register)
        .fold(0, |mask, access| mask | access.mask)
}

/// How many operations of the stream write the x component of `register`.
fn count_local_setters(graph: &flow::Graph, stream: &flow::Block, register: usize) -> usize {
    stream
        .operations
        .iter()
        .filter(|&&id| {
            graph.operations.get(id).is_some_and(|operation| {
                operation
                    .writes
                    .iter()
                    .any(|access| access.register == register && access.mask & 1 != 0)
            })
        })
        .count()
}

/// Attach a fixup to every memory consumer addressed through the updated
/// register, according to which side of the update it reads.
fn annotate_consumers(
    sources: &[SourceOperation],
    bindings: &BTreeMap<(usize, usize), OperandFlow>,
    local: &flow::Analysis,
    site: &UpdateSite,
    result: &mut AnnotatedSources,
) -> Result<(), LoopIrError> {
    let UpdateSite {
        update,
        spec,
        output,
        stride,
    } = *site;
    for consumer in sources {
        let Some(index) = immediate::consumer_operand(consumer.instruction.metadata.index) else {
            continue;
        };
        let operand = usize::from(index);
        let address = required_operand_flow(bindings, consumer.id, operand)?;
        if address.register != output.register {
            continue;
        }
        let read = scalar_read(local, address.operation, address.register)?;
        if read.definitions.contains(&Definition::Uninitialized) {
            return Err(LoopIrError::UndefinedAddress {
                instruction: consumer.id,
                operand,
            });
        }
        let phase = if read.definitions == BTreeSet::from([Definition::Input]) {
            immediate::Phase::Before
        } else if read.definitions == BTreeSet::from([Definition::Operation(output.operation)]) {
            immediate::Phase::After
        } else {
            continue;
        };
        let destination = result
            .sources
            .iter_mut()
            .find(|source| source.id == consumer.id)
            .expect("source copied above");
        if destination
            .fixups
            .iter()
            .any(|fixup| usize::from(fixup.operand) == operand)
        {
            return Err(LoopIrError::DuplicateFixup {
                source: consumer.id,
                operand,
            });
        }
        destination.fixups.push(
            Fixup::from_update(spec, consumer.instruction.metadata.index, phase, stride)?
                .expect("eligible memory consumer"),
        );
        result.annotations.push(InductionAnnotation {
            consumer: consumer.id,
            operand,
            update: update.id,
            register: output.register,
            phase,
        });
    }
    Ok(())
}

fn operand_map(
    operands: &[OperandFlow],
) -> Result<BTreeMap<(usize, usize), OperandFlow>, LoopIrError> {
    let mut result = BTreeMap::new();
    for &operand in operands {
        if result
            .insert((operand.instruction, operand.operand), operand)
            .is_some()
        {
            return Err(LoopIrError::DuplicateOperandFlow {
                instruction: operand.instruction,
                operand: operand.operand,
            });
        }
    }
    Ok(result)
}

fn required_operand_flow(
    bindings: &BTreeMap<(usize, usize), OperandFlow>,
    instruction: usize,
    operand: usize,
) -> Result<OperandFlow, LoopIrError> {
    bindings
        .get(&(instruction, operand))
        .copied()
        .ok_or(LoopIrError::MissingOperandFlow {
            instruction,
            operand,
        })
}

fn scalar_read(
    analysis: &flow::Analysis,
    operation: usize,
    register: usize,
) -> Result<&flow::Read, LoopIrError> {
    analysis
        .reads
        .iter()
        .find(|read| {
            read.operation == operation && read.register == register && read.component == 0
        })
        .ok_or(LoopIrError::MissingFlowRead {
            operation,
            register,
        })
}

/// Clone a legal, allocated modulo table without reducing or rendering it.
///
/// Branch instructions in the result contain their concrete final sense and
/// target. The original stores the sense flag until emission;
/// `Origin` retains that flag while this adapter exposes an ordinary typed
/// instruction to downstream flow. Source operands and annotations are copied,
/// not shared, so repairing one clone never changes a sibling or source.
pub fn expand(
    schedule: &LoopSchedule,
    sources: &[SourceOperation],
    loop_id: u16,
) -> Result<ExpandedLoop, LoopIrError> {
    if loop_id == 0 || loop_id > i16::MAX as u16 {
        return Err(LoopIrError::InvalidLoopId(loop_id));
    }
    let plan = pipeline::expand(schedule)?;
    let source_map = checked_source_map(sources)?;
    check_schedule_sources(schedule, sources, &source_map)?;

    let conditional = &source_map[&schedule.conditional].instruction;
    // A conditional branch to a label, not to a register.
    let target = subtype::branch::REGISTER | subtype::branch::CONDITIONAL;
    if !conditional.metadata.is_branch()
        || conditional.metadata.subtype & target != subtype::branch::CONDITIONAL
    {
        return Err(LoopIrError::InvalidConditional(schedule.conditional));
    }

    let (operations, rows) = clone_parts(&plan, &source_map)?;
    Ok(ExpandedLoop {
        loop_id,
        plan,
        operations,
        rows,
    })
}

/// Index the sources by id, rejecting duplicates and invalid fixups.
fn checked_source_map(
    sources: &[SourceOperation],
) -> Result<BTreeMap<usize, &SourceOperation>, LoopIrError> {
    let mut source_map = BTreeMap::new();
    for source in sources {
        if source_map.insert(source.id, source).is_some() {
            return Err(LoopIrError::DuplicateSource(source.id));
        }
        let mut operands = BTreeSet::new();
        for fixup in &source.fixups {
            let operand = usize::from(fixup.operand);
            if immediate::consumer_operand(source.instruction.metadata.index) != Some(fixup.operand)
                || !source
                    .instruction
                    .operands
                    .get(operand)
                    .is_some_and(|value| {
                        value.direction == Direction::Read
                            && value.register.is_some()
                            && value.immediate.is_some()
                    })
            {
                return Err(LoopIrError::InvalidFixup {
                    source: source.id,
                    operand,
                });
            }
            if !operands.insert(operand) {
                return Err(LoopIrError::DuplicateFixup {
                    source: source.id,
                    operand,
                });
            }
        }
    }
    Ok(source_map)
}

/// Check that every scheduled operation has a source in a legal slot and
/// that no source is left unscheduled.
fn check_schedule_sources(
    schedule: &LoopSchedule,
    sources: &[SourceOperation],
    source_map: &BTreeMap<usize, &SourceOperation>,
) -> Result<(), LoopIrError> {
    for op in &schedule.operations {
        let source = source_map
            .get(&op.id)
            .ok_or(LoopIrError::MissingSource(op.id))?;
        if source.instruction.metadata.issue_mask & (1 << op.pipe.index()) == 0 {
            return Err(LoopIrError::InvalidSlot {
                source: op.id,
                pipe: op.pipe,
            });
        }
        if op.is_nop != (source.instruction.metadata.name == "nop") {
            return Err(LoopIrError::NopMismatch(op.id));
        }
    }
    if let Some(source) = sources
        .iter()
        .find(|source| !schedule.operations.iter().any(|op| op.id == source.id))
    {
        return Err(LoopIrError::ExtraSource(source.id));
    }
    Ok(())
}

/// Clone ids by part, row and pipe.
type CloneRows = Vec<Vec<[Option<CloneId>; 2]>>;

/// The clone operations of every part of the plan, and the table of their
/// ids by part, row and pipe.
fn clone_parts(
    plan: &Expansion,
    source_map: &BTreeMap<usize, &SourceOperation>,
) -> Result<(Vec<CloneOperation>, CloneRows), LoopIrError> {
    let mut operations = Vec::new();
    let mut rows = Vec::new();
    for (part_id, part) in plan.parts.iter().enumerate() {
        let mut part_rows = vec![[None, None]; part.rows.len()];
        for (row, pair) in part.rows.iter().enumerate() {
            for (pipe, entry) in pair.iter().enumerate() {
                let Some(copy) = entry else { continue };
                let source = source_map[&copy.source_id];
                let mut instruction = source.instruction.clone();
                // Source stage syntax is one-based. CloneAppend clears the
                // scheduled stage to zero, represented here by prefix ID 1.
                instruction.loop_id = 1;
                if copy.reverse_branch {
                    reverse_branch(&mut instruction)?;
                }
                if let Some(branch) = part
                    .branch
                    .filter(|branch| branch.source_id == Some(copy.source_id))
                {
                    change_target(&mut instruction, &plan.parts[branch.target].name)?;
                }
                let id = operations.len();
                part_rows[row][pipe] = Some(id);
                operations.push(CloneOperation {
                    id,
                    site: Site {
                        part: part_id,
                        row,
                        pipe: if pipe == 0 { Pipe::Upper } else { Pipe::Lower },
                    },
                    origin: Origin::Source {
                        id: copy.source_id,
                        stage: copy.original_stage,
                        iteration_bias: copy.iteration_bias,
                        reverse_branch: copy.reverse_branch,
                    },
                    instruction,
                    fixups: source.fixups.clone(),
                });
            }
        }
        if let Some(branch) = part.branch.filter(|branch| branch.source_id.is_none()) {
            let id = operations.len();
            part_rows[branch.row][Pipe::Lower.index()] = Some(id);
            operations.push(CloneOperation {
                id,
                site: Site {
                    part: part_id,
                    row: branch.row,
                    pipe: Pipe::Lower,
                },
                origin: Origin::DrainBranch,
                instruction: unconditional_branch(&plan.parts[branch.target].name),
                fixups: Vec::new(),
            });
        }
        rows.push(part_rows);
    }
    Ok((operations, rows))
}

impl ExpandedLoop {
    /// Translate actual rebuilt component definitions to the exact clone and
    /// writing operand expected by `repair`. A definition outside this expanded
    /// group becomes external epoch -1, as in the original. All cloned register
    /// outputs must have mappings, so an omitted local writer cannot silently
    /// masquerade as an external definition. No stage/bias matching is used.
    pub fn reaching_setters(
        &self,
        analysis: &flow::Analysis,
        operands: &[OperandFlow],
    ) -> Result<Vec<ReachingSetters>, LoopIrError> {
        let bindings = operand_map(operands)?;
        let writers = self.writers_by_flow(&bindings)?;

        let mut result = Vec::new();
        for operation in &self.operations {
            for fixup in &operation.fixups {
                let operand = usize::from(fixup.operand);
                let binding = required_operand_flow(&bindings, operation.id, operand)?;
                let read = scalar_read(analysis, binding.operation, binding.register)?;
                let mut setters = BTreeSet::new();
                for definition in &read.definitions {
                    setters.insert(resolve_setter(
                        definition,
                        &writers,
                        binding.register,
                        operation.id,
                        operand,
                    )?);
                }
                result.push(ReachingSetters {
                    clone: operation.id,
                    operand,
                    setters: setters.into_iter().collect(),
                });
            }
        }

        Ok(result)
    }

    /// Group the written register operands of all clones by the flow
    /// operation and register they map to.
    fn writers_by_flow(
        &self,
        bindings: &BTreeMap<(usize, usize), OperandFlow>,
    ) -> Result<BTreeMap<(usize, usize), Vec<Setter>>, LoopIrError> {
        let mut writers: BTreeMap<(usize, usize), Vec<Setter>> = BTreeMap::new();
        for operation in &self.operations {
            for (index, operand) in operation.instruction.operands.iter().enumerate() {
                if operand.direction != Direction::Write || operand.register.is_none() {
                    continue;
                }
                let binding = required_operand_flow(bindings, operation.id, index)?;
                writers
                    .entry((binding.operation, binding.register))
                    .or_default()
                    .push(Setter::Operation {
                        clone: operation.id,
                        operand: index,
                    });
            }
        }
        Ok(writers)
    }

    /// Convenience bridge once the caller has rebuilt expanded component flow.
    pub fn repair_from_flow(
        &self,
        bindings: &[OperandBinding],
        analysis: &flow::Analysis,
        operands: &[OperandFlow],
    ) -> Result<RepairedLoop, LoopIrError> {
        self.repair(bindings, &self.reaching_setters(analysis, operands)?)
    }

    /// Original `getSrcEpLid` location for a particular *clone*, not source ID.
    pub fn setter_location(&self, id: CloneId) -> Result<SetterLocation, LoopIrError> {
        let operation = self
            .operations
            .get(id)
            .ok_or(LoopIrError::InvalidClone(id))?;
        let Origin::Source { iteration_bias, .. } = operation.origin else {
            return Err(LoopIrError::InvalidSetter {
                clone: id,
                operand: 0,
            });
        };
        Ok(SetterLocation {
            loop_id: self.loop_id,
            part: self.plan.parts[operation.site.part].kind.into(),
            iteration_bias,
        })
    }

    /// Apply clone-specific allocation, then repair annotated offsets using
    /// actual rebuilt scalar definitions. This is transactional: on failure
    /// `self` and all source instructions remain unchanged.
    pub fn repair(
        &self,
        bindings: &[OperandBinding],
        reaching: &[ReachingSetters],
    ) -> Result<RepairedLoop, LoopIrError> {
        let mut result = self.clone();
        result.apply_bindings(bindings)?;
        let definitions = self.index_reaching(reaching)?;

        let mut corrections = Vec::new();
        for operation in &self.operations {
            for fixup in &operation.fixups {
                let operand = usize::from(fixup.operand);
                let input = definitions.get(&(operation.id, operand)).ok_or(
                    LoopIrError::MissingSetters {
                        clone: operation.id,
                        operand,
                    },
                )?;
                let setters = self.setter_locations(operation.id, operand, input)?;
                let location = self.setter_location(operation.id)?;
                let context = immediate::Context {
                    loop_id: self.loop_id,
                    part: location.part,
                    iteration_bias: Some(location.iteration_bias),
                    external_input: false,
                    setters,
                };
                let source = self
                    .operand(operation.id, operand)?
                    .immediate
                    .as_deref()
                    .ok_or(LoopIrError::InvalidOperand {
                        clone: operation.id,
                        operand,
                    })?;
                let applied = immediate::apply(&ImmediateValue::parse(source)?, fixup, &context)?;
                result.operand_mut(operation.id, operand)?.immediate = Some(applied.value.source());
                corrections.push((operation.id, applied));
            }
        }

        Ok(RepairedLoop {
            expanded: result,
            corrections,
        })
    }

    /// Install each binding's register on its operand, rejecting duplicates
    /// and bindings that do not name a register operand.
    fn apply_bindings(&mut self, bindings: &[OperandBinding]) -> Result<(), LoopIrError> {
        let mut bound = BTreeSet::new();
        for binding in bindings {
            let operand = self.operand_mut(binding.clone, binding.operand)?;
            if !bound.insert((binding.clone, binding.operand)) {
                return Err(LoopIrError::DuplicateBinding {
                    clone: binding.clone,
                    operand: binding.operand,
                });
            }
            if operand.register.is_none() || binding.register.trim().is_empty() {
                return Err(LoopIrError::InvalidBinding {
                    clone: binding.clone,
                    operand: binding.operand,
                });
            }
            operand.register = Some(binding.register.clone());
        }
        Ok(())
    }

    /// Index the reaching setters by clone and operand, rejecting entries
    /// for operands without a fixup and duplicates.
    fn index_reaching<'a>(
        &self,
        reaching: &'a [ReachingSetters],
    ) -> Result<BTreeMap<(CloneId, usize), &'a ReachingSetters>, LoopIrError> {
        let mut definitions = BTreeMap::new();
        for input in reaching {
            let key = (input.clone, input.operand);
            let operation = self
                .operations
                .get(input.clone)
                .ok_or(LoopIrError::InvalidClone(input.clone))?;
            if !operation
                .fixups
                .iter()
                .any(|fixup| usize::from(fixup.operand) == input.operand)
            {
                return Err(LoopIrError::UnexpectedSetters {
                    clone: input.clone,
                    operand: input.operand,
                });
            }
            if definitions.insert(key, input).is_some() {
                return Err(LoopIrError::DuplicateSetters {
                    clone: input.clone,
                    operand: input.operand,
                });
            }
        }
        Ok(definitions)
    }

    /// The locations of the setters reaching one fixed-up operand of a clone.
    fn setter_locations(
        &self,
        clone: CloneId,
        operand: usize,
        input: &ReachingSetters,
    ) -> Result<Vec<SetterLocation>, LoopIrError> {
        let mut seen = BTreeSet::new();
        let mut setters = Vec::new();
        for setter in &input.setters {
            if !seen.insert(*setter) {
                return Err(LoopIrError::DuplicateSetters { clone, operand });
            }
            setters.push(match *setter {
                Setter::External => SetterLocation::external(),
                Setter::Operation { clone, operand } => {
                    let value = self.operand(clone, operand)?;
                    if value.direction != Direction::Write || value.register.is_none() {
                        return Err(LoopIrError::InvalidSetter { clone, operand });
                    }
                    self.setter_location(clone)?
                }
            });
        }
        Ok(setters)
    }

    fn operand(&self, clone: CloneId, operand: usize) -> Result<&Operand, LoopIrError> {
        self.operations
            .get(clone)
            .ok_or(LoopIrError::InvalidClone(clone))?
            .instruction
            .operands
            .get(operand)
            .ok_or(LoopIrError::InvalidOperand { clone, operand })
    }

    fn operand_mut(&mut self, clone: CloneId, operand: usize) -> Result<&mut Operand, LoopIrError> {
        self.operations
            .get_mut(clone)
            .ok_or(LoopIrError::InvalidClone(clone))?
            .instruction
            .operands
            .get_mut(operand)
            .ok_or(LoopIrError::InvalidOperand { clone, operand })
    }
}

/// The setter a definition reaching a fixed-up operand stands for: external
/// when it comes from outside the expanded group, else its unique clone.
fn resolve_setter(
    definition: &Definition,
    writers: &BTreeMap<(usize, usize), Vec<Setter>>,
    register: flow::RegisterId,
    clone: CloneId,
    operand: usize,
) -> Result<Setter, LoopIrError> {
    match definition {
        Definition::Input => Ok(Setter::External),
        Definition::Uninitialized => Err(LoopIrError::UndefinedAddress {
            instruction: clone,
            operand,
        }),
        Definition::Operation(writer) => match writers.get(&(*writer, register)) {
            None => Ok(Setter::External),
            Some(values) if values.len() == 1 => Ok(values[0]),
            Some(_) => Err(LoopIrError::AmbiguousFlowSetter {
                operation: *writer,
                register,
            }),
        },
    }
}

fn reverse_branch(instruction: &mut Instruction) -> Result<(), LoopIrError> {
    let name = match instruction.metadata.name {
        "ibltz" => "ibgez",
        "ibgez" => "ibltz",
        "iblez" => "ibgtz",
        "ibgtz" => "iblez",
        "ibeq" => "ibne",
        "ibne" => "ibeq",
        _ => {
            return Err(LoopIrError::UnsupportedBranchSense(
                instruction.metadata.index,
            ));
        }
    };
    instruction.metadata =
        instruction::lookup(name, Syntax::Old).expect("original inverse branch descriptor");
    instruction.mnemonic = name.into();
    Ok(())
}

fn change_target(instruction: &mut Instruction, target: &str) -> Result<(), LoopIrError> {
    if instruction.metadata.subtype & subtype::branch::REGISTER != 0 {
        return Err(LoopIrError::InvalidBranchTarget);
    }
    let index = usize::from(instruction.metadata.target_operand)
        .checked_sub(1)
        .ok_or(LoopIrError::InvalidBranchTarget)?;
    let operand = instruction
        .operands
        .get_mut(index)
        .filter(|operand| operand.class == class::IMMEDIATE)
        .ok_or(LoopIrError::InvalidBranchTarget)?;
    operand.immediate = Some(target.into());
    operand.text = target.into();
    Ok(())
}

fn unconditional_branch(target: &str) -> Instruction {
    let metadata = instruction::lookup("b", Syntax::Old).expect("original branch descriptor");
    Instruction {
        metadata,
        mnemonic: "b".into(),
        mask: Mask(0),
        modifiers: 0,
        operands: vec![Operand {
            text: target.into(),
            class: class::IMMEDIATE,
            direction: Direction::Read,
            implicit: false,
            mask: Mask::X,
            register: None,
            forced_register: None,
            immediate: Some(target.into()),
            immediate_kind: metadata.operands[0].immediate_kind,
            tags: Vec::new(),
            field_letters: None,
        }],
        mode: ParseMode::Raw,
        raw_reorder: false,
        loop_id: 1,
        optional: false,
        generated_copy: None,
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit;
    use crate::loops::immediate::{AffineOperator, Phase};
    use crate::loops::pipeline::{LoopOperation, PartKind};
    use crate::parser::{self, ParserOptions, StatementKind};

    fn parse(source: &str) -> Vec<Instruction> {
        parser::parse_with_options(
            source,
            ParserOptions {
                allow_incomplete: true,
                ..ParserOptions::default()
            },
        )
        .unwrap()
        .statements
        .into_iter()
        .filter_map(|statement| match statement.kind {
            StatementKind::Instruction(instruction)
                if instruction.synthetic().is_none() && instruction.metadata.name != "nop" =>
            {
                Some(instruction)
            }
            _ => None,
        })
        .collect()
    }

    fn op(id: usize, row: u16, stage: u8, pipe: Pipe) -> LoopOperation {
        LoopOperation {
            id,
            row,
            stage,
            pipe,
            is_nop: false,
        }
    }

    fn raw_fixture(minimum: u32) -> (LoopSchedule, Vec<SourceOperation>) {
        let instructions =
            parse(crate::test_evidence::text("inputs/pipeline-raw-stages-min1.vcl").as_str());
        let schedule = LoopSchedule {
            period: 5,
            operations: vec![
                op(0, 0, 0, Pipe::Upper),
                op(1, 1, 1, Pipe::Upper),
                op(2, 1, 0, Pipe::Lower),
                op(3, 2, 2, Pipe::Upper),
                op(4, 3, 0, Pipe::Lower),
            ],
            conditional: 4,
            known_minimum: minimum,
            known_safe: 0,
            unique: "PIPE".into(),
            name: "loop".into(),
        };
        let sources = instructions
            .into_iter()
            .enumerate()
            .map(|(id, instruction)| SourceOperation {
                id,
                instruction,
                fixups: Vec::new(),
            })
            .collect();
        (schedule, sources)
    }

    fn clone_at(expanded: &ExpandedLoop, part: usize, source: usize) -> &CloneOperation {
        expanded
            .operations
            .iter()
            .find(|operation| {
                operation.site.part == part
                    && matches!(operation.origin, Origin::Source { id, .. } if id == source)
            })
            .unwrap()
    }

    fn bias(operation: &CloneOperation) -> i16 {
        match operation.origin {
            Origin::Source { iteration_bias, .. } => iteration_bias,
            Origin::DrainBranch => panic!("not a source clone"),
        }
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn raw_original_stage_windows_have_distinct_clone_ids_and_biases() {
        let (schedule, sources) = raw_fixture(1);
        let expanded = expand(&schedule, &sources, 7).unwrap();
        assert_eq!(
            expanded.rows.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![0, 5, 5, 5, 10, 10, 0]
        );
        let first = clone_at(&expanded, 1, 0);
        let second = clone_at(&expanded, 2, 0);
        let kernel = clone_at(&expanded, 3, 0);
        assert_ne!(first.id, second.id);
        assert_ne!(second.id, kernel.id);
        assert_eq!((bias(first), bias(second), bias(kernel)), (0, 1, 2));
        assert_eq!(bias(clone_at(&expanded, 3, 1)), 1);
        assert_eq!(bias(clone_at(&expanded, 3, 3)), 0);
        assert_eq!(
            expanded.setter_location(kernel.id).unwrap(),
            SetterLocation {
                loop_id: 7,
                part: immediate::LoopPart::Kernel,
                iteration_bias: 2
            }
        );
        for operation in &expanded.operations {
            assert_eq!(
                expanded.rows[operation.site.part][operation.site.row][operation.site.pipe.index()],
                Some(operation.id)
            );
            assert_eq!(operation.instruction.loop_id, 1);
        }
        assert_eq!(sources[3].instruction.loop_id, 3);
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn raw_original_early_exit_and_kernel_branches_are_typed_and_retargeted() {
        let (schedule, sources) = raw_fixture(1);
        let expanded = expand(&schedule, &sources, 1).unwrap();
        let expected = &crate::test_evidence::text("oracle/pipeline-raw-stages-min1/stdout.bin");
        for (part, target, sense) in [(1, 5, "ibeq"), (2, 4, "ibeq"), (3, 3, "ibne")] {
            let branch = clone_at(&expanded, part, 4);
            assert_eq!(branch.instruction.metadata.name, sense);
            assert_eq!(
                branch.instruction.operands[2].immediate.as_deref(),
                Some(expanded.plan.parts[target].name.as_str())
            );
            let rendered = emit::instruction(&branch.instruction, &[], 0).unwrap();
            assert!(
                expected.contains(&rendered),
                "missing original branch: {rendered}"
            );
        }
        let drain = expanded
            .operations
            .iter()
            .find(|operation| operation.origin == Origin::DrainBranch)
            .unwrap();
        assert_eq!(
            drain.site,
            Site {
                part: 4,
                row: 8,
                pipe: Pipe::Lower
            }
        );
        assert_eq!(drain.instruction.metadata.name, "b");
        assert_eq!(
            drain.instruction.operands[0].immediate.as_deref(),
            Some(expanded.plan.parts[6].name.as_str())
        );
        assert!(expected.contains(&emit::instruction(&drain.instruction, &[], 0).unwrap()));
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn known_minimum_groups_warmup_without_inventing_tests_or_drain_branches() {
        let (schedule, sources) = raw_fixture(4);
        let expanded = expand(&schedule, &sources, 1).unwrap();
        assert_eq!(
            expanded.rows.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![0, 10, 5, 10, 0]
        );
        assert_eq!(expanded.plan.parts[1].kind, PartKind::Prologue { index: 0 });
        assert!(
            expanded
                .operations
                .iter()
                .all(|operation| operation.origin != Origin::DrainBranch)
        );
        assert_eq!(
            expanded
                .operations
                .iter()
                .filter(|operation| operation.instruction.metadata.is_branch())
                .count(),
            1
        );
        // These MUL copies still exist here. Original later flow pruning
        // removes both; stage-window cloning must not silently emulate that.
        assert_eq!(
            expanded
                .operations
                .iter()
                .filter(|operation| operation.site.part == 1
                    && matches!(operation.origin, Origin::Source { id: 0, .. }))
                .count(),
            2
        );
        let original = &crate::test_evidence::text("oracle/pipeline-raw-stages-min4/stdout.bin");
        let warmup = original
            .split("[EXPL_PIPE_loop__PRO0]")
            .nth(1)
            .unwrap()
            .split("EXPL_PIPE_loop__MAIN_LOOP:")
            .next()
            .unwrap();
        assert!(!warmup.contains("mul"));
        assert!(warmup.contains("VI01,VI01,0x00000000"));
        assert!(warmup.contains("VI01,VI01,0x00000002"));
    }

    fn affine_fixture(
        operator: AffineOperator,
        phase: Phase,
    ) -> (LoopSchedule, Vec<SourceOperation>) {
        let instructions = parse(
            "lq vf03,5(vi02)\n\
                   mul vf04,vf01,vf01\n\
                   iaddiu vi02,vi02,3\n\
                   ibne vi01,vi00,loop\n",
        );
        let sources = instructions
            .into_iter()
            .enumerate()
            .map(|(id, instruction)| SourceOperation {
                id,
                instruction,
                fixups: if id == 0 {
                    vec![Fixup {
                        operator,
                        phase,
                        operand: 1,
                        stride: ImmediateValue::Integer(3),
                    }]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let schedule = LoopSchedule {
            period: 4,
            operations: vec![
                op(0, 0, 0, Pipe::Lower),
                op(1, 0, 1, Pipe::Upper),
                op(2, 1, 0, Pipe::Lower),
                op(3, 2, 0, Pipe::Lower),
            ],
            conditional: 3,
            known_minimum: 1,
            known_safe: 0,
            unique: "IMM".into(),
            name: "loop".into(),
        };
        (schedule, sources)
    }

    fn affine_reaching(expanded: &ExpandedLoop) -> Vec<ReachingSetters> {
        let prologue_update = clone_at(expanded, 1, 2).id;
        let kernel_update = clone_at(expanded, 2, 2).id;
        vec![
            ReachingSetters {
                clone: clone_at(expanded, 1, 0).id,
                operand: 1,
                setters: vec![Setter::External],
            },
            ReachingSetters {
                clone: clone_at(expanded, 2, 0).id,
                operand: 1,
                setters: vec![
                    Setter::Operation {
                        clone: prologue_update,
                        operand: 0,
                    },
                    Setter::Operation {
                        clone: kernel_update,
                        operand: 0,
                    },
                ],
            },
        ]
    }

    #[test]
    fn actual_prologue_kernel_setter_join_repairs_offsets_in_both_parts() {
        for (operator, expected) in [(AffineOperator::Add, "8"), (AffineOperator::Subtract, "2")] {
            let (schedule, sources) = affine_fixture(operator, Phase::After);
            let expanded = expand(&schedule, &sources, 4).unwrap();
            let repaired = expanded.repair(&[], &affine_reaching(&expanded)).unwrap();
            assert_eq!(repaired.corrections.len(), 2);
            for part in [1, 2] {
                let operation = clone_at(&repaired.expanded, part, 0);
                assert_eq!(
                    operation.instruction.operands[1].immediate.as_deref(),
                    Some(expected)
                );
                assert_eq!(
                    clone_at(&expanded, part, 0).instruction.operands[1]
                        .immediate
                        .as_deref(),
                    Some("5")
                );
            }
            assert_eq!(
                sources[0].instruction.operands[1].immediate.as_deref(),
                Some("5")
            );
        }
    }

    #[test]
    fn before_phase_keeps_original_offsets_when_actual_epoch_matches() {
        let (schedule, sources) = affine_fixture(AffineOperator::Add, Phase::Before);
        let expanded = expand(&schedule, &sources, 4).unwrap();
        let repaired = expanded.repair(&[], &affine_reaching(&expanded)).unwrap();
        assert!(
            repaired
                .corrections
                .iter()
                .all(|(_, applied)| applied.correction.factor == 0)
        );
        assert_eq!(
            clone_at(&repaired.expanded, 2, 0).instruction.operands[1]
                .immediate
                .as_deref(),
            Some("5")
        );
    }

    #[test]
    fn symbolic_offsets_and_strides_survive_typed_clone_repair() {
        let (schedule, mut sources) = affine_fixture(AffineOperator::Add, Phase::After);
        sources[0].instruction.operands[1].immediate = Some("BASE+4".into());
        sources[0].fixups[0].stride = ImmediateValue::Symbolic("STRIDE+1".into());
        let expanded = expand(&schedule, &sources, 4).unwrap();
        let repaired = expanded.repair(&[], &affine_reaching(&expanded)).unwrap();
        assert_eq!(
            clone_at(&repaired.expanded, 2, 0).instruction.operands[1]
                .immediate
                .as_deref(),
            Some("(BASE+4)+((STRIDE+1)*1)")
        );
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn per_clone_operand_bindings_do_not_rename_siblings_or_source_operands() {
        let (schedule, sources) = raw_fixture(1);
        let expanded = expand(&schedule, &sources, 1).unwrap();
        let drain_mul = clone_at(&expanded, 4, 1).id;
        let repaired = expanded
            .repair(
                &[OperandBinding {
                    clone: drain_mul,
                    operand: 0,
                    register: "VF01".into(),
                }],
                &[],
            )
            .unwrap();
        assert_eq!(
            repaired.expanded.operations[drain_mul].instruction.operands[0]
                .register
                .as_deref(),
            Some("VF01")
        );
        assert_eq!(
            clone_at(&repaired.expanded, 3, 1).instruction.operands[0]
                .register
                .as_deref(),
            Some("vf04")
        );
        assert_eq!(
            sources[1].instruction.operands[0].register.as_deref(),
            Some("vf04")
        );
    }

    #[test]
    fn missing_actual_reaching_definitions_are_an_error_not_a_source_order_guess() {
        let (schedule, sources) = affine_fixture(AffineOperator::Add, Phase::After);
        let expanded = expand(&schedule, &sources, 1).unwrap();
        assert!(matches!(
            expanded.repair(&[], &[]),
            Err(LoopIrError::MissingSetters { .. })
        ));
        let mut reaches = affine_reaching(&expanded);
        reaches[0].setters.clear();
        assert!(matches!(
            expanded.repair(&[], &reaches),
            Err(LoopIrError::Immediate(immediate::ImmediateError::NoSetters))
        ));
        // An instruction identity cannot substitute for a *writing operand*.
        reaches[0].setters = vec![Setter::Operation {
            clone: clone_at(&expanded, 1, 2).id,
            operand: 1,
        }];
        assert!(matches!(
            expanded.repair(&[], &reaches),
            Err(LoopIrError::InvalidSetter { .. })
        ));
    }

    #[test]
    fn duplicate_bindings_and_annotations_and_unexpected_facts_are_rejected() {
        let (schedule, mut sources) = affine_fixture(AffineOperator::Add, Phase::After);
        let expanded = expand(&schedule, &sources, 1).unwrap();
        let binding = OperandBinding {
            clone: 0,
            operand: 1,
            register: "VI03".into(),
        };
        assert!(matches!(
            expanded.repair(&[binding.clone(), binding], &[]),
            Err(LoopIrError::DuplicateBinding { .. })
        ));
        let mut reaches = affine_reaching(&expanded);
        reaches.push(reaches[0].clone());
        assert!(matches!(
            expanded.repair(&[], &reaches),
            Err(LoopIrError::DuplicateSetters { .. })
        ));
        reaches = affine_reaching(&expanded);
        reaches[0].setters.push(Setter::External);
        assert!(matches!(
            expanded.repair(&[], &reaches),
            Err(LoopIrError::DuplicateSetters { .. })
        ));
        let duplicate = sources[0].fixups[0].clone();
        sources[0].fixups.push(duplicate);
        assert!(matches!(
            expand(&schedule, &sources, 1),
            Err(LoopIrError::DuplicateFixup { .. })
        ));
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn source_id_and_slot_validation_catches_adapter_wiring_errors() {
        let (schedule, sources) = raw_fixture(1);
        assert!(matches!(
            expand(&schedule, &sources, 0),
            Err(LoopIrError::InvalidLoopId(0))
        ));
        assert!(matches!(
            expand(&schedule, &sources[1..], 1),
            Err(LoopIrError::MissingSource(0))
        ));
        let mut duplicate = sources.clone();
        duplicate.push(sources[0].clone());
        assert!(matches!(
            expand(&schedule, &duplicate, 1),
            Err(LoopIrError::DuplicateSource(0))
        ));
        let mut wrong = schedule.clone();
        wrong.operations[0].pipe = Pipe::Lower;
        assert!(matches!(
            expand(&wrong, &sources, 1),
            Err(LoopIrError::InvalidSlot { source: 0, .. })
        ));
    }

    #[test]
    fn all_original_conditional_descriptors_have_symmetric_inverse_senses() {
        for (original, inverse, operands) in [
            ("ibltz", "ibgez", "vi01,loop"),
            ("iblez", "ibgtz", "vi01,loop"),
            ("ibgtz", "iblez", "vi01,loop"),
            ("ibgez", "ibltz", "vi01,loop"),
            ("ibeq", "ibne", "vi01,vi00,loop"),
            ("ibne", "ibeq", "vi01,vi00,loop"),
        ] {
            let mut instruction = parse(&format!("{original} {operands}\n")).remove(0);
            let old = instruction.clone();
            reverse_branch(&mut instruction).unwrap();
            assert_eq!(instruction.metadata.name, inverse);
            assert_eq!(instruction.operands, old.operands);
            reverse_branch(&mut instruction).unwrap();
            assert_eq!(instruction, old);
        }
    }

    fn flow_model<'a>(
        instructions: impl Iterator<Item = (usize, &'a Instruction)>,
        blocks: Vec<flow::Block>,
        inputs: &[&str],
        outputs: &[&str],
    ) -> (flow::Graph, Vec<OperandFlow>, BTreeMap<String, usize>) {
        let instructions: Vec<_> = instructions.collect();
        let mut names = BTreeMap::new();
        let mut operations = vec![
            flow::Operation::default();
            instructions.iter().map(|(id, _)| id + 1).max().unwrap_or(0)
        ];
        let mut operands = Vec::new();
        for (id, instruction) in instructions {
            operations[id].side_effect = instruction.metadata.is_branch();
            for (operand, value) in instruction.operands.iter().enumerate() {
                let Some(name) = value
                    .register
                    .as_ref()
                    .map(|name| name.to_ascii_lowercase())
                else {
                    continue;
                };
                let next = names.len();
                let register = *names.entry(name).or_insert(next);
                operands.push(OperandFlow {
                    instruction: id,
                    operand,
                    operation: id,
                    register,
                });
                let access = flow::Access {
                    register,
                    mask: if instruction::register_family(value.class)
                        == instruction::RegisterFamily::Integer
                    {
                        1
                    } else {
                        value.mask.0
                    },
                };
                match value.direction {
                    Direction::Read => operations[id].reads.push(access),
                    Direction::Write => operations[id].writes.push(access),
                    Direction::Unknown => {}
                }
            }
        }
        let access = |name: &str| flow::Access {
            register: names[name],
            mask: if name.starts_with("vi") { 1 } else { 15 },
        };
        let mut graph = flow::Graph {
            operations,
            blocks,
            entry: 0,
            inputs: inputs.iter().map(|name| access(name)).collect(),
            constants: ["vi00", "vf00"]
                .iter()
                .filter_map(|name| names.get(*name).copied())
                .collect(),
        };
        graph.blocks.last_mut().unwrap().outputs =
            outputs.iter().map(|name| access(name)).collect();
        (graph, operands, names)
    }

    fn expanded_flow(
        expanded: &ExpandedLoop,
        inputs: &[&str],
        outputs: &[&str],
    ) -> (flow::Graph, Vec<OperandFlow>, BTreeMap<String, usize>) {
        let blocks = expanded
            .plan
            .parts
            .iter()
            .enumerate()
            .map(|(id, part)| flow::Block {
                operations: expanded.rows[id]
                    .iter()
                    .flatten()
                    .filter_map(|value| *value)
                    .collect(),
                successors: part.outgoing.iter().map(|edge| edge.target).collect(),
                outputs: Vec::new(),
                timing: None,
                ..flow::Block::default()
            })
            .collect();
        flow_model(
            expanded
                .operations
                .iter()
                .map(|op| (op.id, &op.instruction)),
            blocks,
            inputs,
            outputs,
        )
    }

    #[test]
    fn source_annotation_distinguishes_local_before_from_backedge_after() {
        let (schedule, sources) = affine_fixture(AffineOperator::Add, Phase::Before);
        let mut sources = sources;
        sources[0].fixups.clear();
        let (graph, operands, names) = flow_model(
            sources.iter().map(|op| (op.id, &op.instruction)),
            vec![
                flow::Block {
                    operations: vec![0, 1, 2, 3],
                    successors: vec![0, 1],
                    outputs: Vec::new(),
                    timing: None,
                    ..flow::Block::default()
                },
                flow::Block::default(),
            ],
            &["vi01", "vi02", "vf01"],
            &["vi02", "vf04"],
        );
        let analysis = graph.analyze().unwrap();
        // Whole-CFG flow intentionally has both entry and previous-iteration
        // definitions. Neither source-order guessing nor bias matching works.
        let read = scalar_read(&analysis, 0, names["vi02"]).unwrap();
        assert_eq!(
            read.definitions,
            BTreeSet::from([Definition::Input, Definition::Operation(2)])
        );
        let annotated = annotate_sources(&sources, &graph, &analysis, 0, &operands).unwrap();
        assert_eq!(annotated.annotations.len(), 1);
        assert_eq!(annotated.annotations[0].phase, Phase::Before);
        assert_eq!(annotated.annotations[0].update, 2);
        assert!(sources[0].fixups.is_empty());
        assert_eq!(annotated.sources[0].fixups[0].phase, Phase::Before);
        let expanded = expand(&schedule, &annotated.sources, 1).unwrap();
        let (expanded_graph, flow, _) =
            expanded_flow(&expanded, &["vi01", "vi02", "vf01"], &["vi02", "vf04"]);
        let repaired = expanded
            .repair_from_flow(&[], &expanded_graph.analyze().unwrap(), &flow)
            .unwrap();
        assert!(
            repaired
                .corrections
                .iter()
                .all(|(_, applied)| applied.correction.factor == 0)
        );
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn original_after_update_load_gets_eight_from_actual_external_and_kernel_join() {
        let (schedule, mut sources) = affine_fixture(AffineOperator::Add, Phase::After);
        sources[0].fixups.clear();
        let (graph, operands, _) = flow_model(
            sources.iter().map(|op| (op.id, &op.instruction)),
            vec![
                flow::Block {
                    operations: vec![2, 0, 1, 3],
                    successors: vec![0, 1],
                    outputs: Vec::new(),
                    timing: None,
                    ..flow::Block::default()
                },
                flow::Block::default(),
            ],
            &["vi01", "vi02", "vf01"],
            &["vi02", "vf04"],
        );
        let annotated =
            annotate_sources(&sources, &graph, &graph.analyze().unwrap(), 0, &operands).unwrap();
        assert_eq!(annotated.annotations[0].phase, Phase::After);
        let expanded = expand(&schedule, &annotated.sources, 1).unwrap();
        let (graph, operands, _) =
            expanded_flow(&expanded, &["vi01", "vi02", "vf01"], &["vi02", "vf04"]);
        let analysis = graph.analyze().unwrap();
        let reaching = expanded.reaching_setters(&analysis, &operands).unwrap();
        assert_eq!(reaching, affine_reaching(&expanded));
        let repaired = expanded
            .repair_from_flow(&[], &analysis, &operands)
            .unwrap();
        for part in [1, 2] {
            assert_eq!(
                clone_at(&repaired.expanded, part, 0).instruction.operands[1]
                    .immediate
                    .as_deref(),
                Some("8")
            );
        }
        let original =
            &crate::test_evidence::text("oracle/immediate-scheduled-after-add/stdout.bin");
        assert_eq!(original.matches("8(VI02)").count(), 2);
        assert!(
            crate::test_evidence::text("inputs/immediate-scheduled-after-add.vcl")
                .as_str()
                .contains(
                    "iaddiu p,p,3\n\
                           lq c,5(p)"
                )
        );
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn raw_stage_read_versions_follow_actual_cfg_not_equal_iteration_biases() {
        for minimum in [1, 4] {
            let (schedule, sources) = raw_fixture(minimum);
            let expanded = expand(&schedule, &sources, 1).unwrap();
            let (graph, operands, names) =
                expanded_flow(&expanded, &["vi01", "vf01", "vf02"], &["vi01", "vf02"]);
            let analysis = graph.analyze().unwrap();
            assert!(
                expanded
                    .reaching_setters(&analysis, &operands)
                    .unwrap()
                    .is_empty()
            );
            let setter = clone_at(&expanded, expanded.plan.kernel, 0);
            let reader = clone_at(&expanded, expanded.plan.kernel, 1);
            assert_eq!((bias(setter), bias(reader)), (2, 1));
            let read = scalar_read(&analysis, reader.id, names["vf03"]).unwrap();
            assert_eq!(
                read.definitions,
                BTreeSet::from([Definition::Operation(setter.id)])
            );
        }
    }

    #[test]
    fn multiple_setters_and_nonvisible_inductions_are_not_annotated() {
        let (_, mut sources) = affine_fixture(AffineOperator::Add, Phase::After);
        sources[0].fixups.clear();
        let mut second = sources[2].clone();
        second.id = 4;
        sources.push(second);
        let (graph, operands, _) = flow_model(
            sources.iter().map(|op| (op.id, &op.instruction)),
            vec![
                flow::Block {
                    operations: vec![2, 0, 1, 4, 3],
                    successors: vec![0, 1],
                    outputs: Vec::new(),
                    timing: None,
                    ..flow::Block::default()
                },
                flow::Block::default(),
            ],
            &["vi01", "vi02", "vf01"],
            &["vi02", "vf04"],
        );
        assert!(
            annotate_sources(&sources, &graph, &graph.analyze().unwrap(), 0, &operands)
                .unwrap()
                .annotations
                .is_empty()
        );
        let mut analysis = graph.analyze().unwrap();
        analysis.live_out[0].clear();
        let mut unique = graph.clone();
        unique.blocks[0]
            .operations
            .retain(|&operation| operation != 4);
        assert!(
            annotate_sources(&sources[..4], &unique, &analysis, 0, &operands)
                .unwrap()
                .annotations
                .is_empty()
        );
    }

    #[test]
    fn missing_clone_writer_and_undefined_flow_addresses_fail_explicitly() {
        let (schedule, sources) = affine_fixture(AffineOperator::Add, Phase::After);
        let expanded = expand(&schedule, &sources, 1).unwrap();
        let (graph, mut operands, names) =
            expanded_flow(&expanded, &["vi01", "vi02", "vf01"], &["vi02", "vf04"]);
        let analysis = graph.analyze().unwrap();
        let writer = clone_at(&expanded, 1, 2).id;
        operands.retain(|operand| !(operand.instruction == writer && operand.operand == 0));
        assert!(matches!(
            expanded.reaching_setters(&analysis, &operands),
            Err(LoopIrError::MissingOperandFlow { .. })
        ));
        let (graph, operands, _) = expanded_flow(&expanded, &["vi01", "vf01"], &["vi02", "vf04"]);
        assert!(matches!(
            expanded.reaching_setters(&graph.analyze().unwrap(), &operands),
            Err(LoopIrError::UndefinedAddress { .. })
        ));
        let mut external = analysis;
        let consumer = clone_at(&expanded, 1, 0).id;
        external
            .reads
            .iter_mut()
            .find(|read| {
                read.operation == consumer && read.register == names["vi02"] && read.component == 0
            })
            .unwrap()
            .definitions = BTreeSet::from([Definition::Operation(999)]);
        let mut mapped = operands;
        // The new graph has identical register/operation IDs; the producer
        // outside this generated loop has no cloned writing operand.
        mapped.sort_by_key(|value| (value.instruction, value.operand));
        let reaching = expanded.reaching_setters(&external, &mapped).unwrap();
        assert_eq!(reaching[0].setters, vec![Setter::External]);
    }

    #[test]
    fn automatic_address_updates_copy_the_original_unit_stride_annotation() {
        for (source, operator) in [
            ("lqi vf03,(vi02++)", AffineOperator::Add),
            ("lqd vf03,(--vi02)", AffineOperator::Subtract),
        ] {
            let sources: Vec<_> = parse(&format!(
                "{source}\n\
                                lq vf04,5(vi02)\n\
                                ibne vi01,vi00,loop\n"
            ))
            .into_iter()
            .enumerate()
            .map(|(id, instruction)| SourceOperation {
                id,
                instruction,
                fixups: Vec::new(),
            })
            .collect();
            let (graph, operands, _) = flow_model(
                sources.iter().map(|op| (op.id, &op.instruction)),
                vec![
                    flow::Block {
                        operations: vec![0, 1, 2],
                        successors: vec![0, 1],
                        outputs: Vec::new(),
                        timing: None,
                        ..flow::Block::default()
                    },
                    flow::Block::default(),
                ],
                &["vi01", "vi02"],
                &["vi02", "vf03", "vf04"],
            );
            let result =
                annotate_sources(&sources, &graph, &graph.analyze().unwrap(), 0, &operands)
                    .unwrap();
            assert_eq!(result.annotations.len(), 1);
            let fixup = &result.sources[1].fixups[0];
            assert_eq!(fixup.operator, operator);
            assert_eq!(fixup.phase, Phase::After);
            assert_eq!(fixup.stride, ImmediateValue::Integer(1));
        }
    }

    #[test]
    fn flow_operation_numbers_are_independent_of_source_and_clone_ids() {
        let (schedule, mut sources) = affine_fixture(AffineOperator::Add, Phase::After);
        sources[0].fixups.clear();
        let (mut graph, mut operands, _) = flow_model(
            sources.iter().map(|op| (op.id, &op.instruction)),
            vec![
                flow::Block {
                    operations: vec![2, 0, 1, 3],
                    successors: vec![0, 1],
                    outputs: Vec::new(),
                    timing: None,
                    ..flow::Block::default()
                },
                flow::Block::default(),
            ],
            &["vi01", "vi02", "vf01"],
            &["vi02", "vf04"],
        );
        let renumber = |graph: &mut flow::Graph, operands: &mut [OperandFlow]| {
            let mut prefix = vec![flow::Operation::default(); 10];
            prefix.append(&mut graph.operations);
            graph.operations = prefix;
            for block in &mut graph.blocks {
                for operation in &mut block.operations {
                    *operation += 10;
                }
            }
            for operand in operands {
                operand.operation += 10;
            }
        };
        renumber(&mut graph, &mut operands);
        let annotated =
            annotate_sources(&sources, &graph, &graph.analyze().unwrap(), 0, &operands).unwrap();
        assert_eq!(annotated.annotations[0].update, 2);
        let expanded = expand(&schedule, &annotated.sources, 1).unwrap();
        let (mut graph, mut operands, _) =
            expanded_flow(&expanded, &["vi01", "vi02", "vf01"], &["vi02", "vf04"]);
        renumber(&mut graph, &mut operands);
        let analysis = graph.analyze().unwrap();
        assert_eq!(
            expanded.reaching_setters(&analysis, &operands).unwrap(),
            affine_reaching(&expanded)
        );
        assert_eq!(
            clone_at(
                &expanded
                    .repair_from_flow(&[], &analysis, &operands)
                    .unwrap()
                    .expanded,
                2,
                0
            )
            .instruction
            .operands[1]
                .immediate
                .as_deref(),
            Some("8")
        );
    }
}
