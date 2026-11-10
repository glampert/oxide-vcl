---
paths:
  - "crates/**"
---

# Rust code

- Edition 2024, no external crate dependencies (the tests carry their own
  JSON reader and SHA-256). `unsafe` is forbidden; release builds keep
  overflow checks.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must be clean.
- Comments say what the original does and why the code matches it, attributed
  as in `tracked-text.md`, often with the probe that shows it
  (`(memory-tag-shared)`). Comment on behaviour, not on history.
- Paths to repository data are built from `env!("CARGO_MANIFEST_DIR")` plus
  `/../..` (both crates sit two levels down).

## Tests

- A test that needs `local/evidence` carries
  `#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]` and reads
  files at run time (never `include_str!`), through `tests/support/evidence.rs`
  (or `crate::test_evidence` in unit tests). `build.rs` sets the cfg.
- Every behaviour should also have a test that runs without local data: an
  inline program in the test, or a probe in `tests/probes`.
- Inline VCL sources put one source line per physical line with string
  continuations:

  ```rust
  let source = ".init_vf_all\n\
                --enter\n\
                in_vf a(vf01)\n\
                --endenter\n";
  ```

  A continuation drops the next line's leading whitespace, so keep a line
  that must start with spaces or a tab on the previous line.
- Integration tests live in `crates/oxide-vcl/tests`. They compile with
  `vcl::compile` and take options from `vcl_cli::parse(flags).compile`.
  `oracle_pipeline.rs` has `oracle(case, filename, flags)` for comparing pairs
  with a retained run and `pairs()` for normalized pairs.
- In test programs, avoid names that collide with hardware registers (`p`,
  `q`, `r`, `i`, `acc`, `clip`, `mac`, `ext`, `stat_*`), single letters `x`,
  `y`, `z`, `w` (fields), names ending in a field letter, and tags spelled
  like instructions (`b`).
