//! The preprocessor that `-G` and `-g` run before the parser, the same as
//! vclpp 2's (the user-visible behaviour, `docs/PREPROCESSOR.md`):
//!
//! - standard mode (`-G`): C's directives, `#macro` blocks invoked as
//!   `Name{ args }`, and `#vuprog`/`#endvuprog`;
//! - MASP mode (`-g`): masp's macro language, then C's directives over the
//!   result, as a build runs cpp over masp's output.
//!
//! Its output is VCL source text, with the file and line each output line
//! came from so that the compiler's diagnostics can name them.

mod engine;
mod expander;
pub mod expression;
pub mod lexer;
mod macros;
mod masp;
mod output;
mod source;

pub use source::{FileSystem, SourceLoader, normalize};

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
