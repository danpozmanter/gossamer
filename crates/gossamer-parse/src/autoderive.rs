//! Compile-time codegen for serialization (serde / kotlinx shape).
//! For every user struct whose fields the synthesizer can classify
//! (primitives, growable arrays of supported types, nested user
//! structs), we synthesize per-type free functions
//! `__gos_serde_to_json_<T>` / `__gos_serde_from_json_<T>` (plus the
//! toml / yaml variants) as real Gossamer source, parse it, and merge
//! it into the program. The public surface is the generic call form
//! `to_json::<T>(value)` / `from_json::<T>(text)`, which
//! `rewrite_serde_generic_calls` rewrites into those names. There are
//! no `Type::to_json` methods - one spelling only.
//!
//! Because the synthesized functions are ordinary Gossamer code, they
//! compile through every tier (VM + Cranelift + LLVM) automatically.
//! There is no VM-only intercept; no runtime schema registry; no
//! per-call dispatch overhead.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use gossamer_ast::{
    EnumDecl, EnumVariant, GenericArg, Item, ItemKind, ModBody, ModulePath, NodeId, SourceFile,
    StructBody, StructDecl, TypeKind, UseDecl, UseTarget,
};
use gossamer_lex::{FileId, Keyword, Lexer, Punct, SourceMap, Span, TokenKind};

use crate::ParseDiagnostic;

/// Classification of a struct field for the synthesizer. Anything
/// outside this set causes the struct to be skipped - we don't want
/// to emit code that won't compile.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FieldKind {
    /// Any signed or unsigned narrow integer (i8/i16/i32/u8/u16/u32),
    /// stored as its source-level spelling. JSON round-trips through
    /// `i64` then casts back at extract time.
    Int(&'static str),
    I64,
    F64,
    Bool,
    String,
    /// `[T]` / `Vec<T>` of any supported kind.
    Vec(Box<FieldKind>),
    /// Nested user struct, referenced by source-level name.
    Struct(String),
    /// `Option<T>` - JSON `null` for `None`, else the inner value. A missing
    /// object key also decodes to `None`.
    Option(Box<FieldKind>),
    /// A tuple `(A, B, ...)` - a JSON array of heterogeneous elements.
    Tuple(Vec<FieldKind>),
    /// `HashMap<String, V>` - a JSON object. Keys are sorted on encode so the
    /// text is deterministic across tiers.
    Map(Box<FieldKind>),
    /// `json::Value` - a dynamic JSON document, passed through unchanged.
    Json,
}

impl FieldKind {
    fn from_type(ty: &gossamer_ast::Type, structs: &HashSet<String>) -> Option<Self> {
        // A generic argument that must itself be a supported field kind.
        let arg_kind = |g: &GenericArg, structs: &HashSet<String>| -> Option<Self> {
            let GenericArg::Type(inner) = g else {
                return None;
            };
            Self::from_type(inner, structs)
        };
        match &ty.kind {
            TypeKind::Path(path) => {
                // `json::Value` (any path ending in `json::Value`) is a dynamic
                // JSON document, serialized by pass-through.
                let segs = &path.segments;
                if segs.len() >= 2
                    && segs[segs.len() - 1].name.name == "Value"
                    && segs[segs.len() - 2].name.name == "json"
                {
                    return Some(Self::Json);
                }
                if segs.len() != 1 {
                    return None;
                }
                let seg = &segs[0];
                let name = seg.name.name.as_str();
                if seg.generics.is_empty() {
                    return match name {
                        "i64" => Some(Self::I64),
                        "i8" => Some(Self::Int("i8")),
                        "i16" => Some(Self::Int("i16")),
                        "i32" => Some(Self::Int("i32")),
                        "u8" => Some(Self::Int("u8")),
                        "u16" => Some(Self::Int("u16")),
                        "u32" => Some(Self::Int("u32")),
                        "f64" => Some(Self::F64),
                        "f32" => Some(Self::F64),
                        "bool" => Some(Self::Bool),
                        "String" => Some(Self::String),
                        other if structs.contains(other) => Some(Self::Struct(other.to_string())),
                        _ => None,
                    };
                }
                match name {
                    "Vec" if seg.generics.len() == 1 => {
                        Some(Self::Vec(Box::new(arg_kind(&seg.generics[0], structs)?)))
                    }
                    "Option" if seg.generics.len() == 1 => {
                        Some(Self::Option(Box::new(arg_kind(&seg.generics[0], structs)?)))
                    }
                    "HashMap" if seg.generics.len() == 2 => {
                        // Only `String`-keyed maps map cleanly to a JSON object.
                        let GenericArg::Type(key) = &seg.generics[0] else {
                            return None;
                        };
                        let string_key = matches!(
                            &key.kind,
                            TypeKind::Path(kp)
                                if kp.segments.len() == 1 && kp.segments[0].name.name == "String"
                        );
                        if !string_key {
                            return None;
                        }
                        Some(Self::Map(Box::new(arg_kind(&seg.generics[1], structs)?)))
                    }
                    _ => None,
                }
            }
            TypeKind::Slice(inner) => Some(Self::Vec(Box::new(Self::from_type(inner, structs)?))),
            TypeKind::Tuple(elems) => {
                let mut kinds = Vec::with_capacity(elems.len());
                for e in elems {
                    kinds.push(Self::from_type(e, structs)?);
                }
                Some(Self::Tuple(kinds))
            }
            _ => None,
        }
    }

    /// Source-level expression for this field's `Default::default()`
    /// value, used by `#[derive(Default)]` synthesis.
    fn default_literal(&self) -> String {
        match self {
            Self::I64 | Self::Int(_) => "0".to_string(),
            Self::F64 => "0.0".to_string(),
            Self::Bool => "false".to_string(),
            Self::String => "\"\"".to_string(),
            // Empty literal; the struct field's declared type pins the element.
            Self::Vec(_) => "[]".to_string(),
            Self::Struct(name) => format!("{name}::default()"),
            Self::Option(_) => "None".to_string(),
            Self::Tuple(elems) => format!(
                "({})",
                elems
                    .iter()
                    .map(Self::default_literal)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Map(_) => "HashMap::new()".to_string(),
            Self::Json => "json::Value::Null".to_string(),
        }
    }

    /// Source-level type spelling for a `let mut acc: ... = []`
    /// declaration used while accumulating a Vec field.
    fn type_spelling(&self) -> String {
        match self {
            Self::I64 => "i64".to_string(),
            Self::Int(name) => (*name).to_string(),
            Self::F64 => "f64".to_string(),
            Self::Bool => "bool".to_string(),
            Self::String => "String".to_string(),
            Self::Vec(inner) => format!("[{}]", inner.type_spelling()),
            Self::Struct(name) => name.clone(),
            Self::Option(inner) => format!("Option<{}>", inner.type_spelling()),
            Self::Tuple(elems) => format!(
                "({})",
                elems
                    .iter()
                    .map(Self::type_spelling)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Map(inner) => format!("HashMap<String, {}>", inner.type_spelling()),
            Self::Json => "json::Value".to_string(),
        }
    }

    /// Gossamer source fragment that renders a single value bound to
    /// `expr` to JSON-syntax text. Returns an expression of type
    /// `String` - or, for nested structs, a `?`-propagating call.
    fn render_to_json(&self, expr: &str) -> String {
        match self {
            Self::I64 | Self::Int(_) | Self::F64 | Self::Bool => {
                format!("format!(\"{{}}\", {expr})")
            }
            Self::String => format!("format!(\"\\\"{{}}\\\"\", &{expr})"),
            Self::Vec(inner) => render_vec_to_json(expr, inner),
            Self::Struct(name) => format!("{}({expr})?", to_json_fn(name)),
            Self::Option(inner) => {
                let some_render = inner.render_to_json("__inner");
                format!(
                    "match {expr} {{ Some(__inner) => {some_render}, None => \"null\".to_string() }}"
                )
            }
            Self::Tuple(elems) => render_tuple_to_json(expr, elems),
            Self::Map(inner) => render_map_to_json(expr, inner),
            Self::Json => format!("json::render({expr})"),
        }
    }

    /// Gossamer source for a strict match-extract block. `value_expr`
    /// is the `json::Value` bound name; the block evaluates to the
    /// typed Gossamer value or `return`s a structured `errors::Error`
    /// with `path` (e.g. "field `tags`: ...") on mismatch.
    fn extract_strict(&self, value_expr: &str, path: &str) -> String {
        match self {
            Self::I64 => format!(
                "match json::as_i64({value_expr}) {{ Some(__v) => __v, None => return Err(errors::new(\"{path}: expected i64\")) }}"
            ),
            Self::Int(width) => format!(
                "match json::as_i64({value_expr}) {{ Some(__v) => __v as {width}, None => return Err(errors::new(\"{path}: expected {width}\")) }}"
            ),
            Self::F64 => format!(
                "match json::as_f64({value_expr}) {{ Some(__v) => __v, None => return Err(errors::new(\"{path}: expected f64\")) }}"
            ),
            Self::Bool => format!(
                "{{ let __rendered = json::render({value_expr})\n            if __rendered == \"true\" {{ true }} else if __rendered == \"false\" {{ false }} else {{ return Err(errors::new(\"{path}: expected bool\")) }} }}"
            ),
            Self::String => format!(
                "match json::as_str({value_expr}) {{ Some(__v) => __v, None => return Err(errors::new(\"{path}: expected string\")) }}"
            ),
            Self::Vec(inner) => extract_vec_strict(value_expr, inner, path),
            Self::Struct(name) => format!(
                "match {}(&json::render({value_expr})) {{ Ok(__v) => __v, Err(__e) => return Err(errors::wrap(__e, \"{path}\")) }}",
                from_json_fn(name)
            ),
            Self::Option(inner) => {
                let some_extract = inner.extract_strict(value_expr, path);
                format!("if json::is_null({value_expr}) {{ None }} else {{ Some({some_extract}) }}")
            }
            Self::Tuple(elems) => extract_tuple_strict(value_expr, elems, path),
            Self::Map(inner) => extract_map_strict(value_expr, inner, path),
            // The field already holds a parsed dynamic JSON value.
            Self::Json => value_expr.to_string(),
        }
    }

    /// `true` if this kind decodes a missing object key to a value (rather than
    /// erroring). Only `Option` does - a missing optional field is `None`.
    fn tolerates_missing_key(&self) -> bool {
        matches!(self, Self::Option(_))
    }
}

/// Mangled name of the synthesized serializer free function for a
/// given operation and type. The public surface is the generic call
/// form `to_json::<T>(v)` / `from_json::<T>(s)`, which the parse-time
/// `rewrite_serde_generic_calls` pass rewrites into these names.
pub(crate) fn serde_fn(op: &str, ty: &str) -> String {
    format!("__gos_serde_{op}_{ty}")
}
fn to_json_fn(ty: &str) -> String {
    serde_fn("to_json", ty)
}
fn from_json_fn(ty: &str) -> String {
    serde_fn("from_json", ty)
}

fn render_vec_to_json(expr: &str, inner: &FieldKind) -> String {
    // Inline a tiny per-Vec assembly. We emit a block expression so
    // the surrounding `out += ...` lands a single concatenated String.
    let elem_render = inner.render_to_json("__item");
    format!(
        "{{ let mut __buf = \"[\".to_string()\n            let mut __first = true\n            for __item in {expr} {{\n                if !__first {{ __buf += \",\" }}\n                __first = false\n                __buf += {elem_render}\n            }}\n            __buf += \"]\"\n            __buf }}"
    )
}

fn extract_vec_strict(value_expr: &str, inner: &FieldKind, path: &str) -> String {
    let elem_path = format!("{path}[index]");
    let inner_extract = inner.extract_strict("__elem", &elem_path);
    let elem_ty = inner.type_spelling();
    format!(
        "match json::as_array({value_expr}) {{\n                Some(__arr) => {{\n                    let mut __out: [{elem_ty}] = []\n                    for __elem in __arr {{\n                        let __converted = {inner_extract}\n                        __out.push(__converted)\n                    }}\n                    __out\n                }}\n                None => return Err(errors::new(\"{path}: expected array\")),\n            }}"
    )
}

fn render_tuple_to_json(expr: &str, elems: &[FieldKind]) -> String {
    let mut out = String::from("{ let mut __buf = \"[\".to_string()\n");
    for (i, k) in elems.iter().enumerate() {
        if i > 0 {
            out.push_str("            __buf += \",\"\n");
        }
        let er = k.render_to_json(&format!("{expr}.{i}"));
        out.push_str(&format!("            __buf += {er}\n"));
    }
    out.push_str("            __buf += \"]\"\n            __buf }");
    out
}

fn extract_tuple_strict(value_expr: &str, elems: &[FieldKind], path: &str) -> String {
    let n = elems.len();
    let mut out = format!(
        "match json::as_array({value_expr}) {{\n                Some(__arr) => {{\n                    if __arr.len() != {n} {{ return Err(errors::new(\"{path}: expected {n}-element array\")) }}\n"
    );
    let mut names = Vec::with_capacity(n);
    for (i, k) in elems.iter().enumerate() {
        let ex = k.extract_strict(&format!("__arr[{i}]"), &format!("{path}.{i}"));
        out.push_str(&format!("                    let __e{i} = {ex}\n"));
        names.push(format!("__e{i}"));
    }
    out.push_str(&format!("                    ({})\n", names.join(", ")));
    out.push_str(&format!(
        "                }}\n                None => return Err(errors::new(\"{path}: expected array\")),\n            }}"
    ));
    out
}

fn render_map_to_json(expr: &str, inner: &FieldKind) -> String {
    // Sort keys so the object text is deterministic across tiers (a HashMap's
    // iteration order is not stable and differs interp-vs-compiled).
    let vr = inner.render_to_json("__v");
    format!(
        "{{ let mut __ks = {expr}.keys()\n            __ks.sort()\n            let mut __buf = \"{{\".to_string()\n            let mut __first = true\n            for __k in __ks {{\n                if !__first {{ __buf += \",\" }}\n                __first = false\n                __buf += format!(\"\\\"{{}}\\\":\", __k)\n                if let Some(__v) = {expr}.get(&__k) {{ __buf += {vr} }}\n            }}\n            __buf += \"}}\"\n            __buf }}"
    )
}

fn extract_map_strict(value_expr: &str, inner: &FieldKind, path: &str) -> String {
    // Unique bind names (`__map*`) so nested maps and the enclosing field's own
    // `__child` binding never collide - the compiled tier resolves shadows
    // differently from the VM, so an ambiguous reuse silently miscompiles.
    let vt = inner.type_spelling();
    let ve = inner.extract_strict("__mapval", &format!("{path}[key]"));
    format!(
        "match json::keys({value_expr}) {{\n                Some(__mapkeys) => {{\n                    let mut __map: HashMap<String, {vt}> = HashMap::new()\n                    for __mapk in __mapkeys {{\n                        let __mapval = match json::get({value_expr}, &__mapk) {{ Some(__mc) => __mc, None => return Err(errors::new(\"{path}: missing key\")) }}\n                        let __mapentry = {ve}\n                        __map.insert(__mapk, __mapentry)\n                    }}\n                    __map\n                }}\n                None => return Err(errors::new(\"{path}: expected object\")),\n            }}"
    )
}

/// Mangled name of the synthesized field-reflection function for a
/// struct, reached via `typeInfo::<Type>()`.
#[must_use]
pub(crate) fn type_info_fn(ty: &str) -> String {
    format!("__gos_typeinfo_{ty}")
}

/// Synthesizes the `comptime fn` backers for the compile-time macros:
/// the `regex!("…")` / `sql!("…")` validators, and the `codegen!(…)`
/// source-emitter. Emitted only when the source uses the matching macro,
/// so programs that use none carry no extra items. Each validator returns
/// its input on success and `panic!`s on malformed input - a comptime
/// panic fails the build. `__gos_codegen` is the identity passthrough the
/// comptime pass keys on to splice a result as raw source rather than as a
/// quoted literal.
fn synthesize_validators(source: &str) -> String {
    let mut out = String::new();
    if source.contains("codegen!") {
        out.push_str("comptime fn __gos_codegen(__src: String) -> String { __src }\n");
    }
    if source.contains("regex!") {
        out.push_str(
            "comptime fn __gos_regex_validate(p: String) -> String {\n\
             \tmatch regex::compile(&p) {\n\
             \t\tOk(_) => p,\n\
             \t\tErr(__e) => panic!(\"invalid regex `{}`: {}\", p, __e),\n\
             \t}\n\
             }\n",
        );
    }
    if source.contains("sql!") {
        out.push_str(
            "comptime fn __gos_sql_validate(q: String) -> String {\n\
             \tif q.len() == 0 { panic!(\"empty SQL statement\") }\n\
             \tlet mut depth = 0\n\
             \tlet mut i = 0\n\
             \twhile i < q.len() {\n\
             \t\tlet b = q.byte_at(i)\n\
             \t\tif b == 40 { depth += 1 }\n\
             \t\tif b == 41 { depth -= 1 }\n\
             \t\tif depth < 0 { panic!(\"unbalanced parentheses in SQL: {}\", q) }\n\
             \t\ti += 1\n\
             \t}\n\
             \tif depth != 0 { panic!(\"unbalanced parentheses in SQL: {}\", q) }\n\
             \tq\n\
             }\n",
        );
    }
    out
}

/// Renders an AST type as a compact source-like string for reflection
/// (`typeInfo`). Falls back to the leaf path segment for shapes the
/// renderer does not special-case.
fn ty_to_string(ty: &gossamer_ast::ty::Type) -> String {
    use gossamer_ast::ty::TypeKind;
    match &ty.kind {
        TypeKind::Unit => "()".to_string(),
        TypeKind::Slice(inner) => format!("[{}]", ty_to_string(inner)),
        TypeKind::Array { elem, .. } => format!("[{}]", ty_to_string(elem)),
        TypeKind::Ref { inner, .. } => ty_to_string(inner),
        TypeKind::Tuple(elems) => format!(
            "({})",
            elems
                .iter()
                .map(ty_to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeKind::Path(tp) => match tp.segments.last() {
            Some(seg) if !seg.generics.is_empty() => {
                let args: Vec<String> = seg
                    .generics
                    .iter()
                    .map(|g| match g {
                        GenericArg::Type(t) => ty_to_string(t),
                        GenericArg::Const(_) => "_".to_string(),
                    })
                    .collect();
                format!("{}<{}>", seg.name.name, args.join(", "))
            }
            Some(seg) => seg.name.name.clone(),
            None => "_".to_string(),
        },
        _ => "_".to_string(),
    }
}

/// Every item in `items`, descending into inline module bodies so a
/// declaration nested in a `mod name { ... }` is visible to the
/// whole-program synthesis passes. A multi-file package is auto-bundled
/// into one source by wrapping each sibling file in `mod <stem> { ... }`
/// (see `gossamer-cli`'s sibling auto-bundle), so a struct declared in
/// another file lives one module level deep; the synthesizers must reach
/// it exactly as the resolver does when it flattens the inline module
/// tree for name resolution.
fn flatten_items(items: &[Item]) -> Vec<&Item> {
    let mut out = Vec::new();
    collect_flat_items(items, &mut out);
    out
}

fn collect_flat_items<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
    for item in items {
        out.push(item);
        if let ItemKind::Mod(decl) = &item.kind
            && let ModBody::Inline(inner) = &decl.body
        {
            collect_flat_items(inner, out);
        }
    }
}

/// Synthesizes a `fn __gos_typeinfo_<Name>() -> [(String, String)]`
/// returning each field's `(name, type)` for every named-field struct,
/// so `typeInfo::<Name>()` reflects the type's fields at compile time
/// (the comptime reflection surface). Only emitted when the source
/// mentions `typeInfo` so non-reflecting programs carry no extra items.
#[must_use]
pub fn synthesize_type_info(parsed: &SourceFile) -> String {
    let mut out = String::new();
    for item in flatten_items(&parsed.items) {
        let ItemKind::Struct(decl) = &item.kind else {
            continue;
        };
        let StructBody::Named(fields) = &decl.body else {
            continue;
        };
        if !decl.generics.params.is_empty() {
            continue;
        }
        let entries: Vec<String> = fields
            .iter()
            .map(|f| format!("(\"{}\", \"{}\")", f.name.name, ty_to_string(&f.ty)))
            .collect();
        out.push_str(&format!(
            "fn {}() -> [(String, String)] {{ [{}] }}\n",
            type_info_fn(&decl.name.name),
            entries.join(", "),
        ));
    }
    out
}

/// Rewrites `typeInfo::<Type>()` into a call to the synthesized
/// `__gos_typeinfo_<Type>()` reflection function.
pub fn rewrite_type_info_calls(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    use gossamer_ast::expr::{Expr, ExprKind};
    use gossamer_ast::visitor::walk_expr_mut;

    struct Rewriter;
    impl VisitorMut for Rewriter {
        fn visit_expr(&mut self, expr: &mut Expr) {
            walk_expr_mut(self, expr);
            let ExprKind::Call { callee, .. } = &mut expr.kind else {
                return;
            };
            let ExprKind::Path(path) = &mut callee.kind else {
                return;
            };
            if path.segments.len() != 1 {
                return;
            }
            let seg = &mut path.segments[0];
            if seg.name.name != "typeInfo" || seg.generics.len() != 1 {
                return;
            }
            let GenericArg::Type(ty) = &seg.generics[0] else {
                return;
            };
            let TypeKind::Path(tp) = &ty.kind else {
                return;
            };
            let Some(type_seg) = tp.segments.last() else {
                return;
            };
            seg.name.name = type_info_fn(&type_seg.name.name);
            seg.generics.clear();
        }
    }
    Rewriter.visit_source_file(sf);
}

/// Hands out fresh node ids for cloned `inline for` bodies. The resolver
/// and checker key maps on `NodeId`, so each unrolled copy needs unique
/// ids rather than the source body's (which would collide).
struct InlineForIds {
    next: u32,
}

impl InlineForIds {
    fn id(&mut self) -> NodeId {
        let id = NodeId::from_raw(self.next);
        self.next += 1;
        id
    }
}

/// Renumbers a cloned subtree's node ids in place.
struct InlineForRenum<'a> {
    ids: &'a mut InlineForIds,
}

impl gossamer_ast::VisitorMut for InlineForRenum<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        e.id = self.ids.id();
        gossamer_ast::visitor::walk_expr_mut(self, e);
    }
    fn visit_pattern(&mut self, p: &mut gossamer_ast::pattern::Pattern) {
        p.id = self.ids.id();
        gossamer_ast::visitor::walk_pattern_mut(self, p);
    }
    fn visit_stmt(&mut self, s: &mut gossamer_ast::stmt::Stmt) {
        s.id = self.ids.id();
        gossamer_ast::visitor::walk_stmt_mut(self, s);
    }
    fn visit_type(&mut self, t: &mut gossamer_ast::ty::Type) {
        t.id = self.ids.id();
        gossamer_ast::visitor::walk_type_mut(self, t);
    }
}

