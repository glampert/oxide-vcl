# Public corpus comparison

The 52 programs in `tests/corpus/` (see its README) are compiled by the port
and compared with the output of the original vcl 1.4beta7. 47 are accepted
and 5 are rejected by the original. The port matches the original on all 52:
the exit status and the listing (comments and whitespace ignored) for the
accepted programs, and rejection for the others.
`crates/oxide-vcl/tests/public_corpus.rs` requires this, and
`tools/corpus_status.py` prints the comparison per program.

The five rejected programs are repros that the original refuses with implicit
`CLIPa` used-before-set diagnostics: `openvcl__test_repro_quad_adc_bug`,
`openvcl__test_repro_quad_adc_bug_fjtrujy`, `openvcl__repros_clip_join`,
`openvcl__repros_clip_loop_entry` and `openvcl__repros_clipw_implicit_w`.

A match on this corpus says nothing about programs outside it. Emitting the
same pairs does not establish cycle accuracy, and the original's optimizer
time budget makes some choices depend on the environment (see below).

## How the original was run

- Both compilers receive `-t1 -uCORPUS` and the same input basename. The
  original runs under the oracle emulator ([RUNTIME.md](RUNTIME.md)) in a
  fresh copy of the root filesystem, and its binary hash is checked on every
  run.
- The emulator's optimizer-alarm scale is 1 for every program. The minimum
  optimizer timeout (`-t1`, one second) is used, not the default four.
- Runs that exceed the 120-second wall bound are repeated with a 600-second
  bound. Six programs needed this. A run with no guest exit, an emulator
  exception or a timeout is a blocker, never a compiler rejection, and partial
  output is not accepted as success.
- Emulated CPU time is not native time, so the loop search's watchdog stops
  some strategies that finish natively. For the two programs where this
  changed the result, the corpus holds the original's output on native x86
  hardware (`native-stdout.bin`) as the reference.
- `openvcl__examples_triangle-gasp_vu1Triangle` declares `-g --gasp $MASP`.
  It is expanded once with masp and both compilers receive the same expanded
  bytes, so this compares the compilers after preprocessing only. The raw
  source is kept as `source.vcl`.
- The corpus manifest hash is the SHA-256 of the source bytes, a NUL byte and
  the joined manifest flags; it is not the file hash.

## Comparison levels

The goal is identical output. Comparisons keep these apart:

- byte equality of stdout and stderr, and the exit status;
- normalized instruction pairs (masks, registers, E bits, directives, labels,
  timing annotations);
- for differing schedules, whether the observable effects agree
  ([TESTING.md](TESTING.md)).

Schedule inequality alone establishes neither wrong semantics nor
equivalence.

## Reproduction

The local tools and evidence are not tracked.

```sh
# Capture the original's runs (alarm scale 1) and pick the completed attempt
# for each program, preferring the first completed one in the given order.
local/analysis/runtime/venv/bin/python local/tools/validate_external_corpus.py \
  --capture --jobs 2 --time-scale 1 --out local/evidence/corpus/alarm1
python3 local/tools/select_corpus_attempts.py \
  local/evidence/corpus/alarm1/oracle local/evidence/corpus/alarm1/retries-600 \
  --allow-time-scale 1

# Compare the port with the saved outputs.
python3 tools/compare_corpus.py target/debug/oxide-vcl --oracle tests/corpus \
  --out local/evidence/corpus/comparison
python3 tools/summarize_corpus.py
python3 -m unittest discover -s tools -p 'test_*.py'
```

`select_corpus_attempts.py` checks the original binary hash, the compiler
input hash, the flags and an explicitly allowed alarm scale for every attempt,
and keeps an explicit blocked observation when none completed. A longer
attempt is made with `tools/run_oracle.py --timeout 600 --time-scale 1`
and the same `-- -t1 -uCORPUS @INPUT@` guest arguments.
