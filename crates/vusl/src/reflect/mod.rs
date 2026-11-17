//! The reflection: what the host needs to bind a program, as JSON for
//! tools ([`json`]) and a C++ header for the runtime ([`cpp`]), both from
//! one model ([`model`]).

pub mod cpp;
pub mod json;
pub mod model;

pub use model::{Reflection, reflect};
