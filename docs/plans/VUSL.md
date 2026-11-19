# VUSL: a shading language for VU1, compiled through oxide-vcl

## Status

The milestones are defined under [Milestones](#milestones-each-ends-with-fmt-clippy-the-full-release-suite-and-the-public-corpus-5252).

| Milestone | Status |
|---|---|
| 0. Branch and groundwork | Done |
| 1. Front end | Done |
| 2. Reflection | Done |
| 3. End-to-end baseline | Done |
| 4. Optimizer | Done |
| 5. VU features | Done |
| 6. Ports and benchmarks | Done |
| 7. Docs | Done |

## Context

VU1 microprograms are written by hand in VCL. VCL is assembly with symbolic registers, and
oxide-vcl schedules, pairs and allocates it. The goal is a GLSL-like language, VUSL (Vector Unit
Shading Language), whose compiler emits VCL that is at least as good as the hand-written
programs.

The pipeline stays two tools: `prog.vusl` goes to `oxide-vusl`, which runs the preprocessor, the
frontend, then `vcl::compile` in-process. That writes `prog.vsm` plus the reflection files, and
`prog.vsm` goes to dvp-as. The work happens on a new branch, `vusl`, in two new crates, `vusl` and
`oxide-vusl`.

**What the research established, which shapes everything below:**

- **oxide-vcl schedules exactly the operations it is given.** It removes dead code, narrows field
  masks, folds VI immediates, reorders and pairs. It does not do CSE, mul+add→madd fusion, ACC or
  broadcast selection, floating-point constant folding, copy propagation, loop-invariant hoisting
  or pointer merging. The frontend has to do all of these to match hand-written code.
- **A loop is pipelined only when it is a single block** that ends in a conditional VI branch back
  to its label. The block must not contain XGKICK, XTOP, XITOP, random-unit instructions or
  `--barrier`. Loop hints are `--LoopCS c,s`, `--LoopExtra` and `--LoopAbs`. In oxide mode
  `--LoopCS 0,0` opts a loop out, and `loops`/`unswitch` handle unhinted loops and uniform
  branches.
- **The real programs** (doom3 world_lm and world_strips, quake2 particles and textured_triangles,
  ps2gl, tyra) work the same way:
  - Each runs once per MSCAL, or resumes with MSCNT after `--cont`.
  - Constants sit at fixed low addresses, and the per-batch data is reached through XTOP in the
    VIF BASE/OFFSET double buffer.
  - Headers carry counts in spare lanes.
  - GIF tag templates come from the EE and get NLOOP/EOP patched with `isw.x`.
  - The ADC bit is derived from clip flags.
  - The interface is always empty `--enter`/`--exit`; all I/O goes through data memory.
  - C++ and VCL are kept in sync by shared `#define` headers or literal `static_assert`s, which
    VUSL's reflection replaces.
- **Hand-written programs think in lanes:** packed colour in `.x` with the position in `.yzw`,
  lightmap UVs kept in `.zw`, Q stored straight into `.z` with `addq.z`.
- **Reusable pieces:**
  - `vcl::compile(&str, name, &Options)` works on memory.
  - `vcl::preprocessor::preprocess` with a `SourceLoader`.
  - The `vcl::diagnostics` `Reporter`, which handles colour, multi-file quoting and
    `set_line_origins` for mapping generated lines back to source.
  - `crates/oxide-vcl/tests/support/vu_model.rs`, a timed or logical VU model, and `scenes.rs`,
    which gives VU1 start states for the four doom3/quake2 programs, packet capture, drawn-primitive
    counts and cycles.

**Decisions made with the user:**

1. **Kernel model.** `void main()` runs once per MSCAL and controls everything. A range-for gives
   the compiler the loop shape. A per-vertex stage could be added later as sugar.
2. **Strict cost.** An operation with no direct VU mapping is a compile error that names the
   explicit form. Source cost and instruction cost agree.
3. **Bindings are `main`'s parameters.** There are no global variables: file scope holds types,
   constants and functions. The memory a program uses arrives as `main`'s parameters, so it can be
   scoped and passed down to functions. Because everything is inlined, this is purely front-end
   syntax.
4. **No special block syntax.** Memory is described with plain `struct`s. Output that the program
   builds in the input buffer is just the struct's last member, ending in an open array (`[]`).
   Access comes from the parameter qualifiers (`uniform`/`in`/`out`/`inout`), and placement from a
   `layout` on the parameter.
5. **`int16`, not `int`.** The VI registers are 16 bits wide, and the name says so.
6. **`qword_t` in C++.** The header uses `qword_t` like tamtypes.h, and its value types mirror the
   VUSL declarations.

## The language

### Principle: no hidden instructions

Each construct (operator, builtin, assignment, constructor) has a documented lowering with a
maximum instruction count, listed in the reference's cost table. The compiler never emits an
instruction that no construct accounts for, and optimizations only remove or fuse.

The following are not constructs. Each is an error with a suggested rewrite:

| Rejected | Suggested rewrite |
|---|---|
| Vector operands of one operation in different lanes (`a.xy + b.zw`) | Assign one to a variable declared with `layout(lane = ...)`, which moves its lanes |
| A swizzle read that names a component twice (`v.xx`) | The scalar `v.x`, which an operation broadcasts |
| An instruction's operand outside the lanes it works in (`cross`, `clipw` and the EFU's vector operations take x, y and z) | The same `layout(lane = x)` move |
| Vector ÷ vector | Per-lane scalar divisions |
| `int16` `*`, `/`, `%`, `<<`, `>>`, `^`, `~` | VU has no such instructions; write adds |
| Per-lane selects and `?:` | `if` |
| Arithmetic on integer-lane vectors | Would flush as denormals |
| Lane moves of raw bits that would need an FMAC op | Only `mr32`/`mfir`/`mtir` move bits |

### Types and lanes

