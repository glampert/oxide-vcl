//! The file's declarations (structs and their layouts, constants,
//! functions, file pragmas) and `main`'s parameters bound in memory.

use std::collections::HashMap;

use super::consteval;
use super::hir::{Const, Param, ParamPlacement, Vif};
use super::layout::{MemberSpec, Packer, StructDef, Unpack};
use super::types::{Lane, Length, StructId, Type, builtin_type};
use super::{Checked, Problem, problem};
use crate::diagnostics::VuslMessage;
use crate::syntax::Pos;
use crate::syntax::ast::{self, ArrayLen, ExprKind, Item, Qualifier};

/// Data memory: 1024 qwords on VU1.
pub const DATA_MEMORY_QWORDS: u32 = 1024;

/// What the file declares.
pub struct Declarations<'a> {
    pub structs: Vec<StructDef>,
    pub struct_ids: HashMap<String, StructId>,
    /// File-scope constants: their value and type.
    pub consts: HashMap<String, (Const, Type)>,
    pub functions: HashMap<String, &'a ast::Function>,
    pub main: &'a ast::Function,
    pub program_name: Option<String>,
}

impl Declarations<'_> {
    pub fn struct_names(&self) -> Vec<String> {
        self.structs.iter().map(|def| def.name.clone()).collect()
    }

    pub fn type_name(&self, ty: &Type) -> String {
        ty.name(&self.struct_names())
    }
}

/// A file-scope name and where it was declared, for redeclarations.
struct Names(HashMap<String, Pos>);

impl Names {
    fn declare(&mut self, name: &ast::Ident) -> Checked<()> {
        if let Some(first) = self.0.get(&name.name) {
            return Err(Box::new(
                problem(
                    VuslMessage::Redeclared {
                        name: name.name.clone(),
                    },
                    name.pos,
                )
                .with_note(
                    *first,
                    VuslMessage::FirstDeclaredHere {
                        name: name.name.clone(),
                    },
                ),
            ));
        }
        self.0.insert(name.name.clone(), name.pos);
        Ok(())
    }
}

/// The type a name spells, given the structs declared so far.
pub fn resolve_type(ty: &ast::TypeName, struct_ids: &HashMap<String, StructId>) -> Checked<Type> {
    if let Some(argument) = &ty.argument {
        return Ok(Type::Ref(Box::new(resolve_type(argument, struct_ids)?)));
    }
    if let Some(builtin) = builtin_type(&ty.name.name) {
        return Ok(builtin);
    }
    match struct_ids.get(&ty.name.name) {
        Some(id) => Ok(Type::Struct(*id)),
        None => Err(Box::new(problem(
            VuslMessage::UnknownType {
                name: ty.name.name.clone(),
            },
            ty.pos,
        ))),
    }
}

/// The value of a constant expression, with the file's constants.
pub fn constant(expr: &ast::Expr, consts: &HashMap<String, (Const, Type)>) -> Option<Const> {
    match &expr.kind {
        ExprKind::Int(value) => Some(Const::Int(*value)),
        ExprKind::Float(value) => Some(Const::Float(*value)),
        ExprKind::Bool(value) => Some(Const::Bool(*value)),
        ExprKind::Name(name) => consts.get(&name.name).map(|(value, _)| *value),
        ExprKind::Unary { op, operand } => consteval::unary(*op, constant(operand, consts)?).ok(),
        ExprKind::Binary { op, lhs, rhs, .. } => {
            consteval::binary(*op, constant(lhs, consts)?, constant(rhs, consts)?).ok()
        }
        _ => None,
    }
}

/// An `int16` constant's value, checked against the type's range.
pub fn int16_range(value: i64) -> bool {
    (-32768..=65535).contains(&value)
}

