# VUSL libraries: code shared by several VU1 programs

## Status

| Milestone | Status |
|---|---|
| 0. Design | Done (this document) |
| 1. Calls in oxide-vcl | Done |
| 2. Libraries in VUSL | Done |
| 3. The doom3 clipper as a library | Done |
| 4. Docs | Done |
| 5. The library in doom3-bfg-ps2 | Done |

Milestones 1–4 are in this repository and milestone 5 in the game repository, whose program
manager uploads the clipper once at 1536 for the world programs that call it.

## Context

VUSL inlines every call, so each program carries its own copy of every function it uses. VU1 micro
memory holds 2048 instructions, which every resident and swapped program shares. Doom 3 will have
a VU1 program per pass, most with the same near-plane and guard-band clipper. Today one program can
even hold two copies: the world_lm port inlines its list pass once per draw pass, so its clipper is
there twice, and it is 1520 instructions against the hand-written program's 1019
(`docs/VUSL_PERFORMANCE.md`).

The goal is a library: functions compiled once, kept resident in micro memory, and called by any
program. Inlining stays the default; a library is for code whose size matters more than the few
cycles a call costs.

doom3-bfg-ps2's program manager (`src/ps2/renderer/vu.h`) already keeps a resident region and a
swap region in micro memory, uploading programs with MPG chains. A library is one more resident
upload.

### What forces the design

