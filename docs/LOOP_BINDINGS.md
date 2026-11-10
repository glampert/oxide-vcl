# Register bindings through loop expansion

vcl 1.4beta7 carries register choices through two expansion paths. Normal
modulo scheduling selects a physical allocation for the folded table before
expansion. Explicit staged raw code is expanded earlier, before the first
external assignment. Both use the same period-window clone step. The
allocation state of each generated part decides which of its inherited
physical assignments survive the later web rebuild. This chapter is inferred
from vcl 1.4beta7 (SHA-256
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`) and
observed from its runs. For expansion and graph construction see
[CONTROL_FLOW.md](CONTROL_FLOW.md#expanding-a-modulo-schedule).

## State and ownership

A stream's working table carries an allocation state, separate from its code
mode and VSM submode. The states are 0 (bad/unset), 1 (none), 2 (externally
assigned) and 3 (locally allocated). Each operand has a web reference (rebuilt
during web construction), a component mask, a forced physical base, an
original register identity (also used by symbolic emission) and a current
register identity (overwritten by web patching and register assignment).

The two allocation passes use a temporary mapping on the register object when
rewriting an operand's current identity. They leave the forced base alone. A
later pool cleanup removes the temporary mappings while the operand
replacements persist. Virtual and physical registers are distinguished by a
flag on the register. A register reference at a table is external if its
input, output or through masks have any component set.

## Driver and normal candidate selection

Early raw expansion comes before the first dependency check. Raw decoding
builds a pre-expansion table; the flow builder changes its mode before
constructing the graph, and allocation state is still zero.

The first global web pass runs before external assignment. That pass first
resets all current tables to state 1, including parts already created by the
early raw expansion. It processes those streams in list order, substitutes
mapped physical registers on external operands and advances each table to
state 2. It then clears the temporary mappings. The normal loop search starts
with those assignments already present in its input operands.

For each normal modulo trial, vcl copies the source allocation state into a
fresh candidate, copies register references and queues instruction images.
The modulo search schedules them, reduces extents and runs the post-schedule
setup with argument 2, which records VSM submode 2 and prepares the candidate
for allocation. Local allocation takes the candidate table, verifies state 2,
code mode 3, valid locks and an empty to-do list, then applies the pool
allocators. Success advances it to state 3; failure removes the candidate. At
a feasible period the driver selects the successful table with the smallest
maximum iteration ID, replacing the incumbent on equal IDs, and installs it.
The second expansion therefore sees a selected, physically allocated modulo
table.

## Clone transport and generated table state

The expansion context records the source allocation state and notes whether
the source VSM submode is 4. Part constructors initialize attached tables in
code mode 3, link each table back to the new stream, and set the allocation
state and VSM submode:

| Generated part | Stream kind | Alloc state and VSM submode |
| --- | ---: | --- |
| Entry | 4 | State 2, submode 1 |
| Prologue | 2 | State 2, submode 1. For source submode 4, inherit state and set submode 2. |
| Kernel | 5 | Inherit source state, submode 2 |
| Epilogue | 3 | Same as the prologue |
| Exit | 1 | State 2, submode 1 |

So a **normal** selected loop produces a state-3 kernel and state-2 prologues
and epilogues. An **early raw** kernel initially inherits the pre-allocation
state, and the first external pass resets the whole expanded graph to state 1
before setting it to 2. The submode-4 exception affects the disposition of raw
prologues and drains, but their initial state still passes through that
global reset.

The window cloner takes a source table, a destination table, a high stage and
a low stage, walks the folded rows and pipe slots, and clones each accepted
slot. A clone copies current, original and forced registers as pointers,
copies component masks and the concrete move encoding, and clears the live
move-record pointer. For each new slot it clears the temporary clone links,
sets the destination table as owner, assigns a row and pipe, and resets the
live iteration ID. With the stage-displacement flag it records `high_stage -
source_iteration` in a separate field. It does not assign another physical
register. Raw preparation then rebuilds the generated instruction list from
its row array.

## Reflow, forced bases, and the second web pass

On the normal-loop path the second expansion is followed by a dependency
rebuild, affine repair, pruning, and another dependency rebuild. The driver
then runs the external binding pass, reruns the global web pass and rebuilds
dependencies again. This traverses the whole current stream list, including
earlier raw parts. Disabling normal loop optimization bypasses this part of
the driver and still permits the first raw expansion and the ordinary
raw-table allocation.

The binding pass uses **post-expansion** register references, indexed by each
operand's current register. A missing reference fails the pass. For state 3 it
pins every register operand. Otherwise it pins operands whose register
reference has nonzero external masks. Pinning requires a physical current
register and agreement with any existing forced base. Other operands keep
whatever forced base they had.

```rust
// Structural sketch of the binding pass, using recomputed references.
for operand in table.operands_with_registers() {
    let current = operand.current;
    let reference = table.references.get(current).ok_or(MissingReference)?;
    if table.allocation_state == Done || reference.is_external() {
        require(current.is_physical());
        require(operand.forced.is_none_or(|base| base == current));
        operand.forced = Some(current);
    }
}
```

Binding is reference-wide: a read of one component and a write of a different
component can share the same reference. If that reference crosses a stream
boundary, both occurrences are pinned irrespective of their component masks.

The web pass clears pool web caches and operand web pointers while keeping
forced bases. Its operand traversal admits registers in a component-bearing
pool except the pool's zero register. It merges read webs with reaching setter
webs and with the linked destination of a read/write operand. A forced base
attaches to the representative strand; two different forced bases on one
representative fail. Gather merges independent roots sharing the same physical
base via that register's cache. It chooses the forced base as the web's
register, or creates a fresh virtual for an unforced web. Patch then writes the
web's register to the operand's current identity and checks that it equals the
forced base wherever there is one.

A physically assigned clone becomes virtual again when its generated part is
in state 2, its register has no external role there, and the rebuilt web
contains no other forced-base operand. A prologue or drain thus keeps a
binding where it communicates with the kernel while still reallocating a value
private to that part. State 3 makes the kernel an unconditional source of
physical bindings. The raw-table driver forces the rows into a lock table and
calls local allocation only for state 2; it accepts state 3 without a second
allocation. Ordinary synthetic tables use their own state-2-to-state-3 path.

## Observed staged raw trace

The original ran `raw-boundary-min2.vcl` with `-t1 -uLOOP` and time scale 25
(exit status 0, no alarm, no emulator error). Five clone windows were
appended from a mode-3/submode-3 folded table of period 5 in allocation state
0. The generated kernel initially inherited state 0, while the prologue and
drain used state 2. By the binding pass the whole graph was in state 2
following the first external allocation. Input and capture:
`local/evidence/traces/move-loop-call/raw-boundary-min2/` (local, not
tracked); raw guest trace in its `trace.json`.

The two written vector names `VF03` and `VF04` take distinct paths:

| Snapshot | Kernel multiply producing source `VF03` | Kernel multiply producing source `VF04` | Drain multiply producing source `VF04` |
| --- | --- | --- | --- |
| Clone rows | Physical `VF03`, forced pointer empty | Physical `VF04`, forced pointer empty | Physical `VF04`, forced pointer empty |
| First web return | Virtual `VF03__0` | Virtual `VF04__0` | Virtual `VF04__1` |
| Binding pass return | Physical `VF03`, forced to `VF03` | Virtual `VF04__0`, unforced | Virtual `VF04__1`, unforced |
| Second web return | Physical `VF03`, forced to `VF03` | Fresh virtual `VF04__0__0` | Fresh virtual `VF04__1__0` |
| Later local allocation return | Physical `VF03`, forced to `VF03` | Physical `VF04`, unforced | Physical `VF01`, unforced |

The drain multiply's destination keeps `VF04` in its original identity field.
A physical spelling in staged raw input therefore does not itself force a
base, and the separate `VF04` web in the drain can receive a different
register. The kernel also runs late local allocation in this raw case, as do
all four other generated mode-3 parts.

## Observed normal modulo trace

The original ran `trace-move-long-input.vcl` with `-t1 -uMOVE` and time scale
25 (exit status 0, no alarms, no emulator error). Eleven successful trial
allocations returned from the modulo caller with period 7. The last
successful candidate's table is the source of all four clone windows; its
maximum stage and branch stage are both 3. This fixture produces a prologue
and kernel, no drain. The kernel is state 3 after cloning, the prologue state
2. The local allocator later runs for entry, prologue and exit, and skips the
kernel. Trace and input/capture:
`local/evidence/traces/move-loop-call/normal-move-loop/` (local, not tracked;
`trace.json`, `stdout.bin`).

The selected table's first `lvmove` shows a value allocated before expansion
and again in the prologue. The folded instruction first carries virtual
destination `move__m0` and virtual input `root__0`; candidate allocation
assigns `VF06` and `VF04`, both with empty forced bases. Cloning preserves the
physical registers and move IDs and clears the move record pointer. The
destination's original identity stays `move__m0` throughout.

| Snapshot | State-2 prologue move destinations, rows 7 and 14 | State-3 kernel move destination, row 0 |
| --- | --- | --- |
| Clone rows | Physical `VF06`, unforced | Physical `VF06`, unforced |
| Binding pass return | Physical `VF06`, unforced | Physical `VF06`, forced to `VF06` |
| Second web return | Fresh virtual `VF06__0` and `VF06__1` | Physical `VF06`, forced to `VF06` |
| Late prologue allocation | Both physical `VF08`, unforced | No new kernel allocation |

The `VF04` input receives a forced base in both parts: the state-2 prologue is
eligible through its cross-stream reference, and the state-3 kernel through
the unconditional rule. The prologue's two new destination webs follow
distinct cloned definitions; their downstream reads are patched with the same
virtual identities before both are packed into `VF08`. The final assembly
prints `move VF08,VF04` in the prologue and `move VF06,VF04` in the kernel.

## Not observed

The staged raw probe covers a drain, and the normal probe covers the
selected-kernel/prologue split with generated moves. The state-2 behaviour of
a normal drain follows from the part constructors and binding-pass rules but
has not been observed: an instrumented normal `pipeline-chain-min4` trial
timed out before any candidate allocation was recorded
(`local/evidence/traces/move-loop-call/pipeline-chain-min4/result.json`), and
there is no successful normal-drain trace or staged-raw fixed-submode-4 trace.
