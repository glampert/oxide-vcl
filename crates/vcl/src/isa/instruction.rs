//! Instruction metadata extracted from the supplied VCL ELF.
//!
//! [`INSTRUCTIONS`] retains the original 214-entry table order. In particular,
//! the first twelve entries implement generalized new-syntax FMAC operations.
//! They must not be removed or deduplicated: table indices are instruction IDs.

mod generated {
    include!("instruction_data.rs");
}

pub use generated::INSTRUCTIONS;

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
    pub const XY: Self = Self(3);
    pub const XZ: Self = Self(5);
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

/// Operand classes, the original's operand descriptor numbers (inferred from
/// vcl 1.4beta7; `local/evidence/tables/operand_classes.json`, local, not
/// tracked). Number 12 is unused.
pub mod class {
    pub const NONE: u8 = 0;
    /// A VF register with the instruction's field selection.
    pub const VECTOR: u8 = 1;
    /// A VI register.
    pub const INTEGER: u8 = 2;
    /// One field of a VF register, chosen by the operand (`vf01x`).
    pub const SCALAR: u8 = 3;
    pub const I: u8 = 4;
    pub const P: u8 = 5;
    /// The EFU result an instruction leaves for the next WAITP.
    pub const NEXT_P: u8 = 6;
    pub const Q: u8 = 7;
    pub const R: u8 = 8;
    pub const ACC: u8 = 9;
    /// ACC with the fixed xyz selection (OPMULA, OPMSUB).
    pub const ACC_XYZ: u8 = 10;
    /// A VF register with the fixed xyz selection.
    pub const VECTOR_XYZ: u8 = 11;
    /// A VF register's fixed x, y, z or w field.
    pub const FIELD_X: u8 = 13;
    pub const FIELD_Y: u8 = 14;
    pub const FIELD_Z: u8 = 15;
    pub const FIELD_W: u8 = 16;
    pub const CLIP: u8 = 17;
    /// The status resources the status classes below expand into
    /// (`analysis::prune::expand`); the table itself never names them.
    pub const STAT_ALU_STICKY: u8 = 18;
    pub const STAT_DIV_STICKY: u8 = 19;
    pub const STAT_DIV: u8 = 20;
    pub const STAT_ALU: u8 = 21;
    /// Status effects: of DIV, SQRT and RSQRT, of FMAC operations, and of
    /// the flag tests (FSAND, FSEQ, FSOR) and FSSET.
    pub const STATUS_FROM_DIVIDE: u8 = 22;
    pub const STATUS_FROM_ALU: u8 = 23;
    pub const STATUS_TEST: u8 = 24;
    pub const STATUS_SET: u8 = 25;
    /// The MAC flags.
    pub const MAC: u8 = 26;
    pub const IMMEDIATE: u8 = 27;
    /// Address operands of loads and stores: `(vi)` as the implicit base of
    /// the incrementing forms, `offset(vi)` (LQ, SQ), `(vi)` (ILWR, ISWR),
    /// `offset(vi)` (ILW, ISW), `(vi++)` and `(--vi)`.
    pub const ADDRESS: u8 = 28;
    pub const ADDRESS_OFFSET: u8 = 29;
    pub const ADDRESS_REGISTER: u8 = 30;
    pub const ADDRESS_REGISTER_OFFSET: u8 = 31;
    pub const ADDRESS_INCREMENT: u8 = 32;
    pub const ADDRESS_DECREMENT: u8 = 33;
    /// The VI register FCAND, FCEQ and FCOR write (always VI01).
    pub const INTEGER_FLAG: u8 = 34;
    /// The VF and VI names that `in_vf`, `out_vf`, `in_vi` and `out_vi` declare.
    pub const DECLARED_VECTOR: u8 = 35;
    pub const DECLARED_INTEGER: u8 = 36;
    /// The data memory a load or store touches.
    pub const MEMORY: u8 = 37;
    /// A generalized new-syntax FMAC operand: a vector, one field, I or Q.
    pub const GENERALIZED: u8 = 38;
    /// Classes of the core (EE) instruction set, which VCL never reads from
    /// this table.
    pub const CORE_GPR32: u8 = 39;
    pub const CORE_GPR64: u8 = 40;
    pub const CORE_GPR128: u8 = 41;
    pub const CORE_GPR_SCALAR: u8 = 42;
    pub const CORE_FPR: u8 = 43;
    pub const CORE_FPR_SCALAR: u8 = 44;
    /// All four clip flag sets (CLIPW, FCSET).
    pub const CLIP_ALL: u8 = 45;
    pub const CORE_ADDRESS_OFFSET: u8 = 46;
    pub const CORE_HI_LO0: u8 = 47;
    pub const CORE_HI_LO1: u8 = 48;
    pub const CORE_FPU_STATUS: u8 = 49;
    pub const CORE_FPU_CONDITION: u8 = 50;
    pub const CORE_FPU_ACC: u8 = 51;
    pub const CORE_FPU_DIVIDE: u8 = 52;
    pub const CORE_SHIFT_AMOUNT: u8 = 53;
    pub const CORE_RETURN_ADDRESS: u8 = 54;
    /// XGKICK's dependency on the external unit.
    pub const EXTERNAL: u8 = 55;

