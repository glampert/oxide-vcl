---
paths:
  - "tests/**"
  - "crates/oxide-vcl/tests/**"
---

# Test data

Both `tests/corpus` and `tests/probes` use one directory per program:
`input.vcl`, the original's `result.json` (`guest_argv`, `exit_status`,
`input_sha256`), `stdout.bin`, `stderr.bin` and, in the corpus, `stdin.bin`.
`crates/oxide-vcl/tests/support/corpus.rs` compares a case: the same acceptance,
and for an accepted program the same listing ignoring comments and whitespace.
An optional `native-stdout.bin` (corpus only) replaces `stdout.bin` as the
reference where the original's emulated run hit its time-out and a native run
did not.

## Public corpus (`tests/corpus`)

- Programs from public repositories, named `project__path_to_file`.
- Each `input.vcl` starts with a provenance header (program name, repository
  and commit, how it was produced, license) ending with
  `; -- end of corpus header --`. The harness strips it and checks the rest
  against `input_sha256`, so the recorded run still applies.
- Adding a program: preprocess it the way its project does, run the original
  with `-t1 -uCORPUS` at time scale 1 (`run-original` skill), add the header,
  an entry in `manifest.json` (append; do not reorder), a provenance row and
  license note in `tests/corpus/README.md`, the license text in `licenses/`
  if new, and update the expected counts in `public_corpus.rs` and the docs
  that state them (`README.md`, `docs/CORPUS_BASELINE.md`,
  `docs/COMPATIBILITY.md`, `docs/TESTING.md`).
- The test requires every program to match. A program the port gets wrong is
  a bug to fix before adding it.

## Probe suite (`tests/probes`)

- Programs written for this project, no header. Copy a run's `input.vcl`,
  `result.json`, `stdout.bin`, `stderr.bin` into a new directory; `probes.rs`
  picks it up.
- `KNOWN_DIFFERENCES` in `probes.rs` lists the probes allowed to differ, each
  with its reason; a listed probe that starts matching fails the test.
- Keep the run in `local/evidence/oracle` too, where the deeper local tests
  and tools look.

## Local corpus

`local/corpus/manifest.json` lists sets of cases in the same format;
`crates/oxide-vcl/tests/local_corpus.rs` checks them when present, allowing each
set's `known_differences`. Never copy anything from there into tracked files.
