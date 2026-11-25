//! The public corpus benchmark (docs/TESTING.md, "Benchmarks"): every
//! program compiled in vcl and oxide mode, with what the compile did
//! (`vcl::stats`) and its size, then run in the timed VU model from the
//! seeded states of the listing checks, with its cycles and stalls. The
//! table is `tests/bench/corpus.txt`. `loops` sweeps the trip counts
//! instead, and gives each loop's kernel, prologue, epilogues and exit
//! their own cycles (`tests/bench/loops.txt`):
//!
//! ```sh
//! cargo test --release -p oxide-vcl --test bench -- --ignored --nocapture
//! BENCH_UPDATE=1 cargo test --release -p oxide-vcl --test bench -- --ignored
//! ```

#[path = "support/bench.rs"]
mod bench;
#[path = "support/corpus.rs"]
mod corpus;
#[path = "support/seeds.rs"]
mod seeds;
#[path = "support/vu_model.rs"]
mod vu_model;

use std::path::PathBuf;

use bench::Row;
use seeds::{MAX_STEPS, SEEDS, count_state, seeded_state};
use vu_model::{Config, Program};

const HEADING: &str = "\
The public corpus in each mode: the listing's instruction pairs; over the
seeded states, how many runs finish, their cycles and stall cycles; and
the compile's work: search nodes, wrangler steps and checks, flow analyses
and the blocks they walk. Written by crates/oxide-vcl/tests/bench.rs.";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// `case` compiled with its flags in `mode`, or `None` when rejected.
fn compile(case: &corpus::Case, mode: &str) -> Option<vcl::Compiled> {
    let mode_flag = format!("--mode={mode}");
    let flags = case
        .arguments
        .iter()
        .map(String::as_str)
        .chain([mode_flag.as_str()]);
    let options = vcl_cli::parse(flags).expect("the case's flags").compile;
    vcl::compile(&case.source, &case.path, &options).ok()
}

/// `measure` on every job, on every core: each compile runs, and counts its
/// work, on one thread. The rows keep the jobs' order.
fn measure_all<J: Sync>(jobs: &[J], measure: impl Fn(&J) -> Vec<Row> + Sync) -> Vec<Row> {
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let measured = std::sync::Mutex::new(vec![Vec::new(); jobs.len()]);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else {
                        break;
                    };
                    let rows = measure(job);
                    measured.lock().unwrap()[index] = rows;
                }
            });
        }
    });
    measured
        .into_inner()
        .unwrap()
        .into_iter()
        .flatten()
        .collect()
}

/// The jobs of a corpus benchmark: every case in each mode.
fn jobs(cases: &[corpus::Case]) -> Vec<(&'static str, &corpus::Case)> {
    ["vcl", "oxide"]
        .into_iter()
        .flat_map(|mode| cases.iter().map(move |case| (mode, case)))
        .collect()
}

/// A row for `case` in `mode`: what its compile did and how it runs.
fn measure(case: &corpus::Case, mode: &str) -> Row {
    let row = Row::new(&[mode, &case.name]);
    let Some(compiled) = compile(case, mode) else {
        return row.value("rejected", 1);
    };
    let row = row.value("pairs", compiled.instruction_pairs as u64);
    let (mut runs, mut cycles, mut stalls) = (0, 0, 0);
    if let Ok(program) = Program::parse(&compiled.assembly) {
        for seed in 0..SEEDS {
            let Ok(execution) = program.run(seeded_state(seed), Config::TIMED, MAX_STEPS) else {
                continue;
            };
            runs += 1;
            cycles += execution.cycles;
            stalls += execution
                .profile
                .iter()
                .map(|&(_, stall)| stall)
                .sum::<u64>();
        }
    }
    let row = row
        .value("runs", runs)
        .value("cycles", cycles)
        .value("stalls", stalls);
    bench::work(row, &compiled.stats)
}

#[test]
#[ignore = "a benchmark: run it with --ignored"]
fn corpus() {
    let cases = corpus::cases(&root().join("tests/corpus"));
    let rows = measure_all(&jobs(&cases), |&(mode, case)| vec![measure(case, mode)]);
    bench::compare_or_update(&root().join("tests/bench/corpus.txt"), HEADING, &rows);
}

/// The trip counts the loop sweep runs: every count seed's, and larger
/// ones, multiples of the strides 1, 2, 3, 4 and 6 among them.
const SWEEP: [u16; 13] = [1, 2, 3, 4, 5, 6, 8, 9, 12, 16, 24, 32, 48];

/// A trip count's value name in the sweep's `total` row.
const SWEEP_NAMES: [&str; 13] = [
    "c1", "c2", "c3", "c4", "c5", "c6", "c8", "c9", "c12", "c16", "c24", "c32", "c48",
];

