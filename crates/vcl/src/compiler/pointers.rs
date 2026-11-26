//! The `pointers` optimization (docs/OPTIMIZATION_PLAN.md, O4): pointers a
//! loop steps by the same amount, whose difference is known, become one.
//!
//! A renderer's vertex loop often walks its input and its output with two
//! pointers set from one `xtop` value plus constants and stepped by the same
//! stride. Each step is an `iaddiu`, a lower instruction, and a kernel the
//! lower pipe bounds is one cycle longer for each. When the difference of
//! the two pointers is a constant at the loop's entry and stays one (both
//! step by the same amount once an iteration), the accesses through one of
//! them can use the other with the difference added to their offsets, and
//! its `iaddiu` goes.
//!
//! The rewrite works on the parsed program, before anything is compiled:
//!
//! - A loop is a label and the last conditional branch back to it, with only
//!   ordinary code between them and no branch from outside into it past its
//!   first labels.
//! - A pointer is a register the loop writes once, with `iaddiu r, r, s`,
//!   `iaddi r, r, s` or `isubiu r, r, s`, in the block that ends with the
//!   branch back. Code below a pointer's step sees it a step on.
//! - What VI registers hold at the loop's entry comes from a forward pass
//!   over the program: a register set by an immediate add is its source plus
//!   a constant, and any other write is a value of its own, which the next
//!   run of that statement replaces. The ways into the loop must agree.
//! - The pointer that goes may only be the base of `lq`, `sq`, `ilw` and
//!   `isw` in the loop, must be dead where the loop exits (every path out
//!   writes it before reading it), and its accesses must keep their offsets
//!   within the signed 11-bit field.
//!
//! A loop that counts down to its exit is the same case: a register stepped
//! once an iteration and tested with `ibne` or `ibeq` against one the loop
//! does not write, while another pointer steps by the same amount (either
//! way). The test can compare that pointer with where it is when the count
//! runs out, computed once before the loop, and the count's step goes. With
//! the count `c` stepping by `s`, the pointer `p` by `s` or `-s`, and the
//! test against `t`, `c` reaches `t` exactly when `p` reaches `p + t - c`
//! or `p + c - t` as they were at the entry, in 16-bit arithmetic as in the
//! VI registers. The count may only be stepped and tested in the loop and
//! must be dead where it exits, and the loop is one block, entered only by
//! falling into it, where the end value is computed. The end costs one or
//! two instructions on the way in, so a loop that runs once is up to two
//! cycles slower. A loop that branches inside is left alone: `unswitch`
//! copies it, and the copies whose kernels the step does not shorten would
//! still pay for the end.
//!
//! Both apply only to a loop whose `--LoopCS`, if it has one, promises no
//! more than any loop does (a minimum of 1 and a safe count of 0), and with
//! at least as many lower instructions as upper ones, where a lower
//! instruction can be a cycle of its kernel. A larger minimum or a safe
//! count lets the pipeliner's schedule decide what happens below the
//! minimum and how many stores run past the end; another schedule decides
//! differently, which the hint allows but which would change what the
//! program computes there. In a
//! loop the upper pipe bounds, a step less only moves the pipeliner's
//! search, sometimes to a longer kernel.

use super::*;
use crate::isa::instruction::subtype::branch::{LINK, REGISTER};

/// A rewrite of one loop and the program it makes. Each is tried on its own
/// (`compile_parsed`) and kept when the kernels get shorter.
pub(super) struct Rewrite {
    /// What it does: the same rewrite of the same loop has the same key.
    pub(super) key: String,
    pub(super) program: parser::Program,
}

/// Every rewrite of `parsed`'s loops, each on its own.
pub(super) fn rewrites(parsed: &parser::Program) -> Vec<Rewrite> {
    let statements = &parsed.statements;
    let labels = label_indices(statements);
    // The last conditional branch back to each label is its loop's latch.
    let mut latches: BTreeMap<usize, usize> = BTreeMap::new();
    for (index, statement) in statements.iter().enumerate() {
        let Some(instruction) = instruction_of(statement) else {
            continue;
        };
        if !is_conditional_branch(instruction) {
            continue;
        }
        if let Some(&header) = branch_target(instruction).and_then(|t| labels.get(&t))
            && header < index
        {
            latches.insert(header, index);
        }
    }
    if latches.is_empty() {
        return Vec::new();
    }
    let graph = Graph::new(statements, &labels);
    let mut found = Vec::new();
    for (&header, &latch) in &latches {
        found.extend(count_with_pointer(parsed, &labels, &graph, header, latch));
    }
    // The pass over the program, only when a loop has two pointers to merge.
    let merging: Vec<(usize, usize)> = latches
        .iter()
        .filter(|&(&header, &latch)| may_merge(statements, &labels, header, latch))
        .map(|(&header, &latch)| (header, latch))
        .collect();
    if !merging.is_empty() {
        let values = Values::of(statements, &graph);
        for (header, latch) in merging {
            found.extend(merge_in_loop(
                parsed, &labels, &graph, &values, header, latch,
            ));
        }
    }
    found
}

