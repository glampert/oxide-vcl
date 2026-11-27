//! VUSL's messages and their wording. They reach `vcl`'s reporter as
//! [`vcl::Message::Frontend`], through `From`; nothing here prints.

use std::fmt;

use vcl::Message;
use vcl::diagnostics::FrontendMessage;

use crate::sema::layout::LayoutErrorKind;
use crate::syntax::lexer::LexErrorKind;
use crate::syntax::parser::SyntaxErrorKind;

/// Every message the VUSL compiler and its driver give.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VuslMessage {
    // The command line.
    /// An option the driver does not know.
    UnknownOption {
        option: String,
    },
    /// An option given without the value it takes.
    MissingValue {
        option: String,
    },
    /// `--color=` with something other than `auto`, `always` or `never`.
    BadColor {
        value: String,
    },
    /// `--mode=` with something other than `oxide` or `vcl`.
    BadMode {
        value: String,
    },
    /// `--opt=` or `--no-opt=` naming something that is not an oxide-vcl
    /// optimization; `name` is empty for an empty list.
    BadOptimization {
        argument: String,
        name: String,
    },
    /// `--vcl-option=` with something oxide-vusl does not pass on.
    BadVclOption {
        argument: String,
        reason: &'static str,
    },
    /// `--vcl-option=` options oxide-vcl's parser rejects, with its reason.
    VclOptionRejected {
        argument: String,
        reason: String,
    },
    /// `--annotate` with no VCL written.
    AnnotateWithoutVcl,
    /// `--opt=` or `--no-opt=` with `--mode=vcl`, which never optimizes.
    OptimizationsNeedOxideMode {
        argument: String,
    },
    /// `-D` with something other than `NAME` or `NAME=VALUE`, or naming a
    /// built-in macro.
    BadDefine {
        argument: String,
    },
    /// No input file.
    NoInput,
    /// A second input file.
    ExtraInput {
        argument: String,
    },

    // Reading the program.
    Lex(LexErrorKind),
    Syntax(SyntaxErrorKind),

    // Declarations.
    /// A type name that is not a type.
    UnknownType {
        name: String,
    },
    /// A name declared twice in one scope.
    Redeclared {
        name: String,
    },
    /// Where a redeclared name was first declared (a note).
    FirstDeclaredHere {
        name: String,
    },
    /// A layout qualifier that does not apply where it is written.
    UnknownLayout {
        name: String,
        context: &'static str,
    },
    /// A layout qualifier with a value it does not take.
    BadLayoutValue {
        name: String,
        expected: &'static str,
    },
    /// A member that VU packing cannot place.
    Layout {
        member: String,
        kind: LayoutErrorKind,
    },
    /// An array length that is neither a positive constant nor an earlier
    /// `int16` member.
    BadArrayLength {
        name: String,
    },
    /// An array where none can be: a local, a parameter.
    ArrayNotAllowed {
        name: String,
    },
    /// A program without `void main(...)`.
    NoMain,
    /// `main` declared other than `void main(parameters)`.
    MainReturns,
    /// A parameter of `main` that is neither a struct in memory nor an
    /// `int16` count.
    MainParamType {
        name: String,
        ty: String,
    },
    /// A parameter of `main` with no placement.
    NoPlacement {
        name: String,
    },
    /// Two placements for one parameter.
    TwoPlacements {
        name: String,
    },
    /// A placement that does not suit the parameter.
    BadPlacement {
        name: String,
        reason: &'static str,
    },
    /// A parameter that does not fit in data memory at its address.
    OutsideMemory {
        name: String,
        address: u32,
        qwords: u32,
    },
    /// Two parameters' memory overlapping.
    Overlap {
        first: String,
        second: String,
    },
    /// A struct larger than the double-buffer half it is in.
    DoesNotFit {
        name: String,
        qwords: u32,
        half: u32,
    },
    /// A `const` whose value is not known at compile time.
    NotConstant {
        name: String,
    },
    /// A constant out of its type's range.
    ConstantRange {
        value: String,
        ty: &'static str,
    },
    /// A pragma the compiler does not know (a warning).
    UnknownPragma {
        name: String,
    },
    /// A known pragma with arguments it does not take.
    BadPragma {
        name: String,
        expected: &'static str,
    },
    /// A pragma where it does not apply.
    MisplacedPragma {
        name: String,
        place: &'static str,
    },
    /// A function no call reaches, so it is not checked (a warning).
    UnusedFunction {
        name: String,
    },
    /// `export` or file-scope memory in a file without `#pragma library`.
    OutsideLibrary {
        what: &'static str,
    },
    /// An export's definition whose signature is not its prototype's.
    ExportMismatch {
        name: String,
    },
    /// An export parameter or result that cannot travel in a register.
    ExportParameter {
        name: String,
        reason: &'static str,
    },
    /// An export's values that do not fit the library's register bank.
    ExportRegisters {
        name: String,
        kind: &'static str,
        first: u8,
        last: u8,
    },
    /// An `out` or `inout` argument of a library call that is not a whole
    /// variable.
    CallArgument {
        name: String,
    },
    /// A prototype that the library's own build never defines.
    ExportUndefined {
        name: String,
    },
    /// A library's `save` memory that is not an `inout` struct at a fixed
    /// address with room for the saved registers.
    SaveMemory {
        name: String,
        qwords: u32,
    },

    // Expressions.
    UnknownName {
        name: String,
    },
    UnknownFunction {
        name: String,
    },
    UnknownMember {
        ty: String,
        member: String,
    },
    /// A swizzle with letters other than x, y, z and w, or past the value's
    /// components.
    BadSwizzle {
        swizzle: String,
        ty: String,
    },
    /// A swizzle read that names a component twice.
    RepeatedComponent {
        swizzle: String,
    },
    WrongArgumentCount {
        name: String,
        expected: String,
        found: usize,
    },
    /// An operand or value of the wrong type.
    TypeMismatch {
        context: String,
        expected: String,
        found: String,
    },
    /// Vector operands placed in different lanes.
    LaneMismatch {
        context: String,
        left: String,
        right: String,
    },
    /// An operand that an instruction takes in particular lanes.
    NeedsLanes {
        what: String,
        lanes: &'static str,
        found: String,
    },
    /// An operation the VU has no instruction for.
    NoInstruction {
        what: String,
        help: &'static str,
    },
    /// Raw bits that would have to move between lanes through an FMAC.
    RawLaneMove {
        from: String,
        to: String,
    },
    /// Something that cannot be assigned to.
    NotAssignable {
        what: &'static str,
    },
    /// A write to read-only memory.
    ReadOnly {
        name: String,
    },
    /// A read of write-only memory.
    WriteOnly {
        name: String,
    },
    /// A variable or component read before anything sets it.
    UseBeforeSet {
        name: String,
    },
    /// A constant array index past the end.
    IndexOutOfRange {
        index: i64,
        length: u32,
    },
    /// An assignment or increment inside a larger expression.
    NestedAssignment,
    /// A built-in with no value (`xgkick`, `rinit`, `rxor`) inside an
    /// expression.
    NoValue {
        name: String,
    },
    /// A condition that is not `bool`.
    NotBool {
        found: String,
    },
    /// A local with different placements on two paths into one point.
    PlacementMerge {
        name: String,
        first: String,
        second: String,
    },
    /// A function that calls itself.
    Recursion {
        name: String,
    },
    /// Where an inlined function was called (a note).
    CalledHere {
        name: String,
    },

    // Statements.
    /// `return` that is not a function's last statement.
    ReturnNotLast,
    /// A non-void function without a final `return value;`.
    MissingReturn {
        name: String,
    },
    /// `return value;` in a void function, or `return;` in another.
    ReturnMismatch {
        name: String,
    },
    BreakOutsideLoop,
    ContinueOutsideLoop,
    YieldOutsideMain,
    /// `yield` in a range-for, whose elements are in the batch it stops on.
    YieldInRange,
    /// A ref set from memory other than the memory it was first set from.
    RefWithin {
        name: String,
    },
    /// Memory read through a ref that is not a variable.
    RefNotVariable,
    /// A `layout(spill)` parameter read or written.
    SpillUsed {
        name: String,
    },
    /// `break` or `continue` in a loop `#pragma unroll` repeats.
    UnrolledLoopLeaves,
    /// A range that is not an array in memory.
    RangeNotArray {
        found: String,
    },
    /// A switch on something other than an `int16` or an enum.
    SwitchType {
        found: String,
    },
    /// A case label that is not a constant.
    CaseNotConstant,
    /// Two case labels with one value.
    DuplicateCase {
        value: String,
    },
    /// Where a duplicate case label's value was first (a note).
    CaseFirst {
        value: String,
    },
    /// A case with statements that does not end in `break`, so it would
    /// run into the next case.
    CaseFallthrough,
    /// A `break` of a switch that is not a case's last statement.
    BreakInCase,
    /// A type alias named like a built-in type.
    BuiltinTypeName {
        name: String,
    },
    /// An enum declared with an underlying type other than `int16`.
    EnumUnderlying {
        name: String,
        found: String,
    },
    /// `Enum::name` naming no value of the enum.
    UnknownEnumerator {
        ty: String,
        name: String,
    },
    /// `Name::value` where `Name` is not an enum.
    NotAnEnum {
        name: String,
    },
    /// An enum where another type is wanted, or the other way round:
    /// enums convert only with a constructor.
    EnumConversion {
        context: String,
        expected: String,
        found: String,
    },
    /// Arithmetic or logic on an enum's value, which only compares.
    EnumOperation {
        op: String,
        ty: String,
    },
    /// A type with no constructor: `qword(...)`, `bool(...)`.
    NoConstructor {
        ty: String,
        help: &'static str,
    },
    /// A function with statements called on the right of `&&` or `||`,
    /// whose statements a false or true left side would have to skip.
    CallInShortCircuit {
        name: String,
    },
    /// A whole-memory copy of an array whose length is read at run time.
    CopyLengthAtRunTime {
        name: String,
    },
    /// A constant of a type constants cannot have.
    ConstantType {
        name: String,
        ty: String,
    },
    /// A struct variable of a struct with an array member.
    StructArrayMember {
        ty: String,
        member: String,
    },
    /// A first range whose length is open.
    RangeOpen,
    /// Ranges of different lengths.
    RangeLengths,
    /// A range element declared with a type other than its array's.
    RangeElementType {
        expected: String,
        found: String,
    },
    /// A clip judgement read where the clip flags no longer hold it.
    ClipOutOfReach {
        reason: &'static str,
    },
    /// `clipbits()` of a judgement a newer one follows.
    ClipBitsNotNewest,
    /// A feature the compiler does not have yet.
    NotYet {
        what: &'static str,
    },
}

