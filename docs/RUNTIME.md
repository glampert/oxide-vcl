# Original-binary runtime oracle

The runtime oracle executes the supplied `vcl` ELF, whose SHA-256 is
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`.
Its `-h` output identifies version **1.4beta7**, compiled at **15:26:57 on
December 17, 2004**, for Linux. This is a 32-bit i386 dynamically linked ELF.

## Reproduce a run

The runner and its setup are tracked in `tools/`; the binary and everything
derived from it stay in the untracked `local/`:

- `local/vcl`: the original ELF (checked against the SHA-256 above);
- `local/oracle.json`: two addresses in that binary, of its alarm handler and
  the time-out flag the handler sets, which `run_oracle.py` needs to emulate
  the optimizer's alarm (`{"alarm_handler": "0x...", "alarm_flag_pointer":
  "0x..."}`); the runner stops with a message when the file is missing;
- `local/analysis/runtime`: the emulated i386 runtime that the setup builds.

```sh
python3 tools/setup_oracle.py
local/analysis/runtime/venv/bin/python tools/run_oracle.py \
  --name help-short -- -h
local/analysis/runtime/venv/bin/python tools/run_oracle.py \
  --name add --input tests/probes/sched-add-symbol/input.vcl \
  -- -t1 -uORACLE @INPUT@
```

`tools/replay_oracle.py` reruns recorded cases from their saved arguments and
inputs (`--oracle tests/probes`, `tests/corpus` or `local/evidence/oracle`),
into a separate output folder. `tools/run_native.py` runs the original on an
x86 Linux host with the same runtime's loader and libraries, through the real
`alarm(2)`, and writes the same result layout.

The setup installs Qiling 1.4.6 and extracts these checksum-verified Ubuntu
20.04 i386 packages: `libc6_2.31-0ubuntu9`, `libstdc++5_3.3.6-30ubuntu2`,
and `libgcc-s1_10-20200411-0ubuntu1`. Their exact package names, HTTPS archive
locations, and SHA-256 values are in `tools/setup_oracle.py`. Each run copies
the clean rootfs into a private temporary directory, copies the supplied ELF
into `/bin/vcl`, verifies its digest, and supplies the selected source under
`/work/`. The environment contains only `PATH=/bin:/usr/bin`.

The setup also accepts `--package-cache DIRECTORY` and `--venv-source DIRECTORY`
for offline reuse. The observed runtime versions are Qiling 1.4.6 and Unicorn
2.1.4. Keystone's Python module is replaced by a constants-only stub because
its native assembler does not load in this Python environment; the oracle never
invokes an assembler.

The emulation setup and optimizer-alarm workaround follow the runtime recipe of
the openvcl project. Only that recipe is used: no openvcl implementation or
instruction table defines VCL behavior here, and all probe programs were
written independently and executed against the supplied binary.

Under the execution tool's sandbox on an arm64 macOS host, Unicorn traps with
SIGILL in its native `init_cache_info` before guest instructions run; run the
oracle outside the sandbox. This is a host-emulator initialization constraint,
not a VCL crash.

## Evidence records and replay

Each `local/evidence/oracle/CASE/` (local, not tracked) contains:

- `result.json`: binary/input/output SHA-256 digests, exact guest argv,
  emulator versions, raw exit argument, parent-visible exit status, CPU and
  wall time, timeout configuration, and alarm counters. New runs explicitly
  intercept exit syscalls to distinguish guest termination from emulator timeout.
- `input.vcl`, when there is a selected source, and `stdin.bin`.
- `stdout.bin` and `stderr.bin`, preserving exact bytes, including spacing.
- `emulator-error.txt`, empty for completed runs without emulator exceptions.
- `generated/`, for files created beneath guest `/work`, including `-o` and
  `-j` outputs.

The i386 `exit(-1)` syscall argument appears as unsigned `4294967295`; its
parent-visible Unix exit status is **255**. Both values are recorded. An
emulator exception is distinct from a guest compiler failure.

Replay saved invocations to a separate directory using:

```sh
local/analysis/runtime/venv/bin/python tools/replay_oracle.py
local/analysis/runtime/venv/bin/python tools/replay_oracle.py \
  sched-empty alloc-vf-all hw-mulq-declared-q
```

The exploratory case generator is `local/tools/probe_oracle.py`; the cases are
replayed from the saved argv/input records. `tools/compare_oracle.py` replays those records against a native
candidate executable and compares stdout, stderr, exit status, and generated
files independently. It relocates `/work` into a temporary directory and
preserves the original anonymous-label seed with `-u` when needed. It restores
guest paths in diagnostics, but does not remove whitespace, comments, timing
annotations, instructions, or other behavioral differences. Differences are
saved under ignored `local/analysis/differential`.

```sh
python3 tools/compare_oracle.py target/debug/oxide-vcl \
  sched-empty scheduled-comment hw-mulq-declared-q