/// Collects each non-generic named-field struct's `(field_name,
/// field_type_string)` list - the data backing both `typeInfo::<T>()` and
/// the `inline for` unroller.
fn collect_struct_fields(sf: &SourceFile) -> HashMap<String, Vec<(String, String)>> {
    let mut map = HashMap::new();
    for item in flatten_items(&sf.items) {
        let ItemKind::Struct(decl) = &item.kind else {
            continue;
        };
        let StructBody::Named(fields) = &decl.body else {
            continue;
        };
        if !decl.generics.params.is_empty() {
            continue;
        }
        let entries = fields
            .iter()
            .map(|f| (f.name.name.clone(), ty_to_string(&f.ty)))
            .collect();
        map.insert(decl.name.name.clone(), entries);
    }
    map
}

/// Returns the concrete type name `T` when `iter` is `typeInfo::<T>()`,
/// the reflection call a compile-time field loop iterates.
fn typeinfo_loop_target(iter: &gossamer_ast::expr::Expr) -> Option<String> {
    use gossamer_ast::expr::ExprKind;
    let ExprKind::Call { callee, args } = &iter.kind else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let ExprKind::Path(path) = &callee.kind else {
        return None;
    };
    if path.segments.len() != 1 {
        return None;
    }
    let seg = &path.segments[0];
    if seg.name.name != "typeInfo" || seg.generics.len() != 1 {
        return None;
    }
    let GenericArg::Type(ty) = &seg.generics[0] else {
        return None;
    };
    let TypeKind::Path(tp) = &ty.kind else {
        return None;
    };
    Some(tp.segments.last()?.name.name.clone())
}

/// Expands every `for PAT in typeInfo::<T>() { BODY }` over a known struct
/// `T` into a straight-line block: the body is cloned once per field, the
/// loop variables bound to the field's `(name, type)` as comptime string
/// literals, `field_of(recv, name)` resolved to `recv.<name>`, and
/// `match` / `if` over the comptime values folded to the taken branch.
/// Pure AST -> AST in the single compile, so the emitted field code lowers
/// natively on every tier and the construct never reaches the comptime
/// fold pass (no whole-program re-compile tax).
pub fn expand_typeinfo_loops(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    let fields = collect_struct_fields(sf);
    if fields.is_empty() {
        return;
    }
    let mut ids = InlineForIds {
        next: sf.next_node_id,
    };
    let mut expander = TypeInfoLoopExpander {
        fields: &fields,
        ids: &mut ids,
    };
    expander.visit_source_file(sf);
    sf.next_node_id = ids.next;
}

struct TypeInfoLoopExpander<'a> {
    fields: &'a HashMap<String, Vec<(String, String)>>,
    ids: &'a mut InlineForIds,
}

impl gossamer_ast::VisitorMut for TypeInfoLoopExpander<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        // Post-order: expand nested field loops before the enclosing one
        // clones the body.
        gossamer_ast::visitor::walk_expr_mut(self, e);
        let ExprKind::For {
            pattern,
            iter,
            body,
            ..
        } = &e.kind
        else {
            return;
        };
        let Some(tyname) = typeinfo_loop_target(iter) else {
            return;
        };
        let Some(flds) = self.fields.get(&tyname) else {
            return;
        };
        let block = unroll_typeinfo_loop(pattern, body, flds, self.ids);
        e.kind = ExprKind::Block(block);
    }
}

/// Unrolls one field loop body across `fields`, returning the straight-line
/// block. Each field's copy is scoped in its own nested block so any `let`
/// bindings the body introduces do not collide across iterations.
fn unroll_typeinfo_loop(
    pattern: &gossamer_ast::pattern::Pattern,
    body: &gossamer_ast::expr::Expr,
    fields: &[(String, String)],
    ids: &mut InlineForIds,
) -> gossamer_ast::expr::Block {
    use gossamer_ast::expr::Block;
    use gossamer_ast::stmt::{Stmt, StmtKind};
    let mut stmts = Vec::with_capacity(fields.len());
    for (fname, ftype) in fields {
        let mut binds: HashMap<String, String> = HashMap::new();
        bind_loop_pattern(pattern, fname, ftype, &mut binds);
        let mut clone = body.clone();
        let mut subst = InlineForSubst { binds: &binds };
        subst.visit_expr(&mut clone);
        let mut renum = InlineForRenum { ids };
        renum.visit_expr(&mut clone);
        let span = clone.span;
        let id = ids.id();
        stmts.push(Stmt::new(
            id,
            span,
            StmtKind::Expr {
                expr: Box::new(clone),
                has_semi: true,
            },
        ));
    }
    Block {
        stmts,
        tail: None,
        synthetic: true,
        is_arena: false,
        is_comptime: false,
    }
}

/// Binds the loop pattern's identifiers to the current field's `(name,
/// type)` strings. Supports the canonical `(name, ty)` tuple pattern;
/// wildcards bind nothing.
fn bind_loop_pattern(
    pattern: &gossamer_ast::pattern::Pattern,
    fname: &str,
    ftype: &str,
    binds: &mut HashMap<String, String>,
) {
    use gossamer_ast::pattern::PatternKind;
    let PatternKind::Tuple(elems) = &pattern.kind else {
        return;
    };
    if elems.len() != 2 {
        return;
    }
    bind_one(&elems[0], fname, binds);
    bind_one(&elems[1], ftype, binds);
}

fn bind_one(p: &gossamer_ast::pattern::Pattern, val: &str, binds: &mut HashMap<String, String>) {
    use gossamer_ast::pattern::PatternKind;
    if let PatternKind::Ident {
        name,
        subpattern: None,
        ..
    } = &p.kind
    {
        binds.insert(name.name.clone(), val.to_string());
    }
}

/// Substitutes bound loop variables with their comptime string values,
/// resolves `field_of(recv, name)` projections, and folds `match` / `if` /
/// string operations over the now-comptime values. Post-order, so a node's
/// children are already substituted when it is folded.
struct InlineForSubst<'a> {
    binds: &'a HashMap<String, String>,
}

impl gossamer_ast::VisitorMut for InlineForSubst<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        gossamer_ast::visitor::walk_expr_mut(self, e);
        if let Some(kind) = self.fold_node(e) {
            e.kind = kind;
        }
    }
}

impl InlineForSubst<'_> {
    fn fold_node(&self, e: &gossamer_ast::expr::Expr) -> Option<gossamer_ast::expr::ExprKind> {
        use gossamer_ast::common::Ident;
        use gossamer_ast::expr::{ExprKind, FieldSelector, Literal};
        match &e.kind {
            ExprKind::Path(p) if p.segments.len() == 1 && p.segments[0].generics.is_empty() => self
                .binds
                .get(p.segments[0].name.name.as_str())
                .map(|v| ExprKind::Literal(Literal::String(v.clone()))),
            ExprKind::Call { callee, args } if is_field_of(callee) && args.len() == 2 => {
                let ExprKind::Literal(Literal::String(fld)) = &args[1].kind else {
                    return None;
                };
                Some(ExprKind::FieldAccess {
                    receiver: Box::new(args[0].clone()),
                    field: FieldSelector::Named(Ident::new(fld.clone())),
                })
            }
            ExprKind::Match { scrutinee, arms } => {
                let s = literal_str(scrutinee)?;
                let body = select_arm(&s, arms)?;
                Some(body.kind.clone())
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let ExprKind::Literal(Literal::Bool(b)) = &condition.kind else {
                    return None;
                };
                if *b {
                    Some(then_branch.kind.clone())
                } else {
                    Some(
                        else_branch
                            .as_ref()
                            .map_or(ExprKind::Literal(Literal::Unit), |x| x.kind.clone()),
                    )
                }
            }
            ExprKind::Binary { op, lhs, rhs } => fold_binary(*op, lhs, rhs),
            _ => None,
        }
    }
}

fn is_field_of(callee: &gossamer_ast::expr::Expr) -> bool {
    use gossamer_ast::expr::ExprKind;
    let ExprKind::Path(p) = &callee.kind else {
        return false;
    };
    p.segments.len() == 1 && p.segments[0].name.name == "field_of"
}

fn literal_str(e: &gossamer_ast::expr::Expr) -> Option<String> {
    use gossamer_ast::expr::{ExprKind, Literal};
    if let ExprKind::Literal(Literal::String(s)) = &e.kind {
        Some(s.clone())
    } else {
        None
    }
}

/// Selects the first arm (no guard) whose pattern matches the comptime
/// string `s`: a string-literal pattern equal to `s`, a wildcard, or an
/// or-pattern containing either.
fn select_arm<'a>(
    s: &str,
    arms: &'a [gossamer_ast::expr::MatchArm],
) -> Option<&'a gossamer_ast::expr::Expr> {
    arms.iter()
        .find(|arm| arm.guard.is_none() && pattern_matches_str(&arm.pattern, s))
        .map(|arm| &arm.body)
}

fn pattern_matches_str(p: &gossamer_ast::pattern::Pattern, s: &str) -> bool {
    use gossamer_ast::expr::Literal;
    use gossamer_ast::pattern::PatternKind;
    match &p.kind {
        PatternKind::Wildcard => true,
        PatternKind::Literal(Literal::String(lit)) => lit == s,
        PatternKind::Or(alts) => alts.iter().any(|a| pattern_matches_str(a, s)),
        _ => false,
    }
}

/// Folds string concatenation and string equality over two comptime string
/// literals; other shapes stay runtime.
fn fold_binary(
    op: gossamer_ast::common::BinaryOp,
    lhs: &gossamer_ast::expr::Expr,
    rhs: &gossamer_ast::expr::Expr,
) -> Option<gossamer_ast::expr::ExprKind> {
    use gossamer_ast::common::BinaryOp;
    use gossamer_ast::expr::{ExprKind, Literal};
    let (l, r) = (literal_str(lhs)?, literal_str(rhs)?);
    match op {
        BinaryOp::Add => Some(ExprKind::Literal(Literal::String(l + &r))),
        BinaryOp::Eq => Some(ExprKind::Literal(Literal::Bool(l == r))),
        BinaryOp::Ne => Some(ExprKind::Literal(Literal::Bool(l != r))),
        _ => None,
    }
}

/// Specializes generic field-loop templates per turbofish call site, so a
/// reflection-driven serializer can be written once as
/// `fn name<T>(v: T) -> String { ... for (n, t) in typeInfo::<T>() ... }`
/// and called as `name::<User>(x)`. Each `name::<C>(args)` site gets a
/// monomorphic copy `__gos_inlinefor_name_C` with the type parameter
/// replaced by `C` everywhere - so the inner `typeInfo::<T>()` becomes
/// `typeInfo::<C>()`, which the unroller then expands - the call is
/// rewritten to that copy, and the generic template is removed so it is
/// never type-checked generically. Runs before `expand_typeinfo_loops`.
/// Concrete-type loops use no turbofish and skip this pass entirely; a
/// template called without a turbofish leaves no specialization and the
/// removed template surfaces as an ordinary unknown-name error.
pub fn specialize_inline_for_generics(sf: &mut SourceFile) {
    use gossamer_ast::{ItemKind, Visitor};
    let mut templates: HashMap<String, String> = HashMap::new();
    for item in &sf.items {
        if let ItemKind::Fn(decl) = &item.kind
            && let Some(param) = inline_for_template_param(decl)
        {
            templates.insert(decl.name.name.clone(), param);
        }
    }
    if templates.is_empty() {
        return;
    }
    let mut wanted: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    let mut collector = TurbofishCollector {
        templates: &templates,
        wanted: &mut wanted,
    };
    collector.visit_source_file(sf);

    let mut ids = InlineForIds {
        next: sf.next_node_id,
    };
    let mut copies: Vec<gossamer_ast::Item> = Vec::new();
    for item in &sf.items {
        let ItemKind::Fn(decl) = &item.kind else {
            continue;
        };
        let Some(param) = templates.get(&decl.name.name) else {
            continue;
        };
        let Some(types) = wanted.get(&decl.name.name) else {
            continue;
        };
        for concrete in types {
            copies.push(specialize_template(item, decl, param, concrete, &mut ids));
        }
    }
    sf.next_node_id = ids.next;

    sf.items.extend(copies);
    let mut rewriter = TurbofishRewriter {
        templates: &templates,
    };
    gossamer_ast::VisitorMut::visit_source_file(&mut rewriter, sf);
    sf.items.retain(|item| match &item.kind {
        ItemKind::Fn(decl) => !templates.contains_key(&decl.name.name),
        _ => true,
    });
}

/// Returns the type-parameter name of a generic function whose body has a
/// `for ... in typeInfo::<T>()` loop over that parameter.
fn inline_for_template_param(decl: &gossamer_ast::FnDecl) -> Option<String> {
    use gossamer_ast::{GenericParam, Visitor};
    let params: Vec<String> = decl
        .generics
        .params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type { name, .. } => Some(name.name.clone()),
            _ => None,
        })
        .collect();
    if params.is_empty() {
        return None;
    }
    let body = decl.body.as_ref()?;
    let mut probe = TemplateProbe {
        params: &params,
        found: None,
    };
    probe.visit_expr(body);
    probe.found
}

struct TemplateProbe<'a> {
    params: &'a [String],
    found: Option<String>,
}

impl gossamer_ast::Visitor for TemplateProbe<'_> {
    fn visit_expr(&mut self, e: &gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        if self.found.is_some() {
            return;
        }
        if let ExprKind::For { iter, .. } = &e.kind
            && let Some(t) = typeinfo_loop_target(iter)
            && self.params.contains(&t)
        {
            self.found = Some(t);
            return;
        }
        gossamer_ast::visitor::walk_expr(self, e);
    }
}

struct TurbofishCollector<'a> {
    templates: &'a HashMap<String, String>,
    wanted: &'a mut HashMap<String, std::collections::BTreeSet<String>>,
}

impl gossamer_ast::Visitor for TurbofishCollector<'_> {
    fn visit_expr(&mut self, e: &gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        if let ExprKind::Call { callee, .. } = &e.kind
            && let Some((name, concrete)) = turbofish_call_target(callee)
            && self.templates.contains_key(&name)
        {
            self.wanted.entry(name).or_default().insert(concrete);
        }
        gossamer_ast::visitor::walk_expr(self, e);
    }
}

/// Returns `(fn_name, concrete_type)` when `callee` is `fn_name::<Concrete>`.
fn turbofish_call_target(callee: &gossamer_ast::expr::Expr) -> Option<(String, String)> {
    use gossamer_ast::expr::ExprKind;
    let ExprKind::Path(p) = &callee.kind else {
        return None;
    };
    if p.segments.len() != 1 {
        return None;
    }
    let seg = &p.segments[0];
    if seg.generics.len() != 1 {
        return None;
    }
    let GenericArg::Type(ty) = &seg.generics[0] else {
        return None;
    };
    let TypeKind::Path(tp) = &ty.kind else {
        return None;
    };
    Some((seg.name.name.clone(), tp.segments.last()?.name.name.clone()))
}

/// Builds one monomorphic copy of `decl` with the type parameter `param`
/// replaced by `concrete`, freshly numbered and renamed.
fn specialize_template(
    item: &gossamer_ast::Item,
    decl: &gossamer_ast::FnDecl,
    param: &str,
    concrete: &str,
    ids: &mut InlineForIds,
) -> gossamer_ast::Item {
    use gossamer_ast::items::Generics;
    use gossamer_ast::{FnParam, Ident, ItemKind, VisitorMut};
    let mut copy = decl.clone();
    copy.name = Ident::new(format!("__gos_inlinefor_{}_{concrete}", decl.name.name));
    copy.generics = Generics { params: Vec::new() };

    let mut sub = SubstTypeParam { param, concrete };
    for p in &mut copy.params {
        if let FnParam::Typed { ty, .. } = p {
            sub.visit_type(ty);
        }
    }
    if let Some(ret) = &mut copy.ret {
        sub.visit_type(ret);
    }
    if let Some(body) = &mut copy.body {
        sub.visit_expr(body);
    }

    let mut renum = InlineForRenum { ids };
    for p in &mut copy.params {
        if let FnParam::Typed { pattern, ty, .. } = p {
            renum.visit_pattern(pattern);
            renum.visit_type(ty);
        }
    }
    if let Some(ret) = &mut copy.ret {
        renum.visit_type(ret);
    }
    if let Some(body) = &mut copy.body {
        renum.visit_expr(body);
    }

    gossamer_ast::Item::new(
        ids.id(),
        item.span,
        item.attrs.clone(),
        item.visibility,
        ItemKind::Fn(copy),
    )
}

/// Replaces a type parameter path `T` with the concrete type `concrete`
/// throughout the visited types - including the `T` inside the body's
/// `typeInfo::<T>()` turbofish, which the type walker reaches.
struct SubstTypeParam<'a> {
    param: &'a str,
    concrete: &'a str,
}

impl gossamer_ast::VisitorMut for SubstTypeParam<'_> {
    fn visit_type(&mut self, ty: &mut gossamer_ast::ty::Type) {
        use gossamer_ast::ty::TypeKind;
        gossamer_ast::visitor::walk_type_mut(self, ty);
        if let TypeKind::Path(tp) = &mut ty.kind
            && tp.segments.len() == 1
            && tp.segments[0].name.name == self.param
            && tp.segments[0].generics.is_empty()
        {
            tp.segments[0].name = gossamer_ast::Ident::new(self.concrete);
        }
    }
}

/// Rewrites each `name::<C>(args)` template call to `__gos_inlinefor_name_C`.
struct TurbofishRewriter<'a> {
    templates: &'a HashMap<String, String>,
}

impl gossamer_ast::VisitorMut for TurbofishRewriter<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        gossamer_ast::visitor::walk_expr_mut(self, e);
        let ExprKind::Call { callee, .. } = &mut e.kind else {
            return;
        };
        let ExprKind::Path(p) = &mut callee.kind else {
            return;
        };
        if p.segments.len() != 1 {
            return;
        }
        let orig = p.segments[0].name.name.clone();
        if !self.templates.contains_key(&orig) {
            return;
        }
        let concrete = {
            let seg = &p.segments[0];
            if seg.generics.len() != 1 {
                return;
            }
            let GenericArg::Type(ty) = &seg.generics[0] else {
                return;
            };
            let TypeKind::Path(tp) = &ty.kind else {
                return;
            };
            match tp.segments.last() {
                Some(s) => s.name.name.clone(),
                None => return,
            }
        };
        let seg = &mut p.segments[0];
        seg.name = gossamer_ast::Ident::new(format!("__gos_inlinefor_{orig}_{concrete}"));
        seg.generics.clear();
    }
}

/// Walks `parsed` for struct definitions and synthesizes
/// serialization-method source for each eligible struct. Returns the
/// generated source text, ready to be parsed and merged.
#[must_use]
pub fn synthesize_serde_impls(parsed: &SourceFile) -> String {
    let mut out = String::new();
    out.push_str("// Synthesized by `gossamer-parse::autoderive`.\n");
    out.push('\n');

    let struct_names: HashSet<String> = flatten_items(&parsed.items)
        .into_iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Struct(decl)
                if matches!(&decl.body, StructBody::Named(_) | StructBody::Tuple(_)) =>
            {
                Some(decl.name.name.clone())
            }
            _ => None,
        })
        .collect();

    for item in flatten_items(&parsed.items) {
        let ItemKind::Struct(decl) = &item.kind else {
            continue;
        };
        if !decl.generics.params.is_empty() {
            continue;
        }
        match &decl.body {
            StructBody::Named(fields) => {
                let typed: Option<Vec<(String, FieldKind)>> = fields
                    .iter()
                    .map(|f| {
                        FieldKind::from_type(&f.ty, &struct_names).map(|k| (f.name.name.clone(), k))
                    })
                    .collect();
                if let Some(typed) = typed {
                    emit_impl(&mut out, decl, &typed);
                }
            }
            StructBody::Tuple(fields) => {
                let typed: Option<Vec<FieldKind>> = fields
                    .iter()
                    .map(|f| FieldKind::from_type(&f.ty, &struct_names))
                    .collect();
                if let Some(typed) = typed {
                    emit_tuple_impl(&mut out, decl, &typed);
                }
            }
            StructBody::Unit => {}
        }
    }
    out
}

/// Emits the serde free functions for a tuple struct: a JSON object keyed
/// by position (`{"0":v0,"1":v1}`), reusing the `to_json`-backed toml/yaml
/// wrappers. Positional access `value.N` and the `Name(..)` constructor are
/// rewritten through the tuple-struct machinery.
fn emit_tuple_impl(out: &mut String, decl: &StructDecl, fields: &[FieldKind]) {
    let name = &decl.name.name;
    emit_tuple_to_json(out, name, fields);
    emit_tuple_from_json(out, name, fields);
    emit_to_toml(out, name);
    emit_from_toml(out, name);
    emit_to_yaml(out, name);
    emit_from_yaml(out, name);
}

fn emit_tuple_to_json(out: &mut String, name: &str, fields: &[FieldKind]) {
    out.push_str("// Render a tuple struct as a position-keyed JSON object. Auto-derived.\n");
    out.push_str(&format!(
        "pub fn {}(value: {name}) -> Result<String, errors::Error> {{\n",
        to_json_fn(name)
    ));
    out.push_str("    let mut out = \"\"\n    out += \"{\"\n");
    for (i, kind) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str("    out += \",\"\n");
        }
        out.push_str(&format!("    out += \"\\\"{i}\\\":\"\n"));
        let lit = kind.render_to_json(&format!("value.{i}"));
        out.push_str(&format!("    out += {lit}\n"));
    }
    out.push_str("    out += \"}\"\n    Ok(out)\n}\n\n");
}

