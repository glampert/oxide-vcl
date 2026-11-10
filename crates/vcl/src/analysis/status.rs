//! Current/sticky VU status resources and their implicit source interface.
//!
//! The original's dependency binding explicitly appends `xin_hw`
//! definitions for global registers 0x39/0x3a to each ENTER stream. Its helper
//! constructs instruction ID 0xbe and binds that output operand.
//! These are inherited runtime values, not literal zero initialization. Fresh
//! ALU/divide status and MAC are not supplied by that implicit interface.
//!
//! Accesses below come from `prune::expand`, the semantic reconstruction of
//! the original's status patching. Reads and writes are intentionally separate.
//! In particular, arithmetic reads the current sticky token but does not define
//! a new sticky token; FSSET and sticky FSTST operations define those tokens.

use crate::isa::instruction::Direction;
use crate::parser::Instruction;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resource {
    Alu,
    Divide,
    StickyAlu,
    StickyDivide,
    Mac,
}

impl Resource {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Alu => "STAT_ALU",
            Self::Divide => "STAT_DIV",
            Self::StickyAlu => "STAT_ALU_S",
            Self::StickyDivide => "STAT_DIV_S",
            Self::Mac => "MAC",
        }
    }

    pub const fn original_global_id(self) -> u8 {
        match self {
            Self::Alu => 0x37,
            Self::Divide => 0x38,
            Self::StickyAlu => 0x39,
            Self::StickyDivide => 0x3a,
            Self::Mac => 0x3b,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Alu,
            Self::Divide,
            Self::StickyAlu,
            Self::StickyDivide,
            Self::Mac,
        ]
        .into_iter()
        .find(|resource| name.eq_ignore_ascii_case(resource.name()))
    }

    pub const fn is_implicit_entry_input(self) -> bool {
        matches!(self, Self::StickyAlu | Self::StickyDivide)
    }
}

const ENTRY_INPUTS: [Resource; 2] = [Resource::StickyAlu, Resource::StickyDivide];

/// Opaque inherited status values declared by the original at each ENTER.
/// Do not seed these as constants: FSSET/FSTST writes must replace them and
/// later arithmetic must retain a dependency on the reaching token definition.
pub const fn implicit_entry_inputs() -> &'static [Resource; 2] {
    &ENTRY_INPUTS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub resource: Resource,
    pub direction: Direction,
    pub components: u8,
    pub latency: i16,
    pub blocking: bool,
    pub source_operand: usize,
    /// Index in `prune::expand(instruction).operands`, suitable for a UseEdge.
    pub expanded_operand: usize,
}

