# Generated-part extent cleanup and termination

This chapter describes how vcl 1.4beta7 trims generated normal prologue, drain and
exit parts, and the port's conservative implementation. vcl's code was analysed
statically (inferred from vcl 1.4beta7); the notes are kept locally and are not
tracked. Raw reorder, stall guards and endpoint limits are in
[RAW_BOUNDARIES.md](RAW_BOUNDARIES.md).

## Modes and pass order

vcl's part factories (prologue, epilogue, kernel, entry, exit) do not give all
generated parts identical reduction rules. The kernel is fixed submode 2. Normal
prologues/drains use submode 1; explicitly fixed raw input gives its prologue/drain
a fixed submode. Raw `reorder` input is also movable but needs more of the raw
reduction engine than the port's adapter implements.

A mode predicate identifies movable VSM as code mode 3 with submode 1 or 3. The
linear reducer uses the same gate, requires period zero, and repeatedly invokes
pull-up and drag-down until the enabled passes stop making progress. Fixed modes and
modulo tables do not enter that loop. A further check can disable motion when
selected tables stall or are externally pinned, so movable mode alone does not
permit moving every operation.

The main routine calls linear reduction, then the tidy pass. The cap and final
empty-row passes run later, behind an option-controlled condition. `-C` disables
those final size/cap passes; it does not skip earlier extent work.

## Extents use users and boundary constraints

Extent reduction examines output-user bounds before trying later issue positions.
Drag-down scans backward, excludes branches from ordinary sinking, limits candidates
by table end and each output's user/latency bound, and pins inputs carrying an
external constraint mark. It validates each move through the lock checker and
derives a new start from the remaining input constraints rather than deleting
leading NOPs indiscriminately.

Pull-up scans forward and pulls instructions toward their input bounds. It tracks
the last occupied or effect-required row, handles the conditional and its lower
delay instruction separately, and can shorten the end without violating the
external minimum table length. A separate routine has another branch/delay
interchange path; it refuses fixed modes, looping tables, short frames, dependent
swaps and nonempty final rows.

Example: in the after-add fixture, the selected warmup rows start as:

```text
row 0: NOP                       LQ c,8(p)
row 1: NOP                       ISUBIU count,count,1
row 2: NOP                       IADDIU p,p,3
row 3: NOP                       IBEQ count,VI00,drain
row 4: MUL d,c,a                 NOP
```

The count update must stay at least two rows ahead of its conditional read. The
pointer update has no local branch user and can issue in the lower slot of row 4,
which executes as the delay pair on both outgoing paths. This shortens its live
extent without changing how many updates execute. The rewritten address 8 already
reflects the earlier affine scheduling pass, so late sinking must not apply that
offset correction again.

## Empty exits and E placement

The tidy pass first marks ordinary streams whose outgoing targets are synthetic
ordinary exits. It then hides emission links, suppresses obsolete branches, changes
the end class, and appends only enough rows for ordinary E termination. Macro exits
stay distinct. A separate predicate supplies the lower-slot safety rule;
`control::required_exit_padding` reconstructs it.

After merging or bypassing an empty generated exit, the focused drain is:

```text
row 0: NOP                       NOP
row 1: NOP                       NOP
row 2: NOP[E]                    NOP
row 3: ADD b,b,d                 NOP
```

The ADD completes the final pending iteration. Its preceding rows keep incoming
readiness, while the unused row after it disappears. Four rows already satisfy
termination safety, so another two-row exit is unnecessary. E follows from the final
penultimate row, not from a source marker or a hard-coded drain name. Analysis edges
stay present even when assembly links and empty exit rows become invisible.

## Port implementation

`SelectedTable` in `compile` owns selected rows plus a `TableMode`; its rows remain
the provenance for pair reads during component flow. Fixed kernels and explicit raw
input keep their layouts. Normal linear parts can enter a separate reduction
adapter; mode is not inferred from label spelling, instruction count or immediate
values.

`linear_cleanup::reduce` takes a local `SchedulingProblem`, selected rows,
caller-proven sink candidates and a minimum frame length:

