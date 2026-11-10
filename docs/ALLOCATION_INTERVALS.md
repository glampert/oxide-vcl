# Allocation intervals, component holes, and path liveness

The compiler integration is documented in
[ALLOCATION_REPAIR.md](ALLOCATION_REPAIR.md). The rules below were inferred from
vcl 1.4beta7 and checked with the probes listed under "Probes".

vcl does **not** compute one whole-program convex hull per source variable. It
builds a byte raster from independent component definition locks within an
individual scheduled stream/table. Different definitions and components keep
separate intervals and holes. External assignment checks interference separately
in each stream, while through components stay reserved on paths that must
preserve them.

## Lock positions and their provenance

An instruction's schedule position has these fields:

| Field | Meaning |
| --- | --- |
| instruction absolute line | Signed 16-bit absolute issue line |
| instruction pipe | Pipe 0 or 1 |
| instruction loop iteration | Loop iteration |
| instruction folded row | Signed 16-bit folded issue row |
| component lock absolute line | Copied absolute issue line |
| component lock folded row | Copied folded issue row |

The lock-position step copies the instruction's absolute line and folded row into
the lock. A linear schedule requires them equal; with period `P` it requires
`absolute % P == folded`. The folded row is **not** a completion time.

For each nonconstant ordinary read operand vcl sets positions and adds input
locks. A separate pass over ordinary output operands sets positions and inserts
output locks. Component masks select which individual locks are visited. A
rebuild step recreates locks for already positioned instructions, and an update
step revises them when positions move.

The special register object used as a constant is excluded from those walks.
Every other ordinary direction-2 output receives this treatment, in either pipe;
cross-pipe writers get no extra lock. Input locks store positions too but are
linked as users of a producer; they are not the outer output-lock list that the
raster builder rasterizes.

An external-input owner lock has owner kind 2 and positions `-1`. An
instruction-owned lock has owner kind 1. An externally visible output is marked
separately on the producer lock.

## One definition interval

For one output/definition lock the range is computed as:

```rust
fn definition_range(lock: &Definition, period: i32) -> Range<i32> {
    let start = match lock.owner {
        ExternalInput => -1,
        Instruction(inst) => checked_issue_line(inst, lock),
    };
    let mut end = if lock.external_output {
        if period == 0 || start < 0 { 30000 }
        else { start + period - start % period }
    } else {
        -1
    };
    for user in lock.users.iter().filter(|user| user.kind != 2) {
        let issue = checked_issue_line(user.instruction, user);
        assert!(issue > start);
        end = end.max(issue);
    }
    if end < 0 { end = start + 1; }
    start..end
}
```

`checked_issue_line` verifies agreement between the instruction's absolute line
and its lock's absolute line and requires a valid pipe. User kind 2 is skipped.
An externally visible producer starts with `end` at a sentinel or period boundary
before its user lines are considered.

A local definition at issue 2 with last use at issue 7 occupies `[2,7)`, **not**
`[2,8)` and not `[write_completion,7)`. With no ordinary users or external
output, `[start,start+1)` gives a dead write a nonempty reservation. An old
value's last read and a new value's first write may therefore occupy the same
issue pair in one physical register.

This helper does not use latency, throughput or readiness statistics. A separate
routine computes `issue + latency`, input lead times and warnings about use
before readiness, and instruction placement uses those. Extending every
allocation interval to completion time would be a new, more conservative policy,
not a translation of vcl.

## Raster construction

The raster builder clears 4096 temporary bytes, walks selected register
components, and takes each component's current lock list. It iterates **every
output lock**; each contributes its own interval, not the minimum start and
maximum end of the list.

For each interval `s..e`:

1. A negative `s` is allowed only for an external-input owner. In a linear table
   use zero. In a loop use zero if `e == 30000`, otherwise `(e / P) * P`.
2. For the sentinel `e == 30000`, use the active table end in a linear table; in
   a loop use `s + P - s % P`, the next boundary after `s`.
3. If the lock's folded row is nonnegative, set bit `0x80` at that row.
4. For each integer `t` in `[s,e)`, set the component bit at row `t` (linear) or
   `t % P` (modulo).

The `0x80` row is **one issue row**, not every row of a latency window. Every
ordinary output contributes it, including a partial VF writer. Two virtual values
may share disjoint VF components throughout overlapping lifetimes, but two
separate writes cannot get the same physical register at the same issue row just
because their masks are disjoint. Packing checks this high bit along with the
component bits.

Repeated component occupancy within one candidate is normally an internal overlap
error. vcl permits it when the lock list's first special flag is nonzero, or when
the period is nonzero and its second special flag is nonzero. These flags are
separate from testing a candidate against another physical register's occupancy
and must be kept when handling special/through/modulo cases.

## Table boundaries and control-flow paths

Local allocation runs on **one** scheduled/VSM table. The pool allocator resets
occupancy, installs that table's forced mappings, then builds and packs its
remaining virtual registers. Its instruction scan and lock lists belong to that
table.

External mapping is a separate pass over all streams (see
[EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md)). It chooses a physical mapping
for an external register after running the disjointness discard on every stream:

- If the candidate has no register reference in a stream, that stream
  contributes no exclusion.
- VSM tables compare per-component intervals from the selected-range helper using
  strict half-open overlap, `a.start < b.end && b.start < a.end`.
- Other supported table kinds use external-input, output and through masks
  directly, not the distance between blocks in emitted-file order.
- A shared-output-row check (simultaneous-write check) adds the `0x80` exclusion
  bit when both values are written in a common raw row.

