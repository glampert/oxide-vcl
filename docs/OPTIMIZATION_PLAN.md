# Beyond the original: an optimization plan

The port reproduces the original VCL (vcl 1.4beta7) on every corpus we have: the 52
public programs, the 769 oracle fixtures it can run, and the two local
sets of disassembled VU programs. This chapter asks the next question: where does the
original leave cycles on the table, and which code generation changes could
win them back without changing what programs compute?

Two optimizations exist: `exit` (part of O9) and `loops` (O1 without
O10's budget option). The measuring and checking tools of section 5 exist; section 6
gives the order of the rest.

The ground rules:

- **Compatibility stays the default.** Optimizations run only in oxide
  mode (`--mode=oxide`); vcl mode's output stays identical to the
  original's. Each optimization has a name for `--opt=` so that it can be
  turned off on its own: oxide mode enables them all, `--opt=none` none,
  and `--opt=NAME,...` exactly those named.
- **Same results, bit for bit.** Every optimization in the main plan keeps
  the arithmetic each program performs, operation for operation. Rewrites
  that change rounding (for example `sqrt` then `div` into `rsqrt`) are
  listed separately as fast-math candidates and are not part of the plan.
- **Measured, not assumed.** Each item gives the evidence it rests on and
  the gain it can at most bring.

## 1. Timing model

These figures come from the VU User's Manual 6.0, sections 3.4 (pipeline,
hazards), 3.5 (termination) and 7.4 (throughput/latency list). The port's instruction table
(`crates/vcl/src/isa/instruction_data.rs`, matching vcl 1.4beta7) agrees with the
manual on every latency and throughput. The only difference is branches:
the manual counts the delay slot, so it lists 2. The timing model is
therefore correct; the gains described below are algorithmic.

| Class | Throughput / latency | Notes |
| --- | --- | --- |
| FMAC (upper arithmetic, `MOVE`, `MR32`, `MFIR`, `MFP`, `LQ`/`SQ`, `ILW`/`ISW`, `ITOF`/`FTOI`, `CLIP`, `RGET`/`RNEXT`) | 1 / 4 | Hazards are checked per x/y/z/w field; VF00 is never a hazard |
| Integer ALU (`IADD*`, `ISUB*`, `IAND`, `IOR`, `MTIR`, `XTOP`, `XITOP`, flag reads `FMAND`…) | 1 / 1 | Results bypass the dummy stages |
| `DIV`, `SQRT` | 7 / 7 | The FDIV unit is busy for 7 cycles; a second FDIV op stalls |
| `RSQRT` | 13 / 13 | |
| EFU (`ESADD` 10/11, `ERSQRT` 17/18, `ESIN` 28/29, `EATAN` 53/54, …) | latency = throughput + 1 | One EFU op at a time |
| `FCSET`, `FSSET` | 1 / 4 | |
| Branches | 1 issue + 1 delay slot | A conditional branch needs one instruction between the integer setter and the branch; flag checks are exempt |
| E bit | 1 delay slot | The slot may not hold branches, loads/stores, `xgkick`, `xtop`/`xitop`; results in flight still complete, FDIV/EFU results land in Q/P later |

Rules that matter for reordering:

- Q, P, ACC, I and the MAC/status/clip flags are **not interlocked**. A
  reader sees whatever value has landed by its issue cycle. Reordering
  around them must keep the exact landing order (the port already models
  this: `flow::Timing`, `WAITQ`/`WAITP`, the ABS cap).
- When an upper and a lower instruction write the same register in one
  cycle, the upper write wins for the **whole register**, even if the fields
  differ. Packing must never pair two writes to one register.
- An `xgkick` after another `xgkick` stalls the *following* instruction
  until the first PATH1 transfer ends.

## 2. Where the original leaves cycles

The tables below come from the original's own output. In `stdout.bin`,
every table has a header `; _LNOPT_w=` (straight-line) or `; _LPOPT_w=`
(modulo-scheduled loop kernel), and stalls the original knew about carry
`STALL_LATENCY ?n` / `STALL_THRUPUT ?n` notes. Loop prologues, epilogues
and their entry/exit points are `_LNOPT_w` tables with an empty strategy:
the loop optimizer installs them as they are and never reschedules them.
`tools/survey_output.py tests/corpus` prints these counts (section 5). A
copy is a `move`, or a `max` or `mini` of a register with itself; a delay
slot is the row after a branch; exit slots are the empty slots from an E
row to its table's end.

