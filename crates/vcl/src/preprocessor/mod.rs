//! The preprocessor that `-G` and `-g` run before the parser, the same as
//! vclpp 2's (the user-visible behaviour, `docs/PREPROCESSOR.md`):
//!
//! - standard mode (`-G`): C's directives, `#macro` blocks invoked as
//!   `Name{ args }`, and `#vuprog`/`#endvuprog`;
//! - MASP mode (`-g`): masp's macro language, then C's directives over the
//!   result, as a build runs cpp over masp's output.
//!
//! Its output is VCL source text, with the file and line each output line
//! came from so that the compiler's diagnostics can name them. With
//! [`PreprocessOptions::c_syntax`] it preprocesses VUSL instead, whose
//! frontend reads the output.

mod engine;
mod expander;
pub mod expression;
pub mod lexer;
mod macros;
mod masp;
mod output;
mod source;

pub use source::{FileSystem, MemoryFiles, SourceLoader, normalize};

use crate::diagnostics::{Diagnostic, SourceLocation};

/// Whether `name` is one of the built-in macros, which `--define` cannot
/// define.
pub fn is_builtin_macro(name: &str) -> bool {
    macros::MacroTable::is_builtin_name(name)
}

/// The preprocessor's settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreprocessOptions {
    /// MASP mode (`-g`): masp's directives first, then C's.
    pub masp: bool,
    /// `-I`: directories to look for included files in.
    pub include_dirs: Vec<String>,
    /// `--define=NAME[=VALUE]` outside MASP mode: macros defined before the
    /// source, as `#define NAME VALUE`.
    pub defines: Vec<(String, String)>,
    /// `--define=NAME[=VALUE]` in MASP mode: variables, read as `\&NAME`.
    pub masp_variables: Vec<(String, String)>,
    /// `--comment-char=C` in MASP mode: characters that start a comment, as
    /// `;` does.
    pub comment_chars: String,
    /// `--vcl-boilerplate`: the standard `.init_*` directives and the
    /// `--enter`/`--exit` blocks around the code.
    pub vcl_boilerplate: bool,
    /// `--fold-constants`: integer constant expressions replaced by their
    /// values.
    pub fold_constants: bool,
    /// `--flatten-subscripts`: `name[0]` becomes `name_0`, `name[x]` `namex`.
    pub flatten_subscripts: bool,
    /// `--warn-undef`: a warning for each name an `#if` reads as 0.
    pub warn_undef: bool,
    /// C syntax, for the VUSL frontend (standard mode only):
    ///
    /// - `;` is punctuation, not a comment;
    /// - a `#pragma` other than `#pragma once` goes to the output as its own
    ///   line, with macros in its arguments expanded, for the frontend to
    ///   read;
    /// - a name followed by `{` is code unless it names a block macro;
    /// - `#vuprog` and `#endvuprog` are unknown directives, and their absence
    ///   is not warned about.
    pub c_syntax: bool,
}

/// A source file the preprocessor read, for quoting in diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadFile {
    pub name: String,
    pub text: String,
}

/// The preprocessed program.
#[derive(Clone, Debug)]
pub struct Preprocessed {
    /// The VCL source text.
    pub text: String,
    pub warnings: Vec<Diagnostic>,
    /// Every file read, the main one first.
    pub files: Vec<ReadFile>,
    /// For each line of `text`, the file and line it came from (`None` for
    /// lines the preprocessor writes itself, such as the boilerplate).
    pub line_origins: Vec<Option<SourceLocation>>,
}

/// A preprocessing run that stopped at an error.
#[derive(Clone, Debug)]
pub struct PreprocessFailure {
    pub error: Diagnostic,
    /// The warnings before the error.
    pub warnings: Vec<Diagnostic>,
    pub files: Vec<ReadFile>,
}

