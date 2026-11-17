//! What a VUSL compile is asked to do.

/// A compile's settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// `-I`: directories `#include` looks in, after the including file's.
    pub include_dirs: Vec<String>,
    /// `-D NAME[=VALUE]`: macros defined before the source, as
    /// `#define NAME VALUE` (VALUE is 1 when absent).
    pub defines: Vec<(String, String)>,
}
