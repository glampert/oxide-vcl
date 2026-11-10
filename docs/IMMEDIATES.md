# VI induction and immediate displacement repair

Findings are observed from vcl 1.4beta7 or inferred from it. The numeric
expression language is covered by [LANGUAGE.md](LANGUAGE.md).

## Purpose

The scheduler can move a memory operation across an update of its VI base
register. For a constant-stride induction this need not change the accessed
address: changing the immediate displacement compensates for the different VI
version seen at issue. For example, this source order:

```asm
iaddiu p,p,3
lq c,5(p)
```

can become an earlier `lq c,8(p)` followed by the original `iaddiu p,p,3`.
vcl 1.4beta7 does this in probe `immediate-scheduled-after-add`. Replacing the
update with `isubiu p,p,3` produces displacement 2 instead. The VI update's
immediate stays 3.

This mechanism does not generally rewrite IADDIU constants or branch target
labels. Its eligible destination instructions are only four direct-displacement
memory instructions.

A separate general coalescer can change VI update constants after expansion;
see [INDUCTION.md](INDUCTION.md).

## Instruction tables

Inferred from vcl 1.4beta7, the repair uses a source table of induction
updates and a destination table of repairable memory instructions. Instruction
IDs are descriptor indices.

| Instruction ID | Instruction | Operation | Input operand | Output operand | Stride |
| --- | --- | --- | --- | --- | --- |
|109|iaddi|add|1|0|operand 2|
|110|iaddiu|add|1|0|operand 2|
|112|isubiu|subtract|1|0|operand 2|
|158|lqi|add|2|1|constant 1|
|163|sqi|add|2|1|constant 1|
|159|lqd|subtract|2|1|constant 1|
|164|sqd|subtract|2|1|constant 1|

Operand numbering includes implicit operands. For LQI/SQI/LQD/SQD the explicit
address operand is a write, and the implicit operand 2 reads the old address
register. Operation code 1 is add and 2 is subtract. A negative stride-operand
index selects a constant stride; the four autoincrement/decrement entries use
the constant 1.

The destination table matches LQ (155), SQ (160), ILW (156) and ISW (161). In
every case the repairable memory operand is operand 1.

`local/tools/extract_immediate_tables.py` reproduces the tables from the
original executable; raw bytes are kept in
`local/evidence/tables/immediate_fixups.json` (local, not tracked).

## Initialization and annotation

The affine pass has two modes. Initialization clears each stream's
generated-loop identity, clears instruction affine metadata, sets the operand
index to -1, invalidates the stored stride expression, and sets the cloned
iteration bias to -1. It then annotates eligible users on every stream.

For one register reference, annotation requires:

- Scalar bit 0 is present in both external-input and external-output masks.
- The stream reference contains exactly one scalar setter.
- That setter is in the source table.
- Its indexed input and output operands refer to the same register object.

It walks users of the pre-update visible external value and users linked to the
update's output. Only direct-displacement memory consumers from the destination
table qualify, and their selected base operand must have scalar mask 1. The two
sets get different phases:

| Phase | Meaning before transformation |
| --- | --- |
| Before | Reads the external/pre-update induction value |
| After | Reads this iteration's update result |

Each consumer stores the source operation code, phase, selected operand index,
and a copied stride expression. Annotating an already annotated instruction is
an error; the pass does not combine arbitrary inductions.

While queueing instructions, an annotated base operand is treated as a
special read that carries no ordinary pointer-version constraint. This lets the
memory use cross the VI update. It is safe because later rebuilt flow and
displacement repair restore the original effective-address interpretation.

## Expansion metadata and reaching-setter epochs

Cloning in the pipeline sets a clone's bias to
`window_max_iteration - original_scheduled_stage`. It does not immediately
change the memory offset. After expanded streams are linked and component flow
has been rebuilt, the repair step resolves which induction update the memory
base actually sees.

The resolver uses each reaching setter's generated-loop identity, construct
kind, and cloned bias. The construct-kind values are:

|Value|Construct|
|---|---|
|0|Outside generated loop|
|1|Generated exit|
|2|Prologue|
|3|Epilogue|
|4|Generated entry|
|5|Kernel|

An empty reaching-setter list is an error. If any reaching setter belongs to a
different generated-loop identity, the resolver returns external epoch -1
before counting local setters. Otherwise it accepts at most two local setters.