| Type | Lives in | Notes |
|---|---|---|
| `float`, `vec2`, `vec3`, `vec4` | VF lanes | Every vector value has a **placement**, the lanes holding its components, as part of its static type (printed `vec2@zw`) |
| `mat3`, `mat4` | 3 or 4 qwords, columns (GLSL) | `layout(row_major)` stores rows. `m[i]` is the i-th stored qword, a row when row_major, so indexing is always direct; this differs from GLSL. The index must be constant. A local matrix takes its initializer's storage |
| `int16` | VI, 16-bit wrap | Signed compares. A memory `int16` takes a 32-bit lane: `ilw` reads the low 16 bits, `isw` stores them zero-extended. There is no `int`; writing it is an error that suggests `int16` |
| `bool` | VI non-zero, or a pending flag test | A float comparison is a masked `sub` and `fsand` (S, or S and Z, the operands swapped for `>` and `>=`; `!=` adds an `iaddi`), `any()` FCAND's `vi01`, `||` `ior`, an `int16` `!=` `isub`. The rest (`&&`, `!`, other `int16` comparisons) set 0 or 1 around a branch |
| `int32`, `uint32`, `ivec2..4`, `uvec2..4` | VF lanes as raw 32-bit integers | Unpacked integers, `ftoi` results, packed colours: move, store and convert only. Signedness only matters to `itof` and to the reflected C++ type. `uint32(i)` from an `int16` is `mfir`. `floatBitsToUint(v)`, `floatBitsToInt`, `uintBitsToFloat` and `intBitsToFloat` give the same register's bits under the other type, with no instruction |
| `qword` | 128 opaque bits | GIF tags, A+D packets; `.x`..`.w` readable and writable as `int16` |
| `clipflags` | one CLIPW judgement | From `clipw()`, consumed by `any()`/`adc()` |
| `ref<T>` | VI qword address | Explicit addressing. `ref<T>(m)` is the address of memory `m`, a `T` or an array of them; `ref<T>(m, a)` an `int16` address `a` within `m`. `r.member` and `r[i]` are memory through it, and it passes as memory to functions and `xgkick`. `r + n` advances `n * sizeof(T)` qwords, with `n` constant unless `sizeof(T) == 1`; `r - s` is a distance in elements under the same rule; refs compare; `int16(r)` is the address. `ref<T>(m)` of memory of another type views it under `T`'s layout, which must fit in it; an array, or memory reached through a ref, is a run of memory any `T` fits. A ref stays within the memory it was first set from, which gives stores through it an alias region |

**Placement rules:**

- **Loads** keep memory's lanes.
- **Swizzle reads move nothing.** The placement follows the components: `v.zw` of a vector in
  lanes xyzw is in lanes z and w, and `v.yx` in lanes y and x.
- **Element-wise ops** need equal placements, except a scalar operand. A scalar is always a free
  broadcast (`ft[x]`), and commutative ops swap their operands to allow it.
- **A plain local takes the lanes of each value assigned to it.** Where paths meet (after an `if`,
  around a loop), they must agree.
- **Lanes move only in an assignment into fixed lanes:** a member in memory, components of a
  variable (`st.xy = uv`), or a local declared with `layout(lane = L)`, whose components then sit
  in consecutive lanes from L. Such a local may take values from any lanes on any path.
  - The move costs `mr32` steps for a rotation, otherwise one broadcast op per lane.
  - Raw bits only move by rotation; any other raw move is rejected.
- **Scalars are placed by the compiler** in the lane of their store or non-broadcast use. Writing
  a scalar into one lane (`st.z = q`) costs at most one instruction (`addq.z st, vf00, q`) and
  nothing when the scalar was computed there.
- **Constructors** cost nothing when the consumer only broadcasts their components:
  `m * vec4(p.pos, 1.0)` uses `p[y..w]` and `vf00[w]`. Otherwise they cost one instruction per
  part not computed in place.

### Memory: plain structs bound as `main`'s parameters

There is no block syntax. Memory is described with ordinary `struct`s, usually in an include file
that several programs share. A program binds them as `main`'s parameters, and the parameter carries
everything VU-specific:

- **Access** comes from GLSL's parameter qualifiers.
- **Placement** comes from a `layout`.

```glsl
void main(layout(address = 0)                        uniform Frame frame,
          layout(xtop, vif_base = 8, vif_offset = 474) inout   Batch batch)
```

| Qualifier | Meaning |
|---|---|
| `uniform` | Host-written constants, read-only. This mirrors GLSL; reflection marks them |
| `in` | Read-only data, typically per batch |
| `out` | Write-only |
| `inout` | Read and written: in-place transforms, output built in the input buffer, state kept across runs |

| Placement | Meaning |
|---|---|
| `layout(address = N)` | A fixed qword address |
| `layout(xtop)` | The current double buffer, read with `xtop` |
| `vif_base = B, vif_offset = O` | The VIF1 BASE and OFFSET register values the host sets. They go to reflection, and the compiler checks that the struct's largest size fits the half |
| `layout(xitop)` on an `int16` parameter | The count the host sent with VIF ITOP |
| `spill` | On an `inout` struct at a fixed address: the region becomes `.vu_work_area`, where oxide-vcl spills. The program cannot use it. When values spill, the VCL is generated again with fewer hoisted invariants, and the compile with the fewest spills is kept |

**Checks.** Every parameter of `main` needs a placement. Parameters whose memory overlaps are an
error unless one says `alias = other`.

**Access per member.** Reflection reports what the program reads and writes in each member, so
the host learns that `batch.packet` is output without any syntax for it.

**Passing down.** A struct parameter bound to memory is passed as a reference, never copied, and
the same goes for a range-for element. The qualifier sets the access, as in
`void emitCorner(out Sprite s, vec3 w)`. Since everything is inlined, this costs nothing. A local
`struct` variable lives in registers.

**Packing rules ("VU packing"):**

- Members go in order. A scalar takes one lane and a `vecN` takes N consecutive lanes.
- A member that does not fit in the current qword starts the next one.
- `vec4`, `qword`, matrices, structs and arrays start on a qword, and their stride rounds up to
  whole qwords.
- Member qualifiers:
  - `layout(offset = q)` and `layout(lane = y)` override the placement: `layout(lane = y) vec3 pos`
    puts it in yzw.
  - `layout(unpack = v4_16, unsigned)` records the VIF format for reflection. It is type-checked:
    integer formats need `ivec`/`uint`/`qword` members.

**Arrays:**

| Form | Meaning |
|---|---|
| `T a[7]` | A constant length |
| `T a[count]` | A dependent length: an earlier `int16` member of the same struct, or a `layout(xitop)` parameter of `main` |
| `T a[]` | Open: as many elements as the program writes. Only as the last member, and a struct ending in one only as the last member of its parent, like a C flexible array member |

**Offsets after a dependent array.** Members after a dependent array, such as a whole `Packet`
after `particles[count]`, get run-time offsets. These are computed with adds, since there is no
multiply, so the element stride must be small. A member there may instead take a fixed qword with
`layout(offset = q)`; the arrays before it must then end by `q`, which reflection reports as a
limit on their lengths.

