//! What programs compute: each program here is compiled through oxide-vcl
//! and run in the VU model, whose data memory and XGKICK addresses after
//! the run must be the reference interpreter's.
//!
//! The model runs timed: VCL's schedules rely on Q, P and the flags
//! changing only when their latency has passed. The memory is filled with
//! small powers of two and their sums, which the model's `f32` and the
//! interpreter's compute exactly; each program sets the lengths and
//! divisors it reads.

#[path = "../../oxide-vcl/tests/support/vu_model.rs"]
mod vu_model;

use vcl::preprocessor::MemoryFiles;
use vusl::interp::{self, Machine};

/// Values every operation in these programs keeps exact.
const VALUES: [f32; 12] = [
    1.0, 2.0, 4.0, 0.5, 0.25, -1.0, -2.0, -0.5, 3.0, -3.0, 8.0, 1.5,
];

struct Compiled {
    checked: vusl::Checked,
    vcl: String,
    assembly: String,
}

fn compile(source: &str) -> Compiled {
    compile_with(source, &MemoryFiles::new())
}

fn compile_with(source: &str, files: &MemoryFiles) -> Compiled {
    let options = vusl::Options::default();
    let preprocessed = vusl::preprocess("test.vusl", source, &options, files).unwrap();
    let checked = match vusl::check("test.vusl", &preprocessed) {
        Ok(checked) => checked,
        Err(failure) => panic!("{}", failure.error),
    };
    let (vcl, compiled) = match vusl::compile(&checked, &options) {
        Ok(compiled) => compiled,
        Err(failure) => match *failure {
            vusl::CompileFailure::Generate(failure) => panic!("{}", failure.error),
            vusl::CompileFailure::Vcl { vcl, errors } => panic!("{}\n{errors:?}", vcl.text),
        },
    };
    Compiled {
        checked,
        vcl: vcl.text,
        assembly: compiled.assembly,
    }
}

/// Memory of exact values, from a seed.
fn random_memory(seed: u64) -> Vec<[u32; 4]> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..1024)
        .map(|_| {
            let mut qword = [0; 4];
            for lane in &mut qword {
                *lane = VALUES[(next() % VALUES.len() as u64) as usize].to_bits();
            }
            qword
        })
        .collect()
}

/// Runs `source` both ways on `seeds` memories, prepared by `setup`
/// (which may also set XTOP and XITOP), and compares the results.
fn agree(source: &str, seeds: u64, setup: impl Fn(&mut Vec<[u32; 4]>, &mut u16, &mut u16)) {
    agree_compiled(compile(source), seeds, setup);
}

fn agree_compiled(
    compiled: Compiled,
    seeds: u64,
    setup: impl Fn(&mut Vec<[u32; 4]>, &mut u16, &mut u16),
) {
    agree_except(compiled, seeds, 0..0, setup);
}

/// As [`agree_compiled`], with the qwords `ignored` (a work area only the
/// compiled code writes) left out of the comparison.
fn agree_except(
    compiled: Compiled,
    seeds: u64,
    ignored: std::ops::Range<usize>,
    setup: impl Fn(&mut Vec<[u32; 4]>, &mut u16, &mut u16),
) {
    let program = vu_model::Program::parse(&compiled.assembly)
        .unwrap_or_else(|error| panic!("{error}\n{}", compiled.assembly));
    for seed in 0..seeds {
        let mut memory = random_memory(seed);
        let (mut top, mut itop) = (0, 0);
        setup(&mut memory, &mut top, &mut itop);
        let mut machine = Machine::new(memory.clone(), top, itop);
        interp::run(&compiled.checked.program, &mut machine, 100_000)
            .unwrap_or_else(|error| panic!("interpreter: {error}"));
        let state = vu_model::State {
            memory,
            top,
            itop,
            ..vu_model::State::default()
        };
        let mut execution = program
            .run(state, vu_model::Config::TIMED, 100_000)
            .unwrap_or_else(|error| panic!("model, seed {seed}: {error}\n{}", compiled.vcl));
        for qword in ignored.clone() {
            execution.state.memory[qword] = machine.memory[qword];
        }
        let differences: Vec<String> = execution
            .state
            .memory
            .iter()
            .zip(&machine.memory)
            .enumerate()
            .filter(|(_, (model, reference))| model != reference)
            .take(8)
            .map(|(address, (model, reference))| {
                let show = |qword: &[u32; 4]| {
                    qword.map(|bits| format!("{} ({bits:#x})", f32::from_bits(bits)))
                };
                format!(
                    "qword {address}: model {:?}, interpreter {:?}",
                    show(model),
                    show(reference)
                )
            })
            .collect();
        assert!(
            differences.is_empty(),
            "seed {seed}:\n{}\n--- generated VCL\n{}",
            differences.join("\n"),
            compiled.vcl
        );
        let kicks: Vec<u16> = execution
            .state
            .kicks
            .iter()
            .map(|(address, _)| *address)
            .collect();
        assert_eq!(kicks, machine.kicks, "seed {seed}: kicks");
    }
}

