//! Component-level reaching definitions and liveness for the reconstruction IR.
//!
//! The original driver rebuilds dependencies, backtraces
//! component definitions across streams, and prunes to a fixed
//! point. This module expresses those data-flow requirements
//! with owned sets rather than the original intrusive register-lock lists.
//! Exact equivalence of special hardware-register locks is tracked in docs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

pub type RegisterId = usize;
pub type OperationId = usize;
pub type BlockId = usize;
type Component = (RegisterId, u8);
type State = BTreeMap<Component, BTreeSet<Definition>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Access {
    pub register: RegisterId,
    pub mask: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Operation {
    pub reads: Vec<Access>,
    pub writes: Vec<Access>,
    pub side_effect: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub operations: Vec<OperationId>,
    pub successors: Vec<BlockId>,
    /// Values that must remain available when leaving this stream/program.
    pub outputs: Vec<Access>,
    /// Issue layout of a fixed table; `None` keeps program-order semantics.
    pub timing: Option<Timing>,
    /// A call (`bal`, `jalr`): every successor is a subroutine entry, and the
    /// subroutine returns to this stream.
    pub call_return: Option<BlockId>,
    /// Successors that are inferred call returns of an indirect branch.
    pub returns: Vec<BlockId>,
}

/// Issue cycles of a fixed table. A non-interlocked write (Q, P) listed in
/// `delays` becomes visible `delay` cycles after issue, possibly in a
/// successor block; a read at cycle `t` sees writes landed at or before `t`.
#[derive(Clone, Debug, Default)]
pub struct Timing {
    /// Issue cycle of each operation, parallel to `Block::operations`.
    pub issue: Vec<u32>,
    pub length: u32,
    pub delays: BTreeMap<(OperationId, RegisterId), u32>,
}

/// Reaching definitions plus writes in flight: (landing cycle relative to
/// the block start, component) to definitions, and whether every incoming
/// path carries that write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FlowState {
    defs: State,
    pending: BTreeMap<(u32, Component), (BTreeSet<Definition>, bool)>,
}

impl FlowState {
    /// Apply in-flight writes landing at or before `cycle` (all if `None`).
    fn land(&mut self, cycle: Option<u32>) {
        let due: Vec<_> = self
            .pending
            .keys()
            .copied()
            .filter(|&(time, _)| cycle.is_none_or(|cycle| time <= cycle))
            .collect();
        for key in due {
            let (definitions, complete) = self.pending.remove(&key).unwrap();
            if complete {
                self.defs.insert(key.1, definitions);
            } else {
                self.defs.entry(key.1).or_default().extend(definitions);
            }
        }
    }

    fn join(states: &[&FlowState]) -> FlowState {
        let mut result = FlowState::default();
        for state in states {
            for (&component, defs) in &state.defs {
                result.defs.entry(component).or_default().extend(defs);
            }
            for (&key, (defs, _)) in &state.pending {
                result.pending.entry(key).or_default().0.extend(defs);
            }
        }
        for (key, (_, complete)) in result.pending.iter_mut() {
            *complete = states
                .iter()
                .all(|state| state.pending.get(key).is_some_and(|(_, c)| *c));
        }
        result
    }
}

