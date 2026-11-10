# Original final dispatch reduction

This chapter describes the final physical-table passes in vcl 1.4beta7
(inferred from vcl 1.4beta7).
[Scheduling](SCHEDULING.md) explains which cap instructions synchronize P,
Q, and flag outputs and how their destination registers are selected.
[Raw boundaries](RAW_BOUNDARIES.md) covers the earlier movement and
minimum-length construction.

## Pass order and eligibility

When the final-reduction flag is set, the cap insertion pass runs first. A
positive cap count triggers a preliminary dependency rebuild. A nonnegative
count then reaches the compression pass. `-C` clears the flag and bypasses
both transformations. The normal final dependency rebuild and stall loading
still run afterwards.

Both passes visit eligible streams in list order. A stream is eligible when
it has an installed table with mode 3 and submode 1 or 3. External state is
prepared with mode zero, the table operation runs, and the stream is marked
done. Empty tables return before minimum-length checks. Nonempty tables
reject a negative table minimum or a physical extent already below that
minimum. After a successful cap insertion, the table is relinked. If the
relink fails, the original logs a diagnostic and returns zero even though the
cap was inserted.

## Recovery row and timestamp edits

A row has two instructions, a skip mark, and a signed timestamp. Timestamps
are seeded relative to the table start, and occupied rows are checked against
the instruction's recorded issue tick. For each empty candidate row, the
compression scan gathers the in-flight writes across the twelve resource
pools. The recovery row is the earliest row where a tracked writer or
interlocked reader can absorb the lost cycle. For a next P writer the scan
uses candidate time plus the remaining time minus one. For Q it uses
candidate time plus remaining time. For an interlocked user it uses the
latter threshold for tracked writes with latency greater than one.
Before marking the row skipped, the pass performs these checks.

- For each in-flight write, inspect its first dependency user (skipping
  leading links the pass ignores). If both operands have the relevant flag
  clear and the user precedes recovery, its advanced time must still reach
  the ready time.
- When the next row precedes recovery, reject if the early-read check holds
  for the next row. That check scans lower-pipe operands marked as
  early-sampled, including the early-sampled IALU conditional source, and
  folds the register's pending counters.
- Scan shifted instructions in `[candidate + 1, recovery)` for protected
  operands and check advanced issue times against the maximum of two signed
  per-instruction extent bounds. A bounded-shift output/read branch also
  compares `advanced_ready = old_row_tick + latency - 1` with the recovery
  tick; when it is smaller, it looks for an exact matching tick in the suffix
  beginning at recovery and tests for a read there. No runtime case has
  exercised this branch, so the port's behaviour there is unverified.
- If there is no recovery inside the table, scan occupied rows from table
  start to end. The change is vetoed when
  `max(tail_c, tail_d) < logical_len - 1 - candidate_tick`, where `tail_c`
  and `tail_d` are a second pair of signed per-instruction bounds. The right
  side uses the same candidate time for every instruction. The signed
  inequality is reproduced as is.
- A whole-tail shortening must preserve the table minimum. It must leave at
  least two logical cycles when the stream end kind is anything other than
  1. The last physical row has its own gate: it is eligible only with end
  kind 1 and branch subtype 0.

After accepting the candidate, the pass decrements the timestamps
in the half-open range `[candidate, recovery)` by one. The row remains
in the table at its physical index. Only the absence of recovery inside
the table reduces the table's logical length.

```rust
// Structural sketch after all vetoes pass.
rows[candidate].skipped = true;
for row in &mut rows[candidate..recovery.min(physical_end)] {
    row.timestamp -= 1;
}
if recovery >= physical_end {
    logical_len -= 1;
}
```

The row replay skips a marked row without issuing it. For a normal row
it checks that the replay issue time matches the CPU tick and the
stored timestamp plus one. It rewrites the instruction's recorded issue
tick from the actual post-stall tick. The emitter later walks the original
physical row indices, so a marked row can consume a stream flag
without printing it. The paired `external-vf-two` captures demonstrate
the visible E-bit consequence of the final pass
([output flags and original captures](OUTPUT_FORMAT.md#dispatch-formatting-flags-and-statistics)).

## Synchronization preparation for raw tables

The raw-table wrapper runs the synchronization preparation after linear
reduction, then validates moves and reinstalls the table. This
preparation expects period zero. It scans forward and backward, taking
an instruction only when its row has exactly one occupied pipe.

The movement search runs from the legal earliest or latest bound
back toward the instruction's old row. It ordinarily tests
one-occupied-slot candidates before removing and reinserting the
instruction. An interlocked producer dependency can make the upward search
try the exact earliest row regardless of occupancy. The reverse scan leaves
an instruction at that producer-driven earliest row. The latest bound pins
branches and protected operands, and otherwise starts at the table end minus
one minus the maximum of the signed extent bounds. The raw oracle corpus
measures the combined final result.
