# Dependency tracing through a shared VU subroutine

Two BAL instructions can call the same subroutine, so its JR has two return
edges. vcl 1.4beta7 must not treat a value defined before one call as
reaching the continuation of the other. This chapter describes the rule
inferred from vcl 1.4beta7 and the three compiler runs (`-t1 -uCALL`, time
scale 1) that check it. How return edges are built is in
[INDIRECT_FLOW.md](INDIRECT_FLOW.md).

## How a continuation selects a caller

Consider this physical layout and its inferred control-flow edges.

```text
caller A ─── physical next ─── continuation A
caller B ─── physical next ─── continuation B
shared subroutine S

caller A ─── call ───┐
                     ├──> S
caller B ─── call ───┘

S ─── return ───> continuation A
S ─── return ───> continuation B
```

Dependency tracing walks backward from a use, one register component at a
time. Inferred from vcl 1.4beta7:

- Call edges into a subroutine start disabled for each trace.
- When the walk crosses a return edge into continuation A, it takes the
  caller from the continuation's **physical predecessor**, A, and enables A's
  call edge into S. B's call edge stays disabled until a trace path
  activates it. A trace started at continuation B enables B's edge instead.
  The same activation applies to indirect-call edges.
- When the walk then extends backward through S, it follows only the enabled
  call edges, so it reaches A and not B.
- If a stream has call edges and all of them are disabled (a trace rooted
  directly in S, for example the link register read by the JR), the walk
  admits all of them, so both callers are reached.
- A caller activated this way is queued directly, even if S was already
  expanded before the activation.

A walk from the first continuation that explored both callers as equally
valid would reach the entry through the second caller without finding the
first caller's definitions. The caller filtering prevents that.

## Guest observations

Fixture outputs are under `local/evidence/oracle` (local, not tracked); inputs
are `local/evidence/inputs/trace-call-*.vcl`. All three fixtures exited
without emulator errors or fired alarms. The accepted inputs have two
`bal VI01,subroutine` instructions and one untagged `jr VI01`, so the return
edges are inferred.

| Fixture | Observed original behavior |
| --- | --- |
| [Sequential calls](../local/evidence/oracle/trace-call-two-continuations/) | Status 0, 12 printed pairs. First continuation computes `preserved` into VF03. The second call uses VF02 for `argument` and the subroutine writes VF04. The second continuation reads VF03 and VF04. |
| [Alternative calls](../local/evidence/oracle/trace-call-alternate-continuations/) | Status 0, 17 printed pairs. The conditional selects between two call sites. Each defines its own saved value into VF03 before the shared subroutine. Each continuation reads its saved value from VF03 and the returned result from VF01. |
| [Missing first saved value](../local/evidence/oracle/trace-call-alternate-unset/) | Status 255. The only source change deletes the first call's definition of `first_saved`. Diagnostics name all four components at `first_return`. |

The alternative-call control isolates the caller-filtering rule: with it, a
trace from the first continuation finds the first call's `first_saved`
definition; without it, the trace would reach the entry through the second
caller and report the value unset. A recorded instrumented run of the
alternative-call source (`local/evidence/traces/move-loop-call/`) shows the
same pattern: a trace from the first continuation enables and enqueues only
caller A, one from the second only caller B, and one rooted in the
subroutine (`VI01`) enqueues both.

The fixtures cover direct BAL with a common JR register. They do not
establish the behaviour for indirect-call (JALR) edges beyond the shared
activation rule above.

## The port's model

`build_flow_graph` gives a call block (`bal`, `jalr`) only its target edges
and records the stream after it as the call's return. `infer_returns`
searches back from each indirect branch for setters of its register: a
call's link write adds a return edge to that call's return stream, any other
setter (or an entry input) needs explicit tags. Return edges add paths, so
the search repeats until nothing changes, and only then reports an error.
Explicit tags keep their own edges alongside the inferred returns.

`flow::Graph::analyze` then copies every subroutine once per call string
(at most eight calls deep, fewer if the copies would exceed a size budget).
A return edge leads only to the top caller's return stream; an explicit tag
on a return leaves every call. The copies are analysed as one flat graph and
folded back: reads merge their definitions per operand, liveness is the
union, and `copy_live_out` keeps each copy's live set so register lifetimes
can give every copy its own rows. That reproduces the caller filtering
above: `trace-call-alternate-continuations` shares VF03 between the two
saved values, and `trace-call-alternate-unset` is rejected.

Scheduling and timing use the uncopied graph with call and return edges.
