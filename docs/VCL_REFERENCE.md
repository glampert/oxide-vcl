# VCL reference

A guide to writing vector unit (VU) programs for `oxide-vcl`, this project's
compatible reimplementation of the VCL compiler (vcl 1.4beta7). It covers the
source language, every `--` annotation and `.` directive, the preprocessor
and the command line.

VCL takes VU code written as a plain list of instructions, one per line, in
program order, with symbolic register names. It works out the dependencies,
pairs upper and lower instructions, schedules them around latencies, inserts
the required waits, assigns physical registers and writes paired VU assembly
for the PS2 assembler (`ee-as`/`dvp-as`). You describe what the code computes
and which registers it receives and returns; VCL decides the timing.

The examples are complete programs unless they say otherwise. Each one
compiles with `oxide-vcl -uDEMO file.vcl`; outputs are shortened to the
interesting lines.

## A first program

```text
; Scale a vector by the x component of another.
--enter
in_vf   position(vf01)
in_vf   scale(vf02)
--endenter

        mulx    result, position, scale

--exit
out_vf  result(vf03)
--endexit
```

`--enter ... --endenter` lists what the program receives: `position` arrives
in VF01 and `scale` in VF02. `--exit ... --endexit` lists what it returns:
`result` must be in VF03 at the end. Between them is the code. The output:

```text
__v_DEMO_4:
         mulx[E]       VF03,VF01,VF02x          NOP
         NOP                                    NOP
```

Each output line is one VU instruction pair, upper on the left and lower on
the right. VCL set the E bit that ends the program and filled the delay slot.

Most programs also need temporary registers, and VCL may only use registers
you allow. By default **no register is allocable**:

```text
.init_vf_all            ; let VCL use VF01-VF31 for temporaries
--enter
in_vf   a(vf01)
--endenter
        add     t, a, a
        mul     b, t, a
--exit
out_vf  b(vf02)
--endexit
```

Without `.init_vf_all` (or `.init_vf` with a list), this program fails because
`t` has nowhere to live: `vcl failed to allocate a register of type VU User
floating point, but there's no registers of this type available for it to
use.. maybe adding a .init_.. directive would help`. When registers are
allowed but all are in use, the message names the register that did not fit.

## Source files

### Lines, comments and labels

One instruction per line. Instruction names, register names, directives and
keywords are case-insensitive.

```text
.init_vf_all
/* Block comments
   may span lines. */
--enter
in_vf a(vf01)       // a line comment
in_vi base(vi01)    ; also a line comment
--endenter
        lq      b,
=               2(base)         ; '=' continues the previous line
        add     c, b, a
----*
        ; text between these markers is copied to the output unchanged
*----
--exit
out_vf c(vf02)
--endexit
```

- `;` and `//` start a comment to the end of the line; `/* ... */` may span
  lines. `#` is **not** a comment: `add b, a, a # note` is read as a register
  named `a#note` and rejected.
- A line starting with `=` continues the previous instruction. A continuation
  with nothing before it is an error.
- Lines between `----*` and `*----` are passed through to the output as
  assembler text.
- A label is a name followed by `:`, alone or before an instruction. Labels
  must be unique. Labels nothing refers to are not printed.

### Registers

| Kind | Names |
| --- | --- |
| Vector (VF) | `vf00`-`vf31` (`vf1` and `vf01` are the same) |
| Integer (VI) | `vi00`-`vi15` |
| Hardware | `ACC`, `I`, `Q`, `P`, `R` |
| Symbolic | any other name made of letters, digits, `_`, `$`, `[` and `]`, shorter than 63 characters |

VF00 and VI00 are the constant registers (VF00 = (0, 0, 0, 1), VI00 = 0); they
are never allocated. A symbolic name gets its register family from the first
instruction that uses it, so a name cannot be both a vector and an integer.
Physical names may be used directly; they are never renamed.