- **Calls use absolute addresses.** BAL and B take an 11-bit signed offset in instructions, about
  ±1024, relative to the branch. A program can be uploaded anywhere in the swap region, so its
  distance to a library is not known when it is assembled. A call is therefore
  `iaddiu vaddr, vi00, entry` and `jalr vlink, vaddr`, and the return `jr vlink`. JALR stores the
  address after its delay slot in `VI[it]` and jumps to `VI[is]`, in 8-byte units, with one delay
  slot and a throughput and latency of 2 (VU User's Manual, JALR). The library sits at a fixed
  address, and programs stay relocatable.
- **vcl compiles one program at a time.** It schedules and allocates a program as a whole and knows
  nothing outside it. Inferred from vcl 1.4beta7 (`docs/INDIRECT_FLOW.md`), a JALR whose target
  register is set by an ordinary instruction such as `iaddiu` needs an explicit tag list of its
  targets, which must be labels in the program. A call into code outside the program needs a
  contract instead: which registers carry arguments and results, and which the callee may change.
  `--enter` and `--exit` already state such a contract at a program's edges, with fixed physical
  registers.
- **The allocator must keep live values out of the callee's registers.** A value live across a call
  must be in a register the library does not write, or in memory. vcl's register availability
  (`.init_vf`, `.init_vi`) is per program, not per point, so it cannot express this.

## Design

### Layer 1: calls in oxide-vcl (`Mode::Oxide` only)

Two annotations, which vcl 1.4beta7 does not have. `docs/VCL_REFERENCE.md` ("Calls") is their
reference, and `docs/COMPATIBILITY.md` notes them as oxide-only.

**`--call` in a caller,** right before the JALR it describes:

```text
        iaddiu    vi11, n, 5            ; an argument, where the callee reads it
        iaddiu    entry, vi00, 1600     ; the callee's micro address
--call
in_vi   vi11
out_vi  vi11
out_vi  vi12
out_vf  vf24
out_vi  vi15
--endcall
        jalr      vi15, entry
```

- `in_` lines name the physical registers and hardware resources the callee reads; `out_` lines
  those it writes, results and clobbers alike. The caller moves arguments into those registers and
  results out of them with ordinary instructions.
- The declared registers become implicit operands of the JALR, pinned to their physical
  registers, as is its link register. The address register can be symbolic.
- **In the flow graph,** the call ends its block and falls through to the next one, where the
  callee returns. No tag list is needed, and the redundant-branch pass leaves it.
- **In the allocator,** a value live across the call is kept out of every register the call
  writes, or goes to the work area. The writes land as late as JALR's own latency, so an earlier
  write of the same register (the address, say) does not collide with them.
- **In the scheduler,** nothing moves across the call's block boundary, and a loop that holds a
  call is not pipelined. The usual delay-slot exchange may move an earlier lower instruction into
  the JALR's delay slot, which still runs before the callee.

**`--return` in a library:**

```text
.init_vf vf24
.init_vi vi11-vi12
--enter
in_vi   x(vi11)
in_vi   vi15
--endenter
        iaddiu    y, x, 10
--return vi15
out_vi  y(vi11)
--endexit
```

`--return` is an exit that ends with `jr vi15` and its delay slot instead of the E bit, with its
outputs in the registers listed. The link register is an output, so nothing reuses it. The
library keeps to its bank with `.init_vf` and `.init_vi`. Each entry is a program of its own: its
branches are relative, so its listing can be placed anywhere.

### Layer 2: libraries in VUSL

**The interface is a VUSL header** that the library and its callers both include:

```glsl
// clipper.vush
#pragma once
#pragma library(clipper, address = 1536, vf = 24, vi = 10, saved_vf = 8, saved_vi = 4, save = saves)
#include "doom3.vush"

struct ClipSaves { qword q[9]; };
struct ClipSpill { qword q[7]; };
layout(address = 0) uniform Frame frame;              // the library's own memory
layout(address = 957) inout ClipBuffers clip;
layout(address = 1008) inout ClipSaves saves;          // its saved registers
layout(address = 1017, spill) inout ClipSpill spill;   // its work area

export void ClipTriangle(inout ref<WindowVertex> cursor, inout int16 room,
                         ref<Projected> a, ref<Projected> b, ref<Projected> c, float pass);
```

- `#pragma library(name, address = A, vf = F, vi = I)` names the library, gives the micro address
  it is uploaded to, and its register bank: VF*F*–VF31 and VI*I*–VI14. Arguments, results and the
  library's own values use the bank, and every call clobbers it, with VI15 (the link), VI01, ACC,
  Q, P, I and the MAC and clip flags. The defaults are VF16 and VI11.
- `saved_vf = n`, `saved_vi = n` and `save = memory` let the library work in the *n* registers below
  each bank too: an entry stores them to `memory` on entry, behind a barrier, and loads them back
  before it returns. A call then clobbers only the bank. Without saves the clipper needs VI06–VI14
  as its bank, and its callers cannot keep their loop state through a call.
- **Memory at file scope.** The header binds the library's memory with the same placements as
  `main`'s parameters. In a caller these are memory the program does not name but the library
  uses: they take part in the overlap check against `main`'s parameters (unless one says `alias`),
  and the value cache forgets them at a call.
- **Exports** are prototypes in the header (`export` and a signature, no body) and definitions in
  the library's source. Entry *k*, in declaration order, is at `address + 2k`.
- **The calling convention comes from the signature.** In declaration order, `int16` and `ref<T>`
  parameters take VI11, VI12, ... and `float` and vector parameters VF*F*, VF*F+1*, ..., in the
  lanes their types fix. A return value takes the next register of its kind. `in` and `inout`
  values travel in; `out` and `inout` values and the return value travel back. A signature that
  does not fit the bank is an error. Memory parameters are not allowed: a ref passes an address.
- **A ref parameter** may point anywhere: the library knows nothing of the caller's memory, so
  stores through it may change any memory, and reads through it are forgotten at its stores.
- **Functions that are not exported** are inlined into the exports, as today.

**The library build** (a file with `#pragma library`, export definitions and no `main`) writes one
listing:

- an entry table, one pair per export and its delay slot: `b` to the export's code;
- each export compiled as a program of its own: `--enter` binding its parameters to their
  registers, the body, and `--return vi15` binding its results.

Entry addresses so stay fixed when the library's code changes, and callers need only the header.
The table costs one branch per call.

**A caller** includes the header and calls the exports:

- **A call** to an exported function becomes the arguments moved into their registers, the entry
  address `iaddiu`, a `--call` block and the JALR, then the results moved out. Values live across
  it stay out of the bank.
- **The value cache** forgets every value read from memory other than `uniform` memory, Q, and the
  clip flags' judgements.
- **The interpreter** runs a call as the inlined body when the definition is visible, so tests
  include the library's source rather than only its header; a program that sees only the
  prototype still compiles.

### Layer 3: the doom3 clipper

- `clipTriangle` and its helpers move from `tests/vusl/programs/include/doom3.vush` into a
  `clipper.vusl` library.
- New copies of the world_strips and world_lm ports call it; the inlined ports stay for
  reference. The pass becomes a run-time argument: the hand-written world_lm already runs one list
  pass for both draw passes, with the pass kept in memory.
- The VU model in the programs suite loads the library at its address next to the program.
- The report gains a column for the library's size, and a line for the micro memory the two
  programs take with and without it.
- doom3-bfg-ps2 uploads the library once into the resident region, at the address its header
  gives. That change is in the game repository and follows its commit rules.

## Costs

Measured in `docs/VUSL_PERFORMANCE.md` ("Libraries"): the two library ports and the clipper take
34% fewer instructions than the two inlined ports, and the scenes from 29% fewer to 12% more cycles
than the hand-written programs (the inlined ports: 3% to 43% fewer).

- **Register pressure** is most of it: what a loop keeps live across a call cannot use the bank,
  and vcl keeps a value that crosses streams in one register for all of them, so some of the
  callers' values live in the work area for the whole program.
- **Each call:** the address `iaddiu`, the JALR, the entry table's branch and the JR, each with its
  delay slot, the argument and result moves, the saves and restores, and the scheduling lost at
  the call.
- **Specialization:** constants that inlining folds into the callee, such as world_lm's pass, are
  arguments instead.

## Decisions

- **The bank** is VF16–VF31 and VI11–VI14 by default. The clipper's is VF24–VF31 and VI10–VI14
  (its five VI arguments), with VF16–VF23 and VI06–VI09 saved. A library may also spill to a
  work area of its own.
- **Library memory:** a library owns data memory, declared in its header. Memory it only reads may
  overlap its callers'; memory it writes may not. The clipper's saves and work area take
  1008–1023, which doom3's memory map leaves free.
- **Entry addresses** are numbers: the header gives the library's address, and the entry table
  fixes each entry's offset.
- **The ports:** the inlined ports stay as they are, for reference; the library versions are new
  copies (`world_strips_lib`, `world_lm_lib`) beside them.

## Milestones

Each ends with fmt, clippy, the full release suite and the public corpus at 52/52. The vcl path stays
unchanged: the new annotations are rejected in vcl mode.

1. **Calls in oxide-vcl.**
   - Parse `--call`/`--endcall` and `--return`, which ends with `--endexit`.
   - The call's registers as pinned implicit operands of its JALR; the call as a fallthrough in the
     flow graph; `jr` emission for `--return`.
   - Tests: hand-written VCL callers and libraries run together in the VU model with the library at
     a fixed address. One case where values live across a call keep their registers; one where
     pressure forces a spill; one with calls on several paths.
   - `docs/VCL_REFERENCE.md` and `docs/COMPATIBILITY.md`.
2. **Libraries in VUSL.**
   - `#pragma library`, `export` and prototypes, and file-scope memory in libraries.
   - The library build: the entry table and one entry program per export.
   - The calling convention; calls as `--call`; the checker's signature and overlap checks; the
     cache rules after a call.
   - The interpreter's inlined meaning.
   - Tests: check cases for bad signatures, prototypes and overlaps; codegen cases for a call and
     a library entry; semantics tests that run a library and a program together.
3. **The doom3 clipper as a library.**
   - The ports switched over, and the programs suite and report extended.
   - `docs/VUSL_PERFORMANCE.md` updated with the size saved and the cycles paid.
4. **Docs.** `docs/VUSL.md` (libraries, the header, the calling convention) and
   `docs/VUSL_COMPILER.md` (the library build and call lowering).
5. **The library in doom3-bfg-ps2.** Its program manager uploads `clipper.vsm` once into the
   resident region at 1536, and the world programs that clip call it. The change follows the game
   repository's commit rules.

## Verification

- The programs suite keeps identical packets and the interpreter's memory on every scene, with
  the clipper called instead of inlined.
- The report shows world_strips and world_lm together needing fewer instructions with the library
  than without, and each scene's cycles, with the change against the inlined build.
- doom3-bfg-ps2 builds with the library uploaded once, and its capture tests draw the same frames.
