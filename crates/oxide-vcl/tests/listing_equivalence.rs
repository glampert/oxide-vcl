//! Equivalence of two listings of one program: both run in the timed VU
//! model (`support/vu_model.rs`, `Config::TIMED`) from the same seeded
//! states, and must leave the same data memory and send the same GIF
//! packets (docs/TESTING.md, "Listing equivalence"). This is the check
//! oxide mode's optimizations answer to (docs/OPTIMIZATION_PLAN.md):
//! each keeps the arithmetic, so results are bit-identical.

#[path = "support/corpus.rs"]
mod corpus;
#[path = "support/vu_model.rs"]
mod vu_model;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use vcl_cli as cli;
use vu_model::{Config, Program, State};

/// Random states, then one state per trip count in [`COUNTS`].
const RANDOM_SEEDS: u64 = 4;
/// What counts and pointers read with ILW, and the incoming VI values,
/// are in the count seeds: trip counts 1 to 6 of a loop that counts down
/// by one, and 1 to 4 of one that counts down by three. Pipelined loops
/// exit early from their prologues at small counts.
const COUNTS: [u16; 8] = [1, 2, 3, 4, 5, 6, 9, 12];
const SEEDS: u64 = RANDOM_SEEDS + COUNTS.len() as u64;
const MAX_STEPS: usize = 200_000;

fn cases() -> Vec<corpus::Case> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut cases = corpus::cases(&root.join("tests/corpus"));
    cases.extend(corpus::cases(&root.join("tests/probes")));
    cases
}

/// The listing of `case` with its recorded flags plus `extra`, or `None`
/// when the program is rejected.
fn compile(case: &corpus::Case, extra: &[&str]) -> Option<String> {
    let flags = case
        .arguments
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied());
    let options = cli::parse(flags).ok()?.compile;
    vcl::compile(&case.source, &case.path, &options)
        .ok()
        .map(|compiled| compiled.assembly)
}

/// A random start: VF values of mixed sign and magnitude; data memory
/// whose low 16 bits are 12, 24, 36 or 48, so that counts and pointers read
/// with ILW stay short and loops stepping by 1, 2, 3, 4 or 6 to a bound
/// reach it; small VI, TOP and ITOP values. A count seed sets those low
/// bits and VI01 to VI15 to its count instead.
fn seeded_state(seed: u64) -> State {
    let count = seed
        .checked_sub(RANDOM_SEEDS)
        .map(|index| COUNTS[index as usize]);
    let mut random = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    let float = |next: &mut dyn FnMut() -> u64| {
        let value = next();
        let sign = (value & 1) as u32;
        let exponent = 123 + (value >> 1) % 9; // 2^-4 to 2^4
        let mantissa = (value >> 8) as u32 & 0x7f_0000;
        sign << 31 | (exponent as u32) << 23 | mantissa
    };
    let mut state = State::default();
    for register in &mut state.vf[1..] {
        *register = std::array::from_fn(|_| float(&mut next));
    }
    for qword in &mut state.memory {
        *qword = std::array::from_fn(|_| {
            let low = count.map_or((next() % 4 + 1) as u32 * 12, u32::from);
            float(&mut next) & !0xffff | low
        });
    }
    for register in &mut state.vi[1..16] {
        *register = count.unwrap_or((next() % 32 + 1) as u16);
    }
    state.acc = std::array::from_fn(|_| float(&mut next));
    state.q = float(&mut next);
    state.p = float(&mut next);
    state.i = float(&mut next);
    state.top = (next() % 512) as u16;
    state.itop = (next() % 32 + 1) as u16;
    state
}

/// What a run leaves that another listing of the program must reproduce.
#[derive(Debug, PartialEq)]
struct Observed {
    memory: Vec<[u32; 4]>,
    kicks: Vec<(u16, Vec<[u32; 4]>)>,
}

/// The outcome of running one listing from one seed.
#[derive(Debug)]
enum Run {
    Finished(Observed),
    /// The step limit: a loop whose count the seed made long.
    Unfinished,
    Failed(String),
}

fn run(program: &Program, seed: u64, max_steps: usize) -> Run {
    match program.run(seeded_state(seed), Config::TIMED, max_steps) {
        Ok(execution) => Run::Finished(Observed {
            memory: execution.state.memory,
            kicks: execution.state.kicks,
        }),
        Err(error) if error.starts_with("execution exceeded") => Run::Unfinished,
        Err(error) => Run::Failed(error),
    }
}

