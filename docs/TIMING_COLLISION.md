# Physical VF write-port collision

Two VF writes issued in the same row (one upper, one lower) to the same
physical register collide on the register's write port, even when their
component masks are disjoint. The allocator must not pack such writes into
one register; the timing replay rejects a schedule that does.
[ALLOCATION_REPAIR.md](ALLOCATION_REPAIR.md) covers interval construction,
exclusive issue locks and candidate retries.

## The rule

SCE's VU User's Manual §3.4.3, "Priority for Writing to a Register", states
that an upper/lower simultaneous write to the same register discards the
lower result. This holds at register granularity even when the destination
fields differ. Lower VF-writing instruction remarks (for example LQ) repeat
the rule. Ordinary VF read interlocks remain component-specific (§3.4.1); the
write-port rule must not broaden an unrelated XYZ read into a hazard against
a pending W write.

A collision looks like this in the replay's terms: an upper operation writing
XYZ (latency 4) and a lower operation writing W (latency 4) in one issue row
to the same physical register both complete in the same cycle, and the replay
reports `WriteCollision { first, second, register, cycle }`. Keeping the
component masks disjoint is insufficient; the fix is to prevent the invalid
physical assignment or reschedule the writes, not to compress around it.
The public-corpus program `openvcl__test_fixtures_fast` is the regression
reproducer.

## Original scheduler and allocator behaviour

Inferred from vcl 1.4beta7: its occupancy tracking records ordinary component
liveness and also an extra whole-register lock at the issue row of each
component register lock. For a linear table the lock row equals the ordinary
issue row; for a loop it is the ordinary row modulo the period. So this is an
**issue-row** reservation, not a separate completion-cycle interval.
Candidate packing checks overlap including this lock; component counts and
packing scores exclude it. Two values that both need the lock in the same
issue row cannot share a physical register, even if their ordinary component
bits are disjoint. See [ALLOCATION.md](ALLOCATION.md) for the packing policy
and lock intervals.

Every VF-writing operand in vcl 1.4beta7's instruction table has latency 4:
upper FMAC, conversion, MAX/MINI, ABS and OPMSUB, and lower RGET/RNEXT, MFP,
MFIR, MOVE/MR32, LQ/LQI/LQD. The abstract VMOVE forms also have latency 4.
Generalized new-syntax FMAC entries take latency 4 for a VF destination and
latency 1 for an ACC destination. ACC is a separate hardware register and is
not part of VF allocation.

Consequently simultaneous VF completions correspond to simultaneous VF issue
rows: reserving the whole-register lock on each VF write issue row is enough,
and no completion-time allocation policy is needed. Same-pipe writes cannot
occupy one issue row; two writes in one row are the upper/lower pair that need
exclusion.

The port's timing replay keeps its completion-cycle guard as an independent
validation of the final physical schedule, so a later transformation or a
future instruction-latency extension cannot silently introduce a collision.

## Original-binary probes

Three small programs run vcl 1.4beta7 (SHA-256
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`)
with `-t1 -uCOLLISION` and alarm scale 1. They all exit 0. Inputs are in
`local/evidence/inputs/timing-collision-*.vcl`; exact stdout/stderr and runtime metadata
are in `local/evidence/oracle/timing-collision-*/`; a compact hash manifest is
`local/evidence/analysis/timing-collision/probes.json`. No openvcl implementation or
output was used.

`timing-collision-same-web` writes XYZ and W of one escaping `joined` value,
bound to VF02. The original uses consecutive issue rows:

```text
add[E].xyz VF02,VF01,VF01    NOP
NOP                         mfir.w VF02,VI01
```

`timing-collision-packed-webs` instead binds the two independent escaping
values to VF02 and VF03. The original can pair both writes safely:

```text
add[E].xyz VF02,VF01,VF01    mfir.w VF03,VI01
NOP                         NOP
```

`timing-collision-internal-webs` creates independent XYZ and W temporaries and
stores them to separate memory locations, without externally pinning either
destination. The original reuses VF01 for both values and separates their issue
rows:

```text
add.xyz VF01,VF01,VF01       NOP
NOP                         mfir.w VF01,VI01
NOP                         sq.xyz VF01,0(VI00)  ; latency stall 2
NOP[E]                      sq.w VF01,1(VI00)
NOP                         NOP
```

The port may choose different but equally valid registers for the internal
case (different VF registers, writes paired); these probes establish valid
choices, not exact allocation/scheduling equivalence.

## Regression coverage

The timing tests check the exact collision fields, and that `compress`
rejects an invalid baseline rather than trying to optimize it into
acceptance. Further tests cover:

- Same-pair XYZ/W writes to different physical registers remain valid.
- Consecutive-pair XYZ/W writes to one physical register preserve both results.
- Whole-register write-port exclusion does not broaden component read interlocks.

The timing module detects collisions; the compiler's allocation layer
installs the extra lock at each VF write issue row, preserves component
liveness, and retries allocation/scheduling.
