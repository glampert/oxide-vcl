# VUSL libraries: code shared by several VU1 programs

## Status

| Milestone | Status |
|---|---|
| 0. Design | Done (this document) |
| 1. Calls in oxide-vcl | Not started |
| 2. Libraries in VUSL | Not started |
| 3. The doom3 clipper as a library | Not started |
| 4. Docs | Not started |

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

Two annotations, which vcl 1.4beta7 does not have. `docs/COMPATIBILITY.md` and
`docs/VCL_REFERENCE.md` describe them as oxide-only.

**`--call` in a caller:**

```text
--call clipper_ClipTriangle(vi13)
in_vi   cursor(vi11)
in_vi   room(vi12)
in_vi   a(vi13)
in_vf.x pass(vf24)
out_vi  cursor(vi11)
out_vi  room(vi12)
clobber vf24-vf31, vi11-vi14, vi01, acc, q, p, i, status, mac, clip
--endcall
```

- The name is the entry's symbol. The register in parentheses holds its address, which vcl loads
  with `iaddiu` from the symbol's value (an absolute micro address that the assembler or linker
  resolves). The link register is VI15 by convention, and is clobbered.
- The `in_` and `out_` lines name the caller's values and the physical registers they travel in, as
  in `--enter` and `--exit`. The allocator places arguments with the same external assignment it
  uses at those boundaries (`docs/EXTERNAL_ALLOCATION.md`).
- `clobber` lists everything the callee may change: VF and VI ranges, VI01, and the hardware
  resources ACC, Q, P, I, the status and MAC flags, and the clip flags.
- **In the flow graph,** the call is one instruction that falls through, not a branch. It reads its
  `in_` registers and writes its `out_` registers and its clobbers.
- **In the allocator,** a value live across the call gets a register outside the clobbers, or goes
  to the work area.
- **In the scheduler,** the call is a barrier: nothing moves across it. vcl emits the `iaddiu`, the
  `jalr` and its delay slot. A loop holding a call does not pipeline, as with `xgkick`.

**`--return` in a library:**

```text
.global clipper_ClipTriangle
--enter
in_vi   cursor(vi11)
...
--endenter
        ...
--return vi15
out_vi  cursor(vi11)
out_vi  room(vi12)
--endreturn
```

`--return` is an exit that ends in `jr vi15` and its delay slot instead of the E bit, with its
outputs in the registers listed. The library restricts its allocation to its clobbers with
`.init_vf` and `.init_vi`, which vcl already supports. One listing holds every entry of a library.
Each entry is a `.global` label, and `-j` writes their offsets.

A library entry may not `--cont`, and its code must not use `xtop` for memory the caller did not
pass it.

### Layer 2: libraries in VUSL

**A library file** names itself and exports functions:

```glsl
#pragma library(clipper, vf = 24..31, vi = 11..14)
#include "doom3.vush"

layout(address = 0) uniform Frame frame;            // the library's own memory
layout(address = 957) inout ClipBuffers clip;

export void ClipTriangle(inout ref<WindowVertex> cursor, inout int16 room,
                         ref<Projected> a, ref<Projected> b, ref<Projected> c, float pass)
{
    ...
}
```

- `#pragma library(name, vf = R, vi = R)` gives the register bank: the registers arguments,
  results and the library's own values use, which every call clobbers. The defaults are VF24–VF31
  and VI11–VI14.
- **Memory at file scope.** A library may bind memory there, with the same placements as `main`'s
  parameters: the clipper's buffers and the frame constants it reads. Programs see none of it
  except through what they pass.
- **The calling convention comes from the signature.** In declaration order, `in` and `inout`
  values take bank registers in the lanes their types fix, and `out` and `inout` values return in
  bank registers. A `ref<T>` passes as its VI address. A struct in memory passes as the ref to it.
  A signature that does not fit the bank is an error.
- **Functions that are not exported** are inlined into the exports, as today.
- **The compiler writes:**
  - the library's `.vsm`, with one `--enter`/`--return` per export;
  - an interface file, `clipper.vuslib`, in JSON. It records:
    - the bank;
    - for each export, its symbol, offset, argument and result registers with their lanes, and
      the memory it reads and writes, both its own and through each ref argument;
    - the library's memory bindings and size in instructions;
  - optionally a C++ header with the size and symbols, for the program manager, which picks the
    library's resident address.

**A program imports it:**

```glsl
#pragma import(clipper)              // found through -L <dir>
```

- **A call** to an exported function becomes a `--call` block, not an inlined body. The call loads
  the entry's address as `clipper_ClipTriangle`, an absolute symbol the build defines as the
  library's resident address plus the entry's offset.
- **The checker** checks the arguments against the interface: types, lanes and access. It checks
  that the library's memory does not overlap `main`'s parameters, unless they say `alias` (for
  example, the frame constants both read).
