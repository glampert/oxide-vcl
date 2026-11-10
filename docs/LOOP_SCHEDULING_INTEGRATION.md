# Normal-loop scheduling helper

`crates/vcl/src/loops/loop_schedule.rs` builds a bounded dependency/resource
problem for a normal, unscheduled single-stream loop. It returns candidate
folded rows and iteration stages for the pipeline-expansion code. This is the
standalone modulo scheduler; the compiler's default loop scheduler is the
port of vcl's loop search in [LPGEN_PORT.md](LPGEN_PORT.md), and this helper
runs when `VCL_OLD_LOOPS=1` is set. A successful graph schedule is not a
complete transformed program: allocation, move repair, side-effect checks,
cloning, immediate repair and rebuilt flow remain required.

The descriptions of vcl are inferred from vcl 1.4beta7 (static analysis). No
openvcl source or output was used. vcl's routines and this helper have
different internal representations.

## vcl eligibility and ordering

vcl's loop-eligibility check requires a conditional control-flow class and a
selected outgoing edge returning to the same stream. A backward textual
branch alone does not establish eligibility. The loop search also requires the
stream's enable field to be nonzero. The `loop-integer-backedge` and
`loop-vector-backedge` runs use only linear optimization. `loop-vector-counted`,
with `--LoopCS 1,0`, invokes loop optimization and produces a four-row modulo
kernel.

The Rust helper takes an explicit enabled flag, verified conditional identity
and selected backedge. It additionally restricts itself to:

- One reachable body block, with one conditional self-edge and one other exit.
- Exactly one branch, the final source operation, issued in the lower pipe.
- Body metadata matching the flow graph's effective read/write masks exactly.
- Every nonconstant written register having an explicit latency.
- Reaching definitions consistent with this single-stream source order.

It rejects disabled loops, missing or stale flow, ambiguous local definitions,
unknown latencies, unsupported control flow, and explicit hazard distances
other than zero or one. These are helper limits, not claims that vcl rejects
such programs. The compiler keeps a legal linear schedule when the normal-loop
optimization is unavailable.

vcl orders its passes as in [SCHEDULING.md](SCHEDULING.md): validate source
flow, prune, form webs and external assignments, initialize immediate fixups,
optimize eligible loops, expand the resulting modular tables, and rebuild flow,
webs and assignments. The helper belongs before expansion; it does not
reinterpret already staged raw instructions.

## API

```rust
let problem = loop_schedule::build(
    &graph,
    &analysis,
    &body_metadata,
    loop_control,
    &proven_hazard_edges,
)?;
let candidate = loop_schedule::schedule(&problem, &search_options)?;
```

`Operation` supplies a stable flow operation ID, a `schedule::Operation`,
per-register write latencies, per-register additional input timing marks,
side-effect classification, affine `relaxed_reads`, and an `is_nop` flag.
Metadata may be supplied in any order: the graph block's operation list defines
source order.

`Control` supplies the body block, conditional operation ID, actual selected
backedge, whether the branch is conditional, the enable flag, and
known-minimum and known-safe counts. The known minimum must be positive.
Source parsing validates directive syntax; the helper does not guess these
settings from instruction names.

`Problem.scheduling` contains local-index dependencies. `Problem.ids` maps
them back to stable source IDs. `SearchOptions` carries maximum period,
LoopExtra, LoopAbs, and the deterministic scheduling budget/horizon.
`Scheduled` returns the ordinary schedule, `pipeline::LoopOperation`
placements using the original IDs, the conditional ID and both count hints.
The caller supplies loop names and a unique prefix to construct
`pipeline::LoopSchedule`.

The returned placements are dependency/resource candidates. They must not be
emitted directly as assembly or treated as allocated modulo tables.

## Recurrence construction

For one register component, a single source-ordered body has at most one local
reaching definition at each read:

1. If a writer precedes the read, the last such writer supplies the current
   iteration: distance zero.
2. Otherwise, the body's final writer supplies the previous iteration:
   distance one. A read/write operation's old input uses this rule as well.
3. If the body never writes the component, its definition is an entry value.
   The helper adds no internal RAW edge; expansion must preserve that entry
   value and its physical/interface constraints.

The helper checks that the actual `flow::Analysis` local-definition set
matches this rule. External/preheader definitions remain meaningful initial
values. Uninitialized alternatives are rejected.

A dependency means:

```text
time(consumer) + distance * period >= time(producer) + latency
```

For RAW, latency is the producing register operand's latency plus the consumer
input's extra timing mark. Different components can contribute to one edge;
the maximum necessary latency is kept. Disjoint components do not create a
RAW recurrence.

The helper also adds conservative reuse constraints:

- A read precedes the next overwrite issue, wrapping to distance one if
  needed. A read/write operation reads before its own write and needs no
  ordinary self-edge for that same issue.
- Consecutive writes preserve completion order, including the last-to-first
  edge. Its issue separation is `max(0, previous_latency + 1 - next_latency)`.
- Memory reads/stores keep source issue order and a last-to-first ordering
  edge. No memory disjointness is inferred from textual address spelling.
- Additional proven forced hazards can be supplied explicitly in local body
  indices with distance zero or one.

These reuse constraints can be stricter than vcl's when its move-repair engine
creates additional register identities. The final allocator must still reserve
whole-register VF write-port locks on folded issue rows, alongside
per-component liveness; see [TIMING_COLLISION.md](TIMING_COLLISION.md).

## Conditional-input timing marks

The weighted dependency rule adds output latency to the input operand's extra
timing byte. It does not replace every input latency with a blanket minimum of
two.