/// Runs `compiled` both ways on `seeds` memories as [`agree_compiled`]
/// does, resuming at each `yield` after `host` changes the memory, XTOP and
/// XITOP, for as long as it returns true; it learns how many runs ended.
#[allow(clippy::type_complexity)]
fn agree_resumed(
    compiled: Compiled,
    seeds: u64,
    setup: impl Fn(&mut Vec<[u32; 4]>, &mut u16, &mut u16),
    host: impl Fn(usize, &mut Vec<[u32; 4]>, &mut u16, &mut u16) -> bool,
) {
    let program = vu_model::Program::parse(&compiled.assembly)
        .unwrap_or_else(|error| panic!("{error}\n{}", compiled.assembly));
    for seed in 0..seeds {
        let mut memory = random_memory(seed);
        let (mut top, mut itop) = (0, 0);
        setup(&mut memory, &mut top, &mut itop);
        let mut machine = Machine::new(memory.clone(), top, itop);
        let mut runs = 0;
        interp::run_with_host(
            &compiled.checked.program,
            &mut machine,
            100_000,
            &mut |machine| {
                runs += 1;
                host(
                    runs,
                    &mut machine.memory,
                    &mut machine.top,
                    &mut machine.itop,
                )
            },
        )
        .unwrap_or_else(|error| panic!("interpreter: {error}"));
        let mut state = vu_model::State {
            memory,
            top,
            itop,
            ..vu_model::State::default()
        };
        let mut start = 0;
        let mut runs = 0;
        loop {
            let execution = program
                .run_from(state, start, vu_model::Config::TIMED, 100_000)
                .unwrap_or_else(|error| panic!("model, seed {seed}: {error}\n{}", compiled.vcl));
            state = execution.state;
            start = execution.resume;
            runs += 1;
            if start >= program.pair_count()
                || !host(runs, &mut state.memory, &mut state.top, &mut state.itop)
            {
                break;
            }
        }
        assert_eq!(
            state.memory, machine.memory,
            "seed {seed}\n{}",
            compiled.vcl
        );
        let kicks: Vec<u16> = state.kicks.iter().map(|(address, _)| *address).collect();
        assert_eq!(kicks, machine.kicks, "seed {seed}: kicks");
    }
}

fn set_int(memory: &mut [[u32; 4]], qword: usize, lane: usize, value: u16) {
    memory[qword][lane] = u32::from(value);
}

fn set_float(memory: &mut [[u32; 4]], qword: usize, lane: usize, value: f32) {
    memory[qword][lane] = value.to_bits();
}

#[test]
fn arithmetic_lanes_and_constants() {
    let source = "struct Particle { uint32 color; vec3 pos; };\n\
                  struct Block {\n\
                  \x20   vec4 a; vec4 b; Particle p;\n\
                  \x20   vec4 sum; vec4 product; vec3 offset; float scale;\n\
                  \x20   vec4 negated; vec4 clamped; vec4 built; vec4 shifted; uvec4 bits;\n\
                  };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   vec4 a = block.a;\n\
                  \x20   vec4 b = block.b;\n\
                  \x20   block.sum = a + b * 2.0;\n\
                  \x20   block.product = a * b.y - b;\n\
                  \x20   block.offset = block.p.pos + 1.5;\n\
                  \x20   block.scale = a.x * b.w + a.z;\n\
                  \x20   block.negated = -a;\n\
                  \x20   block.clamped = clamp(abs(b), 0.5, a.w * 2.0);\n\
                  \x20   block.built = vec4(block.p.pos.zx, 1.0, a.y);\n\
                  \x20   layout(lane = x) vec3 moved = block.p.pos;\n\
                  \x20   block.shifted.xyz = moved * min(a.x, b.y);\n\
                  \x20   block.shifted.w = max(a.z, 0.0);\n\
                  \x20   block.bits.x = block.p.color;\n\
                  \x20   block.bits.yzw = uvec3(ftoi4(block.p.pos));\n\
                  }\n";
    agree(source, 8, |_, _, _| {});
}

#[test]
fn matrices_both_ways() {
    let source = "struct Frame {\n\
                  \x20   mat4 columns;\n\
                  \x20   layout(row_major) mat4 rows;\n\
                  \x20   vec4 v; vec3 p; float pad;\n\
                  \x20   vec4 out0; vec4 out1; vec4 out2;\n\
                  \x20   mat4 product;\n\
                  };\n\
                  void main(layout(address = 0) inout Frame frame)\n\
                  {\n\
                  \x20   frame.out0 = frame.columns * frame.v;\n\
                  \x20   frame.out1 = vec4(frame.p, 1.0) * frame.rows;\n\
                  \x20   mat4 m = frame.columns;\n\
                  \x20   #pragma unroll\n\
                  \x20   for (int16 c = 0; c < 4; ++c)\n\
                  \x20       m[c].xyz *= frame.v.xyz;\n\
                  \x20   frame.out2 = m * vec4(frame.p.zyx, 0.0);\n\
                  \x20   frame.product = frame.columns * m;\n\
                  }\n";
    agree(source, 8, |_, _, _| {});
}

#[test]
fn divisions_and_roots() {
    let source = "struct Block { vec4 v; float d; float square; vec4 q; vec2 r; };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   block.q = block.v / block.d;\n\
                  \x20   block.q.w = 1.0 / block.d;\n\
                  \x20   block.r.x = sqrt(block.square);\n\
                  \x20   block.r.y = block.v.x / block.d * rsqrt(block.square);\n\
                  }\n";
    agree(source, 8, |memory, _, _| {
        // d is a power of two, and square a power of four.
        set_float(memory, 1, 0, 0.5);
        set_float(memory, 1, 1, 16.0);
    });
}

