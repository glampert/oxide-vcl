//! Instruction metadata extracted from the supplied VCL ELF.
//!
//! [`INSTRUCTIONS`] retains the original 214-entry table order. In particular,
//! the first twelve entries implement generalized new-syntax FMAC operations.
//! They must not be removed or deduplicated: table indices are instruction IDs.

mod generated {
    include!("instruction_data.rs");
}

pub use generated::INSTRUCTIONS;

pub const SOURCE_SHA256: &str = "83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Syntax {
    #[default]
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Read,
    Write,
    Unknown,
}

/// VCL numbers components x=1, y=2, z=4 and w=8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mask(pub u8);

impl Mask {
    pub const X: Self = Self(1);
    pub const Y: Self = Self(2);
    pub const Z: Self = Self(4);
    pub const W: Self = Self(8);
    pub const XYZ: Self = Self(7);
    pub const XYZW: Self = Self(15);

    pub fn parse(value: &str) -> Option<Self> {
        let mut result = 0;
        for c in value.bytes() {
            result |= match c.to_ascii_lowercase() {
                b'x' => 1,
                b'y' => 2,
                b'z' => 4,
                b'w' => 8,
                _ => return None,
            };
        }
        Some(Self(result))
    }

    pub fn is_scalar(self) -> bool {
        self.0.count_ones() == 1
    }

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn suffix(self) -> String {
        [(1, 'x'), (2, 'y'), (4, 'z'), (8, 'w')]
            .into_iter()
            .filter_map(|(bit, c)| (self.0 & bit != 0).then_some(c))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperandSpec {
    /// Original OP_* class number; see `local/evidence/tables/operand_classes.json`.
    pub class: u8,
    pub direction_code: u8,
    pub implicit: bool,
    /// Signed dependency offset. -1 is used for reads, +4 for FMAC results.
    pub latency: i8,
    pub uses_mask: bool,
    pub immediate_kind: u8,
    /// One-based link to an earlier explicit operand, or zero.
    pub patch: u8,
}

impl OperandSpec {
    pub fn direction(self) -> Direction {
        match self.direction_code {
            1 => Direction::Read,
            2 => Direction::Write,
            _ => Direction::Unknown,
        }
    }
}

/// The decode-time special handling a descriptor selects, inferred from
/// vcl 1.4beta7. `hook_argument` parameterizes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    None,
    /// Generalized new-syntax FMAC operations (the first twelve entries).
    Fmac,
    /// EFU operations: fixed input fields are installed before flow.
    Efu,
    /// MR32 rotates its source components.
    Mr32,
    /// ILWR/ISWR register-indirect memory operands.
    RegisterMemory,
    /// XGKICK.
    Xgkick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstructionDef {
    pub index: u16,
    pub name: &'static str,
    /// 0=no component selection, 1=arbitrary mask, 2=xyz, 3=one lane.
    pub mask_mode: u8,
    /// Upper=1, lower=2, either=3.
    pub issue_mask: u16,
    /// FMAC=1, IALU=2, MOVE=4, LS=8, EFU=16, DIV=32, RAND=64,
    /// EXT=128, BRANCH=256, as stored in the source descriptor.
    pub unit_mask: u16,
    pub throughput: u16,
    /// 0=raw-only, 1=ordinary, 2=synthetic, 3=branch, 4=load/store.
    pub kind: u8,
    pub flags: u8,
    pub subtype: u8,
    /// 1-based position of a branch's target operand (0 when none).
    pub target_operand: u8,
    pub hook_argument: u32,
    pub hook: Hook,
    pub operands: &'static [OperandSpec],
}

impl InstructionDef {
    pub fn explicit_operands(self) -> impl Iterator<Item = &'static OperandSpec> {
        self.operands.iter().filter(|operand| !operand.implicit)
    }

    pub fn is_synthetic(self) -> bool {
        self.kind == 2
    }

    pub fn is_branch(self) -> bool {
        self.kind == 3
    }

    pub fn is_memory(self) -> bool {
        self.kind == 4
    }
}

/// Match exactly as the original's lookup does: case-insensitive, table order retained,
/// with the first twelve generalized entries unavailable in old syntax.
pub fn lookup(name: &str, syntax: Syntax) -> Option<&'static InstructionDef> {
    let start = if syntax == Syntax::Old { 12 } else { 0 };
    INSTRUCTIONS[start..]
        .iter()
        .find(|instruction| instruction.name.eq_ignore_ascii_case(name))
}

