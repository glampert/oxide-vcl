//! The reflection as a C++17 header (`--header=FILE`), for the runtime:
//! structs matching the VU layouts, checked with `static_assert`, and the
//! program's addresses, offsets, sizes and limits as `constexpr` values.
//!
//! Structs from included files sit in `vusl::layout_<hash>` behind include
//! guards keyed on their layout, so headers of programs that share them
//! can be included together; the program's own are in `vusl::<program>`.
//! A struct's C++ definition covers its members at fixed offsets: what
//! follows an array whose length is read at run time is given by offset
//! functions instead.

use std::fmt::Write;

use super::model::{ParamInfo, Reflection, StructInfo, extent_text, path_name};
use crate::sema::hir::ParamPlacement;
use crate::sema::layout::{Extent, StructDef};
use crate::sema::types::{Length, Type};

const VALUE_TYPES: &str = "\
// The VUSL value types, shared by every generated header.
#ifndef VUSL_TYPES_V1
#define VUSL_TYPES_V1
namespace vusl
{
#if defined(_EE)
using ::qword_t;
#else
typedef union alignas(16)
{
#if defined(__SIZEOF_INT128__)
    unsigned __int128 qw;
#endif
    std::uint8_t b[16];
    std::uint16_t hw[8];
    std::uint32_t sw[4];
    std::uint64_t dw[2];
} qword_t; // the layout of tamtypes.h's qword_t, for host tools
#endif

typedef std::int32_t int32;
typedef std::uint32_t uint32;
typedef std::int32_t int16_lane; // an int16 member: the VU reads the low 16 bits, stores them zero-extended

struct vec2 { float x, y; };
struct vec3 { float x, y, z; };
struct alignas(16) vec4 { float x, y, z, w; };
struct ivec2 { int32 x, y; };
struct ivec3 { int32 x, y, z; };
struct alignas(16) ivec4 { int32 x, y, z, w; };
struct uvec2 { uint32 x, y; };
struct uvec3 { uint32 x, y, z; };
struct alignas(16) uvec4 { uint32 x, y, z, w; };
struct alignas(16) mat3 { vec4 q[3]; }; // the stored qwords, xyz of each: columns, or rows when row_major
struct alignas(16) mat4 { vec4 q[4]; }; // the stored qwords: columns, or rows when row_major

// An array element of fewer than four lanes, which takes a qword.
template <typename T>
struct alignas(16) element
{
    T value;
};
} // namespace vusl
#endif
";

/// `particles` as `Particles`.
fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A dotted path as a C++ name: `packet.n` as `packet_n`.
fn identifier(path: &str) -> String {
    path.replace('.', "_")
}

/// The lengths an extent reads, as C++ parameters, and the expression.
fn extent_function(extent: &Extent, structs: &[StructDef], id: usize) -> (String, String) {
    let mut parameters: Vec<String> = Vec::new();
    for term in &extent.terms {
        let name = identifier(&path_name(structs, id, &term.count));
        let parameter = format!("std::uint32_t {name}");
        if !parameters.contains(&parameter) {
            parameters.push(parameter);
        }
    }
    let body = identifier_text(&extent_text(extent, structs, id));
    (parameters.join(", "), body)
}

fn identifier_text(text: &str) -> String {
    text.replace('.', "_")
}

/// How a struct is named where `in_program` code (or a shared struct's
/// own namespace, when false) refers to it.
fn struct_ref(infos: &[StructInfo], id: usize, in_program: bool) -> String {
    let info = &infos[id];
    if info.shared && !in_program {
        format!("::vusl::layout_{}::{}", info.hash, info.def.name)
    } else {
        info.def.name.clone()
    }
}

/// The C++ type of a lane or qword value.
fn value_type(ty: &Type, infos: &[StructInfo], in_program: bool) -> String {
    match ty {
        Type::Float => "float".into(),
        Type::Vec(n) => format!("vec{n}"),
        Type::Int16 => "int16_lane".into(),
        Type::Int32 => "int32".into(),
        Type::Uint32 => "uint32".into(),
        Type::IVec(n) => format!("ivec{n}"),
        Type::UVec(n) => format!("uvec{n}"),
        Type::Qword => "qword_t".into(),
        Type::Mat { size, .. } => format!("mat{size}"),
        Type::Struct(id) => struct_ref(infos, *id, in_program),
        _ => "qword_t".into(),
    }
}

