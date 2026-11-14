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
    /// Optimizations chosen with `--opt=`; `None` is oxide mode's default,
    /// all of them. vcl mode never optimizes: see [`Options::optimizes`].
    pub optimizations: Option<Optimizations>,
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

    /// Whether `optimization` runs: only in oxide mode, where `--opt=`
    /// chooses them and every one runs by default.
    pub fn optimizes(&self, optimization: Optimization) -> bool {
        self.enabled_optimizations().contains(optimization)
    }

    /// The optimizations that run: none in vcl mode; in oxide mode those
    /// chosen with `--opt=`, or all of them.
    pub fn enabled_optimizations(&self) -> Optimizations {
        match self.mode {
            crate::Mode::Vcl => Optimizations::NONE,
            crate::Mode::Oxide => self.optimizations.unwrap_or_else(Optimizations::all),
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
    /// Memory operands shorter than vcl 1.4beta7's token decoder allows,
    /// such as `ilw.x t,0(p)`, are accepted (`operands`).
    Operands,
}

impl Fix {
    /// Every fix, in the order the help lists them.
    pub const ALL: [Fix; 6] = [
        Fix::Imm,
        Fix::Alias,
        Fix::Clip,
        Fix::End,
        Fix::Size,
        Fix::Operands,
    ];

    /// The fix's name on the command line.
    pub const fn name(self) -> &'static str {
        match self {
            Fix::Imm => "imm",
            Fix::Alias => "alias",
            Fix::Clip => "clip",
            Fix::End => "end",
            Fix::Size => "size",
            Fix::Operands => "operands",
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

/// A code generation improvement over vcl 1.4beta7
/// (docs/OPTIMIZATION_PLAN.md), named for `--opt=NAME` so that it can be
/// turned off on its own. None exists yet: the plan's first phase adds one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Optimization {}

impl Optimization {
    /// Every optimization, in the order the help lists them.
    pub const ALL: [Optimization; 0] = [];

    /// The optimization's name on the command line.
    pub const fn name(self) -> &'static str {
        match self {}
    }

    /// The optimization a command-line name selects.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|optimization| optimization.name() == name)
    }

    const fn bit(self) -> u32 {
        match self {}
    }
}

/// A set of [`Optimization`]s.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Optimizations(u32);

impl Optimizations {
    /// No optimization (`--opt=none`).
    pub const NONE: Optimizations = Optimizations(0);

    /// Every optimization (`--opt=all`, and oxide mode's default).
    pub fn all() -> Self {
        Optimization::ALL.into_iter().fold(Self::NONE, Self::with)
    }

    /// This set with `optimization` added.
    pub const fn with(self, optimization: Optimization) -> Self {
        Self(self.0 | optimization.bit())
    }

    /// The optimizations in either set.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, optimization: Optimization) -> bool {
        self.0 & optimization.bit() != 0
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

    #[test]
    fn only_oxide_mode_optimizes() {
        assert_eq!(Optimization::from_name("all"), None);
        assert_eq!(Optimization::from_name("none"), None);
        let all = Optimizations::all();
        let vcl = Options {
            optimizations: Some(all),
            ..Options::default()
        };
        assert_eq!(vcl.enabled_optimizations(), Optimizations::NONE);
        let oxide = Options {
            mode: crate::Mode::Oxide,
            ..Options::default()
        };
        assert_eq!(oxide.enabled_optimizations(), all);
        let none = Options {
            optimizations: Some(Optimizations::NONE),
            ..oxide
        };
        assert!(none.enabled_optimizations().is_empty());
    }
}
