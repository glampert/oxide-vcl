# Allocator-generated moves and rollback

vcl 1.4beta7 creates temporary register identities during modulo scheduling
(inferred from vcl 1.4beta7). A placement can request a move when its consumer
would extend a value's lifetime too far or conflict with another output lock.
The scheduler can discard the move and restore the prior register and lock
identities while backtracking. The allocator's first-free search for
externally bound registers is a separate stage; see
[EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md). The port implements this in
`crates/vcl/src/scheduler/wrangle.rs`; see [LPGEN_PORT.md](LPGEN_PORT.md).

## Placement failure and eligibility

Both wranglers, A and B, check a candidate placement with the placement
checker. If the result contains "move needed", they try dragging setters in
mode 4 and check again if a setter moved. A remaining "move needed" passes the
failed instruction and candidate time to the move selector.

The input-lock check clears the lock's reason before testing the candidate.
Three paths set the move bit when the lock's movable flag is 1:

| Reason | Placement condition |
| --- | --- |
| 1 | A nonconstant external value would exceed its allowed span and the candidate is at or after the external parent lock's cutoff. |
| 2 | The proposed value interval overlaps another output lock with zero current users. This branch also needs a nonzero period and a global flag, which A sets and B clears. |
| 3 | The consumer is at least one period after its ordinary parent's reference time, where the proposed use extends beyond the parent's current last use. |

An operand carries a move-eligibility value, copied into its lock's movable
flag. Modulo queueing sets eligibility for VI and VF inputs unless already
marked 2. Output eligibility has an additional register-reference condition.
The placement checker distinguishes a movable lock from the same lifetime
conflict on a nonmovable operand.

A move can be constructed only while the sum of the table's three
generated-move counters is smaller than twice the period, so generated moves
only occur in positive-period searches. VI moves have a second gate: if
lower-only moves already equal or exceed the period, the table is flagged and
no move is made; in B this can trigger a restart under the alternate pipe
policy.

## Selecting the value

The move selector returns immediately if the failed instruction already has a
move. Otherwise it scores the instruction's VI and VF inputs whose
eligibility is not 2. For each component in the operand mask it reads the
input lock's parent. A parent with an external owner is treated as external;
an ordinary parent must have a nonnegative placement.

