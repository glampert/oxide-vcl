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
fn loops_waits_for_q_from_before_the_loop() {
    // The first MULQ reads the quotient of the DIV before the loop, which
    // vcl's pipeliner would read before it lands (F7): the `entry` fix puts
    // a WAITQ before the loop, and the loop is pipelined.
    let source = |body: &str| {
        format!(
            ".init_vf_all\n\
             .init_vi_all\n\
             --enter\n\
             in_vf a(vf01)\n\
             in_vi p(vi01)\n\
             in_vi n(vi02)\n\
             --endenter\n\
             div Q,a.x,a.y\n\
             loop:\n\
             mulq b,a,Q\n\
             {body}\
             iaddiu p,p,1\n\
             lq a,0(p)\n\
             div Q,a.x,a.y\n\
             isubiu n,n,1\n\
             ibne n,vi00,loop\n\
             --exit\n\
             --endexit\n"
        )
    };
    let longer = source(
        "mul c,b,b\n\
         add c,c,b\n\
         sq c,0(p)\n",
    );
    let (plain, optimized) = same_results(&longer);
    assert!(pipelined(&optimized), "{optimized}");
    let waits = |listing: &str| rows(listing).iter().any(|row| row.ends_with("waitq"));
    assert!(waits(&optimized) && !waits(&plain), "{optimized}");
    // A loop pipelining does not shorten keeps no wait either.
    let short = source("sq b,0(p)\n");
    let (_, optimized) = same_results(&short);
    assert!(!pipelined(&optimized) && !waits(&optimized), "{optimized}");
    assert_eq!(optimized, compile(&short, &["--opt=exit"]));
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

#[test]
fn loops_leaves_a_loop_marked_loopcs_zero() {
    // `--LoopCS 0,0` keeps the optimization off one loop: in oxide mode
    // without the warning vcl mode prints for it.
    let source = SCALE_LOOP.replace("loop:\n", "loop:\n--LoopCS 0,0\n");
    let options = cli::parse(["-t1", "-uOPT", "--mode=oxide"])
        .unwrap()
        .compile;
    let compiled = vcl::compile(&source, "/work/opt.vcl", &options).unwrap();
    assert!(compiled.warnings.is_empty(), "{:?}", compiled.warnings);
    assert!(!pipelined(&compiled.assembly), "{}", compiled.assembly);
    assert_eq!(compiled.assembly, compile(&source, &["--no-opt=loops"]));
    let options = cli::parse(["-t1", "-uOPT"]).unwrap().compile;
    let vcl = vcl::compile(&source, "/work/opt.vcl", &options).unwrap();
    assert_eq!(vcl.warnings.len(), 1);
}

/// A transform loop of `vertices` vertices an iteration: 14 instructions
/// each, with a DIV and a MULQ.
fn transform_loop(vertices: usize) -> String {
    let mut source = String::from(
        ".init_vf_all\n\
         .init_vi_all\n\
         --enter\n\
         in_vi src(vi01)\n\
         in_vi count(vi02)\n\
         in_vi dst(vi03)\n\
         in_vf row0(vf01)\n\
         in_vf row1(vf02)\n\
         in_vf row2(vf03)\n\
         in_vf row3(vf04)\n\
         in_vf bias(vf05)\n\
         --endenter\n\
         loop:\n",
    );
    for k in 0..vertices {
        let (first, second) = (2 * k, 2 * k + 1);
        source += &format!(
            "lq pos{k},{first}(src)\n\
             lq col{k},{second}(src)\n\
             mulax acc,row0,pos{k}.x\n\
             madday acc,row1,pos{k}.y\n\
             maddaz acc,row2,pos{k}.z\n\
             maddw clp{k},row3,vf00.w\n\
             div Q,vf00.w,clp{k}.w\n\
             mulq scr{k},clp{k},Q\n\
             add scr{k},scr{k},bias\n\
             ftoi4 fix{k},scr{k}\n\
             mul col{k},col{k},bias\n\
             ftoi0 col{k},col{k}\n\
             sq col{k},{first}(dst)\n\
             sq fix{k},{second}(dst)\n"
        );
    }
    let step = 2 * vertices;
    source += &format!(
        "iaddiu src,src,{step}\n\
         iaddiu dst,dst,{step}\n\
         iaddi count,count,-1\n\
         ibgtz count,loop\n\
         --exit\n\
         --endexit\n"
    );
    source
}

#[test]
fn loops_bounds_the_search_on_a_large_loop() {
    // 60 instructions: without the work budget, the strategies that find
    // no schedule search for about 20 seconds in a release build before
    // their step budgets end them; with it, the whole compile takes well
    // under one. The strategy that wins is one of the cheap ones, so the
    // schedule is the one the unbounded search finds: a period of 36.
    let source = transform_loop(4);
    let start = std::time::Instant::now();
    let (plain, optimized) = same_results(&source);
    let bound = if cfg!(debug_assertions) { 120 } else { 10 };
    assert!(
        start.elapsed() < std::time::Duration::from_secs(bound),
        "{:?}",
        start.elapsed()
    );
    assert!(!pipelined(&plain));
    assert!(
        optimized.contains("_LPOPT_w=[ dUp ] 36 [36 36] 36"),
        "{optimized}"
    );
}

// ----------------------------------------------
// unswitch
// ----------------------------------------------

/// Runs `source`'s listings with and without optimizations, for modes 0
/// and 1 in VI03 and trip counts 1 to 6 in VI02, and requires the same
/// memory. A qword's W is 1 at addresses that are multiples of 3, else 0.
/// Returns the listings and both cycle counts at trip count 6, mode 1.
fn unswitched_same_results(source: &str) -> (String, String, u64, u64) {
    let plain = compile(source, &["--opt=none"]);
    let optimized = compile(source, &[]);
    let mut cycles = (0, 0);
    for mode in [0, 1] {
        for count in 1..=6 {
            let mut state = source_state();
            state.vi[2] = count;
            state.vi[3] = mode;
            state.set_vector(2, [0.5, -2.0, 3.0, 1.0]);
            for (address, qword) in state.memory.iter_mut().enumerate() {
                let w = u32::from(address % 3 == 0);
                *qword = [
                    ((address % 7 + 1) as f32).to_bits(),
                    2.0f32.to_bits(),
                    0.5f32.to_bits(),
                    w,
                ];
            }
            let program = |listing: &str| Program::parse(listing).unwrap();
            let before = program(&plain)
                .run(state.clone(), Config::TIMED, 4096)
                .unwrap();
            let after = program(&optimized).run(state, Config::TIMED, 4096).unwrap();
            assert_eq!(
                after.state.memory, before.state.memory,
                "mode {mode}, count {count}\n{optimized}"
            );
            if (mode, count) == (1, 6) {
                cycles = (before.cycles, after.cycles);
            }
        }
    }
    (plain, optimized, cycles.0, cycles.1)
}

fn branch_rows(listing: &str, mnemonic: &str) -> usize {
    rows(listing)
        .iter()
        .filter(|row| row.split_whitespace().any(|word| word == mnemonic))
        .count()
}

const MODE_LOOP: &str = ".init_vf_all\n\
                         .init_vi_all\n\
                         --enter\n\
                         in_vi base(vi01)\n\
                         in_vi count(vi02)\n\
                         in_vi mode(vi03)\n\
                         in_vf scale(vf02)\n\
                         --endenter\n\
                         loop:\n\
                         lq value,0(base)\n\
                         ibeq mode,vi00,plain_first\n\
                         mul value,value,scale\n\
                         plain_first:\n\
                         sq value,0(base)\n\
                         lq other,1(base)\n\
                         ibeq mode,vi00,plain_second\n\
                         mul other,other,scale\n\
                         plain_second:\n\
                         sq other,1(base)\n\
                         iaddiu base,base,2\n\
                         isubiu count,count,1\n\
                         ibne count,vi00,loop\n\
                         --exit\n\
                         --endexit\n";

#[test]
fn unswitch_gives_each_outcome_of_a_mode_its_own_loop() {
    // MODE never changes in the loop: one test before it picks a copy
    // that scales both values or one that scales neither, and the two
    // tests per iteration are gone. Each copy is then a loop of one
    // block, which the `loops` optimization pipelines.
    let (plain, optimized, before, after) = unswitched_same_results(MODE_LOOP);
    assert_eq!(branch_rows(&plain, "ibeq"), 2);
    assert_eq!(branch_rows(&optimized, "ibeq"), 1, "{optimized}");
    assert!(optimized.contains("loop__u0") && optimized.contains("loop__u1"));
    assert!(pipelined(&optimized), "{optimized}");
    assert!(after < before, "{before} -> {after} cycles\n{optimized}");
    // Turned off on its own, the other optimizations still run.
    let without = compile(MODE_LOOP, &["--no-opt=unswitch"]);
    assert_eq!(without, compile(MODE_LOOP, &["--opt=exit,loops"]));
    assert_eq!(branch_rows(&without, "ibeq"), 2);
    assert_eq!(
        compile(MODE_LOOP, &["--opt=unswitch"])
            .matches("__u1:")
            .count(),
        1
    );
}

#[test]
fn unswitch_shares_a_special_case_and_tests_the_mode_again_after_it() {
    // The arm that runs an inner loop is a special case: one copy of it,
    // shared by both copies of the loop. It changes MODE, and the loop
    // continues in the copy for the new value.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi base(vi01)\n\
                  in_vi count(vi02)\n\
                  in_vi mode(vi03)\n\
                  in_vf scale(vf02)\n\
                  --endenter\n\
                  loop:\n\
                  lq value,0(base)\n\
                  ibeq mode,vi00,plain_first\n\
                  mul value,value,scale\n\
                  plain_first:\n\
                  ilw.w split,0(base)\n\
                  ibne split,vi00,special\n\
                  sq value,0(base)\n\
                  b advance\n\
                  special:\n\
                  iaddiu parts,vi00,3\n\
                  part_loop:\n\
                  add value,value,scale\n\
                  isubiu parts,parts,1\n\
                  ibne parts,vi00,part_loop\n\
                  sq value,0(base)\n\
                  iaddiu mode,mode,1\n\
                  advance:\n\
                  iaddiu base,base,1\n\
                  isubiu count,count,1\n\
                  ibne count,vi00,loop\n\
                  --exit\n\
                  --endexit\n";
    let (_, optimized, _, _) = unswitched_same_results(source);
    assert!(optimized.contains("loop__u1:"), "{optimized}");
    assert_eq!(optimized.matches("part_loop:").count(), 1, "{optimized}");
}

#[test]
fn unswitch_finds_the_usual_path_wherever_it_sits_in_the_source() {
    // The usual arm comes after the special case and branches back up to
    // it. That branch runs forward, so it makes no inner loop: the usual
    // arm is still copied, and its MODE test resolves in each copy.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi base(vi01)\n\
                  in_vi count(vi02)\n\
                  in_vi mode(vi03)\n\
                  in_vf scale(vf02)\n\
                  --endenter\n\
                  loop:\n\
                  lq value,0(base)\n\
                  ibeq mode,vi00,plain_first\n\
                  mul value,value,scale\n\
                  plain_first:\n\
                  ilw.w split,0(base)\n\
                  ibeq split,vi00,usual\n\
                  special:\n\
                  iaddiu parts,vi00,3\n\
                  part_loop:\n\
                  add value,value,scale\n\
                  isubiu parts,parts,1\n\
                  ibne parts,vi00,part_loop\n\
                  sq value,0(base)\n\
                  b advance\n\
                  usual:\n\
                  lq other,1(base)\n\
                  ibeq mode,vi00,plain_second\n\
                  mul other,other,scale\n\
                  plain_second:\n\
                  ilw.w more,2(base)\n\
                  ibne more,vi00,special\n\
                  sq other,1(base)\n\
                  sq value,0(base)\n\
                  advance:\n\
                  iaddiu base,base,1\n\
                  isubiu count,count,1\n\
                  ibne count,vi00,loop\n\
                  --exit\n\
                  --endexit\n";
    let (plain, optimized, _, _) = unswitched_same_results(source);
    assert!(
        optimized.contains("usual__u0:") && optimized.contains("usual__u1:"),
        "{optimized}"
    );
    assert_eq!(optimized.matches("part_loop:").count(), 1, "{optimized}");
    // Both MODE tests resolve in the copies. What is left is the SPLIT test
    // in each copy, and MODE's dispatches at the header and after the
    // special case.
    assert_eq!(branch_rows(&plain, "ibeq"), 3);
    assert_eq!(branch_rows(&optimized, "ibeq"), 4, "{optimized}");
}

#[test]
fn unswitch_shares_what_an_early_out_skips_when_it_all_leads_to_an_inner_loop() {
    // SPLIT skips straight to the advance. What it skips is a setup, the
    // inner loop and code after it, all on the way into the loop or out of
    // it: a special case, so the setup's own branch (SMALL) is there once,
    // not in both copies.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi base(vi01)\n\
                  in_vi count(vi02)\n\
                  in_vi mode(vi03)\n\
                  in_vf scale(vf02)\n\
                  --endenter\n\
                  loop:\n\
                  lq value,0(base)\n\
                  ibeq mode,vi00,plain_first\n\
                  mul value,value,scale\n\
                  plain_first:\n\
                  sq value,0(base)\n\
                  ilw.w split,0(base)\n\
                  ibeq split,vi00,advance\n\
                  iaddiu parts,vi00,3\n\
                  ilw.w small,1(base)\n\
                  ibeq small,vi00,setup_done\n\
                  iaddiu parts,vi00,1\n\
                  setup_done:\n\
                  part_loop:\n\
                  add value,value,scale\n\
                  isubiu parts,parts,1\n\
                  ibne parts,vi00,part_loop\n\
                  sq value,0(base)\n\
                  advance:\n\
                  iaddiu base,base,1\n\
                  isubiu count,count,1\n\
                  ibne count,vi00,loop\n\
                  --exit\n\
                  --endexit\n";
    let (plain, optimized, _, _) = unswitched_same_results(source);
    assert!(optimized.contains("loop__u1:"), "{optimized}");
    assert_eq!(optimized.matches("part_loop:").count(), 1, "{optimized}");
    assert_eq!(
        optimized.matches("setup_done").count(),
        plain.matches("setup_done").count()
    );
    assert!(!optimized.contains("setup_done__u"), "{optimized}");
}

#[test]
fn unswitch_copies_what_an_early_out_skips_when_some_of_it_leads_to_no_inner_loop() {
    // CULL skips the usual output and, beside it, a special case with an
    // inner loop. The usual output reaches no loop, so what CULL skips is
    // not a special case as a whole: the usual output is copied, and its
    // MODE test resolves in each copy.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi base(vi01)\n\
                  in_vi count(vi02)\n\
                  in_vi mode(vi03)\n\
                  in_vf scale(vf02)\n\
                  --endenter\n\
                  loop:\n\
                  lq value,0(base)\n\
                  ilw.x cull,2(base)\n\
                  ibne cull,vi00,advance\n\
                  ilw.w split,0(base)\n\
                  ibne split,vi00,special\n\
                  ibeq mode,vi00,plain_out\n\
                  mul value,value,scale\n\
                  plain_out:\n\
                  sq value,0(base)\n\
                  b advance\n\
                  special:\n\
                  iaddiu parts,vi00,3\n\
                  part_loop:\n\
                  add value,value,scale\n\
                  isubiu parts,parts,1\n\
                  ibne parts,vi00,part_loop\n\
                  sq value,0(base)\n\
                  advance:\n\
                  iaddiu base,base,1\n\
                  isubiu count,count,1\n\
                  ibne count,vi00,loop\n\
                  --exit\n\
                  --endexit\n";
    let (_, optimized, _, _) = unswitched_same_results(source);
    assert!(optimized.contains("loop__u1:"), "{optimized}");
    assert_eq!(optimized.matches("part_loop:").count(), 1, "{optimized}");
    // MODE is tested only by the dispatches, at the header and after the
    // special case: the usual output's test resolved in both copies.
    let mode_tests = rows(&optimized)
        .iter()
        .filter(|row| row.contains("ibeq VI03") || row.contains("ibne VI03"))
        .count();
    assert_eq!(mode_tests, 2, "{optimized}");
}

/// A loop that tests three modes, none of which it changes: MODE (VI03)
/// twice an iteration, FORM (VI04) and SHADE (VI05) once each.
const THREE_MODE_LOOP: &str = ".init_vf_all\n\
                               .init_vi_all\n\
                               --enter\n\
                               in_vi base(vi01)\n\
                               in_vi count(vi02)\n\
                               in_vi mode(vi03)\n\
                               in_vi form(vi04)\n\
                               in_vi shade(vi05)\n\
                               in_vf scale(vf02)\n\
                               --endenter\n\
                               loop:\n\
                               lq value,0(base)\n\
                               ibeq mode,vi00,plain_first\n\
                               mul value,value,scale\n\
                               plain_first:\n\
                               ibeq form,vi00,plain_second\n\
                               add value,value,scale\n\
                               plain_second:\n\
                               ibeq shade,vi00,plain_third\n\
                               sub value,value,scale\n\
                               plain_third:\n\
                               sq value,0(base)\n\
                               lq other,1(base)\n\
                               ibeq mode,vi00,plain_fourth\n\
                               mul other,other,scale\n\
                               plain_fourth:\n\
                               sq other,1(base)\n\
                               iaddiu base,base,2\n\
                               isubiu count,count,1\n\
                               ibne count,vi00,loop\n\
                               --exit\n\
                               --endexit\n";

/// Runs `source`'s listings with and without optimizations for every
/// combination of VI03, VI04 and VI05 in 0 and 1, and trip counts 1 to 4,
/// and requires the same memory. Returns the optimized listing.
fn three_modes_same_results(source: &str) -> String {
    let plain = compile(source, &["--opt=none"]);
    let optimized = compile(source, &[]);
    for modes in 0..8 {
        for count in 1..=4 {
            let mut state = source_state();
            state.vi[2] = count;
            state.vi[3] = modes & 1;
            state.vi[4] = (modes >> 1) & 1;
            state.vi[5] = (modes >> 2) & 1;
            state.set_vector(2, [0.5, -2.0, 3.0, 1.0]);
            for (address, qword) in state.memory.iter_mut().enumerate() {
                *qword = [((address % 7 + 1) as f32).to_bits(), 0, 0, 0];
            }
            let program = |listing: &str| Program::parse(listing).unwrap();
            let before = program(&plain)
                .run(state.clone(), Config::TIMED, 4096)
                .unwrap();
            let after = program(&optimized).run(state, Config::TIMED, 4096).unwrap();
            assert_eq!(
                after.state.memory, before.state.memory,
                "modes {modes:03b}, count {count}\n{optimized}"
            );
        }
    }
    optimized
}

#[test]
fn unswitch_gives_each_copy_its_own_copies_for_a_second_mode() {
    // MODE, tested most, picks the first two copies; each of those is a
    // loop that still tests FORM, and gets a copy per outcome of it. SHADE
    // stays a branch: a copy of a copy is not unswitched again.
    let optimized = three_modes_same_results(THREE_MODE_LOOP);
    for copy in [
        "loop__u0__u0",
        "loop__u0__u1",
        "loop__u1__u0",
        "loop__u1__u1",
    ] {
        assert!(
            optimized.contains(&format!("{copy}:")),
            "{copy}\n{optimized}"
        );
    }
    assert!(!optimized.contains("__u0__u0__u"), "{optimized}");
    // In each of the four copies, the SHADE test and nothing else.
    let shade_tests = rows(&optimized)
        .iter()
        .filter(|row| row.contains("ibeq VI05") || row.contains("ibne VI05"))
        .count();
    assert_eq!(shade_tests, 4, "{optimized}");
}

#[test]
fn unswitch_once_when_four_copies_would_not_fit_in_micro_memory() {
    // Four copies of 700 stores are more than VU1's 2,048 instructions,
    // two are not: the loop gets the two.
    let stores: String = (0..700)
        .map(|offset| {
            let barrier = if offset % 25 == 24 { "--barrier\n" } else { "" };
            format!("isw.x count,{}(base)\n{barrier}", offset % 1000)
        })
        .collect();
    let source =
        THREE_MODE_LOOP.replace("sq other,1(base)\n", &format!("sq other,1(base)\n{stores}"));
    let optimized = compile(&source, &[]);
    assert!(optimized.contains("loop__u1:"), "{optimized}");
    assert!(!optimized.contains("loop__u1__u0"), "{optimized}");
}

#[test]
fn unswitch_leaves_a_loop_that_changes_its_mode() {
    let source = MODE_LOOP.replace(
        "isubiu count,count,1\n",
        "isubiu count,count,1\n\
         iaddiu mode,mode,1\n",
    );
    let (_, optimized, _, _) = unswitched_same_results(&source);
    assert_eq!(optimized, compile(&source, &["--no-opt=unswitch"]));
}

#[test]
fn unswitch_leaves_a_program_that_would_not_fit_in_micro_memory() {
    // Two copies of a loop of 1,100 stores would need more than VU1's
    // 2,048 instructions; of 400, they fit. Barriers keep the streams
    // short, and quick to schedule.
    let with_stores = |count: usize| {
        let stores: String = (0..count)
            .map(|offset| {
                let barrier = if offset % 25 == 24 { "--barrier\n" } else { "" };
                format!("isw.x count,{}(base)\n{barrier}", offset % 1000)
            })
            .collect();
        MODE_LOOP.replace("sq other,1(base)\n", &format!("sq other,1(base)\n{stores}"))
    };
    let fits = with_stores(400);
    assert!(compile(&fits, &[]).contains("loop__u1"));
    let too_large = with_stores(1100);
    let unchanged = compile(&too_large, &["--no-opt=unswitch"]);
    assert_eq!(compile(&too_large, &[]), unchanged);
    assert!(!unchanged.contains("loop__u1"));
}

// ----------------------------------------------
// pointers
// ----------------------------------------------

/// Runs `source`'s listings with and without `pointers` from states with
/// trip counts 1 to 6 (VI02) and its base in VI01, and requires the same
/// memory and VF04.
fn pointer_results(source: &str) -> (String, String) {
    let plain = compile(source, &["--no-opt=pointers"]);
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

/// The kernel's period in a listing.
fn period(listing: &str) -> u32 {
    let line = listing
        .lines()
        .find(|line| line.starts_with("; _LPOPT_w=["))
        .unwrap_or_else(|| panic!("no kernel\n{listing}"));
    line.split(']')
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

// Two pointers from one base, both stepped by 1; the store sits between
// the steps, so it sees the kept pointer a step on.
const TWO_POINTERS: &str = ".init_vf_all\n\
                            .init_vi_all\n\
                            --enter\n\
                            in_vi top(vi01)\n\
                            in_vi count(vi02)\n\
                            in_vf scale(vf02)\n\
                            --endenter\n\
                            iaddiu src,top,5\n\
                            iaddiu dst,top,120\n\
                            iadd last,src,count\n\
                            loop:\n\
                            lq value,0(src)\n\
                            mul value,value,scale\n\
                            lq other,1(src)\n\
                            iaddiu src,src,1\n\
                            sq value,0(dst)\n\
                            sq.x other,2(dst)\n\
                            iaddiu dst,dst,1\n\
                            ibne src,last,loop\n\
                            --exit\n\
                            --endexit\n";

#[test]
fn pointers_merges_two_pointers_a_loop_steps_together() {
    let (plain, optimized) = pointer_results(TWO_POINTERS);
    assert!(period(&optimized) < period(&plain), "{plain}\n{optimized}");
    // One pointer steps in the kernel, not two.
    let kernel = |listing: &str| {
        let start = listing.find("__MAIN_LOOP:").unwrap();
        let end = listing[start..]
            .find("__EPI")
            .map_or(listing.len(), |at| start + at);
        listing[start..end].matches("iaddiu").count()
    };
    assert_eq!(kernel(&optimized) + 1, kernel(&plain), "{optimized}");
}

#[test]
fn pointers_leaves_pointers_whose_difference_is_not_known() {
    // `dst` is `src` plus the count, which the loop's code cannot fold
    // into an offset.
    let source = TWO_POINTERS.replace("iaddiu dst,top,120\n", "iadd dst,top,count\n");
    let (plain, optimized) = pointer_results(&source);
    assert_eq!(plain, optimized);
}

#[test]
fn pointers_leaves_a_pointer_the_code_after_the_loop_reads() {
    let source = TWO_POINTERS.replace("--exit\n", "sq scale,0(dst)\n--exit\n");
    let (plain, optimized) = pointer_results(&source);
    assert_eq!(plain, optimized);
}

#[test]
fn pointers_counts_down_with_a_pointer() {
    // The count's step goes: the loop ends when the pointer reaches where
    // it is when the count runs out, computed before the loop.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi top(vi01)\n\
                  in_vi count(vi02)\n\
                  in_vf scale(vf02)\n\
                  --endenter\n\
                  iaddiu src,top,5\n\
                  iadd dst,src,count\n\
                  iadd left,top,count\n\
                  loop:\n\
                  lq value,0(src)\n\
                  mul value,value,scale\n\
                  sq value,0(dst)\n\
                  iaddiu src,src,1\n\
                  iaddiu dst,dst,1\n\
                  isubiu left,left,1\n\
                  ibne left,top,loop\n\
                  --exit\n\
                  --endexit\n";
    let (plain, optimized) = pointer_results(source);
    assert!(period(&optimized) < period(&plain), "{plain}\n{optimized}");
    assert!(!optimized.contains("isubiu"), "{optimized}");
}

#[test]
fn pointers_leaves_a_rewrite_that_does_not_shorten_the_kernel() {
    // The loop waits on DIV each iteration: a step less leaves the period
    // as it is, and the pipeliner would choose a deeper schedule.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vf a(vf01)\n\
                  in_vi n(vi02)\n\
                  in_vi p(vi03)\n\
                  --endenter\n\
                  div Q,a.x,a.y\n\
                  loop:\n\
                  --LoopCS 1,0\n\
                  mulq b,a,Q\n\
                  sq b,0(p)\n\
                  iaddiu p,p,1\n\
                  lq a,0(p)\n\
                  div Q,a.x,a.y\n\
                  isubiu n,n,1\n\
                  ibne n,vi00,loop\n\
                  --exit\n\
                  --endexit\n";
    assert_eq!(
        compile(source, &[]),
        compile(source, &["--no-opt=pointers"])
    );
}

// ----------------------------------------------
// hoist
// ----------------------------------------------

/// A clipping pass's shape: a header that tests the corner, a store when it
/// is kept, a second test that every corner makes, a store when it holds,
/// and the latch. The second test and the latch's steps run whenever the
/// header does.
const PASS: &str = ".syntax new\n\
                    .init_vf_all\n\
                    .init_vi_all\n\
                    --enter\n\
                    in_vi src(vi01)\n\
                    in_vi count(vi02)\n\
                    in_vi dst(vi03)\n\
                    in_vf limit(vf02)\n\
                    --endenter\n\
                    iaddiu kept,vi00,0\n\
                    lpass:\n\
                    lq.xyzw cur,0(src)\n\
                    lq.xyzw nxt,1(src)\n\
                    mul.xyzw prod,cur,nxt\n\
                    sub.w diff,cur,limit[w]\n\
                    fsand flagA,2\n\
                    ibne flagA,vi00,lskip\n\
                    sq.xyzw cur,0(dst)\n\
                    iaddiu dst,dst,1\n\
                    iaddiu kept,kept,1\n\
                    lskip:\n\
                    sub.w later,prod,limit[w]\n\
                    fsand flagB,2\n\
                    ibeq flagB,vi00,lnext\n\
                    add.xyzw mid,cur,nxt\n\
                    sq.xyzw mid,0(dst)\n\
                    iaddiu dst,dst,1\n\
                    lnext:\n\
                    iaddiu src,src,1\n\
                    isubiu count,count,1\n\
                    ibgtz count,lpass\n\
                    --exit\n\
                    out_vi kept(vi04)\n\
                    --endexit\n";

/// Runs `source`'s listings with and without `hoist` for 1 to 6 corners
/// (VI02) read from VI01 and written to VI03, with corners on both sides of
/// both tests, and requires the same memory and VI04 (the corners kept).
fn hoist_results(source: &str, dst: u16) -> (String, String) {
    pass_results(source, dst, "--no-opt=hoist")
}

/// [`hoist_results`] against the listing compiled with `without`.
fn pass_results(source: &str, dst: u16, without: &str) -> (String, String) {
    let plain = compile(source, &[without]);
    let optimized = compile(source, &[]);
    for count in 1..=6 {
        let mut state = State::default();
        state.vi[1] = 10;
        state.vi[2] = count;
        state.vi[3] = dst;
        state.set_vector(2, [0.0, 0.0, 0.0, 2.5]);
        for (address, qword) in state.memory.iter_mut().enumerate() {
            let w = (address % 5) as f32 - 1.0;
            *qword = [(address % 7) as f32 - 2.0, 1.5, -0.5, w].map(f32::to_bits);
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
        assert_eq!(after.state.vi[4], before.state.vi[4], "count {count}");
    }
    (plain, optimized)
}

/// The rows a table walks, stall rows included, and its code, by the name
/// in its comment.
fn table<'a>(listing: &'a str, name: &str) -> (u32, &'a str) {
    let marker = format!("[{name}]");
    let start = listing
        .lines()
        .position(|line| line.starts_with("; _LNOPT_w=[") && line.ends_with(&marker))
        .unwrap_or_else(|| panic!("no table {name}\n{listing}"));
    let lines: Vec<&str> = listing.lines().collect();
    let walked = lines[start]
        .split(']')
        .nth(2)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let begin = listing.find(lines[start]).unwrap() + lines[start].len();
    let end = listing[begin..]
        .find("; _L")
        .map_or(listing.len(), |at| begin + at);
    (walked, &listing[begin..end])
}

#[test]
fn hoist_moves_the_second_test_and_the_latch_into_the_header() {
    let (plain, optimized) = hoist_results(PASS, 200);
    let cycles = |listing: &str| {
        ["lpass", "lskip", "lnext"]
            .map(|name| table(listing, name).0)
            .iter()
            .sum::<u32>()
    };
    assert!(cycles(&optimized) < cycles(&plain), "{plain}\n{optimized}");
    // The latch and the second test keep only their branches: a row and its
    // delay slot each.
    assert_eq!(table(&optimized, "lnext").0, 2, "{optimized}");
    let (walked, test) = table(&optimized, "lskip");
    assert_eq!(walked, 2, "{optimized}");
    assert!(!test.contains("fsand"), "{optimized}");
}

#[test]
fn hoist_keeps_a_flag_writer_below_a_flag_read() {
    // The second test reads the flags of a SUB in the header, and stays where
    // it is: the kept corner's arm writes its register. Moved into the
    // header, the latch's MUL would be the last flag writer before it; no
    // other FMAC instruction stands between them.
    let source = PASS
        .replace(
            "ibne flagA,vi00,lskip\n",
            "sub.w later,prod,limit[w]\nibne flagA,vi00,lskip\n",
        )
        .replace(
            "iaddiu kept,kept,1\nlskip:\nsub.w later,prod,limit[w]\n",
            "iaddiu kept,kept,1\niaddiu flagB,vi00,0\nlskip:\n",
        )
        .replace("add.xyzw mid,cur,nxt\nsq.xyzw mid,", "sq.xyzw nxt,")
        .replace(
            "iaddiu src,src,1\n",
            "mul.w scaled,cur,limit[w]\nsq.w scaled,40(src)\niaddiu src,src,1\n",
        );
    let (_, optimized) = hoist_results(&source, 200);
    // It may join the second test, after its FSAND.
    let (_, header) = table(&optimized, "lpass");
    assert!(!header.contains("mulw.w"), "{optimized}");
}

#[test]
fn hoist_keeps_a_load_below_a_store() {
    // The latch's load reads what the kept corner's store may have written
    // (the corners are written over the ones the loop reads next).
    let source = PASS.replace(
        "iaddiu src,src,1\n",
        "lq.xyzw peek,1(src)\nsq.xyzw peek,100(src)\niaddiu src,src,1\n",
    );
    let (_, optimized) = hoist_results(&source, 11);
    let (_, latch) = table(&optimized, "lnext");
    assert!(latch.contains("lq"), "{optimized}");
}

// ----------------------------------------------
// speculate
// ----------------------------------------------

/// A clipping pass with its intersection: when the edge to the next corner
/// crosses the plane, the cut point is the corners' difference scaled by
/// their distances' ratio. The intersection divides first and waits for Q.
const CUT: &str = ".syntax new\n\
                   .init_vf_all\n\
                   .init_vi_all\n\
                   --enter\n\
                   in_vi src(vi01)\n\
                   in_vi count(vi02)\n\
                   in_vi dst(vi03)\n\
                   in_vf limit(vf02)\n\
                   --endenter\n\
                   iaddiu kept,vi00,0\n\
                   lpass:\n\
                   lq.xyzw cur,0(src)\n\
                   lq.xyzw nxt,1(src)\n\
                   mul.w dcur,cur,limit[w]\n\
                   mul.w dnxt,nxt,limit[w]\n\
                   mul.w cut,dcur,dnxt[w]\n\
                   sub.w diff,dcur,vf00[x]\n\
                   fsand flagA,2\n\
                   ibne flagA,vi00,lskip\n\
                   sq.xyzw cur,0(dst)\n\
                   iaddiu dst,dst,1\n\
                   iaddiu kept,kept,1\n\
                   lskip:\n\
                   sub.w later,cut,vf00[x]\n\
                   fsand flagB,2\n\
                   ibeq flagB,vi00,lnext\n\
                   lcut:\n\
                   sub.w den,dcur,dnxt[w]\n\
                   div q,dcur[w],den[w]\n\
                   sub.xyzw edge,nxt,cur\n\
                   mulaq.xyzw acc,edge,q\n\
                   madd.xyzw mid,cur,vf00[w]\n\
                   sq.xyzw mid,0(dst)\n\
                   iaddiu dst,dst,1\n\
                   lnext:\n\
                   iaddiu src,src,1\n\
                   isubiu count,count,1\n\
                   ibgtz count,lpass\n\
                   --exit\n\
                   out_vi kept(vi04)\n\
                   --endexit\n";

fn speculate_results(source: &str) -> (String, String) {
    pass_results(source, 200, "--no-opt=speculate")
}

#[test]
fn speculate_starts_the_division_in_the_header() {
    let (plain, optimized) = speculate_results(CUT);
    // The header takes the division, the difference it divides by and the
    // edge in its stall cycles, and is no longer for them.
    let (before, _) = table(&plain, "lpass");
    let (after, header) = table(&optimized, "lpass");
    assert!(after <= before, "{plain}\n{optimized}");
    assert!(header.contains("div"), "{optimized}");
    assert!(header.contains(" sub "), "{optimized}");
    // The intersection no longer waits for Q from its own start.
    let (before, _) = table(&plain, "lcut");
    let (after, cut) = table(&optimized, "lcut");
    assert!(after < before, "{plain}\n{optimized}");
    assert!(!cut.contains("div"), "{optimized}");
}

#[test]
fn speculate_keeps_work_whose_register_the_other_path_reads() {
    // The latch stores the last edge the intersection computed: on the
    // corners it skips, that is an earlier corner's.
    let source = CUT
        .replace(
            "iaddiu kept,vi00,0\n",
            "iaddiu kept,vi00,0\nsub.xyzw edge,vf00,vf00\n",
        )
        .replace(
            "iaddiu src,src,1\n",
            "sq.xyzw edge,100(src)\niaddiu src,src,1\n",
        );
    let (_, optimized) = speculate_results(&source);
    let (_, header) = table(&optimized, "lpass");
    assert!(!header.contains(" sub "), "{optimized}");
}

#[test]
fn speculate_keeps_a_division_whose_q_the_other_path_reads() {
    // After the loop, Q is the last cut's ratio, or the one before the loop
    // when no edge was cut: the last corner's edge may not have been.
    let source = CUT
        .replace(
            "iaddiu kept,vi00,0\n",
            "iaddiu kept,vi00,0\ndiv q,vf00[w],limit[w]\n",
        )
        .replace(
            "--exit\n",
            "mulq.xyzw scale,limit,q\nsq.xyzw scale,300(vi00)\n--exit\n",
        );
    let (_, optimized) = speculate_results(&source);
    let (_, header) = table(&optimized, "lpass");
    assert!(!header.contains("div"), "{optimized}");
}

#[test]
fn speculate_keeps_a_flag_writer_above_a_test_it_would_feed() {
    // The intersection's first instruction reads the second test's
    // difference, which `hoist` moves into the header just above that test's
    // FSAND. Landed there, its subtraction, of the opposite sign, would be
    // the flags the FSAND reads; past the FSAND, nothing in the header writes
    // the flags again before the streams after it read them.
    let source = CUT.replace(
        "lcut:\n",
        "lcut:\nsub.w scaled,vf00,later\nsq.w scaled,50(dst)\n",
    );
    let (_, optimized) = speculate_results(&source);
    let (_, header) = table(&optimized, "lpass");
    assert!(!header.contains(" sub.w "), "{optimized}");
}

#[test]
fn speculate_keeps_a_flag_writer_its_own_stream_tests() {
    // The intersection first tests the current corner's side again and skips
    // the cut for one side. Moved into the header, its ADD would leave the
    // FSAND reading the second test's flags instead.
    let source = CUT.replace(
        "lcut:\n",
        "lcut:\nadd.w side,dcur,vf00[x]\nfsand flagC,2\nibne flagC,vi00,lnext\n",
    );
    let (_, optimized) = speculate_results(&source);
    let (_, header) = table(&optimized, "lpass");
    assert!(!header.contains("addx.w"), "{optimized}");
}

// ----------------------------------------------
// steps
// ----------------------------------------------

/// A loop that copies two qwords a pass and steps each pointer once a
/// qword, so the lower pipe bounds its kernel.
const STEPPED: &str = ".init_vf_all\n\
                       .init_vi_all\n\
                       --enter\n\
                       in_vi base(vi01)\n\
                       in_vi count(vi02)\n\
                       in_vi dst(vi03)\n\
                       --endenter\n\
                       loop:\n\
                       lq first,0(base)\n\
                       lq second,1(base)\n\
                       add total,first,second\n\
                       sq total,0(dst)\n\
                       sq first,1(dst)\n\
                       iaddiu base,base,1\n\
                       iaddiu base,base,1\n\
                       iaddiu dst,dst,1\n\
                       iaddiu dst,dst,1\n\
                       isubiu count,count,1\n\
                       ibne count,vi00,loop\n\
                       --exit\n\
                       --endexit\n";

/// The `iaddiu` instructions of a listing's kernel.
fn steps_in(listing: &str) -> usize {
    let start = listing
        .find("; _LPOPT_w=[")
        .unwrap_or_else(|| panic!("no kernel\n{listing}"));
    let kernel = &listing[start..];
    let end = kernel[1..].find("; _L").map_or(kernel.len(), |at| at + 1);
    kernel[..end]
        .lines()
        .filter(|line| line.contains(" iaddiu "))
        .count()
}

#[test]
fn steps_folds_a_run_of_steps_into_one() {
    // Expanding the kernel merges the steps it holds, but the search
    // gives each step a slot of the lower pipe: 10 lower instructions and
    // a period of 10, against 8 folded.
    let (_, optimized) = same_results(STEPPED);
    let unfolded = compile(STEPPED, &["--no-opt=steps"]);
    assert_eq!(period(&unfolded), 10, "{unfolded}");
    assert_eq!(period(&optimized), 8, "{optimized}");
}

#[test]
fn steps_moves_the_steps_before_an_access_into_its_offset() {
    // Each load and store reads the pointer one step on: folded, they take
    // that step into their offsets.
    let source = STEPPED
        .replace(
            "lq first,0(base)\nlq second,1(base)\n",
            "lq first,0(base)\niaddiu base,base,1\nlq second,0(base)\n",
        )
        .replace(
            "sq total,0(dst)\nsq first,1(dst)\niaddiu base,base,1\niaddiu base,base,1\n",
            "sq total,0(dst)\niaddiu dst,dst,1\nsq first,0(dst)\niaddiu base,base,1\n",
        )
        .replace("iaddiu dst,dst,1\niaddiu dst,dst,1\n", "iaddiu dst,dst,1\n");
    let (_, optimized) = same_results(&source);
    let unfolded = compile(&source, &["--no-opt=steps"]);
    assert!(steps_in(&optimized) < steps_in(&unfolded), "{optimized}");
}

#[test]
fn steps_keeps_a_run_whose_pointer_is_read_between() {
    // `base` is stored between its steps, as a value; `dst`'s steps fold.
    let source = ".init_vf_all\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi base(vi01)\n\
                  in_vi dst(vi03)\n\
                  --endenter\n\
                  lq first,0(base)\n\
                  iaddiu base,base,1\n\
                  isw.x base,0(dst)\n\
                  iaddiu base,base,1\n\
                  lq second,0(base)\n\
                  sq second,1(dst)\n\
                  iaddiu dst,dst,1\n\
                  iaddiu dst,dst,1\n\
                  sq first,0(dst)\n\
                  --exit\n\
                  --endexit\n";
    let (_, optimized) = same_results(source);
    let steps = |listing: &str| listing.matches(" iaddiu ").count();
    let unfolded = compile(source, &["--no-opt=steps"]);
    assert_eq!(steps(&unfolded), 4, "{unfolded}");
    assert_eq!(steps(&optimized), 3, "{optimized}");
}