/// The constant typed as `ty`, when it fits: an integer constant may be a
/// float.
pub fn typed_constant(value: Const, ty: &Type) -> Option<Const> {
    match (value, ty) {
        (Const::Int(n), Type::Int16) if int16_range(n) => Some(Const::Int(n)),
        (Const::Int(n), Type::Float) => Some(Const::Float(n as f32)),
        (Const::Float(f), Type::Float) => Some(Const::Float(f)),
        (Const::Bool(b), Type::Bool) => Some(Const::Bool(b)),
        _ => None,
    }
}

/// Reads the file's declarations.
pub fn declarations<'a>(
    file: &'a ast::File,
    warnings: &mut Vec<Problem>,
) -> Checked<Declarations<'a>> {
    let mut names = Names(HashMap::new());
    let mut structs: Vec<StructDef> = Vec::new();
    let mut struct_ids: HashMap<String, StructId> = HashMap::new();
    let mut consts: HashMap<String, (Const, Type)> = HashMap::new();
    let mut functions: HashMap<String, &'a ast::Function> = HashMap::new();
    let mut main = None;
    let mut program_name: Option<String> = None;
    let xitop = xitop_names(file);
    for item in &file.items {
        match item {
            Item::Pragma(pragma) => {
                program_pragma(pragma, &mut program_name, warnings)?;
            }
            Item::Struct(decl) => {
                names.declare(&decl.name)?;
                let def = struct_def(decl, &structs, &struct_ids, &consts, &xitop)?;
                struct_ids.insert(def.name.clone(), structs.len());
                structs.push(def);
            }
            Item::Const(decl) => {
                names.declare(&decl.name)?;
                let ty = resolve_type(&decl.ty, &struct_ids)?;
                let not_constant = || {
                    Box::new(problem(
                        VuslMessage::NotConstant {
                            name: decl.name.name.clone(),
                        },
                        decl.name.pos,
                    ))
                };
                if !matches!(ty, Type::Int16 | Type::Float | Type::Bool) {
                    return Err(Box::new(problem(
                        VuslMessage::NotYet {
                            what: "a constant of a vector, matrix or struct type",
                        },
                        decl.ty.pos,
                    )));
                }
                let value = constant(&decl.value, &consts).ok_or_else(not_constant)?;
                let Some(value) = typed_constant(value, &ty) else {
                    return Err(Box::new(problem(
                        VuslMessage::TypeMismatch {
                            context: format!("constant '{}'", decl.name.name),
                            expected: ty.name(&[]),
                            found: const_type_name(value).into(),
                        },
                        decl.value.pos,
                    )));
                };
                consts.insert(decl.name.name.clone(), (value, ty));
            }
            Item::Function(function) => {
                names.declare(&function.name)?;
                if function.name.name == "main" {
                    main = Some(function);
                } else {
                    functions.insert(function.name.name.clone(), function);
                }
            }
        }
    }
    let Some(main) = main else {
        return Err(Box::new(problem(VuslMessage::NoMain, Pos::default())));
    };
    if main.ret.name.name != "void" || main.ret.argument.is_some() {
        return Err(Box::new(problem(VuslMessage::MainReturns, main.ret.pos)));
    }
    Ok(Declarations {
        structs,
        struct_ids,
        consts,
        functions,
        main,
        program_name,
    })
}

/// The type a constant has, for messages.
pub fn const_type_name(value: Const) -> &'static str {
    match value {
        Const::Int(_) => "an integer",
        Const::Float(_) => "a float",
        Const::Bool(_) => "a bool",
    }
}

/// The names of the pragmas that apply to statements.
pub const STATEMENT_PRAGMAS: [&str; 5] =
    ["unroll", "nounroll", "pipeline", "nopipeline", "barrier"];

