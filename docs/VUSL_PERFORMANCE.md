# VUSL performance

The VUSL programs of quake2-ps2 and doom3-bfg-ps2, copied into `tests/vusl/programs`
(`tests/vusl/programs/README.md` gives the commits), against the hand-written VCL programs they
replaced, which are in the public corpus (`tests/corpus`). Each draws the same packets as the
hand-written program on every scene.

## Method

- **Compilation.** The hand-written program goes through oxide-vcl with `--mode=oxide --opt=all`.
  The VUSL program goes through `vusl::compile`, which hands its VCL to the same compiler with the
  same settings.
- **Runs.** Both run in the VU model (`crates/oxide-vcl/tests/support/vu_model.rs`) with timing
  (`Config::TIMED`). They start from the VU1 states in `crates/oxide-vcl/tests/support/scenes.rs`,
  which the oxide-vcl scene tests also use. A doom3 program runs with the clipper library at its
  address after it.
- **Scenes.** Each scene is the batches a host would send for one view:
  - the quake2 triangle scenes are a wall inside the guard band, a floor that the near plane cuts,
    and two keyframe models, one of them across the near plane;
  - the doom3 world scenes are strips seen as a wall, a floor and a side, and the `lit` scenes add
    the lightmap pass.
- **What the test asserts.** `ports_draw_the_same_and_are_no_slower` (`crates/vusl/tests/programs.rs`)
  requires, on every scene:
  - the packets the VUSL program kicks equal the hand-written program's;
  - its data memory and kicks are the VUSL interpreter's, work areas aside (for a library's
    caller, with the library's source included, since the interpreter runs a call as the export's
    definition);
  - a program that calls no library takes no more cycles than the hand-written one; the library's
    callers instead take fewer instructions together, with the library, than their hand-written
    programs.

The table comes from the report test, and the `profile` test prints where a program spends its
cycles on each scene, by label:

```sh
cargo test --release -p vusl --test programs report -- --ignored --nocapture
VUSL_PROFILE=world_lm cargo test --release -p vusl --test programs profile -- --ignored --nocapture
```

## Results

Instructions are the 64-bit pairs in micro memory. Cycles are the model's count for the whole
scene.

| Program | Instructions (VUSL / hand-written) | Scene | Cycles (VUSL) | Cycles (hand-written) | Change |
|---|---|---|---:|---:|---:|
| quake2 particles | 109 / 114 | burst | 1018 | 1189 | −14.4% |
| quake2 textured_triangles | 1147 / 1212 | world wall | 1801 | 2805 | −35.8% |
| | | world floor | 13750 | 16768 | −18.0% |
| | | model | 1175 | 1578 | −25.5% |
| | | model near | 3202 | 3786 | −15.4% |
| doom3 world_strips | 323 + 458 / 695 | wall | 2184 | 3896 | −43.9% |
| | | floor | 40247 | 47371 | −15.0% |
| | | side | 15863 | 19314 | −17.9% |
| doom3 world_lm | 644 + 458 / 1019 | wall | 2130 | 2259 | −5.7% |
| | | wall lit | 3149 | 3476 | −9.4% |
| | | floor | 40668 | 46153 | −11.9% |
| | | floor lit | 80283 | 91197 | −12.0% |
| | | side | 16220 | 18224 | −11.0% |
| | | side lit | 31451 | 35403 | −11.2% |

The doom3 programs' 458 is the clipper library, resident at 1536 and shared: the two programs and
the library take 1,425 instructions, the two hand-written programs 1,714.

## Where the differences come from

- **particles.**
  - The loop body is 16 upper and 16 lower instructions; the original's is 19 and 17.
  - The loop ends when its input pointer reaches its end, so no counter steps.
- **textured_triangles.**
  - The colour mode and the batch format stay in registers for the whole triangle loop. vcl
    unswitches the loop on both, so each of its four copies tests neither per triangle. The slow
    path (`if (slow)`: seeding the clipper, the warp, the clipping passes and the fan) is one copy
    that all four share, which keeps the program near the hand-written one's size.
  - The input stride is chosen once.
  - The cull and the clipping passes test a sign as a float compare against -1/16, exactly what
    FTOI4 of the value clamped to [-1, 1] gives (FTOI4 truncates, so it is negative only from
    -1/16 down). The compare reads the MAC flags; the original's clamp, FTOI4 and MTIR chain is
    longer.
  - A packed colour is stored from the register its corner was loaded into (`floatBitsToUint`),
    as the original stores it. The clipper keeps a cut's packed colour the same way.
  - The original loads its twelve light qwords again for every triangle and keeps its window
    state in memory. The port spills only the current window, which changes once per full
    window.
- **world_strips.**
  - The original's transform loop has a `--barrier` between its loads and its arithmetic, and vcl
    does not pipeline a loop that holds one. The VUSL transform loop is one block, which vcl
    pipelines.
  - The strip path copies each window's run of vertices with no room test per vertex, so its loop
    is one block too.
- **world_lm.**
  - Its strip path and transform loop gain as world_strips' do; the scenes that clip pay for the
    library call.
- **The clipper library.** Its passes test signs as textured_triangles' do, and a plane's
  distance is `w` scaled less or plus the plane's coordinate, with no negated copy. Each call costs the argument and result moves, the JALR and the entry
  table's branch, the library's saves and restores of VI06–VI09 and VF16–VF23, the JR, and the
  scheduling that cannot cross the call. The clipper's pass is an argument, where an inlined
  clipper folds it. A call may change VI10–VI15 and VI01, so what crosses it must fit in VI02–VI09;
  what does not is stored when written and loaded back after each call (world_lm saves the XTOP
  base, its list walk's counters and its window state; world_strips the XTOP base and two flag
  masks), and the strip paths, which call nothing, keep it all in registers. Three things keep
  these values from costing the rest of the program:
  - **The call's block holds only the call.** vcl gives a value that crosses blocks one register
    for its whole life, and treats every value its block mentions as live with the registers the
    call takes. oxide-vusl puts a `--barrier` before the argument moves, so the code before the
    call no longer counts.
  - **The values that meet a call are assigned registers first.** In vcl's first-free order a
    value that never met a call could take the last register one that does could use.
  - **What still does not fit is saved across the calls, not spilled.** A spilled value is loaded
    before every read, so a spilled XTOP base would be reloaded for every index entry of the strip
    path.

## Raw bits

vcl may schedule a MOVE in the upper pipe as MAX of the source with itself, which flushes bit
patterns that read as denormal floats. world_lm's GIF tags hold such patterns, in their REGS lane.
So a copy of raw bits that the peephole passes leave becomes a reload from memory or four `mr32`s
(`docs/VUSL_COMPILER.md`, Lowering). The same holds for a float whose bits `floatBitsToUint` reads.