#[test]
fn int16_control_flow() {
    let source = "struct Block { int16 n; int16 flags; int16 out0; int16 out1; vec4 values; vec4 out2; };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   int16 total = 0;\n\
                  \x20   for (int16 i = 0; i < block.n; ++i)\n\
                  \x20   {\n\
                  \x20       if (i == 2)\n\
                  \x20           continue;\n\
                  \x20       total += i;\n\
                  \x20       if (total > 9)\n\
                  \x20           break;\n\
                  \x20   }\n\
                  \x20   block.out0 = total | 0x8000;\n\
                  \x20   int16 left = block.n;\n\
                  \x20   int16 steps = 0;\n\
                  \x20   while (left >= 2 && (block.flags & 1) != 0)\n\
                  \x20   {\n\
                  \x20       left = left - 2;\n\
                  \x20       steps++;\n\
                  \x20   }\n\
                  \x20   block.out1 = -steps;\n\
                  \x20   if (block.values.x < block.values.y || block.values.z >= 2.0)\n\
                  \x20       block.out2 = block.values * 2.0;\n\
                  \x20   else\n\
                  \x20       block.out2 = block.values;\n\
                  }\n";
    agree(source, 12, |memory, _, _| {
        set_int(memory, 0, 0, 7);
        set_int(memory, 0, 1, 3);
    });
}

#[test]
fn ranges_copies_and_kicks() {
    let source = "struct In { vec3 pos; float weight; };\n\
                  struct Out { vec4 scaled; ivec3 fixed; int16 index; };\n\
                  struct Packet { qword tag; Out items[]; };\n\
                  struct Batch {\n\
                  \x20   layout(lane = w) int16 count;\n\
                  \x20   vec4 scale;\n\
                  \x20   qword tag;\n\
                  \x20   In items[count];\n\
                  \x20   Packet packet;\n\
                  };\n\
                  void main(layout(xtop, vif_base = 100, vif_offset = 300) inout Batch batch)\n\
                  {\n\
                  \x20   batch.packet.tag = batch.tag;\n\
                  \x20   int16 index = 0;\n\
                  \x20   for (In item : batch.items, out Out result : batch.packet.items)\n\
                  \x20   {\n\
                  \x20       result.scaled = vec4(item.pos * batch.scale.xyz, item.weight);\n\
                  \x20       result.fixed = ftoi4(item.pos);\n\
                  \x20       result.index = index;\n\
                  \x20       index++;\n\
                  \x20   }\n\
                  \x20   xgkick(batch.packet);\n\
                  }\n";
    agree(source, 6, |memory, top, _| {
        *top = 100;
        set_int(memory, 100, 3, 5);
        // The packet's tag, copied from the batch: NLOOP 0 with EOP, so the
        // model reads one qword.
        memory[102] = [0x8000, 0, 0, 0];
    });
}

#[test]
fn clipping_and_adc() {
    let source = "struct Block { vec4 a; vec4 b; vec4 c; int16 adc0; int16 adc1; int16 anyOut; int16 pad; };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   clipflags ja = clipw(block.a.xyz, block.a.w);\n\
                  \x20   clipflags jb = clipw(block.b.xyz, block.b.w);\n\
                  \x20   clipflags jc = clipw(block.c.xyz, block.c.w);\n\
                  \x20   block.adc0 = adc(jc);\n\
                  \x20   block.adc1 = adc(ja, jb);\n\
                  \x20   block.anyOut = 0;\n\
                  \x20   if (any(ja, jb, jc))\n\
                  \x20       block.anyOut = 1;\n\
                  }\n";
    agree(source, 16, |_, _, _| {});
}

#[test]
fn inlined_functions_with_memory_and_out_parameters() {
    let source = "struct Corner { vec4 st; ivec3 xyz; int16 adc; };\n\
                  struct Block { vec4 clip; vec4 scale; Corner corners[2]; int16 total; };\n\
                  vec3 place(vec3 x, vec3 scale, vec3 offset) { return x * scale + offset; }\n\
                  void emit(out Corner corner, vec4 clip)\n\
                  {\n\
                  \x20   float q = 1.0 / clip.w;\n\
                  \x20   corner.st = vec4(clip.xyz * q, q);\n\
                  \x20   corner.xyz = ftoi4(clip.xyz);\n\
                  \x20   corner.adc = 0;\n\
                  }\n\
                  void count(inout int16 total, int16 amount) { total += amount; }\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   int16 total = 1;\n\
                  \x20   vec4 clip = vec4(place(block.clip.xyz, block.scale.xyz, block.clip.xyz), 2.0);\n\
                  \x20   emit(block.corners[0], clip);\n\
                  \x20   emit(block.corners[1], block.clip * 2.0);\n\
                  \x20   count(total, 3);\n\
                  \x20   count(total, total);\n\
                  \x20   block.total = total;\n\
                  }\n";
    agree(source, 8, |memory, _, _| {
        // clip.w is a divisor: a power of two.
        set_float(memory, 0, 3, 0.5);
    });
}

/// The check suite's particle program (quake2-ps2's particles.vcl in VUSL),
/// on a batch of three particles with a matrix that leaves w at 1.
#[test]
fn values_are_reused_only_while_they_hold() {
    let source = "struct Block {\n\
                  \x20   vec4 a;\n\
                  \x20   float f; float g; vec2 h;\n\
                  \x20   vec4 out0; vec4 out1; vec4 out2;\n\
                  \x20   int16 n; int16 m; int16 o0; int16 o1;\n\
                  };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   vec4 first = block.a;\n\
                  \x20   block.a.y = block.f;\n\
                  \x20   vec4 second = block.a;\n\
                  \x20   block.out0 = first + second;\n\
                  \x20   float f = block.f;\n\
                  \x20   block.f = block.g;\n\
                  \x20   block.out2.x = f + block.f;\n\
                  \x20   float s = 1.0;\n\
                  \x20   for (int16 i = 0; i < block.n; ++i)\n\
                  \x20   {\n\
                  \x20       block.out1 = block.out1 + block.a;\n\
                  \x20       s = s * 2.0;\n\
                  \x20       block.out2.y = s + block.g * 3.0;\n\
                  \x20       if (i == 1)\n\
                  \x20           continue;\n\
                  \x20       block.out2.z = s + block.g * 3.0;\n\
                  \x20   }\n\
                  \x20   if (block.m > 2)\n\
                  \x20       block.out0 = block.a * 2.0;\n\
                  \x20   else\n\
                  \x20       block.a = block.out1;\n\
                  \x20   block.out1 = block.a * 2.0;\n\
                  \x20   int16 m = block.m;\n\
                  \x20   block.m = m + 1;\n\
                  \x20   block.o0 = block.m + 1;\n\
                  \x20   block.o1 = m + 1;\n\
                  }\n";
    agree(source, 12, |memory, _, _| {
        // m above 2 on some seeds and not on others.
        let m = (memory[0][0] >> 23 & 3) as u16 + 1;
        set_int(memory, 5, 0, 3);
        set_int(memory, 5, 1, m);
    });
}

