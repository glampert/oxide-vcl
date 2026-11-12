//! The `clip` fix (docs/VCL_BUGS.md, F3): clip history that no instruction
//! observes need not be declared.
//!
//! CLIPW shifts the earlier clip judgments up by six bits, so vcl 1.4beta7
//! counts it as a read of the whole history (`CLIPa`): a program that runs
//! CLIPW without `in_hw_clip` is rejected with "register CLIPa is used
//! before set", even when it tests only judgments its own CLIPWs made
//! (observed from vcl 1.4beta7, `openvcl__repros_clipw_implicit_w`).
//!
//! With the fix, those reads are harmless when every instruction that looks
//! at the clip flag sees only defined judgments. Judgment `j` (bits
//! `6j..6j+5`, the current one being 0) is defined on a path when at least
//! `j + 1` CLIPWs ran on it since the region's entry, or when FCSET set the
//! whole flag. The region is then compiled as if its `--enter` declared
//! `in_hw_clip CLIP`, so it schedules as the declared program does.

use super::*;
use crate::isa::instruction::implicit_register;

/// The clip flag's width: four judgments of six bits.
const JUDGMENTS: u32 = 4;
const BITS: u32 = 6;
const ALL_BITS: u32 = (1 << (JUDGMENTS * BITS)) - 1;

/// The reads (indices into `analysis.reads`) of undeclared clip history
/// that the `clip` fix may ignore: CLIPW's reads of `CLIPa` with no
/// definition on some path, when nothing in the region observes a judgment
/// that is undefined on some path. Empty when some instruction does.
pub(super) fn unobserved_reads(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
) -> BTreeSet<usize> {
    let history = |read: &flow::Read| {
        program
            .registers
            .named(read.register)
            .name
            .eq_ignore_ascii_case(implicit_register(class::CLIP_ALL).unwrap_or_default())
    };
    let undeclared: BTreeSet<usize> = analysis
        .reads
        .iter()
        .enumerate()
        .filter(|(_, read)| {
            read.definitions.contains(&Definition::Uninitialized)
                && history(read)
                && opcode(region, read.operation) == id::CLIPW
        })
        .map(|(index, _)| index)
        .collect();
    if undeclared.is_empty() {
        return undeclared;
    }

    let defined = defined_judgments(program, region, analysis);
    let observed_defined = analysis
        .reads
        .iter()
        .filter(|read| is_clip(program, read))
        .all(|read| {
            let mask = observed_bits(region, read.operation);
            let judgments = read
                .definitions
                .iter()
                .map(|definition| judgments_after(definition, &defined))
                .min()
                .unwrap_or(0);
            mask >> (judgments * BITS) == 0
        });
    if observed_defined {
        undeclared
    } else {
        BTreeSet::new()
    }
}

/// Whether a read is of the clip flag or its history.
fn is_clip(program: &Source, read: &flow::Read) -> bool {
    let name = &program.registers.named(read.register).name;
    [class::CLIP, class::CLIP_ALL]
        .into_iter()
        .filter_map(implicit_register)
        .any(|register| name.eq_ignore_ascii_case(register))
}

fn opcode(region: &Region, operation: usize) -> u16 {
    region.instructions[operation].instruction.metadata.index
}

/// How many judgments each clip-flag writer leaves defined, on every path
/// that reaches it: CLIPW adds one to the fewest that reach its read of the
/// history, FCSET and `in_hw_clip` define all four. Computed from the top
/// down, so a loop's CLIPW settles at what its entry path gives it.
fn defined_judgments(
    program: &Source,
    region: &Region,
    analysis: &flow::Analysis,
) -> BTreeMap<usize, u32> {
    let mut defined: BTreeMap<usize, u32> = BTreeMap::new();
    for read in &analysis.reads {
        defined.entry(read.operation).or_insert(JUDGMENTS);
    }
    let history_reads: Vec<&flow::Read> = analysis
        .reads
        .iter()
        .filter(|read| is_clip(program, read) && opcode(region, read.operation) == id::CLIPW)
        .collect();

    loop {
        let mut changed = false;
        for read in &history_reads {
            let before = read
                .definitions
                .iter()
                .map(|definition| judgments_after(definition, &defined))
                .min()
                .unwrap_or(0);
            let after = (before + 1).min(JUDGMENTS);
            let entry = defined.entry(read.operation).or_insert(JUDGMENTS);
            if after < *entry {
                *entry = after;
                changed = true;
            }
        }
        if !changed {
            return defined;
        }
    }
}

