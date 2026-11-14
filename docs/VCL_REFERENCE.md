# VCL reference

A guide to writing vector unit (VU) programs for `oxide-vcl`, this project's
compatible reimplementation of the VCL compiler (vcl 1.4beta7). It covers the
source language, every `--` annotation and `.` directive, and the command
line.

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
ignored. The `--LoopAbs` message says `value must be >=2`, vcl's wording,
although 2 is refused. A negative `--LoopExtra` is an error.

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
| `-G` | Run the C preprocessor first: `gcc -P -E -C -x assembler-with-cpp`. |
| `-g` | Run gasp first, for macros and source line information: `gasp -p -s -c ';'`. |
| `-g+` | Run gasp with its alternate syntax: `gasp -p -a -s -c ';'`. |
| `-I<dir>` | Pass an include directory to the preprocessors. |
| `-W` | Skip all preprocessing. |
| `-K` | Keep the preprocessors' temporary files. |
| `-L` | Do not pipeline loops. |
| `-d` | Plain in-order code only; implies `-L`. |
| `-C` | Skip the code size reduction pass. |
| `-M` | Timid memory access mode: keep every pair of loads and stores that touch overlapping fields in program order, except in software-pipelined loops (see [Memory order and tags](#memory-order-and-tags)). |
| `-q` | Quiet: print only warnings and errors. |
| `-h`, `--help` | Print the help screen. |
| `-v`, `--version` | Print oxide-vcl's version and build date on standard output. Not in vcl 1.4beta7, which rejects both. |
| `--mode=vcl` | Generate the code vcl 1.4beta7 generates, with its exit statuses. The default. |
| `--mode=oxide` | Enable oxide-vcl's own behaviour, which breaks compatibility with vcl 1.4beta7: its exit statuses (see [Exit status and messages](#exit-status-and-messages)), every fix (`--fix=all`), openvcl's math functions in `loi` expressions ([Math functions in `loi`](#math-functions-in-loi---modeoxide)), and future optimizations. |
| `--fix=<names>` | Enable fixes for vcl 1.4beta7's defects, by name, separated by commas; `--fix=all` enables every one. May be repeated. See [Fixes](#fixes). |
| `--warnings-as-errors` | Fail the run when it prints a warning, from the command line or the program. Not in vcl 1.4beta7. |

The preprocessing stages run in the order `-G`, `-g`, `-g+`, each on the
previous stage's output; any combination may be given.

`--mode`, `--fix` and `--warnings-as-errors` are oxide-vcl's own options;
vcl 1.4beta7 has no long options and rejects them as unknown switches.

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
| `operands` | An ILW, ISW, ILWR or ISWR memory operand that vcl rejects only for its length (`ilw.x t,0(p)`, `ilwr.x t,(p)`, `0(p):tag`). With the fix it compiles as its longer spellings (`0(p)x`, `0(ptr)`) do. Operands with a wrong shape are still rejected: a capital field mask, a field mask after an LQ or SQ operand, or text before `(p++)`. |

```sh
oxide-vcl --fix=end -oskin.vsm skin.vcl       # vcl's output, with the E bit fix
oxide-vcl --fix=all -oskin.vsm skin.vcl       # every fix, vcl's exit statuses
```

### Examples

```sh
oxide-vcl -oskin.vsm skin.vcl                 # compile to a file
oxide-vcl -uSkin -U -oskin.vsm skin.vcl       # unique labels with prefix Skin
oxide-vcl -g -Iinclude -oskin.vsm skin.vcl    # expand gasp macros first
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