fn emit_tuple_from_json(out: &mut String, name: &str, fields: &[FieldKind]) {
    out.push_str("// Parse a position-keyed JSON object into a tuple struct. Auto-derived.\n");
    out.push_str(&format!(
        "pub fn {}(text: &String) -> Result<{name}, errors::Error> {{\n",
        from_json_fn(name)
    ));
    out.push_str("    let v = json::parse(text)?\n");
    for (i, kind) in fields.iter().enumerate() {
        let path = format!("element `{i}`");
        let extract = kind.extract_strict("__child", &path);
        let missing = if kind.tolerates_missing_key() {
            "None".to_string()
        } else {
            format!("return Err(errors::new(\"missing element `{i}`\"))")
        };
        out.push_str(&format!(
            "    let __f{i} = match json::get(v, \"{i}\") {{\n        Some(__child) => {extract},\n        None => {missing},\n    }}\n"
        ));
    }
    let args: Vec<String> = (0..fields.len()).map(|i| format!("__f{i}")).collect();
    out.push_str(&format!("    Ok({name}({}))\n}}\n\n", args.join(", ")));
}

fn emit_impl(out: &mut String, decl: &StructDecl, fields: &[(String, FieldKind)]) {
    let name = &decl.name.name;
    emit_to_json(out, name, fields);
    emit_from_json(out, name, fields);
    emit_to_toml(out, name);
    emit_from_toml(out, name);
    emit_to_yaml(out, name);
    emit_from_yaml(out, name);
}

fn emit_to_json(out: &mut String, name: &str, fields: &[(String, FieldKind)]) {
    out.push_str(
        "// Render a value as a JSON object. Auto-derived; reached via `to_json::<T>(value)`.\n",
    );
    out.push_str(&format!(
        "pub fn {}(value: {name}) -> Result<String, errors::Error> {{\n",
        to_json_fn(name)
    ));
    out.push_str("    let mut out = \"\"\n");
    out.push_str("    out += \"{\"\n");
    for (i, (fname, kind)) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str("    out += \",\"\n");
        }
        out.push_str(&format!("    out += \"\\\"{fname}\\\":\"\n"));
        let lit = kind.render_to_json(&format!("value.{fname}"));
        out.push_str(&format!("    out += {lit}\n"));
    }
    out.push_str("    out += \"}\"\n");
    out.push_str("    Ok(out)\n");
    out.push_str("}\n\n");
}

fn emit_to_toml(out: &mut String, name: &str) {
    out.push_str("// Render a value as TOML. Auto-derived; reached via `to_toml::<T>(value)`.\n");
    out.push_str(&format!(
        "pub fn {}(value: {name}) -> Result<String, errors::Error> {{\n",
        serde_fn("to_toml", name)
    ));
    out.push_str(&format!("    let j = {}(value)?\n", to_json_fn(name)));
    out.push_str("    toml::from_json(&j)\n");
    out.push_str("}\n\n");
}

fn emit_from_toml(out: &mut String, name: &str) {
    out.push_str(
        "// Parse TOML text into a value. Auto-derived; reached via `from_toml::<T>(text)`.\n",
    );
    out.push_str(&format!(
        "pub fn {}(text: &String) -> Result<{name}, errors::Error> {{\n",
        serde_fn("from_toml", name)
    ));
    out.push_str("    let j = toml::to_json(text)?\n");
    out.push_str(&format!("    {}(&j)\n", from_json_fn(name)));
    out.push_str("}\n\n");
}

fn emit_to_yaml(out: &mut String, name: &str) {
    out.push_str("// Render a value as YAML. Auto-derived; reached via `to_yaml::<T>(value)`.\n");
    out.push_str(&format!(
        "pub fn {}(value: {name}) -> Result<String, errors::Error> {{\n",
        serde_fn("to_yaml", name)
    ));
    out.push_str(&format!("    let j = {}(value)?\n", to_json_fn(name)));
    out.push_str("    yaml::from_json(&j)\n");
    out.push_str("}\n\n");
}

fn emit_from_yaml(out: &mut String, name: &str) {
    out.push_str(
        "// Parse YAML text into a value. Auto-derived; reached via `from_yaml::<T>(text)`.\n",
    );
    out.push_str(&format!(
        "pub fn {}(text: &String) -> Result<{name}, errors::Error> {{\n",
        serde_fn("from_yaml", name)
    ));
    out.push_str("    let j = yaml::to_json(text)?\n");
    out.push_str(&format!("    {}(&j)\n", from_json_fn(name)));
    out.push_str("}\n\n");
}

fn emit_from_json(out: &mut String, name: &str, fields: &[(String, FieldKind)]) {
    out.push_str(
        "// Parse a JSON object into a value. Auto-derived; reached via `from_json::<T>(text)`.\n// Returns `Err` when a required field is missing or a field's value\n// type does not match the declaration; the error names the field.\n",
    );
    out.push_str(&format!(
        "pub fn {}(text: &String) -> Result<{name}, errors::Error> {{\n",
        from_json_fn(name)
    ));
    out.push_str("    let v = json::parse(text)?\n");
    for (fname, kind) in fields {
        let path = format!("field `{fname}`");
        let extract = kind.extract_strict("__child", &path);
        // A missing `Option` field decodes to `None` rather than erroring.
        let missing = if kind.tolerates_missing_key() {
            "None".to_string()
        } else {
            format!("return Err(errors::new(\"missing field `{fname}`\"))")
        };
        out.push_str(&format!(
            "    let {fname} = match json::get(v, \"{fname}\") {{\n        Some(__child) => {extract},\n        None => {missing},\n    }}\n"
        ));
    }
    out.push_str(&format!("    Ok({name} {{ "));
    let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
    out.push_str(&names.join(", "));
    out.push_str(" })\n");
    out.push_str("}\n\n");
}