#[test]
fn elements_and_loop_invariants() {
    let source = "struct Item { vec3 v; float k; };\n\
                  struct Row { vec4 weight; int16 n; Item items[4]; };\n\
                  struct Batch {\n\
                  \x20   layout(lane = w) int16 count;\n\
                  \x20   vec4 scale;\n\
                  \x20   Row rows[count];\n\
                  };\n\
                  void main(layout(xtop, vif_base = 100, vif_offset = 300) inout Batch batch)\n\
                  {\n\
                  \x20   for (inout Row row : batch.rows)\n\
                  \x20   {\n\
                  \x20       for (inout Item item : row.items)\n\
                  \x20       {\n\
                  \x20           item.v = item.v * batch.scale.xyz + row.weight.xyz;\n\
                  \x20           item.k = item.v.y;\n\
                  \x20           row.weight.x = item.k + 1.0;\n\
                  \x20       }\n\
                  \x20       row.n = row.n + 1;\n\
                  \x20       batch.scale.y = row.weight.x;\n\
                  \x20   }\n\
                  }\n";
    agree(source, 6, |memory, top, _| {
        *top = 100;
        set_int(memory, 100, 3, 3);
    });
}

#[test]
fn multiply_adds_lanes_and_shared_pointers() {
    let source = "struct V { vec4 a; vec4 b; };\n\
                  struct O { vec4 sum; vec4 lanes; };\n\
                  struct Block {\n\
                  \x20   vec4 s; vec4 t;\n\
                  \x20   V vs[3];\n\
                  \x20   O os[3];\n\
                  \x20   vec4 out0; vec4 out1;\n\
                  \x20   int16 n;\n\
                  };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   for (V v : block.vs, out O o : block.os)\n\
                  \x20   {\n\
                  \x20       o.sum = v.a * block.s + v.b * block.t - v.a * v.b.x;\n\
                  \x20       o.lanes.x = 2.0;\n\
                  \x20       o.lanes.y = -v.a.z;\n\
                  \x20       o.lanes.z = 1.0 / v.b.w;\n\
                  \x20       o.lanes.w = -v.b.y;\n\
                  \x20   }\n\
                  \x20   vec4 k;\n\
                  \x20   k.w = 3.0;\n\
                  \x20   k.x = -block.t.y;\n\
                  \x20   k.y = float(block.n);\n\
                  \x20   k.z = sqrt(block.s.x * block.s.x);\n\
                  \x20   block.out0 = k;\n\
                  \x20   block.out1.xy = block.s.xy * block.t.x + block.t.xy * block.s.y;\n\
                  }\n";
    let compiled = compile(source);
    // One pointer for both arrays, whose elements are both two qwords; one
    // chain of multiply-adds; one store for the four lanes.
    assert!(!compiled.vcl.contains("io_"), "{}", compiled.vcl);
    assert!(compiled.vcl.contains("msub.xyzw"), "{}", compiled.vcl);
    assert!(
        compiled.vcl.contains("sq.xyzw    fqword"),
        "{}",
        compiled.vcl
    );
    agree_compiled(compiled, 8, |memory, _, _| {
        for (element, divisor) in [2.0, 0.5, 4.0].into_iter().enumerate() {
            set_float(memory, 3 + 2 * element, 3, divisor);
        }
        set_int(memory, 16, 0, 5);
    });
}

#[test]
fn loops_keep_fewer_invariants_when_registers_run_out() {
    // 24 uniform qwords read in a loop that keeps 24 products live: with
    // every invariant in a register the VF registers do not suffice.
    let mut source = String::from(
        "struct Uniforms { vec4 u[24]; };\n\
         struct In { vec4 v; };\n\
         struct Out { vec4 o[12]; };\n\
         struct Batch { layout(lane = w) int16 count; In items[count]; Out outs[count]; };\n\
         void main(layout(address = 0) uniform Uniforms k,\n\
         \x20         layout(xtop, vif_base = 100, vif_offset = 400) inout Batch batch)\n\
         {\n\
         \x20   for (In e : batch.items, out Out r : batch.outs)\n\
         \x20   {\n",
    );
    for index in 0..24 {
        source.push_str(&format!("        vec4 a{index} = e.v * k.u[{index}];\n"));
    }
    for index in 0..12 {
        source.push_str(&format!(
            "        r.o[{index}] = a{index} + a{};\n",
            23 - index
        ));
    }
    source.push_str("    }\n}\n");
    let compiled = compile(&source);
    let options = vusl::Options::default();
    let Ok(all_hoisted) = compiled.checked.generate() else {
        panic!("the code is generated");
    };
    let Err(errors) = vusl::assemble(&all_hoisted, "test", &options) else {
        panic!("too many registers with every invariant hoisted");
    };
    assert!(
        errors.iter().any(|error| matches!(
            error.message,
            vcl::Message::Compile(vcl::diagnostics::CompileMessage::AllocationPoolFull { .. })
        )),
        "{errors:?}"
    );
    agree_compiled(compiled, 3, |memory, top, _| {
        *top = 100;
        set_int(memory, 100, 3, 2);
    });
}