/// The bytes a member at a fixed offset takes in its C++ struct.
fn member_bytes(ty: &Type, structs: &[StructDef]) -> u32 {
    match ty {
        Type::Float | Type::Int16 | Type::Int32 | Type::Uint32 => 4,
        Type::Vec(n) | Type::IVec(n) | Type::UVec(n) => 4 * u32::from(*n),
        Type::Qword => 16,
        Type::Mat { size, .. } => 16 * u32::from(*size),
        Type::Struct(id) => 16 * fixed_part(&structs[*id]),
        Type::Array(element, Length::Fixed(n)) => {
            let element = match **element {
                Type::Struct(id) => 16 * structs[id].size.fixed,
                Type::Mat { size, .. } => 16 * u32::from(size),
                _ => 16,
            };
            n * element
        }
        _ => 0,
    }
}

/// The qwords a struct's C++ definition covers: up to its first member at
/// a run-time offset or of run-time length.
fn fixed_part(def: &StructDef) -> u32 {
    for member in &def.members {
        if !member.offset.is_fixed() {
            return member.offset.fixed;
        }
        if matches!(
            member.ty,
            Type::Array(_, Length::Member(_) | Length::Xitop | Length::Open)
        ) {
            return member.offset.fixed;
        }
    }
    def.size.fixed
}

/// Whether a member ends the C++ definition: it or what follows it is at
/// a run-time place.
fn ends_definition(ty: &Type, structs: &[StructDef]) -> bool {
    match ty {
        Type::Struct(id) => {
            let def = &structs[*id];
            !def.size.is_fixed() || def.open.is_some()
        }
        _ => false,
    }
}