```

Byte equality and schedule observations are separate. The additional
`tools/compare_outputs.py ORIGINAL_ASSEMBLY CANDIDATE_ASSEMBLY` reports raw byte
equality, normalized pair equality, labels and exported names, end-bit pair
positions, NOP counts, issued-pair counts, and annotated stalls independently.
It keeps `[E]` and branch targets. Its `pairs_plus_annotated_stalls` metric is
an annotation-based count, not a complete VU cycle simulation. Different
normalized schedules are reported as different; that alone is not proof of a
semantic failure because a scheduler may validly reorder independent work.
Python tests exercise normalization, label preservation, E-bit loss, and
separate NOP/stall accounting:

```sh
python3 -m unittest discover -s tools -p test_compare_outputs.py
```

## Established behavior

The simplest successful scheduled program has explicit entry and exit blocks:

```asm
--enter
in_vf a(vf01)
--endenter
add b, a, a
--exit
out_vf b(vf02)
--endexit
```

`sched-add-symbol` emits an upper `add[E] VF02,VF01,VF01` paired with lower
`NOP`, followed by `NOP NOP`. Empty entry/exit blocks emit two pairs, with
`NOP[E]` in the first upper slot. A single `lq` similarly carries `[E]` in its
pair's upper `NOP`, followed by one empty pair. There is no four-cycle drain
after an externally visible VF result in these cases.

The default mode schedules single instructions. `.vsm` and `.raw` select raw
mode, requiring two instructions on one source line, such as `nop nop`; `|`
is not a separator. Raw code still needs enclosing entry/exit context. Source
mnemonics are case-insensitive; `;` and `//` introduce comments, while `#`
does not. `in_vf a(vf01)` maps a symbolic name to a physical register. A comma
between the symbolic and physical names is rejected.

Unmapped intermediate values require a declared allocation pool. For example,
`.init_vf vf03` permits a `mul c,a,a; add b,c,a` chain to allocate `c` to VF03;
`.init_vf_all` permits choosing VF02 and reusing that register for `b`. The
unconsumed result of `mul unused,a,a` is pruned. Component validity is tracked:
`add.xyz` followed by a full `out_vf` is rejected because W was never set;
`out_vf.xyz` succeeds.

Q, I, ACC, CLIP, MAC, and ALU status dependencies are not generally assumed
initialized. `hw-*-undeclared` cases expose use-before-set diagnostics for
those actual reads. Valid declarations include `in_hw_q Q`, `in_hw_i I`,
`in_hw_acc ACC`, and `in_hw_clip CLIP`. `mula ACC,a,a` establishes ACC for a
following `madd`; those instructions can issue in adjacent pairs. The original
rejects `in_hw_mac MAC` with an internal error and exit status 1. An ALU
operation establishes flag dependencies used by `fsand` and `fmand`; those
cases preserve an otherwise unused ALU result and emit an `abs VF00,VF02`
before the flag read. See `hw-fsand-after-alu` and `hw-fmand-after-alu`.

`--exitm` replaces `--exit`, accepts the output declarations, and still needs
`--endexit`. With no macro argument it emits no end bit or trailing empty pair.
Putting `--exitm` inside an existing exit block is rejected.

## CLI findings

Inferred from vcl 1.4beta7, the original's argument parser works as below.
`crates/oxide-vcl/src/cli.rs` implements it and tests its boundary cases.
The port's help screen, `crates/oxide-vcl/src/help.txt`, is its own: it names
the project's version and build date and describes what each flag does in
the port. Flags the port accepts without effect are listed separately.

- `-h`, `-H`, and even `-help` print help and exit immediately. `--help`,
  `--version`, and `-v` are rejected, despite `-v` appearing in the usage line.
- The second byte selects the switch. Most switches ignore remaining bytes;
  `-quiet` enables `-q`. Attached values are required for `-o`, `-j`, `-u`,
  `-m`, and `-t`; a bare `-o` gives an empty output filename.
- Parsing stops at the first non-option; more than one remaining argument is
  an error. `input.vcl -q` therefore fails. `--` is not an option terminator.
- `-t` uses signed decimal `strtol` and clamps to at least 1. Empty, invalid,
  negative, and zero values become 1; `-t3junk` becomes 3; `-t0x20` becomes 1.
- `-g` and `-g+` select preprocessing; `-gbad` fails, while `-g+bad` is accepted.
  `-W`/`-w` skip the preprocessing stages. `-I...` is ignored by VCL and
  reserved for forwarding to preprocessors.