#[test]
fn bool_values() {
    let source = "struct Corner { vec4 clip; };\n\
                  struct Block {\n\
                  \x20   vec4 a;\n\
                  \x20   int16 n; int16 m; int16 o0; int16 o1;\n\
                  \x20   int16 o2; int16 o3; int16 o4; int16 o5;\n\
                  \x20   Corner corners[3];\n\
                  };\n\
                  int16 flag(bool value)\n\
                  {\n\
                  \x20   int16 result = 0;\n\
                  \x20   if (value)\n\
                  \x20       result = 1;\n\
                  \x20   return result;\n\
                  }\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   bool less = block.a.x < block.a.y;\n\
                  \x20   bool greater = block.a.z > 1.0;\n\
                  \x20   bool same = block.a.x == block.a.w;\n\
                  \x20   bool differ = block.a.y != block.a.z;\n\
                  \x20   bool ordered = block.n < block.m;\n\
                  \x20   bool both = less && greater;\n\
                  \x20   bool none = !(same || differ);\n\
                  \x20   bool outside = false;\n\
                  \x20   for (Corner corner : block.corners)\n\
                  \x20       outside = outside || any(clipw(corner.clip.xyz, corner.clip.w));\n\
                  \x20   block.o0 = flag(less) + flag(greater) + flag(same);\n\
                  \x20   block.o1 = flag(differ) + flag(block.n != block.m) + flag(ordered);\n\
                  \x20   block.o2 = flag(both) + flag(none);\n\
                  \x20   block.o3 = flag(outside);\n\
                  \x20   block.o4 = flag(block.a.x <= block.a.y) + flag(block.a.y >= block.a.x);\n\
                  }\n";
    agree(source, 24, |memory, _, _| {
        let n = (memory[0][0] >> 23 & 3) as u16;
        let m = (memory[0][1] >> 23 & 3) as u16;
        set_int(memory, 1, 0, n);
        set_int(memory, 1, 1, m);
    });
}

#[test]
fn ranges_counted_by_xitop() {
    let source = "struct Vertex { vec4 pos; };\n\
                  struct Out { vec4 scaled; };\n\
                  struct Batch { vec4 scale; Vertex vertices[count]; Out outs[count]; qword tag; };\n\
                  void main(layout(xitop) int16 count,\n\
                  \x20         layout(xtop, vif_base = 100, vif_offset = 400) inout Batch batch)\n\
                  {\n\
                  \x20   for (Vertex v : batch.vertices, out Out o : batch.outs)\n\
                  \x20       o.scaled = v.pos * batch.scale;\n\
                  \x20   batch.tag.x = count | 0x8000;\n\
                  \x20   xgkick(batch.tag);\n\
                  }\n";
    for count in [0, 1, 5] {
        agree(source, 3, |memory, top, itop| {
            *top = 100;
            *itop = count;
            memory[101 + 2 * count as usize] = [0, 0, 0, 0];
        });
    }
}

#[test]
fn yields_resume_with_the_next_batch() {
    let source = "struct Frame { vec4 scale; };\n\
                  struct Item { vec4 value; };\n\
                  struct Batch { Item items[n]; };\n\
                  void main(layout(address = 0) uniform Frame frame,\n\
                  \x20         layout(xitop) int16 n,\n\
                  \x20         layout(xtop, vif_base = 100, vif_offset = 200) inout Batch batch)\n\
                  {\n\
                  \x20   vec4 scale = frame.scale * 2.0;\n\
                  \x20   while (true)\n\
                  \x20   {\n\
                  \x20       for (inout Item item : batch.items)\n\
                  \x20           item.value = item.value * scale;\n\
                  \x20       yield;\n\
                  \x20   }\n\
                  }\n";
    let compiled = compile(source);
    // The first batch at 100, then two more in the other half and back,
    // each of another length, and a frame that changes between them.
    agree_resumed(
        compiled,
        4,
        |_, top, itop| {
            *top = 100;
            *itop = 3;
        },
        |runs, memory, top, itop| {
            memory[0][1] = 3.0f32.to_bits();
            *top = if runs % 2 == 1 { 300 } else { 100 };
            *itop = runs as u16 + 1;
            runs < 3
        },
    );
}

