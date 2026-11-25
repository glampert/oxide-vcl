//! What the reflection says, computed once from the checked program for
//! both writers: the program, its parameters with their placements, sizes,
//! limits and the members it reads, writes and kicks, and its structs'
//! layouts.

use crate::sema::DATA_MEMORY_QWORDS;
use crate::sema::hir::{Base, Count, EnumDef, LoopKind, ParamPlacement, Program, Stmt};
use crate::sema::layout::{Extent, StructDef};
use crate::sema::type_names;
use crate::sema::types::{Length, Type};
use crate::syntax::ast::Qualifier;

/// The reflection of one program.
#[derive(Clone, Debug)]
pub struct Reflection {
    pub program: String,
    /// The file the program was compiled from.
    pub source: String,
    /// 64-bit instructions in micro memory, once the code exists.
    pub instructions: Option<usize>,
    pub uses: Vec<&'static str>,
    pub structs: Vec<StructInfo>,
    /// Every enum, by id; the writers describe those a struct holds.
    pub enums: Vec<EnumInfo>,
    pub params: Vec<ParamInfo>,
    /// The library the program calls, or that this build is.
    pub library: Option<LibraryInfo>,
}

/// `#pragma library`: the library's name and micro address, and for the
/// library's own build its exports, in entry-table order.
#[derive(Clone, Debug)]
pub struct LibraryInfo {
    pub name: String,
    /// In 64-bit instructions, as MPG and MSCAL take addresses.
    pub address: u16,
    /// Empty in a program that calls the library.
    pub exports: Vec<String>,
}

impl LibraryInfo {
    /// The micro address of export `index`.
    pub fn entry_address(&self, index: usize) -> u16 {
        self.address + 2 * index as u16
    }
}

/// A struct and where it was declared.
#[derive(Clone, Debug)]
pub struct StructInfo {
    pub def: StructDef,
    pub file: String,
    /// Declared in an included file: its C++ definition is shared between
    /// headers, keyed on `hash`.
    pub shared: bool,
    /// A hash of the layout, as 8 hex digits.
    pub hash: String,
}

/// An enum and where it was declared.
#[derive(Clone, Debug)]
pub struct EnumInfo {
    pub def: EnumDef,
    pub file: String,
    /// Declared in an included file, as for a struct.
    pub shared: bool,
    /// A hash of its name and values, as 8 hex digits.
    pub hash: String,
    /// A struct member holds it: the writers describe it.
    pub used: bool,
}

/// One of `main`'s parameters.
#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub qualifier: Qualifier,
    /// The struct's index, or `None` for an `int16` count.
    pub struct_id: Option<usize>,
    pub placement: ParamPlacement,
    pub spill: bool,
    /// Memory the library binds at file scope, in a program that calls it.
    pub library: bool,
    /// Its qwords: the struct's fixed part and dependent lengths, plus the
    /// open arrays the program writes, by the lengths it writes them with.
    pub size: Extent,
    /// Members read, written and kicked, as dotted paths.
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub kicks: Vec<String>,
    /// The largest length that fits, when the size has one.
    pub limit: Option<Limit>,
}

/// The largest value of a length read from memory that keeps a parameter
/// within its space.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limit {
    /// The `int16` member, as a dotted path from the parameter.
    pub count: String,
    pub max: u32,
    /// The qwords there are: the double-buffer half, or data memory from
    /// the parameter's address.
    pub capacity: u32,
    pub why: String,
    /// What must fit in `capacity`: the size of everything, or of the
    /// arrays before a member at a fixed offset (a bound), as `fixed +
    /// stride * count`.
    pub fixed: u32,
    pub stride: u32,
    /// Whether the limit is a bound's, which the size does not show.
    pub bound: bool,
}

/// A path of member indices as dotted member names, from struct `id`.
/// Array members lead into their elements.
pub fn path_name(structs: &[StructDef], id: usize, path: &[usize]) -> String {
    if path.is_empty() {
        // A length that is the count XITOP reads.
        return "xitop".into();
    }
    let mut names = Vec::new();
    let mut current = Some(id);
    for &index in path {
        let Some(def) = current.and_then(|id| structs.get(id)) else {
            break;
        };
        let Some(member) = def.members.get(index) else {
            break;
        };
        names.push(member.name.clone());
        current = match &member.ty {
            Type::Struct(next) => Some(*next),
            Type::Array(element, _) => match **element {
                Type::Struct(next) => Some(next),
                _ => None,
            },
            _ => None,
        };
    }
    names.join(".")
}

