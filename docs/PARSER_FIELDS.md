# Legacy fields, indexed names, and EFU build normalization

Static observations are inferred from vcl 1.4beta7; small runs of it
corroborate the rules.

## Legacy trailing fields

For the old scalar decoder (class 3), a suffix helper scans the maximal
trailing case-insensitive x/y/z/w string, accumulates its bits, and rejects a
repeated bit. A scalar requires one bit. A second helper removes that suffix
and an optional preceding dot from the copied register token.

So `mtir result,valuew` and `mtir result,value.W` refer to the same register
and W field. For fixed-field forms, `addx b,a,cx` selects X and uses register
`c`, not `cx`.

The same helpers process ordinary and fixed-field old vector operands, and the
port handles those classes the same way:

```rust
// Old source: valuexy -> register value, fields XY
// Old source: value.xy -> register value, fields XY
// Reject valuexx: one lane occurs twice.
```

The syntax is ambiguous: in old syntax `matrix` in a vector operation means
`matri` with X selected. A probe that declares `matri` confirms this. New
syntax keeps `matrix` whole.

Vector interface operands (`in_vf`, `out_vf`) strip the same trailing string
from the name (observed from vcl 1.4beta7, `field-letters-*` probes):
`in_vf caty(vf01)` binds `cat`, so the program's uses of `caty` all refer to
one register. Only a dotted suffix selects an interface operand's fields:
`out_vf res.xyz(vf02)` reads XYZ, while `out_vf resxyz(vf02)` reads all of
`res` and is rejected after an `add.xyz res, ...` (`res.w` used before set).
Integer interface operands keep the whole name (`in_vi county`) and take no
dotted fields: `in_vi cnt.x(vi01)` is rejected with `can't use '.' in the name
of a register : 'cnt.x'`, the error vcl gives any invalid character (`#` in
`a#note`). A name made only of lane letters (`xyzw`) stays whole in every
class.

New syntax keeps every name whole, in instructions and interfaces alike
(`field-letters-new-*`): `caty` is `caty`, nothing warns in either mode, and a
glued broadcast such as `scalex` is just another name. Fields are selected by
brackets (`m[x]`) and by the instruction suffix (`out_vf.xyz res(vf02)`); a
dotted interface name (`out_vf res.xyz(vf02)`, `in_vf src.xyz(vf01)`) is
rejected with the invalid-character error. Because each syntax reads a name
its own way, a name ending in a field letter does not cross a `.syntax new`:
old-syntax `caty` is `cat`, and new-syntax `caty` is another register.

When an instruction has an explicit mask, a selection that differs from it is
overridden by that mask (vcl warns `getNextInstruction: forced mod bits!`):
`move.xyzw copy, first` writes all of `cop`. Without an explicit mask the
selection must agree with the other operands', or the line is rejected (`bad
mod bits`).

A name read this way is almost always a misspelling. In vcl mode the port
compiles it as vcl does and warns once per name, at its first line
(`'copy' is read as register 'cop' with field y: ...`); in oxide mode it is an
error. Selections that are clearly meant do not warn: a broadcast (`scalex`),
a dotted suffix (`res.xyz`) and a physical register (`vf03xyz`)
(`crates/oxide-vcl/tests/field_letters.rs`).

## Numeric brackets are register names

The new-syntax bracket decoder recognizes a final bracket group only when its
contents are lane letters. Numeric contents make it return false, and the
generalized class 38 branch of the operand decoder then keeps the entire token
as a vector register name. Recognized scalar bracket selections must have
exactly one distinct lane.

| Source | Register identity | Selection |
| --- | --- | --- |
| `model[0]` | `model[0]` | ordinary vector |
| `model[0][w]` | `model[0]` | W broadcast |
| `model[1]` in `add.x` | `model[1]` | X vector mask, not broadcast |
| `model[xy]` in generalized third operand | rejected | multiple scalar lanes |

A shared `bracket_field` helper recognizes actual lane groups; the destination
restriction and opcode derivation use the same distinction, so an indexed third
vector operand in `add.x` is not classified as ADDX. For example
`madd mvp[0],viewProj[3],model[0][w]` parses.

The bracket helper ORs repeated lane bits, unlike the old suffix helper's
duplicate check. The port keeps this difference between the two syntax modes.

## EFU source mask hook

The descriptor describes the source of EFU instructions such as `esadd` as
FL4, but vcl 1.4beta7 applies a hook before dependency binding that writes the
source operand's mask from the descriptor's hook argument:

| Argument | Action | Examples |
| --- | --- | --- |
| 1 | Preserve scalar selection | EATAN, ERCPR, ERSQRT, ESIN, ESQRT, EEXP |
| 2 | Set XY | EATANXY |
| 3 | Set XZ | EATANXZ |
| 4 | Set XYZ | ELENG, ERLENG, ERSADD, ESADD |
| 5 | Set XYZW | ESUM |

The port applies this normalization before flow analysis, alongside the
ILWR/ISWR hook. ESADD has no W dependency; ESUM requires W; ESADD rejects an
absent Z.

## Tests and probes

`local/tools/probe_parser_forms.py` captures or reuses 27 originals run with
`-t1 -uPARSER`. Twenty are accepted and match the port's normalized pairs;
seven are rejected by both. The probes cover all four MTIR fields, dotted and
undotted forms, ordinary old vector suffixes, old/new trailing-name ambiguity,
indexed destinations and third operands, nested scalar brackets, six fixed EFU
masks, and negative controls for malformed or missing fields.

Sources and original output are in `local/evidence/inputs/parser-*` and
`local/evidence/oracle/parser-*/`; the manifest is
`local/evidence/analysis/parser-forms.json` (all local, not tracked). Unit
tests assert register identities, masks and derived opcodes, including
`model[1]` staying a vector in a masked ADD. Integration tests check the
twenty schedules and seven rejections.

A new-syntax output declaration cannot be an indexed name such as `result[0].x`
(vcl 1.4beta7 rejects it). To update one field of an indexed register, declare
the full incoming result, update X, and export the same full name.

```sh
local/analysis/runtime/venv/bin/python local/tools/probe_parser_forms.py --run
cargo test
cargo clippy --all-targets -- -D warnings
```

Allocation and timing differences are covered in
[ALLOCATION_REPAIR.md](ALLOCATION_REPAIR.md).