/// Whether the loop has the shape a merge needs, with two pointers that
/// step by the same amount; what they hold at its entry is not looked at.
fn may_merge(
    statements: &[parser::Statement],
    labels: &BTreeMap<String, usize>,
    header: usize,
    latch: usize,
) -> bool {
    let mut start = header;
    while start > 0 && matches!(statements[start - 1].kind, StatementKind::Label(_)) {
        start -= 1;
    }
    let inside = start..=latch;
    if !ordinary_body(&statements[inside.clone()])
        || !promises_nothing(&statements[inside.clone()])
        || !lower_bound(&statements[inside.clone()])
        || entered_past_header(statements, labels, header, &inside)
    {
        return false;
    }
    let tail = (start..latch)
        .rev()
        .find(|&index| {
            matches!(statements[index].kind, StatementKind::Label(_))
                || instruction_of(&statements[index]).is_some_and(|i| i.metadata.is_branch())
        })
        .map_or(start, |index| index + 1);
    let pointers = loop_pointers(statements, start, latch, tail);
    let steps: Vec<i64> = pointers.values().map(|pointer| pointer.step).collect();
    steps
        .iter()
        .enumerate()
        .any(|(at, step)| steps[at + 1..].contains(step))
}

/// Each kernel's period in a listing, by its table's name.
pub(super) fn kernel_periods(assembly: &str) -> BTreeMap<String, u32> {
    assembly
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("; _LPOPT_w=[")?;
            let (_, rest) = rest.split_once(']')?;
            let period = rest.split_whitespace().next()?.parse().ok()?;
            let name = rest.rsplit_once('[')?.1.trim_end().strip_suffix(']')?;
            Some((name.to_string(), period))
        })
        .collect()
}

/// Whether `after`'s kernels are shorter in all than `before`'s, with none
/// longer and none new.
pub(super) fn shorter_kernels(before: &str, after: &str) -> bool {
    let (before, after) = (kernel_periods(before), kernel_periods(after));
    after.values().sum::<u32>() < before.values().sum::<u32>()
        && after
            .iter()
            .all(|(name, period)| before.get(name).is_some_and(|old| period <= old))
}

