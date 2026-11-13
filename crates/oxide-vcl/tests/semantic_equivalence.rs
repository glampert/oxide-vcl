//! Compare observable results, independently of issue-pair layout. See
//! docs/TESTING.md for the deliberately restricted numerical/timing contract.

#[path = "support/vu_model.rs"]
mod vu_model;

use std::path::Path;
use vcl_cli as cli;
use vu_model::{Program, State};

struct Escapes<'a> {
    vf: &'a [(usize, u8)],
    vi: &'a [usize],
    q: bool,
}

fn seeded_state(seed: u64) -> State {
    let mut random = seed.wrapping_add(0x9e3779b97f4a7c15);
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    let mut state = State::default();
    for register in 1..32 {
        state.set_vector(
            register,
            std::array::from_fn(|_| ((next() % 16 + 1) as f32) / 4.0),
        );
    }
    for qword in &mut state.memory {
        *qword = std::array::from_fn(|_| (((next() % 16 + 1) as f32) / 4.0).to_bits());
    }
    for register in &mut state.vi[1..16] {
        *register = (next() % 24 + 2) as u16;
    }
    // The retained loop fixtures have positive incoming VI01 trip counts;
    // memory fixtures use it as a base. Keep both within the modeled region.
    state.vi[1] = (seed % 8 + 1) as u16;
    state.vi[2] = 32;
    state.acc = [0.5_f32.to_bits(); 4];
    state.q = 0.5_f32.to_bits();
    state.p = 2.0_f32.to_bits();
    state.i = 1.0_f32.to_bits();
    state
}

fn compare(case: &str, escapes: Escapes<'_>) {
    let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("local/evidence/oracle")
        .join(case);
    let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
    let original = std::fs::read_to_string(directory.join("stdout.bin")).unwrap();
    let mut flags = vec!["-t1", "-uORACLE"];
    if case.ends_with("-disabled") {
        flags.push("-L");
    }
    if matches!(
        case,
        "cleanup-normal-noreduce"
            | "cleanup-normal-min4-noreduce"
            | "raw-boundary-no-compress"
            | "raw-boundary-min4-no-compress"
    ) {
        flags.push("-C");
    }
    let options = cli::parse(flags).unwrap().compile;
    let result = vcl::compile(&source, &format!("/work/{case}.vcl"), &options)
        .unwrap_or_else(|errors| panic!("{case}: port rejected original fixture: {errors:?}"));
    if case.starts_with("immediate-scheduled-")
        || case.starts_with("immediate-pipeline-")
        || case == "loop-vector-counted"
        || case.starts_with("pipeline-raw-stage")
        || case.starts_with("cleanup-")
        || case.starts_with("raw-boundary-")
    {
        assert!(
            result.assembly.contains("__MAIN_LOOP:"),
            "{case}: optimization silently fell back"
        );
    }
    let expected =
        Program::parse(&original).unwrap_or_else(|e| panic!("{case}: original parse: {e}"));
    let actual =
        Program::parse(&result.assembly).unwrap_or_else(|e| panic!("{case}: port parse: {e}"));
    for seed in 0..64 {
        let mut initial = seeded_state(seed);
        if case == "coalesce-join-blocked"
            || case == "external-vi-join"
            || case.starts_with("flow-delay-")
            || case.starts_with("boundary-ilw-")
        {
            initial.vi[2] = (seed % 2) as u16;
        }
        if case == "flow-delay-tail-debt" {
            let address = usize::from(initial.vi[1]);
            initial.memory[address][0] = (seed % 16 + 32) as u32;
        }
        if case.starts_with("boundary-ilw-") {
            let address = usize::from(initial.vi[1]);
            for (lane, value) in initial.memory[address].iter_mut().enumerate() {
                *value = (seed % 16 + 32 + lane as u64) as u32;
            }
        }
        if case == "raw-boundary-old-read" {
            // MTIR observes low bits from the paired OLD VF03 input. Other
            // arithmetic still uses the seeded small dyadic VF01 values.
            initial.vf[3][0] = 1.0_f32.to_bits() | (seed + 1) as u32;
        }
        if case == "raw-boundary-min2" {
            initial.vi[1] = (seed % 8 + 2) as u16;
        }
        if matches!(
            case,
            "branch-forward-conditional" | "raw-delay-conditional-join"
        ) {
            initial.vi[1] = (seed % 4) as u16;
        }
        if matches!(
            case,
            "pipeline-raw-stages-min4"
                | "cleanup-normal-min4"
                | "cleanup-normal-min4-noreduce"
                | "raw-boundary-min4"
                | "raw-boundary-min4-no-compress"
        ) {
            // LoopCS is a caller promise. Exercise only its valid trip counts.
            initial.vi[1] = (seed % 8 + 4) as u16;
        }
        let expected = expected
            .execute(initial.clone(), 4096)
            .unwrap_or_else(|e| panic!("{case} seed {seed}: original execution: {e}"));
        let actual = actual
            .execute(initial, 4096)
            .unwrap_or_else(|e| panic!("{case} seed {seed}: port execution: {e}"));
        assert!(expected.steps > 0 && actual.steps > 0);
        for &(register, mask) in escapes.vf {
            for lane in 0..4 {
                if mask & (1 << lane) != 0 {
                    assert_eq!(
                        actual.state.vf[register][lane], expected.state.vf[register][lane],
                        "{case} seed {seed}: VF{register:02} lane {lane}"
                    );
                }
            }
        }
        for &register in escapes.vi {
            assert_eq!(
                actual.state.vi[register], expected.state.vi[register],
                "{case} seed {seed}: VI{register:02}"
            );
        }
        if escapes.q {
            assert_eq!(actual.state.q, expected.state.q, "{case} seed {seed}: Q");
        }
        assert_eq!(
            actual.state.memory, expected.state.memory,
            "{case} seed {seed}: memory"
        );
    }
}