/// Extracts the trait names listed in an item's `#[derive(...)]`
/// attributes (e.g. `["Clone", "PartialEq"]`). Multiple `#[derive(...)]`
/// attributes accumulate.
fn derive_list(attrs: &gossamer_ast::Attrs) -> Vec<String> {
    let mut out = Vec::new();
    for attr in &attrs.outer {
        let is_derive =
            attr.path.segments.len() == 1 && attr.path.segments[0].name.name == "derive";
        if !is_derive {
            continue;
        }
        if let Some(tokens) = &attr.tokens {
            for tok in tokens.split(',') {
                let name = tok.trim();
                if !name.is_empty() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// Walks `parsed` for `#[derive(...)]`-annotated structs and synthesizes
/// the requested trait methods as real Gossamer `impl` source. Clone,
/// PartialEq/Eq, and Default lower through every tier exactly like
/// hand-written methods; the `==` / `!=` operators route to the
/// synthesized `eq` in MIR (see the builder's binary-op lowering).
#[must_use]
/// Head name of a `Type` that is a single-segment path (`Point` ->
/// `"Point"`), used to attach an `impl` block to its target type.
fn type_head_name(ty: &gossamer_ast::Type) -> Option<&str> {
    match &ty.kind {
        TypeKind::Path(path) if path.segments.len() == 1 => {
            Some(path.segments[0].name.name.as_str())
        }
        _ => None,
    }
}

/// Types for which the user already wrote an `impl Type { fn fmt(&self) -> ... }`,
/// so the synthesizer must not emit a conflicting structural `fmt`.
fn types_with_user_fmt(parsed: &SourceFile) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in flatten_items(&parsed.items) {
        if let ItemKind::Impl(decl) = &item.kind
            && decl.trait_ref.is_none()
            && let Some(name) = type_head_name(&decl.self_ty)
            && decl
                .items
                .iter()
                .any(|i| matches!(i, gossamer_ast::ImplItem::Fn(f) if f.name.name == "fmt"))
        {
            out.insert(name.to_string());
        }
    }
    out
}

/// Scalar field types a synthesized `fmt` can render directly via
/// `format!("{}", field)` on every tier.
fn is_scalar_fmt_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "String"
    )
}

/// Whether a field type renders inside a synthesized `fmt` on the compiled
/// tiers: a scalar, or a struct / enum that itself ends up with a `fmt`
/// (tracked in `formattable`). Containers (`Vec`, `Option`, tuple, `HashMap`),
/// references-to-containers, channels, and function types are excluded -
/// `{}` on them does not lower through the implicit `fmt` path, so a type
/// carrying one keeps the runtime's default render and gets no implicit `fmt`.
fn ty_is_renderable(ty: &gossamer_ast::Type, formattable: &HashSet<String>) -> bool {
    match &ty.kind {
        TypeKind::Path(path) if path.segments.len() == 1 => {
            let seg = &path.segments[0];
            if !seg.generics.is_empty() {
                return false;
            }
            let name = seg.name.name.as_str();
            is_scalar_fmt_name(name) || formattable.contains(name)
        }
        TypeKind::Ref { inner, .. } => ty_is_renderable(inner, formattable),
        _ => false,
    }
}

/// Type heads whose values carry no meaningful equality / ordering, so a
/// synthesized `self.f == other.f` over a field of one would not typecheck.
/// A struct carrying one of these gets no automatic comparison (comparing it
/// is then a clean check error, never a miscompile).
fn is_noncomparable_head(name: &str) -> bool {
    matches!(
        name,
        "Sender"
            | "Receiver"
            | "Mutex"
            | "RwLock"
            | "JoinHandle"
            | "WaitGroup"
            | "Once"
            | "Context"
            | "AtomicBool"
            | "AtomicI8"
            | "AtomicI16"
            | "AtomicI32"
            | "AtomicI64"
            | "AtomicU8"
            | "AtomicU16"
            | "AtomicU32"
            | "AtomicU64"
            | "AtomicUsize"
            | "AtomicIsize"
    )
}

/// A field type over which a synthesized `eq` and `cmp` are correct and
/// lower identically on every tier: a scalar / `String` leaf, or a nested
/// struct / enum already proven comparable. Containers, tuples, generic
/// parameters, and channel / fn types are deliberately excluded - those need
/// an explicit `#[derive(PartialEq)]` / `#[derive(Ord)]`, which force the
/// synthesis without the by-value guarantee.
fn ty_is_comparable(ty: &gossamer_ast::Type, comparable: &HashSet<String>) -> bool {
    match &ty.kind {
        TypeKind::Ref { inner, .. } => ty_is_comparable(inner, comparable),
        TypeKind::Path(path) => {
            let Some(seg) = path.segments.last() else {
                return false;
            };
            if !seg.generics.is_empty() {
                return false;
            }
            let name = seg.name.name.as_str();
            !is_noncomparable_head(name) && (is_scalar_fmt_name(name) || comparable.contains(name))
        }
        _ => false,
    }
}

/// Like [`ty_is_comparable`] but for ordering (`cmp`), which additionally
/// excludes `bool`: `<` on a `bool` does not lower on the compiled tiers, so a
/// struct carrying a `bool` is equatable (`==`) but not auto-orderable - it
/// gets `eq` but not `cmp`. (Equality on a `bool` field lowers fine.)
fn ty_is_orderable(ty: &gossamer_ast::Type, orderable: &HashSet<String>) -> bool {
    match &ty.kind {
        TypeKind::Ref { inner, .. } => ty_is_orderable(inner, orderable),
        TypeKind::Path(path) => {
            let Some(seg) = path.segments.last() else {
                return false;
            };
            if !seg.generics.is_empty() {
                return false;
            }
            let name = seg.name.name.as_str();
            name != "bool"
                && !is_noncomparable_head(name)
                && (is_scalar_fmt_name(name) || orderable.contains(name))
        }
        _ => false,
    }
}

/// Types for which the user already wrote a method named `method` (in an
/// inherent or trait `impl`), so the synthesizer must not emit a conflicting
/// structural one. Mirrors [`types_with_user_fmt`] for `eq` / `cmp`.
fn types_with_user_method(parsed: &SourceFile, method: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in flatten_items(&parsed.items) {
        if let ItemKind::Impl(decl) = &item.kind
            && let Some(name) = type_head_name(&decl.self_ty)
            && decl
                .items
                .iter()
                .any(|i| matches!(i, gossamer_ast::ImplItem::Fn(f) if f.name.name == method))
        {
            out.insert(name.to_string());
        }
    }
    out
}

/// Synthesizes `impl` blocks for the `#[derive(...)]` traits, plus a
/// structural `fmt` for every struct / enum that is formattable but has no
/// `fmt` of its own, so `{}` / `{:?}` lowers on the compiled tiers exactly as
/// it renders on the VM. Returns the appended source.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "linear orchestration: collect names, fields, formattable + comparable sets, then emit"
)]
pub fn synthesize_derive_impls(parsed: &SourceFile) -> String {
    let struct_names: HashSet<String> = flatten_items(&parsed.items)
        .into_iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Struct(decl)
                if matches!(&decl.body, StructBody::Named(_) | StructBody::Tuple(_)) =>
            {
                Some(decl.name.name.clone())
            }
            _ => None,
        })
        .collect();
    let user_fmt = types_with_user_fmt(parsed);
    let user_eq = types_with_user_method(parsed, "eq");
    let user_cmp = types_with_user_method(parsed, "cmp");

    // Field types per struct / enum, used to grow the `formattable` set.
    let mut field_tys: HashMap<String, Vec<&gossamer_ast::Type>> = HashMap::new();
    for item in flatten_items(&parsed.items) {
        match &item.kind {
            ItemKind::Struct(decl) => {
                let tys: Vec<&gossamer_ast::Type> = match &decl.body {
                    StructBody::Named(fields) => fields.iter().map(|f| &f.ty).collect(),
                    StructBody::Tuple(fields) => fields.iter().map(|f| &f.ty).collect(),
                    StructBody::Unit => Vec::new(),
                };
                field_tys.insert(decl.name.name.clone(), tys);
            }
            ItemKind::Enum(decl) if decl.generics.params.is_empty() => {
                field_tys.insert(
                    decl.name.name.clone(),
                    decl.variants.iter().flat_map(variant_fields).collect(),
                );
            }
            _ => {}
        }
    }
    // A type ends up with a `fmt` if the user wrote one or a `#[derive(Debug)]`
    // requests one; seed the formattable set with those, then grow it to the
    // fixpoint of types whose every field is a scalar or an already-formattable
    // type. A struct/enum reaches a `fmt` only if all its fields actually
    // render - so a field referencing a non-formattable type (or a container)
    // never produces a `format!("{}", field)` the compiled tiers cannot lower.
    let mut formattable: HashSet<String> = HashSet::new();
    for item in flatten_items(&parsed.items) {
        let derives = derive_list(&item.attrs);
        let name = match &item.kind {
            ItemKind::Struct(d)
                if matches!(&d.body, StructBody::Named(_) | StructBody::Tuple(_)) =>
            {
                Some(&d.name.name)
            }
            ItemKind::Enum(d) if d.generics.params.is_empty() => Some(&d.name.name),
            _ => None,
        };
        if let Some(n) = name
            && (derives.iter().any(|d| d == "Debug") || user_fmt.contains(n))
        {
            formattable.insert(n.clone());
        }
    }
    loop {
        let mut changed = false;
        for (name, tys) in &field_tys {
            if formattable.contains(name) {
                continue;
            }
            if tys.iter().all(|ty| ty_is_renderable(ty, &formattable)) {
                formattable.insert(name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Grow the set of structs / enums that compare by value structurally on
    // every tier: a type is comparable once every field is a scalar / String
    // or an already-comparable nested type. This drives automatic `eq` / `cmp`
    // synthesis, so `==` / `<` work on a plain `struct Point { x, y }` with no
    // `#[derive(...)]` - exactly as they already do on tuples.
    let mut comparable: HashSet<String> = HashSet::new();
    loop {
        let mut changed = false;
        for (name, tys) in &field_tys {
            if comparable.contains(name) {
                continue;
            }
            if tys.iter().all(|ty| ty_is_comparable(ty, &comparable)) {
                comparable.insert(name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Orderable types (drives `cmp`): comparable, minus any `bool` field, since
    // `<` on a `bool` does not lower on the compiled tiers. A bool-bearing type
    // is still in `comparable` (it gets `eq`), just not here.
    let mut orderable: HashSet<String> = HashSet::new();
    loop {
        let mut changed = false;
        for (name, tys) in &field_tys {
            if orderable.contains(name) {
                continue;
            }
            if tys.iter().all(|ty| ty_is_orderable(ty, &orderable)) {
                orderable.insert(name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut out = String::new();
    for item in flatten_items(&parsed.items) {
        let mut derives = derive_list(&item.attrs);
        // Synthesize a structural `fmt` for every formattable struct / enum that
        // lacks one, so `{}` / `{:?}` lowers on the compiled tiers exactly as it
        // renders on the VM.
        let implicit_target = match &item.kind {
            ItemKind::Struct(d)
                if matches!(&d.body, StructBody::Named(_) | StructBody::Tuple(_)) =>
            {
                Some(&d.name.name)
            }
            ItemKind::Enum(d) if d.generics.params.is_empty() => Some(&d.name.name),
            _ => None,
        };
        if let Some(tn) = implicit_target
            && formattable.contains(tn)
            && !user_fmt.contains(tn)
            && !derives.iter().any(|d| d == "Debug")
        {
            derives.push("Debug".to_string());
        }
        // Synthesize `eq` / `cmp` for every by-value-comparable struct / enum
        // that has no user-written one, so structural `==` and `<` work with no
        // `#[derive(...)]`. The synthesized methods key off the same `PartialEq`
        // / `Ord` markers an explicit derive uses, so the two paths never
        // double-emit.
        if let Some(tn) = implicit_target {
            if comparable.contains(tn)
                && !user_eq.contains(tn)
                && !derives.iter().any(|d| d == "PartialEq" || d == "Eq")
            {
                derives.push("PartialEq".to_string());
            }
            if orderable.contains(tn)
                && !user_cmp.contains(tn)
                && !derives.iter().any(|d| d == "Ord" || d == "PartialOrd")
            {
                derives.push("Ord".to_string());
            }
        }
        if derives.is_empty() {
            continue;
        }
        match &item.kind {
            ItemKind::Struct(decl) => match &decl.body {
                StructBody::Named(fields) => {
                    emit_struct_derive_impl(&mut out, decl, fields, &derives, &struct_names);
                }
                StructBody::Tuple(fields) => {
                    emit_tuple_struct_derive_impl(&mut out, decl, fields, &derives, &struct_names);
                }
                StructBody::Unit => {}
            },
            ItemKind::Enum(decl) if decl.generics.params.is_empty() => {
                emit_enum_derive_impl(&mut out, decl, &derives);
            }
            _ => {}
        }
    }
    out
}

/// Iterator over the payload field types of an enum variant (empty for unit
/// variants), for the implicit-`fmt` formattability check.
fn variant_fields(v: &EnumVariant) -> impl Iterator<Item = &gossamer_ast::Type> {
    let tys: Vec<&gossamer_ast::Type> = match &v.body {
        StructBody::Unit => Vec::new(),
        StructBody::Tuple(fields) => fields.iter().map(|f| &f.ty).collect(),
        StructBody::Named(fields) => fields.iter().map(|f| &f.ty).collect(),
    };
    tys.into_iter()
}

/// The match pattern and the value-reconstruction for one enum variant,
/// binding each payload field to `{prefix}{i}` - e.g. for `V(a, b)` with prefix
/// `__s`: `("E::V(__s0, __s1)", "E::V(__s0, __s1)", ["__s0", "__s1"])`.
fn variant_shape(enum_name: &str, v: &EnumVariant, prefix: &str) -> (String, String, Vec<String>) {
    let vn = &v.name.name;
    match &v.body {
        StructBody::Unit => (
            format!("{enum_name}::{vn}"),
            format!("{enum_name}::{vn}"),
            Vec::new(),
        ),
        StructBody::Tuple(fields) => {
            let binds: Vec<String> = (0..fields.len()).map(|i| format!("{prefix}{i}")).collect();
            let joined = binds.join(", ");
            (
                format!("{enum_name}::{vn}({joined})"),
                format!("{enum_name}::{vn}({joined})"),
                binds,
            )
        }
        StructBody::Named(fields) => {
            let binds: Vec<String> = (0..fields.len()).map(|i| format!("{prefix}{i}")).collect();
            let pat: Vec<String> = fields
                .iter()
                .zip(&binds)
                .map(|(f, b)| format!("{}: {b}", f.name.name))
                .collect();
            (
                format!("{enum_name}::{vn} {{ {} }}", pat.join(", ")),
                format!("{enum_name}::{vn} {{ {} }}", pat.join(", ")),
                binds,
            )
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one block per derived trait (clone/eq/cmp/debug/default); splitting scatters the emit"
)]
fn emit_enum_derive_impl(out: &mut String, decl: &EnumDecl, derives: &[String]) {
    let name = &decl.name.name;
    let has = |t: &str| derives.iter().any(|d| d == t);
    let want_clone = has("Clone");
    let want_eq = has("PartialEq") || has("Eq");
    let want_cmp = has("PartialOrd") || has("Ord");
    let want_default = has("Default");
    let want_debug = has("Debug");
    if !(want_clone || want_eq || want_cmp || want_default || want_debug) {
        return;
    }
    out.push_str(&format!(
        "// Auto-derived from #[derive(...)] for {name}.\nimpl {name} {{\n"
    ));
    if want_clone {
        out.push_str(&format!(
            "    fn clone(&self) -> {name} {{\n        match self {{\n"
        ));
        for v in &decl.variants {
            let (pat, recon, _) = variant_shape(name, v, "__c");
            out.push_str(&format!("            {pat} => {recon},\n"));
        }
        out.push_str("        }\n    }\n");
    }
    if want_eq {
        // Nested single matches (a tuple `match (self, other)` over enum
        // variant patterns isn't reliably matched): match `self`'s variant,
        // then match `other` against the same variant inside the arm.
        out.push_str(&format!(
            "    fn eq(&self, other: &{name}) -> bool {{\n        match self {{\n"
        ));
        for v in &decl.variants {
            let (lpat, _, lbinds) = variant_shape(name, v, "__a");
            let (rpat, _, rbinds) = variant_shape(name, v, "__b");
            let cond = if lbinds.is_empty() {
                "true".to_string()
            } else {
                lbinds
                    .iter()
                    .zip(&rbinds)
                    .map(|(a, b)| format!("{a} == {b}"))
                    .collect::<Vec<_>>()
                    .join(" && ")
            };
            out.push_str(&format!(
                "            {lpat} => match other {{ {rpat} => {cond}, _ => false }},\n"
            ));
        }
        out.push_str("        }\n    }\n");
    }
    if want_cmp {
        // Order by variant declaration position first (rank), then compare
        // payloads of a same-rank pair lexicographically. Returns -1 / 0 / 1;
        // the operator routing tests `Type::cmp(a, b) <op> 0`.
        out.push_str(&format!("    fn cmp(&self, other: &{name}) -> i64 {{\n"));
        for (side, var) in [("self", "__rs"), ("other", "__ro")] {
            out.push_str(&format!("        let {var} = match {side} {{\n"));
            for (i, v) in decl.variants.iter().enumerate() {
                let vn = &v.name.name;
                let pat = match &v.body {
                    StructBody::Unit => format!("{name}::{vn}"),
                    StructBody::Tuple(fields) => {
                        let wilds = vec!["_"; fields.len()].join(", ");
                        format!("{name}::{vn}({wilds})")
                    }
                    StructBody::Named(_) => format!("{name}::{vn} {{ .. }}"),
                };
                out.push_str(&format!("            {pat} => {i},\n"));
            }
            out.push_str("        }\n");
        }
        out.push_str("        if __rs < __ro { return -1 }\n        if __rs > __ro { return 1 }\n");
        out.push_str("        match self {\n");
        for v in &decl.variants {
            let (lpat, _, lbinds) = variant_shape(name, v, "__a");
            let (rpat, _, rbinds) = variant_shape(name, v, "__b");
            if lbinds.is_empty() {
                out.push_str(&format!("            {lpat} => 0,\n"));
            } else {
                let mut body = String::new();
                for (a, b) in lbinds.iter().zip(&rbinds) {
                    body.push_str(&format!(
                        "if {a} < {b} {{ return -1 }}\n                if {b} < {a} {{ return 1 }}\n                "
                    ));
                }
                body.push('0');
                out.push_str(&format!(
                    "            {lpat} => match other {{\n                {rpat} => {{\n                {body}\n                }},\n                _ => 0,\n            }},\n"
                ));
            }
        }
        out.push_str("        }\n    }\n");
    }
    if want_debug {
        out.push_str("    fn fmt(&self) -> String {\n        match self {\n");
        for v in &decl.variants {
            let (pat, _, binds) = variant_shape(name, v, "__d");
            let vn = &v.name.name;
            let arm = match &v.body {
                StructBody::Unit => format!("\"{vn}\""),
                StructBody::Tuple(_) => {
                    let holes = binds.iter().map(|_| "{}").collect::<Vec<_>>().join(", ");
                    format!("format!(\"{vn}({holes})\", {})", binds.join(", "))
                }
                StructBody::Named(fields) => {
                    let parts: Vec<String> = fields
                        .iter()
                        .map(|f| format!("{}: {{}}", f.name.name))
                        .collect();
                    format!(
                        "format!(\"{vn} {{{{ {} }}}}\", {})",
                        parts.join(", "),
                        binds.join(", ")
                    )
                }
            };
            out.push_str(&format!("            {pat} => {arm},\n"));
        }
        out.push_str("        }\n    }\n");
    }
    if want_default {
        // Rust requires `#[default]` on exactly one (unit) variant.
        let default_variant = decl.variants.iter().find(|v| {
            v.attrs
                .outer
                .iter()
                .any(|a| a.path.segments.len() == 1 && a.path.segments[0].name.name == "default")
        });
        if let Some(v) = default_variant {
            if matches!(v.body, StructBody::Unit) {
                out.push_str(&format!(
                    "    fn default() -> {name} {{ {name}::{} }}\n",
                    v.name.name
                ));
            }
        }
    }
    out.push_str("}\n\n");
}

/// `("<T, U>", "Name<T, U>")` for a generic struct, or `("", "Name")` for a
/// non-generic one. Lifetime / const params are skipped (rare in derives).
fn struct_generics(decl: &StructDecl) -> (String, String) {
    let names: Vec<&str> = decl
        .generics
        .params
        .iter()
        .filter_map(|p| match p {
            gossamer_ast::GenericParam::Type { name, .. } => Some(name.name.as_str()),
            _ => None,
        })
        .collect();
    if names.is_empty() {
        (String::new(), decl.name.name.clone())
    } else {
        let args = format!("<{}>", names.join(", "));
        (args.clone(), format!("{}{args}", decl.name.name))
    }
}

/// Emits `Clone` / `PartialEq` / `Default` / `Debug` impls for a tuple
/// struct, using positional access `self.N` and positional construction
/// `Name(..)` (rewritten to the struct-literal form by
/// `rewrite_tuple_struct_ctors`). Debug renders `Name(v0, v1)`.
#[allow(
    clippy::too_many_lines,
    reason = "one block per derived trait; splitting scatters the emit"
)]
fn emit_tuple_struct_derive_impl(
    out: &mut String,
    decl: &StructDecl,
    fields: &[gossamer_ast::TupleField],
    derives: &[String],
    structs: &HashSet<String>,
) {
    let name = &decl.name.name;
    let has = |t: &str| derives.iter().any(|d| d == t);
    let want_clone = has("Clone");
    let want_eq = has("PartialEq") || has("Eq");
    let want_cmp = has("PartialOrd") || has("Ord");
    let want_default = has("Default");
    let want_debug = has("Debug");
    if !(want_clone || want_eq || want_cmp || want_default || want_debug) {
        return;
    }
    let (gen_decl, self_ty) = struct_generics(decl);
    let n = fields.len();
    out.push_str(&format!(
        "// Auto-derived from #[derive(...)] for {name}.\nimpl{gen_decl} {self_ty} {{\n"
    ));
    if want_clone {
        let init: Vec<String> = (0..n).map(|i| format!("self.{i}")).collect();
        out.push_str(&format!(
            "    fn clone(&self) -> {self_ty} {{ {name}({}) }}\n",
            init.join(", ")
        ));
    }
    if want_eq {
        if n == 0 {
            out.push_str(&format!(
                "    fn eq(&self, other: &{self_ty}) -> bool {{ true }}\n"
            ));
        } else {
            let conds: Vec<String> = (0..n).map(|i| format!("self.{i} == other.{i}")).collect();
            out.push_str(&format!(
                "    fn eq(&self, other: &{self_ty}) -> bool {{ {} }}\n",
                conds.join(" && ")
            ));
        }
    }
    if want_cmp {
        out.push_str(&format!("    fn cmp(&self, other: &{self_ty}) -> i64 {{\n"));
        for i in 0..n {
            out.push_str(&format!(
                "        if self.{i} < other.{i} {{ return -1 }}\n        if other.{i} < self.{i} {{ return 1 }}\n"
            ));
        }
        out.push_str("        0\n    }\n");
    }
    if want_default {
        let typed: Option<Vec<FieldKind>> = fields
            .iter()
            .map(|f| FieldKind::from_type(&f.ty, structs))
            .collect();
        if let Some(typed) = typed {
            let init: Vec<String> = typed.iter().map(FieldKind::default_literal).collect();
            out.push_str(&format!(
                "    fn default() -> {self_ty} {{ {name}({}) }}\n",
                init.join(", ")
            ));
        }
    }
    if want_debug {
        let placeholders: Vec<&str> = (0..n).map(|_| "{}").collect();
        let argvals: Vec<String> = (0..n).map(|i| format!("self.{i}")).collect();
        out.push_str(&format!(
            "    fn fmt(&self) -> String {{ format!(\"{name}({})\", {}) }}\n",
            placeholders.join(", "),
            argvals.join(", ")
        ));
    }
    out.push_str("}\n");
}

fn emit_struct_derive_impl(
    out: &mut String,
    decl: &StructDecl,
    fields: &[gossamer_ast::StructField],
    derives: &[String],
    structs: &HashSet<String>,
) {
    let name = &decl.name.name;
    let has = |t: &str| derives.iter().any(|d| d == t);
    let want_clone = has("Clone");
    let want_eq = has("PartialEq") || has("Eq");
    let want_cmp = has("PartialOrd") || has("Ord");
    let want_default = has("Default");
    let want_debug = has("Debug");
    if !(want_clone || want_eq || want_cmp || want_default || want_debug) {
        return;
    }
    // `(gen_decl, self_ty)` = ("<T>", "Pair<T>") for a generic struct, else
    // ("", "Pair"). Struct *literals* never carry the args, so the
    // reconstruction below stays `{name} { … }`.
    let (gen_decl, self_ty) = struct_generics(decl);
    let field_names: Vec<&str> = fields.iter().map(|f| f.name.name.as_str()).collect();
    out.push_str(&format!(
        "// Auto-derived from #[derive(...)] for {name}.\nimpl{gen_decl} {self_ty} {{\n"
    ));
    if want_clone {
        // Reconstruct with a field-by-field copy. In the GC model a value
        // struct's fields are shared by copy; this avoids a per-field
        // `.clone()` call (which the VM's name-global method dispatch would
        // misroute back to `Type::clone`).
        let init: Vec<String> = field_names
            .iter()
            .map(|f| format!("{f}: self.{f}"))
            .collect();
        out.push_str(&format!(
            "    fn clone(&self) -> {self_ty} {{ {name} {{ {} }} }}\n",
            init.join(", ")
        ));
    }
    if want_eq {
        if field_names.is_empty() {
            out.push_str(&format!(
                "    fn eq(&self, other: &{self_ty}) -> bool {{ true }}\n"
            ));
        } else {
            let conds: Vec<String> = field_names
                .iter()
                .map(|f| format!("self.{f} == other.{f}"))
                .collect();
            out.push_str(&format!(
                "    fn eq(&self, other: &{self_ty}) -> bool {{ {} }}\n",
                conds.join(" && ")
            ));
        }
    }
    if want_cmp {
        // Lexicographic field-by-field ordering returning -1 / 0 / 1; the
        // operator routing tests `Type::cmp(a, b) <op> 0`. Each `<` recurses:
        // scalars / String compare natively, a nested struct routes to its own
        // `cmp`.
        out.push_str(&format!("    fn cmp(&self, other: &{self_ty}) -> i64 {{\n"));
        for f in &field_names {
            out.push_str(&format!(
                "        if self.{f} < other.{f} {{ return -1 }}\n        if other.{f} < self.{f} {{ return 1 }}\n"
            ));
        }
        out.push_str("        0\n    }\n");
    }
    if want_default {
        // Per-field default literal needs each field classified; if any
        // field type is outside the supported set, skip Default rather
        // than emit code that won't compile.
        let typed: Option<Vec<(String, FieldKind)>> = fields
            .iter()
            .map(|f| FieldKind::from_type(&f.ty, structs).map(|k| (f.name.name.clone(), k)))
            .collect();
        if let Some(typed) = typed {
            let init: Vec<String> = typed
                .iter()
                .map(|(f, k)| format!("{f}: {}", k.default_literal()))
                .collect();
            out.push_str(&format!(
                "    fn default() -> {self_ty} {{ {name} {{ {} }} }}\n",
                init.join(", ")
            ));
        }
    }
    if want_debug {
        // `fmt(&self) -> String` rendering `Name { f0: v0, f1: v1 }`, matching
        // the VM's `value.rs::write_struct` byte-for-byte so all tiers agree.
        // `{}` on each field recurses (primitives print directly; a nested
        // struct field routes to its own `fmt`). `{{` / `}}` are literal braces.
        let mut tmpl = String::new();
        tmpl.push_str(name);
        tmpl.push_str(" {{ ");
        for (i, f) in field_names.iter().enumerate() {
            if i > 0 {
                tmpl.push_str(", ");
            }
            tmpl.push_str(f);
            tmpl.push_str(": {}");
        }
        tmpl.push_str(" }}");
        let argvals: Vec<String> = field_names.iter().map(|f| format!("self.{f}")).collect();
        if field_names.is_empty() {
            out.push_str(&format!(
                "    fn fmt(&self) -> String {{ format!(\"{tmpl}\") }}\n"
            ));
        } else {
            out.push_str(&format!(
                "    fn fmt(&self) -> String {{ format!(\"{tmpl}\", {}) }}\n",
                argvals.join(", ")
            ));
        }
    }
    out.push_str("}\n\n");
}

/// Preprocesses a Gossamer source string by appending synthesized
/// `from_json` / `to_json` impl blocks for every eligible struct.
/// Returns the augmented source. Callers should put the augmented
/// source into the source map before invoking `parse_source_file`.
#[must_use]
pub fn augment_source(source: &str) -> String {
    // Compile-time validation macro backers (`regex!` / `sql!`).
    let validators = synthesize_validators(source);
    // Stdlib structs (pem::Block, …) are real Gossamer structs +
    // wrapper functions injected here; the wrappers call leaf
    // `gos_rt_*` intrinsics that return tuples/bytes, so the same
    // code compiles + runs on every tier. `rewrite_stdlib_struct_surface`
    // (in parse_with_autoderive) redirects the user's
    // `encoding::pem::*` call / literal / type sites onto these.
    let stdlib_wrappers = synthesize_stdlib_wrappers(source);
    let (serde, derives, type_info) = if source_may_need_ast_synthesis(source) {
        let mut probe_map = SourceMap::new();
        let probe_file = probe_map.add_file("<autoderive-probe>", source.to_string());
        let (parsed, _) = crate::parse_source_file(source, probe_file);
        let serde = synthesize_serde_impls(&parsed);
        let derives = synthesize_derive_impls(&parsed);
        // Field-reflection functions for `typeInfo::<T>()`, emitted only
        // when the source reflects (keeps non-reflecting programs lean).
        let type_info = if source.contains("typeInfo") {
            synthesize_type_info(&parsed)
        } else {
            String::new()
        };
        (serde, derives, type_info)
    } else {
        (String::new(), String::new(), String::new())
    };
    if synth_is_empty(&serde)
        && stdlib_wrappers.is_empty()
        && derives.is_empty()
        && type_info.is_empty()
        && validators.is_empty()
    {
        return source.to_string();
    }
    if std::env::var_os("GOS_AUTODERIVE_DEBUG").is_some() {
        eprintln!("=== autoderive synth ===\n{serde}{derives}{stdlib_wrappers}=== /autoderive ===");
    }
    let mut combined = String::with_capacity(
        source.len() + serde.len() + derives.len() + stdlib_wrappers.len() + 2,
    );
    combined.push_str(source);
    if !combined.ends_with('\n') {
        combined.push('\n');
    }
    combined.push('\n');
    if !synth_is_empty(&serde) {
        combined.push_str(&serde);
    }
    combined.push_str(&derives);
    combined.push_str(&stdlib_wrappers);
    combined.push_str(&type_info);
    combined.push_str(&validators);
    combined
}

/// Returns true when an AST walk could synthesize source. Most files contain
/// only functions and imports; for those, avoid the probe parse and let the
/// later authoritative frontend parse handle normal rewrites.
fn source_may_need_ast_synthesis(source: &str) -> bool {
    let mut map = SourceMap::new();
    let file = map.add_file("<autoderive-prescan>", String::new());
    let mut lexer = Lexer::new(source, file);
    loop {
        let token = lexer.next_token();
        match token.kind {
            TokenKind::Keyword(Keyword::Struct | Keyword::Enum) => return true,
            TokenKind::Punct(Punct::Hash) => return true,
            TokenKind::Eof => return false,
            _ => {}
        }
    }
}

fn synthesize_stdlib_wrappers(source: &str) -> String {
    let mut stdlib_wrappers = String::new();
    if source.contains("pem::") {
        stdlib_wrappers.push_str(PEM_WRAPPERS);
    }
    if source.contains("x509::") {
        stdlib_wrappers.push_str(X509_WRAPPERS);
    }
    if source.contains("fs::metadata") {
        stdlib_wrappers.push_str(FS_METADATA_WRAPPERS);
    }
    if source.contains("tar::") {
        stdlib_wrappers.push_str(TAR_WRAPPERS);
    }
    if source.contains("zip::") {
        stdlib_wrappers.push_str(ZIP_WRAPPERS);
    }
    if source.contains("sql::") {
        stdlib_wrappers.push_str(SQL_WRAPPERS);
    }
    if HTTP_SECURITY_MARKERS.iter().any(|m| source.contains(m)) {
        stdlib_wrappers.push_str(HTTP_SECURITY_WRAPPERS);
    }
    if source.contains("time::after") {
        stdlib_wrappers.push_str(TIME_TIMER_WRAPPERS);
    }
    stdlib_wrappers
}

/// Real-struct + wrapper source for `std::encoding::pem`. The
/// wrappers fold the leaf intrinsics' tuple/byte returns into real
/// `__gos_pem_Block` structs, which lower natively on every tier.
/// Source substrings that pull in [`HTTP_SECURITY_WRAPPERS`]. Only the
/// request/response-integrated gap surface triggers injection; the bare
/// `csrf::issue_token` / `session::sign` / `cookie::*` primitives are
/// already wired and must not drag the wrappers (and their `use`s) into
/// programs that only touch them.
const HTTP_SECURITY_MARKERS: &[&str] = &[
    "csrf::Config",
    "csrf::config",
    "csrf::check",
    "csrf::extract_token",
    "csrf::attach_cookie",
    "csrf::origin_allowed",
    "csrf::RouteAuth",
    "session::signed",
    "session::encrypted",
    "session::with_session",
    "session::save",
    "session::load",
    "session::Store",
    "form::Form",
    "form::parse",
    "multipart::parse",
    "multipart::Part",
    "multipart::boundary",
    "form_file",
];

const PEM_WRAPPERS: &str = r"
struct __gos_pem_Block { block_type: String, bytes: [u8] }
fn __gos_pem_decode(s: &String) -> Result<__gos_pem_Block, errors::Error> {
    let (t, b) = __gos_pem_decode_raw(s)?
    Ok(__gos_pem_Block { block_type: t, bytes: b })
}
fn __gos_pem_decode_all(s: &String) -> Result<[__gos_pem_Block], errors::Error> {
    let raws = __gos_pem_decode_all_raw(s)?
    let mut out: [__gos_pem_Block] = []
    for r in raws {
        out.push(__gos_pem_Block { block_type: r.0, bytes: r.1 })
    }
    Ok(out)
}
fn __gos_pem_encode(b: __gos_pem_Block) -> String {
    __gos_pem_encode_raw(b.block_type, b.bytes)
}
";

/// Channel-returning timer wrapper for `std::time`. `time::after(d)` returns a
/// `Receiver` that yields once after `d`, firing on a goroutine that completes,
/// so the result composes with `select` / `while let`.
const TIME_TIMER_WRAPPERS: &str = r"
fn __gos_time_after_fire(tx: Sender<i64>, d: time::Duration) {
    time::sleep(d)
    tx.send(1)
    tx.close()
}
fn __gos_time_after(d: time::Duration) -> Receiver<i64> {
    let (tx, rx) = channel(1)
    go __gos_time_after_fire(tx, d)
    rx
}
";

/// Real-struct + wrapper source for `std::crypto::x509`.
const X509_WRAPPERS: &str = r"
struct __gos_x509_CertInfo { subject: String, issuer: String, serial: [u8], not_before_unix: i64, not_after_unix: i64, san_dns: [String], sha256: [u8] }
fn __gos_x509_parse_pem(s: &String) -> Result<__gos_x509_CertInfo, errors::Error> {
    let (subject, issuer, serial, nb, na, san, sha) = __gos_x509_parse_pem_raw(s)?
    Ok(__gos_x509_CertInfo { subject: subject, issuer: issuer, serial: serial, not_before_unix: nb, not_after_unix: na, san_dns: san, sha256: sha })
}
";

/// Real-struct + wrapper source for `std::fs::metadata`. Folds the
/// leaf intrinsic's 6-tuple into a real `Metadata` struct so
/// `fs::metadata(p).size` / `.is_file` lower natively on every tier.
/// Field order MUST match the VM's `fs::Metadata` (see
/// `builtin_fs_metadata`).
const FS_METADATA_WRAPPERS: &str = r"
struct __gos_fs_Metadata { size: i64, is_file: bool, is_dir: bool, is_symlink: bool, readonly: bool, modified_unix_ms: i64 }
fn __gos_fs_metadata(path: &String) -> Result<__gos_fs_Metadata, errors::Error> {
    let (size, is_file, is_dir, is_symlink, readonly, modified) = __gos_fs_metadata_raw(path)?
    Ok(__gos_fs_Metadata { size: size, is_file: is_file, is_dir: is_dir, is_symlink: is_symlink, readonly: readonly, modified_unix_ms: modified })
}
";

/// Real-struct + wrapper source for `std::archive::tar` (read).
/// `write` lowers directly (no struct).
const TAR_WRAPPERS: &str = r"
struct __gos_tar_TarEntry { name: String, data: [u8], is_dir: bool }
fn __gos_tar_read(data: &[u8]) -> Result<[__gos_tar_TarEntry], errors::Error> {
    let raws = __gos_tar_read_raw(data)?
    let mut out: [__gos_tar_TarEntry] = []
    for r in raws {
        out.push(__gos_tar_TarEntry { name: r.0, data: r.1, is_dir: r.2 })
    }
    Ok(out)
}
";

/// Real-struct + wrapper source for `std::archive::zip` (read).
const ZIP_WRAPPERS: &str = r"
struct __gos_zip_ZipEntry { name: String, data: [u8], is_dir: bool }
fn __gos_zip_read(data: &[u8]) -> Result<[__gos_zip_ZipEntry], errors::Error> {
    let raws = __gos_zip_read_raw(data)?
    let mut out: [__gos_zip_ZipEntry] = []
    for r in raws {
        out.push(__gos_zip_ZipEntry { name: r.0, data: r.1, is_dir: r.2 })
    }
    Ok(out)
}
";

/// Real-struct + wrapper source for `std::database::sql`. `Conn` /
/// `Rows` / `Row` / `Tx` are real Gossamer structs holding an opaque
/// `i64` handle; methods call scalar-shaped `__gos_sql_*_raw` leaf
/// intrinsics (sentinel error convention, message via
/// `__gos_sql_last_error_raw`), so the same code runs on every tier.
const SQL_WRAPPERS: &str = r#"
enum __gos_sql_Value { Null, Bool(bool), Int(i64), Float(f64), Text(String), Blob([u8]) }
enum __gos_sql_IsolationLevel { Default, ReadUncommitted, ReadCommitted, RepeatableRead, Serializable }
struct __gos_sql_Conn { __handle: i64 }
struct __gos_sql_Rows { __handle: i64 }
struct __gos_sql_Row { __handle: i64 }
struct __gos_sql_Tx { __handle: i64 }
struct __gos_sql_Stmt { __handle: i64 }
struct __gos_sql_Pool { __handle: i64 }
struct __gos_sql_Notification { channel: String, payload: String, process_id: i64 }
struct __gos_sql_Select { table: String, cols: [String], wheres: [String], binds: [__gos_sql_Value], order: String, lim: i64, off: i64 }
fn __gos_sql_err() -> errors::Error {
    errors::new(__gos_sql_last_error_raw())
}
fn __gos_sql_row_guard(k: i64) -> Result<(), errors::Error> {
    if k == -2 { return Err(errors::new("sql: row is no longer valid (cursor advanced or rows closed)")) }
    Ok(())
}
fn __gos_sql_open(name: &String, url: &String) -> Result<__gos_sql_Conn, errors::Error> {
    let h = __gos_sql_open_raw(name, url)
    if h < 0 { return Err(__gos_sql_err()) }
    Ok(__gos_sql_Conn { __handle: h })
}
fn __gos_sql_drivers() -> [String] {
    let joined = __gos_sql_drivers_raw()
    if joined == "" { return [] }
    joined.split(",")
}
fn __gos_sql_bind(params: &[__gos_sql_Value]) -> i64 {
    let p = __gos_sql_params_new_raw()
    for v in params {
        match v {
            __gos_sql_Value::Null => __gos_sql_params_push_null_raw(p),
            __gos_sql_Value::Bool(b) => __gos_sql_params_push_bool_raw(p, if b { 1 } else { 0 }),
            __gos_sql_Value::Int(n) => __gos_sql_params_push_int_raw(p, n),
            __gos_sql_Value::Float(f) => __gos_sql_params_push_float_raw(p, f),
            __gos_sql_Value::Text(s) => __gos_sql_params_push_text_raw(p, s),
            __gos_sql_Value::Blob(b) => __gos_sql_params_push_blob_raw(p, b),
        }
    }
    p
}
impl __gos_sql_Conn {
    fn execute(&mut self, sql: &String, params: &[__gos_sql_Value]) -> Result<i64, errors::Error> {
        let n = __gos_sql_conn_execute_raw(self.__handle, sql, __gos_sql_bind(params))
        if n < 0 { return Err(__gos_sql_err()) }
        Ok(n)
    }
    fn query(&mut self, sql: &String, params: &[__gos_sql_Value]) -> Result<__gos_sql_Rows, errors::Error> {
        let h = __gos_sql_conn_query_raw(self.__handle, sql, __gos_sql_bind(params))
        if h < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_Rows { __handle: h })
    }
    fn query_each(&mut self, sql: &String, params: &[__gos_sql_Value], f: Fn(__gos_sql_Row)) -> Result<(), errors::Error> {
        let h = __gos_sql_conn_query_raw(self.__handle, sql, __gos_sql_bind(params))
        if h < 0 { return Err(__gos_sql_err()) }
        let mut rows = __gos_sql_Rows { __handle: h }
        defer rows.close()
        loop {
            let next = rows.next_row()?
            let Some(row) = next else { break }
            f(row)
        }
        Ok(())
    }
    fn begin(&mut self) -> Result<__gos_sql_Tx, errors::Error> {
        let h = __gos_sql_conn_begin_raw(self.__handle)
        if h < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_Tx { __handle: h })
    }
    fn begin_with(&mut self, iso: __gos_sql_IsolationLevel) -> Result<__gos_sql_Tx, errors::Error> {
        let code = match iso {
            __gos_sql_IsolationLevel::Default => 0,
            __gos_sql_IsolationLevel::ReadUncommitted => 1,
            __gos_sql_IsolationLevel::ReadCommitted => 2,
            __gos_sql_IsolationLevel::RepeatableRead => 3,
            __gos_sql_IsolationLevel::Serializable => 4,
        }
        let h = __gos_sql_conn_begin_with_raw(self.__handle, code)
        if h < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_Tx { __handle: h })
    }
    fn ping(&mut self) -> Result<(), errors::Error> {
        if __gos_sql_conn_ping_raw(self.__handle) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn set_busy_timeout(&mut self, ms: i64) -> Result<(), errors::Error> {
        if __gos_sql_conn_set_busy_timeout_raw(self.__handle, ms) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn interrupt(&self) {
        let _ = __gos_sql_conn_interrupt_raw(self.__handle)
    }
    fn prepare(&mut self, sql: &String) -> Result<__gos_sql_Stmt, errors::Error> {
        let h = __gos_sql_conn_prepare_raw(self.__handle, sql)
        if h < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_Stmt { __handle: h })
    }
    fn copy_in(&mut self, sql: &String, data: &[u8]) -> Result<i64, errors::Error> {
        let n = __gos_sql_conn_copy_in_raw(self.__handle, sql, data)
        if n < 0 { return Err(__gos_sql_err()) }
        Ok(n)
    }
    fn copy_out(&mut self, sql: &String) -> Result<[u8], errors::Error> {
        if __gos_sql_conn_copy_out_run_raw(self.__handle, sql) < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_conn_copy_out_take_raw(self.__handle))
    }
    fn listen(&mut self, channel: &String) -> Result<(), errors::Error> {
        if __gos_sql_conn_listen_raw(self.__handle, channel) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn unlisten(&mut self, channel: &String) -> Result<(), errors::Error> {
        if __gos_sql_conn_unlisten_raw(self.__handle, channel) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn poll_notification(&mut self, timeout_ms: i64) -> Result<Option<__gos_sql_Notification>, errors::Error> {
        let s = __gos_sql_conn_poll_notification_raw(self.__handle, timeout_ms)
        if s < 0 { return Err(__gos_sql_err()) }
        if s == 0 { return Ok(None) }
        Ok(Some(__gos_sql_Notification {
            channel: __gos_sql_notification_channel_raw(self.__handle),
            payload: __gos_sql_notification_payload_raw(self.__handle),
            process_id: __gos_sql_notification_pid_raw(self.__handle),
        }))
    }
    fn close(&mut self) -> Result<(), errors::Error> {
        if __gos_sql_conn_close_raw(self.__handle) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
}
impl __gos_sql_Stmt {
    fn execute(&mut self, params: &[__gos_sql_Value]) -> Result<i64, errors::Error> {
        let n = __gos_sql_stmt_execute_raw(self.__handle, __gos_sql_bind(params))
        if n < 0 { return Err(__gos_sql_err()) }
        Ok(n)
    }
    fn query(&mut self, params: &[__gos_sql_Value]) -> Result<__gos_sql_Rows, errors::Error> {
        let h = __gos_sql_stmt_query_raw(self.__handle, __gos_sql_bind(params))
        if h < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_Rows { __handle: h })
    }
    fn close(&mut self) {
        let _ = __gos_sql_stmt_close_raw(self.__handle)
    }
}
impl __gos_sql_Pool {
    fn acquire(&self) -> Result<__gos_sql_Conn, errors::Error> {
        let h = __gos_sql_pool_get_raw(self.__handle)
        if h < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_Conn { __handle: h })
    }
    fn live(&self) -> i64 {
        __gos_sql_pool_live_raw(self.__handle)
    }
    fn idle(&self) -> i64 {
        __gos_sql_pool_idle_raw(self.__handle)
    }
    fn close_idle(&self) {
        let _ = __gos_sql_pool_close_idle_raw(self.__handle)
    }
}
fn __gos_sql_pool_open(driver: &String, url: &String, max: i64) -> Result<__gos_sql_Pool, errors::Error> {
    __gos_sql_pool_open_with(driver, url, 0, max, 30000, 300000, 1800000)
}
fn __gos_sql_pool_open_with(driver: &String, url: &String, min: i64, max: i64, acquire_ms: i64, idle_ms: i64, lifetime_ms: i64) -> Result<__gos_sql_Pool, errors::Error> {
    let h = __gos_sql_pool_new_raw(driver, url, min, max, acquire_ms, idle_ms, lifetime_ms)
    if h < 0 { return Err(__gos_sql_err()) }
    Ok(__gos_sql_Pool { __handle: h })
}
fn __gos_sql_migrate_up(db: &mut __gos_sql_Conn, dir: &String) -> Result<i64, errors::Error> {
    let n = __gos_sql_migrate_up_raw(db.__handle, dir)
    if n < 0 { return Err(__gos_sql_err()) }
    Ok(n)
}
fn __gos_sql_join(parts: &[String], sep: String) -> String {
    let mut out = ""
    let mut first = true
    for p in parts {
        if first {
            out = format!("{}", p)
            first = false
        } else {
            out = format!("{}{}{}", out, sep, p)
        }
    }
    out
}
fn __gos_sql_select_new(table: &String) -> __gos_sql_Select {
    __gos_sql_Select { table: table.clone(), cols: [], wheres: [], binds: [], order: "", lim: -1, off: -1 }
}
fn __gos_sql_copy_strs(xs: &[String]) -> [String] {
    let mut out: [String] = []
    for x in xs { out.push(x) }
    out
}
fn __gos_sql_copy_vals(xs: &[__gos_sql_Value]) -> [__gos_sql_Value] {
    let mut out: [__gos_sql_Value] = []
    for x in xs { out.push(x) }
    out
}
fn __gos_sql_is_simple_ident(s: &String) -> bool {
    let n = s.len()
    if n == 0 { return false }
    let mut i = 0
    let mut dots = 0
    let mut start = true
    while i < n {
        let b = s.byte_at(i)
        if b == 46 {
            if start { return false }
            dots += 1
            if dots > 1 { return false }
            start = true
            i += 1
            continue
        }
        let alpha = (b >= 65 && b <= 90) || (b >= 97 && b <= 122) || b == 95
        if start {
            if !alpha { return false }
            start = false
        } else {
            if !(alpha || (b >= 48 && b <= 57)) { return false }
        }
        i += 1
    }
    if start { return false }
    true
}
fn __gos_sql_quote_ident(ident: &String) -> String {
    if __gos_sql_is_simple_ident(ident) {
        return format!("{}", ident)
    }
    format!("\"{}\"", ident.replace("\"", "\"\""))
}
fn __gos_sql_quote_idents(xs: &[String]) -> [String] {
    let mut out: [String] = []
    for x in xs { out.push(__gos_sql_quote_ident(x)) }
    out
}
impl __gos_sql_Select {
    fn columns(&self, cols: &[String]) -> __gos_sql_Select {
        let mut c = __gos_sql_copy_strs(&self.cols)
        for x in cols { c.push(x) }
        __gos_sql_Select { table: self.table, cols: c, wheres: __gos_sql_copy_strs(&self.wheres), binds: __gos_sql_copy_vals(&self.binds), order: self.order, lim: self.lim, off: self.off }
    }
    fn where_eq(&self, column: &String, v: __gos_sql_Value) -> __gos_sql_Select {
        let mut b = __gos_sql_copy_vals(&self.binds)
        b.push(v)
        let mut w = __gos_sql_copy_strs(&self.wheres)
        w.push(format!("{} = ${}", __gos_sql_quote_ident(column), b.len()))
        __gos_sql_Select { table: self.table, cols: __gos_sql_copy_strs(&self.cols), wheres: w, binds: b, order: self.order, lim: self.lim, off: self.off }
    }
    fn order_by(&self, column: &String, ascending: bool) -> __gos_sql_Select {
        let dir = if ascending { "ASC" } else { "DESC" }
        __gos_sql_Select { table: self.table, cols: __gos_sql_copy_strs(&self.cols), wheres: __gos_sql_copy_strs(&self.wheres), binds: __gos_sql_copy_vals(&self.binds), order: format!("{} {}", __gos_sql_quote_ident(column), dir), lim: self.lim, off: self.off }
    }
    fn limit(&self, n: i64) -> __gos_sql_Select {
        __gos_sql_Select { table: self.table, cols: __gos_sql_copy_strs(&self.cols), wheres: __gos_sql_copy_strs(&self.wheres), binds: __gos_sql_copy_vals(&self.binds), order: self.order, lim: n, off: self.off }
    }
    fn offset(&self, n: i64) -> __gos_sql_Select {
        __gos_sql_Select { table: self.table, cols: __gos_sql_copy_strs(&self.cols), wheres: __gos_sql_copy_strs(&self.wheres), binds: __gos_sql_copy_vals(&self.binds), order: self.order, lim: self.lim, off: n }
    }
    fn params(&self) -> [__gos_sql_Value] {
        __gos_sql_copy_vals(&self.binds)
    }
    fn render(&self) -> String {
        let cols = if self.cols.len() == 0 { "*" } else { __gos_sql_join(&__gos_sql_quote_idents(&self.cols), ", ") }
        let mut out = format!("SELECT {} FROM {}", cols, __gos_sql_quote_ident(&self.table))
        if self.wheres.len() > 0 {
            out = format!("{} WHERE {}", out, __gos_sql_join(&self.wheres, " AND "))
        }
        if self.order != "" {
            out = format!("{} ORDER BY {}", out, self.order)
        }
        if self.lim >= 0 {
            out = format!("{} LIMIT {}", out, self.lim)
        }
        if self.off >= 0 {
            out = format!("{} OFFSET {}", out, self.off)
        }
        out
    }
}
impl __gos_sql_Rows {
    fn next_row(&mut self) -> Result<Option<__gos_sql_Row>, errors::Error> {
        let h = __gos_sql_rows_next_row_raw(self.__handle)
        if h < 0 { return Err(__gos_sql_err()) }
        if h == 0 { return Ok(None) }
        Ok(Some(__gos_sql_Row { __handle: h }))
    }
    fn close(&mut self) -> Result<(), errors::Error> {
        if __gos_sql_rows_close_raw(self.__handle) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn columns(&self) -> [String] {
        let joined = __gos_sql_rows_columns_raw(self.__handle)
        if joined == "" { return [] }
        joined.split(",")
    }
}
impl __gos_sql_Row {
    fn get_i64(&self, column: &String) -> Result<i64, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k != 2 {
            return Err(errors::newf("sql: column {} is not Int", column))
        }
        Ok(__gos_sql_row_get_i64_raw(self.__handle, column))
    }
    fn get_f64(&self, column: &String) -> Result<f64, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k != 3 && k != 2 {
            return Err(errors::newf("sql: column {} is not Float", column))
        }
        Ok(__gos_sql_row_get_f64_raw(self.__handle, column))
    }
    fn get_bool(&self, column: &String) -> Result<bool, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k != 1 {
            return Err(errors::newf("sql: column {} is not Bool", column))
        }
        Ok(__gos_sql_row_get_bool_raw(self.__handle, column) != 0)
    }
    fn get_text(&self, column: &String) -> Result<String, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k != 4 {
            return Err(errors::newf("sql: column {} is not Text", column))
        }
        Ok(__gos_sql_row_get_text_raw(self.__handle, column))
    }
    fn get_blob(&self, column: &String) -> Result<[u8], errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k != 5 {
            return Err(errors::newf("sql: column {} is not Blob", column))
        }
        Ok(__gos_sql_row_get_blob_raw(self.__handle, column))
    }
    fn get_opt_i64(&self, column: &String) -> Result<Option<i64>, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k == 0 { return Ok(None) }
        if k != 2 { return Err(errors::newf("sql: column {} is not Int", column)) }
        Ok(Some(__gos_sql_row_get_i64_raw(self.__handle, column)))
    }
    fn get_opt_f64(&self, column: &String) -> Result<Option<f64>, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k == 0 { return Ok(None) }
        if k != 3 && k != 2 { return Err(errors::newf("sql: column {} is not Float", column)) }
        Ok(Some(__gos_sql_row_get_f64_raw(self.__handle, column)))
    }
    fn get_opt_bool(&self, column: &String) -> Result<Option<bool>, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k == 0 { return Ok(None) }
        if k != 1 { return Err(errors::newf("sql: column {} is not Bool", column)) }
        Ok(Some(__gos_sql_row_get_bool_raw(self.__handle, column) != 0))
    }
    fn get_opt_text(&self, column: &String) -> Result<Option<String>, errors::Error> {
        let k = __gos_sql_row_kind_raw(self.__handle, column)
        __gos_sql_row_guard(k)?
        if k == 0 { return Ok(None) }
        if k != 4 { return Err(errors::newf("sql: column {} is not Text", column)) }
        Ok(Some(__gos_sql_row_get_text_raw(self.__handle, column)))
    }
    fn is_null(&self, column: &String) -> bool {
        __gos_sql_row_kind_raw(self.__handle, column) == 0
    }
    fn width(&self) -> i64 {
        __gos_sql_row_width_raw(self.__handle)
    }
}
impl __gos_sql_Tx {
    fn commit(&mut self) -> Result<(), errors::Error> {
        if __gos_sql_tx_commit_raw(self.__handle) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn rollback(&mut self) -> Result<(), errors::Error> {
        if __gos_sql_tx_rollback_raw(self.__handle) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn execute(&mut self, sql: &String) -> Result<i64, errors::Error> {
        let n = __gos_sql_tx_execute_raw(self.__handle, sql)
        if n < 0 { return Err(__gos_sql_err()) }
        Ok(n)
    }
    fn execute_params(&mut self, sql: &String, params: &[__gos_sql_Value]) -> Result<i64, errors::Error> {
        let n = __gos_sql_tx_execute_params_raw(self.__handle, sql, __gos_sql_bind(params))
        if n < 0 { return Err(__gos_sql_err()) }
        Ok(n)
    }
    fn query(&mut self, sql: &String, params: &[__gos_sql_Value]) -> Result<__gos_sql_Rows, errors::Error> {
        let h = __gos_sql_tx_query_params_raw(self.__handle, sql, __gos_sql_bind(params))
        if h < 0 { return Err(__gos_sql_err()) }
        Ok(__gos_sql_Rows { __handle: h })
    }
    fn savepoint(&mut self, name: &String) -> Result<(), errors::Error> {
        if __gos_sql_tx_savepoint_raw(self.__handle, name) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn release_savepoint(&mut self, name: &String) -> Result<(), errors::Error> {
        if __gos_sql_tx_release_savepoint_raw(self.__handle, name) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
    fn rollback_to_savepoint(&mut self, name: &String) -> Result<(), errors::Error> {
        if __gos_sql_tx_rollback_to_savepoint_raw(self.__handle, name) < 0 { return Err(__gos_sql_err()) }
        Ok(())
    }
}
"#;

/// Real-struct + wrapper source for the request/response-integrated
/// `std::http::{csrf, session, form, multipart}` surface. Pure
/// composition over the already-wired csrf / session / cookie / aead /
/// hmac / hex / url primitives, so it lowers natively on every tier.
const HTTP_SECURITY_WRAPPERS: &str = r##"
// ---- shared helpers ----
fn __gos_http_header_lookup(headers: &[(String, String)], name: &String) -> String {
    let target = name.to_lowercase()
    let mut found = ""
    for (k, v) in headers {
        if k.to_lowercase() == target { found = v }
    }
    found
}
fn __gos_http_bytes_to_str(b: &[u8]) -> String {
    let mut buf = bytes::Buffer::new()
    for x in b { buf.push(x) }
    buf.to_string()
}
fn __gos_http_first12(b: &[u8]) -> [u8] {
    let mut out: [u8] = []
    let mut i = 0
    while i < 12 { out.push(b[i]); i += 1 }
    out
}
fn __gos_http_trim_slash(s: &String) -> String {
    let n = s.len()
    if n > 0 && s.ends_with("/") { s.substring(0, n - 1) } else { s.substring(0, n) }
}
fn __gos_http_origin_host(origin: &String) -> String {
    let mut host: String = match origin.split_once("://") {
        Some((_, r)) => r,
        None => origin.substring(0, origin.len()),
    }
    match host.split_once("/") { Some((h, _)) => host = h, None => {} }
    match host.split_once("?") { Some((h, _)) => host = h, None => {} }
    match host.split_once("#") { Some((h, _)) => host = h, None => {} }
    host
}
fn __gos_http_origin_from_referer(referer: &String) -> String {
    match referer.split_once("://") {
        Some((scheme, _)) => scheme + "://" + &__gos_http_origin_host(referer),
        None => "",
    }
}
fn __gos_http_origins_equal(a: &String, b: &String) -> bool {
    __gos_http_trim_slash(a).to_lowercase() == __gos_http_trim_slash(b).to_lowercase()
}

// ---- csrf (request/response integrated) ----
struct __gos_http_csrf_Config {
    cookie_name: String,
    header_name: String,
    form_field: String,
    key: [u8],
    trusted_origins: [String],
    secure: bool,
    same_site: String,
    max_age_secs: i64,
    safe_methods: [String],
    exempt_prefixes: [String],
}
enum __gos_http_csrf_RouteAuth { BearerOnly, CookieSession, None }
fn __gos_http_csrf_config(key: [u8]) -> __gos_http_csrf_Config {
    __gos_http_csrf_Config {
        cookie_name: "gos_csrf",
        header_name: "X-CSRF-Token",
        form_field: "_csrf",
        key: key,
        trusted_origins: [],
        secure: true,
        same_site: "Lax",
        max_age_secs: 86400,
        safe_methods: ["GET", "HEAD", "OPTIONS", "TRACE"],
        exempt_prefixes: [],
    }
}
fn __gos_http_csrf_is_safe(config: &__gos_http_csrf_Config, method: &String) -> bool {
    let m = method.to_lowercase()
    let mut safe = false
    for s in config.safe_methods {
        if s.to_lowercase() == m { safe = true }
    }
    safe
}
fn __gos_http_csrf_extract_token(r: http::Request, config: &__gos_http_csrf_Config) -> Option<String> {
    let h = __gos_http_header_lookup(&r.headers, &config.header_name)
    if h != "" { return Some(h) }
    let ct = __gos_http_header_lookup(&r.headers, &"content-type")
    if ct.to_lowercase().starts_with("application/x-www-form-urlencoded") {
        let f = r.form_value(config.form_field)
        if f != "" { return Some(f) }
    }
    None
}
fn __gos_http_csrf_origin_allowed(r: http::Request, config: &__gos_http_csrf_Config) -> bool {
    let method = r.method()
    let is_safe = __gos_http_csrf_is_safe(config, &method)
    let origin = __gos_http_header_lookup(&r.headers, &"origin")
    let referer = __gos_http_header_lookup(&r.headers, &"referer")
    let mut candidate = ""
    if origin != "" {
        candidate = origin
    } else if referer != "" {
        let o = __gos_http_origin_from_referer(&referer)
        if o == "" { return is_safe }
        candidate = o
    } else {
        return is_safe
    }
    if config.trusted_origins.len() > 0 {
        let mut ok = false
        for t in config.trusted_origins {
            if __gos_http_origins_equal(&t, &candidate) { ok = true }
        }
        return ok
    }
    let host = __gos_http_header_lookup(&r.headers, &"host")
    if host == "" { return false }
    __gos_http_origin_host(&candidate).to_lowercase() == host.to_lowercase()
}
fn __gos_http_csrf_check(r: http::Request, route_auth: __gos_http_csrf_RouteAuth, config: &__gos_http_csrf_Config) -> Result<(), errors::Error> {
    match route_auth {
        __gos_http_csrf_RouteAuth::BearerOnly => return Ok(()),
        _ => {}
    }
    let method = r.method()
    if __gos_http_csrf_is_safe(config, &method) { return Ok(()) }
    if config.exempt_prefixes.len() > 0 {
        let path = r.path()
        for p in config.exempt_prefixes {
            if path.starts_with(&p) { return Ok(()) }
        }
    }
    if !__gos_http_csrf_origin_allowed(r, config) {
        return Err(errors::new("csrf: origin not allowed"))
    }
    let cookie_header = __gos_http_header_lookup(&r.headers, &"cookie")
    if cookie_header == "" { return Err(errors::new("csrf: missing cookie header")) }
    let pairs = http::cookie::parse_cookie_header(&cookie_header)
    let mut cookie_token = ""
    for (k, v) in pairs {
        if k == config.cookie_name { cookie_token = v }
    }
    if cookie_token == "" { return Err(errors::new("csrf: missing csrf cookie")) }
    let supplied = match __gos_http_csrf_extract_token(r, config) {
        Some(t) => t,
        None => return Err(errors::new("csrf: missing csrf token")),
    }
    http::csrf::verify_token(&cookie_token, &supplied, &config.key)
}
// A function that returns an `http::Response` must stay strictly
// straight-line: a branch (`if` / `match`) between the handle param and
// the `with_header` that mutates it loses the mutation on the compiled
// tiers, so every conditional that shapes the header string lives in a
// pure `String` helper and the response builder only concatenates calls.
fn __gos_http_max_age_attr(max_age_secs: i64) -> String {
    if max_age_secs > 0 { "; Max-Age=" + &format!("{}", max_age_secs) } else { "" }
}
fn __gos_http_secure_attr(secure: bool) -> String {
    if secure { "; Secure" } else { "" }
}
fn __gos_http_csrf_cookie_value(token: &String, config: &__gos_http_csrf_Config) -> String {
    let bare = http::cookie::serialize(&config.cookie_name, token)
    bare + "; Path=/" + &__gos_http_max_age_attr(config.max_age_secs)
        + &__gos_http_secure_attr(config.secure) + "; SameSite=" + &config.same_site
}
fn __gos_http_csrf_attach_cookie(resp: http::Response, token: &String, config: &__gos_http_csrf_Config) -> http::Response {
    let sc = __gos_http_csrf_cookie_value(token, config)
    resp.with_header("set-cookie", &sc)
}

// ---- session (signed + AES-256-GCM encrypted store) ----
struct __gos_http_session_Store {
    key: [u8],
    cookie_name: String,
    encrypted: bool,
    secure: bool,
    max_age_secs: i64,
}
fn __gos_http_session_signed(key: [u8]) -> __gos_http_session_Store {
    __gos_http_session_Store { key: key, cookie_name: "gos_session", encrypted: false, secure: true, max_age_secs: 86400 }
}
fn __gos_http_session_encrypted(key: [u8]) -> __gos_http_session_Store {
    __gos_http_session_Store { key: key, cookie_name: "gos_session", encrypted: true, secure: true, max_age_secs: 86400 }
}
fn __gos_http_session_seal(key: &[u8], data: &String) -> Result<String, errors::Error> {
    let pt = data.as_bytes()
    let mac = crypto::hmac::sha256_mac(key, &pt)
    let nonce = __gos_http_first12(&mac)
    let empty: [u8] = []
    let ct = crypto::aead::aes_256_gcm_seal(key, &nonce, &pt, &empty)?
    Ok(encoding::hex::encode(&nonce) + "." + &encoding::hex::encode(&ct))
}
fn __gos_http_session_open(key: &[u8], cookie: &String) -> Result<String, errors::Error> {
    let (n, c) = match cookie.split_once(".") {
        Some(p) => p,
        None => return Err(errors::new("session: bad framing")),
    }
    let nonce = encoding::hex::decode(&n)?
    let ct = encoding::hex::decode(&c)?
    let empty: [u8] = []
    let pt = crypto::aead::aes_256_gcm_open(key, &nonce, &ct, &empty)?
    Ok(__gos_http_bytes_to_str(&pt))
}
fn __gos_http_session_encode(store: &__gos_http_session_Store, data: &String) -> String {
    if store.encrypted {
        match __gos_http_session_seal(&store.key, data) {
            Ok(v) => v,
            Err(_) => "",
        }
    } else {
        http::session::sign(data, &store.key)
    }
}
fn __gos_http_session_cookie_value(store: &__gos_http_session_Store, data: &String) -> String {
    let cookie_val = __gos_http_session_encode(store, data)
    let bare = http::cookie::serialize(&store.cookie_name, &cookie_val)
    bare + "; Path=/; HttpOnly" + &__gos_http_max_age_attr(store.max_age_secs)
        + &__gos_http_secure_attr(store.secure) + "; SameSite=Lax"
}
// load / save are free functions, not methods: a `&self` method that
// returns the 2-word `Result` while also taking an opaque-handle arg
// (`http::Request`) miscompiles the call on the LLVM tier, whereas the
// free-function form is sound - and `session::load(store, req)` /
// `session::save(store, resp, data)` is also the data-first spelling.
fn __gos_http_session_save(store: &__gos_http_session_Store, resp: http::Response, data: &String) -> http::Response {
    let sc = __gos_http_session_cookie_value(store, data)
    resp.with_header("set-cookie", &sc)
}
fn __gos_http_session_cookie_raw(store: &__gos_http_session_Store, r: http::Request) -> String {
    let cookie_header = __gos_http_header_lookup(&r.headers, &"cookie")
    let pairs = http::cookie::parse_cookie_header(&cookie_header)
    let mut raw = ""
    for (k, v) in pairs {
        if k == store.cookie_name { raw = v }
    }
    raw
}
fn __gos_http_session_load(store: &__gos_http_session_Store, r: http::Request) -> Result<String, errors::Error> {
    let raw = __gos_http_session_cookie_raw(store, r)
    if raw == "" { return Err(errors::new("session: cookie not present")) }
    if store.encrypted {
        __gos_http_session_open(&store.key, &raw)
    } else {
        http::session::verify(&raw, &store.key)
    }
}
fn __gos_http_session_load_or_empty(store: &__gos_http_session_Store, r: http::Request) -> String {
    match __gos_http_session_load(store, r) {
        Ok(d) => d,
        Err(_) => "",
    }
}
fn __gos_http_session_with_session(store: &__gos_http_session_Store, r: http::Request, resp: http::Response, f: Fn(String) -> String) -> http::Response {
    let current = __gos_http_session_load_or_empty(store, r)
    let updated = f(current)
    __gos_http_session_save(store, resp, &updated)
}

// ---- form (application/x-www-form-urlencoded) ----
struct __gos_http_form_Form { pairs: [(String, String)] }
fn __gos_http_form_parse(body: &String) -> __gos_http_form_Form {
    let mut pairs: [(String, String)] = []
    let raw_pairs: [String] = strings::split(body, "&")
    for pair in raw_pairs {
        let p: String = pair
        if p == "" { continue }
        match p.split_once("=") {
            Some((k, v)) => pairs.push((url::query_unescape(&k), url::query_unescape(&v))),
            None => pairs.push((url::query_unescape(&p), "")),
        }
    }
    __gos_http_form_Form { pairs: pairs }
}
fn __gos_http_form_get(form: &__gos_http_form_Form, name: &String) -> String {
    for (k, v) in form.pairs {
        if k == *name { return v }
    }
    ""
}
fn __gos_http_form_get_all(form: &__gos_http_form_Form, name: &String) -> [String] {
    let mut out: [String] = []
    for (k, v) in form.pairs {
        if k == *name { out.push(v) }
    }
    out
}
fn __gos_http_form_has(form: &__gos_http_form_Form, name: &String) -> bool {
    for (k, _v) in form.pairs {
        if k == *name { return true }
    }
    false
}
fn __gos_http_form_count(form: &__gos_http_form_Form) -> i64 {
    form.pairs.len()
}

// ---- multipart (multipart/form-data, RFC 7578) ----
struct __gos_http_multipart_Part {
    name: String,
    filename: String,
    content_type: String,
    content: [u8],
}
fn __gos_http_multipart_boundary(content_type: &String) -> String {
    match content_type.split_once("boundary=") {
        Some((_, rest)) => {
            let raw = match rest.split_once(";") {
                Some((b, _)) => b,
                None => rest,
            }
            raw.trim_matches("\"")
        },
        None => "",
    }
}
fn __gos_http_multipart_header_value(head: &String, key: &String) -> String {
    let target = key.to_lowercase()
    let lines: [String] = strings::lines(head)
    for line in lines {
        let l: String = line
        match l.split_once(":") {
            Some((k, v)) => {
                if k.trim().to_lowercase() == target { return v.trim() }
            },
            None => {},
        }
    }
    ""
}
fn __gos_http_multipart_disp_param(disp: &String, key: &String) -> String {
    let needle = key.clone() + "=\""
    match disp.split_once(&needle) {
        Some((_, rest)) => {
            match rest.split_once("\"") {
                Some((val, _)) => val,
                None => "",
            }
        },
        None => "",
    }
}
fn __gos_http_multipart_parse(body: &[u8], boundary: &String) -> [__gos_http_multipart_Part] {
    let text = __gos_http_bytes_to_str(body)
    let delim = "--" + boundary
    let segments: [String] = strings::split(&text, &delim)
    let mut parts: [__gos_http_multipart_Part] = []
    for seg in segments {
        let s: String = seg
        let trimmed = s.trim()
        if trimmed == "" || trimmed == "--" { continue }
        match s.split_once("\r\n\r\n") {
            Some((head, rest)) => {
                let mut content_str: String = rest
                if content_str.ends_with("\r\n") {
                    content_str = content_str.substring(0, content_str.len() - 2)
                }
                let disp = __gos_http_multipart_header_value(&head, &"content-disposition")
                let name = __gos_http_multipart_disp_param(&disp, &"name")
                let filename = __gos_http_multipart_disp_param(&disp, &"filename")
                let ctype = __gos_http_multipart_header_value(&head, &"content-type")
                parts.push(__gos_http_multipart_Part {
                    name: name,
                    filename: filename,
                    content_type: ctype,
                    content: content_str.as_bytes(),
                })
            },
            None => {},
        }
    }
    parts
}
fn __gos_http_request_form_file(r: http::Request, name: &String) -> Option<__gos_http_multipart_Part> {
    let ct = __gos_http_header_lookup(&r.headers, &"content-type")
    let boundary = __gos_http_multipart_boundary(&ct)
    if boundary == "" { return None }
    let parts = __gos_http_multipart_parse(&r.raw_body, &boundary)
    for p in parts {
        if p.name == *name && p.filename != "" { return Some(p) }
    }
    None
}

"##;

/// Returns the field count when `e` is `Name(args)` and `Name` is a declared
/// tuple struct whose arity matches `args.len()`.
fn tuple_ctor_arity(e: &gossamer_ast::expr::Expr, arity: &HashMap<String, usize>) -> Option<usize> {
    use gossamer_ast::expr::ExprKind;
    let ExprKind::Call { callee, args } = &e.kind else {
        return None;
    };
    let ExprKind::Path(p) = &callee.kind else {
        return None;
    };
    if p.segments.len() != 1 {
        return None;
    }
    let n = arity.get(p.segments[0].name.name.as_str()).copied()?;
    (n == args.len()).then_some(n)
}

/// Rewrites a tuple-struct constructor call `Pt(a, b)` into the equivalent
/// struct literal `Pt { 0: a, 1: b }`, so construction (and `.N` access, with
/// tuple fields modelled as "0".."N-1") runs through the named-field
/// machinery on every tier. Only single-segment calls naming a declared
/// tuple struct with a matching argument count are rewritten.
pub fn rewrite_tuple_struct_ctors(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    let mut arity: HashMap<String, usize> = HashMap::new();
    for item in flatten_items(&sf.items) {
        if let ItemKind::Struct(decl) = &item.kind
            && let StructBody::Tuple(fields) = &decl.body
        {
            arity.insert(decl.name.name.clone(), fields.len());
        }
    }
    if arity.is_empty() {
        return;
    }
    TupleCtorRewriter { arity: &arity }.visit_source_file(sf);
}

struct TupleCtorRewriter<'a> {
    arity: &'a HashMap<String, usize>,
}

impl gossamer_ast::VisitorMut for TupleCtorRewriter<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::{ExprKind, StructExprField};
        gossamer_ast::visitor::walk_expr_mut(self, e);
        if tuple_ctor_arity(e, self.arity).is_none() {
            return;
        }
        let ExprKind::Call { callee, args } = std::mem::replace(&mut e.kind, ExprKind::Error)
        else {
            return;
        };
        let ExprKind::Path(path) = callee.kind else {
            return;
        };
        let fields = args
            .into_iter()
            .enumerate()
            .map(|(i, a)| StructExprField {
                name: gossamer_ast::Ident::new(i.to_string()),
                value: Some(a),
            })
            .collect();
        e.kind = ExprKind::Struct {
            path,
            fields,
            base: None,
        };
    }

    fn visit_pattern(&mut self, p: &mut gossamer_ast::pattern::Pattern) {
        use gossamer_ast::pattern::{FieldPattern, PatternKind};
        gossamer_ast::visitor::walk_pattern_mut(self, p);
        let convert = matches!(&p.kind, PatternKind::TupleStruct { path, elems }
            if path.segments.len() == 1
                && self
                    .arity
                    .get(path.segments[0].name.name.as_str())
                    .is_some_and(|&n| n == elems.len()));
        if !convert {
            return;
        }
        let PatternKind::TupleStruct { path, elems } =
            std::mem::replace(&mut p.kind, PatternKind::Wildcard)
        else {
            return;
        };
        let fields = elems
            .into_iter()
            .enumerate()
            .map(|(i, pat)| FieldPattern {
                name: gossamer_ast::Ident::new(i.to_string()),
                pattern: Some(pat),
            })
            .collect();
        p.kind = PatternKind::Struct {
            path,
            fields,
            rest: false,
        };
    }
}

/// Convenience wrapper that augments `source` then parses the
/// result against `file`. Returns the merged `SourceFile` and any
/// parse diagnostics. Callers MUST have already added the augmented
/// source to their source map (see `augment_source`) for span
/// resolution to work.
#[must_use]
pub fn parse_with_autoderive(source: &str, file: FileId) -> (SourceFile, Vec<ParseDiagnostic>) {
    let (mut sf, mut diags) = crate::parse_source_file(source, file);
    // The entry file is implicitly `fn main`: fold its bare top-level
    // statements into one (or report a conflict with an explicit `fn main`)
    // before the rewrites below, so the synthesized body receives the same
    // serde-turbofish and synthetic-use treatment as any function body. This
    // is the single compile/analysis parse entry - every codegen tier and the
    // LSP reach the implicit main through here. The REPL and `gos fmt`/`doc`/
    // `lint` use the raw `parse_source_file` and are unaffected.
    diags.extend(crate::entry_main::synthesize_entry_main(&mut sf));
    rewrite_tuple_struct_ctors(&mut sf);
    infer_serde_turbofish(&mut sf);
    desugar_sort_by_key(&mut sf);
    // Runs on the un-mangled AST: `rewrite_serde_generic_calls` below turns a
    // serde turbofish into a bare mangled name, erasing the type argument the
    // check keys on.
    diags.extend(serde_unsupported_field_diags(&sf));
    rewrite_serde_generic_calls(&mut sf);
    specialize_inline_for_generics(&mut sf);
    expand_typeinfo_loops(&mut sf);
    rewrite_type_info_calls(&mut sf);
    rewrite_json_set_mutators(&mut sf);
    rewrite_stdlib_struct_surface(&mut sf);
    inject_synthetic_uses(&mut sf, file);
    (sf, diags)
}

/// Reports serde turbofish calls (`to_json::<T>(v)`, `from_json::<T>(s)`, and
/// the toml/yaml forms) whose struct `T` has a field the synthesizer cannot
/// classify. Such a struct is silently skipped by `synthesize_serde_impls`, so
/// without this the call would surface only as an opaque unknown-name error.
/// Gated on use - a struct with an unsupported field is fine until it actually
/// flows through a serde call - and deduplicated to one diagnostic per struct,
/// pointing at the first offending field.
fn serde_unsupported_field_diags(sf: &SourceFile) -> Vec<ParseDiagnostic> {
    let struct_names: HashSet<String> = flatten_items(&sf.items)
        .into_iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Struct(decl)
                if matches!(&decl.body, StructBody::Named(_) | StructBody::Tuple(_)) =>
            {
                Some(decl.name.name.clone())
            }
            _ => None,
        })
        .collect();
    let decls: HashMap<&str, &StructDecl> = flatten_items(&sf.items)
        .into_iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Struct(decl) if decl.generics.params.is_empty() => {
                Some((decl.name.name.as_str(), decl))
            }
            _ => None,
        })
        .collect();
    // `augment_source` has already appended a `__gos_serde_to_json_<T>` for every
    // struct the synthesizer accepted (and the user may hand-provide one), so its
    // presence means the type is serializable - only its absence is a dropped
    // struct worth diagnosing.
    let synthesized: HashSet<&str> = sf
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Fn(f) => f.name.name.strip_prefix("__gos_serde_to_json_"),
            _ => None,
        })
        .collect();

    let mut diags = Vec::new();
    let mut reported: HashSet<String> = HashSet::new();
    for (op, ty_name) in collect_serde_turbofish_calls(sf) {
        if reported.contains(&ty_name) || synthesized.contains(ty_name.as_str()) {
            continue;
        }
        let Some(decl) = decls.get(ty_name.as_str()) else {
            continue;
        };
        let offending = match &decl.body {
            StructBody::Named(fields) => fields.iter().find_map(|f| {
                FieldKind::from_type(&f.ty, &struct_names)
                    .is_none()
                    .then(|| (f.name.name.clone(), ty_to_string(&f.ty), f.ty.span))
            }),
            StructBody::Tuple(fields) => fields.iter().enumerate().find_map(|(i, f)| {
                FieldKind::from_type(&f.ty, &struct_names)
                    .is_none()
                    .then(|| (i.to_string(), ty_to_string(&f.ty), f.ty.span))
            }),
            StructBody::Unit => None,
        };
        if let Some((field, field_ty, span)) = offending {
            reported.insert(ty_name.clone());
            diags.push(ParseDiagnostic::new(
                crate::ParseError::SerdeUnserializableField {
                    ty: ty_name,
                    field,
                    field_ty,
                    op,
                },
                span,
            ));
        }
    }
    diags
}

