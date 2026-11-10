# Component pruning and current/sticky status flow

Address operands (classes 28-33) use scalar dependency mask 1 even when their
parsed memory field selector is Y/Z/W. Expansion keeps the parsed selector for
emission while supplying the scalar mask to initial validation and pruning, so
`iBase.w` or `buffer.w` is not an undefined read. See
[ADDRESS_DEPENDENCIES.md](ADDRESS_DEPENDENCIES.md).

Behaviour here is observed from vcl 1.4beta7 or inferred from it.

## Validity comes before dead-code removal

Component flow runs before pruning, so an instruction that will eventually be
dead can still make the input invalid. Dead-code removal does not excuse an
undefined source operand.

```asm
; Rejected: the initial ADD reads a.yzw before they are defined.
--enter
in_vf.x a(vf01)
--endenter
add b,a,a
--exit
out_vf.x b(vf02)
--endexit
```

With `in_vf a(vf01)` instead, initial flow succeeds and pruning narrows ADD to
`.x`, because the output interface observes only `b.x`. Explicit `add.x` with
`in_vf.x` also succeeds. These are `prune-add-input-x`, `prune-add-output-x`,
and `prune-add-explicit-x` under `local/evidence/oracle/` (local, not tracked).

`prune-dead-undefined`, `prune-dead-partial`, and `prune-dead-hardware-q` show
that initial validation applies even to an unused multiply: an undefined
vector, undefined YZW lanes, and undefined Q all fail before the multiply could
be removed.

## Propagation algorithm

Per-operand prune bits start cleared. Instructions with nonzero special
descriptor flags, structural synthetics, branches, and stores are retained
roots; they start with their input bits marked. Raw/synthetic tables have
separate preservation rules.

Two propagation steps repeat until no new bits are found. For each output
component, the pass follows its linked input users; if a user's component is
marked, the output component is marked too. Newly required outputs mark
required bits on every register input through an output-to-input transfer:

| Output/input class combination | Required input bits |
| --- | --- |
| Both classes are VF4 (`1`) or ACC (`9`) | Same live output bits |
| Same pair with the MR32 hook | Output bits rotated left: X to Y to Z to W to X |
| Every other combination | Entire existing input operand mask |

Output classes ACCXYZ (`10`) and VF_XYZ (`11`) are indivisible: when any
component is needed, their complete original mask is marked. OPMULA and OPMSUB
therefore stay `.xyz` when only X is exported; no smaller cross-product
expression is derived. A broadcast source keeps its fixed scalar lane while an
ordinary vector source can narrow.

A final step commits the result. A completely unmarked removable instruction is
deleted. For an arbitrary-mask operation, needed VF4/ACC input fields are
combined, the MR32 input mask is rotated back to destination coordinates, and
the instruction mask and matching operands are updated. An unused virtual
destination is replaced by its pool's zero register when no implicit backlink
requires it; physical destinations are not replaced by this rule.

An unused LQI result shows that destination liveness and instruction liveness
differ: LQI stays because its address-register update is used, its unneeded
virtual VF result can target VF00, and the implicit read/write link for the
updated integer base remains.

## Port implementation

`crates/vcl/src/analysis/prune.rs` separates graph propagation from physical
source rewriting:

* `expand` converts parsed operands to effective operands, including status
  expansion and the MR32 read mask. `source_operand` preserves the parsed index;
  expanded operands have distinct indices even when they share a placeholder.
* `required_inputs_expanded` implements the class transfer for one instruction;
  `required_inputs` accepts parsed indices.
* `prune_expanded` returns a narrowed instruction, effective operands, required
  read masks, live output masks, zero-destination candidates, and retained state.
* `propagate` takes explicit reaching-definition `UseEdge`s and visible output
  `LiveRoot`s and unions required read components back into producer outputs
  until stable, preserving branches/stores/raw operations and explicit keep
  roots. Joins have multiple incoming edges; loops can have cyclic edges.

Bit unions are monotonic, so rooted cycles converge without a recursion cutoff.
Undefined/external/constant values have no internal producer edge; the caller
must still perform the initial full-source flow check.