#[test]
fn refs_as_cursors() {
    let source = "struct Vertex { vec4 pos; };\n\
                  struct Out { vec4 value; };\n\
                  struct Pair { vec4 first; vec4 second; };\n\
                  struct Window { qword tag; Out items[8]; Pair pairs[4]; };\n\
                  struct Frame { vec4 scale; int16 window; int16 back; int16 o0; int16 o1; };\n\
                  struct Batch { layout(lane = w) int16 count; Vertex vertices[count]; };\n\
                  void put(ref<Out> at, vec4 value)\n\
                  {\n\
                  \x20   at.value = value;\n\
                  }\n\
                  void fill(out Pair pair, vec4 value)\n\
                  {\n\
                  \x20   pair.first = value;\n\
                  \x20   pair.second = value * 2.0;\n\
                  }\n\
                  void main(layout(address = 0) inout Frame frame,\n\
                  \x20         layout(address = 10) inout Window window,\n\
                  \x20         layout(xtop, vif_base = 100, vif_offset = 300) in Batch batch)\n\
                  {\n\
                  \x20   ref<Out> cursor = ref<Out>(window.items);\n\
                  \x20   ref<Out> start = cursor;\n\
                  \x20   ref<Pair> pairs = ref<Pair>(window.pairs);\n\
                  \x20   for (Vertex v : batch.vertices)\n\
                  \x20   {\n\
                  \x20       if (v.pos.x < 1.0)\n\
                  \x20       {\n\
                  \x20           put(cursor, v.pos * frame.scale);\n\
                  \x20           cursor += 1;\n\
                  \x20       }\n\
                  \x20   }\n\
                  \x20   fill(pairs, frame.scale);\n\
                  \x20   pairs[1].first = window.items[0].value;\n\
                  \x20   pairs = pairs + 2;\n\
                  \x20   pairs.second.x = 4.0;\n\
                  \x20   window.tag.x = (cursor - start) | 0x8000;\n\
                  \x20   frame.window = int16(cursor);\n\
                  \x20   ref<Out> back = ref<Out>(window.items, frame.back);\n\
                  \x20   back.value.y = 8.0;\n\
                  \x20   frame.o0 = int16(back[1].value.x);\n\
                  \x20   if (cursor != start)\n\
                  \x20       frame.o1 = 1;\n\
                  \x20   xgkick(window.tag);\n\
                  }\n";
    agree(source, 12, |memory, top, _| {
        *top = 100;
        set_int(memory, 100, 3, 6);
        // The ref read from memory: the third item.
        set_int(memory, 1, 1, 13);
    });
}

#[test]
fn unrolled_ranges() {
    let source = "struct Item { vec4 value; };\n\
                  struct Out { vec4 value; int16 index; int16 pad0; int16 pad1; int16 pad2; };\n\
                  struct Block { vec4 scale; Item fixed[7]; Item two[2]; };\n\
                  struct Batch {\n\
                  \x20   layout(lane = w) int16 count;\n\
                  \x20   Item items[count];\n\
                  \x20   Out outs[count];\n\
                  };\n\
                  void main(layout(address = 0) inout Block block,\n\
                  \x20         layout(xtop, vif_base = 100, vif_offset = 300) inout Batch batch)\n\
                  {\n\
                  \x20   #pragma unroll(3)\n\
                  \x20   for (inout Item item : block.fixed)\n\
                  \x20       item.value = item.value * block.scale;\n\
                  \x20   #pragma unroll\n\
                  \x20   for (inout Item item : block.two)\n\
                  \x20       item.value = item.value + block.scale;\n\
                  \x20   int16 index = 0;\n\
                  \x20   #pragma unroll(4)\n\
                  \x20   #pragma pipeline(min = 1)\n\
                  \x20   for (Item item : batch.items, out Out result : batch.outs)\n\
                  \x20   {\n\
                  \x20       result.value = item.value * block.scale.x;\n\
                  \x20       result.index = index;\n\
                  \x20       index++;\n\
                  \x20   }\n\
                  }\n";
    for count in [0, 1, 3, 4, 6, 9] {
        agree(source, 2, |memory, top, _| {
            *top = 100;
            set_int(memory, 100, 3, count);
        });
    }
}

#[test]
fn values_that_do_not_fit_spill_to_the_work_area() {
    // 34 vectors computed before the loop and read in every iteration:
    // more than the VF registers, whatever the schedule.
    let mut source = String::from(
        "struct Uniforms { vec4 u[34]; };\n\
         struct In { vec4 v; };\n\
         struct Out { vec4 o[17]; };\n\
         struct Scratch { qword q[32]; };\n\
         struct Batch { layout(lane = w) int16 count; In items[count]; Out outs[count]; };\n\
         void main(layout(address = 0) uniform Uniforms k,\n\
         \x20         layout(address = 900, spill) inout Scratch scratch,\n\
         \x20         layout(xtop, vif_base = 100, vif_offset = 400) inout Batch batch)\n\
         {\n",
    );
    for index in 0..34 {
        source.push_str(&format!("    vec4 c{index} = k.u[{index}] * 2.0;\n"));
    }
    source.push_str(
        "    for (In e : batch.items, out Out r : batch.outs)\n\
         \x20   {\n",
    );
    for index in 0..17 {
        source.push_str(&format!(
            "        r.o[{index}] = e.v * c{index} + c{};\n",
            33 - index
        ));
    }
    source.push_str("    }\n}\n");
    let compiled = compile(&source);
    assert!(
        compiled.vcl.contains(".vu_work_area vu1, 900, 32"),
        "{}",
        compiled.vcl
    );
    agree_except(compiled, 2, 900..932, |memory, top, _| {
        *top = 100;
        set_int(memory, 100, 3, 2);
    });
}

#[test]
fn efu_results() {
    let source = "struct Block {\n\
                  \x20   vec3 a; float s;\n\
                  \x20   vec4 b;\n\
                  \x20   float t; float u; float v; float pad;\n\
                  \x20   vec4 out0; vec4 out1; vec4 out2;\n\
                  };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   block.out0.x = esadd(block.a);\n\
                  \x20   block.out0.y = eleng(block.a);\n\
                  \x20   block.out0.z = esum(block.b);\n\
                  \x20   block.out0.w = esqrt(block.s);\n\
                  \x20   block.out1.x = ercpr(block.t);\n\
                  \x20   block.out1.y = ersqrt(block.u);\n\
                  \x20   block.out1.z = ersadd(block.a);\n\
                  \x20   block.out1.w = erleng(block.a);\n\
                  \x20   block.out2.x = esin(block.v);\n\
                  \x20   block.out2.y = eatan(block.v);\n\
                  \x20   block.out2.z = eexp(block.v);\n\
                  }\n";
    // Inputs whose results are exact: (1, 2, 2) has length 3, and
    // sin, atan and e^-x at 0.
    agree(source, 4, |memory, _, _| {
        for (lane, value) in [1.0, 2.0, 2.0, 4.0].into_iter().enumerate() {
            set_float(memory, 0, lane, value);
        }
        for (lane, value) in [0.5, 4.0, 0.0, 0.0].into_iter().enumerate() {
            set_float(memory, 2, lane, value);
        }
    });
}

