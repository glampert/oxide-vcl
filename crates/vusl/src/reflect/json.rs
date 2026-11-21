//! The reflection as JSON (`--reflect=FILE`), schema version 1, for tools:
//! the program, its parameters and its structs' layouts.

use std::fmt::Write;

use super::model::{Reflection, extent_text, path_name};
use crate::sema::hir::ParamPlacement;
use crate::sema::layout::Extent;
use crate::sema::types::{Length, Type};

/// A JSON value, with an object's keys in the order given.
enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

fn object(entries: Vec<(&str, Json)>) -> Json {
    Json::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn string(text: impl Into<String>) -> Json {
    Json::Str(text.into())
}

fn strings(texts: &[String]) -> Json {
    Json::Array(texts.iter().cloned().map(Json::Str).collect())
}

fn quote(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Whether a value prints on one line: scalars, and arrays of scalars.
fn is_flat(value: &Json) -> bool {
    match value {
        Json::Array(items) => items
            .iter()
            .all(|item| !matches!(item, Json::Array(_) | Json::Object(_))),
        Json::Object(entries) => entries.is_empty(),
        _ => true,
    }
}

fn write_value(out: &mut String, value: &Json, depth: usize) {
    let indent = "  ".repeat(depth + 1);
    let close = "  ".repeat(depth);
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Json::Str(text) => quote(out, text),
        Json::Array(items) if is_flat(value) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_value(out, item, depth);
            }
            out.push(']');
        }
        Json::Array(items) => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&indent);
                write_value(out, item, depth + 1);
                out.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&close);
            out.push(']');
        }
        Json::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Json::Object(entries) => {
            out.push_str("{\n");
            for (index, (key, item)) in entries.iter().enumerate() {
                out.push_str(&indent);
                quote(out, key);
                out.push_str(": ");
                write_value(out, item, depth + 1);
                out.push_str(if index + 1 < entries.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            out.push_str(&close);
            out.push('}');
        }
    }
}

/// A size: its fixed qwords and, for each length read from memory, the
/// qwords each unit of it adds.
fn qwords(extent: &Extent, structs: &[crate::sema::layout::StructDef], id: usize) -> Json {
    let per: Vec<Json> = extent
        .terms
        .iter()
        .map(|term| {
            object(vec![
                ("length", string(path_name(structs, id, &term.count))),
                ("qwords", Json::Int(i64::from(term.stride))),
            ])
        })
        .collect();
    let mut entries = vec![("fixed", Json::Int(i64::from(extent.fixed)))];
    if !per.is_empty() {
        entries.push(("per", Json::Array(per)));
    }
    object(entries)
}

