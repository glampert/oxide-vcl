# Probe suite

Small VCL programs written for this project, each pinning down one behaviour
of SCE vcl 1.4beta7, with the original's recorded run.
`crates/oxide-vcl/tests/probes.rs` compiles every program with the port and
requires the original's result: the same acceptance, and for an accepted
program the same listing, with comments and whitespace ignored. It is the
public corpus comparison applied to targeted inputs, and it runs everywhere,
CI included.

The programs are the inputs of this project's original-binary probes; the
full records (traces, emulator logs, analysis) stay in the untracked
`local/evidence/oracle`, which the deeper local-only tests read. Program
names give the area they probe, for example `instruction-*` (one per
instruction form), `checker-*` (latency and hazard checks), `memory-*`,
`loop-*`, `pipeline-*`, `raw-*`, `visibility-*`, `name-*` (reserved register
names) and `rule-*` (rules found on the local corpus of disassembled
programs, rewritten as small programs).

## Layout

Each directory is one program, in the corpus format (`tests/corpus/README.md`):

- `input.vcl`: the program;
- `result.json`: the original's run: its arguments (`guest_argv`), exit
  status and the SHA-256 of the input it compiled;
- `stdout.bin`, `stderr.bin`: the original's output.

The original ran under the oracle emulator (`docs/RUNTIME.md`). vcl itself
is SCE's and is not distributed here.

## Known differences

`KNOWN_DIFFERENCES` in the test lists the probes whose result the port does
not reproduce, each with its reason: command-line behaviour that this
in-process comparison does not model, and a few port differences (raw boundary
reduction, `-s` register names, one copy next to raw blocks). A listed probe
that starts to match fails the test, so the list only shrinks.

## Adding a probe

Write the program, run the original on it
(`tools/run_oracle.py --time-scale 1 -- -t1 -uNAME @INPUT@`), and copy
the run's `input.vcl`, `result.json`, `stdout.bin` and `stderr.bin` into a new
directory here. The test picks it up.