### Public corpus (52 programs)

| Kind | Tables | Rows | Stall cycles | Empty rows | Copies | Empty delay slots |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Loop kernels | 43 | 974 | 0 | 34 | 25 | 0 of 43 |
| Loop prologue/epilogue/entry/exit | 203 | 3,688 | 1,782 | 360 | 106 | 0 of 150 |
| Other straight-line streams | 616 | 6,026 | 1,413 | 462 | 50 | 141 of 328 |

Further straight-line slack:

- 187 leading empty rows (81 in loop parts, 106 elsewhere): the original
  keeps them as readiness debt for values arriving from predecessor
  streams.
- 63 exit tables carry 235 empty slots in their E row, delay slot and
  padding.

Across the 49 programs of the corpus's first survey the original logged
`time out..` 238 times: those searches were cut off by wall-clock limits,
not proven optimal.

The two local corpus sets of disassembled VU programs (117 programs, used only for aggregate
counts here) contain no loop hints, so all of their code is straight-line:
3,051 tables, 21,680 rows, 1,673 stall cycles, 1,368 copies, and 773 of
2,289 branch delay slots empty.

### Loop kernels against their bounds

For a kernel the resource bound is `max(upper ops, lower ops)` per
iteration, together with the FDIV/EFU unit occupancy. Most kernels already
reach it. The exceptions are:

| Kernel | Original period | Bound | Why |
| --- | ---: | ---: | --- |
| `general*` `pt_light_vert_loop` | 39 / 26 | 34 / 21 (upper) | The source asks for `--LoopExtra 5` |
| `general*` `final_loop` | 5 | 4 (lower) | `--LoopExtra 1` |
| `general_quad`, `general_nospec_quad` `xform_loop` | 48 | 45 | Periods 46 and 47 tried and abandoned (search timed out) |
| `general_tri`, `general_nospec_tri`, `general_pv_diff_tri` `xform_loop` | 36 | 35 | Period 35 abandoned |
| `general_pv_diff_quad` `xform_loop` | 51 | 50 | Period 50 abandoned |

Native runs of the original reach 47 for quad
([LPGEN_PORT.md](LPGEN_PORT.md)), which confirms that these are search
limits, not infeasible periods.

The port's deterministic search reaches the native result, and a larger
search does not get further. With its default step budget (20,000) and with
ten times that (`VCL_LPGEN_BUDGET=200000`), the port finds the same periods:

| Kernel | Period, budget 20,000 | Period, budget 200,000 | Bound |
| --- | ---: | ---: | ---: |
| `general_quad` `xform_loop` | 47 (12 s) | 47 (105 s) | 45 |
| `general_tri` `xform_loop` | 36 (16 s) | 36 (203 s) | 35 |
| `general_pv_diff_quad` `xform_loop` | 51 (6 s) | 51 (58 s) | 50 |

The compile time grows with the budget and the period does not move: the
original's list scheduler, given more time, does not find the bound. O2
needs a different scheduler, not a larger budget.

The resource bound itself is not fixed: several kernels spend bound slots
on bookkeeping that could be removed.

- **Duplicate pointer increments.** In 17 of the 43 kernels, two pointers
  advance by the same stride. Examples are `next_input` and `next_output`,
  both `+3`, in `general` `xform_loop`, and `VI02`/`VI03` in
  `fast_nolights`. In the lower-bound kernels (`general` `xform_loop`, 22
  lower ops; `fast_nolights`, 12) one increment is a whole period cycle.
- **Copies.** 25 copies (`move`, `max A,B,B`) sit in kernels. They come
  from lifetimes longer than the period and from source moves.
- **Loop invariants.** `pt_light_vert_loop` writes `atten.x = 1.0`
  (`addw.x atten, vf00, vf00`) every iteration. Nothing else in the loop
  writes that component, and the kernel is upper-pipe bound.

### Loops the original does not pipeline

The original pipelines only loops annotated with `--LoopCS`. Unannotated
single-stream loops are scheduled as straight-line code, so each iteration
pays the full latency chain. In the public corpus there are 13 distinct
such loops:

| Loop | Cycles/iteration (rows + stalls) | Resource bound |
| --- | ---: | ---: |
| `openvcl__examples_sprite_vu1Sprite` `vertexLoop` | 27 (10 + 17) | 7 (one `DIV`) |
| `quake2-ps2__src_ps2_renderer_vu1progs_particles` `lParticleLoop` | 40 | 19 |
| `tyra__engine_src_renderer_3d_pipeline_dynamic_core_programs_dynpip_c_vu1` `vertexLoop` | 59 | 36 |
| `tyra__engine_src_renderer_3d_pipeline_minecraft_programs_as_is_mcpip_as_is_vu1` `vertexLoop` | 30 | 23 |
| all 13 | 811 | 636 |

`sprite` shows the pattern most clearly: `lqi` → 4 FMAC → `div` → `mulq` →
`ftoi4` → `sqi` is one serial chain, and every link stalls.

## 3. Opportunities, by expected value

Each item lists its evidence, the most it can gain, a design sketch, and
risks. The gain is per execution of the code concerned. Kernels run once
per vertex; prologues, epilogues and setup run once per batch or per light.

### O1. Pipeline unannotated loops

*Evidence:* the 13 loops above; 811 → about 636 cycles per iteration in
total (−22%), and about 4× for `sprite`.

*Design.* Reuse the port's existing modulo scheduler and loop expansion
(`loop_schedule`, `loop_ir`, `expand_software_loops`). What the original
needs from `--LoopCS min,safe` can be derived instead:

- **Minimum trip count.** When it can't be proven, emit a guard: a trip
  count below the stage count branches to the unpipelined body, which is
  kept as a fallback. This costs code size; see the budget option in O10.
- **Speculation.** VU data memory loads never fault (addresses wrap inside
  VU memory), so loads and FMAC work of iterations that don't exist are
  harmless. Stores, `xgkick`, `isw` and Q/P/flag effects whose results
  escape must stay inside real iterations. That is the same "critical
  effect" rule the original enforces with its known-safe count.
- **Exit test.** The loop counter (`ibne`/`ibgtz` on an `iaddi`/`isubiu`
  counter, or a pointer compared with an end pointer) is recognised. Other
  loop shapes stay unpipelined.

*Risks:* code growth, and loops whose trip count comes in at runtime and
is often 1 or 2. Default to pipelining only when the kernel at least halves
the iteration cost or the user asks for it.

*The `loops` optimization* (`--opt=loops`, `with_derived_loop_hints` in
`crates/vcl/src/compiler/region.rs`) is the first step. vcl 1.4beta7
already pipelines a loop the design above describes, given
`--LoopCS count,safe`; what the source has to promise is only the
minimum trip count and how many iterations' stores may run past the last.
A loop vcl would pipeline but for the missing hint (one block of ordinary
code ending in a conditional branch to its own label) always runs its body
at least once, so `--LoopCS 1,0` promises nothing it does not do, and
vcl's expansion with a count of 1 keeps the early exits in the prologue:
no separate guard is needed. The optimization gives such loops that hint,
with three limits:

- **Profit.** The loop search tries only periods shorter than an iteration
  of the loop scheduled as straight-line code (`lpgen_optimize_loop`); if
  none is found, the loop stays as it is.
- **Size.** A region whose pipelined loops would take the program past the
  2048 instructions of VU1 micro memory is compiled without them.
