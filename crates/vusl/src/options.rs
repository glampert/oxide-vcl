//! What a VUSL compile is asked to do.

/// A compile's settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// `-I`: directories `#include` looks in, after the including file's.
    pub include_dirs: Vec<String>,
    /// `-D NAME[=VALUE]`: macros defined before the source, as
    /// `#define NAME VALUE` (VALUE is 1 when absent).
    pub defines: Vec<(String, String)>,
    /// `--mode=`: how `vcl` compiles the generated VCL, oxide mode unless
    /// asked otherwise.
    pub mode: vcl::Mode,
    /// `--opt=`: the oxide-vcl optimizations chosen; `None` runs them all.
    pub optimizations: Option<vcl::Optimizations>,
    /// `--no-opt=`: the oxide-vcl optimizations turned off, whatever
    /// `--opt=` chose.
    pub disabled_optimizations: vcl::Optimizations,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            include_dirs: Vec::new(),
            defines: Vec::new(),
            mode: vcl::Mode::Oxide,
            optimizations: None,
            disabled_optimizations: vcl::Optimizations::NONE,
        }
    }
}
