//! Work-area and memory-dependency facts recovered from the supplied ELF.
//!
//! The shipped Marigold CPU profile does **not** install the memory register
//! pool. Every valid `.vu_work_area` request therefore fails in vcl mode.
//! The dormant constructor is modeled separately for analysis;
//! its allocation methods simply return success without doing work.
//! See `docs/MEMORY.md`. This module models vcl only; oxide mode's spilling
//! is `compiler/spill.rs`.

use std::fmt;

pub const MEMORY_POOL_CLASS: u8 = 11;
pub const FIRST_MEMORY_REGISTER_ID: u32 = 10_000;

/// The exact nonzero bootstrap records, in original order.
pub const MARIGOLD_REGISTER_CLASSES: [u8; 3] = [9, 8, 10];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Device {
    Vu0,
    Vu1,
}

impl Device {
    pub const fn capacity(self) -> u32 {
        match self {
            Self::Vu0 => 256,
            Self::Vu1 => 1024,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Vu0 => "vu0",
            Self::Vu1 => "vu1",
        }
    }

    pub fn parse(source: &str) -> Result<Self, MemoryError> {
        if source.eq_ignore_ascii_case("vu0") {
            Ok(Self::Vu0)
        } else if source.eq_ignore_ascii_case("vu1") {
            Ok(Self::Vu1)
        } else if source.eq_ignore_ascii_case("core") {
            Err(MemoryError::CoreUnsupported)
        } else {
            Err(MemoryError::UnknownDevice)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryError {
    CoreUnsupported,
    UnknownDevice,
    EmptyArea,
    NegativeAddress,
    AreaOverrun,
    MissingVuMemoryPool,
    InvalidComponentMask(u8),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CoreUnsupported => "work: core cpu work area.. not yet",
            Self::UnknownDevice => {
                "work: target device not set defining a work area.. must be one of core,vu0 or vu1!"
            }
            Self::EmptyArea => "size is <=0 for a work area",
            Self::NegativeAddress => "ea is negative for a work area",
            Self::AreaOverrun => "work area overruns size of device",
            Self::MissingVuMemoryPool => "init work area - failed to get vu mem pool",
            Self::InvalidComponentMask(_) => "invalid memory component mask",
        })
    }
}

impl std::error::Error for MemoryError {}

/// A validated directive request. Validation is separate from activation:
/// successful bounds checking does not mean the shipped profile supports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkAreaRequest {
    pub device: Device,
    pub address: u32,
    pub size: u32,
    /// Constructor names use the original base token, not its decimal value.
    pub base_token: String,
}

impl WorkAreaRequest {
    pub fn parse(device: &str, base_token: &str, size_token: &str) -> Result<Self, MemoryError> {
        Self::validate(
            Device::parse(device)?,
            decimal_prefix(base_token),
            decimal_prefix(size_token),
            base_token,
        )
    }

    pub fn from_parsed(device: &str, address: i32, size: u32) -> Result<Self, MemoryError> {
        let device = Device::parse(device)?;
        if size == 0 {
            return Err(MemoryError::EmptyArea);
        }
        if address < 0 {
            return Err(MemoryError::NegativeAddress);
        }
        if u64::from(address as u32) + u64::from(size) > u64::from(device.capacity()) {
            return Err(MemoryError::AreaOverrun);
        }
        Ok(Self {
            device,
            address: address as u32,
            size,
            base_token: address.to_string(),
        })
    }

    fn validate(
        device: Device,
        address: i32,
        size: i32,
        base_token: &str,
    ) -> Result<Self, MemoryError> {
        // Match the handler's error priority: size is checked before address.
        if size < 1 {
            return Err(MemoryError::EmptyArea);
        }
        if address < 0 {
            return Err(MemoryError::NegativeAddress);
        }
        if i64::from(address) + i64::from(size) > i64::from(device.capacity()) {
            return Err(MemoryError::AreaOverrun);
        }
        Ok(Self {
            device,
            address: address as u32,
            size: size as u32,
            base_token: base_token.into(),
        })
    }
}

