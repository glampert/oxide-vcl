# Beyond the original: an optimization plan

The port reproduces the original VCL (vcl 1.4beta7) on every corpus we have: the 52
public programs, the 769 oracle fixtures it can run, and the two local
sets of disassembled VU programs. This chapter asks the next question: where does the
original leave cycles on the table, and which code generation changes could
win them back without changing what programs compute?

Seven optimizations exist: `exit` (part of O9), `loops` (O1 without
O10's budget option), `unswitch` (O11), `steps` (O2's bookkeeping, and
O4's folding of repeated steps), `pointers` (O4 without post-increment
folding), and `hoist` and `speculate` (O5 step 1 without its delay-slot
filling); oxide-vusl also contracts single products (O12). The measuring and checking tools of section 5 exist; section 6
gives the order of the rest.

The ground rules:

- **Compatibility stays the default.** Optimizations run only in oxide
  mode (`--mode=oxide`); vcl mode's output stays identical to the
  original's. Each optimization has a name for `--opt=` so that it can be
  turned off on its own: oxide mode enables them all, `--opt=none` none,
  `--opt=NAME,...` exactly those named, and `--no-opt=NAME,...` all but
  those named.
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

The compile time grows with the budget and the period does not move. What
keeps these kernels off their bound is their own bookkeeping, not the
search: each steps its pointers once per vertex, four or three
`iaddiu` per pointer, and each step takes a slot of the lower pipe. With
the steps folded (`steps`, O2) the same search reaches the bound: quad
45, tri 35, and pv_diff_quad 45, whose bound falls to 45 as well.

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

### Renderer scenes

The renderer scenes of docs/TESTING.md run doom3's and quake2's programs on
the memory their hosts send. Cycles in oxide mode, and where they go:

| Program and scene | Cycles (`--opt=none` → all) | Where the optimized cycles go |
| --- | ---: | --- |
| doom3 `world_lm`, wall (strip path, nearly every batch) | 3,372 → 2,207 | `lTransform` kernel 47%; the strip emit loop `lStripRoom` 44%, issue-bound |
| doom3 `world_lm`, floor (clipped) | 47,417 → 43,419 | the five clip passes 83%, a quarter of it stalls |
| doom3 `world_strips`, wall | 3,896 → 3,896 | `lTransform` 75%, 952 of its 2,933 cycles stalls |
| quake2 `textured_triangles`, world wall | 4,783 → 2,805 | `lTriangleLoop`'s copy for the world's colour mode (O11) |
| quake2 `textured_triangles`, world floor (clipped) | 18,691 → 15,863 | the clip passes |
| quake2 `textured_triangles`, model | 1,858 → 1,578 | `lKeyframes` in the model's copy (O11) |
| quake2 `particles`, burst | 2,435 → 1,189 | the pipelined particle loop |

`world_strips`' transform loop has a `--barrier` in its body, which makes
it two streams, so neither vcl nor the `loops` optimization pipelines it;
`world_lm`'s has none. Without that barrier, oxide mode pipelines it and
the wall scene takes 2,209 cycles (−43%), with the same packets.

What remains on doom3's usual path is not the scheduler's to find.
`lTransform`'s kernel sits at its bound, 21 upper instructions per
vertex. The strip emit loop is bound by its lower instructions, 14 per
vertex with an empty upper pipe, so pipelining it past its window-full
branch would gain nothing. Three of those 14 repeat work: the `iMask`
constant is set again for each of the four vertices, and each vertex
adds to `iOutPtr` and `iVertsLeft`. `steps` (O2) folds the additions,
which takes the `world_lm` wall from 2,259 to 2,207 cycles (−2.3%);
folding the constant too, by hand, took it to 2,106 (−7% in all):
repeated-expression removal within a block, applied to a loop that is
not pipelined. On the clipped scenes the clip passes stall a
quarter of their cycles across their blocks (O5).

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
with these limits:

- **Profit.** The loop search tries only periods shorter than an iteration
  of the loop scheduled as straight-line code (`lpgen_optimize_loop`); if
  none is found, the loop stays as it is.
- **Compile time.** The search has a work budget in position checks
  (`DERIVED_LOOP_WORK`): 200,000 per strategy and 20,000,000 for the whole
  search, every period and strategy, which is about two seconds of a release
  build. A loop hinted in the source searches as vcl does, with the step
  budget alone. Most of the cost is the wrangler's setter drags: in a period
  it cannot schedule, one strategy can make tens of millions of checks within
  its 20,000 steps on a body of 50 to 70 instructions. The strategies that
  win make far fewer, at most about 311,000 over the public and local
  corpora. With the budget, every derived loop in them gets the same period
  as without it, and all but two get the same schedule. A 70-instruction
  branch-free clip pass, which takes four minutes to search unbounded, gives
  up within the budget and stays as it is.
- **Opt-out.** `--LoopCS 0,0` in a loop leaves it as it is, without the
  warning vcl mode prints for a count of 0. The original reads the same source
  as a loop without a hint.
