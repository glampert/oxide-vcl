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
    /// How `vcl` compiles the generated VCL beyond its mode and label
    /// prefix, which `mode` and the program's name set: the optimizations
    /// (`--opt=`, `--no-opt=`) and what `--vcl-option=` passes.
    pub vcl: vcl::Options,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            include_dirs: Vec::new(),
            defines: Vec::new(),
            mode: vcl::Mode::Oxide,
            vcl: vcl::Options::default(),
        }
    }
}
