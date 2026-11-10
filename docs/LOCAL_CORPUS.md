# Local corpus of disassembled VU programs (local only)

Besides the public corpus in `tests/corpus`, two unpublished reference sets
come from disassembled VU programs. They are derived from copyrighted code,
so they live in the gitignored `local/corpus/` and must never be committed,
pushed or bundled with the public corpus. Only the tools, this description
and the test are tracked. `local/corpus/manifest.json` names the sets; this
document calls them set A (28 programs) and set B (89 programs), and
`local/corpus/SET` stands for a set's directory.

`crates/oxide-vcl/tests/local_corpus.rs` reads the manifest and requires every
case to match the original (exit status and listing). Without the manifest it
passes and does nothing. The port matches the original on all cases of both
sets, including the original's rejections. The comparison can be run per set:

```sh
tools/corpus_status.py --candidate target/release/oxide-vcl --oracle local/corpus/SET
```

The default `--candidate` is the debug build; pass the release binary or
rebuild debug first, or stale results (exit 255, time-outs) appear.

Each case was run through the original with
`tools/run_oracle.py --time-scale 1 --timeout 3000 -- -t1 -uCORPUS @INPUT@`
(1 s to 4 min under emulation). A set's `provenance.json` records the listing
and input hashes, approximations, inputs, and the original's exit status and
time-outs. Cases whose original run hit the emulator's watchdog match the
port too, so the watchdog did not change those results.

# Set A

Set A holds the 28 VU microprograms of one executable, as listed by a VU
disassembler (one flat listing per program). 20 are accepted by the original;
the others are rejected, either for external assignment or for an untagged
`jr`.

## Building it

`tools/unschedule.py LISTING.vsm -o input.vcl --inputs-from target/release/oxide-vcl`
turns a scheduled listing back into VCL source that both compilers schedule
again:

- each upper/lower pair becomes sequential old-syntax instructions (physical
  registers), NOP fillers dropped; within a pair the half that reads what the
  other writes goes first (both read old values; `loi` writes I at T);
- a branch's delay-slot instruction moves before the branch; a labelled delay
  slot is also emitted at its label;
- an `[E]` end bit and its delay slot become `--cont` (MSCNT resumes after it);
- `jr`/`jalr` get target tags from constant code addresses loaded into their
  register (`iaddiu vi15, vi00, 0x13` becomes `L_0098`);
- the disassembler's negative branch targets wrap modulo micro memory (16 KB
  VU1, 4 KB VU0);
- registers the compiler reports used before set become `--enter` inputs
  (`in_vf vf05(vf05)`, `in_hw_*` for hardware registers), repeated until it
  compiles; `waitq`/`waitp` and `[M]` are dropped.

The result keeps the programs' instructions and control flow, not their
hardware timing: 20 of the 28 need an approximation (mostly a delay slot that
writes the register its branch tests), listed in each input's header. That is
fine for a compiler reference: both compilers must schedule the same valid
input identically.

# Set B

Set B comes from the VU microcode of three more executables, as listed by a
VU disassembler. It is derived from copyrighted code too and follows the
same rules: gitignored, never committed, pushed or bundled.

## Building it

These listings are VIF streams, not single microprograms, so
`tools/split_listing.py LISTING OUTDIR --prefix NAME [--vu0-from OFFSET]`
first cuts them into programs:

- consecutive `MPG` blocks up to the next non-`MPG` VIF code (`BASE`,
  `STCYCL`) form one package, a micro-memory image (two executables have
  one each, the third six VU1 and two VU0 packages);
- an entry point is an instruction nothing reaches (no fall-through, direct
  branch or `bal` return): MSCAL targets and routines reached through a
  table-driven `jr`;
