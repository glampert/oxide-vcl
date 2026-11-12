//! Source-to-assembly orchestration for the reconstructed scheduler.
//!
//! This owned IR deliberately separates source, data flow,
//! schedule positions, allocation, and emission. Coverage differences from the
//! original optimizer are recorded in `docs/COMPATIBILITY.md`.

use crate::allocator::allocation::{Allocator, VirtualRegister};
use crate::analysis::flow::{self, Access, Definition};
use crate::diagnostics::{CompileMessage, Diagnostic, Message, Severity};
use crate::isa::instruction::{
    self, Direction, MaskMode, RegisterFamily, Syntax, class, id, issue, order, subtype, unit,
};
use crate::parser::{self, Directive, Instruction, ParseMode, StatementKind, Synthetic};
use crate::scheduler::schedule::{
    self, Dependency, Operation, RegisterAccess, ScheduleOptions, SchedulingProblem,
};
use crate::scheduler::timing::{
    self, Synchronization, TimedRow, TimingOperation, TimingRead, TimingWrite,
};
use crate::{Fix, Options};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

mod clip_history;
mod external_assignment;
mod flow_graph;
mod generated_rows;
mod raw_reduction;
mod region;
mod rows;
mod software_loops;
mod source_passes;
mod spill;
mod table_planning;
mod table_timing;
mod wrangle_tables;

use flow_graph::*;
use generated_rows::*;
use region::*;
use rows::*;
use software_loops::*;
use source_passes::*;
use table_planning::*;
use table_timing::*;
use wrangle_tables::*;

/// A compile failure: the source line (0 when there is none) and the typed
/// message, rendered by [`crate::diagnostics`] in either mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub line: usize,
    pub message: Message,
    kind: CompileErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CompileErrorKind {
    General,
    Allocation,
    /// No register fits a value live across streams (oxide mode only):
    /// spilling can help, scheduling strategies cannot.
    ExternalAllocation,
    ZeroCopyDestination,
    /// A region reads undeclared clip history that nothing observes, which
    /// the `clip` fix declares before compiling it again. Its message is
    /// vcl 1.4beta7's "used before set" error.
    UndeclaredClipHistory,
}

impl CompileError {
    /// The original copy-detector abort can return status zero despite an
    /// ERROR diagnostic and no assembly. Keep library failure distinct.
    pub fn exit_status(&self) -> u8 {
        if self.kind == CompileErrorKind::ZeroCopyDestination {
            0
        } else {
            255
        }
    }
}

impl CompileError {
    /// The error as a diagnostic, for [`crate::diagnostics::Reporter`].
    pub fn diagnostic(&self) -> Diagnostic {
        Diagnostic::new(Severity::Error, self.line, self.message.clone())
    }
}

/// Renders in vcl mode: `line N: message`, or the message alone.
impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic().fmt(f)
    }
}

impl std::error::Error for CompileError {}

#[derive(Clone, Debug, Default)]
pub struct Compiled {
    pub assembly: String,
    pub header: String,
    pub instruction_pairs: usize,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    Vector,
    Integer,
    Hardware,
}

#[derive(Clone, Debug)]
struct Register {
    name: String,
    family: Family,
    physical: Option<usize>,
}

#[derive(Clone, Default)]
struct Registers {
    ids: BTreeMap<(Family, String), usize>,
    values: Vec<Register>,
}

impl Registers {
    fn get(&mut self, name: &str, family: Family) -> usize {
        let physical = if family == Family::Hardware {
            None
        } else {
            physical(name, family)
        };
        let name = if family == Family::Hardware {
            name.to_ascii_uppercase()
        } else {
            physical.map_or_else(|| name.to_string(), |id| physical_name(family, id))
        };
        let key = (family, name.to_ascii_lowercase());
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = self.values.len();
        self.values.push(Register {
            name,
            family,
            physical,
        });
        self.ids.insert(key, id);
        id
    }
    fn named(&self, id: usize) -> &Register {
        &self.values[id]
    }
}

fn physical(name: &str, family: Family) -> Option<usize> {
    let name = name.to_ascii_lowercase();
    let (prefix, limit) = match family {
        Family::Vector => ("vf", 32),
        Family::Integer => ("vi", 16),
        Family::Hardware => return Some(0),
    };
    let digits = name.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok().filter(|&id| id < limit)
}

fn physical_name(family: Family, id: usize) -> String {
    match family {
        Family::Vector => format!("VF{id:02}"),
        Family::Integer => format!("VI{id:02}"),
        Family::Hardware => String::new(),
    }
}

