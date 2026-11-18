//! Ports of hand-written corpus programs to VUSL: `tests/vusl/programs/<case>/`
//! holds `input.vusl` and `hand-written`, the name of the corpus case it
//! ports. Both are compiled in oxide mode and run in the VU model, timed,
//! on every scene `scenes.rs` has for the corpus case: the GIF packets must
//! be the hand-written program's, and the cycles no more than its. The
//! ignored `report` prints the cycles and instructions of each.

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

/// A program compiled both ways: its listing, its instruction pairs.
struct Built {
    program: Program,
    instructions: usize,
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
    }
}

fn ported(case: &Path) -> Built {
    let path = case.join("input.vusl");
    let name = path.to_string_lossy().into_owned();
    let text = std::fs::read_to_string(&path).unwrap();
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
    Built {
        program: Program::parse(&compiled.assembly).unwrap(),
        instructions: compiled.instruction_pairs,
    }
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
        let (name, _, _, runs) = runs(case);
        assert!(!runs.is_empty(), "{name}: no scenes");
        for run in runs {
            if !run.same {
                failures.push(format!("{name} {}: the packets differ", run.scene));
            }
            if run.vusl > run.hand {
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
        println!(
            "{name}: {} instructions, hand-written {}",
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
        }
    }
}
