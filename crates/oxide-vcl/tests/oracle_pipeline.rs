//! End-to-end regression checks against saved output from the supplied ELF.
//!
//! The normalizer removes presentation differences (case, whitespace, integer
//! notation). It retains issue-pair order, upper/lower placement, all modifiers
//! including E, register assignment, masks, and branch operands. A passing case
//! establishes only that fixture's schedule; this is not an equivalence proof.

#[path = "support/evidence.rs"]
mod evidence;

use std::path::Path;
use vcl_cli as cli;

fn oracle(case: &str, filename: &str, flags: &[&str]) {
    let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("local/evidence/oracle")
        .join(case);
    let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
    let expected = std::fs::read_to_string(directory.join("stdout.bin")).unwrap();
    let options = cli::parse(flags.iter().copied()).unwrap().compile;
    let compiled = vcl::compile(&source, filename, &options)
        .unwrap_or_else(|errors| panic!("{case}: unexpectedly rejected: {errors:?}"));
    let expected_pairs = pairs(&expected);
    assert!(
        !expected_pairs.is_empty(),
        "{case}: expected fixture has no assembly pairs"
    );
    assert_eq!(
        pairs(&compiled.assembly),
        expected_pairs,
        "{case}: instruction pairs differ"
    );
}

fn pairs(assembly: &str) -> Vec<(String, String)> {
    assembly
        .lines()
        .filter_map(|line| {
            let leading = line.len() - line.trim_start().len();
            let lower = leading + 59;
            if leading < 9 || line.len() <= lower || line.trim_start().starts_with(';') {
                return None;
            }
            let upper = line[..lower].trim();
            let lower = line[lower..].split(';').next().unwrap().trim();
            if upper.is_empty() || lower.is_empty() || upper.starts_with('.') {
                return None;
            }
            Some((normalize(upper), normalize(lower)))
        })
        .collect()
}

fn normalize(instruction: &str) -> String {
    let mut fields = instruction.splitn(2, char::is_whitespace);
    let opcode = fields.next().unwrap().to_ascii_lowercase();
    let operands: String = fields
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let mut normalized = String::new();
    let mut token = String::new();
    let flush = |token: &mut String, output: &mut String| {
        if token.is_empty() {
            return;
        }
        let number = if let Some(hex) = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16).ok()
        } else if token.bytes().all(|byte| byte.is_ascii_digit()) {
            token.parse::<u64>().ok()
        } else {
            None
        };
        if let Some(number) = number {
            output.push_str(&number.to_string());
        } else {
            output.push_str(token);
        }
        token.clear();
    };
    for character in operands.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            token.push(character);
        } else {
            flush(&mut token, &mut normalized);
            normalized.push(character);
        }
    }
    flush(&mut token, &mut normalized);
    if normalized.is_empty() {
        opcode
    } else {
        format!("{opcode} {normalized}")
    }
}

macro_rules! fixture {
    ($test:ident, $case:literal, $($flag:literal),* $(,)?) => {
        #[test]
        #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
        fn $test() {
            oracle($case, concat!("/work/", $case, ".vcl"), &[$($flag),*]);
        }
    };
}