/// Actual shipped-profile behavior: the bootstrap never installs class 11.
pub fn activate_marigold(_request: &WorkAreaRequest) -> Result<DormantPool, MemoryError> {
    Err(MemoryError::MissingVuMemoryPool)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySlot {
    pub id: u32,
    pub offset: u32,
    pub name: String,
    pub components: u8,
}

/// Static reconstruction of unreachable `memRegPool::InitMemArea` code.
/// Calling this API explicitly explores the constructor; it does not represent
/// acceptance of a source directive by the supplied executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DormantPool {
    pub area: WorkAreaRequest,
    pub slots: Vec<MemorySlot>,
}

impl DormantPool {
    pub fn initialize(area: WorkAreaRequest) -> Result<Self, MemoryError> {
        if area.size == 0 {
            return Err(MemoryError::EmptyArea);
        }
        if u64::from(area.address) + u64::from(area.size) > u64::from(area.device.capacity()) {
            return Err(MemoryError::AreaOverrun);
        }
        let slots = (0..area.size)
            .map(|offset| MemorySlot {
                id: FIRST_MEMORY_REGISTER_ID + offset,
                offset,
                name: format!("({}+{offset})(vi00)", area.base_token),
                components: 4,
            })
            .collect();
        Ok(Self { area, slots })
    }

    /// The original's allocation method unconditionally returns 1.
    pub const fn assign_external(&self) -> bool {
        true
    }
    /// The original's allocation method unconditionally returns 1.
    pub const fn discard_disjoint(&self) -> bool {
        true
    }
    /// The original's allocation method unconditionally returns 1.
    pub const fn assign_local(&self) -> bool {
        true
    }
}

/// Tagged memory dependencies use hardware-pool virtual resources, not the
/// dormant memory register pool. Loads read a tag; stores define it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryDirection {
    Load,
    Store,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagEffect {
    pub name: String,
    pub direction: MemoryDirection,
    pub components: u8,
    pub latency: u8,
}

/// Semantic records added when the original binds dependencies.
/// Name validation/interning remains the hardware register pool's job.
pub fn tag_effects(
    direction: MemoryDirection,
    components: u8,
    tags: &[String],
) -> Result<Vec<TagEffect>, MemoryError> {
    if components == 0 || components & !15 != 0 {
        return Err(MemoryError::InvalidComponentMask(components));
    }
    Ok(tags
        .iter()
        .map(|name| TagEffect {
            name: name.clone(),
            direction,
            components,
            latency: match direction {
                MemoryDirection::Load => 0,
                MemoryDirection::Store => 1,
            },
        })
        .collect())
}

