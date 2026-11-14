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
  4), and a reader sees the value of its issue cycle. WAITQ and WAITP wait
  until the change.
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
- RNEXT steps a deterministic stand-in for the hardware's random generator.

Not modeled: the stall of an XGKICK after an XGKICK, the integer branch
hazard, and the VU's exact rounding. The model's unit tests check field
interlocks, Q landing and WAITQ, flag timing, ILW interlocks, GIF packet
capture and saturating arithmetic.

## Listing equivalence

`crates/oxide-vcl/tests/listing_equivalence.rs` compares two listings of
one program by running both in the timed configuration from four seeded
states: VF values of mixed sign between 2^-4 and 2^4, VI values from 1 to
32, TOP below 512, and data memory whose low 16 bits are 12, 24, 36 or 48,
so that counts and pointers read with ILW are short and loops stepping by
1, 2, 3, 4 or 6 to a bound end. Two listings are equivalent when, on every
seed both finish, they leave the same data memory and kick the same GIF
packets. Listings with the same text are equivalent without running; a run
that reaches the 200,000-pair limit is retried with eight times the limit
when the other finished.

Over the public corpus and the probes it requires:

- oxide mode with every optimization to compute what `--opt=none` computes,
  and to accept the same programs;
- oxide mode's code to compute what the original's recorded code computes,
  except for the nine programs the alias fix repairs (`FIXED`);
- every oxide-mode listing to run in the model, except three whose
  symbolic registers or immediates cannot execute (`NOT_EXECUTABLE`).

These three compile the corpus twice and are ignored in debug builds; the
release suite runs them. A fourth test shows that a schedule reading Q one
row before the quotient lands is a difference, which the logical
configuration does not see.