fixture!(
    external_pointer_uses_prescheduled_assignment,
    "coalesce-cross-stream",
    "-t1",
    "-uCOPY"
);
fixture!(
    external_join_keeps_the_earlier_flag_reservation,
    "coalesce-join-blocked",
    "-t1",
    "-uCOPY"
);

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn external_allocation_controls_match_original_pairs() {
    for case in [
        "external-vi-cross",
        "external-vi-local",
        "external-vi-join",
        "external-vi-enabled-pool",
        "external-vf-lanes",
        "external-vf-two",
        "external-vf-enabled-pool",
        "external-raw-fixed",
        "external-raw-reorder",
    ] {
        oracle(case, &format!("/work/{case}.vcl"), &["-t1", "-uEXT"]);
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn external_operand_choices_match_even_where_termination_layout_differs() {
    let instructions = |text: &str| {
        pairs(text)
            .into_iter()
            .flat_map(|(a, b)| [a, b])
            .filter(|op| !op.starts_with("nop"))
            .map(|op| {
                if let Some(start) = op.find('[') {
                    let end = op.find(']').unwrap();
                    format!("{}{}", &op[..start], &op[end + 1..])
                } else {
                    op
                }
            })
            .collect::<Vec<_>>()
    };
    let options = cli::parse(["-t1", "-uEXT"]).unwrap().compile;
    for case in [
        "external-vi-two",
        "external-vi-two-reversed",
        "external-vi-through",
        "external-vi-sequential",
    ] {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(case);
        let source = std::fs::read_to_string(root.join("input.vcl")).unwrap();
        let original = std::fs::read_to_string(root.join("stdout.bin")).unwrap();
        let native = vcl::compile(&source, &format!("/work/{case}.vcl"), &options).unwrap();
        assert_eq!(
            instructions(&native.assembly),
            instructions(&original),
            "{case}: allocated instructions differ"
        );
        // Pair/E-bit equality is deliberately a separate gate: these controls
        // expose the still-unported merge/termination ordering.
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn exhausted_external_pool_rejects_before_local_schedule_retries() {
    let source = &evidence::text("inputs/external-vi-exhaustion.vcl");
    let errors = vcl::compile(source, "external.vcl", &vcl::Options::default()).unwrap_err();
    assert!(errors.iter().any(|error| {
        error
            .message
            .to_string()
            .contains("external register assignment failed")
    }));
    assert!(!errors.iter().any(vcl::CompileError::vcl_exits_zero));
}

fixture!(
    independent_lower_branch_delay,
    "flow-delay-independent",
    "-t1",
    "-uFLOW"
);
fixture!(
    merge_ordinary_fallthrough,
    "flow-merge-plain",
    "-t1",
    "-uFLOW"
);
fixture!(
    keep_exported_stream_boundary,
    "flow-merge-export",
    "-t1",
    "-uFLOW"
);
fixture!(keep_barrier_boundary, "flow-merge-fence", "-t1", "-uFLOW");
fixture!(
    explicit_continuation_emits_end_bit,
    "flow-merge-continue",
    "-t1",
    "-uFLOW"
);
fixture!(
    label_debug_flag_survives,
    "flow-merge-debug",
    "-t1",
    "-uFLOW"
);
fixture!(label_mark_flag_survives, "flow-merge-mark", "-t1", "-uFLOW");
fixture!(label_trap_flag_survives, "flow-merge-trap", "-t1", "-uFLOW");
fixture!(
    combined_label_flags_use_original_order,
    "flow-merge-all-flags",
    "-t1",
    "-uFLOW"
);
fixture!(
    merge_repeated_unique_fallthroughs,
    "flow-merge-chain",
    "-t1",
    "-uFLOW"
);
fixture!(
    dependent_branch_keeps_its_readiness,
    "induction-collapse-branch-blocked",
    "-t1",
    "-uORACLE"
);

fixture!(
    raw_reorder_debris_reduction,
    "cleanup-raw-reorder",
    "-t1",
    "-uCLEAN"
);
fixture!(
    loaded_vi_can_occupy_an_independent_delay,
    "flow-delay-tail-debt",
    "-t1",
    "-uFLOW"
);
fixture!(
    fixed_source_provides_readiness,
    "flow-merge-fixed",
    "-t1",
    "-uFLOW"
);
fixture!(
    raw_delay_join_retains_external_readiness,
    "raw-delay-conditional-join",
    "-t1",
    "-uORACLE"
);
fixture!(
    raw_motion_precedes_final_compression_switch,
    "raw-boundary-no-compress",
    "-t1",
    "-uBOUND",
    "-C"
);
fixture!(
    raw_motion_ignores_normal_search_switch,
    "raw-boundary-search-disabled",
    "-t1",
    "-uBOUND",
    "-L"
);
fixture!(
    raw_noreorder_keeps_fixed_debris,
    "raw-boundary-fixed",
    "-t1",
    "-uBOUND"
);
fixture!(
    raw_motion_preserves_paired_old_reads,
    "raw-boundary-old-read",
    "-t1",
    "-uBOUND"
);
fixture!(loaded_vi_y_boundary, "boundary-ilw-y", "-t1", "-uBOUND");
fixture!(loaded_vi_z_boundary, "boundary-ilw-z", "-t1", "-uBOUND");
fixture!(loaded_vi_w_boundary, "boundary-ilw-w", "-t1", "-uBOUND");
fixture!(
    selected_raw_user_retains_outgoing_debt,
    "boundary-ilw-fixed-user",
    "-t1",
    "-uBOUND"
);
fixture!(
    multihop_vi_boundary_uses_cfg_distance,
    "boundary-fixed-vi-branch",
    "-t1",
    "-uBOUND"
);

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn observed_mac_output_keeps_its_raw_drain_layout() {
    let source = &evidence::text("inputs/raw-boundary-flags-observed.vcl");
    let options = cli::parse(["-t1", "-uBOUND"]).unwrap().compile;
    let output = vcl::compile(source, "flags.vcl", &options).unwrap();
    let drain = output
        .assembly
        .split("EXPL_BOUND_loop__EPI1:\n")
        .nth(1)
        .unwrap()
        .split("EXPL_BOUND_loop__EXIT_POINT:\n")
        .next()
        .unwrap();
    let retained = pairs(drain);
    assert_eq!(
        retained.len(),
        10,
        "live MAC output must prevent unobserved-flag motion"
    );
    assert!(retained[1].0.starts_with("mul "));
    assert!(retained[7].0.starts_with("add "));
    assert!(output.assembly.contains("fmand"));
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn legacy_e_switch_masks_empty_upper_slots_but_keeps_occupied_upper_flags() {
    oracle("cli-no-endbit", "/work/sched-add-symbol.vcl", &["-e"]);
    let source = &evidence::text("inputs/flow-merge-debug.vcl");
    let options = cli::parse(["-t1", "-uFLOW", "-e"]).unwrap().compile;
    let output = vcl::compile(source, "flow.vcl", &options).unwrap();
    assert!(output.assembly.contains("NOP[D]"));
    assert!(!output.assembly.contains("NOP[DE]"));
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn continuation_cannot_overwrite_an_exit_end_kind() {
    let source = &evidence::text("inputs/flow-cont-invalid-end.vcl");
    let errors = vcl::compile(source, "flow.vcl", &vcl::Options::default()).unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        error.message,
        vcl::Message::Parse(vcl::diagnostics::ParseMessage::SyntheticContext)
    )));
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn merge_keeps_export_offsets_and_never_hides_a_join_target() {
    let options = cli::parse(["-t1", "-uFLOW"]).unwrap().compile;
    for case in ["flow-merge-export", "flow-delay-independent"] {
        let source = std::fs::read_to_string(format!(
            "{}/local/evidence/inputs/{case}.vcl",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
        ))
        .unwrap();
        let output = vcl::compile(&source, "flow.vcl", &options).unwrap();
        if case == "flow-merge-export" {
            assert!(output.assembly.contains("next:\n"));
            assert!(
                output
                    .header
                    .lines()
                    .any(|line| line.starts_with("next ") && line.ends_with("= 1"))
            );
        } else {
            assert!(output.assembly.contains("taken:\n"));
            assert!(
                output
                    .header
                    .lines()
                    .any(|line| line.starts_with("taken ") && line.ends_with("= 3"))
            );
        }
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn merge_drops_only_labels_inside_unprotected_unique_chains() {
    let options = cli::parse(["-t1", "-uFLOW"]).unwrap().compile;
    let source = &evidence::text("inputs/flow-merge-chain.vcl");
    let output = vcl::compile(source, "flow.vcl", &options).unwrap();
    assert!(!output.assembly.contains("first:\n"));
    assert!(!output.assembly.contains("second:\n"));
    // Protected boundaries keep two streams, but only an emitted branch or an
    // export makes the second stream's label visible.
    let labels = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| !line.starts_with(';') && line.trim_end().ends_with(':'))
            .map(|line| line.trim().to_string())
            .collect()
    };
    for case in [
        "flow-merge-fence",
        "flow-merge-debug",
        "flow-merge-continue",
        "flow-merge-fixed",
        "flow-merge-export",
    ] {
        let directory = format!(
            "{}/local/evidence/oracle/{case}",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
        );
        let source = std::fs::read_to_string(format!("{directory}/input.vcl")).unwrap();
        let expected = std::fs::read_to_string(format!("{directory}/stdout.bin")).unwrap();
        let output = vcl::compile(&source, "flow.vcl", &options).unwrap();
        assert_eq!(labels(&output.assembly), labels(&expected), "{case}");
    }
}

fixture!(scalar_base_ilw_x, "address-ilw-x", "-t1", "-uADDRESS");
fixture!(scalar_base_ilw_y, "address-ilw-y", "-t1", "-uADDRESS");
fixture!(scalar_base_ilw_z, "address-ilw-z", "-t1", "-uADDRESS");
fixture!(scalar_base_ilw_w, "address-ilw-w", "-t1", "-uADDRESS");
fixture!(scalar_base_isw_x, "address-isw-x", "-t1", "-uADDRESS");
fixture!(scalar_base_isw_y, "address-isw-y", "-t1", "-uADDRESS");
fixture!(scalar_base_isw_z, "address-isw-z", "-t1", "-uADDRESS");
fixture!(scalar_base_isw_w, "address-isw-w", "-t1", "-uADDRESS");

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn scalar_memory_base_must_still_be_defined() {
    let source = &evidence::text("inputs/address-ilw-missing-base.vcl");
    let errors = vcl::compile(source, "address.vcl", &vcl::Options::default()).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.message.to_string() == "'base.x' is read before anything writes it")
    );
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn recovered_parser_forms_match_original_pairs() {
    let mut names = Vec::new();
    for lane in ["x", "y", "z", "w"] {
        for style in ["suffix", "dot"] {
            names.push(format!("parser-mtir-{style}-{lane}"));
        }
    }
    names.extend(
        [
            "parser-old-vector-suffix",
            "parser-old-name-ending-field",
            "parser-new-name-ending-field",
            "parser-new-indexed-destination",
            "parser-new-indexed-vector-third",
            "parser-new-indexed-mtir",
            "parser-efu-eatanxy-fields",
            "parser-efu-eatanxz-fields",
            "parser-efu-eleng-fields",
            "parser-efu-erleng-fields",
            "parser-efu-ersadd-fields",
            "parser-efu-esadd-fields",
        ]
        .map(str::to_owned),
    );
    for name in names {
        oracle(&name, &format!("/work/{name}.vcl"), &["-t1", "-uPARSER"]);
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn malformed_parser_forms_and_missing_efu_fields_still_fail() {
    for case in [
        "parser-old-duplicate-fields",
        "parser-new-suffix-is-not-scalar",
        "parser-new-multilane-third",
        "parser-new-scalar-destination",
        "parser-old-scalar-missing-field",
        "parser-efu-esadd-missing-z",
        "parser-efu-esum-missing-w",
    ] {
        let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(case);
        let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        assert!(
            vcl::compile(&source, case, &vcl::Options::default()).is_err(),
            "{case}"
        );
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn allocation_boundaries_match_original_acceptance() {
    for (case, accepted) in [
        ("alloc-interval-components", true),
        ("alloc-interval-overlap-reject", false),
        ("alloc-interval-read-write-boundary", true),
        ("alloc-interval-paired-writes-reject", false),
        ("alloc-interval-branch-reuse", true),
        ("alloc-interval-through-reject", false),
        ("alloc-enter-unforced-move", false),
        ("alloc-enter-unforced-add", false),
        ("alloc-enter-inherit-binding", true),
        ("timing-collision-same-web", true),
        ("timing-collision-packed-webs", true),
        ("timing-collision-internal-webs", true),
    ] {
        let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(case);
        let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        let result = vcl::compile(&source, case, &vcl::Options::default());
        assert_eq!(result.is_ok(), accepted, "{case}: {result:?}");
    }
}

fixture!(empty_exit_has_end_bit_and_delay_pair, "sched-empty", "-t1");
fixture!(symbolic_vf_mapping, "sched-add-symbol", "-t1");
fixture!(lower_integer_issue, "scheduled-iaddiu", "-t1", "-uORACLE");
fixture!(partial_vf_output, "mask-output", "-t1", "-uORACLE");
fixture!(
    temporary_register_reuse_and_original_end_bit_loss,
    "alloc-vf-all",
    "-t1",
    "-uORACLE"
);
fixture!(
    unreduced_latency_nops_and_end_bit,
    "exit-chain-no-reduce",
    "-t1",
    "-uORACLE",
    "-C"
);
fixture!(declared_q_read, "hw-mulq-declared-q", "-t1", "-uORACLE");
fixture!(
    declared_accumulator_read,
    "hw-madd-declared-acc",
    "-t1",
    "-uORACLE"
);
fixture!(
    accumulator_write_then_read,
    "hw-mula-madd",
    "-t1",
    "-uORACLE"
);
fixture!(division_scalar_operands, "emit-div", "-t1", "-uORACLE");
fixture!(clip_special_emitter, "emit-cliplw", "-t1", "-uORACLE");
fixture!(
    raw_simultaneous_independent_issue,
    "raw-independent-pair",
    "-t1",
    "-uORACLE"
);
fixture!(
    raw_reads_old_vf_value_before_write,
    "raw-read-before-write",
    "-t1",
    "-uORACLE"
);
fixture!(
    raw_load_writes_after_upper_reads,
    "raw-load-before-write",
    "-t1",
    "-uORACLE"
);
fixture!(
    integer_branch_latency_and_delay_pair,
    "loop-integer-backedge",
    "-t1",
    "-uORACLE"
);
fixture!(
    case_insensitive_virtual_registers,
    "mapping-input-different-case",
    "-t1",
    "-uORACLE"
);
fixture!(
    separate_component_definitions,
    "mapping-component-rewrites",
    "-t1",
    "-uORACLE"
);
fixture!(
    postincrement_load,
    "memory-load-increment",
    "-t1",
    "-uORACLE"
);
fixture!(
    predecrement_load,
    "memory-load-decrement",
    "-t1",
    "-uORACLE"
);
fixture!(
    postincrement_store,
    "memory-store-increment",
    "-t1",
    "-uORACLE"
);
fixture!(
    predecrement_store,
    "memory-store-decrement",
    "-t1",
    "-uORACLE"
);
fixture!(declared_p_read, "qp-input-p-mfp", "-t1", "-uORACLE");
fixture!(q_synchronization_cap, "qp-div-mulq", "-t1", "-uORACLE");
fixture!(p_synchronization_cap, "qp-efu-mfp", "-t1", "-uORACLE");
fixture!(
    mac_vector_interlock_cap,
    "hw-fmand-after-alu",
    "-t1",
    "-uORACLE"
);
fixture!(
    status_vector_interlock_cap,
    "hw-fsand-after-alu",
    "-t1",
    "-uORACLE"
);
fixture!(
    unconditional_branch_exit_fold,
    "branch-forward-unconditional",
    "-t1",
    "-uORACLE"
);
fixture!(
    conditional_exit_edges,
    "branch-forward-conditional",
    "-t1",
    "-uORACLE"
);
fixture!(
    case_insensitive_branch_target,
    "branch-target-case",
    "-t1",
    "-uORACLE"
);
fixture!(
    random_register_write_then_read,
    "random-rinit-rget",
    "-t1",
    "-uORACLE"
);
fixture!(
    random_register_next,
    "random-rnext-declared",
    "-t1",
    "-uORACLE"
);

#[test]
fn normalization_preserves_end_bits_slots_masks_and_branch_targets() {
    assert_eq!(
        normalize("IADDIU VI02, VI01, 0x00000001"),
        "iaddiu VI02,VI01,1"
    );
    assert_ne!(normalize("nop[E]"), normalize("nop"));
    assert_ne!(
        normalize("add.xy VF02,VF01,VF01"),
        normalize("add.zw VF02,VF01,VF01")
    );
    assert_ne!(normalize("b loop"), normalize("b other"));
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn real_use_before_set_is_rejected() {
    for case in [
        "branch-forward-unset-path",
        "hw-mulq-undeclared",
        "hw-madd-undeclared",
        "random-rget-uninitialized",
        "raw-read-before-write-unset",
        "raw-write-conflict",
        "prune-dead-undefined",
        "prune-dead-partial",
        "prune-dead-hardware-q",
    ] {
        let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(case);
        let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        assert!(
            vcl::compile(&source, "test.vcl", &vcl::Options::default()).is_err(),
            "{case}: unsafe use-before-set was accepted"
        );
    }
}

fixture!(
    explicit_pipeline_minimum_one,
    "pipeline-raw-stages-min1",
    "-t1",
    "-uPIPE"
);
fixture!(
    explicit_pipeline_minimum_four,
    "pipeline-raw-stages-min4",
    "-t1",
    "-uPIPE"
);
fixture!(
    explicit_pipeline_survives_disable_search,
    "pipeline-raw-stages-disabled",
    "-t1",
    "-uPIPE",
    "-L"
);
fixture!(
    scheduled_counted_vector_loop,
    "loop-vector-counted",
    "-t1",
    "-uORACLE"
);

fixture!(
    unprefixed_raw_loop_without_search,
    "pipeline-raw-default-disabled",
    "-t1",
    "-uPIPE",
    "-L"
);

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn invalid_raw_stage_tables_preserve_original_rejections() {
    for case in ["pipeline-raw-single-stage", "pipeline-raw-default-stage"] {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(case);
        let metadata = std::fs::read_to_string(root.join("result.json")).unwrap();
        assert!(metadata.contains("\"exit_status\": 255"));
        let source = std::fs::read_to_string(root.join("input.vcl")).unwrap();
        assert!(
            vcl::compile(
                &source,
                case,
                &cli::parse(["-t1", "-uPIPE"]).unwrap().compile
            )
            .is_err(),
            "{case}"
        );
    }
}

fixture!(
    explicit_pipeline_memory_add,
    "immediate-pipeline-before-add",
    "-t1",
    "-uIMM"
);
fixture!(
    explicit_pipeline_memory_subtract,
    "immediate-pipeline-before-subtract",
    "-t1",
    "-uIMM"
);

fixture!(
    raw_stage_backward_textual_definition,
    "pipeline-raw-stage-backref",
    "-t1",
    "-uPIPE"
);

fixture!(
    normal_pipeline_affine_after_add,
    "immediate-scheduled-after-add",
    "-t1",
    "-uIMM"
);
fixture!(
    normal_pipeline_affine_before_add,
    "immediate-scheduled-add",
    "-t1",
    "-uIMM"
);
fixture!(
    normal_pipeline_affine_after_subtract,
    "immediate-scheduled-after-subtract",
    "-t1",
    "-uIMM"
);
fixture!(
    normal_pipeline_minimum_four_cleanup,
    "cleanup-normal-min4",
    "-t1",
    "-uCLEAN"
);
fixture!(
    normal_pipeline_extent_cleanup_survives_c,
    "cleanup-normal-noreduce",
    "-t1",
    "-uCLEAN",
    "-C"
);

fixture!(
    normal_pipeline_minimum_four_without_compression,
    "cleanup-normal-min4-noreduce",
    "-t1",
    "-uCLEAN",
    "-C"
);
fixture!(
    normal_pipeline_standard_alarm_profile,
    "cleanup-normal-scale1",
    "-t1",
    "-uIMM"
);

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn general_coalescing_and_copy_controls_match_original_pairs() {
    for case in [
        "induction-collapse-subtract-chain",
        "induction-collapse-sign-flip",
        "induction-collapse-memory",
    ] {
        oracle(case, &format!("/work/{case}.vcl"), &["-t1", "-uORACLE"]);
    }
    for case in [
        "copy-iadd-zero",
        "copy-isub-zero",
        "copy-iaddiu-zero",
        "copy-isubiu-zero",
        "copy-iaddiu-symbol",
        "copy-iaddiu-debug",
        "copy-add-xyz-zero",
        "copy-sub-xyz-zero",
        "copy-add-full-zero",
        "copy-addx-zero",
        "copy-mulw-one",
        "copy-mulx-zero",
        "copy-explicit-move",
        "copy-flags-live",
        "copy-fixed-isubiu-zero",
        "copy-reorder-isubiu-zero",
        "copy-reorder-add-xyz",
        "copy-fixed-constant-dest",
        "copy-reorder-move-lower",
        "coalesce-ordinary-memory",
        "coalesce-output-observed",
        "coalesce-multiple-users",
    ] {
        oracle(case, &format!("/work/{case}.vcl"), &["-t1", "-uCOPY"]);
    }
}

#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn a_copy_into_the_constant_register_fails_where_vcl_exits_zero() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("local/evidence/oracle/copy-reorder-constant-dest");
    let source = std::fs::read_to_string(root.join("input.vcl")).unwrap();
    let errors = vcl::compile(&source, "copy.vcl", &vcl::Options::default()).unwrap_err();
    assert!(errors[0].vcl_exits_zero());
    assert!(
        errors[0]
            .message
            .to_string()
            .contains("the constant register")
    );
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_oxide-vcl"))
        .args(["-t1", "-uCOPY"])
        .arg(root.join("input.vcl"))
        .output()
        .unwrap();
    // vcl exits 0 here (`result.json`); the port fails the run.
    assert_eq!(
        run.status.code(),
        Some(i32::from(vcl::diagnostics::VCL_EXIT_FAILURE))
    );
    assert_eq!(run.stdout, std::fs::read(root.join("stdout.bin")).unwrap());
    assert!(
        String::from_utf8_lossy(&run.stderr)
            .contains("error: this copy writes to the constant register")
    );
}

/// Probes from the local corpus work (docs/LOCAL_CORPUS.md): branches
/// on `--enter` inputs, identity copies, and a deleted jump's feeding code.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn branch_input_identity_copy_and_redundant_branch_probes_match() {
    for (case, file) in [
        ("branch-input-a-nouse", "a_nouse"),
        ("branch-input-b-local", "b_local"),
        ("branch-input-br-in", "br_in"),
        ("branch-input-c-onlyone", "c_onlyone"),
        ("branch-input-d-far", "d_far"),
        ("branch-input-e-two", "e_two"),
        ("branch-input-join", "join"),
        ("identity-copy-c-coal", "c_coal"),
        ("identity-copy-c-iadd", "c_iadd"),
        ("identity-copy-c-iaddiu", "c_iaddiu"),
        ("identity-copy-c-vadd", "c_vadd"),
        ("identity-copy-line", "line"),
        ("redundant-branch-feeder", "b"),
    ] {
        oracle(case, &format!("/work/{file}.vcl"), &["-t1", "-uP"]);
    }
}

/// XGKICK ordering (descriptor flags 0x60: after every
/// earlier store and the previous kick, before later stores) and its end
/// distance in a `--cont` stream, which decides Vuta1 over
/// Normal2; a block running into `--exit` gets no end distance.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn xgkick_order_and_end_distance_probes_match() {
    for (case, file) in [
        ("xgkick-memory-order", "kc"),
        ("xgkick-cont-end", "kb"),
        ("xgkick-exit-end", "ka"),
    ] {
        oracle(case, &format!("/work/{file}.vcl"), &["-t1", "-uP"]);
    }
}

/// Calls (`bal`) reach only their subroutine; an untagged `jr` returns to the
/// stream after each `bal` that set its register, and each
/// return keeps its own caller's values. A branch folded into
/// an exit it does not fall into stays as a lower `nop` carrying the E bit.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn calls_with_inferred_returns_match() {
    for (case, file, unit) in [
        (
            "trace-call-two-continuations",
            "trace-call-two-continuations",
            "CALL",
        ),
        (
            "trace-call-alternate-continuations",
            "trace-call-alternate-continuations",
            "CALL",
        ),
        (
            "audit-indirect-inferred",
            "audit-indirect-inferred",
            "INDIRECT",
        ),
        (
            "alloc-interval-branch-reuse",
            "alloc-interval-branch-reuse",
            "ORACLE",
        ),
    ] {
        oracle(
            case,
            &format!("/work/{file}.vcl"),
            &["-t1", &format!("-u{unit}")],
        );
    }
    // An unset value on one caller's path, a JALR through a BAL-set register
    // and a JR through an ordinary input still fail.
    for case in [
        "trace-call-alternate-unset",
        "audit-indirect-jalr-inferred",
        "audit-indirect-untagged",
    ] {
        let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(case);
        let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        assert!(
            vcl::compile(&source, "test.vcl", &vcl::Options::default()).is_err(),
            "{case}: accepted"
        );
    }
}

/// Shared memory tags order a load against a store, not two stores
/// (observed from vcl 1.4beta7). Untagged accesses and accesses with
/// different tags through different base registers are free to swap.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn memory_tags_match_original_pairs() {
    for (case, file) in [
        ("memory-tag-none", "tag1"),
        ("memory-tag-shared", "tag2"),
        ("memory-tag-disjoint", "tag3"),
        ("memory-tag-store-store", "store-store"),
        ("memory-tag-fields", "fields"),
    ] {
        oracle(case, &format!("/work/{file}.vcl"), &["-t1", "-uDEMO"]);
    }
}

/// Row of the first lower instruction starting with `opcode`.
fn lower_row(assembly: &str, opcode: &str) -> usize {
    pairs(assembly)
        .iter()
        .position(|(_, lower)| lower.starts_with(opcode))
        .unwrap_or_else(|| {
            panic!(
                "no {opcode} in\n\
                                   {assembly}"
            )
        })
}

/// The same rules as `memory_tags_match_original_pairs`, without the
/// local evidence: which of two memory accesses the port issues first.
#[test]
fn shared_memory_tags_keep_a_load_after_its_store() {
    let compile = |first: &str, second: &str| {
        let source = format!(
            ".init_vf_all\n\
             --enter\n\
             in_vi src(vi01)\n\
             in_vi dst(vi02)\n\
             in_vf a(vf01)\n\
             --endenter\n\
             mul c, a, a\n\
             mul c, c, c\n\
             {first}\n\
             {second}\n\
             add.y d, v, v\n\
             --exit\n\
             out_vf.y d(vf03)\n\
             --endexit\n"
        );
        vcl::compile(&source, "tags.vcl", &vcl::Options::default())
            .unwrap_or_else(|errors| panic!("{errors:?}"))
            .assembly
    };
    // A store waiting on a long chain, then an independent load.
    for (store, load, ordered) in [
        ("sq c, 0(dst)", "lq v, 0(src)", false),
        ("sq c, 0(dst):out", "lq v, 0(src):in", false),
        ("sq c, 0(dst):frame", "lq v, 0(src):frame", true),
        ("sq c, 0(dst):left:frame", "lq v, 0(src):frame:right", true),
        // Disjoint fields never alias, even with a shared tag.
        ("sq.x c, 0(dst):frame", "lq.y v, 0(src):frame", false),
    ] {
        let assembly = compile(store, load);
        assert_eq!(
            lower_row(&assembly, "sq") < lower_row(&assembly, "lq"),
            ordered,
            "{store} / {load}:\n\
             {assembly}"
        );
    }
    // Two stores sharing a tag may still swap: the ready store of `a`
    // through `src` (VI01) issues before the store of the long chain.
    let assembly = compile(
        "sq c, 0(dst):frame",
        "sq a, 0(src):frame\n\
                                                  lq v, 1(src)",
    );
    let first_store = pairs(&assembly)
        .into_iter()
        .find(|(_, lower)| lower.starts_with("sq"))
        .unwrap();
    assert!(
        first_store.1.to_ascii_lowercase().contains("(vi01)"),
        "stores kept in order:\n\
         {assembly}"
    );
}

/// An invalid loop hint argument is reported and compilation continues with
/// the arguments before it (observed from vcl 1.4beta7); only a negative
/// `--LoopExtra` still ends in failure.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn invalid_loop_hints_match_the_original() {
    for (case, file) in [
        ("loop-hint-count-zero", "loopcs0"),
        ("loop-hint-safe-negative", "loopcs-safe-neg"),
        ("loop-hint-abs-two", "loopabs2"),
    ] {
        oracle(case, &format!("/work/{file}.vcl"), &["-t1", "-uDEMO"]);
    }
    let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("local/evidence/oracle/loop-hint-extra-negative");
    let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
    assert!(vcl::compile(&source, "/work/loopextra-neg.vcl", &vcl::Options::default()).is_err());
}

/// A loop with one bad hint compiles, with the original's message as a
/// warning; a valid hint after it still applies.
#[test]
fn invalid_loop_hints_warn_and_compile() {
    let compile = |hints: &str| {
        let source = format!(
            ".init_vf_all\n\
             .init_vi_all\n\
             --enter\n\
             in_vi src(vi01)\n\
             in_vi count(vi02)\n\
             in_vf scale(vf02)\n\
             --endenter\n\
             loop:\n\
             {hints}\n\
             lq v, 0(src)\n\
             mul v, v, scale\n\
             sq v, 0(src)\n\
             iaddiu src, src, 1\n\
             isubiu count, count, 1\n\
             ibne count, vi00, loop\n\
             --exit\n\
             --endexit\n"
        );
        vcl::compile(&source, "hints.vcl", &vcl::Options::default())
    };
    let pipelined = |assembly: &str| assembly.contains("_MAIN_LOOP");
    let count_zero = compile("--LoopCS 0,0").unwrap();
    assert!(
        count_zero.warnings[0]
            .to_string()
            .contains("minimum known count (argument 1)")
    );
    assert!(!pipelined(&count_zero.assembly), "{}", count_zero.assembly);
    let safe_negative = compile("--LoopCS 4,-1").unwrap();
    assert!(
        safe_negative.warnings[0]
            .to_string()
            .contains("minimum safe count (argument 2)")
    );
    assert!(pipelined(&safe_negative.assembly));
    let abs_two = compile(
        "--LoopCS 4,0\n\
                           --LoopAbs 2",
    )
    .unwrap();
    assert!(
        abs_two.warnings[0]
            .to_string()
            .contains("--LoopAbs: the extra loop count must be at least 2")
    );
    assert_eq!(
        abs_two.assembly,
        compile("--LoopCS 4,0").unwrap().assembly,
        "an invalid --LoopAbs is ignored"
    );
    let extra = compile(
        "--LoopCS 4,0\n\
                         --LoopExtra -1",
    )
    .unwrap_err();
    assert!(
        extra[0]
            .message
            .to_string()
            .contains("--LoopExtra: the extra loop count must be 0 or more")
    );
}

/// Running out of registers names the register type (the directive that
/// gives more is in the message's help).
#[test]
fn allocation_failures_say_which_directive_helps() {
    let message = |source: &str| {
        vcl::compile(source, "alloc.vcl", &vcl::Options::default()).unwrap_err()[0]
            .message
            .to_string()
    };
    let empty = message(
        "--enter\n\
         in_vi a(vi01)\n\
         --endenter\n\
         iadd t, a, a\n\
         iadd b, t, a\n\
         --exit\n\
         out_vi b(vi02)\n\
         --endexit\n",
    );
    assert_eq!(empty, "no VI register is available to allocate");
    let small = message(
        ".init_vf vf10\n\
         --enter\n\
         in_vf a(vf01)\n\
         --endenter\n\
         add t, a, a\n\
         mul u, a, a\n\
         add b, t, u\n\
         --exit\n\
         out_vf b(vf02)\n\
         --endexit\n",
    );
    assert!(small.contains("out of VF registers for 'u'"), "{small}");
}

/// The retained runs of the original reject what the port rejects.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn allocation_pool_failures_match_the_original_status() {
    for case in [
        "alloc-pool-empty-vf",
        "alloc-pool-empty-vi",
        "alloc-pool-small-vf",
    ] {
        let directory = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("local/evidence/oracle")
            .join(case);
        let result = std::fs::read_to_string(directory.join("result.json")).unwrap();
        assert!(result.contains("\"exit_status\": 255"), "{case}");
        let source = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        assert!(
            vcl::compile(&source, "alloc.vcl", &vcl::Options::default()).is_err(),
            "{case}: accepted"
        );
    }
}

/// Memory tags constrain software-pipelined loops as resources: with a
/// shared tag the next iteration's load waits for this iteration's store, so
/// vcl 1.4beta7 settles on period 9 against a bound of 6
/// (`memory-tag-loop`); different tags or disjoint fields leave the loop as
/// if untagged.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn tagged_loops_match_original_pairs() {
    for case in [
        "memory-tag-loop",
        "memory-tag-loop-untagged",
        "memory-tag-loop-disjoint",
        "memory-tag-loop-store-first",
        "memory-tag-loop-fields",
        "memory-tag-loop-fields-untagged",
    ] {
        oracle(case, "/work/loop.vcl", &["-t1", "-uDEMO"]);
    }
}