With one setter:

| Consumer | Permitted setter | Actual epoch |
| --- | --- | --- |
|Prologue|one local setter|setter bias|
|Kernel|Prologue or Kernel|setter bias|
|Epilogue|Prologue or Epilogue|setter bias|
|Epilogue|Kernel|setter bias - 1|

With two setters, they must have different construct kinds. A prologue consumer
rejects this ambiguity. Kernel or epilogue consumers accept only one prologue
and one kernel setter, normalize the kernel bias by minus one, and require the
two resulting epochs to agree. Same-kind pairs, more setters, or mismatched
epochs are errors.

The one-setter and two-setter kernel rules differ on purpose; an unconditional
minus-one rule for every kernel setter would disagree with vcl 1.4beta7.

## Correction arithmetic

For expanded code, the expected epoch starts with the consumer clone's
nonnegative iteration bias. For unexpanded code the bias must retain the -1
sentinel and the expected epoch starts at zero. The Before phase subtracts 1;
After does not.

For unexpanded code, the actual epoch is -1 if the selected operand still has
external bits, otherwise 0. Expanded code uses the resolver above. The numeric
correction is:

```rust
let expected = clone_bias - if phase == Before { 1 } else { 0 };
let difference = expected - actual_setter_epoch;
let factor = match update_operator {
    Add => difference,
    Subtract => -difference,
};
let repaired = old_offset.wrapping_add(stride.wrapping_mul(factor));
```

If both stored expressions are numeric, they are converted to signed 32-bit
integers and the wrapping signed result is stored. If either expression is
unresolved, the complete source expressions are preserved in this generated
shape:

```text
(old_offset)+((stride)*factor)
```

A zero difference leaves the immediate unchanged. The arithmetic does not check
the final instruction encoding's immediate width. The repair returns 0 for a
numeric repair, 2 for a symbolic repair, and -1 on internal failure. The source
table contains no operation code 3; vcl 1.4beta7 reports that mode as not
implemented.

The non-initialization mode applies repairs and then clears the annotations,
stride expression, clone bias, and generated-loop identities. Applying the same
nonzero correction twice without resetting metadata is not idempotent.

## Observations from vcl 1.4beta7

Probe sources are under `local/evidence/inputs/`; raw outputs and run metadata
are under `local/evidence/oracle/` (both local, not tracked).

| Probe | Source order / update | Observed memory displacement |
| --- | --- | --- |
|`immediate-scheduled-add`|LQ 5(p), then p+=3|5|
|`immediate-scheduled-after-add`|p+=3, then LQ 5(p)|8 after scheduling LQ before the update|
|`immediate-scheduled-after-subtract`|p-=3, then LQ 5(p)|2 after scheduling LQ before the update|
|`immediate-pipeline-before-add`|Explicit raw-loop stages and p+=3|5|
|`immediate-pipeline-before-subtract`|Explicit raw-loop stages and p-=3|5|

The three scheduled probes use `--LoopAbs 5` and select a nonzero-stage loop.
The scheduler's choice depends on its CPU-alarm time bound, so these are
observations under the recorded timer settings, not a claim of time-independent
strategy selection.

Explicit raw loops are expanded before the affine initialization pass, so those
first raw clones do not receive affine annotations and their offsets remain 5.
The raw-loop sources need a loop-ID-bearing upper `ABS VF00` operation (later
pruned) before the first loop-ID-bearing lower instruction; a plain upper NOP
is rejected by the raw builder.

`local/tools/probe_immediates.py` reproduces the probes.
`crates/vcl/src/loops/immediate.rs` implements table access, eligibility
checks, before/after annotations, source-epoch resolution, correction planning,
and application; its tests include the observed 5 to 8 and 5 to 2 corrections
and unchanged induction constants.

## Integration requirements

The caller must retain the phase annotation while scheduling, materialize
expanded clone biases and construct kinds, rebuild actual component reaching
setters, and apply each correction once. Declared input values can be
represented by `SetterLocation::external()`.

This pass changes a displacement expression only. VI register remapping belongs
to the register-web/allocation stages and branch target rewriting to
pipeline/control-flow construction. Dependency analysis must be rerun after
structural edits. Arbitrary multi-setter arithmetic, multiple inductions on one
annotated consumer, and unsupported affine operation code 3 are not handled.
