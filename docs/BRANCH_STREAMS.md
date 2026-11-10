# Branch-delay adjustment and adjacent-stream merging

This chapter describes how vcl 1.4beta7 (SHA-256
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`)
exchanges a lower-pipe branch tail, merges adjacent streams and prints stream
flags, and what the port implements. Behaviour is inferred from vcl 1.4beta7;
no openvcl implementation was consulted.

The port covers a bounded ordinary-stream boundary adapter and preserves
explicit continuations, fences and label flags. Remaining layout and
physical-register differences are recorded below. VI-load zero tails,
fixed-predecessor timing, intermediate CFG distance and the raw reorder bridge
are in [RAW_BOUNDARIES.md](RAW_BOUNDARIES.md); cross-stream and join
physical-register assignment is in
[EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md). Selected/loop bindings
remain a separate lifecycle boundary.

## Original procedures

The following responsibilities were inferred from vcl 1.4beta7. The names
below describe roles, not the original's identifiers.

- The original's branch-delay exchange: one lower-pipe tail exchange.
- Merge adjacent eligible streams.
- Transfer the merged successor's end properties.
- Prepare external operand and extent bounds.
- Stream commands, including CONT and barrier.
- Shared D/E/T/M flag string builder.
- Stream termination and label-flag emission.

Supporting responsibilities: a helper tests a predecessor write against branch
reads; another checks candidate placement; the table concatenator appends
dispatch entries and transfers instruction ownership; another checks exported
labels; the stream splitter splits source streams and collects label flags.

The original's order matters. External allocation comes
before linear optimization. The linear optimizer prepares external
information, schedules, reduces extents, runs the branch-delay exchange,
validates/patches moves and tries local allocation. Main calls stream merging
afterward. The port runs the delay exchange before its local allocation and
merges after allocation.

## The branch-tail exchange

For a linear table with `n` rows, the original requires:

- A movable mode: fixed VSM submodes 2/4 return without modification.
- Zero modulo period, at least three rows, and room above external minimum
  extent (the table minimum).
- The branch in lower slot `n - 2`, and an entirely empty final row `n - 1`.
- Both branch earliest bounds at most `n - 3`.
- No component written by the old lower instruction at `n - 3` is read by
  the branch. The helper excludes immutable pool-zero resources.
- Placement checks succeed for the earlier branch and, if present, the
  displaced lower instruction at `n - 2`. The displaced instruction's two
  external tail bounds must both be zero.

The mutation is precisely:

```text
before                        after
row n-3: upper A | lower P     row n-3: upper A | branch B
row n-2: upper C | branch B    row n-2: upper C | lower P
row n-1: empty   | empty       [removed]
```

The displaced instruction executes in the delay pair on both branch paths.
Upper operations keep their rows. A branch with no previous lower instruction
can also move into that empty lower slot. A branch that reads P's result cannot
use the exchange even if another placement heuristic would consider it cheap.

`linear_cleanup::adjust_branch_delay` implements this transformation. The
orchestrator retains two edge sets: actual RAW/WAR/WAW/memory constraints and
the scheduler's extra source-order barriers into a branch. Only the latter
barriers may be crossed by the exchange. It checks earliest bounds, actual
edges, component writes, paired resource conflicts and execution-unit
throughput before accepting the new table.

```rust
candidate[n - 3][1] = Some(branch);
candidate[n - 2][1] = previous_lower;
candidate.truncate(n - 1);
// Accept only after checking the two changed placements and output extent.
```

The compiler proves a zero tail only for unmodified ordinary
VI operations with latency at most one, no memory/flag/other side effects and
no additional outgoing branch-read debt. It also retains extent required by
other escaping non-VF results. Fixed tables, selected generated tables, dumb
mode and general memory/implicit-output motion are excluded. The pure helper
supports broader caller-proven placements; the compiler does not invent those
proofs. Full raw reduction and the original external lock tracker are not
ported.

## Incoming readiness across a direct edge

An external producer does not impose its entire latency as a leading delay
in the consumer block: issue pairs already elapsed after the producer in its
predecessor count. For ordinary direct predecessors that have already been
scheduled, the bounded adapter uses:

```rust
let incoming_debt = producer_latency.saturating_sub(guaranteed_issue_pairs);
consumer.earliest = consumer.earliest.max(incoming_debt);
```

Every reaching definition contributes a constraint. The count starts with the
producer's own issue pair. Branch frames protect their full row span; ordinary
frames count only nonempty pairs because final replay may remove empty rows.
The adapter uses the producer's own predecessor table, never a flat address
distance through mutually exclusive CFG paths. Unsigned saturation prevents
already satisfied latency from becoming an artificial delay.

The original's external-bound preparation is substantially more general. It resets four per-op
bounds, walks component external setters/users, combines setter latency and
consumer bias, subtracts inter-stream distance and scheduled
distance to/from the boundary, then records maximum
positive requirements. It distinguishes instructions with selected images
from unscheduled streams and maintains further through locks
and minimum extent. The Rust adapter does not reconstruct this whole tracker.

Back edges, indirect/multihop reaching definitions without a direct scheduled
predecessor, generated/raw tables and slow VI-to-branch biases use a
conservative bound.
Ordinary IALU-to-branch reads keep a two-row minimum.

## Late stream concatenation

The merge routine is a gate followed by dispatch concatenation. Both original
streams must be movable linear VSM modes 1/3; current end and successor start
must be ordinary. It requires one outgoing reference, one incoming reference,
a kind-zero edge to the physically next stream, and no current branch. It
also rejects current end fences, successor start fences, current end debug
flags, successor top debug flags and successor exported labels.

On success the table concatenator appends rows, reassigns instruction owners and rebuilds
lists. The merge then copies the successor end kind, branch subtype, end flags
and fence. References are retargeted, the successor is removed, and the same
current stream is reconsidered for another merge. There is no arbitrary
instruction rescheduling in this routine.

The compiler reuses `control::can_merge_adjacent`. It keeps source CFG edges
and allocation lifetimes intact and concatenates emission tables after
allocation. A separate last-member index supplies the merged group's outgoing
edge facts on repeated merges. Joins, exports, continuations, flags and fences
block merging. End and output properties transfer from the final member.
The late merge adapter admits ordinary VCL blocks with no selected table;
the original's broader movable raw/generated table cases remain excluded.

The original does not emit every source label. In the boundary probes, a
redundant source branch disappears and several nonexported `next` labels are
absent even where a fence/debug property prevents concatenation. The native
compiler still emits some of these labels and header entries. Therefore label
presence alone is not evidence that the original merged two streams. The
predicate comes from the binary; pair equality is reported separately from
label/header and full-byte equality.

## Continuation and flag emission

`--cont` changes ordinary end kind 1 to kind 2. It is not a plain fallthrough
marker. The original's termination step gives kinds 2 and 3 ordinary E termination and safe-end
padding; CONT still requires a physical successor in flow. The retained
control has two separate E-terminated pieces, four total pairs. The port
preserves this end kind through splitting.

`--barrier` sets the stream's start fence byte; a barrier splits before the
command. Label modifiers are ORed into top flags by the builder and
applied to the first emitted upper instruction, including an empty NOP. The
shared builder prints flags in **D, E, T, M** order. The port
emitter uses one shared formatter for instructions and empty upper slots.
The combined control produces `mul[DETM]` exactly.

The `-e` handling is asymmetric: the empty-NOP hook masks
off E, while the occupied-instruction hook passes the flag
through. The `cli-no-endbit` control emits `add[E]` under
`-e`. The port preserves this rather than using uniform flag
`-e`. This behavior is preserved rather than replacing it with uniform flag
suppression. Source `.noebits` remains a separate input directive.

## Controlled original observations and validation

`local/tools/probe_control.py --capture` authors the sources and runs the existing
isolated original-binary Qiling recipe with `-t1 -uFLOW`, alarm scale 1 and a
30-second wall timeout. The default invocation verifies the original/source/
stream hashes, exact argv, completed expected status (zero or 255) and absence of emulator
errors/timeouts. The observations and one rejected-context control are summarized in
`local/evidence/analysis/control-probes.json` (local, not tracked); full streams and metadata are under
`local/evidence/oracle/flow-*`. No timeout is treated as a language rejection.

| Controls | Original/native pairs | Result |
| --- | --- | --- |
| Independent VI branch delay | 5 / 5 | Exact normalized pairs |
| Plain/export/fence/debug/mark/trap/combined-flag/chain boundaries | 3 / 3 each | Exact normalized pairs |
| CONT boundary | 4 / 4 | Exact normalized pairs |
| Loaded-VI branch tail (`flow-delay-tail-debt`) | | Exact normalized pairs |
| Fixed raw boundary (`flow-merge-fixed`) | | Exact normalized pairs |
| Upper operation near branch | 6 / 6 | Different upper row; later original scheduling choice differs |

The invalid CONT context is rejected by both compilers. The original rejects
it earlier in parsing with
`encountered cont directive in bad context..mode=2`; the native parser reports
its generic invalid-enter/exit-context error. Full diagnostic bytes
are not equal. The `induction-collapse-branch-blocked` control is an exact
pair match. The cross-stream and join controls have the original three/five
pair counts, but use VI01/VI02 where the original externally assigns
VI02/VI03 (see [EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md)). No register
renaming is hidden by the comparison.

Unit tests cover the tail exchange, component dependence, earliest bounds,
real zero-latency edges, missing zero-tail proof, minimum extent, upper-pair
conflicts, throughput, nonempty final rows, modulo exclusion and empty lower
slots. End-to-end tests compare retained original pairs
(`crates/oxide-vcl/tests/oracle_pipeline.rs`) and check that merged
chains remove internal labels while exports/joins retain correct offsets.

Bounded execution comparisons (`crates/oxide-vcl/tests/semantic_equivalence.rs`)
run 64 states each, with both conditional outcomes and varied loaded VI
addresses, checking declared VF/VI escapes and all memory. D/T/M
markers are covered by exact pair tests only: the model deliberately does not
support their hardware behavior. CONT is also excluded from this execution
comparison because execution beyond the first E termination requires an
external restart. This model is not a cycle-accurate or numerical-edge proof.

Not ported: raw reorder/full boundary reduction beyond what
[RAW_BOUNDARIES.md](RAW_BOUNDARIES.md) lists, the remaining scheduling
choices, and exact diagnostics.