/// The same rule without the local evidence: a shared tag between the load
/// and the store keeps iterations from overlapping, so the load is emitted
/// once (no prologue copy); different tags still pipeline.
#[test]
fn shared_tags_keep_loop_iterations_in_order() {
    let compile = |load: &str, store: &str| {
        let source = format!(
            ".init_vf_all\n\
             .init_vi_all\n\
             --enter\n\
             in_vi src(vi01)\n\
             in_vi dst(vi02)\n\
             in_vi count(vi03)\n\
             in_vf scale(vf02)\n\
             --endenter\n\
             loop:\n\
             --LoopCS 4, 0\n\
             lq v, 0(src){load}\n\
             mul v, v, scale\n\
             sq v, 0(dst){store}\n\
             iaddiu src, src, 1\n\
             iaddiu dst, dst, 1\n\
             isubiu count, count, 1\n\
             ibne count, vi00, loop\n\
             --exit\n\
             --endexit\n"
        );
        vcl::compile(&source, "loop.vcl", &vcl::Options::default())
            .unwrap_or_else(|errors| panic!("{errors:?}"))
            .assembly
    };
    let loads = |assembly: &str| {
        pairs(assembly)
            .iter()
            .filter(|(_, lower)| lower.starts_with("lq"))
            .count()
    };
    assert_eq!(loads(&compile("", "")), 2, "untagged loop pipelines");
    assert_eq!(loads(&compile(":in", ":out")), 2, "different tags pipeline");
    let shared = compile(":buf", ":buf");
    assert_eq!(loads(&shared), 1, "{shared}");
}

