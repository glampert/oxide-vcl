# Program structure and reconstruction

## Artifact and toolchain

The supplied `vcl` is a dynamically linked ELF32 Intel 80386 executable. Its
SHA-256 is `83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`.
The help routine identifies itself as VCL
1.4beta7, built at 15:26:57 on December 17, 2004 for Linux. The compiler comment
section names GCC 3.2.2. These embedded identifiers describe the artifact; they
do not authenticate its provenance. The port is reconstructed from the
original's observed behaviour; the original is not modified or redistributed.

## What the program does

VCL translates a source language with virtual VF/VI registers, register
interfaces, control-flow labels, and optional loop hints into paired VU
microassembly. It emits assembler text, not PS2 machine code. The output is
intended for a VU-capable assembler and can include `.vu`, alignment, globals,
and DMA/MPG wrappers.

The two instruction columns are the VU upper and lower issue slots. A pair can
stall together. Emitted pair count therefore differs from execution time. VCL
exploits this distinction: its final reduction can replace explicit empty rows
with hardware interlocks or synchronization instructions and annotate remaining
stalls in comments.

Inferred from vcl 1.4beta7, it is C++ with virtual register pools, a CPU model,
stream objects, instruction tables, operands, register locks, and register-web
strands.

The focused binary walkthroughs cover
[indirect flow and interfaces](INDIRECT_FLOW.md),
[final dispatch reduction](FINAL_REDUCTION.md), and
[assembly, headers, and preprocessing](OUTPUT_FORMAT.md).

## Main path

The original's main path, inferred from vcl 1.4beta7:

```text
arguments → input bytes → optional preprocessors → parser
    → decoded lines → streams and control-flow graph
    → component flow / memory dependencies / statistics
    → pruning and register webs → external register allocation
    → loop optimization and expansion → rebuild flow and prune
    → raw handling and linear optimization → local register allocation
    → stream merging / moves / reduction / end handling
    → caps and empty-row compression → final validation / stalls
    → assembly text and optional label header
```

The original does not perform one analysis followed by one rewrite. It reruns
flow, immediate fixups, pruning, binding, and move normalization after
transformations, so each later pass reasons about the new graph.
[SCHEDULING.md](SCHEDULING.md) gives the observed pass order.

## Original pipeline stages

Inferred from vcl 1.4beta7, the original divides into these stages, in
roughly the order they run:

- Main: options, pass driver and file output.
- Help and version text, matched by exact strings and the runtime `help-short`
  case.
- Preprocessor staging: caller and process command construction.
- Parser setup: initialize parser buffers and source locations, then construct
  the processed parser buffer (lexical processing and source locations).
- Instruction lookup (case-insensitive descriptor walk) and initialization of
  the instruction definitions (214 records).
- Decoded line entries: source parser output becomes decoded instruction
  entries.
- Stream splitting: split decoded entries into streams, with modes, labels,
  branch targets and synthetics.
- Branch-link resolution: direct/indirect target and continuation handling.
- Dependency rebuild: component and memory dependencies, using the emulator,
  backtrace and trace-pair calls.
- Dispatch emulation: simultaneous issue and register timing for one dispatch
  entry.
- Fixed-point pruning: repeats pruning until zero changes.
- Register-web gather and patch: per-table strands, merge, patch.
- External assignment across streams, with pool callbacks and operand
  rewrite.
- Multi-strategy linear optimization: descending strategy trials.
- Increasing-period loop search: loop bounds, trial scheduling, allocation.
- Loop expansion: entry/prologue/kernel/epilogue construction.
- Local allocation after scheduling: state checks and per-pool assignment.
- Generic local pool packing: occupied-row priority and best-fit assignment.
- Cap pass over streams: per-stream external preparation and reduction.
- Empty-row compression pass: per-stream reduction wrapper.
- Assembly emission: statistics, file preamble, streams and footer.
- User label list emission, which calls each stream's label emitter.

## Data flow

A VF has four independently tracked components. VCL associates each instruction
operand with direction, component mask, output availability offset, blocking
mode, source location, and register-web information. Reading one component does
not initialize the others. Writes to VF00 and VI00 are excluded from ordinary
output dependencies. Register names may refer to a virtual value, a physical
base register, or a forced mapping.