/// One struct's definition, its checks and its constants.
fn write_struct(
    out: &mut String,
    infos: &[StructInfo],
    structs: &[StructDef],
    id: usize,
    in_program: bool,
) {
    let def = &structs[id];
    let name = &def.name;
    let mut fields = Vec::new();
    let mut asserts = Vec::new();
    let mut at = 0u32;
    let mut pad = 0;
    for member in &def.members {
        if !member.offset.is_fixed()
            || matches!(
                member.ty,
                Type::Array(_, Length::Member(_) | Length::Xitop | Length::Open)
            )
        {
            break;
        }
        let start = member.offset.fixed * 16 + 4 * member.lane.index() as u32;
        if start > at {
            let lanes = (start - at) / 4;
            let field = if lanes == 1 {
                format!("uint32 _pad{pad};")
            } else {
                format!("uint32 _pad{pad}[{lanes}];")
            };
            fields.push((field, "unused".to_string()));
            pad += 1;
        }
        let lanes = match &member.ty {
            Type::Int16 => Some(1),
            Type::Qword => None,
            ty => ty.vf_lanes(),
        };
        let place = match (&member.ty, lanes) {
            (_, Some(lanes)) => {
                let letters: String = (0..lanes)
                    .filter_map(|lane| {
                        crate::sema::types::Lane::from_index(member.lane.index() + lane)
                    })
                    .map(|lane| lane.letter())
                    .collect();
                if lanes == 4 {
                    format!("qword {}", member.offset.fixed)
                } else {
                    format!("qword {}, {letters}", member.offset.fixed)
                }
            }
            (ty, None) => {
                let qwords = member_bytes(ty, structs) / 16;
                if qwords > 1 {
                    format!(
                        "qwords {}-{}",
                        member.offset.fixed,
                        member.offset.fixed + qwords - 1
                    )
                } else {
                    format!("qword {}", member.offset.fixed)
                }
            }
        };
        let mut note = place;
        if let Type::Mat {
            row_major: true, ..
        } = member.ty
        {
            note.push_str(", row_major");
        }
        if let Some(unpack) = member.unpack {
            let _ = write!(
                note,
                ", unpack {}{}",
                unpack.name(),
                if unpack.unsigned { " unsigned" } else { "" }
            );
        }
        let field = match &member.ty {
            Type::Array(element, Length::Fixed(n)) => {
                let element_type = value_type(element, infos, in_program);
                if element.vf_lanes().is_some_and(|lanes| lanes < 4) {
                    format!("element<{element_type}> {}[{n}];", member.name)
                } else {
                    format!("{element_type} {}[{n}];", member.name)
                }
            }
            ty => format!("{} {};", value_type(ty, infos, in_program), member.name),
        };
        fields.push((field, note));
        asserts.push((member.name.clone(), start));
        at = start + member_bytes(&member.ty, structs);
        if ends_definition(&member.ty, structs) {
            break;
        }
    }
    let _ = writeln!(out, "struct alignas(16) {name}\n{{");
    let width = fields
        .iter()
        .map(|(field, _)| field.len())
        .max()
        .unwrap_or(0);
    for (field, note) in &fields {
        let _ = writeln!(out, "    {field:width$} // {note}");
    }
    let _ = writeln!(out, "}};");
    let fixed = fixed_part(def).max(at.div_ceil(16));
    let _ = writeln!(
        out,
        "static_assert(sizeof({name}) == {} * 16, \"VUSL layout of {name}\");",
        fixed.max(1)
    );
    for (member, start) in asserts {
        if start == 0 {
            continue;
        }
        let bytes = match (start / 16, start % 16) {
            (0, lanes) => lanes.to_string(),
            (qwords, 0) => format!("{qwords} * 16"),
            (qwords, lanes) => format!("{qwords} * 16 + {lanes}"),
        };
        let _ = writeln!(
            out,
            "static_assert(offsetof({name}, {member}) == {bytes}, \"VUSL layout of {name}\");"
        );
    }
    // Offsets and sizes that C++ members cannot give.
    let mut constants = Vec::new();
    for member in &def.members {
        let dynamic_length = matches!(
            member.ty,
            Type::Array(_, Length::Member(_) | Length::Xitop | Length::Open)
        );
        let label = format!("{name}{}", capitalized(&member.name));
        if !member.offset.is_fixed() {
            let (parameters, body) = extent_function(&member.offset, structs, id);
            constants.push(format!(
                "constexpr std::uint32_t {label}({parameters}) {{ return {body}; }} // {name}::{}",
                member.name
            ));
        } else if dynamic_length {
            constants.push(format!(
                "constexpr std::uint32_t k{label} = {}; // {name}::{}",
                member.offset.fixed, member.name
            ));
        }
        if let Type::Array(element, length) = &member.ty
            && !matches!(length, Length::Fixed(_))
        {
            let stride = match **element {
                Type::Struct(inner) => structs[inner].size.fixed,
                Type::Mat { size, .. } => u32::from(size),
                _ => 1,
            };
            constants.push(format!(
                "constexpr std::uint32_t k{label}Stride = {stride}; // qwords per element"
            ));
        }
    }
    if def.size.is_fixed() {
        if !def.members.is_empty() && def.open.is_some() {
            constants.push(format!(
                "constexpr std::uint32_t k{name}Qwords = {}; // without its open array",
                def.size.fixed
            ));
        }
    } else {
        let (parameters, body) = extent_function(&def.size, structs, id);
        let note = if def.open.is_some() {
            " // without its open array"
        } else {
            ""
        };
        constants.push(format!(
            "constexpr std::uint32_t {name}Qwords({parameters}) {{ return {body}; }}{note}"
        ));
    }
    if !constants.is_empty() {
        let _ = writeln!(
            out,
            "// Qwords: offsets from the struct's start, and sizes."
        );
        for constant in constants {
            let _ = writeln!(out, "{constant}");
        }
    }
}

