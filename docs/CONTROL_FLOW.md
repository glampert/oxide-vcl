# Streams, delayed branches, exits, and expanded loops

This chapter describes vcl 1.4beta7 (the original executable, SHA-256
`83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274`).
Its behaviour here is inferred from vcl 1.4beta7; names for its mechanisms
describe roles, not the original's identifiers. No openvcl implementation was
consulted.

## Analysis graph versus emitted layout

A stream is a control-flow node and owns an instruction table. The flow
monitor keeps a physical stream list, and each stream has next/previous
links. A stream also has an incoming-reference list and an outgoing-reference
list. References contain a target stream, an edge kind, and a visibility
byte.

These three structures must remain distinct in a reconstruction:

1. Physical stream order determines ordinary fallthrough, code layout, and the
   continuation address of branch-and-link.
2. Analysis references carry reaching values and dependencies between streams.
3. Reference visibility controls which targets need emitted labels and linkage.

Hiding a reference demonstrates the distinction directly: the original
clears the visibility bytes of the outgoing and the matching incoming
reference. It does **not** unlink the references. Later end folding hides a
branch without discarding the value-flow relationship to an output
declaration.

The original end modes (inferred from vcl 1.4beta7):

| Stored value | Role |
| --- | --- |
| 1 | Ordinary stream, optionally ending in a branch |
| 2 | Explicit continuation (`--cont`), requiring a physical successor |
| 3 | Ordinary synthetic exit (`--exit ... --endexit`) |
| 4 | Macro exit (`--exitm ... --endexit`), requiring an attached macro string |

The original's `--cont` handler changes ordinary end mode 1 to continuation mode 2 for `--cont`.
Encountering it in another end mode is an error. It also decodes loop hints and
sets barrier state; these synthetics are commands on a stream, not emitted VU
instructions.

## Target resolution and edge types

The branch-target resolver first creates fallthrough edges and resolves explicit
branch targets. The branch descriptor subtype uses bit 1 for a register target,
bit 2 for a conditional branch, and bit 3 for a link. The edge-kind helper
derives these edge kinds from the subtype:

| Edge kind | Meaning | Common subtypes |
| --- | --- | --- |
| 0 | Fallthrough / default physical continuation | no branch |
| 1 | Direct taken branch | `b`=1, conditional branches=5 |
| 2 | Indirect taken branch | `jr`=3 |
| 3 | Direct branch-and-link | `bal`=9 |
| 4 | Indirect branch-and-link | `jalr`=11 |

An ordinary stream gets an edge to its physical successor when it has no
branch or when its branch is conditional. Direct branches resolve their
immediate target through a dedicated helper. Explicit continuation mode requires a
physical successor and no branch. Exit modes require no branch; a macro exit
also requires its macro text.

The branch resolver searches the stream name lists with a lookup whose
comparison is case-insensitive for ASCII letters: ASCII letters are compared case-insensitively. Thus
`b Done` can resolve a label spelled `done:`. Keep the original spelling for
emission; a case-sensitive symbol map incorrectly rejects this observed case.
This does not mean every name-list operation is case-insensitive. The separate
exact-name helper uses exact comparison, including the export check
used by the adjacent-stream merge gate.

The edge-adding helper validates edge-kind compatibility, adds the source's outgoing
reference, and adds the corresponding destination incoming reference. The graph
contains matched references in both directions.

## Inferring branch-to-register destinations

For a branch-to-register carrying explicit tags, the resolver splits the
colon-separated retained tag string, resolves each named stream, and adds an
edge of the proper indirect kind. An explicit tag that names a missing label
causes an error. An untagged JR can obtain a target from a reaching
branch-and-link setter.

It then runs the indirect-link inference helper repeatedly until no new inferred links are added.
For one indirect branch this helper:

1. Gets the branch's target input register.
2. Scans backwards from immediately before the branch in its own stream.
3. If necessary, follows predecessor streams and scans backwards from their
   last instruction. Per-stream visit states bound the traversal.
4. Finds setters of the target register.
5. If a setter is branch-and-link, its **physical successor stream** is a
   possible return destination. Add a kind-2 edge unless it already exists.
6. If a setter is not branch-and-link and the indirect branch has no explicit
   tags, reject the input with the diagnostic requesting a tag list.

The whole resolver repeats because newly inferred return edges can expose more
predecessor paths. It does not infer arbitrary register arithmetic into an
address. With explicit tags, encountering another setter can stop that backward
path without demanding arithmetic inference. At completion it frees the
temporary tag strings.

