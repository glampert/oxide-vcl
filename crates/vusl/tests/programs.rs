//! The VUSL programs of quake2-ps2 and doom3-bfg-ps2, each against the
//! hand-written corpus program it replaced: `tests/vusl/programs/<case>/`
//! holds `input.vusl`, a copy of the project's source, and `hand-written`,
//! the name of the corpus case; `include` holds the files they include
//! (`tests/vusl/programs/README.md` gives the commits). Both are compiled in oxide mode and
//! run in the VU model, timed, on every scene `scenes.rs` has for the corpus
//! case: the GIF packets must be the hand-written program's, and the cycles
//! no more than its. A program that calls a library runs with the library
//! (`include/<name>.vusl`) at its address: it trades cycles for size, so the
//! library's callers must instead take fewer instructions together, with
//! their one library, than their hand-written programs together. The ignored
//! `report` prints the cycles and instructions of each.

#[path = "../../oxide-vcl/tests/support/bench.rs"]
mod bench;
#[path = "../../oxide-vcl/tests/support/scenes.rs"]
#[allow(dead_code)]
mod scenes;
#[path = "../../oxide-vcl/tests/support/vu_model.rs"]
mod vu_model;

use std::path::{Path, PathBuf};

use vcl::preprocessor::FileSystem;
use vu_model::{Config, Program};

