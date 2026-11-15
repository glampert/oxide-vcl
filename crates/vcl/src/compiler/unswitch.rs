//! The `unswitch` optimization (docs/OPTIMIZATION_PLAN.md, O11): a loop
//! whose usual path branches on a register that path never writes gets one
//! copy of that path for each outcome, without those branches.
//!
//! A renderer's per-primitive loop often tests a mode the host set for the
//! whole batch (lit or not, a vertex format) once per vertex. Each test
//! ends a stream, so the scheduler cannot overlap one vertex's work with
//! the next one's. With a copy per outcome the tests disappear, the
//! streams between them join, and the ordinary scheduler packs them.
//!
//! The rewrite works on the parsed program, before anything is compiled:
//!
//! - A loop is a label and the last conditional branch back to it.
//! - Its *usual path* is every block except inner loops and the *special
//!   cases*: the blocks a branch reaches, before the code its two arms
//!   share, when they lead to an inner loop the other arm does not (a
//!   clipping pass, say). Special cases stay as one shared copy. An inner
//!   loop is the blocks on a cycle that avoids the header, so where a block
//!   sits in the source does not decide what it is.
//! - The condition is the most frequent conditional branch on the usual
//!   path whose registers no usual-path instruction writes. Branches on
//!   the same registers, with the same or the opposite test, resolve in
//!   each copy.
//! - Every way into the usual path from outside it (the code before the
//!   loop, the special cases, other code) goes through a *dispatch*: the
//!   condition's branch to the copy where it holds, then a branch to the
//!   other. A dispatch keeps the original label, so those branches are
//!   unchanged, and it tests the register again, so the special cases may
//!   write it.
//!
//! Each copy's labels get a suffix (`__u0`, `__u1`), and a dispatch or a
//! fall-through that no longer follows its block becomes a `b`. A copy is
//! a loop of its own and may be unswitched once more, on another
//! condition (`unswitch_loops`' `nested`): its copies get a second suffix
//! (`__u1__u0`).

use super::*;
use crate::isa::instruction::subtype;

/// Most loops one program unswitches: each one parses the program again.
const MAX_LOOPS: usize = 16;

/// The parsed program with its loops unswitched, or `None` when no loop
/// qualifies, and whether a copy was unswitched again.
///
/// With `nested`, the two copies of a loop are loops of their own and may
/// each be unswitched once more, on another condition: a loop that tests
/// two modes (a colour form and a vertex format) gets a copy for each pair
/// of outcomes, four at most. A copy of a copy is not unswitched again.
pub(super) fn unswitch_loops(
    parsed: &parser::Program,
    options: &Options,
    nested: bool,
) -> Option<(parser::Program, bool)> {
    let mut current = parsed.clone();
    let mut labels = Labels::default();
    let mut changed = false;
    for _ in 0..MAX_LOOPS {
        let mut program = Source::new(options);
        program.parse_program(current.clone()).ok()?;
        let Some(next) = unswitch_one(&current, &program, &mut labels, nested) else {
            break;
        };
        current = next;
        changed = true;
    }
    changed.then_some((current, labels.nested))
}

/// The loop headers `unswitch_loops` has dealt with, as lowercase labels.
#[derive(Default)]
struct Labels {
    /// Headers already looked at, and every label a rewrite made that is
    /// not to be unswitched: those are not looked at again.
    seen: BTreeSet<String>,
    /// The copies' headers that may be unswitched once more (`nested`).
    copies: BTreeSet<String>,
    /// Whether one of them was.
    nested: bool,
}

/// A branch test and its registers, and the branches with it: statement
/// index and whether the branch negates the test.
type TestedBranches<'a> = ((&'a str, Vec<usize>), Vec<(usize, bool)>);

/// Where control goes after a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Block(usize),
    /// Out of the loop: the statement after it, or a label elsewhere.
    Outside,
}

/// A block of the loop: statement indices, its labels first.
#[derive(Clone, Debug)]
struct LoopBlock {
    labels: Vec<usize>,
    body: Vec<usize>,
    /// The branch that ends the block.
    branch: Option<usize>,
    fall: Option<Next>,
    jump: Option<Next>,
}