**Sizes.** The compiler computes each struct's size as a fixed part plus per-count terms. An open
array's length comes from the loop that writes it: a range-for zipped with `particles[count]`
writes `count` elements.

**Limits.** From the sizes the compiler solves for the largest count that fits the VIF half, and
reflection exports it (`kMaxCount` below). A program that writes an open array through `ref<T>`
cursors gets no solved limit, only the fixed part.

**XITOP lengths.** An array length may name one of `main`'s `layout(xitop)` parameters
(`Vertex vertices[count]`): the count the host sent with VIF ITOP. Offsets after it and sizes
take it as a term, reflection calls it `xitop`, and a range over the array is counted by it.

### Statements and loops

The GLSL statement set is supported, plus these:

- **Range-for:** `for (Particle p : batch.particles, out Sprite s : batch.packet.sprites) { ... }` zips
  ranges with `in` (default), `inout` (write back in place) or `out` element views. The trip count
  is the first range's length. The compiler rotates the loop into a guarded do-while and
  strength-reduces the element addresses to pointer increments.
- **Index loops:** ordinary `for (int16 i = ...)` loops get the same treatment.
- **`break`:** allowed. It splits the body, so the loop no longer pipelines, and the reference says
  so.
- **`yield;`:** `--cont`. MSCNT resumes after it, and values stay in registers (ps2gl/tyra run
  model B). The host sends another batch before MSCNT, so after a yield XTOP and the
  `layout(xitop)` parameters are read again, and nothing read from memory is kept. A yield in a
  range-for is an error: its elements are in the batch it stops on.
- **`return;`, or the end of `main`:** `--exit`.
- **Functions:** always inlined. `in`, `out` and `inout` parameters; no recursion. An `in`
  argument that is a constant is a constant in the body, so conditions on it fold; one that is a
  local the call does not change (the body does not assign the parameter and no `out` or `inout`
  argument names the local) is that local; anything else is copied.
- **Program name:** one entry per file. The name comes from `#pragma program(name)` and gives
  `name_CodeStart`/`End`; otherwise it is the file stem.
- **Scalar comparisons in conditions** have fixed lowerings:
  - `int16`: `ib**`, with `isub` first when neither side is zero.
  - `float`: a masked `sub` plus `fmand`/`fsand` and `ib**`, at 2 instructions plus the branch.
  - `any(v < w)` is the vector form.

### Builtins (each with a fixed, documented lowering)

**Math:**

| Builtin | Lowering |
|---|---|
| `M * v` | `mula`/`madda`/`madd`; a 0 component drops a term, a 1 uses `vf00[w]` |
| `dot` | `mul` + `adda`/`madd` |
| `cross` | `opmula`/`opmsub` |
| `normalize`, `length` | `dot` + `rsqrt`/`sqrt` + Q form |
| `1.0 / s`, `x / s` | `div` and the Q forms (`mulq`, `addq`, `maddq`) |
| `sqrt`, `rsqrt` | The FDIV unit |
| `min`, `max`, `abs`, `clamp` | `mini`, `max`, `abs`; `clamp` is both |

**Conversions and moves:** `ftoi0/4/12/15`, `itof0/4/12/15` (with `vec4(iv)` and `ivec4(v)` as the
0 forms), `mr32()`, `int(iv.x)`/`iv.x = i` (`mtir`/`mfir`), and the `floatBitsTo*`/`*BitsToFloat`
reinterpretations, which cost nothing. A float whose bits are read this way keeps them through
copies: see the raw moves under Units.

**Clip flags:**

- `clipw(vec3 judge, float w)`, `any(c...)`, `adc(c...)` and `clipped(c...)`. `adc` returns an
  `int16` whose bit 15 is set when any judgement is outside; the other bits are unspecified
  (`fcand vi01, mask` plus `iaddiu t, vi01, 0x7fff`). `clipped` is FCAND's 0 or 1 itself.
- The compiler picks the `fcand` mask from how many recent judgements are combined. The flags hold
  the last four, so the judgements read must be within four of the newest `clipw`, and the newest
  must be in the reading block, where VCL waits for its flags. Older ones may be in earlier
  blocks when no branch between them judges. Anything else is an error. `fcset 0` comes once.

**EFU (VU1):** `esadd`, `ersadd`, `eleng`, `erleng`, `esum`, `ercpr`, `esqrt`, `ersqrt`, `esin`,
`eatan`, `eexp`. The result lands in P and is read through `mfp`. The domains are documented.

**Data movement:**

- `xtop()` and `xitop()`, which the placements usually imply.
- `xgkick(memory)`: any struct or member bound to memory, or a `ref`. It kicks from that address.
- Struct and array copies between memory (`batch.packet.tags = batch.tags;` costs an `lq` and an
  `sq` per qword).

**Contraction:** a sum of two or more products is one ACC chain (`mula`, `madda`, `msuba`, then
`madd` or `msub`). VU1 rounds each product before adding it, so the chain computes what the
separate operations would; `precise` is accepted and changes nothing. Constant expressions fold in
IEEE single precision; the docs note that the VU is not IEEE.

**Literals:**

- 0.0 and 1.0 come from `vf00`.
- Other float literals cost a `loi` plus the I-register form (`muli`/`addi`/...), or two
  instructions once when hoisted into a register.
- `int16` literals are `iaddiu` (two instructions for 0x8000 and above).

### Pragmas

The preprocessor keeps `#pragma` lines, macro-expanding their arguments, and the parser attaches
each one to the next statement or to file scope:

| Pragma | Effect |
|---|---|
| `#pragma program(name)` | `.name` |
| `#pragma pipeline(min = C, safe = S, extra = E, abs = A)` | `--LoopCS C,S`, `--LoopExtra`, `--LoopAbs` |
| `#pragma nopipeline` | `--LoopCS 0,0` (oxide mode) |
| `#pragma unroll(N)`, `#pragma unroll` (full, constant trip count), `#pragma nounroll` | Frontend unrolling. `unroll(N)`, N from 1 to 16, applies to range-fors: a loop over groups of N elements, its body N times with the elements N strides apart, then the rest. A length read at run time splits with `iand`, so N must be a power of two there. `#pragma unroll` repeats a `for` loop with constant bounds, or a range-for of constant length, fully. An unrolled loop cannot `break` or `continue` |
| `#pragma barrier` | `--barrier` at that point |