    /// Any address operand of a load or store.
    pub const fn is_address(class: u8) -> bool {
        matches!(class, ADDRESS..=ADDRESS_DECREMENT)
    }
}

/// How an instruction converts and prints an immediate operand: the low five
/// bits are the field width, bits 5 and 6 the value type (inferred from
/// vcl 1.4beta7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImmediateKind(pub u8);

/// An immediate's value type, from [`ImmediateKind::value_type`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImmediateType {
    Unsigned,
    Signed,
    Float,
    /// Printed as written.
    Literal,
}

impl ImmediateKind {
    pub const NONE: Self = Self(0);
    /// LOI's 32-bit float.
    pub const FLOAT: Self = Self(0x40);
    /// The exit macro's text.
    pub const LITERAL: Self = Self(0x60);

    pub const fn unsigned(width: u8) -> Self {
        Self(width)
    }

    pub const fn signed(width: u8) -> Self {
        Self(0x20 | width)
    }

    pub const fn width(self) -> u8 {
        self.0 & 0x1f
    }

    /// The conversion applied before printing; none for [`Self::NONE`].
    pub const fn value_type(self) -> Option<ImmediateType> {
        if self.0 == 0 {
            return None;
        }
        Some(match self.0 & 0x60 {
            0x00 => ImmediateType::Unsigned,
            0x20 => ImmediateType::Signed,
            0x40 => ImmediateType::Float,
            _ => ImmediateType::Literal,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperandSpec {
    /// One of the [`class`] numbers.
    pub class: u8,
    pub direction: Direction,
    pub implicit: bool,
    /// Signed dependency offset. -1 is used for reads, +4 for FMAC results.
    pub latency: i8,
    pub uses_mask: bool,
    pub immediate_kind: ImmediateKind,
    /// One-based link to an earlier explicit operand, or zero.
    pub patch: u8,
}

/// The decode-time special handling a descriptor selects, inferred from
/// vcl 1.4beta7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    None,
    /// Generalized new-syntax FMAC operations (the first twelve entries).
    Fmac,
    /// EFU operations: the fixed input fields installed before flow (x for
    /// the one-field operations, which keep the source's own selection).
    Efu(Mask),
    /// MR32 rotates its source components.
    Mr32,
    /// ILWR/ISWR register-indirect memory operands.
    RegisterMemory,
    /// XGKICK.
    Xgkick,
}

/// Which field selections an instruction takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskMode {
    /// No field selection.
    Unmasked,
    /// Any non-empty selection.
    Any,
    /// Only `.xyz` (CLIPW, OPMULA, OPMSUB).
    Xyz,
    /// Exactly one field (ILW, ILWR).
    OneLane,
}

/// The pipes an instruction can issue in, as a bit set.
pub mod issue {
    pub const UPPER: u8 = 1;
    pub const LOWER: u8 = 2;
    /// Either pipe (NOP and the abstract `vmove`).
    pub const EITHER: u8 = UPPER | LOWER;
}

/// The functional units an instruction occupies, as a bit set.
pub mod unit {
    pub const NONE: u16 = 0;
    pub const FMAC: u16 = 1;
    pub const IALU: u16 = 2;
    pub const MOVE: u16 = 4;
    pub const LOAD_STORE: u16 = 8;
    pub const EFU: u16 = 16;
    pub const DIVIDE: u16 = 32;
    pub const RANDOM: u16 = 64;
    /// The external unit (XGKICK, XTOP, XITOP).
    pub const EXTERNAL: u16 = 128;
    pub const BRANCH: u16 = 256;
}

/// Memory ordering classes for instructions without an address, as a bit
/// set (inferred from vcl 1.4beta7). Only XGKICK carries any.
pub mod order {
    pub const NONE: u8 = 0;
    /// Ordered like a store: after earlier loads, and later loads after it.
    pub const WRITES_MEMORY: u8 = 0x10;
    /// Ordered like a load: after earlier stores, and later stores after it.
    pub const READS_MEMORY: u8 = 0x20;
    /// After the nearest earlier instruction that also has this bit.
    pub const CHAINED: u8 = 0x40;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstructionKind {
    /// NOP, WAITP and WAITQ, which VCL treats as raw code.
    RawOnly,
    Ordinary,
    /// Directives and declarations (`--enter`, `in_vf`, ...).
    Synthetic,
    Branch,
    /// Loads and stores.
    Memory,
}

/// Subtypes, whose meaning depends on the instruction's kind.
pub mod subtype {
    pub const NONE: u8 = 0;
    /// Ordinary and raw-only instructions other than NOP and XGKICK.
    pub const PLAIN: u8 = 1;

    /// Branch subtypes, a bit set.
    pub mod branch {
        pub const JUMP: u8 = 1;
        /// The target is a register (JR, JALR).
        pub const REGISTER: u8 = 2;
        pub const CONDITIONAL: u8 = 4;
        /// Writes the return address (BAL, JALR).
        pub const LINK: u8 = 8;
    }

    pub mod memory {
        pub const LOAD: u8 = 1;
        pub const STORE: u8 = 2;
    }

    /// Synthetic subtypes, read by `parser::Instruction::synthetic`.
    pub mod synthetic {
        pub const ENTER: u8 = 1;
        pub const END_ENTER: u8 = 2;
        pub const EXIT: u8 = 3;
        pub const END_EXIT: u8 = 4;
        pub const CONTINUE: u8 = 5;
        pub const INPUT: u8 = 6;
        pub const OUTPUT: u8 = 7;
        pub const LOOP_COUNT: u8 = 8;
        pub const LOOP_EXTRA: u8 = 9;
        pub const BARRIER: u8 = 10;
        pub const EXIT_MACRO: u8 = 11;
        pub const LOOP_ABSOLUTE: u8 = 12;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstructionDef {
    pub index: u16,
    pub name: &'static str,
    pub mask_mode: MaskMode,
    /// The [`issue`] pipes.
    pub issue_mask: u8,
    /// The [`unit`]s.
    pub unit_mask: u16,
    pub throughput: u16,
    pub kind: InstructionKind,
    /// The [`order`] classes.
    pub order_flags: u8,
    /// One of the [`subtype`]s for the instruction's kind.
    pub subtype: u8,
    /// 1-based position of a branch's target or a load or store's address
    /// operand (0 when none).
    pub target_operand: u8,
    pub hook: Hook,
    pub operands: &'static [OperandSpec],
}

impl InstructionDef {
    pub fn explicit_operands(self) -> impl Iterator<Item = &'static OperandSpec> {
        self.operands.iter().filter(|operand| !operand.implicit)
    }

    pub fn is_synthetic(self) -> bool {
        self.kind == InstructionKind::Synthetic
    }

    pub fn is_branch(self) -> bool {
        self.kind == InstructionKind::Branch
    }

    pub fn is_memory(self) -> bool {
        self.kind == InstructionKind::Memory
    }

    pub fn is_store(self) -> bool {
        self.is_memory() && self.subtype == subtype::memory::STORE
    }

    pub fn is_conditional_branch(self) -> bool {
        self.is_branch() && self.subtype & subtype::branch::CONDITIONAL != 0
    }
}

/// Table indices (instruction IDs) the passes single out.
pub mod id {
    /// The generalized new-syntax FMAC entries come first, before this index.
    pub const GENERALIZED_END: u16 = 12;
    pub const NOP: u16 = 12;
    pub const ADD: u16 = 34;
    pub const ADDX: u16 = 37;
    pub const ADDY: u16 = 38;
    pub const ADDZ: u16 = 39;
    pub const SUB: u16 = 48;
    pub const MULW: u16 = 68;
    pub const CLIPW: u16 = 104;
    pub const IADD: u16 = 108;
    pub const IADDI: u16 = 109;
    pub const IADDIU: u16 = 110;
    pub const ISUB: u16 = 111;
    pub const ISUBIU: u16 = 112;
    pub const FCAND: u16 = 138;
    pub const FCEQ: u16 = 139;
    pub const FCOR: u16 = 140;
    pub const FCGET: u16 = 141;
    pub const FCSET: u16 = 142;
    pub const MOVE: u16 = 153;
    pub const LQ: u16 = 155;
    pub const ILW: u16 = 156;
    pub const LQI: u16 = 158;
    pub const LQD: u16 = 159;
    pub const SQ: u16 = 160;
    pub const ISW: u16 = 161;
    pub const SQI: u16 = 163;
    pub const SQD: u16 = 164;
    pub const XGKICK: u16 = 165;
    pub const XTOP: u16 = 166;
    pub const XITOP: u16 = 167;
    /// The abstract moves: integer and vector, either pipe, and the
    /// lower- and upper-pipe forms.
    pub const IMOVE: u16 = 178;
    pub const LIMOVE: u16 = 179;
    pub const VMOVE: u16 = 180;
    pub const LVMOVE: u16 = 181;
    pub const UVMOVE: u16 = 182;
    pub const IN_HW_CLIP: u16 = 199;
    pub const OUT_HW_CLIP: u16 = 208;
}

/// Match exactly as the original's lookup does: case-insensitive, table order retained,
/// with the first twelve generalized entries unavailable in old syntax.
pub fn lookup(name: &str, syntax: Syntax) -> Option<&'static InstructionDef> {
    let start = if syntax == Syntax::Old {
        usize::from(id::GENERALIZED_END)
    } else {
        0
    };
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
    use class::*;
    match class {
        VECTOR | SCALAR | VECTOR_XYZ | FIELD_X..=FIELD_W | DECLARED_VECTOR => {
            RegisterFamily::Vector
        }
        INTEGER | ADDRESS..=INTEGER_FLAG | DECLARED_INTEGER => RegisterFamily::Integer,
        I..=ACC_XYZ | CLIP..=MAC | CLIP_ALL | EXTERNAL => RegisterFamily::Hardware,
        MEMORY => RegisterFamily::Memory,
        CORE_GPR32..=CORE_GPR_SCALAR | CORE_ADDRESS_OFFSET | CORE_RETURN_ADDRESS => {
            RegisterFamily::CoreGeneral
        }
        CORE_FPR | CORE_FPR_SCALAR => RegisterFamily::CoreFloat,
        CORE_HI_LO0..=CORE_SHIFT_AMOUNT => RegisterFamily::CoreHardware,
        _ => RegisterFamily::None,
    }
}

/// Name created by the original's implicit-operand constructor.
/// Status classes intentionally have no name here: the status expansion expands
/// them into actual ALU/divide status resources later in the original pipeline.
pub fn implicit_register(class: u8) -> Option<&'static str> {
    use class::*;
    match class {
        I => Some("I"),
        P => Some("P"),
        NEXT_P => Some("tt_nP"),
        Q => Some("Q"),
        R => Some("R"),
        ACC | ACC_XYZ => Some("ACC"),
        CLIP => Some("CLIP"),
        MAC => Some("MAC"),
        MEMORY => Some("ramreg"),
        CLIP_ALL => Some("CLIPa"),
        EXTERNAL => Some("EXT"),
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
    fn named_ids_are_the_table_indices_of_their_names() {
        for (index, name) in [
            (id::NOP, "nop"),
            (id::CLIPW, "clipw"),
            (id::FCAND, "fcand"),
            (id::FCEQ, "fceq"),
            (id::FCGET, "fcget"),
            (id::FCOR, "fcor"),
            (id::FCSET, "fcset"),
            (id::IN_HW_CLIP, "in_hw_clip"),
            (id::OUT_HW_CLIP, "out_hw_clip"),
            (id::ADD, "add"),
            (id::ADDX, "addx"),
            (id::ADDY, "addy"),
            (id::ADDZ, "addz"),
            (id::SUB, "sub"),
            (id::MULW, "mulw"),
            (id::IADD, "iadd"),
            (id::IADDI, "iaddi"),
            (id::IADDIU, "iaddiu"),
            (id::ISUB, "isub"),
            (id::ISUBIU, "isubiu"),
            (id::MOVE, "move"),
            (id::LQ, "lq"),
            (id::ILW, "ilw"),
            (id::LQI, "lqi"),
            (id::LQD, "lqd"),
            (id::SQ, "sq"),
            (id::ISW, "isw"),
            (id::SQI, "sqi"),
            (id::SQD, "sqd"),
            (id::XGKICK, "xgkick"),
            (id::XTOP, "xtop"),
            (id::XITOP, "xitop"),
            (id::IMOVE, "imove"),
            (id::LIMOVE, "limove"),
            (id::VMOVE, "vmove"),
            (id::LVMOVE, "lvmove"),
            (id::UVMOVE, "uvmove"),
        ] {
            assert_eq!(lookup(name, Syntax::Old).unwrap().index, index, "{name}");
        }
        assert!(
            INSTRUCTIONS[..usize::from(id::GENERALIZED_END)]
                .iter()
                .all(|instruction| instruction.hook == Hook::Fmac)
        );
        assert_ne!(
            INSTRUCTIONS[usize::from(id::GENERALIZED_END)].hook,
            Hook::Fmac
        );
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
                .any(|operand| operand.class == class::MAC && operand.implicit)
        );
        let div = lookup("div", Syntax::Old).unwrap();
        assert_eq!(div.unit_mask, unit::DIVIDE);
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