/// Collects `(op, type_name)` for every serde turbofish call in `sf`
/// (`to_json::<T>` / `from_json::<T>` and the toml/yaml forms, bare or
/// format-module-qualified), on the un-mangled AST.
fn collect_serde_turbofish_calls(sf: &SourceFile) -> Vec<(String, String)> {
    use gossamer_ast::Visitor;
    use gossamer_ast::expr::{Expr, ExprKind};
    use gossamer_ast::visitor::walk_expr;

    struct Collector {
        calls: Vec<(String, String)>,
    }
    impl Visitor for Collector {
        fn visit_expr(&mut self, expr: &Expr) {
            walk_expr(self, expr);
            let ExprKind::Call { callee, .. } = &expr.kind else {
                return;
            };
            let ExprKind::Path(path) = &callee.kind else {
                return;
            };
            let seg = match path.segments.len() {
                1 => &path.segments[0],
                2 => {
                    let head = path.segments[0].name.name.as_str();
                    let tail = path.segments[1].name.name.as_str();
                    if !matches!(
                        (head, tail),
                        ("yaml", "from_yaml" | "to_yaml") | ("toml", "from_toml" | "to_toml")
                    ) {
                        return;
                    }
                    &path.segments[1]
                }
                _ => return,
            };
            if !matches!(
                seg.name.name.as_str(),
                "to_json" | "from_json" | "to_toml" | "from_toml" | "to_yaml" | "from_yaml"
            ) || seg.generics.len() != 1
            {
                return;
            }
            let GenericArg::Type(ty) = &seg.generics[0] else {
                return;
            };
            let TypeKind::Path(tp) = &ty.kind else {
                return;
            };
            let Some(type_seg) = tp.segments.last() else {
                return;
            };
            self.calls
                .push((seg.name.name.clone(), type_seg.name.name.clone()));
        }
    }

    let mut collector = Collector { calls: Vec::new() };
    collector.visit_source_file(sf);
    collector.calls
}

