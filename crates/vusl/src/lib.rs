//! VUSL, the Vector Unit Shading Language: a GLSL-like language for PS2 VU1
//! microprograms (`docs/VUSL.md`, `docs/VUSL_COMPILER.md`).
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
    /// The program, or for a library's own build the library itself.
    pub program: sema::hir::Program,
    /// A library's entries, one per export in entry-table order; none for a
    /// program.
    pub entries: Vec<sema::hir::Program>,
    pub warnings: Vec<Diagnostic>,
    /// The file each of the program's structs was declared in.
    pub struct_files: Vec<String>,
    /// The file each of the program's enums was declared in.
    pub enum_files: Vec<String>,
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

impl Vcl {
    /// The text with a comment before each run of lines from one VUSL
    /// line: its file, its number and its text, from `files`.
    pub fn annotated(&self, files: &[preprocessor::ReadFile]) -> String {
        let mut out = String::new();
        let mut last: Option<(&str, usize)> = None;
        for (index, line) in self.text.lines().enumerate() {
            let origin = self.origins.get(index).and_then(Option::as_ref);
            if let Some(origin) = origin
                && last != Some((origin.file.as_str(), origin.line))
            {
                last = Some((origin.file.as_str(), origin.line));
                let source = files
                    .iter()
                    .find(|file| file.name == origin.file)
                    .and_then(|file| file.text.lines().nth(origin.line.saturating_sub(1)))
                    .map_or("", str::trim);
                out.push_str(&format!(
                    "        ; {}({}): {source}\n",
                    origin.file, origin.line
                ));
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

impl Checked {
    /// The program's reflection; `instructions` once the code exists.
    pub fn reflection(&self, instructions: Option<usize>) -> reflect::Reflection {
        let exports: Vec<String> = self
            .entries
            .iter()
            .filter_map(|entry| entry.entry.as_ref().map(|entry| entry.export.clone()))
            .collect();
        reflect::reflect(
            &self.program,
            &self.source,
            &self.struct_files,
            &self.enum_files,
            &exports,
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

    /// The program's VCL, or for a library every entry's, one after the
    /// other.
    pub fn generate_with(&self, settings: codegen::Settings) -> Result<Vcl, Box<Failure>> {
        if !self.is_library() {
            return self.generate_program(&self.program, settings);
        }
        let mut text = String::new();
        let mut origins = Vec::new();
        for entry in &self.entries {
            let vcl = self.generate_program(entry, settings)?;
            text.push_str(&vcl.text);
            origins.extend(vcl.origins);
        }
        Ok(Vcl { text, origins })
    }

    /// Whether this is a library's own build: entries to compile, no
    /// program.
    pub fn is_library(&self) -> bool {
        !self.entries.is_empty()
    }

    fn generate_program(
        &self,
        program: &sema::hir::Program,
        settings: codegen::Settings,
    ) -> Result<Vcl, Box<Failure>> {
        let locator = self.locator();
        match codegen::generate(program, &self.source, settings) {
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

/// Compiles generated VCL with `vcl`, in `options.mode` and with
/// `options.vcl`'s settings, its generated labels prefixed with the
/// program's name.
pub fn assemble(
    vcl: &Vcl,
    program: &str,
    options: &Options,
) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    let vcl_options = vcl::Options {
        mode: options.mode,
        unique: Some(program.to_string()),
        ..options.vcl.clone()
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

/// How many values vcl kept in the work area for want of registers.
fn spills(compiled: &vcl::Compiled) -> usize {
    compiled
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning.message,
                vcl::Message::Compile(vcl::diagnostics::CompileMessage::RegisterSpilled { .. })
            )
        })
        .count()
}

/// Generates `checked`'s VCL and compiles it with `vcl`. When vcl runs out
/// of registers, or spills values to a `layout(spill)` work area, the VCL
/// is generated again with fewer of the loops' invariants kept in
/// registers across them, and the compile with the fewest spills is kept.
pub fn compile(
    checked: &Checked,
    options: &Options,
) -> Result<(Vcl, vcl::Compiled), Box<CompileFailure>> {
    if checked.is_library() {
        return compile_library(checked, options);
    }
    compile_program(checked, &checked.program, options)
}

/// A library's own build: an entry table, one pair per export and its
/// delay slot, each a branch to the export's code, then every export
/// compiled as a program of its own. Entry `k` so stays at the library's
/// address plus `2k` whatever its code becomes.
fn compile_library(
    checked: &Checked,
    options: &Options,
) -> Result<(Vcl, vcl::Compiled), Box<CompileFailure>> {
    let name = &checked.program.name;
    let mut table = format!(
        "; {name}: a VUSL library of {} exports\n\t\t.vu\n\t\t.align 4\n\t\t.global\t{name}_CodeStart\n\t\t.global\t{name}_CodeEnd\n{name}_CodeStart:\n",
        checked.entries.len()
    );
    let mut text = String::new();
    let mut origins = Vec::new();
    let mut code = String::new();
    let mut compiled = vcl::Compiled::default();
    for entry in &checked.entries {
        let (vcl, entry_compiled) = compile_program(checked, entry, options)?;
        // The table branches to a local label at the export's start: a
        // branch to a global symbol leaves dvp-as a relocation that ld
        // cannot apply ("unsupported relocation").
        let local = format!("{}_Entry", entry.name);
        let jump = format!("{:<14}{local}", "b");
        table.push_str(&format!("{}\n", vcl::emit::pair("NOP", &jump)));
        table.push_str(&format!("{}\n", vcl::emit::pair("NOP", "NOP")));
        text.push_str(&vcl.text);
        origins.extend(vcl.origins);
        // The table opened the listing's one VU section: dvp-as rejects
        // a `.vu` inside it.
        let start = format!("{}_CodeStart:", entry.name);
        for line in entry_compiled.assembly.lines() {
            if line.trim() != ".vu" {
                code.push_str(line);
                code.push('\n');
            }
            if line.trim() == start {
                code.push_str(&local);
                code.push_str(":\n");
            }
        }
        compiled.instruction_pairs += entry_compiled.instruction_pairs;
        compiled.warnings.extend(entry_compiled.warnings);
    }
    compiled.instruction_pairs += 2 * checked.entries.len();
    compiled.assembly = format!("{table}{code}\t\t.align 4\n{name}_CodeEnd:\n");
    Ok((Vcl { text, origins }, compiled))
}

/// `program`'s VCL compiled, as [`compile`] describes.
///
/// The first attempt keeps up to [`HOIST_LIMITS`]`[0]` invariants in
/// registers. When its result keeps values in memory or runs out of
/// registers, the other limits are compiled at the same time, and their
/// results are taken in order as if they had been tried one after another.
fn compile_program(
    checked: &Checked,
    program: &sema::hir::Program,
    options: &Options,
) -> Result<(Vcl, vcl::Compiled), Box<CompileFailure>> {
    let attempt = |hoist_limit| -> Result<Attempt, Box<CompileFailure>> {
        let vcl = checked
            .generate_program(program, codegen::Settings { hoist_limit })
            .map_err(|failure| Box::new(CompileFailure::Generate(failure)))?;
        let result = assemble(&vcl, &program.name, options);
        Ok((vcl, result))
    };
    let mut choice = Choice::default();
    if choice.consider(attempt(HOIST_LIMITS[0])?) {
        let others: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = HOIST_LIMITS[1..]
                .iter()
                .map(|&hoist_limit| scope.spawn(move || attempt(hoist_limit)))
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        });
        for other in others {
            if !choice.consider(other?) {
                break;
            }
        }
    }
    match choice.best {
        Some(compiled) => Ok(compiled),
        None => Err(Box::new(choice.failure.expect("at least one attempt"))),
    }
}

/// One hoist limit's VCL and what vcl made of it.
type Attempt = (Vcl, Result<vcl::Compiled, Vec<vcl::CompileError>>);

/// The attempts considered so far: the result with the fewest values kept in
/// memory, the first of equals, and the last failure.
#[derive(Default)]
struct Choice {
    best: Option<(Vcl, vcl::Compiled)>,
    failure: Option<CompileFailure>,
}

impl Choice {
    /// Takes one attempt into account; false when no later one can be
    /// chosen.
    fn consider(&mut self, (vcl, result): Attempt) -> bool {
        match result {
            Ok(compiled) => {
                let fewer = self
                    .best
                    .as_ref()
                    .is_none_or(|(_, kept)| spills(&compiled) < spills(kept));
                if fewer {
                    self.best = Some((vcl, compiled));
                }
                !self
                    .best
                    .as_ref()
                    .is_some_and(|(_, kept)| spills(kept) == 0)
            }
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
                let done = !out_of_registers || self.best.is_some();
                self.failure = Some(CompileFailure::Vcl { vcl, errors });
                !done
            }
        }
    }
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
            let enum_files = program
                .enums
                .iter()
                .map(|def| locator.locate(def.pos).file)
                .collect();
            Ok(Checked {
                program,
                entries: checking.entries,
                warnings,
                struct_files,
                enum_files,
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
