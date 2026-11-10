# Loop optimizer (lpgen)

The port schedules normal loops with a port of vcl's loop search
(`wrangle::optimize_loop` in `crates/vcl/src/scheduler/wrangle.rs`, driven by
`compile::lpgen_schedule`), reusing the linear wrangler port in the same
file. `VCL_OLD_LOOPS=1` selects the earlier standalone modulo scheduler
(`crates/vcl/src/loops/loop_schedule.rs`, see
[LOOP_SCHEDULING_INTEGRATION.md](LOOP_SCHEDULING_INTEGRATION.md)). This file
records what vcl 1.4beta7 does in the loop search (inferred from vcl 1.4beta7
unless noted) and how the port reproduces or differs from it.

## Driver

For each stream accepted by the loop-eligibility test (with the loop enable
set):

1. Compute external info. The first period is the loop bound plus `LoopExtra`.
   A `LoopAbs` above 1 replaces it unless smaller (then a warning).
2. Per period: clone a work table, then for strategy index 10 down to 0 (all
   11 have the loop bit) print `lpgen[period](limit: ...)`, prepare a table
   for the period, reset CPU stats and dynamic stats, and run wrangler A or B.
   On success, reduce extents, run the post-schedule setup with argument 2
   (which sets the table's max iteration ID), then local allocation. A
   candidate that allocates replaces the kept one unless the kept one has a
   smaller max iteration ID (ties replace).
3. No success at a period: restore the work table, period+1. Success: commit,
   install, print `Opt::Loop <name> <len>/<bound>`.

Loop bound (`wrangle::loop_bound`): the linear issue, unit and fan-out terms;
a latency term where a read with no earlier writer takes the body's last
writer (previous iteration) as producer; the throughput of Q/P writers; at
least two. The probes start at vcl's period.

## Watchdog and step budget

vcl bounds each wrangler run by CPU time: wrangler B `to_do/5+1` seconds
linear and `to_do/40+3` for loops, at least the `-t` value; wrangler A
`to_do/5+4` and `to_do/30+6`. An infeasible period is only abandoned when
every strategy times out. The port cannot reproduce a CPU alarm, so it uses a
deterministic budget of 20 000 steps that also counts wrangler A re-ranking.

Observations from the original's runs (tools under `local/tools`, evidence
under `local/evidence/lpgen/watchdog` and `local/evidence/native`, local, not
tracked):

- The alarm is host process CPU time times `--time-scale` under the emulation
  harness (`tools/run_oracle.py`). `to_do` and the seconds formula agree
  on every traced run.
- CPU time is noisy and no work counter predicts it well (per-step cost varies
  roughly 400-1200 us). Runs within about 20% of their limit are not
  reproducible.
- Native runs of the original (x86-64 Linux CI runners, three runs per corpus
  input, compared with `local/tools/compare_native.py`) give identical output
  across runs and match the emulated corpus on 47 of 49 inputs, so emulation
  speed hardly matters. Searches that time out at scale 1 also time out
  natively.
- The two exceptions are general_quad and general_nospec_quad: natively, period
  47 `normal1` finishes, so the loop is at period 47 rather than 48. The corpus
  reference for these two is the native output (`native-stdout.bin` beside the
  emulated `stdout.bin`, provenance in `native-stdout.json`).
- Period 47 `normal2`, which the port solves in 473 steps, still times out
  after 9 native seconds.

`tools/run_native.py` runs the original the same way on any x86-64
Linux machine.

## Wrangler B loop branches

- Completion with an empty to-do list: end = period, then loop side-effect
  statistics (yielding `a`, `cond`, `store`, `critical`). If `critical <
  cond` or `store < cond - known_safe`, store `cond` as the new floor, mark
  stores with absolute line `< period*(cond-safe)` and criticals with line `<
  period*cond`, and backtrack to them.
- A ranked move instruction with pipes 3 gets a pipe from the two per-pipe
  counters and the restart flag.
- Before checking: lock the line, drag setters (table, instruction, position,
  `period>>1`, or `period` if full-period, 0), unlock.
