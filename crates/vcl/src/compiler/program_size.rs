//! The `size` fix (docs/VCL_BUGS.md, F5): programs too large for micro
//! memory.
//!
//! A VU branch encodes its target as an 11-bit signed offset, in
//! instructions, added to the address of the instruction in its delay slot
//! (VU User's Manual, B and the conditional branches). vcl 1.4beta7 emits
//! labels and leaves the encoding to the assembler, which keeps the low 11
//! bits of a farther offset. The program counter wraps around VU1's 2048
//! instructions of micro memory, so such a branch still lands on its target
//! as long as the program fits. A program that does not fit is the real
//! failure, and with the fix it gets a warning.

use super::*;

/// VU1 micro memory, in instructions.
pub(super) const MICRO_MEMORY: usize = 2048;

/// The warnings for a compiled listing.
pub(super) fn check(output: &Compiled) -> Vec<Diagnostic> {
    if output.instruction_pairs > MICRO_MEMORY {
        vec![Diagnostic::warning(CompileMessage::ProgramTooLarge {
            rows: output.instruction_pairs,
        })]
    } else {
        Vec::new()
    }
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_programs_larger_than_micro_memory_are_reported() {
        let sized = |instruction_pairs| Compiled {
            instruction_pairs,
            ..Compiled::default()
        };
        assert!(check(&sized(2048)).is_empty());
        let warnings: Vec<_> = check(&sized(2049)).into_iter().map(|d| d.message).collect();
        assert_eq!(
            warnings,
            [Message::from(CompileMessage::ProgramTooLarge {
                rows: 2049
            })]
        );
    }
}