impl LoopBlock {
    fn successors(&self) -> impl Iterator<Item = Next> + '_ {
        self.fall.iter().chain(self.jump.iter()).copied()
    }
}

/// A branch statement's instruction, when it is one.
fn branch_instruction(statement: &parser::Statement) -> Option<&Instruction> {
    match &statement.kind {
        StatementKind::Instruction(instruction)
            if instruction.synthetic().is_none() && instruction.metadata.is_branch() =>
        {
            Some(instruction)
        }
        _ => None,
    }
}

/// A branch's label operand.
fn branch_target(instruction: &Instruction) -> Option<&str> {
    instruction
        .operands
        .iter()
        .rev()
        .find_map(|operand| operand.immediate.as_deref())
}

/// The statement with its branch's label operand replaced.
fn retarget(statement: &parser::Statement, target: &str) -> parser::Statement {
    let mut statement = statement.clone();
    if let StatementKind::Instruction(instruction) = &mut statement.kind
        && let Some(operand) = instruction
            .operands
            .iter_mut()
            .rev()
            .find(|operand| operand.immediate.is_some())
    {
        operand.immediate = Some(target.to_string());
        operand.text = target.to_string();
    }
    statement
}

/// A conditional branch's test, as a test and whether it is negated:
/// `ibne` is `ibeq` negated, `iblez` is `ibgtz` and `ibgez` is `ibltz`.
fn test_of(mnemonic: &str) -> Option<(&'static str, bool)> {
    Some(match mnemonic.to_ascii_lowercase().as_str() {
        "ibeq" => ("eq", false),
        "ibne" => ("eq", true),
        "ibgtz" => ("gtz", false),
        "iblez" => ("gtz", true),
        "ibltz" => ("ltz", false),
        "ibgez" => ("ltz", true),
        _ => return None,
    })
}

/// Unswitches the first loop not yet looked at that qualifies.
fn unswitch_one(
    parsed: &parser::Program,
    program: &Source,
    labels: &mut Labels,
    nested: bool,
) -> Option<parser::Program> {
    let statements = &parsed.statements;
    let mut defined = BTreeMap::new();
    for (index, statement) in statements.iter().enumerate() {
        if let StatementKind::Label(label) = &statement.kind {
            defined
                .entry(label.name.to_ascii_lowercase())
                .or_insert(index);
        }
    }
    // Each label's last conditional branch back to it.
    let mut loops = BTreeMap::new();
    for (index, statement) in statements.iter().enumerate() {
        let Some(instruction) = branch_instruction(statement) else {
            continue;
        };
        if !instruction.metadata.is_conditional_branch() {
            continue;
        }
        if let Some(target) = branch_target(instruction)
            && let Some(&header) = defined.get(&target.to_ascii_lowercase())
            && header < index
        {
            loops.insert(header, index);
        }
    }
    for (header, latch) in loops {
        let StatementKind::Label(label) = &statements[header].kind else {
            continue;
        };
        let name = label.name.to_ascii_lowercase();
        if !labels.seen.insert(name.clone()) {
            continue;
        }
        if let Some((rewritten, made, copies)) = unswitch_loop(parsed, program, header, latch) {
            let is_copy = labels.copies.contains(&name);
            labels.nested |= is_copy;
            for made in made {
                if nested && !is_copy && copies.contains(&made) {
                    labels.copies.insert(made);
                } else {
                    labels.seen.insert(made);
                }
            }
            return Some(rewritten);
        }
    }
    None
}