Consider this successful original input:

```asm
.init_vf_all
--enter
in_vf a(vf01)
--endenter
mul c, a, a
add b, c, a
--exit
out_vf b(vf02)
--endexit
```

The read of `c` depends on the multiply's result. `a` is live until its last
read. `b` must be in VF02 on exit. The allocator can put `c` in VF02 too because
the read of `c` and write of `b` share an instruction: the old value is consumed
before the replacement becomes visible. With no `.init_vf...` directive, this
particular temporary-allocation case fails in the original. Fixed interface
registers do not automatically expose the entire register pool as free scratch.

The output's two instruction rows can still represent five cycles: the second
row stalls for the pending VF result. See `local/evidence/oracle/alloc-vf-all/` (local, not tracked) and
[RUNTIME.md](RUNTIME.md), including the observed end-bit deletion under the
original reduction pass.

## Rust design

The reconstruction uses owned vectors, typed source statements and explicit
errors. It does not reproduce the original fixed global-array limits merely
for implementation resemblance. Integer-overflow checks remain enabled in
release builds; unsafe code is forbidden.

The library crate, `crates/vcl`, groups its modules by phase. The
command-line driver is the separate `crates/oxide-vcl` crate.

| Module | Responsibility |
| --- | --- |
| `isa::instruction` | Generated original table, masks, operand classes and lookup |
| `parser`, `parser::expression` | Lexing, directives, old/new syntax, raw pairs, typed source IR; immediate expressions |
| `analysis::flow` | Component reaching definitions, reachable blocks, liveness and dead chains |
| `analysis::web` | Operand-level register strands |
| `analysis::prune`, `analysis::copies`, `analysis::induction` | Liveness pruning, identity recognition and internal moves, atomic VI immediate-coalescing plans |
| `analysis::control`, `analysis::status`, `analysis::memory` | Stream end kinds, status-flag resources, work-area and memory-dependency facts |
| `scheduler::schedule` | Recovered priorities, two-slot bounded backtracking and modulo constraints |
| `scheduler::wrangle`, `scheduler::timing` | The linear table optimizer core; replay-validated empty-row removal |
| `scheduler::linear_cleanup`, `scheduler::raw_cleanup`, `scheduler::boundary` | Movable generated-table extents, raw-table reduction, inter-stream issue distance |
| `loops::loop_schedule` | Counted single-stream recurrences, bounded modulo search and effect ages |
| `loops::pipeline`, `loops::loop_ir`, `loops::immediate` | Clone windows, generated control flow, typed clones and affine repair |
| `allocator::allocation` | Per-component occupancy, forced mappings and original packing score |
| `allocator::external_allocation`, `allocator::lifetime` | Assignment across streams; component/stream intervals and exclusive issue locks |
| `compiler` | Source/stream lowering, analysis and scheduling/allocation integration, one file per phase |
| `emit` | Register/operand rendering and paired text layout |
| `diagnostics` | Modes, typed messages and the reporter that prints all output |
| `oxide-vcl` (driver crate) | Original argument rules, file I/O and preprocessing |

### Modes and diagnostics

`vcl::Mode` selects the compiler: `Vcl`, the default, generates vcl
1.4beta7's code, which the tests assert; `Oxide` (`--mode=oxide`) enables
the port's own fixes and optimizations, which may break compatibility. The
mode travels in `vcl::Options::mode`; code that departs from vcl checks it
and keeps the vcl path unchanged. The modes differ in their exit statuses
(`Mode::failure_status`, `Mode::usage_status`: `VCL_EXIT_FAILURE` 255,
`OXIDE_EXIT_FAILURE` 1, `OXIDE_EXIT_USAGE` 2), field letters in names (an error in oxide mode), spilling into a `.vu_work_area`,
`loi`'s math functions, and the fixes for vcl's defects: oxide mode enables
every `vcl::Fix`, which vcl mode enables one by one with `--fix=`
([VCL_BUGS.md](VCL_BUGS.md)). A fix is checked with `Options::fixes(fix)`,
and `Source::fixes` carries the set into the compiler. The mode does not
change how messages or the listing's comments are printed.