A loop without a pipeline pragma gets no hint, and oxide-vcl's `loops` optimization decides
whether to pipeline it.

### Example: quake2's particles program

This is the whole of quake2's `particles.vcl`, with the same memory layout, so the scenes test can
compare it with the hand-written program.

```glsl
// frame.vush: the frame constants every quake2 program reads.
#pragma once
struct Frame {
    layout(row_major) mat4 mvp;           // rows: the EE's row-vector convention
    vec4  gsScale;
    vec4  gsOffset;
    vec3  clipScale;  float warpPhase;    // one qword: xyz + w
    vec4  colorClamp;
};
```

```glsl
// particles.vusl
#pragma program(VU1Prog_Particles)
#include "frame.vush"

struct Particle {                         // one qword
    uint32 color;                         // x: RGBA bytes, raw-copied into the A+D qword
    vec3   pos;                           // yzw
};

struct Sprite {                           // five qwords of the GS packet
    uint32 color;  float q;  int16 reg;   // A+D RGBAQ (w unused)
    vec4   uv0;
    ivec3  xyz0;   int16 adc0;            // XYZ2: 12.4 x, y and z; ADC is bit 15 of w
    vec4   uv1;
    ivec3  xyz1;   int16 adc1;
};

struct Packet {                           // the GS packet, sent with one XGKICK
    qword  tags[7];
    Sprite sprites[];                     // open: as many as the program writes
};

struct Batch {                            // one double-buffer half
    layout(lane = w) int16 count;
    vec4     quadOffset;                  // xyz: the corner offset, w: the distance blow-up rate
    vec4     uv0, uv1;
    qword    tags[7];                     // the GIF tags the EE prepared
    Particle particles[count];
    Packet   packet;                      // built by the program, right after the particles
};

void main(layout(address = 0)                         uniform Frame frame,
          layout(xtop, vif_base = 8, vif_offset = 474) inout   Batch batch)
{
    // The GS scale folded into the matrix and the corner offset, once a batch.
    mat4 mvp = frame.mvp;
    #pragma unroll
    for (int16 r = 0; r < 4; ++r)
        mvp[r].xyz *= frame.gsScale.xyz;  // mvp[r] is a row: row_major
    vec3 quadOffset = batch.quadOffset.xyz * frame.gsScale.xyz;
    vec3 judgeScale;
    judgeScale.x = frame.clipScale.x / frame.gsScale.x;   // div + addq.x each
    judgeScale.y = frame.clipScale.y / frame.gsScale.y;
    judgeScale.z = frame.clipScale.z / frame.gsScale.z;

    batch.packet.tags = batch.tags;       // 7 x lq/sq

    #pragma pipeline(min = 1)
    for (Particle p : batch.particles, out Sprite s : batch.packet.sprites)
    {
        vec4 c = vec4(p.pos, 1.0) * mvp;  // mul/madd/madd/madd: p[y], p[z], p[w], vf00[w]
        clipflags j = clipw(c.xyz * judgeScale, c.w);
        float q = 1.0 / c.w;
        float k = (c.w * batch.quadOffset.w + 1.0) * q;    // mul.w, add.w, mulq.w
        vec3 w0 = frame.gsOffset.xyz + c.xyz * q;          // mulaw.xyz + maddq.xyz
        vec3 w1 = w0 + quadOffset * k;
        int16 flag = adc(j);                               // fcand vi01, 0x3f + iaddiu

        s.color = p.color;  s.q = q;  s.reg = 1;           // sq.x from p's register; addq.y + sq.yz
        s.uv0  = batch.uv0;          s.uv1  = batch.uv1;   // invariant loads hoisted
        s.xyz0 = ftoi4(w0);          s.adc0 = flag;        // ftoi4.xyz + sq.xyz; isw.w
        s.xyz1 = ftoi4(w1);          s.adc1 = flag;
    }

    xgkick(batch.packet);                 // xtop + 11 + count; placed behind a --barrier after the loop
}
```

**What the loop body compiles to.** It should produce the hand-written instruction mix:

- 16 upper instructions;
- the `div`;
- one `lq`, eight stores, `fcand` and the ADC add.

The compiler then picks the exit test. An end-pointer compare (`ibne pIn, pEnd`) replaces the
countdown's `iaddi`, which saves one lower-pipe instruction per iteration. That is 15 lower against
16 upper, where the hand-written loop has 16 lower.

**What the compiler works out statically:**

- **`packet`'s offset.** It sits after `particles[count]`, at `11 + count` qwords.
- **How many sprites are written.** The open `sprites[]` is zipped with `particles[count]`, so the
  loop writes `count` of them.
- **The limit.** The whole batch, `11 + count + 7 + 5·count`, must fit the 474-qword half, which
  gives `count ≤ 76`.

## Reflection

### `--reflect=prog.json` (schema version 1)

It records:

- The program: its name, symbols, instruction count and source.
- The features it uses: xtop, xitop, clipw, efu, xgkick and yield.
- Each parameter of `main`:
  - its name, qualifier and struct type;
  - its placement (`address`, or `xtop` with VIF BASE/OFFSET);
  - its size in qwords, as a fixed part plus per-length terms, counting the open arrays the
    program writes;
  - the members it reads, writes and kicks, as dotted paths;
  - the solved `limit`.
- Each type, in an array: the file it was declared in, its layout hash, its size and open array,
  and its members. A member has its name, type, qword (a number, or an expression after a
  dependent array), lane, length, stride, matrix order and the VIF `unpack` (format and
  signedness). With the member's qword and its array's stride, that gives the host its UNPACK and
  `STCYCL` settings.

It is written by a small hand-rolled writer, since no crates are allowed.

### `--header=prog.h` (C++17: `<cstddef>`, `<cstdint>`, plus `<tamtypes.h>` on the EE)

- **Value types.** `vusl::qword_t` is the SDK's own when `_EE` is defined (tamtypes.h), and a
  layout-identical union otherwise, for host tools such as asset cookers. `vec2`–`vec4`,
  `ivec2`–`ivec4`, `uvec2`–`uvec4`, `mat3`, `mat4`, `int32`, `uint32` and `int16_lane` mirror the
  VUSL types, so a generated struct reads like its declaration. `element<T>` holds an array
  element of fewer than four lanes, which takes a whole qword.
- **Structs.** One C++ struct per VUSL struct. Lanes the packing skips get explicit
  `_pad` members, and `sizeof` and every `offsetof` are checked with `static_assert`. A struct with
  dynamic arrays gets a struct for its fixed part, plus offset constants and size functions for
  the rest.
