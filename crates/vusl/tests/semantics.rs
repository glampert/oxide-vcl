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
        let execution = program
            .run(state, vu_model::Config::TIMED, 100_000)
            .unwrap_or_else(|error| panic!("model, seed {seed}: {error}\n{}", compiled.vcl));
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