- **Lowering** moves arguments into place, which the peephole and vcl's coalescing usually make
  free. After the call it forgets every cached value the library may write: its own memory, the
  regions of the refs it writes through, and the clobbered registers.
- **Reflection** lists the libraries a program needs and the symbols it uses.
- **The interpreter** runs an exported function as an inlined call, with the same meaning, so the
  differential tests keep working.
- **Inlining on request.** A `-D` switch (`VUSL_INLINE_LIBRARIES`) makes the compiler inline a
  library's exports instead, for comparing size and cycles.

### Layer 3: the doom3 clipper

- `clipTriangle` and its helpers move from `tests/vusl/programs/include/doom3.vush` into a
  `clipper.vusl` library.
- The world_strips and world_lm ports import it. The pass becomes a run-time argument: the
  hand-written world_lm already runs one list pass for both draw passes, with the pass kept in
  memory.
- The VU model in the programs suite loads the library at its address next to the program.
- The report gains a column for the library's size, and a line for the micro memory the two
  programs take with and without it.
- doom3-bfg-ps2 uploads the library once into the resident region and defines its entry symbols.
  That change is in the game repository and follows its commit rules.

## Costs

- **Cycles per call:** about 4 to 6. That is the address `iaddiu`, the JALR and its delay slot, the
  JR and its delay slot, and the scheduling lost at the barrier. The clipper runs only for
  triangles outside the guard band, so the scenes' totals should move by well under 1%; the
  programs suite measures it.
- **Specialization:** constants that inlining folds into the callee, such as world_lm's pass, are
  arguments instead.
- **Register pressure** near calls: values live across a call cannot use the bank.

## A first step without layer 1

To prove the upload, addressing and measurement before the allocator work:

- callers drop the bank from `.init_vf`/`.init_vi` entirely;
- VUSL emits the JALR as raw code between two `--barrier`s;
- the library ends each export with `--exitm` whose text is the JR and its delay slot.

Every caller then loses the bank everywhere, not only across calls, and world_lm already spills
VI values. This step is for measurement and is replaced by layer 1.

## Open questions

- **The bank's size.** VF24–VF31 and VI11–VI14 fit the clipper's signature (five VI arguments and
  one float) and its own values, but its passes need more VF registers than eight. One choice is
  a larger VF bank, VF16–VF31. The other is the library spilling to its own work area, like a
  program. Measuring the clipper compiled under each bank decides it.
- **Library memory.** The recommendation is that a library may own data memory (the clip buffers),
  declared at file scope as above. The alternative passes all of it by ref, which costs argument
  registers.
- **Resolving entry addresses.** Either the build defines the entry symbols (`clipper_ClipTriangle =
  base + offset`) when it links, or the program manager patches the `iaddiu` immediates at upload.
  The first keeps programs plain; it needs the library's base address fixed at build time.
- **Branch.** This could continue on `vusl` or start a branch of its own from it.

## Milestones

Each ends with fmt, clippy, the full release suite and the public corpus at 52/52. The vcl path stays
unchanged: the new annotations are rejected in vcl mode.

1. **Calls in oxide-vcl.**
   - Parse `--call`/`--endcall` and `--return`/`--endreturn`.
   - The call as a flow-graph node; clobbers in webs and allocation; the scheduling barrier;
     `jalr` and `jr` emission.
   - Tests: hand-written VCL callers and libraries run together in the VU model with the library at
     a fixed address. One case where values live across a call keep their registers; one where
     pressure forces a spill; one with calls on several paths.
   - `docs/VCL_REFERENCE.md` and `docs/COMPATIBILITY.md`.
2. **Libraries in VUSL.**
   - `#pragma library`, `export`, file-scope memory in libraries, and `#pragma import` with `-L`.
   - The interface file; the calling convention; calls as `--call`; the checker's interface and
     overlap checks; the cache rules after a call.
   - The interpreter's inlined meaning, and `VUSL_INLINE_LIBRARIES`.
   - Tests: check cases for bad imports, signatures and overlaps; codegen cases for a call and a
     library entry; semantics tests that run a library and a program together.
3. **The doom3 clipper as a library.**
   - The ports switched over, and the programs suite and report extended.
   - `docs/VUSL_PERFORMANCE.md` updated with the size saved and the cycles paid.
4. **Docs.** `docs/VUSL.md` (libraries, imports, the calling convention) and
   `docs/VUSL_COMPILER.md` (the interface file and call lowering).

## Verification

- The programs suite keeps identical packets and the interpreter's memory on every scene, with
  the clipper called instead of inlined.
- The report shows world_strips and world_lm together needing fewer instructions with the library
  than without, and each scene's cycles, with the change against the inlined build.
- doom3-bfg-ps2 builds with the library uploaded once, and its capture tests draw the same frames.
