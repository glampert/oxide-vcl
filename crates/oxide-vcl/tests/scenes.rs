//! The renderer scenes (`support/scenes.rs`, docs/TESTING.md "Scenes"):
//! the original's recorded listing must draw each scene's triangles, and
//! oxide mode's listings, with and without optimizations, must leave the
//! same memory and send the same GIF packets.

#[path = "support/corpus.rs"]
mod corpus;
#[path = "support/scenes.rs"]
mod scenes;
#[path = "support/vu_model.rs"]
mod vu_model;

use std::path::PathBuf;
use vcl_cli as cli;
use vu_model::{Config, Execution, Program};

const MAX_STEPS: usize = 2_000_000;

fn case(name: &str) -> corpus::Case {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    corpus::load(&root.join("tests/corpus").join(name))
}

fn compile(case: &corpus::Case, extra: &[&str]) -> String {
    let flags = case
        .arguments
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied());
    let options = cli::parse(flags).unwrap().compile;
    vcl::compile(&case.source, &case.path, &options)
        .unwrap()
        .assembly
}

fn run(listing: &str, scene: &scenes::Scene) -> Execution {
    Program::parse(listing)
        .unwrap()
        .run(scene.state.clone(), Config::TIMED, MAX_STEPS)
        .unwrap_or_else(|error| panic!("{}: {error}", scene.name))
}

const CASES: [&str; 4] = [
    "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_strips",
    "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_lm",
    "quake2-ps2__src_ps2_renderer_vu1progs_textured_triangles",
    "quake2-ps2__src_ps2_renderer_vu1progs_particles",
];

#[test]
fn the_original_draws_each_scene() {
    for name in CASES {
        let case = case(name);
        for scene in scenes::scenes(name) {
            let execution = run(&case.expected, &scene);
            let drawn = scenes::drawn_primitives(&execution.state.kicks);
            println!(
                "{name} {}: {} kicks, {drawn} primitives, {} cycles",
                scene.name,
                execution.state.kicks.len(),
                execution.cycles
            );
            assert_eq!(drawn, scene.primitives, "{name} {}", scene.name);
            assert_eq!(
                scenes::off_window(&execution.state.kicks),
                0,
                "{name} {}: drawn vertices outside the GS window",
                scene.name
            );
        }
    }
}

#[test]
fn optimizations_keep_what_each_scene_draws() {
    for name in CASES {
        let case = case(name);
        let unoptimized = compile(&case, &["--mode=oxide", "--opt=none"]);
        let optimized = compile(&case, &["--mode=oxide"]);
        for scene in scenes::scenes(name) {
            let (before, after) = (run(&unoptimized, &scene), run(&optimized, &scene));
            assert_eq!(
                after.state.memory, before.state.memory,
                "{name} {}",
                scene.name
            );
            assert_eq!(
                after.state.kicks, before.state.kicks,
                "{name} {}",
                scene.name
            );
            assert_eq!(
                scenes::drawn_primitives(&after.state.kicks),
                scene.primitives,
                "{name} {}",
                scene.name
            );
            println!(
                "{name} {}: {} -> {} cycles",
                scene.name, before.cycles, after.cycles
            );
        }
    }
}

/// Where a renderer spends its cycles on each scene, by the label each row
/// follows, before and after oxide mode's optimizations: `VCL_PROFILE=<case>
/// cargo test --release --test scenes scene_profile -- --ignored --nocapture`.
#[test]
#[ignore = "a report, not a check"]
fn scene_profile() {
    let Ok(name) = std::env::var("VCL_PROFILE") else {
        return;
    };
    let case = case(&name);
    for (side, flags) in [
        ("unoptimized", &["--mode=oxide", "--opt=none"][..]),
        ("optimized", &["--mode=oxide"]),
    ] {
        let listing = compile(&case, flags);
        let program = Program::parse(&listing).unwrap();
        let labels = program.labels();
        for scene in scenes::scenes(&name) {
            let execution = run(&listing, &scene);
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
            println!("{side} {}: {} cycles", scene.name, execution.cycles);
            for (label, (issues, stalls)) in rows.iter().take(8) {
                println!(
                    "    {label:44} {:6} cycles {:5.1}% ({issues} issues, {stalls} stalls)",
                    issues + stalls,
                    100.0 * (issues + stalls) as f64 / execution.cycles as f64
                );
            }
        }
    }
}
