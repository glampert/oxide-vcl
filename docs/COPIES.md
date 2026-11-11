# General immediate coalescing and copy normalization

General numeric coalescing and the copy recognizer take part in ordinary
compilation, not only generated-loop cleanup. Behaviour described for vcl is
inferred from vcl 1.4beta7 by static analysis and checked with independently
written inputs and recorded vcl output/status.

## Cleanup boundaries in vcl

vcl's cleanup driver invokes scalar collapsing, detects moves, rebuilds flow,
invokes a pruning hook, and rebuilds again. The pruning hook is a stub that
returns success without changing the program. Copy detection creates internal
instructions that stay eligible for ordinary pruning and allocation. The stub does
not justify deleting every self-copy, removing a raw issue site, or substituting
arbitrary algebraic rules.

The scalar pass visits every stream, whether or not it came from loop expansion.
Move detection has a different gate: UTA tables are eligible, as are VSM tables
with movable submode 1 or 3. Fixed raw modes keep their encodings. So a scheduled
or reorderable raw `ISUBIU ...,0` becomes an internal integer copy, while a fixed
raw one remains ISUBIU.

The port validates all original source reads first, then prunes and runs general
scalar cleanup before normal loop generation. It repeats numeric cleanup and copy
detection after expansion and pruning. Explicit raw expansion precedes strict flow
as described in [PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md). Fixed generated
kernels and explicitly fixed raw layouts are protected.

## Coalescing uses complete users

The arithmetic and range rules in [INDUCTION.md](INDUCTION.md) apply unchanged. A
producer's nonzero unsigned 15-bit delta can be pushed into all compatible
IADDIU/ISUBIU or LQ/SQ/ILW/ISW users. Every user must read the correct operand and
have that exact producer as its only reaching definition. Any branch, escaping
output, unsupported operation, symbolic immediate, incompatible join or
out-of-range result blocks the whole producer. Approved users change atomically
and the producer's immediate becomes zero.

The port indexes actual operand users across blocks, including paired read owners.
A block boundary is not itself an escaping observation. Explicit EXIT bindings are
observations, and a consumer with multiple definitions is a barrier even when each
candidate delta would fit numerically.

```text
pointer = base + 3
result  = memory[pointer + 5]
```

With no observation of `pointer`, the source becomes a zero-delta copy followed by
a load at offset 8. If `pointer` is also declared as an output, both the increment
and load offset 5 remain. Two independent loads must both accept the new offsets
before the producer changes. With two possible pointer definitions at a join, the
offset stays 5 and neither producer is zeroed.

The fixed-point scan follows physical block/instruction order. Numeric rewrites do
not change register operands, masks or latencies, so the adapter caches
reaching-definition identities and builds fresh opcode/immediate views as constants
propagate. Structural copy conversion receives a fresh analysis. A deterministic
transformation budget bounds the scan; budget exhaustion is not treated as proof of
vcl rejection.

## Copy-recognition rules

The zero input means the actual pool-zero register, not an arbitrary variable whose
value happens to be zero. Destination and source belong to the same register family
and carry the same effective nonempty component mask.

| Instruction | Required identity | Additional condition | Internal kind |
| --- | --- | --- | --- |
| IADD, ISUB | third operand is VI00 | source is operand 1 | IMOVE |
| IADDI, IADDIU, ISUBIU | numeric immediate is zero | unresolved expressions stay unchanged | IMOVE |
| MOVE | explicit vector transfer | effective mask retained | VMOVE |
| ADD, SUB | third operand is VF00 | W absent from destination; secondary outputs unobserved | VMOVE |
| ADDX, ADDY, ADDZ | broadcast source is VF00 | secondary outputs unobserved | VMOVE |
| MULW | broadcast source is VF00.w | secondary outputs unobserved | VMOVE |

VF00.w is one, so full-field ADD/SUB with VF00 is not an identity. Broadcast
VF00.x/y/z is zero even when the destination includes W. MULW can use the constant
one, whereas MULX with VF00.x computes zero and remains arithmetic. Other commuted
identities or broadcast opcodes are outside this rule.

A separate check examines output observations beyond the ordinary destination.
MAC/status use preserves arithmetic even when vector values alone would look like a
copy. In `copy-flags-live` the ADD and an ABS interlock cap before FMAND are
retained. The bounded numerical model does not implement flags, so this case is
checked against vcl pairs.

The move constructor starts a fresh instruction, so a source D modifier disappears
on recognized copy conversion (`copy-iaddiu-debug`); the port does the same. Fixed
raw source is outside the conversion gate and keeps its ordinary encoding.

## Internal state and physical encoding

Parsed `Instruction.generated_copy` is always None. Only a proven conversion sets
its kind and replaces the descriptor/operands with internal IMOVE (178) or VMOVE
(180). This state survives typed cloning. Source-written `imove`, `limove`,
`vmove`, `lvmove` and `uvmove` keep vcl's missing-base-state emission failure.

vcl resolves the output kind after pipe selection, then emits:

| Internal output descriptor | Pipe | Assembly |
| --- | --- | --- |
| 179, LIMOVE | lower | `iaddiu destination,source,0` |
| 181, LVMOVE | lower | `move destination,source` |
| 182, UVMOVE | upper | `max destination,source,source` |

The integer hook writes literal decimal zero, unlike the hexadecimal formatting of
an ordinary source IADDIU immediate. Upper MAX has no ADD/MUL flag output. Actual
component masks and end modifiers pass through the ordinary renderer.

