# Affine immediate repair

`loop_ir::annotate_sources`, `ExpandedLoop::reaching_setters` and
`ExpandedLoop::repair_from_flow` connect ordinary component flow to the affine
displacement primitives. They consume typed instructions and explicit
operand-to-flow identity mappings. They do not choose modulo placements or
infer register versions from iteration biases.

The behavior authority is the supplied ELF (vcl 1.4beta7), SHA-256
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`. vcl does
the following (inferred from vcl 1.4beta7):

- Recognizes a unique scalar induction and annotates before/after memory users.
- Looks up an update among the affine update forms and a displacement consumer
  among the consumer forms.
- Clones operands and affine annotations, assigning clone bias as window
  maximum minus source stage.
- Resolves the actual source epoch from rebuilt reaching setters, compares
  expected and actual epochs, selects a signed correction and applies the
  immediate multiply/add.

## Source annotation

The annotation pass requires the scalar register to cross the stream's input
and output boundaries, have one scalar setter in that stream, and be both the
input and output register of that setter. It recognizes IADDI, IADDIU, ISUBIU,
LQI/SQI and LQD/SQD. Only LQ, SQ, ILW and ISW displacement operands are
consumers. Branch targets and arbitrary immediate operands are not repaired.

The adapter derives boundary masks from `Analysis::live_in/live_out`, counts
actual scalar setters in the block's flow operations, and checks the update's
input/output logical register identity. These must be the source register
identities used before web allocation splits individual versions.

Before/after classification uses **stream-local** reaching definitions. The
adapter analyzes the same block operations in isolation, with the stream's
live-ins as external seeds and its live-outs as required outputs. An address
read reached by the external seed is `Phase::Before`; one reached by the sole
local update is `Phase::After`.

This matters for a loop: whole-CFG reaching definitions for a read before its
update can contain both the declared initial input and the update from the
previous iteration, which does not make it an after-update read in the source
stream. The local distinction corresponds to the register reference's
visible-input list versus the local setter's user list; the isolated-flow
formulation is a reconstruction of that distinction, not a claim about vcl's
graph algorithm.

```rust,ignore
let annotated = loop_ir::annotate_sources(
    &source_operations,
    &source_graph,
    &source_analysis,
    source_block,
    &source_operand_flow,
)?;
let expanded = loop_ir::expand(&modulo_schedule, &annotated.sources, loop_id)?;
```

`OperandFlow` separates identities that must not be conflated:

```rust
# use vcl::loop_ir::OperandFlow;
let operand = OperandFlow {
    instruction: 42, // source operation ID, later a dense clone ID
    operand: 1,      // includes implicit operands
    operation: 17,   // operation ID in the actual flow graph
    register: 9,     // logical register/component-flow identity
};
```

Two raw issue slots may share a single flow operation to preserve paired reads
before either slot's writes; the bridge does not substitute textual
instruction order for that grouping. Duplicate operand mappings, missing
update or consumer mappings, and duplicate annotations are errors. The caller
passes sources belonging to the selected source block and an analysis from the
same graph, and performs ordinary undefined-input checks before annotation.

## Expanded reaching definitions

After expansion, the actual component-flow graph is created for all generated
parts, including prologue early exits, kernel backedges and drains. Each
cloned operand is bound to its flow operation and logical register using
`OperandFlow`. The bridge translates each annotated scalar read's actual
definitions into:

* `Setter::External` for a declared initial input or a producer outside this
  generated-loop group;
* `Setter::Operation { clone, operand }` for the exact cloned writing operand.

All cloned register outputs require mappings, including outputs that are not
induction variables, so a forgotten local writer is never silently treated as
external. Multiple cloned writing operands for the same defining flow
operation/register are ambiguous and rejected. Undefined addresses fail
explicitly. The helper never picks a writer merely because it has the
consumer's iteration bias.

```rust,ignore
let repaired = expanded.repair_from_flow(
    &clone_register_bindings,
    &expanded_analysis,
    &clone_operand_flow,
)?;
```

`repair_from_flow` preserves the input adapter and original source
instructions. The returned copy contains repaired typed immediates and records
the expected epoch, actual epoch and signed correction factor for each fixup.
Symbolic offsets and strides keep their complete original expressions.

## Epoch arithmetic and original observations

For a consumer with clone bias `b`, the expected epoch is `b - 1` for a
before-update read and `b` for an after-update read. The source-epoch helper
handles actual setter locations. An external definition gives epoch -1. A
kernel/prologue join compares the prologue setter's bias to the kernel
setter's bias minus one; they must agree. A single local kernel setter is
different and is not decremented. Epilogue consumers also adjust kernel
setters as the source-epoch resolver does.

For addition the correction is `stride * (expected - actual)`; subtraction
reverses the sign. Arithmetic with both operands numeric wraps at 32 bits.

The source `immediate-scheduled-after-add.vcl` contains:

```text
iaddiu p,p,3
lq c,5(p)
```

The original schedules the load before the update in the expanded prologue and
kernel, emitting `8(VI02)` twice. In the prologue the after-update load has
bias 0, expected epoch 0 and actual external epoch -1. In the kernel it has
bias 1; the actual prologue/kernel setter biases are 0 and 1, which normalize
to epoch 0. Both loads therefore get one extra stride: `5 + 3 = 8`. The
corresponding subtract probe emits `2(VI02)`. Unit tests build the source
annotation and the full expanded-CFG reaching definitions and check these
corrections.

The raw-stage min1/min4 observations show that clone bias is not a complete
register-version identity. In the kernel the stage-1 MUL has bias 1, but its
VF03 read reaches the preceding stage-0 MUL with bias 2 in actual row order.
The helper's tests verify that actual CFG relation for both known-minimum
settings.

With known minimum four there are no warmup early-exit edges, and the original
removes warmup MULs whose values the kernel overwrites. With minimum one,
early drain paths keep warmup definitions. These differences come from actual
CFG liveness and later allocation, not from window membership, and the affine
bridge does not infer them. The original also coalesces the min4 warmup VI
decrements from 1/1 to row-preserving 0/2; that arithmetic collapse is
separate from displacement repair (see [INDUCTION.md](INDUCTION.md)).

## Compiler integration order and limits

1. Complete ordinary source component flow and pruning.
2. Annotate source memory consumers before scheduling moves updates across
   them.
3. Obtain a legal allocated modulo schedule, then expand using annotated
   sources.
4. Splice generated parts and all edges into the real CFG; preserve clone IDs.
5. Rebuild component flow using actual paired-read/delay-slot semantics and any
   required web moves. Supply the resulting operand and setter identities.
6. Repair affine immediates, then rebuild effects where needed and continue
   allocation, arithmetic collapse, pruning, timing checks and final emission.

The bridge does not create copy/phi moves, prove side-effect eligibility,
select modulo periods or reproduce final register allocation. Whole-program
flow can contain outside producers; all such producers have external epoch -1
for the generated loop. A source or clone mapping from an older graph is
invalid input. If a caller coalesces setters or removes clones, it must
rebuild the flow mappings before repairing again from the unmodified offsets:
repairing already repaired offsets applies the correction twice.

The tests cover the typed bridge and the selected original observations; they
do not establish whole-program pipeline equivalence.

## Compiler boundary

`compile::normal_loop_plan` annotates eligible normal sources before modulo
placement and relaxes only their proven address dependencies. Generated parts
are spliced into the actual region, pair ownership and cloned writer mappings
are rebuilt, and `repair_from_flow` updates typed offsets before allocation.
Three normal affine execution cases cover both update directions and source
before/after reads over 64 states each; their selected kernels reproduce the
original five-row placement.

vcl expands explicit raw stages before it initializes affine annotations, so
the compiler does not annotate raw source instructions before cloning; doing
so changes the authored drain offsets. The raw-flow bridge tests check
identity/epoch handling and do not imply that vcl runs this analysis at the raw
boundary. See [PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md) for the
pass-order evidence and [LINEAR_CLEANUP.md](LINEAR_CLEANUP.md) for the
prologue/drain pair differences; arbitrary program equivalence is not
established.