impl VuslMessage {
    /// Whether the message is about the program's source, rather than the
    /// command line or the driver.
    pub fn in_source(&self) -> bool {
        use VuslMessage::*;
        !matches!(
            self,
            UnknownOption { .. }
                | MissingValue { .. }
                | BadColor { .. }
                | BadMode { .. }
                | BadOptimization { .. }
                | OptimizationsNeedOxideMode { .. }
                | BadVclOption { .. }
                | VclOptionRejected { .. }
                | AnnotateWithoutVcl
                | BadDefine { .. }
                | NoInput
                | ExtraInput { .. }
        )
    }

    /// The message's text, the token it is about and a suggested fix.
    pub fn wording(&self) -> FrontendMessage {
        let (text, subject, help) = wording(self);
        FrontendMessage {
            text,
            subject,
            help: help.map(str::to_string).or_else(|| dynamic_help(self)),
            in_source: self.in_source(),
        }
    }
}

/// A help line that depends on the message's fields.
fn dynamic_help(message: &VuslMessage) -> Option<String> {
    use VuslMessage::*;
    match message {
        UnknownType { name } => match name.as_str() {
            "int" => Some("VU integers are 16 bits: write int16".into()),
            "uint" => Some("a raw 32-bit lane is uint32; a VI integer is int16".into()),
            "double" | "half" => Some("VU floats are 32 bits: write float".into()),
            _ => None,
        },
        BadPragma { name, expected } => Some(format!("write #pragma {name}{expected}")),
        EnumConversion { expected, .. } => Some(format!(
            "convert it explicitly with a constructor: {expected}(value)"
        )),
        CallInShortCircuit { name } => Some(format!(
            "call it before the condition and test its result: float result = {name}(...);"
        )),
        BadOptimization { argument, .. } => {
            let option = argument
                .split_once('=')
                .map_or(argument.as_str(), |(option, _)| option);
            let names: Vec<_> = vcl::Optimization::ALL
                .iter()
                .map(|optimization| optimization.name())
                .collect();
            Some(format!(
                "use {option}=all, {option}=none, or a comma-separated list of: {}",
                names.join(", ")
            ))
        }
        LaneMismatch { .. } => Some(
            "assign one of them to a variable declared with layout(lane = ...), which moves \
             its lanes"
                .into(),
        ),
        _ => None,
    }
}

