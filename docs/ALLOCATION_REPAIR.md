# Allocation-aware scheduling and write-port repair

This chapter describes how the port's allocator and scheduler interact, based on
vcl 1.4beta7. See [ALLOCATION.md](ALLOCATION.md) for the allocation model and
[ALLOCATION_INTERVALS.md](ALLOCATION_INTERVALS.md) for lock evidence.

## Component intervals and stream holes

The port does not reduce a web to one global `[first,last)` span with a union
component mask; that would invent interference in unrelated stream ranges and
reserve dead components for the whole span. vcl builds per-output, per-component
intervals, as described in [ALLOCATION_INTERVALS.md](ALLOCATION_INTERVALS.md).

`crates/vcl/src/allocator/lifetime.rs` constructs owned rasters from scheduled
pairs and CFG live-out sets. Each table is walked backwards:

```rust
reserve_components_live_after_this_pair();
for write in both_pipes.outputs() {
    reserve(write.register, row, write.mask | EXCLUSIVE);
    live_before.remove_components(write.register, write.mask);
}
for read in both_pipes.inputs() {
    live_before.add_components(read.register, read.mask);
}
```

The dependency graph's live-out includes output declarations and loop-through
components, which stay reserved through padding and intermediate streams even
without a local read. Other components can have holes. Reads are exclusive
interval endpoints, so a destination may reuse a source location in the same pair
as its last read. Both pipes' reads precede either write in forward execution, so
the reverse pass processes all writes before adding either read. It uses the
scheduled operations' actual effects, keeping lower raw inputs even when the upper
NOP is omitted from emitted rows.

This backward construction is the port's own implementation. It recovers
component and path reservations from the compiler's data flow; it is not claimed
to reproduce every vcl lock-list mutation or loop sentinel.

## Issue-row exclusivity

vcl records absolute and folded issue position in each lock, and its occupancy
builder sets bit `0x80` at the folded row for output locks. Two lock-creation
routines create these for ordinary nonconstant outputs in either pipe.

Every ordinary output in the port gets this bit in its occupancy row. Two webs
therefore cannot share one physical VF when both write in the same pair, even for
XYZ and W (both results have latency four and complete together). Disjoint live
component sets can still share that VF at other rows. The issue-row lock suffices
for this; allocation intervals are not extended to completion cycles.

The packing cost and tie-breaking are unchanged by this. The timing replay and
compression guard stays active and rejects invalid input before compression; see
[TIMING_COLLISION.md](TIMING_COLLISION.md) for the trace, probes and latency
inventory.

## Rejecting schedules that cannot allocate

vcl validates local allocation as part of candidate acceptance rather than
accepting a shorter schedule and failing the source when allocation fails.

The port keeps this distinction with a typed internal allocation failure. It first
tries the ordinary best schedule. If allocation fails, it tries the eligible
linear strategy table in reverse original order, fully allocating and validating
each attempt, and selects the shortest successful result. A failed attempt cannot
append partial output. Dumb mode keeps its single-strategy behaviour. If no attempt
succeeds, the allocation failure is returned. There is no spill allocator or guard
bypass.

The retry is whole-region and compares completed pair counts, whereas vcl has
separate external and local allocation phases and per-stream optimization. It is
therefore a supported acceptance policy, not proof of vcl's exact candidate
ordering. Exact register choices, moves and scheduling remain differences.

## Incoming bindings and an empty raster

A source read in its first pair can have an empty local raster because its
last-read endpoint is exclusive. This must not fabricate a VF00 incoming value.
A fresh bare `in_vf source` is rejected during interface decoding; a physical name
or existing source binding is valid, and an earlier `source(vf01)` binding
persists across regions and can be reused by a later bare declaration. The port
resolves that existing binding or returns vcl's missing-base error, and does not
reserve a fake component row to hide an undeclared physical interface.

vcl's local packing primitive can technically fit an empty candidate against zero
availability, while its external allocator separately excludes disabled physical
registers. The port keeps the two separate and does not change the generic packing
cost. The binding probes are in [ALLOCATION_INTERVALS.md](ALLOCATION_INTERVALS.md).

## Verification

Every corpus input's vcl acceptance is checked against
`local/evidence/corpus/original-acceptance.tsv` (local, not tracked). Of the 49
corpus inputs, 44 compile and five are rejected, matching vcl. Acceptance does not
show that schedules are semantically or cycle correct; normalized schedules mostly
differ from vcl's.

Tests cover interval holes, component separation, simultaneous writers, last-read
reuse, loop invariants, constant registers, malformed schedules, acceptance
boundaries and inherited input bindings. Two bounded execution comparisons check
component reuse and distinct-register simultaneous writes over 64 states each,
under the existing numerical/timing restrictions of that model.

Evidence (local, not tracked):

- `local/evidence/analysis/allocation-interval-probes.json`: nine vcl allocation/binding runs.