An ordinary parent contributes age `failed_time - (parent_time + 1)` and widens
the register's scratch bounds with `parent_time + 1` and `parent_time +
parent_latency`. A reason-1 lock imposes a score floor of 20,000 and reason 2
a floor of 10,000; reason 3 has no special floor. The operand's score is the
maximum of its qualifying ages and floors. A strictly greater score replaces
the previous winner, so the first operand wins a tie. The remembered lock
within the winning operand is the **last masked component visited**, even if
another component gave the highest score; its parent is used for ordering the
generated instruction.

When the parent's owner is an instruction, the move gets ordering key
`producer.key + 1` and depth `producer.depth + 1`. With an external owner it
uses the failing consumer's key minus one and depth 1. The move inherits the
corresponding instruction's retry threshold and location record, and its time
bound is the failed time minus the move output latency. An eligible source
can still produce no move. The fallback scans old components whose active
output locks had users, chooses the earliest owner by sequence field, or uses
the table's done endpoint if none is found; it marks a rewind point.

The selector's outcomes: queued; rewind point marked; the failed instruction
already has a move; no source operand scores; internal failure.

## Register and lock mutation

Integer and vector moves have separate pools. Each record owns the temporary
register and up to four lane-lock lists. The generated instruction points back
to its record and carries an abstract move descriptor. Operands are destination
first, source second. The constructor binds the source to the chosen old
register, sets both operands' eligibility to 1, and initially allows the
full-period setter drag on the instruction.

The mutation walks the table's pending list in order. For each instruction it
scans inputs before outputs. Three masks distinguish lanes overwritten by a
pending output remaining on the old register, lanes overwritten by a pending
output retargeted to the temporary, and lanes needing a copy from the old
value. A nonmovable input, or one touching a pending output left on the old
register, clears the generated instruction's full-period flag. An eligible
input is retargeted by one of two paths per lane:

- An unplaced parent must already belong to the temporary register's lane
  list. The input keeps that parent while it moves to the temporary's lane
  list.
- With a placed parent, the first qualifying input for a lane installs the
  move's own input lock on the old lane's current output and a move output
  lock on the temporary lane. The pending input changes its parent to that
  move output. Later inputs for the lane reuse it.

A pending output on the old register with eligibility 1 moves to the
temporary lane list, keeping its own output lock identity. Other outputs stay
on the old register and extend the old-write mask. If the bookkeeping would
need a value spread across unavailable old and new components, vcl diagnoses
an inconsistent state.

The move mask is the set of lanes where an actual copy was needed. A nonempty
mask is written to the instruction and both operand masks, and the move goes
at the head of the pending list; the table's instruction and move counts are
incremented. An empty mask recycles the move and requests a rewind. The
registration helpers update lane membership, parent pointers and per-parent
user counters explicitly.

## Resolution and two levels of undo

The resolver turns an abstract integer move into a concrete integer copy, and
an abstract vector move into a concrete lower-pipe or upper-pipe copy. It
updates the descriptor, pipe eligibility and the counters for abstract
dual-pipe, upper-only and lower-only moves. The emitter uses the concrete
form to produce IADDIU with zero, MOVE, or MAX with a repeated source
(see [OUTPUT_FORMAT.md](OUTPUT_FORMAT.md#instruction-specific-emitters)).

Undoing a placement differs from removing the generated move. Placement undo
clears the concrete form and restores the abstract descriptor and its move
counter class, then removes the dispatch placement, rolls back installed
locks, and moves the instruction from done to pending. The temporary value
and its retargeted consumers can still exist afterwards.

When the wrangler discards generated moves during rewind, it follows the
pending list backwards from its tail, through each instruction's predecessor
link, up to and including the supplied instruction, saving the next pointer
before removing each move. The structural undo first unresolves a concrete
move if needed and unlinks it from pending. It scans the remaining pending
operands and changes references to the temporary register back to the old
register.

```rust
// Structural sketch for one lane of a pending input.
let parent = input.parent;
let restored_parent = if copied_lane && parent == move_output_lock {
    move_input_lock.parent
} else {
    parent
};
deregister_input(temporary_lane, input);
register_input(old_lane, restored_parent, input);
```

An output is removed from the temporary lane's list and reregistered on the
old lane with its prior owner and timing fields. After restoring pending
operands, undo deregisters the move's source locks on its copied lanes,
decrements instruction and move counts, clears the temporary's lane lists and
returns the record to its pool. A lock-registration error branch logs a
diagnostic and returns before normal recycle, leaving partial mutation; no
guest observation exercises that path.

## Original guest control

`trace-move-long-input.vcl` enables counted-loop scheduling and uses a
temporary VF result at the start and end of a chain of dependent operations.
It has no source copy instructions. With `-t1 -uMOVE`, the original tries
eleven period-7 strategies and reports `Opt::Loop loop 7/7`. It exits 0 and
prints five MOVE instructions in the generated entry part and three in the
seven-pair loop kernel, which confirms that vcl emits synthesized copies for
this fixture. The final assembly does not expose discarded candidates or the
state of the pending list before and after undo. Input:
`local/evidence/inputs/trace-move-long-input.vcl`. Original metadata, stdout
and stderr: `local/evidence/oracle/trace-move-long-input/` (local, not
tracked).

An address trace of the same input captures 33 successful move-selector calls
and 33 concrete resolutions across the eleven strategies. All nonzero reason
values in the failed instructions are 3. For the first attempt, the failed
ADD's operand 2 initially refers to `root__0` and its four locks have reason
3. The activation snapshot shows `vmove move__m0,root__0` with mask `0xf`. In
lane x, the ADD's input-lock object now points to the move's output lock, and
the move's input lock points to the ADD's previous parent: the parent-chain
splice, observed before allocation maps the temporary to a VF number. No
structural undo occurred in this run. Trace:
`local/evidence/traces/move-loop-call/trace-move-long-input/trace.json`
(local, not tracked).