/// The loop from the label at `header` to the branch at `latch`,
/// unswitched, the labels the rewrite made, and the two copies' headers
/// among them (lowercase).
fn unswitch_loop(
    parsed: &parser::Program,
    program: &Source,
    header: usize,
    latch: usize,
) -> Option<(parser::Program, Vec<String>, [String; 2])> {
    let statements = &parsed.statements;
    // The header block starts at the first of the labels before its code.
    let mut start = header;
    while start > 0 && matches!(statements[start - 1].kind, StatementKind::Label(_)) {
        start -= 1;
    }
    if latch + 1 >= statements.len() {
        return None;
    }
    // Only ordinary code and labels: no regions, raw code or directives.
    for statement in &statements[start..=latch] {
        match &statement.kind {
            StatementKind::Label(_) => {}
            StatementKind::Instruction(instruction) if instruction.mode == ParseMode::Vcl => {
                match instruction.synthetic() {
                    None
                    | Some(Synthetic::Barrier)
                    | Some(Synthetic::LoopCount)
                    | Some(Synthetic::LoopExtra)
                    | Some(Synthetic::LoopAbsolute) => {}
                    Some(_) => return None,
                }
                if instruction.synthetic().is_none()
                    && instruction.metadata.is_branch()
                    && instruction.metadata.subtype
                        & (subtype::branch::REGISTER | subtype::branch::LINK)
                        != 0
                {
                    return None;
                }
            }
            _ => return None,
        }
    }

    let blocks = loop_blocks(statements, start, latch);
    let count = blocks.len();
    let inner = inner_loops(&blocks);
    let cold = special_cases(&blocks, &inner);
    // The usual path: what the header reaches without entering an inner
    // loop or a special case.
    let mut usual = vec![false; count];
    usual[0] = true;
    let mut work = vec![0];
    while let Some(id) = work.pop() {
        for next in blocks[id].successors() {
            if let Next::Block(to) = next
                && !usual[to]
                && !inner[to]
                && !cold[to]
            {
                usual[to] = true;
                work.push(to);
            }
        }
    }

    // The registers the usual path writes, from the compiled view of each
    // line.
    let mut located: BTreeMap<usize, Vec<&Located>> = BTreeMap::new();
    for region in &program.regions {
        for instruction in &region.instructions {
            located
                .entry(instruction.line)
                .or_default()
                .push(instruction);
        }
    }
    let mut written = BTreeSet::new();
    for (id, block) in blocks.iter().enumerate() {
        if !usual[id] {
            continue;
        }
        for &index in block.body.iter().chain(block.branch.iter()) {
            for instruction in located.get(&statements[index].line).into_iter().flatten() {
                written.extend(
                    instruction
                        .effects
                        .writes
                        .iter()
                        .map(|write| write.register),
                );
            }
        }
    }
    // The usual path's branches on registers it does not write, grouped by
    // test and registers.
    let mut tests: Vec<TestedBranches> = Vec::new();
    for (id, block) in blocks.iter().enumerate() {
        let Some(index) = block.branch else {
            continue;
        };
        let Some(instruction) = branch_instruction(&statements[index]) else {
            continue;
        };
        if !usual[id] || !instruction.metadata.is_conditional_branch() {
            continue;
        }
        let Some((test, negated)) = test_of(&instruction.mnemonic) else {
            continue;
        };
        let Some(compiled) = located
            .get(&statements[index].line)
            .and_then(|all| all.iter().find(|one| one.instruction.metadata.is_branch()))
        else {
            continue;
        };
        let mut registers: Vec<usize> = compiled
            .effects
            .reads
            .iter()
            .map(|read| read.register)
            .collect();
        registers.sort_unstable();
        registers.dedup();
        if registers.is_empty() || registers.iter().any(|register| written.contains(register)) {
            continue;
        }
        let key = (test, registers);
        match tests.iter_mut().find(|(known, _)| *known == key) {
            Some((_, branches)) => branches.push((index, negated)),
            None => tests.push((key, vec![(index, negated)])),
        }
    }
    let (_, branches) = tests
        .into_iter()
        .rev()
        .max_by_key(|(_, branches)| branches.len())?;
    let condition = branches[0].0;
    let condition_negated = branches[0].1;
    // Whether a resolved branch is taken in the copy where the condition's
    // first branch is taken (`holds`).
    let resolved: BTreeMap<usize, bool> = branches
        .iter()
        .map(|&(index, negated)| (index, negated == condition_negated))
        .collect();

    let rewrite = Rewrite::new(parsed, program, &blocks, &usual, start, latch);
    let copies = rewrite.copy_labels[0]
        .clone()
        .map(|name| name.to_ascii_lowercase());
    let (rewritten, made) = rewrite.emit(condition, &resolved);
    Some((rewritten, made, copies))
}

