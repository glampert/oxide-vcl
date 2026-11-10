# Software-pipeline compiler integration

The Rust compiler emits generated entry, warmup, kernel, drain and exit parts
for explicit raw stages and for counted normal loops. This chapter describes
the integration, the original evidence behind it, and the remaining
differences. The behavior authority is the supplied ELF (vcl 1.4beta7),
SHA-256 `83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`.
No openvcl implementation or output was used.

## Two expansion boundaries in the original

Inferred from vcl 1.4beta7: the main driver expands explicit raw stages before
its first dependency-check wrapper. Later, normal loop optimization
initializes affine annotations, searches schedules, expands them, and rebuilds
dependencies again.

This gives two compiler paths:

```text
explicit raw stages -> clone windows -> actual CFG -> strict dependency check

normal source -> strict dependency check -> prune -> annotate induction
              -> modulo search -> clone windows -> actual CFG -> repair offsets
```

Both paths then prune, construct register webs, allocate, validate physical
pairs and emit. Affine annotation is not applied to raw source instructions
before early raw expansion. The observed `immediate-pipeline-before-add` and
`...before-subtract` originals emit displaced loads with offset 5 in their
drains; annotating before expansion would change those loads to 2 and 8.

Textual raw flow is not validated before expansion; the expanded execution CFG
is. The `pipeline-raw-stage-backref` original shows this:

```asm
2..mul vf04,vf03,vf01 nop
1..mul vf03,vf01,vf01 1..isubiu vi01,vi01,1
3..add vf02,vf02,vf04 nop
nop 1..ibne vi01,vi00,loop
nop nop
```

The first textual VF03 read has no declared incoming value, but stage zero
writes it in warmup before stage one executes, and the original accepts the
input. Ordinary unscheduled use-before-set rejection remains strict.

## Source stage fields and raw controls

The instruction decoder initializes its parsed stage to zero, changes it only
for an explicit `number..` or `+..` prefix, and stores it in the instruction.
The `.rawloop`/`.vsmloop` handler increments a separate parser-side source
grouping counter; the Rust parser leaves unprefixed instructions at stage zero.

The raw-table builder's rules:

- A positive prefix is prohibited on NOP.
- Once a non-NOP carries a stage, other non-NOPs in that table must carry
  stages.
- A staged branch requires its explicit delay pair, with no stray rows beyond
  it.
- Absolute issue position is `row + period * (prefix - 1)`.
- A loop-marked table whose maximum resulting stage is zero is rejected.

So spelling every operation with `1..` is a rejected raw-loop table, not
equivalent to ordinary unprefixed raw input (original guest exit status 255,
`max loop id 0 in loop`).

An unprefixed `.vsmloop` body is an ordinary raw table. With a `--LoopCS 1,0`
conditional self-loop, normal optimization tries the loop search, whose local
clone step requires UTA mode; the original rejects with `table to be locally
cloned isn't uta`. Adding `-L` prevents that search and succeeds with seven
pairs. The compiler preserves both outcomes. `-L` does not suppress the
earlier expansion of explicitly staged raw input: the multi-stage control
still emits 37 pairs.