The selected-range helper handles a component in the through mask but not the
ordinary input/output masks by returning `-1..30000` and a through flag. This
preserves a value on a path where it is neither read nor written locally but must
reach a later user. Removing false convex-hull interference must not remove these
through reservations.

vcl first forms register webs and merged strands; a source spelling is not a
sufficient definition identity. A reconstruction may keep one mapping per
connected web across blocks, giving it a **set of per-block, per-component raster
spans**. Linear concatenation is a storage convention but must not fill unrelated
blocks between those spans.

## Probes

All six allocation probe sources are under `local/evidence/inputs/` (local, not
tracked); their copied source, stdout, stderr, hashes, exit status and emulator
metadata are under `local/evidence/oracle/<name>/`. Each enables only VF03 as an
allocable temporary and uses fixed physical input/output registers elsewhere.

| Probe | vcl result | What it establishes |
| --- | --- | --- |
| `alloc-interval-components` | 0 | Separate virtual x and y temporaries with overlapping live spans both map VF03. |
| `alloc-interval-overlap-reject` | 255 | Making the second temporary x requires a second register; vcl reports one unallocated local. |
| `alloc-interval-paired-writes-reject` | 255 | Separate x and y temporary writes in one upper/lower pair cannot both use VF03. |
| `alloc-interval-read-write-boundary` | 0 | Final upper read of old VF03 and lower write of new VF03 share the same pair. |
| `alloc-interval-branch-reuse` | 0 | Distinct temporaries in mutually exclusive arms both map VF03. |
| `alloc-interval-through-reject` | 255 | A value defined before a branch and used after its join reserves VF03 through an intervening arm, leaving no register for that arm's temporary. |

The component probe emits:

```asm
add.x     VF03,VF01,VF02    ; first virtual value, x
mul.y     VF03,VF01,VF02    ; second virtual value, y
nop
nop
add[E].x  VF04,VF03,VF01    ; final x use
add.y     VF04,VF03,VF02    ; final y use
```

The read/write boundary probe emits this pair at row 4:

```asm
add[E] VF02,VF03,VF01       move VF03,VF04
```

vcl accepts it, then warns about the next pair using the MOVE result before its
latency is ready. This establishes allocation boundaries, not safe hardware
timing. Likewise the branch-reuse probe's evidence is the chosen VF03 mappings,
not semantic correctness on both paths. The through probe's diagnostic names
`VF03.XYZW - VF03 -THR` while the local `scratch_temp` stays unallocated across
all seven attempted candidate schedules.

Names ending in component suffixes such as `tx` and `ty` are parsed by the
component-suffix grammar and do not reliably denote independent virtual names;
the probes use `temp_one`, `temp_two` or other names.

## Reconstruction model

Keep mapping identity, control-flow liveness and raster storage separate:

```rust
struct WebAllocation {
    web: WebId,
    blocks: Vec<BlockOccupancy>,
}
struct BlockOccupancy {
    block: BlockId,
    component_intervals: Vec<(Component, Range<IssueRow>)>,
    exclusive_issue_rows: Vec<IssueRow>,
}
```

For ordinary linear instructions, a backward walk initialized by the block's
live-out component masks recovers occupancy. Reserve values live *after* the issue
row; kill written components and add components read from the old state. Set
every ordinary write's issue-row high bit and keep dead writes nonempty. Incoming
values needed only by the current pair need not occupy that pair's mask, which
gives the observed last-read/new-write boundary. CFG through masks must
initialize and preserve intervals without a local textual use. Pair reads are
collected before writes.

This walk is a modelling suggestion, not vcl's algorithm. It still needs special
handling for modulo schedules, special read modes, merged webs, forced mappings
and the overlap-permission flags. Do not substitute invented completion locks or
assume every user is an ordinary same-iteration read.

## Unforced ENTER operands and empty row-zero ranges

Probes `alloc-enter-unforced-move` and `alloc-enter-unforced-add` test an unbound
input with only VF02 allocable:

```asm
.init_vf vf02
--enter
in_vf source
--endenter
move dest,source             ; alternate probe: add dest,source,source
--exit
out_vf dest(vf02)
--endexit
```

Both exit 255 during decoding, before allocation, with `can't figure out which
base register to map source to` on the `in_vf source` instruction. An unbound
virtual ENTER input is therefore not an automatic-allocation request, even when a
temporary register is available.

The operand decoder's rule: without an explicit parenthesized base it uses the
virtual register's **existing** physical mapping; an actual physical register
binds to itself; it errors when the register is virtual and neither binding
exists.

`alloc-enter-inherit-binding` shows the existing mapping survives an ENTER/EXIT
region boundary. Its first region declares `in_vf source(vf01)` and copies to
VF02; its second declares bare `in_vf source` and copies the same way. vcl exits
zero and emits two separate E-terminated regions, each using
`max[E] VF02,VF01,VF01` followed by a NOP pair. A reconstruction must not require
a new explicit base in every ENTER region. This is a source-decoding rule,
independent of allocator lifetime.

An incoming value read only at row zero has an empty `[0,0)` range in the raster
model. The generic local packing cost checks component availability only where
the candidate raster has bits, and has no unconditional physical-ID-zero
exclusion, so an empty candidate can pass it with availability zero. The external
allocator, however, first pre-excludes every physical register whose availability
flag is clear, and then picks only an ID with exclusion byte zero. The unbound
ENTER probes cannot show a reachable mapping to VF00. Constant-resource exclusion
and binding resolution must stay explicit invariants; an empty raster must not
turn an unbound source into the constant VF00.
