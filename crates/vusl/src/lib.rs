//! VUSL, the Vector Unit Shading Language: a GLSL-like language for PS2 VU1
//! microprograms (`docs/plans/VUSL.md`).
//!
//! A program goes through the compiler's preprocessor in C syntax, then the
//! frontend checks it and generates VCL source, which [`vcl::compile`]
//! schedules into the listing that dvp-as assembles.

pub mod codegen;
pub mod diagnostics;
pub mod interp;
mod options;
pub mod reflect;
pub mod sema;
pub mod syntax;

pub use options::Options;

use vcl::diagnostics::{Diagnostic, Note, SourceLocation};
use vcl::preprocessor::{self, PreprocessFailure, PreprocessOptions, Preprocessed, SourceLoader};

use diagnostics::VuslMessage;
use sema::Problem;
use syntax::Pos;

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

/// A checked program and its warnings, located in the source files.
pub struct Checked {
    pub program: sema::hir::Program,
    pub warnings: Vec<Diagnostic>,
    /// The file each of the program's structs was declared in.
    pub struct_files: Vec<String>,
    /// The file the program was read from.
    pub source: String,
    /// Where each line of the preprocessed program came from.
    origins: Vec<Option<SourceLocation>>,
}

/// The VCL source generated for a program, with the place in the VUSL
/// source each line came from.
pub struct Vcl {
    pub text: String,
    pub origins: Vec<Option<SourceLocation>>,
}

impl Checked {
    /// The program's reflection; `instructions` once the code exists.
    pub fn reflection(&self, instructions: Option<usize>) -> reflect::Reflection {
        reflect::reflect(
            &self.program,
            &self.source,
            &self.struct_files,
            instructions,
        )
    }

    fn locator(&self) -> Locator<'_> {
        Locator {
            main: &self.source,
            origins: &self.origins,
        }
    }

    /// Generates the program's VCL source.
    pub fn generate(&self) -> Result<Vcl, Box<Failure>> {
        self.generate_with(codegen::Settings::default())
    }

    pub fn generate_with(&self, settings: codegen::Settings) -> Result<Vcl, Box<Failure>> {
        let locator = self.locator();
        match codegen::generate(&self.program, &self.source, settings) {
            Ok(generated) => Ok(Vcl {
                text: generated.text,
                origins: generated
                    .origins
                    .into_iter()
                    .map(|pos| pos.map(|pos| locator.locate(pos)))
                    .collect(),
            }),
            Err(problem) => Err(Box::new(Failure {
                error: locator.diagnostic(true, &problem),
                warnings: Vec::new(),
            })),
        }
    }
}

/// Compiles generated VCL with `vcl`, in `options.mode`, its generated
/// labels prefixed with the program's name.
pub fn assemble(
    vcl: &Vcl,
    program: &str,
    options: &Options,
) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    let vcl_options = vcl::Options {
        mode: options.mode,
        unique: Some(program.to_string()),
        ..vcl::Options::default()
    };
    vcl::compile(&vcl.text, &format!("{program}.vcl"), &vcl_options)
}

/// Why a checked program did not compile.
pub enum CompileFailure {
    /// Its code could not be generated.
    Generate(Box<Failure>),
    /// vcl rejected the generated VCL.
    Vcl {
        vcl: Vcl,
        errors: Vec<vcl::CompileError>,
    },
}

/// How many loop invariants lowering may keep in registers, in the order
/// tried: fewer when the registers do not fit.
const HOIST_LIMITS: [usize; 3] = [16, 6, 0];

/// Generates `checked`'s VCL and compiles it with `vcl`. When vcl runs out
/// of registers, the VCL is generated again with fewer of the loops'
/// invariants kept in registers across them.
pub fn compile(
    checked: &Checked,
    options: &Options,
) -> Result<(Vcl, vcl::Compiled), Box<CompileFailure>> {
    let mut failure = None;
    for hoist_limit in HOIST_LIMITS {
        let vcl = checked
            .generate_with(codegen::Settings { hoist_limit })
            .map_err(|failure| Box::new(CompileFailure::Generate(failure)))?;
        match assemble(&vcl, &checked.program.name, options) {
            Ok(compiled) => return Ok((vcl, compiled)),
            Err(errors) => {
                let out_of_registers = errors.iter().any(|error| {
                    matches!(
                        error.message,
                        vcl::Message::Compile(
                            vcl::diagnostics::CompileMessage::AllocationPoolFull { .. }
                                | vcl::diagnostics::CompileMessage::WorkAreaFull { .. }
                        )
                    )
                });
                let done = !out_of_registers;
                failure = Some(CompileFailure::Vcl { vcl, errors });
                if done {
                    break;
                }
            }
        }
    }
    Err(Box::new(failure.expect("at least one attempt")))
}

