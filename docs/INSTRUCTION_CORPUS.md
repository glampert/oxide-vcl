# Descriptor-driven instruction corpus

vcl 1.4beta7 accepts 379 of 385 controlled standalone programs generated from
its instruction descriptor table. The other six produce ordinary compiler
diagnostics. Every eligible descriptor index from 0 through 167 has at least
one accepted program. There were no emulator timeouts or exceptions.

The corpus establishes broad source acceptance and concrete output
observations. It does not establish VU execution equivalence, complete operand
coverage, or coverage of every scheduler interaction. Descriptor indices
identify entries in the extracted source table; generalized spellings resolve
to other concrete descriptors, so they are not observed runtime MUID values.

The port accepts all 379 successful programs, rejects all six negative
controls, and produces the same normalized pairs as vcl 1.4beta7 for all 379.
Pair normalization removes formatting and comments and folds opcode case; it
keeps issue slots, registers, operands and modifier bits, including E, so a
different register choice or end-bit placement stays visible. The regression
test is `crates/oxide-vcl/tests/oracle_instructions.rs`.

## Reproduction and retained evidence

`local/tools/probe_instructions.py` reads `local/evidence/tables/instructions.json`
and checks the original executable's SHA-256 before generating inputs.

```sh
# Generate inputs or refresh comparisons using retained original observations.
python3 local/tools/probe_instructions.py --candidate target/release/oxide-vcl

# Execute missing original observations for an inclusive descriptor range.
local/analysis/runtime/venv/bin/python local/tools/probe_instructions.py \
  --run --candidate target/release/oxide-vcl --first 0 --last 40

cargo test --test oracle_instructions
```

Running the original requires the host permission Unicorn needs. Each program
runs in a fresh Qiling root filesystem with its own evidence directory. The
guest receives `-t1 -uINSTRUCTION`, with a 30-second wall limit and a simulated
CPU-alarm scale of 25. A timeout, missing guest exit, or emulator exception is a
blocker, never a successful compilation.

All of the following is local and not tracked. `local/evidence/inputs/instruction-*`
holds generated sources; the matching `local/evidence/oracle/instruction-*/`
directories hold the source, original stdout, stderr and run metadata. Native
stdout, stderr and normalized comparisons are under
`local/evidence/instruction-comparison/`. The manifest
`local/evidence/analysis/instruction-probes.json` records source hashes,
observations and coverage by descriptor index, and the identity of the native
candidate compared (the binary is copied into a SHA-256-keyed directory first).

## Input construction

Programs declare real VF/VI input registers and hardware resources, initialize
the physical register pools, and explicitly export instruction results.
Variants exercise concrete and generalized forms, full and `.xyz` masks,
broadcast/scalar syntax, and generalized I/Q operands. Memory templates use a
declared address input and a controlled displacement. MAC/status reads receive
ALU producers; FSEQ receives a divide producer as well. Seeded variants
exercise WAITP after ESADD and FSSET after an ALU producer.

Synthetic instructions, branch/control-flow descriptors and internal abstract
moves are skipped; their effects need control-flow or allocator state tests. A
NOP source has no non-NOP output opcode by definition. All other accepted cases
keep a non-NOP output opcode; the manifest records the observed set, since
acceptance alone does not show that an operation survived dead-code
elimination.

The six rejected variants:

| Case | Original diagnostic cause |
| --- | --- |
| Generalized MAX with Q | Derived `maxq` descriptor is absent. |
| Generalized MINI with Q | Derived `miniq` descriptor is absent. |
| Unseeded WAITP | No ESADD/EFU writer establishes `tt_nP`. |
| Unseeded FSSET followed by FSAND | Sticky observation also requires an ALU status producer. |
| ISW.xyz with an x-only address selection | Instruction and address masks disagree. |
| ISWR.xyz with an x-only address selection | Instruction and address masks disagree. |

## Behaviour these observations establish

Generalized I/Q operands have a scalar component mask after descriptor
resolution; keeping the initial vector mask would introduce false I.y/Q.y
dependencies. Generalized MADD/MSUB and accumulator variants carry an implicit
ACC read, which does not trigger the restriction on explicitly placing ACC
after the destination.

The ILWR/ISWR hook (inferred from vcl 1.4beta7) rewrites ILWR/ISWR to ILW/ISW,
changes the address operand to immediate-address class 31, and installs a
signed zero displacement. The parser applies this normalization before
data-flow analysis.
