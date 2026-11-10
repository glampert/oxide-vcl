# Register allocation and register webs

The descriptions below are inferred from vcl 1.4beta7. Function names are
explanatory; the original is stripped. See
[ALLOCATION_REPAIR.md](ALLOCATION_REPAIR.md) for how the port integrates
occupancy and allocation-aware scheduling,
[ALLOCATION_INTERVALS.md](ALLOCATION_INTERVALS.md) for lock and interval
evidence, and [EXTERNAL_ALLOCATION.md](EXTERNAL_ALLOCATION.md) for the
pre-scheduling external policy, UTA/VSM mask and range rules, reference discovery
order, binding across loop expansion and the generated-move lifecycle.

The port applies external first-free choices to ordinary unscheduled VF/VI
streams as stable pins before local scheduling. The local raster packing stage
keeps its own policy. Selected/raw/expanded-loop and partial VF availability
lifecycles use the earlier path.

## Two distinct allocation stages

vcl performs external allocation across streams before local instruction
scheduling. It resets allocation state, visits the stream list, requires table
allocation state 1, and calls each of twelve register-pool objects through their
external-allocation virtual method. Afterwards it replaces externally mapped
virtual operands with physical registers, marks the table state 2, and cleans up.

Local allocation requires state 2, scheduled/VSM table mode 3, valid register
locks and no unscheduled instructions. It invokes each pool's local-allocation
method, cleans up, and advances the table to state 3. Scheduling and allocation
therefore interact: a candidate schedule may fail local allocation and cause the
scheduler to try another candidate.

## Per-component time occupancy

vcl's generic local allocator uses these tables and globals:

| Storage | Meaning |
| --- | --- |
| Temporary table, 4096 bytes | Occupancy for one virtual register, one byte per time row |
| Accumulated table, indexed `row * 32 + physical` | Accumulated physical-register occupancy |
| Per-pool byte array by physical ID | Components the user allows the allocator to use |
| Global | Component count for the active pool |
| Global | Number of candidate physical registers |
| Two globals | Active half-open time range |
| Global | Modulo period, zero for a linear table |

Low bits represent independent components; bit `0x80` is an additional lock
condition. It participates in collisions but is excluded from the available
component and packing-score calculations. A VF is not a single indivisible
allocation unit: two virtual values can occupy disjoint components of the same VF
at the same time.

The lock-table builder walks register-component lock lists, asks the interval
routine for each live interval, and ORs the component bit into every row in
`[start, end)`. For loops it indexes `row % period`. External input and output
sentinels are adjusted to the active table bounds or loop boundaries. The
sentinel end `30000` means live beyond the table, not an actual scheduling cycle.
A nonnegative extra lock row sets bit `0x80`.

## Candidate scoring

Explanatory Rust for vcl's cost function (not recovered source):

```rust
fn packing_cost(allowed: u8, candidate: &[u8], occupied: &[u8], parts: u32)
    -> Option<u32>
{
    let mut cost = 0;
    for (&want, &used) in candidate.iter().zip(occupied) {
        if allowed & want & 0x7f != want & 0x7f || want & used != 0 {
            return None;
        }
        cost += parts - ((want | used) & 0x7f).count_ones();
    }
    Some(cost)
}
```

Physical IDs are visited in ascending order. A zero-cost fit returns immediately.
Otherwise strictly lower cost replaces the best candidate, so ties keep the
earlier physical ID. This favours packing new components into already occupied
registers rather than spreading values.

The commit step rechecks availability and overlap, then commits each row using
OR. A failed schedule must discard its allocation state before retrying.

## Virtual-register priority

The local allocator first installs forced mappings. It scans instruction operands,
builds each virtual register's temporary occupancy, and stores the number of
nonzero rows as its priority. Newly found virtual registers are prepended to a
pending list.

It repeatedly selects the pending register with the **largest** occupied-row
count, using a strict comparison, so equal-priority values keep pending-list
order (the reverse of discovery order). A nearby diagnostic says "lowest
register", but the comparison selects the largest count. It then chooses a
physical ID by the packing metric, commits occupancy and removes the register from
the pending list.

Failure produces a diagnostic about the pool and `.init_..` availability; no
spill-to-memory fallback was observed. Once all values are assigned, operands are
rewritten to their physical mappings.

## External assignment

For each unmapped virtual external reference, vcl clears candidate exclusion
masks, runs its disjointness exclusion pass across all streams, and picks the
first physical register with zero exclusion byte. This differs from local best-fit
packing.

The exclusion pass compares the external's component liveness against forced or
already mapped references. In VSM mode it computes interval overlap per component,
with separate handling for loop-through values and additional locks. Other
supported table modes use input/output/through masks. The exclusion byte is
accumulated per physical register.

## Register webs

vcl clears operand web pointers, builds per-table webs, gathers connected strands
and patches each table. This runs before global allocation and again after
transformations. A virtual source name alone is not a sufficient allocation
identity: different reaching definitions and merged strands must be represented.

## Limitations

The generic packing algorithm and its tie-breaking are established. Exact
interval endpoints for every special register-lock type, all twelve pool
subclasses, move-induced implicit web patches and inter-stream strand merging are
not reconstructed or differentially tested. The port's packing primitive alone
does not establish whole-program allocation compatibility.