/// The judgments a definition of the clip flag leaves defined.
fn judgments_after(definition: &Definition, defined: &BTreeMap<usize, u32>) -> u32 {
    match definition {
        Definition::Uninitialized => 0,
        Definition::Input => JUDGMENTS,
        // FCSET, `in_hw_clip` and every other writer define the whole flag.
        Definition::Operation(operation) => defined.get(operation).copied().unwrap_or(JUDGMENTS),
    }
}

/// The clip-flag bits an instruction's result depends on (VU User's Manual,
/// FCAND, FCOR, FCEQ, FCGET). FCAND tests the bits its mask sets, FCOR the
/// bits its mask clears, FCGET the low twelve. CLIPW only shifts the
/// history, which the judgment count accounts for. Anything else, and a
/// mask that is not a number, is taken to see the whole flag.
fn observed_bits(region: &Region, operation: usize) -> u32 {
    let instruction = &region.instructions[operation].instruction;
    let mask = || {
        instruction
            .operands
            .iter()
            .find_map(|operand| operand.immediate.as_deref())
            .and_then(|text| crate::parser::expression::evaluate(text).ok())
            .and_then(|value| value.number())
            .map(|value| value.to_i32() as u32 & ALL_BITS)
    };
    match instruction.metadata.index {
        id::CLIPW => 0,
        id::FCAND => mask().unwrap_or(ALL_BITS),
        id::FCOR => mask().map_or(ALL_BITS, |mask| !mask & ALL_BITS),
        id::FCGET => (1 << (2 * BITS)) - 1,
        _ => ALL_BITS,
    }
}

/// `parsed` with `in_hw_clip CLIP` declared in the `--enter` of the region
/// whose instructions span `lines`.
pub(super) fn declare_history(
    parsed: &parser::Program,
    lines: &std::ops::RangeInclusive<usize>,
) -> Option<parser::Program> {
    let enter = parsed.statements.iter().rposition(|statement| {
        statement.line <= *lines.start()
            && matches!(&statement.kind, StatementKind::Instruction(instruction)
                if instruction.synthetic() == Some(Synthetic::Enter))
    })?;
    let line = parsed.statements[enter].line;
    // An input declaration parses only inside an `--enter` block.
    let declaration = parser::parse_with_options(
        "--enter\nin_hw_clip CLIP\n--endenter\n",
        parser::ParserOptions {
            syntax: Syntax::Old,
            allow_incomplete: true,
        },
    )
    .ok()?
    .statements
    .into_iter()
    .nth(1)?;

    let mut statements = parsed.statements.clone();
    statements.insert(
        enter + 1,
        parser::Statement {
            line,
            ..declaration
        },
    );
    Some(parser::Program { statements })
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declaration_parses_as_the_clip_input() {
        let source = "--enter\n\
                      --endenter\n\
                      clipw.xyz vf01, vf01w\n\
                      --exit\n\
                      --endexit\n";
        let parsed = parser::parse_with_options(
            source,
            parser::ParserOptions {
                syntax: Syntax::Old,
                allow_incomplete: false,
            },
        )
        .unwrap();
        let declared = declare_history(&parsed, &(3..=3)).unwrap();
        let StatementKind::Instruction(instruction) = &declared.statements[1].kind else {
            panic!("{:?}", declared.statements[1]);
        };
        assert_eq!(instruction.metadata.index, id::IN_HW_CLIP);
        assert_eq!(declared.statements[1].line, 1);
        assert_eq!(declared.statements.len(), parsed.statements.len() + 1);
    }
}