/// Maps a stdlib `module::item` (matched on the last two segment
/// names) to the mangled name of the injected wrapper / struct, so
/// both `encoding::pem::decode` and the bare `pem::decode` map.
fn mangled_stdlib_name(parent: &str, item: &str) -> Option<&'static str> {
    match (parent, item) {
        ("pem", "decode") => Some("__gos_pem_decode"),
        ("pem", "decode_all") => Some("__gos_pem_decode_all"),
        ("pem", "encode") => Some("__gos_pem_encode"),
        ("pem", "Block") => Some("__gos_pem_Block"),
        ("x509", "parse_pem") => Some("__gos_x509_parse_pem"),
        ("x509", "CertInfo") => Some("__gos_x509_CertInfo"),
        ("fs", "metadata") => Some("__gos_fs_metadata"),
        ("fs", "Metadata") => Some("__gos_fs_Metadata"),
        // tar/zip `read` route through the struct wrapper; `write`
        // lowers directly (no struct), so it is NOT rewritten.
        ("tar", "read") => Some("__gos_tar_read"),
        ("tar", "TarEntry") => Some("__gos_tar_TarEntry"),
        ("zip", "read") => Some("__gos_zip_read"),
        ("zip", "ZipEntry") => Some("__gos_zip_ZipEntry"),
        ("sql", "open") => Some("__gos_sql_open"),
        ("sql", "drivers") => Some("__gos_sql_drivers"),
        ("sql", "Conn") => Some("__gos_sql_Conn"),
        ("sql", "Rows") => Some("__gos_sql_Rows"),
        ("sql", "Row") => Some("__gos_sql_Row"),
        ("sql", "Tx") => Some("__gos_sql_Tx"),
        ("sql", "Value") => Some("__gos_sql_Value"),
        ("sql", "IsolationLevel") => Some("__gos_sql_IsolationLevel"),
        ("sql", "Stmt") => Some("__gos_sql_Stmt"),
        ("sql", "Pool") => Some("__gos_sql_Pool"),
        ("sql", "Notification") => Some("__gos_sql_Notification"),
        ("sql", "Select") => Some("__gos_sql_Select"),
        ("sql", "pool_open") => Some("__gos_sql_pool_open"),
        ("sql", "pool_open_with") => Some("__gos_sql_pool_open_with"),
        ("sql", "migrate_up") => Some("__gos_sql_migrate_up"),
        // Gossamer-native driver dispatch: `register_native` captures
        // the driver's env + dispatch fn-address (custom MIR lowering,
        // hooked on the mangled leaf name); the `native_*` /
        // `value_*` helpers are the side-channel a `.gos` driver reads
        // and writes through.
        ("sql", "register_native") => Some("__gos_sql_register_native"),
        ("sql", "native_url") => Some("__gos_sql_native_url"),
        ("sql", "native_sql") => Some("__gos_sql_native_sql"),
        ("sql", "native_parent") => Some("__gos_sql_native_parent"),
        ("sql", "native_out_handle") => Some("__gos_sql_native_out_handle"),
        ("sql", "native_iso") => Some("__gos_sql_native_iso"),
        ("sql", "native_timeout") => Some("__gos_sql_native_timeout"),
        ("sql", "native_channel") => Some("__gos_sql_native_channel"),
        ("sql", "native_param_count") => Some("__gos_sql_native_param_count"),
        ("sql", "native_param") => Some("__gos_sql_native_param"),
        ("sql", "native_data") => Some("__gos_sql_native_data"),
        ("sql", "native_push_column") => Some("__gos_sql_native_push_column"),
        ("sql", "native_push_value") => Some("__gos_sql_native_push_value"),
        ("sql", "native_row_ready") => Some("__gos_sql_native_row_ready"),
        ("sql", "native_set_error") => Some("__gos_sql_native_set_error"),
        ("sql", "native_emit_bytes") => Some("__gos_sql_native_emit_bytes"),
        ("sql", "native_set_notification") => Some("__gos_sql_native_set_notification"),
        ("sql", "native_set_handle") => Some("__gos_sql_native_set_handle"),
        ("sql", "native_handle") => Some("__gos_sql_native_handle"),
        ("sql", "value_null") => Some("__gos_sql_native_value_null"),
        ("sql", "value_bool") => Some("__gos_sql_native_value_bool"),
        ("sql", "value_int") => Some("__gos_sql_native_value_int"),
        ("sql", "value_float") => Some("__gos_sql_native_value_float"),
        ("sql", "value_text") => Some("__gos_sql_native_value_text"),
        ("sql", "value_blob") => Some("__gos_sql_native_value_blob"),
        ("sql", "value_kind") => Some("__gos_sql_native_value_kind"),
        ("sql", "value_int_of") => Some("__gos_sql_native_value_int_of"),
        ("sql", "value_float_of") => Some("__gos_sql_native_value_float_of"),
        ("sql", "value_text_of") => Some("__gos_sql_native_value_text_of"),
        ("sql", "value_blob_of") => Some("__gos_sql_native_value_blob_of"),
        // Channel-returning timer: `time::after(d)` fires on a goroutine that
        // sleeps then sends, so the result is usable in `select` / `while let`.
        ("time", "after") => Some("__gos_time_after"),
        // std::http::csrf request/response-integrated surface.
        ("csrf", "Config") => Some("__gos_http_csrf_Config"),
        ("csrf", "config") => Some("__gos_http_csrf_config"),
        ("csrf", "RouteAuth") => Some("__gos_http_csrf_RouteAuth"),
        ("csrf", "extract_token") => Some("__gos_http_csrf_extract_token"),
        ("csrf", "origin_allowed") => Some("__gos_http_csrf_origin_allowed"),
        ("csrf", "check") => Some("__gos_http_csrf_check"),
        ("csrf", "attach_cookie") => Some("__gos_http_csrf_attach_cookie"),
        // std::http::session signed + AES-GCM store surface.
        ("session", "Store") => Some("__gos_http_session_Store"),
        ("session", "signed") => Some("__gos_http_session_signed"),
        ("session", "encrypted") => Some("__gos_http_session_encrypted"),
        ("session", "save") => Some("__gos_http_session_save"),
        ("session", "load") => Some("__gos_http_session_load"),
        ("session", "with_session") => Some("__gos_http_session_with_session"),
        // std::http::form url-encoded parser.
        ("form", "Form") => Some("__gos_http_form_Form"),
        ("form", "parse") => Some("__gos_http_form_parse"),
        ("form", "get") => Some("__gos_http_form_get"),
        ("form", "get_all") => Some("__gos_http_form_get_all"),
        ("form", "has") => Some("__gos_http_form_has"),
        ("form", "count") => Some("__gos_http_form_count"),
        // std::http::multipart (multipart/form-data) parser.
        ("multipart", "Part") => Some("__gos_http_multipart_Part"),
        ("multipart", "parse") => Some("__gos_http_multipart_parse"),
        ("multipart", "boundary") => Some("__gos_http_multipart_boundary"),
        _ => None,
    }
}

