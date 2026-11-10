---
name: investigate-divergence
description: Find out why oxide-vcl produces different output from the original vcl for a program, and fix it. Use when a corpus or probe comparison fails or a new program differs.
---

# Investigate a divergence

1. **Confirm it.** Rebuild release, then compare on the original's exact
   input, flags and file name (`tests/probes/<case>/result.json` has
   `guest_argv`). Many apparent differences are harness artefacts: a
   relative path in a stream label, `-o`/`-j` output going to files, or `-s`
   names.
2. **Shrink it.** Cut the program down while it still differs, and write
   variants that change one thing. Run each on both compilers (`run-original`
   skill). Variants that agree tell you as much as those that differ.
3. **Find the first difference.** For schedules, `local/tools/compare_wrangle.py`
   shows the first insertion where the two disagree, and the original's
   `rank` events give each candidate's height (H), depth (D), fan-out (F),
   ASAP and latest values. The port prints its own with
   `VCL_WRANGLE_CANDIDATES=1`; `VCL_WRANGLE_TRACE=1` and
   `VCL_WRANGLE_PROBLEM=1` show its table (see `docs/LPGEN_PORT.md`, Debugging
   switches, for the rest).
4. **Look for a missing fact, not a missing tweak.** Past causes: an implicit
   operand the port dropped (XGKICK's EXT dependency), a resource the original
   models as a register (memory tags), a rule applied at the wrong stage.
   Read the instruction's descriptor (`crates/vcl/src/isa/instruction_data.rs`)
   for implicit operands.
5. **Check every corpus before keeping a fix.** A plausible rule that is wrong
   usually breaks many programs: run the public corpus, both local corpus sets
   (`tools/corpus_status.py --oracle local/corpus/SET`), the probe suite and the
   full test suite. Revert experiments that do not hold up.
6. **Record it.** Keep the runs that pin the behaviour (probe suite and
   `local/evidence/oracle`), add a test that needs no local data, and describe
   the rule in the relevant doc.