- each entry's program is everything its flow reaches, up to `[E]` and its
  delay slot, written in the `.vsm` listing form `tools/unschedule.py`
  reads; branch offsets wrap around micro memory; identical programs are
  kept once (one executable's loaders repeat in every package).

That gives 89 programs (16, 15 and 58 from the three executables). The
largest executable's dispatchers end in a `jr` on a renderer address loaded
from memory and never reach an E bit, so they are not programs on their
own. Of the 89, 12 are rejected by the original: a `jr` whose register is
reloaded from memory has no tag list. Inputs and the original's results are in
`local/corpus/SET/<case>/`, hashes and sources in the set's `provenance.json`.

The split listings are not kept, since nothing reads them. They regenerate
byte for byte (and match `split_sha256` in the provenance) by running
`tools/split_listing.py` on each disassembled listing with the prefix and
VU0 offset recorded in the provenance.

`--enter` inputs come from the port's used-before-set errors as in set A,
except for 42 programs that use calls: for those,
`unschedule.py --inputs-from-original DIR` declares every register the
original lists as used before set (`INFO: UBS:`), reruns, and keeps the last
run as the reference.

# Rules these sets exercise

Behaviour the port implements because of these programs (observed from vcl
1.4beta7). The probes behind the copy, branch, `xgkick` and call rules are
`identity-copy-*`, `redundant-branch-feeder`, `branch-input-*` and
`xgkick-*` under `local/evidence/oracle/` (local, not tracked), checked by
`crates/oxide-vcl/tests/oracle_pipeline.rs`, as are the call fixtures.

Flow and pruning:

- Unreachable code (after a `jr` whose tags do not name it, or a second VU0
  entry point after a `b`) is not visited by flow; its reads join no register
  web. Labels named only by unreachable code still print.
- `xtop`/`xitop` are pruned when their result is unused; of the external-unit
  instructions only `xgkick` is a root.
- The prune commit removes a detected copy whose destination and source are
  one register object, i.e. one web: a partial `mulw.y vf03, vf03, vf00w` read
  back with the other components (vu_268277683). A full overwrite
  (`iaddiu p, p, 0`, `add.xyz v, v, vf00`) starts its own web and stays; so do
  copies coalescing leaves after loop expansion.
- A branch whose every target is the physically next block is deleted before
  the source prune, so code that only fed it disappears (vu_1401563139's
  `iaddiu vi04, vi00, 1; iand vi04, vi02, vi04`). With an unreachable block in
  between, the deletion happens at emission.
- Flag-cap scratch registers are chosen while caps are inserted: a producer
  that gets no cap reserves nothing, and only liveness (not every declared
  input) marks a register used (vu_3323147251: VF16, VF03, VF13).
- Every merged table is raw-reduced as one, not only those with an installed
  loop part: a stream whose jump to the next reachable stream was deleted
  merges with it, and the second part's instructions are pulled up.

Dependencies and ordering:

- Affine base reads after an induction setter (mode-2 locks) count in the
  static chain statistics: `iaddiu vi11, vi11, 7` has fan-out 4 from the
  stores after it, which decides a Vuta tie (vu_2250509715).
- The alias test does not separate `disp(reg)` from `disp(reg)mask`: an
  `sq ...,3(vi04)` aliases an earlier `ilw.w ...,3(vi04)` (vu_1518195603,
  vu_1342934707, vu_1401563139).
- `xgkick` carries descriptor order flags 0x60, which the force-dependency
  pass honours: it follows every earlier store (it has no address to compare)
  and the previous `xgkick`, and a later store follows it. Loads are not
  ordered against it.
- `xgkick` also reads and writes the external-unit resource EXT (two implicit
  operands in its descriptor; every ENTER declares EXT as an input). Each kick
  therefore depends on the previous one with latency 1, which counts in the
  scheduler's height and fan-out: with a second kick to follow, the first is
  ranked ahead of independent work (`doom3-passthrough`, public corpus
  `doom3-bfg-ps2__src_ps2_renderer_vu1progs_passthrough`).
- The stream order counts an output once per reading operand: `mul r,a,a` uses
  `a` twice.
- In a `--cont` stream (end mode 2), every `xgkick` has an end distance of 2
  while the table is scheduled, so Normal2's table is two rows longer than the
  lower bound and Vuta1's, with the kick moved up, wins in one case. A block
  running into `--exit` is still an ordinary stream at that point and gets no
  end distance.
- After all tables are installed, an address read that the scheduler moved
  above its pointer increment (offset rewritten) takes the increment's
  setters for timing, because the original rebuilds dependencies.

Branches and timing:

- The early-sampled branch read offset also applies to an input definition: an
  `ibeq` on an `--enter` VI issues one row after an independent instruction
  instead of beside it.
- Empty-row compression's early-branch check reads the register's countdown,
  so a value entering the table counts from the first row: the branch keeps
  that row instead of being pulled up against its input (`e_two` keeps
  `add / - / ibeq / add`).
- An early-sampled branch reading a register needs one more cycle unless the
  register is its setter's first operand, a load destination; a
  post-increment such as `sqi (VI04++)` is not.
- A branch folded into an exit it does not fall into is scheduled as the branch
  and printed as a lower `nop` with the E bit.
- External timing paths do not pass through a `--cont` stream, though they may
  start in one (for example when a subroutine returns to the `--cont` stream
  that falls into it).

Calls:

- A `bal` reaches only its subroutine, and an untagged `jr` returns to the
  stream after every `bal` that set its register, iterated to a fixpoint
  before any error. Data flow copies each subroutine per call string, so a
  return carries only its own caller's values (see
  [SHARED_SUBROUTINES.md](SHARED_SUBROUTINES.md#the-ports-model)).
