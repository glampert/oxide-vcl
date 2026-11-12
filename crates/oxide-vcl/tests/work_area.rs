//! `.vu_work_area`. vcl 1.4beta7 rejects every work area, and a program whose
//! registers do not fit (observed, the `spill-*` probes); vcl mode does the
//! same. Oxide mode spills the values that do not fit into the area: these
//! tests run its output on the VU model and compare the results with the
//! same program built with every register allowed.

#[path = "support/vu_model.rs"]
mod vu_model;

use vcl::diagnostics::{CompileMessage, Message, ParseMessage};
use vcl_cli as cli;
use vu_model::{Program, State};

/// More VF values live at once than the four registers allowed.
const VECTOR_BODY: &str = "--enter\n\
                           in_vf src(vf10)\n\
                           --endenter\n\
                           add.xyzw ta, src, src\n\
                           mul.xyzw tb, src, src\n\
                           sub.xyzw tc, src, vf00\n\
                           add.xyzw td, ta, src\n\
                           mul.xyzw te, tb, src\n\
                           add.xyzw tf, tc, ta\n\
                           add.xyzw sum1, ta, tb\n\
                           add.xyzw sum2, tc, td\n\
                           add.xyzw sum3, te, tf\n\
                           add.xyzw sum4, sum1, sum2\n\
                           add.xyzw res, sum4, sum3\n\
                           --exit\n\
                           out_vf res(vf11)\n\
                           --endexit\n";

/// More VI values live at once than the two registers allowed.
const INTEGER_BODY: &str = "--enter\n\
                            in_vi base(vi10)\n\
                            --endenter\n\
                            iaddiu ca, base, 1\n\
                            iaddiu cb, base, 2\n\
                            iaddiu cc, base, 3\n\
                            iaddiu cd, base, 4\n\
                            iadd sa, ca, cb\n\
                            iadd sb, cc, cd\n\
                            iadd sc, sa, ca\n\
                            iadd sd, sb, cd\n\
                            iadd tot, sc, sd\n\
                            --exit\n\
                            out_vi tot(vi11)\n\
                            --endexit\n";

/// Three accumulators carried around a loop the optimizer may pipeline, with
/// two VF registers allowed.
const LOOP_BODY: &str = "--enter\n\
                         in_vf src(vf10)\n\
                         in_vi cnt(vi10)\n\
                         --endenter\n\
                         add.xyzw acc1, src, vf00\n\
                         mul.xyzw acc2, src, src\n\
                         add.xyzw acc3, src, src\n\
                         iaddiu left, cnt, 0\n\
                         top:\n\
                         --LoopCS 1,1\n\
                         add.xyzw acc1, acc1, src\n\
                         add.xyzw acc2, acc2, acc1\n\
                         add.xyzw acc3, acc3, acc2\n\
                         isubiu left, left, 1\n\
                         ibne left, vi00, top\n\
                         add.xyzw tot1, acc1, acc2\n\
                         add.xyzw res, tot1, acc3\n\
                         --exit\n\
                         out_vf res(vf11)\n\
                         --endexit\n";

const SOURCE: [f32; 4] = [1.0, 2.0, 0.5, 3.0];

fn compile(
    source: &str,
    mode: &str,
    header: &str,
) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    let options = cli::parse(["-t1", "-uDEMO", mode]).unwrap().compile;
    vcl::compile(&format!("{header}{source}"), "/work/spill.vcl", &options)
}

fn vcl(source: &str, header: &str) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    compile(source, "--mode=vcl", header)
}

fn oxide(source: &str, header: &str) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    compile(source, "--mode=oxide", header)
}

/// The one error of a rejected program.
fn rejection(result: Result<vcl::Compiled, Vec<vcl::CompileError>>) -> Message {
    let errors = result.expect_err("the program is rejected");
    assert_eq!(errors.len(), 1, "{errors:?}");
    errors[0].message.clone()
}

/// Memory with every qword holding its own index, to show stray writes.
fn marked_memory() -> Vec<[u32; 4]> {
    (0..1024)
        .map(|index| [(index as f32).to_bits(); 4])
        .collect()
}

/// Runs a listing with VF10 holding [`SOURCE`] and VI10 holding `count`.
/// The listing runs to its end: vcl's final compression can drop the E row
/// of these programs, or leave it before the last rows (as vcl 1.4beta7
/// does), so its E bits are removed and an ending pair is appended.
fn run(assembly: &str, count: u16) -> State {
    let mut state = State::default();
    state.set_vector(10, SOURCE);
    state.vi[10] = count;
    state.memory = marked_memory();

    let ending = format!("{:9}{:<59}NOP\n{:9}{:<59}NOP\n", "", "NOP[E]", "", "NOP");
    let assembly = format!("{}\n{ending}", assembly.replace("[E]", "   "));
    let execution = Program::parse(&assembly)
        .unwrap()
        .execute(state, 4096)
        .unwrap_or_else(|failure| panic!("{failure}\n{assembly}"));
    assert!(execution.steps < 4096);
    execution.state
}

