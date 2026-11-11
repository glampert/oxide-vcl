# VCL source language: implementation notes

This chapter records the exact source-language rules of vcl 1.4beta7 that
the port reproduces, with the evidence behind them. For a user's guide to
writing programs, with examples of every annotation and directive, see
[VCL_REFERENCE.md](VCL_REFERENCE.md).

The rules are inferred from vcl 1.4beta7 (SHA-256
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`) and
checked by runs of it; the fixtures named below are under
`local/evidence/oracle/` (local, not tracked). The original parses the whole
input itself; it does not take tokens from an external assembler.

Old trailing field syntax (`valuew` or `value.w`) differs from new bracket
selection: in new syntax `model[0]` is a vector name and `model[0][w]` selects
its W field. See [PARSER_FIELDS.md](PARSER_FIELDS.md) for the exact rules.
Memory field selections and scalar VI dependencies are distinct: `ilw.w
count,0(base)` selects W in memory while reading the scalar base address; see
[ADDRESS_DEPENDENCIES.md](ADDRESS_DEPENDENCIES.md).

## Instruction and directive tables

The instruction table has 214 entries in the original's order; an entry's
index is its instruction ID, and several passes compare IDs directly (the
raw-only NOP is 12, `waitp` and `waitq` are 115 and 116; the ones the port
compares are named in `instruction::id`). The first twelve entries are the
generalized new-syntax FMAC operations. The directive table has exactly 28
entries.

`local/tools/extract_tables.py` (local, not tracked) extracts both tables from
`local/vcl` and writes `local/evidence/tables/{instructions,directives,operand_classes,manifest}.json`.
The port's copy is generated into `crates/vcl/src/isa/instruction_data.rs`,
with every coded field spelt with the names `crates/vcl/src/isa/instruction.rs`
defines: operand classes (`class::VECTOR`), field-selection modes
(`MaskMode`), issue pipes, functional units, memory order flags, kinds and
subtypes, and immediate selectors (`ImmediateKind::signed(11)`):

```sh
python3 local/tools/extract_tables.py   # reads local/vcl
cargo test --lib parser::tests
```

## Lexical rules

Whitespace and commas delimit tokens. A comma ends an operand; two
consecutive commas or a leading comma fail tokenization. Tokens of one operand
are joined, so `4 ( pointer )` reads as `4(pointer)`.

Semicolon and `//` start line comments; `/* ... */` can span lines. `#` is an
ordinary token character, so `add b,a,a # note` reads a register named
`a#note` and is rejected (`scheduled-comment`, `scheduled-comment-slashes`,
`scheduled-comment-hash`).

A colon ends a label. A line starting with an isolated `=` continues the
previous instruction; a continuation with no instruction before it, and a new
label on a continued instruction, are errors. Lines between `----*` and
`*----` are passed through as assembler text. With gasp preprocessing, the
lexer also reads gasp's source-location comments.

Instruction names, hardware aliases, directives and keyword arguments compare
case-insensitively. Symbolic register names use
`abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789[]_$`, must be
shorter than 63 bytes, and must not collide with an instruction name or a
hardware resource: every resource (`acc`, `i`, `q`, `p`, `r`, `clip`,
`clipa`, `mac`, `ext`, `tt_np`, `stat_*`) is refused as a VF name, and all
but the scalars `i`, `q`, `p` and `r` as a VI name (`name-*` probes).
`.name`'s `_CodeStart`/`_CodeEnd` globals print before the program's own
`.global` labels.
Physical aliases such as `vf1`/`vf01` are canonicalized.

## Interfaces

```text
--enter
in_vf a(vf01)
--endenter

add b, a, a

--exit
out_vf b(vf02)
--endexit
```

This binding form is accepted in `sched-add-symbol`. `in_vf a, vf01` is not an
alternative spelling: it gives a second operand to a declaration that takes
one. `in_vi`/`out_vi` are the integer forms; `in_hw_acc`, `in_hw_i`,
`in_hw_q`, `in_hw_p`, `in_hw_r`, `in_hw_clip`, `in_hw_mac` and the matching
`out_hw_*` declare hardware resources (`in_hw_i I`). `xin_hw` and `xout_hw`
create external dependencies. All of these are entries in the instruction
table, not directives, and none emits an opcode.

Interface context is validated as a small state machine:

```rust
enum InterfaceContext { Ordinary, Enter, Exit }

match synthetic {
    Enter    if context == Ordinary => context = Enter,
    EndEnter if context == Enter    => context = Ordinary,
    Exit     if context == Ordinary => context = Exit,
    EndExit  if context == Exit     => context = Ordinary,
    Input    if context == Enter    => (),
    Output   if context == Exit     => (),
    _ => reject_invalid_context(),
}
```

At least one enter and one exit (`--exit`, `--exitm` or `--cont`) must be
present, and the final context must be ordinary. Ordinary instructions inside
an interface block are rejected when streams are built.

`--exitm <macro text>` starts an exit block that ends with the given assembler
text; the text may be empty, but `--endexit` is still required, and `--exitm`
inside an exit block is rejected. `--cont` ends ordinary code as an exit
boundary without an output block; after an exit it is an error. `--barrier`
starts a new stream fenced from the previous one. The loop hints are
`--LoopAbs`, `--LoopExtra`, `--LoopMinCountMinSafe` and its synonym
`--LoopCS`:

| Hint | Valid operands |
| --- | --- |
| `--LoopCS count, safe` | `count` > 0, `safe` >= 0 |
| `--LoopExtra n` | `n` >= 0 |
| `--LoopAbs n` | `n` >= 3 |

An invalid argument is reported and the compile continues, with the
arguments before it applied (observed from vcl 1.4beta7 in
`loop-hint-count-zero`, `loop-hint-safe-negative` and `loop-hint-abs-two`):
`--LoopCS 0,0` sets nothing, `--LoopCS 1,-1` keeps the count, and
`--LoopAbs 2` is ignored, with the message "value must be >=2". A negative
`--LoopExtra` is reported the same way, but the original then fails in loop
scheduling (`loop-hint-extra-negative`); the port rejects it.

## Masks, scalar operands and new syntax

The component mask uses **x=1, y=2, z=4, w=8**. An instruction's mask mode is
one of: no mask, arbitrary selection, fixed XYZ, or exactly one selected
component.

Old syntax accepts fields on operands, as in `add b.xy,a.xy,a.xy`; without an
instruction-level suffix the full-vector operands must agree. Broadcast forms
(`addx`, `muly`, `subq`) are separate instructions. Scalar operands use the
suffix form, as in `div Q,a.x,b.w`.

`.syntax new` also searches the twelve generalized FMAC entries, so a generic
`add` or `mul` is resolved once its operand types are known. A bracketed field
(`source[x]`) is a scalar; I and Q are scalars too. The concrete instruction is
chosen like this (inferred from vcl 1.4beta7):

```rust
let mut concrete = generic_mnemonic.to_owned();
if destination_is_acc && !concrete.ends_with('a') {
    concrete.push('a');
}
match third_operand {
    ScalarI => concrete.push('i'),
    ScalarQ => concrete.push('q'),
    Component(X) => concrete.push('x'),
    Component(Y) => concrete.push('y'),
    Component(Z) => concrete.push('z'),
    Component(W) => concrete.push('w'),
    FullVector => (),
}
let definition = find_instruction(&concrete, OldSyntax)?;
```

ACC may only be the first vector operand, and a scalar only the third.
`.syntax old` restores classic lookup.

`[M]`, `[E]`, `[D]`, `[T]` flags (bits 1, 2, 4, 8) are decoded separately from
component masks. Scheduled instructions accept D and T; labels and raw
instructions accept M, D and T. No source context accepts E: exits set it.
Raw `nop[e] nop` is rejected (`raw-endbit`).

An optional `~` prefix is kept on the instruction and has no effect on the
output. A `number..` prefix gives a raw-loop stage; values over 32 fail, and
`+..` is a special value. Without a prefix the stage is zero, also inside
`.vsmloop`. A raw loop whose maximum stage is zero, and a positive prefix on
NOP, are rejected; see [PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md).

## Memory operands and tags

| Operand class | Port name (`class::`) | Source shape |
| --- | --- | --- |
| `OP_V_EA` | `ADDRESS` | `(integer_register)` |
| `OP_V_EA_IMM` | `ADDRESS_OFFSET` | `displacement(integer_register)` |
| `OP_V_EA_REG` | `ADDRESS_REGISTER` | `(integer_register)x` |
| `OP_V_EA_IMM_REG` | `ADDRESS_REGISTER_OFFSET` | `displacement(integer_register)x` |
| `OP_V_EA_INC` | `ADDRESS_INCREMENT` | `(integer_register++)` |
| `OP_V_EA_DEC` | `ADDRESS_DECREMENT` | `(--integer_register)` |

An omitted displacement is zero. Integer load/store field forms need a
component on the instruction or on the operand, and conflicting selections
fail. The base register may be symbolic; increment and decrement effects come
from the instruction's operand records.

A colon suffix carries tags: `lq value,0(pointer):array` or
`jr destination:target0:target1`. At most one operand per instruction may be
tagged.

- On an indirect branch, each tag names a possible target label.
- On loads and stores, a shared tag orders a load against a store whose
  fields overlap; two stores with a shared tag stay free to swap. Observed from
  vcl 1.4beta7: a store and a later load through different base registers swap
  with no tags (`memory-tag-none`) or different tags (`memory-tag-disjoint`)
  and stay in order with a shared tag (`memory-tag-shared`); two stores with a
  shared tag swap (`memory-tag-store-store`); disjoint fields swap
  (`memory-tag-fields`). The same holds across iterations of a software-pipelined loop
  (`memory-tag-loop-*`); see [MEMORY.md](MEMORY.md).

Direct branch targets come from the immediate expression. A missing direct or
tagged target fails flow construction, and duplicate labels fail validation.
Labels after the last instruction are kept for output but cannot carry
modifiers.

## Directives

The 28 directives, with the behaviour observed from vcl 1.4beta7. Spellings
not in this table are errors; in particular there are no `.name_vf`,
`.name_vi`, `.vcl`, `.vclloop` or `.work` directives, although the work-area
messages say "work".

| Directive | Behaviour |
| --- | --- |
| `.vu` | Accepted; no effect. |
| `.mpg address` | Set the MPG target address (decimal). |
| `.vsm [reorder\|noreorder]` | Enter raw two-slot mode; default `noreorder`. |
| `.endvsm` | Leave raw mode. |
| `.vsmloop [reorder\|noreorder]` | Enter raw loop mode; nesting is rejected. |
| `.endvsmloop` | Close the raw loop and restore the previous mode. |
| `.init_vf list` / `.init_vi list` | Make the listed physical registers allocable. |
| `.rem_vf list` / `.rem_vi list` | Make them unallocable again. |
| `.init_vf_all` / `.init_vi_all` | Make every register except VF00/VI00 allocable. |
| `.vu_work_area device,address,size` | Request work memory; always fails (below). |
| `.name name` | Set the program's external name. |
| `.syntax new\|old` | Change operand syntax and instruction lookup. |
| `.align ...` | Accepted and skipped. |
| `.global name` | Export a label. |
| `.noaligns` | Disable alignment directives. |
| `.noebits` | Prints the E-bit message but disables alignment, like `.noaligns`. |
| `.raw`, `.endraw`, `.rawloop`, `.endrawloop` | Exact aliases of the `.vsm` forms. |
| `.unique [name]` | Make local labels unique, optionally with a new prefix. |
| `.show list` / `.hide list` | Push a name visibility scope. |
| `.endshow` / `.endhide` | Pop it; the wrong kind is an error. |

No register is allocable until a directive allows it. Register lists accept
`vf03,vf04` and inclusive `vf03-vf31`; a decreasing range warns and is
swapped. Symbolic, unallocable, missing and malformed names are rejected, and
a bare number (`.init_vf 3`) is not a register.

`.vu_work_area` recognizes `vu0` (capacity 256) and `vu1` (capacity 1024), in
VCL's allocation units; `core` is recognized and fails as unimplemented.
Address must be nonnegative, size positive and address+size within capacity,
parsed as base-10 `strtol` (`0x20` reads as 0). Every valid request then fails
because vcl 1.4beta7 never installs the memory pool; see
[MEMORY.md](MEMORY.md).

Opening a raw block inside a raw loop is rejected; opening one while already
raw warns and reapplies the mode. A raw loop restores the code mode it saved
when it closes.

Visibility directives push a scope of names, numbered from 1 in the order
they open across the file. While scopes are open, each symbolic register name
is resolved from the innermost scope outward: the first `.hide` scope that
lists it, or `.show` scope that does not, renames it to `<number><name>`
(`t` becomes `1t`); a name every scope lets through is unchanged. Lists match
case-sensitively. Physical names, hardware registers and names with a leading
underscore are never renamed. An unmatched closing directive warns and
succeeds, closing the wrong kind fails, and an unclosed scope warns at the
end. Observed from vcl 1.4beta7 in the `visibility-*` runs, which the port
matches (`crates/oxide-vcl/tests/visibility.rs`).

## Operand records

Each instruction lists up to twelve operands: explicit source operands and
implicit effects (ACC, Q/P/R/I, MAC, clip and status flags). Each record gives
the operand class, read or write, whether it is implicit, a signed dependency
offset, whether it follows the component mask, an immediate kind and an
optional link to an earlier operand.

The signed offset describes when the operand is read or written relative to
issue: input reads are usually **-1**, a VF arithmetic result **4**, a VI ALU
result **1**, Q results **7** or **13**. An EFU instruction also occupies its
unit for its throughput (EATAN 53, EEXP 43, ESIN 28). Functional units are
FMAC, IALU, MOVE, LS, EFU, DIV, RAND, EXT and BRANCH; an incrementing load uses
both LS and IALU.

Status flags are placeholder operands expanded later: ALU arithmetic writes
the non-sticky status at latency four and reads the sticky status; divide
status follows the Q result's latency; a flag test with a numeric mask depends
only on the bits it selects.

## Immediate expressions

| Operators | Precedence | Interpretation |
| --- | ---: | --- |
| unary `~`, `!`, `+`, `-`, `#` | 30 | Complement, logical not, identity, negate, absolute value |
| `**` | 26 | Recognized; no numeric evaluation |
| `*`, `/`, `%` | 25 | Multiply, divide, integer remainder |
| `+`, `-` | 20 | Add, subtract |
| `<<`, `>>` | 19 | 32-bit shifts; count masked to five bits |
| `&` | 18 | Bitwise AND |
| `^` | 17 | Bitwise XOR |
| `\|` | 16 | Bitwise OR |
| `&&` | 15 | Integer logical AND; both sides evaluated |
| `\|\|` | 14 | Recognized; no numeric evaluation |

Equal precedence associates left to right. Numeric `2**3` and `1||0` fail
with "unknown binary operation". The port returns errors for integer division
by zero and signed-division overflow.

Numbers are unsigned 32-bit, signed 32-bit or single-precision float; a binary
operation promotes to the larger type. Decimal integers are signed, `0x` is
unsigned, `0b` signed binary, and `0z` reinterprets hexadecimal bits as a
float. `loi`'s float operand also reads `0x` as float bits, so `loi
0x3f800000` loads 1.0. Decimal fractions and exponents (`1e+2`) are floats.
The scanner accepts underscores but converts with `strtod`, so `1_2.5`
evaluates to 1.0.

Integer arithmetic wraps at 32 bits. Float-to-integer conversion truncates
toward zero; NaN and out-of-range signed conversions give the x87 indefinite
integer, and unsigned conversion takes the low 32 bits of the signed 64-bit
result. The port computes float intermediates in `f64` where the original uses
x87 extended precision, so exceptional rounding cases may differ.

If any leaf is symbolic the whole expression is kept as written: `LABEL+2*3`
is emitted as `LABEL+2*3`, and a symbolic expression containing `||` is kept
although numeric `||` fails.

```rust
use vcl::parser::expression::{evaluate, Evaluation};

match evaluate("LABEL + 2 * 3")? {
    Evaluation::Number(value) => { /* convert to operand's target type */ }
    Evaluation::Unresolved { source, symbols } => {
        // source == "LABEL+2*3"; symbols == ["LABEL"]
    }
}
# Ok::<(), vcl::parser::expression::ExpressionError>(())
```

The operand's immediate kind, not the literal's type, sets the target type.
Signed targets print in decimal and others as eight-digit hexadecimal.
Effective-address displacements print numeric integers in decimal, keep
unresolved expressions, and print floats with `%1.9e`. A displacement may
nest parentheses: `lq vf02,(0x10+2)(vi01)` emits `18(VI01)`.

Known IADDI values become IADDIU for a nonnegative value and ISUBIU with the
magnitude for a negative one: `iaddi b,a,-(2+3)` emits `isubiu
VI02,VI01,5`, while `iaddiu b,a,-1` emits `iaddiu VI02,VI01,0xffffffff`, and
IADDIU's 32768 emits `0x00008000`. The `expr-emit-*` and `expr-fix-*`
fixtures cover these cases.

A parsed instruction carries no compiler-created copy marker; recognized
copies become internal moves later ([COPIES.md](COPIES.md)).
