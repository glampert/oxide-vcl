# Dependency analysis and instruction scheduling

This describes vcl 1.4beta7 (SHA-256
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`) as
inferred from the supplied ELF unless a run of the original is named. No
openvcl implementation or behavior description was used.

vcl is substantially more than a two-column instruction packer. It
reconstructs component-level value flow, tracks issue and availability times
separately, schedules virtual and physical resources together, can synthesize
register moves, tries several priority heuristics, backtracks when tentative
placements fail, builds modulo loops, expands them into
entry/prologue/kernel/epilogue/exit streams, and finally replaces some
explicit empty issue rows with synchronization instructions and hardware
stalls. Exact output equivalence requires all those stages.

## Objects and time coordinates

Each instruction (inferred from vcl 1.4beta7) carries: its operands
(component mask, read or write direction, availability latency, blocking
mode); allowed issue pipes (upper, lower, either); a functional-unit pressure
set; a loop side-effect category (0 none, 1 store, 2 irreversible/special); a
memory kind (none, load, store); throughput; order/barrier flags; its original
sequence number; and its position (absolute issue line, pipe, loop iteration,
folded row). Each stream owns an input table, a selected optimized table, a
current work table, a final emit table and a saved work table. A table has a
source-order list, a to-do list, a done list in insertion order, a candidate
list, one dispatch entry per issue row, and a start, end and loop period.

Absolute and folded lines relate as follows:

```rust
fn position(absolute: i32, period: i32) -> (i32, i32) {
    if period == 0 || absolute < period {
        (absolute, 0)
    } else {
        (absolute % period, absolute / period)
    }
}
```

The original stores the absolute and folded lines as signed 16-bit values and
the iteration as a byte. It uses 30000 as an impossible-position sentinel. It
keeps the scheduling start, end and period as process-global state and checks
`end-start == period` for a finite modulo table. The port passes an owned time
context explicitly instead.

Do not conflate issue row, processor cycle and modulo row. The emulator
assigns an actual processor timestamp to every emitted issue row, including
extra cycles spent on stalls. The final empty-row reduction can shorten the
emitted program without shortening the required execution time.

## Pass orchestration

The relevant order of the main driver:

1. Parse/build streams; expand already specified raw loops, fix pass-throughs,
   chase dependencies, and prepare binding/allocation.
2. Prune to a fixed point, rebuild dependencies, normalize/prune abstract
   moves, then externally allocate.
3. If global loop generation is enabled, initialize immediate fixups, gather
   stats, optimize eligible loops, expand scheduled loops, and redo flow,
   immediate, pruning, binding and dependency passes.
4. Normalize moves again; prepare raw/synthetic streams and optimize
   unallocated linear streams.
5. Rebuild dependencies and merge eligible adjacent streams.
6. Normalize moves, relax selected raw tables, rebuild dependencies, and do a
   new reduction/validation pass.
7. Tidy control-flow/end emission, rebuild dependencies; optionally add caps
   and compress empty rows.
8. Final dependency check, load accumulated stalls into dispatch entries, and
   emit source text.

Move normalization optionally collapses instructions, detects abstract moves,
rebuilds flow, runs move pruning (a no-op that always reports success in this
version), and rebuilds flow again. Recomputing analyses after each structural
edit is a deliberate part of the design.

## Component-level flow and dependency recovery

The dependency pass distinguishes internal analysis failure from input
hazards and can downgrade remaining final-pass hazards depending on its
arguments. It:

* resets processor state, stream flow information and flow sequence numbers;
* emulates every stream, linking readers to setters independently for each
  register component;
* builds memory/order dependencies;
* computes static chain/resource statistics;
* traces external readers and throughput writes over predecessor streams,
  builds source/target trace pairs, and recomputes statistics.

Each operand's direction, component mask, latency and blocking mode takes
part in the analysis. Hardware zero registers are excluded: a dependency is
never built on a syntactic mention of `VF00` or `VI00`.

The emulation order for a pair of simultaneous instructions:

1. Inspect both instructions' reads and writes and their hardware readiness.
2. Determine the maximum required latency/throughput stall and advance
   simulated processor cycles accordingly in raw mode. An unscheduled
   instruction that would stall is an error, because dependencies should
   model it before scheduling.
3. Link all reads to the previously pending/completed writer of each
   component. Reads without a local writer become external-input bits.
4. Only after both instructions' reads are processed, install their writes.
   Nonblocking writes enter a small pending-output pool; blocking writes flush
   previous pending outputs. Record per-component latency and throughput.
5. Advance the processor once for the issue itself.

So reading and writing in one bundle does not mean the read sees the bundle's
new value.

A dependent integer-ALU conditional branch is special. If the emulator
detects that its marked branch read needs an internal latency stall, it
reports that the target behavior is not guaranteed and fails (`ialu op too
close`). This is an input restriction; vcl does not insert NOPs around every
such case.

### Memory and explicit ordering

The memory-dependency pass rebuilds forced pre/post dependency lists per
instruction. It scans backwards from each instruction and adds an edge when:

* a current load/store must follow an earlier store, or a current store must
  follow an earlier load;
* their memory lane masks intersect, and memory mode/tag/effective-address
  tests indicate aliasing;
* explicit matching-order flags require serialization.

An instruction with memory tag text but no memory/order matching criterion is
an error. An unnormalized instruction-barrier flag is also an error: the
diagnostics demand an earlier `--barrier` fixup. Three further order-flag bits
supply load, store and special ordering classes. Address/tag disambiguation
has two distinct tests; a frontend must keep that distinction rather than
declare different textual addresses independent.

The pass also tracks ACC and I and copies forced predecessors onto some ACC
producers. Candidate availability is therefore not simply a vector-register
RAW DAG. The forced-predecessor check requires every forced predecessor to be
scheduled and returns `max(predecessor.issue+1)`; an unscheduled predecessor
gives `-1`.

## Hardware resources as registers

The register table has 62 records. The decoded records are in
`local/evidence/analysis/register-table.json` (local, not tracked).

| Name | Parts | Throughput-tracked |
|---|---:|---:|
| VF00..VF31 | 4 | no |
| VI00..VI15 | 1 | no |
| ACC | 4 | no |
| I | 1 | no |
| Q | 1 | yes |
| tt_nP (synthetic, for WAITP caps) | 1 | no |
| P | 1 | yes |
| R, CLIP, STAT_ALU, STAT_DIV, STAT_ALU_S, STAT_DIV_S, MAC, CLIPa, EXT | 1 | no |

VF registers have class 9, VI class 8, and hardware/synthetic registers class
10. VF00/VI00 are immutable zero records. Nonzero VF/VI records are
allocatable; the special records are not.

Status flags are deliberately split. Virtual status operands expand into
STAT_ALU, STAT_DIV and their sticky forms; immediate masks on status-test
operations are examined. For numeric immediate values irrelevant pieces are
avoided. For unresolved literal expressions vcl warns and conservatively
assumes all pieces are used. Sticky ALU/division flags attach as external
inputs for externally entered streams. EFU operand masks are adjusted
(`1`→unchanged, `2`→xy, `3`→xz, `4`→xyz, `5`→xyzw).

The SCE VU manual gives the hardware reason: P/Q results do not trigger
ordinary data-dependency interlocks and need `WAITP`/`WAITQ` when software
needs synchronization. That is corroborative context; the vcl policies above
are inferred from vcl 1.4beta7. See local
`PS2Docs/markdown/VU_Users_Manual.md`, P/Q descriptions around lines
1825-1842 and stall examples around lines 3781-4049.

## Static and dynamic scheduling statistics

Chain information is calculated in forward and reverse source order. The
per-instruction fields:

| Field | Meaning |
|---|---|
| Height | weighted height toward consumers |
| Height edges | maximum number of dependency edges toward consumers |
| Depth | weighted depth from producers |
| Depth edges | maximum number of dependency edges from producers |
| Direct consumers | number of directly dependent later instructions |
| Recursive weight | recursive consumer weight (sum of `child.weight+1`) |
| Downstream edges | maximum downstream dependency edge count |
| Extra depth | an additional weighted depth accumulator |

For an ordinary linked component the weighted edge is the output operand's
latency plus the input operand's additional timing offset. Reading several
components of one producer does not multiply its fan-out count. The recursive
weight does count multiple paths through a DAG; it is not a count of unique
descendants.

The same routine derives lower bounds from upper-only/lower-only/flexible
issue counts, dependency latency, per-resource throughput and functional-unit
pressure. Flexible operations are repeatedly assigned to whichever pipe has
fewer operations. Loop lower bounds are at least two, unless the stream cannot
loop. The diagnostics map reasons 1..10 to instruction latency, throughput,
upper count, lower count, cannot-loop, depth, height, branch depth, issue
pressure (resource in the high byte), and circular chain.

The linear lower bound additionally includes maximum chain height/depth plus
one and branch dependency depth plus two. The loop bound and the linear bound
are stored per stream; their getters clamp nonpositive values to zero. The
work-table resource horizon is clamped to `[64,16384]` but may be enlarged to
an existing raw table's length.

Dynamic stats are recomputed throughout search. Done instructions begin with
their actual issue times. Remaining instructions get an ASAP time from
currently valid producer locks and from reuse constraints. A reverse traversal
propagates latest times. The depth field is then overwritten with the dynamic
ASAP and the height field with `max_ASAP - latest`. Heuristics using these
fields therefore respond to the partial schedule. Physical-value liveness and
writer reuse can constrain ASAP in addition to producer-to-consumer latency.

## Eleven strategies

There are eleven strategies. The decoded table is in
`local/evidence/analysis/scheduler-strategies.json` (local, not tracked).
Each runs under one of two search procedures, "wrangler" A or B.

| Index | Name | Wrangler | Linear allowed |
|---:|---|---|---|
| 10 | dumb2 | A | no |
| 9 | normal2 | A | yes |
| 8 | vuta1 | A | yes |
| 7 | another | B | yes |
| 6 | normal | B | yes |
| 5 | dUp | B | no |
| 4 | vuta | B | yes |
| 3 | hDown | B | no |
| 2 | normal1 | B | no |
| 1 | ldumb | B | yes |
| 0 | dumb | B | yes |

Both optimizers try them in descending index order. Linear optimization
normally tries indices 10 down to 0, filters by the linear bit, and may skip
index 0 when an earlier candidate already succeeded. A main-call argument can
limit the range to index 0. Loop optimization always visits all enabled loop
strategies for a period before deciding which successful table to keep.

Let `H,D` be dynamic height/depth, `Nh,Nd` the static link-count fields,
`F,W` the direct-consumer and recursive-consumer fields, `O` original order,
`P` retry penalty, `E` the candidate's earliest legal line, and `S`
accumulated delay. Primary/secondary scores are maximized:

| Strategy | Primary | Secondary | Start proposal |
|---|---|---|---|
| dumb | minimum O | none | E+S, at least current cursor for an ordinary instruction |
| ldumb | minimum O | none | E+S, at least cursor-64 and 0 for an ordinary instruction |
| normal | H-P | -O | E+S |
| normal1 | D+H-P | W-P | E+S |
| hDown | W-P+H+B | F-P+(Nd>>2)+B | E+S |
| dUp | W-P | D+F-P | E+S |
| vuta | W-P+32D+B | F-P+Nd+B | E+S |
| another | W-P+H+B | F-P+(Nd>>2)+B | E+S |
| dumb2 | minimum O | none | E+(P>>1), at least cursor for an ordinary instruction |
| normal2 | H-P | -O | E+(P>>2) |
| vuta1 | W-P+32D+Bmac | F-P+Nd+Bmac | E+(P>>2) |

`B=400` for each nonzero-register input of STAT_ALU, STAT_DIV or MAC. `Bmac`
counts only MAC. **`normal` and `normal2` compute B but discard it**, so
adding B to their priority would change behavior.

After primary/secondary ties most strategies prefer the earlier start
proposal, then a larger height/link-count. The tie state has quirks: fields
used only in a tertiary tie are not always reset when the primary improves,
and some callbacks compare `Nh` against a saved `H`. A lexicographic tuple is
a readable approximation but is not bit-for-bit identical on all ties.
Traversal order matters: candidate insertion pushes onto the list head, so
visitation is the reverse of the to-do scan. The Rust primitive keeps the
stateful tie comparisons.

The preferred pipe for the B strategies and dumb2 is `P&1`; for normal2/vuta1
it is `(P>>3)&1`. Single-pipe instructions ignore it. Retry penalties
therefore alter both priority and placement.

## Candidate construction and placement

Wrangler B's builder clears candidates, recomputes dynamic stats and scans the
to-do list. It stops after adding more than 16 linear candidates (so there
can be 17). A's builder stops after more than 4 (up to 5) and considers an
extra modulo latest-time bound. No such truncation applies to loop candidates.

For each instruction the initial line is the maximum of its two
minimum-position fields, forced predecessors and ready producer component
locks. If any required writer or forced predecessor is not yet scheduled the
instruction is ineligible. A branch is withheld while more than one to-do
instruction remains in an unbounded linear table; it is then proposed no
earlier than `done_size-2`. In a fixed period its proposal is `period-2`.

For a loop, category-1 stores cannot precede the selected loop iteration minus
the known-safe count; category-2 operations cannot precede the selected loop
iteration itself. The bounds are converted back to absolute lines by
multiplying by the period.

The placement search works as follows:

```text
repeat:
    for each allowed pipe, starting at the preferred pipe:
        test the current absolute line
        if only a MOVE_NEEDED result remains: accept this proposal
        otherwise retain the checker's next possible line
    if no next line is below 30000: fail
    move to the earliest of the proposed next lines; retry both pipes
```

Fixed-pipe instructions have one pipe. Flexible-pipe search restarts pipe
preference from upper after advancing the line. It need not try `line+1`: the
checker can jump directly to the next availability boundary.

The placement check enforces minimum positions, end-distance constraints,
finite table bounds, a loop-iteration index of at most 16, optional
staggered-memory-access parity (`-S`), forced predecessor readiness, slot
occupancy, pairing, and input/output register lock constraints. Its result is
a set of causes, not one enum value. The causes: dispatch slot unavailable;
bundle pairing invalid; register move needed (the proposal can be tentatively
considered); outside the search horizon or too many loop iterations;
minimum/end/stagger constraint; a register lock's minimum-position
restriction; forced predecessor unavailable or not yet reached; internal
dependency checker failure; and the lock conflicts described below.

### Pairing

Pairing requires each instruction's issue mask to allow its slot (0 upper, 1
lower) and disallows both instructions writing the same nonzero register. The
check compares the register object itself; it does not allow a same-bundle
double write merely because the component masks differ. Reads are handled by
the separate lock checker.

### Value lifetimes and throughput

The input checker rejects use before `writer_ready+input_offset` (offset 8).
Reading later than the writer's current last-use position can extend its
lifetime, unless that would overlap another value of the same physical
resource. In a loop it can request a move for a movable input when the
lifetime would overrun a period or collide with another live value; immovable
inputs produce hard conflicts. An external loop input has separate
constant-versus-carried handling and a window relative to the external
output's latency.

The output checker:

* checks issue-throughput;
* takes availability as `issue+latency` for nonblocking output, or `issue+1`
  for blocking output;
* rejects conflicting live values, output lifetime overlap and external
  output windows;
* returns the next line that clears the conflict, folded back into a suitable
  future iteration for modulo scheduling.

The throughput check covers inclusive issue-resource occupancy intervals
`[issue, issue+throughput-1]`. The overlap check handles ordinary or cyclic
inclusive intervals: touching occupied endpoints conflict, adjacent intervals
do not. For cyclic intervals it normalizes starts modulo the period, keeps
lengths, and checks both ordinary and wraparound overlap.

## Wrangler control flow and bounded backtracking

B rebuilds the candidate list for each choice. A can keep the current list and
discard a candidate that fails placement, trying another before rebuilding.
Both perform these logical steps:

```text
while instructions remain:
    rebuild needed candidates and dynamic stats
    rank a candidate and find a candidate line/pipe
    assign an abstract move's pipe if needed
    move/drag related setters to make a legal value lifetime when possible
    check placement again
    if the only remaining conflict is MOVE_NEEDED:
        synthesize a move and restart / backtrack as directed
    else if legal:
        install component locks, write the dispatch slot,
        transfer instruction from to-do to done
    else:
        rewind to the first affected done instruction
        undo placements and synthesized moves
        increase retry penalty and sometimes the accumulated line delay
        rebuild candidates
```

Successful insertion installs the absolute/folded position, pipe, lock state,
insertion serial number and list membership. Undoing an insertion, creating a
move, undoing a move and dragging setters are separate operations that
substantially change search state; see [MOVE_ROLLBACK.md](MOVE_ROLLBACK.md).

On backtracking the retry penalty increases; when it exceeds the threshold it
is reset and the threshold increases. B also increments the accumulated delay.
The modulo threshold is capped at `period*16`; A additionally caps against
eight times the dynamic chain horizon. vcl uses wall-clock watchdogs instead
of a deterministic search-node limit. B's timeout is
`max(user_min, to_do/5+1)` seconds for linear and `to_do/40+3` for loops; A
uses `to_do/5+4` and `to_do/30+6`. The port uses a deterministic step budget
instead (see [LPGEN_PORT.md](LPGEN_PORT.md)).

## Choosing and expanding loop schedules

The eligibility check accepts only certain conditional control-flow classes
whose selected outgoing edge returns to the same stream. A backward textual
branch is insufficient. The loop search also tests a per-stream enable flag.

The first period is `minimum_loop_bound + LoopExtra`. A valid `LoopAbs` of at
least two can override it; if it is smaller than the lower bound plus extra,
vcl warns and uses the larger bound. For each period:

1. Clone a local loop work table, keeping the old table.
2. For every enabled strategy, prepare a fresh lock table, reset processor
   optimization stats, run the wrangler, reduce extents, patch/validate moves,
   and locally allocate registers. A schedule that cannot allocate is
   rejected.
3. Keep the successful candidate with the smallest maximum scheduled iteration
   ID; ties replace the earlier candidate. All candidates in a round share a
   period, so this minimizes pipeline depth within it.
4. If none succeed, restore the previous work table and increment the period.
   Otherwise commit the new work table and install the selected optimized
   table.

After all instructions are inserted, the wrangler checks loop side-effect
statistics. If critical or recoverable effects occur too early relative to the
chosen loop iteration/known-safe count, it marks the offending instructions
for rescheduling and backtracks. Correct modulo occupancy alone is not enough
for a safe transformed loop.

Scheduled modular tables are then expanded into enter, prologue, main,
epilogue and exit tables. Known-minimum and known-safe loop counts control
which pieces are needed. When absent, the known minimum is 1 and the safe
count is **10000**; the diagnostic text says 1000, but the value used is
10000. Immediate fixup before and after these edits is necessary because
address increments move between iterations/prologue/epilogue.

## Linear optimization and late reduction

Streams requiring unscheduled optimization must have completed external
allocation and have period zero. For each allowed strategy vcl prepares a
fresh intermediate table, computes dynamic stats, runs the wrangler, reduces
register extents, adjusts branch-delay layout, patches/validates moves, and
tries local allocation. It keeps strictly shorter candidates by length; ties
keep the earlier strategy. It can stop once the result reaches the stream's
computed lower bound. With no successful candidate it reports failure rather
than emitting input order.

The stream merger merges adjacent optimized streams only when their
control-flow edges, allocation modes, entry/exit classes and explicit nonmerge
flags permit it. It requires a unique edge and no separating branch. The
merged stream is retried.

A relaxation step releases selected optimization state and clears
insertion/list state on raw instructions so later passes can reduce a
combined stream. A reduction/validation step reconstructs external information
and the lock table, linearly reduces eligible tables, prepares synchronization
reduction, validates moves and installs the table again.

### Caps, WAIT instructions, and empty issue rows

[FINAL_REDUCTION.md](FINAL_REDUCTION.md) details the call order, raw
synchronization moves, timestamp edits and compression vetoes.

The cap finder handles certain nonblocking outputs that can be capped by an
instruction in an otherwise usable slot:

* Q: `waitq`;
* P: `waitp`, referring to the synthetic `tt_nP` resource;
* upper STAT_ALU / STAT_ALU_S / MAC outputs: `abs`, writing VF00 and reading
  a still-valid ordinary VF result from the flag-setting operation.

The flag case requires the producer to be in upper pipe 0, its first operand
to be an output register, and that register to be VF. A producer with a
nonzero VF destination supplies that exact register and the first operand's
component mask. A producer writing VF00 cannot supply a useful interlock; for
that case vcl considers VF1 through VF31 whose pool entries are both enabled
and visible, intersects their availability with the producer's components, and
excludes components read, written or passed through anywhere in the current
table. It chooses the first physical ID with the largest surviving component
count; ties keep the earlier ID.

The cap is `abs.<mask> VF00, cap_source`. When a scratch register was
selected, the original producer's VF00 output is redirected to that scratch
register, so the temporary is written by the **original producer** and the
ABS result is discarded. ABS has no flag output. Its VF read makes the
hardware stall both issue slots until the producer's ordinary vector result is
ready, synchronizing a paired lower flag read without replacing that flag
value. This is dependency enforcement, not an algebraic simplification of the
user's arithmetic.

The candidate scan has these constraints:

1. It considers only a real, nonzero, nonblocking output with at least one
   registered user. Unsupported output kinds receive no cap.
2. Starting after the producer row, a free slot is eligible only when that
   row's emulated timestamp is at least `producer_time + output_latency`. For
   P, a latency greater than one is reduced by one before the comparison.
3. Only the cap instruction's allowed issue pipes are considered. It keeps the
   latest candidate sharing a nonempty bundle; if all candidates are empty it
   keeps the first.
4. The scan stops at the first read of the capped output register, tested by
   operand direction and component intersection, not by instruction name.
5. Between candidates, cap-source components overwritten by later
   instructions are removed. Once none remain, scanning stops. The mask saved
   with an earlier candidate is its snapshot at that location, before writes
   in that candidate's own bundle.
6. It counts intervening nonempty bundles and skips insertion when the
   selected candidate already has at least the adjusted output latency's
   worth. Otherwise it installs the cap, updates source-register references
   and relinks the table.

The saved runtime fixture `local/evidence/oracle/hw-fsand-after-alu/` (local,
not tracked) shows the nonzero-destination case: `add VF02,VF01,VF01` is
followed by a bundle with `abs VF00,VF02` in upper and `fsand VI01,1` in
lower, and that bundle reports three latency-stall cycles. `hw-fmand-after-alu`
shows the corresponding MAC flag case. These establish those executions, not
every scratch selection or control-flow case above.

The empty-row compressor scans empty rows while emulating the processor. It
gathers in-flight outputs and identifies the next synchronization-sensitive
user/throughput writer. For P it adjusts one cycle differently from Q
(`latency-1` versus `latency` in throughput consideration). It removes a row
only if actual readiness/stalls preserve dependencies, min-position and
end-distance bounds, external minimal length and minimum end padding, and it
updates later logical timestamps. An explicit NOP may vanish while a
wait-induced processor stall preserves execution time. A generic "remove every
empty bundle" pass is incorrect.

The final tidy step marks eligible terminal streams, removes disappearing
flow edges, marks the end instruction, and extends tables to at least two
issue rows when end-bit restrictions require it. A final check rejects some
final lower instructions and branches near the end. The final stall loader
writes three recorded stall counters per dispatch entry.

## Rust reconstruction boundary

`crates/vcl/src/scheduler/schedule.rs` is an owned scheduling core with
strategy ranking, pipe pairing, resource intervals, cyclic overlap, explicit
dependency timing, period search and bounded rollback. Its graph input is
explicit, so it can validate timing edges and resource occupancy without a
parser. Unit tests cover the primitives and the engine's behavior. The
exported primitive scores use caller-supplied metrics; the standalone graph
engine derives an approximation when none are supplied. Passing these unit
tests does not make a schedule byte-for-byte equivalent to vcl's.

The standalone engine does not itself cover the full lock graph, external flow
backtracing, dynamic anti-dependency/lifetime repair, synthesized move search,
ACC barriers, status rewriting, loop prologue/epilogue construction, cap
insertion, or stall/NOP compression; the compiler pipeline provides those
elsewhere (see [LPGEN_PORT.md](LPGEN_PORT.md), [COPIES.md](COPIES.md) and
[FINAL_REDUCTION.md](FINAL_REDUCTION.md)). Its bounded backtracking is
deterministic and does not reproduce vcl's retry order and watchdog.

### Replay-validated timing reconstruction

`crates/vcl/src/scheduler/timing.rs` reconstructs late synchronization and
empty-row removal. Its input is a straight-line **physical-register** stream,
not a source-order CFG. Each read carries its expected producer identity; each
write carries its component mask and availability latency. The replay applies
both bundle reads before either bundle's writes, waits on interlocked VF/VI
inputs, applies resource throughput, and checks every resulting read's
producer. This catches a flag read whose meaning changes because a different
pending write completes during an unrelated VF stall.

`Synchronization::Q` and `Synchronization::P` use free lower slots;
`Synchronization::Vector { register, components, producer }` uses a free upper
slot for `ABS VF00, source`. A Vector cap checks that each selected component
still belongs to the specified producer after the cap-induced stall; an
overwritten component is an explicit `CapReadMismatch`. The optional cap
inserter follows the availability threshold and nonempty-bundle preference,
accepts a cap only if a full replay succeeds and it enables removal of
physical rows, preserves caller-protected rows, and does not increase explicit
output-drain debt.

This helper does not choose scratch registers or redirect a VF00 producer; the
compiler must supply a nonzero cap source. It also does not shrink candidate
component masks automatically, rewrite register locks, or handle cross-stream
timing. It validates the exact mask supplied by its caller and therefore may
keep rows that vcl removes. End-bit and branch-delay protection is supplied by
the caller; flattening alternative CFG paths into one replay does not
establish correctness at a join. Unit tests cover noninterlocked flags and
ACC, P/Q waits, vector caps, component overwrite, changed producer versions,
simultaneous read/write ordering, resource stalls and output-drain bounds.

## Generated-part cleanup

Selected tables carry an explicit fixed/linear mode. Normal generated
prologues and drains use conservative extent reduction before allocation;
modulo kernels and explicit raw source tables keep their rows. Effective
per-instruction reads are materialized from pair owners before placement
dependencies are built, so a lower read never inherits an unrelated upper
operation's consumer identity.

`linear_cleanup::reduce` trims trailing holes, preserves leading readiness
rows and explicit branch-delay pairs, and sinks caller-proven pure VI updates
within all actual dependencies, unit occupancy and boundary separation. Empty
movable exit chains can inherit ordinary E termination without deleting
analysis edges. Final physical replay then compresses eligible rows unless
`-C` disables that pass. See [LINEAR_CLEANUP.md](LINEAR_CLEANUP.md) for the
mode gate, extent and tidy rules, controlled outputs and remaining motion
limitations.

## General copy normalization

Copy normalization recognizes specific identities after ordinary pruning
establishes which MAC/status outputs are observed. Its move detector admits
UTA and movable VSM submodes 1/3; fixed raw tables keep their original
instructions. The move-pruning routine itself is a no-op in this version, so
that entry point is not evidence of a further copy-elimination algorithm.

The compiler carries internal IMOVE/VMOVE state separately from
source-written abstract opcodes. Integer copies resolve to a concrete IALU
copy; vector copies resolve to a lower (LS) or upper (FMAC) copy. They emit
as IADDIU with literal zero, MOVE, or MAX source,source respectively. Physical
replay uses the resolved IALU/LS/FMAC metadata rather than the abstract
zero-unit mask. These three units have no cross-pipe sharing; normal
issue-slot occupancy already constrains their throughput-one copies. See
[COPIES.md](COPIES.md) for rule boundaries, tests and internal failure status.

## Ordinary branch tails and stream boundaries

The compiler performs vcl's final lower-pipe exchange (for branch delay slots)
for caller-proven ordinary VI operations. It checks real dependencies,
earliest bounds, pair resources, throughput and outgoing debt; fixed, modulo
and generated tables keep their separate paths. Already scheduled direct
ordinary predecessors reduce incoming latency by guaranteed elapsed issue
pairs rather than adding the entire setter latency again at the next block.

After allocation, unique unprotected ordinary fallthrough tables concatenate
without rescheduling. Joins, exports, CONT, fences and label flags prevent the
merge. The analysis graph remains available for producer replay. Exact rules
and the remaining differences (external allocation, ILW delay, raw boundary)
are in [BRANCH_STREAMS.md](BRANCH_STREAMS.md).

## Raw reorder

`raw_cleanup::reduce` applies a bounded version of vcl's alternating
pull-up/drag-down loop to selected raw prologues/drains after allocation.
`compile::raw_reduction` checks the initial-stall gate, projects only
unobserved implicit FMAC flags, materializes concrete pair reads and builds
physical anti/output dependencies, including reverse same-pair WAR. Final
replay checks the candidate before installing it. Fixed/modulo and unsupported
frames keep their selected layouts.

Incoming latency can credit a fixed source and intermediate CFG issue spans;
outgoing tail stays zero for future unscheduled ordinary VI users. The full
execution-time getters and hazard-broadcast locks are broader than this
conservative duration adapter. See [RAW_BOUNDARIES.md](RAW_BOUNDARIES.md) for
exact controls and the remaining minimum-count endpoint work.
