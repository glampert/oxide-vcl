//! The public corpus benchmark (docs/TESTING.md, "Benchmarks"): every
//! program compiled in vcl and oxide mode, with what the compile did
//! (`vcl::stats`) and its size, then run in the timed VU model from the
//! seeded states of the listing checks, with its cycles and stalls. The
//! table is `tests/bench/corpus.txt`:
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
use seeds::{MAX_STEPS, SEEDS, seeded_state};
use vu_model::{Config, Program};

const HEADING: &str = "\
The public corpus in each mode: the listing's instruction pairs; over the
seeded states, how many runs finish, their cycles and stall cycles; and
the compile's work: search nodes, wrangler steps and checks, flow analyses
and the blocks they walk. Written by crates/oxide-vcl/tests/bench.rs.";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// A row for `case` in `mode`: what its compile did and how it runs.
fn measure(case: &corpus::Case, mode: &str) -> Row {
    let mode_flag = format!("--mode={mode}");
    let flags = case
        .arguments
        .iter()
        .map(String::as_str)
        .chain([mode_flag.as_str()]);
    let row = Row::new(&[mode, &case.name]);
    let options = vcl_cli::parse(flags).expect("the case's flags").compile;
    let Ok(compiled) = vcl::compile(&case.source, &case.path, &options) else {
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
    let jobs: Vec<(&str, &corpus::Case)> = ["vcl", "oxide"]
        .into_iter()
        .flat_map(|mode| cases.iter().map(move |case| (mode, case)))
        .collect();
    // Each compile runs, and counts its work, on one thread: the jobs are
    // shared out, and the rows kept in job order.
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let measured = std::sync::Mutex::new(vec![None; jobs.len()]);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(mode, case)) = jobs.get(index) else {
                        break;
                    };
                    let row = measure(case, mode);
                    measured.lock().unwrap()[index] = Some(row);
                }
            });
        }
    });
    let rows: Vec<Row> = measured
        .into_inner()
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    bench::compare_or_update(&root().join("tests/bench/corpus.txt"), HEADING, &rows);
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