/// Preprocesses `text`, the file named `name`, reading the files it
/// includes through `loader`.
pub fn preprocess(
    name: &str,
    text: &str,
    options: &PreprocessOptions,
    loader: &dyn SourceLoader,
) -> Result<Preprocessed, Box<PreprocessFailure>> {
    let mut sources = source::Sources::new(loader, &options.include_dirs);
    let main = sources.add_main(name, text);
    let mut engine = engine::Engine::new(options, sources);
    let result = engine.run(main).and_then(|()| engine.write_output());
    let mut files: Vec<ReadFile> = Vec::new();
    for file in &engine.sources.files {
        if file.path.is_some() && !files.iter().any(|read| read.name == file.name) {
            files.push(ReadFile {
                name: file.name.clone(),
                text: file.text.clone(),
            });
        }
    }
    match result {
        Ok((text, origins)) => {
            let line_origins = origins
                .into_iter()
                .map(|origin| {
                    origin.map(|loc| {
                        SourceLocation::new(engine.sources.file(loc.file).name.clone(), loc.line, 0)
                    })
                })
                .collect();
            Ok(Preprocessed {
                text,
                warnings: engine.warnings,
                files,
                line_origins,
            })
        }
        Err(error) => Err(Box::new(PreprocessFailure {
            error: *error,
            warnings: engine.warnings,
            files,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Message, PreprocessMessage};

    fn c_syntax(files: &[(&str, &str)]) -> Result<Preprocessed, Box<PreprocessFailure>> {
        let mut loader = MemoryFiles::new();
        for (name, text) in files {
            loader.insert(name, *text);
        }
        let options = PreprocessOptions {
            c_syntax: true,
            ..PreprocessOptions::default()
        };
        preprocess(files[0].0, files[0].1, &options, &loader)
    }

    #[test]
    fn c_syntax_keeps_semicolons_and_braces() {
        let source = "struct Frame {\n\
                      \x20   vec4 a; // the first\n\
                      };\n\
                      void main() {\n\
                      \x20   if (a) { b = 1; } else { b = 2; }\n\
                      \x20   do { b; } while (a);\n\
                      }\n";
        let result = c_syntax(&[("main.vusl", source)]).unwrap();
        assert_eq!(
            result.text,
            "struct Frame {\n    vec4 a;\n};\nvoid main() {\n    if (a) { b = 1; } else { b = 2; }\n    do { b; } while (a);\n}\n"
        );
        // No #vuprog is expected.
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn c_syntax_passes_pragmas_through_expanded() {
        let source = "#define COUNT 4\n\
                      #pragma unroll(COUNT)\n\
                      for (;;) {}\n\
                      #if 0\n\
                      #pragma barrier\n\
                      #endif\n\
                      #  pragma program(name) // a comment\n";
        let result = c_syntax(&[("main.vusl", source)]).unwrap();
        assert_eq!(
            result.text,
            "#pragma unroll(4)\nfor (;;) {}\n#  pragma program(name)\n"
        );
        let lines: Vec<usize> = result
            .line_origins
            .iter()
            .map(|origin| origin.as_ref().unwrap().line)
            .collect();
        assert_eq!(lines, [2, 3, 7]);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn c_syntax_keeps_pragma_once() {
        let main = "#include \"frame.vush\"\n\
                    #include \"frame.vush\"\n\
                    use;\n";
        let header = "#pragma once\n\
                      struct Frame {};\n";
        let result = c_syntax(&[("main.vusl", main), ("frame.vush", header)]).unwrap();
        assert_eq!(result.text, "struct Frame {};\nuse;\n");
        let origin = result.line_origins[0].as_ref().unwrap();
        assert_eq!((origin.file.as_str(), origin.line), ("frame.vush", 2));
    }

    #[test]
    fn c_syntax_has_no_vuprog() {
        let failure = c_syntax(&[("main.vusl", "#vuprog name\n")]).unwrap_err();
        assert_eq!(
            failure.error.message,
            Message::Preprocess(PreprocessMessage::UnknownDirective {
                name: "vuprog".into()
            })
        );
    }

    #[test]
    fn c_syntax_still_expands_block_macros() {
        let source = "#macro Twice: v\n\
                      \x20   v;\n\
                      \x20   v;\n\
                      #endmacro\n\
                      Twice{ a }\n";
        let result = c_syntax(&[("main.vusl", source)]).unwrap();
        assert_eq!(result.text, "    a;\n    a;\n");
    }
}