This mechanism is materially different from linking every `jr` to every `bal`
continuation, and from treating `bal` as an ordinary conditional edge. A faithful
port needs physical layout plus reaching setters across predecessor paths.

The fixed inference kind, JALR validation, and caller-sensitive traversal
are covered with original controls in [INDIRECT_FLOW.md](INDIRECT_FLOW.md).

## Raw pairs and branch delay slots

The original's raw builder transfers each source pair into a dispatch entry
with the upper instruction first and the lower second. It checks pair
compatibility with a dedicated predicate, rejecting wrong-pipe instructions and two nonzero outputs to the
same register object. This rejects two writes even if their component masks
are disjoint.

The original's row replay establishes simultaneous pair semantics. After determining stalls,
it loops over **both pipes' reads** and links them to existing setters. Only
after that full read pass does it loop over **both pipes' outputs** and install
the new setters. An upper write cannot satisfy a lower read in the same pair.
The retained `raw-read-before-write-unset` fixture confirms that a missing old
VF02.x is rejected even though the upper instruction writes VF02 in that pair.

The raw builder allows at most one branch in a continuous stream. It stores
the branch's dispatch row, then requires exactly one subsequent dispatch row:

```rust
let rows_after_branch = total_rows - branch_row - 1;
match rows_after_branch {
    0 => reject_missing_delay_slot(),
    1 => accept(),
    _ => reject_stray_branch(),
}
```

If a mode transition or target label splits away the intended delay row, the
original rejects the input rather than silently inventing the missing raw
instruction. Raw-loop input has a separate diagnostic requiring an explicit
delay slot. Before the raw builder, the original's pre-check also rejects a branch inside a
delay slot and a target/mode boundary that would leave the branch without its
expected row.

Explicitly staged raw-loop instructions carry positive source loop IDs. The builder converts
them to absolute issue positions as:

```rust
absolute_row = raw_row + period * (source_loop_id - 1);
```

It records the largest iteration ID and rejects inconsistent mixtures of
looped/nonlooped instructions and loop IDs attached to NOPs. Those absolute
positions drive later expansion, rather than treating the source IDs as output
labels.

## Removing inaccessible streams and redundant branches

The reachability pass first marks streams with entry start type 2 as reachable. It also
marks pass-through streams, then propagates reachability through outgoing
references to a fixed point. Unreachable streams have their incoming/outgoing
references unlinked and are removed from the physical list.

The same function can remove a redundant branch after pruning:

```text
for each stream with a physical next stream:
    if it has a branch and its code submode is not 4:
        if every outgoing reference targets that physical next stream:
            remember whether a kind-0 edge already exists
            unlink the taken branch edge
            delete the branch instruction and clear branch subtype
            add a kind-0 edge if needed
```

The test is on **all outgoing targets**, not just the immediate branch target.
Removing an unreachable intervening stream can make the branch destination the
physical next stream and unlock this simplification. The original successful
`branch-forward-unconditional` fixture consequently emits just the live `add`
and its E termination; its dead `sub` and redundant `b` disappear.

## Concatenating adjacent streams

The merge pass scans physical neighbors and merges only when all its constraints
hold:

- Both current work tables are VSM mode 3 with submode 1 or 3
  (the stream-eligibility predicate).
- The current end mode is ordinary 1 and the next start type is ordinary 1.
- The current stream has exactly one outgoing reference and the next has
  exactly one incoming reference.
- The current has no branch instruction, and its kind-0 target is the next.
- Current end fence, next start fence, current debug-end bits, and next
  debug-top bits are clear.
- The next stream has no label found in the relevant exported-name list.

The merge concatenates their tables through the table concatenator, then transfers the
next stream's end mode, branch subtype, end debug bits, and end fence onto the
current stream. The table concatenator copies dispatch rows, transfers instruction
ownership, rebuilds source-order links, empties the old table, and prepares both
tables. The outer merge pass removes the connecting edge, redirects every next
stream successor to the merged stream, removes the next stream, and retries
from the same current stream so several neighbors can collapse.

It does not simply merge all adjacent labels or all single-successor blocks.
Debugging, exports, start/end types, branches, and submodes are observable merge
boundaries.

## Folding ordinary code into an E exit

The Tidy pass selects final emit tables and separately marks a
stream as foldable when:

```rust
current.end_type == 1
    && !current.outgoing.is_empty()
    && current.outgoing.iter().all(|edge| edge.target.end_type == 3)
```