/// How two listings of a program compare.
#[derive(Debug)]
enum Verdict {
    /// The same text, comments and spacing aside.
    Identical,
    /// Both finished from this many seeds and left the same.
    Equivalent(usize),
    /// No seed finished both: why.
    Unchecked(String),
    /// How they differ.
    Different(String),
}

fn compare(left: &str, right: &str) -> Verdict {
    if corpus::normalized(left) == corpus::normalized(right) {
        return Verdict::Identical;
    }
    let (left, right) = match (Program::parse(left), Program::parse(right)) {
        (Ok(left), Ok(right)) => (left, right),
        (Err(error), _) | (_, Err(error)) => return Verdict::Unchecked(format!("parse: {error}")),
    };
    let mut seeds = 0;
    let mut reason = String::new();
    for seed in 0..SEEDS {
        let mut outcomes = [run(&left, seed, MAX_STEPS), run(&right, seed, MAX_STEPS)];
        // A listing that executes fewer pairs can finish where the other
        // reaches the limit: give the unfinished one longer.
        if matches!(outcomes, [Run::Finished(_), Run::Unfinished]) {
            outcomes[1] = run(&right, seed, 8 * MAX_STEPS);
        } else if matches!(outcomes, [Run::Unfinished, Run::Finished(_)]) {
            outcomes[0] = run(&left, seed, 8 * MAX_STEPS);
        }
        match outcomes {
            [Run::Finished(a), Run::Finished(b)] => {
                if a != b {
                    return Verdict::Different(format!("seed {seed}: {}", difference(&a, &b)));
                }
                seeds += 1;
            }
            [Run::Unfinished, Run::Unfinished] => reason = "step limit".into(),
            [Run::Failed(a), Run::Failed(b)] if a == b => reason = a,
            [a, b] => return Verdict::Different(format!("seed {seed}: {a:?} against {b:?}")),
        }
    }
    if seeds == 0 {
        Verdict::Unchecked(reason)
    } else {
        Verdict::Equivalent(seeds)
    }
}

impl Verdict {
    fn describe(&self) -> String {
        match self {
            Verdict::Identical => "identical".into(),
            Verdict::Equivalent(seeds) => format!("equivalent on {seeds} seeds"),
            Verdict::Unchecked(reason) => format!("not checked: {reason}"),
            Verdict::Different(how) => format!("different: {how}"),
        }
    }
}

fn difference(left: &Observed, right: &Observed) -> String {
    if let Some(address) = (0..left.memory.len()).find(|&a| left.memory[a] != right.memory[a]) {
        return format!(
            "memory qword {address}: {:08x?} against {:08x?}",
            left.memory[address], right.memory[address]
        );
    }
    if left.kicks.len() != right.kicks.len() {
        return format!("{} kicks against {}", left.kicks.len(), right.kicks.len());
    }
    let kick = (0..left.kicks.len())
        .find(|&k| left.kicks[k] != right.kicks[k])
        .unwrap_or(0);
    format!(
        "kick {kick}: from {} against {}",
        left.kicks[kick].0, right.kicks[kick].0
    )
}

/// Each case's listings: the original's recorded one (`None` where it
/// rejected the program or wrote it to a file), and oxide mode's without
/// and with optimizations.
struct Listings {
    name: String,
    original: Option<String>,
    unoptimized: Option<String>,
    optimized: Option<String>,
}

/// Every public corpus program and probe, compiled once for all the tests
/// here, in parallel.
fn listings() -> &'static [Listings] {
    static LISTINGS: OnceLock<Vec<Listings>> = OnceLock::new();
    LISTINGS.get_or_init(|| {
        let cases = cases();
        let next = AtomicUsize::new(0);
        let workers = std::thread::available_parallelism().map_or(4, usize::from);
        let mut listings: Vec<(usize, Listings)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| {
                        let mut done = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(case) = cases.get(index) else {
                                return done;
                            };
                            done.push((
                                index,
                                Listings {
                                    name: case.name.clone(),
                                    // With -o the listing went to a file.
                                    original: (case.exit_code == 0
                                        && !case.arguments.iter().any(|a| a.starts_with("-o")))
                                    .then(|| case.expected.clone()),
                                    unoptimized: compile(case, &["--mode=oxide", "--opt=none"]),
                                    optimized: compile(case, &["--mode=oxide"]),
                                },
                            ));
                        }
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect()
        });
        listings.sort_by_key(|(index, _)| *index);
        listings.into_iter().map(|(_, listing)| listing).collect()
    })
}