- `-q` suppresses informational messages according to parse order. Thus
  `-q -h` produces no output. Warnings from deprecated `-P`/`-Z` remain visible.
- `-P` and `-Z` print deprecation warnings and do not disable the current
  pruning or immediate-fixup passes. `-a`/`-A` report that the option is always on.

## Limits and surprising original output

These probes establish individual behaviors, not whole-program equivalence.
The runtime contains the dynamic loader and libraries, not a Linux process
environment with working `cpp` and `ee-gasp`; preprocessing integration is not
validated. Loop optimization and large realistic programs require additional
coverage.

VCL uses `alarm()` to bound optimizer search. The emulation recipe substitutes
a watcher that reproduces the original handler's one-byte flag store when the
handler is the original's alarm handler. A guest second is scaled to a number
of process-CPU seconds (25 by default; the corpus runs use `--time-scale 1`).
This is an approximation of native timing. Optimizer path choices after a
timeout are not established, and a slow emulated run can stop a strategy that
finishes natively. The emulator wall limit (`--timeout`, 60 s by default;
long runs need more) is separate from VCL's own limit.

The size-reduction pass can change more than instruction spacing. In the
five-cycle two-instruction `mul`/`add` chain, `-C` retains three intervening
empty pairs and places `[E]` on the penultimate pair. The default pass removes
the empty pairs and their end bit, emits only `mul` and `add`, and annotates
`STALL_LATENCY ?3`. This exact surprising output is retained in `alloc-vf-all`
versus `exit-chain-no-reduce`.

## Probe map

The table names exact case directories beneath `local/evidence/oracle/`. Source,
argv, stdout, stderr, and exit status remain available together in every case.

| Cases | Controlled question | Observed result |
| --- | --- | --- |
| `help-short`, `cli-H`, `cli-help-tail`, `help-long`, `version-short` | Accepted help/version spellings | H/h/-help accepted; long options and -v rejected |
| `cli-stop-on-file`, `cli-two-inputs`, `cli-double-dash` | Argument boundaries | Stop at first filename; exactly one allowed; -- rejected |
| `cli-output-empty`, `cli-output-file`, `cli-header-file`, `cli-same-file` | Attached paths and file output | Empty path fails at emit; output file has assembly without stdout wrapper; identical input/output rejected |
| `cli-time-zero`, `cli-time-negative`, `cli-time-junk`, `cli-time-trailing` | Timeout coercion | Invalid/nonpositive clamps; decimal prefix accepted |
| `cli-deprecated-P`, `cli-deprecated-Z`, `cli-legacy-a` | Historical options | Current warnings/no-op behavior retained |
| `sched-empty`, `sched-add-symbol`, `scheduled-iaddiu` | Minimal scheduled emission | Exact pair layout, [E] position, final NOP, physical mapping |
| `raw-nop`, `raw-pair-pipe`, `raw-in-enter`, `raw-math` | Raw syntax | Two space-separated instructions; explicit entry/exit |
| `scheduled-comment`, `scheduled-comment-slashes`, `scheduled-comment-hash` | Comment syntax | Semicolon and // accepted; # rejected |
| `scheduled-dead`, `alloc-vf-all`, `alloc-vf-single`, `alloc-vf-list` | Pruning and register pools | Dead result pruned; pool enables intermediate register |
| `scheduled-mask`, `mask-output` | Component validity | Full output rejects unset W; xyz output succeeds |
| `hw-mulq-undeclared`, `hw-mulq-declared-q` | Q initialization | Q read requires declaration or earlier writer |
| `hw-addi-undeclared`, `hw-addi-declared-i` | I initialization | I read requires declaration or earlier writer |
| `hw-madd-undeclared`, `hw-madd-declared-acc`, `hw-mula-madd` | ACC dependency | Explicit ACC input or preceding mula required; adjacent mula/madd allowed |
| `hw-fcand-undeclared`, `hw-fcand-declared-clip` | CLIP initialization | Flag read requires CLIP input or earlier writer |
| `hw-fsand-undeclared`, `hw-fsand-after-alu`, `hw-fmand-after-alu` | ALU status/MAC | Real flag dependencies; live ALU flag writes and inserted abs |
| `exit-load`, `exit-load-no-reduce`, `exit-chain-no-reduce` | Exit padding and reduction | No full result drain; reduction can remove NOP containing E |
| `exitm-replaces-exit`, `scheduled-exitm-exit` | Exit macro context | Replaces exit block marker, not an instruction inside it |
| `emit-div`, `emit-cliplw` | Special operand spelling | VF01x scalar suffixes; clipw.xyz alias |
| `emit-imove`, `emit-vmove`, `emit-uvmove`, `emit-lvmove` | Abstract move emission | Original rejects source form because baseMuid is unset |