It marks each matching reference invisible using the hide-reference helper, suppresses the
current branch instruction by setting its suppress flag, clears branch
subtype, and changes the current end type to 3. Analysis links remain intact.

For `branch-forward-conditional`, the branch stream still has a code successor
and an exit successor and is not folded. The fallthrough `mul` stream has only
an exit successor and becomes an E stream. The explicitly targeted `done:`
exit still emits its own E/NOP pair. This explains why a single shared exit
declaration can yield several physical termination sequences.

### E safety and padding

A termination-hazard predicate checks lower-pipe termination hazards. Tidy appends empty rows
until the table has at least two dispatch rows and this predicate holds:

- The last lower slot is not a memory operation and not a branch.
- The last lower slot is not instruction table index 165, 166, or 167
  (`xgkick`, `xtop`, `xitop`).
- The penultimate lower slot is not index 165 (`xgkick`).

The E bit belongs to the penultimate output row after this adjustment. A final
load/store therefore needs one padding row, while final XGKICK can require two.
Simply forcing a minimum of two total rows misplaces E for a store/load pair.
Macro exits use their explicit string rather than this ordinary E rule.
The later reduction pass can remove empty rows, including the observed original
E-loss case described in [RUNTIME.md](RUNTIME.md); that is a later transformation.

`crates/vcl/src/analysis/control.rs` contains independently testable semantic ports of the subtype
mapping, ASCII label comparison, merge predicate, exit-fold predicate, and exact
padding rule. They do not mutate a caller's analysis graph.

## Expanding a modulo schedule

Loop eligibility requires branch subtype 1 or 5 and the taken
kind-1 edge to point to the same stream. A textual backward branch alone does
not establish eligibility. The optimizer's period search and side-effect
constraints are described in [SCHEDULING.md](SCHEDULING.md).

The expansion entry is called after a finite-period VSM table has
been selected. The part builder creates the constituent tables. Its meaningful
parameters are:

| Symbol here | Original source |
| --- | --- |
| `P` | Selected modulo period of the table |
| `M` | Greatest scheduled iteration ID of the table |
| `C` | Conditional branch's iteration ID |
| `K` | Stream's known minimum loop count, at least 1 |
| `S` | Stream's known safe count, nonnegative |

It rejects absent period/conditional, `M<C`, invalid known counts, or a
non-VSM input. The original fallthrough successor must be the physical next
stream. The generated basename is `EXPL_<unique>_<stream>_`.

### Cloning a stage window

The copy helper appends one copy of the source period to a destination table. It
visits source dispatch rows and both pipes, cloning an instruction when its
scheduled iteration ID lies in an inclusive requested window. A boolean
controls whether conditional branches are included. Another boolean controls
whether NOPs are copied. With immediate fixup enabled, each clone receives a
relative iteration bias on the instruction:

```rust
clone.iteration_bias = requested_max_iteration - original.iteration_id;
```

The clone's runtime iteration ID resets to zero and its position is the new
linear table position. This bias is consumed by later immediate fixups; merely
copying assembly text loses it.

### Entry, prologues, kernel, epilogues, exit

The part builder creates these parts:

- `<base>_ENTRY_POINT`, an entry stream.
- Up to sixteen `<base>_PRO<n>` streams.
- `<base>_MAIN_LOOP`, a one-period kernel.
- Up to sixteen `<base>_EPI<n>` streams.
- `<base>_EXIT_POINT`, an exit stream.

The basename already ends with an underscore, so these suffixes produce the
observed double underscore, for example `EXPL_PIPE_loop__MAIN_LOOP`.

Prologue grouping starts with `boundary = min(M, K - 1 + C)`. For stage
`s=0..M-1`, it appends a period containing instruction iteration IDs `0..=s`.
The conditional is included when `s>=boundary`. It finishes a prologue at the
boundary or the final prologue stage, records its iteration interval, marks its
conditional for reversed sense, then advances the grouping
boundary. A larger known minimum count can group more warmup periods before
the first necessary early-exit test.

The kernel clones the inclusive window `0..=M` into one period, preserving its
conditional and NOP layout, and records its branch as an ordinary looping
conditional. The branch target is rewritten to the kernel's own name.

If `M>C`, the drain span is `(M-C)*P`. Epilogues are constructed for prologue
early exits in reverse order. Each contains `M-C` appended period copies with
an advancing lower iteration bound, omitting the original conditional. This
drains only operations already launched by that path. Intermediate epilogues
receive an unconditional branch in their penultimate row; the last falls into
the generated exit. Those synthetic branches are initially `UNRESOLVED` and
are retargeted before the graph is installed.