macro_rules! kernel {
    ($test:ident, $case:literal, [$($register:literal : $mask:literal),*], [$($vi:literal),*]) => {
        #[test]
        #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
        fn $test() { compare($case, Escapes { vf: &[$(($register, $mask)),*], vi: &[$($vi),*], q: false }); }
    };
}

kernel!(straightline_add, "sched-add-symbol", [2:15], []);
kernel!(component_mask, "mask-output", [2:7], []);
kernel!(allocation_reuses_disjoint_components, "alloc-interval-components", [4:3], []);
kernel!(simultaneous_disjoint_writes_use_distinct_registers, "timing-collision-packed-webs", [2:7, 3:8], []);
kernel!(accumulator_chain, "hw-mula-madd", [2:15], []);
kernel!(declared_scalar_q, "hw-mulq-declared-q", [2:15], []);
kernel!(raw_simultaneous_old_vector_read, "raw-read-before-write", [2:15], [2]);
kernel!(raw_load_after_upper_read, "raw-load-before-write", [1:15, 2:15], []);
kernel!(integer_backedge, "loop-integer-backedge", [], [1]);
kernel!(vector_backedge, "loop-vector-backedge", [2:15], [1]);
kernel!(counted_vector_loop, "loop-vector-counted", [2:15], [1]);
kernel!(unconditional_forward_branch, "branch-forward-unconditional", [2:15], []);
kernel!(conditional_forward_branch, "branch-forward-conditional", [2:15], []);
kernel!(case_insensitive_target, "branch-target-case", [2:15], []);
kernel!(load_postincrement, "memory-load-increment", [2:15], [1]);
kernel!(load_predecrement, "memory-load-decrement", [2:15], [1]);
kernel!(store_postincrement, "memory-store-increment", [], [1]);
kernel!(store_predecrement, "memory-store-decrement", [], [1]);
kernel!(store_load_order, "memory-store-load-order", [2:15], []);
kernel!(affine_after_increment, "immediate-scheduled-after-add", [2:15], [1, 2]);
kernel!(affine_before_increment, "immediate-scheduled-add", [2:15], [1, 2]);
kernel!(affine_after_decrement, "immediate-scheduled-after-subtract", [2:15], [1, 2]);

kernel!(staged_raw_affine_before_add, "immediate-pipeline-before-add", [2:15], [1, 2]);
kernel!(staged_raw_affine_before_subtract, "immediate-pipeline-before-subtract", [2:15], [1, 2]);

kernel!(staged_raw_backward_textual_definition, "pipeline-raw-stage-backref", [2:15], [1]);

