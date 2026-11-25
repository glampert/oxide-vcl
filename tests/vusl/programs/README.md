# VUSL programs

The VU1 programs of two public PS2 ports, as their projects write them in VUSL, each against the
hand-written VCL program it replaced (`hand-written`, a case in `tests/corpus`).
`crates/vusl/tests/programs.rs` runs both on that case's scenes.

| Case | Project (commit) | Files |
|---|---|---|
| `particles`, `textured_triangles` | [glampert/quake2-ps2](https://github.com/glampert/quake2-ps2) (415226b) | `src/ps2/renderer/vu1progs/{particles,textured_triangles}.vusl` as `input.vusl`, and `frame.vush` in `include` |
| `world_lm`, `world_strips` | [glampert/doom3-bfg-ps2](https://github.com/glampert/doom3-bfg-ps2) (440b77c) | `src/ps2/renderer/vu1progs/{world_lm,world_strips}.vusl` as `input.vusl`, and `clipper.vush`, `clipper.vusl`, `vu_memory.vush` and `world.vush` in `include` |

The files are copied unchanged, except that `clipper.vusl` and `textured_triangles` name their
clip planes with an `enum class ClipPlane` where the projects pass an `int16` index, and choose
each plane's term in `planeDistance` with a `switch` where the projects chain `if`s; the
generated code is the same. `include` is on the include path. The two doom3 programs call the
clipper library (`include/clipper.vusl`), which the suite places at its address after each.

Licences: the quake2-ps2 files are GPL-2.0 (`tests/corpus/licenses/GPL-2.0.txt`), the
doom3-bfg-ps2 files GPL-3.0-or-later (`tests/corpus/licenses/GPL-3.0.txt`), as each file's
header says. `tests/corpus/README.md` describes both projects.