pub fn by_index(index: u16) -> Option<&'static InstructionDef> {
    INSTRUCTIONS.get(usize::from(index))
}

/// Register pool classes consumed by the operand decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterFamily {
    Vector,
    Integer,
    Hardware,
    Memory,
    CoreGeneral,
    CoreFloat,
    CoreHardware,
    None,
}

pub fn register_family(class: u8) -> RegisterFamily {
    match class {
        1 | 3 | 11 | 13..=16 | 35 => RegisterFamily::Vector,
        2 | 28..=34 | 36 => RegisterFamily::Integer,
        4..=10 | 17..=26 | 45 | 55 => RegisterFamily::Hardware,
        37 => RegisterFamily::Memory,
        39..=42 | 46 | 54 => RegisterFamily::CoreGeneral,
        43 | 44 => RegisterFamily::CoreFloat,
        47..=53 => RegisterFamily::CoreHardware,
        _ => RegisterFamily::None,
    }
}

/// Name created by the original's implicit-operand constructor.
/// Status classes 22–25 intentionally have no name here: the status expansion expands
/// them into actual ALU/divide status resources later in the original pipeline.
pub fn implicit_register(class: u8) -> Option<&'static str> {
    match class {
        4 => Some("I"),
        5 => Some("P"),
        6 => Some("tt_nP"),
        7 => Some("Q"),
        8 => Some("R"),
        9 | 10 => Some("ACC"),
        17 => Some("CLIP"),
        26 => Some("MAC"),
        37 => Some("ramreg"),
        45 => Some("CLIPa"),
        // XGKICK's implicit external-unit dependency.
        55 => Some("EXT"),
        _ => None,
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_table_identity_and_indices_are_preserved() {
        assert_eq!(INSTRUCTIONS.len(), 214);
        assert_eq!(lookup("ADD", Syntax::Old).unwrap().index, 34);
        assert_eq!(lookup("add", Syntax::New).unwrap().index, 5);
        assert_eq!(lookup("waitp", Syntax::Old).unwrap().index, 115);
        assert_eq!(lookup("waitq", Syntax::Old).unwrap().index, 116);
        assert!(by_index(214).is_none());
    }

    #[test]
    fn dependencies_include_hardware_effects_and_signed_offsets() {
        let add = lookup("add", Syntax::Old).unwrap();
        assert_eq!(add.operands[0].latency, 4);
        assert_eq!(add.operands[1].latency, -1);
        assert_eq!(add.explicit_operands().count(), 3);
        assert!(
            add.operands
                .iter()
                .any(|operand| operand.class == 26 && operand.implicit)
        );
        let div = lookup("div", Syntax::Old).unwrap();
        assert_eq!(div.unit_mask, 32);
        assert_eq!(div.throughput, 7);
        assert_eq!(lookup("eatan", Syntax::Old).unwrap().throughput, 53);
    }

    #[test]
    fn masks_keep_binary_lane_order() {
        assert_eq!(Mask::parse("WxX"), Some(Mask(9)));
        assert_eq!(Mask::XYZ.suffix(), "xyz");
        assert!(Mask::XYZ.contains(Mask::Y));
        assert!(Mask::W.is_scalar());
        assert!(!Mask::XYZ.is_scalar());
        assert_eq!(Mask::parse("q"), None);
    }
}