/// Why a program did not check: the first error, and the warnings before
/// it.
pub struct Failure {
    pub error: Diagnostic,
    pub warnings: Vec<Diagnostic>,
}

/// Where the lines of the preprocessed program came from.
struct Locator<'a> {
    main: &'a str,
    origins: &'a [Option<SourceLocation>],
}

impl Locator<'_> {
    fn locate(&self, pos: Pos) -> SourceLocation {
        let line = pos.line as usize;
        let column = pos.column as usize;
        let origin = line
            .checked_sub(1)
            .and_then(|index| self.origins.get(index))
            .and_then(Option::as_ref);
        match origin {
            Some(origin) => SourceLocation::new(origin.file.clone(), origin.line, column),
            None => SourceLocation::new(self.main, line, column),
        }
    }

    fn diagnostic(&self, error: bool, problem: &Problem) -> Diagnostic {
        let diagnostic = if error {
            Diagnostic::error(problem.message.clone())
        } else {
            Diagnostic::warning(problem.message.clone())
        };
        let notes = problem
            .notes
            .iter()
            .map(|(pos, message)| Note::new(Some(self.locate(*pos)), message.clone()))
            .collect();
        diagnostic
            .located_at(self.locate(problem.pos))
            .with_notes(notes)
    }
}

/// The name a program without `#pragma program(name)` gets from its file:
/// the stem, with anything but letters, digits and `_` as `_`.
pub fn default_program_name(file: &str) -> String {
    let stem = std::path::Path::new(file).file_stem().map_or_else(
        || file.to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match name.chars().next() {
        Some(first) if !first.is_ascii_digit() => name,
        _ => format!("_{name}"),
    }
}

/// Checks the preprocessed program from the file named `name`: parses it,
/// lays out its structs, binds `main`'s parameters and elaborates `main`.
pub fn check(name: &str, preprocessed: &Preprocessed) -> Result<Checked, Box<Failure>> {
    let locator = Locator {
        main: name,
        origins: &preprocessed.line_origins,
    };
    let file = match syntax::parse(&preprocessed.text) {
        Ok(file) => file,
        Err(failure) => {
            let message = match &failure {
                syntax::SyntaxFailure::Lex(error) => VuslMessage::Lex(error.kind.clone()),
                syntax::SyntaxFailure::Parse(error) => VuslMessage::Syntax(error.kind.clone()),
            };
            let problem = Problem {
                message,
                pos: failure.pos(),
                notes: Vec::new(),
            };
            return Err(Box::new(Failure {
                error: locator.diagnostic(true, &problem),
                warnings: Vec::new(),
            }));
        }
    };
    let checking = sema::check(&file, &default_program_name(name));
    let warnings = checking
        .warnings
        .iter()
        .map(|problem| locator.diagnostic(false, problem))
        .collect();
    match checking.program {
        Ok(program) => {
            let struct_files = program
                .structs
                .iter()
                .map(|def| locator.locate(def.pos).file)
                .collect();
            Ok(Checked {
                program,
                warnings,
                struct_files,
                source: name.to_string(),
                origins: preprocessed.line_origins.clone(),
            })
        }
        Err(problem) => Err(Box::new(Failure {
            error: locator.diagnostic(true, &problem),
            warnings,
        })),
    }
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

    #[test]
    fn programs_are_named_after_their_file() {
        assert_eq!(default_program_name("src/world_lm.vusl"), "world_lm");
        assert_eq!(default_program_name("a-b.c.vusl"), "a_b_c");
        assert_eq!(default_program_name("<stdin>"), "_stdin_");
        assert_eq!(default_program_name("3d.vusl"), "_3d");
    }
}