type Wording = (String, Option<String>, Option<&'static str>);

fn wording(message: &VuslMessage) -> Wording {
    use VuslMessage::*;
    let plain = |text: String| (text, None, None);
    let about = |text: String, subject: &str| (text, Some(subject.to_string()), None);
    match message {
        UnknownOption { option } => (
            format!("unknown option '{option}'"),
            None,
            Some("run 'oxide-vusl --help' for the options"),
        ),
        MissingValue { option } => plain(format!("option '{option}' needs a value")),
        BadColor { value } => (
            format!("unknown --color value '{value}'"),
            None,
            Some("use --color=auto, --color=always or --color=never"),
        ),
        BadMode { value } => (
            format!("unknown --mode value '{value}'"),
            None,
            Some("use --mode=oxide (the default) or --mode=vcl"),
        ),
        BadOptimization { argument, name } => {
            let option = argument
                .split_once('=')
                .map_or(argument.as_str(), |(option, _)| option);
            if name.is_empty() {
                plain(format!("{option}= names no optimization"))
            } else {
                plain(format!("unknown optimization '{name}'"))
            }
        }
        BadVclOption { argument, reason } => (
            format!("--vcl-option={argument} is not passed to oxide-vcl: {reason}"),
            None,
            Some(
                "pass one of oxide-vcl's compile options, such as -c, -L or --fix=, per --vcl-option=",
            ),
        ),
        VclOptionRejected { argument, reason } => (
            format!("oxide-vcl rejects --vcl-option={argument}: {reason}"),
            None,
            Some("run 'oxide-vcl --help' for its options"),
        ),
        AnnotateWithoutVcl => (
            "--annotate comments the generated VCL, which this run does not write".into(),
            None,
            Some("add --emit-vcl=<file> or --stop-at-vcl"),
        ),
        OptimizationsNeedOxideMode { argument } => (
            format!("{argument} applies only in oxide mode"),
            None,
            Some("drop --mode=vcl, which compiles the generated VCL without optimizations"),
        ),
        BadDefine { argument } => (
            format!("can't define '{argument}'"),
            None,
            Some("write -D NAME or -D NAME=VALUE, with a name that is not a built-in macro"),
        ),
        NoInput => (
            "no input file".into(),
            None,
            Some("name the program to compile: oxide-vusl prog.vusl"),
        ),
        ExtraInput { argument } => (
            format!("more than one input file: '{argument}'"),
            None,
            Some("compile one program per run"),
        ),
        Lex(kind) => lex(kind),
        Syntax(kind) => syntax(kind),
        UnknownType { name } => about(format!("unknown type '{name}'"), name),
        Redeclared { name } => about(format!("'{name}' is declared twice"), name),
        FirstDeclaredHere { name } => about(format!("'{name}' is first declared here"), name),
        UnknownLayout { name, context } => (
            format!("'{name}' is not a layout qualifier of {context}"),
            Some(name.clone()),
            None,
        ),
        BadLayoutValue { name, expected } => (
            format!("layout qualifier '{name}' takes {expected}"),
            Some(name.clone()),
            None,
        ),
        Layout { member, kind } => layout(member, kind),
        BadArrayLength { name } => (
            format!(
                "the length of '{name}' is neither a positive constant, an earlier int16 member nor a layout(xitop) parameter of main"
            ),
            Some(name.clone()),
            Some(
                "write a constant, the name of an int16 member before it or of main's layout(xitop) parameter, or [] for an open array at the end",
            ),
        ),
        ArrayNotAllowed { name } => (
            format!("'{name}' cannot be an array: VU registers are not indexed"),
            Some(name.clone()),
            Some("keep arrays in memory, in a struct that main takes as a parameter"),
        ),
        NoMain => (
            "the program has no main function".into(),
            None,
            Some("write void main(...), with the memory it uses as its parameters"),
        ),
        MainReturns => (
            "main returns nothing: declare it void main(...)".into(),
            Some("main".into()),
            None,
        ),
        MainParamType { name, ty } => (
            format!("main's parameter '{name}' is a {ty}, not a struct in memory"),
            Some(name.clone()),
            Some("main takes structs bound in memory, and an int16 with layout(xitop)"),
        ),
        NoPlacement { name } => (
            format!("main's parameter '{name}' has no placement"),
            Some(name.clone()),
            Some("give it layout(address = N), or layout(xtop) for the current double buffer"),
        ),
        TwoPlacements { name } => about(format!("'{name}' is given two placements"), name),
        BadPlacement { name, reason } => about(format!("the placement of '{name}' {reason}"), name),
        OutsideMemory {
            name,
            address,
            qwords,
        } => (
            format!(
                "'{name}' at address {address} needs {qwords} qwords, past the end of data memory (1024)"
            ),
            Some(name.clone()),
            None,
        ),
        Overlap { first, second } => (
            format!("'{first}' and '{second}' overlap in data memory"),
            Some(second.clone()),
            Some("move one, or declare the overlap with layout(alias = ...)"),
        ),
        DoesNotFit { name, qwords, half } => (
            format!(
                "'{name}' needs at least {qwords} qwords, more than the {half}-qword double-buffer half"
            ),
            Some(name.clone()),
            None,
        ),
        NotConstant { name } => (
            format!("the value of '{name}' is not a compile-time constant"),
            Some(name.clone()),
            None,
        ),
        ConstantRange { value, ty } => about(format!("{value} is out of {ty}'s range"), value),
        UnknownPragma { name } => about(format!("ignoring unknown #pragma {name}"), name),
        BadPragma { name, .. } => about(format!("#pragma {name} has bad arguments"), name),
        MisplacedPragma { name, place } => about(format!("#pragma {name} {place}"), name),
        UnusedFunction { name } => (
            format!("function '{name}' is never called, so it is not checked"),
            Some(name.clone()),
            None,
        ),
        OutsideLibrary { what } => (
            format!("{what} needs #pragma library: only a library has it"),
            None,
            Some("name the library first: #pragma library(name, address = A)"),
        ),
        ExportMismatch { name } => (
            format!("'{name}' is defined with another signature than its prototype"),
            Some(name.clone()),
            Some("an export's definition repeats its prototype's types, qualifiers and names"),
        ),
        ExportParameter { name, reason } => (
            format!("'{name}' cannot be passed to a library: {reason}"),
            Some(name.clone()),
            Some("pass int16, ref<T>, float, vectors and raw integer types; memory goes by ref"),
        ),
        ExportRegisters {
            name,
            kind,
            first,
            last,
        } => (
            format!(
                "'{name}' passes more {kind} values than the library's bank holds ({kind}{first:02}-{kind}{last:02})"
            ),
            Some(name.clone()),
            Some(
                "pass fewer values, or start the bank lower with #pragma library(..., vf = F, vi = I)",
            ),
        ),
        CallArgument { name } => (
            format!("the out or inout argument for '{name}' must be a variable"),
            Some(name.clone()),
            Some(
                "a library returns values in registers, into whole variables: assign a member or component from one after the call",
            ),
        ),
        SaveMemory { name, qwords } => (
            format!(
                "the library saves its registers in '{name}', which needs to be inout memory at an address, of {qwords} qwords at least"
            ),
            Some(name.clone()),
            Some(
                "declare it in the header: layout(address = A) inout Saves saves; four VI registers take a qword, a VF register one",
            ),
        ),
        ExportUndefined { name } => (
            format!("export '{name}' has no definition in the library"),
            Some(name.clone()),
            Some("define it after its prototype, with the same signature"),
        ),
        UnknownName { name } => about(format!("unknown name '{name}'"), name),
        UnknownFunction { name } => about(format!("unknown function '{name}'"), name),
        UnknownMember { ty, member } => about(format!("{ty} has no member '{member}'"), member),
        BadSwizzle { swizzle, ty } => (
            format!("'{swizzle}' does not select components of a {ty}"),
            Some(swizzle.clone()),
            Some("a swizzle names components with x, y, z and w"),
        ),
        RepeatedComponent { swizzle } => (
            format!("'{swizzle}' reads a component twice, which no VU operand does"),
            Some(swizzle.clone()),
            Some("use the scalar component: an operation broadcasts it"),
        ),
        WrongArgumentCount {
            name,
            expected,
            found,
        } => about(
            format!("'{name}' takes {expected}, but is given {found}"),
            name,
        ),
        TypeMismatch {
            context,
            expected,
            found,
        } => plain(format!("{context} needs {expected}, not {found}")),
        LaneMismatch {
            context,
            left,
            right,
        } => plain(format!(
            "{context} combines lanes {left} with lanes {right}: no instruction moves them"
        )),
        NeedsLanes { what, lanes, found } => (
            format!("{what} takes its operands in lanes {lanes}, not {found}"),
            None,
            Some(
                "assign the value to a variable declared with layout(lane = x), which moves its lanes",
            ),
        ),
        NoInstruction { what, help } => (
            format!("{what}: the VU has no instruction for it"),
            None,
            Some(help),
        ),
        RawLaneMove { from, to } => (
            format!("raw bits in lanes {from} cannot move to lanes {to}"),
            None,
            Some("raw bits move only by MR32 rotation; an FMAC move would flush them as denormals"),
        ),
        NotAssignable { what } => plain(format!("{what} cannot be assigned to")),
        ReadOnly { name } => about(format!("'{name}' is read-only"), name),
        WriteOnly { name } => about(format!("'{name}' is write-only (out)"), name),
        UseBeforeSet { name } => about(format!("'{name}' is read before it is set"), name),
        IndexOutOfRange { index, length } => plain(format!(
            "index {index} is past the end of an array of {length}"
        )),
        NestedAssignment => (
            "an assignment or increment inside an expression".into(),
            None,
            Some("write it as a statement of its own"),
        ),
        NoValue { name } => (
            format!("'{name}' has no value"),
            Some(name.clone()),
            Some("call it as a statement of its own"),
        ),
        NotBool { found } => plain(format!("a condition needs a bool, not {found}")),
        PlacementMerge {
            name,
            first,
            second,
        } => (
            format!("'{name}' is in lanes {first} on one path here and {second} on another"),
            Some(name.clone()),
            Some("declare it with layout(lane = ...): assignments then move into those lanes"),
        ),
        Recursion { name } => about(
            format!("'{name}' calls itself: functions are inlined, so it cannot recurse"),
            name,
        ),
        CalledHere { name } => about(format!("in the call to '{name}' here"), name),
        ReturnNotLast => (
            "return before a function's end".into(),
            Some("return".into()),
            Some("functions are inlined: a return must be the last statement"),
        ),
        MissingReturn { name } => (
            format!("'{name}' does not end with return"),
            Some(name.clone()),
            None,
        ),
        ReturnMismatch { name } => about(
            format!("the return does not match the type '{name}' returns"),
            "return",
        ),
        BreakOutsideLoop => about("break outside a loop".into(), "break"),
        ContinueOutsideLoop => about("continue outside a loop".into(), "continue"),
        YieldOutsideMain => ("yield outside main".into(), Some("yield".into()), None),
        RefWithin { name } => (
            format!("'{name}' would point outside the memory it was first set from"),
            Some(name.clone()),
            Some(
                "a ref stays within one parameter member or array: use another ref for other memory",
            ),
        ),
        NoConstructor { ty, help } => (format!("{ty} has no constructor"), None, Some(help)),
        SwitchType { found } => (
            format!("a switch needs an int16 or an enum, not {found}"),
            None,
            Some("compare other values with if and else"),
        ),
        CaseNotConstant => (
            "a case's value must be a constant".into(),
            None,
            Some("write an integer constant or one of the enum's values, or compare with if"),
        ),
        DuplicateCase { value } => plain(format!("two cases are {value}")),
        CaseFirst { value } => plain(format!("the first case {value} is here")),
        CaseFallthrough => (
            "this case's statements would run into the next case's".into(),
            None,
            Some(
                "end them with break; labels with no statements between them (case 1: case 2:) share statements",
            ),
        ),
        BreakInCase => (
            "a switch's break must be its case's last statement".into(),
            None,
            Some("choose with if and else inside the case: a break ends only the whole case"),
        ),
        BuiltinTypeName { name } => (
            format!("'{name}' is a built-in type and cannot be declared again"),
            Some(name.clone()),
            None,
        ),
        EnumUnderlying { name, found } => (
            format!("enum '{name}' holds {found}: an enum's values are int16"),
            None,
            Some("drop the type after ':', or write : int16"),
        ),
        UnknownEnumerator { ty, name } => about(format!("{ty} has no value '{name}'"), name),
        NotAnEnum { name } => (
            format!("'{name}' is not an enum: only an enum's values are named with ::"),
            Some(name.clone()),
            None,
        ),
        EnumConversion {
            context,
            expected,
            found,
        } => (
            format!("{context} needs {expected}, not {found}: enums convert only explicitly"),
            None,
            None,
        ),
        EnumOperation { op, ty } => (
            format!("'{op}' on {ty}: an enum's values only compare with each other"),
            None,
            Some("convert it with int16(...) for arithmetic, and back with its constructor"),
        ),
        CallInShortCircuit { name } => (
            format!(
                "'{name}' runs statements, which the right side of && or || cannot: they would \
                 run whatever the left side is"
            ),
            Some(name.clone()),
            None,
        ),
        CopyLengthAtRunTime { name } => (
            format!("copying '{name}' would need a loop: its length is read at run time"),
            None,
            Some(
                "copy its elements in a range-for: for (T item : from, out T copy : to) copy = item;",
            ),
        ),
        ConstantType { name, ty } => (
            format!("constant '{name}' is a {ty}: constants are int16, float, bool or vectors"),
            Some(name.clone()),
            Some(
                "build the value in a variable: a matrix from vector constants with mat4(...), \
                 a struct with its constructor",
            ),
        ),
        StructArrayMember { ty, member } => (
            format!(
                "a {ty} variable can't be kept in registers: its member '{member}' is an array"
            ),
            None,
            Some("keep it in memory, or copy the members you need into variables"),
        ),
        UnrolledLoopLeaves => (
            "an unrolled loop cannot break or continue".into(),
            None,
            Some("its body is repeated: drop the break or continue, or the #pragma unroll"),
        ),
        SpillUsed { name } => (
            format!(
                "'{name}' is layout(spill): oxide-vcl's work area, which the program cannot use"
            ),
            Some(name.clone()),
            Some("keep the program's own data in another parameter"),
        ),
        RefNotVariable => {
            plain("only a ref<T> variable can be read through: write r[1].x for (r + 1).x".into())
        }
        YieldInRange => (
            "yield in a range-for".into(),
            Some("yield".into()),
            Some("the elements are in the batch the program stops on; yield between range-fors"),
        ),
        RangeNotArray { found } => plain(format!("a range needs an array in memory, not {found}")),
        RangeOpen => (
            "the first range is an open array, whose length is unknown".into(),
            None,
            Some("put a range with a length first"),
        ),
        RangeLengths => (
            "ranges of different lengths".into(),
            None,
            Some("zip arrays of the same length, or open ones after the first"),
        ),
        RangeElementType { expected, found } => plain(format!(
            "the elements are {expected}, but the range declares {found}"
        )),
        ClipOutOfReach { reason } => (
            format!("the clipw judgement is no longer in the clip flags: {reason}"),
            None,
            Some(
                "read judgements with any(), adc(), clipped() or clipbits() within four of them, the newest in its clipw's block, with no branch between that judges",
            ),
        ),
        ClipBitsNotNewest => (
            "clipbits() reads only the newest judgement, and a newer clipw follows this one".into(),
            None,
            Some(
                "read a judgement's bits before the next clipw: FCGET reads the two newest, and VU1 has no shift to bring the older one's down",
            ),
        ),
        NotYet { what } => plain(format!("{what} is not implemented yet")),
    }
}

fn lex(kind: &LexErrorKind) -> Wording {
    match kind {
        LexErrorKind::UnknownCharacter(character) => (
            format!("unexpected character '{character}'"),
            Some(character.to_string()),
            None,
        ),
        LexErrorKind::BadNumber(text) => (
            format!("malformed number '{text}'"),
            Some(text.clone()),
            None,
        ),
        LexErrorKind::OctalNumber(text) => (
            format!("'{text}' starts with 0"),
            Some(text.clone()),
            Some("VUSL has no octal numbers: write it in decimal, or in hex with 0x"),
        ),
        LexErrorKind::NumberTooLarge(text) => {
            (format!("'{text}' is too large"), Some(text.clone()), None)
        }
        LexErrorKind::StrayHash => (
            "'#' outside a #pragma line".into(),
            Some("#".into()),
            Some("the preprocessor reads every other directive at the start of a line"),
        ),
    }
}

fn syntax(kind: &SyntaxErrorKind) -> Wording {
    match kind {
        SyntaxErrorKind::Expected { expected, found } => {
            let subject = found
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .map(str::to_string);
            (format!("expected {expected}, found {found}"), subject, None)
        }
        SyntaxErrorKind::KeywordAsName(word) => (
            format!("'{word}' is a keyword and cannot name anything"),
            Some(word.clone()),
            None,
        ),
        SyntaxErrorKind::GlobalVariable(name) => (
            format!("'{name}' is a variable at file scope"),
            Some(name.clone()),
            Some(
                "file scope holds types, constants and functions; memory reaches the program \
                 as main's parameters",
            ),
        ),
        SyntaxErrorKind::UnscopedEnum => (
            "an enum must be scoped: VUSL has no C enums".into(),
            None,
            Some("write enum class Name { ... }, and name its values as Name::Value"),
        ),
        SyntaxErrorKind::NotCallable => (
            "only a function, a built-in or a type can be called".into(),
            None,
            None,
        ),
    }
}

fn layout(member: &str, kind: &LayoutErrorKind) -> Wording {
    let subject = Some(member.to_string());
    match kind {
        LayoutErrorKind::LaneOverflow { lane, lanes } => (
            format!(
                "'{member}' needs {lanes} lanes, more than a qword has from lane {}",
                lane.letter()
            ),
            subject,
            None,
        ),
        LayoutErrorKind::LaneOnQwords => (
            format!("'{member}' takes whole qwords, so it cannot start at a lane"),
            subject,
            None,
        ),
        LayoutErrorKind::OffsetBackwards { offset, end } => (
            format!(
                "'{member}' is placed at qword {offset}, before the members ahead of it end (qword {end})"
            ),
            subject,
            None,
        ),
        LayoutErrorKind::AfterOpen => (
            format!("'{member}' follows an open array, which must be last"),
            subject,
            None,
        ),
        LayoutErrorKind::VariableElements => (
            format!("the elements of '{member}' have no fixed size"),
            subject,
            Some("an array's elements cannot hold arrays whose length is read at run time"),
        ),
        LayoutErrorKind::NotInMemory => (
            format!("'{member}' has a type that cannot be in memory"),
            subject,
            Some("bool, clipflags and ref values live in registers; store an int16 instead"),
        ),
        LayoutErrorKind::UnpackMismatch { components } => (
            format!("the unpack format of '{member}' does not fit its {components} lanes"),
            subject,
            Some(
                "the format's components must match the member, and 8- and 16-bit formats unpack integers",
            ),
        ),
    }
}

impl From<VuslMessage> for Message {
    fn from(message: VuslMessage) -> Self {
        Message::Frontend(message.wording())
    }
}

impl fmt::Display for VuslMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.wording().text)
    }
}