fn program_pragma(
    pragma: &ast::Pragma,
    program_name: &mut Option<String>,
    warnings: &mut Vec<Problem>,
) -> Checked<()> {
    let name = &pragma.name.name;
    if name == "program" {
        let argument = match pragma.args.as_deref() {
            Some([arg]) if arg.is_flag() => arg.key().map(str::to_string),
            _ => None,
        };
        let Some(argument) = argument else {
            return Err(Box::new(problem(
                VuslMessage::BadPragma {
                    name: name.clone(),
                    expected: "(name)",
                },
                pragma.pos,
            )));
        };
        if program_name.is_some() {
            return Err(Box::new(problem(
                VuslMessage::Redeclared {
                    name: "#pragma program".into(),
                },
                pragma.pos,
            )));
        }
        *program_name = Some(argument);
        return Ok(());
    }
    if STATEMENT_PRAGMAS.contains(&name.as_str()) {
        return Err(Box::new(problem(
            VuslMessage::MisplacedPragma {
                name: name.clone(),
                place: "applies to statements, not to the file",
            },
            pragma.pos,
        )));
    }
    warnings.push(problem(
        VuslMessage::UnknownPragma { name: name.clone() },
        pragma.pos,
    ));
    Ok(())
}

/// A layout argument's name, or an error at it.
fn layout_key(arg: &ast::Arg) -> Checked<&str> {
    arg.key().ok_or_else(|| {
        Box::new(problem(
            VuslMessage::UnknownLayout {
                name: crate::syntax::print::expr(&arg.value),
                context: "a member",
            },
            arg.pos,
        ))
    })
}

fn bad_value(name: &str, expected: &'static str, pos: Pos) -> Box<Problem> {
    Box::new(problem(
        VuslMessage::BadLayoutValue {
            name: name.into(),
            expected,
        },
        pos,
    ))
}

/// A layout argument's value as a name: `w` in `lane = w`.
fn name_value(arg: &ast::Arg) -> Option<&str> {
    match (&arg.name, &arg.value.kind) {
        (Some(_), ExprKind::Name(name)) => Some(&name.name),
        _ => None,
    }
}

/// A layout argument's value as a non-negative integer constant.
fn integer_value(arg: &ast::Arg, consts: &HashMap<String, (Const, Type)>) -> Option<u32> {
    arg.name.as_ref()?;
    match constant(&arg.value, consts)? {
        Const::Int(value) => u32::try_from(value).ok(),
        _ => None,
    }
}

/// The names of `main`'s `layout(xitop)` parameters, which array lengths
/// may name.
fn xitop_names(file: &ast::File) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) if function.name.name == "main" => Some(function),
            _ => None,
        })
        .flat_map(|main| &main.params)
        .filter(|param| {
            param
                .layout
                .iter()
                .flat_map(|layout| &layout.args)
                .any(|arg| arg.is_flag() && arg.key() == Some("xitop"))
        })
        .map(|param| param.name.name.clone())
        .collect()
}

