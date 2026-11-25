//! The syntax tree written back as source, with every operator's operands
//! in parentheses, so a test can see how the program was read.

use std::fmt::Write;

use super::ast::*;

/// The whole program.
pub fn file(file: &File) -> String {
    let mut out = String::new();
    for item in &file.items {
        match item {
            Item::Pragma(pragma_item) => {
                let _ = writeln!(out, "{}", pragma(pragma_item));
            }
            Item::Struct(decl) => {
                let _ = writeln!(out, "struct {} {{", decl.name.name);
                for member in &decl.members {
                    let names: Vec<String> = member.names.iter().map(declarator).collect();
                    let _ = writeln!(
                        out,
                        "    {}{} {};",
                        layout_prefix(&member.layout),
                        type_name(&member.ty),
                        names.join(", ")
                    );
                }
                out.push_str("};\n");
            }
            Item::Enum(decl) => {
                let underlying = decl
                    .underlying
                    .as_ref()
                    .map_or(String::new(), |ty| format!(" : {}", type_name(ty)));
                let _ = writeln!(out, "enum class {}{underlying} {{", decl.name.name);
                for value in &decl.values {
                    match &value.value {
                        Some(written) => {
                            let _ = writeln!(out, "    {} = {},", value.name.name, expr(written));
                        }
                        None => {
                            let _ = writeln!(out, "    {},", value.name.name);
                        }
                    }
                }
                out.push_str("};\n");
            }
            Item::Alias(alias) => {
                let _ = writeln!(out, "type {} = {};", alias.name.name, type_name(&alias.ty));
            }
            Item::Const(decl) => {
                let _ = writeln!(
                    out,
                    "const {} {} = {};",
                    type_name(&decl.ty),
                    decl.name.name,
                    expr(&decl.value)
                );
            }
            Item::Function(function) => {
                let params: Vec<String> = function.params.iter().map(param).collect();
                let _ = write!(
                    out,
                    "{}{} {}({})",
                    if function.export { "export " } else { "" },
                    type_name(&function.ret),
                    function.name.name,
                    params.join(", ")
                );
                if function.prototype {
                    out.push_str(";\n");
                } else {
                    out.push('\n');
                    block(&mut out, &function.body, 0);
                    out.push('\n');
                }
            }
            Item::Memory(memory) => {
                let _ = writeln!(out, "{};", param(memory));
            }
        }
    }
    out
}

fn pragma(pragma: &Pragma) -> String {
    match &pragma.args {
        None => format!("#pragma {}", pragma.name.name),
        Some(args) => format!("#pragma {}({})", pragma.name.name, arguments(args)),
    }
}

fn arguments(args: &[Arg]) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|arg| match &arg.name {
            Some(name) => format!("{} = {}", name.name, expr(&arg.value)),
            None => expr(&arg.value),
        })
        .collect();
    args.join(", ")
}

fn layout_prefix(layout: &Option<Layout>) -> String {
    match layout {
        Some(layout) => format!("layout({}) ", arguments(&layout.args)),
        None => String::new(),
    }
}

/// A type as written.
pub fn type_name(ty: &TypeName) -> String {
    match &ty.argument {
        Some(argument) => format!("{}<{}>", ty.name.name, type_name(argument)),
        None => ty.name.name.clone(),
    }
}

fn declarator(declarator: &Declarator) -> String {
    let mut text = declarator.name.name.clone();
    match &declarator.array {
        Some(ArrayLen::Open) => text.push_str("[]"),
        Some(ArrayLen::Expr(length)) => {
            let _ = write!(text, "[{}]", expr(length));
        }
        None => {}
    }
    if let Some(init) = &declarator.init {
        let _ = write!(text, " = {}", expr(init));
    }
    text
}

fn qualifier_prefix(qualifier: &Option<(Qualifier, super::Pos)>) -> String {
    match qualifier {
        Some((qualifier, _)) => format!("{} ", qualifier.spelling()),
        None => String::new(),
    }
}

fn param(param: &Param) -> String {
    format!(
        "{}{}{} {}",
        layout_prefix(&param.layout),
        qualifier_prefix(&param.qualifier),
        type_name(&param.ty),
        param.name.name
    )
}

fn block(out: &mut String, block: &Block, depth: usize) {
    let indent = "    ".repeat(depth);
    let _ = writeln!(out, "{indent}{{");
    for statement in &block.stmts {
        stmt(out, statement, depth + 1);
    }
    let _ = write!(out, "{indent}}}");
}