/// The blocks of the statements `start..=latch`. A block starts at a label
/// after code and after a branch.
fn loop_blocks(statements: &[parser::Statement], start: usize, latch: usize) -> Vec<LoopBlock> {
    let mut blocks: Vec<LoopBlock> = Vec::new();
    let mut open = false;
    for (index, statement) in statements.iter().enumerate().take(latch + 1).skip(start) {
        let is_label = matches!(statement.kind, StatementKind::Label(_));
        let current_has_code = blocks
            .last()
            .is_some_and(|block| !block.body.is_empty() || block.branch.is_some());
        if !open || (is_label && current_has_code) {
            blocks.push(LoopBlock {
                labels: Vec::new(),
                body: Vec::new(),
                branch: None,
                fall: None,
                jump: None,
            });
            open = true;
        }
        let block = blocks.last_mut().unwrap();
        if is_label {
            block.labels.push(index);
        } else if branch_instruction(statement).is_some() {
            block.branch = Some(index);
            open = false;
        } else {
            block.body.push(index);
        }
    }
    let mut label_block = BTreeMap::new();
    for (id, block) in blocks.iter().enumerate() {
        for &index in &block.labels {
            if let StatementKind::Label(label) = &statements[index].kind {
                label_block.insert(label.name.to_ascii_lowercase(), id);
            }
        }
    }
    let count = blocks.len();
    for (id, block) in blocks.iter_mut().enumerate() {
        let after = if id + 1 < count {
            Next::Block(id + 1)
        } else {
            Next::Outside
        };
        match block
            .branch
            .and_then(|index| branch_instruction(&statements[index]))
        {
            None => block.fall = Some(after),
            Some(instruction) => {
                if instruction.metadata.is_conditional_branch() {
                    block.fall = Some(after);
                }
                block.jump = Some(
                    branch_target(instruction)
                        .and_then(|target| label_block.get(&target.to_ascii_lowercase()))
                        .map_or(Next::Outside, |&to| Next::Block(to)),
                );
            }
        }
    }
    blocks
}

/// The inner loops: the blocks on a cycle that does not pass through the
/// header. Found from the control flow rather than from the source order, so
/// a branch to an earlier label that only runs forward (a hot arm placed
/// after a special case and branching up to it) makes no loop.
fn inner_loops(blocks: &[LoopBlock]) -> Vec<bool> {
    let count = blocks.len();
    let successors = |id: usize| {
        blocks[id].successors().filter_map(|next| match next {
            Next::Block(to) if to != 0 => Some(to),
            _ => None,
        })
    };
    (0..count)
        .map(|id| {
            let mut seen = vec![false; count];
            let mut work: Vec<usize> = successors(id).collect();
            while let Some(at) = work.pop() {
                if at == id {
                    return true;
                }
                if !seen[at] {
                    seen[at] = true;
                    work.extend(successors(at));
                }
            }
            false
        })
        .collect()
}

