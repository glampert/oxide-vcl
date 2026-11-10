# Register-web reconstruction

A source register name does not identify one allocation lifetime. vcl groups
connected operand occurrences into register webs, then assigns each unpinned web a
new virtual register. Reusing a name after an independent definition can
therefore produce a different allocation, including for source names such as
`vf10` when that base register is enabled for allocation.

This document describes vcl 1.4beta7. Evidence is static analysis (inferred from
vcl 1.4beta7) and runs of vcl retained under `local/evidence/oracle/web-*/`
(local, not tracked). `local/tools/probe_webs.py` reproduces eleven probes: eight
successful programs and three rejected partial-mask directive spellings.

## Pass and data structures

vcl's web driver clears all instruction operand web pointers, visits instruction
tables, runs its gather step, and patches operands in a final patch step.

Each operand initially has its own strand. A web entry holds:

| Field | Meaning |
| --- | --- |
| 1 | Assigned register, initially null |
| 2 | Original register identity |
| 3 | Forced base register, initially null |
| 4 | Forward pointer to merged terminal strand |
| 5 | Live terminal use count |
| 6 | Original strand number |
| 7 | Union of component masks |

vcl limits the strand array to 16384 entries; the port uses dynamically sized
owned vectors.

The operand-adding step processes a register operand only if its pool's
forwardable field is nonzero and it is not the pool's zero register. Hardware
resources and zero registers bypass this rewriting. An operand whose source
register is a base register is not excluded from a web by that fact.

For each eligible operand the step creates or resolves its strand and ORs in its
complete component mask. An input operand visits the reaching-definition list
separately for every selected component, resolves every definition's output
operand strand, and merges it with the input strand. An empty input list is an
internal error at this stage, since undefined-use validation has already run.

```rust
for input in instruction.inputs() {
    let input_web = strand_for(input);
    for component in input.mask.components() {
        for output in input.reaching_definitions(component) {
            merge(input_web, strand_for(output))?;
        }
    }
}
```

This is a register-identity requirement, not an arithmetic dependence. One
instruction operand names only one allocated register, so a single `temp.xy` read
combines the strands that define X and Y even if different instructions defined
them. Separate scalar `temp.x` and `temp.y` operands do not create that union. A
whole EXIT operand likewise combines the definitions of its declared components.

Control flow follows from the same logic. A read at a join merges all possible
reaching definitions; a loop-carried input merges its initial value and its
backedge definitions when both appear in that input's reaching lists. There is no
rule merging every occurrence of the same source name: the new destination and old
input in `mul temp,temp,a` start as different strands, and a later recurrence or
explicit constraint can join them.

The step also handles an implicit update backlink: when the operand's update-link
field is nonzero and its direction is input, it resolves the referenced operand and
merges those strands. The port exposes this as `linked_operand`; the caller
translates the parser's update relationship to the effective operand index. This
makes address updates use the same physical register as the address read.

## Explicit physical bindings

An operand holds a forced base register independently of its current source
register. The forced-base setter follows the strand's forward pointers and installs
the base on its terminal entry. A second, different base on that terminal web
fails with:

```text
regweb:: set map - can't map to different base registers..
```

The forced register must be a base register. A nonvirtual source register
explicitly bound to a different base is also rejected.

The merge step follows terminal pointers, combines component masks and use counts,
transfers an existing forced mapping through the forced-base setter, and retires
the old representative. A conflict cannot silently select one physical base.

The gather step first coalesces all terminal webs bound to the same base, using the
base register's web cache, including different source identities or disjoint
component masks, and attaches that physical register to each pinned terminal web.
For an unpinned terminal web it calls the pool's register-creation routine with a
generated name `original_name__generation` and increments the source register's
generation counter. Two independent unpinned definitions of one source name thus
become separate virtual registers, while all uses connected by a web share one
allocation identity.

The patch step resolves the terminal strand, loads its assigned register, and
writes it into the operand's source register. If the operand has a forced base,
the patched register must equal that base.

