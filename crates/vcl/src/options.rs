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
    /// Fixes for defects of vcl 1.4beta7 chosen with `--fix=` (oxide mode
    /// enables them all: see [`Options::fixes`]).
    pub fixes: Fixes,
}

impl Options {
    /// Whether `fix` applies: chosen with `--fix=`, or implied by oxide mode.
    pub fn fixes(&self, fix: Fix) -> bool {
        self.enabled_fixes().contains(fix)
    }

    /// The fixes that apply: those chosen with `--fix=`, or all of them in
    /// oxide mode.
    pub fn enabled_fixes(&self) -> Fixes {
        match self.mode {
            crate::Mode::Vcl => self.fixes,
            crate::Mode::Oxide => Fixes::all(),
        }
    }
}

/// A fix for a defect that vcl 1.4beta7 has (docs/VCL_BUGS.md), chosen by
/// name with `--fix=NAME`. Without it the port reproduces the defect.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Fix {
    /// Numeric immediates that do not fit their encoding field are
    /// legalized, rejected or warned about, and displacement repair never
    /// pushes a memory offset out of its field (`imm`).
    Imm,
    /// A load or store is kept in order with an earlier one when both
    /// addresses are provably the same location, even through different
    /// base registers (`alias`).
    Alias,
    /// CLIPW without `in_hw_clip` is accepted when nothing observes the
    /// undeclared clip history (`clip`).
    Clip,
    /// Every exit and `--cont` keeps a row with the E bit and its delay
    /// slot, which vcl 1.4beta7's final compression can delete (`end`).
    End,
    /// A warning for a program larger than VU1 micro memory (`size`).
    Size,
}

impl Fix {
    /// Every fix, in the order the help lists them.
    pub const ALL: [Fix; 5] = [Fix::Imm, Fix::Alias, Fix::Clip, Fix::End, Fix::Size];

    /// The fix's name on the command line.
    pub const fn name(self) -> &'static str {
        match self {
            Fix::Imm => "imm",
            Fix::Alias => "alias",
            Fix::Clip => "clip",
            Fix::End => "end",
            Fix::Size => "size",
        }
    }

    /// The fix a command-line name selects.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|fix| fix.name() == name)
    }

    const fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// A set of [`Fix`]es.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Fixes(u32);

impl Fixes {
    /// No fix: vcl 1.4beta7's behaviour.
    pub const NONE: Fixes = Fixes(0);

    /// Every fix (`--fix=all`).
    pub fn all() -> Self {
        Fix::ALL.into_iter().fold(Self::NONE, Self::with)
    }

    /// This set with `fix` added.
    pub const fn with(self, fix: Fix) -> Self {
        Self(self.0 | fix.bit())
    }

    /// The fixes in either set.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn insert(&mut self, fix: Fix) {
        *self = self.with(fix);
    }

    pub const fn contains(self, fix: Fix) -> bool {
        self.0 & fix.bit() != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_are_named_and_oxide_mode_implies_them() {
        for fix in Fix::ALL {
            assert_eq!(Fix::from_name(fix.name()), Some(fix));
            assert!(Fixes::all().contains(fix));
            assert!(!Fixes::NONE.contains(fix));
        }
        assert_eq!(Fix::from_name("all"), None);
        let vcl = Options::default();
        assert!(!vcl.fixes(Fix::End));
        let chosen = Options {
            fixes: Fixes::NONE.with(Fix::End),
            ..Options::default()
        };
        assert!(chosen.fixes(Fix::End));
        let oxide = Options {
            mode: crate::Mode::Oxide,
            ..Options::default()
        };
        assert!(oxide.fixes(Fix::End));
    }
}
