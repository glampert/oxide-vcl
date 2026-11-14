//! Oxide mode's optimizations (docs/OPTIMIZATION_PLAN.md), each against
//! `--opt=none`: the listing it changes, and the same results in the timed
//! VU model. `listing_equivalence.rs` checks them over the whole corpus.

#[path = "support/vu_model.rs"]
mod vu_model;

use vcl_cli as cli;
use vu_model::{Config, Execution, Program, State};

fn compile(source: &str, flags: &[&str]) -> String {
    let flags = ["-t1", "-uOPT", "--mode=oxide"].iter().chain(flags);
    let options = cli::parse(flags.copied()).unwrap().compile;
    vcl::compile(source, "/work/opt.vcl", &options)
        .unwrap()
        .assembly
}

/// The listing's rows, with spacing collapsed: `"add[E] VF04,VF04,VF01 NOP"`.
fn rows(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(|line| line.split(';').next().unwrap())
        .filter(|code| code.starts_with("         ") && !code.trim().is_empty())
        .map(|code| code.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

fn run(listing: &str, state: State) -> Execution {
    Program::parse(listing)
        .unwrap()
        .run(state, Config::TIMED, 64)
        .unwrap()
}

fn source_state() -> State {
    let mut state = State::default();
    state.set_vector(1, [1.5, 2.0, -0.5, 4.0]);
    state.vi[1] = 40;
    state
}

// ----------------------------------------------
// exit
// ----------------------------------------------

const UPPER_LAST: &str = ".init_vf_all\n\
                          .init_vi_all\n\
                          --enter\n\
                          in_vf src(vf01)\n\
                          --endenter\n\
                          mul first,src,src\n\
                          add second,src,src\n\
                          add result,first,second\n\
                          --exit\n\
                          out_vf result(vf04)\n\
                          --endexit\n";

#[test]
fn exit_runs_the_last_upper_instruction_in_the_delay_slot() {
    // vcl ends on the last instruction's row and an empty delay row; the
    // last ADD may run in the delay slot, so the E bit goes on the row
    // before it and the program is a row shorter.
    let plain = compile(UPPER_LAST, &["--opt=none"]);
    let optimized = compile(UPPER_LAST, &[]);
    assert_eq!(
        rows(&plain)[1..],
        [
            "add VF01,VF01,VF01 NOP",
            "add[E] VF04,VF04,VF01 NOP",
            "NOP NOP"
        ]
    );
    assert_eq!(
        rows(&optimized)[1..],
        ["add[E] VF01,VF01,VF01 NOP", "add VF04,VF04,VF01 NOP"]
    );
    assert_eq!(compile(UPPER_LAST, &["--opt=exit"]), optimized);
    let (before, after) = (run(&plain, source_state()), run(&optimized, source_state()));
    assert_eq!(after.state.vf[4], before.state.vf[4]);
    let expected = [1.5_f32, 2.0, -0.5, 4.0].map(|lane| (lane * lane + (lane + lane)).to_bits());
    assert_eq!(after.state.vf[4], expected);
    assert_eq!(after.cycles + 1, before.cycles);
}

#[test]
fn exit_keeps_memory_access_out_of_the_delay_slot() {
    // A store may not run in the E delay slot: it already carries the E
    // bit, so nothing changes.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vf src(vf01)\n\
                  in_vi base(vi01)\n\
                  --endenter\n\
                  mul first,src,src\n\
                  add result,first,src\n\
                  sq result,0(base)\n\
                  --exit\n\
                  --endexit\n";
    let optimized = compile(source, &[]);
    assert_eq!(optimized, compile(source, &["--opt=none"]));
    let rows = rows(&optimized);
    assert_eq!(
        rows[rows.len() - 2..],
        ["NOP[E] sq VF01,0(VI01)", "NOP NOP"]
    );
}

const KICK_BARRIER: &str = ".init_vf_all\n\
                            .init_vi_all\n\
                            --enter\n\
                            in_vf src(vf01)\n\
                            in_vi base(vi01)\n\
                            --endenter\n\
                            sq src,0(base)\n\
                            xgkick base\n\
                            --barrier\n\
                            --cont\n\
                            iaddiu base,base,1\n\
                            --exit\n\
                            out_vi base(vi01)\n\
                            --endexit\n";

#[test]
fn exit_ends_on_the_xgkick_of_the_stream_before_an_empty_exit() {
    // The barrier puts the `--cont` exit in a table of its own, holding
    // only the E row and its delay row. The stream before falls into it,
    // so the XGKICK row takes the E bit and the exit table keeps only the
    // delay row.
    let plain = compile(KICK_BARRIER, &["--opt=none"]);
    let optimized = compile(KICK_BARRIER, &[]);
    assert_eq!(
        rows(&plain)[1..4],
        ["NOP xgkick VI01", "NOP[E] NOP", "NOP NOP"]
    );
    assert_eq!(rows(&optimized)[1..3], ["NOP[E] xgkick VI01", "NOP NOP"]);
    assert_eq!(rows(&optimized).len() + 1, rows(&plain).len());
    // The final exit is unchanged.
    assert_eq!(
        rows(&optimized)[3..],
        ["NOP[E] iaddiu VI01,VI01,0x00000001", "NOP NOP"]
    );
    let (before, after) = (run(&plain, source_state()), run(&optimized, source_state()));
    assert_eq!(after.state.kicks, before.state.kicks);
    assert_eq!(after.state.memory, before.state.memory);
    assert_eq!(after.state.kicks.len(), 1);
    assert_eq!(after.cycles + 1, before.cycles);
}

#[test]
fn exit_leaves_an_exit_that_a_branch_reaches() {
    // `done` is a branch target, so its E row stays; only the exit the
    // XGKICK falls into moves.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vf src(vf01)\n\
                  in_vi base(vi01)\n\
                  in_vi flag(vi02)\n\
                  --endenter\n\
                  sq src,0(base)\n\
                  ibeq flag,vi00,done\n\
                  xgkick base\n\
                  done:\n\
                  --exit\n\
                  --endexit\n";
    let plain = compile(source, &["--opt=none"]);
    let optimized = compile(source, &[]);
    let target = |listing: &str| {
        let lines: Vec<&str> = listing.lines().collect();
        let label = lines.iter().position(|line| *line == "done:").unwrap();
        rows(&lines[label..].join("\n"))
    };
    assert_eq!(target(&optimized), ["NOP[E] NOP", "NOP NOP"]);
    assert_eq!(target(&optimized), target(&plain));
    assert!(rows(&optimized).contains(&"NOP[E] xgkick VI01".to_string()));
    for flag in [0, 1] {
        let mut state = source_state();
        state.vi[2] = flag;
        let (before, after) = (run(&plain, state.clone()), run(&optimized, state));
        assert_eq!(after.state.kicks, before.state.kicks, "flag {flag}");
        assert_eq!(after.state.memory, before.state.memory, "flag {flag}");
    }
}

#[test]
fn exit_changes_nothing_without_end_bits() {
    for source in [UPPER_LAST, KICK_BARRIER] {
        let disabled = compile(source, &["-e"]);
        assert!(!disabled.contains("[E]"));
        assert_eq!(disabled, compile(source, &["-e", "--opt=none"]));
    }
}

// ----------------------------------------------
// loops
// ----------------------------------------------

/// Runs `source`'s listings with and without optimizations from states
/// with trip counts 1 to 6, and requires the same memory and VF04.
fn same_results(source: &str) -> (String, String) {
    let plain = compile(source, &["--opt=none"]);
    let optimized = compile(source, &[]);
    for count in 1..=6 {
        let mut state = source_state();
        state.vi[2] = count;
        for (address, qword) in state.memory.iter_mut().enumerate() {
            *qword =
                [(address % 7 + 1) as f32, 2.0, 0.5, (address % 3 + 1) as f32].map(f32::to_bits);
        }
        let program = |listing: &str| Program::parse(listing).unwrap();
        let before = program(&plain)
            .run(state.clone(), Config::TIMED, 4096)
            .unwrap();
        let after = program(&optimized).run(state, Config::TIMED, 4096).unwrap();
        assert_eq!(
            after.state.memory, before.state.memory,
            "count {count}\n{optimized}"
        );
        assert_eq!(
            after.state.vf[4], before.state.vf[4],
            "count {count}\n{optimized}"
        );
    }
    (plain, optimized)
}

fn pipelined(listing: &str) -> bool {
    listing.contains("_LPOPT_w")
}

const SCALE_LOOP: &str = ".init_vf_all\n\
                          .init_vi_all\n\
                          --enter\n\
                          in_vi base(vi01)\n\
                          in_vi count(vi02)\n\
                          in_vf scale(vf02)\n\
                          --endenter\n\
                          loop:\n\
                          lq value,0(base)\n\
                          mul value,value,scale\n\
                          sq value,0(base)\n\
                          iaddiu base,base,1\n\
                          isubiu count,count,1\n\
                          ibne count,vi00,loop\n\
                          --exit\n\
                          --endexit\n";

#[test]
fn loops_pipelines_a_loop_without_loopcs() {
    // As if it had `--LoopCS 1,0`: a prologue, a kernel and an epilogue,
    // with an early exit for a single iteration.
    let (plain, optimized) = same_results(SCALE_LOOP);
    assert!(!pipelined(&plain));
    assert!(pipelined(&optimized), "{optimized}");
    assert!(optimized.contains("EXPL_OPT_loop__MAIN_LOOP:"));
    assert!(!pipelined(&compile(SCALE_LOOP, &["--opt=exit"])));
    assert!(!pipelined(&compile(SCALE_LOOP, &["-L"])));
    // vcl mode pipelines only with the hint.
    let options = cli::parse(["-t1", "-uOPT"]).unwrap().compile;
    let vcl = vcl::compile(SCALE_LOOP, "/work/opt.vcl", &options).unwrap();
    assert!(!pipelined(&vcl.assembly));
}

#[test]
fn loops_keeps_values_the_code_after_the_loop_reads() {
    // Q from the last iteration's DIV, and the MAC flag of its SUB, read
    // after the loop.
    let q_after = ".init_vf_all\n\
                   .init_vi_all\n\
                   --enter\n\
                   in_vi base(vi01)\n\
                   in_vi count(vi02)\n\
                   --endenter\n\
                   loop:\n\
                   lq value,0(base)\n\
                   mul value,value,value\n\
                   div Q,value.x,value.w\n\
                   sq value,0(base)\n\
                   iaddiu base,base,1\n\
                   isubiu count,count,1\n\
                   ibne count,vi00,loop\n\
                   mulq result,value,Q\n\
                   --exit\n\
                   out_vf result(vf04)\n\
                   --endexit\n";
    let (_, optimized) = same_results(q_after);
    assert!(pipelined(&optimized), "{optimized}");
    let flags_after = ".init_vf_all\n\
                       .init_vi_all\n\
                       --enter\n\
                       in_vi base(vi01)\n\
                       in_vi count(vi02)\n\
                       --endenter\n\
                       loop:\n\
                       lq value,0(base)\n\
                       sub value,value,value\n\
                       sq value,0(base)\n\
                       iaddiu base,base,1\n\
                       isubiu count,count,1\n\
                       ibne count,vi00,loop\n\
                       iaddiu mask,vi00,0xffff\n\
                       fmand flags,mask\n\
                       isw.x flags,0(vi00)\n\
                       --exit\n\
                       --endexit\n";
    let (_, optimized) = same_results(flags_after);
    assert!(pipelined(&optimized), "{optimized}");
}

#[test]
fn loops_leaves_a_loop_that_reads_q_from_before_it() {
    // The first MULQ reads the quotient of the DIV before the loop. vcl's
    // pipeliner would read it before it lands (`loop-entry-q-in-flight`).
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vf a(vf01)\n\
                  in_vi n(vi02)\n\
                  in_vi p(vi01)\n\
                  --endenter\n\
                  div Q,a.x,a.y\n\
                  loop:\n\
                  mulq b,a,Q\n\
                  sq b,0(p)\n\
                  iaddiu p,p,1\n\
                  lq a,0(p)\n\
                  div Q,a.x,a.y\n\
                  isubiu n,n,1\n\
                  ibne n,vi00,loop\n\
                  --exit\n\
                  --endexit\n";
    let (_, optimized) = same_results(source);
    assert!(!pipelined(&optimized), "{optimized}");
}

#[test]
fn loops_leaves_a_loop_that_pipelining_does_not_shorten() {
    // Each iteration's IADDI feeds the next: a period of one iteration's
    // length gains nothing over the loop as it is.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi count(vi02)\n\
                  --endenter\n\
                  loop:\n\
                  iaddiu value,count,10\n\
                  iaddi count,count,-1\n\
                  ibne count,vi00,loop\n\
                  --exit\n\
                  out_vi value(vi01)\n\
                  --endexit\n";
    assert!(!pipelined(&compile(source, &[])));
    assert_eq!(compile(source, &[]), compile(source, &["--opt=exit"]));
}
