# Original VCL defects and an opt-in fix mode

[vu-checker](https://github.com/glampert/vu-checker) (`check_vu_code.py`)
guards a VCL build against six kinds of silent failure:

- `crossloop`, `regalloc` and `loopvar`: liveness.
- `immediates`: field widths.
- `latency`: unpadded flag, Q and branch-condition reads.
- `branches`: wrapped branch offsets (benign: they land by wraparound).

Its author found them with OpenVCL. A second set of minimal repros from
the same work sits in the public corpus as `repro__*` and
`*test_repro_quad_adc_bug`.

Every one of those categories was tested against vcl 1.4beta7 ("the
original" below) with synthetic probes. The results fall into three groups:

- defects the original shares, which the port reproduces by default;
- OpenVCL-only defects, which the original does not have;
- problems outside VCL.

The shared defects get opt-in fixes, one per defect, plus a warning for
program size: `imm` (F1), `alias` (F2), `clip` (F3), `end` (F4) and
`size` (F5).

Ground rules:

- **Compatibility stays the default.** Without a fix, output stays
  identical to the original's, defects included.
- **Named fixes.** `--fix=NAME` enables one fix, `--fix=a,b` several, and
  `--fix=all` every one; the option may be repeated. `--mode=oxide` enables
  every fix. The original's argument parser rejects every argument whose
  second byte is `-` with `bad args '…', try -h for help` (see
  `cli::parse`), so no valid original command line changes meaning. An
  unknown name is rejected the same way.
- **Fixes are not optimizations.** They change output only where the
  original's output is wrong, or where it rejects a valid program. Each
  keeps vcl's messages unless oxide mode is on too. In the library a fix
  is `vcl::Fix`, chosen in `Options::fixes`; `Options::fixes(fix)` tells
  whether it applies.

## 1. Method

Each probe is an independently written source under `local/evidence/inputs/`.
Its original-binary run (`tools/run_oracle.py`, alarm scale 1) is stored
under `local/evidence/oracle/<name>/` (local, not tracked). The two affine-repair probes use scale
0.1, like the `immediate-scheduled-*` probes they extend.

- The 26 accepted `checker-*` probes all equal the port line for line, with
  comments and whitespace ignored.
- `checker-parse-*` probes record parser behaviour (section 5).
- The `repro__*` programs and their original outputs are in
  the public corpus (`tests/corpus`), and the port matches all of them.

[crates/oxide-vcl/tests/vcl_bugs.rs](../crates/oxide-vcl/tests/vcl_bugs.rs) pins the evidence:

- the line-for-line equality;
- every out-of-field immediate the original emits, found by a field-width
  checker modelled on vu-checker's;
- the alias defects, by running both outputs in the test VU model
  (`crates/oxide-vcl/tests/support/vu_model.rs`) and comparing against sequential
  semantics;
- the clip-history rejections;
- the liveness probes, executed against a reference computed in the test.

Field widths and latencies come from the VU User's Manual 6.0 at
`~/Repos/GitHub/PS2Docs/markdown`:

- §3.4.8 (branch conditions);
- §3.4.4 and Figures 3-16/3-17 (flag timing);
- the instruction reference for each field width.

## 2. Findings

| vu-checker category / repro | In the original? | Evidence | Fix |
| --- | --- | --- | --- |
| `immediates`: source values | **Yes** | `checker-imm-*-source`, `openvcl__repros_immediates_out_of_range`, `openvcl__test_repro_quad_adc_bug_fjtrujy` | F1 |
| `immediates`: introduced by affine repair | **Yes** | `checker-imm-repair-add`, `checker-imm-repair-subtract` | F1 |
| `immediates`: IADDI | No: normalized to IADDIU/ISUBIU | `checker-imm-iaddi-normal`, `checker-imm-iaddi-raw` | — |
| Store/load and store/store through different bases | **Yes** | `openvcl__repros_store_load_alias`, `openvcl__repros_store_store_alias`, `checker-mem-copied-base` | F2 |
| Accesses to one qword in a pipelined loop | **Yes**, even through one base and under `-M` | `mem-loop-same-base`, `-same-base-timid`, `-accumulator`, `-store-store` | F2 (not `mem-loop-copied-base`) |
| Clip programs without `in_hw_clip` | **Yes**, valid programs are rejected | `openvcl__repros_clip_join`, `openvcl__repros_clip_loop_entry`, `openvcl__repros_clipw_implicit_w`, both `quad_adc_bug` | F3 |
| Exit rows losing the E bit | **Yes** (already known) | `external-vf-two`; one `--cont` stream in the local corpus | F4 |
| `crossloop`, `regalloc`, `loopvar` | No | `checker-live-*`, `openvcl__repros_crossloop_4regs`, `openvcl__repros_loop_pointer` | — |
| `latency`: clip, MAC, status, Q, branch condition across joins, back edges and calls | No | `checker-lat-*`, `openvcl__repros_mac_join`, `openvcl__repros_ilw_branch_label` | — |
| `openvcl__repros_masked_write_branch`, `openvcl__repros_seed_store_order` | No | corpus outputs are correct | — |
| `-c` schedules differently | No | identical pairs with and without `-c`: 3 corpus programs on the original, 49 on the port | — |
| `branches`: wrapped offsets | No: the program counter wraps, so they land | VCL emits labels; dvp-as encodes | program-size warning only (F5) |

## 3. Defects the original shares

### F1. Immediates outside their field

**What the original does.** It passes numeric immediates through to the
output without checking any field width. dvp-as then keeps the low bits
without a diagnostic. Two sources produce such values.

*Source values* (`checker-imm-memory-source`, `-iaddiu-source`,
`-flags-source`, `openvcl__repros_immediates_out_of_range`):

| Field | Width | Original output | After dvp-as |
| --- | --- | --- | --- |
| IADDIU, ISUBIU | 15 bits unsigned | `iaddiu VI02,VI01,0x00009c40` (40000), `isubiu …,0x00011170` (70000) | 7232, 4464 |
| IADDIU, ISUBIU, negative | | `isubiu VI01,VI01,0xffffffff` (source `-1`) | subtracts 32767 instead of adding 1 |
| LQ, SQ, ILW, ISW | 11 bits signed | `lq …,1024(VI01)`, `sq …,-1025(VI01)`, `ilw.x …,2047(VI01)`, `isw.x …,-2000(VI00)` | -1024, 1023, -1, 48 |
| FCAND, FCOR, FCEQ, FCSET | 24 bits | `fcand VI01,16777217`, `fcset 16777218` | 1, 2 |
| FSAND, FSOR, FSEQ, FSSET | 12 bits | `fsand VI02,0x00001001` | 1 |

The two public quad-ADC repros differ in one line. One has
`iaddiu vi05,vi00,0xffff` and the other `0x7fff`. The first is exactly
this defect: 0xffff becomes 0x7fff.

*Affine displacement repair* (`checker-imm-repair-add`, `-subtract`):

- When the scheduler moves a load across its pointer update, the repair
  step adjusts the offset ([IMMEDIATES.md](IMMEDIATES.md)).
- It does the arithmetic in 32 bits and never checks the field.
- So `lq c,1022(p)` after `iaddiu p,p,3` is emitted as
  `lq VF03,1025(VI02)`, and `-1023` after `isubiu` as `-1026`.
- The source is in range, and the user never sees the bad value unless
  they read the listing.

*Not affected:*

- **IADDI.** Numeric IADDI is always rewritten to IADDIU or ISUBIU by
  the original's scalar collapsing, raw blocks included: `iaddi …,-18` → `isubiu …,18`.
- **The general coalescer.** It range-checks every rewrite
  ([INDUCTION.md](INDUCTION.md)).
- **Symbolic immediates.** These are still unresolved at emission, for
  example `iaddi vi2,vi1,SYMBOL+1`. Only the assembler knows the value.

*Memory offsets and wraparound.* The manual defines the address as
`(Imm11 + VI[is]) × 16` and does not say what happens past the end of
data memory. If the hardware wraps modulo the memory size (1024 qwords
on VU1, 256 on VU0), an offset truncated modulo 2048 would hit the same
qword. Nothing we have establishes that, so the plan treats a truncated
offset as a defect.

**Fix (`--fix=imm`).** `crates/vcl/src/compiler/immediate_fields.rs`. The
field widths come from the manual and are in the instruction table:
IADDI 5 bits signed, IADDIU/ISUBIU 15 bits unsigned, LQ/SQ/ILW/ISW 11 bits
signed, FC* 24 bits, FS* 12 bits.

1. *Exact VI steps.*
   - VI registers are 16 bits (manual §3.2), so IADDIU/ISUBIU arithmetic
     is modulo 65536.
   - A step outside the field is reduced modulo 65536 as it is emitted:
     IADDIU if the result is at most 32767, ISUBIU with `65536 - value`
     if that is.
   - Steps inside the field are left as written. The original already
     does the same for IADDI. For example:
     - `iaddiu …,0xffff` → `isubiu …,1`
     - `isubiu …,-1` → `iaddiu …,1`
     - `iaddiu …,40000` → `isubiu …,25536`
     - `isubiu …,70000` → `isubiu …,4464`
   - Adding 0x8000 has no encoding: neither `iaddiu …,32768` nor
     `isubiu …,32768` fits 15 bits. That is an error, whose help suggests
     two steps (`0x7fff`, then `1`), as `quake2-ps2` programs write it.
   - `openvcl__test_repro_quad_adc_bug_fjtrujy` writes
     `iaddiu vi05,vi00,0xffff`. The assembler makes that 0x7fff; the fix
     emits `isubiu …,1`, which is 0xffff, as written. A program tuned to the
     truncated value would behave differently with the fix.
2. *Flag masks.*
   - FCAND/FCOR/FSAND/FSOR: the bits past the flag's width do not exist,
     so the hardware result equals the truncated mask's. It is a warning,
     and the listing carries the mask's existing bits, as the assembler
     would.
   - FCEQ/FSEQ/FCSET/FSSET: the value means something else after
     truncation. It is an error.
3. *Memory offsets in source* are errors. Legalizing one needs a second
   base register, and allocation runs before emission; this is not worth an
   allocator interaction.
4. *Displacement repair.* The source's offsets are checked first, so an
   offset outside its field at emission comes from the repair. Each
   consumer's repaired offset is fixed in advance (its offset plus or minus
   the stride, by its phase and the update's direction), but whether the
   scheduler moves it is known only after scheduling. So:
   - At emission, such an offset fails the compile.
   - The instruction on that line is marked to keep its offset
     (`Instruction::fixed_offset`): neither the linear tables nor the loop
     builder annotate it as an affine consumer. It keeps its ordinary
     dependency on the update and cannot cross it.
   - The program is compiled again. Output changes only where a repaired
     offset left its field.
   - `checker-imm-repair-*` then emit offsets 1022 and -1023, and their
     loops stay pipelined.
5. *Symbolic immediates in narrow fields* (IADDI, memory offsets) are
   warnings, since only the assembler can check them.

Errors and warnings name the source line, in vcl's layout or oxide's.

### F2. Memory order through different base registers

**What the original does.** The memory-order pass orders
two accesses whose fields intersect only in three cases
([MEMORY.md](MEMORY.md)):

- timid mode `-M` is on;
- their tags match;
- their effective addresses are equal by the original's address comparison, which needs the same
  register identity, the same external-input state, the same reaching
  setter and the same immediate.

Two different registers holding the same address are assumed not to alias:

- `openvcl__repros_store_load_alias`:
  - `iWalk = 900`, then `sq fC,1(iWalk)` and `lq fD,901(vi00)`.
  - The original hoists the load above the store, so qword 5 receives the
    stale contents of 901.
  - The coalescer has already turned `iWalk` into `iaddiu VI01,VI00,0`
    with offset 901. Both addresses are then visibly `901 + 0`, through
    different registers.
- `openvcl__repros_store_store_alias`: the two stores to 901 are swapped, so the
  product, not `fA`, lands last.
- `checker-mem-copied-base`: `iCopy = base + 0`. A load through `iCopy`
  moves above a store through `base` at the same offset.
- `checker-mem-same-base`: the same accesses through one register are
  ordered.
- With `-M` (`checker-mem-store-*-timid`), the original orders the pairs
  and the results are correct.

This is the original's documented optimistic model: VCL expects tags or
`-M` where pointers alias. It is a defect only where the compiler can see
that two addresses are equal. That is the case in all three probes.

*Pipelined loops.* A software-pipelined loop runs on a copy of its table
without the memory-order lists, so only shared tags order its accesses
there (see [MEMORY.md](MEMORY.md#ordinary-memory-operations-and-alias-tags)).
Accesses to one qword swap even through one base register, and `-M` does
not prevent it (observed from vcl 1.4beta7):

- `mem-loop-same-base` stores `100(base)` and loads `100(base)`. The
  pipelined loop loads each iteration's qword before its store, so it reads
  stale memory. `mem-loop-same-base-timid` is the same program under `-M`,
  with the same loop.
- `mem-loop-copied-base` does the same through a copy of the base.
- `mem-loop-accumulator` keeps a sum at `50(vi00)`. The next iteration's
  load runs before this iteration's store, so the sum misses terms.
- `mem-loop-store-store` stores twice to `100(base)`, and the first store
  lands last.
- `mem-loop-load-store` loads `100(base)` before storing over it; the
  schedule happens to keep that order.

**Fix (`--fix=alias`).** `crates/vcl/src/compiler/memory_alias.rs` gives each
memory access a symbolic address, `root + c`:

- The base register is followed back through its reaching definitions:
  IADDIU, ISUBIU and IADDI by a number, IADD and ISUB with VI00, VI moves,
  and the steps of LQI, SQI (+1), LQD and SQD (-1). vcl's displacement
  repair (`iaddiu VI01,VI00,0` with offset 901) is one of these steps.
- The root is VI00 (an absolute address), or the register at its reaching
  definitions where the steps stop: an input, a load, or a join whose
  incoming values differ. A loop's induction register is such a root.
- `c` is the sum of the steps and the offset, modulo 65536. A symbolic
  offset gives no address.

In a linear table, two accesses with intersecting fields, the same root and
the same `c` keep their order, beside vcl's three cases.

In a pipelined loop, the accesses of the body with one address form a
group when one of them is a store. A group is invariant when its root is
VI00 or a register no operation of the body defines: then it is the same
qword in every iteration. After vcl's loop search, the fix checks the
schedule: a later access of a group must run after an earlier one (store
and load, load and store, or two stores, over intersecting fields), and
for an invariant group the next iteration's first access must run after
this iteration's last. When the schedule breaks that, each group becomes a
resource of the loop table, as a tag does: every access reads it, every
store writes it, and only the last store's value reaches the next
iteration. The loop is searched again; if that fails or still breaks the
order, it stays unpipelined, where the linear rule applies. A loop that
vcl schedules in order keeps vcl's schedule. All four broken probes then
stay pipelined and execute to the sequential result.
`mem-loop-same-base`, `-same-base-timid` and `-store-store` keep vcl's
period (7, 7 and 6). `mem-loop-accumulator` goes from 6 to 9: its sum now
waits for the previous iteration's load, add and store, the recurrence
vcl's period ignores.

Every other pair keeps vcl's no-alias assumption: different constants
never alias, and pairs that do not resolve stay unordered, as without the
fix. That includes two induction registers that step alike: in
`mem-loop-copied-base` the base and its copy start equal and both step by
1, but each is its own root. A next-iteration access at another offset
from an induction register (`c1 − c2 = k·s` for stride `s`) is not
resolved either.

#### 3.1 The cost of `-M`

The port compiled each of the 44 accepted corpus programs twice, with and
without `-M`, and counted emitted rows. Rows measure static size, not
cycles. The port's `-M` output has been checked against the original only
on the two `-timid` probes.

| Measure | Default | `-M` |
| --- | ---: | ---: |
| Rows, 44 programs | 9,076 | 9,164 (+1.0%) |
| Programs that grow | | 16 |
| Largest growth | | `tyra__engine_src_renderer_3d_pipeline_dynamic_core_programs_dynpip_td_vu1` 155 → 173 (+12%); `tyra__engine_src_renderer_3d_pipeline_dynamic_core_programs_dynpip_c_vu1` 80 → 92 (+15%) |
| Programs that shrink | | 6, by 1–3 rows (search variation) |

So ordering every pair costs little in code size on most programs, but
up to 15% on the Tyra dynamic pipelines. Its cycle cost in loop kernels
is not measured. Step 2 keeps the original's assumption for unresolved
pairs, so `--fix=alias` changes only provably wrong output. `-M` orders
every pair, except in pipelined loops.

### F3. Clip history counted as read

**What the original does.**

- CLIPW shifts the earlier clip judgments up by six bits, so the original
  counts it as a read of the history register `CLIPa` (`implicit operand
  'CLIPa'`).
- Any program that runs CLIPW without declaring `in_hw_clip` is rejected
  with "register CLIPa is used before set". That holds even when the
  program never looks at the history.
- Five corpus programs fail this way. The smallest,
  `openvcl__repros_clipw_implicit_w`, tests only the current judgment
  (`fcand vi01,0x3f`).

With the history declared (`checker-lat-*-declared`), the same programs
compile, and the original pads the clip-flag reads correctly across the
join and the loop entry.

**Fix (`--fix=clip`).** `crates/vcl/src/compiler/clip_history.rs`:

1. *Count the defined judgments.* Judgment `j` occupies bits `6j…6j+5`,
   the current one being 0. On every path from the region's entry, count
   the CLIPWs, capped at 4, and take the fewest at joins; FCSET and
   `in_hw_clip` define all four. The count comes from the reaching
   definitions of the flow analysis: a CLIPW leaves one more judgment than
   the fewest that reach its read of `CLIPa`, and a loop's CLIPW settles at
   what its entry path gives it.
2. *Find what each clip-flag read observes* (VU User's Manual, the FC*
   instructions):
   - FCAND tests the bits its immediate sets.
   - FCOR tests the bits its immediate *clears*: it sets VI01 when the
     flag OR the immediate is all ones. The manual's example,
     `fcor vi01,0xffdf7d`, tests three judgments.
   - FCGET sees the low 12 bits; FCEQ, `out_hw_clip` and an immediate that
     is not a number see all 24.
   - CLIPW's own read of `CLIPa` observes nothing: the shift is what the
     count accounts for.
3. *Set aside unobserved history.* If every read observes only judgments
   below its count, CLIPW's undefined reads of `CLIPa` are not errors. The
   region is then compiled again with `in_hw_clip CLIP` declared in its
   `--enter` block, so it schedules exactly as the declared program does.
4. *Otherwise* the program is rejected as the original rejects it, with
   the same "used before set" error. FCAND `0xffffff` after one CLIPW is
   rejected; after four CLIPWs it is the quad-ADC pattern, which is
   defined.

With the fix, `openvcl__repros_clip_join`, `openvcl__repros_clip_loop_entry`
and `openvcl__test_repro_quad_adc_bug` equal the original's output for
their `checker-lat-*-declared` counterparts line for line, and
`openvcl__repros_clipw_implicit_w` and
`openvcl__test_repro_quad_adc_bug_fjtrujy` compile.

### F4. Exit rows without an E bit

**What the original does.** It sets the E bit on the row it walked second
to last, the row before the E delay slot. Its final compression can then
delete that row, or the delay slot after it (observed from vcl 1.4beta7):

- `external-vf-two` is printed without any E bit, so the VU runs past the
  end of the program;
- `end-cont-lost` keeps the final exit's E row, but its `--cont` stream has
  none, so the program runs on into the continuation instead of stopping;
- so does one `--cont` stream in the local corpus.

The loss is common in short straight-line programs: of the original's
recorded runs (public corpus, probes and local corpus), 51 that end in a
plain `--exit`, without `-e`, have no E bit at all. No recorded run puts a
forbidden instruction in the delay slot.

**Fix (`--fix=end`).** After final compression, each table that ends at an
exit or a `--cont` must end in its E row and the delay row after it
(`Emitter::end_row` in `crates/vcl/src/compiler/region.rs`):

- If both are in place and the delay row holds nothing the manual forbids
  there (section 3.5.3: a branch, XTOP, XITOP, XGKICK, or a VU memory load or
  store), the table is unchanged.
- Otherwise the E bit goes on the table's last row, and an empty delay row
  is added after it. The E row itself may hold any instruction. An empty
  table gets an empty `NOP[E]` row and its delay row.

`external-vf-two` then ends in `add[E] VF04,VF02,VF03` and an empty row,
one row longer than the original's output. The added rows are counted in the table comment and in `iCount`. With `-e`
the fix does nothing. Filling the E row and its delay slot with useful work
belongs to O9 in [OPTIMIZATION_PLAN.md](OPTIMIZATION_PLAN.md).

### F5. Program size

A VU branch's target is an 11-bit signed offset, in instructions, from the
instruction in its delay slot: -1024 to 1023. The original emits labels
and leaves the encoding to the assembler, which keeps the low 11 bits of
a farther offset (dvp-as warns "operand out of range"). VU1 micro memory
holds 2048 instructions and the program counter wraps around it, so the
truncated offset still lands on the target, wherever the program is
loaded, as long as the program fits. A public build of `quake2-ps2`
assembles three such branches, from OpenVCL's output, and vu-checker's
`branches` pass finds them landing by wraparound; the program works. Two
programs in set A have such branches, 1025 to 1249 instructions away, in
the original's output. Far branches are therefore not a defect; a program
larger than micro memory is.

**Fix (`--fix=size`).** `crates/vcl/src/compiler/program_size.rs` warns
about a program of more than 2048 instructions. Far branches get no
warning, and the output is unchanged.

## 4. OpenVCL-only defects (absent from the original)

| Probe | Shape | Original behaviour |
| --- | --- | --- |
| `checker-live-crossloop`, `openvcl__repros_crossloop_4regs` | VI set in one loop, read in a later sibling loop that does not set it | The register stays reserved through the second loop; `openvcl__repros_crossloop_4regs` keeps `iVal` in VI02 with only 4 VI registers |
| `checker-live-backedge`, `openvcl__repros_loop_pointer` | Value last written mid-loop, read above that write on the next iteration | Reused only between its last read and its redefinition |
| `checker-live-nested` | Outer counter live through an inner loop of temporaries | VI01 kept across the inner loop |
| `checker-lat-clip-join`, `-clip-join-declared`, `-clip-loop-declared`, `-quad-adc-declared` | CLIPW → FCAND across a join, a loop entry and back edge, four CLIPWs | ≥ 4 cycles on every path |
| `checker-lat-mac-join`, `checker-lat-mac-return`, `openvcl__repros_mac_join` | FMAC → FMAND across a join and across a subroutine return | ≥ 4 cycles on every path, padded at the return site |
| `checker-lat-status-loop` | FMAC at a loop's bottom → FSAND at its top | ≥ 4 cycles around the back edge |
| `checker-lat-q-join`, `checker-lat-q-loop`, `checker-lat-q-call` | DIV → MULQ across a join, a back edge and a call | ≥ 7 cycles on every path |
| `checker-lat-branch-join`, `checker-lat-branch-loop`, `openvcl__repros_ilw_branch_label` | Integer setter → conditional branch across a label or a back edge | One row between them on every path (§3.4.8) |

The liveness probes are executed in `crates/oxide-vcl/tests/vcl_bugs.rs`; the latency
probes are pinned by line equality. Within a block the original also
forces flag reads with a dummy upper `abs VF00,VFxx`. That instruction
reads the FMAC's destination, stalls until the result lands, and so
delays the flag check paired with it (`hw-fsand-after-alu`). Figures
3-16 and 3-17 of the manual confirm that flag checks see results by time,
including stalls, so the trick is sound.

The original also places FCSET one row before an FCAND that must still
read the old flags (`checker-imm-flags-source`). This is correct: FCSET
writes at the S stage, with latency 4.

## 5. Parser and stream behaviour the port matches

The port matches the original on the following parser, stream-label and
binding behaviours. They are default-mode behaviour, not opt-in fixes.

1. **Short memory operands.**
   - A base named `p` or `q` is not read as the P or Q register: `ilw.x
     t,0(p)`, `isw.x t,0(p)` and `ilw.x t,0(q)` fail with "failed to decode
     operand" because of the operand's length, while `0(ptr)` works.
   - The original's token decoder has a minimum length for each memory form
     (inferred from vcl 1.4beta7):
     - 3 bytes for `(reg)` and `off(reg)` (LQ/SQ), which must end in `)`.
     - 4 bytes for `(reg)mask` (ILWR/ISWR), which must start with `(`.
     - 5 bytes for `off(reg)mask` (ILW/ISW), `(reg++)` and `(--reg)`.
   - The lengths count the trailing mask. They are taken after the
     `:tag` suffix is removed.
   - So any one-letter base with a one-character displacement fails:
     `0(a)`, `9(p)`, `(pq)`, `0(p):tag`, and `ilwr.x t,(p)`. Meanwhile
     `10(p)`, `0(p)x`, `(pqr)` and `ilwr.x t,(pq)` decode.
     `checker-imm-memory-source` passes only because `2047(p)` is long
     enough.
   - Only lowercase `x`, `y`, `z` and `w` may follow the `)` (`0(p)X`
     fails). Nothing may follow an LQ/SQ operand (`0(p)x` fails). The
     increment forms must start with `(` (`x(p++)` fails).
   - The port applies the same rules (`memory_operand` in
     `crates/vcl/src/parser/mod.rs`). `memory_operand_shapes_match_the_original` checks
     the 16 `checker-parse-*` probes: the port accepts exactly the cases
     the original accepts, with equal output.
2. **Generated stream labels.**
   - An unlabeled region start is named `__v_<unique>_<uid>`. The number is
     a flow UID, not a region index: three straight-line regions are
     `__v_FIX_4`, `__v_FIX_8` and `__v_FIX_12`, and a raw program starts at
     `__v_FIX_5` (`checker-imm-iaddi-raw`).
   - Inferred from vcl 1.4beta7: each source entry (one line, with up to two
     instructions) gets a UID from one counter for the whole program.
     Entries with one UID form a stream, and a stream without a label is
     named after its UID. The counter starts at 1 and advances:
     - when the parse mode or raw submode changes (`label-raw-submode`,
       `label-raw-split`);
     - for a passthrough block, and again after it
       (`label-passthrough`);
     - for a label with D/T/M flags (`label-flagged`), but not for a
       plain label nothing names (`label-unreferenced`);
     - on entering or leaving a raw loop, each `.vsmloop` having its own
       ordinal (`label-raw-loop`);
     - for a branch target or indirect tag (`label-end-label`,
       `label-tag`);
     - at ENTER, EXIT, an exit macro and a barrier, which start the new
       stream (`label-exitm`, `label-barrier`);
     - after ENDENTER, ENDEXIT and CONT (`label-cont`);
     - after a branch, or in raw code after its delay pair
       (`label-raw-branch`).
   - A region with no code has no named stream (`label-empty`,
     `sched-empty`), and a region that starts with a label keeps it
     (`label-target-first`).
   - The port computes the same UIDs (`flow_uids` in
     `crates/vcl/src/compiler/mod.rs`). `crates/oxide-vcl/tests/stream_labels.rs` checks the 14 `label-*`
     probes and the stream labels of every accepted probe;
     `checker_probes_match_the_original`,
     `crates/oxide-vcl/tests/interface_bindings.rs` and `crates/oxide-vcl/tests/oracle_pipeline.rs`
     compare labels too. They also match on the corpus and the local corpus.
3. **Interface bindings across webs.**
   - `in_vi t(vi02)`, then `ilw.x t,10(p)`, then `out_vi t(vi03)` compiles
     in the original, with "changing binding for t from VI02 to VI03".
   - How the original binds (inferred from vcl 1.4beta7):
     - It keeps one current base per name, which survives regions.
     - A mapped `in_vi`/`out_vi` operand replaces that base and warns
       when it changes.
     - An unmapped operand takes the current base. A name that has no
       base is an error, at ENTER and at EXIT
       (`binding-unbound-output`).
     - Each interface operand keeps the base it saw. A web that reaches
       two different bases is rejected ("can't map to different base
       registers"): an unchanged input
       (`binding-same-web`), a redefinition on one path only
       (`binding-one-path`), one value leaving two exits in different
       registers (`binding-two-exits-same-web`), or one exit naming it
       twice (`binding-same-exit-twice`).
     - Repeated ENTER bindings of one name are one input, and the last
       one wins (`binding-two-inputs`).
   - The port does the same. The current bindings live in
     `Source::bindings`. Inputs record their base in `Region::forced`,
     outputs in `Block::output_forced`, and the web builder pins each
     interface strand to its own base.
   - `crates/oxide-vcl/tests/interface_bindings.rs` checks the 16 `binding-*` probes.
     Acceptance, output and the rejection reason all match the original.

## 6. Testing the fixes

Fix-mode expectations go into `crates/oxide-vcl/tests/vcl_bugs.rs` beside the
defect they repair, so each defect stays pinned in both modes. Default-mode
suites (oracle pipeline, public corpus, local corpus) must stay unchanged.
Per fix:

- F1: under `--fix=imm`, `--fix=all` and `--mode=oxide`,
  `checker-imm-iaddiu-source` leaves no out-of-field immediate (`out_of_field`
  in `crates/oxide-vcl/tests/vcl_bugs.rs`) and executes to the source's 16-bit
  results in the VU model; out-of-field offsets and flag values (inline, and
  `openvcl__repros_immediates_out_of_range`) are rejected with their line;
  masks and symbolic immediates give warnings; `checker-imm-repair-*` keep
  offsets 1022 and -1023 in a pipelined loop that executes to the source's
  sum.
- F5: `the_size_fix_warns_only_about_programs_too_large` builds raw
  programs with a branch past its offset's reach (no warning) and one
  larger than micro memory (a warning).
- F3: `the_clip_fix_compiles_the_repros_as_their_declared_versions`
  compares the repros with the original's `-declared` outputs.
  `the_clip_fix_declares_history_that_nothing_observes` checks inline
  programs under `--fix=clip`, `--fix=all` and `--mode=oxide`: those whose
  reads see only defined judgments compile as their declared versions, and
  those that read an undefined judgment (on some path) stay rejected.
- F2: `the_alias_fix_orders_accesses_to_the_same_location` executes the
  three alias programs to the sequential result under `--fix=alias`,
  `--fix=all` and `--mode=oxide`, and leaves `checker-mem-same-base`
  unchanged. `the_alias_fix_follows_steps_copies_and_increments` checks
  inline programs for each kind of step: each reads stale memory without
  the fix and the stored value with it, and pairs at different constants or
  unrelated roots compile as without the fix.
  `pipelined_loops_reorder_accesses_to_the_same_location` pins the
  original's `mem-loop-*` loops, with and without `-M`, reading stale
  memory. `the_alias_fix_orders_same_locations_in_pipelined_loops` and
  `the_alias_fix_orders_each_kind_of_pair_in_pipelined_loops` run the
  fixed loops for 6, 7 and 9 iterations: they stay pipelined and execute
  to the sequential result, `mem-loop-load-store` keeps vcl's listing, and
  `mem-loop-copied-base` compiles as without the fix.
- F4: `final_compression_can_delete_the_end_row` executes `external-vf-two`
  and `end-cont-lost` in the VU model, which runs past the end of the first
  and through the `--cont` of the second;
  `the_end_fix_keeps_an_end_row_and_its_delay_slot` checks that under
  `--fix=end`, `--fix=all` and `--mode=oxide` both stop at their E rows with
  the right results.
