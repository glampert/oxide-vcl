//! The file's declarations (structs and their layouts, constants,
//! functions, file pragmas) and `main`'s parameters bound in memory.

use std::collections::HashMap;

use super::consteval;
use super::hir::{Const, EnumDef, Library, Param, ParamPlacement, Vif};
use super::layout::{MemberSpec, Packer, StructDef, Unpack};
use super::types::{Lane, Length, Type, TypeNames, builtin_type};
use super::{Checked, Problem, problem};
use crate::diagnostics::VuslMessage;
use crate::syntax::Pos;
use crate::syntax::ast::{self, ArrayLen, ExprKind, Item, Qualifier};

/// Data memory: 1024 qwords on VU1.
pub const DATA_MEMORY_QWORDS: u32 = 1024;

/// What the file declares.
pub struct Declarations<'a> {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    /// The types the file names: its structs and enums, and its aliases'
    /// types.
    pub types: HashMap<String, Type>,
    /// File-scope constants: their value and type.
    pub consts: HashMap<String, (Const, Type)>,
    /// File-scope vector constants: their components.
    pub vectors: HashMap<String, Vec<f32>>,
    pub functions: HashMap<String, &'a ast::Function>,
    /// `main`, which a library's own build has none of.
    pub main: Option<&'a ast::Function>,
    pub program_name: Option<String>,
    /// `#pragma library(...)`: the library the program calls, or is.
    pub library: Option<Library>,
    /// The library's exports, in declaration order: their entry indices.
    pub exports: Vec<String>,
    /// The library's memory, bound at file scope.
    pub memory: Vec<&'a ast::Param>,
}

impl Declarations<'_> {
    /// Whether a call of `name` calls the library rather than inlining:
    /// in a program, which has `main`, calls of exports do.
    pub fn calls_library(&self, name: &str) -> bool {
        self.main.is_some() && self.exports.iter().any(|export| export == name)
    }
}