#[test]
fn raw_bits_move_exactly() {
    // Small integers read as denormal floats, which a MAX would flush: a
    // copy of them between registers must stay exact.
    let source = "struct Block { ivec4 a; ivec4 b; int16 n; int16 pad0; int16 pad1; int16 pad2; ivec4 out0; ivec4 out1; };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   ivec4 chosen = block.a;\n\
                  \x20   if (block.n > 0)\n\
                  \x20       chosen = block.b;\n\
                  \x20   ivec4 kept = chosen;\n\
                  \x20   if (block.n > 1)\n\
                  \x20       kept = mr32(chosen);\n\
                  \x20   block.out0 = kept;\n\
                  \x20   block.out1 = mr32(mr32(block.a));\n\
                  }\n";
    agree(source, 6, |memory, _, _| {
        let n = (memory[3][0] >> 23 & 3) as u16;
        memory[0] = [1, 0x512, 3, 0x7fff];
        memory[1] = [0x8000_0005, 6, 0x200, 9];
        set_int(memory, 2, 0, n);
    });
}

#[test]
fn float_bits_read_exactly() {
    // Floats whose bits are read keep them through copies, even bits that
    // read as denormals; reading them costs no instruction.
    let source = "struct Block { vec4 a; vec4 b; int16 n; int16 pad0; int16 pad1; int16 pad2; uvec4 out0; vec4 out1; uint32 low; uint32 high; float pad3; float pad4; };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   vec4 chosen = block.a;\n\
                  \x20   if (block.n > 0)\n\
                  \x20       chosen = block.b;\n\
                  \x20   vec4 kept = chosen;\n\
                  \x20   if (block.n > 1)\n\
                  \x20       kept = mr32(chosen);\n\
                  \x20   block.out0 = floatBitsToUint(kept);\n\
                  \x20   block.out1 = uintBitsToFloat(floatBitsToUint(block.b));\n\
                  \x20   block.low = floatBitsToUint(kept.x);\n\
                  \x20   block.high = floatBitsToUint(kept.y);\n\
                  }\n";
    let compiled = compile(source);
    assert!(!compiled.vcl.contains("ftoi"), "{}", compiled.vcl);
    agree_compiled(compiled, 6, |memory, _, _| {
        let n = (memory[3][0] >> 23 & 3) as u16;
        memory[0] = [1, 0x512, 3, 0x7fff];
        memory[1] = [0x8000_0005, 6, 0x200, 9];
        set_int(memory, 2, 0, n);
    });
}

#[test]
fn in_parameters_alias_only_what_the_call_leaves_alone() {
    // `grow(w, w)` writes `w` through `to` while reading `v`: `v` is a copy
    // of `w` there. `twice(v)` reads `v` itself.
    let source = "struct Block { vec4 a; vec4 out0; vec4 out1; };\n\
                  vec4 twice(vec4 v) { return v + v; }\n\
                  void grow(vec4 v, out vec4 to) { to = v + v; to = to + v; }\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   vec4 v = block.a;\n\
                  \x20   block.out0 = twice(v);\n\
                  \x20   vec4 w = v;\n\
                  \x20   grow(w, w);\n\
                  \x20   block.out1 = w;\n\
                  }\n";
    let compiled = compile(source);
    let a = [1.0_f32, 2.0, 0.5, -1.5];
    let mut memory = vec![[0_u32; 4]; 1024];
    memory[0] = a.map(f32::to_bits);
    let mut machine = Machine::new(memory, 0, 0);
    interp::run(&compiled.checked.program, &mut machine, 1000).unwrap();
    assert_eq!(machine.memory[1], a.map(|v| (v * 2.0).to_bits()));
    assert_eq!(machine.memory[2], a.map(|v| (v * 3.0).to_bits()));
    agree_compiled(compiled, 4, |_, _, _| {});
}

#[test]
fn copies_stay_while_a_loop_changes_their_source() {
    // `start` copies `from` before the loop, which advances `from` after
    // reading `start`: the copy cannot read `from` instead.
    let source = "struct Delta { int16 count; int16 pad0; int16 pad1; int16 pad2; };\n\
                  struct Block { int16 n; int16 pad0; int16 pad1; int16 pad2; vec4 data[8]; Delta deltas[8]; };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   ref<vec4> from = ref<vec4>(block.data);\n\
                  \x20   ref<vec4> start = from;\n\
                  \x20   ref<Delta> to = ref<Delta>(block.deltas);\n\
                  \x20   int16 left = block.n;\n\
                  \x20   do\n\
                  \x20   {\n\
                  \x20       to.count = from - start;\n\
                  \x20       from += 1;\n\
                  \x20       to += 1;\n\
                  \x20       left--;\n\
                  \x20   } while (left > 0);\n\
                  }\n";
    agree(source, 6, |memory, _, _| {
        let n = 1 + (memory[3][0] >> 23 & 3) as u16;
        set_int(memory, 0, 0, n);
    });
}

/// A library's header, its source, and where it lives in micro memory.
const LIBRARY_HEADER: &str = "#pragma once\n\
                              #pragma library(mathlib, address = 400, vf = 16, vi = 11)\n\
                              struct Scratch { vec4 kept; vec4 pad; };\n\
                              layout(address = 900) inout Scratch scratch;\n\
                              export float twiceSum(vec4 v, inout int16 count);\n\
                              export void store(ref<vec4> at, vec4 value);\n\
                              export int16 bump(int16 value);\n";