kernel!(staged_raw_minimum_one, "pipeline-raw-stages-min1", [2:15], [1]);
kernel!(staged_raw_minimum_four, "pipeline-raw-stages-min4", [2:15], [1]);
kernel!(staged_raw_ignores_disable_search, "pipeline-raw-stages-disabled", [2:15], [1]);

kernel!(unprefixed_raw_retains_linear_pairs, "pipeline-raw-default-disabled", [2:15], [1]);

kernel!(raw_delay_vf_reaches_taken_target, "raw-delay-vf-taken", [3:15], []);
kernel!(
    raw_delay_vi_reaches_taken_target,
    "raw-delay-vi-taken",
    [],
    [3]
);
kernel!(raw_delay_definition_reaches_both_successors, "raw-delay-conditional-join", [3:15], []);

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn strict_missing_input_in_raw_delay_is_rejected() {
    let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("local/evidence/oracle/raw-delay-vf-missing-input");
    let metadata = std::fs::read_to_string(directory.join("result.json")).unwrap();
    assert!(metadata.contains("\"exit_status\": 255"));
    let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
    let options = cli::parse(["-t1", "-uORACLE"]).unwrap().compile;
    let errors = vcl::compile(&source, "/work/raw-delay-vf-missing-input.vcl", &options)
        .expect_err("original rejects missing incoming VF01");
    assert!(!errors.is_empty());
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn oracle_software_pipeline_matches_source_memory_recurrence() {
    // An independent scalar reference for the actual original optimized loop:
    // p += 3; c = memory[p+5]; b += c*a; count--. This checks stage expansion
    // and the 5 -> 8 immediate correction, rather than only comparing ports.
    let original = std::fs::read_to_string(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle/immediate-scheduled-after-add/stdout.bin"),
    )
    .unwrap();
    let program = Program::parse(&original).unwrap();
    for seed in 0..64 {
        let initial = seeded_state(seed);
        let count = initial.vi[1];
        let mut base = initial.vi[2];
        let mut sum = initial.vf[2].map(f32::from_bits);
        let multiplier = initial.vf[1].map(f32::from_bits);
        for _ in 0..count {
            base += 3;
            for lane in 0..4 {
                sum[lane] +=
                    f32::from_bits(initial.memory[(base + 5) as usize][lane]) * multiplier[lane];
            }
        }
        let output = program.execute(initial, 4096).unwrap().state;
        assert_eq!(
            output.vf[2],
            sum.map(f32::to_bits),
            "seed {seed}: recurrence"
        );
        assert_eq!(output.vi[1], 0);
        assert_eq!(output.vi[2], base);
    }
}

kernel!(normal_pipeline_minimum_four, "cleanup-normal-min4", [2:15], [1, 2]);
kernel!(normal_pipeline_without_size_reduction, "cleanup-normal-noreduce", [2:15], [1, 2]);
kernel!(reorderable_raw_pipeline, "cleanup-raw-reorder", [2:15], [1]);

kernel!(normal_pipeline_minimum_four_without_compression, "cleanup-normal-min4-noreduce", [2:15], [1, 2]);

kernel!(
    coalesce_raw_subtract,
    "induction-collapse-subtract-chain",
    [],
    [1]
);
kernel!(
    coalesce_raw_sign_change,
    "induction-collapse-sign-flip",
    [],
    [3]
);
kernel!(coalesce_raw_memory, "induction-collapse-memory", [2:15], []);
kernel!(
    coalesce_branch_observer,
    "induction-collapse-branch-blocked",
    [],
    [1]
);
kernel!(coalesce_ordinary_address, "coalesce-ordinary-memory", [2:15], []);
kernel!(coalesce_escaping_address_stays_observable, "coalesce-output-observed", [2:15], [3]);
kernel!(coalesce_all_memory_users, "coalesce-multiple-users", [2:15,3:15], []);
kernel!(coalesce_across_stream, "coalesce-cross-stream", [2:15], []);
kernel!(coalesce_join_retains_original_constants, "coalesce-join-blocked", [2:15], []);
kernel!(external_integer_cross_stream, "external-vi-cross", [4:15], []);
kernel!(external_local_stays_locally_allocated, "external-vi-local", [4:15], []);
kernel!(external_multiple_values, "external-vi-two", [], [4]);
kernel!(
    external_discovery_order_reversal,
    "external-vi-two-reversed",
    [],
    [4]
);
kernel!(external_through_value, "external-vi-through", [], [4]);
kernel!(external_sequential_reuse, "external-vi-sequential", [], [4]);
kernel!(external_branch_join, "external-vi-join", [4:15], []);
kernel!(external_restricted_integer_pool, "external-vi-enabled-pool", [4:15], []);
kernel!(external_vector_output_lanes, "external-vf-lanes", [4:3], []);
kernel!(external_fixed_raw_uses_existing_path, "external-raw-fixed", [4:15], []);
kernel!(external_reorder_raw_uses_existing_path, "external-raw-reorder", [4:15], []);
kernel!(independent_delay_paths, "flow-delay-independent", [2:15], [3]);
kernel!(upper_occupancy_delay_paths, "flow-delay-upper", [2:15], [3]);
kernel!(loaded_vi_boundary_paths, "flow-delay-tail-debt", [2:15], [3]);
kernel!(unique_stream_merge, "flow-merge-plain", [2:15], [3]);
kernel!(exported_stream_boundary, "flow-merge-export", [2:15], [3]);
kernel!(barrier_stream_boundary, "flow-merge-fence", [2:15], [3]);
kernel!(fixed_stream_boundary, "flow-merge-fixed", [2:15], [3]);
kernel!(unique_stream_chain, "flow-merge-chain", [2:15], [3]);
kernel!(raw_reorder_minimum_two, "raw-boundary-min2", [2:15], [1]);
kernel!(raw_reorder_minimum_four, "raw-boundary-min4", [2:15], [1]);
kernel!(raw_reorder_without_final_compression, "raw-boundary-no-compress", [2:15], [1]);
kernel!(raw_reorder_minimum_four_without_compression, "raw-boundary-min4-no-compress", [2:15], [1]);
kernel!(raw_reorder_without_normal_search, "raw-boundary-search-disabled", [2:15], [1]);
kernel!(raw_noreorder_fixed_layout, "raw-boundary-fixed", [2:15], [1]);
kernel!(raw_reorder_old_pair_value, "raw-boundary-old-read", [2:15], [1,2]);
kernel!(loaded_vi_y_path, "boundary-ilw-y", [2:15], [3]);
kernel!(loaded_vi_z_path, "boundary-ilw-z", [2:15], [3]);
kernel!(loaded_vi_w_path, "boundary-ilw-w", [2:15], [3]);
kernel!(loaded_vi_selected_user, "boundary-ilw-fixed-user", [2:15], [3]);
kernel!(fixed_vi_multihop_path, "boundary-fixed-vi-branch", [], [1]);
kernel!(copy_integer_add_zero, "copy-iadd-zero", [], [3]);
kernel!(copy_integer_subtract_zero, "copy-isub-zero", [], [3]);
kernel!(copy_integer_zero_immediate, "copy-isubiu-zero", [], [3]);
kernel!(
    copy_debug_modifier_original_normalization,
    "copy-iaddiu-debug",
    [],
    [3]
);
kernel!(copy_vector_xyz_zero, "copy-add-xyz-zero", [2:7], []);
kernel!(copy_vector_xyz_subtract_zero, "copy-sub-xyz-zero", [2:7], []);
kernel!(copy_vector_zero_w_is_one, "copy-add-full-zero", [2:15], []);
kernel!(copy_vector_broadcast_zero, "copy-addx-zero", [2:15], []);
kernel!(copy_vector_broadcast_one, "copy-mulw-one", [2:15], []);
kernel!(copy_vector_multiply_zero_is_not_identity, "copy-mulx-zero", [2:15], []);
kernel!(copy_vector_move, "copy-explicit-move", [2:15], []);
kernel!(copy_fixed_raw_zero, "copy-fixed-isubiu-zero", [], [3]);
kernel!(copy_reorder_raw_zero, "copy-reorder-isubiu-zero", [], [3]);
kernel!(copy_reorder_upper_zero, "copy-reorder-add-xyz", [2:7], []);
kernel!(copy_reorder_lower_move, "copy-reorder-move-lower", [2:15], []);
