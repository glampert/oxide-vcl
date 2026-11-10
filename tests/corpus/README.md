# Public corpus

These are 52 VU1 programs from public repositories, each with SCE vcl
1.4beta7's output for it. `crates/oxide-vcl/tests/public_corpus.rs` compiles every program with
the port and requires the original's exit status and listing, with comments
and whitespace ignored. `tools/corpus_status.py` prints the same comparison
per program.

## Layout

Each program has its own directory, in the format of the local corpora
(`crates/oxide-vcl/tests/local_corpus.rs`):

- `input.vcl`: the program as vcl compiled it, after a short provenance
  header. The tests drop the header (up to and including the line
  `; -- end of corpus header --`) and check that what remains has the
  `input_sha256` recorded in `result.json`. A header line shifts the line
  numbers in diagnostics, so compile the file without it when comparing
  diagnostics by hand.
- `result.json`: the original's run: its command line (`-t1 -uCORPUS`), exit
  status, the SHA-256 of its binary, input and outputs, and timing.
- `stdin.bin`, `stdout.bin` and `stderr.bin`: what it read and printed.
- `native-stdout.bin` and `native-stdout.json` (two programs): the original's
  output on native x86 hardware. It is the reference where emulation changed
  the result. Emulated, the loop search's watchdog stops a strategy that
  finishes natively.
- `source.vcl` (`openvcl__examples_triangle-gasp_vu1Triangle` only): the
  example before masp preprocessing.

Names are `project__path`: the repository name, then the file's path in it
with `/` turned into `_` and the extension dropped. The fjtrujy/openvcl fork's
`test/repro/quad_adc_bug.vcl` differs from glampert/openvcl's and gets an
`_fjtrujy` suffix.

The original's outputs were recorded under an x86 emulator. Its watchdog
counts emulated CPU time, with the scale set to 1 (`alarm1`). vcl itself is
SCE's and is not distributed here.

## Programs and provenance

`manifest.json` lists every program's repository, commit, path, how it was
produced, its license, and other repositories that hold the same file.

| Programs | Source (commit) | Files | How | License |
|---|---|---|---|---|
| `quake2-ps2__src_ps2_renderer_vu1progs_{textured_triangles,particles}` | [glampert/quake2-ps2](https://github.com/glampert/quake2-ps2) (12f2f9a); [glampert/quake-ps2](https://github.com/glampert/quake-ps2) (ac62ce9) has the same files | `src/ps2/renderer/vu1progs/*.vcl`, with `vu_clip.i` and `vu_common.i` | `vclpp -I src/ps2/renderer/vu1progs -Wundef -Werror -j` | GPL-2.0 |
| `tyra__*` (15) | [h4570/tyra](https://github.com/h4570/tyra) (c59f406) | `engine/src/renderer/**/*.vclpp` | `vclpp`, run from `engine/` | Apache-2.0 |
| `ps2gl__vu1_scei`, `ps2gl__examples_tricked_out_billboard_renderer` | [glampert/ps2gl](https://github.com/glampert/ps2gl) (794bedc) | `vu1/scei.vcl`, `examples/tricked_out/billboard_renderer.vcl` | `vclpp -m -x -f -I …` (masp mode) | LGPL-2.1 |
| `openvcl__test_fixtures_*` (12) | [glampert/openvcl](https://github.com/glampert/openvcl) (6f8607b), from ps2dev/openvcl | `test/fixtures/*.vcl` | copied | LGPL-2.1 |
| `openvcl__examples_*` (4) | glampert/openvcl (6f8607b), from ps2dev/openvcl | `examples/<name>/vu1*.vcl` | copied; triangle-gasp preprocessed by masp | AFL-2.0 |
| `openvcl__test_repro_quad_adc_bug` | glampert/openvcl (6f8607b), from ps2dev/openvcl | `test/repro/quad_adc_bug.vcl` | copied | AFL-2.0 |
| `openvcl__test_repro_quad_adc_bug_fjtrujy` | [fjtrujy/openvcl](https://github.com/fjtrujy/openvcl) master (aa5e594) | `test/repro/quad_adc_bug.vcl`, the original with `0xffff` | copied | AFL-2.0 |
| `openvcl__repros_*` (12) | glampert/openvcl, `test/local-corpus` branch (5729d1f) | `repros/*.vcl` | copied | AFL-2.0 |
| `doom3-bfg-ps2__src_ps2_renderer_vu1progs_{passthrough,world_lm,world_strips}` | [glampert/doom3-bfg-ps2](https://github.com/glampert/doom3-bfg-ps2) (adc427d) | `src/ps2/renderer/vu1progs/*.vcl`, with `vu_clip.i`, `vu_window.i` and `vu_memory.h` | `vclpp -I src/ps2/renderer/vu1progs -Wundef -Werror -j`, vclpp 95911cd, run from the repository root | GPL-3.0-or-later |

- **quake2-ps2 and quake-ps2**, by Guilherme Lampert, are PS2 ports of Quake II
  and Quake. They are under GPL-2.0, like the id Software sources they build on.
- **doom3-bfg-ps2**, by Guilherme Lampert, is a PS2 port of Doom 3 BFG
  Edition, under GPL-3.0-or-later like the id Software sources it builds on.
  Each program's header reads "This source code is released under the GNU
  GPL-3.0-or-later license"; `licenses/GPL-3.0.txt` is the repository's
  `LICENSE.txt`.
- **Tyra**, by Sandro Sobczyński: each program's header reads "Copyright 2022,
  tyra - https://github.com/h4570/tyra. Licensed under Apache License 2.0". The
  repository has no LICENSE file, so `licenses/Apache-2.0.txt` is the text from
  apache.org.
- **ps2gl**, by Sony Computer Entertainment America, is under LGPL-2.1 (its
  headers read "Copyright (C) 2000,2001,2002 Sony Computer Entertainment
  America"). The 12 fixtures are ps2gl's `vu1/*.vcl` renderers with their `.i`
  includes expanded. Francisco Javier Trujillo Mata committed them to
  ps2dev/openvcl without the header (3d8ee88), so they keep ps2gl's license.
- **openvcl**: the examples' headers read "Copyright (C) 2004 Jesper Svennevid,
  Daniel Collin. Licensed under the AFL v2.0." `test/repro/quad_adc_bug.vcl`,
  by Francisco Javier Trujillo Mata, and the `repros/` programs have no header
  of their own, so the repository's AFL-2.0 applies.

Each program keeps the license of its source; the texts are in `licenses/`.

## Adding a program

Record the original's run for the program, under the original's options, in
the same format. Add the header, an entry in `manifest.json`, and a row here.
The test counts the programs, so update its expected total too.