/// The special cases: for each conditional branch outside an inner loop
/// with code on both arms, the blocks an arm reaches before the code both
/// arms share, when that arm leads to an inner loop the other does not. A
/// branch that skips straight to the shared code (an early out, or a skip
/// past a loop) marks nothing.
fn special_cases(blocks: &[LoopBlock], inner: &[bool]) -> Vec<bool> {
    let count = blocks.len();
    // Post-dominators, with the loop's exits and the branches back to the
    // header as one exit node (`count`).
    let exit = count;
    let successors = |id: usize| -> Vec<usize> {
        blocks[id]
            .successors()
            .map(|next| match next {
                Next::Block(0) | Next::Outside => exit,
                Next::Block(to) => to,
            })
            .collect()
    };
    let mut post = vec![vec![true; count + 1]; count + 1];
    post[exit] = vec![false; count + 1];
    post[exit][exit] = true;
    let mut changed = true;
    while changed {
        changed = false;
        for id in (0..count).rev() {
            let mut next = vec![true; count + 1];
            for to in successors(id) {
                for (flag, &dominated) in next.iter_mut().zip(&post[to]) {
                    *flag &= dominated;
                }
            }
            next[id] = true;
            if next != post[id] {
                post[id] = next;
                changed = true;
            }
        }
    }
    let reach = |from: Next, branch: usize| -> Vec<bool> {
        let mut seen = vec![false; count];
        let Next::Block(first) = from else {
            return seen;
        };
        let stops = |to: usize| to == 0 || (post[branch][to] && to != branch);
        if stops(first) {
            return seen;
        }
        seen[first] = true;
        let mut work = vec![first];
        while let Some(id) = work.pop() {
            for next in blocks[id].successors() {
                if let Next::Block(to) = next
                    && !seen[to]
                    && !stops(to)
                {
                    seen[to] = true;
                    work.push(to);
                }
            }
        }
        seen
    };
    let mut cold = vec![false; count];
    for (id, block) in blocks.iter().enumerate() {
        let (Some(fall), Some(jump)) = (block.fall, block.jump) else {
            continue;
        };
        if inner[id] {
            continue;
        }
        let arms = [reach(fall, id), reach(jump, id)];
        if arms.iter().any(|arm| !arm.contains(&true)) {
            continue;
        }
        for (arm, other) in [(0, 1), (1, 0)] {
            let own_loop = (0..count).any(|to| arms[arm][to] && inner[to] && !arms[other][to]);
            if own_loop {
                for (flag, &reached) in cold.iter_mut().zip(&arms[arm]) {
                    *flag |= reached;
                }
            }
        }
    }
    cold
}

/// The rewrite of one loop.
struct Rewrite<'a> {
    statements: &'a [parser::Statement],
    blocks: &'a [LoopBlock],
    usual: &'a [bool],
    start: usize,
    latch: usize,
    /// Lowercase names of every label in the program and every one made.
    taken: BTreeSet<String>,
    made: Vec<String>,
    /// Each usual block's label in each copy (`[false, true]`).
    copy_labels: Vec<[String; 2]>,
    /// The label of each block outside the usual path, or of a usual
    /// block's dispatch: its first label, or one made for it.
    shared_labels: BTreeMap<usize, String>,
    /// The usual blocks with a dispatch.
    dispatched: BTreeSet<usize>,
    /// The `b` statement every new jump is made from.
    jump: parser::Statement,
}