fn struct_def(
    decl: &ast::StructDecl,
    structs: &[StructDef],
    struct_ids: &HashMap<String, StructId>,
    consts: &HashMap<String, (Const, Type)>,
    xitop: &[String],
) -> Checked<StructDef> {
    let mut packer = Packer::new();
    let mut seen: HashMap<String, Pos> = HashMap::new();
    for member in &decl.members {
        let base = resolve_type(&member.ty, struct_ids)?;
        // The member line's layout applies to each name on it.
        let mut lane = None;
        let mut offset = None;
        let mut unpack: Option<Unpack> = None;
        let mut unsigned = false;
        let mut row_major = None;
        for arg in member.layout.iter().flat_map(|layout| &layout.args) {
            let key = layout_key(arg)?;
            match key {
                "lane" => {
                    let letter = name_value(arg)
                        .filter(|name| name.len() == 1)
                        .and_then(|name| name.chars().next())
                        .and_then(Lane::from_letter);
                    lane = Some(letter.ok_or_else(|| bad_value(key, "x, y, z or w", arg.pos))?);
                }
                "offset" => {
                    offset = Some(
                        integer_value(arg, consts)
                            .ok_or_else(|| bad_value(key, "a qword offset", arg.pos))?,
                    );
                }
                "unpack" => {
                    let format = name_value(arg).and_then(Unpack::from_name);
                    unpack = Some(format.ok_or_else(|| {
                        bad_value(key, "a VIF format such as v4_32, v4_16 or v4_8", arg.pos)
                    })?);
                }
                "unsigned" if arg.is_flag() => unsigned = true,
                "row_major" | "column_major" if arg.is_flag() => {
                    if !matches!(base, Type::Mat { .. }) {
                        return Err(Box::new(problem(
                            VuslMessage::UnknownLayout {
                                name: key.into(),
                                context: "a member that is not a matrix",
                            },
                            arg.pos,
                        )));
                    }
                    row_major = Some(key == "row_major");
                }
                _ => {
                    return Err(Box::new(problem(
                        VuslMessage::UnknownLayout {
                            name: key.into(),
                            context: "a member",
                        },
                        arg.pos,
                    )));
                }
            }
        }
        if unsigned {
            match unpack.as_mut() {
                Some(format) => format.unsigned = true,
                None => {
                    return Err(Box::new(problem(
                        VuslMessage::UnknownLayout {
                            name: "unsigned".into(),
                            context: "a member without unpack",
                        },
                        member.pos,
                    )));
                }
            }
        }
        let base = match (base, row_major) {
            (Type::Mat { size, .. }, Some(row_major)) => Type::Mat { size, row_major },
            (base, _) => base,
        };
        for declarator in &member.names {
            if let Some(first) = seen.get(&declarator.name.name) {
                return Err(Box::new(
                    problem(
                        VuslMessage::Redeclared {
                            name: declarator.name.name.clone(),
                        },
                        declarator.name.pos,
                    )
                    .with_note(
                        *first,
                        VuslMessage::FirstDeclaredHere {
                            name: declarator.name.name.clone(),
                        },
                    ),
                ));
            }
            seen.insert(declarator.name.name.clone(), declarator.name.pos);
            let ty = match &declarator.array {
                None => base.clone(),
                Some(length) => {
                    let length =
                        array_length(length, &declarator.name, packer.members(), consts, xitop)?;
                    Type::Array(Box::new(base.clone()), length)
                }
            };
            let spec = MemberSpec {
                name: declarator.name.name.clone(),
                ty,
                lane,
                offset,
                unpack,
                pos: declarator.name.pos,
            };
            packer.place(spec, structs).map_err(|error| {
                Box::new(problem(
                    VuslMessage::Layout {
                        member: error.member,
                        kind: error.kind,
                    },
                    error.pos,
                ))
            })?;
        }
    }
    let (members, size, open) = packer.finish();
    Ok(StructDef {
        name: decl.name.name.clone(),
        pos: decl.name.pos,
        members,
        size,
        open,
    })
}

/// A member array's length: a positive constant, an earlier `int16`
/// member, a `layout(xitop)` parameter of `main`, or open.
fn array_length(
    length: &ArrayLen,
    name: &ast::Ident,
    earlier: &[super::layout::MemberDef],
    consts: &HashMap<String, (Const, Type)>,
    xitop: &[String],
) -> Checked<Length> {
    let bad = || {
        Box::new(problem(
            VuslMessage::BadArrayLength {
                name: name.name.clone(),
            },
            name.pos,
        ))
    };
    let expr = match length {
        ArrayLen::Open => return Ok(Length::Open),
        ArrayLen::Expr(expr) => expr,
    };
    if let ExprKind::Name(count) = &expr.kind
        && let Some(index) = earlier.iter().position(|member| member.name == count.name)
    {
        return match earlier[index].ty {
            Type::Int16 => Ok(Length::Member(vec![index])),
            _ => Err(bad()),
        };
    }
    if let ExprKind::Name(count) = &expr.kind
        && xitop.contains(&count.name)
    {
        return Ok(Length::Xitop);
    }
    match constant(expr, consts) {
        Some(Const::Int(value)) if value > 0 && value <= i64::from(DATA_MEMORY_QWORDS) => {
            Ok(Length::Fixed(value as u32))
        }
        _ => Err(bad()),
    }
}