- **Size.** A region whose pipelined loops would take the program past the
  2048 instructions of VU1 micro memory is compiled without them, and so is
  a region whose registers no longer allocate with them.
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

*The `steps` optimization* (`--opt=steps`, `--no-opt=steps`;
`crates/vcl/src/compiler/steps.rs`) removes the bookkeeping that keeps the
evidence kernels off their bound. It rewrites the parsed program before
`pointers` does:

- A block is a run of ordinary instructions between labels, branches,
  barriers, raw code and directives. In a block, steps of one register by
  an immediate (`iaddiu r, r, k`, `isubiu r, r, k` or `iaddi r, r, k`, all
  of one of those) make a run as long as nothing between them reads or
  writes the register, except as the base of an `lq`, `sq`, `ilw` or
  `isw` with an offset.
- The run becomes its last step, by the sum, when the sum fits the
  instruction's immediate field. The accesses between its steps take the
  steps before them into their offsets, which must stay within the signed
  11-bit field (`steps_moves_the_steps_before_an_access_into_its_offset`,
  and `steps_keeps_a_run_whose_pointer_is_read_between` for a read that
  ends a run).
- The register holds what it held after each run and every access reads
  the address it read, whatever a loop's `--LoopCS` promises: unlike
  `pointers`, it changes nothing a schedule could expose.

Expanding a kernel already merges the steps it holds, but the search
gives each step a lower-pipe slot first, so the period pays for them
(`steps_folds_a_run_of_steps_into_one`: 10 cycles, 8 folded).

*Measured* (`tests/bench/loops.txt`, `tests/bench/scenes.txt`): the ps2gl
`xform_loop` kernels reach their bounds, quad and `nospec_quad` 47 → 45,
`pv_diff_quad` 51 → 45, the three `tri` 36 → 35. Those kernels are a
small part of their programs' runs, where the lighting loops take most of
the cycles: the whole of `general_quad` is 7 cycles faster at 4 vertices
and 32 at 48. The `tri` kernels' shorter period comes with a deeper
schedule, a prologue and a longer epilogue: 6 cycles slower at 3 vertices,
7 faster at 48, and 9 to 14% more code, where `general_quad` and
`nospec_quad` have 13 and 18% less. doom3's hand-written `world_lm` strip and lightmap loops
take 5% fewer cycles a vertex, and its wall scene 2.3% fewer (lit, 3.0%).
No VUSL port changes: oxide-vusl steps a pointer once per element.

*What remains* of O2 after `steps`, against the first period each search
tries: tyra's `dynpip_d` 95 against 93, and openvcl's sprite and
triangle examples 9 against 7, in loops of two or three iterations. The
kernels with `--LoopExtra` sit at their bound plus what the source asks
for (`pt_light_vert_loop` 39, 34 without the hint).

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

*Measured, step 1.* Each installed part (prologue, epilogue, entry and
exit point) was scheduled again by the linear wrangler, timed against the
kernel and its other parts as ordinary streams are. The new rows were kept
only when they were shorter, when no Q, P or flag value was in flight
across the part's boundary, and when every read of the region still saw
the same definitions by timed flow. The listings computed what they did
before, but the gain was small:

| Measure | Before | After |
| --- | ---: | ---: |
| Public corpus, rows and stalls in oxide mode | 15,239 | 15,109 |
| Local corpus, sets A and B | 26,058 | 26,047 |
| VUSL scenes, the ports' cycles | 252,414 | 252,416 |
| VUSL scenes, the hand-written programs' cycles | 293,419 | 293,380 |

Most of the public corpus's 130 is one epilogue, ps2gl's `pt_light`
`EPI0` (93 cycles to 57 in three programs, once per point light). In the
cycle report most changed programs gained one to three cycles a run; tyra's
`stapip_cull_tc` lost 2 and textured_triangles' world floor scene 10, as
fewer rows before compression are not always fewer cycles after it. A part
is mostly one iteration's dependency chain (`pt_light`'s `EPI2` waits on
`esadd`, `ersqrt` and then a run of dependent `mul.w`), which no order
within the part shortens, and raw reduction and compression already remove
the expanded rows' slack. So step 1 is not part of oxide mode. What remains
is overlapping a part with the code beside it, step 2, which needs O5.

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

*The `pointers` optimization* (`--opt=pointers`, `--no-opt=pointers`;
`crates/vcl/src/compiler/pointers.rs`) rewrites the parsed program before
`unswitch` does, in two ways:

- **Merged pointers.** Two registers a loop steps once an iteration by the
  same immediate, in the block that ends with its branch back, whose
  difference at the loop's entry is a constant, become one. A forward pass
  over the program gives each VI register's value as a base plus a
  constant (VI00, the register's incoming value, or what a statement last
  wrote, which the statement's next run replaces), and every way into the
  loop must agree. The register that goes may only be the base of `lq`,
  `sq`, `ilw` and `isw` in the loop, must be dead where the loop exits, and
  its accesses must keep their offsets in the signed 11-bit field.
- **A pointer for the count.** A register stepped by as much as a pointer
  and tested with `ibne` or `ibeq` against one the loop does not write is
  replaced in the test by the pointer, compared with where it is when the
  count runs out: `p + t - c` or `p + c - t` at the entry, computed on the
  way in, which is exact in 16-bit arithmetic. The loop must be one block
  entered only by falling into it, with the count only stepped and tested
  and dead after it.

Both apply only to loops with at least as many lower instructions as upper
ones, and whose `--LoopCS`, if they have one, promises a minimum of 1 and
a safe count of 0. A larger minimum or a safe count lets the pipeliner's
schedule decide what runs at fewer iterations and how many stores run past
the end (`mem-loop-load-store`, `--LoopCS 6,1`, stores one iteration past
its end as written and none rewritten). The hint allows either, but the
program would compute something else there. Each rewrite is then compiled
on its own and kept only when the sum of the program's kernel periods
falls and no kernel gets longer: a step less can also move the pipeliner's
search to a deeper schedule of the same period (`loop-entry-q-in-flight`),
or to a longer one (tyra's `dynpip_d` would go from 95 to 97; the upper
pipe bounds its kernel). A program with candidates compiles once more for
each.

*Measured* (`tests/bench/work.txt`): one kernel of the public corpus is a
cycle shorter, tyra's `mcpip_as_is` 23 → 22, which takes 3.3% fewer cycles
at 48 vertices and up to four more at six or fewer, where the count's end
costs more than the iterations save. ps2gl's lower-bound loops would gain
too (`fast_nolights` 12 → 11, `general` and `general_nospec` `xform_loop`
22 → 21), but they promise a safe count (`--LoopCS 1,3`). The local corpus
and the VUSL scenes do not change; the VUSL ports already share one pointer
where they can. The other lower-bound kernels need what this does not do:
doom3's and quake2's fan loops count down with `ibgtz`, which no pointer
test matches, and `stapip_as_is_c` counts down in a loop that `unswitch`
splits. Folding a block's runs of steps into one is `steps` (O2);
folding steps into `lqi`/`sqi` post-increments is not done.

### O5. Schedule across streams

*Evidence, static:* 1,413 stall cycles in straight-line streams, 187
leading empty rows, 141 of 328 empty branch delay slots, and 235 empty exit
slots in the public corpus. In the local corpus, 773 empty delay slots. The
per-light setup `dir_light_loop_lid` (`esadd` latency 11 → `mfp` →
`ersqrt` 18 → `mfp`) stalls 278 cycles across the variants because nothing
in its own stream can overlap the EFU chain. Most of this code runs once
per batch or per light.

*Evidence, at run time:* the stalls that cost cycles in the renderer scenes
sit in loops with branches, which no pipeliner takes. The profile report of
`crates/vusl/tests/programs.rs` (`VUSL_PROFILE=<case> cargo test --release
-p vusl --test programs profile -- --ignored --nocapture`) gives each
label's cycles. For the VUSL ports in oxide mode, the clip passes'
Sutherland-Hodgman loops (`clipPass` in
`tests/vusl/programs/include/clipper.vusl`, and textured_triangles' own)
take:

| Scene | Cycles | Stall cycles | In the clip-pass loops |
| --- | ---: | ---: | --- |
| `world_lm` floor | 30,936 | 5,016 | 21,098 cycles (68%), 4,792 stalls (96%) |
| `world_lm` side | 8,089 | 777 | 2,740 cycles (34%), 622 stalls (80%) |
| `textured_triangles` world floor | 12,586 | 2,064 | at least 6,863 cycles (55%), 1,944 stalls (94%), in the five loop headers alone |
| `world_lm` wall, `textured_triangles` model | 2,131, 1,175 | 9, 0 | none |

The unclipped scenes have almost no stalls left, so O5's gain is on
clipped batches. A clip pass's body is five streams: a header that loads
the corners and computes their plane distances, a conditional store of the
corner, a second test, a conditional intersection and the latch. On the
usual path (corner kept, edge not cut) an iteration of the near pass costs
39 cycles as written, and about 35 with `hoist` (below), which moves the
second test's `subx.w` and `fsand` and the latch's steps into the header:

| Stream | Cycles | Stall cycles | With `hoist` | Stall cycles |
| --- | ---: | ---: | ---: | ---: |
| Header | 23 | 8 | 26 | 10 |
| Store the corner | 5 | 0 | 5 | 0 |
| Second test | 7 | 3 | 2 (`ibeq`, an empty delay slot) | 0 |
| Latch | 4 (`isubiu`, an empty row, `ibgtz`, delay slot) | 0 | 2 (`ibgtz`, an empty delay slot) | 0 |

`speculate` (below) leaves this path as it is: what it moves into the
header takes stall cycles there, and the header still takes 26.

The header is one dependency chain, `lq` → `mul` → `sub` → `mul` → `mul`
→ the flag test and its branch, and no order within it is shorter. Moving
the chain to another stream does not help: rotating `clipPass` in the
source, so that each iteration computes the next corner's distance in the
latch, puts the whole chain in the latch (18 cycles), adds two copies, and
makes the floor scene 4.5% slower. VU latency overlaps only with
independent instructions issued after it, so the chain's instructions have
to be placed in later streams and later iterations, which only a scheduler
working across streams can do.

*Design*, in decreasing order of measured value:

1. **Move work between streams that run on every path, and fill delay
   slots.** A stream dominated by another that it also post-dominates, in
   the same loop, runs exactly when that one does (in the clip pass: the
   header, the second test and the latch). Before scheduling, an oxide-mode
   rewrite, like `unswitch` and `pointers`, moves independent instructions
   up into the earlier stream: the second test's `subx.w`, the latch's
   counter decrement and pointer increment. It also speculates
   side-effect-free instructions of a conditional stream whose results are
   dead on the other paths, such as the intersection's `sub`s, into the
   header's stall cycles. Every FMAC instruction writes the MAC and status
   flags, so each flag read (`fsand`, `fmand`) must still see its setter's
   value at its own cycle; Q and P keep their landing order. Delay slots
   and exit rows are then filled from the moved work: move an independent
   instruction from before a branch into its slot; for an unconditional `b`
   or a fall-into, copy the target's first instruction into the slot and
   retarget the branch one instruction later when the target has no other
   predecessor that relies on it; place final stores in the E row and upper
   work in its delay slot. *Expected* (from the tables' rows): about 6 of
   the 39 cycles per corner on the usual path. Medium effort. The `hoist`
   and `speculate` optimizations below are this step without its delay
   slots and exit rows: `hoist` gains about 4 of those cycles, and
   `speculate` shortens the intersection, which is off the usual path.
2. **Pipeline loops with branches.** The streams that every iteration runs
   form the kernel, and the conditional streams stay as holes on their
   paths. Later corners' loads and distance chains issue in earlier
   iterations' streams. Speculation is safe on the VU: loads do not fault
   and addresses wrap, and an FMAC result has no side effect but the flags,
   whose readers keep their exact timing as in step 1. In the clip pass the
   streams every iteration runs hold about 9 upper and 8 lower instructions
   and 3 branches per corner; with the store, the usual path would take
   about 18 to 20 cycles instead of 35. *Expected:* at best −25 to −35% on
   the floors and about −15% on the `world_lm` side; register pressure (two or three corners in flight)
   and the flag timing will take part of that. This is a new modulo
   scheduler, as O2 needs, so it extends O2's rather than building a second
   one. Large to very large effort.
3. **Superblocks and hoisting for setup code.** Treat a chain of streams
   joined by fall-through, or by a branch to a block with a single
   predecessor, as one scheduling region, with compensation copies at side
   entries, and move work such as the light setup above the preceding
   loop's epilogue, or overlap it with the previous light iteration, when
   its inputs are ready. This is where the static stalls, the leading empty
   rows and the setup chains get their independent work. *Expected:* little
   in the measured scenes, since that code runs once per batch or per
   light; ps2gl's per-light EFU chain is the best case.

The port already keeps exact cross-stream timing (external minimum
positions, end distances, the original's path rules), so each step can
reuse it at the boundaries of the streams it changes. All three are in
oxide-vcl, so hand-written programs compiled in oxide mode gain too; the
hand-written `world_lm` floor scene stalls 10,198 of its 43,419 cycles.

*Risks:* keeping Q, P and flag landing order across the old stream
boundaries, register pressure from speculated values, and code duplication
at side entries (step 3). Steps 2 and 3 are the largest pieces of work in
the plan.

*The `hoist` optimization* (`--opt=hoist`, `--no-opt=hoist`;
`crates/vcl/src/compiler/hoist.rs`) does step 1's moves between streams that
run on every path, on the parsed program, after `pointers` and `unswitch`
have settled it:

- A loop is a label and the last conditional branch back to it, with only
  ordinary code between them (no barrier, raw code, region or register
  branch), no loop inside it and no branch from outside into it past its
  first labels. Its blocks start at labels and end after branches, and one
  iteration ends at a branch back to the header or out of the loop.
- A block runs exactly when an earlier one does when the earlier one
  dominates it and it post-dominates the earlier one, within the iteration.
  Each instruction of such a block moves to the end of the earliest of
  them, before its branch, unless it conflicts with something it would
  move above: that branch, every block on the paths between the two, and
  what stays above it in its own block.
- Two instructions conflict when one writes a register the other reads or
  writes (the implicit ACC, Q, P, I, R and MAC included), when a load
  would move above a store, or when both touch the status and MAC flags,
  or both the clip flags. Every FMAC instruction writes the flags, so a
  flag read still sees the writer it saw
  (`hoist_keeps_a_flag_writer_below_a_flag_read`). Stores, branches,
  `xgkick` and the other external and random-unit instructions stay where
  they are.
- Every path runs the same instructions with the same dependencies, so the
  program computes the same values. The moved program is compiled once
  more and kept for each loop whose moved streams walk fewer rows in all,
  stall rows included: the last count of a table's comment, which is what
  a stream takes when it is entered with nothing in flight (the first
  count is a lower bound, not a cycle count). When only some loops gain,
  the program is compiled once more with just those.

It leaves the second test and the latch each a branch and an empty delay
slot. Filling those slots needs work from the next iteration, which is
step 2's. Rotating the header's first load into the latch's slot, with a
copy before the loop, takes one row off the header's table, but the load's
latency is then still in flight when the header starts, so it is not
done. The intersection's first work goes into the header's stall cycles
instead, where its latency is spent before the intersection starts
(`speculate`, below).

*Measured* without `speculate`: the near pass's usual path takes about 35
cycles a corner instead of 39, and the
clipper library is 443 instructions instead of 458. The stalls stay as
they were: what goes is the rows the test and the latch spent on their own
work, and the empty row before the latch's branch.

| Scene (VUSL port) | Without `hoist` | With it | Change |
| --- | ---: | ---: | ---: |
| `world_lm` floor | 33,839 | 32,336 | −4.4% |
| `world_lm` side | 8,367 | 8,169 | −2.4% |
| `world_strips` floor | 40,107 | 38,316 | −4.5% |
| `world_strips` side | 15,847 | 15,283 | −3.6% |
| `textured_triangles` world floor | 13,664 | 13,016 | −4.7% |
| `textured_triangles` model near | 3,186 | 3,081 | −3.3% |

The scenes without clipping do not change. The hand-written programs
compiled in oxide mode gain too (`world_lm` floor 46,153 → 44,959, −2.6%). In the public
corpus five programs change, all faster: the three renderer programs and
openvcl's `crossloop_4regs` and `loop_pointer` repros, −2.3% over the
seeded runs in all. A program with a loop it changes compiles once more:
about 0.4 s more for doom3's `world_lm`, 1.2 s for quake2's
`textured_triangles`.

*The `speculate` optimization* (`--opt=speculate`, `--no-opt=speculate`;
`crates/vcl/src/compiler/speculate.rs`) does step 1's speculation, on the
program `hoist` leaves, in the loops `hoist` takes and with its blocks:

- A block that does not post-dominate the header runs on some iterations
  only. It gives its first work to the earliest block that runs on every
  iteration and dominates it, the receiver: each instruction that no other
  instruction of the block produces inputs for, and each division or EFU
  instruction whose producers in the block all move (in the clip pass:
  the cut point's `sub.w` and `div`, and the three corner differences).
  What reads Q or P stays, so the latency is spent in the streams between.
- Each goes as far up into the receiver as `hoist`'s register and memory
  conflicts allow, with nothing on the paths between the two blocks in its
  way, and only when its results are dead on the paths that skip its
  block: a search from where it lands, through the rest of the program,
  finds no read of a field it writes before that field is written again.
  An exit reads the outputs it declares; raw code, register branches and
  calls read everything. It writes no hardware register by name and no
  declared one, Q, P, ACC and I included only by that search
  (`speculate_keeps_work_whose_register_the_other_path_reads`,
  `speculate_keeps_a_division_whose_q_the_other_path_reads`).
- The flags follow `analysis::status`: an FMAC instruction writes the ALU
  status and MAC flags and a division the divide status, and a test reads
  those its mask selects. For each of them that some test in the program
  reads, the moved instruction's write must be one nothing reads: after it
  in its block, and after where it lands in the receiver, the next
  instruction that touches that flag writes it
  (`speculate_keeps_a_flag_writer_its_own_stream_tests`, and
  `a_flag_writer_does_not_land_where_a_test_would_read_it` in
  `compiler/speculate.rs`: compiled, that move would also delay the test,
  which the comparison of streams rejects on its own). Flag
  tests, clip-flag instructions and writers of the sticky flags stay, and
  so does every status writer when a test reads the sticky flags, which
  the moved work would add to on the paths that skip it.
- The program is compiled once more and kept for each loop whose blocks
  that gave work walk fewer rows in all with none of its other blocks
  walking more; the receivers run on every path, so the usual path costs
  no more. A block without a label is matched to its table by its place
  among the loop's tables. When only some loops gain, the program is
  compiled once more with just those.

*Measured* (`tests/bench/scenes.txt`, `tests/bench/corpus.txt`): each clip
pass's header still walks 28 rows, now holding the moved work in its stall
rows, and its intersection walks 11 instead of 21. The clipper library
stays at 443 instructions.

| Scene (VUSL port) | With `hoist` | With `speculate` too | Change |
| --- | ---: | ---: | ---: |
| `world_lm` floor | 32,336 | 30,936 | −4.3% |
| `world_lm` side | 8,169 | 8,089 | −1.0% |
| `world_strips` floor | 38,316 | 36,916 | −3.7% |
| `world_strips` side | 15,283 | 15,123 | −1.0% |
| `textured_triangles` world floor | 13,016 | 12,586 | −3.3% |
| `textured_triangles` model near | 3,081 | 3,001 | −2.6% |

The scenes without clipping do not change, and the headers take as many
cycles as before on every scene. The hand-written programs compiled in
oxide mode gain too, and are 12 instructions smaller each: `world_lm`
floor 44,959 → 43,419 (−3.4%), `world_strips` floor 46,177 → 44,637
(−3.3%), `textured_triangles` world floor 16,336 → 15,863 (−2.9%).
In the public corpus four programs are faster, the three renderer
programs and openvcl's `loop_pointer` repro, −2.5% over their seeded runs
in all; six more, openvcl's test fixtures and ps2gl's `scei`, are 2
instructions smaller with the same cycles; none is slower. A program with
a loop it changes compiles once more: the hand-written renderer programs'
compiles and the clipper library's do half as much work again, and the
VUSL ports' compiles, which try several programs, compile 13 programs
instead of 11 for `world_lm` and 27 instead of 21 for
`textured_triangles`.

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

Pipelining (O1, O2), unswitching (O11), unrolling (O6) and superblock
duplication (O5) all grow code. VU1 micro memory holds 2048 instructions;
the largest corpus program uses 998 (`quake2`, 1,212 with `unswitch`) and
the ps2gl renderers about 600, but renderers usually share micro memory
with each other. `loops` and `unswitch` keep a program within micro
memory and spend no budget beyond that; `--no-opt=` turns off the one
that costs too much. The
improved mode needs a size budget option (absolute, or growth relative to
compatibility output) and a greedy pass that spends it where cycles per
byte are best: kernels first.

### O11. Unswitch loops on a mode

*Evidence:* quake2's `lTriangleLoop` tests `iColorMode`, which the host
sets per batch and the loop never writes, twice for each of a triangle's
three vertices: once around the lighting and once around the colour
write. Each test ends a stream, so the work of one vertex cannot overlap
the next one's, and the loop's blocks stall 40–50% of their cycles in the
renderer scenes. tyra's `stapip` programs test a single-colour flag once
per vertex in the same way.

*The `unswitch` optimization* (`--opt=unswitch`, `--no-opt=unswitch`;
`crates/vcl/src/compiler/unswitch.rs`) rewrites the parsed program before
it is compiled:

- A loop is a label and the last conditional branch back to it. Its
  *usual path* is every block except inner loops and *special cases*. A
  special case is what an arm of a two-armed branch reaches before the
  code both arms share, when it leads to an inner loop the other arm does
  not reach: quake2's clipping and warp code, both of which end in the
  clipped polygon's fan loop. A branch that skips straight to the shared
  code (an early out) marks what it skips only when every block of it is
  on the way into an inner loop or back out of one: the VUSL port's
  `if (slow)` around its seeding, warp, clipping passes and fan loop is a
  special case, while the hand-written program's backface cull, which
  skips the usual output as well as the clipping code, is not. An inner
  loop is the blocks on a cycle that does
  not pass through the header, found from the control flow: a branch to
  an earlier label that only runs forward, such as a usual arm placed
  after a special case and branching up to it, makes none, so where the
  blocks sit in the source does not change the usual path.
- The condition is the most frequent `ibeq`/`ibne`/`ibgtz`/`iblez`/
  `ibltz`/`ibgez` on the usual path whose registers no usual-path
  instruction writes. The usual path is copied once for each outcome; in
  each copy the branches with that test or its opposite become jumps or
  disappear, and code no longer reached goes with them.
- Everything that enters the usual path from elsewhere (the code before
  the loop, a special case falling back into it, a branch from outside,
  a `.global` label) goes through a dispatch that keeps the original
  label and tests the condition again. A special case may therefore write
  the condition's registers.
- Loops are unswitched one at a time, outermost first. Each of a loop's
  two copies is a loop of its own and is unswitched once more when it
  tests another mode it never writes, so a loop with two such modes (a
  colour form and a vertex format) has a copy for each pair of outcomes,
  four in all. A copy of a copy is not unswitched again. An inner loop in
  shared code can be unswitched on its own: quake2's fan loop is.
- When the result fails to compile or would not fit in VU1 micro memory,
  the loops are unswitched once only, without the second level; when that
  does not fit either, the program is compiled as written.

The copies join the code between the tests into longer streams, and a
copy that becomes a loop of one block is pipelined by `loops`. In the
public corpus it changes 5 programs:

| Program | Rows (`--no-opt=unswitch` → all) | Cycles at 12 iterations (equivalence state) |
| --- | ---: | ---: |
| quake2 `textured_triangles` | 997 → 1,212 | 2,706 → 2,569 |
| tyra `stapip_as_is_c` | 59 → 125 | 144 → 116 |
| tyra `stapip_as_is_tc` | 70 → 98 | 172 → 151 |
| tyra `stapip_cull_c` | 83 → 192 | 233 → 194 |
| tyra `stapip_cull_tc` | 89 → 259 | 245 → 210 |

The equivalence state's memory is random, not a renderer's batch. On the
renderer scenes the world wall goes from 4,782 to 2,805
cycles (−41%), the clipped floor from 18,690 to 16,768, and the model from
1,857 to 1,578 (−15%). The tyra programs grow most because both copies of
their vertex loops are pipelined, each with its prologue and epilogue.
`--no-opt=unswitch` keeps the smaller program where micro memory is
shared.

No loop in the public corpus tests a second mode it never writes, so the
second level changes none of those listings: quake2's reads its vertex
format from memory every triangle. Held in a register for the whole batch,
with the vertex advance moved into each format's block and the triangle
written out ahead of its clip decision, the loop splits by colour form and
then by format, and on the renderer scenes the world wall takes 1,754
cycles and the model 1,192 (1,840 and 1,344 with one level).

*Limits:* two conditions per loop, four copies at most; the special-case
rule is structural, so a rare arm without an inner loop is copied with the
usual path (quake2's window flush).

### O12. Contract one product and a sum into an ACC chain (VUSL)

*Evidence:* `a * b + c` was `mul` then `add`. Both forms take two
instructions, but the `add` waits for the `mul`'s result (four cycles of
FMAC latency), whereas ACC has no data hazards (VU User's Manual, §3.4.1),
so `mula acc, a, b` followed by `madd d, c, vf00[w]` issues back to back.
The product is rounded before the add in both forms, and `c` times 1.0 is
`c`, so the result is the same bit for bit.

*In oxide-vusl* (`contract`, `crates/vusl/src/codegen/mod.rs`): a sum
with one product and at least one other term is a chain too, with `msub`
for a negative last term. An added product goes first, so the chain
claims ACC only once the product's operands are ready. With the other
term first (`mula acc, c, vf00[w]`, then `madd d, a, b`), ACC stays
claimed across the product's latency. That made the wrangler's B
strategies run out of steps on one of textured_triangles' large tables:
its compile took six times as long, for the same cycles. A subtracted
product (`c - a * b`) keeps `c` first and ends with `msub`. A product
with Q takes the Q form (`mulaq`, `maddaq`, `msubaq`, `maddq`, `msubq`),
so the viewport transform (`offset + v * q`) and the clipper's
interpolations (`cur + (next - cur) * t`) are chains too.

*Measured* (`tests/bench/scenes.txt`): every scene is as fast or faster,
−0.4% in all (model −3.0%, the world_lm floor −0.5%). Loop prologues and
epilogues lose about 40% of their stall cycles, and the clipper's
`ClipTriangle` loop now pipelines. Kernel periods are unchanged, so the
shared ACC costs the pipelined loops nothing; world_strips' loop has a
shorter prologue and runs its kernel a few more times. The ports'
instructions go from 2,700 to 2,681 pairs.

The Q forms take another 0.3% (−0.7% in all, 254,265 → 252,414 cycles)
and 1.5% of the stalls, mostly in the clipper's interpolations: the
world_lm and world_strips floors −0.3%, textured_triangles' world floor
−0.6%. Particles' kernel keeps its 16 cycles, but the search's first
strategies miss it now, so its loop search does nearly six times the
checks (still a third of a second) and the program is 113 pairs, as
before O12.

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
   `all`, the default, or `none`), and `--no-opt=`, which drops the named
   ones from that; `Options::optimizes` asks whether one runs. The first
   is `exit` (O9).
3. **Timed model.** `crates/oxide-vcl/tests/support/vu_model.rs` has a
   second configuration, `Config::TIMED`: pairs stall on VF fields still in
   the FMAC pipeline (latency 4), on ILW results, and on a busy FDIV or
   EFU; Q, P and the MAC, status and clip flags are not interlocked and
   change when their latency has passed; WAITQ waits for Q and WAITP until
   the cycle before P lands. A conditional branch right after the integer
   instruction that sets its register reads the value from before.
   Arithmetic is deterministic f32 within the VU's range, addresses wrap,
   and each XGKICK records the GIF packet it sends. It covers every
   instruction the corpus and probes use. Not modeled: the ordering of an
   XGKICK after an XGKICK, and the VU's exact rounding.
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

5. **Benchmarks.** `crates/oxide-vcl/tests/bench.rs` and the `bench` test
   of `crates/vusl/tests/programs.rs` record, per program and scene, the
   model's cycles and stall cycles, the code size and the compile's work
   counts (`vcl::stats`) in tables under `tests/bench`, and print what a
   change does to them (docs/TESTING.md, "Benchmarks"). The loop sweep
   runs trip counts 1 to 48 and gives each loop's kernel, prologue,
   epilogues and exit their own cycles: a kernel's cycles per iteration
   apart from what entering and leaving the loop costs. Stalls are split
   by what they waited for (an FMAC result, a load, an ILW, a busy FDIV or
   EFU, WAITQ or WAITP), per program and per loop part, and the compile's
   work by phase (source, loop expansion, preparation, scheduling,
   allocation, table work).

The model runs the manual's worked timing examples (figures 3-3 to 3-17)
as tests (docs/TESTING.md, "The timed configuration").

Still open: an XGKICK during a PATH1 transfer is not timed, since the
manuals give no transfer rate; the sweep does not run a count of 0; and a
program whose count is not where the count states put it (a constant, a
field the host fills) keeps its own count in the sweep. O1 needs the
last of these before it lands.

## 6. Phases

The order weighs measured gain against risk and effort. Effort is a rough
size: small is days, medium one to two weeks, large three to five weeks.

| Phase | Items | Effort | Expected result |
| --- | --- | --- | --- |
| 0 | Survey tool, `--opt=` switch, timed model, equivalence harness (section 5) | Medium | Baseline per table and per program; a check that oxide output computes what vcl output computes |
| 1 | O9 (`exit`: the E row on the last instructions) | Small | A row and a cycle off 36 of 52 public programs |
| 2 | O1 (pipeline unannotated loops) with O10 (code budget): `loops` and the `entry` fix | Large | About −22% over the 13 loops (811 → about 636 cycles per iteration), about 3× on `sprite` |
| 2a | O11 (`unswitch`: a copy of a loop's usual path per mode) | Medium | quake2 world triangles −41%, models −15%; tyra `stapip` −15 to −20% |
| 3 | O3 step 1 (reschedule loop prologues and epilogues): measured, not kept | Medium | 130 of the public corpus's 15,239 rows and stalls; little in the scenes |
| 4 | O4 (merge induction variables, post-increment folding): `pointers`, without the folding | Medium | One cycle per iteration in lower-bound kernels whose hint promises nothing more (`mcpip_as_is` 23 → 22; ps2gl's would gain but promise a safe count) |
| 4a | O5 step 1 (move work between streams that run on every path; fill delay slots): `hoist` and `speculate`, without the delay slots | Medium | The clip pass's usual path 39 → about 35 cycles a corner, and its intersection 21 → 11 rows; the clipped scenes −3.3 to −8.6% |
| 4b | O2's bookkeeping: `steps` (fold a block's runs of pointer steps) | Small | quad 47 → 45, tri 36 → 35, pv_diff_quad 51 → 45; doom3 `world_lm` wall −2.3% |
| 5 | O5 step 2 (pipeline loops with branches), on the modulo scheduler O2 describes | Large to very large | The clipped floors at best −25 to −35% |
| 6 | O6 (copies), O7 (allocation) | Large | Shorter chains, lower pressure; frees periods in high-pressure kernels |
| 7 | O5 step 3 (superblocks, hoisting setup code), O8 (memory/kick precision) | Very large | Setup chains and stream boundaries; the largest remaining static stalls, little at run time |
| 8 | O12 (single-product ACC chains in VUSL, Q forms): done | Small | The VUSL scenes −0.7%, no scene slower; prologue and epilogue stalls about −40% |

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
  tight dependency chains. Filling them needs work from other streams.
  O5 step 1's moves (`hoist`) leave the clip pass's test and latch with
  their branches alone, and what would fill those slots is the next
  iteration's work, which is step 2's.
- **O1 is the largest measured runtime gain.** vcl's own pipeliner does
  the work given `--LoopCS 1,0` (the `loops` optimization), with the
  `entry` fix for the values it does not time from before the loop. What
  remains of O10 is a size option for programs that share micro memory.
- **O11 after O1**, for the renderer scenes: it is what quake2's triangle
  loop needs, and its copies feed O1's pipelining.
- **O3 before O2 and O4** although they save more per vertex: O3 keeps the
  original's data flow, so its risk is low. It reschedules code that is
  already allocated, so reuse of physical registers limits what it finds.
  Step 1 alone found little (O3, *Measured*): what the parts lose is
  their overlap with the code beside them, which step 2 and O5 bring.
- **O2 after O4**, because O4 lowers the bounds O2 aims at. Its first step,
  `steps`, needed no new scheduler: the original's search reaches the
  evidence kernels' bounds once their steps fold. `--LoopExtra` stays
  honoured: the source asks for those cycles.
- **O5 step 1 after O4**: the clip passes hold almost all the stalls left
  in the renderer scenes (O5, *Evidence, at run time*), and step 1 needs no
  new scheduler: it rewrites the source the original's wrangler schedules,
  as `unswitch` and `pointers` do.
- **O5 step 2 with O2's scheduler**: step 2 needs a modulo scheduler that
  is not the original's, with conditional streams in its kernel. After
  `steps`, what is left for that scheduler in single-block loops is small
  (O2, *What remains*).
- **O5 step 3 last**: the largest piece of work, with code duplication
  at side entries, for code that runs once per batch.

Each phase ends with the corpus measured on both outputs (cycles per
iteration, per batch, code size), the equivalence harness passing, and
vcl mode's output still identical to the original's.