impl<'a> Rewrite<'a> {
    fn new(
        parsed: &'a parser::Program,
        program: &Source,
        blocks: &'a [LoopBlock],
        usual: &'a [bool],
        start: usize,
        latch: usize,
    ) -> Self {
        let statements = &parsed.statements[..];
        let taken = statements
            .iter()
            .filter_map(|statement| match &statement.kind {
                StatementKind::Label(label) => Some(label.name.to_ascii_lowercase()),
                _ => None,
            })
            .collect();
        let options = parser::ParserOptions {
            allow_incomplete: true,
            ..Default::default()
        };
        let jump = parser::parse_with_options("b unswitch_target\n", options)
            .expect("an unconditional branch parses")
            .statements
            .into_iter()
            .find(|statement| branch_instruction(statement).is_some())
            .expect("an unconditional branch parses");
        let mut rewrite = Rewrite {
            statements,
            blocks,
            usual,
            start,
            latch,
            taken,
            made: Vec::new(),
            copy_labels: Vec::new(),
            shared_labels: BTreeMap::new(),
            dispatched: BTreeSet::new(),
            jump,
        };
        let header = rewrite.label_name(blocks[0].labels[0]);
        for id in 0..blocks.len() {
            if !usual[id] {
                rewrite.copy_labels.push([String::new(), String::new()]);
                continue;
            }
            let names = match blocks[id].labels.first() {
                Some(&index) => {
                    let name = rewrite.label_name(index);
                    [format!("{name}__u0"), format!("{name}__u1")]
                }
                None => [format!("{header}__u0_{id}"), format!("{header}__u1_{id}")],
            };
            let names = names.map(|name| rewrite.fresh(&name));
            rewrite.copy_labels.push(names);
        }
        // A usual block needs a dispatch when anything outside the usual
        // path reaches it: the code before the loop (the header), a block
        // outside the usual path, a branch from outside the loop, or the
        // host (`.global`).
        rewrite.dispatched.insert(0);
        for (id, block) in blocks.iter().enumerate() {
            if usual[id] {
                continue;
            }
            for next in block.successors() {
                if let Next::Block(to) = next
                    && usual[to]
                {
                    rewrite.dispatched.insert(to);
                }
            }
        }
        let outside_targets: BTreeSet<String> = statements
            .iter()
            .enumerate()
            .filter(|(index, _)| *index < start || *index > latch)
            .filter_map(|(_, statement)| branch_instruction(statement).and_then(branch_target))
            .map(str::to_ascii_lowercase)
            .collect();
        for (id, block) in blocks.iter().enumerate() {
            let named_outside = block.labels.iter().any(|&index| {
                let name = rewrite.label_name(index);
                outside_targets.contains(&name.to_ascii_lowercase())
                    || program
                        .globals
                        .iter()
                        .any(|global| global.eq_ignore_ascii_case(&name))
            });
            if usual[id] && named_outside {
                rewrite.dispatched.insert(id);
            }
        }
        rewrite
    }

    fn label_name(&self, index: usize) -> String {
        match &self.statements[index].kind {
            StatementKind::Label(label) => label.name.clone(),
            _ => unreachable!("a block's labels are label statements"),
        }
    }

    /// `name`, or `name` with a number after it when that is taken.
    fn fresh(&mut self, name: &str) -> String {
        let mut candidate = name.to_string();
        let mut number = 1;
        while self.taken.contains(&candidate.to_ascii_lowercase()) {
            candidate = format!("{name}_{number}");
            number += 1;
        }
        self.taken.insert(candidate.to_ascii_lowercase());
        self.made.push(candidate.to_ascii_lowercase());
        candidate
    }

    /// The label a block outside the usual path, or a usual block's
    /// dispatch, is reached by.
    fn shared_label(&mut self, id: usize) -> String {
        if let Some(name) = self.shared_labels.get(&id) {
            return name.clone();
        }
        let name = match self.blocks[id].labels.first() {
            Some(&index) => self.label_name(index),
            None => {
                let header = self.label_name(self.blocks[0].labels[0]);
                self.fresh(&format!("{header}__d{id}"))
            }
        };
        self.shared_labels.insert(id, name.clone());
        name
    }

    /// The exit's label: the label after the loop, or one made for it.
    fn exit_label(&mut self) -> String {
        if let Some(name) = self.shared_labels.get(&usize::MAX) {
            return name.clone();
        }
        let name = match &self.statements[self.latch + 1].kind {
            StatementKind::Label(label) => label.name.clone(),
            _ => {
                let header = self.label_name(self.blocks[0].labels[0]);
                self.fresh(&format!("{header}__exit"))
            }
        };
        self.shared_labels.insert(usize::MAX, name.clone());
        name
    }

    fn jump_to(&self, target: &str, line: usize) -> parser::Statement {
        parser::Statement {
            line,
            ..retarget(&self.jump, target)
        }
    }

    fn label(name: &str, modifiers: u8, line: usize) -> parser::Statement {
        parser::Statement {
            line,
            kind: StatementKind::Label(parser::Label {
                name: name.to_string(),
                modifiers,
            }),
        }
    }