/// The loop's countdown replaced by a test of another of its pointers.
fn count_with_pointer(
    parsed: &parser::Program,
    labels: &BTreeMap<String, usize>,
    graph: &Graph,
    header: usize,
    latch: usize,
) -> Option<Rewrite> {
    let statements = &parsed.statements;
    let mut start = header;
    while start > 0 && matches!(statements[start - 1].kind, StatementKind::Label(_)) {
        start -= 1;
    }
    let inside = start..=latch;
    // Entered only by falling into its first label: the end goes before
    // it, where a branch to the labels would not run it.
    let falls_in = start > 0
        && graph.successors[start - 1]
            .iter()
            .any(|&(to, _)| to == start);
    if !falls_in
        || !ordinary_body(&statements[inside.clone()])
        || !promises_nothing(&statements[inside.clone()])
        || !lower_bound(&statements[inside.clone()])
        || entered_past_header(statements, labels, header, &inside)
        || statements.iter().enumerate().any(|(index, statement)| {
            !inside.contains(&index)
                && instruction_of(statement)
                    .and_then(branch_target)
                    .and_then(|target| labels.get(&target))
                    .is_some_and(|&at| (start..=header).contains(&at))
        })
    {
        return None;
    }
    let test = instruction_of(&statements[latch])?;
    if !matches!(test.mnemonic.to_ascii_lowercase().as_str(), "ibne" | "ibeq") {
        return None;
    }
    let compared: Vec<(usize, String)> = test
        .operands
        .iter()
        .enumerate()
        .filter(|(_, operand)| operand.direction == Direction::Read && operand.register.is_some())
        .map(|(at, operand)| {
            (
                at,
                operand.register.as_deref().unwrap().to_ascii_lowercase(),
            )
        })
        .collect();
    let [(first_at, first), (second_at, second)] = &compared[..] else {
        return None;
    };
    // One block: a loop that branches inside is unswitched into copies, and
    // each would pay for the end, the copies whose kernels it does not
    // shorten too.
    let tail = header + 1;
    if (tail..latch).any(|index| {
        matches!(statements[index].kind, StatementKind::Label(_))
            || instruction_of(&statements[index]).is_some_and(|i| i.metadata.is_branch())
    }) {
        return None;
    }
    let pointers = loop_pointers(statements, start, latch, tail);
    let written: BTreeSet<String> = statements[inside.clone()]
        .iter()
        .filter_map(instruction_of)
        .flat_map(registers_written)
        .collect();
    let (count_at, count, limit_at, limit) =
        if pointers.contains_key(first) && !written.contains(second) {
            (*first_at, first, *second_at, second)
        } else if pointers.contains_key(second) && !written.contains(first) {
            (*second_at, second, *first_at, first)
        } else {
            return None;
        };
    let counter = pointers[count];
    // The count is only stepped and tested.
    let used_elsewhere = inside.clone().any(|index| {
        index != counter.step_at
            && index != latch
            && instruction_of(&statements[index])
                .is_some_and(|i| i.operands.iter().any(|operand| names(operand, count)))
    });
    if used_elsewhere || live_after(statements, graph, &inside, count) {
        return None;
    }
    let (pointer, along) = pointers.iter().find_map(|(name, pointer)| {
        (name != count && name != limit && pointer.step.abs() == counter.step.abs())
            .then_some((name, pointer.step == counter.step))
    })?;

    // A fresh name for the end.
    let used: BTreeSet<String> = statements
        .iter()
        .filter_map(instruction_of)
        .flat_map(|instruction| {
            instruction
                .operands
                .iter()
                .filter_map(|operand| operand.register.as_deref().map(str::to_ascii_lowercase))
        })
        .collect();
    let end = (0..)
        .map(|n| format!("pointer_end_{n}"))
        .find(|name| !used.contains(name))?;
    // p + t - c when they step the same way, p + c - t when they do not.
    let mut code = Vec::new();
    if along {
        code.push(format!("isub {end}, {limit}, {count}"));
        code.push(format!("iadd {end}, {end}, {pointer}"));
    } else {
        code.push(format!("iadd {end}, {pointer}, {count}"));
        if limit != "vi00" {
            code.push(format!("isub {end}, {end}, {limit}"));
        }
    }
    let line = statements[start].line;
    let options = parser::ParserOptions {
        allow_incomplete: true,
        ..Default::default()
    };
    let mut added = Vec::new();
    for text in code {
        let statement = parser::parse_with_options(&format!("{text}\n"), options)
            .ok()?
            .statements
            .into_iter()
            .find(|statement| instruction_of(statement).is_some())?;
        added.push(parser::Statement { line, ..statement });
    }

    let mut rewritten = parsed.clone();
    let StatementKind::Instruction(branch) = &mut rewritten.statements[latch].kind else {
        return None;
    };
    for (at, register) in [(count_at, pointer), (limit_at, &end)] {
        let operand = &mut branch.operands[at];
        operand.text = register.clone();
        operand.register = Some(register.clone());
    }
    rewritten.statements.remove(counter.step_at);
    for (offset, statement) in added.into_iter().enumerate() {
        rewritten.statements.insert(start + offset, statement);
    }
    Some(Rewrite {
        key: format!(
            "count {} {count} by {pointer}",
            label_name(statements, header)
        ),
        program: rewritten,
    })
}

/// The merges of two of the loop's pointers.
fn merge_in_loop(
    parsed: &parser::Program,
    labels: &BTreeMap<String, usize>,
    graph: &Graph,
    values: &Values,
    header: usize,
    latch: usize,
) -> Vec<Rewrite> {
    let statements = &parsed.statements;
    // The header block starts at the first of the labels before its code.
    let mut start = header;
    while start > 0 && matches!(statements[start - 1].kind, StatementKind::Label(_)) {
        start -= 1;
    }
    let inside = start..=latch;
    let mut found = Vec::new();
    if !ordinary_body(&statements[inside.clone()])
        || !promises_nothing(&statements[inside.clone()])
        || !lower_bound(&statements[inside.clone()])
        || entered_past_header(statements, labels, header, &inside)
    {
        return found;
    }
    // The block that ends with the branch back: after the loop's last label
    // or branch before the latch.
    let tail = (start..latch)
        .rev()
        .find(|&index| {
            matches!(statements[index].kind, StatementKind::Label(_))
                || instruction_of(&statements[index]).is_some_and(|i| i.metadata.is_branch())
        })
        .map_or(start, |index| index + 1);

    let pointers = loop_pointers(statements, start, latch, tail);
    if pointers.len() < 2 {
        return found;
    }
    let Some(entry) = values.entry(statements, graph, start, header, &inside) else {
        return found;
    };
    let test: BTreeSet<String> = instruction_of(&statements[latch])
        .map(|instruction| registers_read(instruction).collect())
        .unwrap_or_default();

    for (kept, kept_pointer) in &pointers {
        for (gone, gone_pointer) in &pointers {
            if kept == gone || kept_pointer.step != gone_pointer.step || test.contains(gone) {
                continue;
            }
            let (Value::Known(kept_entry), Value::Known(gone_entry)) =
                (entry.get(kept), entry.get(gone))
            else {
                continue;
            };
            if kept_entry.base != gone_entry.base || live_after(statements, graph, &inside, gone) {
                continue;
            }
            let difference = gone_entry.offset - kept_entry.offset;
            if let Some(program) = rewrite(
                parsed,
                &inside,
                (kept, kept_pointer),
                (gone, gone_pointer),
                difference,
            ) {
                let label = label_name(statements, header);
                found.push(Rewrite {
                    key: format!("merge {label} {gone} into {kept}"),
                    program,
                });
            }
        }
    }
    found
}