Observed in `pipeline-raw-single-stage`, `pipeline-raw-default-stage`,
`pipeline-raw-default-disabled` and `pipeline-raw-stages-disabled` (the
default-stage fixture's unprefixed instructions have decoded stage zero).

## Normal-loop scheduling

`normal_loop_plan` accepts a direct conditional self-loop with a positive
explicit LoopCS minimum, one terminal lower branch, ordinary VCL operations
and no source modifiers. Random-unit operations and external transfers keep
linear compilation. Indirect and multi-block loops are outside the adapter.
These are port limits, not vcl rejection rules.

By default the loop is scheduled by the port of vcl's loop search
([LPGEN_PORT.md](LPGEN_PORT.md)). With `VCL_OLD_LOOPS=1` it uses the
standalone modulo scheduler: `loop_schedule::build` consumes effective
component flow, per-write latencies and conditional-input timing marks, builds
current-iteration and backedge RAW dependencies, cyclic reuse constraints and
conservative memory order, and verifies reaching definitions before admitting
an edge. The equations are in
[LOOP_SCHEDULING_INTEGRATION.md](LOOP_SCHEDULING_INTEGRATION.md). In that
mode, at each period the adapter tries the strategy list in reverse order,
checks effect ages before selection, minimizes maximum stage and replaces an
earlier candidate on equal stage, as vcl's loop search does. The first
feasible period wins. Search is bounded to 4096 nodes per strategy, at most
eight periods above the starting bound, and period/horizon at most 256; budget
exhaustion is not proof of infeasibility.

Proven affine address reads may cross a VI setter. They keep their read
effects and flow validation but omit the corresponding placement RAW/WAR
edges. Only `loop_ir::annotate_sources` can justify that relaxation. Setter
recurrences, other register reads and memory ordering stay constrained. After
cloning, actual producer identities determine the displacement repair.

Stores obey `stage >= conditional_stage - known_safe`; critical effects obey
`stage >= conditional_stage`. The compiler additionally treats writes to
components live across the loop boundary as critical, so an observable
induction update cannot advance an extra iteration before an early exit (a
candidate with conditional stage two but visible pointer/count updates in
stage one would otherwise process two iterations for a trip count of one).
Age validation is applied to each strategy's candidate, not only the selected
one. Memory-order edges are computed independently: an autoincrement load can
be both memory and an escaping VI update.

vcl additionally mutates register locks, synthesizes moves, reduces extents
and allocates the folded table before accepting a candidate. The standalone
adapter keeps conservative reuse edges and allocates after expansion, so it
does not claim identical candidate selection or complete modulo allocation. A
failed normal transformation is transactional: its partial output is
discarded and a legal linear candidate is retried with all allocation and
timing guards active. Explicit raw stages never fall back to linear code.

## Clones, flow and numeric cleanup

`pipeline::expand` describes clone windows and graph edges; `loop_ir::expand`
produces owned typed instructions with dense clone IDs. `compile` maps those
IDs into the real region. It keeps source entry labels, rewrites/inverts
conditional targets, creates drain branches, and keeps fixed pair rows
including empty branch-delay slots.

Both slots read the old pair state. `flow_owner` maps a lower read to the first
active instruction in its pair, while each output keeps its actual writing
operation. The same ownership is used during pruning, web construction,
affine repair and final physical timing checks. Independent operations are not
assigned register versions by equal stage or bias alone.

For a normal scheduled loop:

```rust,ignore
let annotated = loop_ir::annotate_sources(&sources, &graph, &flow, block, &operands)?;
let expanded = loop_ir::expand(&schedule, &annotated.sources, loop_id)?;
// Splice all generated paths, then analyze the actual CFG and pair reads.
let repaired = expanded.repair_from_flow(&[], &expanded_flow, &clone_operands)?;
```

The repair uses external seeds and exact cloned VI writers, including
kernel/prologue joins. For `p += 3; lq c,5(p)`, moving the load ahead of its
setter emits offset 8; subtraction emits offset 2. Symbolic expressions keep
their expression form. See [IMMEDIATE_INTEGRATION.md](IMMEDIATE_INTEGRATION.md).

`coalesce_immediates` applies a separate numeric transformation. It considers
every actual scalar user, refuses branch/escaping/mixed-definition
observations, changes every approved consumer atomically and zeroes the
producer immediate; flow is rebuilt after each change. This produces the raw
min4 warmup's 0/2 decrements without a stage-number multiplier, and the min1
early-exit branches prevent coalescing. [INDUCTION.md](INDUCTION.md) describes
the general pass and its representability limits. The coalescer also runs
before normal loop generation and on ordinary tables, with complete cross-block
users participating. Internal copy recognition follows pruning and keeps fixed
raw encoding rules; see [COPIES.md](COPIES.md).

## Test coverage and remaining differences

The following fixtures (`crates/oxide-vcl/tests/oracle_pipeline.rs`) match the
original's normalized pairs, including slots, registers, masks, immediates,
targets and E bits:

| Fixture | Original pairs | Coverage |
| --- | ---: | --- |
| raw stages, minimum 1 | 37 | separate warmups and early drains |
| raw stages, minimum 4 | 27 | joined warmup, pruning and 0/2 arithmetic |
| raw stages with `-L` | 37 | mandatory early raw expansion |
| raw stage backward textual definition | 37 | expansion before strict dependency checks |
| raw affine before add/subtract | 37 each | authored drain offsets and actual flow |
| normal counted vector | 6 | modulo placement, branch/delay row, output |
| unprefixed raw with `-L` | 7 | decoded stage zero and ordinary raw path |

The three normal affine cases match the complete 14-pair original output,
including the five-row kernel. The cleanup moves the prologue pointer update
into the multiply's lower slot, shortens the drain and folds its empty exit
before assigning E. Known minimum four additionally allows final empty-row
compression (12 pairs); `-C` keeps 14. See
[LINEAR_CLEANUP.md](LINEAR_CLEANUP.md) for the modes, extent rules, profile
control and limits. The adapter does not implement all of vcl's writer motion
or the raw `reorder` reduction engine.

Execution comparisons run 64 seeded states per fixture, compare declared
escaping registers and all 1024 modeled memory qwords, and require pipeline
labels on scheduled cases to prevent silent fallback. Trip counts are 1-8,
except minimum-four promises use 4-11. The model does not prove hardware
timing, VU numerical edge behavior, flags or external transfers; see
[TESTING.md](TESTING.md).

`loop-integer-counted` did not reach guest exit within its 60-second
original-emulator bound; its original outcome is unknown, not a rejection or an
exact comparison. Original observations have explicit per-run alarm scales;
the synthetic captures use scales 0.1, 1 and 25. The same after-add
source/arguments give byte-identical original stdout at scales 0.1 and 1; this
does not establish profile independence in general, and no timing profile is
substituted for another.

Reproduction:

```sh
python3 local/tools/probe_pipeline.py       # validate the original observations
python3 local/tools/probe_induction.py      # validate the coalescing observations
cargo test --all-targets
python3 tools/compare_corpus.py target/debug/oxide-vcl --out local/evidence/differential
python3 tools/compare_corpus.py target/debug/oxide-vcl \
  --oracle tests/corpus \
  --out local/evidence/corpus/comparison
```

Use `--capture` on a probe tool only when intentionally rerunning the original
through the installed emulator. The unknown integer attempt is retained by the
pipeline validator and is not recaptured automatically. Original inputs,
output bytes, hashes and termination metadata are under the named
`local/evidence/oracle/` directories (local, not tracked).
