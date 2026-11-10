# Assembly output, label headers, and preprocessing

Statements about vcl 1.4beta7 internals are inferred from it. Probes use a
fixed unit name and `-t1`.

## Assembly traversal and offsets

The output driver writes the optimizer-loop preamble when there are eligible
loop records, writes the emitter banner, clears register-usage counters, and
writes the assembly prologue. In physical stream order it stores the current
cumulative pair count on each stream, emits the stream, and advances the count
by the number of printed pairs. The footer adds the terminal directives,
`iCount`, and register statistics. Source lines, assembly labels, optimizer
comments, and raw text can occupy output lines without advancing the count.

The prologue can emit `.vu`, alignment, exported names, and the named
`CodeStart` and `CodeEnd` globals. MPG output suppresses the normal `.vu`
directive and, when named, adds DMA boundary names. Its packet preamble
includes `DMAret *`, three VIF NOPs, and `MPG <decimal>,*`. The footer handles
alignment, `CodeEnd`, trailing labels, and the closing MPG and DMA directives
as applicable. Stdout is wrapped with the five-line `VCL CODE` banner and five
trailing separators; the `-o` path writes the bare assembly. The label header
is built after assembly generation and written when `-j` is set.

## Three separate label decisions

1. The stream emitter admits an entry stream (start kind 2), a stream with an
   incoming reference marked for emission, or a stream carrying pass-through
   text. An invisible stream contributes zero pairs; its offset was already
   recorded.
2. Inside an admitted stream, the assembly emitter filters its label list. A
   label survives if the target lookup recognizes it, the exact export lookup
   recognizes it, or the label has its keep flag. The target lookup considers
   entry-successor names, branch operand text, and the flow monitor's recorded
   target list.
3. The header walks the retained label list in stream order. If the offset is
   nonnegative, it writes each non-null name except names beginning with the
   prefix `__v_`, in the format `"%-40s = %d\n"`. It applies neither the
   incoming-edge check nor the assembly label lookup.

The pass-through payload path writes a newline, the payload, and a newline with
a pair count of zero. An EXITM macro line is appended with a leading newline
and nine spaces.

In the saved header probe, `live` appears in assembly and is the only header
entry, at offset zero. Unreferenced `unused` is absent from both outputs. The
generated name `__v_AUDIT_5` occurs in an optimizer comment and has no header
entry. The distinction between the filtering stages comes from analysing the
binary; the probe records only the visible result. Input:
`local/evidence/inputs/audit-header-visibility.vcl`; original stdout and header:
`local/evidence/oracle/audit-header-visibility/` (local, not tracked).

## Dispatch formatting, flags, and statistics

The stream table writer keeps separate walked-row and printed-pair counts. The
optimizer comment's suffix is `printed [a b] walked [name]`, with the two
interior values loaded as signed shorts. The pair formatter checks the row's
skip flag: a skipped row must have both slots empty and writes nothing. Other
rows use two nine-space prefixes and two instruction fields padded to at least
50 characters. The per-instruction flags from both slots join the stream flags
before the upper slot is printed; the lower slot receives zero flags. The row
stores the stall count, and bits 0-2 of its stall-kind select `LATENCY`,
`THRUPUT`, and `RFWRITE` after the `STALL` prefix. The annotation ends in
` ?<count>`.

E exits (end kinds 2 and 3) place E at the second-to-last physical emission
row, including the formatter's padding row if one was added. Other ends place
stream end flags at the last physical table row. The first physical iteration
consumes entry flags. Flags are chosen before the pair formatter can skip the
row. The internal E check records that the iteration selected an E row and can
pass when no printed row contains E.

```rust
// Structural sketch for ordinary assembly output.
for index in table_start..emission_end {
    let row = row_or_padding(index);
    let flags = take_entry_flags() | terminal_flags_at(index);
    walked += 1;
    if row.skipped {
        assert!(row.is_empty());
        continue;
    }
    emit_pair(row, flags);
    printed += 1;
}
```