/// Listings the model cannot run: symbolic registers or immediates.
const NOT_EXECUTABLE: [&str; 3] = ["cli-symbolic", "copy-iaddiu-symbol", "expr-unresolved"];

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "compiles the corpus twice; the release suite runs it"
)]
fn optimizations_keep_what_every_program_computes() {
    let mut failures = Vec::new();
    let mut compared = 0;
    for listing in listings() {
        match (&listing.unoptimized, &listing.optimized) {
            (None, None) => {}
            (Some(unoptimized), Some(optimized)) => match compare(unoptimized, optimized) {
                Verdict::Identical => {}
                Verdict::Equivalent(seeds) => compared += seeds,
                verdict => failures.push(format!("{}: {}", listing.name, verdict.describe())),
            },
            _ => failures.push(format!("{}: accepted only with --opt=none", listing.name)),
        }
    }
    eprintln!("{compared} runs of optimized listings matched");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Programs whose oxide-mode code computes something other than the
/// original's, because a fix repairs it (docs/VCL_BUGS.md).
const FIXED: [(&str, &str); 10] = [
    ("openvcl__repros_store_load_alias", "alias"),
    ("openvcl__repros_store_store_alias", "alias"),
    ("checker-mem-copied-base", "alias"),
    ("mem-loop-accumulator", "alias"),
    ("mem-loop-carried-raw", "alias"),
    ("mem-loop-copied-base", "alias"),
    ("mem-loop-same-base", "alias"),
    ("mem-loop-same-base-timid", "alias"),
    ("mem-loop-store-store", "alias"),
    ("loop-entry-q-in-flight", "entry"),
];

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "compiles the corpus twice; the release suite runs it"
)]
fn oxide_mode_computes_what_the_original_computes_except_where_fixed() {
    let mut report = Vec::new();
    let mut compared = 0;
    for listing in listings() {
        let (Some(original), Some(oxide)) = (&listing.original, &listing.unoptimized) else {
            continue;
        };
        let fixed = FIXED.iter().any(|(name, _)| *name == listing.name);
        match compare(original, oxide) {
            Verdict::Identical => {}
            Verdict::Equivalent(seeds) => {
                compared += seeds;
                if fixed {
                    report.push(format!(
                        "{}: now equivalent, remove from FIXED",
                        listing.name
                    ));
                }
            }
            Verdict::Unchecked(_) | Verdict::Different(_)
                if NOT_EXECUTABLE.contains(&listing.name.as_str()) => {}
            Verdict::Different(_) if fixed => {}
            verdict => report.push(format!("{}: {}", listing.name, verdict.describe())),
        }
    }
    eprintln!("{compared} runs of listings that differ from the original's matched");
    assert!(report.is_empty(), "{report:#?}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "compiles the corpus twice; the release suite runs it"
)]
fn the_timed_model_runs_every_listing() {
    let mut report = Vec::new();
    for listing in listings() {
        let Some(text) = &listing.unoptimized else {
            continue;
        };
        let outcome = match Program::parse(text) {
            Ok(program) => (0..SEEDS)
                .map(|seed| run(&program, seed, MAX_STEPS))
                .find(|run| !matches!(run, Run::Unfinished)),
            Err(error) => Some(Run::Failed(format!("parse: {error}"))),
        };
        let executable = !NOT_EXECUTABLE.contains(&listing.name.as_str());
        match outcome {
            Some(Run::Finished(_)) if !executable => {
                report.push(format!(
                    "{}: runs now, remove from NOT_EXECUTABLE",
                    listing.name
                ));
            }
            Some(Run::Finished(_)) => {}
            Some(Run::Failed(_)) if !executable => {}
            outcome => report.push(format!("{}: {outcome:?}", listing.name)),
        }
    }
    assert!(report.is_empty(), "{report:#?}");
}