- **Values from before the loop.** vcl's pipeliner times Q, P and the MAC,
  status and clip flags, which are not interlocked, between iterations but
  not from the code before the loop (defect F7 in
  [VCL_BUGS.md](VCL_BUGS.md#f7-values-in-flight-at-a-pipelined-loops-entry)).
  The `entry` fix, implied by oxide mode, waits for them before the loop: a
  WAITQ or WAITP row, or empty rows for a flag. A loop a branch can enter
  past the wait is left alone, and a loop that ends up not pipelined is
  compiled again without its wait, which would only cost cycles.

In the public corpus it pipelines 18 loops, from 1,002 cycles per iteration
in all as straight-line code (rows and stalls) to a period sum of 742
(−26%):

| Loops | Straight-line | Period |
| --- | ---: | ---: |
| openvcl examples `vertexLoop` (`sprite`, `triangle`, `triangle-gasp`, `triangle_strip`) | 27 each | 9 each |
| quake2 `particles` `lParticleLoop` | 40 | 19 |
| doom3 `world_lm` `lTransform`, `lFanLoop`; `world_strips` `lFanLoop` | 43, 36, 36 | 21, 28, 28 |
| tyra `dynpip` `vertexLoop` (c, d, tc, td) | 59, 107, 64, 125 | 36, 95, 48, 105 |
| tyra `mcpip` `vertexLoop` (as_is, cull) | 30, 45 | 23, 33 |
| tyra `stapip` `vertexLoop` (as_is d, td; cull d, td) | 67, 70, 84, 88 | 57, 60, 75, 78 |

quake2's particles, `dynpip_c` and `mcpip_as_is` reach their bounds. The
code grows: rows and stalls over the corpus go from 13,016 to 14,682, the
prologues and epilogues. In the cycle report (docs/TESTING.md), over the
public corpus and the probes at each count seed, one run of 733 is slower:
`checker-lat-status-loop` at one iteration, 9 → 10 cycles, from the rows
the `entry` fix adds for a flag. quake2's particles go from 515 to 277
cycles at the largest count.

### O2. Reach the bound in kernels the original's search gives up on

*Evidence:* quad 48 → 45 (47 natively and in the port), tri 36 → 35,
pv_diff_quad 51 → 50; section 2 shows that a larger search budget does not
close the gap. If
`--LoopExtra` becomes a hint the improved mode may beat, `pt_light` goes
39 → 34 and `final_loop` 5 → 4. These are the hottest loops of the
largest ps2gl renderers.

*Design.* The original's modulo search is a list scheduler with wall-clock
cut-offs. The port already has a deterministic step budget
([LPGEN_PORT.md](LPGEN_PORT.md)). In the improved mode:

1. Compute the bound properly: resource bound (upper, lower, FDIV and EFU
   occupancy) and recurrence bound (latency cycles through loop-carried
   dependences, including ACC, Q/P and flags).
2. Try periods from that bound upward with an iterative modulo scheduler
   (Rau's IMS, or swing modulo scheduling for lower register pressure).
3. Where the heuristic misses the bound, run an exact solver on small
   kernels: a CP/ILP model of slot, unit and latency constraints. VU
   kernels have tens of operations, which is well within reach.
4. Keep register allocation in the loop (the original rejects schedules it
   cannot allocate). Prefer, among schedules at the same period, the one
   with the fewest stages and the lowest register pressure.

*Risks:* compile time (cap the solver), and more stages meaning longer
prologues and epilogues (O3 offsets this).

### O3. Schedule loop prologues, epilogues and entry/exit points

*Evidence:* 1,782 stall cycles in 3,688 rows. In `general` the loop parts
cost 406 rows plus 209 stall cycles per batch, against 84 cycles per vertex
summed over its kernels. A typical case: the `xform_loop` epilogue
`EPI0` issues `opmula` two cycles after the `sub` it reads (`STALL ?2`),
while independent integer work sits in later rows.

*Design.*

1. Run the ordinary linear scheduler on each installed part, after
   expansion. Fix only the boundary timing towards and from the kernel; the
   port already computes it (`TimingContext::apply`).
2. Merge each epilogue with the stream that follows it, and each prologue
   with the setup code before it, so their empty slots take independent
   setup work. This builds on O5.
3. Where two epilogues exist (`EPI0`/`EPI1`, one per exit iteration
   phase), share their common tail.

*Risks:* low. The data flow is the same as the original's.

### O4. Pointer increments in kernels

*Evidence:* 17 kernels; in the lower-bound ones one fewer lower op is one
fewer period cycle (`general` `xform_loop` 22 → 21, `fast_nolights`
12 → 11).

*Design.*

- **Merge induction variables.** Two pointers with the same stride whose
  difference is a compile-time constant (both derived from one `xtop`
  value plus constants, as in ps2gl) become one register. Accesses through
  the second pointer get the offset folded into their displacement, which
  must stay within the signed 11-bit range. The loop test moves to the
  surviving register. The port's affine machinery (`AffineUse`, the
  displacement repair) already rewrites displacements across increments.
- **Post-increment folding.** When the accesses through a pointer in one
  iteration can use `lqi`/`sqi` and together add up to the stride (stride 1,
  or stride 3 with three qword accesses), the `iaddiu` disappears. Partial
  folding still helps when it shortens the lower count.

*Risks:* displacement overflow, and aliasing assumptions. The two pointers
must not be compared or stored as values.

### O5. Schedule across streams

*Evidence:* 1,413 stall cycles in straight-line streams, 187 leading empty
rows, 141 of 328 empty branch delay slots, and 235 empty exit slots in the
public corpus. In the local corpus, 773 empty delay slots. The per-light setup
`dir_light_loop_lid` (`esadd` latency 11 → `mfp` → `ersqrt` 18 → `mfp`)
stalls 278 cycles across the variants because nothing in its own stream
can overlap the EFU chain.

*Design*, in increasing order of reach:

1. **Fill delay slots and exit rows.** Move an independent instruction from
   before a branch into its slot. For an unconditional `b` or a fall-into,
   copy the target's first instruction into the slot and retarget the
   branch one instruction later; the target needs no other predecessor that
   relies on it. Place final stores in the E row (they can't go in its delay
   slot) and upper work in the delay slot.
2. **Superblocks.** Treat a chain of streams joined by fall-through, or by
   a branch to a block with a single predecessor, as one scheduling region,
   with compensation copies at side entries. This is where the leading
   empty rows and the setup chains get their independent work.
3. **Hoist across loops.** Move work such as the light setup above the
   preceding loop's epilogue, or overlap it with the previous light
   iteration, when its inputs are ready.

The port already keeps exact cross-stream timing (external minimum
positions, end distances, the original's path rules), so a region scheduler
can reuse it at the region boundary.

*Risks:* code duplication at side entries, and keeping Q/P/flag landing
order across the old stream boundaries. This is the largest piece of work
in the plan.

### O6. Eliminate copies

*Evidence:* 25 copies in kernels, 106 in loop parts, 50 elsewhere in the
public corpus; 1,368 in the local corpus (hand-written register shuffles). A
copy is an FMAC op with latency 4, so one on a dependence chain costs four
cycles, not one.

*Design.* Copy propagation and coalescing on the register webs the port
already builds (`construct_register_webs`). Replace reads of the copy's
destination with its source wherever the source isn't redefined first, and
delete the copy when it becomes dead. Partial-field copies (`move.xyz`)
merge only when the other fields don't conflict. For copies inside a
kernel that come from lifetimes longer than the period, use modulo variable
expansion: unroll the kernel ×2 (or ×k) and rename instead of copying,
within the code budget.

*Risks:* register pressure rises when lifetimes are merged. Drive it from
the allocator (O7), and keep a copy when removing it would cause a spill or
a forced-register conflict.

### O7. Register allocation that serves the schedule

*Evidence:* `quake2-ps2__src_ps2_renderer_vu1progs_textured_triangles` uses all 31 allocatable VF
registers and `tyra__engine_src_renderer_3d_pipeline_static_core_programs_cull_stapip_cull_td_vu1` 28. The original rejects loop
schedules that fail local allocation, so pressure costs period cycles.

*Design.*

- Rename before scheduling, so false WAR/WAW dependences never constrain
  the scheduler; the webs already give this per value. Then allocate after
  scheduling with interval colouring on the final rows. This is what
  `lifetime` and `allocation` do today, but without the original's
  external-then-local split, which pins choices early.
- Make scheduling pressure-aware where pressure is near 31: in the list
  priority, prefer instructions that end lifetimes.
- At joins, allocate the same register on all incoming paths to avoid
  copies, which feeds O6.
- Never spill: VU code has no stack. When pressure is too high, back off by
  scheduling a longer period or a later issue.

*Risks:* none for correctness beyond the existing checks. The gains are
indirect, through O2 and O6.

### O8. Memory and `xgkick` ordering

*Evidence:* the original orders an `xgkick` after **every** earlier store
(descriptor flags 0x60) and orders memory accesses by base register
conservatively. Renderers kick one buffer while filling the next, so the
stores for the next buffer are held back for no reason.

*Design.* Analyse each address as a base value plus an offset (the
`xtop`/`xitop` results plus constants, or loop pointers with known stride).
An `xgkick` then depends only on stores that can reach the buffer it sends:
from the kick address up to the GIF tag's length when that is a known
constant, otherwise everything, as now. Two accesses with the same base and
disjoint offsets never conflict. The port's alias kinds and timid
memory mode (`-M`) stay available as the conservative fallback.

*Risks:* GIF packet lengths computed at runtime. Fall back whenever the
range is unknown.

### O9. End bits and exits

*Evidence:* 235 empty exit slots (section 2). The original's final
compression can also delete the row carrying the E bit. That defect is
fixed: `--fix=end`, implied by oxide mode, keeps an E row and its delay
slot on every exit and `--cont` path
([VCL_BUGS.md](VCL_BUGS.md#f4-exit-rows-without-an-e-bit)). What remains is
the empty slots.

*Design.* Fill the E row and its delay slot as in O5 step 1, and drop the
safe-end padding rows: the manual guarantees that results in flight
complete after termination.

*The `exit` optimization* (`--opt=exit`, `Emitter::compact_exit` and
`Emitter::end_in_previous_tables` in `crates/vcl/src/compiler/region.rs`)
does the part that moves no instruction. vcl 1.4beta7 ends an exit table
with an E row and a delay row after its last instruction, usually both
empty. With `exit`:

- The last row that holds an instruction becomes the E delay slot, with the
  E bit on the row before it, when the manual allows it there (no branch,
  XTOP, XITOP, XGKICK, load or store). Otherwise that row carries the E bit
  and an empty delay row follows. The rows after the delay slot go.
- An exit table that holds no instruction, reached only by falling through
  from the table before it (none of its labels is named, and the table
  before ends without a branch), ends that earlier table instead. This is
  the stream that ends in XGKICK before `--barrier` and `--cont`.

Every instruction issues in the cycle it did; the program ends sooner. In
the public corpus 36 of 52 programs are a row and a cycle shorter (13,052
to 13,016 rows and stalls in all); in the local corpus 56 of 117 programs
lose 66 rows. Most exits end in an XGKICK or a store,
which may not run in the delay slot, so the delay slot stays empty: filling
it needs an independent instruction moved past them, which is O5 step 1's
reordering. With `-e` the optimization does nothing.

### O10. Code size budget

Pipelining (O1, O2), unrolling (O6) and superblock duplication (O5) all
grow code. VU1 micro memory holds 2048 instructions; the largest corpus
program uses 998 (`quake2`) and the ps2gl renderers about 600, but
renderers usually share micro memory with each other. The improved mode
needs a size budget option (absolute, or growth relative to compatibility
output) and a greedy pass that spends it where cycles per byte are best:
kernels first.

## 4. Not in this plan

- **Fast-math rewrites** (`sqrt`+`div` → `rsqrt`; `esadd`+`ersqrt` →
  FMAC sums plus `rsqrt`; reassociated multiply-adds): they change results,
  so they need a separate opt-in flag with documented error bounds, if
  anyone wants them at all.
- **Algorithm changes**, such as building the ADC bit with float ops
  instead of `iand`/`ior`/`mfir`: these belong to the program author.
- **VU0 macro mode** and COP2 interaction: out of scope for micro-mode code.

## 5. Infrastructure first

None of the above is safe without measuring and checking tools. They are
phase 0:

1. **Survey tool.** `tools/survey_output.py` parses the `_LNOPT_w` and
   `_LPOPT_w` tables and stall notes of a listing and counts, per table
   kind, rows, stall cycles, empty rows, copies, empty delay and exit
   slots and leading empty rows; `--kernels` lists each kernel's period
   against its resource bound. It reads the recorded outputs of a corpus
   or compiles each case itself in a mode (`--side vcl --side oxide`,
   `--side oxide:--opt=none`), and with two sides compares them per
   program and, with `--tables`, per table. Section 2's figures come from
   it.
2. **The switch.** Optimizations run only in oxide mode. Each has a name
   in `vcl::Optimization` for `--opt=`, which keeps the named ones (or
   `all`, the default, or `none`); `Options::optimizes` asks whether one
   runs. The first is `exit` (O9).
3. **Timed model.** `crates/oxide-vcl/tests/support/vu_model.rs` has a
   second configuration, `Config::TIMED`: pairs stall on VF fields still in
   the FMAC pipeline (latency 4), on ILW results, and on a busy FDIV or
   EFU; Q, P and the MAC, status and clip flags are not interlocked and
   change when their latency has passed; WAITQ and WAITP wait for them.
   Arithmetic is deterministic f32 within the VU's range, addresses wrap,
   and each XGKICK records the GIF packet it sends. It covers every
   instruction the corpus and probes use. Not modeled: the ordering of an
   XGKICK after an XGKICK, the integer branch hazard (the compiler keeps
   the setter one instruction away), and the VU's exact rounding.
4. **Equivalence checking.** `crates/oxide-vcl/tests/listing_equivalence.rs`
   runs two listings of one program from the same seeded states (random VF
   values, small VI values, data memory whose low bits make short counts
   and pointers) and compares data memory and the GIF packets kicked.
   Because the plan keeps every arithmetic operation, results must be
   bit-identical. Over the public corpus and the probes it checks that
   oxide mode with every optimization computes what `--opt=none`
   computes, and that oxide mode's code computes what the original's
   recorded code computes, except for the nine programs the alias fix
   repairs. A test shows that it catches a read of Q one row before the
   quotient lands, which an interlocked model accepts.

Still open: the model's cycle counts (`Execution::cycles`) are not yet
reported per program and path, so the survey still uses the static
`STALL ?n` notes; the timing has not been checked against the manual's
worked examples (figures 3-3 to 3-18); and the seeds do not aim trip
counts at 0, 1 or the stage boundaries of a pipelined loop. O1 needs the
last of these before it lands.

## 6. Phases

The order weighs measured gain against risk and effort. Effort is a rough
size: small is days, medium one to two weeks, large three to five weeks.

| Phase | Items | Effort | Expected result |
| --- | --- | --- | --- |
| 0 | Survey tool, `--opt=` switch, timed model, equivalence harness (section 5) | Medium | Baseline per table and per program; a check that oxide output computes what vcl output computes |
| 1 | O9 (`exit`: the E row on the last instructions) | Small | A row and a cycle off 36 of 52 public programs |
| 2 | O1 (pipeline unannotated loops) with O10 (code budget): `loops` and the `entry` fix | Large | About −22% over the 13 loops (811 → about 636 cycles per iteration), about 3× on `sprite` |
| 3 | O3 step 1 (reschedule loop prologues and epilogues) | Medium | Most of the 1,782 stall cycles in loop parts |
| 4 | O4 (merge induction variables, post-increment folding) | Medium | One cycle per iteration in lower-bound kernels (`general` `xform_loop` 22 → 21, `fast_nolights` 12 → 11) |
| 5 | O2 (bound-reaching modulo scheduler) | Medium to large | quad 47 → 45, tri 36 → 35, pv_diff_quad 51 → 50 |
| 6 | O6 (copies), O7 (allocation) | Large | Shorter chains, lower pressure; frees periods in high-pressure kernels |
| 7 | O5 (delay slots, superblocks, hoisting), O8 (memory/kick precision) | Very large | Setup chains and stream boundaries; the largest remaining static stalls |

Why this order:

- **Phase 1 first.** Exit-row placement moves no instruction, so each
  change is local and easy to check, and it exercised the Phase 0 tool
  chain on a small change.
- **Delay slots wait for O5's regions.** Moving an instruction into a
  delay slot saves a cycle only when its own row then empties. In the
  public corpus only 5 of the 372 branch delay slots with an empty upper
  slot, and 2 of the 63 E delay slots, have an upper instruction before
  them whose row would empty and that nothing between reads or overwrites
  (a static estimate, flags and timing aside): the rows before a branch are
  tight dependency chains. Filling them needs work from other streams,
  which is what O5's regions bring.
- **O1 is the largest measured runtime gain.** vcl's own pipeliner does
  the work given `--LoopCS 1,0` (the `loops` optimization), with the
  `entry` fix for the values it does not time from before the loop. What
  remains of O10 is a size option for programs that share micro memory.
- **O3 before O2 and O4** although they save more per vertex: O3 keeps the
  original's data flow, so its risk is low. It reschedules code that is
  already allocated, so reuse of physical registers limits what it finds.
- **O2 after O4**, because O4 lowers the bounds O2 aims at, and O2 needs a
  new scheduler (section 2). `--LoopExtra` stays honoured: the source asks
  for those cycles.
- **O5 steps 2–3 last**: the largest piece of work, with code duplication
  at side entries.

Each phase ends with the corpus measured on both outputs (cycles per
iteration, per batch, code size), the equivalence harness passing, and
vcl mode's output still identical to the original's.
