# Oxide VCL (oxide-vcl)

A Rust reimplementation of SCE's VCL 1.4beta7, the PlayStation 2 vector unit
compiler. The goal is the same output as the original for the same input and
options, ignoring whitespace and comments. **The original's observed
behaviour is the authority**, not the PS2 manuals and not openvcl, whose
implementation is never used.

Start with `README.md` (status, layout, docs index) and
`docs/VCL_REFERENCE.md` (the language and command line, for users). Rules in
`.claude/rules/` and skills in `.claude/skills/` hold the details; personal,
untracked notes are in `CLAUDE.local.md` when present.

## Layout

- `crates/vcl`: the compiler library, by phase: `isa`, `parser`, `analysis`,
  `scheduler`, `loops`, `allocator`, `emit`, `compiler` (one file per phase),
  plus `diagnostics` (modes, typed messages, the output `Reporter`). Entry
  point `vcl::compile`, settings `vcl::Options`.
- `crates/oxide-vcl`: the `oxide-vcl` driver (`main.rs`, plus `cli.rs` as the
  `vcl_cli` library target so tests share the argument parser) and all
  integration tests (`tests/`).
- `tests/corpus`: public VU programs with the original's recorded runs.
- `tests/probes`: small programs written for this project, each pinning one
  behaviour, with the original's recorded runs.
- `tools/`: Python tools: corpus comparison, listing conversion, and the
  oracle runner and its setup (`setup_oracle.py`, `run_oracle.py`,
  `replay_oracle.py`, `run_native.py`), `compare_openvcl.py`, which
  scores oxide-vcl against openvcl (`docs/OPENVCL_COMPARISON.md`), and
  `survey_output.py`, which counts stalls and empty slots per table and
  compares two modes' listings (`docs/OPTIMIZATION_PLAN.md`).
- `docs/`: one chapter per mechanism; `LANGUAGE.md` is the implementer's
  view of the source language.
- `local/` (gitignored, **no git backup**): the original binary, its runs and
  traces (`local/evidence`), the emulated runtime (`local/analysis`), tools that
  read the binary's internals (`local/tools`), and the local corpus.

## Build and test

```sh
cargo build --release
cargo test --release                  # a few minutes; debug is much slower
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
python3 tools/corpus_status.py --candidate target/release/oxide-vcl
```

`corpus_status.py` defaults to the debug binary, which goes stale: pass the
release binary. With `local/` present, `cargo test` also runs the
local-evidence tests and the local corpus (`local/corpus/manifest.json`).
Without it (CI, a fresh clone) those are skipped and the public corpus and
probe suite still run. CI (`.github/workflows/ci.yml`) runs fmt, clippy and
the tests in debug and release.

A change is done when fmt, clippy and the full release suite pass and the
public corpus is still all-matching; check the local corpus sets too when
scheduling or allocation changed.

## Working rules

- Commits go to `main`. The remote (`origin`, GitHub `glampert/oxide-vcl`)
  is public: **push only when the user asks**, and scan what is pushed
  against `.claude/rules/tracked-text.md` first. End commit messages with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
  (or the attribution the session gives).
- Before changing behaviour, establish what the original does: run it on a
  small program (`.claude/skills/run-original`). Do not guess from the
  manuals or from other implementations.
- Match the original's generated code even where it is odd. Its messages
  are not reproduced: every diagnostic uses the port's own wording and
  layout, in both modes. Where the port deliberately differs, say so in the
  docs.
- Two modes (`vcl::Mode`, `--mode=`): `vcl` (default) is the original's
  code generation and exit statuses, and is what the tests assert; `oxide`
  holds fixes and optimizations. Anything that breaks compatibility goes
  behind `options.mode == Mode::Oxide`, leaving the vcl path unchanged.
  Optimizations (`docs/OPTIMIZATION_PLAN.md`) each get a `vcl::Optimization`
  name for `--opt=`, run where `options.optimizes(...)` says, and must keep
  `tests/listing_equivalence.rs` passing.
- No `println!`/`eprintln!` outside `crates/vcl/src/diagnostics.rs`: messages
  are `vcl::diagnostics` enum variants (wording, subject and suggested fix in
  `diagnostics/oxide.rs`), printed through `Reporter`; developer traces use
  `vcl::trace!` or `vcl::trace_if!(switch, ...)`, with switches read once
  from `VCL_*` environment variables into `vcl::debug::Switches`.
- Every behaviour fix gets a test that runs without local data (an inline
  program, or a probe in `tests/probes`) and, where an original run exists, a
  comparison with it.
- Tracked text has strict content rules: see `.claude/rules/tracked-text.md`.
  Never commit anything under `local/`.

## Things that bite

- The Bash tool is zsh: unquoted `$var` does not word-split, and `===` at the
  start of a command is an expansion error (`.claude/rules/shell.md`).
- `local/` has no git copy. Never `rm -rf` a path built from a variable.
- The original runs under an emulator: give big programs `--timeout 3000`,
  run long jobs in the background, and use `--time-scale 1` to reproduce the
  recorded corpus runs.
- Symbolic names in VCL collide with hardware names (`p`, `q`, `r`, `acc`, ...)
  and single letters `x`, `y`, `z`, `w` read as fields; avoid them in test
  programs.