- Run the check. If only `MOVE_NEEDED` remains: drag setters again (mode 4,
  full period); if still `MOVE_NEEDED`, synthesize a move (returns 0..4);
  nonzero means backtrack.
- The backtracking threshold is capped at `period*16`.
- Wrangler A's backtrack keeps an exhausted move pending with a raised
  threshold and drops it only past 16 periods; wrangler B drops it at once.
  The port follows each.

## Checker differences in loop mode (global period)

- Positions: absolute line, folded row = line % period, iteration ID (lid) =
  line / period. Dispatch slots are indexed by folded row.
- Minimum position / end distance use the folded row; a failing minimum jumps
  to the next absolute line with that folded row; lid > 16 fails.
- Interval overlap and throughput are cyclic.
- Input check: a read more than one period after its producer needs a move (if
  the input is movable, otherwise a hard conflict); loop-carried external
  inputs have their own window against the first external output.
- Prep: lists used by movable instructions are marked movable;
  throughput-tracked registers get no implicit external lock.
- A loop write operand is movable only when its register reference's output
  mask is clear, and move synthesis clears the full-period flag on a write
  that is not movable. The port treats a write of a register that escapes the
  table (`Operand::escapes` on any write) as fixed.
- The loop table carries no forced memory/order links: the strategy runs work
  on a clone whose forced-link list is empty, so a store is a candidate as
  soon as its own inputs are placed. The port gives loop tables no forced
  links.
- Each read (the first read of a register part per instruction) is tagged with
  its instruction's pipes (lower only, upper only, both). Linking adds these to
  three producer sums, which the statistics step turns into the lines the
  readers need to issue: a `div` with two upper-pipe `mulq` readers lives until
  ready+1. The port computes these sums.
- Move detection gives every detected copy a move ID, integer `imove` included
  and not only the dual-issue `vmove`. A ranked copy whose preferred pipe is 1
  while the lower-only count fills the period restarts the search from the head
  of the done list. The port's `dual_copy` covers both. A dual-issue source
  copy also takes its pipe from the issue counters before placement in a loop,
  and goes back to dual-issue when undone.
- A synthesized move's register is a fresh virtual one even when the moved
  value lives in a fixed register (`move__m11 <- VI04` is allocated VI09 in
  the loop table). Loop local allocation keeps a move's register local:
  readers retargeted to it still carry their old outside-source flag, which
  would otherwise make it look external.
- A copy found by move detection that the loop allocation puts on its source's
  register is dropped like a synthesized move, and leaves both the schedule
  and the loop's source list. A move made equal later by a part's own
  allocation stays.
- The output check in a loop skips the list's entry value only when it has no
  users or the write is the external output; otherwise the entry value takes
  part in the overlap check.

## Port behaviour after scheduling

- Reduce extents runs on every successful candidate, over the folded rows with
  absolute lines. It sinks loads, `ilw` and pointer updates toward their first
  use.
- Loop local allocation (including its raster and live-range steps) folds rows
  modulo the period: an incoming value starts with the iteration block of its
  last use and an escaping value ends with the block of its write. A candidate
  that fails allocation is rejected. Move registers are numbered across all
  loops of a region.
- vcl expands normal loops after webs and external assignment, so the port
  expands on the webbed region (operand spellings synced to web names). Moves
  become generated `vmove`/`imove` copies; retargeted operands are rewritten
  by operand index.
- Every clone keeps the loop allocation's register: each access is pinned
  (`Region::pinned`) and the web builder forces its web to that register. In
  each prologue/epilogue part vcl then splits a loop-allocated register that
  is neither live into nor out of that part into fresh virtual registers
  (`VF04__0`, `VF04__1`); the port drops those pins and the part's local
  allocation makes the same choices. The split counts values passing through
  a part without a reference there: the loop allocation may pack a local `.w`
  value into a register whose `.xyz` holds a loop invariant, and then the
  register is kept. Registers bound by the first external assignment (before
  loop optimization) stay bound after expansion as pins on every access; see
  [LOOP_BINDINGS.md](LOOP_BINDINGS.md).
