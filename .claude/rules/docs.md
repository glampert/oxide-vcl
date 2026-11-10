---
paths:
  - "docs/**"
  - "README.md"
---

# Documentation

- Describe what vcl 1.4beta7 does and what the port does, in the present
  tense. No progress reports, dated status snapshots, fix histories, finished
  checklists or speculation. A real current limitation goes in
  `docs/COMPATIBILITY.md`.
- Attribute the original's behaviour as "observed from vcl 1.4beta7" or
  "inferred from vcl 1.4beta7", and name the probe or test that shows it.
  Paths under `local/` are marked "(local, not tracked)".
- `docs/VCL_REFERENCE.md` is for users writing VU programs. Every complete
  program in it must compile with `oxide-vcl`; recheck after language changes.
- Keep relative links and `#anchor` links working when renaming headings.
- Plain, concise sentences; no marketing.