/// Collapses the `csrf::RouteAuth::X` enum-variant and the
/// `form::Form::parse` associated-function paths onto their injected
/// names, guarded on the `csrf` / `form` head so a user's own
/// `RouteAuth::X` / `Form::parse` is left alone. Returns true when it
/// rewrote `path`.
fn collapse_http_security_path(path: &mut gossamer_ast::PathExpr) -> bool {
    let n = path.segments.len();
    if n < 3 {
        return false;
    }
    if path.segments[n - 3].name.name.as_str() == "csrf"
        && path.segments[n - 2].name.name.as_str() == "RouteAuth"
    {
        let variant = std::mem::replace(
            &mut path.segments[n - 1],
            gossamer_ast::PathSegment::new(""),
        );
        path.segments = vec![
            gossamer_ast::PathSegment::new("__gos_http_csrf_RouteAuth"),
            variant,
        ];
        return true;
    }
    if path.segments[n - 3].name.name.as_str() == "form"
        && path.segments[n - 2].name.name.as_str() == "Form"
        && path.segments[n - 1].name.name.as_str() == "parse"
    {
        path.segments = vec![gossamer_ast::PathSegment::new("__gos_http_form_parse")];
        return true;
    }
    false
}

/// Rewrites `recv.form_file(name)` into a call of the injected
/// `__gos_http_request_form_file(recv, name)` free wrapper. The
/// `form_file` source marker is what pulled the multipart wrappers in,
/// so the rewrite only ever fires when they are present.
fn rewrite_form_file_method(expr: &mut gossamer_ast::expr::Expr) {
    use gossamer_ast::expr::{Expr, ExprKind};
    let span = expr.span;
    let ExprKind::MethodCall {
        receiver, mut args, ..
    } = std::mem::replace(&mut expr.kind, ExprKind::Tuple(Vec::new()))
    else {
        return;
    };
    let mut call_args = Vec::with_capacity(2);
    call_args.push(*receiver);
    call_args.append(&mut args);
    let callee = Expr {
        id: NodeId::DUMMY,
        span,
        kind: ExprKind::Path(gossamer_ast::PathExpr {
            segments: vec![gossamer_ast::PathSegment::new(
                "__gos_http_request_form_file",
            )],
        }),
    };
    expr.kind = ExprKind::Call {
        callee: Box::new(callee),
        args: call_args,
    };
}

/// Reports whether `e` is a place expression cheap and side-effect-free
/// to evaluate more than once (a name, field, or constant-indexed chain).
fn is_reevaluable_place(e: &gossamer_ast::expr::Expr) -> bool {
    use gossamer_ast::expr::ExprKind;
    match &e.kind {
        ExprKind::Path(_) => true,
        ExprKind::FieldAccess { receiver, .. } => is_reevaluable_place(receiver),
        ExprKind::Index { base, index } => {
            is_reevaluable_place(base)
                && matches!(index.kind, ExprKind::Literal(_) | ExprKind::Path(_))
        }
        _ => false,
    }
}

/// Wraps `place = set_call; place` into a value-yielding block so a
/// mutator rewrite both persists the update and stays usable in
/// expression position.
fn writeback_block(
    place: gossamer_ast::expr::Expr,
    set_call: gossamer_ast::expr::Expr,
    span: gossamer_lex::Span,
) -> gossamer_ast::expr::ExprKind {
    use gossamer_ast::common::AssignOp;
    use gossamer_ast::expr::{Block, Expr, ExprKind};
    use gossamer_ast::stmt::{Stmt, StmtKind};
    let assign = Expr {
        id: NodeId::DUMMY,
        span,
        kind: ExprKind::Assign {
            op: AssignOp::Assign,
            place: Box::new(place.clone()),
            value: Box::new(set_call),
        },
    };
    ExprKind::Block(Block {
        stmts: vec![Stmt::new(
            NodeId::DUMMY,
            span,
            StmtKind::Expr {
                expr: Box::new(assign),
                has_semi: true,
            },
        )],
        tail: Some(Box::new(place)),
        synthetic: true,
        is_arena: false,
        is_comptime: false,
    })
}

/// Rewrites a `json::set(&mut place, key, value)` mutator call into
/// `{ place = json::set(place, key, value); place }` so the in-place
/// update persists. `json::set` is a functional helper (it returns a
/// new `json::Value`); the `&mut place` spelling reads as a mutation,
/// so the returned value must be written back to `place`. The block
/// also yields the updated value, keeping the call usable in
/// expression position. The functional form (`let x = json::set(obj,
/// k, v)`, no `&mut`) is left untouched. Returns `true` when it fired.
fn rewrite_json_set_mutator(expr: &mut gossamer_ast::expr::Expr) -> bool {
    use gossamer_ast::common::UnaryOp;
    use gossamer_ast::expr::{Expr, ExprKind};

    let ExprKind::Call { callee, args } = &expr.kind else {
        return false;
    };
    if args.len() != 3 {
        return false;
    }
    let ExprKind::Path(path) = &callee.kind else {
        return false;
    };
    let segs: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
    let is_json_set = matches!(
        segs.as_slice(),
        ["json", "set"] | ["encoding", "json", "set"] | ["std", "encoding", "json", "set"]
    );
    if !is_json_set {
        return false;
    }
    // First arg must be `&mut place` / `&place` over a place expression
    // that is safe to re-evaluate (a name, field, or index chain - no
    // calls). Anything else keeps the functional semantics.
    let ExprKind::Unary {
        op: UnaryOp::RefMut | UnaryOp::RefShared,
        operand,
    } = &args[0].kind
    else {
        return false;
    };
    if !is_reevaluable_place(operand) {
        return false;
    }
    let place = (**operand).clone();
    let span = expr.span;
    let ExprKind::Call { callee, mut args } =
        std::mem::replace(&mut expr.kind, ExprKind::Tuple(Vec::new()))
    else {
        unreachable!("matched Call above");
    };
    // Replace the `&mut place` first argument with the bare place so
    // the inner call lowers through the ordinary functional path.
    args[0] = place.clone();
    let set_call = Expr {
        id: NodeId::DUMMY,
        span,
        kind: ExprKind::Call { callee, args },
    };
    expr.kind = writeback_block(place, set_call, span);
    true
}

/// Walks the program rewriting every `json::set(&mut place, k, v)`
/// mutator call into a value-yielding write-back block (see
/// `rewrite_json_set_mutator`).
pub fn rewrite_json_set_mutators(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    use gossamer_ast::expr::Expr;
    use gossamer_ast::visitor::walk_expr_mut;

    struct Rewriter;
    impl VisitorMut for Rewriter {
        fn visit_expr(&mut self, expr: &mut Expr) {
            walk_expr_mut(self, expr);
            rewrite_json_set_mutator(expr);
        }
    }
    Rewriter.visit_source_file(sf);
}

/// Redirects the user-facing stdlib struct surface
/// (`encoding::pem::decode(..)`, the `pem::Block { .. }` literal,
/// `pem::Block` type annotations) onto the injected real-struct
/// wrappers. Mirrors `rewrite_serde_generic_calls` but covers
/// multi-segment module paths in call, struct-literal, and type
/// positions.
pub fn rewrite_stdlib_struct_surface(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    use gossamer_ast::expr::{Expr, ExprKind};
    use gossamer_ast::ty::{Type, TypeKind};
    use gossamer_ast::visitor::{walk_expr_mut, walk_type_mut};

    fn collapse_expr(path: &mut gossamer_ast::PathExpr) {
        let n = path.segments.len();
        if n < 2 {
            return;
        }
        // Enum-variant paths: `sql::Value::Int(..)` /
        // `sql::IsolationLevel::Serializable` collapse to the
        // injected enum + variant, guarded on the `sql` segment so
        // a user's own `Value::Int` is untouched.
        if n >= 3 && path.segments[n - 3].name.name.as_str() == "sql" {
            let enum_name = path.segments[n - 2].name.name.as_str();
            if let Some(mangled) = match enum_name {
                "Value" => Some("__gos_sql_Value"),
                "IsolationLevel" => Some("__gos_sql_IsolationLevel"),
                _ => None,
            } {
                let variant = std::mem::replace(
                    &mut path.segments[n - 1],
                    gossamer_ast::PathSegment::new(""),
                );
                path.segments = vec![gossamer_ast::PathSegment::new(mangled), variant];
                return;
            }
            // Associated free functions: `sql::Select::new(..)`,
            // `sql::Pool::open(..)`, `sql::migrate::up(..)` collapse
            // to their injected free-fn wrappers.
            let assoc = (enum_name, path.segments[n - 1].name.name.as_str());
            if let Some(mangled) = match assoc {
                ("Select", "new") => Some("__gos_sql_select_new"),
                ("Pool", "open") => Some("__gos_sql_pool_open"),
                ("Pool", "open_with") => Some("__gos_sql_pool_open_with"),
                ("migrate", "up") => Some("__gos_sql_migrate_up"),
                _ => None,
            } {
                path.segments = vec![gossamer_ast::PathSegment::new(mangled)];
                return;
            }
        }
        if collapse_http_security_path(path) {
            return;
        }
        if let Some(name) = mangled_stdlib_name(
            path.segments[n - 2].name.name.as_str(),
            path.segments[n - 1].name.name.as_str(),
        ) {
            let mut seg = gossamer_ast::PathSegment::new(name);
            seg.generics = std::mem::take(&mut path.segments[n - 1].generics);
            path.segments = vec![seg];
        }
    }

    fn collapse_type(path: &mut gossamer_ast::ty::TypePath) {
        let n = path.segments.len();
        if n < 2 {
            return;
        }
        // `sql::Error` is the standard error type at the language
        // level - redirect to `errors::Error`.
        if path.segments[n - 2].name.name.as_str() == "sql"
            && path.segments[n - 1].name.name.as_str() == "Error"
        {
            path.segments = vec![
                gossamer_ast::ty::TypePathSegment::new("errors"),
                gossamer_ast::ty::TypePathSegment::new("Error"),
            ];
            return;
        }
        if let Some(name) = mangled_stdlib_name(
            path.segments[n - 2].name.name.as_str(),
            path.segments[n - 1].name.name.as_str(),
        ) {
            let mut seg = gossamer_ast::ty::TypePathSegment::new(name);
            seg.generics = std::mem::take(&mut path.segments[n - 1].generics);
            path.segments = vec![seg];
        }
    }

    struct Rewriter;
    impl VisitorMut for Rewriter {
        fn visit_expr(&mut self, expr: &mut Expr) {
            walk_expr_mut(self, expr);
            // `r.form_file(name)` is a request/multipart convenience that
            // composes the injected multipart parser, so it lowers to the
            // free wrapper `__gos_http_request_form_file(r, name)` rather
            // than a runtime method.
            if let ExprKind::MethodCall { name, args, .. } = &expr.kind
                && name.name.as_str() == "form_file"
                && args.len() == 1
            {
                rewrite_form_file_method(expr);
                return;
            }
            match &mut expr.kind {
                ExprKind::Call { callee, .. } => {
                    if let ExprKind::Path(path) = &mut callee.kind {
                        collapse_expr(path);
                    }
                }
                ExprKind::Path(path) => collapse_expr(path),
                ExprKind::Struct { path, .. } => collapse_expr(path),
                _ => {}
            }
        }
        fn visit_type(&mut self, ty: &mut Type) {
            walk_type_mut(self, ty);
            if let TypeKind::Path(tp) = &mut ty.kind {
                collapse_type(tp);
            }
        }
    }
    Rewriter.visit_source_file(sf);
}

/// Desugars `xs.sort_by_key(f)` / `xs.sort_by_key_desc(f)` method calls into
/// `xs.sort_by(|a, b| { let ka = f(a); let kb = f(b); cmp })`, where `cmp`
/// orders by the key with `<`. Because the key is compared with the `<`
/// operator (which works on scalars, strings, and tuples on every tier),
/// multi-key sorting via a tuple key - `xs.sort_by_key(|e| (e.a, e.b))` -
/// works without pinning the key to `i64`. Source-to-source, so it compiles
/// bit-identically on all tiers.
use gossamer_ast::VisitorMut as _;
use gossamer_ast::expr::Expr as SbkExpr;
use gossamer_ast::expr::ExprKind as SbkExprKind;
use gossamer_ast::pattern::Pattern as SbkPattern;
use gossamer_ast::stmt::Stmt as SbkStmt;
use gossamer_lex::Span as SbkSpan;

const SBK_KEY: &str = "__gos_sbk_key";
const SBK_A: &str = "__gos_sbk_a";
const SBK_B: &str = "__gos_sbk_b";
const SBK_KA: &str = "__gos_sbk_ka";
const SBK_KB: &str = "__gos_sbk_kb";

/// AST builder for the `sort_by_key` desugar. Holds a fresh-id counter -
/// the checker records inferred types per `NodeId`, so every synthesized
/// node needs a unique id rather than `DUMMY`.
struct SbkBuilder {
    next: u32,
}

