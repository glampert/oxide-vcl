# VUSL samples

Two small VU1 programs written in [VUSL](../docs/VUSL.md), each with the EE program that runs it.
Each builds into a standalone ELF for a PS2 or an emulator such as PCSX2.

| Sample | What the VUSL program shows |
|---|---|
| [`hello_triangle`](hello_triangle) | The smallest useful program: one batch at a fixed address, a range-for over its vertices, colour and position converted to the GS's formats (`ftoi0`, `ftoi4`), and one `xgkick` |
| [`rotating_cube`](rotating_cube) | A model-view-projection matrix (`vec4(pos, 1.0) * mvp`), the perspective divide, depth, the VIF double buffer through XTOP, and guard-band judgement: `clipw` on each corner, and `adc` to have the GS skip a triangle with a corner outside |

Each sample is three files:

- `<sample>.vusl`, the VU1 program.
- `main.cpp`, the EE side. It sets up the GS with libgraph and libdraw, uploads the program and
  sends its data with libpacket2 and libdma, and for the cube builds the matrix with libmath3d.
  It fills VU1 memory through the structs and constants in the header oxide-vusl generates from
  the program (`<sample>.vusl.h`), so the two sides cannot disagree on the layout.
- `Makefile`, which builds oxide-vusl from this repository with cargo, compiles the program into a
  `.vsm` listing and its header, assembles the listing with `dvp-as`, and compiles and links the
  EE program with the SDK's own `samples/Makefile.pref` and `samples/Makefile.eeglobal_cpp`.

## Building and running

You need the [ps2dev](https://github.com/ps2dev/ps2dev) toolchain, with `PS2SDK` set and its
`bin` directories (for `mips64r5900el-ps2-elf-g++` and `dvp-as`) on `PATH`, and a Rust
toolchain for cargo.

```sh
cd samples/rotating_cube
make
```

This writes `rotating_cube.elf`. Run it in PCSX2 (`pcsx2 -elf rotating_cube.elf`, or through
the GUI's "Start File"), or on a PS2 with `ps2client execee host:rotating_cube.elf`. `make clean`
removes the outputs.

CI builds no C++ here, since that would need the whole SDK: `crates/oxide-vusl/tests/samples.rs`
compiles each sample's VUSL program and header as its Makefile does.