Every message is a typed value in `vcl::diagnostics`, never preformatted
text: `Message` groups `CliMessage` (command line), `DriverMessage` (files,
streams, preprocessors), `ParseMessage` and `CompileMessage`.
`CompileMessage::Internal` carries a failure of one of the port's own phases,
for which vcl has no message. A `Diagnostic` adds severity and source line.
Each message has one wording (`diagnostics/oxide.rs`: `Wording`, its text,
the token it is about and a suggested fix); vcl 1.4beta7's texts are not
reproduced. `Reporter` owns standard output and standard error and prints
everything the driver writes (notices, warnings, compile and driver errors,
the help screen and the assembly). It prints each diagnostic as
`file(line): severity: text` with the source line it was given
(`Reporter::set_source`), a caret under the token and the fix, counts
warnings and errors for `--warnings-as-errors` and the closing count, and
prints nothing on standard output for a failure. `CompileError` and
`Compiled::warnings` carry these values; their `Display` is `line N: text`.
Developer traces print through `vcl::trace!`, or `vcl::trace_if!` with the
name of a switch; the switches are the `VCL_*` environment variables, read
once into `vcl::debug::Switches`.

Unit coverage of a score or allocator does not establish the original
scheduler's complete lock, move, loop-expansion, or final-reduction behavior.
[COMPATIBILITY.md](COMPATIBILITY.md) lists the current boundary.

## Compiler orchestration

The compiler expands explicit raw stages before strict dependency checking,
matching the original's early raw expansion. It validates and prunes
unscheduled sources before searching eligible normal loops. Generated parts
preserve pair ownership; flow and webs are rebuilt after cloning and immediate
repair. See [PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md).

`compiler` retains selected table rows for provenance and paired-read
ownership, with an explicit fixed/linear mode. Linear normal parts admit extent
cleanup; modulo kernels and explicit raw layout keep their coordinates.
Allocation uses the changed issue rows. Final replay and compression then
compute physical rows and header offsets. [LINEAR_CLEANUP.md](LINEAR_CLEANUP.md)
distinguishes these passes and the original mode and latency gates.

General scalar coalescing runs before normal loop generation and again on
expanded code. It indexes complete users across blocks and preserves declared
outputs and ambiguous definitions. Source instructions carry an original
operand conversion selector and an optional compiler-created copy marker;
resolved copies are emitted as IADDIU, MOVE or MAX. Source-written abstract
moves keep their error. See [COPIES.md](COPIES.md).

Ordinary scheduled branch frames keep real placement constraints separate from
source-order barriers. A bounded lower-pipe tail exchange runs before local
allocation. Direct ordinary predecessors contribute only the remaining incoming
readiness debt, counting rows guaranteed to survive final replay. Unique
ordinary fallthrough emission tables concatenate after allocation; source
CFG and lifetimes stay intact. Explicit CONT ends, barriers and label debug
flags survive source splitting. See [BRANCH_STREAMS.md](BRANCH_STREAMS.md).

Raw reorder code has a separate `RawLinear` mode. Its reduction runs after
physical allocation, projects only proven unobserved implicit FMAC flags,
checks initial stalls, and builds actual RAW plus physical WAR/WAW constraints.
The raw reducer preserves paired old reads while moving rows; final physical
replay accepts or rejects its candidate. Ordinary incoming readiness uses a
shortest CFG path through guaranteed duration lower bounds, with fixed-source
timing and outgoing VI tail debt kept separate. See
[RAW_BOUNDARIES.md](RAW_BOUNDARIES.md).

Ordinary regions prepare descriptor-order external-reference summaries after
webs and before scheduling. The separate external allocator compares all
stream views and chooses the first clear physical exclusion byte; its
assignments become pins for the local lifetime allocator, so the decision
holds even when a later schedule shortens a source's last-read lifetime.
Raw, selected and expanded-loop regions do not use it. See
[EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md).

## Building and running

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo run --bin oxide-vcl -- -h
cargo run --bin oxide-vcl -- -uEXAMPLE local/evidence/inputs/sched-add-symbol.vcl
```

The tools that run the original binary are local and not tracked. Qiling is
needed only to run the Linux oracle, not to build or run the Rust program. See
[RUNTIME.md](RUNTIME.md) for the isolated, hash-checked oracle workflow.