- **Shared types.** Types from included files go in `vusl::layout_<hash>` behind include guards
  keyed on a hash of the layout. Two program headers that share `frame.vush` can then be included
  in one translation unit. Types declared in the program's own file go in its namespace.
- **Struct constants.** Offsets that C++ members cannot give, for arrays of run-time length and
  what follows them:
  - `kBatchParticles`, an offset in qwords, and its element `Stride`;
  - `BatchPacket(count)`, an offset after a dependent array;
  - `BatchQwords(count)`, a size.
- **Program constants.** These are `constexpr`, in `namespace vusl::<program>`, one set per
  parameter of `main`:
  - `kFrameAddress`;
  - `kBatchVifBase` / `kBatchVifOffset`;
  - `BatchUsedQwords(count)`, which counts the open arrays `main` writes;
  - the solved limit `kBatchMaxCount`, checked with a `static_assert`;
  - `kInstructions`, once code is generated.
- **Program declaration hook.** An optional `VUSL_DECLARE_PROGRAM(name)`, used when the includer
  defines it (doom3 declares `_CodeStart` symbols with a section attribute).
- **Later option:** `--header-type=vec4:math::Vec4` maps member types to the host's own types,
  guarded by `static_assert`s on size and alignment.

### Example: the reflection for `VU1Prog_Particles`

These are the files `oxide-vusl --check --reflect=… --header=…` writes for the program above.
They are `tests/vusl/check/particles/expected.h` and `expected.json`, whose source includes
`frame.vush` as `../../include/frame.vush`. There is no `kInstructions` yet, because `--check`
generates no code.

`particles.h`:

```cpp
// Generated by oxide-vusl 0.1.0 from input.vusl. Do not edit.
#pragma once

#include <cstddef>
#include <cstdint>
#if defined(_EE)
#include <tamtypes.h>
#endif

// The VUSL value types, shared by every generated header.
#ifndef VUSL_TYPES_V1
#define VUSL_TYPES_V1
namespace vusl
{
#if defined(_EE)
using ::qword_t;
#else
typedef union alignas(16)
{
#if defined(__SIZEOF_INT128__)
    __extension__ unsigned __int128 qw; // a GNU extension: quiet under -pedantic
#endif
    std::uint8_t b[16];
    std::uint16_t hw[8];
    std::uint32_t sw[4];
    std::uint64_t dw[2];
} qword_t; // the layout of tamtypes.h's qword_t, for host tools
#endif

typedef std::int32_t int32;
typedef std::uint32_t uint32;
typedef std::int32_t int16_lane; // an int16 member: the VU reads the low 16 bits, stores them zero-extended

struct vec2 { float x, y; };
struct vec3 { float x, y, z; };
struct alignas(16) vec4 { float x, y, z, w; };
struct ivec2 { int32 x, y; };
struct ivec3 { int32 x, y, z; };
struct alignas(16) ivec4 { int32 x, y, z, w; };
struct uvec2 { uint32 x, y; };
struct uvec3 { uint32 x, y, z; };
struct alignas(16) uvec4 { uint32 x, y, z, w; };
struct alignas(16) mat3 { vec4 q[3]; }; // the stored qwords, xyz of each: columns, or rows when row_major
struct alignas(16) mat4 { vec4 q[4]; }; // the stored qwords: columns, or rows when row_major

// An array element of fewer than four lanes, which takes a qword.
template <typename T>
struct alignas(16) element
{
    T value;
};
} // namespace vusl
#endif

// Frame (../../include/frame.vush), shared by the programs that include it.
#ifndef VUSL_LAYOUT_Frame_941eb3fc
#define VUSL_LAYOUT_Frame_941eb3fc
namespace vusl::layout_941eb3fc
{
struct alignas(16) Frame
{
    mat4 mvp;        // qwords 0-3, row_major
    vec4 gsScale;    // qword 4
    vec4 gsOffset;   // qword 5
    vec3 clipScale;  // qword 6, xyz
    float warpPhase; // qword 6, w
    vec4 colorClamp; // qword 7
};
static_assert(sizeof(Frame) == 8 * 16, "VUSL layout of Frame");
static_assert(offsetof(Frame, gsScale) == 4 * 16, "VUSL layout of Frame");
static_assert(offsetof(Frame, gsOffset) == 5 * 16, "VUSL layout of Frame");
static_assert(offsetof(Frame, clipScale) == 6 * 16, "VUSL layout of Frame");
static_assert(offsetof(Frame, warpPhase) == 6 * 16 + 12, "VUSL layout of Frame");
static_assert(offsetof(Frame, colorClamp) == 7 * 16, "VUSL layout of Frame");
} // namespace vusl::layout_941eb3fc
#endif

namespace vusl::VU1Prog_Particles
{
using Frame = layout_941eb3fc::Frame;

struct alignas(16) Particle
{
    uint32 color; // qword 0, x
    vec3 pos;     // qword 0, yzw
};
static_assert(sizeof(Particle) == 1 * 16, "VUSL layout of Particle");
static_assert(offsetof(Particle, pos) == 4, "VUSL layout of Particle");

struct alignas(16) Sprite
{
    uint32 color;    // qword 0, x
    float q;         // qword 0, y
    int16_lane reg;  // qword 0, z
    uint32 _pad0;    // unused
    vec4 uv0;        // qword 1
    ivec3 xyz0;      // qword 2, xyz
    int16_lane adc0; // qword 2, w
    vec4 uv1;        // qword 3
    ivec3 xyz1;      // qword 4, xyz
    int16_lane adc1; // qword 4, w
};
static_assert(sizeof(Sprite) == 5 * 16, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, q) == 4, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, reg) == 8, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, uv0) == 1 * 16, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, xyz0) == 2 * 16, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, adc0) == 2 * 16 + 12, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, uv1) == 3 * 16, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, xyz1) == 4 * 16, "VUSL layout of Sprite");
static_assert(offsetof(Sprite, adc1) == 4 * 16 + 12, "VUSL layout of Sprite");

struct alignas(16) Packet
{
    qword_t tags[7]; // qwords 0-6
};
static_assert(sizeof(Packet) == 7 * 16, "VUSL layout of Packet");
// Qwords: offsets from the struct's start, and sizes.
constexpr std::uint32_t kPacketSprites = 7; // Packet::sprites
constexpr std::uint32_t kPacketSpritesStride = 5; // qwords per element
constexpr std::uint32_t kPacketQwords = 7; // without its open array

struct alignas(16) Batch
{
    uint32 _pad0[3];  // unused
    int16_lane count; // qword 0, w
    vec4 quadOffset;  // qword 1
    vec4 uv0;         // qword 2
    vec4 uv1;         // qword 3
    qword_t tags[7];  // qwords 4-10
};
static_assert(sizeof(Batch) == 11 * 16, "VUSL layout of Batch");
static_assert(offsetof(Batch, count) == 12, "VUSL layout of Batch");
static_assert(offsetof(Batch, quadOffset) == 1 * 16, "VUSL layout of Batch");
static_assert(offsetof(Batch, uv0) == 2 * 16, "VUSL layout of Batch");
static_assert(offsetof(Batch, uv1) == 3 * 16, "VUSL layout of Batch");
static_assert(offsetof(Batch, tags) == 4 * 16, "VUSL layout of Batch");
// Qwords: offsets from the struct's start, and sizes.
constexpr std::uint32_t kBatchParticles = 11; // Batch::particles
constexpr std::uint32_t kBatchParticlesStride = 1; // qwords per element
constexpr std::uint32_t BatchPacket(std::uint32_t count) { return 11 + count; } // Batch::packet
constexpr std::uint32_t BatchQwords(std::uint32_t count) { return 18 + count; } // without its open array

// main's parameters: qword addresses in VU1 data memory.
constexpr std::uint32_t kFrameAddress = 0; // frame: uniform Frame
// batch: inout Batch, at XTOP in the VIF1 double buffer:
constexpr std::uint32_t kBatchVifBase = 8;
constexpr std::uint32_t kBatchVifOffset = 474;
constexpr std::uint32_t BatchUsedQwords(std::uint32_t count) { return 18 + 6 * count; } // with the open arrays main writes
constexpr std::uint32_t kBatchMaxCount = 76; // the largest count for which the double-buffer half holds it
static_assert(BatchUsedQwords(kBatchMaxCount) <= 474, "VUSL limit");
} // namespace vusl::VU1Prog_Particles

#ifdef VUSL_DECLARE_PROGRAM
VUSL_DECLARE_PROGRAM(VU1Prog_Particles)
#endif
```

