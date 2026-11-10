# General VI immediate coalescing

Statements about vcl 1.4beta7 internals are inferred from it.

Software-loop cloning copies VI update instructions and their immediates. The
later appearance of `ISUBIU 0` followed by `ISUBIU 2` in an expanded prologue
comes from a general immediate-coalescing pass, not a stage-number formula. The
distinct memory-displacement epoch repair is in
[IMMEDIATES.md](IMMEDIATES.md).

## Raw pipeline fixtures

Both raw loops (`pipeline-raw-stages-min1` and `pipeline-raw-stages-min4`) have
period 5, maximum stage 2, conditional stage 0, and source
`isubiu vi01,vi01,1` in stage 0.

With minimum trip count 1, vcl 1.4beta7 emits two separate five-row prologues,
each containing a decrement by one and a conditional early-exit branch. Each
decrement's result is tested by a branch, so its value cannot be changed by
pushing the decrement into a later operation.

With minimum trip count 4, the early exit tests disappear and the two prologue
windows are joined. The decrements keep their issue sites but change constants:

```asm
; row 1 in the combined prologue
isubiu VI01,VI01,0
; row 6 in the combined prologue
isubiu VI01,VI01,2
```

The kernel still decrements by one. This follows ordinary scalar flow: the
first prologue decrement's only user is the second decrement, so its constant
is absorbed there. The second decrement's value flows into the loop header,
where the next kernel decrement also has a loop-carried reaching definition;
that join prevents pushing its constant again. No rule such as
`immediate *= clone_bias` applies.

## Pass order

The cleanup stage runs the immediate collapser when immediate collapsing is
enabled, then detects moves, rebuilds dependencies, prunes moves, and rebuilds
again. The driver runs this cleanup at several transformation boundaries,
including after generated streams are merged and dependencies rebuilt.
Raw-loop expansion and epoch displacement repair are separate stages.

The collapser first sweeps all streams and normalizes numeric IADDI (instruction
ID 109) to IADDIU (110) for a nonnegative immediate or ISUBIU (112) for a
negative immediate, using its magnitude. Symbolic immediates do not take this
path. The normalization is independent of whether the operation later
coalesces.

It then repeatedly scans streams and instruction lists until a complete scan
makes no change. Only IADDIU and ISUBIU are candidates. The immediate must be
numeric, nonzero, and unsigned 15-bit: `0 < immediate < 32768`. The signed delta
is positive for IADDIU and negative for ISUBIU. The source register is operand
1, output operand 0, immediate operand 2. Input and output need not be the same
register, so a copy-with-offset can fold too.

## User and reaching-definition conditions

For the candidate's output operand the pass walks its complete user-edge list.
Every edge must satisfy all of these:

1. The user is not the candidate itself. A loop-carried self-use prevents the
   push.
2. The user is a supported consumer and its numeric immediate can absorb the
   delta within the user instruction's representable range.
3. The edge's read operand is exactly the consumer's input operand (operand 1
   for all supported users).
4. Every reaching-definition edge on that user operand comes from this
   candidate. A join with any other definition prevents the push.

An unsupported branch, a scalar observation, a symbolic immediate, a user of a
different operand, or any out-of-range user blocks the entire candidate; users
that happen to be compatible are not changed. Escaping outputs count as
observations even when represented as boundary sets instead of instruction
users.

vcl 1.4beta7 uses intrusive flow lists, not a distance test on issue rows or
equality of virtual names. A compiler must supply complete post-clone operand
flow; guessing only the next VI instruction can change a branch test or an
observable output.

## User arithmetic and limits

| Instruction IDs | Instruction | Read operand | Immediate operand | Proposed numeric value |
| --- | --- | --- | --- | --- |
| 110 | IADDIU | 1 | 2 | `old_immediate + producer_delta` |
| 112 | ISUBIU | 1 | 2 | `old_immediate - producer_delta` |
| 155, 156, 160, 161 | LQ, ILW, SQ, ISW | 1 | 1 | `old_offset + producer_delta` |