#[derive(Clone, Debug)]
pub struct Graph {
    pub operations: Vec<Operation>,
    pub blocks: Vec<Block>,
    pub entry: BlockId,
    pub inputs: Vec<Access>,
    /// Hardware zero/constant registers are always initialized; their writes
    /// have no effect, including writes to VF00 and VI00.
    pub constants: BTreeSet<RegisterId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Definition {
    Uninitialized,
    Input,
    Operation(OperationId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub operation: OperationId,
    pub register: RegisterId,
    pub component: u8,
    pub definitions: BTreeSet<Definition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndefinedRead {
    pub operation: Option<OperationId>,
    pub block: BlockId,
    pub register: RegisterId,
    pub component: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub block: BlockId,
    pub register: RegisterId,
    pub component: u8,
    pub definitions: BTreeSet<Definition>,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub reachable: BTreeSet<BlockId>,
    pub reads: Vec<Read>,
    pub outputs: Vec<Output>,
    pub live_operations: BTreeSet<OperationId>,
    pub undefined: Vec<UndefinedRead>,
    /// Live components at the input and output of each block, conventional
    /// backward liveness. Kept separate from reaching-definition identities.
    pub live_in: Vec<BTreeSet<Access>>,
    pub live_out: Vec<BTreeSet<Access>>,
    /// Live-out of each reachable copy of a block: one entry per block
    /// outside calls, one per call string inside a subroutine. `live_out`
    /// is their union.
    pub copy_live_out: Vec<Vec<BTreeSet<Access>>>,
    /// Non-interlocked hazards: a live read in a fixed table
    /// that issues while a write from another block is still in flight sees
    /// the older value. The reader's block keeps its reads of that component
    /// in place and the writer's block its writes;
    /// both are (block, register, component).
    pub hazard_reads: BTreeSet<(BlockId, RegisterId, u8)>,
    pub hazard_writes: BTreeSet<(BlockId, RegisterId, u8)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowError {
    InvalidEntry(BlockId),
    InvalidSuccessor {
        block: BlockId,
        successor: BlockId,
    },
    InvalidOperation {
        block: BlockId,
        operation: OperationId,
    },
    ReusedOperation(OperationId),
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid control flow: {self:?}")
    }
}

impl std::error::Error for FlowError {}

fn parts(access: Access) -> impl Iterator<Item = Component> {
    (0..8)
        .filter_map(move |part| (access.mask & (1 << part) != 0).then_some((access.register, part)))
}

/// Walk a block in issue order, calling `visit` with the definitions each
/// operation's reads see.
fn walk(
    graph: &Graph,
    block: &Block,
    mut state: FlowState,
    mut visit: impl FnMut(OperationId, &FlowState),
) -> FlowState {
    let timing = block.timing.as_ref();
    if timing.is_none() {
        state.land(None);
    }
    for (index, &id) in block.operations.iter().enumerate() {
        let time = timing.map(|timing| timing.issue[index]);
        if time.is_some() {
            state.land(time);
        }
        visit(id, &state);
        for &access in &graph.operations[id].writes {
            if graph.constants.contains(&access.register) {
                continue;
            }
            let delay = timing
                .and_then(|timing| timing.delays.get(&(id, access.register)))
                .copied()
                .filter(|&delay| delay > 0);
            for component in parts(access) {
                let definitions = BTreeSet::from([Definition::Operation(id)]);
                match (time, delay) {
                    (Some(time), Some(delay)) => {
                        state
                            .pending
                            .insert((time + delay, component), (definitions, true));
                    }
                    _ => {
                        state.defs.insert(component, definitions);
                    }
                }
            }
        }
    }
    if let Some(timing) = timing {
        state.land(Some(timing.length.saturating_sub(1)));
        state.pending = std::mem::take(&mut state.pending)
            .into_iter()
            .map(|((time, component), value)| ((time - timing.length, component), value))
            .collect();
    }
    state
}

/// Deepest call nesting given its own copies; deeper calls share them.
const CALL_DEPTH: usize = 8;

impl Graph {
    /// Data flow follows each call into its own copy of the subroutine, which
    /// returns only to that caller (caller-sensitive tracing).
    /// The results of every copy fold back onto the original blocks and
    /// operations.
    pub fn analyze(&self) -> Result<Analysis, FlowError> {
        if self.entry >= self.blocks.len() {
            return Err(FlowError::InvalidEntry(self.entry));
        }
        if self.blocks.iter().all(|block| block.call_return.is_none()) {
            return self.analyze_flat();
        }
        for (id, block) in self.blocks.iter().enumerate() {
            if let Some(&successor) = block.successors.iter().find(|&&s| s >= self.blocks.len()) {
                return Err(FlowError::InvalidSuccessor {
                    block: id,
                    successor,
                });
            }
        }
        // Deep call strings can multiply copies; shallower ones still keep
        // each return with its immediate caller.
        let budget = 4096 + 16 * self.blocks.len();
        let mut depth = CALL_DEPTH;
        let (expanded, block_origin, operation_origin) = loop {
            if let Some(expansion) = self.expand(depth, budget) {
                break expansion;
            }
            depth /= 2;
        };
        let analysis = expanded.analyze_flat()?;
        Ok(analysis.fold(self.blocks.len(), &block_origin, &operation_origin))
    }

    /// Copy subroutines per call string of at most `depth` calls. Blocks
    /// outside any call keep their ids; copies and their operations are
    /// appended. `None` when the copies exceed `budget` blocks.
    fn expand(
        &self,
        depth: usize,
        budget: usize,
    ) -> Option<(Graph, Vec<BlockId>, Vec<OperationId>)> {
        let mut graph = Graph {
            operations: self.operations.clone(),
            blocks: self.blocks.clone(),
            entry: self.entry,
            inputs: self.inputs.clone(),
            constants: self.constants.clone(),
        };
        let mut block_origin: Vec<BlockId> = (0..self.blocks.len()).collect();
        let mut operation_origin: Vec<OperationId> = (0..self.operations.len()).collect();
        let mut index: BTreeMap<(BlockId, Vec<BlockId>), BlockId> = (0..self.blocks.len())
            .map(|block| ((block, Vec::new()), block))
            .collect();
        let mut queue: VecDeque<(BlockId, Vec<BlockId>)> = (0..self.blocks.len())
            .map(|block| (block, Vec::new()))
            .collect();
        while let Some((original, stack)) = queue.pop_front() {
            let id = index[&(original, stack.clone())];
            let block = &self.blocks[original];
            let mut successors = Vec::new();
            for &successor in &block.successors {
                let key = if let Some(back) = block.call_return {
                    let mut inner = stack.clone();
                    if inner.len() < depth && !inner.contains(&back) {
                        inner.push(back);
                    }
                    (successor, inner)
                } else if block.returns.contains(&successor) {
                    match stack.last() {
                        Some(&top) if top == successor => {
                            (successor, stack[..stack.len() - 1].to_vec())
                        }
                        Some(top) if block.returns.contains(top) => continue,
                        _ => (successor, stack.clone()),
                    }
                } else if !block.returns.is_empty() {
                    // An explicit tag of a return leaves every call.
                    (successor, Vec::new())
                } else {
                    (successor, stack.clone())
                };
                let target = if let Some(&target) = index.get(&key) {
                    target
                } else {
                    let source = &self.blocks[key.0];
                    let mut copy = source.clone();
                    let mut renamed = BTreeMap::new();
                    copy.operations = source
                        .operations
                        .iter()
                        .map(|&operation| {
                            let new = graph.operations.len();
                            graph.operations.push(self.operations[operation].clone());
                            operation_origin.push(operation);
                            renamed.insert(operation, new);
                            new
                        })
                        .collect();
                    if let Some(timing) = &mut copy.timing {
                        timing.delays = std::mem::take(&mut timing.delays)
                            .into_iter()
                            .map(|((operation, register), delay)| {
                                ((renamed[&operation], register), delay)
                            })
                            .collect();
                    }
                    let target = graph.blocks.len();
                    if target >= budget {
                        return None;
                    }
                    graph.blocks.push(copy);
                    block_origin.push(key.0);
                    index.insert(key.clone(), target);
                    queue.push_back(key);
                    target
                };
                successors.push(target);
            }
            graph.blocks[id].successors = successors;
        }
        Some((graph, block_origin, operation_origin))
    }

    fn analyze_flat(&self) -> Result<Analysis, FlowError> {
        if self.entry >= self.blocks.len() {
            return Err(FlowError::InvalidEntry(self.entry));
        }
        let mut owners = BTreeSet::new();
        let mut predecessors = vec![Vec::new(); self.blocks.len()];
        for (id, block) in self.blocks.iter().enumerate() {
            for &operation in &block.operations {
                if operation >= self.operations.len() {
                    return Err(FlowError::InvalidOperation {
                        block: id,
                        operation,
                    });
                }
                if !owners.insert(operation) {
                    return Err(FlowError::ReusedOperation(operation));
                }
            }
            for &successor in &block.successors {
                if successor >= self.blocks.len() {
                    return Err(FlowError::InvalidSuccessor {
                        block: id,
                        successor,
                    });
                }
                predecessors[successor].push(id);
            }
        }
        let mut reachable = BTreeSet::new();
        let mut queue = VecDeque::from([self.entry]);
        while let Some(block) = queue.pop_front() {
            if reachable.insert(block) {
                queue.extend(&self.blocks[block].successors);
            }
        }

        let keys: BTreeSet<_> = self
            .operations
            .iter()
            .flat_map(|op| op.reads.iter().chain(&op.writes))
            .chain(&self.inputs)
            .chain(self.blocks.iter().flat_map(|block| &block.outputs))
            .copied()
            .flat_map(parts)
            .collect();
        let initialized: BTreeSet<_> = self.inputs.iter().copied().flat_map(parts).collect();
        let seed: State = keys
            .iter()
            .map(|&component| {
                let defined =
                    initialized.contains(&component) || self.constants.contains(&component.0);
                (
                    component,
                    BTreeSet::from([if defined {
                        Definition::Input
                    } else {
                        Definition::Uninitialized
                    }]),
                )
            })
            .collect();

        let mut incoming: Vec<FlowState> = vec![FlowState::default(); self.blocks.len()];
        let mut outgoing = incoming.clone();
        let entry_state = FlowState {
            defs: seed.clone(),
            pending: BTreeMap::new(),
        };
        loop {
            let mut changed = false;
            for &id in &reachable {
                let mut sources: Vec<&FlowState> = Vec::new();
                if id == self.entry {
                    sources.push(&entry_state);
                }
                for &pred in &predecessors[id] {
                    // Only predecessors already reached by the iteration.
                    if reachable.contains(&pred) && !outgoing[pred].defs.is_empty() {
                        sources.push(&outgoing[pred]);
                    }
                }
                let next = FlowState::join(&sources);
                // Wait until this block has at least one initialized predecessor.
                if next.defs.is_empty() && !seed.is_empty() {
                    continue;
                }
                let out = walk(self, &self.blocks[id], next.clone(), |_, _| {});
                if next != incoming[id] || out != outgoing[id] {
                    incoming[id] = next;
                    outgoing[id] = out;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut reads = Vec::new();
        let mut outputs = Vec::new();
        let mut undefined = Vec::new();
        let mut roots = BTreeSet::new();
        let mut operation_blocks = BTreeMap::new();
        let owner: BTreeMap<OperationId, BlockId> = self
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(block, b)| b.operations.iter().map(move |&op| (op, block)))
            .collect();
        let mut hazards = Vec::new();
        for &id in &reachable {
            let mut state = walk(
                self,
                &self.blocks[id],
                incoming[id].clone(),
                |operation, flow| {
                    let defs = &flow.defs;
                    operation_blocks.insert(operation, id);
                    let op = &self.operations[operation];
                    if op.side_effect {
                        roots.insert(operation);
                    }
                    for &access in &op.reads {
                        for (register, component) in parts(access) {
                            let definitions = defs
                                .get(&(register, component))
                                .cloned()
                                .unwrap_or_default();
                            reads.push(Read {
                                operation,
                                register,
                                component,
                                definitions,
                            });
                            // The original flags a read whose visible definition
                            // with timing differs from the one without: the
                            // latest write before it, still in flight from
                            // another block.
                            let latest = flow
                                .pending
                                .iter()
                                .filter(|((_, pending), _)| *pending == (register, component))
                                .max_by_key(|((time, _), _)| *time);
                            if let Some((_, (writers, _))) = latest {
                                for writer in writers {
                                    if let Definition::Operation(writer) = writer {
                                        let block = owner[writer];
                                        if block != id {
                                            hazards
                                                .push((operation, id, block, register, component));
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
            );
            state.land(None);
            let state = state.defs;
            for &access in &self.blocks[id].outputs {
                for (register, component) in parts(access) {
                    if let Some(definitions) = state.get(&(register, component)) {
                        outputs.push(Output {
                            block: id,
                            register,
                            component,
                            definitions: definitions.clone(),
                        });
                        for &definition in definitions {
                            match definition {
                                Definition::Operation(operation) => {
                                    roots.insert(operation);
                                }
                                Definition::Uninitialized => undefined.push(UndefinedRead {
                                    operation: None,
                                    block: id,
                                    register,
                                    component,
                                }),
                                Definition::Input => {}
                            }
                        }
                    }
                }
            }
        }
        let mut dependencies: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for read in &reads {
            for &definition in &read.definitions {
                if let Definition::Operation(writer) = definition {
                    dependencies
                        .entry(read.operation)
                        .or_default()
                        .insert(writer);
                }
            }
        }
        let mut live_operations = BTreeSet::new();
        let mut pending: Vec<_> = roots.into_iter().collect();
        while let Some(operation) = pending.pop() {
            if live_operations.insert(operation)
                && let Some(writers) = dependencies.get(&operation)
            {
                pending.extend(writers);
            }
        }
        // Unused instructions can be pruned without reporting undefined reads.
        for read in &reads {
            if live_operations.contains(&read.operation)
                && read.definitions.contains(&Definition::Uninitialized)
            {
                undefined.push(UndefinedRead {
                    operation: Some(read.operation),
                    block: operation_blocks[&read.operation],
                    register: read.register,
                    component: read.component,
                });
            }
        }
        let (live_in, live_out) = self.liveness(&reachable, &live_operations);
        let mut hazard_reads = BTreeSet::new();
        let mut hazard_writes = BTreeSet::new();
        for (operation, reader, writer, register, component) in hazards {
            if live_operations.contains(&operation) {
                hazard_reads.insert((reader, register, component));
                hazard_writes.insert((writer, register, component));
            }
        }
        let copy_live_out = (0..self.blocks.len())
            .map(|block| {
                if reachable.contains(&block) {
                    vec![live_out[block].clone()]
                } else {
                    Vec::new()
                }
            })
            .collect();
        Ok(Analysis {
            reachable,
            reads,
            outputs,
            live_operations,
            undefined,
            live_in,
            live_out,
            copy_live_out,
            hazard_reads,
            hazard_writes,
        })
    }

    fn liveness(
        &self,
        reachable: &BTreeSet<usize>,
        live: &BTreeSet<usize>,
    ) -> (Vec<BTreeSet<Access>>, Vec<BTreeSet<Access>>) {
        let mut ins: Vec<BTreeSet<Component>> = vec![BTreeSet::new(); self.blocks.len()];
        let mut outs = ins.clone();
        loop {
            let mut changed = false;
            for &id in reachable.iter().rev() {
                let block = &self.blocks[id];
                let mut next: BTreeSet<_> = block
                    .successors
                    .iter()
                    .flat_map(|&succ| ins[succ].iter().copied())
                    .collect();
                next.extend(block.outputs.iter().copied().flat_map(parts));
                let mut input = next.clone();
                for &operation in block.operations.iter().rev().filter(|id| live.contains(id)) {
                    let op = &self.operations[operation];
                    for &write in &op.writes {
                        if !self.constants.contains(&write.register) {
                            for part in parts(write) {
                                input.remove(&part);
                            }
                        }
                    }
                    input.extend(op.reads.iter().copied().flat_map(parts));
                }
                if outs[id] != next || ins[id] != input {
                    outs[id] = next;
                    ins[id] = input;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let combine = |states: Vec<BTreeSet<Component>>| {
            states
                .into_iter()
                .map(|state| {
                    let mut masks: BTreeMap<usize, u8> = BTreeMap::new();
                    for (register, part) in state {
                        *masks.entry(register).or_default() |= 1 << part;
                    }
                    masks
                        .into_iter()
                        .map(|(register, mask)| Access { register, mask })
                        .collect()
                })
                .collect()
        };
        (combine(ins), combine(outs))
    }
}

impl Analysis {
    /// Map a call-expanded analysis back onto the original blocks and
    /// operations, merging what the copies saw.
    fn fold(
        self,
        blocks: usize,
        block_origin: &[BlockId],
        operation_origin: &[OperationId],
    ) -> Analysis {
        let definition = |definition: &Definition| match definition {
            Definition::Operation(operation) => Definition::Operation(operation_origin[*operation]),
            other => *other,
        };
        // An operation reading one register twice has one read per operand;
        // only the copies of the same operand merge.
        let mut reads: Vec<Read> = Vec::new();
        let mut read_index: BTreeMap<(OperationId, RegisterId, u8, usize), usize> = BTreeMap::new();
        let mut occurrences: BTreeMap<(OperationId, RegisterId, u8), usize> = BTreeMap::new();
        for read in self.reads {
            let operation = operation_origin[read.operation];
            let definitions: BTreeSet<_> = read.definitions.iter().map(definition).collect();
            let occurrence = occurrences
                .entry((read.operation, read.register, read.component))
                .or_default();
            let key = (operation, read.register, read.component, *occurrence);
            *occurrence += 1;
            if let Some(&at) = read_index.get(&key) {
                reads[at].definitions.extend(definitions);
            } else {
                read_index.insert(key, reads.len());
                reads.push(Read {
                    operation,
                    register: read.register,
                    component: read.component,
                    definitions,
                });
            }
        }
        let mut outputs: Vec<Output> = Vec::new();
        let mut output_index: BTreeMap<(BlockId, RegisterId, u8), usize> = BTreeMap::new();
        for output in self.outputs {
            let block = block_origin[output.block];
            let definitions: BTreeSet<_> = output.definitions.iter().map(definition).collect();
            let key = (block, output.register, output.component);
            if let Some(&at) = output_index.get(&key) {
                outputs[at].definitions.extend(definitions);
            } else {
                output_index.insert(key, outputs.len());
                outputs.push(Output {
                    block,
                    register: output.register,
                    component: output.component,
                    definitions,
                });
            }
        }
        let mut undefined: Vec<UndefinedRead> = Vec::new();
        for read in self.undefined {
            let read = UndefinedRead {
                operation: read.operation.map(|operation| operation_origin[operation]),
                block: block_origin[read.block],
                register: read.register,
                component: read.component,
            };
            if !undefined.contains(&read) {
                undefined.push(read);
            }
        }
        let merge = |states: Vec<BTreeSet<Access>>| {
            let mut masks: Vec<BTreeMap<RegisterId, u8>> = vec![BTreeMap::new(); blocks];
            for (block, state) in states.into_iter().enumerate() {
                for access in state {
                    *masks[block_origin[block]]
                        .entry(access.register)
                        .or_default() |= access.mask;
                }
            }
            masks
                .into_iter()
                .map(|masks| {
                    masks
                        .into_iter()
                        .map(|(register, mask)| Access { register, mask })
                        .collect()
                })
                .collect()
        };
        let mut copy_live_out = vec![Vec::new(); blocks];
        for &block in &self.reachable {
            copy_live_out[block_origin[block]].push(self.live_out[block].clone());
        }
        let hazards = |set: BTreeSet<(BlockId, RegisterId, u8)>| {
            set.into_iter()
                .map(|(block, register, part)| (block_origin[block], register, part))
                .collect()
        };
        Analysis {
            reachable: self
                .reachable
                .iter()
                .map(|&block| block_origin[block])
                .collect(),
            reads,
            outputs,
            live_operations: self
                .live_operations
                .iter()
                .map(|&operation| operation_origin[operation])
                .collect(),
            undefined,
            live_in: merge(self.live_in),
            live_out: merge(self.live_out),
            copy_live_out,
            hazard_reads: hazards(self.hazard_reads),
            hazard_writes: hazards(self.hazard_writes),
        }
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn access(register: usize, mask: u8) -> Access {
        Access { register, mask }
    }

    fn op(reads: &[(usize, u8)], writes: &[(usize, u8)]) -> Operation {
        Operation {
            reads: reads.iter().map(|&(r, m)| access(r, m)).collect(),
            writes: writes.iter().map(|&(r, m)| access(r, m)).collect(),
            side_effect: false,
        }
    }

    fn graph(operations: Vec<Operation>, blocks: Vec<Block>, inputs: Vec<Access>) -> Graph {
        Graph {
            operations,
            blocks,
            entry: 0,
            inputs,
            constants: BTreeSet::new(),
        }
    }

    #[test]
    fn partial_write_does_not_initialize_other_components() {
        let graph = graph(
            vec![op(&[], &[(1, 1)])],
            vec![Block {
                operations: vec![0],
                outputs: vec![access(1, 15)],
                ..Block::default()
            }],
            vec![],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(
            result
                .undefined
                .iter()
                .map(|read| read.component)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    /// Two callers share a subroutine whose return goes back to each one;
    /// each continuation sees only its own caller's definitions.
    #[test]
    fn calls_return_only_to_their_own_caller() {
        // 0: branch; 1: caller A writes r1; 2: continuation A reads r1, r3;
        // 3: caller B writes r2; 4: continuation B reads r2, r3;
        // 5: subroutine writes r3 and returns.
        let used = |reads: &[(usize, u8)]| Operation {
            side_effect: true,
            ..op(reads, &[])
        };
        let operations = vec![
            op(&[], &[]),
            op(&[], &[(1, 1)]),
            used(&[(1, 1), (3, 1)]),
            op(&[], &[(2, 1)]),
            used(&[(2, 1), (3, 1)]),
            op(&[], &[(3, 1)]),
        ];
        let block = |operation: usize, successors: Vec<usize>| Block {
            operations: vec![operation],
            successors,
            ..Block::default()
        };
        let mut blocks = vec![
            block(0, vec![1, 3]),
            block(1, vec![5]),
            block(2, vec![]),
            block(3, vec![5]),
            block(4, vec![]),
            block(5, vec![2, 4]),
        ];
        blocks[1].call_return = Some(2);
        blocks[3].call_return = Some(4);
        blocks[5].returns = vec![2, 4];
        let result = graph(operations, blocks, vec![]).analyze().unwrap();
        assert!(result.undefined.is_empty(), "{:?}", result.undefined);
        let defined = |operation: usize, register: usize| {
            result
                .reads
                .iter()
                .find(|read| read.operation == operation && read.register == register)
                .unwrap()
                .definitions
                .clone()
        };
        assert_eq!(defined(2, 1), BTreeSet::from([Definition::Operation(1)]));
        assert_eq!(defined(4, 2), BTreeSet::from([Definition::Operation(3)]));
        assert_eq!(defined(4, 3), BTreeSet::from([Definition::Operation(5)]));
        // The subroutine is live for both callers: r1 and r2 pass through it.
        assert_eq!(result.copy_live_out[5].len(), 2);
        assert_eq!(
            result.live_out[5],
            BTreeSet::from([access(1, 1), access(2, 1), access(3, 1)])
        );
    }

    #[test]
    fn reads_are_resolved_before_same_instruction_writes() {
        let graph = graph(
            vec![op(&[(1, 1)], &[(1, 1)])],
            vec![Block {
                operations: vec![0],
                outputs: vec![access(1, 1)],
                ..Block::default()
            }],
            vec![access(1, 1)],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(
            result.reads[0].definitions,
            BTreeSet::from([Definition::Input])
        );
        assert!(result.undefined.is_empty());
    }

    #[test]
    fn branch_join_retains_both_reaching_definitions() {
        let graph = graph(
            vec![
                op(&[], &[(1, 1)]),
                op(&[], &[(1, 1)]),
                op(&[(1, 1)], &[(2, 1)]),
            ],
            vec![
                Block {
                    successors: vec![1, 2],
                    ..Block::default()
                },
                Block {
                    operations: vec![0],
                    successors: vec![3],
                    ..Block::default()
                },
                Block {
                    operations: vec![1],
                    successors: vec![3],
                    ..Block::default()
                },
                Block {
                    operations: vec![2],
                    outputs: vec![access(2, 1)],
                    ..Block::default()
                },
            ],
            vec![],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(
            result.reads[0].definitions,
            BTreeSet::from([Definition::Operation(0), Definition::Operation(1)])
        );
        assert_eq!(result.live_operations, BTreeSet::from([0, 1, 2]));
        assert!(result.undefined.is_empty());
    }

    #[test]
    fn missing_definition_on_one_branch_is_reported() {
        let graph = graph(
            vec![op(&[], &[(1, 1)]), op(&[(1, 1)], &[(2, 1)])],
            vec![
                Block {
                    successors: vec![1, 2],
                    ..Block::default()
                },
                Block {
                    operations: vec![0],
                    successors: vec![3],
                    ..Block::default()
                },
                Block {
                    successors: vec![3],
                    ..Block::default()
                },
                Block {
                    operations: vec![1],
                    outputs: vec![access(2, 1)],
                    ..Block::default()
                },
            ],
            vec![],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(result.undefined.len(), 1);
        assert_eq!(result.undefined[0].operation, Some(1));
    }

    #[test]
    fn loop_carried_definition_reaches_next_iteration() {
        let graph = graph(
            vec![op(&[(1, 1)], &[(1, 1)])],
            vec![
                Block {
                    successors: vec![1],
                    ..Block::default()
                },
                Block {
                    operations: vec![0],
                    successors: vec![1, 2],
                    ..Block::default()
                },
                Block {
                    outputs: vec![access(1, 1)],
                    ..Block::default()
                },
            ],
            vec![access(1, 1)],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(
            result.reads[0].definitions,
            BTreeSet::from([Definition::Input, Definition::Operation(0)])
        );
        assert!(result.undefined.is_empty());
        assert!(result.live_in[1].contains(&access(1, 1)));
    }

    #[test]
    fn unreachable_blocks_do_not_contribute_errors_or_liveness() {
        let graph = graph(
            vec![Operation {
                side_effect: true,
                ..op(&[(99, 15)], &[])
            }],
            vec![
                Block::default(),
                Block {
                    operations: vec![0],
                    ..Block::default()
                },
            ],
            vec![],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(result.reachable, BTreeSet::from([0]));
        assert!(result.undefined.is_empty());
        assert!(result.live_operations.is_empty());
    }

    #[test]
    fn dead_chain_is_pruned_but_side_effect_dependencies_survive() {
        let graph = graph(
            vec![
                op(&[(9, 1)], &[(1, 1)]),
                op(&[(1, 1)], &[(2, 1)]),
                op(&[(3, 1)], &[(4, 1)]),
                Operation {
                    side_effect: true,
                    ..op(&[(4, 1)], &[])
                },
            ],
            vec![Block {
                operations: vec![0, 1, 2, 3],
                ..Block::default()
            }],
            vec![access(3, 1)],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(result.live_operations, BTreeSet::from([2, 3]));
        assert!(result.undefined.is_empty());
    }

    #[test]
    fn zero_register_writes_do_not_create_definitions() {
        let mut graph = graph(
            vec![op(&[], &[(0, 15)]), op(&[(0, 15)], &[(1, 15)])],
            vec![Block {
                operations: vec![0, 1],
                outputs: vec![access(1, 15)],
                ..Block::default()
            }],
            vec![],
        );
        graph.constants.insert(0);
        let result = graph.analyze().unwrap();
        assert_eq!(result.live_operations, BTreeSet::from([1]));
        assert!(
            result
                .reads
                .iter()
                .all(|read| read.definitions == BTreeSet::from([Definition::Input]))
        );
    }

    #[test]
    fn rejects_malformed_graphs() {
        assert!(graph(vec![], vec![], vec![]).analyze().is_err());
        assert!(
            graph(
                vec![],
                vec![Block {
                    successors: vec![2],
                    ..Block::default()
                }],
                vec![]
            )
            .analyze()
            .is_err()
        );
        assert!(
            graph(
                vec![],
                vec![Block {
                    operations: vec![0],
                    ..Block::default()
                }],
                vec![]
            )
            .analyze()
            .is_err()
        );
        assert!(
            graph(
                vec![op(&[], &[])],
                vec![Block {
                    operations: vec![0, 0],
                    ..Block::default()
                }],
                vec![]
            )
            .analyze()
            .is_err()
        );
    }

    #[test]
    fn delayed_write_lands_in_the_next_fixed_table() {
        // Block 0 (2 rows): op 0 writes Q (register 1) at row 0 with delay 3;
        // op 1 writes Q at row 1 with delay 3. Block 1 (2 rows): op 2 reads Q
        // at row 0 (cycle 2 after op 0, before it lands), op 3 at row 1.
        let mut first = Timing {
            issue: vec![0, 1],
            length: 2,
            delays: BTreeMap::new(),
        };
        first.delays.insert((0, 1), 3);
        first.delays.insert((1, 1), 3);
        let graph = graph(
            vec![
                op(&[], &[(1, 1)]),
                op(&[], &[(1, 1)]),
                op(&[(1, 1)], &[]),
                op(&[(1, 1)], &[]),
            ],
            vec![
                Block {
                    operations: vec![0, 1],
                    successors: vec![1],
                    timing: Some(first),
                    ..Block::default()
                },
                Block {
                    operations: vec![2, 3],
                    timing: Some(Timing {
                        issue: vec![0, 1],
                        length: 2,
                        delays: BTreeMap::new(),
                    }),
                    ..Block::default()
                },
            ],
            vec![access(1, 1)],
        );
        let result = graph.analyze().unwrap();
        let seen = |operation| {
            result
                .reads
                .iter()
                .find(|read| read.operation == operation)
                .unwrap()
                .definitions
                .clone()
        };
        assert_eq!(seen(2), BTreeSet::from([Definition::Input]));
        assert_eq!(seen(3), BTreeSet::from([Definition::Operation(0)]));
        // Neither read is live, so neither is a hazard.
        assert!(result.hazard_reads.is_empty());
    }

    #[test]
    fn live_read_before_an_incoming_write_lands_is_a_hazard() {
        // Block 0 writes Q (register 1) at its last row with delay 3; block
        // 1 reads it at row 0 (cycle 1 after issue) and keeps the result.
        let mut first = Timing {
            issue: vec![0],
            length: 1,
            delays: BTreeMap::new(),
        };
        first.delays.insert((0, 1), 3);
        let mut read = op(&[(1, 1)], &[]);
        read.side_effect = true;
        let graph = graph(
            vec![op(&[], &[(1, 1)]), read],
            vec![
                Block {
                    operations: vec![0],
                    successors: vec![1],
                    timing: Some(first),
                    ..Block::default()
                },
                Block {
                    operations: vec![1],
                    timing: Some(Timing {
                        issue: vec![0],
                        length: 1,
                        delays: BTreeMap::new(),
                    }),
                    ..Block::default()
                },
            ],
            vec![access(1, 1)],
        );
        let result = graph.analyze().unwrap();
        assert_eq!(result.hazard_reads, BTreeSet::from([(1, 1, 0)]));
        assert_eq!(result.hazard_writes, BTreeSet::from([(0, 1, 0)]));
    }
}
