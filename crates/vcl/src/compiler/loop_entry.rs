//! The `entry` fix (vcl 1.4beta7 defect F7, docs/VCL_BUGS.md): a
//! software-pipelined loop that reads Q, P or a flag before writing it
//! waits for the value the code before it may still be computing.
//!
//! Q, P and the MAC, status and clip flags are not interlocked. vcl's
//! pipeliner keeps their latencies between iterations, but not from the
//! code before the loop, so the kernel's first rows can read such a value
//! before it lands (observed from vcl 1.4beta7, `loop-entry-q-in-flight`).
//! The fix puts raw code before the loop's label, as if the source had it:
//! a `nop waitq` or `nop waitp` row, which stalls only while the value is
//! still on its way, and enough rows for a flag (latency 4) set in the last
//! row before them to land.

use super::*;

/// The registers whose entry value a loop reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EntryReads {
    q: bool,
    p: bool,
    flags: bool,
}

impl EntryReads {
    fn any(self) -> bool {
        self.q || self.p || self.flags
    }

    /// The raw rows that wait for these values.
    fn raw_rows(self) -> Vec<&'static str> {
        let mut rows = Vec::new();
        if self.q {
            rows.push("nop waitq");
        }
        if self.p {
            rows.push("nop waitp");
        }
        // A flag set in the row before these lands 4 cycles after it
        // issued: three rows later the loop's first row reads it.
        while self.flags && rows.len() < 3 {
            rows.push("nop nop");
        }
        rows
    }
}

/// A loop the fix waits before: its first label, and what it reads.
pub(super) struct EntryWait {
    label: String,
    first_line: usize,
    reads: EntryReads,
    /// The loop has `--LoopCS`; otherwise the `loops` optimization gives
    /// it one.
    annotated: bool,
}

impl EntryWait {
    pub(super) fn annotated(&self) -> bool {
        self.annotated
    }
}

/// Whether the listing has the loop's kernel, named after its label
/// (`EXPL_<unique>_<label>__MAIN_LOOP`).
pub(super) fn pipelined(wait: &EntryWait, listing: &str) -> bool {
    listing.contains(&format!("_{}__MAIN_LOOP:", wait.label))
}

/// The loops of `program` that will be software-pipelined and read an
/// entry value: with `--LoopCS` when the `entry` fix applies, and without
/// it when the `loops` optimization will give them one. Only loops entered
/// by falling into their label qualify; a branch to the label from
/// elsewhere would pass the wait.
pub(super) fn entry_waits(program: &Source, options: &Options) -> Vec<EntryWait> {
    if options.disable_loops {
        return Vec::new();
    }
    let mut waits = Vec::new();
    for region in &program.regions {
        for (id, block) in region.blocks.iter().enumerate() {
            let annotated = block.loop_hints.minimum.is_some();
            let wanted = if annotated {
                options.fixes(Fix::Entry)
            } else {
                options.optimizes(Optimization::Loops)
            };
            if !wanted || !is_ordinary_self_loop(region, block) {
                continue;
            }
            let named_elsewhere = region.blocks.iter().enumerate().any(|(other, from)| {
                other != id
                    && matches!(&from.end, End::Branch { target: Some(target), .. }
                        if block.labels.iter().any(|label| label.eq_ignore_ascii_case(target)))
            }) || block
                .labels
                .iter()
                .any(|label| program.globals.contains(label));
            let reads = entry_reads(program, region, block);
            if named_elsewhere || !reads.any() {
                continue;
            }
            waits.push(EntryWait {
                label: block.labels[0].clone(),
                first_line: region.instructions[block.operations[0]].line,
                reads,
                annotated,
            });
        }
    }
    waits
}

/// Whether a block is a loop vcl's pipeliner takes: ordinary code that
/// ends in a conditional branch to one of its own labels.
pub(super) fn is_ordinary_self_loop(region: &Region, block: &Block) -> bool {
    let self_loop = matches!(&block.end, End::Branch { target: Some(target), conditional: true, call: false, .. }
        if block.labels.iter().any(|label| label.eq_ignore_ascii_case(target)));
    self_loop
        && !block.operations.is_empty()
        && block
            .operations
            .iter()
            .all(|&id| region.instructions[id].instruction.mode == ParseMode::Vcl)
}

/// Whether the block reads Q, P or a flag before writing it.
pub(super) fn reads_entry_value(program: &Source, region: &Region, block: &Block) -> bool {
    entry_reads(program, region, block).any()
}

/// The non-interlocked registers the block reads before writing them.
/// CLIPW's read of the clip history (`CLIPA`) is not one: the flags
/// shift in order.
fn entry_reads(program: &Source, region: &Region, block: &Block) -> EntryReads {
    let mut reads = EntryReads::default();
    let mut written = BTreeSet::new();
    for &id in &block.operations {
        let effects = &region.instructions[id].effects;
        for read in &effects.reads {
            if written.contains(&read.register) {
                continue;
            }
            match program.registers.named(read.register).name.as_str() {
                "Q" => reads.q = true,
                "P" => reads.p = true,
                "CLIP" | "MAC" | "STAT_ALU" | "STAT_DIV" => reads.flags = true,
                _ => {}
            }
        }
        written.extend(effects.writes.iter().map(|write| write.register));
    }
    reads
}

/// The parsed program with each wait's raw code before its loop's label.
/// Returns false when a label is not found, and the program is unchanged.
pub(super) fn insert_entry_waits(parsed: &mut parser::Program, waits: &[EntryWait]) -> bool {
    let mut insertions = Vec::new();
    for wait in waits {
        let Some(index) = parsed.statements.iter().rposition(|statement| {
            statement.line <= wait.first_line
                && matches!(&statement.kind, StatementKind::Label(label)
                    if label.name.eq_ignore_ascii_case(&wait.label))
        }) else {
            return false;
        };
        // One raw block per row: every inserted statement takes the label's
        // line, and raw code pairs instructions by line.
        let text: String = wait
            .reads
            .raw_rows()
            .iter()
            .map(|row| format!(".vsm\n{row}\n.endvsm\n"))
            .collect();
        let options = parser::ParserOptions {
            allow_incomplete: true,
            ..Default::default()
        };
        let Ok(snippet) = parser::parse_with_options(&text, options) else {
            return false;
        };
        let line = parsed.statements[index].line;
        let statements = snippet
            .statements
            .into_iter()
            .map(|statement| parser::Statement { line, ..statement });
        insertions.push((index, statements.collect::<Vec<_>>()));
    }
    insertions.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
    for (index, statements) in insertions {
        parsed.statements.splice(index..index, statements);
    }
    true
}

/// The lowercase labels of the loops `waits` waits before.
pub(super) fn waited_labels(waits: &[EntryWait]) -> BTreeSet<String> {
    waits
        .iter()
        .map(|wait| wait.label.to_ascii_lowercase())
        .collect()
}
