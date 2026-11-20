//! Every complete program in `docs/VUSL.md` (a `glsl` block with a `main`)
//! checks and compiles. A block whose first line is `// name.vush` is a
//! header the programs after it may include.

use vcl::preprocessor::MemoryFiles;

#[test]
fn the_reference_programs_compile() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/VUSL.md");
    let text = std::fs::read_to_string(path).unwrap();
    let mut programs = 0;
    let mut headers = MemoryFiles::new();
    for (index, block) in text.split("```glsl\n").skip(1).enumerate() {
        let source = block.split("```").next().unwrap();
        if let Some(name) = source
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("// "))
            .filter(|name| name.ends_with(".vush"))
        {
            headers.insert(name, source);
            continue;
        }
        if !source.contains("void main") {
            continue;
        }
        programs += 1;
        let name = format!("example{index}.vusl");
        let options = vusl::Options::default();
        let preprocessed = vusl::preprocess(&name, source, &options, &headers).unwrap();
        let checked = vusl::check(&name, &preprocessed)
            .unwrap_or_else(|failure| panic!("{source}\n{}", failure.error));
        assert!(
            vusl::compile(&checked, &options).is_ok(),
            "{source}\ndoes not compile"
        );
    }
    assert!(programs >= 3, "found only {programs} programs");
}
