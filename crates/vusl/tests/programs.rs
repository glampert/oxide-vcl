//! Ports of hand-written corpus programs to VUSL: `tests/vusl/programs/<case>/`
//! holds `input.vusl` and `hand-written`, the name of the corpus case it
//! ports. Both are compiled in oxide mode and run in the VU model, timed,
//! on every scene `scenes.rs` has for the corpus case: the GIF packets must
//! be the hand-written program's, and the cycles no more than its. A port
//! that calls a library (`<case>_lib`, beside the inlined `<case>`) runs with
//! the library (`include/<name>.vusl`) at its address: it trades cycles for
//! size, so it must instead take fewer instructions, the library's
//! included, than the inlined port. The ignored `report` prints the cycles
//! and instructions of each.

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
/// for a port its checked program and the qwords of its work areas.
struct Built {
    program: Program,
    instructions: usize,
    checked: Option<vusl::Checked>,
    /// The instructions of the library a port calls, if it calls one.
    library: Option<usize>,
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
    }
}

/// The listing of `path`, checked and compiled, and its instructions.
fn compile(path: &Path) -> (vusl::Checked, vcl::Compiled) {
    let name = path.to_string_lossy().into_owned();
    let text = std::fs::read_to_string(path).unwrap();
    let options = vusl::Options::default();
    let preprocessed = vusl::preprocess(&name, &text, &options, &FileSystem)
        .unwrap_or_else(|failure| panic!("{name}: {:?}", failure.error));
    let checked = match vusl::check(&name, &preprocessed) {
        Ok(checked) => checked,
        Err(failure) => panic!("{}", failure.error),
    };
    let (_, compiled) = match vusl::compile(&checked, &options) {
        Ok(compiled) => compiled,
        Err(failure) => match *failure {
            vusl::CompileFailure::Generate(failure) => panic!("{}", failure.error),
            vusl::CompileFailure::Vcl { vcl, errors } => panic!("{}\n{errors:?}", vcl.text),
        },
    };
    (checked, compiled)
}

fn ported(case: &Path) -> Built {
    let (checked, compiled) = compile(&case.join("input.vusl"));
    let mut assembly = compiled.assembly.clone();
    let mut library = None;
    // A port that calls a library: the library at its address after it.
    if let Some(lib) = &checked.program.library {
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
    }
    Built {
        program: Program::parse(&assembly).unwrap(),
        instructions: compiled.instruction_pairs,
        checked: Some(checked),
        library,
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
    let ignored = work_areas(checked);
    for (address, (model, reference)) in run.state.memory.iter().zip(&machine.memory).enumerate() {
        if model != reference && !ignored.contains(&address) {
            return Some(format!(
                "qword {address}: compiled {model:x?}, interpreted {reference:x?}"
            ));
        }
    }
    let kicks: Vec<u16> = run
        .state
        .kicks
        .iter()
        .map(|(address, _)| *address)
        .collect();
    (kicks != machine.kicks)
        .then(|| format!("kicks: compiled {kicks:?}, interpreted {:?}", machine.kicks))
}

/// A scene run both ways: the cycles each took, and whether the packets
/// are the same.
struct Run {
    scene: String,
    hand: u64,
    vusl: u64,
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
    for case in &cases {
        let (name, _, vusl, runs) = runs(case);
        assert!(!runs.is_empty(), "{name}: no scenes");
        if let Some(library) = vusl.library {
            let inlined = name
                .strip_suffix("_lib")
                .expect("a library port is <case>_lib");
            let inlined = ported(&case.with_file_name(inlined)).instructions;
            if vusl.instructions + library >= inlined {
                failures.push(format!(
                    "{name}: {} instructions and a library of {library}, the inlined port {inlined}",
                    vusl.instructions
                ));
            }
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
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "prints the cycles of each port; run with --ignored --nocapture"]
fn report() {
    for case in cases() {
        let (name, hand, vusl, runs) = runs(&case);
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
}