fn family(class: u8) -> Option<Family> {
    match instruction::register_family(class) {
        RegisterFamily::Vector => Some(Family::Vector),
        RegisterFamily::Integer => Some(Family::Integer),
        RegisterFamily::Hardware => Some(Family::Hardware),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct Located {
    line: usize,
    instruction: Instruction,
    operands: Vec<Option<usize>>,
    effects: Effects,
}

#[derive(Clone, Debug, Default)]
struct Effects {
    reads: Vec<Access>,
    writes: Vec<Access>,
    latencies: BTreeMap<usize, u16>,
    side_effect: bool,
}

#[derive(Clone, Debug, Default)]
enum End {
    #[default]
    Fallthrough,
    Continue,
    Branch {
        target: Option<String>,
        tags: Vec<String>,
        conditional: bool,
        call: bool,
    },
    Exit {
        macro_text: Option<String>,
    },
}

#[derive(Clone, Debug, Default)]
struct LoopHints {
    minimum: Option<u32>,
    safe: u32,
    extra: u16,
    absolute: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableMode {
    Fixed,
    Linear,
    /// Reorderable raw debris is reduced after physical assignment. Synthetic
    /// endpoints remain protected until their full boundary lifecycle is ported.
    RawLinear,
}

#[derive(Clone, Debug)]
struct SelectedTable {
    rows: Vec<[Option<usize>; 2]>,
    mode: TableMode,
    /// A prologue/epilogue part installed from the loop optimizer: its rows
    /// are kept (no strategy search) and go through the late raw reduction.
    installed: bool,
    /// For a loop kernel scheduled by the loop optimizer: the strategy that
    /// won (vcl prints the kernel as an `_LPOPT_` table).
    kernel_strategy: Option<crate::scheduler::schedule::Strategy>,
}

#[derive(Clone, Debug, Default)]
struct Block {
    labels: Vec<String>,
    operations: Vec<usize>,
    outputs: Vec<Access>,
    /// Physical base an EXIT binds an output to (register ID to physical
    /// number). `Region::forced` holds a name's first binding; an output in
    /// another register is a separate web (`regweb`), so its
    /// own base stays with the exit.
    output_forced: BTreeMap<usize, usize>,
    end: End,
    loop_hints: LoopHints,
    start_fence: bool,
    end_fence: bool,
    debug_top: u8,
    debug_end: u8,
    /// Selected issue slots establish paired-read ownership during flow. Only
    /// linear generated parts admit subsequent extent/empty-row reduction.
    selected_table: Option<SelectedTable>,
    /// The stream's flow uid, which names an unlabelled stream
    /// (`__v_<unique>_<uid>`).
    stream: Option<u16>,
    /// The name vcl's optimizer comment gives a generated loop part.
    part_name: Option<String>,
}

#[derive(Clone, Default)]
struct Region {
    instructions: Vec<Located>,
    blocks: Vec<Block>,
    inputs: Vec<Access>,
    forced: BTreeMap<usize, usize>,
    raw_text: Vec<String>,
    /// Physical register (as a register ID) bound to one access of an
    /// instruction: (instruction, write, register). Set on installed loop
    /// kernels, whose registers come from loop local allocation.
    pinned: BTreeMap<(usize, bool, usize), usize>,
    /// Flow UID of the region's first code entry, which names an unlabeled
    /// first stream `__v_<unique>_<uid>`. None when the
    /// region has no code, whose stream the original leaves unnamed.
    stream: Option<u16>,
    /// Source register each `__web_N` register was formed from, for
    /// diagnostics.
    web_sources: BTreeMap<usize, usize>,
}

impl Region {
    fn block(&mut self) -> &mut Block {
        if self.blocks.is_empty() {
            self.blocks.push(Block::default());
        }
        self.blocks.last_mut().unwrap()
    }
    fn split(&mut self) {
        if !self.block().operations.is_empty() || !matches!(self.block().end, End::Fallthrough) {
            self.blocks.push(Block::default());
        }
    }
    fn nonempty(&self) -> bool {
        !self.inputs.is_empty()
            || !self.instructions.is_empty()
            || self
                .blocks
                .iter()
                .any(|b| matches!(b.end, End::Exit { .. }))
    }
}

/// An open `.show`/`.hide` scope and its file-wide number.
#[derive(Clone, Debug)]
struct Visibility {
    show: bool,
    names: Vec<String>,
    number: u32,
}

#[derive(Clone)]
struct Source {
    registers: Registers,
    /// Open visibility scopes, innermost last, and how many have been opened.
    visibility: Vec<Visibility>,
    scopes_opened: u32,
    regions: Vec<Region>,
    allowed_vf: Vec<u8>,
    allowed_vi: Vec<u8>,
    name: Option<String>,
    globals: BTreeSet<String>,
    syntax: Syntax,
    align: bool,
    /// The original's `.vu` directive handler sets this flag.
    vu_directive: bool,
    end_bit: bool,
    mpg: Option<String>,
    unique_labels: bool,
    unique_prefix: Option<String>,
    mode: crate::Mode,
    /// The fixes for vcl 1.4beta7's defects that apply.
    fixes: crate::Fixes,
    warnings: Vec<Diagnostic>,
    /// Names already warned about for their trailing field letters.
    field_letter_names: BTreeSet<String>,
    /// Each name's current base binding (register ID to physical number),
    /// the register's binding in the original decoder. Every
    /// mapped IN/OUT operand replaces it; an unmapped one takes it. It
    /// survives regions.
    bindings: BTreeMap<usize, usize>,
    /// Oxide mode's `.vu_work_area` requests, in source order.
    work_areas: Vec<spill::WorkArea>,
}

/// The original's global flow UIDs, one per source statement
/// (zero for statements that are not code). Entries with one UID form one
/// stream. The counter starts at 1 and advances for a change of parse mode or
/// raw submode, for a passthrough block and again after it, for a flagged
/// label, on entering or leaving a raw loop (each `.vsmloop` has its own
/// ordinal), for a branch target or indirect tag, at an ENTER, EXIT, exit
/// macro or barrier (which start the new stream), after an ENDENTER, ENDEXIT
/// or CONT, and after a branch (in raw code, after its delay pair).
fn flow_uids(statements: &[parser::Statement], targets: &BTreeSet<String>) -> Vec<u16> {
    let mut uids = vec![0; statements.len()];
    let mut counter: u16 = 1;
    let mut mode = (0u8, false);
    let (mut loops, mut raw_loop, mut flow) = (0u16, 0u16, 0u16);
    let mut delay = 0u8;
    let mut previous_pass = false;
    let (mut flagged, mut target) = (false, false);
    let mut index = 0;
    while index < statements.len() {
        let statement = &statements[index];
        let first = match &statement.kind {
            StatementKind::Label(label) => {
                flagged |= label.modifiers != 0;
                target |= targets.contains(&label.name.to_ascii_lowercase());
                index += 1;
                continue;
            }
            StatementKind::Directive(directive) => {
                match directive {
                    Directive::RawLoop { .. } => {
                        loops += 1;
                        raw_loop = loops;
                    }
                    Directive::EndRawLoop => raw_loop = 0,
                    _ => {}
                }
                index += 1;
                continue;
            }
            StatementKind::RawText(_) => {
                // A passthrough block is one entry.
                if !previous_pass {
                    counter += 1;
                    previous_pass = true;
                }
                index += 1;
                continue;
            }
            StatementKind::Instruction(instruction) => instruction,
        };
        // One source line is one entry, with up to two instructions.
        let end = statements[index..]
            .iter()
            .position(|other| {
                other.line != statement.line || !matches!(other.kind, StatementKind::Instruction(_))
            })
            .map_or(statements.len(), |count| index + count);
        let raw = first.mode == ParseMode::Raw;
        let entry = (if raw { 3 } else { 1 }, raw && first.raw_reorder);
        if entry != mode {
            counter += 1;
            mode = entry;
        }
        if std::mem::take(&mut previous_pass) {
            counter += 1;
        }
        if std::mem::take(&mut flagged) {
            counter += 1;
        }
        if raw_loop == 0 {
            if flow > 0 {
                flow = 0;
                counter += 1;
            }
        } else if raw_loop != flow {
            counter += 1;
            flow = raw_loop;
        }
        if std::mem::take(&mut target) {
            counter += 1;
        }
        let mut uid = counter;
        for statement in &statements[index..end] {
            let StatementKind::Instruction(instruction) = &statement.kind else {
                continue;
            };
            match instruction.synthetic() {
                Some(
                    Synthetic::Enter | Synthetic::Exit | Synthetic::Barrier | Synthetic::ExitMacro,
                ) => {
                    counter += 1;
                    uid += 1;
                }
                Some(Synthetic::EndEnter | Synthetic::EndExit | Synthetic::Continue) => {
                    counter += 1
                }
                Some(_) => {}
                None if instruction.metadata.is_branch() => delay = if raw { 2 } else { 1 },
                None => {}
            }
        }
        if delay != 0 {
            if delay == 1 {
                counter += 1;
            }
            delay -= 1;
        }
        uids[index..end].fill(uid);
        index = end;
    }
    uids
}

impl Source {
    fn new(options: &Options) -> Self {
        let mut registers = Registers::default();
        for id in 0..32 {
            registers.get(&physical_name(Family::Vector, id), Family::Vector);
        }
        for id in 0..16 {
            registers.get(&physical_name(Family::Integer, id), Family::Integer);
        }
        Self {
            registers,
            regions: Vec::new(),
            allowed_vf: vec![0; 32],
            allowed_vi: vec![0; 16],
            name: None,
            globals: BTreeSet::new(),
            syntax: if options.new_syntax {
                Syntax::New
            } else {
                Syntax::Old
            },
            align: !options.disable_alignment,
            vu_directive: false,
            // The historical -e byte is consulted only by the empty-NOP
            // emitter. Real upper instructions still receive their E bit.
            end_bit: true,
            mpg: options.mpg.filter(|&n| n >= 0).map(|n| n.to_string()),
            unique_labels: options.unique_labels,
            unique_prefix: None,
            visibility: Vec::new(),
            scopes_opened: 0,
            mode: options.mode,
            fixes: options.enabled_fixes(),
            warnings: Vec::new(),
            field_letter_names: BTreeSet::new(),
            bindings: BTreeMap::new(),
            work_areas: Vec::new(),
        }
    }

    /// The name a symbolic register has inside the open visibility scopes
    /// (observed from vcl 1.4beta7, `visibility-*`). The innermost scope that
    /// makes the name local renames it to `<scope number><name>`: a `.hide`
    /// scope that lists it, or a `.show` scope that does not. Otherwise the
    /// next scope outward decides. Lists match case-sensitively; physical
    /// registers, hardware registers and names starting with `_` are never
    /// renamed.
    fn qualify(&self, name: &str, family: Family) -> Option<String> {
        if family == Family::Hardware || name.starts_with('_') || physical(name, family).is_some() {
            return None;
        }
        self.visibility.iter().rev().find_map(|scope| {
            let listed = scope.names.iter().any(|listed| listed == name);
            (listed != scope.show).then(|| format!("{}{name}", scope.number))
        })
    }

    /// Lowers a parsed instruction: checks its operand names, applies the
    /// visibility scopes, assigns register identities and collects its effects.
    fn lower(
        &mut self,
        mut instruction: Instruction,
        line: usize,
    ) -> Result<Located, CompileError> {
        self.note_field_letters(&mut instruction, line)?;
        reject_reserved_names(&instruction, line)?;
        if !self.visibility.is_empty() {
            self.qualify_operands(&mut instruction);
        }
        let operands = self.operand_ids(&instruction, line)?;
        // Memory tags get a register identity for the loop tables, which
        // model them as resources (`add_memory_tag_resources`). They are not
        // data-flow effects.
        if instruction.metadata.is_memory() {
            self.register_memory_tags(&instruction);
        }
        let effects = self.collect_effects(&instruction);
        Ok(Located {
            line,
            instruction,
            operands,
            effects,
        })
    }

    /// Handles operands whose symbolic name ended in field letters.
    fn note_field_letters(
        &mut self,
        instruction: &mut Instruction,
        line: usize,
    ) -> Result<(), CompileError> {
        // Old syntax reads a symbolic name's trailing x/y/z/w as a field
        // selection (`copy` as `cop` with field y). vcl compiles that
        // silently; in vcl mode the port compiles it the same way and warns,
        // in oxide mode it is an error. The note is cleared so that copies
        // of the instruction (loop clones) do not repeat it.
        for operand in &mut instruction.operands {
            let Some(token) = operand.field_letters.take() else {
                continue;
            };
            let message = CompileMessage::FieldLettersInName {
                name: operand.register.clone().unwrap_or_default(),
                fields: token[operand.register.as_ref().map_or(0, String::len)..].to_string(),
                token,
            };
            match self.mode {
                crate::Mode::Vcl => {
                    if let CompileMessage::FieldLettersInName { token, .. } = &message
                        && self.field_letter_names.insert(token.clone())
                    {
                        self.warnings.push(Diagnostic::warning(message).at(line));
                    }
                }
                crate::Mode::Oxide => return Err(error(line, message)),
            }
        }
        Ok(())
    }

    /// Renames the operands' symbolic registers into the open visibility scopes.
    fn qualify_operands(&self, instruction: &mut Instruction) {
        for operand in &mut instruction.operands {
            if let (Some(name), Some(family)) = (&operand.register, family(operand.class))
                && let Some(local) = self.qualify(name, family)
            {
                operand.register = Some(local);
            }
        }
    }

    /// The register identity of each operand, `None` for operands without one.
    fn operand_ids(
        &mut self,
        instruction: &Instruction,
        line: usize,
    ) -> Result<Vec<Option<usize>>, CompileError> {
        let mut operands = Vec::with_capacity(instruction.operands.len());
        for operand in &instruction.operands {
            let id = if let Some(name) = &operand.register {
                if let Some(family) = family(operand.class) {
                    Some(self.registers.get(name, family))
                } else if matches!(
                    operand.class,
                    class::STATUS_FROM_DIVIDE..=class::STATUS_SET | class::EXTERNAL
                ) {
                    None
                } else {
                    return Err(error(line, CompileMessage::OperandPoolUnsupported));
                }
            } else {
                None
            };
            operands.push(id);
        }
        Ok(operands)
    }

    /// Gives each memory tag of an address operand a hardware register identity.
    fn register_memory_tags(&mut self, instruction: &Instruction) {
        for operand in &instruction.operands {
            if class::is_address(operand.class) {
                for tag in &operand.tags {
                    self.registers.get(&format!(":{tag}"), Family::Hardware);
                }
            }
        }
    }

    /// The reads, writes, latencies and side-effect flag of an instruction.
    fn collect_effects(&mut self, instruction: &Instruction) -> Effects {
        let expanded = crate::analysis::prune::expand(instruction);
        let mut effects = Effects::default();
        for operand in &expanded.operands {
            let (Some(name), Some(family)) = (&operand.register, family(operand.class)) else {
                continue;
            };
            let register = self.registers.get(name, family);
            let access = Access {
                register,
                mask: operand.mask,
            };
            match operand.direction {
                Direction::Read => effects.reads.push(access),
                Direction::Write => {
                    effects.writes.push(access);
                    effects
                        .latencies
                        .insert(register, operand.latency.max(0) as u16);
                }
                Direction::Unknown => {}
            }
            if matches!(
                operand.class,
                class::ADDRESS_INCREMENT | class::ADDRESS_DECREMENT
            ) {
                effects.writes.push(access);
                effects.latencies.insert(register, 1);
            }
        }

        effects.side_effect = instruction.metadata.is_branch()
            || instruction.metadata.is_store()
            // XGKICK starts a transfer; XTOP/XITOP only read VIF state and
            // are pruned when unused (`can_prune`, as in the original).
            || (instruction.metadata.unit_mask & unit::EXTERNAL != 0
                && !crate::analysis::prune::can_prune(instruction))
            || instruction.modifiers != 0
            || instruction.mode == ParseMode::Raw;
        effects
    }
}

/// Symbolic names may not be spelled like a hardware resource
/// (observed from vcl 1.4beta7): every resource is reserved for VF
/// names, and all but the scalar I, Q, P and R for VI names.
fn reject_reserved_names(instruction: &Instruction, line: usize) -> Result<(), CompileError> {
    for operand in &instruction.operands {
        let (Some(name), Some(family @ (Family::Vector | Family::Integer))) =
            (&operand.register, family(operand.class))
        else {
            continue;
        };
        let scalar = matches!(name.to_ascii_uppercase().as_str(), "I" | "Q" | "P" | "R");
        if source_passes::hardware_register(name).is_some()
            && !(family == Family::Integer && scalar)
        {
            let kind = if family == Family::Vector {
                "VU User floating point"
            } else {
                "VU User integer"
            };
            return Err(error(
                line,
                CompileMessage::ReservedName {
                    name: name.to_string(),
                    kind: kind.to_string(),
                },
            ));
        }
    }
    Ok(())
}

impl Source {
    fn parse_program(&mut self, program: parser::Program) -> Result<(), CompileError> {
        let mut state = ParseState {
            current: Region::default(),
            binding: None,
            raw_delay: None,
        };
        let branch_labels = branch_label_set(&program);
        let uids = flow_uids(&program.statements, &branch_labels);

        for (index, statement) in program.statements.into_iter().enumerate() {
            let line = statement.line;
            if state.raw_delay.is_some()
                && !matches!(&statement.kind, StatementKind::Instruction(instruction)
                    if instruction.mode == ParseMode::Raw && instruction.synthetic().is_none())
            {
                return Err(error(line, CompileMessage::RawBranchDelayBeforeBoundary));
            }
            match statement.kind {
                StatementKind::Directive(directive) => {
                    self.apply_directive(&mut state, directive, line)?
                }
                StatementKind::Label(label) => apply_label(&mut state, label, &branch_labels),
                StatementKind::RawText(text) => {
                    state.current.raw_text.push(text);
                }
                StatementKind::Instruction(instruction) => {
                    if let Some(synthetic) = instruction.synthetic() {
                        self.apply_synthetic(&mut state, synthetic, instruction, line)?;
                        continue;
                    }
                    self.apply_instruction(&mut state, instruction, line, uids[index])?;
                }
            }
        }

        if state.raw_delay.is_some() {
            return Err(error(0, CompileMessage::RawBranchDelay));
        }
        if state.current.nonempty() {
            self.regions.push(state.current);
        }
        self.fold_synthetic_exits();
        Ok(())
    }

    /// Applies one directive to the source settings or the open region.
    fn apply_directive(
        &mut self,
        state: &mut ParseState,
        directive: Directive,
        line: usize,
    ) -> Result<(), CompileError> {
        match directive {
            Directive::Vu => self.vu_directive = true,
            Directive::Name(name) => self.name = Some(name),
            Directive::Global(name) => {
                self.globals.insert(name);
            }
            Directive::Syntax(syntax) => self.syntax = syntax,
            Directive::NoAligns => self.align = false,
            // The original's no-ebits directive handler writes the same byte as no_aligns.
            Directive::NoEbits => self.align = false,
            Directive::Mpg(address) => self.mpg = Some(address),
            Directive::InitAll { vector } => {
                let (allowed, mask) = if vector {
                    (&mut self.allowed_vf, 15)
                } else {
                    (&mut self.allowed_vi, 1)
                };
                allowed[1..].fill(mask);
            }
            Directive::InitRegisters {
                vector,
                enable,
                names,
            } => self.init_registers(vector, enable, names, line)?,
            Directive::Align(_) => {} // Output boundaries carry the original default alignment.
            Directive::Raw { .. }
            | Directive::EndRaw
            | Directive::RawLoop { .. }
            | Directive::EndRawLoop => state.current.split(),
            Directive::Visibility { show, names } => {
                self.scopes_opened += 1;
                self.visibility.push(Visibility {
                    show,
                    names,
                    number: self.scopes_opened,
                });
            }
            // The parser has already rejected a scope closed with the
            // other kind; an unmatched close only warns.
            Directive::EndVisibility { .. } => {
                self.visibility.pop();
            }
            Directive::Unique(prefix) => {
                self.unique_labels = true;
                if prefix.is_some() {
                    self.unique_prefix = prefix;
                }
            }
            Directive::WorkArea {
                device,
                address,
                size,
                address_text,
                size_text,
            } => {
                // vcl never installs the memory pool a work area needs
                // (observed from vcl 1.4beta7, `spill-vf-area`); oxide mode
                // spills registers that do not fit into it.
                if self.mode == crate::Mode::Oxide {
                    let area = spill::WorkArea::parse(&device, &address_text, &size_text)
                        .map_err(|message| error(line, message))?;
                    self.work_areas.push(area);
                    return Ok(());
                }
                let request =
                    crate::analysis::memory::WorkAreaRequest::from_parsed(&device, address, size)
                        .map_err(|e| internal(line, e))?;
                crate::analysis::memory::activate_marigold(&request)
                    .map_err(|e| internal(line, e))?;
            }
        }
        Ok(())
    }

    /// Enables or disables the physical registers named by an
    /// `.init_*_registers` directive in the allowed masks.
    fn init_registers(
        &mut self,
        vector: bool,
        enable: bool,
        names: Vec<String>,
        line: usize,
    ) -> Result<(), CompileError> {
        let family = if vector {
            Family::Vector
        } else {
            Family::Integer
        };
        let allowed = if vector {
            &mut self.allowed_vf
        } else {
            &mut self.allowed_vi
        };

        for name in names {
            let (base, suffix) = name.split_once('.').unwrap_or((&name, ""));
            let (first, last) = if let Some((first, last)) = base.split_once('-') {
                (physical(first, family), physical(last, family))
            } else {
                let id = physical(base, family);
                (id, id)
            };
            let first = first.ok_or_else(|| {
                error(
                    line,
                    CompileMessage::InvalidPhysicalRegister {
                        name: name.to_string(),
                    },
                )
            })?;
            let last = last.ok_or_else(|| {
                error(
                    line,
                    CompileMessage::InvalidPhysicalRegister {
                        name: name.to_string(),
                    },
                )
            })?;
            let mask = if vector {
                if suffix.is_empty() {
                    15
                } else {
                    instruction::Mask::parse(suffix)
                        .ok_or_else(|| error(line, CompileMessage::InvalidComponentMask))?
                        .0
                }
            } else {
                1
            };

            for (id, allowed) in allowed
                .iter_mut()
                .enumerate()
                .take(first.max(last) + 1)
                .skip(first.min(last))
            {
                if id != 0 {
                    if enable {
                        *allowed |= mask;
                    } else {
                        *allowed &= !mask;
                    }
                }
            }
        }
        Ok(())
    }

    /// Applies one synthetic (assembler-level) instruction to the open region.
    fn apply_synthetic(
        &mut self,
        state: &mut ParseState,
        synthetic: Synthetic,
        instruction: Instruction,
        line: usize,
    ) -> Result<(), CompileError> {
        match synthetic {
            Synthetic::Enter => self.begin_enter(state),
            Synthetic::EndEnter => state.binding = None,
            Synthetic::Exit => {
                state.current.block().end = End::Exit { macro_text: None };
                state.binding = Some(false);
            }
            Synthetic::ExitMacro => {
                let text = instruction
                    .operands
                    .iter()
                    .find_map(|o| o.immediate.as_ref())
                    .cloned()
                    .unwrap_or_default();
                state.current.block().end = End::Exit {
                    macro_text: Some(text.trim_matches('"').to_string()),
                };
                state.binding = Some(false);
            }
            Synthetic::EndExit => state.binding = None,
            Synthetic::Input | Synthetic::Output => {
                if instruction.mnemonic.eq_ignore_ascii_case("in_hw_mac") {
                    return Err(error(line, CompileMessage::ExplicitMacWrite));
                }
                let located = self.lower(instruction, line)?;
                for (operand, id) in located.instruction.operands.iter().zip(&located.operands) {
                    let Some(id) = *id else { continue };
                    self.bind_interface_operand(state, operand, id, line)?;
                }
            }
            Synthetic::Barrier => {
                state.current.split();
                state.current.block().start_fence = true;
            }
            Synthetic::Continue => {
                if !matches!(state.current.block().end, End::Fallthrough) {
                    return Err(error(line, CompileMessage::ContWithEndMode));
                }
                state.current.block().end = End::Continue;
                state.current.split();
            }
            Synthetic::LoopCount => self.apply_loop_count(state, &instruction, line)?,
            Synthetic::LoopAbsolute => self.apply_loop_absolute(state, &instruction, line)?,
            Synthetic::LoopExtra => apply_loop_extra(state, &instruction, line)?,
            Synthetic::Unknown => {
                return Err(error(line, CompileMessage::UnknownSynthetic));
            }
        }
        Ok(())
    }

    /// Starts a new input region at an ENTER and declares its implicit inputs.
    fn begin_enter(&mut self, state: &mut ParseState) {
        if state.current.nonempty() {
            self.regions.push(std::mem::take(&mut state.current));
        }
        state.binding = Some(true);
        // The original's dependency binder adds these two
        // inherited sticky values at every ENTER.
        // This declares incoming state, not a zero value.
        for resource in crate::analysis::status::implicit_entry_inputs() {
            let register = self.registers.get(resource.name(), Family::Hardware);
            state.current.inputs.push(Access { register, mask: 1 });
        }
        // It also declares the external-unit resource
        // that XGKICK reads and writes (inferred from
        // vcl 1.4beta7), so each kick depends on the
        // previous one.
        let register = self.registers.get("EXT", Family::Hardware);
        state.current.inputs.push(Access { register, mask: 1 });
    }

    /// Binds one operand of an ENTER or EXIT interface statement as a region
    /// input or a block output, with its forced physical base if any.
    fn bind_interface_operand(
        &mut self,
        state: &mut ParseState,
        operand: &parser::Operand,
        id: usize,
        line: usize,
    ) -> Result<(), CompileError> {
        let family = self.registers.named(id).family;
        let name = self.registers.named(id).name.clone();
        let named = family != Family::Hardware && self.registers.named(id).physical.is_none();
        // The original decoder gives every interface operand
        // its own base: an explicit mapping, which
        // also rebinds the name, or else the
        // name's current binding. A fresh name
        // with neither is an error, at ENTER and
        // EXIT alike.
        let base = match &operand.forced_register {
            Some(text) => {
                let physical = physical(text, family);
                if let Some(physical) = physical
                    && let Some(previous) = self.bindings.insert(id, physical)
                    && previous != physical
                {
                    self.warnings.push(
                        Diagnostic::warning(CompileMessage::BindingChanged {
                            name: name.to_string(),
                            from: physical_name(family, previous),
                            to: physical_name(family, physical),
                        })
                        .at(line),
                    );
                }
                physical
            }
            None if named && state.binding.is_some() => {
                Some(self.bindings.get(&id).copied().ok_or_else(|| {
                    error(
                        line,
                        CompileMessage::UnmappableBase {
                            name: name.to_string(),
                        },
                    )
                })?)
            }
            None => None,
        };
        let access = Access {
            register: id,
            mask: operand.mask.0,
        };

        if state.binding == Some(true) {
            state.current.inputs.push(access);
            // Repeated ENTER bindings of a name
            // are one value: the last one wins.
            if let Some(base) = base {
                state.current.forced.insert(id, base);
            }
        } else {
            let block = state.current.block();
            block.outputs.push(access);
            if let Some(base) = base {
                // One EXIT reads one value, so two
                // bases are one web split in two.
                if let Some(previous) = block.output_forced.insert(id, base)
                    && previous != base
                {
                    return Err(error(
                        line,
                        CompileMessage::ConflictingBases {
                            first: physical_name(family, base),
                            second: physical_name(family, previous),
                        },
                    ));
                }
            }
        }
        Ok(())
    }

    /// Lowers a real instruction and appends it to the open region, ending
    /// the block at a branch and tracking a raw branch delay slot.
    fn apply_instruction(
        &mut self,
        state: &mut ParseState,
        instruction: Instruction,
        line: usize,
        uid: u16,
    ) -> Result<(), CompileError> {
        let located = self.lower(instruction, line)?;
        let is_branch = located.instruction.metadata.is_branch();
        if is_branch {
            if state.raw_delay.is_some() {
                return Err(error(line, CompileMessage::BranchInRawDelaySlot));
            }
            let metadata = located.instruction.metadata;
            let target = located
                .instruction
                .operands
                .iter()
                .rev()
                .find_map(|o| o.immediate.clone());
            let tags = located
                .instruction
                .operands
                .iter()
                .flat_map(|o| o.tags.iter().cloned())
                .collect();
            state.current.block().end = End::Branch {
                target,
                tags,
                conditional: metadata.subtype
                    == subtype::branch::JUMP | subtype::branch::CONDITIONAL,
                call: metadata.subtype & subtype::branch::LINK != 0,
            };
        }

        let current = &mut state.current;
        let id = current.instructions.len();
        current.instructions.push(located);
        current.block().operations.push(id);
        current.stream.get_or_insert(uid);
        current.block().stream.get_or_insert(uid);

        if is_branch {
            if current.instructions[id].instruction.mode == ParseMode::Raw {
                state.raw_delay = Some((line, 0));
            } else {
                current.blocks.push(Block::default());
            }
        } else if let Some((branch_line, count)) = &mut state.raw_delay
            && line != *branch_line
        {
            *count += 1;
            if *count == 2 {
                current.blocks.push(Block::default());
                state.raw_delay = None;
            }
        }
        Ok(())
    }

    // An invalid loop hint argument is reported and the
    // compile goes on (observed from vcl 1.4beta7): the
    // arguments are applied in order up to the bad one,
    // so `--LoopCS 0,0` sets nothing and `--LoopCS 1,-1`
    // keeps the count. The messages are the original's,
    // including `--LoopAbs`'s ">=2" for a minimum of 3.
    /// Applies a `--LoopCS` hint to the open block.
    fn apply_loop_count(
        &mut self,
        state: &mut ParseState,
        instruction: &Instruction,
        line: usize,
    ) -> Result<(), CompileError> {
        let minimum = immediate_i32(instruction, 0, line)?;
        let safe = immediate_i32(instruction, 1, line)?;
        if minimum < 1 {
            self.warnings
                .push(Diagnostic::warning(CompileMessage::LoopCsKnownCount).at(line));
            return Ok(());
        }
        state.current.block().loop_hints.minimum = Some(minimum as u32);
        if safe < 0 {
            self.warnings
                .push(Diagnostic::warning(CompileMessage::LoopCsSafeCount).at(line));
            return Ok(());
        }
        state.current.block().loop_hints.safe = safe as u32;
        Ok(())
    }

    /// Applies a `--LoopAbs` hint to the open block.
    fn apply_loop_absolute(
        &mut self,
        state: &mut ParseState,
        instruction: &Instruction,
        line: usize,
    ) -> Result<(), CompileError> {
        let value = immediate_i32(instruction, 0, line)?;
        if value < 3 {
            self.warnings
                .push(Diagnostic::warning(CompileMessage::LoopAbsCount).at(line));
            return Ok(());
        }
        let value =
            u16::try_from(value).map_err(|_| error(line, CompileMessage::LoopPeriodRange))?;
        state.current.block().loop_hints.absolute = Some(value);
        Ok(())
    }

    /// Folds a synthetic exit into its immediately preceding code stream; the
    /// original performs this during stream merging.
    fn fold_synthetic_exits(&mut self) {
        for region in &mut self.regions {
            let mut id = 1;
            while id < region.blocks.len() {
                if region.blocks[id].labels.is_empty()
                    && region.blocks[id].operations.is_empty()
                    && matches!(region.blocks[id].end, End::Exit { .. })
                    && matches!(region.blocks[id - 1].end, End::Fallthrough)
                {
                    let exit = region.blocks.remove(id);
                    region.blocks[id - 1].end = exit.end;
                    region.blocks[id - 1].outputs.extend(exit.outputs);
                    region.blocks[id - 1]
                        .output_forced
                        .extend(exit.output_forced);
                } else {
                    id += 1;
                }
            }
        }
    }
}

/// The region being built and the interface and branch-delay state that
/// carries from one statement to the next.
struct ParseState {
    current: Region,
    binding: Option<bool>,
    raw_delay: Option<(usize, u8)>,
}

/// The lowercased labels that are branch targets or indirect tags.
fn branch_label_set(program: &parser::Program) -> BTreeSet<String> {
    // Only a branch target or indirect tag starts a stream at its label;
    // immediates and `.global` exports do not (verified on the original).
    program
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            StatementKind::Instruction(instruction)
                if instruction.synthetic().is_none() && instruction.metadata.is_branch() =>
            {
                Some(instruction)
            }
            _ => None,
        })
        .flat_map(|instruction| {
            let target = instruction
                .operands
                .iter()
                .rev()
                .find_map(|o| o.immediate.clone());
            target
                .into_iter()
                .chain(instruction.operands.iter().flat_map(|o| o.tags.clone()))
        })
        .map(|label| label.to_ascii_lowercase())
        .collect()
}

/// Places a label: a referenced or modified one starts a new stream, an
/// unreferenced one inside a stream is dropped.
fn apply_label(state: &mut ParseState, label: parser::Label, branch_labels: &BTreeSet<String>) {
    let current = &mut state.current;
    if label.modifiers != 0 || branch_labels.contains(&label.name.to_ascii_lowercase()) {
        current.split();
    } else if !current.block().operations.is_empty()
        || !matches!(current.block().end, End::Fallthrough)
    {
        // An unreferenced label inside a stream is discarded.
        return;
    }
    current.block().debug_top |= label.modifiers;
    current.block().labels.push(label.name);
}

// A negative extra period is reported too, but the
// original then fails in loop scheduling (exit 255).
/// Applies a `--LoopExtra` hint to the open block.
fn apply_loop_extra(
    state: &mut ParseState,
    instruction: &Instruction,
    line: usize,
) -> Result<(), CompileError> {
    let value = immediate_i32(instruction, 0, line)?;
    if value < 0 {
        return Err(error(line, CompileMessage::LoopExtraCount));
    }
    let value = u16::try_from(value).map_err(|_| error(line, CompileMessage::LoopPeriodRange))?;
    state.current.block().loop_hints.extra = value;
    Ok(())
}

fn immediate_i32(
    instruction: &Instruction,
    index: usize,
    line: usize,
) -> Result<i32, CompileError> {
    let text = instruction
        .operands
        .get(index)
        .and_then(|operand| operand.immediate.as_deref())
        .ok_or_else(|| error(line, CompileMessage::MissingLoopHint))?;
    crate::parser::expression::evaluate(text)
        .ok()
        .and_then(|value| value.number())
        .map(|value| value.to_i32())
        .ok_or_else(|| error(line, CompileMessage::LoopHintNotConstant))
}
fn error(line: usize, message: impl Into<Message>) -> CompileError {
    CompileError {
        line,
        message: message.into(),
        kind: CompileErrorKind::General,
    }
}

/// A failure inside one of the port's phases, described by that phase.
fn internal(line: usize, detail: impl fmt::Display) -> CompileError {
    error(line, CompileMessage::Internal(detail.to_string()))
}

fn allocation_error(failure: crate::allocator::allocation::AllocationError) -> CompileError {
    let mut result = error(0, CompileMessage::Allocation(failure.clone()));
    if matches!(
        failure,
        crate::allocator::allocation::AllocationError::NoFit { .. }
            | crate::allocator::allocation::AllocationError::ForcedConflict { .. }
    ) {
        result.kind = CompileErrorKind::Allocation;
    }
    result
}

/// Compile a complete VCL source. Parsing and allocation errors retain source
/// line numbers. The caller decides where output files and diagnostics go.
pub fn compile(
    source: &str,
    filename: &str,
    options: &Options,
) -> Result<Compiled, Vec<CompileError>> {
    let parsed = parser::parse_with_options(
        source,
        parser::ParserOptions {
            syntax: if options.new_syntax {
                Syntax::New
            } else {
                Syntax::Old
            },
            allow_incomplete: false,
        },
    )
    .map_err(|errors| {
        errors
            .into_iter()
            .map(|e| error(e.line, e.message))
            .collect::<Vec<_>>()
    })?;

    if options.mode != crate::Mode::Oxide {
        return compile_fixed(parsed, filename, options).map_err(|failure| vec![failure.error]);
    }

    // Oxide mode spills a value to the work area each time a region's
    // registers do not fit, and compiles the rewritten program again.
    let mut spills = spill::Spills::default();
    let failure = match compile_fixed(parsed.clone(), filename, options) {
        Ok(output) => return Ok(output),
        Err(failure) => failure,
    };
    spills.add(&parsed, *failure).map_err(|e| vec![e])?;

    // A loop that does not fit fails only after the loop optimizer has
    // searched every period, so the spills are first found with it off. A
    // pipelined loop can need more, which the second pass adds.
    let linear = Options {
        disable_loops: true,
        ..options.clone()
    };
    while let Err(failure) = compile_fixed(spills.apply(&parsed), filename, &linear) {
        if spills.add(&parsed, *failure).is_err() {
            break;
        }
    }

    loop {
        match compile_fixed(spills.apply(&parsed), filename, options) {
            Ok(mut output) => {
                output.warnings.extend(spills.notes());
                return Ok(output);
            }
            Err(failure) => spills.add(&parsed, *failure).map_err(|e| vec![e])?,
        }
    }
}

/// Why compiling a parsed program failed, with what spilling needs to know.
struct Failure {
    error: CompileError,
    /// The source lines of the region that failed, when a region did.
    lines: Option<std::ops::RangeInclusive<usize>>,
    work_areas: Vec<spill::WorkArea>,
}

impl From<CompileError> for Box<Failure> {
    fn from(error: CompileError) -> Self {
        Box::new(Failure {
            error,
            lines: None,
            work_areas: Vec::new(),
        })
    }
}

/// Compiles a parsed program with the source rewrites its fixes need: with
/// the `clip` fix, each region whose undeclared clip history nothing
/// observes gets `in_hw_clip CLIP`, and the program is compiled again.
fn compile_fixed(
    mut parsed: parser::Program,
    filename: &str,
    options: &Options,
) -> Result<Compiled, Box<Failure>> {
    // Each round declares the history in one more region, so there are at
    // most as many rounds as regions.
    let regions = parsed
        .statements
        .iter()
        .filter(|statement| {
            matches!(&statement.kind, StatementKind::Instruction(instruction)
                if instruction.synthetic() == Some(Synthetic::Enter))
        })
        .count();
    for _ in 0..regions {
        let failure = match compile_parsed(parsed.clone(), filename, options) {
            Err(failure) if failure.error.kind == CompileErrorKind::UndeclaredClipHistory => {
                failure
            }
            result => return result,
        };
        let declared = failure
            .lines
            .as_ref()
            .and_then(|lines| clip_history::declare_history(&parsed, lines));
        match declared {
            Some(declared) => parsed = declared,
            None => return Err(failure.into_general()),
        }
    }
    compile_parsed(parsed, filename, options).map_err(|failure| failure.into_general())
}

impl Failure {
    /// The failure with its error reported as it is, not retried.
    fn into_general(mut self: Box<Self>) -> Box<Self> {
        if self.error.kind == CompileErrorKind::UndeclaredClipHistory {
            self.error.kind = CompileErrorKind::General;
        }
        self
    }
}

/// Compiles a parsed program.
fn compile_parsed(
    parsed: parser::Program,
    filename: &str,
    options: &Options,
) -> Result<Compiled, Box<Failure>> {
    let mut program = Source::new(options);
    program.parse_program(parsed)?;

    let mut output = Compiled::default();
    output.warnings.append(&mut program.warnings);
    write_prologue(&program, options, &mut output);

    let unique = program
        .unique_prefix
        .as_deref()
        .or(options.unique.as_deref())
        .unwrap_or(filename)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if program.unique_labels {
        make_labels_unique(&mut program, &unique);
    }

    for region in &program.regions {
        if let Err(error) = compile_region(&program, region, &unique, options, &mut output) {
            let lines = region.instructions.iter().map(|located| located.line);
            return Err(Box::new(Failure {
                error,
                lines: lines.clone().min().zip(lines.max()).map(|(a, b)| a..=b),
                work_areas: program.work_areas.clone(),
            }));
        }
    }
    write_epilogue(&program, options, &mut output);
    Ok(output)
}

/// Writes the listing header: banner, `.vu`/`.align`, `.global` exports,
/// the MPG header and the `.name` start label.
fn write_prologue(program: &Source, options: &Options, output: &mut Compiled) {
    if options.mode == crate::Mode::Vcl {
        // vcl's emitter banner, with oxide-vcl's name and version where vcl
        // names itself.
        output.assembly.push_str(&format!(
            "; =================================================\n\
             ; flowMon::Emit() oxide-vcl {} produced this code:\n",
            crate::VERSION
        ));
    } else {
        output.assembly.push_str(&format!(
            "; oxide-vcl {} produced this code\n",
            crate::VERSION
        ));
    }
    // Prologue: old syntax or an explicit `.vu`, unless MPG output.
    if (program.syntax == Syntax::Old || program.vu_directive) && program.mpg.is_none() {
        output.assembly.push_str("\t\t.vu\n");
    }
    if program.align {
        output.assembly.push_str("\t\t.align 4\n");
    }
    // `.name`'s labels come first, whatever the source order (observed from
    // vcl 1.4beta7, `audit-header-visibility`).
    if let Some(name) = &program.name {
        output.assembly.push_str(&format!(
            "\t\t.global\t{name}_CodeStart\n\t\t.global\t{name}_CodeEnd\n"
        ));
    }
    for global in &program.globals {
        output
            .assembly
            .push_str(&format!("\t\t.global\t{global}\n"));
    }
    if let Some(address) = &program.mpg {
        output.assembly.push_str(&format!(
            "\t\tDMAret\t*\n\t\tvifnop\n\t\tvifnop\n\t\tvifnop\n\t\tMPG\t{address},*\n"
        ));
    }
    if let Some(name) = &program.name {
        output.assembly.push_str(&format!("{name}_CodeStart:\n"));
    }
}

/// Prefixes every label and branch target that is not a `.global` export
/// with the unique name.
fn make_labels_unique(program: &mut Source, unique: &str) {
    let rename = |name: &str| {
        if program
            .globals
            .iter()
            .any(|global| global.eq_ignore_ascii_case(name))
        {
            name.to_string()
        } else {
            format!("{unique}_{name}")
        }
    };
    for region in &mut program.regions {
        for block in &mut region.blocks {
            for label in &mut block.labels {
                *label = rename(label);
            }
            if let End::Branch { target, tags, .. } = &mut block.end {
                if let Some(target) = target {
                    *target = rename(target);
                }
                for tag in tags {
                    *tag = rename(tag);
                }
            }
        }
        for located in &mut region.instructions {
            if located.instruction.metadata.is_branch() {
                for operand in &mut located.instruction.operands {
                    if let Some(value) = &mut operand.immediate {
                        *value = rename(value);
                        operand.text = value.clone();
                    }
                    for tag in &mut operand.tags {
                        *tag = rename(tag);
                    }
                }
            }
        }
    }
}

/// Writes the listing trailer: closing alignment and labels, MPG end
/// markers and the instruction and register statistics.
fn write_epilogue(program: &Source, options: &Options, output: &mut Compiled) {
    if program.align {
        output.assembly.push_str("\t\t.align 4\n");
    }
    if let Some(name) = &program.name {
        output.assembly.push_str(&format!("{name}_CodeEnd:\n"));
    }
    if program.mpg.is_some() {
        output.assembly.push_str("\t.EndMPG\n\t.EndDmaData\n");
    }
    let (integer, float) = register_counts(&output.assembly);
    if options.mode == crate::Mode::Vcl {
        output
            .assembly
            .push_str(&format!(";\tiCount={}\n", output.instruction_pairs));
        output.assembly.push_str(&format!(
            "; register stats:\n;{integer:4} VU User integer\n;{float:4} VU User floating point\n"
        ));
    } else {
        output.assembly.push_str(&format!(
            "; instruction pairs: {}\n",
            output.instruction_pairs
        ));
        output.assembly.push_str(&format!(
            "; registers used: {integer} VU integer; {float} VU floating point\n"
        ));
    }
}

/// vcl's register statistics: the distinct integer and floating-point
/// registers the listing names, VI00 and VF00 included (observed from vcl
/// 1.4beta7 on every probe and public corpus program).
fn register_counts(assembly: &str) -> (usize, usize) {
    let mut integer = BTreeSet::new();
    let mut float = BTreeSet::new();
    for line in assembly.lines() {
        let code = line.split(';').next().unwrap_or_default().as_bytes();
        for index in 0..code.len().saturating_sub(3) {
            let previous = index.checked_sub(1).map(|i| code[i]);
            if previous.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
                continue;
            }
            let digits = &code[index + 2..index + 4];
            if !digits.iter().all(u8::is_ascii_digit)
                || code.get(index + 4).is_some_and(u8::is_ascii_digit)
            {
                continue;
            }
            match &code[index..index + 2] {
                b"VI" => integer.insert(digits.to_vec()),
                b"VF" => float.insert(digits.to_vec()),
                _ => false,
            };
        }
    }
    (integer.len(), float.len())
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod visibility_tests {
    use super::*;

    fn scopes(open: &[(bool, &[&str])]) -> Source {
        let mut source = Source::new(&Options::default());
        for &(show, names) in open {
            source.scopes_opened += 1;
            source.visibility.push(Visibility {
                show,
                names: names.iter().map(|name| name.to_string()).collect(),
                number: source.scopes_opened,
            });
        }
        source
    }

    fn name(source: &Source, name: &str) -> String {
        source
            .qualify(name, Family::Vector)
            .unwrap_or_else(|| name.to_string())
    }

    #[test]
    fn hide_makes_only_its_names_local() {
        let source = scopes(&[(false, &["t"])]);
        assert_eq!(name(&source, "t"), "1t");
        assert_eq!(name(&source, "a"), "a");
    }

    #[test]
    fn show_makes_every_other_name_local() {
        let source = scopes(&[(true, &["a", "b"])]);
        assert_eq!(name(&source, "a"), "a");
        assert_eq!(name(&source, "t"), "1t");
    }

    #[test]
    fn the_innermost_scope_that_localizes_a_name_numbers_it() {
        // `.show a, b` then `.hide u`: `u` is the inner scope's, `t` the
        // outer scope's, and `a` stays visible through both.
        let source = scopes(&[(true, &["a", "b"]), (false, &["u"])]);
        assert_eq!(name(&source, "u"), "2u");
        assert_eq!(name(&source, "t"), "1t");
        assert_eq!(name(&source, "a"), "a");
        // `.show a, b, t` then `.show t, b`: `t` passes both lists.
        let source = scopes(&[(true, &["a", "b", "t"]), (true, &["t", "b"])]);
        assert_eq!(name(&source, "t"), "t");
        assert_eq!(name(&source, "u"), "2u");
        assert_eq!(name(&source, "a"), "2a");
    }

    #[test]
    fn physical_hardware_and_underscore_names_are_never_renamed() {
        let source = scopes(&[(false, &["vf01", "_a", "acc"])]);
        assert_eq!(source.qualify("vf01", Family::Vector), None);
        assert_eq!(source.qualify("_a", Family::Vector), None);
        assert_eq!(source.qualify("acc", Family::Hardware), None);
    }

    #[test]
    fn scope_lists_match_names_case_sensitively() {
        let source = scopes(&[(false, &["T"])]);
        assert_eq!(name(&source, "t"), "t");
        assert_eq!(name(&source, "T"), "1T");
    }
}

#[cfg(test)]
mod listing_comment_tests {
    use super::*;

    #[test]
    fn register_stats_count_distinct_registers_with_fields() {
        let listing = "__v_A_4:\n\
                       ; _LNOPT_w=[ normal2 ] 2 [2 0] 2   [__v_A_4]\n         \
                       div           Q,VF00w,VF04w       NOP\n         \
                       mul.xyz       VF04,VF04,Q         iaddiu        VI01,VI00,0x00000012\n\
                       ;\tiCount=2\n";
        // VF00 and VF04; VI00 and VI01. Comment lines and immediates do not count.
        assert_eq!(register_counts(listing), (2, 2));
    }

    #[test]
    fn table_comments_follow_vcls_layout() {
        let linear = TableNote {
            strategy: Some(schedule::Strategy::Normal2),
            lower_bound: Some(4),
            ..TableNote::default()
        };
        assert_eq!(
            linear.comment(3, 3, "__v_ADDRESS_4"),
            "; _LNOPT_w=[ normal2 ] 3 [4 0] 3   [__v_ADDRESS_4]\n"
        );
        assert_eq!(
            TableNote::default().comment(5, 5, "EXPL_FIX_loop__ENTRY_POINT"),
            "; _LNOPT_w=[ ] 5 [5 0] 5   [EXPL_FIX_loop__ENTRY_POINT]\n"
        );
        let kernel = TableNote {
            kernel_strategy: Some(schedule::Strategy::LinearDumb),
            ..TableNote::default()
        };
        assert_eq!(
            kernel.comment(5, 5, "EXPL_FIX_loop__MAIN_LOOP"),
            "; _LPOPT_w=[ ldumb ] 5 [5 5] 5   [EXPL_FIX_loop__MAIN_LOOP]\n"
        );
    }
}