fn vector(state: &State, register: usize) -> [f32; 4] {
    state.vf[register].map(f32::from_bits)
}

/// The qwords the program changed, outside `area`.
fn stray_writes(state: &State, area: std::ops::Range<usize>) -> Vec<usize> {
    let marked = marked_memory();
    (0..1024)
        .filter(|index| !area.contains(index) && state.memory[*index] != marked[*index])
        .collect()
}

/// The addresses of the listing's VI00-based loads and stores.
fn spill_addresses(assembly: &str) -> Vec<u32> {
    assembly
        .split_whitespace()
        .filter_map(|token| token.split(',').nth(1)?.strip_suffix("(VI00)"))
        .map(|address| address.parse().unwrap())
        .collect()
}

/// What [`VECTOR_BODY`] computes, lane by lane.
fn vector_expected() -> [f32; 4] {
    std::array::from_fn(|lane| {
        let src = SOURCE[lane];
        let one = if lane == 3 { 1.0 } else { 0.0 };
        let (ta, tb, tc) = (src + src, src * src, src - one);
        let (td, te, tf) = (ta + src, tb * src, tc + ta);
        (ta + tb) + (tc + td) + (te + tf)
    })
}

/// The notes that report each spill: name and address.
fn spills(compiled: &vcl::Compiled) -> Vec<(String, u32)> {
    compiled
        .warnings
        .iter()
        .filter_map(|diagnostic| match &diagnostic.message {
            Message::Compile(CompileMessage::RegisterSpilled { name, address, .. }) => {
                Some((name.clone(), *address))
            }
            _ => None,
        })
        .collect()
}

// ----------------------------------------------
// vcl mode: compliance with vcl 1.4beta7
// ----------------------------------------------

#[test]
fn vcl_mode_rejects_every_work_area() {
    // The memory pool is never installed (`spill-vf-area`, `spill-vi-area`).
    for header in [
        ".init_vf vf01-vf04\n.vu_work_area vu1,1000,8\n",
        ".vu_work_area vu0,12,4\n",
        ".vu_work_area vu1,0x3e8,8\n",
    ] {
        let message = rejection(vcl(VECTOR_BODY, header));
        assert_eq!(
            message.vcl_text(),
            "init work area - failed to get vu mem pool",
            "{header}"
        );
    }
}

#[test]
fn vcl_mode_does_not_spill_registers_that_do_not_fit() {
    // vcl fails the table (`spill-vf-none`); the port names the register.
    let message = rejection(vcl(VECTOR_BODY, ".init_vf vf01-vf04\n"));
    assert!(
        matches!(
            message,
            Message::Compile(CompileMessage::AllocationPoolFull { vector: true, .. })
        ),
        "{message:?}"
    );
}

// ----------------------------------------------
// Oxide mode: spilling
// ----------------------------------------------

#[test]
fn oxide_mode_spills_vf_values_that_do_not_fit() {
    let spilled = oxide(
        VECTOR_BODY,
        ".init_vf vf01-vf04\n.vu_work_area vu1,1000,8\n",
    )
    .unwrap();
    let reference = oxide(VECTOR_BODY, ".init_vf_all\n").unwrap();

    let notes = spills(&spilled);
    assert!(!notes.is_empty());
    assert!(
        notes
            .iter()
            .all(|(_, address)| (1000..1008).contains(address))
    );
    assert!(spills(&reference).is_empty());

    let addresses = spill_addresses(&spilled.assembly);
    assert!(!addresses.is_empty(), "{}", spilled.assembly);
    assert!(
        addresses
            .iter()
            .all(|address| (1000..1008).contains(address))
    );

    let state = run(&spilled.assembly, 0);
    assert_eq!(vector(&state, 11), vector_expected());
    assert_eq!(vector(&state, 11), vector(&run(&reference.assembly, 0), 11));
    assert!(stray_writes(&state, 1000..1008).is_empty());
}

#[test]
fn oxide_mode_spills_vi_values_that_do_not_fit() {
    let spilled = oxide(
        INTEGER_BODY,
        ".init_vi vi01-vi02\n.vu_work_area vu1,1000,8\n",
    )
    .unwrap();
    let reference = oxide(INTEGER_BODY, ".init_vi_all\n").unwrap();

    assert!(!spills(&spilled).is_empty());
    assert!(spilled.assembly.contains("isw.x"), "{}", spilled.assembly);
    assert!(spilled.assembly.contains("ilw.x"), "{}", spilled.assembly);

    // From base 5: 6, 7, 8 and 9, then 13, 17, 19, 26 and their sum 45.
    let state = run(&spilled.assembly, 5);
    assert_eq!(state.vi[11], 45);
    assert_eq!(state.vi[11], run(&reference.assembly, 5).vi[11]);
    assert!(stray_writes(&state, 1000..1008).is_empty());
}