/// The lowercase name of the label at `index`.
fn label_name(statements: &[parser::Statement], index: usize) -> String {
    match &statements[index].kind {
        StatementKind::Label(label) => label.name.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// A register the loop steps once an iteration: where, and by how much.
#[derive(Clone, Copy, Debug)]
struct Pointer {
    step_at: usize,
    step: i64,
}

/// The loop's pointers by lowercase name: registers written once in the
/// loop, by an immediate add to themselves in the tail block.
fn loop_pointers(
    statements: &[parser::Statement],
    start: usize,
    latch: usize,
    tail: usize,
) -> BTreeMap<String, Pointer> {
    let mut writes: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, statement) in statements.iter().enumerate().take(latch + 1).skip(start) {
        if let Some(instruction) = instruction_of(statement) {
            for register in registers_written(instruction) {
                writes.entry(register).or_default().push(index);
            }
        }
    }
    let mut pointers = BTreeMap::new();
    for (register, at) in writes {
        let [step_at] = at[..] else {
            continue;
        };
        if step_at < tail || is_physical(&register) {
            continue;
        }
        let Some(instruction) = instruction_of(&statements[step_at]) else {
            continue;
        };
        if let Some((destination, source, step)) = immediate_add(instruction)
            && destination == register
            && source == register
        {
            pointers.insert(register, Pointer { step_at, step });
        }
    }
    pointers
}

/// The program with `gone`'s accesses in the loop made through `kept`, and
/// `gone`'s step dropped; `None` when an access's offset would not fit or
/// the loop uses `gone` in another way.
fn rewrite(
    parsed: &parser::Program,
    inside: &std::ops::RangeInclusive<usize>,
    (kept, kept_pointer): (&String, &Pointer),
    (gone, gone_pointer): (&String, &Pointer),
    difference: i64,
) -> Option<parser::Program> {
    let statements = &parsed.statements;
    let mut rewritten = parsed.clone();
    for index in inside.clone() {
        if index == gone_pointer.step_at {
            continue;
        }
        let Some(instruction) = instruction_of(&statements[index]) else {
            continue;
        };
        let uses: Vec<usize> = instruction
            .operands
            .iter()
            .enumerate()
            .filter(|(_, operand)| names(operand, gone))
            .map(|(at, _)| at)
            .collect();
        if uses.is_empty() {
            continue;
        }
        if !offset_access(instruction) {
            return None;
        }
        // What `kept` and `gone` hold here, each one step on below its step.
        let after = |pointer: &Pointer| i64::from(index > pointer.step_at);
        let shift = difference + gone_pointer.step * (after(gone_pointer) - after(kept_pointer));
        let StatementKind::Instruction(target) = &mut rewritten.statements[index].kind else {
            return None;
        };
        for at in uses {
            let operand = &mut target.operands[at];
            if operand.class != MEMORY_OFFSET
                || operand.direction != Direction::Read
                || operand.forced_register.is_some()
            {
                return None;
            }
            let offset = evaluate(operand.immediate.as_deref().unwrap_or("0"))? + shift;
            if !(-1024..=1023).contains(&offset) {
                return None;
            }
            operand.text = format!("{offset}({kept})");
            operand.register = Some(kept.clone());
            operand.immediate = Some(offset.to_string());
        }
    }
    rewritten.statements.remove(gone_pointer.step_at);
    Some(rewritten)
}

/// Whether `register` is live where the loop exits: some path out of it
/// reads it before writing it, or leaves for a return this cannot follow.
fn live_after(
    statements: &[parser::Statement],
    graph: &Graph,
    inside: &std::ops::RangeInclusive<usize>,
    register: &str,
) -> bool {
    let mut work: Vec<usize> = inside
        .clone()
        .flat_map(|index| graph.successors[index].iter().map(|&(to, _)| to))
        .filter(|to| !inside.contains(to))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(index) = work.pop() {
        if !seen.insert(index) {
            continue;
        }
        if graph.leaves[index] {
            return true;
        }
        match &statements[index].kind {
            StatementKind::Instruction(instruction) => {
                if instruction.operands.iter().any(|operand| {
                    operand.direction != Direction::Write && names(operand, register)
                }) {
                    return true;
                }
                if registers_written(instruction).any(|written| written == register) {
                    continue;
                }
            }
            StatementKind::RawText(_) => return true,
            _ => {}
        }
        work.extend(graph.successors[index].iter().map(|&(to, _)| to));
    }
    false
}

/// The program's control flow, statement by statement.
struct Graph {
    /// Each statement's successors, and whether the edge is a call's
    /// return, past which nothing is known.
    successors: Vec<Vec<(usize, bool)>>,
    /// Statements that leave for a place this cannot follow (`jr`).
    leaves: Vec<bool>,
}

impl Graph {
    fn new(statements: &[parser::Statement], labels: &BTreeMap<String, usize>) -> Self {
        let count = statements.len();
        let mut successors = vec![Vec::new(); count];
        let mut leaves = vec![false; count];
        for (index, statement) in statements.iter().enumerate() {
            let next = (index + 1 < count).then_some(index + 1);
            let mut add = |to: Option<usize>, call_return: bool| {
                if let Some(to) = to {
                    successors[index].push((to, call_return));
                }
            };
            let StatementKind::Instruction(instruction) = &statement.kind else {
                add(next, false);
                continue;
            };
            match instruction.synthetic() {
                Some(Synthetic::EndExit) => {}
                Some(_) => add(next, false),
                None if instruction.metadata.is_branch() => {
                    let subtype = instruction.metadata.subtype;
                    let target = branch_target(instruction).and_then(|t| labels.get(&t).copied());
                    if subtype & REGISTER != 0 {
                        if subtype & LINK != 0 {
                            add(next, true);
                        }
                        leaves[index] = true;
                    } else if subtype & LINK != 0 {
                        add(target, false);
                        add(next, true);
                    } else {
                        add(target, false);
                        if is_conditional_branch(instruction) {
                            add(next, false);
                        }
                    }
                }
                None => add(next, false),
            }
        }
        Graph { successors, leaves }
    }
}

/// A VI value as a base plus a constant.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Affine {
    base: Base,
    offset: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Base {
    /// VI00.
    Zero,
    /// A register's value when the program starts.
    Incoming(String),
    /// What the statement at this index last wrote to this register.
    Written(usize, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Known(Affine),
    /// Different on the ways here, or past a call.
    Unknown,
}

impl Value {
    /// Whether the value is what the statement at `index` wrote, or is
    /// computed from that.
    fn written_by(&self, index: usize) -> bool {
        matches!(self, Value::Known(Affine { base: Base::Written(at, _), .. }) if *at == index)
    }
}

/// What each register holds before a statement; a register not in the map
/// still holds its incoming value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State(BTreeMap<String, Value>);

impl State {
    fn get(&self, register: &str) -> Value {
        if register == "vi00" {
            return Value::Known(Affine {
                base: Base::Zero,
                offset: 0,
            });
        }
        self.0.get(register).cloned().unwrap_or_else(|| {
            Value::Known(Affine {
                base: Base::Incoming(register.to_string()),
                offset: 0,
            })
        })
    }

    /// The state after `instruction`, the statement at `index`. A statement
    /// that runs again replaces what it wrote before, and what was computed
    /// from that.
    fn after(&self, index: usize, instruction: &Instruction, tracked: &BTreeSet<String>) -> State {
        let mut state = self.clone();
        let computed = immediate_add(instruction).map(|(destination, source, step)| {
            let value = match self.get(&source) {
                Value::Known(mut affine) if !Value::Known(affine.clone()).written_by(index) => {
                    affine.offset += step;
                    Value::Known(affine)
                }
                _ => Value::Known(Affine {
                    base: Base::Written(index, destination.clone()),
                    offset: 0,
                }),
            };
            (destination, value)
        });
        for value in state.0.values_mut() {
            if value.written_by(index) {
                *value = Value::Unknown;
            }
        }
        if let Some((destination, value)) = computed {
            state.0.insert(destination, value);
            return state;
        }
        for register in registers_written(instruction).filter(|r| tracked.contains(r)) {
            let value = Value::Known(Affine {
                base: Base::Written(index, register.clone()),
                offset: 0,
            });
            state.0.insert(register, value);
        }
        state
    }

    /// Everything after a call: unknown.
    fn unknown(registers: &BTreeSet<String>) -> State {
        State(
            registers
                .iter()
                .map(|register| (register.clone(), Value::Unknown))
                .collect(),
        )
    }

    /// The values both states agree on.
    fn merge(&self, other: &State) -> State {
        let mut merged = BTreeMap::new();
        for register in self.0.keys().chain(other.0.keys()) {
            let (a, b) = (self.get(register), other.get(register));
            merged.insert(register.clone(), if a == b { a } else { Value::Unknown });
        }
        State(merged)
    }
}

/// What each register holds before each statement.
struct Values {
    before: Vec<Option<State>>,
    registers: BTreeSet<String>,
}

impl Values {
    fn of(statements: &[parser::Statement], graph: &Graph) -> Self {
        // Only a register an immediate add reads or writes can be part of a
        // pointer's value; the others are not followed.
        let registers: BTreeSet<String> = statements
            .iter()
            .filter_map(instruction_of)
            .filter_map(immediate_add)
            .flat_map(|(destination, source, _)| [destination, source])
            .filter(|register| register != "vi00")
            .collect();
        let mut values = Values {
            before: vec![None; statements.len()],
            registers,
        };
        if statements.is_empty() {
            return values;
        }
        values.before[0] = Some(State::default());
        let mut work = vec![0];
        while let Some(index) = work.pop() {
            for (to, state) in values.out_edges(statements, graph, index) {
                let next = match &values.before[to] {
                    Some(old) => old.merge(&state),
                    None => state,
                };
                if values.before[to].as_ref() != Some(&next) {
                    values.before[to] = Some(next);
                    work.push(to);
                }
            }
        }
        values
    }

    /// The state along each edge out of the statement at `index`.
    fn out_edges(
        &self,
        statements: &[parser::Statement],
        graph: &Graph,
        index: usize,
    ) -> Vec<(usize, State)> {
        let Some(before) = &self.before[index] else {
            return Vec::new();
        };
        let after = match &statements[index].kind {
            StatementKind::Instruction(instruction) => {
                before.after(index, instruction, &self.registers)
            }
            StatementKind::RawText(_) => State::unknown(&self.registers),
            _ => before.clone(),
        };
        graph.successors[index]
            .iter()
            .map(|&(to, call_return)| {
                let state = if call_return {
                    State::unknown(&self.registers)
                } else {
                    after.clone()
                };
                (to, state)
            })
            .collect()
    }

    /// What the registers hold on entering the loop `inside` from outside,
    /// at its labels `start..=header`; `None` when nothing enters it.
    fn entry(
        &self,
        statements: &[parser::Statement],
        graph: &Graph,
        start: usize,
        header: usize,
        inside: &std::ops::RangeInclusive<usize>,
    ) -> Option<State> {
        let mut entry: Option<State> = None;
        for index in 0..statements.len() {
            if inside.contains(&index) {
                continue;
            }
            for (to, state) in self.out_edges(statements, graph, index) {
                if (start..=header).contains(&to) {
                    entry = Some(match entry {
                        Some(old) => old.merge(&state),
                        None => state,
                    });
                }
            }
        }
        entry
    }
}

/// The operand class of an `offset(base)` memory operand.
const MEMORY_OFFSET: u8 = 29;

/// Whether an operand names `register` (lowercase).
fn names(operand: &parser::Operand, register: &str) -> bool {
    operand
        .register
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(register))
}

/// Whether the instruction is an `lq`, `sq`, `ilw` or `isw`, whose base
/// register reads with an offset.
fn offset_access(instruction: &Instruction) -> bool {
    let name = instruction.mnemonic.to_ascii_lowercase();
    let base = name.split('.').next().unwrap_or(&name);
    matches!(base, "lq" | "sq" | "ilw" | "isw")
}

/// `destination = source + step` for `iaddiu`, `iaddi` and `isubiu`, with
/// lowercase register names.
fn immediate_add(instruction: &Instruction) -> Option<(String, String, i64)> {
    let name = instruction.mnemonic.to_ascii_lowercase();
    let sign = match name.as_str() {
        "iaddiu" | "iaddi" => 1,
        "isubiu" => -1,
        _ => return None,
    };
    let [destination, source, step] = &instruction.operands[..] else {
        return None;
    };
    if destination.forced_register.is_some() || source.forced_register.is_some() {
        return None;
    }
    let destination = destination.register.as_deref()?.to_ascii_lowercase();
    let source = source.register.as_deref()?.to_ascii_lowercase();
    let step = evaluate(step.immediate.as_deref()?)?;
    Some((destination, source, sign * step))
}

/// An immediate's value, when it is a constant expression.
fn evaluate(text: &str) -> Option<i64> {
    let evaluation = crate::parser::expression::evaluate(text).ok()?;
    Some(i64::from(evaluation.number()?.to_i32()))
}

/// The lowercase registers an instruction writes.
fn registers_written(instruction: &Instruction) -> impl Iterator<Item = String> + '_ {
    instruction
        .operands
        .iter()
        .filter(|operand| operand.direction == Direction::Write)
        .filter_map(|operand| operand.register.as_deref().map(str::to_ascii_lowercase))
}

