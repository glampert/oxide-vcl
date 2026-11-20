# Oxide VCL

## A Rust replacement for the PlayStation 2 Vector Unit Compiler.

`oxide-vcl` compiles VCL, the PlayStation 2 Vector Unit language, into scheduled
VU assembly. It is a Rust reimplementation of SCE's `vcl 1.4beta7` that aims to
produce byte-identical output given the same input and options. `vcl 1.4beta7`'s
behaviour is the authority: it was established by analysing and running the original
executable, with the PS2 manuals for hardware context.

To write programs for it, start with the [VCL language reference](docs/VCL_REFERENCE.md).
The syntax, every `--` annotation, `.` directive, and command line option is described there.

## Status

- All 52 public corpus programs in `tests/corpus/` get the original's result:
  47 compile to the same listing and 5 are rejected as the original rejects
  them. An extended local corpus of disassembled VU programs (117 cases, not
  distributed here) matches too.
- Matching the corpus does not prove equivalence on programs it does not
  cover. Known gaps are listed in [COMPATIBILITY.md](docs/COMPATIBILITY.md).
- Defects of the original that the port reproduces by default, and the
  opt-in fixes for them (`--fix=`, or every one with `--mode=oxide`), are in
  [VCL_BUGS.md](docs/VCL_BUGS.md).