Because names are case-insensitive, a symbolic name may not be spelled like a
hardware register or resource: `acc`, `i`, `q`, `p`, `r`, `clip`, `clipa`,
`mac`, `ext`, `tt_np` and the `stat_*` status names are refused for vector
registers (`'r' is a hardware register and can't name a VF register`), and
all but `i`, `q`, `p` and `r` for integer registers. An ILW, ISW, ILWR or
ISWR memory operand must also be long enough: `ilw.w n, 3(p)` gives `'3(p)'
is not a memory operand` whatever the base is named, because
`off(reg)` needs 5 characters there and `(reg)` 4, counting a field mask
after the `)` but not a `:tag`. `10(p)`, `3(ptr)` and `3(p)w` work, and so
does `3(p)` with `--fix=operands` ([Fixes](#fixes)). In the classic syntax a vector
register name's trailing `x`, `y`, `z` and `w` are read as fields, in
instructions and in `in_vf`/`out_vf` alike: `sticky` is register `stick` with
field y, and `copy` is `cop`. vcl compiles this silently and `oxide-vcl`
compiles it the same way with a warning, `file(N): warning: 'copy' is read as
register 'cop' with field selection .y`; with `--mode=oxide` it is an error. Avoid
names ending in one of these letters, and single-letter names `x`, `y`, `z`
and `w`. Tags must not be
instruction names either: in `lq v, 0(src):frame:b` the `b` is read as the
branch instruction and the line is rejected.

### Instructions and component masks

Instructions are the VU instruction set as the VU User's Manual describes it,
in the classic ("old") syntax by default. Components are selected with a
suffix on the instruction or on each operand:

```text
        add.xyz b, a, a         ; destination mask on the instruction
        add     c.xy, a.xy, b.xy ; or on each operand (they must agree)
        mulx    d, a, b         ; broadcast: b's x times each of a's fields
        div     Q, a.x, b.w     ; scalar operands use a field suffix
        mulq    e, a, Q
```

VCL inserts `waitq`/`waitp` itself. Writing them is only allowed in raw
mode (`.vsm`).

Interface declarations take a mask too: `out_vf.xy result(vf04)` returns only
X and Y.

### New syntax

`.syntax new` switches to generic arithmetic mnemonics. The broadcast or
accumulator variant is chosen from the operands: a bracketed field selects a
scalar, and `acc` as the destination selects the accumulator form.

```text
.syntax new
.init_vf_all
--enter
in_vf   v(vf01)
in_vf   m(vf02)
--endenter
        add     a, v, m[x]      ; becomes addx
        mul     acc, v, m[w]    ; becomes mulaw
        madd    b, a, m
        add.xy  c, b, v         ; destination fields still use a suffix
--exit
out_vf.xy c(vf03)
--endexit
```

In new syntax, the accumulator may only be the first operand, and a scalar
only the third. Names keep their trailing letters (`caty` is one name), and
an interface's fields go on the instruction (`out_vf.xy c(vf03)`), not the
name: `out_vf c.xy(vf03)` is rejected. `.syntax old` switches back.

### Memory operands

| Form | Example |
| --- | --- |
| `(base)` | `lq a, (base)` |
| `offset(base)` | `lq a, 16(base)` |
| `(base)field` | `ilw a, (base)w` |
| `offset(base)field` | `ilw a, 3(base)w` |
| `(base++)` | `lqi a, (base++)` |
| `(--base)` | `sqd a, (--base)` |

The offset may be an expression, as in `lq a, (0x10+2)(base)`. Integer loads
and stores (`ilw`, `isw`) need one field, given either on the instruction
(`ilw.w`) or on the operand (`3(base)w`), but not both differently.

```text
.init_vi_all
--enter
in_vi   base(vi01)
in_vi   value(vi02)
--endenter
        ilw.w   count, 3(base)
        isw.x   value, 0(base)
--exit
out_vi  count(vi03)
--endexit
```

### Memory order and tags

VCL assumes that memory accesses do not alias unless it can see that they
do. A load and a store (in either order), or two stores, that touch
overlapping fields keep their program order only when:

- they use the same base register value and the same offset, or
- timid mode (`-M`) is on, or
- for a load and a store only: they share a tag.

Accesses through **different base registers are otherwise free to swap**,
even if the registers hold the same address. Mark accesses that may alias
with a common tag after a colon, or use `-M` for the whole program:

```text
.init_vf_all
--enter
in_vi   src(vi01)
in_vi   dst(vi02)
in_vf   a(vf01)
--endenter
        sq      a, 0(dst):frame
        lq      b, 0(src):frame     ; shares a tag: stays after the store
        add     c, b, b
--exit
out_vf  c(vf03)
--endexit
```

With no tags, or with different tags (`:out` and `:in`), the load moves above
the store. Two stores that share a tag may still swap: a tag is something
loads read and stores write, so it orders a load against a store only.

In a loop that VCL software-pipelines, a shared tag also orders accesses
across iterations: the next iteration's load waits for this iteration's
store, which can lengthen the loop's period. Use tags only where the
accesses may really alias.

In a software-pipelined loop, **only tags order memory accesses**: the same
base register and offset, and `-M`, do not. A store and a load of the same
qword in one iteration can swap, and a sum kept in memory can read a stale
value. Tag such accesses, or use `--fix=alias` ([Fixes](#fixes)).

On an indirect jump, tags list the labels it can reach:
`jr target:first:second`. An instruction can have at most one tagged
operand.

### Immediates and expressions

Immediate operands accept C-like expressions:

| Operators (high to low) | Meaning |
| --- | --- |
| `~` `!` `+` `-` `#` (unary) | complement, not, identity, negate, absolute value |
| `*` `/` `%` | multiply, divide, remainder |
| `+` `-` | add, subtract |
| `<<` `>>` | 32-bit shifts |
| `&`, then `^`, then `\|` | bitwise and, xor, or |
| `&&` | logical and |

Decimal numbers are signed, `0x...` is unsigned, `0b...` binary, `0z...`
reinterprets hexadecimal bits as a float, and numbers with a fraction or
exponent are floats. `loi` reads `0x...` as float bits, so `loi 0x3f800000`
loads 1.0. `**` and `||` are recognised but fail when both sides are numbers.

An immediate that does not parse, or parses into numbers that cannot be
computed (`2**3`, `~1.5`, `1/0`), is an error on its line that says what is
wrong, for example `'3*' is not a valid immediate for iaddiu: the expression
ends after '*', which needs a value after it`.

An expression that names a symbol is kept as written for the assembler:

```text
.init_vi_all
--enter
in_vi   base(vi01)
--endenter
        iaddiu  a, base, (1 << 4) + 2*3     ; iaddiu VI02,VI01,0x00000016
        iaddi   b, base, -(2+3)             ; isubiu VI03,VI01,5
        iaddiu  c, base, BASE + 4           ; iaddiu VI04,VI01,BASE+4
--exit
out_vi  a(vi02)
out_vi  b(vi03)
out_vi  c(vi04)
--endexit
```

#### Math functions in `loi` (`--mode=oxide`)

With `--mode=oxide`, a `loi` expression may call the math functions of
openvcl's expression solver. vcl 1.4beta7 has no function calls: it rejects
`loi sin(1)` ("failed to parse immediate"), and vcl mode still does, so a
program that uses them compiles only in oxide mode (and with openvcl).

| Function | Result |
| --- | --- |
| `abs(x)` | absolute value |
| `exp(x)` | exponential |
| `sin(x)`, `cos(x)`, `tan(x)` | trigonometry, in radians |
| `sinh(x)`, `cosh(x)`, `tanh(x)` | hyperbolic trigonometry |
| `asin(x)`, `acos(x)`, `atan(x)` | inverse trigonometry |
| `atan2(y, x)` | the angle of the point (x, y), as C's `atan2`: `atan2(1, 2)` is `atan(1/2)` |
| `pow(x, y)` | `x` raised to `y` |
| `log(x)`, `log10(x)` | natural and base-10 logarithms |
| `sqrt(x)` | square root |
| `pi()` | pi |

The functions work in double precision and `loi` loads the result rounded to
a float, the bits openvcl emits for the same expression. Names are lowercase.
Arguments are expressions, and spaces after the commas are allowed. Inside
an expression that calls a function, an exponent needs no sign (`1.8e2`).
An unknown function, a wrong number of arguments, or an argument that is a
symbol rather than a number is an error. openvcl lists `tanh` but emits it
unevaluated; here it is evaluated.

```text
.init_vf_all
--enter
in_vf   v(vf01)
--endenter
        loi     sin(45 * (pi()/1.8e2))      ; loi 0x3f3504f3 (0.7071068)
        muli    scaled, v, i
--exit
out_vf  scaled(vf02)
--endexit
```

### Instruction prefixes and flags

| Syntax | Meaning |
| --- | --- |
| `add[D] b, a, a` | Set the D (debug break) bit on this instruction. |
| `mul[T] c, b, b` | Set the T (debug halt) bit. |
| `label[M]:` | M, D or T on a label applies to the first instruction emitted there. |
| `2..add b, a, a` | Raw loops only: the instruction belongs to iteration stage 2 (1-32; `+..` is a special stage). |
| `~add b, a, a` | Accepted and kept; it has no effect on the output. |

The E bit cannot be written in source (`add[E]` is rejected): VCL sets it at
each exit and `--cont`.

## Program structure: `--` annotations

Annotations start with `--` and stand on a line of their own. They describe
the program to the compiler; they never become instructions.

### `--enter` ... `--endenter`

Opens the program and lists its inputs, one declaration per line.

```text
--enter
in_vf   vertex(vf01)        ; symbolic name bound to a register
in_vf   vf02                ; a physical register used by name
in_vi   count(vi01)
in_hw_i I                   ; the I register holds a value on entry
--endenter
```

| Declaration | Declares |
| --- | --- |
| `in_vf name(vfNN)` / `in_vf vfNN` | a vector input, optionally masked (`in_vf.xyz`) |
| `in_vi name(viNN)` / `in_vi viNN` | an integer input |
| `in_hw_acc`, `in_hw_i`, `in_hw_q`, `in_hw_p`, `in_hw_r`, `in_hw_clip`, `in_hw_mac` | a hardware register's value on entry, e.g. `in_hw_i I` |
| `xin_hw resource` | an external dependency on a hardware resource (VCL inserts these for status flags itself) |

`in_vf a, vf01` is not another spelling of `in_vf a(vf01)`; it is rejected.
A program needs at least one `--enter`, and at least one `--exit`,
`--exitm` or `--cont` (`the program has no --exit block`). Reading a register that
is neither an input nor written first is an error: `'VF02.x' is read before
anything writes it`.

### `--exit` ... `--endexit`

Ends a path through the program and lists what must be in which register
there. Every path must end in an exit (or `--cont`).

```text
--exit
out_vf  result(vf03)
out_vf.xy uv(vf04)          ; only X and Y are returned
out_vi  count(vi01)
--endexit
```

| Declaration | Declares |
| --- | --- |
| `out_vf name(vfNN)` / `out_vf vfNN` | a vector output, optionally masked |
| `out_vi name(viNN)` / `out_vi viNN` | an integer output |
| `out_hw_acc`, `out_hw_i`, ... `out_hw_mac` | a hardware register's value at exit |
| `xout_hw resource` | an external output dependency |

VCL ends the exit with the E bit and its delay slot. vcl 1.4beta7 can lose
both in its final compression, so that the VU runs past the end of the
program; `--fix=end` keeps them (see [Fixes](#fixes)). A program may have
several exits, one per path:

```text
.init_vf_all
--enter
in_vf   a(vf01)
in_vi   flag(vi01)
--endenter
        ibeq    flag, vi00, square
        add     b, a, a
        b       done
square:
        mul     b, a, a
done:
--exit
out_vf  b(vf02)
--endexit
```

Here VCL places a copy of the exit at the end of each branch.

### `--exitm macro`

An exit that ends with assembler text you supply instead of the E bit, for
example a macro that chains to the next program. It takes the same output
declarations and still needs `--endexit`.

```text
--enter
in_vf   a(vf01)
--endenter
        add     b, a, a
--exitm NEXT_STAGE
out_vf  b(vf02)
--endexit
```

The output ends with the line `NEXT_STAGE` and no E bit. With no macro text,
the exit simply ends without an E bit. `--exitm` cannot appear inside another
exit block.

### `--cont`

Stops the program here with an E bit, so that the next `MSCNT` resumes at the
following instruction. Values live across the stop stay in their registers.

```text
.init_vf_all
--enter
in_vf   a(vf01)
--endenter
        add     b, a, a
--cont                       ; the VU stops here; MSCNT continues below
        mul     c, b, b
--exit
out_vf  c(vf02)
--endexit
```

As at an exit, vcl 1.4beta7 can lose this E bit, so the program runs on
without stopping; `--fix=end` keeps it (see [Fixes](#fixes)).

`--cont` ends ordinary code only: directly after an exit it is an error
(`CONT directive encountered - but end mode isn't null`).

### `--barrier`

A scheduling fence: VCL starts a new block here, and no instruction moves
across it in either direction. Use it to keep code in a fixed order relative
to something VCL cannot see, such as memory shared with another processor.

```text
.init_vf_all
--enter
in_vf   a(vf01)
in_vi   base(vi01)
--endenter
        sq      a, 0(base)
--barrier
        lq      b, 1(base)
        add     c, b, b
--exit
out_vf  c(vf02)
--endexit
```

### Loop hints: `--LoopCS`, `--LoopExtra`, `--LoopAbs`

VCL software-pipelines a loop when it is a single block whose last
instruction is a conditional branch back to its own first label, and the block
has a `--LoopCS` hint. Put the hints right after the loop's label.

| Annotation | Meaning |
| --- | --- |
| `--LoopCS count, safe` | The loop always runs at least `count` times (at least 1). Stores from up to `safe` iterations past the last may execute harmlessly (at least 0), so later iterations' stores may start before the exit test. `--LoopMinCountMinSafe` is the long spelling. |
| `--LoopExtra n` | Start the search for a loop period `n` cycles above the minimum (0 or more). |
| `--LoopAbs n` | Start the search at a period of at least `n` cycles (3 or more). It never goes below the minimum. |

An out-of-range hint argument is reported and skipped, as vcl does, and the
program still compiles: `--LoopCS 0,0` sets nothing (the loop is not
pipelined), `--LoopCS 4,-1` keeps the count of 4, and `--LoopAbs 2` is
ignored. In oxide mode a count of 0 is not reported: `--LoopCS 0,0` is how
a program keeps the `loops` optimization off one loop (see
[Optimizations](#optimizations)), and the original compiles the same source
with that loop as written. The `--LoopAbs` message says `value must be >=2`,
vcl's wording, although 2 is refused. A negative `--LoopExtra` is an error.

```text
.init_vf_all
.init_vi_all
--enter
in_vi   src(vi01)
in_vi   count(vi02)
in_vf   scale(vf02)
--endenter
loop:
--LoopCS 4, 0
        lq      v, 0(src)
        mul     v, v, scale
        sq      v, 0(src)
        iaddiu  src, src, 1
        isubiu  count, count, 1
        ibne    count, vi00, loop
--exit
--endexit
```

VCL emits a prologue, a kernel labelled `..._MAIN_LOOP` that overlaps
several iterations, and an epilogue. A larger `count` lets it drop early-exit
tests from the prologue. `-L` turns loop pipelining off.

## Raw code

Raw blocks give you exact control. Each line holds an upper and a lower
instruction, which VCL keeps paired as written. It still assigns registers to
symbolic names. Branches in raw code need their delay slot written out.

```text
.init_vf_all
--enter
in_vf   a(vf01)
--endenter
.vsm
        add     b, a, a         nop
        nop                     nop
        nop                     nop
        nop                     nop
        mul     c, b, b         nop
.endvsm
--exit
out_vf  c(vf02)
--endexit
```

Use the symbolic names in raw code; the physical register an input is bound
to (`vf01` above) is a different name there.

`.vsm reorder` lets VCL reorder the raw rows; `.vsm noreorder` (the default)
keeps them. A raw loop marks each instruction with its iteration stage, and
VCL expands it into prologue, kernel and epilogue:

```text
.init_vf_all
--enter
in_vf   vf01
in_vf   vf02
in_vi   vi01
--endenter
.vsmloop
loop:
--LoopCS 1,0
1..mul vf03,vf01,vf01   nop
2..mul vf04,vf03,vf01   1..isubiu vi01,vi01,1
3..add vf02,vf02,vf04   nop
nop                     1..ibne vi01,vi00,loop
nop                     nop
.endvsmloop
--exit
out_vf  vf02
out_vi  vi01
--endexit
```

The loop's label and `--LoopCS` must be inside the `.vsmloop` block.

## Directives

Directives start with `.` and take effect from where they appear. There are
exactly 28; anything else is an error.

### Program identity and output layout

**`.vu`**: Accepted and ignored. (The `.vu` line in the output is always
written for old-syntax programs.)

**`.name name`**: Names the program. VCL emits global `name_CodeStart` and
`name_CodeEnd` labels around the code, for loading or measuring it.

```text
.name   ScaleVector
.global ScaleVector_Entry
.init_vf_all
--enter
in_vf   a(vf01)
--endenter
ScaleVector_Entry:
        add     b, a, a
        mul     c, b, a
--exit
out_vf  c(vf02)
--endexit
```

```text
		.global	ScaleVector_CodeStart
		.global	ScaleVector_CodeEnd
		.global	ScaleVector_Entry
ScaleVector_CodeStart:
ScaleVector_Entry:
         add           VF02,VF01,VF01           NOP
         mul           VF02,VF02,VF01           NOP
		.align 4
ScaleVector_CodeEnd:
```

**`.global name`**: Exports a label (`.global` in the output). Global labels
keep their names under `.unique` and `-U`.

**`.mpg address`**: Wraps the program in a DMA/VIF packet (`DMAret`, three
`vifnop`, `MPG address,*` ... `.EndMPG`, `.EndDmaData`) that uploads it to
micro memory at `address` (decimal). Same as `-m<address>`.

```text
.mpg 32
```

**`.align ...`**: Accepted and ignored. VCL writes `.align 4` before and after
the code on its own.

**`.noaligns`**: Omits the `.align` lines. Same as `-f`.

**`.noebits`**: Meant to disable E bits, but in vcl 1.4beta7 it prints its
message and then does what `.noaligns` does: E bits stay and the `.align`
lines go. Use `-e` or `--exitm` to drop E bits.

### Register availability

These directives decide which physical registers VCL may use for
temporaries. Registers bound in `--enter`/`--exit` are always usable for
those values.

**`.init_vf list`**, **`.init_vi list`**: Allow the listed registers. A list
is comma-separated and may contain inclusive ranges.

```text
.init_vf vf10-vf15, vf20
.init_vi vi05-vi09
```

A decreasing range (`vf15-vf10`) is accepted with a warning. Bare numbers,
symbolic names and component suffixes (`vf07.xyz`) are rejected.

**`.init_vf_all`**, **`.init_vi_all`**: Allow every VF register except VF00,
or every VI register except VI00.

**`.rem_vf list`**, **`.rem_vi list`**: Take registers back, for example ones
another program keeps a value in.

```text
.init_vf_all
.rem_vf vf28-vf31            ; vf28-vf31 hold constants for other programs
```

### Syntax

**`.syntax new`**, **`.syntax old`**: Switch between the generic new syntax
and the classic syntax (see [New syntax](#new-syntax)). `-n` starts in new
syntax.

### Raw code

**`.vsm [reorder|noreorder]`** ... **`.endvsm`**: A raw block (see
[Raw code](#raw-code)). `.raw` and `.endraw` are the same directives. Opening
a raw block inside one warns and reapplies the mode.

**`.vsmloop [reorder|noreorder]`** ... **`.endvsmloop`**: A raw loop with
stage-prefixed instructions. `.rawloop` and `.endrawloop` are the same.
Raw loops cannot nest, and a raw block cannot open inside one.

### Names

**`.unique [prefix]`**: Prefixes every label except globals with a unique
string, so several programs can be assembled together. The prefix is
`prefix` if given, else the `-u` name, else the input file name (characters
other than letters, digits and `_` become `_`). `-U` does the same from the
command line.

```text
.unique Blur
.init_vf_all
.init_vi_all
--enter
in_vf   a(vf01)
in_vi   n(vi01)
--endenter
again:                       ; emitted as Blur_again
        add     b, a, a
        isubiu  n, n, 1
        ibne    n, vi00, again
--exit
out_vf  b(vf02)
--endexit
```

**`.show list`** ... **`.endshow`**, **`.hide list`** ... **`.endhide`**:
Scopes that give symbolic names inside them their own identity, so that a
macro or other shared code can use temporaries without touching the
caller's registers of the same name.

- `.hide list`: the listed names are local to the scope. An outer `a` hidden
  this way cannot be read inside it, and a hidden `t` written inside does not
  exist after `.endhide`.
- `.show list`: the opposite. Only the listed names refer to the surrounding
  code; every other symbolic name inside is local. This is the usual way to
  wrap a macro body: list its inputs and outputs.

```text
.init_vf_all
--enter
in_vf   a(vf01)
in_vf   t(vf03)             ; the caller's t
--endenter
.show a, b                  ; a macro reading a and producing b
        add     t, a, a     ; its own temporary: not the caller's t
        mul     b, t, t
.endshow
        add     c, b, t     ; reads the caller's t, still in vf03
--exit
out_vf  c(vf02)
--endexit
```

Without the scope, the macro's `add` would overwrite the caller's `t`. VCL
renames each local name with the scope's number, counting scopes from the
start of the file, so `t` in the first scope is `1t` (visible with `-s`, and
in messages such as `'1a.x' is read before anything writes it`). Two scopes
never share a local,
which keeps two expansions of one macro apart. Further rules:

- Scopes nest. A name is resolved from the innermost scope outward: the first
  scope that makes it local decides its number, and a name that every
  scope lets through is the caller's.
- Physical registers (`vf01`) and names starting with `_` are never renamed.
- The lists match names case-sensitively, unlike the rest of VCL: `.hide T`
  does not hide `t`.
- Closing a scope with the other kind (`.hide` ... `.endshow`) is an error. A
  close with no scope open, and a scope still open at the end of the file,
  only warn.

### Work memory

**`.vu_work_area device, address, size`**: Gives the compiler `size` qwords
of data memory from qword `address`, for values that do not fit in the
allowed registers. `device` is `vu0` (256 qwords) or `vu1` (1024 qwords).

In vcl 1.4beta7 every such request fails (`init work area - failed to get vu
mem pool`): that version never set up the memory it needs. `oxide-vcl` does
the same by default.

With `--mode=oxide`, a program whose VF or VI registers do not fit spills
values into the work area instead of failing. A spilled value is stored to
its own qword after each write and loaded back before each read, so it only
holds a register briefly. Each spill is reported as a note:

```text
.init_vf vf01-vf04
.vu_work_area vu1, 1000, 8
--enter
in_vf   src(vf10)
--endenter
        add     ta, src, src
        mul     tb, src, src
        ...
```

```text
prog.vcl(6): note: 'ta' does not fit in the VF registers: it is kept in vu1 data memory at qword 1000, the work area
```

- The area must be memory the program does not use itself: the compiler
  assumes nothing else reads or writes it while the program runs.
- Addresses and sizes are in qwords, decimal or `0x` hexadecimal (vcl reads
  only a decimal prefix, so `0x3e8` would be 0 there). Several work areas
  are used one after another.
- Values are spilled one at a time, preferring the one live over the longest
  stretch of code, until the program fits. When the area is full, the error
  says so; make it larger or allow more registers.
- Interface values (`in_vf`, `out_vi`, ...), raw code, names inside
  `.hide`/`.show` scopes, the registers of `(vi++)`/`(--vi)` addresses and
  branch link registers are never spilled.
- In a loop that spills, the `safe` count of `--LoopCS` is treated as 0:
  a spill store from an extra iteration would overwrite a value still in
  use. A loop whose pipelined schedule still does not fit is compiled as a
  plain loop.

## Preprocessor

`-G` and `-g` run a preprocessor over the source before compiling it, in the
same run. It is [vclpp](https://github.com/glampert/vclpp) 2's, the
preprocessor the PS2 ports of Quake II and Doom 3 build their VU programs
with:

- `-G`, standard mode: C's directives, block macros and `#vuprog`;
- `-g`, MASP mode: the macro language of masp and GASP, the assembler
  preprocessors classic VU code was written for, then C's directives over
  the result.

`--preprocess-only` writes the preprocessed source instead of compiling it.
Errors and warnings name the file, line and column at fault, the chain of
`#include`s that led there and the macros the code came out of. The
compiler's own messages about preprocessed code name the line the code was
written on, in whichever file that is.

### Standard mode (`-G`)

Comments and blank lines are left out of the output, and each line of code
keeps its indentation. `;`, `//` and `/* */` comments all work.

```text
#define kWindowA   10
#define kWindowB   (kWindowA + kWindowSize)
#define ADDR(base, offset)  ((base) + (offset))
#define STR(x)     #x
#define LABEL(n)   l ## n ## Loop
```

`#define` defines a constant, or with a parameter list straight after the
name a function-like macro, as in C: `#` turns an argument into a string,
`##` pastes two tokens together, and `...` takes any number of arguments, as
`__VA_ARGS__`. A macro applies to the code after its definition, and what it
expands to is expanded again, so macros can be made of other macros. A
function-like macro's name without an argument list after it is left alone.

**Block macros** hold lines of code:

```text
#macro MatrixMultiplyVertex: result, matrix, vertex
    mul  acc,    matrix[0], vertex[x]
    madd acc,    matrix[1], vertex[y]
    madd acc,    matrix[2], vertex[z]
    madd result, matrix[3], vertex[w]
#endmacro

    MatrixMultiplyVertex{ vPos, fMVP, vPos }
```

The invocation is replaced by the body's lines. Arguments are separated by
commas, and brackets of any kind group them, so an argument can be an
expression like `kBase + 3`; an argument list can continue on the next lines.
A block macro can invoke other block macros and use any other macro, and `##`
works in its body. A block macro that invokes itself, directly or through
others, is an error, and so is `Name{ ... }` with no `#macro Name` before it.

**Include files**: `#include "file"` is looked for next to the file that
includes it, then in the `-I` directories in order, then in the working
directory; `#include <file>` only in the `-I` directories. `#pragma once` and
include guards work as in C.

**Conditionals**: `#if`, `#ifdef`, `#ifndef`, `#elif`, `#else` and `#endif`
work as in C, with `defined` and C's integer operators, `?:` included; as in
C, `&&`, `||` and `?:` do not evaluate an operand they skip, so
`#if defined(N) && 64 / N` is safe. A name that is not a
macro counts as 0 (`--warn-undef` warns about it), and `true` and `false`
count as 1 and 0. Also available: `#undef`, `#error`, `#warning` and
`#pragma once`.

**Programs**: `#vuprog Name` ... `#endvuprog` mark the program; the name
becomes VCL's `.name` directive. The preprocessor warns if either is missing.

**Built-in macros**: `__FILE__`, `__LINE__`, `__COUNTER__`, and `__VCLPP__`,
which is 2.

A complete program, compiled with `oxide-vcl -G -uDEMO scale.vcl`:

```text
#macro ScaleVector: result, vector
    mul result, vector, factor[x]
#endmacro

#vuprog ScaleVector
.init_vf_all
.syntax new
.vu
--enter
in_vf vector(vf01)
in_vf factor(vf02)
--endenter
    ScaleVector{ scaled, vector }
--exit
out_vf scaled(vf03)
--endexit
#endvuprog
```

### Preprocessor options

They apply with `-G` or `-g`, in either mode, and are an error without them.

| Option | Effect |
| --- | --- |
| `-I<dir>` | Look for included files in `dir`. May be repeated. |
| `--define=<name>[=<value>]` | Define a macro before the source, as `#define name value` (the value is 1 by default). With `-g` it sets the masp variable `\&name` instead. |
| `--vcl-boilerplate` | Put `.init_vi_all`, `.init_vf_all`, `.syntax new`, `.vu` and empty `--enter` and `--exit` blocks around the code. |
| `--fold-constants` | Replace each integer constant expression that stands on its own by its value: `1010 + 0(vi00)` becomes `1010(vi00)`. One that an operator next to it would take part in, like the `1 - 2` in `a - 1 - 2`, is left alone. |
| `--flatten-subscripts` | Turn a subscript of one digit, or of `w`, `x`, `y` or `z`, into a suffix: `matrix[0]` becomes `matrix_0`, `vf01[x]` becomes `vf01x`. |
| `--warn-undef` | Warn when an `#if` reads a name that is not a macro as 0. |
| `--comment-char=<c>` | With `-g`: make `c` start comments as well as `;`. |
| `--preprocess-only` | Write the preprocessed source to `-o` or standard output, and stop. |
| `--warnings-as-errors` | Also fails a run whose preprocessing printed a warning, before anything is written. |

`--vcl-boilerplate` declares `.init_vi_all`, so VI01 is allocable too: VCL
keeps VI01 free across the FCAND, FCEQ and FCOR that write it (observed from
vcl 1.4beta7, probe `vi01-n15-fc`). vclpp lists VI02 to VI14 instead, for
openvcl, which does not.

### MASP mode (`-g`)

```text
        .macro  dot3    output, vec1, vec2, op=mul
        \op.xyz \output, \vec1, \vec2
        .endm

loop:   dot3    vf01, vf02, vf03            ; mul.xyz vf01, vf02, vf03
        dot3    vf01, vf02, vf03, op=add    ; add.xyz vf01, vf02, vf03
        iaddiu  vi02, vi00,
+               kOffset                     ; '+' in column 1 continues a line
```

**Lines**: a name in column 1 is a label, which may go without its colon (one
is added). Code and directives are indented. Comments and blank lines are
left out of the output.

**Macros**: `.macro name params` ... `.endm`. A macro is invoked by its name,
followed by a space or the end of the line, then its arguments, separated by
commas; quotes keep a comma inside an argument. Macro names ignore case,
parameter names do not. A parameter can have a default value, and an argument
can name its parameter, as `op=add` does; after one argument given by name,
the rest have to be too. A missing argument takes its default, or is empty.
`name .macro params`, with the name in column 1, defines a macro too.

In the body, `\name` is replaced by the argument wherever it appears, even
inside a word; the name is the longest one after the backslash, and one that
is no parameter's is left as written. `\@` is the number of expansions before
this one, from 0, which gives each expansion labels of its own. The expanded
lines are read again, so they can invoke other macros and use any directive.
`.exitm` ends an expansion early. A macro can invoke itself as long as an
`.aif` stops it; expansions nest up to 200 deep.

**Conditionals**: `.aif`, `.aelse`, `.aendi`. `.aif` compares two operands
with `EQ`, `NE`, `LT`, `LE`, `GT` or `GE`, in any case. Two quoted strings
compare as text; anything else is an integer expression, read as `#if`
reads one. An `.aif` with no comparison holds if its value is not 0.

**Loops**: `.arepeat count` ... `.aendr` repeats its lines, and `.awhile
condition` ... `.aendw` repeats them while the condition holds. Either stops
after 100000 passes.

**Variables and constants**: `name .assignc "text"` gives a variable some
text, `name .assigna expression` the value of an integer expression, and
`\&name` is replaced by the value anywhere. `name .equ expression` gives a
name a value, which replaces the name wherever it appears as a whole word,
even inside quotes. The name goes in column 1; an indented `.equ` passes
through to VCL.

**Files**: `.include "file"` looks next to the including file, then in the
`-I` directories, then in the working directory. `.end` ends the program,
even in an included file; the preprocessor warns if there is none.

**Other directives** pass through as code: VCL's `.name`, `.init_vf_all` or
`.align`, for example. GASP's data and listing directives are errors:
`.data`, `.datab`, `.sdata`, `.sdatab`, `.sdataz`, `.sdatac`, `.res`,
`.sres`, `.sresc`, `.sresz`, `.print`, `.heading`, `.page`, `.form`, `.org`,
`.radix`, `.alternate`, `.program`, `.export` and `.assign`. `-g+` (gasp's
alternate syntax) is not supported.

**C directives**: lines that start with `#` are carried out after masp's
directives, in order, as cpp would run over masp's output. `#include` takes
only a header's directives, as `cpp -imacros` does, since VU code shares its
headers with C code. `#macro`, `Name{ }` and `#vuprog` are not available in
MASP mode.

## Command line

```sh
oxide-vcl [options] [inputfile]
```

`oxide-vcl` reads `inputfile`, or standard input when there is none, and writes
the assembly to standard output unless `-o` is given. Options come before the
file, and a value follows its letter directly: `-ofile.vsm`, not
`-o file.vsm`. Most options ignore extra letters (`-quiet` is `-q`).

| Option | Effect |
| --- | --- |
| `-o<file>` | Write the assembly to `file`. |
| `-j<file>` | Also write a header of the program's global labels and their offsets, e.g. `ScaleVector_Entry = 0`. |
| `-m[<address>]` | Wrap the code in an MPG packet at `address` (default 0), like `.mpg`. |
| `-c` | Comment each row with the source's register names: the commented copy, then the row, then a blank line. |
| `-s` | Emit symbolic register names instead of assigning registers. |
| `-f` | Omit `.align` lines, like `.noaligns`. |
| `-e` | Do not set E bits on the empty pairs VCL adds at exits. |
| `-u<name>` | Use `name` instead of the file name for generated and unique labels. |
| `-U` | Make all labels except globals unique, like `.unique`. |
| `-n` | Start in new syntax. |
| `-G` | Preprocess the source first, in standard mode: C directives, `#macro` blocks and `#vuprog` (see [Preprocessor](#preprocessor)). |
| `-g` | Preprocess the source first, in MASP mode: masp's `.macro`, `.aif`, `.include` and the rest, then C directives. |
| `-g+` | gasp's alternate syntax: accepted on the command line, but an error when preprocessing runs. |
| `-I<dir>` | Look for included files in `dir`. May be repeated. |
| `-W` | Skip preprocessing. |
| `-K` | Accepted; preprocessing keeps no temporary files. |
| `-L` | Do not pipeline loops. |
| `-d` | Plain in-order code only; implies `-L`. |
| `-C` | Skip the code size reduction pass. |
| `-M` | Timid memory access mode: keep every pair of loads and stores that touch overlapping fields in program order, except in software-pipelined loops (see [Memory order and tags](#memory-order-and-tags)). |
| `-q` | Quiet: print only warnings and errors. |
| `-h`, `--help` | Print the help screen. |
| `-v`, `--version` | Print oxide-vcl's version and build date on standard output. Not in vcl 1.4beta7, which rejects both. |
| `--mode=vcl` | Generate the code vcl 1.4beta7 generates, with its exit statuses. The default. |
| `--mode=oxide` | Enable oxide-vcl's own behaviour, which breaks compatibility with vcl 1.4beta7: its exit statuses (see [Exit status and messages](#exit-status-and-messages)), every fix (`--fix=all`), openvcl's math functions in `loi` expressions ([Math functions in `loi`](#math-functions-in-loi---modeoxide)), and its optimizations ([Optimizations](#optimizations)). |
| `--fix=<names>` | Enable fixes for vcl 1.4beta7's defects, by name, separated by commas; `--fix=all` enables every one. May be repeated. See [Fixes](#fixes). |
| `--opt=<names>` | With `--mode=oxide` only: run just the named optimizations, separated by commas; `--opt=all` (the default) runs every one, `--opt=none` none. May be repeated. See [Optimizations](#optimizations). |
| `--no-opt=<names>` | With `--mode=oxide` only: do not run the named optimizations, whatever `--opt=` chose, in any order. May be repeated. |
| `--warnings-as-errors` | Fail the run when it prints a warning, from the command line or the program. Not in vcl 1.4beta7. |

`-g` with `-G` is MASP mode, which runs C's directives too. The
preprocessor's own long options are listed under [Preprocessor
options](#preprocessor-options). vcl 1.4beta7 runs `gcc -P -E -C -x
assembler-with-cpp` for `-G` and `gasp` for `-g`; oxide-vcl runs no external
program.

`--mode`, `--fix`, `--opt`, `--no-opt`, `--warnings-as-errors` and the
preprocessor's options are oxide-vcl's own; vcl 1.4beta7 has no long options
and rejects them as unknown switches.

Accepted for compatibility, without effect: `-t<seconds>` (vcl's optimizer
time limit; `oxide-vcl` has none), `-S`, `-D`, `-Q`, `-a` and `-p` (Microsoft-style
errors, now the only style). `-P` and `-Z` only
print a deprecation warning.

### Fixes

vcl 1.4beta7 has defects that make it emit wrong code for valid programs
([VCL_BUGS.md](VCL_BUGS.md)). By default oxide-vcl reproduces them, so that
its output equals the original's. Each fix changes the output only where it
is wrong, keeps the rest of vcl's behaviour and messages, and is chosen by
name with `--fix=`; `--mode=oxide` enables all of them.

| Fix | Repairs |
| --- | --- |
| `imm` | Numeric immediates that do not fit their field, which the assembler truncates without a word. IADDIU and ISUBIU steps are rewritten exactly (`iaddiu …,0xffff` is `isubiu …,1`; adding 0x8000 in one step is an error). Memory offsets outside -1024 to 1023 and FCEQ, FCSET, FSEQ or FSSET values outside their field are errors. FCAND, FCOR, FSAND and FSOR masks with bits past the flag are warnings, as are symbolic IADDI steps and memory offsets. vcl's own rewrite of an offset when it moves a load or store across its pointer's update never leaves the field. |
| `alias` | A load or store that vcl moves across an earlier store, or a store across an earlier load, to the same qword through a different base register: `sq …,1(p)` with `p = 900` and `lq …,901(vi00)`, or a base and its copy. With the fix, two accesses keep their order when their addresses are provably equal: the base registers are followed back through IADDIU, ISUBIU, IADDI, IADD and ISUB with VI00, VI moves and the LQI, SQI, LQD and SQD steps, to VI00 or a common register; in a loop, a pointer and a copy that step alike are equal. In a software-pipelined loop, where vcl orders only tagged accesses, accesses to one qword keep their order within an iteration and from one iteration to the next, including a pointer's accesses a whole number of steps apart (a load at `99(p)` of what the previous iteration stored at `100(p)`); the loop is searched again only when vcl's schedule breaks that order. Pairs that cannot be resolved keep vcl's no-alias assumption. |
| `clip` | A program that runs CLIPW without declaring `in_hw_clip`, which vcl rejects ("register CLIPa is used before set") even when nothing reads the earlier judgments. With the fix it compiles as if `--enter` declared `in_hw_clip CLIP`, as long as every FCAND, FCOR, FCEQ, FCGET and `out_hw_clip` sees only judgments its own CLIPWs made on every path; otherwise it is still rejected. |
| `end` | An exit or `--cont` whose E bit or delay slot vcl's final compression deleted, so that the VU ran on past it. The fix puts the E bit on the table's last row and adds an empty delay row after it. It does the same when the delay slot holds an instruction the hardware forbids there (a branch, a load or store, XGKICK, XTOP or XITOP). With `-e` it does nothing. |
| `size` | A warning for a program larger than the 2048 instructions of VU1 micro memory. A branch farther than its 11-bit offset reaches is not warned about: the assembler truncates the offset, and the program counter wraps around micro memory to the target. |
| `entry` | A software-pipelined loop whose first iteration reads Q, P or a flag that the code before the loop sets: vcl's kernel can read it before it lands. With the fix a `waitq` or `waitp` row, or for a flag up to three empty rows, comes before the loop's label, as if written in raw code. Loops reached by a branch to their label are left as vcl compiles them. |
| `operands` | An ILW, ISW, ILWR or ISWR memory operand that vcl rejects only for its length (`ilw.x t,0(p)`, `ilwr.x t,(p)`, `0(p):tag`). With the fix it compiles as its longer spellings (`0(p)x`, `0(ptr)`) do. Operands with a wrong shape are still rejected: a capital field mask, a field mask after an LQ or SQ operand, or text before `(p++)`. |

```sh
oxide-vcl --fix=end -oskin.vsm skin.vcl       # vcl's output, with the E bit fix
oxide-vcl --fix=all -oskin.vsm skin.vcl       # every fix, vcl's exit statuses
```

### Optimizations

Oxide mode is also where code generation improves on vcl 1.4beta7
([OPTIMIZATION_PLAN.md](OPTIMIZATION_PLAN.md)). Each optimization keeps
the arithmetic the program performs, operation for operation, and has a
name for `--opt=`, so that one can be turned off to find out whether it
causes a difference or is not worth its cost. `--mode=oxide` runs them
all; `--opt=none` runs none, `--opt=NAME,...` exactly those named, and
`--no-opt=NAME,...` all but those named. vcl mode never optimizes, and
`--opt=` or `--no-opt=` without `--mode=oxide` is an error.

| Optimization | Improves |
| --- | --- |
| `loops` | Loops without `--LoopCS`. A loop vcl would software-pipeline if it had the hint is pipelined as if it had `--LoopCS 1,0`, which every such loop satisfies: it runs at least once, and no store runs past its last iteration. A loop stays as it is when pipelining does not shorten its iterations, when the search finds no schedule within its work budget, when the program would no longer fit in VU1 micro memory, or when it has `--LoopCS 0,0`. The budget counts the scheduler's work, not time, so the output is the same on every host: on a large loop body a search takes about two seconds at most, where an unbounded one can take minutes. A loop that reads Q, P or a flag set before it gets the `entry` fix's wait, and is left alone when a branch to its label could pass the wait. `-L` turns it off with the rest of loop pipelining. |
| `unswitch` | Loops that test a mode the loop never changes, such as a lighting or vertex format flag the host sets for a batch. A branch on registers that the loop's usual path does not write (`ibeq`, `ibne`, `ibgtz`, `iblez`, `ibltz`, `ibgez`) is decided once before the loop, which then runs in one of two copies of its usual path, each without that branch or the others with the same test. The code between the tests joins into longer streams, which schedule better, and a copy that becomes a loop of one block can be pipelined. The *special cases* stay as one copy: an arm of a branch that leads to an inner loop the other arm does not reach (a clipping pass, say). Code that enters the usual path from outside it, a special case included, tests the mode again, so a special case may change it. Each copy that tests a second such mode gets a copy per outcome of it too, four copies in all, and no more. The program grows by the copied code: when four copies would not fit in VU1 micro memory it gets two, when two would not it is compiled without unswitching, and `--no-opt=unswitch` turns it off for programs that must stay small. |
| `exit` | The end of a program and of each `--cont` stream. vcl puts the E bit and its delay slot on rows after the last instruction, usually both empty. With `exit` the last instruction runs in the delay slot when the hardware allows it there, and otherwise carries the E bit itself; the program is a row and a cycle shorter. An XGKICK before `--barrier` and an exit takes the E bit the same way. No instruction moves, and with `-e` nothing changes. |

```sh
oxide-vcl --mode=oxide -oskin.vsm skin.vcl              # every optimization
oxide-vcl --mode=oxide --opt=none -oskin.vsm skin.vcl   # fixes only
oxide-vcl --mode=oxide --no-opt=unswitch -oskin.vsm skin.vcl   # all but unswitch
```

A loop that `unswitch` copies:

```text
loop:
    lq      value, 0(base)
    ibeq    mode, vi00, plain     ; mode is never written in the loop
    mul     value, value, scale
plain:
    sq      value, 0(base)
    iaddiu  base, base, 1
    isubiu  count, count, 1
    ibne    count, vi00, loop
```

becomes, in effect, one test of `mode` before the loop and two loops of
one block each: one with the `mul`, one without it. Their labels are the
loop's with `__u0` and `__u1` after them.

### Examples

```sh
oxide-vcl -oskin.vsm skin.vcl                 # compile to a file
oxide-vcl -uSkin -U -oskin.vsm skin.vcl       # unique labels with prefix Skin
oxide-vcl -G -Iinclude -oskin.vsm skin.vcl    # preprocess, then compile
oxide-vcl -g -Ivu1 -oskin.vsm skin.vcl        # masp source
oxide-vcl -G --preprocess-only skin.vcl       # see the preprocessed source
oxide-vcl -m0 -jskin.h -oskin.vsm skin.vcl    # MPG packet plus label header
cat skin.vcl | oxide-vcl -q > skin.vsm        # filter mode
```

### Exit status and messages

Messages go to standard error in the `file(line): severity: message` form
that editors and IDEs read, with the source line, a caret under the token at
fault and, where there is one, a suggested fix. Both modes print them the
same way; they are oxide-vcl's own, not vcl 1.4beta7's text.

```text
skin.vcl(12): error: unknown instruction 'ad'
  12 |     ad res, src, src
     |     ^^
     = help: did you mean 'add'?
oxide-vcl: 1 error; no output written
```

Messages without a source line start with the file name, or with
`oxide-vcl:` for the command line and files. Severities are `error`,
`warning` and `note` (`-q` hides notes). A failed compile ends with the
number of errors.

The exit status follows the mode. vcl mode exits 0 on success and 255 on
any failure, as vcl does. (vcl exits 0 after rejecting a copy into the
constant register VI00 or VF00, though it writes no listing; oxide-vcl fails
that run like any other.) Oxide mode exits 0 on success, 1 when the program
fails to compile, a file can't be read or written, or a warning fails the
run, and 2 for a bad command line.

`--warnings-as-errors` fails a run that prints any warning, after printing
every warning and error: each warning prints as an error, marked
`[--warnings-as-errors]`.

A failed run never writes its output: there is no assembly on standard
output, and no `-o` listing or `-j` header, and a file from an earlier run is
left as it was. Oxide mode also writes the listing and the header together,
through temporary files beside them, so a failure partway leaves neither.
With make, use `-o` rather than redirecting standard output: the shell
creates a redirected target even when the compile fails, and its new
timestamp makes the target look up to date.

```make
%.vsm: %.vcl
	oxide-vcl --mode=oxide --warnings-as-errors -o$@ $<
```

### Reading the output

```text
__v_DEMO_4:
         add           VF02,VF01,VF01           NOP
         mul           VF02,VF02,VF01           NOP      ;	STALL_LATENCY ?3
;	iCount=2
```

- `__v_<name>_<n>:` labels each block of straight-line code; `<name>` is the
  `-u` name or the file name.
- Each line is one pair: upper instruction, then lower instruction.
- `STALL_LATENCY ?3` marks a pair that waits 3 cycles for a result.
- `iCount` is the number of pairs.
