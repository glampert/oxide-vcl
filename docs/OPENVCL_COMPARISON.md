# Code generation compared with openvcl

How the code oxide-vcl generates compares with [openvcl](https://github.com/ps2dev/openvcl)'s, on eleven programs
from the public corpus (`tests/corpus`): both quake2-ps2 and all three
doom3-bfg-ps2 renderer programs, both ps2gl programs, two tyra pipelines and
two of openvcl's own programs. The versions are oxide-vcl 0.1.0 and openvcl
0.4.0, as installed with the ps2dev toolchain.

`tools/compare_openvcl.py` reproduces everything here (see
[Reproducing](#reproducing)).

## Method

Each program is compiled by both compilers with the arguments recorded in its
corpus case (`-t1 -uCORPUS` for all eleven), from the same input without the
corpus header. oxide-vcl's listing is the one the corpus tests check, so its
numbers here are the ones the test suite pins.

Both listings are then scored by the same model: openvcl's static cost
analyzer (`openvcl --cost`). It reads a scheduled `.vsm` and does not run it:

- static cycles: every listing row (an upper and lower instruction pair) is
  one cycle;
- estimated cycles: static cycles plus the stalls it models, such as Q and P
  waits (`waitq`, `waitp`) and FDIV and EFU issue stalls;
- each block (from a label to the next) is counted once, unless weighted with
  `--cost-loop LABEL=N`.

The numbers are a static estimate, not a hardware measurement, and the model's
accuracy has not been checked here. They compare the two compilers' output
under one model.

## Whole programs

Estimated cycles, every block counted once. "Paired" is the number of
cycles that issue both an upper and a lower instruction.

| program | oxide-vcl | openvcl | openvcl / oxide-vcl | paired, oxide-vcl / openvcl |
|---|---:|---:|---:|---:|
| quake2-ps2 particles | 64 | 91 | 1.42x | 12 / 3 |
| quake2-ps2 textured_triangles | 1070 | 1877 | 1.75x | 108 / 51 |
| doom3-bfg-ps2 passthrough | 6 | 6 | 1.00x | 0 / 0 |
| doom3-bfg-ps2 world_lm | 972 | 1294 | 1.33x | 119 / 47 |
| doom3-bfg-ps2 world_strips | 706 | 963 | 1.36x | 91 / 40 |
| ps2gl tricked_out_billboard_renderer | 135 | 109 | 0.81x (see below) | 48 / 5 |
| ps2gl vu1_scei | 204 | 317 | 1.55x | 72 / 22 |
| tyra dynpip_td_vu1 | 155 | 272 (incomplete) | 1.75x | 71 / 39 |
| tyra stapip_cull_tc_vu1 | 90 | 127 (incomplete) | 1.41x | 31 / 18 |
| openvcl test_fixtures_general | 734 | 564 | 0.77x (see below) | 172 / 39 |
| openvcl examples_triangle_strip | 32 | 50 | 1.56x | 2 / 1 |

Where the loops are not software pipelined (quake2-ps2, doom3-bfg-ps2, tyra),
these totals are a fair comparison block by block. For example, the hot
blocks of the two larger quake2-ps2 and doom3-bfg-ps2 programs:

| block | oxide-vcl | openvcl |
|---|---:|---:|
| quake2-ps2 particles `lParticleLoop` | 32 | 59 |
| quake2-ps2 textured_triangles `lClipTriangle` | 111 | 319 |
| quake2-ps2 textured_triangles `lWarpWhole` | 109 | 316 |
| quake2-ps2 textured_triangles `lKeyframes` | 61 | 123 |
| clip-plane loops (`lNearLoop`, `lXlLoop`, ...) | 28 to 30 | 45 |
| doom3-bfg-ps2 world_lm `lTransform` | 36 | 65 |
| doom3-bfg-ps2 world_lm `lFanLoop` | 35 | 68 |
| doom3-bfg-ps2 world_lm `lListClip` | 38 | 66 |
| doom3-bfg-ps2 world_lm `lLightmapRoom` | 80 | 93 |

openvcl is ahead by one cycle in two small blocks (`lStripQword`, 21 against
20, and `lListEntry`, 22 against 21) and behind or level everywhere else.

## Software-pipelined loops

In the two programs where openvcl's total is lower, oxide-vcl software
pipelines the loops (`--LoopCS`). An expanded loop is a start-up section, a
steady-state main loop (`EXPL_<unit>_<label>__MAIN_LOOP`) and several wind-down
copies (`__EPI0`, `__EPI1`, ...). Counting every block once charges all those
copies, although only the main loop repeats per vertex. openvcl pipelines the
same loops (`__PROLOG`, `__DRAIN`) with a longer body. Cycles per pass of the
loop that repeats:

| program | loop | oxide-vcl | openvcl |
|---|---|---:|---:|
| ps2gl tricked_out_billboard_renderer | `xform_loop_lid` | 24 | 65 |
| ps2gl vu1_scei | `xform_loop_lid` | 19 | 49 |
| openvcl test_fixtures_general | `xform_loop_lid` | 22 | 43 |
| openvcl test_fixtures_general | `dir_light_vert_loop_lid` | 18 | 56 |
| openvcl test_fixtures_general | `pt_light_vert_loop_lid` | 39 | 131 |
| openvcl test_fixtures_general | `final_loop_lid` | 5 | 17 |

For any realistic vertex count, these loops dominate: oxide-vcl's are 1.95x to
3.4x shorter per pass. How many source iterations one pass of oxide-vcl's
main loop covers was not checked, so these are per-pass figures.

## What differs

- **Pairing.** oxide-vcl issues loads, stores and integer updates in the lower
  pipe beside the upper-pipe arithmetic. openvcl mostly runs one pipe at a
  time: it pairs 1.7 to 9.6 times fewer cycles in every program except
  passthrough (172 against 39 in test_fixtures_general).
- **Latency filling.** openvcl waits out latencies with rows of `nop`s, where
  oxide-vcl schedules independent work into them. Cycles with only `nop`s:
  57 against 821 in textured_triangles, 78 against 335 in world_lm, 6 against
  188 in test_fixtures_general. `lParticleLoop` is 28 cycles in oxide-vcl and
  57 in openvcl, which runs the arithmetic first, then the ten stores and
  counter updates one per row.
- **Delay slots.** oxide-vcl fills the branch delay slot (the loop's last
  store in `lParticleLoop`); openvcl leaves a `nop` before the branch and in
  the slot.
- **Waits.** openvcl sometimes shows fewer `waitq` stall cycles, because the
  `nop` rows before a `waitq` already cover the divide's latency.

## openvcl's tyra output is incomplete

Both tyra programs write `clipw.xyz vertex1, vertex1`, with no field on the
second operand. oxide-vcl reads it as the `w` field of that register
(`clipw.xyz VF17xyz, VF17w`). openvcl prints
`Invalid argument (clipw.xyz vertex1, >>> vertex1 <<<)` for each of the three,
exits with status 1, and still writes a listing, without the three `clipw`
instructions. Its costs for these two programs are for that incomplete code.

## Reproducing

```sh
cargo build --release
python3 tools/compare_openvcl.py --openvcl "$PS2DEV" --out /tmp/openvcl-comparison
```

`--openvcl` takes the openvcl executable or an install prefix that holds
`bin/openvcl`. Without program names the tool runs the eleven above; pass
corpus case names (directories in `tests/corpus`) to compare others. It
writes each program's listings, diffs and cost reports to
`OUT/<program>/`, and the tables to `OUT/summary.md`.