    /// Where `next` is reached from copy `copy` (`None` from shared code).
    fn target(&mut self, next: Next, copy: Option<usize>) -> String {
        match next {
            Next::Block(to) => match copy {
                Some(copy) if self.usual[to] => self.copy_labels[to][copy].clone(),
                _ => self.shared_label(to),
            },
            Next::Outside => self.exit_label(),
        }
    }

    /// The line a block's added statements take: its last statement's.
    fn last_line(&self, id: usize) -> usize {
        let block = &self.blocks[id];
        let last = block
            .branch
            .or(block.body.last().copied())
            .or(block.labels.last().copied())
            .unwrap_or(self.start);
        self.statements[last].line
    }

    /// The rewritten program and the labels it made.
    fn emit(
        mut self,
        condition: usize,
        resolved: &BTreeMap<usize, bool>,
    ) -> (parser::Program, Vec<String>) {
        let header_line = self.statements[self.start].line;
        let mut out: Vec<parser::Statement> = self.statements[..self.start].to_vec();
        // The header's dispatch takes the loop's place.
        let header_labels = self.blocks[0].labels.clone();
        out.extend(
            header_labels
                .iter()
                .map(|&index| self.statements[index].clone()),
        );
        let holds = self.copy_labels[0][1].clone();
        let fails = self.copy_labels[0][0].clone();
        out.push(parser::Statement {
            line: header_line,
            ..retarget(&self.statements[condition], &holds)
        });
        out.push(self.jump_to(&fails, header_line));
        for copy in [0, 1] {
            self.emit_copy(copy, resolved, &mut out);
        }
        // The blocks outside the usual path, once, as written.
        for id in 0..self.blocks.len() {
            if self.usual[id] {
                continue;
            }
            let block = &self.blocks[id];
            let line = self.last_line(id);
            if block.labels.is_empty() && self.shared_labels.contains_key(&id) {
                let name = self.shared_labels[&id].clone();
                out.push(Self::label(&name, 0, line));
            }
            out.extend(
                block
                    .labels
                    .iter()
                    .chain(&block.body)
                    .chain(&block.branch)
                    .map(|&index| self.statements[index].clone()),
            );
            if let Some(next) = block.fall {
                let target = self.target(next, None);
                out.push(self.jump_to(&target, line));
            }
        }
        // The other dispatches.
        for id in self.dispatched.clone() {
            if id == 0 {
                continue;
            }
            let line = self.last_line(id);
            let block = &self.blocks[id];
            if block.labels.is_empty() {
                let name = self.shared_label(id);
                out.push(Self::label(&name, 0, line));
            } else {
                out.extend(
                    block
                        .labels
                        .iter()
                        .map(|&index| self.statements[index].clone()),
                );
            }
            let holds = self.copy_labels[id][1].clone();
            let fails = self.copy_labels[id][0].clone();
            out.push(parser::Statement {
                line,
                ..retarget(&self.statements[condition], &holds)
            });
            out.push(self.jump_to(&fails, line));
        }
        if let Some(name) = self.shared_labels.get(&usize::MAX).cloned()
            && !matches!(
                self.statements[self.latch + 1].kind,
                StatementKind::Label(_)
            )
        {
            out.push(Self::label(&name, 0, self.statements[self.latch + 1].line));
        }
        out.extend(self.statements[self.latch + 1..].iter().cloned());
        let statements = drop_unused_labels(drop_jumps_to_next(out), &self.made);
        (parser::Program { statements }, self.made)
    }

