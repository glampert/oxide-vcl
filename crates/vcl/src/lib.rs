//! Reconstruction of the SCE VCL vector unit compiler (vcl 1.4beta7).
//!
//! Each primitive's behaviour is observed from or inferred from vcl 1.4beta7,
//! as its module documentation notes. Whole-program compatibility is tracked separately in
//! the investigation ledger; passing unit tests does not imply equivalence.
//!
//! The crate is organised by compiler phase:
//!
//! - [`isa`]: the VU instruction table and operand metadata;
//! - [`parser`]: VCL source, directives and immediate expressions;
//! - [`analysis`]: data flow, register webs, pruning, copies, induction
//!   variables, control-flow predicates, status flags and memory facts;
//! - [`scheduler`]: linear table scheduling, wrangling and row timing;
//! - [`loops`]: modulo scheduling and software-loop expansion;
//! - [`allocator`]: register occupancy and physical assignment;
//! - [`emit`]: assembly formatting;
//! - [`compiler`]: the driver that runs the phases over a whole program;
//! - [`diagnostics`]: every message and the writer that prints them, in vcl
//!   or oxide [`Mode`].
//!
//! [`compile`] is the entry point; [`Options`] holds the settings it reads.

pub mod allocator;
pub mod analysis;
pub mod compiler;
pub mod diagnostics;
pub mod emit;
pub mod isa;
pub mod loops;
mod options;
pub mod parser;
pub mod scheduler;

pub use compiler::{CompileError, Compiled, compile};
pub use diagnostics::{Diagnostic, Message, Mode};
pub use options::{Fix, Fixes, Options};

/// The project's version, shared by every crate in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod test_evidence;
