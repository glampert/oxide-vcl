//! Small control-flow predicates recovered from the supplied VCL ELF.
//!
//! Stream end kinds, exit folding and lower-pipe end safety follow the
//! original. These predicates do
//! not merge or mutate a caller's graph: analysis edges and emission visibility
//! must remain separate, as they do in the original program.

use crate::isa::instruction::{id, subtype::branch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamEnd {
    Ordinary = 1,
    Continue = 2,
    Exit = 3,
    ExitMacro = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LinkKind {
    Fallthrough = 0,
    Direct = 1,
    Indirect = 2,
    DirectLink = 3,
    IndirectLink = 4,
}

/// Translate a branch descriptor's subtype as the original does. CONDITIONAL
/// does not change the taken edge's kind; a conditional also has a separate
/// fallthrough edge.
pub const fn branch_link_kind(subtype: u8) -> LinkKind {
    const INDIRECT_LINK: u8 = branch::REGISTER | branch::LINK;
    if subtype == 0 {
        LinkKind::Fallthrough
    } else {
        match subtype & INDIRECT_LINK {
            branch::REGISTER => LinkKind::Indirect,
            branch::LINK => LinkKind::DirectLink,
            INDIRECT_LINK => LinkKind::IndirectLink,
            _ => LinkKind::Direct,
        }
    }
}

/// The original's stream lookup ultimately uses a comparison that folds
/// ASCII letters. Keep source label
/// spelling for emission while using this comparison for branch resolution.
pub fn same_label(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// A simplified last-lower-slot view, sufficient for the exact end-padding
/// predicate. Descriptor indices are the original 214-entry table indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LowerInstruction {
    pub descriptor_index: u16,
    pub memory: bool,
    pub branch: bool,
}

impl LowerInstruction {
    pub const fn ordinary(descriptor_index: u16) -> Self {
        Self {
            descriptor_index,
            memory: false,
            branch: false,
        }
    }
}

/// The original's lower-pipe restriction for an ordinary E termination.
///
/// The last row may not contain a lower memory/branch instruction, XGKICK,
/// XTOP, or XITOP; the penultimate row may not contain XGKICK. Empty rows are
/// represented by `None`. This checks an already allocated VSM table, not
/// source-level raw instruction spelling.
pub fn safe_end(lower_slots: &[Option<LowerInstruction>]) -> bool {
    if let Some(Some(last)) = lower_slots.last()
        && (last.memory || last.branch || (id::XGKICK..=id::XITOP).contains(&last.descriptor_index))
    {
        return false;
    }
    if let Some(Some(previous)) = lower_slots.iter().rev().nth(1)
        && previous.descriptor_index == id::XGKICK
    {
        return false;
    }
    true
}

/// Number of empty issue rows the end-tidying pass must append before assigning an E bit.
///
/// This directly models the original's extension loop: ordinary code has
/// at least two rows and must satisfy [`safe_end`]. The E bit belongs on the
/// penultimate row after padding. Macro exits do not use this rule.
pub fn required_exit_padding(lower_slots: &[Option<LowerInstruction>]) -> usize {
    let mut padded = lower_slots.to_vec();
    while padded.len() < 2 || !safe_end(&padded) {
        padded.push(None);
    }
    padded.len() - lower_slots.len()
}

/// Tidy folds an ordinary stream into an E exit if every outgoing stream is
/// itself an ordinary synthetic exit. Edges remain in the flow graph but are
/// marked invisible for assembly emission; any branch in the folded stream
/// is suppressed. A macro exit is intentionally not an ordinary exit.
pub fn can_fold_into_exit(current: StreamEnd, successors: &[StreamEnd]) -> bool {
    current == StreamEnd::Ordinary
        && !successors.is_empty()
        && successors.iter().all(|&end| end == StreamEnd::Exit)
}

/// Facts needed by the merge gate. The original also disposes
/// obsolete input tables before this gate, then concatenates dispatch entries
/// and retargets edges. A caller should recompute these facts after each merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeFacts {
    pub current_is_emittable_linear_vsm: bool,
    pub next_is_emittable_linear_vsm: bool,
    pub current_end: StreamEnd,
    pub next_has_ordinary_start: bool,
    pub current_successors: usize,
    pub next_predecessors: usize,
    pub current_zero_edge_targets_next: bool,
    pub current_has_branch: bool,
    pub current_end_fence: bool,
    pub next_start_fence: bool,
    pub current_debug_end: bool,
    pub next_debug_top: bool,
    pub next_has_exported_label: bool,
}

pub fn can_merge_adjacent(facts: MergeFacts) -> bool {
    facts.current_is_emittable_linear_vsm
        && facts.next_is_emittable_linear_vsm
        && facts.current_end == StreamEnd::Ordinary
        && facts.next_has_ordinary_start
        && facts.current_successors == 1
        && facts.next_predecessors == 1
        && facts.current_zero_edge_targets_next
        && !facts.current_has_branch
        && !facts.current_end_fence
        && !facts.next_start_fence
        && !facts.current_debug_end
        && !facts.next_debug_top
        && !facts.next_has_exported_label
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_subtypes_keep_conditional_and_link_distinct() {
        use branch::*;
        assert_eq!(branch_link_kind(0), LinkKind::Fallthrough);
        assert_eq!(branch_link_kind(JUMP), LinkKind::Direct);
        assert_eq!(branch_link_kind(JUMP | REGISTER), LinkKind::Indirect);
        assert_eq!(branch_link_kind(JUMP | CONDITIONAL), LinkKind::Direct);
        assert_eq!(branch_link_kind(JUMP | LINK), LinkKind::DirectLink);
        assert_eq!(
            branch_link_kind(JUMP | REGISTER | LINK),
            LinkKind::IndirectLink
        );
    }

    #[test]
    fn labels_fold_only_ascii_case() {
        assert!(same_label("Done", "done"));
        assert!(!same_label("Done", "done_more"));
        assert!(!same_label("Ä", "ä"));
    }

    #[test]
    fn ordinary_end_has_two_rows() {
        assert_eq!(required_exit_padding(&[]), 2);
        assert_eq!(required_exit_padding(&[None]), 1);
        assert_eq!(required_exit_padding(&[None, None]), 0);
        assert_eq!(
            required_exit_padding(&[Some(LowerInstruction::ordinary(111)), None]),
            0
        );
    }

    #[test]
    fn final_memory_and_branch_need_one_extra_row() {
        let memory = LowerInstruction {
            memory: true,
            ..LowerInstruction::ordinary(155)
        };
        let branch = LowerInstruction {
            branch: true,
            ..LowerInstruction::ordinary(172)
        };
        assert_eq!(required_exit_padding(&[None, Some(memory)]), 1);
        assert_eq!(required_exit_padding(&[None, Some(branch)]), 1);
    }

    #[test]
    fn special_lower_end_hazards_keep_their_different_distances() {
        assert_eq!(
            required_exit_padding(&[None, Some(LowerInstruction::ordinary(165))]),
            2
        );
        assert_eq!(
            required_exit_padding(&[None, Some(LowerInstruction::ordinary(166))]),
            1
        );
        assert_eq!(
            required_exit_padding(&[None, Some(LowerInstruction::ordinary(167))]),
            1
        );
        assert_eq!(
            required_exit_padding(&[Some(LowerInstruction::ordinary(165)), None]),
            1
        );
        assert_eq!(
            required_exit_padding(&[Some(LowerInstruction::ordinary(166)), None]),
            0
        );
    }

    #[test]
    fn fold_requires_a_nonempty_set_of_plain_exit_successors() {
        assert!(can_fold_into_exit(StreamEnd::Ordinary, &[StreamEnd::Exit]));
        assert!(can_fold_into_exit(
            StreamEnd::Ordinary,
            &[StreamEnd::Exit, StreamEnd::Exit]
        ));
        assert!(!can_fold_into_exit(StreamEnd::Ordinary, &[]));
        assert!(!can_fold_into_exit(
            StreamEnd::Ordinary,
            &[StreamEnd::ExitMacro]
        ));
        assert!(!can_fold_into_exit(StreamEnd::Continue, &[StreamEnd::Exit]));
        assert!(!can_fold_into_exit(
            StreamEnd::Ordinary,
            &[StreamEnd::Exit, StreamEnd::Ordinary]
        ));
    }

    #[test]
    fn merge_preserves_join_export_debug_and_fence_boundaries() {
        let yes = MergeFacts {
            current_is_emittable_linear_vsm: true,
            next_is_emittable_linear_vsm: true,
            current_end: StreamEnd::Ordinary,
            next_has_ordinary_start: true,
            current_successors: 1,
            next_predecessors: 1,
            current_zero_edge_targets_next: true,
            current_has_branch: false,
            current_end_fence: false,
            next_start_fence: false,
            current_debug_end: false,
            next_debug_top: false,
            next_has_exported_label: false,
        };
        assert!(can_merge_adjacent(yes));
        assert!(!can_merge_adjacent(MergeFacts {
            next_predecessors: 2,
            ..yes
        }));
        assert!(!can_merge_adjacent(MergeFacts {
            next_has_exported_label: true,
            ..yes
        }));
        assert!(!can_merge_adjacent(MergeFacts {
            current_has_branch: true,
            ..yes
        }));
        assert!(!can_merge_adjacent(MergeFacts {
            current_debug_end: true,
            ..yes
        }));
        assert!(!can_merge_adjacent(MergeFacts {
            next_start_fence: true,
            ..yes
        }));
    }
}