/// The rows of the loop sweep for `case` in `mode`: the cycles at each trip
/// count, then for each table entered how often (issues of its first pair:
/// a kernel's iterations), its cycles and stall cycles, and its cycles per
/// entry in hundredths.
fn sweep(case: &corpus::Case, mode: &str) -> Vec<Row> {
    let total = Row::new(&[mode, &case.name, "total"]);
    let Some(program) =
        compile(case, mode).and_then(|compiled| Program::parse(&compiled.assembly).ok())
    else {
        return vec![total.value("rejected", 1)];
    };
    let mut total = total;
    let mut parts = Vec::new();
    // A program that stops at --cont runs its loops once MSCNT resumes it,
    // from the memory it left: the host's next batch, here the same count.
    let resumes = case.source.contains("--cont");
    for (count, name) in SWEEP.into_iter().zip(SWEEP_NAMES) {
        let Some(runs) = sweep_runs(&program, count, resumes) else {
            continue;
        };
        total = total.value(name, runs.iter().map(|run| run.cycles).sum());
        bench::add_parts(&mut parts, &program, &runs, "CORPUS");
    }
    let mut rows = vec![total];
    rows.extend(bench::part_rows(&[mode, &case.name], parts));
    rows
}

/// The runs of `program` at trip count `count`: one, and with `resumes` a
/// second from where MSCNT resumes it; `None` when one does not finish.
fn sweep_runs(program: &Program, count: u16, resumes: bool) -> Option<Vec<vu_model::Execution>> {
    let first = program
        .run(count_state(count), Config::TIMED, MAX_STEPS)
        .ok()?;
    if !resumes || first.resume >= program.pair_count() {
        return Some(vec![first]);
    }
    let second = program
        .run_from(first.state.clone(), first.resume, Config::TIMED, MAX_STEPS)
        .ok()?;
    Some(vec![first, second])
}

#[test]
#[ignore = "a benchmark: run it with --ignored"]
fn loops() {
    let cases = corpus::cases(&root().join("tests/corpus"));
    let rows = measure_all(&jobs(&cases), |&(mode, case)| sweep(case, mode));
    let heading = "\
The public corpus in each mode, over a sweep of trip counts: the count
seeds' states for 1 to 48, whose memory's low 16 bits and VI01-VI15 hold
the count, and a program that stops at --cont resumed once from where it
stopped. `total` gives the cycles at each count whose runs finish (c1 to
c48). Each table of the listing the runs entered is a row: a loop's
kernel, its prologue, its epilogues together and its exit, or a table of
its own (table:, such as a loop scheduled as straight-line code). A row
gives how often it was entered (issues of its first pair: a kernel's
iterations), its cycles and stall cycles over the sweep, and its cycles
per entry in hundredths (per100: a kernel's cycles per iteration).
Written by crates/oxide-vcl/tests/bench.rs (loops).";
    bench::compare_or_update(&root().join("tests/bench/loops.txt"), heading, &rows);
}

#[test]
fn tables_report_what_changed() {
    let old = bench::format(
        "a table",
        &[
            Row::new(&["oxide", "a"])
                .value("cycles", 100)
                .value("pairs", 10),
            Row::new(&["oxide", "b"]).value("cycles", 50),
            Row::new(&["oxide", "gone"]).value("cycles", 7),
        ],
    );
    let new = [
        Row::new(&["oxide", "a"])
            .value("cycles", 90)
            .value("pairs", 10),
        Row::new(&["oxide", "b"]).value("cycles", 50),
        Row::new(&["oxide", "added"]).value("cycles", 1),
    ];
    assert_eq!(
        bench::report(&old, &new),
        "changed  oxide a: cycles 100 -> 90\n\
         new      oxide added\n\
         gone     oxide gone\n\
         total    oxide:\n\
         \x20          cycles 150 -> 140 (-6.7%)\n"
    );
    assert_eq!(bench::report(&old, &new[..2]).lines().count(), 4);
    assert_eq!(bench::parse(&old)["oxide a"]["pairs"], 10);
}

#[test]
fn loop_tables_name_their_parts() {
    let table = |name: &str, kernel| vu_model::Table {
        start: 0,
        name: name.into(),
        kernel,
    };
    let cases = [
        (
            "EXPL_CORPUS_xform_loop__MAIN_LOOP",
            true,
            "kernel:xform_loop",
        ),
        (
            "EXPL_CORPUS_xform_loop__ENTRY_POINT",
            false,
            "prologue:xform_loop",
        ),
        ("EXPL_CORPUS_xform_loop__PRO2", false, "prologue:xform_loop"),
        ("EXPL_CORPUS_xform_loop__EPI1", false, "epilogue:xform_loop"),
        (
            "EXPL_CORPUS_xform_loop__EXIT_POINT",
            false,
            "exit:xform_loop",
        ),
        ("vertexLoop", false, "table:vertexLoop"),
        ("__v_CORPUS_4", false, "table:__v_CORPUS_4"),
    ];
    for (name, kernel, part) in cases {
        assert_eq!(bench::part(&table(name, kernel), "CORPUS"), part, "{name}");
    }
}