/// The lowercase registers an instruction reads.
fn registers_read(instruction: &Instruction) -> impl Iterator<Item = String> + '_ {
    instruction
        .operands
        .iter()
        .filter(|operand| operand.direction == Direction::Read)
        .filter_map(|operand| operand.register.as_deref().map(str::to_ascii_lowercase))
}

/// Whether a lowercase register name is a physical VI register.
fn is_physical(register: &str) -> bool {
    register
        .strip_prefix("vi")
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// Only labels and ordinary instructions: no regions, raw code, directives,
/// calls or register branches.
fn ordinary_body(statements: &[parser::Statement]) -> bool {
    statements.iter().all(|statement| match &statement.kind {
        StatementKind::Label(_) => true,
        StatementKind::Instruction(instruction) if instruction.mode == ParseMode::Vcl => {
            match instruction.synthetic() {
                None => {
                    !instruction.metadata.is_branch()
                        || instruction.metadata.subtype & (REGISTER | LINK) == 0
                }
                Some(
                    Synthetic::Barrier
                    | Synthetic::LoopCount
                    | Synthetic::LoopExtra
                    | Synthetic::LoopAbsolute,
                ) => true,
                Some(_) => false,
            }
        }
        _ => false,
    })
}

/// Whether the loop's `--LoopCS` promises no more than any loop does: at
/// least one iteration, and no stores past the last. A larger minimum or a
/// safe count lets the pipeliner's schedule decide what runs at fewer
/// iterations and how many stores run past the end, and a rewritten loop
/// gets another schedule: it would compute something else there.
fn promises_nothing(statements: &[parser::Statement]) -> bool {
    statements
        .iter()
        .filter_map(instruction_of)
        .filter(|instruction| instruction.synthetic() == Some(Synthetic::LoopCount))
        .all(|instruction| {
            let value = |at: usize| {
                instruction
                    .operands
                    .get(at)
                    .and_then(|operand| operand.immediate.as_deref())
                    .and_then(evaluate)
            };
            matches!((value(0), value(1)), (Some(minimum), Some(0)) if minimum <= 1)
        })
}

/// Whether the loop has at least as many lower instructions as upper ones.
fn lower_bound(statements: &[parser::Statement]) -> bool {
    use crate::isa::instruction::issue::{LOWER, UPPER};
    let (mut upper, mut lower) = (0, 0);
    for instruction in statements.iter().filter_map(instruction_of) {
        if instruction.synthetic().is_some() {
            continue;
        }
        match instruction.metadata.issue_mask {
            UPPER => upper += 1,
            LOWER => lower += 1,
            _ => {}
        }
    }
    lower >= upper
}

/// Whether a branch from outside the loop names one of its labels past the
/// header.
fn entered_past_header(
    statements: &[parser::Statement],
    labels: &BTreeMap<String, usize>,
    header: usize,
    inside: &std::ops::RangeInclusive<usize>,
) -> bool {
    statements.iter().enumerate().any(|(index, statement)| {
        !inside.contains(&index)
            && instruction_of(statement)
                .and_then(branch_target)
                .and_then(|target| labels.get(&target))
                .is_some_and(|&at| at > header && inside.contains(&at))
    })
}

/// Each label's statement index, by lowercase name.
fn label_indices(statements: &[parser::Statement]) -> BTreeMap<String, usize> {
    statements
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| match &statement.kind {
            StatementKind::Label(label) => Some((label.name.to_ascii_lowercase(), index)),
            _ => None,
        })
        .collect()
}