- `-G` and `-g` preprocess the source in the same run, with the port's own
  preprocessor: [vclpp](https://github.com/glampert/vclpp)'s C directives,
  block macros and MASP mode ([VCL_REFERENCE.md](docs/VCL_REFERENCE.md#preprocessor)).
- Oxide mode also optimizes beyond the original: `exit`, `loops` and
  `unswitch`, each of which `--no-opt=NAME` turns off. They are described
  in [VCL_REFERENCE.md](docs/VCL_REFERENCE.md#optimizations) and
  [OPTIMIZATION_PLAN.md](docs/OPTIMIZATION_PLAN.md).
- [VUSL](#vusl), a GLSL-like language for VU1 microprograms, compiles to VCL
  through oxide-vcl and writes a C++ header and JSON describing the program's
  memory for the host. Its ports of the Quake 2 and Doom 3 programs in the
  corpus draw the same packets as the hand-written VCL on every test scene,
  in fewer cycles ([VUSL_PERFORMANCE.md](docs/VUSL_PERFORMANCE.md)).

## VUSL - The Vector Unit Shading Language

VUSL (Vector Unit Shading Language) is a GLSL-like language for PlayStation 2
VU1 microprograms. `oxide-vusl` compiles a VUSL program to VCL and hands it to
oxide-vcl in the same process (in oxide mode), which schedules, pairs and
allocates it into a `.vsm` listing for `dvp-as`. Every construct has a fixed,
documented lowering: an operation the VU has no instruction for is an error
that names the explicit form, so the source shows what the code costs.

Memory is described with plain structs, which `main`'s parameters bind to VU1
data memory:

```glsl
// transform.vusl: Transforms a batch of vertices to clip space and tints them, in place.
#pragma program(VU1Prog_Transform)

struct Frame {
    mat4 mvp;
    vec4 tint;
};

struct Vertex {
    vec4 position;
    vec4 color;
};

struct Batch {
    layout(lane = w) int16 count;   // the header qword's w
    Vertex vertices[count];
};

void main(layout(address = 0) uniform Frame frame,
          layout(xtop, vif_base = 5, vif_offset = 509) inout Batch batch)
{
    for (inout Vertex vertex : batch.vertices)
    {
        vertex.position = frame.mvp * vertex.position;
        vertex.color = vertex.color * frame.tint;
    }
}
```

- `frame` holds constants the host uploads to address 0; `batch` is the
  half of the VIF1 double buffer that XTOP names.
- The range-for walks `vertices`, whose length is the `count` member, and
  `inout` writes each element back. It becomes a do-while over a pointer,
  with the matrix and the tint loaded once before it; the transform is a
  `mula`/`madd` chain.

```sh
target/release/oxide-vusl --header=transform.h -o transform.vsm transform.vusl
```

writes the listing (47 instructions) and a C++17 header with the layout of
every struct, checked with `static_assert`s, and the program's constants. Its
part for this program (the header also defines the VUSL value types, `vec4`,
`mat4` and the rest, once for every generated header):

```cpp
// transform.h: Generated C++ header from transform.vusl.
namespace vusl::VU1Prog_Transform
{

struct alignas(16) Frame
{
    mat4 mvp;  // qwords 0-3
    vec4 tint; // qword 4
};

struct alignas(16) Vertex
{
    vec4 position; // qword 0
    vec4 color;    // qword 1
};

struct alignas(16) Batch
{
    uint32 _pad0[3];  // unused
    int16_lane count; // qword 0, w
};

// Qwords: offsets from the struct's start, and sizes.
constexpr std::uint32_t kBatchVertices = 1; // Batch::vertices
constexpr std::uint32_t kBatchVerticesStride = 2; // qwords per element

// main's parameters: qword addresses in VU1 data memory.
constexpr std::uint32_t kFrameAddress = 0; // frame: uniform Frame

// batch: inout Batch, at XTOP in the VIF1 double buffer:
constexpr std::uint32_t kBatchVifBase = 5;
constexpr std::uint32_t kBatchVifOffset = 509;
constexpr std::uint32_t kBatchMaxCount = 254; // the largest count for which the double-buffer half holds it

constexpr std::uint32_t kInstructions = 47; // 64-bit instructions in micro memory

} // namespace vusl::VU1Prog_Transform
```

The host code can then build its batches with these types and size them with
`kBatchMaxCount`, instead of keeping its own copy of the layout in sync.
`--reflect=FILE` writes the same description as JSON, and `--emit-vcl=FILE`
the generated `.vcl`.

More:

- [VUSL reference](docs/VUSL.md): the language, the cost of every construct,
  pragmas, libraries, reflection and the command line
- [The VUSL compiler](docs/VUSL_COMPILER.md): phases, lowering, optimizations
  and tests
- [VUSL ports against the hand-written programs](docs/VUSL_PERFORMANCE.md):
  Quake 2's particles and textured triangles, Doom 3's world programs, and the
  Doom 3 clipper as a shared [library](docs/VUSL.md#libraries).

## Performance against [openvcl](https://github.com/ps2dev/openvcl)

On eleven public corpus programs (quake2-ps2, doom3-bfg-ps2, ps2gl, tyra and
openvcl's own), scored by openvcl's own static cost analyzer, **openvcl 0.4.0's
code needs 1.33x to 1.75x the estimated cycles of oxide-vcl's** in eight, and
the smallest, a 6-cycle pass-through, is level. In the other two the totals
favour openvcl only because they count each wind-down copy of oxide-vcl's
software-pipelined loops once; the loop that repeats per vertex is 1.95x to
3.4x shorter per pass in oxide-vcl across all the pipelined loops. Most of the
gain comes from pairing lower-pipe loads, stores and integer updates with
upper-pipe arithmetic and from filling latencies with independent work,
where openvcl issues mostly one pipe at a time and pads with `nop` rows.
See: [OPENVCL_COMPARISON.md](docs/OPENVCL_COMPARISON.md) for a detailed breakdown.

### Performance comparison on [Quake 2 PS2](https://github.com/glampert/quake2-ps2)

Captured a real PS2 over the gameplay frames of two Quake 2 demos (base2 and
waste2), with Quake 2's two VU1 programs built three ways: by openvcl from
the sources written for it, by oxide-vcl (`--mode=oxide --opt=all`) from the
same sources, and by oxide-vcl from sources restructured for it
(vertex format and warp flag held in registers, each triangle written out
before its clip decision, the GS scale folded into the particle transform).
`GsWait` is the time the EE spends waiting on the GS and VU1 side.

| | openvcl | oxide-vcl | oxide-vcl, restructured VU code |
| --- | ---: | ---: | ---: |
| GsWait, gameplay mean | 3.68 ms | 1.93 ms (−48%) | 1.40 ms (−62%) |
| GsWait, p95 | 6.41 ms | 3.61 ms | 2.71 ms |
| GsWait, heavy views (≥ 4K triangles) | 4.78 ms | 2.56 ms | 1.86 ms |
| GsWait per triangle | 0.87 µs | 0.46 µs | 0.33 µs |
| Dropped steady frames | 30 (0.44%) | 5 | 2 |
| Frames with no margin (EE work 15–16.7 ms) | 262 (3.8%) | 16 | 10 |
| EE work p95, base2 / waste2 | 14.5 / 15.0 ms | 11.8 / 12.4 ms | 10.9 / 11.4 ms |

VU1 was the main thing the EE waited on, so the cycles saved show up directly
in frame times.

## Original artifact (not included here)

- `vcl` (SHA-256 `83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`).
- Linux ELF32, little endian Intel 80386, stripped.
- Embedded identification: "VCL 1.4beta7".

The tools that set up and run the original are in `tools/` (`setup_oracle.py`,
`run_oracle.py`, `replay_oracle.py`, `run_native.py`); the binary, its runs
and the analysis tools that read its output are not tracked. [RUNTIME.md](docs/RUNTIME.md)
describes the emulated oracle and what it needs under a `local/` path.

## Layout

The Cargo workspace has four crates:

- `crates/vcl`, the compiler library, organised by phase: `isa` (the VU
  instruction table), `parser` (source and immediate expressions), `analysis`
  (data flow, register webs, pruning, copies, induction variables, control
  flow, status flags, memory), `scheduler` (linear scheduling, the table
  wrangler, row timing and clean-up), `loops` (modulo scheduling and loop
  expansion), `allocator` (register assignment), `emit` (assembly formatting)
  and `compiler`, which runs the phases over a program. `vcl::compile` is the
  entry point and `vcl::Options` its settings; `preprocessor` runs before it
  for `-G` and `-g` (`vcl::preprocessor::preprocess`).
- `crates/oxide-vcl`, the `oxide-vcl` command-line driver: the entry point, the
  original's command-line rules (`cli.rs`, also a small library target so the
  tests share it) and file handling. Its `tests/` hold the integration tests;
  the public corpus they compile is `tests/corpus/`, and the preprocessor's
  cases are in `tests/preprocessor/`.
- `crates/vusl` and `crates/oxide-vusl`, VUSL: a GLSL-like language for VU1
  microprograms that compiles to VCL, and its `oxide-vusl` driver
  ([reference](docs/VUSL.md), [compiler](docs/VUSL_COMPILER.md)).

The workspace version (`Cargo.toml`) is the project's version; `oxide-vcl -h`
prints it with the build date.

## Build and run

The native Rust executable compiles sources itself. It has no external crate
or emulator dependency; the historical binary is only a research oracle.

```sh
cargo build --release
target/release/oxide-vcl -h
target/release/oxide-vcl -oexample.vsm example.vcl

cargo test
cargo clippy --all-targets -- -D warnings
```

`crates/oxide-vcl/tests/public_corpus.rs` compiles the 52 public programs in `tests/corpus/`
(sources, licenses and the original's outputs; see its README) and requires
the original's result for each. `crates/oxide-vcl/tests/probes.rs` does the same
for the probe suite in `tests/probes/`, small programs written to pin down
single behaviours. `crates/oxide-vcl/tests/local_corpus.rs` does the same for
extended corpora listed in an optional `local/corpus/manifest.json`, for
programs that may not be distributed. Tests that need the local original-binary
probes (`local/evidence/oracle`) carry `ignore = "needs local/evidence"`; a
build script sets `cfg(local_evidence)` when that directory exists, so without
it they are skipped. `.github/workflows/ci.yml` builds and tests both
profiles, and checks formatting and clippy.

## Documentation

Using the compiler:

- [VCL reference](docs/VCL_REFERENCE.md): language, annotations, directives, command line
- [Compatibility and known limitations](docs/COMPATIBILITY.md)
- [Original defects and their fixes](docs/VCL_BUGS.md)
- [Code generation compared with openvcl](docs/OPENVCL_COMPARISON.md)

VUSL, a shading language compiled to VCL:

- [VUSL reference](docs/VUSL.md): language, costs, pragmas, reflection, command line
- [The VUSL compiler](docs/VUSL_COMPILER.md): phases, lowering, optimizations, tests
- [VUSL ports against the hand-written programs](docs/VUSL_PERFORMANCE.md)
- [VUSL libraries plan](docs/plans/VUSL_LIBRARIES.md): code shared by several programs

How vcl 1.4beta7 works, and how the port reproduces it:

- [Architecture](docs/ARCHITECTURE.md)
- [Source language implementation notes](docs/LANGUAGE.md)
- [Legacy field decoding, indexed names and EFU hooks](docs/PARSER_FIELDS.md)
- [Scalar address dependencies](docs/ADDRESS_DEPENDENCIES.md)
- [Memory operations and the dormant memory pool](docs/MEMORY.md)
- [Control flow and loop expansion](docs/CONTROL_FLOW.md)
- [Indirect edges and caller-sensitive tracing](docs/INDIRECT_FLOW.md)
- [Shared subroutines](docs/SHARED_SUBROUTINES.md)
- [Branch-delay exchange and stream boundaries](docs/BRANCH_STREAMS.md)
- [Dependencies, search strategies and timing](docs/SCHEDULING.md)
- [Physical write collisions](docs/TIMING_COLLISION.md)
- [Component pruning and status expansion](docs/PRUNING.md)
- [Definition/use register webs](docs/WEBS.md)
- [Copy recognition and coalescing](docs/COPIES.md)
- [VI immediate coalescing](docs/INDUCTION.md)
- [Affine immediate repair](docs/IMMEDIATES.md)
- [Register allocation](docs/ALLOCATION.md)
- [External assignment and generated moves](docs/EXTERNAL_ALLOCATION.md)
- [Allocation repair and candidate acceptance](docs/ALLOCATION_REPAIR.md)
- [Allocation intervals and issue locks](docs/ALLOCATION_INTERVALS.md)
- [Move rollback](docs/MOVE_ROLLBACK.md)
- [Loop scheduling (lpgen)](docs/LPGEN_PORT.md)
- [Modulo dependency construction and search](docs/LOOP_SCHEDULING_INTEGRATION.md)
- [Software-pipeline integration](docs/PIPELINE_INTEGRATION.md)
- [Loop bindings](docs/LOOP_BINDINGS.md)
- [Typed clones and affine flow](docs/IMMEDIATE_INTEGRATION.md)
- [Generated-part extent cleanup](docs/LINEAR_CLEANUP.md)
- [Raw reorder and external readiness](docs/RAW_BOUNDARIES.md)
- [Final dispatch reduction](docs/FINAL_REDUCTION.md)
- [Assembly output, label headers and preprocessing](docs/OUTPUT_FORMAT.md)
- [Preprocessor](docs/PREPROCESSOR.md)

Testing and evidence:

- [Testing](docs/TESTING.md)
- [Public corpus results](docs/CORPUS_BASELINE.md)
- [Local corpus](docs/LOCAL_CORPUS.md)
- [Instruction corpus](docs/INSTRUCTION_CORPUS.md)
- [Runtime oracle](docs/RUNTIME.md)
- [Optimization plan beyond the original](docs/OPTIMIZATION_PLAN.md)

## License

The project's own work (the compiler, driver, tools, documentation and the
probe programs in `tests/probes`) is licensed under either of

- the MIT license ([LICENSE-MIT](LICENSE-MIT)), or
- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)),

at your option. Unless you state otherwise, a contribution intentionally
submitted for inclusion is dual licensed as above, without any additional
terms or conditions.

**Not covered:**

- The programs in `tests/corpus` keep their own licenses (GPL-2.0, GPL-3.0,
  LGPL-2.1, AFL-2.0, Apache-2.0), recorded in each program's header and in
  [tests/corpus/README.md](tests/corpus/README.md). They are test data; the
  compiler does not include or link them.
- The recorded outputs of SCE's vcl 1.4beta7 in `tests/corpus` and
  `tests/probes` (`stdout.bin`, `stderr.bin`, `result.json`) are reference
  data for the tests, not part of the licensed work. vcl itself is SCE's and
  is not distributed here.