The expansion entry then wires the parts:

```text
entry --fallthrough--> first prologue, or kernel if there is none
pro[n] --fallthrough--> pro[n+1], or kernel
pro[n] --taken/reversed--> epilogue selected by its completed iteration
kernel --taken--> kernel
kernel --fallthrough--> first epilogue, or exit if none
epilogue[n] --branch/fallthrough--> generated exit
generated exit --fallthrough--> original physical successor
```

It retargets all incoming original-loop links to the generated entry, unlinks
the old loop's self and fallthrough edges, removes the original stream, and
inserts the parts physically in entry/prologue/kernel/epilogue/exit order.
Original user names are transferred to the generated entry. The return value
is `prologue_count + epilogue_count + 3`.

### Typed expansion implementation and original probes

`crates/vcl/src/loops/pipeline.rs` implements the stage windows, clone selection, iteration
biases, branch reversal/targets, generated names, and physical part order as an
owned plan. It accepts original operation IDs and a selected period/stage/pipe
placement; callers clone their typed operations from those IDs. Synthesized
epilogue branches are represented separately with no source operation ID.
The module preserves empty entry/exit tables because later Tidy determines
their emitted padding.

The optimizer's stage limit of sixteen is separate from the expansion input
limit: explicit raw source loop IDs may be 1 through 32 and become stages 0
through 31. Prologue/epilogue factory arrays are limited to sixteen entries.
Consequently a stage-17 schedule with a known minimum that groups its warmup
can fit, while seventeen individually checked prologues cannot. The typed plan
reports the latter as an explicit parts-limit error. Its configurable total-row
budget is a Rust resource bound, not an asserted original compatibility limit.

Two independently authored original-binary probes exercise a nontrivial
three-stage input at period 5, with `M=2` and conditional stage `C=0`:

| Probe | Known minimum | Original part rows before Tidy exit padding | Final original pairs |
| --- | --- | --- | --- |
| `pipeline-raw-stages-min1` | 1 | entry 0, pro 5, pro 5, kernel 5, epi 10, epi 10, exit 0 | 37 |
| `pipeline-raw-stages-min4` | 4 | entry 0, pro 10, kernel 5, epi 10, exit 0 | 27 |

For the first probe, PRO0's reversed conditional targets EPI1 and PRO1's
targets EPI0. For the second, its combined prologue has no early-exit test.
Both original runs exited zero with no emulation timeout. The raw input label
and `--LoopCS` must belong inside `.vsmloop`; putting them before the mode
transition instead targets another stream and the original fails while
unlinking the expected self edge.

Exact inputs, stdout, stderr, hashes, guest exit status, and emulator settings
are retained in `local/evidence/oracle/pipeline-raw-stages-min1/` and
`local/evidence/oracle/pipeline-raw-stages-min4/` (local, not tracked). `local/tools/probe_pipeline.py` validates them; `--capture` reruns the original. The primitive tests check the observed part sizes and early-exit
routing, and end-to-end compiler tests match all normalized pairs for both
fixtures, including allocation, pruning, the min4 0/2 update coalescing and E bits. Later original passes remove one dead prologue multiply and remap
epilogue temporaries, so this expansion plan alone is not the complete
compiler.

## Integration with the compiler

The expansion plan is wired into emission. Explicit raw expansion precedes
source validation; normal-loop search follows validated source flow. The final
CFG, pair-read ownership, register webs and immediate repair are rebuilt
rather than inferred from clone stage numbers. See
[PIPELINE_INTEGRATION.md](PIPELINE_INTEGRATION.md).

The compiler preserves CONT as end kind 2 with E termination, barrier start
fences and label top flags. It runs the bounded lower branch-tail exchange
before local allocation and the original unique fallthrough merge predicate
after allocation. Merging affects emission tables, while CFG edges remain
intact for flow/liveness. [BRANCH_STREAMS.md](BRANCH_STREAMS.md) details the
original procedures, safe caller subset, direct incoming-debt calculation,
D/E/T/M formatting, controlled observations and remaining differences.

Raw reorder has a post-allocation motion path and a conservative CFG
distance helper. The original's distance helper uses intermediate path-key lengths;
the port's helper computes the minimum guaranteed duration over actual graph
paths, excluding producer/consumer extents. Raw synthetic endpoint merging
and the original two-traversal Tidy lifecycle are not ported; the
minimum-count controls expose that boundary, and
[RAW_BOUNDARIES.md](RAW_BOUNDARIES.md) records the factory modes and observed
layouts.