impl Declarations<'_> {
    pub fn type_names(&self) -> TypeNames {
        type_names(&self.structs, &self.enums)
    }

    pub fn type_name(&self, ty: &Type) -> String {
        ty.name(&self.type_names())
    }

    /// The type `name` names: a built-in, or the file's.
    pub fn named_type(&self, name: &str) -> Option<Type> {
        builtin_type(name).or_else(|| self.types.get(name).cloned())
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
pub fn type_names(structs: &[StructDef], enums: &[EnumDef]) -> TypeNames {
    TypeNames {
        structs: structs.iter().map(|def| def.name.clone()).collect(),
        enums: enums.iter().map(|def| def.name.clone()).collect(),
    }
}

/// The type `ty` names: a built-in, a `ref<T>`, or one of `types`, the
/// file's structs, enums and aliases.
pub fn resolve_type(ty: &ast::TypeName, types: &HashMap<String, Type>) -> Checked<Type> {
    if let Some(argument) = &ty.argument {
        return Ok(Type::Ref(Box::new(resolve_type(argument, types)?)));
    }
    if let Some(builtin) = builtin_type(&ty.name.name) {
        return Ok(builtin);
    }
    match types.get(&ty.name.name) {
        Some(found) => Ok(found.clone()),
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
        // An enum's value is not an integer: only its own type takes it.
        ExprKind::Name(name) => consts
            .get(&name.name)
            .filter(|(_, ty)| !matches!(ty, Type::Enum(_)))
            .map(|(value, _)| *value),
        ExprKind::Unary { op, operand } => consteval::unary(*op, constant(operand, consts)?).ok(),
        ExprKind::Binary { op, lhs, rhs, .. } => {
            consteval::binary(*op, constant(lhs, consts)?, constant(rhs, consts)?).ok()
        }
        _ => None,
    }
}

/// A `vecN` constant's components: `vecN(c, ...)` with N constant
/// components, or one for all. `None` when it is not a constructor of
/// constants.
pub fn vector_constant(
    expr: &ast::Expr,
    n: u8,
    consts: &HashMap<String, (Const, Type)>,
) -> Result<Option<Vec<f32>>, Problem> {
    let ExprKind::Call { callee, args } = &expr.kind else {
        return Ok(None);
    };
    let name = format!("vec{n}");
    if callee.argument.is_some() || callee.name.name != name {
        return Ok(None);
    }
    let mut components = Vec::new();
    for arg in args {
        match constant(arg, consts) {
            Some(Const::Int(value)) => components.push(value as f32),
            Some(Const::Float(value)) => components.push(value),
            _ => return Ok(None),
        }
    }
    match components.len() {
        1 => Ok(Some(vec![components[0]; usize::from(n)])),
        count if count == usize::from(n) => Ok(Some(components)),
        found => Err(problem(
            VuslMessage::WrongArgumentCount {
                name,
                expected: format!("{n} components"),
                found,
            },
            expr.pos,
        )),
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
    let mut enums: Vec<EnumDef> = Vec::new();
    let mut types: HashMap<String, Type> = HashMap::new();
    let mut consts: HashMap<String, (Const, Type)> = HashMap::new();
    let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
    let mut functions: HashMap<String, &'a ast::Function> = HashMap::new();
    let mut main = None;
    let mut program_name: Option<String> = None;
    let mut library: Option<Library> = None;
    let mut exports: Vec<String> = Vec::new();
    let mut memory: Vec<&'a ast::Param> = Vec::new();
    let xitop = xitop_names(file);
    for item in &file.items {
        match item {
            Item::Pragma(pragma) if pragma.name.name == "library" => {
                if library.is_some() {
                    return Err(Box::new(problem(
                        VuslMessage::Redeclared {
                            name: "#pragma library".into(),
                        },
                        pragma.pos,
                    )));
                }
                library = Some(library_pragma(pragma, &consts)?);
            }
            Item::Pragma(pragma) => {
                program_pragma(pragma, &mut program_name, warnings)?;
            }
            Item::Memory(param) => {
                if library.is_none() {
                    return Err(Box::new(problem(
                        VuslMessage::OutsideLibrary {
                            what: "memory at file scope",
                        },
                        param.pos,
                    )));
                }
                if memory
                    .iter()
                    .any(|other| other.name.name == param.name.name)
                {
                    return Err(Box::new(problem(
                        VuslMessage::Redeclared {
                            name: param.name.name.clone(),
                        },
                        param.name.pos,
                    )));
                }
                memory.push(param);
            }
            Item::Function(function) if function.export => {
                if library.is_none() {
                    return Err(Box::new(problem(
                        VuslMessage::OutsideLibrary { what: "export" },
                        function.pos,
                    )));
                }
                let name = &function.name.name;
                match functions.get(name) {
                    // A definition after its prototype, with the same
                    // signature.
                    Some(earlier) if earlier.prototype && !function.prototype => {
                        if !same_signature(earlier, function) {
                            return Err(Box::new(
                                problem(
                                    VuslMessage::ExportMismatch { name: name.clone() },
                                    function.name.pos,
                                )
                                .with_note(
                                    earlier.name.pos,
                                    VuslMessage::FirstDeclaredHere { name: name.clone() },
                                ),
                            ));
                        }
                        functions.insert(name.clone(), function);
                    }
                    _ => {
                        names.declare(&function.name)?;
                        exports.push(name.clone());
                        functions.insert(name.clone(), function);
                    }
                }
            }
            Item::Struct(decl) => {
                names.declare(&decl.name)?;
                let def = struct_def(decl, &structs, &types, &consts, &xitop)?;
                types.insert(def.name.clone(), Type::Struct(structs.len()));
                structs.push(def);
            }
            Item::Enum(decl) => {
                names.declare(&decl.name)?;
                let def = enum_def(decl, &types, &consts)?;
                types.insert(def.name.clone(), Type::Enum(enums.len()));
                enums.push(def);
            }
            Item::Alias(alias) => {
                if builtin_type(&alias.name.name).is_some() {
                    return Err(Box::new(problem(
                        VuslMessage::BuiltinTypeName {
                            name: alias.name.name.clone(),
                        },
                        alias.name.pos,
                    )));
                }
                names.declare(&alias.name)?;
                let ty = resolve_type(&alias.ty, &types)?;
                types.insert(alias.name.name.clone(), ty);
            }
            Item::Const(decl) => {
                names.declare(&decl.name)?;
                let ty = resolve_type(&decl.ty, &types)?;
                let not_constant = || {
                    Box::new(problem(
                        VuslMessage::NotConstant {
                            name: decl.name.name.clone(),
                        },
                        decl.name.pos,
                    ))
                };
                if let Type::Vec(n) = ty {
                    let components = vector_constant(&decl.value, n, &consts)
                        .map_err(Box::new)?
                        .ok_or_else(not_constant)?;
                    vectors.insert(decl.name.name.clone(), components);
                    continue;
                }
                if let Type::Enum(id) = ty {
                    let value = enum_constant(&decl.value, id, &enums, &types, &consts)?;
                    consts.insert(decl.name.name.clone(), (Const::Int(value), ty));
                    continue;
                }
                if !matches!(ty, Type::Int16 | Type::Float | Type::Bool) {
                    return Err(Box::new(problem(
                        VuslMessage::ConstantType {
                            name: decl.name.name.clone(),
                            ty: ty.name(&type_names(&structs, &enums)),
                        },
                        decl.name.pos,
                    )));
                }
                if let Some(found) = enum_typed(&decl.value, &enums, &types, &consts) {
                    return Err(Box::new(problem(
                        VuslMessage::EnumConversion {
                            context: "the constant".into(),
                            expected: ty.name(&TypeNames::default()),
                            found,
                        },
                        decl.value.pos,
                    )));
                }
                let value = constant(&decl.value, &consts).ok_or_else(not_constant)?;
                let Some(value) = typed_constant(value, &ty) else {
                    return Err(Box::new(problem(
                        VuslMessage::TypeMismatch {
                            context: format!("constant '{}'", decl.name.name),
                            expected: ty.name(&TypeNames::default()),
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
    match main {
        None if library.is_none() => {
            return Err(Box::new(problem(VuslMessage::NoMain, Pos::default())));
        }
        Some(main) if main.ret.name.name != "void" || main.ret.argument.is_some() => {
            return Err(Box::new(problem(VuslMessage::MainReturns, main.ret.pos)));
        }
        _ => {}
    }
    Ok(Declarations {
        structs,
        enums,
        types,
        consts,
        vectors,
        functions,
        main,
        program_name,
        library,
        exports,
        memory,
    })
}

/// Whether two declarations of a function have the same signature.
fn same_signature(a: &ast::Function, b: &ast::Function) -> bool {
    let ty = |ty: &ast::TypeName| crate::syntax::print::type_name(ty);
    ty(&a.ret) == ty(&b.ret)
        && a.params.len() == b.params.len()
        && a.params.iter().zip(&b.params).all(|(x, y)| {
            ty(&x.ty) == ty(&y.ty)
                && x.qualifier.map(|(q, _)| q) == y.qualifier.map(|(q, _)| q)
                && x.name.name == y.name.name
        })
}

/// `#pragma library(name, address = A, vf = F, vi = I)`.
fn library_pragma(
    pragma: &ast::Pragma,
    consts: &HashMap<String, (Const, Type)>,
) -> Checked<Library> {
    let bad = || {
        Box::new(problem(
            VuslMessage::BadPragma {
                name: "library".into(),
                expected: "(name, address = A, vf = F, vi = I, saved_vf = SF, saved_vi = SI, save = memory): the library's micro address, the first VF and VI registers of its bank (at most 31 and 14), and how many registers below each it saves, in its memory `save`",
            },
            pragma.pos,
        ))
    };
    let args = pragma.args.as_deref().ok_or_else(bad)?;
    let mut name = None;
    let (mut address, mut vf, mut vi) = (None, 16, 11);
    let (mut saved_vf, mut saved_vi, mut save) = (0, 0, None);
    for arg in args {
        match arg.name.as_ref().map(|name| name.name.as_str()) {
            None if arg.is_flag() && name.is_none() => name = arg.key().map(str::to_string),
            Some("saved_vf") => saved_vf = integer_value(arg, consts).ok_or_else(bad)?,
            Some("saved_vi") => saved_vi = integer_value(arg, consts).ok_or_else(bad)?,
            Some("save") => save = Some(name_value(arg).ok_or_else(bad)?.to_string()),
            Some("address") => {
                address = integer_value(arg, consts).filter(|&value| value < 2048);
                if address.is_none() {
                    return Err(bad());
                }
            }
            Some("vf") => {
                vf = integer_value(arg, consts)
                    .filter(|&v| (1..=31).contains(&v))
                    .ok_or_else(bad)?
            }
            Some("vi") => {
                vi = integer_value(arg, consts)
                    .filter(|&v| (2..=14).contains(&v))
                    .ok_or_else(bad)?
            }
            _ => return Err(bad()),
        }
    }
    let (Some(name), Some(address)) = (name, address) else {
        return Err(bad());
    };
    // Saved registers below the banks: VF01 up and VI02 up (VI01 is the
    // clip flags'), and somewhere to save them.
    let saves = saved_vf + saved_vi > 0;
    if saved_vf >= vf || saved_vi + 2 > vi || saves != save.is_some() {
        return Err(bad());
    }
    Ok(Library {
        name,
        address: address as u16,
        vf: vf as u8,
        vi: vi as u8,
        saved_vf: saved_vf as u8,
        saved_vi: saved_vi as u8,
        save,
        pos: pragma.pos,
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

/// An enum's values: each its own, or one more than the one before (0
/// for the first). A value's expression may name the enum's earlier
/// values, as integers.
fn enum_def(
    decl: &ast::EnumDecl,
    types: &HashMap<String, Type>,
    consts: &HashMap<String, (Const, Type)>,
) -> Checked<EnumDef> {
    if let Some(underlying) = &decl.underlying {
        let ty = resolve_type(underlying, types)?;
        if ty != Type::Int16 {
            return Err(Box::new(problem(
                VuslMessage::EnumUnderlying {
                    name: decl.name.name.clone(),
                    found: ty.name(&TypeNames::default()),
                },
                underlying.pos,
            )));
        }
    }
    let mut scope = consts.clone();
    let mut seen: HashMap<String, Pos> = HashMap::new();
    let mut values = Vec::new();
    let mut next = 0;
    for enumerator in &decl.values {
        let name = &enumerator.name;
        if let Some(first) = seen.insert(name.name.clone(), name.pos) {
            return Err(Box::new(
                problem(
                    VuslMessage::Redeclared {
                        name: name.name.clone(),
                    },
                    name.pos,
                )
                .with_note(
                    first,
                    VuslMessage::FirstDeclaredHere {
                        name: name.name.clone(),
                    },
                ),
            ));
        }
        let value = match &enumerator.value {
            None => next,
            Some(written) => match constant(written, &scope) {
                Some(Const::Int(value)) => value,
                _ => {
                    return Err(Box::new(problem(
                        VuslMessage::NotConstant {
                            name: format!("{}::{}", decl.name.name, name.name),
                        },
                        written.pos,
                    )));
                }
            },
        };
        if !int16_range(value) {
            return Err(Box::new(problem(
                VuslMessage::ConstantRange {
                    value: value.to_string(),
                    ty: "int16",
                },
                enumerator
                    .value
                    .as_ref()
                    .map_or(name.pos, |value| value.pos),
            )));
        }
        scope.insert(name.name.clone(), (Const::Int(value), Type::Int16));
        values.push((name.name.clone(), value));
        next = value + 1;
    }
    Ok(EnumDef {
        name: decl.name.name.clone(),
        pos: decl.name.pos,
        values,
    })
}

/// The enum an initializer's value is of, when it is one of an enum's
/// values, a constant of an enum or an enum's constructor.
fn enum_typed(
    expr: &ast::Expr,
    enums: &[EnumDef],
    types: &HashMap<String, Type>,
    consts: &HashMap<String, (Const, Type)>,
) -> Option<String> {
    let id = match &expr.kind {
        ExprKind::Scoped { scope, .. } => match types.get(&scope.name) {
            Some(Type::Enum(id)) => *id,
            _ => return None,
        },
        ExprKind::Name(name) => match consts.get(&name.name) {
            Some((_, Type::Enum(id))) => *id,
            _ => return None,
        },
        ExprKind::Call { callee, .. } if callee.argument.is_none() => {
            match types.get(&callee.name.name) {
                Some(Type::Enum(id)) => *id,
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(enums[id].name.clone())
}

/// The value of a constant of enum `id`: one of its values
/// (`ClipPlane::Near`), a constant of the same enum, or the enum's
/// constructor of an integer constant (`ClipPlane(2)`).
fn enum_constant(
    expr: &ast::Expr,
    id: usize,
    enums: &[EnumDef],
    types: &HashMap<String, Type>,
    consts: &HashMap<String, (Const, Type)>,
) -> Checked<i64> {
    let names = || type_names(&[], enums);
    let found = match &expr.kind {
        ExprKind::Scoped { scope, name } => {
            let ty = types.get(&scope.name).cloned();
            match ty {
                Some(Type::Enum(of)) => {
                    let value = enums[of].value(&name.name).ok_or_else(|| {
                        Box::new(problem(
                            VuslMessage::UnknownEnumerator {
                                ty: enums[of].name.clone(),
                                name: name.name.clone(),
                            },
                            name.pos,
                        ))
                    })?;
                    if of == id {
                        return Ok(value);
                    }
                    Type::Enum(of).name(&names())
                }
                _ => {
                    return Err(Box::new(problem(
                        VuslMessage::NotAnEnum {
                            name: scope.name.clone(),
                        },
                        scope.pos,
                    )));
                }
            }
        }
        ExprKind::Name(name) => match consts.get(&name.name) {
            Some((Const::Int(value), Type::Enum(of))) if *of == id => return Ok(*value),
            Some((_, ty)) => ty.name(&names()),
            None => "a value that is not constant".into(),
        },
        ExprKind::Call { callee, args }
            if callee.argument.is_none()
                && types.get(&callee.name.name) == Some(&Type::Enum(id)) =>
        {
            match args.as_slice() {
                [arg] => match constant(arg, consts) {
                    Some(Const::Int(value)) if int16_range(value) => return Ok(value),
                    Some(Const::Int(value)) => {
                        return Err(Box::new(problem(
                            VuslMessage::ConstantRange {
                                value: value.to_string(),
                                ty: "int16",
                            },
                            arg.pos,
                        )));
                    }
                    _ => "a constructor of a value that is not an integer constant".into(),
                },
                _ => "a constructor without one argument".into(),
            }
        }
        _ => match constant(expr, consts) {
            Some(Const::Int(value)) => format!("the integer constant {value}"),
            _ => "a value that is not one of its values".into(),
        },
    };
    Err(Box::new(problem(
        VuslMessage::EnumConversion {
            context: "the constant".into(),
            expected: enums[id].name.clone(),
            found,
        },
        expr.pos,
    )))
}

fn struct_def(
    decl: &ast::StructDecl,
    structs: &[StructDef],
    types: &HashMap<String, Type>,
    consts: &HashMap<String, (Const, Type)>,
    xitop: &[String],
) -> Checked<StructDef> {
    let mut packer = Packer::new();
    let mut seen: HashMap<String, Pos> = HashMap::new();
    for member in &decl.members {
        let base = resolve_type(&member.ty, types)?;
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
    let (members, size, open, bounds) = packer.finish();
    Ok(StructDef {
        name: decl.name.name.clone(),
        pos: decl.name.pos,
        members,
        size,
        open,
        bounds,
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
    // main's parameters and the library's memory are named apart: the
    // library's are seen only in its functions.
    let mut seen: [HashMap<String, Pos>; 2] = [HashMap::new(), HashMap::new()];
    let declared = decls
        .main
        .iter()
        .flat_map(|main| &main.params)
        .map(|param| (param, false))
        .chain(decls.memory.iter().map(|param| (*param, true)));
    for (param, library) in declared {
        let name = &param.name.name;
        let seen = &mut seen[usize::from(library)];
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
        let ty = resolve_type(&param.ty, &decls.types)?;
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
                    let other = name_value(arg).and_then(|other| {
                        params
                            .iter()
                            .position(|earlier| earlier.name == other && earlier.library == library)
                    });
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
            library,
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
            // Memory a library only reads, such as the frame constants, may
            // be the caller's too: the library reads it at the call, and
            // never writes it. Memory it writes is its own.
            let read_only =
                |param: &Param| matches!(param.qualifier, Qualifier::Uniform | Qualifier::In);
            let shared = (params[first].library != params[second].library)
                && [first, second]
                    .iter()
                    .all(|&index| !params[index].library || read_only(&params[index]));
            let aliased = aliased || shared;
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
