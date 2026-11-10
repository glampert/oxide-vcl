---
name: add-test-case
description: Add a program with the original vcl's result as a regression case, either to the public corpus (public source programs) or to the probe suite (programs written for this project).
---

# Add a test case

Decide where it belongs:

- **Probe suite** (`tests/probes`): a small program written for this project
  that pins one behaviour. No license questions.
- **Public corpus** (`tests/corpus`): a real program from a public repository,
  under a license that allows redistribution.
- **Local corpus** (`local/corpus`): anything derived from copyrighted code.
  Never tracked; see `.claude/rules/tracked-text.md`.

## Probe

1. Write the program; avoid names that collide with hardware registers or
   field letters.
2. Run the original on it at `--time-scale 1` (`run-original` skill) into a
   scratch directory.
3. Copy `input.vcl`, `result.json`, `stdout.bin` and `stderr.bin` into
   `tests/probes/<name>/` and the whole run into `local/evidence/oracle/<name>`.
4. `cargo test --release --test probes`. If the port differs, fix the port
   (`investigate-divergence` skill) or, for a known gap, add the probe to
   `KNOWN_DIFFERENCES` with its reason.
5. If it pins a rule, also add a short inline test near the related tests.

## Public corpus program

Follow `.claude/rules/test-data.md` (Public corpus): preprocess as the source
project does, record the original with `-t1 -uCORPUS` at time scale 1, add the
provenance header, manifest entry, README row, license text and the updated
counts. The port must match before the program goes in.
