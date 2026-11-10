# Work areas, dormant memory registers, and memory dependencies

The work-area directive is present in vcl 1.4beta7, but its memory register
pool is not installed by the shipped CPU profile. All otherwise valid
`.vu_work_area` probes fail with:

```text
ERROR: init work area - failed to get vu mem pool
```

This is observed behaviour of vcl 1.4beta7, and the port reproduces it. Code
for a dormant pool exists in vcl 1.4beta7 (inferred), but the port does not
invent source syntax, automatic spills, allocation, or lifetime behaviour that
the shipped configuration does not expose.

## The installed CPU profile

Inferred from vcl 1.4beta7, the Marigold CPU profile defines three register
pools, each described by seven 32-bit fields, followed by a zero sentinel:

| Record | Constructor kind | Prefix | Description | Register class | Components | Zero/base resource |
| --- | ---: | --- | --- | ---: | ---: | --- |
| 0 | 1 | `vf` | VU User floating point | 9 | 4 | `vf00` |
| 1 | 1 | `vi` | VU User integer | 8 | 1 | `vi00` |
| 2 | 2 | `hw` | VU hardware register | 10 | 0 in the profile; per-register widths | Hardware `EXT` resource |

Pools are stored by register class. Class 11, the memory pool, is absent, so
the work-area handler's request for class 11 returns nothing. A third
constructor kind that could build a memory pool exists, but no profile record
selects it. A directive or constructor existing in the executable is therefore
not proof that an input program can use the feature.

`local/evidence/analysis/memory-profile.json` (local, not tracked) retains the
profile records and the dormant pool's three assignment methods, each of which
returns one. `local/tools/extract_memory_profile.py` regenerates it and also
enumerates the empty set of source instructions that use operand class 37.

## Directive parsing and failure order

The accepted spelling is `.vu_work_area device,address,size`. There is no
`.workarea` or `.work` entry in the source directive table, despite
diagnostics using the shorter name.

The handler performs these steps in order:

1. Recognize `core`, `vu0`, or `vu1`, case insensitively. `core` immediately
   fails with `work: core cpu work area.. not yet`.
2. Read the address and size tokens with base-10 `strtol` semantics.
3. Reject nonpositive size, then negative address, then an area beyond the
   selected capacity.
4. Look up memory pool 11. It is absent, so the handler reports the
   missing-pool error before constructing any memory registers.

Capacity is 256 units for VU0 and 1024 for VU1, matching the VU manual's 4-Kbyte
and 16-Kbyte data memories in 16-byte quadwords.

The number parser is not an expression parser. `0x20` has decimal prefix zero,
`4tail` has decimal prefix four, and `BASE_LABEL` has no decimal conversion and
yields zero. Those requests pass bounds validation and fail at the absent pool.
Validation and name construction use different representations: the original
base token spelling would be kept for the dormant constructor's names.

Retained probes (local, not tracked):

| Fixture | Directive | Original result |
| --- | --- | --- |
| `memory-area-vu0` | `vu0,12,4` | Missing pool |
| `memory-area-vu1-max` | `vu1,0,1024` | Missing pool |
| `memory-area-hex-base` | `vu0,0x20,4` | Missing pool |
| `memory-area-symbol-base` | `vu0,BASE_LABEL,4` | Missing pool |
| `memory-area-size-prefix` | `vu0,12,4tail` | Missing pool |
| `memory-area-core` | `core,0,4` | Core unsupported |
| `memory-area-overrun` | `vu0,255,2` | Area overrun |
| `memory-area-negative` | `vu0,-1,2` | Negative effective address |
| `memory-area-empty` | `vu0,0,0` | Nonpositive size |

All probes indexed in `local/evidence/analysis/workarea-probes.json` end with
guest exit status 255. Inputs and outputs are under `local/evidence/oracle/`;
`local/tools/probe_workareas.py` reproduces them. Because every probe fails at
configuration, none shows what a configured memory pool would do under
register pressure.

## The dormant constructor

Inferred from vcl 1.4beta7, the constructor validates a nonnull base token and
positive slot count, then allocates `size` register objects. For offset `n`:

```rust
struct DormantMemorySlot {
    id: u32,        // 10000 + n
    register_class: u8, // 11
    components: u8, // four independent parts
    name: String,   // format!("({base_token}+{n})(vi00)")
}
```

For a base token `0x20`, the first two names would be `(0x20+0)(vi00)` and
`(0x20+1)(vi00)`; the source spelling is kept, not the decimal value used by
the bounds check. The generic operand emitter has a class-37 case that would
print `ramreg-[<register-name>]`.

None of the 214 source instruction descriptors has an operand of `OP_V_MEMREG`
class 37, and the explicit source operand decoder has no class-37 branch. Other
code recognizes the class, but that does not establish an accepted source
spelling for memory-register values. The probe attempting `lq vf02,ramreg`
cannot show direct memory-register support; ordinary LQ expects an
effective-address operand.

