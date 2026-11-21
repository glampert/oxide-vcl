# The VUSL compiler

How `oxide-vusl` turns a VUSL program into VCL and then a listing: the phases, what each one
knows, the optimizations lowering does, and the tests. `docs/VUSL.md` is the language as users see
it.

## Crates

```text
crates/vusl/src/
  lib.rs          preprocess(), check() -> Checked, Checked::generate() -> Vcl,
                  Checked::reflection(), assemble(), compile()
  options.rs      Options: include directories, defines, the back-end mode
  diagnostics.rs  VuslMessage: every message, with its wording (no printing)
  syntax/         lexer.rs, ast.rs, parser.rs, print.rs (the tree as text, for messages)
  sema/           check.rs (declarations, layouts, main's bindings), layout.rs (VU packing),
                  elaborate.rs (main with every call inlined, typed and placed), hir.rs (the
                  checked program), types.rs, consteval.rs
  codegen/        mod.rs (lowering), values.rs (what registers hold), peephole.rs, emit.rs
  reflect/        model.rs (the description), json.rs, cpp.rs
  interp.rs       the reference interpreter of the checked program
crates/oxide-vusl/src/
  main.rs, cli.rs (the vusl_cli library target, which the tests share), help.txt
```

Neither crate has external dependencies. The `vcl` crate provides the preprocessor's C-syntax mode
(`docs/PREPROCESSOR.md`), `Message::Frontend` and `Reporter::set_program_name` for diagnostics, and
`vcl::driver` for what both drivers share (`--color=` and writing all files or none).

## Phases

1. **Preprocess** (`vusl::preprocess`). `vcl::preprocessor` in C syntax: `;` is punctuation,
   `#pragma` lines other than `once` stay in the output with their arguments expanded, and
   `__VUSL__` is defined. The line origins it records map every preprocessed line back to its file
   and line, for diagnostics.
2. **Parse** (`syntax`). A lexer over the preprocessed text, where `#pragma` lines become tokens,
   and a recursive-descent parser with C precedence. `?:` and assignments inside expressions are
   parsed so the checker can reject them with a reason.
3. **Check declarations** (`sema/check.rs`). Structs and their layouts, constants, functions and
   file pragmas, then `main`'s parameters: their placements, the overlap check, `alias`, `spill`
   and the double-buffer fit.
4. **Elaborate `main`** (`sema/elaborate.rs`). Names are resolved, every call is inlined, and each
   expression gets its type and placement. The strict rules apply here: an operation with no VU
   instruction, operands in different lanes, a raw lane move, a ref leaving its region, a clip
   judgement out of reach. Constants fold, `#pragma unroll` repeats constant-count loops, and the
   accesses to each member are recorded for reflection. The result is the checked program
   (`sema/hir.rs`).
5. **Reflect** (`reflect`). The description of the parameters and types comes from the checked
   program alone, so `--check` can write it.
6. **Lower** (`codegen`). The checked program becomes VCL text, with a source place per line.
7. **Assemble** (`vusl::assemble`). `vcl::compile` on the text, in oxide mode by default, with `-u`
   set to the program's name so generated labels are unique to it. vcl's diagnostics on the
   generated code are mapped back to VUSL lines through the line origins.

