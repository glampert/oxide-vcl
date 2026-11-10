# Oxide VCL: a compatible VCL compiler

Oxide VCL (`oxide-vcl`) compiles VCL, the PlayStation 2 vector unit language, into scheduled
VU assembly. It is a Rust reimplementation of SCE's vcl 1.4beta7 that aims to
produce the same output, ignoring whitespace and comments, for the same input
and options. vcl 1.4beta7's behaviour is the authority: it was established by
analysing and running the original executable, with the PS2 manuals for
hardware context; openvcl's implementation was not used.

To write programs for it, start with the [VCL reference](docs/VCL_REFERENCE.md):
the language, every `--` annotation and `.` directive, and the command line.

## Status

- All 52 public corpus programs (`tests/corpus/`) get the original's result:
  47 compile to the same listing and 5 are rejected as the original rejects
  them. The local corpus of disassembled VU programs (117 cases, not
  distributed) matches too.
- Matching the corpus does not prove equivalence on programs it does not
  cover. Known gaps are listed in [COMPATIBILITY.md](docs/COMPATIBILITY.md).
- Defects of the original that the port reproduces, and proposed opt-in
  fixes, are in [VCL_BUGS.md](docs/VCL_BUGS.md).

## Original artifact

- File: `local/vcl` (482,736 bytes; local, not tracked).
- SHA-256: `83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`.
- ELF32, little endian Intel 80386, stripped, Linux interpreter `/lib/ld-linux.so.2`.
- Embedded identification: VCL 1.4beta7.

The tools that set up and run the original are in `tools/` (`setup_oracle.py`,
`run_oracle.py`, `replay_oracle.py`, `run_native.py`); the binary, its runs
under `local/evidence/` and the analysis tools that read its internals
(`local/tools/`) are not tracked. [RUNTIME.md](docs/RUNTIME.md) describes the
emulated oracle and what it needs under `local/`.

## Layout

The Cargo workspace has two crates:

- `crates/vcl`, the compiler library, organised by phase: `isa` (the VU
  instruction table), `parser` (source and immediate expressions), `analysis`
  (data flow, register webs, pruning, copies, induction variables, control
  flow, status flags, memory), `scheduler` (linear scheduling, the table
  wrangler, row timing and clean-up), `loops` (modulo scheduling and loop
  expansion), `allocator` (register assignment), `emit` (assembly formatting)
  and `compiler`, which runs the phases over a program. `vcl::compile` is the
  entry point and `vcl::Options` its settings.
- `crates/oxide-vcl`, the `oxide-vcl` command-line driver: the entry point, the
  original's command-line rules (`cli.rs`, also a small library target so the
  tests share it), preprocessing and file handling. Its `tests/` hold the
  integration tests; the public corpus they compile is `tests/corpus/`.

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
- [Original defects and proposed fixes](docs/VCL_BUGS.md)

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

Not covered:

- The programs in `tests/corpus` keep their own licenses (GPL-2.0, GPL-3.0,
  LGPL-2.1, AFL-2.0, Apache-2.0), recorded in each program's header and in
  [tests/corpus/README.md](tests/corpus/README.md). They are test data; the
  compiler does not include or link them.
- The recorded outputs of SCE's vcl 1.4beta7 in `tests/corpus` and
  `tests/probes` (`stdout.bin`, `stderr.bin`, `result.json`) are reference
  data for the tests, not part of the licensed work. vcl itself is SCE's and
  is not distributed here.