/// Effective status operands, including current and sticky FSTST selection.
/// The compiler can bind each Resource to its own ordinary flow-register ID,
/// declare `implicit_entry_inputs` at ENTER, then use normal reaching-definition
/// and read-before-write ordering for these accesses. No invented history edge
/// to every earlier arithmetic instruction is added by this helper.
pub fn accesses(instruction: &Instruction) -> Vec<Access> {
    crate::analysis::prune::expand(instruction)
        .operands
        .iter()
        .enumerate()
        .filter_map(|(index, operand)| {
            Resource::from_name(operand.register.as_deref()?).map(|resource| Access {
                resource,
                direction: operand.direction,
                components: operand.mask,
                latency: operand.latency,
                blocking: operand.blocking,
                source_operand: operand.source_operand,
                expanded_operand: index,
            })
        })
        .collect()
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::instruction::Syntax;
    use crate::parser::{self, ParserOptions, StatementKind};

    fn instruction(source: &str) -> Instruction {
        parser::parse_with_options(
            source,
            ParserOptions {
                syntax: Syntax::Old,
                allow_incomplete: true,
            },
        )
        .unwrap()
        .statements
        .into_iter()
        .find_map(|statement| match statement.kind {
            StatementKind::Instruction(instruction) => Some(instruction),
            _ => None,
        })
        .unwrap()
    }

    fn resources(source: &str) -> Vec<(Resource, Direction)> {
        accesses(&instruction(source))
            .into_iter()
            .map(|access| (access.resource, access.direction))
            .collect()
    }

    fn fixture(name: &str, file: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .join("local/evidence/oracle")
                .join(name)
                .join(file),
        )
        .unwrap()
    }

    fn pairs(name: &str) -> Vec<(String, String)> {
        fixture(name, "stdout.bin")
            .lines()
            .filter(|line| line.starts_with("         ") && line.len() > 68)
            .map(|line| {
                (
                    line[9..68].split_whitespace().collect::<Vec<_>>().join(" "),
                    line[68..]
                        .split(';')
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            })
            .collect()
    }

    #[test]
    fn implicit_entry_inputs_are_sticky_values_not_current_results() {
        assert_eq!(
            implicit_entry_inputs(),
            &[Resource::StickyAlu, Resource::StickyDivide]
        );
        assert_eq!(
            implicit_entry_inputs().map(|resource| resource.original_global_id()),
            [0x39, 0x3a]
        );
        assert!(!Resource::Alu.is_implicit_entry_input());
        assert!(!Resource::Divide.is_implicit_entry_input());
        assert!(!Resource::Mac.is_implicit_entry_input());
        assert_eq!(Resource::from_name("stat_alu_s"), Some(Resource::StickyAlu));
    }

    #[test]
    fn arithmetic_defines_current_status_and_reads_the_inherited_or_reset_token() {
        let access = accesses(&instruction("add b,a,c"));
        assert_eq!(
            access
                .iter()
                .map(|a| (a.resource, a.direction))
                .collect::<Vec<_>>(),
            vec![
                (Resource::Mac, Direction::Write),
                (Resource::Alu, Direction::Write),
                (Resource::StickyAlu, Direction::Read),
            ]
        );
        assert_eq!(access[1].latency, 4);
        assert_eq!(access[2].latency, -1);
        assert!(access[2].blocking);
        assert!(
            !access
                .iter()
                .any(|a| a.resource == Resource::StickyAlu && a.direction == Direction::Write)
        );
    }

    #[test]
    fn divide_uses_a_separate_sticky_token_and_preserves_its_latency() {
        let access = accesses(&instruction("div Q,a.x,c.y"));
        assert_eq!(
            access
                .iter()
                .map(|a| (a.resource, a.direction))
                .collect::<Vec<_>>(),
            vec![
                (Resource::Divide, Direction::Write),
                (Resource::StickyDivide, Direction::Read),
            ]
        );
        assert_eq!(access[0].latency, 7);
        assert_eq!(accesses(&instruction("rsqrt Q,a.x,c.y"))[0].latency, 13);
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn fsset_replaces_sticky_tokens_without_defining_current_status() {
        let access = accesses(&instruction("fsset 0"));
        assert_eq!(
            access
                .iter()
                .map(|a| (a.resource, a.direction, a.latency))
                .collect::<Vec<_>>(),
            vec![
                (Resource::StickyAlu, Direction::Write, 1),
                (Resource::StickyDivide, Direction::Write, 1),
            ]
        );
        let result = fixture("status-fsset-only-sticky", "result.json");
        assert!(result.contains("\"exit_status\": 255"));
        assert!(fixture("status-fsset-only-sticky", "stderr.bin").contains("STAT_ALU.0"));
    }

    #[test]
    fn sticky_test_requires_fresh_current_status_and_updates_its_token() {
        assert_eq!(
            resources("fsand vi01,0x400"),
            vec![
                (Resource::Alu, Direction::Read),
                (Resource::StickyAlu, Direction::Read),
                (Resource::StickyAlu, Direction::Write),
            ]
        );
        assert_eq!(
            resources("fsand vi01,0x100"),
            vec![
                (Resource::Divide, Direction::Read),
                (Resource::StickyDivide, Direction::Read),
                (Resource::StickyDivide, Direction::Write),
            ]
        );
        assert_eq!(
            resources("fsand vi01,1"),
            vec![(Resource::Alu, Direction::Read)]
        );
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn reset_before_arithmetic_is_ordered_but_reset_after_can_share_a_bundle() {
        let before = pairs("status-fsset-before-add");
        assert!(before[0].1.starts_with("fsset "));
        assert!(before[1].0.starts_with("add "));
        let after = pairs("status-fsset-after-add");
        assert!(after[0].0.starts_with("add "));
        assert!(after[0].1.starts_with("fsset "));
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn repeated_sticky_reads_order_the_next_arithmetic_after_the_first_read() {
        let rows = pairs("status-two-sticky-reads");
        let first = rows
            .iter()
            .position(|(_, lower)| lower.starts_with("fsand VI01,"))
            .unwrap();
        let arithmetic = rows
            .iter()
            .position(|(upper, _)| upper.starts_with("sub "))
            .unwrap();
        let second = rows
            .iter()
            .position(|(_, lower)| lower.starts_with("fsand VI02,"))
            .unwrap();
        assert!(first < arithmetic && arithmetic < second);
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn original_prunes_an_earlier_unconsumed_current_writer_even_for_sticky_tests() {
        for (case, removed) in [
            ("status-two-alu-sticky", "{add}"),
            ("status-two-div-sticky", "{sqrt}"),
        ] {
            let diagnostics = fixture(case, "stderr.bin");
            assert!(
                diagnostics.contains("REMOVE INST:") && diagnostics.contains(removed),
                "{case}"
            );
            assert!(
                fixture(case, "result.json").contains("\"exit_status\": 0"),
                "{case}"
            );
        }
    }

    #[test]
    #[cfg_attr(not(local_evidence), ignore = "needs local/evidence")]
    fn a_join_still_requires_a_current_writer_on_every_incoming_path() {
        assert!(
            fixture("status-branch-missing-current", "result.json")
                .contains("\"exit_status\": 255")
        );
        assert!(
            fixture("status-branch-missing-current", "stderr.bin").contains("UBS: done STAT_ALU.0")
        );
        assert!(
            fixture("status-branch-current-join", "result.json").contains("\"exit_status\": 0")
        );
    }
}