On the C++ side, quake2's batch code would then do the following instead of its literal
`static_assert`s:

```cpp
static_assert(sizeof(ParticleVertex) == sizeof(vusl::VU1Prog_Particles::Particle));
static_assert(offsetof(ParticleVertex, x) == offsetof(vusl::VU1Prog_Particles::Particle, pos));
```

It could also use `vusl::VU1Prog_Particles::Particle` directly.

`particles.json` (shortened after the first type):

```json
{
  "vusl": 1,
  "program": {
    "name": "VU1Prog_Particles",
    "source": "input.vusl",
    "symbols": {
      "start": "VU1Prog_Particles_CodeStart",
      "end": "VU1Prog_Particles_CodeEnd"
    },
    "instructions": null
  },
  "uses": ["xtop", "clipw", "xgkick"],
  "parameters": [
    {
      "name": "frame",
      "qualifier": "uniform",
      "type": "Frame",
      "placement": {
        "address": 0
      },
      "qwords": {
        "fixed": 8
      },
      "reads": ["mvp", "gsScale", "clipScale", "gsOffset"],
      "writes": []
    },
    {
      "name": "batch",
      "qualifier": "inout",
      "type": "Batch",
      "placement": {
        "xtop": true,
        "vif_base": 8,
        "vif_offset": 474
      },
      "qwords": {
        "fixed": 18,
        "per": [
          {
            "length": "count",
            "qwords": 6
          }
        ]
      },
      "reads": ["quadOffset", "tags", "count", "particles.pos", "particles.color", "uv0", "uv1"],
      "writes": ["packet.tags", "packet.sprites.color", "packet.sprites.q", "packet.sprites.reg", "packet.sprites.uv0", "packet.sprites.uv1", "packet.sprites.xyz0", "packet.sprites.adc0", "packet.sprites.xyz1", "packet.sprites.adc1"],
      "kicks": ["packet"],
      "limit": {
        "length": "count",
        "max": 76,
        "capacity": 474
      }
    }
  ],
  "types": [
    {
      "name": "Frame",
      "file": "../../include/frame.vush",
      "layout": "941eb3fc",
      "qwords": {
        "fixed": 8
      },
      "members": [
        {
          "name": "mvp",
          "type": "mat4",
          "qword": 0,
          "order": "row_major"
        },
        {
          "name": "gsScale",
          "type": "vec4",
          "qword": 4,
          "lane": "x"
        },
        {
          "name": "gsOffset",
          "type": "vec4",
          "qword": 5,
          "lane": "x"
        },
        {
          "name": "clipScale",
          "type": "vec3",
          "qword": 6,
          "lane": "x"
        },
        {
          "name": "warpPhase",
          "type": "float",
          "qword": 6,
          "lane": "w"
        },
        {
          "name": "colorClamp",
          "type": "vec4",
          "qword": 7,
          "lane": "x"
        }
      ]
    },
    "…"
  ]
}
```

## The compiler

### Crates (workspace members; no external dependencies)

```
crates/vusl/src/
  lib.rs, options.rs        preprocess(), check() -> Checked, Checked::generate() -> Vcl,
                            Checked::reflection(), assemble() (vcl::compile on the generated VCL),
                            compile() (generate and assemble, with fewer hoisted invariants when
                            the registers do not fit)
  diagnostics.rs            VuslMessage enum + wording (no printing)
  syntax/                   lexer.rs (on preprocessed text; #pragma lines become tokens), ast.rs,
                            parser.rs (recursive descent, C precedence), print.rs (the tree as text)
  sema/                     types.rs (types, lanes, placements), layout.rs (VU packing),
                            check.rs (declarations, main's bindings), consteval.rs,
                            elaborate.rs (main with every call inlined: types, placements, the
                            strict rules), hir.rs (the checked program)
  codegen/                  mod.rs (the checked program lowered to VCL), values.rs (what registers
                            hold: reuse, invariants, alias regions), peephole.rs (passes over the
                            instructions), emit.rs (the instructions, their registers and labels,
                            a source place per line)
  reflect/                  model.rs, json.rs, cpp.rs
  interp.rs                 the reference interpreter of the checked program (the test oracle)
crates/oxide-vusl/src/      main.rs, cli.rs (lib target vusl_cli), help.txt; build.rs (build date)
```

