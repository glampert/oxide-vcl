# VUSL reference

VUSL (Vector Unit Shading Language) is a GLSL-like language for PlayStation 2 VU1
microprograms. `oxide-vusl` compiles a VUSL program to VCL and hands it to oxide-vcl in the
same process, which schedules, pairs and allocates it into a `.vsm` listing for `dvp-as`. It can
also write the generated VCL, and a JSON and a C++ description of the program's memory for the
host code that feeds it.

The language follows one rule: **no hidden instructions**. Every construct has a fixed lowering,
listed in the [cost table](#cost-table), and an operation the VU has no instruction for is an
error that names the explicit form. The compiler only removes or fuses instructions that the
source accounts for.

This document is for people writing VUSL programs. `docs/VUSL_COMPILER.md` describes how the
compiler works, and `docs/VUSL_PERFORMANCE.md` compares the ports in `tests/vusl/programs` with
the hand-written programs they port. The examples here are complete programs unless they say
otherwise, and each compiles with `oxide-vusl example.vusl`
(`crates/vusl/tests/reference.rs` checks that).

## A first program

```glsl
// Scales every vector in a batch, in place.
struct Batch
{
    layout(lane = w) int16 count;   // the header qword's w
    vec4 scale;
    vec4 values[count];
};

void main(layout(xtop, vif_base = 0, vif_offset = 512) inout Batch batch)
{
    for (inout vec4 value : batch.values)
    {
        value = value * batch.scale;
    }
}
```

- **Memory** is described by plain structs. `main`'s parameters bind them to VU1 data memory:
  `batch` is the double-buffer half the host filled, found through XTOP.
- **The range-for** walks `values`, whose length is the `count` member. `inout` writes each element
  back.
- **The loop** becomes a guarded do-while over a pointer. `batch.scale` is loaded once before it,
  so each iteration is an `lq`, a `mul` and an `sq`.

`oxide-vusl -o scale.vsm scale.vusl` writes the listing. `--emit-vcl=scale.vcl` also writes the
generated VCL. `--header=scale.h` writes a C++ header with `Batch`'s layout and the largest `count`
that fits the 512-qword half.

## Program structure

A file holds, at file scope:

- **Types:** `struct` declarations, [enums](#enums) (`enum class`) and
  [type aliases](#type-aliases) (`type Name = Type;`).
- **Constants:** `const int16 kName = 45;`, `const float kHalf = 0.5;` and vectors of constants,
  `const vec4 kScale = vec4(1.0, 2.0, kHalf, 4.0);` or `vec4(0.5)` for all four. A vector constant
  costs what its constructor written out would at each use. A `const` in a function may be any of
  these too. Matrices and structs cannot be constants: build them in a variable.
- **Functions:** C-like definitions, with `in` (the default), `out` and `inout` parameters. Every
  call is inlined, so a function costs nothing and cannot recurse, except a call of a
  [library](#libraries)'s export. A function that is never called is not checked, and there is a
  warning about it.
- **One `main`:** `void main(...)`. Its parameters are the program's memory (see
  [main's parameters](#mains-parameters)).
- **Pragmas** (see [pragmas](#pragmas)).

There are no global variables. Data reaches the program through `main`'s parameters, and
functions see only their own parameters and, in a library, the library's memory.

The text goes through the C-syntax preprocessor first (`docs/PREPROCESSOR.md`, "C syntax
(VUSL)"): `#include`, `#define` with arguments, `#if`, `#pragma once`. `__VUSL__` is defined as 1.
Include files conventionally end in `.vush`.

A run of the program is one MSCAL: `main` starts at the beginning and the program ends with
`return;` or at the end of `main`. `yield;` stops it until MSCNT (see [statements](#statements)).
The program's name is what `#pragma program(Name)` says, or else the file's stem with anything
but letters, digits and `_` made `_`. The listing exports `Name_CodeStart` and `Name_CodeEnd`.

## Types

| Type | Lives in | Notes |
|---|---|---|
| `float`, `vec2`, `vec3`, `vec4` | VF lanes | Every value has a **placement**, the lanes that hold its components, printed `vec2@zw` in messages |
| `mat3`, `mat4` | 3 or 4 qwords | Columns, as in GLSL. `layout(row_major)` stores rows. `m[i]` is the i-th stored qword, a row when row-major, and `i` must be constant. A local matrix takes its initializer's storage. `mat4(c0, c1, c2, c3)` and `mat3(c0, c1, c2)` build one from columns; `mat3(m)` of a `mat4` is its first three stored qwords' xyz, the upper-left 3x3 in `m`'s order |
| `int16` | VI registers | 16 bits, wrapping, with signed compares. In memory it takes a 32-bit lane: `ilw` reads the low 16 bits and `isw` stores them zero-extended. There is no `int` |
| `bool` | VI, or a flag test | From comparisons, `any()`, `\|\|`, `&&` and `!` |
| `int32`, `uint32`, `ivec2`–`ivec4`, `uvec2`–`uvec4` | VF lanes, as raw bits | Unpacked integers, `ftoi` results, packed colours. They are moved, stored and converted, never computed with: an FMAC operation would flush them as denormal floats |
| `qword` | 128 opaque bits | GIF tags, A+D data. `.x` to `.w` read and write as `int16` |
| `clipflags` | one CLIPW judgement | Made by `clipw()`, read by `any()`, `adc()`, `clipped()` and `clipbits()`. It can be copied, returned, and passed as an `in` or `out` parameter, and always names the `clipw` it came from |
| `ref<T>` | a VI qword address | Explicit addressing into memory (see [refs](#refs)) |
| A `struct` | memory, or one register per member | In memory as a parameter (see [memory](#memory)), or a variable (see [struct variables](#struct-variables)) |
| An `enum class` | VI, as an `int16` | Named values that compare only with their own type (see [enums](#enums)) |

### Struct variables

A variable of a struct type keeps each member in registers of its own, in the lanes the struct
gives it in memory, so storing a member moves nothing and every path leaves it in the same lanes:

```glsl
struct Vertex
{
    vec4 position;
    layout(lane = w) uint32 color;
};

Vertex transformed(Vertex vertex, mat4 by)
{
    return Vertex(by * vertex.position, vertex.color);
}

Vertex vertex = transformed(batch.source, batch.transform);
vertex.position.w = 1.0;
batch.target = vertex;
```

- `Vertex(...)` takes one value per member, in order. A struct member takes a struct value.
- Reading a struct from memory loads each member; storing one stores each. Assigning a struct
  value sets each member. A member's value that reads a member already set in that assignment is
  computed first, so `pair = Pair(pair.second, pair.first)` swaps; one that reads memory a store
  may overwrite is read before the first store.
- Functions take and return struct variables. An `out` or `inout` parameter is the caller's
  variable; an `in` parameter is too unless the body or another argument changes it, which makes a
  copy.
- A struct with an array member cannot be a variable, a library export cannot take or return one,
  and a struct value is used through its members: there is no struct arithmetic or comparison.

### Enums

`enum class` declares a type of named `int16` values, as C++'s scoped enums do:

```glsl
enum class ClipPlane { Near, PositiveX, NegativeX, PositiveY, NegativeY };
enum class Blend : int16 { Opaque = -1, Add = 2, Multiply }; // Multiply is 3

float planeDistance(vec4 pos, ClipPlane plane)
{
    if (plane == ClipPlane::Near)
        return pos.w - pos.z;
    ...
}

float d = planeDistance(pos, ClipPlane::Near);
```

- A value is named with its enum, `ClipPlane::Near`. Each is one more than the one before, from 0,
  unless it is given: an integer constant, which may name the enum's earlier values, and must fit
  an `int16`. The underlying type is always `int16`; `: int16` may say so.
- An enum's value compares (`==`, `!=`, `<`, `<=`, `>`, `>=`) only with a value of the same enum.
  It does not convert to or from an `int16`, an integer constant or another enum by itself: the
  constructors convert, `ClipPlane(n)` from an `int16`, an integer constant or another enum, and
  `int16(plane)` back. There is no arithmetic on an enum; convert to `int16` for it. A condition
  needs a comparison, not an enum.
- Enums are file-scope types. They can be variables, constants (`const ClipPlane kFirst =
  ClipPlane::Near;`), parameters, struct members in registers or in memory, and array elements.
  In memory a member takes one lane, as an `int16` does.
- An enum costs what an `int16` does: a variable is a VI register, a comparison an `int16`
  comparison, and a conversion nothing. A constant enum argument makes the callee's comparisons
  fold, as an `int16` constant does.
- [Reflection](#reflection) gives each enum a struct member holds: an `enum class` of the lane's
  size in the header, and its values in the JSON.

### Type aliases

`type Name = Type;` at file scope names a type again, as Rust's `type` does: any type a declaration
can name, built-in or the file's, `ref<T>` included.

```glsl
type Color = vec4;
type Cursor = ref<Corner>;

Color tint = Color(1.0);          // a vec4, built with vec4's constructor
Cursor walk = Cursor(clip.a);     // ref<Corner>(clip.a)
```

An alias is the type itself, not a new one: a `Color` is a `vec4` everywhere, in messages and in the
reflection too. Its constructor is the type's. An alias cannot take a built-in type's name, and
`type` is a name like any other outside such a declaration (a member may be called `type`).

### Lanes

VU instructions work lane by lane: a `vec2` in lanes z and w cannot be added to one in x and y
without first moving it. VUSL makes lane moves explicit.

- **Loads keep memory's lanes.** A `vec2` member packed in lanes zw is loaded as `vec2@zw`.
- **Swizzles move nothing.** `v.zw` of a `vec4` is in lanes z and w; `v.yx` is in y and x.
- **Element-wise operations need equal placements,** except with a scalar, which any instruction
  broadcasts (`ft[x]`). Commutative operations swap their operands for that.
- **A plain local takes the lanes of what is assigned to it.** Where paths join, after an `if` or
  around a loop, they must agree.
- **Lanes move only in an assignment into fixed lanes:** a member in memory, components of a
  variable (`st.xy = uv`), or a local declared with `layout(lane = L)`, whose components then sit
  in consecutive lanes from L. When the lanes rotate in fewer `mr32` steps than there are lanes
  to move, the move is those `mr32`s; otherwise it is one broadcast operation per moved lane. Raw
  bits move only by rotation.
- **Scalars go where they are used.** A scalar whose instruction can write any lane is made in the
  lane its store or component write wants: `st.z = q` after a division is a single
  `addq.z st, vf00, q`.
- **Constructors are free when only broadcast.** In `m * vec4(p.pos, 1.0)` the product reads
  `p`'s lanes and VF00's w directly. Otherwise a constructor costs one instruction per part not
  already in place.

```glsl
struct Block
{
    vec2 uv;
    vec2 st;
    vec4 out0;
};

void main(layout(address = 0) inout Block block)
{
    // uv is in lanes xy and st in lanes zw: move st to xy first.
    layout(lane = x) vec2 moved = block.st;
    block.out0.xy = block.uv + moved;
}
```

## Memory

### Structs and VU packing

Memory is described with ordinary structs. The members are packed as follows:

- In order, a scalar takes one lane and a `vecN` takes N consecutive lanes.
- A member that does not fit in what is left of the current qword starts the next one.
- `vec4`, `qword`, matrices, structs and arrays start on a qword, and their stride rounds up to whole
  qwords. An array of `vec2` takes a qword per element.
- `layout(offset = q)` puts a member at qword `q` of its struct, and `layout(lane = y)` at lane y of
  the current qword, or of the next when the current one is past y: `layout(lane = y) vec3 pos`
  takes yzw.
- `layout(unpack = v4_16)`, with `unsigned` for zero extension, records the VIF UNPACK format for
  reflection. The formats are `s_32`, `s_16`, `s_8`, `v2_32`, `v2_16`, `v2_8`, `v3_32`, `v3_16`,
  `v3_8`, `v4_32`, `v4_16`, `v4_8` and `v4_5`. The format's component count must fit the member,
  and 8- and 16-bit formats need integer members.

### Arrays

| Form | Length |
|---|---|
| `T a[7]` | A constant |
| `T a[count]` | An `int16` member declared earlier in the same struct, or a `layout(xitop)` parameter of `main` |
| `T a[]` | Open: as many elements as the program writes. Only as the last member; a struct ending in one may only be the last member of its parent |

Members after an array of run-time length get run-time offsets, computed with adds. Since the VU
has no multiply, the element stride should be small. A member there may take a fixed qword with
`layout(offset = q)` instead; the arrays before it must then end by `q`, and reflection reports
that as a limit on their lengths.

Each struct's size is a fixed part plus a term per run-time length. An open array's length comes
from the loop that writes it: zipped in a range-for with `particles[count]`, it holds `count`
elements.

### main's parameters

Each parameter of `main` is a struct bound to data memory. Its qualifier gives the access and its
`layout` the placement:

| Qualifier | Access |
|---|---|
| `uniform` | Constants the host writes; read only |
| `in` | Read only, typically per batch |
| `out` | Write only |
| `inout` | Read and written: in-place transforms, output built in the input buffer, state kept between runs |

| Placement | Meaning |
|---|---|
| `layout(address = N)` | Qword `N`, 0 to 1023 |
| `layout(xtop, vif_base = B, vif_offset = O)` | The double-buffer half XTOP points to. `vif_base` and `vif_offset` are the VIF1 BASE and OFFSET the host sets; they are optional, and with them the struct's largest size must fit in `O` qwords |
| `layout(xitop) int16 n` | Not memory: the count the host sent with VIF ITOP, read with `xitop` |
| `layout(address = N, spill)` | On an `inout` struct: the region becomes oxide-vcl's `.vu_work_area`, where values that do not fit in registers go. The program cannot use it |
| `alias = other` | Allows this parameter's memory to overlap `other`'s, an earlier parameter |

Every parameter needs a placement, and parameters whose memory overlaps are an error without
`alias`. With `alias`, stores through either may change what the other reads.

A member of a parameter passes to functions by reference, never copied: a function parameter of a
struct type given memory is that memory, and its qualifier must allow what the function does with
it. Reflection
records which members the program reads, writes and kicks, so the host learns that a member is
output without any syntax for it.

### Refs

`ref<T>` is an address in a VI register, for code that walks memory itself: output cursors, the
clipper's ping-pong buffers, windows that roll over.

| Form | Meaning |
|---|---|
| `ref<T>(m)` | The address of memory `m`: a `T`, an array of them, or other memory a `T` fits in, viewed under `T`'s layout. An array, or memory reached through a ref, is a run that any `T` fits |
| `ref<T>(m, a)` | The `int16` address `a`, within `m` |
| `r.member`, `r[i]` | Memory through the ref; it passes to functions and `xgkick` as memory |
| `r + n`, `r += n` | `n * sizeof(T)` qwords on; `n` must be constant unless `sizeof(T)` is 1 |
| `r - s` | The distance in elements, under the same rule |
| `r < s`, `r == s`, ... | Address compares |
| `int16(r)` | The address |

A ref stays within the memory it was first set from: the parameter member or array named in its
first `ref<T>(...)`. That memory is its alias region, so stores through it only forget values read
from that region. Setting it later from other memory is an error.

```glsl
struct Out { vec4 v; };

struct Block
{
    layout(lane = w) int16 count;
    vec4 values[count];
    Out kept[];
};

void main(layout(xtop) inout Block block)
{
    // Copy the vectors with a positive x to the end; the header's count becomes theirs.
    ref<Out> cursor = ref<Out>(block.kept);
    for (vec4 value : block.values)
    {
        if (value.x > 0.0)
        {
            cursor.v = value;
            cursor += 1;
        }
    }
    block.count = cursor - ref<Out>(block.kept);
}
```

## Statements

The GLSL statement set: declarations, assignments with `=` and the compound operators, `++` and
`--`, `if`/`else`, `while`, `do`/`while`, `for`, `break`, `continue`, `return` and blocks. Also:

- **Range-for.** `for (Particle p : batch.particles, out Sprite s : batch.packet.sprites)` zips
  arrays in memory, each element an `in` (the default), `inout` or `out` view of the array's
  element. The first range's length is the trip count, and an open array cannot be first. The loop
  is a do-while behind a guard on the count. Its elements are reached through pointers that step by
  their stride; arrays with the same stride and base share one pointer at different offsets. The
  loop ends when a pointer reaches its end, so no counter steps.
- **`for` and `while` loops** are rotated the same way: the test before the loop, then a
  do-while with the test at its end.
- **`break` and `continue`** split the loop body into blocks, so vcl no longer pipelines it.
- **`switch`** on an `int16` or an [enum](#enums), with C's `case` and `default` labels:

  ```glsl
  switch (plane)
  {
  case ClipPlane::Near:
      d = d - jp.z;
      break;
  case ClipPlane::PositiveX:
  case ClipPlane::NegativeX:     // stacked labels share the statements
      d = d - jp.x;
      break;
  default:
      d = d + jp.y;
  }
  ```

  Each case's value is a constant (an integer, or a value of the switch's enum), and no two are
  the same. A case's statements end with `break`, or `continue` or `return;` (`main`), unless it
  is the last case: VUSL has no implicit fall-through, so labels share statements only when nothing
  is between them. `break` is not allowed anywhere else in a case: it would end the case early.
  `continue` reaches the loop around the switch.

  A switch is an `if` and `else` chain: the value, read once, compared with each case's labels in
  order, and `default` last wherever it is written. A constant value, such as an enum argument of
  an inlined call, leaves only its case.
- **`yield;`** ends this run with `--cont`; MSCNT resumes after it with every register as it was.
  The host sends another batch in between, so afterwards XTOP and the `layout(xitop)` parameters
  are read again and no value read from memory is kept. A yield inside a range-for is an error:
  its elements are in the batch it stopped on.
- **`return;`** in `main` ends the program. In a function, `return value;` must be its last
  statement.
- **`xgkick(m);`** kicks the GIF packet at memory `m`, a struct or member in memory or a ref.
- **Copies between memory.** `batch.packet.tags = batch.tags;` copies whole qwords, one `lq` and one
  `sq` per qword. The size must be fixed: an array whose length is read at run time is copied in a
  range-for.

### Conditions

| Condition | Lowering |
|---|---|
| `int16` compared with 0 | `ib**` |
| `int16` compared with a constant | `iaddiu` or `isubiu` of the constant, then `ib**` |
| `int16` compared with `int16` | `isub`, then `ib**` |
| `float` compared with `float` | `sub` in one lane, `fsand` of S or of S and Z (Z for `==` and `!=`), then `ibne`/`ibeq` |
| `any(j...)` | `fcand`, then `ibne` on VI01 |
| `a \|\| b` | Short-circuit branches |
| `a && b`, `!a` | Branches |

A `bool` value outside a condition is a VI register, true when not 0: a float comparison is its
`fsand` result, `any()` FCAND's VI01, `||` an `ior`, an `int16` `!=` an `isub`, and the others are
set to 0 or 1 around a branch.

## Built-in functions

**Math:**

| Built-in | Lowering |
|---|---|
| `M * v` | `mula`, `madda`, then `madd`: one per column. A constant 0 component drops its term; a 1 uses VF00's w |
| `dot(a, b)` | `mul`, then one broadcast `add` per further lane |
| `cross(a, b)` | `opmula`, `opmsub`. Operands in xyz |
| `normalize(v)`, `length(v)` | A dot product, then `rsqrt` or `sqrt`, then a Q form for `normalize` |
| `x / s`, `1.0 / s` | `div`, then the Q form of the consumer (`mulq`, `addq`) |
| `sqrt(s)`, `rsqrt(s)` | `sqrt`; `rsqrt` of VF00's w |
| `min`, `max`, `abs` | `mini`, `max`, `abs` |
| `clamp(v, low, high)` | `max`, then `mini` |

**Conversions and moves:**

| Built-in | Lowering |
|---|---|
| `ftoi0`, `ftoi4`, `ftoi12`, `ftoi15` | The instruction: float to fixed-point bits |
| `itof0`, `itof4`, `itof12`, `itof15` | The instruction |
| `ivec4(v)`, `vec4(iv)` | `ftoi0`, `itof0` |
| `int16(f)` | `ftoi0`, `mtir` |
| `int16(iv.x)` | `mtir` of a raw lane's low 16 bits |
| `float(i)` | `mfir`, `itof0` |
| `uint32(i)`, `int32(i)` | `mfir`: the `int16` sign-extended into a lane |
| `uint32(iv)` and other raw-to-raw types | Nothing |
| `mr32(v)` | `mr32`: x takes y's lane, y z's, z w's, w x's. Four lanes in xyzw |
| `floatBitsToUint`, `floatBitsToInt`, `uintBitsToFloat`, `intBitsToFloat` | Nothing: the same register's bits under the other type. A float whose bits are read this way keeps them through copies |

**Clip flags:**

| Built-in | Lowering |
|---|---|
| `clipw(judge, w)` | `clipw.xyz`: `judge` a `vec3` in xyz, `w` a float |
| `any(j1, j2, j3)` | `fcand` of the judgements' bits: true when any is outside |
| `adc(j...)` | `fcand`, then `iaddiu t, vi01, 0x7fff`: an `int16` with bit 15 set when any is outside, the GS's ADC bit. Its other bits are unspecified |
| `clipped(j...)` | `fcand`: VI01 itself, 1 when any is outside, else 0 |
| `clipbits(j)` | `fcget`, then `iand` with 0x3f: an `int16` of the newest judgement's six flags, bit 0 +x, 1 −x, 2 +y, 3 −y, 4 +z, 5 −z. Corners whose flags share a bit are all outside that plane, so a triangle with `(fa & fb & fc) != 0` is wholly outside |

A function may judge a corner and return the judgement, as in
`clipflags judge(vec4 clip) { return clipw(clip.xyz, clip.w); }`; a call is inlined, so its result is
that `clipw`'s judgement and costs nothing more.

The clip flags hold the last four judgements, so these read one to three judgements within four
of the newest `clipw`; `clipbits()` reads only the newest, since FCGET returns the two newest
judgements' twelve bits and VU1 has no shift to bring the older one's down. The newest must be in
the reading block, where VCL waits for its flags; the older ones may be in earlier blocks when no
branch between judges. Anything else is an error. The program begins with `fcset 0` when it
judges.

**EFU** (VU1 only; the result lands in P and is read through `mfp`):

| Built-in | Result |
|---|---|
| `esadd(v)`, `ersadd(v)` | x² + y² + z², and its reciprocal; `v` a `vec3` in xyz |
| `eleng(v)`, `erleng(v)` | √(x² + y² + z²), and its reciprocal |
| `esum(v)` | x + y + z + w of a `vec4` |
| `ercpr(s)` | 1 / s |
| `esqrt(s)`, `ersqrt(s)` | √s, 1 / √s |
| `esin(s)` | sin s, for s in [−π/2, π/2] |
| `eatan(s)` | atan s, for s in [−1, 1] |
| `eexp(s)` | e^−s, for s ≥ 0 |

**Random numbers** (the random unit's R register, 23 bits):

| Built-in | Lowering |
|---|---|
| `rinit(s);` | `rinit`: R becomes the low 23 bits of `s`, a `float`, `int32` or `uint32` |
| `rxor(s);` | `rxor`: R becomes R XOR the low 23 bits of `s` |
| `rnext()` | `rnext`: R steps to its next value, read as a `float` in [1, 2) |
| `rget()` | `rget`: R read as `rnext` reads it, R unchanged |

R is a shift register over p(x) = x²³ + x⁵ + 1 (VU User's Manual, 1.1.2): each step shifts it left
and brings in bit 22 XOR bit 4. Every non-zero R comes round again after 2²³ − 1 steps; a zero R
stays zero, and its value reads as 1.0. The float's sign and exponent are 1.0's and R is its
mantissa, so `rnext() - 1.0` is in [0, 1). `rinit` and `rxor` are statements, with no value.

Calls read and step R in the order the source makes them, and two calls are never one value: each
`rnext()` and `rget()` costs its instruction, written straight into the lane its store or local
wants when it is that statement's whole value. R keeps its value from one run to the next. A
program that does not seed it continues the sequence the last run left. A loop with a random call
is not pipelined.

### Contraction and precision

A sum of products and other terms is one ACC chain: `mula`, `madda` and `msuba` for all but the
last, then `madd` or `msub`, with a term that is not a product times VF00's w. n products and k
other terms take n + k instructions instead of 2n + k − 1. For one product that is no fewer
instructions, but the chain does not wait for the product: ACC has no data hazards, so the `madd`
issues right after the `mula`. A product with a division's or root's result reads Q in the chain (`mulaq`,
`maddaq`, `msubaq`, `maddq`, `msubq`). A sum's lanes must match, as for any element-wise
operation. VU1 rounds each product before adding it, so the chain computes what
the separate operations would. `precise` is accepted on declarations and changes nothing.

Constant expressions fold at compile time in IEEE single precision, which differs from the VU's
arithmetic at the edges: the VU has no denormals, infinities or NaNs.

## Cost table

The most instructions each construct emits. Value reuse, hoisting and the peephole passes
([what the compiler does for you](#what-the-compiler-does-for-you)) often make it fewer.

| Construct | Instructions |
|---|---|
| Reading a member of memory | 1: `lq` of the whole qword, which serves every lane read from it (`ilw` for an `int16`) |
| Storing to a member | 1: `sq` with the member's lanes (`isw` for an `int16`). Stores to consecutive lanes of one qword become one `sq` when their values can share a register |
| `a + b`, `a - b`, `a * b`, `-a` | 1. A vector `-a` is `sub` from VF00, and its w lane 2 more |
| `a * b + c` and longer sums of products | One per product and term |
| `x / s` | 2: `div` and the consumer's Q form |
| `0.0`, `1.0` | 0: VF00's lanes |
| Other float literals | 2: `loi` and an `addi` into the lane wanted, once per loop when hoisted |
| `int16` literals | 1: `iaddiu`; 2 for 0x8000 and above |
| `int16` `+`, `-`, `&`, `\|` | 1: `iadd`, `isub`, `iaddiu`, `isubiu`, `iand`, `ior` |
| A lane move | 1 per moved lane, or 1 per `mr32` step when that is fewer |
| A constructor | 0 when only broadcast; 1 per part not computed in place, except literals: a `loi` per distinct literal and one `addi` for its lanes (`muli` for w), so `vec4(0.5)` is 3 |
| `M * v` | One per column |
| Conversions | As listed under [built-in functions](#built-in-functions) |
| A comparison in a condition | 1 or 2, then the branch |
| A `switch` | Per case label, an `int16` comparison with its branch (`iaddiu` and `ibne`); 0 for a constant value |
| A range-for | A guard, a pointer step per distinct stride, the exit branch |
| A struct copy | 2 per qword |
| A struct variable | Its members': each member read, set or stored as a variable of its type |
| An enum | As an `int16`; converting one costs nothing |
| A function call | 0: inlined. An `in` argument that is a local the call leaves alone, or a constant, is not copied |
| A library call | A move per argument and result, the entry address, the JALR and its delay slot, the entry table's branch and its delay slot, the library's saves and restores, its JR and its delay slot |
| `xgkick` | 1, plus the address when it is not in a register |
| `rinit`, `rxor`, `rnext()`, `rget()` | 1 |

## What the compiler does for you

These are the optimizations that the hand-written programs do by hand. Each only removes or fuses
work the source asks for.

- **Value reuse.** A loaded qword, an `int16`, a sum of two VI registers, or a pure expression is
  computed once and used again while it holds, also across inlined calls: an expression of a
  parameter set from the same value as another call's is the same expression. It stops holding
  after a store that may change its
  memory, an assignment to what it reads, a join of paths that did not all compute it, or a
  `yield`. Distinct parameters never alias unless one says `alias`, distinct members never do, and
  the same base with different offsets never does.
- **Loop-invariant hoisting.** Before a loop, the largest pure parts of its body that read nothing
  it changes are computed once: loads, constants, products of invariants. A part inside a branch
  counts only when both arms compute it. VI values leave only loops of one block, except `int16`
  constants that an instruction needs in a register (an `iand` mask, an `isub`'s first operand),
  in a loop that calls no library. At most 16
  values are kept; when the registers run out or values spill, the program is generated again with
  fewer, and the compile with the fewest spills is kept.
- **Pointers and exit tests** for range-fors, as described under [statements](#statements).
- **Contraction** into ACC chains.
- **Q and I.** After a division or root, readers take Q directly until the next one. Float literals
  go through I.
- **Lane placement** of scalars, so they need no move.
- **Zero and negation.** `0.0 + x` is `x`, and `a + (-b)` is `a - b` (and `a - (-b)` is `a + b`),
  also when `-b` is a local set to a negation of locals that keep their values. A local set from a
  read and overwritten by the next statement, its only reader (`float d = pos.w; d = d * s;`),
  needs no copy. These change no non-zero result: only the sign of a zero may differ (`0.0 + x`
  may be +0 where `x` is -0), and locals whose bits `floatBitsTo*` reads are left as written.
- **Copies and addresses.** A copy whose source keeps its value until the copy's last reader is
  dropped; an address of a base plus a constant, used only to address memory, becomes those
  accesses' offsets.

oxide-vcl then does the rest in its oxide mode: scheduling, pairing, register allocation, field
narrowing and dead code, loop pipelining (`loops`), loop unswitching on registers the loop does
not change (`unswitch`), one pointer for pointers that step together (`pointers`), work every
path through a loop's iteration does moved up into its earliest stream (`hoist`), and a conditional
stream's first work started in a stream every iteration runs (`speculate`). See
`docs/OPTIMIZATION_PLAN.md`.

## Rejected constructs

Each of these is an error that says what to write instead:

| Rejected | Instead |
|---|---|
| Operands in different lanes (`a.xy + b.zw`) | Assign one to a `layout(lane = ...)` local, which moves its lanes |
| A swizzle that reads a component twice (`v.xx`) | The scalar `v.x`, which operations broadcast |
| `cross`, `clipw`, `esadd` and the other xyz operations on other lanes | The same `layout(lane = x)` move |
| Vector ÷ vector | One scalar division per lane |
| A scalar minus a vector (`1.0 - v`) | `-(v - 1.0)`: only the second operand broadcasts |
| `int16` `*`, `/`, `%`, `<<`, `>>`, `^`, `~` | Adds: the VU has no such instructions |
| `?:` and per-lane selects | `if` |
| Arithmetic on raw integer types | Convert with `itof` first |
| Raw bits moved to other lanes except by rotation | `mr32` |
| `int` | `int16` |
| Global variables | A parameter of `main` |
| Recursion | A loop |
| A ref set from memory outside its region | Another ref |
| `break` or `continue` in an unrolled loop | A loop that is not unrolled |
| `yield` in a range-for | A `do`/`while` over a ref |
| Using a `spill` parameter | Another parameter |
| A call of a function with statements on the right of `&&` or `\|\|` | Call it before the condition and test its result |
| A copy of an array whose length is read at run time | A range-for that copies each element |
| A matrix or struct constant | A variable built from constants |
| `qword(...)`, `bool(...)` | Copy a qword from memory or set its lanes; compare (`n != 0`) |
| A C enum (`enum Name { ... }`) | `enum class Name { ... }` |
| An enum compared with or set from an integer, a constant or another enum | Its constructor, `Name(n)`, or `int16(e)` |
| Arithmetic on an enum | `int16(e)` first, then the constructor back |
| A case that runs into the next (`case 1: a(); case 2: b();`) | `break` at its end, or stacked labels |
| `break` inside a case's statements | `if` and `else` inside the case |
| A `switch` on a float or a vector | `if` and `else` |

## Pragmas

`#pragma` lines keep their macro-expanded arguments through the preprocessor. A loop pragma
applies to the loop right after it.

| Pragma | Effect |
|---|---|
| `#pragma program(Name)` | At file scope: the program's name |
| `#pragma pipeline(min = C, safe = S, extra = E, abs = A)` | `--LoopCS C,S`, `--LoopExtra E`, `--LoopAbs A` on the loop (`docs/VCL_REFERENCE.md`, loop hints). Each is optional; `min` defaults to 1 and `safe` to 0 |
| `#pragma nopipeline` | `--LoopCS 0,0`: the loop is not pipelined |
| `#pragma unroll(N)` | On a range-for, N from 1 to 16: the body N times per iteration over elements N strides apart, then the rest one at a time. With a length read at run time, N must be a power of two, since the split is an `iand` |
| `#pragma unroll` | A `for` loop with constant bounds, or a range-for of constant length, repeated fully |
| `#pragma nounroll` | Never unrolled |
| `#pragma barrier` | `--barrier` at that point: VCL moves nothing across it |

A loop without a pipeline pragma gets no hint, and oxide-vcl's `loops` optimization decides
whether to pipeline it (`--no-opt=loops` turns it off, [The command line](#the-command-line)). A loop is pipelined only when it is one block (no `if`, `break` or
`continue`) that ends in its exit branch, with no `xgkick`, XTOP, XITOP, random-unit call or
barrier inside. Unknown
pragmas are warnings.

## Libraries

A library is code compiled once, kept resident in micro memory, and called by any program: for
code several programs share whose size matters more than a call's cycles, such as a clipper.
Calls go through oxide-vcl's `--call` and `--return` (`docs/VCL_REFERENCE.md`, calls), so a
program that calls a library needs `--mode=oxide`, the default.

**The header** is VUSL that the library and its callers both include. It names the library, binds
the library's memory, and declares its exports:

```glsl
// mathlib.vush
#pragma once
#pragma library(mathlib, address = 1536, vf = 24, vi = 10, saved_vf = 4, saved_vi = 2, save = saves)

struct Constants { vec4 scale; };
struct Saves { qword q[5]; };

layout(address = 0) uniform Constants constants;   // memory the library only reads
layout(address = 1000) inout Saves saves;          // where it saves registers

export vec4 scaled(vec4 value, int16 times);
```

- **`#pragma library(name, address = A, vf = F, vi = I, ...)`**:
  - `address` is the micro address the library is uploaded to, in 64-bit instructions.
  - `vf` and `vi` start its register bank, VF*F*–VF31 and VI*I*–VI14 (by default VF16 and VI11).
    Arguments and results travel in the bank, and every call may change all of it, VI15 (the link)
    and VI01 too.
  - `saved_vf = n` and `saved_vi = n` let the library also work in the *n* registers below each
    bank, which it saves on entry and puts back before it returns. `save = memory` names the
    library memory they go to: an `inout` struct at an address, with a qword for each four VI
    registers and one for each VF register.
- **Memory at file scope.** A header binds the library's memory with `main`'s placements. Only the
  library's functions see it by name. Memory the library writes must not overlap its callers'
  memory; memory it only reads may, such as frame constants. A `layout(spill)` memory is the
  library's work area.
- **Exports** are declared with `export` and no body, in the order of the library's entry table:
  export *k* is at `address + 2k`. The library's source repeats each one with its body, and
  functions that are not exported are inlined into them.

**The calling convention** comes from the signature. In declaration order, `int16` and `ref<T>`
parameters take VI*I*, VI*I+1*, ..., and `float`, vectors and raw integer types take VF*F*,
VF*F+1*, ..., in their lanes from x. A return value takes the next register of its kind. `in` and
`inout` values travel in, `out` and `inout` values and the result travel back. Memory passes by
ref, and a ref the library gets may point anywhere, so its stores may change any memory but
`uniform` memory. A signature that does not fit the bank is an error. An `out` or `inout`
argument must be a whole variable.

**A program** includes the header and calls the exports like functions:

```glsl
#include "mathlib.vush"

struct Block
{
    vec4 value;
    int16 count;
    int16 pad0;
    int16 pad1;
    int16 pad2;
    vec4 result;
};

void main(layout(address = 4) inout Block block)
{
    block.result = scaled(block.value, block.count);
}
```

A call moves its arguments into their registers, calls, and moves the results out. What is live
across it stays out of the bank, so values the program keeps in registers through a call cost
registers; those that do not fit are saved in the program's work area across the calls, and keep
their registers everywhere else. Afterwards the program reads memory again, but not
`uniform` memory.

**The library's build** is a file with the header, the exports' bodies and no `main`. It writes one
listing: the entry table, a branch and its delay slot per export, then each export compiled as a
program of its own. The table keeps the entries' addresses when the library's code changes, so
callers need only the header.

**The interpreter** runs a call as the export's body, so a program checked against the
interpreter includes the library's source rather than only its header. A program that sees only
the prototypes still compiles, and the interpreter stops at its first call with an error.

Rejected: `export` or file-scope memory without `#pragma library`; a definition whose signature is
not its prototype's; a library build that leaves an export undefined; memory, `bool`, matrices or
`clipflags` as export parameters; `yield` in an export.

## Reflection

`--reflect=prog.json` and `--header=prog.h` describe the program's memory for the host. Both come
from the checked program, so `--check` writes them without generating code. Complete examples are
the check cases with an `expected.json` and `expected.h` in `tests/vusl/check` (`particles`,
`features`, `frame-tint`, `xitop-batch`, and `enum-memory` and `enum-shared` for enums).

### JSON

Schema version 1 (`"vusl": 1`):

- `program`: the name, the source file, the start and end symbols, and the instruction count
  (`null` under `--check`).
- `uses`: the features the program uses, from `xtop`, `xitop`, `clipw`, `efu`, `xgkick`,
  `yield` and `random`.
- `parameters`: for each parameter of `main`:
  - its name, qualifier and type, and its placement (`address`, or `xtop` with `vif_base` and
    `vif_offset`);
  - its size in qwords, a fixed part plus a term per run-time length, counting the open arrays the
    program writes;
  - the members it `reads`, `writes` and `kicks`, as dotted paths;
  - the `limit`: the largest length for which it fits its double-buffer half or the bound a fixed
    offset sets, when there is one.
- `types`: each struct, with the file it is declared in, a hash of its layout, its size, and its
  members. A member has its name, type, qword (a number, or an expression after a run-time-length
  array), lane, length, stride, matrix order and `unpack`. The qword and stride give the host its
  UNPACK address and STCYCL settings.
- `enums`, when a struct member holds one: each enum with its file and its values' names and
  integers.

### C++ header

C++17, with `<cstddef>`, `<cstdint>`, and `<tamtypes.h>` when `_EE` is defined.

- **Value types** in namespace `vusl`: `qword_t` (tamtypes.h's on the EE; a union with the same
  layout otherwise, for host tools), `vec2`–`vec4`, `ivec2`–`ivec4`, `uvec2`–`uvec4`, `mat3`, `mat4`,
  `int32`, `uint32`, and `int16_lane` for an `int16` member's 32-bit lane. `element<T>` holds an
  array element of fewer than four lanes, which takes a qword.
- **One struct per VUSL struct,** with explicit `_pad` members for the lanes the packing skips, and
  `static_assert`s on its size and every member's offset. A struct with arrays of run-time length
  is declared up to the first of them; constants give the offsets and strides of the rest
  (`kBatchParticles`, `kBatchParticlesStride`), of a member at a fixed offset after them
  (`kBatchState` for a `layout(offset = N)` member), and functions the offsets after them
  (`BatchPacket(count)`) and the size (`BatchQwords(count)`).
- **One `enum class` per enum a struct member holds,** with `std::int32_t` underneath, the size of
  the lane it takes, and its values. The VU reads the low 16 bits.
- **Shared types.** A struct or an enum from an included file goes in `vusl::layout_<hash>`, behind
  an include guard on the hash of its layout or values, so the headers of two programs that include the same file can be
  used in one translation unit. Types from the program's own file go in `vusl::<program>`.
- **Program constants** in `vusl::<program>`: each parameter's address (`kFrameAddress`) or VIF base
  and offset (`kBatchVifBase`, `kBatchVifOffset`), its size with the open arrays `main` writes
  (`BatchUsedQwords(count)`), its limit (`kBatchMaxCount`, checked by a `static_assert`), and
  `kInstructions`.
- **Libraries.** A program that calls a library leaves the library's memory to the library's own
  header, whose names may be `main`'s parameters' too, and gives the library's micro address
  (`kClipperMicroAddress`). A library's own header gives its memory, its micro address
  (`kMicroAddress`) and each export's entry (`kClipTriangleEntry`), in 64-bit instructions. The
  JSON marks the library's memory among a caller's `parameters` with `"library": true`, and names
  the library, its address and, in its own build, its exports' entries under `library`.
- **`VUSL_DECLARE_PROGRAM(name)`** is expanded at the end when the includer defines it, for example
  to declare the program's start and end symbols.

The host can then check its own structs against the program's:

```cpp
static_assert(sizeof(ParticleVertex) == sizeof(vusl::VU1Prog_Particles::Particle));
static_assert(offsetof(ParticleVertex, x) == offsetof(vusl::VU1Prog_Particles::Particle, pos));
```

## The command line

```text
oxide-vusl [options] <inputfile>
```

It reads `<inputfile>`, or standard input for `-`, and writes the listing to standard output
unless `-o` is given. A value follows its option as the next argument or glued to it (`-o out.vsm`
or `-oout.vsm`).

| Option | Effect |
|---|---|
| `-o <file>` | Write the listing to `<file>` |
| `--emit-vcl=<file>` | Also write the generated VCL |
| `--stop-at-vcl` | Write the generated VCL instead of the listing |
| `--annotate` | In the VCL written, a comment before each run of lines from one VUSL line: its file, number and text (`; clipper.vusl(52): d = d - jp.z;`). Needs `--emit-vcl` or `--stop-at-vcl`; the listing is the same |
| `--preprocess-only` | Write the preprocessed text and stop |
| `--check` | Check the program and stop: types, lanes, layouts and `main`'s parameters, without generating code |
| `--reflect=<file>` | Also write the reflection JSON |
| `--header=<file>` | Also write the C++ header |
| `-I <dir>` | Look for included files in `<dir>`; may be repeated |
| `-D <name>[=<value>]` | Define a macro (1 when no value is given); may be repeated |
| `--mode=oxide` or `--mode=vcl` | How the generated VCL is compiled: oxide-vcl's oxide mode with all its optimizations (the default), or vcl 1.4beta7's code generation, for cross-checks |
| `--opt=<names>` | Run only the named oxide-vcl optimizations, separated by commas, or `all` or `none`; may be repeated |
| `--no-opt=<names>` | Do not run the named optimizations, whatever `--opt=` chose |
| `--vcl-option=<option>` | Pass one oxide-vcl compile option, as oxide-vcl's command line takes it (`-c`, `-L`, `-M`, `-m0`, `--fix=all`, ...); may be repeated. Options for files, preprocessing, the mode and label names are oxide-vusl's own and are refused; `--opt=` here and oxide-vusl's join |
| `--warnings-as-errors` | Fail the run on any warning |
| `--color=auto`, `always`, `never` | Colour in diagnostics; `auto` when standard error is a terminal |
| `-h`, `--help`; `-v`, `--version` | Help and version |

The optimizations are oxide-vcl's (`docs/OPTIMIZATION_PLAN.md`): `exit` puts the E bit on the
earliest row that can end the program, `loops` pipelines loops that have no pipeline pragma, and
`unswitch` compiles a loop as two copies when it branches on a value it never changes. So
`--no-opt=loops` leaves every loop without a pragma unpipelined, while a `#pragma pipeline` loop
still is, and `--no-opt=unswitch` keeps such a loop in one copy, which is smaller. Both need oxide
mode: `--mode=vcl` with either is a command-line error.

The run writes all of its files or none. The exit status is 0 on success, 1 when the program has
errors (or warnings under `--warnings-as-errors`), and 2 for a bad command line.

Diagnostics quote the VUSL source, through the included files. An error that vcl reports in the
generated code points at the VUSL line that produced it. When values do not fit in the registers,
a note names each value kept in the work area.

## A complete program

`samples/` holds two small programs with the EE code that runs them, built against the PS2 SDK:
`hello_triangle` draws one triangle, and `rotating_cube` transforms a cube, tests depth and judges
its corners against the guard band.

`tests/vusl/check/particles/input.vusl` is quake2's particle program in VUSL, with the same
memory layout as the hand-written `particles.vcl`. Its loop transforms each particle, judges it
against the guard band, and writes a five-qword sprite. `tests/vusl/programs` holds the VUSL
programs quake2-ps2 and doom3-bfg-ps2 build, this one's successor among them, with their scenes.