fn decl(decl: &Decl) -> String {
    let declarators: Vec<String> = decl.declarators.iter().map(declarator).collect();
    format!(
        "{}{}{}{} {}",
        layout_prefix(&decl.layout),
        if decl.constant { "const " } else { "" },
        if decl.precise { "precise " } else { "" },
        type_name(&decl.ty),
        declarators.join(", ")
    )
}

/// A statement on its own lines, at `depth` levels of indentation.
pub fn stmt(out: &mut String, statement: &Stmt, depth: usize) {
    let indent = "    ".repeat(depth);
    match &statement.kind {
        StmtKind::Block(inner) => {
            block(out, inner, depth);
            out.push('\n');
        }
        StmtKind::Pragma(inner) => {
            let _ = writeln!(out, "{indent}{}", pragma(inner));
        }
        StmtKind::Decl(inner) => {
            let _ = writeln!(out, "{indent}{};", decl(inner));
        }
        StmtKind::Expr(inner) => {
            let _ = writeln!(out, "{indent}{};", expr(inner));
        }
        StmtKind::If {
            cond,
            then,
            otherwise,
        } => {
            let _ = writeln!(out, "{indent}if ({})", expr(cond));
            stmt(out, then, depth + 1);
            if let Some(otherwise) = otherwise {
                let _ = writeln!(out, "{indent}else");
                stmt(out, otherwise, depth + 1);
            }
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            let init = match init.as_deref() {
                Some(Stmt {
                    kind: StmtKind::Decl(inner),
                    ..
                }) => decl(inner),
                Some(Stmt {
                    kind: StmtKind::Expr(inner),
                    ..
                }) => expr(inner),
                _ => String::new(),
            };
            let cond = cond.as_ref().map(expr).unwrap_or_default();
            let step = step.as_ref().map(expr).unwrap_or_default();
            let _ = writeln!(out, "{indent}for ({init}; {cond}; {step})");
            stmt(out, body, depth + 1);
        }
        StmtKind::RangeFor { ranges, body } => {
            let ranges: Vec<String> = ranges
                .iter()
                .map(|range| {
                    format!(
                        "{}{} {} : {}",
                        qualifier_prefix(&range.qualifier),
                        type_name(&range.ty),
                        range.name.name,
                        expr(&range.source)
                    )
                })
                .collect();
            let _ = writeln!(out, "{indent}for ({})", ranges.join(", "));
            stmt(out, body, depth + 1);
        }
        StmtKind::While { cond, body } => {
            let _ = writeln!(out, "{indent}while ({})", expr(cond));
            stmt(out, body, depth + 1);
        }
        StmtKind::DoWhile { body, cond } => {
            let _ = writeln!(out, "{indent}do");
            stmt(out, body, depth + 1);
            let _ = writeln!(out, "{indent}while ({});", expr(cond));
        }
        StmtKind::Switch { value, cases } => {
            let _ = writeln!(out, "{indent}switch ({})\n{indent}{{", expr(value));
            for case in cases {
                for label in &case.labels {
                    match label {
                        CaseLabel::Value(value) => {
                            let _ = writeln!(out, "{indent}case {}:", expr(value));
                        }
                        CaseLabel::Default(_) => {
                            let _ = writeln!(out, "{indent}default:");
                        }
                    }
                }
                for inner in &case.body {
                    stmt(out, inner, depth + 1);
                }
            }
            let _ = writeln!(out, "{indent}}}");
        }
        StmtKind::Break => {
            let _ = writeln!(out, "{indent}break;");
        }
        StmtKind::Continue => {
            let _ = writeln!(out, "{indent}continue;");
        }
        StmtKind::Return(value) => match value {
            Some(value) => {
                let _ = writeln!(out, "{indent}return {};", expr(value));
            }
            None => {
                let _ = writeln!(out, "{indent}return;");
            }
        },
        StmtKind::Yield => {
            let _ = writeln!(out, "{indent}yield;");
        }
        StmtKind::Empty => {
            let _ = writeln!(out, "{indent};");
        }
    }
}