vcl computes these marks after reaching-definition construction. It clears a
nonzero input's extra mark, then considers conditional branches. For each
reaching producer it considers the producer's memory classification and
operand component; a direct memory producer at component zero is excluded, as
is a producer that reads one of the condition/status registers (CLIP, STAT,
MAC). For a qualifying producer the conditional input gets extra mark one.
So an ordinary IALU result of latency one gets a two-cycle edge, while direct
loaded VI inputs do not get an extra five-cycle edge just because loads have
latency four.

`Operation.read_delays` is keyed by the actual register identity; an absent
entry means zero. The caller supplies these marks from effective operands and
reaching definitions. The helper does not derive hardware IDs from
reconstruction-local register numbers or assume every conditional input has
identical timing. Marks are additive and apply only to the indicated register.

## Periods, resource bounds and branch slots

vcl derives loop bounds from issue-pipe counts, instruction latency/consumer
pressure, resource throughput/pressure and circular dependencies; the stored
loop bound is at least two. The helper computes a subset: at least two,
upper-only and lower-only counts, half total issue pressure, summed
per-resource throughput, and maximum explicit write latency. It is not an
exact port of every bound or diagnostic reason.

The modulo scheduler also checks recurrence difference constraints, folded
resource occupancy, pipe pairing, finite horizon and branch placement. A
conditional is placed in lower folded row `period - 2`, leaving its physical
delay row at `period - 1`. Absolute issue time and folded row stay distinct;
stage is `absolute_cycle / period`.

The search starts with lower bound plus LoopExtra. A valid LoopAbs can raise
that start but not force a value below it. The helper tries every strategy at
each bounded period, validates side-effect ages for each candidate, then
chooses the minimum maximum stage; equal stages replace the previous
candidate. Search budget exhaustion is distinguishable from failure to find a
schedule. The limit is deterministic, unlike vcl's watchdog and rollback
counters.

vcl placements allow iteration indices 0 through 16, and the scheduler
enforces that limit as well as its absolute horizon. The pipeline expander has
a broader limit for already specified raw stages; that does not permit normal
modulo search to use arbitrary stages.

## Side-effect ages

vcl classifies instruction side effects: stores are category 1;
critical/irreversible or relevant implicit writes are category 2; other
instructions are category zero. Loads do not become stores merely because they
participate in memory ordering. Classification uses the post-pruning effective
operands, preserving live implicit effects.

The maximum conditional stage and the earliest store and critical stages must
satisfy:

```text
store_stage >= conditional_stage - known_safe
critical_stage >= conditional_stage
```

Negative store bounds impose no restriction. vcl marks operations and
backtracks to fix a violation. The helper instead postvalidates the returned
candidate and reports `UnsafeEffect` with operation/stage information. It does
not accept a schedule merely because modulo resource occupancy fits, and does
not invent safe speculative memory accesses or irreversible effects. The
caller keeps its linear alternative when this check fails. The postcheck does
not replace later exit/prologue validation.

## Required compiler steps after a candidate

A safe integration still must:

1. Validate/rewrite physical register assignments and any necessary moves for
   the modulo table, including folded component intervals and extra locks.
2. Construct `pipeline::LoopSchedule` and expand its entry, prologue, kernel,
   epilogue and exit windows.
3. Use typed `loop_ir` clones and the induction/immediate repair.
4. Rebuild component reaching definitions, register webs, pins, forced hazards
   and allocation after transformations. Preserve initial source validity.
5. Validate each emitted path's physical timing, branch targets/delay slots,
   externally observed values, side-effect counts and generated labels.
6. Keep a legal linear candidate available when normal-loop optimization is
   unsupported, fails allocation or exhausts its bounded search.

The generic `schedule_best` compares folded row count, which is identical for
all candidates at a period. This adapter instead uses vcl's maximum iteration
ID rule and validates ages before selection. vcl's extent reduction, move
mutation and folded allocation still differ, so strategy choice and pipeline
depth are not identical to vcl's.

## Tests and original observations

Standalone helper tests cover integer recurrences/branch placement, the
counted-vector shape, forward and backedge RAW definitions, disjoint
components, cyclic memory order, period hints, resource pressure, unsupported
CFGs, stale/undefined flow, side-effect ages, missing latencies, invalid
distances, per-register input timing marks, and affine `relaxed_reads` (which
relax only placement RAW/WAR edges while preserving source validation, setter
recurrence and memory ordering). Compiler and execution tests cover the
integration.

The original run `local/evidence/oracle/loop-vector-counted/` (local, not
tracked) selects period 4, conditional row 2 and maximum iteration ID zero;
the helper reproduces that placement shape in its focused test.

The `loop-integer-counted` probe adds `--LoopCS 1,0` to the plain integer
loop. With the original at `-t1 -uLOOPBUILD` and alarm scale 1 it searches
period 2, times out each strategy, then enters period 3; the harness stops it
after 60 seconds without a guest exit. The original's outcome is therefore
**unknown** (neither acceptance nor rejection), and no exact-equivalence
claim rests on the helper's integer candidate. Output and metadata:
`local/evidence/oracle/loop-integer-counted/`.

## Compiler adapter

The integration supplies additional conditional timing marks from effective
producer metadata; loaded VI inputs do not get an invented extra cycle. It
marks loop-live outputs critical so visible updates cannot advance past an
early exit. Memory edges are added independently of that classification.
Affine relaxations require prior source annotation and are repaired against
the actual expanded CFG. Normal candidates have a transactional linear
fallback; explicit raw tables expand separately before source validation. See
[PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md) for tested outcomes and
the remaining differences in move/allocation/late-cleanup behaviour.