```rust,ignore
let kind = copies::detect(facts)?;
let output_descriptor = kind.resolved_descriptor(selected_pipe);
// Physical replay uses this output descriptor's unit and throughput.
// Emission uses the proven internal marker and the resolved output kind.
```

Abstract copies initially have unit mask zero. After a pipe is selected, physical
replay uses their resolved IALU, LS or FMAC metadata. In the extracted 214-record
table no lower-pipe instruction shares the FMAC bit and no upper-pipe instruction
shares IALU or LS, so throughput-one copy occupancy is already constrained by its
pipe. The standalone search still differs from vcl's dynamic lock/move machinery.

## Operand formatting survives normalization

An operand's original conversion selector survives opcode changes in vcl. Numeric
IADDI becomes IADDIU/ISUBIU but its immediate keeps the original signed selector;
looking up the replacement descriptor's selector would change formatting and
possibly conversion behaviour.

The typed operand retains `immediate_kind` from parsing and numeric rewrites
preserve it. Generated integer-copy emission supplies the literal-zero hook form
separately. A regression changes an IADDI instruction's metadata to IADDIU and
verifies that its operand still uses selector 37 rather than the replacement
descriptor's 15, matching vcl's decimal output.

## Diagnostic failure with process status zero

`copy-reorder-constant-dest` asks for a recognized integer copy into VI00 in a
reorderable raw table. vcl emits no assembly and writes:

```text
INFO: OS_Init(Linux)
ERROR: found a move to a const zero register
ERROR: failed to detect moves!
ERROR: failed to do basic operation processing
```

The process exits with status **zero**. The same source in fixed raw mode compiles,
so the outcome comes from the move-detection gate.

The destination does not have to be spelled VF00 or VI00. vcl gathers
register webs before it detects copies, and a copy whose destination web is
bound to register 0 by an ENTER or EXIT mapping, given or inherited, fails the
same way. Observed from vcl 1.4beta7:

- `copy-output-zero-binding` (`move res, vf00` with `out_vf res(vf00)`) and
  `copy-output-zero-binding-int` (`iaddiu res, src, 0` with `out_vi res(vi00)`).
  The copy survives dead-code removal, and the detector then rejects it. A
  dead copy spelled with VF00 or VI00 in VCL code is removed instead and
  compiles.
- The binding is the web's, not the name's. `copy-output-zero-rebound` (ENTER
  VF02, EXIT VF00) fails, and `copy-output-zero-unbound` (ENTER VF00, EXIT
  VF02) compiles. `copy-output-zero-exits` fails: its copy reaches an exit
  bound to VF00, although another exit binds the name to VF02 later.
  `copy-output-zero-later-region` (a later region's rebinding) compiles, and
  `copy-output-zero-inherited` (an earlier region's VF00 inherited by an
  unmapped EXIT) fails.
- IADDI, IADDIU and ISUBIU with any numeric immediate count as copies here,
  not only immediate 0 (`dead-write-zero-iaddiu`). A symbolic immediate does
  not (`dead-write-zero-iaddiu-symbol`), and `isub res, src, src` does
  (`dead-write-zero-isub`).
- A web bound to two bases fails first with the `regweb` error, at status 255
  (`copy-output-zero-two-bases`).

Any other write into such a web is removed: see
[PRUNING.md](PRUNING.md#writes-into-a-web-bound-to-register-0). The port
builds the webs once after its second copy pass (`spell_zero_webs`) to make
both checks.

The library returns a typed compilation error. The CLI reproduces the status zero,
empty stdout and exact stderr for the retained invocation; these are literal
compatibility diagnostics, not a claim that the executable runs on Linux. Ordinary
compiler errors keep their usual status handling.

The differential comparator classifies `ERROR:` plus absent assembly at status zero
as `diagnostic_failure`, for both vcl and candidate observations, distinct from
compiled output, help and a status-255 rejection. A Rust process test compares the
exact captured streams and status and requires the library result to remain an
error.

## Observations and remaining differences

`local/tools/probe_copies.py` validates 25 vcl observations at alarm scale 1; their
inputs, flags, output digests and guest termination are retained. It runs vcl only
with `--capture`.

The port matches all normalized instruction pairs for integer zero
operands/immediates, symbolic preservation, D conversion, vector mask/broadcast
boundaries, explicit upper/lower moves, live flags, fixed/reorderable raw forms,
fixed writes to constants, ordinary memory coalescing, escaping outputs, multiple
consumers, the cross-stream and join-blocked controls (`coalesce-cross-stream`,
`coalesce-join-blocked`) and `induction-collapse-branch-blocked`. The status-zero
diagnostic failure matches both byte streams and status. The cross-stream and join
matches rely on ordinary external allocation, see
[EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md), and branch-delay handling, see
[BRANCH_STREAMS.md](BRANCH_STREAMS.md).

Execution cases cover output-observed pointers, both arithmetic signs, all memory
users, raw modes, vector identity boundaries and debug conversion, over 64 states
per case (the join case exercises both conditional paths). The logical model
excludes hardware timing, exceptional VU floating point and flag semantics; exact
pair checks and physical replay are separate requirements.

## Limitations

Static analysis of copy recognition, scalar coalescing, the mode gate, the pruning
stub and emission has discovery and indirect-flow limitations and does not by
itself establish runtime behaviour; the vcl observations, pair replays, model
checks and corpus matrix supply narrower validation. The notes are kept locally and
are not tracked. Still open: branch/delay merging, raw reorder reduction,
allocator-generated moves, copy-web allocation, full readiness and exact output
formatting.