/// The provenance header of a public corpus input ends with this line.
const HEADER_END: &str = "; -- end of corpus header --";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(root().join("tests/vusl/programs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("input.vusl").is_file())
        .collect();
    cases.sort();
    cases
}

/// A program compiled both ways: its listing, its instruction pairs, and
/// for a VUSL program what the interpreter runs: the checked program, with
/// the source of the library it calls.
struct Built {
    program: Program,
    instructions: usize,
    checked: Option<vusl::Checked>,
    /// The instructions of the library a port calls, if it calls one.
    library: Option<usize>,
    /// What the compile did, and the library's compile.
    stats: vcl::stats::Stats,
    library_stats: Option<vcl::stats::Stats>,
}

fn hand_written(corpus: &str) -> Built {
    let directory = root().join("tests/corpus").join(corpus);
    let input = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
    let source = match input.find(HEADER_END) {
        Some(end) => &input[end + HEADER_END.len()..],
        None => &input,
    };
    let source = source.strip_prefix('\n').unwrap_or(source);
    let options = vcl::Options {
        mode: vcl::Mode::Oxide,
        unique: Some("CORPUS".into()),
        ..vcl::Options::default()
    };
    let compiled = vcl::compile(source, &format!("{corpus}.vcl"), &options)
        .unwrap_or_else(|errors| panic!("{corpus}: {errors:?}"));
    Built {
        program: Program::parse(&compiled.assembly).unwrap(),
        instructions: compiled.instruction_pairs,
        checked: None,
        library: None,
        stats: compiled.stats,
        library_stats: None,
    }
}

fn options() -> vusl::Options {
    vusl::Options {
        include_dirs: vec![
            root()
                .join("tests/vusl/programs/include")
                .to_string_lossy()
                .into_owned(),
        ],
        ..vusl::Options::default()
    }
}

/// `text`, the source of `path`, preprocessed and checked.
fn check(path: &Path, text: &str) -> vusl::Checked {
    let name = path.to_string_lossy().into_owned();
    let preprocessed = vusl::preprocess(&name, text, &options(), &FileSystem)
        .unwrap_or_else(|failure| panic!("{name}: {:?}", failure.error));
    match vusl::check(&name, &preprocessed) {
        Ok(checked) => checked,
        Err(failure) => panic!("{}", failure.error),
    }
}

/// The listing of `path`, checked and compiled, and its instructions.
fn compile(path: &Path) -> (vusl::Checked, vcl::Compiled) {
    let checked = check(path, &std::fs::read_to_string(path).unwrap());
    let (_, compiled) = match vusl::compile(&checked, &options()) {
        Ok(compiled) => compiled,
        Err(failure) => match *failure {
            vusl::CompileFailure::Generate(failure) => panic!("{}", failure.error),
            vusl::CompileFailure::Vcl { vcl, errors } => panic!("{}\n{errors:?}", vcl.text),
        },
    };
    (checked, compiled)
}

fn ported(case: &Path) -> Built {
    let input = case.join("input.vusl");
    let (mut checked, compiled) = compile(&input);
    let mut assembly = compiled.assembly.clone();
    let mut library = None;
    let mut library_stats = None;
    // A program that calls a library: the library at its address after it.
    // The program includes only the library's header, and the interpreter
    // runs a call as the export's definition, so it runs the program with
    // the library's source included after it.
    if let Some(lib) = checked.program.library.clone() {
        let path = root()
            .join("tests/vusl/programs/include")
            .join(format!("{}.vusl", lib.name));
        let (_, code) = compile(&path);
        let pairs = Program::parse(&assembly).unwrap().pair_count();
        let address = usize::from(lib.address);
        assert!(
            pairs <= address,
            "{}: {pairs} pairs reach the library",
            lib.name
        );
        let padding = format!("{:9}{:<59}NOP\n", "", "NOP").repeat(address - pairs);
        assembly = format!("{assembly}\n{padding}{}", code.assembly);
        library = Some(code.instruction_pairs);
        library_stats = Some(code.stats);
        let text = std::fs::read_to_string(&input).unwrap();
        checked = check(&input, &format!("{text}\n#include \"{}.vusl\"\n", lib.name));
    }
    Built {
        program: Program::parse(&assembly).unwrap(),
        instructions: compiled.instruction_pairs,
        checked: Some(checked),
        library,
        stats: compiled.stats,
        library_stats,
    }
}

/// The qwords of a program's layout(spill) parameters, and of the library
/// it calls its saved registers, which only the compiled code writes.
fn work_areas(checked: &vusl::Checked) -> Vec<usize> {
    let program = &checked.program;
    let save = program
        .library
        .as_ref()
        .and_then(|library| library.save.clone());
    let mut qwords = Vec::new();
    for param in &program.params {
        let saves = param.library && Some(&param.name) == save.as_ref();
        if let (
            true,
            vusl::sema::hir::ParamPlacement::Address(address),
            vusl::sema::types::Type::Struct(id),
        ) = (param.spill || saves, param.placement, &param.ty)
        {
            let size = program.structs[*id].size.fixed;
            qwords.extend(address as usize..(address + size) as usize);
        }
    }
    qwords
}

/// What the interpreter makes of the port on a scene, against the
/// compiled port's run: the first difference.
fn interpreted(built: &Built, scene: &scenes::Scene, run: &vu_model::Execution) -> Option<String> {
    let checked = built.checked.as_ref()?;
    let state = &scene.state;
    let mut machine = vusl::interp::Machine::new(state.memory.clone(), state.top, state.itop);
    if let Err(error) = vusl::interp::run(&checked.program, &mut machine, 10_000_000) {
        return Some(format!("the interpreter: {error}"));
    }
    // The kicks first: a program that kicks differently differs in memory
    // too, and the kicks say where.
    let kicks: Vec<u16> = run
        .state
        .kicks
        .iter()
        .map(|(address, _)| *address)
        .collect();
    if kicks != machine.kicks {
        return Some(format!(
            "kicks: compiled {kicks:?}, interpreted {:?}",
            machine.kicks
        ));
    }
    let ignored = work_areas(checked);
    run.state
        .memory
        .iter()
        .zip(&machine.memory)
        .enumerate()
        .find(|(address, (model, reference))| model != reference && !ignored.contains(address))
        .map(|(address, (model, reference))| {
            format!("qword {address}: compiled {model:x?}, interpreted {reference:x?}")
        })
}

/// A scene run both ways: the cycles each took, and whether the packets
/// are the same.
struct Run {
    scene: String,
    hand: u64,
    vusl: u64,
    /// The stall cycles of each.
    hand_stalls: u64,
    vusl_stalls: u64,
    same: bool,
    drawn: usize,
    expected: usize,
    /// Where the compiled port and the interpreter disagree, if they do.
    mismatch: Option<String>,
}

fn runs(case: &Path) -> (String, Built, Built, Vec<Run>) {
    let corpus = std::fs::read_to_string(case.join("hand-written")).unwrap();
    let corpus = corpus.trim().to_string();
    let hand = hand_written(&corpus);
    let vusl = ported(case);
    let mut runs = Vec::new();
    for scene in scenes::scenes(&corpus) {
        let a = hand
            .program
            .run(scene.state.clone(), Config::TIMED, 2_000_000)
            .unwrap_or_else(|error| panic!("{corpus} {}: hand-written: {error}", scene.name));
        let b = vusl
            .program
            .run(scene.state.clone(), Config::TIMED, 2_000_000)
            .unwrap_or_else(|error| panic!("{corpus} {}: VUSL: {error}", scene.name));
        runs.push(Run {
            scene: scene.name.clone(),
            hand: a.cycles,
            vusl: b.cycles,
            hand_stalls: a.profile.iter().map(|&(_, stall)| stall).sum(),
            vusl_stalls: b.profile.iter().map(|&(_, stall)| stall).sum(),
            same: a.state.kicks == b.state.kicks,
            drawn: scenes::drawn_primitives(&b.state.kicks),
            expected: scene.primitives,
            mismatch: interpreted(&vusl, &scene, &b),
        });
    }
    let name = case.file_name().unwrap().to_string_lossy().into_owned();
    (name, hand, vusl, runs)
}

#[test]
fn ports_draw_the_same_and_are_no_slower() {
    let cases = cases();
    assert!(!cases.is_empty());
    let mut failures = Vec::new();
    // The library's callers' instructions, their library's, and their
    // hand-written programs'.
    let (mut shared, mut library_size, mut hand_size) = (0, 0, 0);
    for case in &cases {
        let (name, hand, vusl, runs) = runs(case);
        assert!(!runs.is_empty(), "{name}: no scenes");
        if let Some(library) = vusl.library {
            shared += vusl.instructions;
            library_size = library_size.max(library);
            hand_size += hand.instructions;
        }
        for run in runs {
            if !run.same {
                failures.push(format!("{name} {}: the packets differ", run.scene));
            }
            if let Some(mismatch) = &run.mismatch {
                failures.push(format!("{name} {}: {mismatch}", run.scene));
            }
            if run.vusl > run.hand && vusl.library.is_none() {
                failures.push(format!(
                    "{name} {}: {} cycles, the hand-written program {}",
                    run.scene, run.vusl, run.hand
                ));
            }
        }
    }
    if library_size > 0 && shared + library_size >= hand_size {
        failures.push(format!(
            "the library's callers take {shared} instructions and their library {library_size}, their hand-written programs {hand_size}"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "prints the cycles of each port; run with --ignored --nocapture"]
fn report() {
    // The micro memory of the library's callers and their one library, and
    // of their hand-written programs.
    let (mut shared, mut library_size, mut hand_size) = (0, 0, 0);
    for case in cases() {
        let (name, hand, vusl, runs) = runs(&case);
        if let Some(size) = vusl.library {
            shared += vusl.instructions;
            library_size = library_size.max(size);
            hand_size += hand.instructions;
        }
        let library = vusl
            .library
            .map(|pairs| format!(" and a library of {pairs}"))
            .unwrap_or_default();
        println!(
            "{name}: {} instructions{library}, hand-written {}",
            vusl.instructions, hand.instructions
        );
        for run in runs {
            println!(
                "  {:<12} {:>6} cycles, hand-written {:>6} ({:+.1}%), drawn {}/{}, packets {}",
                run.scene,
                run.vusl,
                run.hand,
                (run.vusl as f64 / run.hand as f64 - 1.0) * 100.0,
                run.drawn,
                run.expected,
                if run.same { "identical" } else { "DIFFER" }
            );
            if let Some(mismatch) = run.mismatch {
                println!("    against the interpreter: {mismatch}");
            }
        }
    }
    if library_size > 0 {
        println!(
            "the library's callers: {} instructions with their library of {library_size}, hand-written {hand_size}",
            shared + library_size
        );
    }
}

/// The scenes benchmark (docs/TESTING.md, "Benchmarks"): each port and its
/// hand-written program, with their size and what their compiles did, and
/// their cycles and stall cycles on each scene, in `tests/bench/scenes.txt`:
/// `cargo test --release -p vusl --test programs bench -- --ignored
/// --nocapture`, and `BENCH_UPDATE=1` to write the table.
#[test]
#[ignore = "a benchmark: run it with --ignored"]
fn bench() {
    use bench::Row;
    let measured: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = cases()
            .into_iter()
            .map(|case| scope.spawn(move || runs(&case)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    let mut rows = Vec::new();
    let mut libraries = Vec::new();
    for (name, hand, vusl, runs) in &measured {
        for (side, built) in [("vusl", vusl), ("hand", hand)] {
            let row = Row::new(&[side, name, "compile"]).value("pairs", built.instructions as u64);
            rows.push(bench::work(row, &built.stats));
        }
        if let (Some(pairs), Some(stats)) = (vusl.library, vusl.library_stats)
            && libraries.is_empty()
        {
            let row = Row::new(&["vusl", "library", "compile"]).value("pairs", pairs as u64);
            libraries.push(bench::work(row, &stats));
        }
        for run in runs {
            for (side, cycles, stalls) in [
                ("vusl", run.vusl, run.vusl_stalls),
                ("hand", run.hand, run.hand_stalls),
            ] {
                let scene = run.scene.replace(' ', "_");
                rows.push(
                    Row::new(&[side, name, &scene])
                        .value("cycles", cycles)
                        .value("stalls", stalls),
                );
            }
        }
    }
    rows.extend(libraries);
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    let heading = "\
The VUSL ports (vusl) and their hand-written programs (hand), compiled in
oxide mode: instruction pairs and the compile's work (search nodes,
wrangler steps and checks, flow analyses and the blocks they walk), then
cycles and stall cycles on each scene. A port's compile counts every hoist
attempt. Written by crates/vusl/tests/programs.rs (bench).";
    bench::compare_or_update(&root().join("tests/bench/scenes.txt"), heading, &rows);
}

/// Where a program spends its cycles on each scene, by the label each row
/// follows, the library's included: `VUSL_PROFILE=<case> cargo test --release
/// -p vusl --test programs profile -- --ignored --nocapture`.
#[test]
#[ignore = "a report, not a check"]
fn profile() {
    let Ok(name) = std::env::var("VUSL_PROFILE") else {
        return;
    };
    let case = root().join("tests/vusl/programs").join(&name);
    let corpus = std::fs::read_to_string(case.join("hand-written")).unwrap();
    let built = ported(&case);
    let labels = built.program.labels();
    for scene in scenes::scenes(corpus.trim()) {
        let execution = built
            .program
            .run(scene.state.clone(), Config::TIMED, 2_000_000)
            .unwrap();
        let mut totals: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
        for (pc, &(issues, stalls)) in execution.profile.iter().enumerate() {
            let label = labels
                .iter()
                .rev()
                .find(|(at, _)| *at <= pc)
                .map_or("(start)".to_string(), |(_, label)| label.clone());
            let entry = totals.entry(label).or_default();
            entry.0 += issues;
            entry.1 += stalls;
        }
        let mut rows: Vec<_> = totals.into_iter().filter(|(_, (i, s))| i + s > 0).collect();
        rows.sort_by_key(|(_, (issues, stalls))| std::cmp::Reverse(issues + stalls));
        println!("{} {}: {} cycles", name, scene.name, execution.cycles);
        for (label, (issues, stalls)) in rows.iter().take(12) {
            println!(
                "    {label:52} {:6} cycles {:5.1}% ({issues} issues, {stalls} stalls)",
                issues + stalls,
                100.0 * (issues + stalls) as f64 / execution.cycles as f64
            );
        }
    }
}
