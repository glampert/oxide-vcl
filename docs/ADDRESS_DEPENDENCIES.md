# Scalar addresses and memory field selectors

In `ilw.w count,0(base)` the W selects a word inside the memory qword. It does not
select a W component of the scalar VI address. The port keeps W on the parsed
operand for rendering but must not copy it into the address dependency:

```asm
xtop base
ilw.w count,0(base)
```

XTOP defines scalar bit 0 of `base`, so a dependency on `base.w` would be an
invented undefined read. The shared expansion path separates the masks:

```rust
let dependency_mask = if matches!(source.class, 28..=33) {
    Mask::X.0 // scalar VI address; independent of memory fields
} else {
    source.mask.0
};
```

The actual code also keeps MR32's vector rotation. Address classes 28-33 cover
ordinary/displaced EA, field-selected ordinary/displaced EA, and increment/decrement
EA. The parsed memory selector is unchanged. Initial lowering and pruning both use
this expansion, so flow masks agree, and strict initial undefined-read validation
stays active.

## Evidence from vcl 1.4beta7

Inferred from vcl 1.4beta7 by static analysis:

- Operand decoding: classes `0x1e`/`0x1f` set the register mask to 1; trailing
  x/y/z/w parsing accumulates a separate field mask.
- All six address classes map to the same pool as ordinary VI operands.
- Classes 30/31 are described as `(vi_reg)[x/y/z/w]` and `OFFSET(vi_reg)[x/y/z/w]`.
- Non-lane-wise transfers (the output-to-input transfer helper used by pruning) use
  the input's own dependency mask; direct lane propagation applies only when both
  operands are class 1 or 9. A memory selector used as a scalar dependency would
  therefore also corrupt pruning.

## Probes and tests

Nine probes ran vcl to guest exit without emulator exceptions or timeouts:

| Probe | vcl | Port |
| --- | --- | --- |
| XTOP then ILW, separately selecting x/y/z/w | Four successes | Four matching normalized schedules |
| Declared VI source, XTOP then ISW selecting x/y/z/w | Four successes | Four matching normalized schedules |
| ILW.w with an undeclared base | Exit 255 | Rejected: `base.x used before set` |

All valid probes enable `.init_vi_all`; without the VI pool allocation fails.

Sources are `local/evidence/inputs/address-*.vcl`; vcl inputs, stdout/stderr,
statuses, hashes and emulator settings are under `local/evidence/oracle/address-*/`,
the manifest is `local/evidence/analysis/address-probes.json`, and comparisons are
in `local/evidence/differential-address/` (all local, not tracked). Runs use
`-t1 -uADDRESS` and the isolated Qiling runner.

`crates/oxide-vcl/tests/oracle_pipeline.rs` compares the eight successful schedules and
keeps the negative control. `crates/oxide-vcl/tests/public_corpus.rs` checks parsed
x/y/z/w masks against scalar expanded reads and compiles the real programs that use
this pattern. Output of those programs still differs from vcl's (the port emits more
issue pairs); byte-for-byte equality is not achieved. The test-only VU model's
numerical/timing limits are in [TESTING.md](TESTING.md).

```sh
python3 local/tools/probe_addresses.py       # summarize retained observations
# Re-execute vcl with existing Unicorn host permission:
local/analysis/runtime/venv/bin/python local/tools/probe_addresses.py --run
cargo test
cargo clippy --all-targets -- -D warnings
python3 tools/compare_corpus.py target/debug/oxide-vcl --out local/evidence/differential
```