/// An expression with its operators' operands in parentheses.
pub fn expr(expression: &Expr) -> String {
    match &expression.kind {
        ExprKind::Int(value) => value.to_string(),
        ExprKind::Float(value) => format!("{value:?}"),
        ExprKind::Bool(value) => value.to_string(),
        ExprKind::Name(name) => name.name.clone(),
        ExprKind::Scoped { scope, name } => format!("{}::{}", scope.name, name.name),
        ExprKind::Unary { op, operand } => {
            let op = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Plus => "+",
                UnaryOp::Not => "!",
                UnaryOp::BitNot => "~",
                UnaryOp::PreIncrement => "++",
                UnaryOp::PreDecrement => "--",
            };
            format!("({op}{})", expr(operand))
        }
        ExprKind::Postfix { op, operand } => {
            let op = match op {
                PostfixOp::Increment => "++",
                PostfixOp::Decrement => "--",
            };
            format!("({}{op})", expr(operand))
        }
        ExprKind::Binary { op, lhs, rhs, .. } => {
            format!("({} {} {})", expr(lhs), op.spelling(), expr(rhs))
        }
        ExprKind::Assign { op, target, value } => {
            let op = match op {
                AssignOp::Assign => "=".to_string(),
                AssignOp::Compound(op) => format!("{}=", op.spelling()),
            };
            format!("({} {op} {})", expr(target), expr(value))
        }
        ExprKind::Conditional {
            cond,
            then,
            otherwise,
            ..
        } => format!("({} ? {} : {})", expr(cond), expr(then), expr(otherwise)),
        ExprKind::Call { callee, args } => {
            let args: Vec<String> = args.iter().map(expr).collect();
            format!("{}({})", type_name(callee), args.join(", "))
        }
        ExprKind::Field { base, field } => format!("{}.{}", expr(base), field.name),
        ExprKind::Index { base, index } => format!("{}[{}]", expr(base), expr(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::SyntaxFailure;
    use super::super::parse;
    use super::super::parser::SyntaxErrorKind;

    fn round_trip(source: &str) -> String {
        super::file(&parse(source).unwrap())
    }

    fn failure(source: &str) -> SyntaxErrorKind {
        match parse(source).unwrap_err() {
            SyntaxFailure::Parse(error) => error.kind,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn operators_take_cs_precedence() {
        let program = "void main()\n\
                       {\n\
                       \x20   a = b + c * d - -e;\n\
                       \x20   f += g < h && i == j || !k;\n\
                       \x20   m = n.xyz * o[2].w + p(q, r)++;\n\
                       \x20   s = t ? u : v ? w : x;\n\
                       }\n";
        assert_eq!(
            round_trip(program),
            "void main()\n\
             {\n\
             \x20   (a = ((b + (c * d)) - (-e)));\n\
             \x20   (f += (((g < h) && (i == j)) || (!k)));\n\
             \x20   (m = ((n.xyz * o[2].w) + (p(q, r)++)));\n\
             \x20   (s = (t ? u : (v ? w : x)));\n\
             }\n"
        );
    }

    #[test]
    fn declarations_start_with_a_type_and_a_name() {
        let program = "void main()\n\
                       {\n\
                       \x20   vec4 c = a, d;\n\
                       \x20   c = a;\n\
                       \x20   const precise float k = 2.0;\n\
                       \x20   layout(lane = x) vec2 st = uv;\n\
                       \x20   ref<Vertex> r = ref<Vertex>(base);\n\
                       \x20   float table[4];\n\
                       }\n";
        assert_eq!(
            round_trip(program),
            "void main()\n\
             {\n\
             \x20   vec4 c = a, d;\n\
             \x20   (c = a);\n\
             \x20   const precise float k = 2.0;\n\
             \x20   layout(lane = x) vec2 st = uv;\n\
             \x20   ref<Vertex> r = ref<Vertex>(base);\n\
             \x20   float table[4];\n\
             }\n"
        );
    }

    #[test]
    fn structs_parameters_and_pragmas() {
        let program = "#pragma program(particles)\n\
                       struct Batch {\n\
                       \x20   layout(lane = w) int16 count;\n\
                       \x20   vec4 uv0, uv1;\n\
                       \x20   Particle particles[count];\n\
                       \x20   Sprite sprites[];\n\
                       };\n\
                       const float kScale = 2.0;\n\
                       void main(layout(address = 0) uniform Frame frame, layout(xtop, vif_base = 8) inout Batch batch)\n\
                       {\n\
                       \x20   #pragma pipeline(min = 1, safe = 2)\n\
                       \x20   for (Particle p : batch.particles, out Sprite s : batch.sprites)\n\
                       \x20       s.uv0 = p.uv;\n\
                       \x20   #pragma unroll\n\
                       \x20   for (int16 r = 0; r < 4; ++r) { }\n\
                       \x20   for (;;) break;\n\
                       \x20   do yield; while (true);\n\
                       \x20   if (a) return; else { }\n\
                       }\n";
        assert_eq!(
            round_trip(program),
            "#pragma program(particles)\n\
             struct Batch {\n\
             \x20   layout(lane = w) int16 count;\n\
             \x20   vec4 uv0, uv1;\n\
             \x20   Particle particles[count];\n\
             \x20   Sprite sprites[];\n\
             };\n\
             const float kScale = 2.0;\n\
             void main(layout(address = 0) uniform Frame frame, layout(xtop, vif_base = 8) inout Batch batch)\n\
             {\n\
             \x20   #pragma pipeline(min = 1, safe = 2)\n\
             \x20   for (Particle p : batch.particles, out Sprite s : batch.sprites)\n\
             \x20       (s.uv0 = p.uv);\n\
             \x20   #pragma unroll\n\
             \x20   for (int16 r = 0; (r < 4); (++r))\n\
             \x20       {\n\
             \x20       }\n\
             \x20   for (; ; )\n\
             \x20       break;\n\
             \x20   do\n\
             \x20       yield;\n\
             \x20   while (true);\n\
             \x20   if (a)\n\
             \x20       return;\n\
             \x20   else\n\
             \x20       {\n\
             \x20       }\n\
             }\n"
        );
    }

    #[test]
    fn enums_and_aliases_round_trip() {
        let program = "enum class ClipPlane { Near, Left = 1 + 1, Right, };\n\
                       enum class Mode : int16 { Fill = -1 };\n\
                       type Cursor = ref<Corner>;\n\
                       struct Batch { int16 type; };\n\
                       void main() { if (plane == ClipPlane::Near) a = Mode(3); }\n";
        assert_eq!(
            round_trip(program),
            "enum class ClipPlane {\n\
             \x20   Near,\n\
             \x20   Left = (1 + 1),\n\
             \x20   Right,\n\
             };\n\
             enum class Mode : int16 {\n\
             \x20   Fill = (-1),\n\
             };\n\
             type Cursor = ref<Corner>;\n\
             struct Batch {\n\
             \x20   int16 type;\n\
             };\n\
             void main()\n\
             {\n\
             \x20   if ((plane == ClipPlane::Near))\n\
             \x20       (a = Mode(3));\n\
             }\n"
        );
        assert_eq!(
            failure("enum Plane { Near };"),
            SyntaxErrorKind::UnscopedEnum
        );
    }

    #[test]
    fn switches_round_trip() {
        let program = "void main() { switch (mode) { case 0: case Mode::Fill: a = 1; break;\n\
                       default: { b = 2; break; } } }\n";
        assert_eq!(
            round_trip(program),
            "void main()\n\
             {\n\
             \x20   switch (mode)\n\
             \x20   {\n\
             \x20   case 0:\n\
             \x20   case Mode::Fill:\n\
             \x20       (a = 1);\n\
             \x20       break;\n\
             \x20   default:\n\
             \x20       {\n\
             \x20           (b = 2);\n\
             \x20           break;\n\
             \x20       }\n\
             \x20   }\n\
             }\n"
        );
        assert_eq!(
            failure("void main() { switch (a) { b = 1; } }"),
            SyntaxErrorKind::Expected {
                expected: "'case' or 'default'".into(),
                found: "'b'".into()
            }
        );
    }

    #[test]
    fn errors_say_what_was_expected() {
        assert_eq!(
            failure("void main() { a = b }"),
            SyntaxErrorKind::Expected {
                expected: "';'".into(),
                found: "'}'".into()
            }
        );
        assert_eq!(
            failure("float scale;"),
            SyntaxErrorKind::GlobalVariable("scale".into())
        );
        assert_eq!(
            failure("void main() { float in = 1.0; }"),
            SyntaxErrorKind::KeywordAsName("in".into())
        );
        assert_eq!(
            failure("void main() { a.b(c); }"),
            SyntaxErrorKind::NotCallable
        );
        assert_eq!(
            failure("void main() {"),
            SyntaxErrorKind::Expected {
                expected: "'}'".into(),
                found: "the end of the program".into()
            }
        );
        assert_eq!(
            failure("#pragma unroll(4) x\nvoid main() {}"),
            SyntaxErrorKind::Expected {
                expected: "the end of the pragma's line".into(),
                found: "'x'".into()
            }
        );
    }
}