const LIBRARY_SOURCE: &str = "#include \"mathlib.vush\"\n\
                              export float twiceSum(vec4 v, inout int16 count)\n\
                              {\n\
                              \x20   count = count + 1;\n\
                              \x20   scratch.kept = v;\n\
                              \x20   return dot(v, v) + v.x;\n\
                              }\n\
                              export void store(ref<vec4> at, vec4 value)\n\
                              {\n\
                              \x20   at[0] = value * 2.0;\n\
                              }\n\
                              export int16 bump(int16 value) { return value + 3; }\n";
const LIBRARY_ADDRESS: usize = 400;

/// `caller` compiled, and the library at its address after it, as one
/// program. The caller includes the library's source, so the interpreter
/// runs the exports' bodies where the code calls them.
fn with_library(caller: &str) -> Compiled {
    let mut files = MemoryFiles::new();
    files.insert("mathlib.vush", LIBRARY_HEADER);
    files.insert("mathlib.vusl", LIBRARY_SOURCE);
    let library = compile_with(LIBRARY_SOURCE, &files);
    let program = compile_with(caller, &files);
    let pairs = vu_model::Program::parse(&program.assembly)
        .unwrap()
        .pair_count();
    assert!(pairs <= LIBRARY_ADDRESS, "the caller takes {pairs} pairs");
    let padding = format!("{:9}{:<59}NOP\n", "", "NOP").repeat(LIBRARY_ADDRESS - pairs);
    Compiled {
        checked: program.checked,
        vcl: format!("{}\n{}", program.vcl, library.vcl),
        assembly: format!("{}\n{padding}{}", program.assembly, library.assembly),
    }
}

#[test]
fn a_library_listing_is_one_vu_section() {
    // dvp-as rejects a `.vu` inside a VU section, so the entries' own
    // listings lose theirs behind the entry table's.
    let mut files = MemoryFiles::new();
    files.insert("mathlib.vush", LIBRARY_HEADER);
    let library = compile_with(LIBRARY_SOURCE, &files);
    let sections = library
        .assembly
        .lines()
        .filter(|line| line.trim() == ".vu")
        .count();
    assert_eq!(sections, 1, "{}", library.assembly);
    assert!(
        library
            .assembly
            .starts_with("; mathlib: a VUSL library of 3 exports\n")
    );
    for export in ["twiceSum", "store", "bump"] {
        let start = format!("mathlib_{export}_CodeStart:");
        assert!(library.assembly.contains(&start), "{}", library.assembly);
        // The entry table branches to a local label right after it: ld
        // cannot relocate dvp-as's branch to a global symbol.
        let local = format!("mathlib_{export}_Entry");
        assert!(
            library.assembly.contains(&format!("{start}\n{local}:\n")),
            "{}",
            library.assembly
        );
        assert!(
            library
                .assembly
                .lines()
                .any(|line| line.contains(" b ") && line.trim_end().ends_with(&local)),
            "{}",
            library.assembly
        );
        assert!(!library.assembly.contains(&format!(".global\t{local}")));
    }
}

#[test]
fn library_calls_mean_their_exports() {
    // Values live across the calls stay out of the library's registers;
    // results come back into variables, and stores through a ref reach
    // the caller's memory.
    let source = "#include \"mathlib.vusl\"\n\
                  struct Block { vec4 a; vec4 b; int16 n; int16 m; int16 k; int16 pad1; vec4 out0; float f; float g; float h; float j; };\n\
                  void main(layout(address = 0) inout Block block)\n\
                  {\n\
                  \x20   vec4 kept = block.b + block.a;\n\
                  \x20   int16 n = block.n;\n\
                  \x20   float s = twiceSum(block.a, n);\n\
                  \x20   store(ref<vec4>(block.out0), block.b);\n\
                  \x20   int16 k = bump(n);\n\
                  \x20   for (int16 i = 0; i < 2; ++i)\n\
                  \x20       k = bump(k);\n\
                  \x20   block.f = s;\n\
                  \x20   block.g = kept.y;\n\
                  \x20   block.h = kept.w;\n\
                  \x20   block.m = n;\n\
                  \x20   block.k = k;\n\
                  }\n";
    let compiled = with_library(source);
    assert_eq!(compiled.vcl.matches("jalr").count(), 4, "{}", compiled.vcl);
    agree_compiled(compiled, 6, |memory, _, _| {
        let n = (memory[5][1] >> 23 & 63) as u16;
        set_int(memory, 2, 0, n);
    });
}

#[test]
fn the_particle_program() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/vusl");
    let source = std::fs::read_to_string(format!("{root}/check/particles/input.vusl")).unwrap();
    let frame = std::fs::read_to_string(format!("{root}/include/frame.vush")).unwrap();
    let mut files = MemoryFiles::new();
    files.insert("../../include/frame.vush", frame);
    agree_compiled(compile_with(&source, &files), 8, |memory, top, _| {
        // Frame at 0: the matrix's w column (0, 0, 0, 1), a GS scale of
        // powers of two, a clip scale of 1.
        for row in 0..4 {
            set_float(memory, row, 3, if row == 3 { 1.0 } else { 0.0 });
        }
        for lane in 0..4 {
            set_float(memory, 4, lane, [2.0, 2.0, 1.0, 1.0][lane]);
            set_float(memory, 6, lane, 1.0);
        }
        // The batch at 8: three particles, and the first GIF tag with EOP
        // (NLOOP 0), which the packet's copy starts with.
        *top = 8;
        set_int(memory, 8, 3, 3);
        memory[12] = [0x8000, 0, 0, 0];
    });
}