impl SbkBuilder {
    fn id(&mut self) -> NodeId {
        let id = NodeId::from_raw(self.next);
        self.next += 1;
        id
    }
    fn expr(&mut self, span: SbkSpan, kind: SbkExprKind) -> SbkExpr {
        SbkExpr {
            id: self.id(),
            span,
            kind,
        }
    }
    fn path(&mut self, name: &str, span: SbkSpan) -> SbkExpr {
        self.expr(
            span,
            SbkExprKind::Path(gossamer_ast::expr::PathExpr {
                segments: vec![gossamer_ast::expr::PathSegment::new(name)],
            }),
        )
    }
    fn int_lit(&mut self, n: i64, span: SbkSpan) -> SbkExpr {
        self.expr(
            span,
            SbkExprKind::Literal(gossamer_ast::expr::Literal::Int(n.to_string())),
        )
    }
    fn ident_pat(&mut self, name: &str, span: SbkSpan) -> SbkPattern {
        SbkPattern {
            id: self.id(),
            span,
            kind: gossamer_ast::pattern::PatternKind::Ident {
                mutability: gossamer_ast::common::Mutability::Immutable,
                name: gossamer_ast::Ident::new(name),
                subpattern: None,
            },
        }
    }
    fn let_stmt(&mut self, name: &str, init: SbkExpr, span: SbkSpan) -> SbkStmt {
        let pattern = self.ident_pat(name, span);
        SbkStmt::new(
            self.id(),
            span,
            gossamer_ast::stmt::StmtKind::Let {
                pattern,
                ty: None,
                init: Some(Box::new(init)),
            },
        )
    }
    fn block(&mut self, stmts: Vec<SbkStmt>, tail: SbkExpr, span: SbkSpan) -> SbkExpr {
        self.expr(
            span,
            SbkExprKind::Block(gossamer_ast::expr::Block {
                stmts,
                tail: Some(Box::new(tail)),
                synthetic: true,
                is_arena: false,
                is_comptime: false,
            }),
        )
    }
    /// `path(left) < path(right)`.
    fn cmp_paths(&mut self, left: &str, right: &str, span: SbkSpan) -> SbkExpr {
        let left_e = self.path(left, span);
        let right_e = self.path(right, span);
        self.expr(
            span,
            SbkExprKind::Binary {
                op: gossamer_ast::common::BinaryOp::Lt,
                lhs: Box::new(left_e),
                rhs: Box::new(right_e),
            },
        )
    }
    fn key_call(&mut self, arg_name: &str, span: SbkSpan) -> SbkExpr {
        let callee = self.path(SBK_KEY, span);
        let arg = self.path(arg_name, span);
        self.expr(
            span,
            SbkExprKind::Call {
                callee: Box::new(callee),
                args: vec![arg],
            },
        )
    }
    /// Deep-clones `body` with fresh node ids, renaming references to the key
    /// closure's parameter `param` to `to`. Inlining the key body (rather than
    /// capturing the key closure and calling it) sidesteps a native-tier
    /// quirk where a comparator that captures another closure and returns an
    /// aggregate key misbehaves when invoked through the sort callback ABI.
    fn clone_inline(&mut self, body: &SbkExpr, param: &str, to: &str) -> SbkExpr {
        let mut cloned = body.clone();
        SbkRenum { d: self, param, to }.visit_expr(&mut cloned);
        cloned
    }
    /// Builds the `|a, b| { let ka = ...; let kb = ...; cmp }` comparator that
    /// orders elements by their key with `<` (flipped for `desc`).
    fn comparator(
        &mut self,
        first_key: SbkStmt,
        second_key: SbkStmt,
        desc: bool,
        span: SbkSpan,
    ) -> SbkExpr {
        use gossamer_ast::expr::ClosureParam;
        let lt_forward = self.cmp_paths(SBK_KA, SBK_KB, span);
        let lt_backward = self.cmp_paths(SBK_KB, SBK_KA, span);
        let (first, second) = if desc {
            (lt_backward, lt_forward)
        } else {
            (lt_forward, lt_backward)
        };
        let one = self.int_lit(1, span);
        let zero = self.int_lit(0, span);
        let neg = self.int_lit(-1, span);
        let then_one = self.block(vec![], one, span);
        let then_zero = self.block(vec![], zero, span);
        let then_neg = self.block(vec![], neg, span);
        let inner_if = self.expr(
            span,
            SbkExprKind::If {
                condition: Box::new(second),
                then_branch: Box::new(then_one),
                else_branch: Some(Box::new(then_zero)),
            },
        );
        let if_chain = self.expr(
            span,
            SbkExprKind::If {
                condition: Box::new(first),
                then_branch: Box::new(then_neg),
                else_branch: Some(Box::new(inner_if)),
            },
        );
        let cmp_body = self.block(vec![first_key, second_key], if_chain, span);
        let pat_a = self.ident_pat(SBK_A, span);
        let pat_b = self.ident_pat(SBK_B, span);
        self.expr(
            span,
            SbkExprKind::Closure {
                params: vec![
                    ClosureParam {
                        pattern: pat_a,
                        ty: None,
                    },
                    ClosureParam {
                        pattern: pat_b,
                        ty: None,
                    },
                ],
                ret: None,
                body: Box::new(cmp_body),
            },
        )
    }
    /// Builds the `|a, b| { ... }` comparator from an inline-able key `body`,
    /// comparing element-wise so a tuple key with `Reverse(x)` members can mix
    /// ascending and descending. For each element (or the whole key when it is
    /// not a tuple literal): if it is `Reverse(inner)` the element is ordered
    /// descending; `desc` flips every element. Each element value is inlined
    /// (the key param substituted for `a` / `b`) and ordered with `<`, which
    /// works on scalars, strings, and tuples on every tier.
    fn element_wise_comparator(
        &mut self,
        body: &SbkExpr,
        param: &str,
        desc: bool,
        span: SbkSpan,
    ) -> SbkExpr {
        use gossamer_ast::expr::ClosureParam;
        // Decompose a tuple-literal key into its elements; any other key is a
        // single element.
        let elems: Vec<SbkExpr> = match &body.kind {
            SbkExprKind::Tuple(es) => es.clone(),
            _ => vec![body.clone()],
        };
        let zero = self.int_lit(0, span);
        let mut chain = self.block(vec![], zero, span);
        for (i, elem) in elems.iter().enumerate().rev() {
            let (inner, reversed) = sbk_strip_reverse(elem);
            let flip = reversed ^ desc;
            let a_name = format!("__gos_sbk_a{i}");
            let b_name = format!("__gos_sbk_b{i}");
            let a_val = self.clone_inline(inner, param, SBK_A);
            let b_val = self.clone_inline(inner, param, SBK_B);
            let let_a = self.let_stmt(&a_name, a_val, span);
            let let_b = self.let_stmt(&b_name, b_val, span);
            // The "this element is less" direction: `a < b` ascending, or
            // `b < a` when this element is reversed.
            let (less, greater) = if flip {
                (
                    self.cmp_paths(&b_name, &a_name, span),
                    self.cmp_paths(&a_name, &b_name, span),
                )
            } else {
                (
                    self.cmp_paths(&a_name, &b_name, span),
                    self.cmp_paths(&b_name, &a_name, span),
                )
            };
            let neg = self.int_lit(-1, span);
            let one = self.int_lit(1, span);
            let then_neg = self.block(vec![], neg, span);
            let then_one = self.block(vec![], one, span);
            let greater_if = self.expr(
                span,
                SbkExprKind::If {
                    condition: Box::new(greater),
                    then_branch: Box::new(then_one),
                    else_branch: Some(Box::new(chain)),
                },
            );
            let less_if = self.expr(
                span,
                SbkExprKind::If {
                    condition: Box::new(less),
                    then_branch: Box::new(then_neg),
                    else_branch: Some(Box::new(greater_if)),
                },
            );
            chain = self.block(vec![let_a, let_b], less_if, span);
        }
        let pat_a = self.ident_pat(SBK_A, span);
        let pat_b = self.ident_pat(SBK_B, span);
        self.expr(
            span,
            SbkExprKind::Closure {
                params: vec![
                    ClosureParam {
                        pattern: pat_a,
                        ty: None,
                    },
                    ClosureParam {
                        pattern: pat_b,
                        ty: None,
                    },
                ],
                ret: None,
                body: Box::new(chain),
            },
        )
    }
    fn rewrite(&mut self, e: &mut SbkExpr, desc: bool) {
        let span = e.span;
        let SbkExprKind::MethodCall {
            receiver, mut args, ..
        } = std::mem::replace(&mut e.kind, SbkExprKind::Tuple(Vec::new()))
        else {
            return;
        };
        let key_fn = args.pop().expect("checked len == 1");
        // Inline a closure-literal key (`|e| body`) by substituting its single
        // ident param; non-closure keys (a fn name / variable) bind and call.
        let inline_param = sbk_inline_param(&key_fn);
        let (outer_stmts, comparator) = if let Some(param) = inline_param {
            let body = match &key_fn.kind {
                SbkExprKind::Closure { body, .. } => body.clone(),
                _ => unreachable!("inline_param implies a closure"),
            };
            // Build a Reverse-aware element-wise comparator straight from the
            // key body: a tuple key compares lexicographically, with each
            // `Reverse(x)` element ordered descending (and the whole key
            // reversed for `sort_by_key_desc`).
            (
                Vec::new(),
                self.element_wise_comparator(&body, &param, desc, span),
            )
        } else {
            let let_key = self.let_stmt(SBK_KEY, key_fn, span);
            let call_a = self.key_call(SBK_A, span);
            let call_b = self.key_call(SBK_B, span);
            let first_key = self.let_stmt(SBK_KA, call_a, span);
            let second_key = self.let_stmt(SBK_KB, call_b, span);
            (
                vec![let_key],
                self.comparator(first_key, second_key, desc, span),
            )
        };
        let sort_call = self.expr(
            span,
            SbkExprKind::MethodCall {
                receiver,
                name: gossamer_ast::Ident::new("sort_by"),
                generics: Vec::new(),
                args: vec![comparator],
            },
        );
        e.kind = SbkExprKind::Block(gossamer_ast::expr::Block {
            stmts: outer_stmts,
            tail: Some(Box::new(sort_call)),
            synthetic: true,
            is_arena: false,
            is_comptime: false,
        });
    }
}

impl gossamer_ast::VisitorMut for SbkBuilder {
    fn visit_expr(&mut self, e: &mut SbkExpr) {
        gossamer_ast::visitor::walk_expr_mut(self, e);
        let SbkExprKind::MethodCall { name, args, .. } = &e.kind else {
            return;
        };
        let desc = match name.name.as_str() {
            "sort_by_key" => false,
            "sort_by_key_desc" => true,
            _ => return,
        };
        if args.len() == 1 {
            self.rewrite(e, desc);
        }
    }
}

/// Renumbers a cloned key body's node ids and renames its parameter.
struct SbkRenum<'a> {
    d: &'a mut SbkBuilder,
    param: &'a str,
    to: &'a str,
}

impl gossamer_ast::VisitorMut for SbkRenum<'_> {
    fn visit_expr(&mut self, e: &mut SbkExpr) {
        e.id = self.d.id();
        if let SbkExprKind::Path(p) = &mut e.kind
            && p.segments.len() == 1
            && p.segments[0].name.name == self.param
        {
            p.segments[0].name = gossamer_ast::Ident::new(self.to);
        }
        gossamer_ast::visitor::walk_expr_mut(self, e);
    }
    fn visit_pattern(&mut self, p: &mut SbkPattern) {
        p.id = self.d.id();
        gossamer_ast::visitor::walk_pattern_mut(self, p);
    }
    fn visit_stmt(&mut self, s: &mut SbkStmt) {
        s.id = self.d.id();
        gossamer_ast::visitor::walk_stmt_mut(self, s);
    }
}

/// If `e` is a `Reverse(inner)` call (the sort-key descending marker), returns
/// `(inner, true)`; otherwise `(e, false)`. `Reverse` is recognized only here,
/// inside a `sort_by_key` key, where it is stripped before the body is inlined -
/// so it never needs to be a real constructible type.
fn sbk_strip_reverse(e: &SbkExpr) -> (&SbkExpr, bool) {
    if let SbkExprKind::Call { callee, args } = &e.kind
        && args.len() == 1
        && let SbkExprKind::Path(p) = &callee.kind
        && p.segments.len() == 1
        && p.segments[0].name.name == "Reverse"
    {
        return (&args[0], true);
    }
    (e, false)
}

/// The single ident parameter name of a closure-literal key, or `None`.
fn sbk_inline_param(key_fn: &SbkExpr) -> Option<String> {
    let SbkExprKind::Closure { params, .. } = &key_fn.kind else {
        return None;
    };
    if params.len() != 1 {
        return None;
    }
    match &params[0].pattern.kind {
        gossamer_ast::pattern::PatternKind::Ident {
            name,
            subpattern: None,
            ..
        } => Some(name.name.as_str().to_owned()),
        _ => None,
    }
}

/// Desugars `xs.sort_by_key(f)` / `xs.sort_by_key_desc(f)` into
/// `xs.sort_by(|a, b| { let ka = f(a); let kb = f(b); cmp })`, ordering by the
/// key with `<`. Because keys are compared with the `<` operator (which works
/// on scalars, strings, and tuples on every tier), multi-key sorting via a
/// tuple key works without pinning the key to `i64`. A closure-literal key is
/// inlined (its param substituted); other keys are bound and called.
/// Source-to-source, so it compiles bit-identically on all tiers.
pub fn desugar_sort_by_key(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    let mut builder = SbkBuilder {
        next: sf.next_node_id,
    };
    builder.visit_source_file(sf);
    sf.next_node_id = builder.next;
}

/// Fills in the type argument for a bare `from_json(...)` call when the
/// surrounding `let` annotation makes it unambiguous, so the turbofish is
/// optional: `let u: User = from_json(&t)?` is rewritten to carry
/// `from_json::<User>`, and `let u: Result<User, E> = from_json(&t)` to the
/// same. Runs before `rewrite_serde_generic_calls`, which then mangles the
/// now-explicit turbofish like any other. Only `from_json` is inferred (its
/// type is the binding's; `to_json`'s type lives in its argument).
pub fn infer_serde_turbofish(sf: &mut SourceFile) {
    use gossamer_ast::expr::ExprKind;
    use gossamer_ast::stmt::{Stmt, StmtKind};
    use gossamer_ast::visitor::walk_stmt_mut;
    use gossamer_ast::{Type, VisitorMut};

    fn result_ok_type(ty: &Type) -> Option<&Type> {
        let TypeKind::Path(tp) = &ty.kind else {
            return None;
        };
        let seg = tp.segments.last()?;
        if seg.name.name != "Result" {
            return None;
        }
        match seg.generics.first()? {
            GenericArg::Type(t) => Some(t),
            GenericArg::Const(_) => None,
        }
    }

    struct Inferrer;
    impl VisitorMut for Inferrer {
        fn visit_stmt(&mut self, stmt: &mut Stmt) {
            walk_stmt_mut(self, stmt);
            let StmtKind::Let {
                ty: Some(ty),
                init: Some(init),
                ..
            } = &mut stmt.kind
            else {
                return;
            };
            // `from_json(..)?` exposes the binding type directly; bare
            // `from_json(..)` exposes it through the `Result` ok-arm.
            let (call, target): (&mut gossamer_ast::expr::Expr, Type) = match &mut init.kind {
                ExprKind::Try(inner) => (inner.as_mut(), ty.clone()),
                ExprKind::Call { .. } => match result_ok_type(ty) {
                    Some(t) => {
                        let t = t.clone();
                        (init.as_mut(), t)
                    }
                    None => return,
                },
                _ => return,
            };
            let ExprKind::Call { callee, .. } = &mut call.kind else {
                return;
            };
            let ExprKind::Path(path) = &mut callee.kind else {
                return;
            };
            if path.segments.len() != 1 {
                return;
            }
            let seg = &mut path.segments[0];
            if seg.name.name != "from_json" || !seg.generics.is_empty() {
                return;
            }
            if !matches!(target.kind, TypeKind::Path(_)) {
                return;
            }
            seg.generics.push(GenericArg::Type(target));
        }
    }
    Inferrer.visit_source_file(sf);
}

/// Rewrites the generic serde call surface - `to_json::<T>(v)`,
/// `from_json::<T>(s)`, and the toml/yaml variants - into calls to the
/// per-type free functions synthesized by [`synthesize_serde_impls`]
/// (`__gos_serde_<op>_<T>`). This is the single public spelling; there
/// are no `Type::to_json` methods.
pub fn rewrite_serde_generic_calls(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    use gossamer_ast::expr::{Expr, ExprKind};
    use gossamer_ast::visitor::walk_expr_mut;

    struct Rewriter;
    impl VisitorMut for Rewriter {
        fn visit_expr(&mut self, expr: &mut Expr) {
            // Rewrite inner expressions first so nested serde calls
            // (e.g. an argument that is itself `to_json::<U>(x)`) are
            // handled before the enclosing call.
            walk_expr_mut(self, expr);
            let ExprKind::Call { callee, .. } = &mut expr.kind else {
                return;
            };
            let ExprKind::Path(path) = &mut callee.kind else {
                return;
            };
            // Bare `from_json::<T>(s)` or the qualified spelling
            // `json::from_json::<T>(s)` (head must name the matching
            // format module); both collapse to the mangled free fn.
            match path.segments.len() {
                1 => {}
                2 => {
                    let head = path.segments[0].name.name.as_str();
                    let tail = path.segments[1].name.name.as_str();
                    // The bare `from_json::<T>` turbofish is the canonical
                    // spelling; the qualified `json::from_json::<T>` is not a
                    // second path. `from_yaml` / `from_toml` keep their
                    // qualified spellings (the format module disambiguates).
                    let matched = matches!(
                        (head, tail),
                        ("yaml", "from_yaml" | "to_yaml") | ("toml", "from_toml" | "to_toml")
                    );
                    if !matched {
                        return;
                    }
                    path.segments.remove(0);
                }
                _ => return,
            }
            let seg = &mut path.segments[0];
            if !matches!(
                seg.name.name.as_str(),
                "to_json" | "from_json" | "to_toml" | "from_toml" | "to_yaml" | "from_yaml"
            ) || seg.generics.len() != 1
            {
                return;
            }
            let GenericArg::Type(ty) = &seg.generics[0] else {
                return;
            };
            let TypeKind::Path(tp) = &ty.kind else {
                return;
            };
            let Some(type_seg) = tp.segments.last() else {
                return;
            };
            let mangled = serde_fn(seg.name.name.as_str(), type_seg.name.name.as_str());
            seg.name.name = mangled;
            seg.generics.clear();
        }
    }
    Rewriter.visit_source_file(sf);
}

/// Adds `use std::json` and `use std::errors` to the parsed source
/// if it has synthesized impl blocks that depend on them. Idempotent
/// - checks for existing imports before inserting.
pub fn inject_synthetic_uses(sf: &mut SourceFile, file: FileId) {
    // `arena { ... }` desugars to `runtime::arena_push/pop` calls; make
    // the module available without requiring an explicit import.
    if uses_runtime_regions(sf) && !already_imports(&sf.uses, &["std", "runtime"]) {
        sf.uses.push(UseDecl::simple(
            NodeId::DUMMY,
            Span::new(file, 0, 0),
            UseTarget::Module(ModulePath::from_names(["std", "runtime"])),
        ));
    }
    let has_synth = sf.items.iter().any(|item| {
        matches!(&item.kind, ItemKind::Fn(decl)
            if decl.name.name.starts_with("__gos_serde_")
                || decl.name.name.starts_with("__gos_"))
    });
    if !has_synth {
        return;
    }
    let dummy_span = Span::new(file, 0, 0);
    for segs in [
        // Canonical path (matches the manifest and docs); the bare
        // `std::json` spelling is an accepted alias but injecting it
        // alongside a user's `use std::encoding::json` made the two
        // look like rival modules.
        &["std", "encoding", "json"][..],
        &["std", "errors"][..],
        &["std", "encoding", "toml"][..],
        &["std", "encoding", "yaml"][..],
    ] {
        if !already_imports(&sf.uses, segs) {
            sf.uses.push(UseDecl::simple(
                NodeId::DUMMY,
                dummy_span,
                UseTarget::Module(ModulePath::from_names(segs.iter().copied())),
            ));
        }
    }
    // The `regex!` validation macro's synthesized `comptime fn` backer
    // calls `regex::compile`, so the regex module must be in scope.
    let has_regex_validator = sf.items.iter().any(
        |item| matches!(&item.kind, ItemKind::Fn(decl) if decl.name.name == "__gos_regex_validate"),
    );
    if has_regex_validator && !already_imports(&sf.uses, &["std", "regex"]) {
        sf.uses.push(UseDecl::simple(
            NodeId::DUMMY,
            dummy_span,
            UseTarget::Module(ModulePath::from_names(["std", "regex"])),
        ));
    }
    // The `__gos_http_*` request/response-security wrappers compose http,
    // crypto, encoding, bytes, strings, and net::url primitives by their
    // qualified paths, so those modules must be in scope.
    let has_http_sec = sf.items.iter().any(|item| {
        matches!(&item.kind, ItemKind::Fn(decl) if decl.name.name.starts_with("__gos_http_"))
    });
    if has_http_sec {
        for segs in [
            &["std", "http"][..],
            &["std", "strings"][..],
            &["std", "crypto"][..],
            &["std", "encoding"][..],
            &["std", "bytes"][..],
            &["std", "net", "url"][..],
        ] {
            if !already_imports(&sf.uses, segs) {
                sf.uses.push(UseDecl::simple(
                    NodeId::DUMMY,
                    dummy_span,
                    UseTarget::Module(ModulePath::from_names(segs.iter().copied())),
                ));
            }
        }
    }
}

/// True when any expression in the file calls `runtime::arena_push`
/// (the `arena { ... }` desugar, or hand-written region management).
fn uses_runtime_regions(sf: &SourceFile) -> bool {
    use gossamer_ast::Visitor;
    use gossamer_ast::expr::{Expr, ExprKind};
    struct Finder {
        found: bool,
    }
    impl Visitor for Finder {
        fn visit_expr(&mut self, expr: &Expr) {
            if self.found {
                return;
            }
            if let ExprKind::Path(p) = &expr.kind
                && p.segments.len() == 2
                && p.segments[0].name.name == "runtime"
                && matches!(p.segments[1].name.name.as_str(), "arena_push" | "arena_pop")
            {
                self.found = true;
                return;
            }
            gossamer_ast::visitor::walk_expr(self, expr);
        }
    }
    let mut f = Finder { found: false };
    for item in &sf.items {
        gossamer_ast::visitor::walk_item(&mut f, item);
        if f.found {
            return true;
        }
    }
    false
}

fn already_imports(uses: &[UseDecl], segs: &[&str]) -> bool {
    // A use binds its LAST segment (or alias / brace-list entries):
    // `use std::encoding::json` and the synthesized `use std::json`
    // both bind `json`, and injecting the second produces a duplicate
    // -import diagnostic on perfectly valid user code. Compare the
    // bound name, not the full path.
    let bound = segs.last().copied().unwrap_or_default();
    uses.iter().any(|u| {
        if let Some(alias) = &u.alias {
            return alias.name == bound;
        }
        if let Some(list) = &u.list {
            return list.iter().any(|e| {
                e.alias
                    .as_ref()
                    .map_or(e.name.name == bound, |a| a.name == bound)
            });
        }
        match &u.target {
            UseTarget::Module(p) => p.segments.last().is_some_and(|s| s.name == bound),
            UseTarget::Project { id, module } => match module {
                Some(p) => p.segments.last().is_some_and(|s| s.name == bound),
                None => id == bound,
            },
        }
    })
}

fn synth_is_empty(synth: &str) -> bool {
    !synth.contains("__gos_serde_")
}

#[cfg(test)]
mod autoderive_tests {
    use gossamer_lex::SourceMap;

    use crate::ParseError;

    fn serde_field_errors(source: &str) -> Vec<(String, String)> {
        let mut map = SourceMap::new();
        let file = map.add_file("test.gos", source.to_string());
        let (_, diags) = super::parse_with_autoderive(source, file);
        diags
            .into_iter()
            .filter_map(|d| match d.error {
                ParseError::SerdeUnserializableField {
                    field, field_ty, ..
                } => Some((field, field_ty)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unserializable_field_used_in_serde_is_reported() {
        let src = "enum Color { Red, Green }\n\
                   struct Paint { name: String, shade: Color }\n\
                   fn main() { let _ = to_json::<Paint>(Paint { name: \"w\", shade: Color::Red }); }";
        let errs = serde_field_errors(src);
        assert_eq!(errs, vec![("shade".to_string(), "Color".to_string())]);
    }

    #[test]
    fn unserializable_field_never_serialized_is_silent() {
        let src = "enum Color { Red, Green }\n\
                   struct Paint { name: String, shade: Color }\n\
                   fn main() { let p = Paint { name: \"w\", shade: Color::Red }; let _ = p.name; }";
        assert!(serde_field_errors(src).is_empty());
    }

    #[test]
    fn fully_serializable_struct_is_silent() {
        let src = "struct Inner { n: i64 }\n\
                   struct Outer { id: i64, tags: [String], inner: Inner }\n\
                   fn main() { let _ = to_json::<Outer>(Outer { id: 1, tags: [\"a\"], inner: Inner { n: 2 } }); }";
        assert!(serde_field_errors(src).is_empty());
    }

    #[test]
    fn prescan_ignores_type_keywords_in_comments_and_strings() {
        let src = "fn main() {\n\
                   \tlet _ = \"struct NotAType\"\n\
                   \t// enum AlsoNotAType { A }\n\
                   }\n";
        assert!(!super::source_may_need_ast_synthesis(src));
        assert_eq!(super::augment_source(src), src);
    }

    #[test]
    fn prescan_detects_real_type_declarations() {
        assert!(super::source_may_need_ast_synthesis(
            "struct Point { x: i64 }\nfn main() {}\n"
        ));
        assert!(super::source_may_need_ast_synthesis(
            "enum Color { Red }\nfn main() {}\n"
        ));
    }

    #[test]
    fn validator_only_source_still_augments_without_type_declarations() {
        let src = "fn main() { let _ = regex!(\"^[a]+$\") }\n";
        let augmented = super::augment_source(src);
        assert!(augmented.contains("__gos_regex_validate"));
        assert!(augmented.starts_with(src));
    }
}