/// `main`'s parameters, bound in memory.
pub fn bindings(decls: &Declarations) -> Checked<Vec<Param>> {
    let mut params: Vec<Param> = Vec::new();
    let mut seen: HashMap<String, Pos> = HashMap::new();
    for param in &decls.main.params {
        let name = &param.name.name;
        if let Some(first) = seen.get(name) {
            return Err(Box::new(
                problem(
                    VuslMessage::Redeclared { name: name.clone() },
                    param.name.pos,
                )
                .with_note(
                    *first,
                    VuslMessage::FirstDeclaredHere { name: name.clone() },
                ),
            ));
        }
        seen.insert(name.clone(), param.name.pos);
        let ty = resolve_type(&param.ty, &decls.struct_ids)?;
        let qualifier = param
            .qualifier
            .map_or(Qualifier::In, |(qualifier, _)| qualifier);
        let mut placement: Option<ParamPlacement> = None;
        let mut vif_base = None;
        let mut vif_offset = None;
        let mut spill = false;
        let mut alias = None;
        let two = || {
            Box::new(problem(
                VuslMessage::TwoPlacements { name: name.clone() },
                param.name.pos,
            ))
        };
        for arg in param.layout.iter().flat_map(|layout| &layout.args) {
            let key = arg.key().ok_or_else(|| {
                Box::new(problem(
                    VuslMessage::UnknownLayout {
                        name: crate::syntax::print::expr(&arg.value),
                        context: "a parameter of main",
                    },
                    arg.pos,
                ))
            })?;
            match key {
                "address" => {
                    let address = integer_value(arg, &decls.consts)
                        .filter(|address| *address < DATA_MEMORY_QWORDS)
                        .ok_or_else(|| bad_value(key, "a qword address from 0 to 1023", arg.pos))?;
                    if placement
                        .replace(ParamPlacement::Address(address))
                        .is_some()
                    {
                        return Err(two());
                    }
                }
                "xtop" if arg.is_flag() => {
                    if placement
                        .replace(ParamPlacement::Xtop { vif: None })
                        .is_some()
                    {
                        return Err(two());
                    }
                }
                "xitop" if arg.is_flag() => {
                    if placement.replace(ParamPlacement::Xitop).is_some() {
                        return Err(two());
                    }
                }
                "vif_base" => {
                    vif_base = Some(
                        integer_value(arg, &decls.consts)
                            .ok_or_else(|| bad_value(key, "a qword address", arg.pos))?,
                    );
                }
                "vif_offset" => {
                    vif_offset = Some(
                        integer_value(arg, &decls.consts)
                            .filter(|offset| *offset > 0)
                            .ok_or_else(|| bad_value(key, "a number of qwords", arg.pos))?,
                    );
                }
                "spill" if arg.is_flag() => spill = true,
                "alias" => {
                    let other = name_value(arg)
                        .and_then(|other| params.iter().position(|earlier| earlier.name == other));
                    alias = Some(other.ok_or_else(|| {
                        bad_value(key, "the name of an earlier parameter", arg.pos)
                    })?);
                }
                _ => {
                    return Err(Box::new(problem(
                        VuslMessage::UnknownLayout {
                            name: key.into(),
                            context: "a parameter of main",
                        },
                        arg.pos,
                    )));
                }
            }
        }
        let placement = match (placement, vif_base, vif_offset) {
            (None, ..) => {
                return Err(Box::new(problem(
                    VuslMessage::NoPlacement { name: name.clone() },
                    param.name.pos,
                )));
            }
            (Some(ParamPlacement::Xtop { .. }), Some(base), Some(offset)) => {
                if base + 2 * offset > DATA_MEMORY_QWORDS {
                    return Err(Box::new(problem(
                        VuslMessage::BadPlacement {
                            name: name.clone(),
                            reason: "puts the second buffer past the end of data memory",
                        },
                        param.name.pos,
                    )));
                }
                ParamPlacement::Xtop {
                    vif: Some(Vif { base, offset }),
                }
            }
            (Some(placement), None, None) => placement,
            _ => {
                return Err(Box::new(problem(
                    VuslMessage::BadPlacement {
                        name: name.clone(),
                        reason: "needs vif_base and vif_offset together, with xtop",
                    },
                    param.name.pos,
                )));
            }
        };
        match (&ty, placement) {
            (Type::Int16, ParamPlacement::Xitop) => {}
            (_, ParamPlacement::Xitop) => {
                return Err(Box::new(problem(
                    VuslMessage::BadPlacement {
                        name: name.clone(),
                        reason: "is xitop, which gives an int16 count",
                    },
                    param.name.pos,
                )));
            }
            (Type::Struct(_), _) => {}
            _ => {
                return Err(Box::new(problem(
                    VuslMessage::MainParamType {
                        name: name.clone(),
                        ty: decls.type_name(&ty),
                    },
                    param.name.pos,
                )));
            }
        }
        if spill
            && !(matches!(placement, ParamPlacement::Address(_)) && qualifier == Qualifier::Inout)
        {
            return Err(Box::new(problem(
                VuslMessage::BadPlacement {
                    name: name.clone(),
                    reason: "is spill, which needs an inout struct at a fixed address",
                },
                param.name.pos,
            )));
        }
        let fixed = match &ty {
            Type::Struct(id) => decls.structs[*id].size.fixed,
            _ => 0,
        };
        match placement {
            ParamPlacement::Address(address) if address + fixed > DATA_MEMORY_QWORDS => {
                return Err(Box::new(problem(
                    VuslMessage::OutsideMemory {
                        name: name.clone(),
                        address,
                        qwords: fixed,
                    },
                    param.name.pos,
                )));
            }
            ParamPlacement::Xtop { vif: Some(vif) } if fixed > vif.offset => {
                return Err(Box::new(problem(
                    VuslMessage::DoesNotFit {
                        name: name.clone(),
                        qwords: fixed,
                        half: vif.offset,
                    },
                    param.name.pos,
                )));
            }
            _ => {}
        }
        params.push(Param {
            name: name.clone(),
            qualifier,
            ty,
            placement,
            spill,
            alias,
            pos: param.name.pos,
        });
    }
    check_overlaps(decls, &params)?;
    Ok(params)
}