## Observable physical-name virtualization

Compiled by vcl with `-t1 -uWEBS`:

```text
.init_vf_all
--enter
in_vf vf01
--endenter
mul vf10,vf01,vf01
add vf02,vf10,vf01
--exit
out_vf vf02
--endexit
```

With `.init_vf_all`, vcl emits:

```text
mul VF02,VF01,VF01
add VF02,VF02,VF01
```

Without the initialization directive the internal base is retained:

```text
mul VF10,VF01,VF01
add VF02,VF10,VF01
```

The analogous VI program emits an `iaddiu` destination VI02 with `.init_vi_all`
and VI10 without it. Physical-looking source spellings therefore do not universally
force an allocation, and the caller must preserve the distinction between source
identity and an actual forced base. The probes establish these directive
combinations, not individual `.init_*` mask policies. `.init_vf vf02,vf10.x` is
rejected with `not find vf10.x` and exit status 255 before any code is produced, so
it is no evidence of partial-mask availability.

Input overwrite: with `in_vf vf01`, then `mul vf01,vf01,vf01` and
`add vf02,vf01,vf01`, and the allocation pool enabled, vcl emits
`mul VF02,VF01,VF01` then `add VF02,VF02,VF02`. The new value written to the source
spelling `vf01` does not inherit its ENTER binding.

Partial lanes: defining `temp.x` and `temp.y` separately, then reading `temp.xy`,
puts both partial destinations and the later input in VF02. Using a source name
`temp` twice independently yields two live outputs allocated to VF02 and VF03. (vcl
also simplifies the SUB in that probe, so its instruction spelling says nothing
about the web algorithm.)

The fixture `web-forced-distinct` actually binds both EXIT values to the same VF02;
vcl keeps only the later output chain. It shows that output binding/pruning
combination, not a conflicting-base rejection.

## Rust interface and integration

`crates/vcl/src/analysis/web.rs` implements:

```rust
pub fn plan(
    operations: &[Vec<Operand>],
    inputs: &[Binding],
    outputs: &[ExitBinding],
    analysis: &flow::Analysis,
) -> Result<Plan, WebError>;
```

Each `Operand` uses an **effective expanded operand index**, its source register ID,
mask, direction, `forwardable` flag, optional actual forced base and optional
implicit-update link. Hardware operands and pool zero registers have
`forwardable: false`. A base identity uses the caller's register namespace; VF01
and VI01 must not share an identity merely because both physical numbers are 1.

Only forwardable ENTER/EXIT bindings are passed, with reachable, validated, pruned
operations and the matching reaching-definition analysis. `Plan.operands`,
`Plan.inputs` and `Plan.outputs` map occurrences to web IDs; a `None` entry leaves a
hardware/constant occurrence unchanged. Each returned `Web` records its source
identities, union component mask and optional physical pin. The caller allocates a
fresh register identity per web, keeps the family of the source pool, rewrites each
effective occurrence and rebuilds the flow graph. A forced base applies to the web,
not to every later definition of the source name. Unpinned source names that look
physical become virtual when the allocation policy permits that base to be reused.

The helper uses union/find with deterministic first-appearance ordering. Its
representative order is not claimed to match vcl's intrusive-list ordering or
allocator tie-breaking. The allocator is responsible for physical assignments and
occupancy checks. The helper does not discover parser force constraints,
reconstruct every synthetic move backlink, choose loop unrolling or add undefined
initial definitions.

Unit tests cover independent redefinitions, ENTER overwrite separation, partial-lane
joins and separation, EXIT coalescing, branch joins, loop recurrences, implicit
writeback identity, same-base global coalescing, conflicting pins, hardware/zero
bypass and undefined inputs. The compiler integrates the helper with per-component
occupancy and allocation-aware schedule retries; see
[ALLOCATION_REPAIR.md](ALLOCATION_REPAIR.md). Exact whole-program allocation and
output equivalence are not established.