/// The handler calls the i386 libc decimal strtol wrapper.
fn decimal_prefix(source: &str) -> i32 {
    let mut bytes = source.trim_start().bytes().peekable();
    let negative = match bytes.peek() {
        Some(b'-') => {
            bytes.next();
            true
        }
        Some(b'+') => {
            bytes.next();
            false
        }
        _ => false,
    };
    let limit = if negative {
        2_147_483_648_u64
    } else {
        2_147_483_647
    };
    let mut magnitude = 0_u64;
    for digit in bytes.take_while(u8::is_ascii_digit) {
        magnitude = (magnitude * 10 + u64::from(digit - b'0')).min(limit);
    }
    if negative {
        (-(magnitude as i64)) as i32
    } else {
        magnitude as i32
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_has_no_memory_register_pool() {
        assert_eq!(MARIGOLD_REGISTER_CLASSES, [9, 8, 10]);
        assert!(!MARIGOLD_REGISTER_CLASSES.contains(&MEMORY_POOL_CLASS));
        let request = WorkAreaRequest::parse("vu1", "0", "32").unwrap();
        assert_eq!(
            activate_marigold(&request),
            Err(MemoryError::MissingVuMemoryPool)
        );
    }

    #[test]
    fn request_bounds_use_units_of_the_selected_device() {
        assert!(WorkAreaRequest::parse("VU0", "255", "1").is_ok());
        assert!(WorkAreaRequest::parse("vu1", "0", "1024").is_ok());
        assert_eq!(
            WorkAreaRequest::parse("vu0", "255", "2"),
            Err(MemoryError::AreaOverrun)
        );
        assert_eq!(
            WorkAreaRequest::parse("vu1", "1024", "1"),
            Err(MemoryError::AreaOverrun)
        );
        assert_eq!(
            WorkAreaRequest::parse("core", "0", "1"),
            Err(MemoryError::CoreUnsupported)
        );
        assert_eq!(
            WorkAreaRequest::parse("vu2", "0", "1"),
            Err(MemoryError::UnknownDevice)
        );
    }

    #[test]
    fn numeric_tokens_use_decimal_prefix_and_preserve_base_source() {
        let request = WorkAreaRequest::parse("vu0", "0x20", "4tail").unwrap();
        assert_eq!(request.address, 0);
        assert_eq!(request.size, 4);
        assert_eq!(request.base_token, "0x20");
        assert_eq!(
            WorkAreaRequest::parse("vu0", "BASE_LABEL", "4")
                .unwrap()
                .address,
            0
        );
        assert_eq!(decimal_prefix("21474836499999999999999999"), i32::MAX);
        assert_eq!(decimal_prefix("-21474836499999999999999999"), i32::MIN);
        assert_eq!(decimal_prefix("--1"), 0);
    }

    #[test]
    fn error_priority_matches_directive_handler() {
        assert_eq!(
            WorkAreaRequest::parse("vu0", "-1", "0"),
            Err(MemoryError::EmptyArea)
        );
        assert_eq!(
            WorkAreaRequest::parse("vu0", "-1", "1"),
            Err(MemoryError::NegativeAddress)
        );
        assert_eq!(
            MemoryError::MissingVuMemoryPool.to_string(),
            "init work area - failed to get vu mem pool"
        );
    }

    #[test]
    fn dormant_constructor_retains_source_base_and_physical_slot_ids() {
        let pool =
            DormantPool::initialize(WorkAreaRequest::parse("vu0", "0x20", "2").unwrap()).unwrap();
        assert_eq!(
            pool.slots[0],
            MemorySlot {
                id: 10000,
                offset: 0,
                name: "(0x20+0)(vi00)".into(),
                components: 4
            }
        );
        assert_eq!(pool.slots[1].name, "(0x20+1)(vi00)");
        assert_eq!(pool.slots[1].id, 10001);
        assert!(pool.assign_external() && pool.discard_disjoint() && pool.assign_local());
    }

    #[test]
    fn tags_are_component_resources_independent_of_workarea() {
        let tags = vec!["array".into(), "other".into()];
        let reads = tag_effects(MemoryDirection::Load, 7, &tags).unwrap();
        assert_eq!(
            reads[0],
            TagEffect {
                name: "array".into(),
                direction: MemoryDirection::Load,
                components: 7,
                latency: 0
            }
        );
        let writes = tag_effects(MemoryDirection::Store, 1, &tags).unwrap();
        assert_eq!(writes[1].latency, 1);
        assert!(tag_effects(MemoryDirection::Store, 0, &tags).is_err());
        assert!(tag_effects(MemoryDirection::Store, 16, &tags).is_err());
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn retained_original_workarea_probes_all_reject_without_emulator_failures() {
        for name in [
            "memory-area-vu0",
            "memory-area-vu1-max",
            "memory-area-hex-base",
            "memory-area-symbol-base",
            "memory-area-size-prefix",
        ] {
            let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .join("local/evidence/oracle")
                .join(name);
            let stderr = std::fs::read_to_string(root.join("stderr.bin")).unwrap();
            assert!(
                stderr.contains("init work area - failed to get vu mem pool"),
                "{name}"
            );
            let result = std::fs::read_to_string(root.join("result.json")).unwrap();
            assert!(result.contains("\"emulator_error\": false"), "{name}");
        }
    }
}