- The web builder maps an instruction operand to the effective access with the
  same components when one register is read twice from different webs
  (`mulw.xyz v, v, vw`).
- Prologue/epilogue parts are installed tables (`_LNOPT_w=[ ]`): no strategy
  search. Their expanded rows go through local allocation, raw reduction (pull
  and drag), size reduction and empty-row compression, which is what produces
  the accepted latency stalls. A fixed table's lower half reads with its upper
  partner in flow analysis, so the table builder looks producers up through
  the pair owner. Installed parts are raw-reduced even under `-C` (which only
  disables size reduction and compression), and keep their branch to the exit
  point (`b EXIT_POINT` at the end of each non-last epilogue) rather than
  folding it into an E-bit end; when that exit point stays, the last epilogue
  falls through to it instead of taking the E-bit.
- A synthesized `imove` uses component x on its operands, like parsed VI
  operands.
- Move undo walks the to-do list from its tail to the move just put back at
  the head and destroys every pending move; placed moves stay.
- Setter dragging skips producers that are not placed. It first sinks loads: a
  load, or a producer with dynamic depth zero, whose readers are all placed
  moves to the latest line that still feeds the placed reader.
- Q and P are not interlocked, so a DIV/EFU result is visible only after its
  latency. Flow analysis (`flow::Timing`) gives fixed tables their issue
  cycles and keeps such writes in flight until they land, possibly in the next
  part or kernel pass; a write in flight does not end the old value in
  liveness, but liveness still ends a value at the issuing write. MAC,
  STAT_ALU, STAT_DIV and CLIP writes are delayed too (ACC is forwarded and
  stays immediate).
- External timing bounds a writer of a throughput-tracked register by writers
  in other tables: minimum position `throughput - (distance + rows to end) -
  1`, end distance `throughput - (distance + row + 1)`. It measures a reduced
  table to its end before the exit padding, because tidy pads after every
  table is reduced. The timing context looks up a paired lower instruction's
  reads through its upper partner. Late reduction visits streams in order, so
  each table's external timing sees the tables reduced before it.
- A non-interlocked hazard is a read whose latest earlier write is still in
  flight (compared with the definition visible with and without timing). A
  live read in a fixed table that issues while a Q/P write from another table
  is still in flight sees the older value. The reader's and the writer's
  stream references get hazard flags, and external timing turns them into
  protected operands (operand mask AND reference bits). Raw reduction never
  pulls a protected write earlier and never drags a protected read later.
  Protection is per operand and covers only values crossing the table
  boundary: a write is protected only when a reader in another table sees it
  by timed flow (a `div` overwritten later in the table can still be the value
  the next table reads), and a read only when its producer is outside the
  table. Flow reports these as `hazard_reads`/`hazard_writes` (block,
  register, component).
- Flag-producer scratch destination exists only for its ABS cap (e.g. `opmsub
  VF18` followed by `abs.xyz VF00,VF18`); a producer that gets no cap writes
  VF00. The scratch register must be free in the merged table (the epilogue
  merges with the code after the loop), so the choice is re-checked after
  merging. A dead xyz-only destination (OPMSUB, operand class 11) is zeroed
  like a class-1 one; the cap step then gives it a scratch register free in
  that table (not in the whole region), except in a fixed kernel, which keeps
  VF00 without a cap. Raw reduction renumbers registers, including an
  instruction's first output (used by the cap for its source and mask).
- Immediate coalescing after expansion ignores dead clones.
- Stream merging treats installed parts as linear vsm tables: a loop's empty
  entry and PRO0 merge with a preceding `main_loop_lid` table, and the merged
  table is raw-reduced as one (`build_wrangle_table_span`). Each member keeps
  its own read pairing.
- Parts are named after the loop stream's first label, and other streams'
  branches to the loop's labels are retargeted to its entry part; the entry
  prints old labels only at the region start or where something still names
  them.

## Verification

