# Indirect edges and call-sensitive tracing

[Control flow](CONTROL_FLOW.md) describes the stream graph, branch
subtypes, and reciprocal references. This chapter describes indirect
target discovery and the subsequent dependency walk in vcl 1.4beta7
(inferred from vcl 1.4beta7 unless stated otherwise).

## Explicit and inferred targets

An explicit colon-separated tag list is split, each named target is resolved,
and the branch is linked to it with the subtype's edge kind: kind 2 for JR
and kind 4 for JALR. A repeated `(destination, kind)` link is rejected, so a
repeated explicit tag fails even though the two strings designate the same
possible destination.

The resolver also searches backwards for the definition of the target
register. It starts immediately before the branch in its own stream
and can revisit that stream from the tail on a predecessor cycle.
On each path it looks for a nonzero output on the same register object.
A branch-and-link setter contributes the stream physically following
that setter. An ordinary setter ends the path and requires explicit
tags. A path can end without finding a nonzero setter; acceptance can then
still depend on later passes. The resolver repeats until it adds no more
edges.

```rust
// Structural sketch. The edge kind is deliberately fixed at 2.
for setter in reverse_reaching_setters(target_register) {
    if setter.is_branch_and_link() {
        let next = setter.stream.physical_successor()?;
        if !has_reciprocal_edge(branch.stream, next, 2) {
            link_streams(branch.stream, next, 2)?;
        }
    } else if !branch.has_explicit_tags() {
        return Err(NeedTagList);
    }
}
```

Inference uses kind 2 for both JR and JALR. Edge validation rejects a new
kind-2 edge from JALR. Edge kind 5 (indirect branch-and-link return) is
validated as kind 2 while the reciprocal records keep the original kind; the
traced call sites do not establish a producer of kind 5. When retargeting
links, the original rewrites branch operand text for absolute edge kinds 1
and 3, and warns for register edge kinds 2, 4, and 5.

## Dependency traversal and interfaces

Each incoming reference carries two flag bytes that control different
decisions: a traversal flag, used by the dependency walk, and an emission
flag, used for stream visibility
([output visibility](OUTPUT_FORMAT.md#three-separate-label-decisions)). The
walk starts with the traversal flag clear for incoming kinds 3 and 4 and set
for kinds 0, 1, 2 and 5. An incoming kind 2, 4, or 5 requires the destination
stream's physical predecessor; the walk enables that predecessor's matching
call edge and can queue the predecessor. A missing physical predecessor,
destination or reciprocal reference is an error. When a stream has incoming
call edges (kinds 3 or 4) and all are disabled, the walk admits all of them;
otherwise it follows only the enabled ones. Caller selection and the
all-disabled fallback are described in
[SHARED_SUBROUTINES.md](SHARED_SUBROUTINES.md).

Indirect targets are resolved before the implicit-interface pass adds
implicit interfaces, so a synthetic interface inserted there cannot supply a
reaching setter during indirect target discovery. At entry, the implicit pass
appends `xin_hw` for sticky ALU and DIV and for each type-3 pool's EXT
sentinel. At normal exit it prepends `xout_hw` for EXT. It also appends
`xin_hw` for each virtual resource in those pools at both entry and normal
exit; the failure message there says “external output” although it concerns
an input. EXITM (end kind 4) receives neither normal-exit insertion.
Load/store tags are subsequently represented as synthetic memory operands
([memory operands](MEMORY.md)).

## Original controls

Fixtures live under `local/evidence/oracle` (local, not tracked). All six ran
at `-t1 -uINDIRECT`, time scale 1, without emulator error. The successful
tiny graphs permit branch removal in later passes. The observations establish
resolution and final output for these sources; the dependency-walk behaviour
above is inferred from vcl 1.4beta7.

| Fixture | Original observation |
| --- | --- |
| [BAL followed by an untagged JR return](../local/evidence/oracle/audit-indirect-inferred/) | Status 0. BAL remains, and the JR folds into the exit path. |
| [JR with a tag and an explicit input address](../local/evidence/oracle/audit-indirect-explicit/) | Status 0. The redundant branch is removed. |
| [Same JR without the tag](../local/evidence/oracle/audit-indirect-untagged/) | Status 255. Diagnostic asks for a tag list. |
| [JR with the same tag twice](../local/evidence/oracle/audit-indirect-duplicate/) | Status 255. Diagnostic reports a reference that is already present. |
| [JALR using the BAL-set register without a tag](../local/evidence/oracle/audit-indirect-jalr-inferred/) | Status 255. Diagnostic reports incompatible link types. |
| [JALR with a tag and an explicit input address](../local/evidence/oracle/audit-indirect-jalr-explicit/) | Status 0. The redundant branch is removed. |