/// `--LoopCS 1,1` lets one iteration's stores run past the last. Were the
/// spill stores allowed to, the pipelined loop would overwrite the values
/// the code after it reads (14 instead of 10 after one iteration), so a loop
/// that spills keeps its stores before the exit test.
#[test]
fn oxide_mode_spills_values_carried_around_a_loop() {
    let spilled = oxide(
        LOOP_BODY,
        ".init_vf vf01-vf02\n.init_vi vi01-vi02\n.vu_work_area vu1,1000,8\n",
    )
    .unwrap();
    let reference = oxide(LOOP_BODY, ".init_vf_all\n.init_vi vi01-vi02\n").unwrap();

    assert!(!spills(&spilled).is_empty());

    for count in 1..=6 {
        let state = run(&spilled.assembly, count);
        let expected = run(&reference.assembly, count);
        assert_eq!(
            vector(&state, 11),
            vector(&expected, 11),
            "{count} iterations"
        );
        assert!(stray_writes(&state, 1000..1008).is_empty());
    }
}

#[test]
fn oxide_mode_reads_hexadecimal_work_area_addresses() {
    // vcl reads `0x3e8` as its decimal prefix, zero; oxide mode reads 1000.
    let spilled = oxide(
        VECTOR_BODY,
        ".init_vf vf01-vf04\n.vu_work_area vu1,0x3e8,8\n",
    )
    .unwrap();
    let addresses = spill_addresses(&spilled.assembly);
    assert!(!addresses.is_empty());
    assert!(
        addresses
            .iter()
            .all(|address| (1000..1008).contains(address))
    );
}

#[test]
fn oxide_mode_takes_slots_from_each_work_area_in_turn() {
    let spilled = oxide(
        VECTOR_BODY,
        ".init_vf vf01-vf04\n.vu_work_area vu1,500,1\n.vu_work_area vu1,900,8\n",
    )
    .unwrap();
    let addresses: Vec<_> = spills(&spilled)
        .into_iter()
        .map(|(_, address)| address)
        .collect();
    assert_eq!(addresses[0], 500);
    assert!(
        addresses[1..]
            .iter()
            .enumerate()
            .all(|(index, &address)| address == 900 + index as u32)
    );

    let state = run(&spilled.assembly, 0);
    assert_eq!(vector(&state, 11), vector_expected());
    let mut area = stray_writes(&state, 900..908);
    area.retain(|&index| index != 500);
    assert!(area.is_empty());
}

#[test]
fn oxide_mode_reports_a_full_work_area() {
    let message = rejection(oxide(
        VECTOR_BODY,
        ".init_vf vf01-vf04\n.vu_work_area vu1,1000,2\n",
    ));
    let Message::Compile(CompileMessage::WorkAreaFull { vector, size, .. }) = &message else {
        panic!("{message:?}");
    };
    assert!(*vector);
    assert_eq!(*size, 2);
    assert!(
        message
            .wording()
            .help
            .unwrap()
            .contains(".vu_work_area larger")
    );
}

#[test]
fn oxide_mode_suggests_a_work_area_when_registers_do_not_fit() {
    let message = rejection(oxide(VECTOR_BODY, ".init_vf vf01-vf04\n"));
    assert!(
        matches!(
            message,
            Message::Compile(CompileMessage::AllocationPoolFull { vector: true, .. })
        ),
        "{message:?}"
    );
    assert!(
        message
            .wording()
            .help
            .unwrap()
            .contains(".vu_work_area vu1,")
    );
}

#[test]
fn oxide_mode_rejects_malformed_work_areas() {
    let message = rejection(oxide(VECTOR_BODY, ".vu_work_area vu1,12tail,4\n"));
    assert!(
        matches!(
            &message,
            Message::Compile(CompileMessage::WorkAreaNumber { token }) if token == "12tail"
        ),
        "{message:?}"
    );

    // 0x100 is past vu0's 256 qwords, though vcl reads it as zero.
    let message = rejection(oxide(VECTOR_BODY, ".vu_work_area vu0,0x100,1\n"));
    assert_eq!(message, Message::Parse(ParseMessage::WorkAreaOutsideMemory));
}

#[test]
fn oxide_mode_leaves_programs_that_fit_alone() {
    let header = ".init_vf_all\n";
    let with_area = oxide(VECTOR_BODY, &format!("{header}.vu_work_area vu1,1000,8\n")).unwrap();
    let without = oxide(VECTOR_BODY, header).unwrap();
    assert!(spills(&with_area).is_empty());
    assert_eq!(with_area.assembly, without.assembly);
}
