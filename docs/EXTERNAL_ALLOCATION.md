# External allocation before local scheduling

vcl 1.4beta7 fixes cross-stream physical register assignments ("external
allocation") before most scheduling occurs. This affects both register numbers
and which schedules and moves are possible. Everything below about vcl is
inferred from vcl 1.4beta7 and confirmed with controlled observations; it is a
recovered interpretation, not proof of every runtime path.

## Stage order and state

vcl builds webs, runs scalar/copy cleanup, then performs external assignment.
Each stream starts in allocation state 1. Its twelve pool objects are visited in
pool-ID order, in physical stream-list order.

After that pass each external virtual operand is replaced by its assigned
physical register and the table advances to state 2. The choices survive because
the operands are already physical; they are not merely final packing
preferences.

Local assignment requires state 2, a selected VSM mode, valid locks and no
unscheduled instructions. It advances the state to 3. A local failure rejects a
scheduling candidate.

Normal-loop optimization also locally allocates its modulo candidate before
expansion. After expansion, reflow and pruning, vcl records the physical choice
as each external operand's forced base, and a further web pass preserves those
bindings. The port expands normal loops before its single web/late allocation
stage. Matching vcl for transformed loops would need assignment provenance
through scheduling, clones, binding and rebuilt webs; pinning the final expanded
graph is not equivalent.

## Register-reference masks

Per stream, vcl visits instructions and operands in descriptor order and creates
a reference on first encounter. Physical references are skipped by the list
iterators. Each reference carries:

| Field | Meaning |
| --- | --- |
| Register | Register identity |
| `xi` | Components supplied externally |
| `xo` | Components used externally |
| `i` | Components read locally |
| `o` | Components written locally |
| `t` | Components passing through the stream |

A reference is external when `(xi | xo | t) != 0`. External input/output are
subsets of local input/output respectively. A through-only reference need not
have a local instruction operand. Through state comes from the flow backtrace;
the separate hazard-broadcast byte is not `t`.

The pass selects unmapped virtual external references in list order. For each
one it resets an exclusion byte per physical register, then runs the
disjointness discard for **every** stream: the target is compared with physical
and already-mapped references of the same pool. Unassigned local virtuals do not
exclude physical candidates yet.

## External candidate policy

A disabled physical register (availability flag 0) has all component bits set in
the exclusion table. The scan goes through physical IDs upward and returns the
first whose exclusion byte is zero. It does not use the local packing score,
occupied-row priority or component-count cost. Any conflict bit excludes a
candidate.

```rust
for physical in 0..pool.size {
    if exclusions[physical] == 0 {
        return Some(physical);
    }
}
None // no spill fallback observed
```

Already assigned external references contribute to later exclusions, so
reversing two independently defined external values can reverse which gets the
lowest register. Register families are separate pools.

### Unscheduled and synthetic tables

For table modes 1 and 4, activity is the local input/output component union,
plus an extra bit if there is **any** local output:

```rust
fn activity(i: u8, o: u8) -> u8 {
    i | o | if o != 0 { 0x80 } else { 0 }
}
let a = activity(target.i, target.o);
let b = activity(other.i, other.o);
let conflict = (a & b) | (a & other.t) | (b & target.t);
exclusions[other.physical] |= conflict;
```

This is coarser than scheduled lifetimes. An external output and a forced input
with a common component conflict even if a later schedule would place the write
at the input's last-read row. Two outputs conflict via `0x80` even when they
write disjoint VF components in different instructions. Disjoint input-only
components can share a physical VF.

In the cross-stream pointer control, VI01 is a forced local input in its
defining stream, so the external pointer gets VI02. The join control also reads
forced VI02, so the external pointer gets VI03. Late branch-delay motion does not
make an already assigned VI02 available again.

The exclusion is per relevant stream, not a reservation of every ENTER register
for the whole program. A later external can reuse VI01 when the old VI01 value is
neither referenced nor live through its streams (the sequential control). A value
passing across an intervening stream still excludes its base (the through
control).

### Selected VSM tables

Mode 3 uses component ranges from the selected-range helper rather than the
unscheduled activity formula. These are dispatch-position summaries, unlike the
execution-clock getters used for external latency in
[RAW_BOUNDARIES.md](RAW_BOUNDARIES.md).

For each component:

- An absent component has sentinel range `[-4, -3)`.
- Pure through state (`t & !(i | o)`) has `[-1, 30000)` and a through marker.
- Otherwise start is `-1` for external input, or first local output if present.
- End is `30000` for external output or through state; otherwise it is the
  maximum of last local output plus one and last local input.

Two components conflict when their ranges overlap strictly, except when both are
pure-through, subject to a negative-end sentinel guard:

```text
!(a.pure_through && b.pure_through)
&& (a.end >= 0 || b.end >= 0)
&& a.start < b.end && b.start < a.end
```

Read endpoints are exclusive: a first-row input `[-1,0)` need not overlap a new
output beginning at row 0. This is why a selected raw defining stream can reuse
VI01 where an ordinary UTA defining stream uses VI02.

A whole-register issue-row guard also applies. If both references have local
outputs, the simultaneous-write check scans dispatch pairs; any pair writing both
references adds `0x80`, regardless of component masks. Component disjointness
alone does not permit two writes to one physical VF in a pair.

## Web ordering and bindings

vcl's instruction-add step visits descriptor operands, usually destination
first, and lazily creates strands for the operand and its reaching definitions.
The gather step scans strand creation order, merges matching forced bases through
the physical register's cache, then uses that base or creates a new virtual
register.

The port's web connectivity handles reaching definitions, component joins,
bindings and update backlinks, but eagerly creates effective read operands before
write operands, so its internal representative order differs from vcl's creation
order. External reference discovery therefore follows concrete instruction
operands rather than sorting web IDs. Lazy strand ordering is not reproduced.

vcl preserves assigned physical operands through later webs. External references,
or operands in fully allocated state 3, must already be physical, and an existing
forced base must agree. The physical operand is copied into its forced-base field
before the next web reconstruction.

## Generated moves are a different subsystem

Placement repair first tries a placement and setter motion. If the physical
value/lock failure bit `0x100` persists, vcl's move getter inspects eligible
VI/VF input locks and value ages, creates a fresh transfer register, retargets
eligible pending locks and prepends the transfer to the scheduler's todo list. It
supports backtracking.

The move pool creates and recycles separate VI and VF move records with component
lock lists. Three instruction fields hold the move record, the abstract/base
descriptor and the resolved descriptor. Resolution selects integer 179 or vector
181/182. Undo restores retargeted locks and pooled state.

The emitter requires that provenance. The port's proven-copy path implements the
IADDIU-zero/MOVE/duplicate-source upper encodings, but not fresh move creation,
lock retargeting or rollback. Inserting arbitrary copies on local allocation
failure would not implement this mechanism.

## Controlled observations

Fourteen independent controls were run against vcl at alarm scale 1 with
`-t1 -uEXT`. Thirteen compile; the single-VI/two-external exhaustion case rejects
with status 255. Sources, outputs and statuses are under
`local/evidence/oracle/external-*` (local, not tracked), summarized in
`local/evidence/analysis/external-allocation-probes.json`.

They cover same-stream versus cross-stream allocation, destination discovery
order, branch joins, sequential reuse, through values, restricted pools,
exhaustion, disjoint VF output lanes, and fixed/reorderable raw definitions. The
ordinary pointer uses VI02, the join pointer VI03, and the selected raw pointer
reuses VI01. Local outputs still use the existing packing stage.

## Port implementation

`crates/vcl/src/allocator/external_allocation.rs` is the separate first-free
allocator. It takes a pool's component count, boolean availability, ordered
reference tables and already assigned physical anchors, and returns new
assignments as owned state, leaving the input unchanged on failure. It implements
both UTA interference and the selected VSM component-range and same-row rules;
`component_range` reconstructs the dispatch summaries.

`compile/external_assignment.rs` prepares ordinary unscheduled summaries after
web construction and before scheduling. Local masks come from effective accesses;
external masks from actual reaching definitions/users and EXIT observations;
pure-through components from live-in/live-out state without a local write.
References follow parsed descriptor operand order, and references without a
concrete operand are appended deterministically. Forced/physical anchors compete
only in tables where they have a reference. Newly assigned bases become pins for
subsequent local allocation.

The scheduled time-raster allocator keeps its packing score and local priority
policy. External failure is reported before local scheduling retries; a local
schedule cannot change the earlier exclusion decision. No spill or generated move
is inserted on exhaustion.

### Differences and limitations

- The port keeps logical web IDs while fixing their physical bases; vcl rewrites
  operand pointers immediately. Early physical identity effects on scheduling and
  lock mutation are not reproduced.
- Selected/raw regions, transformed loops and partial VF availability keep the
  existing allocation path. Physical availability is boolean in vcl; its
  relationship to the port's component-mask policy must be preserved before
  extending the external gate.
- VI and VF assignments are independent across pools. The port visits each family
  across all tables, while vcl interleaves pool callbacks per stream, so competing
  failures in different families or streams can yield a different first
  diagnostic. Exact diagnostics are not claimed.
- The port pads a local EXIT table before concatenating streams, whereas vcl can
  place E across the combined frame. Four integer controls (two, reversed,
  through, sequential) therefore agree on allocated non-NOP instructions and
  operands but differ in termination NOP/E placement.
- Not implemented: transformed-loop pin propagation, vcl's lazy web ordering,
  physical conflict moves with undo, and exact diagnostics.
- `external-vf-two` and `external-vf-enabled-pool` emit no E bit in vcl despite a
  successful exit. The port matches that, and the strict whole-program execution
  model (which rejects running past the last pair without E) excludes them.

### Control results

| Controls | vcl/port pairs | Result |
| --- | --- | --- |
| VI cross/local/join | 3/3, 3/3, 5/5 | Exact |
| Restricted VI pool | 3 / 3 | Exact |
| VF lanes/two values/restricted pool | 4/4, 3/3, 2/2 | Exact |
| Fixed/reorderable raw defining streams | 3 / 3 each | Exact through the raw path |
| VI two/reversed/through/sequential | 3 / 4 each | Termination layout differs |
| One-register external exhaustion | none, status 255 | Both reject; diagnostic bytes differ |

Tests: the pure-allocator tests in `crates/vcl/src/allocator/external_allocation.rs`
cover order, disabled candidates, forced anchors, inter-stream reuse and through
state, component/output exclusions, selected last-read boundaries, pure-through
sentinels, same-pair writes, malformed input and exhaustion. Bridge tests in
`compile/external_assignment.rs` cover descriptor ordering, Xi/Xo, through-only
references, EXIT classification and lifecycle gates. Validation records are in
`local/evidence/analysis/external-allocation-validation.json`.