For arithmetic users, the absolute value of the proposal must be less than
32768. A negative proposal changes IADDIU to ISUBIU, or ISUBIU to IADDIU, then
stores the magnitude. A zero proposal retains the user's opcode. Memory offsets
must satisfy `-1024 <= proposed < 1024` and keep their opcode and signed
offset. LQI/SQI/LQD/SQD, branches, IADD, arbitrary expressions, and wider
memory ranges are not supported users.

All approved user changes are written; only after the whole user list passes
validation is the producer immediate set to zero and the scan marked changed.
The producer stays in place; a zero-immediate copy can later be recognized and
pruned. Raw outputs may keep the zero instruction at its issue site, as the
probes show.

Arithmetic is 32-bit wrapping addition/negation followed by the
representability checks. The signed-minimum IADDI corner keeps its bit pattern
and is then rejected as a producer; it is not turned into an encodable huge
update.

## Observations from vcl 1.4beta7

Probe sources are `local/evidence/inputs/induction-collapse-*.vcl` with matching
`local/evidence/oracle/` directories (local, not tracked). Raw sources enable
`.init_vi_all` so temporary VI registers are allocable.

| Probe | Source | Emitted constants |
| --- | --- | --- |
| `subtract-chain` | two ISUBIU 1 updates | ISUBIU 0; ISUBIU 2 |
| `sign-flip` | IADDIU 5 then ISUBIU 2 | IADDIU 0; IADDIU 3 |
| `memory` | IADDIU 3 then LQ offset 5 | IADDIU 0; LQ offset 8 |
| `branch-blocked` | ISUBIU 1; conditional use; another ISUBIU 1 | both decrements remain 1 |
| `range-blocked` | IADDIU 3 then LQ offset 1022 | update remains 3; offset remains 1022 |

These distinguish coalescing from displacement epoch repair: no software
pipeline or instruction movement is needed for the 0/2 and 5/8 results, and a
branch observer blocks them even though the arithmetic would fit.

## Port implementation

`crates/vcl/src/analysis/induction.rs` is a planning/application layer:

```rust
normalize_iaddi(operation); // separate initial normalization sweep
let change = plan(producer, &all_users)?;
if let Some(change) = change {
    apply(&change, &mut operation_views)?;
}
```

`Operation` carries a stable ID, original instruction ID, and optional numeric
immediate. `Consumer` adds the exact read operand and all reaching definitions.
`User::EscapingOutput` blocks changing an observed producer. `Plan` returns the
zeroing producer rewrite and every consumer rewrite with each immediate's
operand index. `apply` validates all IDs and snapshots before mutating
anything. A missing reaching-definition list is a graph error, not permission to
assume a unique setter.

The helper does not rebuild CFGs, infer register identities, rewrite register
operands, remove issue slots, choose generated branches, or run a fixed-point
scan; the compiler does those separately and keeps epoch displacement repair
apart.

The compiler runs `coalesce_immediates` on ordinary validated, pruned source
before normal loop generation and on the expanded region. It normalizes IADDI,
indexes complete operand users across reachable blocks and escaping outputs,
then scans in block/instruction order to a bounded fixed point. Branch users,
wrong operands, symbolic values, incompatible joins and range failures block the
entire producer. Cross-block uses are actual users, not invented escaping
observations.

Numeric rewrites preserve register IDs, masks and read/write latencies. Reaching
definitions are cached during the fixed point and consumer opcode/immediate
views refreshed after every rewrite. Copy conversion and pruning get fresh
analyses afterwards. The strict undefined-read check runs before these
transformations.

The unit tests cover the raw-loop 0/2 case, branch/escape restrictions,
add/subtract sign changes, the four direct memory consumers, range boundaries,
multiple users and definitions, symbolic/unsupported users, normalization, and
atomic rejection of stale/missing/duplicate operation views.

Against vcl 1.4beta7, the subtract-chain, sign-flip, memory, range-blocked and
raw min1/min4 cases match; for `branch-blocked` the constants match but the
branch/delay layout differs. See [COPIES.md](COPIES.md) for the remaining
layout differences.
