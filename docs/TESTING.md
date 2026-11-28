# Validation contracts

The port is checked against the original at several levels. None of them
establishes complete VCL or PlayStation 2 compatibility.

- Unit tests exercise recovered primitives.
- `crates/oxide-vcl/tests/oracle_pipeline.rs` and
  `crates/oxide-vcl/tests/oracle_instructions.rs` compare emitted instruction pairs
  with retained original-binary outputs, including slots, masks, E bits and
  register choices.
- `crates/oxide-vcl/tests/semantic_equivalence.rs` compares the observable effects
  of selected outputs even when their pair layouts differ.
- `crates/oxide-vcl/tests/listing_equivalence.rs` runs oxide mode's listings
  of every public corpus program and probe in the timed model and requires
  them to compute what the original's listings and the unoptimized listings
  compute ([Listing equivalence](#listing-equivalence)).
- `crates/oxide-vcl/tests/public_corpus.rs` requires all 52 public corpus programs
  to match the original ([CORPUS_BASELINE.md](CORPUS_BASELINE.md)).
- `crates/oxide-vcl/tests/probes.rs` applies the same comparison to the probe
  suite in `tests/probes/`: about 900 small programs written for this
  project, each with the original's run, so CI covers what the local evidence
  covers at the listing level ([tests/probes/README.md](../tests/probes/README.md)).
  A short list of known differences, each with its reason, is allowed.
- `crates/oxide-vcl/tests/local_corpus.rs` checks the local corpora (below).
- `crates/oxide-vcl/tests/preprocessor.rs` checks the preprocessor against
  vclpp's test suite and against the corpus inputs vclpp produced
  ([Preprocessor](#preprocessor)).

## Original controls by area

Each area has controls whose original output was captured and is checked in
`oracle_pipeline.rs`. Most require identical pairs. A few keep a documented
layout difference and are checked by bounded execution instead; the
documents named here list them.

- Copies and coalescing ([COPIES.md](COPIES.md)): flags, symbolic input, VF00
  field distinctions, raw modes, complete users, output observations and
  joins. One test requires the internal status-zero diagnostic failure to
  return a library error while matching the original CLI's status zero, empty
  stdout and exact stderr; the Python replay classifies this outcome apart from
  successful compilation or help.
- Generated-part cleanup ([LINEAR_CLEANUP.md](LINEAR_CLEANUP.md)): normal
  affine loops, minimum-four and `-C` controls, and a standard-alarm replay.
  Primitive tests cover branch-input latency, explicit delay rows, boundary
  debt, pair-write exclusion, anti-dependencies and throughput limits.
- Software pipelines ([PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md)):
  explicit stage windows, known-minimum early exits, raw memory addresses,
  textual forward definitions, disabled search and source-stage errors. The
  semantic suite requires generated kernel labels on scheduled fixtures, so a
  silent linear fallback cannot satisfy the pipeline assertions. Min4 promises
  are exercised only with trip counts 4-11; other counted fixtures use 1-8.
- Allocation: component/stream raster and issue-lock tests cover negative
  overlap and through-path cases; bounded execution checks cover component reuse
  and simultaneous disjoint writes to distinct VF registers.
- Parsing: an all-52-source syntax gate, register/mask/opcode assertions,
  positive original-pair probes, rejection controls and full-program
  regressions.
- Branches and streams ([BRANCH_STREAMS.md](BRANCH_STREAMS.md)): exact-pair
  controls, controls with retained layout differences and a shared
  invalid-context rejection. Unmarked cases have 64-state execution
  comparisons, including both conditional paths and varied loaded VI addresses.
  D/T/M controls use exact pair tests only, since the model rejects their
  hardware behavior. CONT execution beyond its first E bit needs an external
  restart and is excluded from the kernel comparison. The hash-bound record is
  `local/evidence/analysis/control-validation.json`.
- Raw boundaries ([RAW_BOUNDARIES.md](RAW_BOUNDARIES.md)): controls that match
  pairs and controls that keep documented endpoint, modifier or hardware-output
  layout differences; reducer tests include a 128-frame constraint matrix, and
  path-distance tests cover alternatives, cycles and unknown duration. Kernels
  run 64 states each, including valid minimum-count promises and varied low VF
  bits for a same-pair MTIR read. Live-MAC and D controls are outside the
  execution model.
- External allocation ([EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md)):
  pure-policy and bridge tests; whole-pair matches, integer layouts that match
  allocated operands while differing in termination, and an exhaustion case
  that shares rejection and status. Two original vector controls contain no E
  termination and use strict pair checks rather than a relaxed model.
- Attempt selection for the corpus tools rejects wrong identities and
  undeclared timing profiles, and distinguishes emulator blockers from guest
  errors.

## Bounded execution of emitted microassembly

`crates/oxide-vcl/tests/support/vu_model.rs` is a small **test-only execution model**.
Its default configuration, `Config::LOGICAL`, is described in this section;
`Config::TIMED` is in [The timed configuration](#the-timed-configuration).
Its specification comes from SCE's *VU User's Manual*, version 6.0 (a Markdown
copy is kept with the PS2 manuals). It does not use openvcl implementation code
or another emulator's execution model.

| Model rule | Manual source |
| --- | --- |
| VF00 = (0,0,0,1); VI00 = 0; VI values are 16 bits | §3.2, pp. 20–23 |
| Upper and lower instructions issue concurrently | §3.4.2, p. 44 |
| Upper write wins for the whole VF register, including disjoint destination masks | §3.4.3, p. 44 |
| One physical instruction pair executes after a branch before the target | IBEQ, pp. 161–162; IBNE, pp. 166–167 |
| E terminates after one further pair; restricted E delay instructions | §3.5.3, pp. 53–54; E bit, p. 67 |
| VI arithmetic and signed branch conditions | IADD–ISUBIU, pp. 158–172 |
| Displaced qword loads/stores and predecrement/postincrement by one qword | LQ/LQD/LQI, pp. 177–179; SQ/SQD/SQI, pp. 190–192 |
| Integer memory data widths | ILW/ILWR, pp. 168–169; ISW/ISWR, pp. 173–174 |
| Raw bit transfers and field movement | MFIR, p. 180; MOVE/MR32/MTIR, pp. 181–183 |
| Arithmetic numerical limits differ from IEEE 754 | §2, especially rounding and differences on pp. 11–13 |

The compact `vu-instruction-manual.md` transcription is not used where it
conflicts with the complete manual. For example, its LQI increment description
says `+8`; the full VU manual's operation is `VI[is] = VI[is] + 1`, with VI
holding a qword address. The model follows the latter.

All register and memory values are stored as raw bits. Each pair reads its
preissue state; lower effects commit before upper effects, while a lower VF
write is discarded if that upper slot targets the same VF register. A masked
write preserves unselected fields. VI arithmetic wraps modulo 65536; writes to
VI00 and VF00 are ignored. Direct branch labels resolve without case sensitivity.
Taken and untaken branches both execute their physical delay pair. Termination
requires an E-marked upper instruction followed by one executed pair. Falling
off the assembly or exceeding 4096 pairs is an error in the kernel checks.

Memory is an explicit array of 1024 qwords for ordinary VU1 data. The model
rejects effective addresses outside that array. It deliberately does **not**
guess wraparound, register-window aliases, VU0 mapping, DMA, or external units.
The tests stay within ordinary memory. Integer wrapping is tested independently
of out-of-range memory addressing.

Supported instructions include ADD/SUB/MUL, ABS, MAX/MINI, accumulator
variants, scalar I/Q and broadcast arithmetic, OPMULA/OPMSUB, FTOI/ITOF and
CLIPW; IADD/ISUB/IAND/IOR and immediate forms; B/BAL/JR/JALR, IBEQ/IBNE and
signed conditional branches; LQ/SQ, LQI/SQI, LQD/SQD, ILW/ISW, ILWR/ISWR;
MOVE/MR32/MTIR/MFIR; DIV/SQRT/RSQRT, the EFU instructions, WAITQ/WAITP, LOI
and MFP; the MAC, status and clip flag instructions; XTOP/XITOP and XGKICK;
RINIT/RGET/RNEXT/RXOR. The D, T and M bits are accepted and ignored, and
`-m`'s DMA and VIF lines are skipped. Unknown opcodes, unresolved expressions, malformed registers,
unsupported modifiers, branches in branch delay slots, and forbidden modeled
operations in E delay slots return errors. An instruction appearing in this
supported set does not mean every hardware operand restriction is validated.

### Numerical and timing boundary

Arithmetic accepts zero and finite normal f32 magnitudes from 2^-16 through
2^16. It evaluates an operation in f64 and requires that the result be exactly
representable as f32 within that same range, or zero. Each product in an
accumulator operation passes this check separately. A computation requiring
rounding, underflow, overflow, NaN, infinity, or denormal handling is an error.
This bounded range prevents widely separated operands disappearing in the f64
intermediate. It is a deliberate exact-arithmetic subset, rather than a model
of the VU's truncation and least-significant-bit behavior. The generated kernel
inputs are positive dyadic values from 1/4 to 4, with short bounded loops, so
their arithmetic stays exact. Tests also check that unsupported numerical cases
are rejected.

The logical configuration interlocks logical register dependencies and
completes operations before a subsequent pair reads their results. It does
not simulate pipeline latencies, stalls, the preceding-instruction VI branch
hazard, asynchronous Q/P visibility, or ACC latency. WAITQ and WAITP are
logical no-ops. DIV, square roots and EFU results are restricted to exact
results. Q/P/I and ACC state can be initialized explicitly. Interrupts, DMA
and the GIF's own processing are not modeled.

Consequently a semantic pass checks the intended interlocked computation. It
does **not** establish that an output has safe hardware timing. Pair comparison
and recovered scheduler-latency checks remain necessary for that claim.

### Kernel comparisons

The semantic suite executes saved original output and freshly compiled native
output with identical initial states. Each kernel gets 64 deterministic
pseudorandom small dyadic input states. Incoming VI01 cycles through positive
trip counts 1–8; VI02 starts at qword 32. The assertions compare explicitly
declared escaping VF components, VI registers, and the **entire memory array**.
Allocator scratch registers and issue-pair counts are not observable results.
The seed and failing register/component are included in assertion messages.

Cases cover straight-line arithmetic, partial VF masks, ACC, declared Q,
simultaneous old-VF reads, loads paired with upper reads, forward branches,
case-insensitive targets, integer/vector backedges, counted loops, incrementing
and decrementing loads/stores, store/load order, and affine pointer loops.
Three raw-branch fixtures require values defined in a delay pair to reach the
taken target or both successors. A fourth confirms strict rejection when a
source input used in the delay pair and target was never declared or defined.
Model unit tests independently assert delay behavior, upper whole-register
priority, VI wrapping, immutable constant registers, masks, memory addressing,
unsupported instructions/numerics, and bounded nontermination.

An additional test checks the original optimized affine pipeline against a
scalar recurrence computed directly from its source:

```rust
for _ in 0..count {
    p += 3;
    for lane in 0..4 {
        b[lane] += memory[p + 5][lane] * a[lane];
    }
}
```

This exercises the original prologue/kernel/epilogue and its `LQ 5` to `LQ 8`
repair independently of the Rust compiler's schedule. The compiler comparisons
exercise the integrated five-row modulo kernel with the same offset
correction, and the three normal prologue/drain layouts match original pairs.
See [IMMEDIATES.md](IMMEDIATES.md) for original-binary evidence and fixup
details.

Allocation lifetimes extend through CFG live-in and live-out sets, so a
temporary cannot overwrite a loop-invariant register after the first
iteration. The affine comparisons run more than one iteration to check this.
A failing semantic gate must not be changed into a layout-only comparison or
ignored.

Run the semantic suite with:

```sh
cargo test --test semantic_equivalence
```

For the complete local checks:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests that need the original-binary data under `local/evidence/oracle` (local, not
tracked) carry `ignore = "needs local/evidence"`. A build script sets
`cfg(local_evidence)` when that directory exists; without it those tests are
skipped rather than failing.

`crates/oxide-vcl/tests/local_corpus.rs` checks extended corpora that are not distributed. It
reads `local/corpus/manifest.json` when it exists; each set listed there is a
directory of cases in the public corpus format, and every case must match the
original unless its set lists it under `known_differences`. The test's module
documentation gives the manifest format. Without the manifest it passes and
does nothing.

`.github/workflows/ci.yml` runs on pushes and pull requests: rustfmt and
clippy, then a debug and a release build, each running every test.

How the local corpora are built is described in
[LOCAL_CORPUS.md](LOCAL_CORPUS.md).
### The timed configuration

`Config::TIMED` runs a listing cycle by cycle, from the timing rules of the
VU User's Manual 6.0 (§3.4, §7.4) that the port's instruction table also
follows:

- A pair issues when both instructions can: a read of a VF field waits until
  an FMAC, load or move writing it is 4 cycles old, a read of a VI register
  loaded by ILW waits 4 cycles, and DIV/SQRT/RSQRT or an EFU instruction
  waits while its unit is busy (7, 13, or the EFU throughput).
- Q, P and the MAC, status and clip flags are not interlocked: each changes
  when its latency has passed (Q 7 or 13 cycles, P the EFU latency, flags
  4), and a reader sees the value of its issue cycle. WAITQ waits until Q
  lands, so its pair reads the new Q (figure 3-9); WAITP waits until the
  EFU's Nn stage, a cycle before P lands, and the pair after it reads the
  new P (figure 3-12).
- An integer result is bypassed to the next pair, except to a conditional
  branch (IBEQ, IBNE, IBLTZ, IBGTZ, IBLEZ, IBGEZ): one issued on the cycle
  after the instruction that set its register reads the value from before
  (§3.4.8, figure 3-15). The flag checks (FSAND, FMAND, FCAND and the
  others) are the exception, as the manual says. JR and JALR have no such
  restriction in the manual and read the new value.
- ACC and I are visible to the next pair.
- Arithmetic is `Arithmetic::Deterministic`: f32 on the VU's range (no
  infinity, NaN or denormal; overflow saturates, underflow flushes to
  zero), with each product rounded before an accumulation. This is not the
  VU's rounding, so timed runs compare two listings with each other, never
  with hardware.
- Addresses wrap around the 1024 qwords of data memory, a run that passes
  the last pair ends as if an E bit ended it, and results in flight land
  after termination.
- XGKICK records its address and the GIF packet there: each tag and its
  data (PACKED, REGLIST or IMAGE sizes) up to the tag with EOP.
- RNEXT steps R by the manual's generator (x^23 + x^5 + 1, §1.1.2), and
  RNEXT and RGET return R's 23 bits as a float in [1, 2).
- Each pair's stall cycles are recorded with their cause, the need ready
  last: an FMAC result, a load, an ILW result, a busy FDIV or EFU, WAITQ or
  WAITP (`Execution::stall_causes`, `vu_model::Stall`).

Not modeled: the stall of an XGKICK after an XGKICK, and the VU's exact
rounding. The manuals say an XGKICK issued during a PATH1 transfer stalls
the pair after it until the transfer ends (§3.4.9, figure 3-18; EE User's
Manual §7.1.1), but not how long a transfer takes, so the model does not
time it.

The model's unit tests check field interlocks, Q landing and WAITQ, flag
timing, ILW interlocks, stall causes, GIF packet capture and saturating
arithmetic, and
the tests named `figure_3_*` run the manual's worked examples of §3.4 with
the stalls and values it shows:

- 3-3: a result read four pairs on, no stall;
- 3-4: FMAC hazards per field, three stall cycles each;
- 3-7 and 3-8: Q seven cycles after DIV, the old Q before, and a second DIV
  waiting five cycles for the unit;
- 3-9: WAITQ's six cycles, its pair reading the quotient;
- 3-11 and 3-12: ESADD after ESADD waiting nine cycles, WAITP until Nn,
  the MFP before it reading the old P and the one after the sum;
- 3-14: integer results bypassed, no stall;
- 3-15: a loop branching on a count it decrements: with a slot between
  them three passes, without one a pass more; a branch right after FSAND
  reads its result;
- 3-16 and 3-17: the status flag at the S stage, and after stalls
  FSAND reading instruction 2's status.

## Listing equivalence

`crates/oxide-vcl/tests/listing_equivalence.rs` compares two listings of
one program by running both in the timed configuration from twelve seeded
states. Four are random: VF values of mixed sign between 2^-4 and 2^4, VI
values from 1 to 32, TOP below 512, and data memory whose low 16 bits are
12, 24, 36 or 48, so that counts and pointers read with ILW are short and
loops stepping by 1, 2, 3, 4 or 6 to a bound end. Eight are count seeds:
the low 16 bits of memory and VI01 to VI15 all hold one count, 1, 2, 3, 4,
5, 6, 9 or 12, which gives a loop counting down by one 1 to 6 iterations
and one counting down by three 1 to 4, where a pipelined loop's prologue
exits early. Two listings are equivalent when, on every seed both finish,
they leave the same data memory and kick the same GIF packets. Listings
equal apart from comments and spacing are equivalent without running; a
run that reaches the 200,000-pair limit is retried with eight times the
limit when the other finished.

Over the public corpus and the probes it requires:

- oxide mode with every optimization to compute what `--opt=none` computes,
  and to accept the same programs;
- oxide mode's code to compute what the original's recorded code computes,
  except for the nine programs the alias fix repairs (`FIXED`);
- every oxide-mode listing to run in the model, except three whose
  symbolic registers or immediates cannot execute (`NOT_EXECUTABLE`).

These three compile the corpus twice and are ignored in debug builds; the
release suite runs them. `cycle_report`, an ignored test, prints the cycles
each changed program takes with and without optimizations at each count
seed:

```sh
cargo test --release --test listing_equivalence cycle_report -- --ignored --nocapture
```

A fourth test shows that a schedule reading Q one row before the quotient
lands is a difference, which the logical configuration does not see.

`crates/oxide-vcl/tests/optimizations.rs` checks each optimization on
small inline programs: the listing it changes, its `--opt=` and
`--no-opt=` names, and the same results in the timed configuration as
`--opt=none`.

## Renderer scenes

Random states run code paths at random rates: a renderer's clipper, rare in
a real frame, runs on most random states. `crates/oxide-vcl/tests/support/scenes.rs`
builds the VU1 memory the corpus renderers' hosts leave before MSCAL, from
the layouts they document, with geometry whose result is known:

- doom3-bfg-ps2's `world_strips` and `world_lm` (commit adc427d): the frame
  constants of `MakeFrameConstants` and `math::PerspectiveProjection`, and a
  cooked batch as `src/tools/assets/world.h` lays it out (header, index
  stream, s16 positions and UVs and RGBA bytes under STCYCL 4,1, then the
  batch state). Scenes: a wall ahead, all inside the guard band (the strip
  path); the floor under the eye, across the near plane (the list path and
  the clipper); a wall at a grazing angle; `world_lm` also lit, with its
  lightmap pass.
- quake2-ps2's `textured_triangles` and `particles` (commit 12f2f9a), as
  `vu1.h` lays them out: a wall and a floor of DrawVertex triangles, a
  keyframed box model in view and one across the near plane (lerped,
  shaded and backface culled on VU1), and a particle burst with a third of
  its particles outside the guard band.

`crates/oxide-vcl/tests/scenes.rs` runs the original's recorded listing on
each scene in the timed model and requires it to draw what the scene
predicts: every drawing packet's primitives with ADC clear, counted against
a reference Sutherland-Hodgman clipper on the programs' planes (and, for
the model, the programs' backface determinant), with no drawn vertex
outside the GS's coordinate space. Oxide mode's listings, with and without
optimizations, must then leave the same memory and send the same packets.
`scene_profile`, an ignored test, prints where each scene's cycles go:

```sh
VCL_PROFILE=<case> cargo test --release --test scenes scene_profile -- --ignored --nocapture
```

The scenes are synthetic: they follow the hosts' memory layouts, not
recorded frames, so how often each path runs is a model of a frame.

## Benchmarks

Two benchmarks measure what an optimization or a compile-time change does,
against tables committed in `tests/bench`, one line per measurement:

- `crates/oxide-vcl/tests/bench.rs` (`tests/bench/corpus.txt`): every public
  corpus program, compiled with its recorded flags in vcl and in oxide
  mode, and run in the timed model from the twelve seeded states of
  [Listing equivalence](#listing-equivalence). A row gives the listing's
  instruction pairs; how many runs finish, and their cycles and stall
  cycles, with the stalls by cause; and the compile's work. A rejected
  program is `rejected=1`. The same run writes `tests/bench/work.txt`, the
  compile's work by phase.
- `crates/oxide-vcl/tests/bench.rs`, `loops` (`tests/bench/loops.txt`): the
  same programs over a sweep of trip counts, 1 to 48, from count states
  whose memory's low 16 bits and VI01 to VI15 hold the count; a program that
  stops at `--cont` is resumed once, as MSCNT would, from the memory it
  left. A `total` row gives the cycles at each count that finishes. Then
  every table the runs entered has a row, attributed by the table comments
  the listing carries: a loop's kernel (`kernel:`, the `_LPOPT` table), its
  prologue (`..._ENTRY_POINT` and `..._PROn` together), its epilogues
  (`..._EPIn`) and its exit, or a table of its own (`table:`, such as a loop
  scheduled as straight-line code). A row gives how often the table was
  entered (its first pair's issues, a prologue's first table's: a kernel's
  iterations), its cycles and stall cycles, and its cycles per entry in
  hundredths: a kernel's `per100` is its cycles per iteration, measured
  apart from its prologue and epilogues. Its stalls are given by cause too. Programs whose counts come from
  elsewhere, such as sprite's constant 2, run at their own count throughout.
- `crates/vusl/tests/programs.rs`, `bench` (`tests/bench/scenes.txt`): the
  VUSL ports and their hand-written programs, compiled in oxide mode, with
  their pairs and work, with a `phase:` row for each phase, their cycles
  and stall cycles on each [renderer scene](#renderer-scenes), and their
  pipelined loops' parts over all the scenes, as in `loops`. A port's work
  counts all its hoist attempts, and the clipper library has a row of its
  own.

Every stall is attributed to what the pair waited for. When it waited on
several things, the cause is the one that was ready last, which set the
cycle it issued in; a tie goes to the first in this list. The causes are
written only when nonzero:

| Value | The pair waited for |
| --- | --- |
| `fmac` | a VF field an FMAC operation has not written yet: upper arithmetic, or a lower move such as MFIR, MFP or MOVE |
| `lq` | a VF field an LQ, LQI or LQD has not loaded yet |
| `ilw` | a VI register an ILW or ILWR has not loaded yet |
| `fdiv` | the FDIV unit, busy with an earlier DIV, SQRT or RSQRT |
| `efu` | the EFU, busy with an earlier operation |
| `waitq` | Q, at a WAITQ |
| `waitp` | P, at a WAITP |

The compile's work is `vcl::Compiled::stats` (`crates/vcl/src/stats.rs`):
how many times the whole program was compiled (`programs`: one, plus one
for each rewrite that compiles it again) and how many region compiles that
took (`regions`, retries included); the list scheduler's search nodes, the
wrangler's steps and checks, and the flow analyses with the blocks they
walk to their fixed points. The counts are also split by the phase that
did them:

| Phase | Work |
| --- | --- |
| `source` | Parsing, the source checks, and the rewrites that compile the program again (`steps`, `pointers`, `unswitch`, `hoist`, `speculate`, loop entry waits, the fixes' rounds, spills); the compiles a rewrite starts count to the other phases and to `programs` |
| `loops` | Expanding software-pipelined loops, with the modulo search for each loop's period |
| `prepare` | The rest of a region's preparation: pruning, copies, the flow analyses and webs |
| `schedule` | Each block's table: list scheduling and wrangling |
| `allocate` | Register assignment and allocation |
| `tables` | The table work after allocation (raw reduction, merged streams wrangled again, scratch choices) and emission |
 These depend
only on the program and the options, not on how many threads the searches
ran on ([SCHEDULING.md](SCHEDULING.md#parallel-searches)), so they measure a
compile-time change exactly where time, on a shared machine, does not; a change that alters
them changed what the searches do, not just how fast they run. The cycles
are the timed model's, so they are as good as the model is.

A benchmark prints what changed against its table: each changed row with
its old and new values, rows added or gone, and per group the totals of
each value over the rows both have, a value a row lacks counting as 0. A
row's group is the first word of its key (the mode, or `vusl` and `hand`)
and, for a loop part or phase row, its kind (`kernel`, `phase`), so those
rows total apart from the rows they are parts of. `BENCH_UPDATE=1` writes
the table instead, to commit with the change it measures, so that the diff
shows the change program by program:

```sh
cargo test --release -p oxide-vcl --test bench -- --ignored --nocapture
cargo test --release -p vusl --test programs bench -- --ignored --nocapture
BENCH_UPDATE=1 cargo test --release -p oxide-vcl --test bench -- --ignored
BENCH_UPDATE=1 cargo test --release -p vusl --test programs bench -- --ignored
```

The corpus benchmark and the loop sweep take about a minute and a half each
on eight cores, the scenes one about fifteen seconds. Both are ignored by `cargo test`: a table that
changes is a measurement, not a failure.

## Preprocessor

The preprocessor (`-G`, `-g`) follows vclpp 2, not the original, which runs
external programs ([PREPROCESSOR.md](PREPROCESSOR.md)), so its tests compare
with vclpp:

- `tests/preprocessor/<group>/<case>/` is vclpp's own test suite, 82 cases
  in `compat`, `errors`, `features`, `masp` and `warnings`, plus the port's
  cases for C's expression rules, where it departs from vclpp. `preprocessor.rs`
  runs `oxide-vcl <args> --preprocess-only input.vcl` in each case's
  directory: with `expected.vsm` it must succeed and print exactly that,
  without it fail and print nothing, and its diagnostics must equal
  `expected.stderr`. The outputs are vclpp's but for `--vcl-boilerplate`'s
  `.init_vi_all` and C's `?:`, `&&` and `||`; the diagnostics are vclpp's in
  the `Reporter`'s layout.
  `VCL_UPDATE_PREPROCESSOR=1` rewrites the expected files.
- `vclpp_made_corpus_inputs_come_out_the_same` preprocesses the sources in
  `tests/corpus/sources/` (doom3-bfg-ps2, quake2-ps2, tyra and ps2gl, at the
  commits the corpus headers name) with the vclpp flags each header records,
  and requires each of the 22 inputs back, but for the boilerplate's
  `.init_vi` line.