fn write_param(out: &mut String, param: &ParamInfo, structs: &[StructDef]) {
    let label = capitalized(&param.name);
    let ty = match param.struct_id {
        Some(id) => structs[id].name.clone(),
        None => "int16".into(),
    };
    let what = format!("{}: {} {ty}", param.name, param.qualifier.spelling());
    match param.placement {
        ParamPlacement::Address(address) => {
            let _ = writeln!(
                out,
                "constexpr std::uint32_t k{label}Address = {address}; // {what}"
            );
        }
        ParamPlacement::Xtop { vif: Some(vif) } => {
            let _ = writeln!(out, "// {what}, at XTOP in the VIF1 double buffer:");
            let _ = writeln!(
                out,
                "constexpr std::uint32_t k{label}VifBase = {};",
                vif.base
            );
            let _ = writeln!(
                out,
                "constexpr std::uint32_t k{label}VifOffset = {};",
                vif.offset
            );
        }
        ParamPlacement::Xtop { vif: None } => {
            let _ = writeln!(out, "// {what}, at XTOP");
        }
        ParamPlacement::Xitop => {
            let _ = writeln!(out, "// {what}, from XITOP");
        }
    }
    let Some(id) = param.struct_id else {
        return;
    };
    let def = &structs[id];
    let used_differs = param.size != def.size;
    let size_function = if used_differs {
        let (parameters, body) = extent_function(&param.size, structs, id);
        if param.size.is_fixed() {
            let _ = writeln!(
                out,
                "constexpr std::uint32_t k{label}UsedQwords = {body}; // with the open arrays main writes"
            );
            None
        } else {
            let _ = writeln!(
                out,
                "constexpr std::uint32_t {label}UsedQwords({parameters}) {{ return {body}; }} // with the open arrays main writes"
            );
            Some(format!("{label}UsedQwords"))
        }
    } else if def.size.is_fixed() {
        None
    } else {
        Some(format!("{}Qwords", def.name))
    };
    if let (Some(limit), Some(function)) = (&param.limit, size_function) {
        let constant = format!("k{label}Max{}", capitalized(&identifier(&limit.count)));
        let _ = writeln!(
            out,
            "constexpr std::uint32_t {constant} = {}; // the largest {} for which {}",
            limit.max, limit.count, limit.why
        );
        let _ = writeln!(
            out,
            "static_assert({function}({constant}) <= {}, \"VUSL limit\");",
            limit.capacity
        );
    }
}

/// The reflection as a C++ header.
pub fn write(reflection: &Reflection) -> String {
    let structs: Vec<StructDef> = reflection
        .structs
        .iter()
        .map(|info| info.def.clone())
        .collect();
    let program = &reflection.program;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Generated by oxide-vusl {} from {}. Do not edit.",
        env!("CARGO_PKG_VERSION"),
        reflection.source
    );
    out.push_str(
        "#pragma once\n\n#include <cstddef>\n#include <cstdint>\n#if defined(_EE)\n#include <tamtypes.h>\n#endif\n\n",
    );
    out.push_str(VALUE_TYPES);
    for (id, info) in reflection.structs.iter().enumerate() {
        if !info.shared {
            continue;
        }
        let guard = format!("VUSL_LAYOUT_{}_{}", info.def.name, info.hash);
        let _ = writeln!(
            out,
            "\n// {} ({}), shared by the programs that include it.",
            info.def.name, info.file
        );
        let _ = writeln!(
            out,
            "#ifndef {guard}\n#define {guard}\nnamespace vusl::layout_{}\n{{",
            info.hash
        );
        write_struct(&mut out, &reflection.structs, &structs, id, false);
        let _ = writeln!(out, "}} // namespace vusl::layout_{}\n#endif", info.hash);
    }
    let _ = writeln!(out, "\nnamespace vusl::{program}\n{{");
    for info in reflection.structs.iter().filter(|info| info.shared) {
        let _ = writeln!(
            out,
            "using {} = layout_{}::{};",
            info.def.name, info.hash, info.def.name
        );
    }
    for (id, info) in reflection.structs.iter().enumerate() {
        if info.shared {
            continue;
        }
        out.push('\n');
        write_struct(&mut out, &reflection.structs, &structs, id, true);
    }
    let _ = writeln!(
        out,
        "\n// main's parameters: qword addresses in VU1 data memory."
    );
    for param in &reflection.params {
        write_param(&mut out, param, &structs);
    }
    if let Some(instructions) = reflection.instructions {
        let _ = writeln!(
            out,
            "\nconstexpr std::uint32_t kInstructions = {instructions}; // 64-bit instructions in micro memory"
        );
    }
    let _ = writeln!(
        out,
        "}} // namespace vusl::{program}\n\n#ifdef VUSL_DECLARE_PROGRAM\nVUSL_DECLARE_PROGRAM({program})\n#endif"
    );
    out
}
