//! Compilation settings.
//!
//! These are the switches that change what the compiler produces. Reading
//! files, preprocessing and diagnostics formatting belong to the caller (the
//! `oxide-vcl` driver parses them from the original's command line).

/// Settings for [`crate::compile`]. The default is the original's behaviour
/// with no switches, in [`crate::Mode::Vcl`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// vcl 1.4beta7's behaviour (the default) or oxide-vcl's (`--mode=oxide`).
    pub mode: crate::Mode,
    /// Prefix for generated labels (`-u`); the file name when `None`.
    pub unique: Option<String>,
    /// Emit the source lines as comments (`-c`).
    pub comments: bool,
    /// Emit symbolic register names, skipping register assignment (`-s`).
    pub symbolic: bool,
    /// Parse the new (virtual register) syntax (`-n`).
    pub new_syntax: bool,
    /// Never set the E bit (`-e`).
    pub disable_end_bit: bool,
    /// Omit `.align` directives (`-f`).
    pub disable_alignment: bool,
    /// Schedule loops as straight-line code (`-L`, implied by `-d`).
    pub disable_loops: bool,
    /// Plain in-order code generation only (`-d`).
    pub dumb: bool,
    /// Skip the code size reduction pass (`-C`).
    pub disable_size_reduction: bool,
    /// Wrap the program in a DMA/MPG packet loaded at this address (`-m`).
    pub mpg: Option<i32>,
    /// Make every label except `.global` ones unique (`-U`).
    pub unique_labels: bool,
    /// Timid memory access mode (`-M`).
    pub timid_memory: bool,
}