In the `external-vf-two` capture the optimizer comment records `3 [5 0] 6`; the
three printed pairs contain no E and the last ADD reports `STALL_LATENCY ?3`.
The same input with `-C` records `6 [5 0] 6` and prints `NOP[E]` at zero-based
index 4, with no stall annotation on the last ADD. `-C` bypasses both cap
insertion and compression, which accounts for the E loss. Captures:
`local/evidence/oracle/external-vf-two/stdout.bin` (default) and
`local/evidence/oracle/audit-external-vf-two-no-reduction/stdout.bin` (`-C`).
See [Final reduction](FINAL_REDUCTION.md).

Before printing, the driver resets register usage markers. Non-symbolic operand
formatting ORs the operand component mask into a usage byte on the physical
register. Statistics walk the 12 CPU pool slots, include pools with a nonzero
component count, and count base registers whose usage byte is nonzero, so a
four-component use of one VF register contributes one to that pool's count.
Symbolic formatting uses the original register name and does not update the
markers.

## Instruction-specific emitters

The slot writer chooses a formatting hook from the instruction descriptor. A
missing hook uses the generic formatter. A missing descriptor, or an
instruction with its nop byte set, prints a NOP. Generic formatting writes the
descriptor's mnemonic, adds flags and component modifiers, pads to column 14
within the field, then writes comma-separated nonimplicit operands.

Three hooks are nonzero in the shipped descriptor table (separate from the
parser hook):

| Hook | Descriptor names | Formatting behavior |
| --- | --- | --- |
| Branch hook | `ibltz`, `iblez`, `ibgtz`, `ibgez`, `ibeq`, `ibne` | If the instruction's reversal flag is set, use the reverse-sense descriptor named by the descriptor. A likely branch with the reversal flag errors. |
| Clip hook | `cliplw` | Write the literal format `clipw%s.xyz`, substituting the flag text for `%s`. |
| Move hook | `imove`, `limove`, `vmove`, `lvmove`, `uvmove` | Require a nonzero base move ID and a concrete ID. IDs `0xb3`, `0xb5`, and `0xb6` emit `iaddiu dst,src,0`, masked `move dst,src`, and masked `max dst,src,src`, respectively. |

Descriptor records: `local/evidence/tables/instructions.json` (local, not
tracked). See [Move lifecycle](EXTERNAL_ALLOCATION.md#generated-moves-are-a-different-subsystem).

## External preprocessing and location records

`-G`, `-g`, and `-g+` each enable one stage independently (`-g+` accepts
trailing text after the plus). Enabled stages run in this fixed order, each on
the previous stage's output; enabling both gasp options runs both passes.
`-W` and `-w` skip all stages. The port's `oxide-vcl` driver
(`crates/oxide-vcl/src/main.rs`) runs the same commands, forwarding the `-I...`
arguments in their original spelling and order; `-D` is handled by VCL itself.

| Stage | Option | Command |
| --- | --- | --- |
| 0 | `-G` | `gcc -P -E -C -x assembler-with-cpp` |
| 1 | `-g` | `gasp -p -s -c ';'` |
| 2 | `-g+` | `gasp -p -a -s -c ';'` |

Each stage writes its input to a temporary file, creates a second temporary
file for the result, runs the command as `program switches -I... input >
output` with no added shell quoting, and on zero status reads the output file
for the next stage. Temporary files are removed afterwards; `-K` keeps them.
vcl 1.4beta7 names them `/tmp/vcli_{name}.XXXXXX` and
`/tmp/vclo_{name}.XXXXXX` and runs the command through `system`; the port
creates its files in the system temporary directory and starts the program
directly.

The flag-acceptance fixture `local/evidence/oracle/cli-gplus-tail/result.json`
uses `-W`, so it checks option parsing only. A successful run of each external
tool has no dedicated fixture.

When gasp handling is enabled, the line processor passes semicolon location
lines to a location decoder (inferred from vcl 1.4beta7). The decoder reads up
to sixteen nonzero dot-separated location numbers. A depth increase of more
than one is an error. A one-level increase needs a pending include or macro
marker; the pending macro slot is tested first and uses tag kind 2, and the
pending include slot uses kind 1. The scanner recognizes `.include`
case-insensitively and remembers the following token in the include slot; an
ordinary candidate token can fill the macro slot. The decoder commits the new
depth and per-level counters after scanning the line. The resulting
include/macro location text has no dedicated fixture.
