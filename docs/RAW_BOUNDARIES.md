# Raw reorder reduction and external readiness

This chapter describes how vcl 1.4beta7 shrinks eligible raw prologues and drains
after physical assignment (pull-up and drag-down), how it separates outgoing tail
debt from incoming latency, and how the port reproduces that. vcl behaviour is
inferred from vcl 1.4beta7 by static analysis and controlled observation; the
port's adapter choices are marked as such. The reconstruction is bounded:
synthetic endpoint merging, broader motion and exact whole-program output are not
complete. External register assignment is covered in
[EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md); the physical raw-motion bounds
here use the path established before it.

## Reduction pipeline

vcl processes raw VSM streams after physical assignment. It prepares external
bounds, builds a work table, checks reduction eligibility, applies linear
reduction, prepares final size reduction, validates/patches moves and installs the
candidate.

The reduction loop admits code mode 3 with submode 1 or 3 and period zero. It
alternates enabled pull-up and drag-down passes until the extent stops shrinking;
each pass can also run its own fixed point of local movement. Fixed submodes and
modulo kernels keep their layout.

A separate pressure-oriented extent routine counts written components minus read
components, requires a nonnegative balance plus an output with a user bound, then
tries a later placement, still checked against locks and latencies. This is not
the whole raw drag-down algorithm.

The initial gate disables motion if the stream has recorded stalls or its current
table is shorter than the external minimum extent. Recording a stall marks both
owner streams; a reset step clears it. The gate can also disable motion for the
reference's hazard-broadcast byte, populated by cross-stream hazard broadcasting;
that byte is not an ordinary live-in/live-out intersection. Per-operand
immobility masks are built from input/output/hazard masks, not a blanket rule
that every external register is immobile.

Pull-up visits rows ascending, upper then lower, and tries earlier placements.
The branch is handled separately: a lower delay instruction that writes a
branch-read component stays attached to the branch, while an independent delay
instruction can move. The end is constrained by each instruction's placed row plus
external tail, and by minimum extent. The branch is repositioned to leave one
complete delay row.

Drag-down visits rows descending, upper then lower, trying later positions bounded
by table end, external tails and local users minus latency, then advances the
start using the remaining per-input earliest bounds. Both directions consult
physical placement/lock checks. Flexible-pipe operations prefer lower then upper
moving upward, and upper then lower moving downward. The port retains selected
pipes; vcl's alternative-pipe searches are not implemented.

## Raw factory modes and the port's protected endpoints

vcl's raw factories do not make every generated part fixed:

| Part | vcl mode |
| --- | --- |
| Entry | Movable submode 1 |
| Exit | Movable submode 1 |
| Prologue | Movable 1 unless the base raw loop is fixed mode 4 |
| Epilogue | Same choice as prologue |
| Kernel | Fixed 2 |

The fixed-part selector derives from base mode 4, so `reorder` permits motion in
raw prologues/drains while `noreorder` preserves them.

The port introduces a distinct `TableMode::RawLinear` only for eligible reorder
prologues/epilogues. It keeps synthetic Entry/Exit tables protected as a
conservative choice, which differs from vcl's factory mode. Their full
merging/termination lifecycle is not ported. vcl's Tidy routine computes fold
decisions in one traversal and applies them in another, while the port's
normal-part adapter has additional transitive empty-exit handling; enabling all
raw endpoints under that path would overfold some boundaries.

## Physical raw-motion bridge

`compile/raw_reduction.rs` runs after allocation. It first replays each
candidate's complete physical table without compression. Any replay failure or
latency/throughput/synchronization stall freezes that frame. Memory, modifiers,
observed hardware outputs, explicit hardware operands and ambiguous local/backedge
definitions also freeze it. Other raw/submode cases keep their rows.

For eligible frames the adapter builds a local `SchedulingProblem` using allocated
physical IDs, resolving internal copies to their actual unit and pipe metadata.
The constraints include:

- Actual RAW definitions, with the concrete consumer recovered from selected
  pair-read ownership and vcl's conditional VI sampling bias.
- Physical WAR and completion-order WAW across logical names sharing a physical
  register. Equal-row old-value reads require WAR constraints in both relevant
  directions, including lower-reader to upper-writer.
- Selected pipes, whole-register pair-write exclusion, execution units,
  throughput, incoming earliest bounds and outgoing tails.

Source-list branch barriers are not passed as data dependencies. The two pipes
read before either writes, so iterating source IDs with `from < to` would miss an
old-value lower read of an upper destination and could permit a wrong upward move.

```rust
let candidate = raw_cleanup::reduce(
    &physical_constraints,
    &selected_rows,
    &external_bounds,
    minimum_rows,
)?;
// Check the changed physical table again before installing it.
```

`raw_cleanup::reduce` validates identities, initial constraints and branch layout,
pulls eligible operations upward, shrinks the end, drags operations within their
bounds and rebases the start. Immovable operations keep their row and prohibit
rebasing. No extent growth is accepted. A full physical replay validates the
changed candidate; failure preserves the original frame. Allocation is not rerun
and register numbers are not hardcoded.

### Unobserved implicit FMAC flags

Re-lowering can recreate implicit MAC/current-status effects after pruning.
Treating every such field as an observed hardware dependency would freeze ordinary
MUL/ADD debris. The bridge projects those effects only for FMAC-unit instructions
whose hardware inputs are solely implicit `STAT_ALU_S`, whose hardware writes are
solely `MAC`/`STAT_ALU`, and whose affected writes have no actual component read
or synthetic-output observer anywhere in the analyzed region. Explicit hardware
operands, ACC, I, Q, P and other resources stay excluded.

