# Compatibility and limitations

The port matches the original on all 52 public corpus programs and on the
local corpora ([CORPUS_BASELINE.md](CORPUS_BASELINE.md),
[LOCAL_CORPUS.md](LOCAL_CORPUS.md)). It is not a proven drop-in replacement
for every input the original accepts; this page lists what is implemented and
where the known boundary lies.

## Implemented

- The original's 214 instruction records and 28 directive records are
  extracted from the ELF with a hash check and the original record bytes.
- Old and new syntax, source masks, raw instruction pairs, mapping syntax,
  synthetic blocks, comments and directive spellings are parsed as the original
  does, with tests against the saved original-success inputs.
- The CLI follows the original's byte-selected, attached-value argument parser.
- Generic local occupancy packing uses the original per-component cost,
  physical-register tie-breaking and virtual lifetime priority.
- Scheduling priorities and the pipe, resource and modulo constraints are
  reconstructed. The graph engine uses a bounded search; it does not reproduce
  all of the original's intrusive lock mutations.
- The compiler integrates parsing, flow, pruning, scheduling, allocation and
  text rendering natively. It does not delegate to the original or to openvcl.
- Component propagation, inherited sticky-status entry tokens, operand webs,
  raw paired reads and raw delay-slot transfers are integrated. CFG liveness
  extends allocated lifetimes across loop backedges.
- Explicit raw multi-stage loops, generated early exits and drains,
  flow-derived register versions, affine repair for scheduled loops, expanded
  VI update coalescing, and generated-part extent cleanup with termination
  folding are integrated. Normal counted loops that cannot be pipelined fall
  back to linear scheduling transactionally; raw staged expansion is mandatory.
  See [PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md).
- Numeric coalescing uses complete operand users and escaping outputs. Proven
  copies resolve to the original IADDIU/MOVE/MAX encodings. Fixed raw identity
  operations keep their source encodings; live flags and VF00.w are respected.
  The original's internal status-zero failure is an error in the library and
  is classified separately from compilation by the replay tool
  ([COPIES.md](COPIES.md)).
- Ordinary VI branch tails use a validated lower delay exchange. Unique
  ordinary fallthrough tables concatenate after allocation with the original's
  export, join, fence and debug gates. CONT emits its own E termination and
  label flags reach the first upper instruction
  ([BRANCH_STREAMS.md](BRANCH_STREAMS.md)).
- Eligible raw reorder prologues and drains admit physical pull-up and
  drag-down after allocation, with initial-stall, actual-producer, physical
  alias and observed hardware guards. Ordinary VI tails distinguish future UTA
  users from selected users; fixed predecessors and shortest intermediate CFG
  distances supply conservative incoming timing
  ([RAW_BOUNDARIES.md](RAW_BOUNDARIES.md)).
- External assignment is distinct from local packing for ordinary unscheduled
  regions: descriptor-order masks, all-stream exclusions and first-clear
  choices pin VF/VI values before scheduling
  ([EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md)).
- Explicit errors include the original's work-area configuration failure
  (the shipped CPU profile has no memory pool, and the port does not invent a
  spill allocator), conflicting interface mappings and unresolved indirect
  branches.

## Known limitations

- `-t`, `-S`, `-D` and `-Q` are accepted without effect; the port's search
  has no time limit.
- Allocator-generated register moves, copy-web allocation and ordering, and
  the original's retries are incomplete. Source-level identity detection does
  not reproduce the full move and lock engine, and the web builder does not
  reproduce every external-allocation transformation. The generated-move
  creation, value-lock and undo subsystem is not implemented and is not a
  spill fallback.
- Raw branch-delay output normalization and cross-stream timing are partial.
- Broader memory and implicit-output delay motion, selected movable-table
  merges and the complete external readiness and tail tracker are missing.
  Some unreferenced native labels remain visible.
- Raw synthetic Entry/Exit merging and tidy-pass ordering are incomplete.
  Minimum-count raw controls keep an entry placement difference and extra
  termination rows; modifier-bearing and live MAC frames are conservatively
  frozen. The original's hazard-broadcast gate, alternate pipes and complete
  locks are not reconstructed ([RAW_BOUNDARIES.md](RAW_BOUNDARIES.md)).
- Physical assignments are not preserved through normal modulo
  scheduling, local allocation, expansion, external binding and rebuilt webs
  for selected, raw and expanded-loop regions, and the original's lazy web
  discovery ordering is not ported.
- Inter-stream readiness, branch-to-register inference, calls and returns,
  entry/interface binding and exit-through dependencies are partial
  ([INDIRECT_FLOW.md](INDIRECT_FLOW.md)).
- Pipeline strategy, move and allocation selection, and late optimization of
  generated prologues and drains are incomplete. Arbitrary writer motion, raw
  reorder cleanup and complete boundary readiness are missing.
- The original's final cap, synchronization and row-compression passes are
  partly ported, including their observed quirks; timing validation across
  stream boundaries is incomplete ([FINAL_REDUCTION.md](FINAL_REDUCTION.md)).
- Some special emitter hooks and memory-alias ordering refinements are
  missing.
- Generated labels, diagnostic formatting, statistics, preprocessing flags and
  label-header output follow the recovered paths only in part
  ([OUTPUT_FORMAT.md](OUTPUT_FORMAT.md)). In vcl mode the listing carries
  vcl's banners, table comments, register statistics and `-c` comments, but
  not its loop optimizer report; the bracketed numbers of a table comment
  and some stall notes can differ, as can diagnostics.
- Some valid programs remain outside complete compiler coverage.
- Immediate expressions: vcl 1.4beta7 accepts some that the port rejects,
  and keeps them as written: a missing left operand (`*3`, `2+*3`, `**3`)
  and empty parentheses after an operand (`pi()`, `foo()()`; `2()` is 2).
  For an immediate it rejects, vcl also prints the instruction with the bad
  operand marked and its expected format (`USER: couldn't decode
  instruction`), which the port leaves out. vcl reads a trailing operator
  (`3*`) as an evaluation failure, the port as a syntax error, so their
  first lines can differ.

## Comparing outputs

Original stdout includes comments, optimizer decisions and source-derived
labels. Three different comparisons are needed:

1. Byte-for-byte output and exit status, where exact compatibility is intended.
2. Normalized instruction pairs, end bits, labels, operands and inserted NOPs.
3. Register/data/control-flow behavior and actual timing when different valid
   schedules are produced.

`tools/compare_outputs.py` keeps bytes, normalized schedules and annotated timing
metrics separate. Schedule inequality alone proves neither a semantic failure
nor equivalence. A passing emulator run establishes behavior only for that
fixture and the recorded emulation/timer conditions.
