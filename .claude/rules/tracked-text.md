# Tracked text

Everything committed (code, comments, docs, test data, commit messages) is
meant for publication. It must not contain:

- **Internals of the original binary**: virtual addresses, disassembler names
  (`FUN_`, `DAT_`), struct offsets or record sizes, mentions of the
  disassembler or decompiler, or the original's internal function names.
  Attribute facts as "observed from vcl 1.4beta7" (a run showed it) or
  "inferred from vcl 1.4beta7" (analysis). The version, its SHA-256 and the
  original's user-visible messages may be quoted. Values the tools need from
  the binary go in untracked files under `local/` (for example
  `local/oracle.json` for `tools/run_oracle.py`).
- **Anything about the local corpus's sources**: it is derived from
  copyrighted programs. Call it "the local corpus" or "disassembled VU
  programs", its sets "set A" and "set B", and their directories
  `local/corpus/SET`. Never name the games, executables, disc serials, the
  projects that disassembled them or their tools. Case IDs that are only
  numbers (`vu_<number>`) are fine.
- **The original's help-screen notice or version banner.** The port's help
  (`crates/oxide-vcl/src/help.txt`) is the project's own: version and build date.
- **The user's email or personal paths.**

Before committing, scan the diff for these. The git history has been
rewritten to remove all of them; keep it that way.

What may be committed from the original: its outputs for public programs
(`tests/corpus`, each with its source's license) and for this project's own
probe programs (`tests/probes`). Never the binary, its traces or anything else
under `local/`.