/// XGKICK reads and writes the external-unit resource EXT, so each kick
/// depends on the previous one (inferred from vcl 1.4beta7). The dependency
/// ranks the first kick ahead of independent work when a second kick follows
/// (`doom3-passthrough`); with one kick, the work moves up.
#[test]
#[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
fn xgkick_order_matches_original_pairs() {
    for case in [
        "doom3-passthrough",
        "xgkick-order-swapped",
        "xgkick-order-no-second-kick",
        "xgkick-order-old-syntax",
    ] {
        oracle(case, "/work/kick.vcl", &["-t1", "-uCORPUS"]);
    }
}

/// The same rule without the local evidence.
#[test]
fn a_following_xgkick_keeps_the_first_kick_ahead_of_independent_work() {
    let lower = |last: &str| {
        let source = format!(
            ".init_vi_all\n\
             --enter\n\
             --endenter\n\
             xtop packet\n\
             xgkick packet\n\
             iaddiu tag, vi00, 8\n\
             {last}\n\
             --exit\n\
             --endexit\n"
        );
        let assembly = vcl::compile(&source, "kick.vcl", &vcl::Options::default())
            .unwrap_or_else(|errors| panic!("{errors:?}"))
            .assembly;
        pairs(&assembly)
            .into_iter()
            .map(|(_, lower)| lower.split_whitespace().next().unwrap().to_string())
            .filter(|opcode| opcode != "nop")
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lower("xgkick tag"),
        ["xtop", "xgkick", "iaddiu", "xgkick"],
        "a second kick keeps program order"
    );
    assert_eq!(
        lower("isw.x tag, 0(vi00)"),
        ["xtop", "iaddiu", "xgkick", "isw.x"],
        "with one kick the independent iaddiu moves up"
    );
}