/// An extent as text: `11 + count`, `18 + 6 * count`.
pub fn extent_text(extent: &Extent, structs: &[StructDef], id: usize) -> String {
    let mut parts = Vec::new();
    if extent.fixed != 0 || extent.terms.is_empty() {
        parts.push(extent.fixed.to_string());
    }
    for term in &extent.terms {
        let name = path_name(structs, id, &term.count);
        parts.push(if term.stride == 1 {
            name
        } else {
            format!("{} * {name}", term.stride)
        });
    }
    parts.join(" + ")
}

/// A 32-bit FNV-1a hash of `text`, as 8 hex digits.
fn hash(text: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// A struct's layout as text, for its hash: the members' names, types,
/// places and formats, and the layouts of the structs it holds.
fn layout_text(structs: &[StructDef], enums: &[EnumDef], id: usize) -> String {
    let names = type_names(structs, enums);
    let def = &structs[id];
    let mut text = format!("{}{{", def.name);
    for member in &def.members {
        text.push_str(&format!(
            "{}:{}@{}.{}",
            member.name,
            member.ty.name(&names),
            extent_text(&member.offset, structs, id),
            member.lane.letter()
        ));
        if let Some(unpack) = member.unpack {
            text.push_str(&format!(
                "/{}{}",
                unpack.name(),
                if unpack.unsigned { "u" } else { "" }
            ));
        }
        let element = match &member.ty {
            Type::Array(element, _) => element,
            ty => ty,
        };
        match element {
            Type::Struct(inner) => text.push_str(&layout_text(structs, enums, *inner)),
            Type::Enum(inner) => text.push_str(&enum_text(&enums[*inner])),
            _ => {}
        }
        text.push(';');
    }
    text.push('}');
    text
}

/// An enum's name and values as text, for its hash.
fn enum_text(def: &EnumDef) -> String {
    let values: Vec<String> = def
        .values
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    format!("{}{{{}}}", def.name, values.join(","))
}

/// The enum a member holds, itself or as its array's element.
pub fn member_enum(ty: &Type) -> Option<usize> {
    match ty {
        Type::Enum(id) => Some(*id),
        Type::Array(element, _) => member_enum(element),
        _ => None,
    }
}

/// Every statement, nested ones included.
fn walk<'a>(stmts: &'a [Stmt], visit: &mut dyn FnMut(&'a Stmt)) {
    for stmt in stmts {
        visit(stmt);
        match stmt {
            Stmt::If {
                then, otherwise, ..
            } => {
                walk(then, visit);
                walk(otherwise, visit);
            }
            Stmt::Loop { kind, body, .. } => {
                if let LoopKind::Condition {
                    cond_stmts, step, ..
                } = kind
                {
                    walk(cond_stmts, visit);
                    walk(step, visit);
                }
                walk(body, visit);
            }
            _ => {}
        }
    }
}

/// Computes the reflection of `program`. `struct_files` gives the file
/// each struct was declared in; those other than `source` are shared.
/// `exports` are a library build's exports, in entry-table order.
pub fn reflect(
    program: &Program,
    source: &str,
    struct_files: &[String],
    enum_files: &[String],
    exports: &[String],
    instructions: Option<usize>,
) -> Reflection {
    let structs = &program.structs;
    let enums: Vec<EnumInfo> = program
        .enums
        .iter()
        .enumerate()
        .map(|(id, def)| {
            let file = enum_files
                .get(id)
                .cloned()
                .unwrap_or_else(|| source.to_string());
            let used = structs.iter().any(|def| {
                def.members
                    .iter()
                    .any(|member| member_enum(&member.ty) == Some(id))
            });
            EnumInfo {
                def: def.clone(),
                shared: file != source,
                file,
                hash: hash(&enum_text(def)),
                used,
            }
        })
        .collect();
    let infos: Vec<StructInfo> = structs
        .iter()
        .enumerate()
        .map(|(id, def)| {
            let file = struct_files
                .get(id)
                .cloned()
                .unwrap_or_else(|| source.to_string());
            StructInfo {
                def: def.clone(),
                shared: file != source,
                file,
                hash: hash(&layout_text(structs, &program.enums, id)),
            }
        })
        .collect();
    let mut uses = Vec::new();
    for (used, name) in [
        (program.uses.xtop, "xtop"),
        (program.uses.xitop, "xitop"),
        (program.uses.clip, "clipw"),
        (program.uses.efu, "efu"),
        (program.uses.xgkick, "xgkick"),
        (program.uses.yield_, "yield"),
    ] {
        if used {
            uses.push(name);
        }
    }
    let mut params = Vec::new();
    for (index, param) in program.params.iter().enumerate() {
        let struct_id = match param.ty {
            Type::Struct(id) => Some(id),
            _ => None,
        };
        let mut size = Extent::default();
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        let mut kicks = Vec::new();
        // Whether every write to an open array is a range-for's, whose
        // length the size then counts.
        let mut open_counted = true;
        if let Some(id) = struct_id {
            size = structs[id].size.clone();
            // An open array takes the length the program writes it with.
            if let Some(open) = &structs[id].open {
                let mut counted = false;
                walk(&program.body, &mut |stmt| {
                    let Stmt::Loop {
                        kind: LoopKind::Range { elements, count },
                        ..
                    } = stmt
                    else {
                        return;
                    };
                    let writes_open = elements.iter().any(|&element| {
                        let array = &program.elements[element].array;
                        array.base == Base::Param(index)
                            && array.path == open.path
                            && matches!(array.ty, Type::Array(_, Length::Open))
                    });
                    if !writes_open {
                        return;
                    }
                    match count {
                        Count::Fixed(n) => {
                            size.fixed += n * open.stride;
                            counted = true;
                        }
                        Count::Member(place) if place.base == Base::Param(index) => {
                            size.add(&Extent {
                                fixed: 0,
                                terms: vec![crate::sema::layout::Term {
                                    count: place.path.clone(),
                                    stride: open.stride,
                                }],
                            });
                            counted = true;
                        }
                        Count::Xitop => {
                            size.add(&Extent {
                                fixed: 0,
                                terms: vec![crate::sema::layout::Term {
                                    count: Vec::new(),
                                    stride: open.stride,
                                }],
                            });
                            counted = true;
                        }
                        Count::Member(_) => {}
                    }
                });
                // Written some other way, through a ref, its length is the
                // program's to know.
                let written = program.accesses.iter().any(|access| {
                    access.param == index && access.write && access.path.starts_with(&open.path)
                });
                open_counted = counted || !written;
            }
            for access in &program.accesses {
                if access.param != index {
                    continue;
                }
                let name = path_name(structs, id, &access.path);
                let list = if access.write {
                    &mut writes
                } else {
                    &mut reads
                };
                if !list.contains(&name) {
                    list.push(name);
                }
            }
            walk(&program.body, &mut |stmt| {
                if let Stmt::Kick { place, .. } = stmt
                    && place.base == Base::Param(index)
                {
                    let name = path_name(structs, id, &place.path);
                    if !kicks.contains(&name) {
                        kicks.push(name);
                    }
                }
            });
        }
        let capacity = match param.placement {
            ParamPlacement::Xtop { vif: Some(vif) } => {
                Some((vif.offset, "the double-buffer half holds it"))
            }
            ParamPlacement::Address(address) => Some((
                DATA_MEMORY_QWORDS - address,
                "data memory holds it from its address",
            )),
            _ => None,
        };
        let mut limit = match (capacity, struct_id, size.terms.as_slice()) {
            (Some((capacity, why)), Some(id), [term]) if open_counted && size.fixed <= capacity => {
                Some(Limit {
                    count: path_name(structs, id, &term.count),
                    max: (capacity - size.fixed) / term.stride,
                    capacity,
                    why: why.into(),
                    fixed: size.fixed,
                    stride: term.stride,
                    bound: false,
                })
            }
            _ => None,
        };
        // Arrays that a member at a fixed offset follows must end by it.
        if let Some(id) = struct_id {
            for bound in &structs[id].bounds {
                let [term] = bound.end.terms.as_slice() else {
                    continue;
                };
                if bound.end.fixed > bound.at {
                    continue;
                }
                let count = path_name(structs, id, &term.count);
                let max = (bound.at - bound.end.fixed) / term.stride;
                let tighter = limit
                    .as_ref()
                    .is_none_or(|limit| limit.count == count && max < limit.max);
                if tighter {
                    limit = Some(Limit {
                        count,
                        max,
                        capacity: bound.at,
                        why: format!("the arrays end by qword {}", bound.at),
                        fixed: bound.end.fixed,
                        stride: term.stride,
                        bound: true,
                    });
                }
            }
        }
        params.push(ParamInfo {
            name: param.name.clone(),
            qualifier: param.qualifier,
            struct_id,
            placement: param.placement,
            spill: param.spill,
            library: param.library && exports.is_empty(),
            size,
            reads,
            writes,
            kicks,
            limit,
        });
    }
    Reflection {
        program: program.name.clone(),
        source: source.to_string(),
        instructions,
        uses,
        structs: infos,
        enums,
        params,
        library: program.library.as_ref().map(|library| LibraryInfo {
            name: library.name.clone(),
            address: library.address,
            exports: exports.to_vec(),
        }),
    }
}
