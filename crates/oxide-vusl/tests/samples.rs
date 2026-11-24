//! The VUSL programs of the samples in `samples/` compile as their Makefiles
//! compile them: `oxide-vusl --warnings-as-errors --header=... -o...`, a
//! listing and a header each. The samples' C++ needs the PS2 SDK, so it is
//! built only where one is installed; this keeps their VU programs checked
//! everywhere.

use std::path::{Path, PathBuf};
use std::process::Command;

fn samples() -> Vec<PathBuf> {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples"));
    let mut programs: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| {
            let directory = entry.unwrap().path();
            let name = directory.file_name()?.to_str()?.to_string();
            let program = directory.join(format!("{name}.vusl"));
            program.exists().then_some(program)
        })
        .collect();
    programs.sort();
    programs
}

#[test]
fn every_sample_program_compiles_with_its_header() {
    let programs = samples();
    assert!(
        programs.len() >= 2,
        "found only {} sample programs",
        programs.len()
    );
    let output = std::env::temp_dir().join(format!("oxide-vusl-samples-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    for program in &programs {
        let stem = program.file_stem().unwrap().to_str().unwrap();
        let listing = output.join(format!("{stem}.vsm"));
        let header = output.join(format!("{stem}.vusl.h"));
        let run = Command::new(env!("CARGO_BIN_EXE_oxide-vusl"))
            .arg("--warnings-as-errors")
            .arg(format!("--header={}", header.display()))
            .arg(format!("-o{}", listing.display()))
            .arg(program)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{} does not compile:\n{}",
            program.display(),
            String::from_utf8_lossy(&run.stderr)
        );
        let listing = std::fs::read_to_string(&listing).unwrap();
        assert!(listing.contains("_CodeStart:"), "{stem}: no program start");
        let header = std::fs::read_to_string(&header).unwrap();
        assert!(header.contains("kInstructions"), "{stem}: no header");
    }
    std::fs::remove_dir_all(&output).unwrap();
}