/// The timed model catches a schedule that reads Q too early, which an
/// interlocked model accepts: the second listing moves the MULQ one row
/// up, before the quotient lands.
#[test]
fn a_read_of_q_before_it_lands_is_a_difference() {
    let row = |upper: &str, lower: &str| format!("         {upper:59}{lower}\n");
    let listing = |early: bool| {
        let mut text = row("NOP", "lq VF01,0(VI00)");
        text += &row("NOP", "lq VF02,1(VI00)");
        text += &row("NOP", "NOP");
        text += &row("NOP", "NOP");
        text += &row("NOP", "div Q,VF01x,VF02x");
        for _ in 0..5 {
            text += &row("NOP", "NOP");
        }
        if early {
            text += &row("mulq VF03,VF01,Q", "NOP");
            text += &row("NOP", "NOP");
        } else {
            text += &row("NOP", "NOP");
            text += &row("mulq VF03,VF01,Q", "NOP");
        }
        for _ in 0..3 {
            text += &row("NOP", "NOP");
        }
        text += &row("NOP[E]", "sq VF03,2(VI00)");
        text + &row("NOP", "NOP")
    };
    let (late, early) = (listing(false), listing(true));
    assert!(matches!(compare(&late, &late), Verdict::Identical));
    assert!(
        matches!(compare(&late, &early), Verdict::Different(ref text) if text.contains("qword 2")),
        "{:?}",
        compare(&late, &early)
    );
    let logical = |text: &str| {
        Program::parse(text)
            .unwrap()
            .execute(seeded_exact_state(), 64)
            .unwrap()
            .state
            .memory
    };
    assert_eq!(logical(&late), logical(&early));
}

/// A state the logical model's exact arithmetic accepts.
fn seeded_exact_state() -> State {
    let mut state = State::default();
    state.memory[0] = [6.0_f32.to_bits(); 4];
    state.memory[1] = [3.0_f32.to_bits(); 4];
    state.q = 0.5_f32.to_bits();
    state
}

/// Cycles each changed listing takes with and without optimizations, per
/// count seed: `cargo test --release --test listing_equivalence
/// cycle_report -- --ignored --nocapture`.
#[test]
#[ignore = "a report, not a check"]
fn cycle_report() {
    let cycles = |text: &str, seed: u64| {
        Program::parse(text)
            .ok()?
            .run(seeded_state(seed), Config::TIMED, MAX_STEPS)
            .ok()
            .map(|execution| execution.cycles)
    };
    println!(
        "{:56} cycles at each count (unoptimized -> optimized)",
        "program"
    );
    for listing in listings() {
        let (Some(unoptimized), Some(optimized)) = (&listing.unoptimized, &listing.optimized)
        else {
            continue;
        };
        if corpus::normalized(unoptimized) == corpus::normalized(optimized) {
            continue;
        }
        let columns: Vec<String> = (RANDOM_SEEDS..SEEDS)
            .map(
                |seed| match (cycles(unoptimized, seed), cycles(optimized, seed)) {
                    (Some(a), Some(b)) => format!("{a}->{b}"),
                    _ => "-".into(),
                },
            )
            .collect();
        println!("{:56} {}", listing.name, columns.join(" "));
    }
}

/// Where an optimized program spends its cycles, by the label each row
/// follows: `VCL_PROFILE=<case> cargo test --release --test
/// listing_equivalence profile_report -- --ignored --nocapture`.
#[test]
#[ignore = "a report, not a check"]
fn profile_report() {
    let Ok(name) = std::env::var("VCL_PROFILE") else {
        return;
    };
    let case = cases().into_iter().find(|case| case.name == name).unwrap();
    let listing = compile(&case, &["--mode=oxide"]).unwrap();
    let program = Program::parse(&listing).unwrap();
    let labels = program.labels();
    let mut totals: std::collections::BTreeMap<String, (u64, u64, u64)> = Default::default();
    let mut all = 0;
    for seed in RANDOM_SEEDS..SEEDS {
        let Ok(execution) = program.run(seeded_state(seed), Config::TIMED, MAX_STEPS) else {
            continue;
        };
        all += execution.cycles;
        for (pc, &(issues, stalls)) in execution.profile.iter().enumerate() {
            let label = labels
                .iter()
                .rev()
                .find(|(at, _)| *at <= pc)
                .map_or("(start)".to_string(), |(_, name)| name.clone());
            let entry = totals.entry(label).or_default();
            entry.0 += issues;
            entry.1 += stalls;
            entry.2 += issues + stalls;
        }
    }
    let mut rows: Vec<_> = totals.into_iter().collect();
    rows.sort_by_key(|(_, (_, _, cycles))| std::cmp::Reverse(*cycles));
    println!("{name}: {all} cycles over the count seeds");
    for (label, (issues, stalls, cycles)) in rows.iter().take(25) {
        println!(
            "{label:48} {cycles:7} cycles {:5.1}%  {issues:7} issues {stalls:6} stalls",
            100.0 * *cycles as f64 / all as f64
        );
    }
}