### Pipeline

1. **Preprocess.** `vcl::preprocessor::preprocess` in its C-syntax mode, with `__VUSL__` defined.
2. **Parse, then check.** Name resolution, types and placements, the strict rules, constant
   evaluation, layout, and `#pragma unroll`'s constant-count loops repeated.
3. **Reflect.** This needs only the checked program, so `--check` writes it too.
4. **Lower to VCL**, destination-driven, from the checked program. There is no separate IR: the
   optimizations below work while lowering, through a record of what each register holds, and on
   the lowered instructions after it. The VCL has:
   - `.syntax new`, `.name`, `.init_vf_all`, `.init_vi_all`, empty `--enter`/`--exit`;
   - `fcset 0` when clipping is used, and XTOP read once into a register;
   - loop hints, and `.vu_work_area` from a `spill` parameter.

   Generated names are `f`/`i`, the source name and a number, which keeps them off hardware
   names and field letters.
5. **Compile with oxide-vcl.** `vcl::compile` with `Mode::Oxide` (all fixes and optimizations) and
   `-u` set to the program name; `--mode=vcl` is available for cross-checks. A vcl error on the
   generated code is reported at the VUSL line through the line origins.

### Optimizations the frontend owns

Lowering records what each register holds (`codegen/values.rs`), and a peephole pass runs over
the instructions it produces (`codegen/peephole.rs`). Each optimization is justified by an idiom in
the hand-written programs:

1. **Folding.** Constant expressions fold in the checker, which also repeats `#pragma unroll`'s
   constant-count loops.
2. **Value reuse.** A register's contents are recorded as one of: a whole qword loaded with `lq`;
   an `int16` loaded with `ilw`; a sum of two VI registers; or a pure expression's value, by its
   text. A second use takes the register. A record ends at:
   - a store that may change its memory. Distinct parameters never alias unless one says
     `alias =`, distinct members never do, and the same base register with different offsets
     never does. A member that takes lanes stands for its qword;
   - a write to a local its expression reads, or to the register holding it;
   - a label where paths that did not compute it join;
   - a `yield`, after which the host may have written memory and sent another batch: XTOP and
     XITOP are read again.
3. **Loop shaping:**
   - a range-for is a do-while behind a guard on its count;
   - there is one pointer per element stride and base, stepping by the stride, and elements of
     arrays a constant apart share it with different offsets;
   - the loop ends when the pointer with the smallest stride reaches its end (`ibne`), so no
     counter steps.
4. **LICM.** Before a loop, the largest pure parts of its body that read nothing the loop changes
   and cost an instruction are lowered once: loads, constants and products of invariants. The loop
   then finds them in registers. A part inside a branch is taken only when both arms compute it.
   Parts inside nested loops count too. VI values leave only loops of one block: in a loop with
   branches they would hold VI registers its own counters and pointers need. At most 16 values
   are kept; when vcl runs out of registers or spills, the program is generated again with 6, then
   none, and the compile with the fewest spills is kept (`vusl::compile`).
5. **Memory:**
   - one `lq` of all four lanes serves every lane read from a qword, and VCL narrows its mask;
   - qword assembly: consecutive stores to lanes of one qword become one masked `sq` when their
     values can be made in one register's lanes with fewer extra moves than the stores saved.
6. **Contraction.** A sum of two or more products is one ACC chain. n products and k other terms
   take n + k instructions instead of 2n + k − 1.
7. **Units:**
   - Q: after a division or root, its readers take Q directly (`mulq`, `addq`, `subq`, and
     `addq`/`mulq` into a lane) until the next FDIV operation or label. Lowering's copy of Q is
     dropped when nothing reads it;
   - I literals: `loi` and an `addi`/`muli` into the lane wanted, hoisted out of loops;
   - VF00 for 0 and 1;
   - raw moves: a copy of raw bits, or of a float whose bits are read, that the peephole leaves
     becomes a reload from memory or four `mr32`s. vcl may schedule a MOVE in the upper pipe as
     MAX of the source with itself, which flushes bit patterns that read as denormals.
8. **Lanes.** A scalar whose instruction can put it in any lane (Q and P copies, constants,
   `mfir`, negation) is made in the lane its store or component write wants.
9. **Peephole.** A temporary written by one instruction and then moved whole into a register is
   written there directly. A copy whose destination is only read after it, while its source keeps
   its value on every path between, is replaced by its source; an address made as a base plus a
   constant and only used in memory operands, under the same condition, becomes those operands'
   offsets. A branch back into that stretch (a loop around it) extends it to the branch, so a
   loop that changes the source keeps the copy.

Scheduling, pairing, register allocation, field narrowing and dead code are left to oxide-vcl.

### Instruction selection

Each idiom has a case in `tests/vusl/codegen`, which names the hand-written sequence it stands for:

| Case | Idiom |
|---|---|
| `transform` | Matrix products: the w=1 term as VF00's w, and a 3×3 product |
| `perspective` | The perspective divide and viewport: `div`, `mulq`, `ftoi4` |
| `dot-normalize` | A dot product, and normalize with `rsqrt` and `mulq` |
| `clip-adc` | The guard-band judgement (`mul` + `clipw`) and ADC (`fcand` + `iaddiu 0x7fff`) |
| `color-pack` | A colour clamped and packed: `max`, `mini`, `ftoi0` |
| `q-into-lane` | Q into a lane (`addq.y`), and a constant into a lane |
| `lane-moves` | `mr32` rotations and masked broadcast adds |
| `int-constants` | `int16` immediates, and 0x8000 in two instructions |
| `gif-tag` | The `isw.x` NLOOP/EOP patch |
| `loop-invariants` | Invariants hoisted, and the end-pointer exit test |
| `shared-pointers` | Two arrays stepped by one pointer |
| `multiply-add` | A sum of products as one ACC chain |
| `qword-assembly` | Lane stores built in one register and stored once |
| `load-reuse` | One `lq` for every lane of a qword, again after a store, and raw bits copied |
| `particles` | The particles port: 16 upper and 16 lower instructions in its loop, where the hand-written loop has 19 and 17 |

### Diagnostics

- `VuslMessage` lives in `vusl/src/diagnostics.rs` with its wording.
- It converts to a new `vcl::Message::Frontend { text, subject, help }` variant, so the existing
  `Reporter` prints it with locations, notes, colour and quotes.
- Strict-rule errors carry the placement (`vec2@zw`) and the suggested explicit form.

### Driver: `oxide-vusl`