```rust,ignore
let rows = linear_cleanup::reduce(
    &placement_constraints,
    &selected_rows,
    &eligible_vi_updates,
    required_boundary_rows,
)?;
```

The adapter implements a bounded subset:

1. Keep leading readiness rows and every source branch position/delay pair.
2. Trim trailing empty rows, respecting the required boundary frame.
3. Scan eligible updates backward and try later positions in the selected pipe.
4. Check actual RAW/WAR/WAW and memory edges, pair-write exclusion and functional
   unit occupancy before each accepted move.
5. Recompute allocation issue positions from the changed rows.

Sink candidates have ordinary integer reads/writes, at most one-cycle write latency,
and no memory/branch operation, source modifier or other side effect. An artificial
source-list edge to an unrelated branch does not forbid its delay slot; actual
conditional-input latency edges stay intact. Additional separation is kept when an
outside conditional reads a VI result.

A selected pair's read owner is not necessarily its concrete consumer instruction,
so the compiler materializes effective per-operation read events from the owner's
component definitions before creating placement edges. Otherwise lower reads could
constrain an unrelated upper operation and leave the real lower move unconstrained.

Empty movable generated exit chains can pass an ordinary exit boundary inward. Their
emission suppression keeps CFG liveness, interface pins and global label checks, and
the normal drain then receives ordinary E padding before final physical rendering.
Fixed raw termination is unchanged.

Final replay/compression runs on allocated physical registers. It preserves branch
tables and leading readiness rows, validates expected producers and hardware
resources, and lets interlocked VF waits replace internal empty rows. Movable tables
with escaping non-VF latency debt greater than one keep conservative protection.
`-C` suppresses this final compression.

For minimum four, warmup has no early-exit conditional. After sinking, its count and
pointer updates accompany the retained load/multiply dependency. Final compression
removes two internal empty rows and reports the resulting two-cycle VF interlock
stall: three emitted prologue rows still represent five cycles. With `-C`, those two
physical empty rows remain.

## Controls

`local/tools/probe_cleanup.py` validates input/output digests, vcl executable
identity, flags, guest exit and per-run alarm profiles for ten saved observations.
It runs vcl only with `--capture`.

| Observation | vcl pairs | Port pairs | Comparison |
| --- | ---: | ---: | --- |
| normal before-add affine | 14 | 14 | equal pairs |
| normal after-add affine | 14 | 14 | equal pairs |
| normal after-subtract affine | 14 | 14 | equal pairs |
| after-add, standard alarm profile | 14 | 14 | equal pairs |
| normal minimum four | 12 | 12 | equal pairs |
| normal with `-C` | 14 | 14 | equal pairs |
| normal minimum four with `-C` | 14 | 14 | equal pairs |
| fixed raw minimum one | 37 | 37 | equal pairs |
| fixed raw minimum four | 27 | 27 | equal pairs |
| raw `reorder` | 27 | 27 | equal pairs (see [RAW_BOUNDARIES.md](RAW_BOUNDARIES.md)) |

For the after-add input, vcl's stdout is byte-identical at alarm scales 0.1 and 1;
its diagnostics and watchdog/search traces differ. That is a result for this input,
not proof that all scheduler outputs are independent of the alarm profile.

## Tests and limitations

Unit tests cover sinking with a live branch input, explicit delay rows, leading
readiness, boundary debt, anti-dependencies, whole-register pair writes, throughput,
malformed identities, invalid distances and missing delay rows. Compiler tests keep
masks, registers, slots, targets and E bits. Execution checks compare escaping
registers and all modeled memory over 64 deterministic states, including minimum
promises and disabled compression, under the restricted logical model in
[TESTING.md](TESTING.md) (not PS2 numerical/timing certification).

The normal-part adapter does not reproduce arbitrary VF/hardware writer motion,
leading frame rebasing, complete external timing analysis, full branch-delay swaps,
or vcl's move/lock mutation and modulo allocation. The 49-program corpus and the
retained probes remain gates; byte-for-byte replacement is unfinished.