/// The data memory each fixed or double-buffered parameter takes, at least.
fn regions(decls: &Declarations, params: &[Param]) -> Vec<(usize, u32, u32)> {
    let mut regions = Vec::new();
    for (index, param) in params.iter().enumerate() {
        let fixed = match &param.ty {
            Type::Struct(id) => decls.structs[*id].size.fixed,
            _ => continue,
        };
        match param.placement {
            ParamPlacement::Address(address) => regions.push((index, address, address + fixed)),
            ParamPlacement::Xtop { vif: Some(vif) } => {
                regions.push((index, vif.base, vif.base + 2 * vif.offset));
            }
            _ => {}
        }
    }
    regions
}

fn check_overlaps(decls: &Declarations, params: &[Param]) -> Checked<()> {
    let regions = regions(decls, params);
    for (position, &(first, start, end)) in regions.iter().enumerate() {
        for &(second, other_start, other_end) in &regions[position + 1..] {
            let aliased =
                params[second].alias == Some(first) || params[first].alias == Some(second);
            let empty = start == end || other_start == other_end;
            if !aliased && !empty && start < other_end && other_start < end {
                return Err(Box::new(problem(
                    VuslMessage::Overlap {
                        first: params[first].name.clone(),
                        second: params[second].name.clone(),
                    },
                    params[second].pos,
                )));
            }
        }
    }
    Ok(())
}
