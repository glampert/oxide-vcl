# VUSL performance

The VUSL ports in `tests/vusl/programs` against the hand-written VCL programs they port, which are
in the public corpus (`tests/corpus`). Each port draws the same packets as its original on every
scene, in fewer cycles.

## Method

- **Compilation.** The original goes through oxide-vcl with `--mode=oxide --opt=all`. The port goes
  through `vusl::compile`, which hands its VCL to the same compiler with the same settings.
- **Runs.** Both run in the VU model (`crates/oxide-vcl/tests/support/vu_model.rs`) with timing
  (`Config::TIMED`). They start from the VU1 states in `crates/oxide-vcl/tests/support/scenes.rs`,
  which the oxide-vcl scene tests also use.
- **Scenes.** Each scene is the batches a host would send for one view:
  - the quake2 triangle scenes are a wall inside the guard band, a floor that the near plane cuts,
    and two keyframe models, one of them across the near plane;
  - the doom3 world scenes are strips seen as a wall, a floor and a side, and the `lit` scenes add
    the lightmap pass.
- **What the test asserts.** `ports_draw_the_same_and_are_no_slower` (`crates/vusl/tests/programs.rs`)
  requires three things on every scene:
  - the packets the port kicks equal the original's;
  - the port takes no more cycles;
  - the port's data memory and kicks are the VUSL interpreter's, work areas aside.

The table comes from the report test:

```sh
cargo test --release -p vusl --test programs report -- --ignored --nocapture
```

## Results

Instructions are the 64-bit pairs in micro memory. Cycles are the model's count for the whole
scene.

| Program | Instructions (port / original) | Scene | Cycles (port) | Cycles (original) | Change |
|---|---|---|---:|---:|---:|
| quake2 particles | 113 / 114 | burst | 1020 | 1189 | −14.2% |
| quake2 textured_triangles | 1234 / 1212 | world wall | 2706 | 2805 | −3.5% |
| | | world floor | 16547 | 16768 | −1.3% |
| | | model | 1379 | 1578 | −12.6% |
| | | model near | 3546 | 3786 | −6.3% |
| doom3 world_strips | 765 / 695 | wall | 2223 | 3896 | −42.9% |
| | | floor | 43799 | 47371 | −7.5% |
| | | side | 16479 | 19314 | −14.7% |
| doom3 world_lm | 1520 / 1019 | wall | 2182 | 2259 | −3.4% |
| | | wall lit | 3291 | 3476 | −5.3% |
| | | floor | 44250 | 46153 | −4.1% |
| | | floor lit | 87354 | 91197 | −4.2% |
| | | side | 16834 | 18224 | −7.6% |
| | | side lit | 32618 | 35403 | −7.9% |

## Where the differences come from

- **particles.**
  - The loop body is 16 upper and 16 lower instructions; the original's is 19 and 17.
  - The loop ends when its input pointer reaches its end, so no counter steps.
- **world_strips.**
  - The original's transform loop has a `--barrier` between its loads and its arithmetic, and vcl
    does not pipeline a loop that holds one. The port's transform loop is one block, which vcl
    pipelines.
  - The clipper's planes are constants in each inlined pass, so their conditions fold.
- **world_lm.**
  - The port inlines its list pass once per draw pass, plain and lightmapped, so the clipper is
    there twice, each copy with its pass's constants folded. The original runs one list pass
    twice, with the pass number kept in memory. That makes the port the larger program.
  - The clipper's planes fold as in world_strips.
- **textured_triangles.**
  - The colour mode and the batch format stay in registers for the whole triangle loop. vcl
    unswitches the loop on both, so each of its four copies tests neither per triangle.
  - The input stride is chosen once.
  - A packed colour is stored from the register its corner was loaded into (`floatBitsToUint`),
    as the original stores it. The clipper keeps a cut's packed colour the same way.
  - The original loads its twelve light qwords again for every triangle and keeps its window
    state in memory. The port spills only the current window, which changes once per full
    window.

## Libraries

`world_strips_lib` and `world_lm_lib` are the two doom3 ports with the clipper taken out into a
library (`tests/vusl/programs/include/clipper.vush` and `clipper.vusl`). The library is resident at
1536, takes 472 instructions, entry table included, and each program calls it. Its calls pass
values in VF24–VF31 and VI10–VI14; it also works in VF16–VF23 and VI06–VI09, which it saves on
entry and puts back. The inlined ports stay as they are, for reference.

| Program | Instructions | With the library | Inlined port |
|---|---:|---:|---:|
| world_strips_lib | 276 | 748 | 765 |
| world_lm_lib | 753 | 1225 | 1520 |
| Both | 1029 | 1501 | 2285 |

The two programs and the library together take 34% fewer instructions than the two inlined ports;
each further program that clips adds its own code and not a clipper.

| Program | Scene | Cycles | Hand-written | Change | Inlined port |
|---|---|---:|---:|---:|---:|
| world_strips_lib | wall | 2762 | 3896 | −29.1% | 2223 |
| | floor | 47649 | 47371 | +0.6% | 43799 |
| | side | 18576 | 19314 | −3.8% | 16479 |
| world_lm_lib | wall | 2190 | 2259 | −3.1% | 2182 |
| | wall lit | 3879 | 3476 | +11.6% | 3291 |
| | floor | 47901 | 46153 | +3.8% | 44250 |
| | floor lit | 95558 | 91197 | +4.8% | 87354 |
| | side | 18732 | 18224 | +2.8% | 16834 |
| | side lit | 37604 | 35403 | +6.2% | 32618 |

The library ports trade cycles for size, so the programs suite holds them to their size, not to
the hand-written program's cycles. The cycles go to:

- **Registers across the call.** A call may change VI10–VI15 and VI01. The list path's loop keeps
  its walk state, the XTOP register among it, live across the call, in what VI registers remain;
  vcl keeps a value that crosses streams in one register for all of them, so some of those values
  live in the work area for the whole program, and the scenes that never clip pay for them too.
- **Each call:** the argument and result moves, the JALR and the entry table's branch, the
  library's saves and restores of VI06–VI09 and VF16–VF23, the JR, and the scheduling that cannot
  cross the call.
- **Specialization:** the clipper's pass is an argument, where world_lm's inlined list pass folds
  it.

## Raw bits

vcl may schedule a MOVE in the upper pipe as MAX of the source with itself, which flushes bit
patterns that read as denormal floats. world_lm's GIF tags hold such patterns, in their REGS lane.
So a copy of raw bits that the peephole passes leave becomes a reload from memory or four `mr32`s
(`docs/plans/VUSL.md`, Units). The same holds for a float whose bits `floatBitsToUint` reads.