/// The reflection as JSON text.
pub fn write(reflection: &Reflection) -> String {
    let structs: Vec<crate::sema::layout::StructDef> = reflection
        .structs
        .iter()
        .map(|info| info.def.clone())
        .collect();
    let names: Vec<String> = structs.iter().map(|def| def.name.clone()).collect();
    let program = object(vec![
        ("name", string(&reflection.program)),
        ("source", string(&reflection.source)),
        (
            "symbols",
            object(vec![
                ("start", string(format!("{}_CodeStart", reflection.program))),
                ("end", string(format!("{}_CodeEnd", reflection.program))),
            ]),
        ),
        (
            "instructions",
            reflection
                .instructions
                .map_or(Json::Null, |count| Json::Int(count as i64)),
        ),
    ]);
    let parameters: Vec<Json> = reflection
        .params
        .iter()
        .map(|param| {
            let ty = match param.struct_id {
                Some(id) => names[id].clone(),
                None => "int16".into(),
            };
            let placement = match param.placement {
                ParamPlacement::Address(address) => {
                    object(vec![("address", Json::Int(i64::from(address)))])
                }
                ParamPlacement::Xtop { vif: None } => object(vec![("xtop", Json::Bool(true))]),
                ParamPlacement::Xtop { vif: Some(vif) } => object(vec![
                    ("xtop", Json::Bool(true)),
                    ("vif_base", Json::Int(i64::from(vif.base))),
                    ("vif_offset", Json::Int(i64::from(vif.offset))),
                ]),
                ParamPlacement::Xitop => object(vec![("xitop", Json::Bool(true))]),
            };
            let mut entries = vec![
                ("name", string(&param.name)),
                ("qualifier", string(param.qualifier.spelling())),
                ("type", string(ty)),
                ("placement", placement),
            ];
            if param.spill {
                entries.push(("spill", Json::Bool(true)));
            }
            if param.library {
                entries.push(("library", Json::Bool(true)));
            }
            if let Some(id) = param.struct_id {
                entries.push(("qwords", qwords(&param.size, &structs, id)));
                entries.push(("reads", strings(&param.reads)));
                entries.push(("writes", strings(&param.writes)));
                if !param.kicks.is_empty() {
                    entries.push(("kicks", strings(&param.kicks)));
                }
            }
            if let Some(limit) = &param.limit {
                entries.push((
                    "limit",
                    object(vec![
                        ("length", string(&limit.count)),
                        ("max", Json::Int(i64::from(limit.max))),
                        ("capacity", Json::Int(i64::from(limit.capacity))),
                    ]),
                ));
            }
            object(entries)
        })
        .collect();
    let types: Vec<Json> = reflection
        .structs
        .iter()
        .enumerate()
        .map(|(id, info)| {
            let members: Vec<Json> = info
                .def
                .members
                .iter()
                .map(|member| {
                    let (element, length) = match &member.ty {
                        Type::Array(element, length) => ((**element).clone(), Some(length)),
                        ty => (ty.clone(), None),
                    };
                    let qword = if member.offset.is_fixed() {
                        Json::Int(i64::from(member.offset.fixed))
                    } else {
                        string(extent_text(&member.offset, &structs, id))
                    };
                    let type_name = match &element {
                        Type::Mat { size, .. } => format!("mat{size}"),
                        ty => ty.name(&names),
                    };
                    let mut entries = vec![
                        ("name", string(&member.name)),
                        ("type", string(type_name)),
                        ("qword", qword),
                    ];
                    if (element.vf_lanes().is_some() && element != Type::Qword)
                        || element == Type::Int16
                    {
                        entries.push(("lane", string(member.lane.letter().to_string())));
                    }
                    if let Type::Mat { row_major, .. } = element {
                        let order = if row_major {
                            "row_major"
                        } else {
                            "column_major"
                        };
                        entries.push(("order", string(order)));
                    }
                    if let Some(length) = length {
                        let length = match length {
                            Length::Fixed(n) => Json::Int(i64::from(*n)),
                            Length::Member(path) => string(path_name(&structs, id, path)),
                            Length::Xitop => string("xitop"),
                            Length::Open => string("open"),
                        };
                        entries.push(("length", length));
                        let stride = match &element {
                            Type::Struct(inner) => structs[*inner].size.fixed,
                            Type::Mat { size, .. } => u32::from(*size),
                            _ => 1,
                        };
                        entries.push(("stride", Json::Int(i64::from(stride))));
                    }
                    if let Some(unpack) = member.unpack {
                        entries.push((
                            "unpack",
                            object(vec![
                                ("format", string(unpack.name())),
                                ("unsigned", Json::Bool(unpack.unsigned)),
                            ]),
                        ));
                    }
                    object(entries)
                })
                .collect();
            let mut entries = vec![
                ("name", string(&info.def.name)),
                ("file", string(&info.file)),
                ("layout", string(&info.hash)),
                ("qwords", qwords(&info.def.size, &structs, id)),
            ];
            if let Some(open) = &info.def.open {
                entries.push((
                    "open",
                    object(vec![
                        ("member", string(path_name(&structs, id, &open.path))),
                        ("stride", Json::Int(i64::from(open.stride))),
                    ]),
                ));
            }
            entries.push(("members", Json::Array(members)));
            object(entries)
        })
        .collect();
    let root = object(vec![
        ("vusl", Json::Int(1)),
        ("program", program),
        (
            "uses",
            Json::Array(reflection.uses.iter().map(|name| string(*name)).collect()),
        ),
        ("parameters", Json::Array(parameters)),
        ("types", Json::Array(types)),
    ]);
    let root = match (&reflection.library, root) {
        (Some(library), Json::Object(mut entries)) => {
            let mut info = vec![
                ("name", string(&library.name)),
                ("address", Json::Int(i64::from(library.address))),
            ];
            if !library.exports.is_empty() {
                let exports = library
                    .exports
                    .iter()
                    .enumerate()
                    .map(|(index, export)| {
                        object(vec![
                            ("name", string(export)),
                            ("entry", Json::Int(i64::from(library.entry_address(index)))),
                        ])
                    })
                    .collect();
                info.push(("exports", Json::Array(exports)));
            }
            entries.insert(2, ("library".to_string(), object(info)));
            Json::Object(entries)
        }
        (_, root) => root,
    };
    let mut out = String::new();
    write_value(&mut out, &root, 0);
    out.push('\n');
    out
}