```
oxide-vusl [options] input.vusl
  -o FILE              the .vsm listing (stdout by default)
  --emit-vcl=FILE      also write the generated VCL; --stop-at-vcl writes only that
  --reflect=FILE       reflection JSON          --header=FILE   C++ header
  -I DIR, -D NAME[=V]  preprocessor             --preprocess-only
  --check              check the program and stop
  --annotate           source comments in the VCL
  --mode=oxide|vcl     back-end mode (oxide by default)   --vcl-option=OPT  pass an oxide-vcl option
  --color=, --warnings-as-errors, --help, --version
```

On failure, nothing is written: all files or none, like oxide mode's `write_all_or_nothing`.

### Changes to the `vcl` crate (small, on the branch)

- **Preprocessor:** `PreprocessOptions::c_syntax` (`docs/PREPROCESSOR.md`, "C syntax (VUSL)").
  Under it:
  - `;` is not a comment (`lexer::Syntax::C`);
  - `#pragma` lines other than `once` are written to the output with their arguments expanded,
    instead of being warned about and dropped;
  - a name followed by `{` is code unless it names a block macro;
  - `#vuprog` is an unknown directive, and its absence is not warned about.
  - `MemoryFiles` is an in-memory `SourceLoader`, for tests and for callers that have the text.
- **Diagnostics:**
  - `Message::Frontend(FrontendMessage { text, subject, help, in_source })`, wired through
    `message_from!` and `wording`. `in_source` says whether a diagnostic with only a line is
    located in the program, through its line origins, or by the driver's name.
  - `Reporter::set_program_name`, so the place of a file-less diagnostic and the summary name
    `oxide-vusl`.
- **`vcl::driver`:** what both drivers share. It holds `Color` (`--color=`) with
  `enabled_here()`, and `write_all_or_nothing`. Both moved there from `oxide-vcl`'s driver.

## Testing

Data goes under `tests/vusl/`. Runners live in `crates/vusl/tests/` (library level) and
`crates/oxide-vusl/tests/` (through the driver); `docs/VUSL_COMPILER.md` lists them.
`VUSL_UPDATE=1` regenerates the expected files. They include `vu_model.rs` and `scenes.rs` by
path, as the oxide-vcl tests do.

| Suite | What it checks |
|---|---|
| Unit tests | Lexer, parser round trip, placements, layout, lane solver |
| `check/<case>/` | `input.vusl` plus `expected.stderr`: every strict rule, type and layout error, and the warnings. Cases with `expected.json` and `expected.h` are the reflection suite; when a C++ compiler is present, all their headers are compiled together with `-std=c++17 -fsyntax-only -pedantic -Werror` |
| `codegen/<case>/` | `expected.vcl`, the `--stop-at-vcl` output, one per idiom (`crates/oxide-vusl/tests/codegen.rs`). Each also compiles through vcl. The instruction counts must not grow past the hand-written sequence the case cites |
| `crates/vusl/tests/semantics.rs` | Differential test: programs compiled through oxide-vcl run in `vu_model`'s `Config::TIMED` (VCL's schedules rely on Q, P and the flags landing at their latency, which the logical model does not keep), on seeded memories of exactly representable values. Their data memory and kick addresses must be the interpreter's. The particles program runs on a crafted batch. Later, a small random program generator |
| `programs/<case>/` | Ports of the four scene programs (GPL-3.0 and GPL-2.0 sources, with licence notes like the corpus). In `Config::TIMED` on every scene: packets identical to the hand-written program's and cycles ≤ its cycles, both compiled with `--mode=oxide --opt=all`, and the port's data memory and kicks the interpreter's. An ignored report test prints the table in `docs/VUSL_PERFORMANCE.md` |

## Milestones (each ends with fmt, clippy, the full release suite and the public corpus 52/52)

0. **Branch and groundwork.**
   - `git switch -c vusl`.
   - The `vcl` crate changes above, with tests: preprocessor C syntax and pragma pass-through,
     `Message::Frontend`, and the reporter's program name.
   - The workspace members, crate skeletons, and a driver that preprocesses and reports, with
     `--help`/`--version`.
1. **Front end.** Lexer, parser and AST; checking with placements, the strict rules and layout;
   the `errors` suite.
2. **Reflection.** JSON and the C++ header from the checked tree; the `reflect` suite.
3. **End-to-end baseline.**
   - IR build, the interpreter, direct instruction selection, VCL emission, and `vcl::compile`
     with line origins.
   - Straight-line code, `if`, and index and range loops.
   - The `semantics` suite starts here.
4. **Optimizer.** Passes 1–9 above, with the `codegen` idiom suite.
5. **VU features.**
   - clip flags, `adc`, `xgkick`;
   - `ref<T>`, dependent and open arrays, `xitop`;
   - `yield`, the pragmas, `spill`, EFU, float compares.
6. **Ports and benchmarks.** quake2 particles, then doom3 world_strips, doom3 world_lm, then quake2
   textured_triangles. Iterate the optimizer until every scene matches or beats the hand-written
   program. Results go in `docs/VUSL_PERFORMANCE.md`.
7. **Docs.**
   - `docs/VUSL.md`: the user reference, with the language, the cost table of every construct,
     pragmas, packing, reflection and the driver.
   - `docs/VUSL_COMPILER.md`: the implementer's view, covering phases, IR, the lane solver, the
     idiom catalogue and the tests.
   - The README index and the CLAUDE.md layout, on the branch.

Commits go to `vusl`, not `main`, at the user's request. There is no push until asked. Tracked text
follows `.claude/rules/tracked-text.md`.

## Verification

- `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and
  `cargo test --release`, which includes every VUSL suite and the existing oxide-vcl suites.
- `python3 tools/corpus_status.py --candidate target/release/oxide-vcl` still gives 52/52. The vcl
  crate changes are opt-in, so the compiler is untouched.
- **End to end:**

  ```
  oxide-vusl -I tests/vusl/programs/include --reflect=p.json --header=p.h --emit-vcl=p.vcl particles.vusl -o p.vsm
  ```

  Then check:
  - `dvp-as p.vsm` assembles;
  - the scene test reports identical packets and no more cycles than the hand-written particles
    program;
  - `p.h` compiles under `c++ -fsyntax-only`.
- **Cross-check:** for programs without oxide-only features, the generated VCL compiled with
  `--mode=vcl` gives the same listing as the original vcl 1.4beta7 under the oracle
  (`run-original` skill). This is local only.
