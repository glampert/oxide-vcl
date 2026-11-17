//! VUSL, the Vector Unit Shading Language: a GLSL-like language for PS2 VU1
//! microprograms (`docs/plans/VUSL.md`).
//!
//! A program goes through the compiler's preprocessor in C syntax, then the
//! frontend checks it and generates VCL source, which [`vcl::compile`]
//! schedules into the listing that dvp-as assembles.

pub mod diagnostics;
mod options;

pub use options::Options;

use vcl::preprocessor::{self, PreprocessFailure, PreprocessOptions, Preprocessed, SourceLoader};

/// The macro every VUSL program sees defined, as 1, so that a header shared
/// with C or VCL can tell which compiler reads it.
pub const PREDEFINED_MACRO: &str = "__VUSL__";

/// Preprocesses `text`, the program in the file named `name`, reading the
/// files it includes through `loader`. The preprocessor runs in C syntax:
/// `;` is punctuation, and `#pragma` lines stay in the output for the
/// frontend.
pub fn preprocess(
    name: &str,
    text: &str,
    options: &Options,
    loader: &dyn SourceLoader,
) -> Result<Preprocessed, Box<PreprocessFailure>> {
    let mut defines = vec![(PREDEFINED_MACRO.to_string(), "1".to_string())];
    defines.extend(options.defines.iter().cloned());
    let preprocess_options = PreprocessOptions {
        include_dirs: options.include_dirs.clone(),
        defines,
        c_syntax: true,
        ..PreprocessOptions::default()
    };
    preprocessor::preprocess(name, text, &preprocess_options, loader)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcl::preprocessor::MemoryFiles;

    #[test]
    fn programs_see_vusl_defined_and_their_own_macros() {
        let source = "#if __VUSL__\n\
                      float k = SCALE;\n\
                      #endif\n";
        let options = Options {
            defines: vec![("SCALE".into(), "2.0".into())],
            ..Options::default()
        };
        let result = preprocess("main.vusl", source, &options, &MemoryFiles::new()).unwrap();
        assert_eq!(result.text, "float k = 2.0;\n");
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }
}