The pool has three assignment methods besides destruction (external assignment,
disjointness processing, local assignment), and each returns 1 unconditionally.
No memory spilling or interval-packing algorithm is observed in them.

[`crates/vcl/src/analysis/memory.rs`](../crates/vcl/src/analysis/memory.rs)
separates three concepts:

- `WorkAreaRequest` validates and preserves a source request.
- `activate_marigold` reproduces the missing-pool failure.
- `DormantPool::initialize` models the constructor for static-analysis
  exploration; it is not invoked to make the source directive succeed. Its
  three assignment methods are no-op successes, as in vcl 1.4beta7, and it
  allocates no virtual memory lifetimes.

The request validator uses wide arithmetic for its bounds check instead of
emulating a possibly overflowing signed C addition, so extreme requests fail
safely; ordinary requests follow the original validation and error order.

## Ordinary memory operations and alias tags

LQ/SQ/ILW/ISW and their variants use ordinary VU address registers and memory;
they do not need a work area or the class-11 pool.

Inferred from vcl 1.4beta7, tags on memory operations are bound to virtual
resources in hardware pool 10. For each tag an implicit `OP_EXT_DEP` class-55
operand is added:

| Operation | Direction on tag resource | Availability offset | Component mask |
| --- | --- | ---: | --- |
| Load | Read (1) | 0 | Memory fields touched |
| Store | Write (2) | 1 | Memory fields touched |

Multiple tags create multiple resource effects. Reusing an already-present
implicit register/direction merges component masks. This gives per-component
alias dependencies independent of virtual VF/VI lifetimes.
`memory::tag_effects` returns these records without involving the work-area
pool.

The effect is observed from vcl 1.4beta7 in the `memory-tag-*` runs (local,
not tracked): a store through one base register and a later load through
another swap with no tags or with different tags (`:out`/`:in`), and keep
their order when both carry the same tag (`:buf`). Two stores with the same
tag still swap (a write after a write of the resource is no dependency), and
so do accesses whose fields do not overlap.

The port applies the rule in two places:

- In linear tables, the memory-order test orders a pair that is not two
  stores, with overlapping fields and a common tag.
- A software-pipelined loop runs on a cloned table without the memory-order
  lists, so there the tags are resources, as in vcl 1.4beta7
  (`add_memory_tag_resources`): each tag is a pool-0 register that stores
  write (latency 1) and loads read, over the fields touched. A load before
  the store in the body reads the previous iteration's value, so the period
  must cover the store-to-load recurrence. In `memory-tag-loop` the bound is
  6, every strategy fails at periods 6 to 8, and both compilers settle on
  period 9; `memory-tag-loop-untagged`, `-disjoint`, `-store-first`,
  `-fields` and `-fields-untagged` match too (runs at time scale 1).

The memory-order pass compares memory directions, touched fields, effective
addresses, tags, and barrier flags. Load/load pairs do not need the ordering
that store/load, load/store, or store/store pairs need. Timid mode forces
dependencies conservatively. Effective-address comparison also requires
matching register identity, external-input state, and setter location before
comparing immediate values; comparing only register names or numeric offsets
would omit those data-flow conditions.

This does not make a complete standalone memory-alias solver: the caller must
integrate address-register reaching definitions and barrier semantics. The
compiler's conservative source ordering is tracked in
[COMPATIBILITY.md](COMPATIBILITY.md).

## Original force-dependency pass

Inferred from vcl 1.4beta7, the flow rebuild runs the memory-order pass after
ordinary dependency analysis of each table. It clears existing force-dependency
lists and resets ACC tracking, then walks earlier instructions in reverse. A
current load compares against prior stores, and a current store against prior
loads and stores. Each accepted ordering gets reciprocal list entries.

The instruction's ordering-flag byte supplies additional ordering classes:

- Bit `0x08` is an error at this stage; the barrier fixup should already have
  consumed it.
- Bit `0x10` extends the earlier-load comparison and `0x20` the earlier-store
  comparison. Earlier instructions carrying those flags can also order a later
  load and store, respectively.
- Bit `0x40` links the current flagged instruction to the nearest earlier
  instruction of the same class.

If the current instruction has an effective-address operand, the memory branch
requires an intersecting memory field mask and at least one of the timid-mode
flag, the raw tag-string test, or effective-address equality. Without an
effective-address operand, the flag can order the earlier memory instruction
directly.

The raw tag-string comparison has an unusual loop: it walks pairs of
colon-delimited tokens and compares the two bytes to each other only where a
token ends, so two single final tokens of equal length compare equal even with
different letters. The normal flow-construction path converts source memory
tags into virtual-resource operands and clears the instruction's raw tag
pointer before the rebuild, and the helper returns false if either pointer is
null. No retained observation shows the comparator reached with two populated
pointers.
