//! Checking a parsed program: its declarations and struct layouts, `main`'s
//! parameters bound in memory, then `main`'s body elaborated with every
//! call inlined into the checked program ([`hir::Program`]).

mod check;
mod consteval;
mod elaborate;
pub mod hir;
pub mod layout;
pub mod types;

pub use check::DATA_MEMORY_QWORDS;

use crate::diagnostics::VuslMessage;
use crate::syntax::Pos;
use crate::syntax::ast;

/// A message at a place in the preprocessed program, with its notes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Problem {
    pub message: VuslMessage,
    pub pos: Pos,
    pub notes: Vec<(Pos, VuslMessage)>,
}

impl Problem {
    pub fn with_note(mut self, pos: Pos, message: VuslMessage) -> Self {
        self.notes.push((pos, message));
        self
    }
}

pub(crate) fn problem(message: VuslMessage, pos: Pos) -> Problem {
    Problem {
        message,
        pos,
        notes: Vec::new(),
    }
}

/// A checker's result: the value, or the first error found.
pub(crate) type Checked<T> = Result<T, Box<Problem>>;

/// What checking found: the program, or the error that stopped it, and the
/// warnings before.
pub struct Checking {
    pub program: Result<hir::Program, Box<Problem>>,
    pub warnings: Vec<Problem>,
}

/// Checks `file`. A program without `#pragma program(name)` is named
/// `default_name`.
pub fn check(file: &ast::File, default_name: &str) -> Checking {
    let mut warnings = Vec::new();
    let program = check::declarations(file, &mut warnings).and_then(|decls| {
        let params = check::bindings(&decls)?;
        elaborate::program(&decls, params, default_name, &mut warnings)
    });
    Checking { program, warnings }
}

#[cfg(test)]
mod tests {
    use super::hir::{Access, Count, ExprKind, LoopKind, Program, Stmt};
    use super::types::Type;
    use super::*;

    fn program(source: &str) -> Program {
        let file = crate::syntax::parse(source).unwrap();
        match check(&file, "test").program {
            Ok(program) => program,
            Err(problem) => panic!("{problem:?}"),
        }
    }

    const PARTICLES: &str = "struct Particle { uint32 color; vec3 pos; };\n\
                             struct Batch { layout(lane = w) int16 count; vec4 out0; Particle particles[count]; };\n";

    #[test]
    fn loads_keep_memory_lanes() {
        let source = format!(
            "{PARTICLES}\
             void main(layout(xtop) inout Batch batch)\n\
             {{\n\
             \x20   for (Particle p : batch.particles)\n\
             \x20   {{\n\
             \x20       vec3 position = p.pos;\n\
             \x20       vec2 tail = position.yz;\n\
             \x20   }}\n\
             }}\n"
        );
        let program = program(&source);
        let Stmt::Loop { body, kind, .. } = &program.body[0] else {
            panic!("{:?}", program.body);
        };
        let lanes: Vec<String> = body
            .iter()
            .map(|stmt| match stmt {
                Stmt::SetLocal { value, .. } => value.placement.unwrap().letters(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(lanes, ["yzw", "zw"]);
        // The count is the member before the array.
        let LoopKind::Range {
            count: Count::Member(place),
            ..
        } = kind
        else {
            panic!("{kind:?}");
        };
        assert_eq!(
            (place.path.as_slice(), &place.ty),
            ([0].as_slice(), &Type::Int16)
        );
        assert_eq!(place.lane.letter(), 'w');
    }

    #[test]
    fn unrolled_loops_repeat_their_body_with_constants() {
        let source = "struct Frame { mat4 m; vec4 scale; };\n\
                      void main(layout(address = 0) inout Frame frame)\n\
                      {\n\
                      \x20   mat4 m = frame.m;\n\
                      \x20   #pragma unroll\n\
                      \x20   for (int16 c = 0; c < 4; ++c)\n\
                      \x20       m[c].xyz *= frame.scale.xyz;\n\
                      \x20   frame.m = m;\n\
                      }\n";
        let program = program(source);
        let columns: Vec<Option<usize>> = program.body[1..5]
            .iter()
            .map(|stmt| match stmt {
                Stmt::SetLocal { column, .. } => *column,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(columns, [Some(0), Some(1), Some(2), Some(3)]);
        assert!(matches!(program.body[5], Stmt::Store { .. }));
    }

    #[test]
    fn accesses_name_the_members_read_and_written() {
        let source = format!(
            "{PARTICLES}\
             void main(layout(xtop) inout Batch batch)\n\
             {{\n\
             \x20   for (Particle p : batch.particles)\n\
             \x20       batch.out0.x = p.pos.x;\n\
             }}\n"
        );
        let program = program(&source);
        let accesses: Vec<Access> = program.accesses.clone();
        let access = |path: &[usize], write| Access {
            param: 0,
            path: path.to_vec(),
            write,
        };
        assert_eq!(
            accesses,
            [
                access(&[0], false),
                access(&[2, 1], false),
                access(&[1], true)
            ],
            "{accesses:?}"
        );
    }

    #[test]
    fn calls_inline_their_setup_then_their_body() {
        let source = "struct Block { float a; float b; };\n\
                      float twice(float x) { float y = x + x; return y; }\n\
                      void main(layout(address = 0) inout Block block)\n\
                      {\n\
                      \x20   block.b = twice(block.a) * 2.0;\n\
                      }\n";
        let program = program(source);
        let names: Vec<&str> = program
            .body
            .iter()
            .map(|stmt| match stmt {
                Stmt::SetLocal { local, .. } => program.locals[*local].name.as_str(),
                Stmt::Store { .. } => "store",
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["x", "y", "store"]);
        let Stmt::Store { value, .. } = &program.body[2] else {
            unreachable!()
        };
        let ExprKind::Binary { lhs, .. } = &value.kind else {
            panic!("{value:?}");
        };
        assert!(matches!(lhs.kind, ExprKind::Local(_)));
    }
}