    /// One copy of the usual path: `copy` 1 is where the condition's
    /// branch is taken.
    fn emit_copy(
        &mut self,
        copy: usize,
        resolved: &BTreeMap<usize, bool>,
        out: &mut Vec<parser::Statement>,
    ) {
        let count = self.blocks.len();
        // Each usual block's way out in this copy: (fall, jump, kept branch).
        let mut exits: Vec<(Option<Next>, Option<Next>, bool)> = vec![(None, None, false); count];
        for (id, block) in self.blocks.iter().enumerate() {
            if !self.usual[id] {
                continue;
            }
            exits[id] = match block.branch.and_then(|index| resolved.get(&index)) {
                Some(&taken_when_holds) => {
                    if taken_when_holds == (copy == 1) {
                        (None, block.jump, false)
                    } else {
                        (block.fall, None, false)
                    }
                }
                None => (block.fall, block.jump, block.branch.is_some()),
            };
        }
        // The blocks this copy still reaches from its entries.
        let mut live = vec![false; count];
        let mut work: Vec<usize> = self.dispatched.iter().copied().collect();
        for &id in &work {
            live[id] = true;
        }
        while let Some(id) = work.pop() {
            let (fall, jump, _) = exits[id];
            for next in fall.into_iter().chain(jump) {
                if let Next::Block(to) = next
                    && self.usual[to]
                    && !live[to]
                {
                    live[to] = true;
                    work.push(to);
                }
            }
        }
        for id in 0..count {
            if !self.usual[id] || !live[id] {
                continue;
            }
            let block = &self.blocks[id];
            let line = self.last_line(id);
            let first_line = block
                .labels
                .first()
                .or(block.body.first())
                .map_or(line, |&index| self.statements[index].line);
            // A copy needs one label: every branch to the block in the
            // copy names it.
            let modifiers =
                block
                    .labels
                    .first()
                    .map_or(0, |&index| match &self.statements[index].kind {
                        StatementKind::Label(label) => label.modifiers,
                        _ => 0,
                    });
            out.push(Self::label(
                &self.copy_labels[id][copy],
                modifiers,
                first_line,
            ));
            out.extend(
                block
                    .body
                    .iter()
                    .map(|&index| self.statements[index].clone()),
            );
            let (fall, jump, kept) = exits[id];
            if let (Some(next), Some(index)) = (jump, block.branch) {
                let outside_target = matches!(next, Next::Outside);
                if kept {
                    if outside_target {
                        out.push(self.statements[index].clone());
                    } else {
                        let target = self.target(next, Some(copy));
                        out.push(retarget(&self.statements[index], &target));
                    }
                } else if outside_target {
                    // A resolved branch out of the loop: the same label,
                    // unconditionally.
                    let original = branch_instruction(&self.statements[index])
                        .and_then(branch_target)
                        .unwrap_or_default()
                        .to_string();
                    out.push(self.jump_to(&original, line));
                } else {
                    let target = self.target(next, Some(copy));
                    out.push(self.jump_to(&target, line));
                }
            }
            if let Some(next) = fall {
                let target = self.target(next, Some(copy));
                out.push(self.jump_to(&target, line));
            }
        }
    }
}

/// The statements without the labels in `made` that no branch names.
fn drop_unused_labels(
    statements: Vec<parser::Statement>,
    made: &[String],
) -> Vec<parser::Statement> {
    let named: BTreeSet<String> = statements
        .iter()
        .filter_map(|statement| branch_instruction(statement).and_then(branch_target))
        .map(str::to_ascii_lowercase)
        .collect();
    statements
        .into_iter()
        .filter(|statement| match &statement.kind {
            StatementKind::Label(label) => {
                let name = label.name.to_ascii_lowercase();
                named.contains(&name) || !made.contains(&name)
            }
            _ => true,
        })
        .collect()
}

/// The statements without each unconditional `b` to a label right after
/// it.
fn drop_jumps_to_next(statements: Vec<parser::Statement>) -> Vec<parser::Statement> {
    let mut kept = Vec::with_capacity(statements.len());
    for (index, statement) in statements.iter().enumerate() {
        if let Some(instruction) = branch_instruction(statement)
            && instruction.metadata.subtype == subtype::branch::JUMP
            && let Some(target) = branch_target(instruction)
        {
            let next_labels = statements[index + 1..]
                .iter()
                .map_while(|next| match &next.kind {
                    StatementKind::Label(label) => Some(label),
                    _ => None,
                });
            if next_labels
                .into_iter()
                .any(|label| label.name.eq_ignore_ascii_case(target))
            {
                continue;
            }
        }
        kept.push(statement.clone());
    }
    kept
}