- `crates/oxide-vcl/tests/lpgen_probes.rs`: outputs equal the original's
  (normalized) for eight of nine probes; `clip` pipelines at period 10 where
  the original's searches timed out until period 11.
- `crates/oxide-vcl/tests/public_corpus.rs`: every public corpus program matches
  the reference output; general_quad and general_nospec_quad match their
  native references.
- Per strategy run, `local/tools/compare_lpgen.py <source>
  local/evidence/lpgen/<probe>/trace.json.gz` compares every traced run,
  including allocations. Matching strategy runs: loop-vector-counted,
  safe-hints, chain, two-loads 11/11 each; lqi-sqi, transform 22/22 each; clip
  20/20; project 10/11; add-stream 10/11.

Probe inputs are `local/evidence/inputs/lpgen-*.vcl` (summaries from
`local/tools/summarize_lpgen.py`):

| Probe | Period | Notes |
|---|---|---|
| loop-vector-counted | 4 | all strategies identical, no moves, lid 0 |
| lpgen-safe-hints | 5 | `another`/`hDown` add 3-4 moves (lid 4); others none (lid 2-3) |
| lpgen-project | 7 | every strategy drags setters and adds 3-10 moves |

vcl splits values per definition (`v__0`, `v__1`) and names moves `move__mN`.

## Known limitations

- Individual strategy runs search differently from the original in places:
  tri's period 36 Vuta times out in the original where the port finishes, and
  Normal2/Another/Normal/HeightDown finish in the original and exhaust the
  port's budget; quad's period 47 Normal2 times out in the original but the
  port succeeds; clip's period 10 Dumb2 times out in the original against 53
  port steps. The accepted loops match the references; a step budget cannot
  reconcile such runs.
- Open differences in the search: `project`/`dumb2` (wrangler A has a pending
  move, sequence number `maddw+1`, as a candidate right after `maddw` is
  placed; the port has none) and `add-stream`/`dumb` deep in backtracking.
- Stalls in the flow timing of compressed tables are not ported (flow runs on
  the expanded layout), nor are loop-specific acceptance gates: random-unit and
  external-transfer operations keep their linear schedule.

## Debugging switches

Port: `VCL_LPGEN_TRACE=1` prints every strategy run, the loop allocation
(`regs`) and the chosen candidate. `VCL_PLACE_TRACE` (also prints genmove
results and move pipe choices), `VCL_INPUT_TRACE=index` (also prints the
overlapping value on an input failure), `VCL_DRAG_TRACE`, `VCL_MOVE_TRACE`,
`VCL_CAND_TRACE`, `VCL_LPGEN_STEPS`, `VCL_LINE_TRACE=index` (each line check
of that candidate), `VCL_RAW_MOVES=1` (every raw-reduction pull and drag, with
`VCL_WRANGLE_PROBLEM=1` for the input table) and `VCL_HAZARD=1` (flow
hazards).

Original-side tracing (`local/tools/trace_original.py`, local, not tracked):
the `linear` mode hooks the driver (`loop_strategy`, `loop_wrangled` with
watchdog flag, `loop_allocated`, `loop_accepted`). `TRACE_RUNS=period:name`
(guest strategy names such as `47:normal2`; `linear` also stores events outside
loop runs) stores per-step events only inside those runs.
`TRACE_UNLIMITED=period:name` holds that run's alarm off, `TRACE_WORK=1`
records inserts and undos per run, `TRACE_INSNS=1` guest instructions.
`TRACE_UIDS` selects probes: wrangler A has `register_pin`, `line_search`,
`place_check` and `drag_try`; wrangler B has `b_check`, `input_check` and
`input_overlap`; `local_alloc`, `forced_check` and `raw_reduce` (each placed
instruction's timing bounds and `protected` field), `move` and `local_move_b`
cover allocation and raw reduction. `local/tools/compare_lpgen.py` ranks
instructions from the placed tables, maps a traced run to body indices, and for
stopped runs prints how many of the original's steps the port reproduces.
`local/tools/pair_watchdog.py` pairs the original's watchdog runs
(`trace_original.py watchdog`) with the port's `lpwork` counters.
