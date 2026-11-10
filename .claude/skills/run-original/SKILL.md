---
name: run-original
description: Run the original SCE vcl 1.4beta7 under the emulated oracle to see what it does with a program or options. Use before changing compiler behaviour, when a doc claim needs checking, or to record a new probe.
---

# Run the original vcl

The original is a 32-bit Linux ELF; it runs here under Qiling. Everything
it needs is under the untracked `local/`:

- `local/vcl` (checked against its SHA-256 before every run);
- `local/oracle.json`, the binary's alarm-hook addresses;
- `local/analysis/runtime`, built by `python3 tools/setup_oracle.py` (offline:
  `--package-cache local/analysis/runtime/debs --venv-source ...`).

If any is missing, say so and stop; do not try to recreate the binary.

## A run

From the repository root, with the runtime's Python:

```sh
local/analysis/runtime/venv/bin/python tools/run_oracle.py \
  --name my-case --input /path/to/my-case.vcl \
  --out local/evidence/oracle --time-scale 1 --timeout 3000 \
  -- -t1 -uDEMO @INPUT@
```

- `@INPUT@` becomes `/work/<input basename>`. Without `-u`, generated labels
  contain that path, so compare with the same file name.
- Results land in `<out>/<name>/`: `stdout.bin`, `stderr.bin`, `result.json`
  (`exit_status`, `emulator_timeout`, hashes, `guest_argv`).
- `--time-scale 1` reproduces the recorded corpus runs. The default (25) and
  large values (1000 disables optimizer time-outs) can change the output of
  programs whose loop search times out.
- `--timeout` is wall seconds; the default 60 is too short for real programs.
- Several runs can go in parallel (`&` and `wait`); long ones belong in the
  background.
- Do not overwrite existing evidence directories; write new runs to a scratch
  `--out` and copy them in when they are worth keeping.

`tools/replay_oracle.py --oracle tests/probes --out <scratch> NAME` reruns a
recorded case. `tools/run_native.py` runs the original on an x86 Linux host
with real timing.

## Reading the result

- Compare the port on the same input and flags:
  `target/release/oxide-vcl -t1 -uDEMO my-case.vcl`. Normalize away comments and
  whitespace when comparing listings (`tools/corpus_status.py` does this).
- The original's stderr names its strategies and time-outs (`lpgen[...]`,
  `WARN: time out..`, `Opt::Loop name period/bound`) and its errors (`USER:`,
  `ERROR:`). Its messages are worth matching verbatim in the port.
- A status of 255 with `USER:` lines is a rejection; check that the program is
  valid before reading meaning into it (hardware-name collisions and field
  letters in names are common mistakes).

## Tracing

`local/tools/trace_original.py linear ...` records the original's linear and
loop wrangler (candidate ranking, insertions, allocation) to `trace.json`.
It keeps 5000 events unless `TRACE_LIMIT` is raised; a capped trace silently
drops later tables. `TRACE_SKIP=rank` makes traces smaller (but
`compare_wrangle.py` needs the `insert` events). `local/tools/compare_wrangle.py
SOURCE` prints the original's and the port's insertions per strategy side by
side. These tools read the binary's internals and stay untracked.