`Liveness.pruned` entries carry both the selected fields and the live dependency
subsets. An instruction that stays for a MAC/status result may still physically
write a vector destination or other flag outputs, so a caller must apply
zero-register substitutions and preserve physical side effects before
scheduling.

## Status expansion

The source descriptor table contains virtual classes rather than all effective
status operands. They are expanded before component flow:

| Virtual class | Effective status operands |
| --- | --- |
| `22` / DIV status | Write STAT_DIV with the producer's result latency; read STAT_DIV_S with offset -1, blocking |
| `23` / ALU status | Write STAT_ALU at latency 4; read STAT_ALU_S with offset -1, blocking |
| `24` / FSTST | Select current/sticky reads by the test immediate; sticky selections also write the corresponding sticky token at latency 1 |
| `25` / FSSET | Write STAT_ALU_S and STAT_DIV_S at latency 1, blocking |

MAC is a separate direct implicit operand, not the same resource as current or
sticky ALU status.

For FSAND, bits `0x00f` require current ALU status and bits `0x030` current
divide status. Bits `0xc00` require both current ALU and sticky ALU; bits
`0x3c0` both current divide and sticky divide. FSOR uses the complement of the
immediate. FSEQ and unresolved symbolic immediates select all four resources.
These are compiler dependency selection rules, not an implementation of the VU
flag arithmetic.

## Sticky status is an implicit inherited interface

The first arithmetic instruction can read STAT_ALU_S without an explicit
`in_hw_...` declaration. Inferred from vcl 1.4beta7: for each stream with ENTER
type 2, the interface binder adds the sticky ALU and sticky divide hardware
registers (STAT_ALU_S and STAT_DIV_S) as incoming inputs (the "fpu sticky
flags as xin" and "div sticky flags as xin" diagnostics). Each is bound to the
selected hardware register with a mask of that register's component count (one
for both sticky resources) and appended to the ENTER table.

This supplies an opaque incoming value. It does not initialize either flag
register to zero, and it does not make writes ineffective. Ordinary reaching
definitions model FSSET and FSTST replacing those tokens.

Fresh STAT_ALU, STAT_DIV, and MAC are not supplied as incoming values.
FSAND `0x400` with no preceding ALU operation therefore fails with
used-before-set STAT_ALU, even if FSSET precedes it: FSSET writes sticky tokens,
not the fresh ALU result the test also requires.

`crates/vcl/src/analysis/status.rs` exposes these inherited inputs and maps
effective status accesses to typed resource names, directions, latencies, and
original IDs. The compiler binds them to flow register IDs and reuses the
expanded operand indices when building pruning edges.

## Ordering observations

`local/tools/probe_status.py` retains twelve cases in
`local/evidence/analysis/status-probes.json` (local, not tracked):

* `status-fsset-before-add` emits FSSET before ADD; ADD's sticky-token read
  depends on the reaching FSSET output.
* `status-fsset-after-add` pairs ADD with FSSET in the same bundle: reads occur
  before writes in a pair, so the old sticky token is consumed before FSSET
  replaces it.
* `status-two-sticky-reads` keeps the first FSAND before the next SUB, then the
  second FSAND; the first sticky FSTST's output token reaches the subsequent
  arithmetic read.
* `status-two-alu-sticky` removes an earlier unconsumed ADD before SUB and FSAND
  `0x400`; `status-two-div-sticky` likewise removes an unconsumed SQRT before
  DIV and FSAND `0x100`. No edge from a sticky test to every earlier arithmetic
  writer is added.
* `status-branch-missing-current` fails with used-before-set STAT_ALU on a path
  reaching the test without an ALU definition; `status-branch-current-join`
  succeeds when both incoming paths define current ALU status.
* `status-fsor-all-mask` produced no guest exit under the emulator; the outcome
  of vcl 1.4beta7 on it is not established.

The explicit ENTER input resolves the initial sticky read but does not remove
the need for cross-stream reaching-writer, path-timing and join checks. Exact
call/return path matching and timing across independent compilation regions
remain open; see [COMPATIBILITY.md](COMPATIBILITY.md).