When the live-MAC probe adds `FMAND` after the loop, its observed output prevents
the projection and the port's EPI1 keeps its ten rows rather than the two-row
unobserved layout. vcl can optimize more of this case; that is a documented
difference. D-marked frames are conservatively frozen.

## Outgoing tail is distinct from result latency

vcl's external-bound preparation resets four per-operation external bounds to
zero. An outgoing consumer contributes only when the selected-image check reports
an optimized image or a VSM owner. Future ordinary unscheduled UTA consumers
contribute no outgoing tail, and synthetic EXIT observations do not contribute the
operation-user bound either.

An ILW result with latency four can thus have **zero extra outgoing tail** during
ordinary scheduling. If its consumers are future ordinary streams and the branch
is independent, vcl's branch-delay exchange may put ILW into the branch delay row,
and the later consumer carries its own incoming debt from ILW's changed position.
The VI-only case needs no additional P/Q throughput tracking; vcl initializes that
register property from the table.

`ordinary_tail_separation` supplies this distinction to both delay eligibility and
minimum extent. Selected/raw or already scheduled users keep the conservative
outgoing bound; the fixed raw consumer control confirms the future-UTA exemption
does not apply to an already selected VSM user.

For an ILW moved to delay row 1 of a two-row branch frame, a directly targeted
ordinary LQ sees one guaranteed issue pair elapsed, leaving incoming debt three.
The resulting layout is eight pairs, including the conditional alternate
definition.

## Incoming distance follows CFG paths

Fixed raw layout stays protected against motion but still supplies timing. For a
one-row fixed IADDIU predecessor, one issue pair has elapsed at the next stream; a
latency-one result needs no extra leading NOP.

vcl's distance getters use **execution time**, not dispatch row:

```text
producer_to_end = logical_length - execution_tick - 1
consumer_from_start = execution_tick
```

The row replay records the execution timestamp after stalls. The path distance
helper sums intermediate stream lengths from a path key, excluding endpoints, and
its length helper can use selected VSM logical length or a positive unscheduled
UTA estimate.

`boundary::minimum_path_length` computes a conservative minimum over CFG paths
using caller-proven duration lower bounds. Unknown intermediates contribute zero.
Protected fixed/branch tables contribute their rows; ordinary compressed frames
contribute nonempty pairs; raw debris that can still move contributes one pair.
Producer remainder and intermediate distance are kept separate, so an untaken
physical block never contributes time at a join.

```rust
let elapsed = guaranteed_producer_remainder.saturating_add(shortest_path);
let incoming_debt = latency.saturating_sub(elapsed_as_u16);
```

The port does not reconstruct exact hardware execution clocks, all path keys or
all conditional-input marking. The raw bridge pins unknown/multihop inputs to
their selected row when it cannot prove an earlier time.

## Controls and remaining differences

`cleanup-raw-reorder` matches vcl's 27 pairs. Both five-row prologues become four
rows: MUL and VI decrement share row 0, the conditional issues at row 2, and row 3
remains its delay pair. The kernel keeps its five selected rows. EPI0 has a
three-cycle VF interlock and keeps its ten rows under the initial-stall gate.
EPI1's spaced MUL/ADD frame reduces to five latency-preserving rows, then final
compression emits two pairs with a three-cycle interlock. The separate EXIT stays
two rows.

`local/tools/probe_raw_boundaries.py` authors and validates 14 controls using
`-t1 -uBOUND`, alarm scale 1 and a 30-second wall timeout. All selected guests
completed with status zero. Hashes, flags and alarms are in
`local/evidence/analysis/raw-boundary-probes.json` (local, not tracked) and each
vcl directory under `local/evidence/oracle/`.

| Controls | vcl/port pairs | Result |
| --- | --- | --- |
| Reorder, final compression disabled | 30 / 30 | Exact normalized pairs |
| Reorder, normal search disabled | 27 / 27 | Exact normalized pairs |
| Fixed/noreorder | 37 / 37 | Exact normalized pairs |
| Paired old-value MTIR | 29 / 29 | Exact normalized pairs |
| ILW y/z/w boundaries | 8 / 8 each | Exact normalized pairs |
| Selected fixed raw ILW user | 6 / 6 | Exact normalized pairs |
| Fixed VI producer through branch | 8 / 8 | Exact normalized pairs |
| Known minimum two | 20 / 22 | Entry placement and endpoint lifecycle differ |
| Known minimum four, with/without `-C` | 17 / 19 each | Frozen drain retains extra termination rows |
| D-marked frame | 27 / 29 | Modifier-bearing frames remain protected |
| Observed MAC output | 28 / 37 | Hardware-output frame remains protected |

`-C` does not disable earlier raw motion, and `-L` does not erase explicit raw
stages or their reduction.

Tests: `raw_cleanup::reduce` has unit tests including a 128-frame deterministic
matrix checking identities, pipes, earliest/tail bounds, branch delay, actual
edges and throughput after movement and rebasing; path tests cover alternatives,
cycles, unknown duration, invalid graphs and overflow. Twelve unmarked kernels
execute vcl and port output over 64 states each (valid minimum-count promises,
both conditional paths, varied loaded VI values, nonzero low bits observed by the
old-value MTIR), comparing declared escapes and the whole memory array. Flags and
D behaviour are outside that logical model and are tested structurally or by pair
comparison. Validation records are in
`local/evidence/analysis/raw-boundary-validation.json`.

Not implemented: the hazard-broadcast mask/lock engine, alternate-pipe motion,
general memory/hardware movement, exact execution-time external bounds, raw
synthetic Entry/Exit merging and Tidy order. A live-through guard, blanket
leading-NOP deletion or forced register assignment does not substitute for them.
The 49 real-program acceptance outcomes remain a gate; whole-program hardware
behaviour and exact byte output are separate unfinished targets.