fn instruction_of(statement: &parser::Statement) -> Option<&Instruction> {
    match &statement.kind {
        StatementKind::Instruction(instruction) => Some(instruction),
        _ => None,
    }
}

/// A branch that reads a register and may fall through.
fn is_conditional_branch(instruction: &Instruction) -> bool {
    instruction.synthetic().is_none()
        && instruction.metadata.is_branch()
        && instruction.metadata.subtype & (REGISTER | LINK) == 0
        && instruction
            .operands
            .iter()
            .any(|operand| operand.register.is_some() && operand.direction == Direction::Read)
}

/// A branch's target label, lowercase.
fn branch_target(instruction: &Instruction) -> Option<String> {
    if instruction.synthetic().is_some() || !instruction.metadata.is_branch() {
        return None;
    }
    instruction
        .operands
        .iter()
        .rev()
        .find(|operand| operand.register.is_none())
        .and_then(|operand| operand.immediate.as_deref())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(source: &str) -> Vec<String> {
        let parsed = parser::parse(source).unwrap();
        rewrites(&parsed)
            .into_iter()
            .map(|rewrite| rewrite.key)
            .collect()
    }

    const ENTER: &str = ".init_vf_all\n\
                         .init_vi_all\n\
                         --enter\n\
                         in_vi top(vi01)\n\
                         in_vi count(vi02)\n\
                         --endenter\n";

    #[test]
    fn pointers_from_one_base_merge_into_the_tested_one() {
        let source = format!(
            "{ENTER}\
             iaddiu src,top,5\n\
             iaddiu dst,top,120\n\
             iadd last,src,count\n\
             loop:\n\
             lq value,0(src)\n\
             sq value,0(dst)\n\
             iaddiu src,src,1\n\
             iaddiu dst,dst,1\n\
             ibne src,last,loop\n\
             --exit\n\
             --endexit\n"
        );
        // `src` is the test's, so only `dst` goes.
        assert_eq!(keys(&source), ["merge loop dst into src"]);
        let parsed = parser::parse(&source).unwrap();
        let merged = &rewrites(&parsed)[0].program;
        let text: Vec<String> = merged
            .statements
            .iter()
            .filter_map(instruction_of)
            .map(|instruction| {
                let operands: Vec<&str> = instruction
                    .operands
                    .iter()
                    .map(|operand| operand.text.as_str())
                    .collect();
                format!("{} {}", instruction.mnemonic, operands.join(","))
            })
            .collect();
        assert!(text.contains(&"sq value,115(src)".to_string()), "{text:?}");
        assert!(
            !text.iter().any(|line| line.starts_with("iaddiu dst,dst")),
            "{text:?}"
        );
    }

    #[test]
    fn a_hint_that_promises_more_leaves_the_loop_alone() {
        let source = |hint: &str| {
            format!(
                "{ENTER}\
                 iaddiu src,top,5\n\
                 iaddiu dst,top,120\n\
                 iadd last,src,count\n\
                 loop:\n\
                 {hint}\
                 lq value,0(src)\n\
                 sq value,0(dst)\n\
                 iaddiu src,src,1\n\
                 iaddiu dst,dst,1\n\
                 ibne src,last,loop\n\
                 --exit\n\
                 --endexit\n"
            )
        };
        assert_eq!(keys(&source("--LoopCS 1,0\n")).len(), 1);
        // Six at least, or one store past the end: the schedule decides
        // what runs at fewer iterations and past the end.
        assert!(keys(&source("--LoopCS 6,0\n")).is_empty());
        assert!(keys(&source("--LoopCS 1,3\n")).is_empty());
    }

    #[test]
    fn pointers_from_two_values_do_not_merge() {
        // `dst` comes from the first `xtop`, `src` from the second.
        let source = format!(
            "{ENTER}\
             xtop base\n\
             iaddiu dst,base,120\n\
             xtop base\n\
             iaddiu src,base,5\n\
             iadd last,src,count\n\
             loop:\n\
             lq value,0(src)\n\
             sq value,0(dst)\n\
             iaddiu src,src,1\n\
             iaddiu dst,dst,1\n\
             ibne src,last,loop\n\
             --exit\n\
             --endexit\n"
        );
        assert!(keys(&source).is_empty());
    }

    #[test]
    fn a_countdown_tested_with_ibne_counts_with_a_pointer() {
        let source = format!(
            "{ENTER}\
             iaddiu src,top,5\n\
             iadd left,top,count\n\
             loop:\n\
             lq value,0(src)\n\
             sq value,1(src)\n\
             iaddiu src,src,1\n\
             isubiu left,left,1\n\
             ibne left,top,loop\n\
             --exit\n\
             --endexit\n"
        );
        assert_eq!(keys(&source), ["count loop left by src"]);
        // `ibgtz` stops at the first count at or below zero, which no
        // pointer test matches.
        assert!(keys(&source.replace("ibne left,top,loop", "ibgtz left,loop")).is_empty());
        // A loop that branches inside is left alone.
        let branching = source.replace(
            "sq value,1(src)\n",
            "ibeq count,vi00,skip\n\
             sq value,1(src)\n\
             skip:\n",
        );
        assert!(keys(&branching).is_empty());
    }

    #[test]
    fn kernel_periods_come_from_the_lpopt_comments() {
        let listing = "; _LPOPT_w=[ ldumb ] 6 [6 6] 6   [EXPL_DEMO_loop__MAIN_LOOP]\n\
                       ; _LNOPT_w=[ ] 3 [3 0] 3   [EXPL_DEMO_loop__EPI0]\n\
                       ; _LPOPT_w=[ vuta ] 29 [29 29] 29   [EXPL_DEMO_inner__MAIN_LOOP]\n";
        let periods = kernel_periods(listing);
        assert_eq!(periods.len(), 2);
        assert_eq!(periods["EXPL_DEMO_loop__MAIN_LOOP"], 6);
        assert_eq!(periods["EXPL_DEMO_inner__MAIN_LOOP"], 29);
        let shorter = listing.replace("] 6 [6 6]", "] 5 [5 5]");
        assert!(shorter_kernels(listing, &shorter));
        assert!(!shorter_kernels(&shorter, listing));
        assert!(!shorter_kernels(listing, listing));
    }
}