`vusl::compile` runs 6 and 7 up to three times (see [hoisting](#hoisting)).

A file with `#pragma library` and no `main` is a library's own build (`elaborate::library`): its
checked result is the library (its memory, no code) and one *entry* program per export, in
entry-table order. `vusl::compile` compiles each entry on its own, as above, and writes the entry
table before them ([libraries](#libraries)).

### Elaboration

Inlining is by substitution: a function's parameters are symbols in a scope of their own.

- A struct parameter is memory, the caller's place with the parameter's access.
- An `out` or `inout` value parameter is an alias of the caller's target.
- An `in` parameter is:
  - a constant, when the argument is one and the body never assigns the parameter, so that
    conditions on it fold;
  - the caller's local itself, when the argument is a plain local, the body never assigns the
    parameter, no `out` or `inout` argument of the call names the local, and it is not refreshed
    by a yield;
  - otherwise a new local, set from the argument.

Each local carries, as the walk goes, the placement of its value and which components are set.
Paths that join must agree on placements, except for `layout(lane = ...)` locals, whose every
assignment moves into their lanes. A read of an unset component is an error.

Refs carry a `Within`: the parameter, member path and access of the memory they were first set
from. A ref set from an element is normalized to its array, and one set through another ref to that
ref's memory. Reassigning a ref compares only the base and path, and narrows the access.

**The clip rule.** Each `clipw` records its block, its index among the judgements, and an epoch.
`any`, `adc` and `clipped` take one to three judgements:

- all within four of the newest `clipw`, which is how many the flags hold;
- the newest in the current block, since VCL waits for a judgement's flags only within its block;
- none older than a branch that judges, and the epoch advances before every loop body.

Breaking any of these is an error naming the reason.

### The checked program

`sema/hir.rs`. The program is `main`'s body, every call inlined: statements (`SetLocal`, `Store`,
`Copy`, `Kick`, `If`, `Loop`, `Break`, `Continue`, `Exit`, `Yield`, `Barrier`) over typed, placed
expressions. A memory `Place` is a base (a parameter, a ref local, or a range element) plus a
member path, a constant offset and run-time indices. Loops are `Condition` loops (with their
condition's statements, and whether the test comes first) or `Range` loops over elements with a
count. There is no SSA form and no separate IR: lowering works on this tree.

## Lowering

`codegen/mod.rs` is destination-driven. An expression computes into the register its consumer
wants where it can. Vectors stay in the lanes the checker placed them in, and a scalar whose
instruction can write any lane is made in the lane its store or component write wants
(`vf_in_lane`, `any_lane`): Q and P copies, constants through I, `mfir`, negation. There is no
separate lane solver.

The VCL starts with `.syntax new`, `.name`, `.init_vf_all`, `.init_vi_all` and empty `--enter` and
`--exit` blocks, then `.vu_work_area` for a `spill` parameter, `fcset 0` when the program judges,
and XTOP read once into a register. Generated names are `f` or `i`, the source name and a number
(`fpos0_12`, `iroom_5`), which keeps them off hardware names and field letters.

- **Memory places** are a base register and an immediate: VI00 and the address for a fixed
  parameter, the XTOP register for the double buffer, a ref's register, a range element's pointer.
  Lengths read from memory and run-time indices add to the base.
- **Range loops** (`range_loop`) become a do-while behind a guard on the count. There is one pointer
  per element stride and base, which elements of arrays a constant apart share at different
  offsets, and the loop ends when the pointer with the smallest stride reaches its end (`ibne`).
  `unroll(N)` lowers the body N times with the elements N strides apart, then the rest one at a
  time (`unrolled_range`); a run-time count splits with `iand`.
- **Conditions** branch directly on `int16` values or their difference, with immediates for
  constants (`int_branch`). Float comparisons subtract in one lane and test S, or S and Z, with
  `fsand` (`float_branch`). A `bool` as a value is a VI register (`bool_value`).
- **Matrix products** are `mula`/`madda`/`madd` chains on the vector's components as broadcast
  operands. A component that is the constant 1 is VF00's w, and a 0 drops its term.
- **Contraction** (`contract`). A sum of two or more products is one ACC chain, with `msuba` and
  `msub` for the negative terms.
- **Q** (`q_holds`). After a `div`, `sqrt` or `rsqrt`, readers take Q directly (`mulq`, `addq`, and
  `addq`/`mulq` into a lane) until the next FDIV operation or label. Lowering still copies Q into a
  register after each one; the peephole drops the copy when nothing reads it.
- **Qword assembly** (`qword_run`). Consecutive stores to lanes of one qword become one masked `sq`
  when their values can be made in one register's lanes with fewer extra moves than the stores
  saved.
- **Raw moves** (`copy`). vcl may schedule a MOVE in the upper pipe as MAX of the source with itself,
  which flushes bit patterns that read as denormal floats. A copy of raw bits, or of a float local
  whose bits a `floatBitsTo*` conversion reads (`bit_read_locals`), carries an exact form: a reload
  from memory when the value came from there, or four `mr32`s. The form replaces the move if the
  peephole leaves it.
- **Spills** reach VCL as a `.vu_work_area`. When vcl spills, the driver shows a note per value
  kept there.

### Value reuse

`codegen/values.rs` keeps a `Cache` of what registers hold. Its keys are:

- a whole qword loaded with `lq`, by base register and offset;
- an `int16` loaded with `ilw`, by base, offset and lane;
- a sum of two VI registers;
- a pure expression, by a text key (`expr_key`).

Each entry also records the memory reads it depends on, as a `Region` and the base and offset when
known, and the locals and range elements it reads.

- **Stores.** A store forgets the entries whose reads it may change (`may_alias`). Regions are a
  parameter plus a path of members, elements and lane runs. Different parameters overlap only when
  one says `alias`. A ref's region is its `Within`.
- **Local writes.** Writing a local or a register forgets the entries that read it
  (`clobber_vf`, `clobber_vi`).
- **Labels.** Lowering snapshots the cache at a branch and, at the join, keeps the entries every
  path has.
- **Range copies.** Each unrolled copy of a range body forgets what read the previous copy's
  elements.
- **Yields.** After a yield (`resumed`), every entry read from memory goes, and XTOP and the xitop
  locals are read again.

### Hoisting

Before a loop, `invariants` collects the largest pure parts of its body that read nothing the loop
changes (its `Effects`: locals written, memory stored) and cost an instruction: loads, constants,
products of invariants.

- It descends into nested loops.
- It takes a part inside an `if` only when both arms compute it, matched by `expr_key`.
- VI values leave only loops of one block. In a loop with branches they hold VI registers that the
  loop's own counters and pointers need, and vcl spills.

Lowering computes the chosen parts before the loop, and the cache finds them inside it. Each loop
keeps at most `Settings::hoist_limit` values. `vusl::compile` generates the program with limits of
16, 6, then 0 (`HOIST_LIMITS`). It stops at the first compile with no spills, and otherwise keeps
the one with the fewest. A vcl error other than running out of registers or work area ends the
attempts.

## Libraries

A library's calls go through oxide-vcl's `--call` and `--return` (`docs/VCL_REFERENCE.md`,
calls), which give vcl the register contract it cannot see across programs.

- **Declarations** (`sema/check.rs`). `#pragma library` gives a `Library`: the name, the micro
  address, the banks and the saved registers. File-scope memory is bound after `main`'s parameters
  with `Param::library` set, under names of its own that only functions see. The overlap check
  lets library memory that is `uniform` or `in` share a caller's memory. Exports keep their
  declaration order, which is their entry index; a definition replaces its prototype when the
  signatures agree.
- **The calling convention** (`elaborate::call_registers`) is computed the same way for a call and
  for the entry, from the signature alone.
- **A call** (`Elaborator::library_call`). A call of an export from a program is
  `Stmt::Call`: the inlined setup and body, which the interpreter runs, and a `CallArg` per value,
  with its register, the expression passed in and the local it comes back into. A call that sees
  only the prototype has no body. The clip epoch advances: the library judges.
- **Lowering a call** (`Lowering::library_call`). The setup is lowered, then a `--barrier`, so
  the call's block holds only the argument moves and the JALR: vcl gives a value that crosses blocks
  one register for its whole life, and counts every value the call's block mentions as live with
  the registers the call takes. Each argument is computed
  into its physical register (`Output::vf_physical`, a name the peephole passes never take for a
  variable), then `iaddiu entry, vi00, address`, a `--call` block reading the arguments and
  writing the whole bank, VI15 and VI01, and `jalr vi15, entry`. After it the value cache forgets
  what was read from memory other than `uniform` memory, Q is unknown, the clip judgements are out
  of reach, and each result is moved into its local.
- **An entry** (`hir::Entry`). Its parameters are locals bound in `--enter` to their registers, VI15
  with them. `.init_vf` and `.init_vi` keep it to the bank and the saved registers. A saved
  register comes in under a name of its own, is stored to the `save` memory (four VI registers a
  qword, `isw`; a VF register a qword, `sq`), and the code then runs behind a `--barrier`, so the
  register is free there. Before `--return vi15`, behind another barrier, the saved registers are
  loaded back and bound as outputs with the results. A ref parameter's `Within` is any memory
  (`Base::Param(usize::MAX)`), whose stores alias everything but `uniform` memory. Labels take the
  entry's name as a prefix, since entries share one listing.
- **The library's listing** (`compile_library`) is the entry table, `b <entry>_Entry` and a NOP
  pair per export, then each entry's listing, with the local label `<entry>_Entry` right after its
  global `<entry>_CodeStart`. The table branches to the local label because dvp-as leaves a branch
  to a global symbol as a relocation, `R_MIPS_DVP_11_PCREL`, which ld rejects ("unsupported
  relocation").

## The peephole passes

`codegen/peephole.rs` runs over the finished instruction list (`emit::Line`), in this order:

1. **`coalesce_moves`.** A temporary written by one instruction and then moved whole into a register
   is written there directly.
2. **`propagate_copies`.** A copy (`move`, `iaddiu r, t, 0`) whose destination is only read after it
   is replaced by its source. A variable that a declaration line names (an entry's `in_vi
   icursor_3(vi11)`) keeps its name, and counts those lines among its mentions. The source must keep its value up to the destination's last reader
   in program order. A branch from further on back into that stretch extends it to the branch, so
   a loop that changes the source keeps the copy. A branch from before the copy into the stretch
   rules it out (`live_range`). A VI01 source stays within its block, since FCAND rewrites it.
3. **`fold_offsets`.** `iaddiu t, b, k` whose `t` is only used in memory operands, under the same
   conditions, becomes those operands' offsets, while they stay within ±1023.
4. **`exact_raw_moves`.** Raw moves still present become their exact form.
5. **`drop_unread_q_copies`.** Lowering's copy of Q is dropped when nothing reads it.

Scheduling, pairing, register allocation, field narrowing, dead code, pipelining and unswitching
are oxide-vcl's.

## The interpreter

`interp.rs` runs the checked program on a `Machine`: data memory, XTOP, XITOP and the kicks. It
defines what a program means, independently of its code.

- Values are their components, whatever lanes the code puts them in.
- Floats follow the VU: inputs with a zero exponent are zero, results that would be denormal flush,
  and the largest exponent saturates.
- Arithmetic is `f32` in the order the code computes it. A vector over a scalar is the vector times
  the scalar's reciprocal, as the FDIV unit and a Q multiply do.
- `int16` values wrap at 16 bits.

`run_with_host` calls the host at each yield, which may change memory, XTOP and XITOP before the
program resumes.

## Diagnostics

Every message is a `VuslMessage` variant whose wording (text, subject, help) is in
`diagnostics.rs`. It converts to `vcl::Message::Frontend`, so `vcl`'s `Reporter` prints it with the
source quoted through the line origins, notes ("in the call to 'f' here") and colour. Messages for
the strict rules give the placement (`vec2@zw`) and the explicit form to write. No code outside the
driver prints.

## Tests

`VUSL_UPDATE=1` regenerates the expected files of the data-driven suites.

| Suite | What it checks |
|---|---|
| Unit tests (`cargo test -p vusl --lib`) | The lexer, the parser's round trip through `print`, types and placements, layout, constant evaluation, the interpreter's lanes |
| `tests/vusl/check/<case>/` (`crates/oxide-vusl/tests/check.rs`) | `input.vusl` and `expected.stderr`: every error and warning, through the driver. Cases with an `expected.json` and `expected.h` are also the reflection suite |
| `crates/oxide-vusl/tests/reflect.rs` | The reflection of those cases, and every header compiled together with `c++ -std=c++17 -fsyntax-only -Wall -Wextra -Werror -pedantic` when a compiler is present (`CXX` picks it) |
| `tests/vusl/codegen/<case>/` (`crates/oxide-vusl/tests/codegen.rs`) | `expected.vcl`, the `--stop-at-vcl` output, for each idiom below. Each also compiles through vcl |
| `crates/vusl/tests/semantics.rs` | Differential tests: inline programs compiled through oxide-vcl run in `vu_model`'s timed configuration on seeded memories of exactly representable values, and their data memory and kicks must be the interpreter's. `agree_resumed` resumes at yields with new batches; `with_library` lays a program and a library out in one micro memory |
| `crates/vusl/tests/programs.rs` | The ports in `tests/vusl/programs` on their scenes: packets identical to the hand-written program's, no more cycles, and memory and kicks the interpreter's. A `<case>_lib` port runs with its library at the library's address, and instead of the cycle bound takes fewer instructions, its library's included, than the inlined `<case>`. Its ignored `report` test prints `docs/VUSL_PERFORMANCE.md`'s table |
| `crates/vusl/tests/reference.rs` | Every complete program in `docs/VUSL.md` checks and compiles, with its header blocks as include files |
| `crates/oxide-vusl/tests/driver.rs` | The command line: options, outputs written all or none, exit statuses, diagnostics |

The semantics and programs suites use `crates/oxide-vcl/tests/support/vu_model.rs` and
`scenes.rs` by path, as the oxide-vcl tests do. The timed configuration matters: VCL's schedules
rely on Q, P and the flags landing at their latency.

### Idioms

Each case in `tests/vusl/codegen` names the hand-written sequence it stands for:

| Case | Idiom |
|---|---|
| `transform` | Matrix products: the w = 1 term as VF00's w, and a 3×3 product |
| `perspective` | The perspective divide and viewport: `div`, `mulq`, `ftoi4`; Q's copy dropped |
| `dot-normalize` | A dot product as `mul` and two `add`s; normalize with `rsqrt` and `mulq` |
| `clip-adc` | The guard-band judgement (`mul`, `clipw`) and ADC (`fcand`, `iaddiu 0x7fff`) |
| `bool-any` | Whether any vertex is outside, as `fcand` and `ior` into a bool |
| `color-pack` | A colour clamped and packed: `max`, `mini`, `ftoi0` |
| `q-into-lane` | Q into a lane (`addq.y`), and a constant into a lane |
| `lane-moves` | `mr32` rotations and broadcast adds |
| `int-constants` | `int16` immediates, `isubiu` for small negatives, 0x8000 in two instructions |
| `gif-tag` | The `isw.x` NLOOP/EOP patch of a host template |
| `loop-invariants` | Invariants hoisted, and the end-pointer exit test |
| `shared-pointers` | Two arrays stepped by one pointer |
| `multiply-add` | A sum of products as one ACC chain |
| `qword-assembly` | Lane stores built in one register and stored once |
| `load-reuse` | One `lq` for every lane of a qword, again after a store, and raw bits copied |
| `ref-cursor` | A ref as an output cursor, and the count written |
| `unrolled-range` | `unroll(2)` on a run-time count: `iand`, pairs, then the rest |
| `library-call` | A library call: arguments into the bank, the `--call` block, the JALR, the result moved out |
| `library-entry` | A library's entries: the bank's `.init_vf`/`.init_vi`, parameters bound in `--enter`, `--return vi15` |
| `library-saves` | An entry's saved registers: stored behind a barrier on entry, loaded back before `--return` |
| `particles` | The particles port: 16 upper and 16 lower instructions in its loop, where the hand-written loop has 19 and 17 |

## Debugging

- `--stop-at-vcl` and `--emit-vcl=` show the generated VCL. Every line's source place is kept, so a
  VCL error points at the VUSL line.
- `--mode=vcl` compiles the VCL with vcl 1.4beta7's code generation, so a program without
  oxide-only features can be checked against the original under the oracle (`run-original` skill).
- `--check` stops before lowering: types, lanes, layouts and reflection.
- oxide-vcl's `VCL_*` trace switches apply to the generated VCL as to any other
  (`docs/LPGEN_PORT.md`).
