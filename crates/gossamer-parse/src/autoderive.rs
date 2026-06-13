//! Compile-time codegen for serialization (serde / kotlinx shape).
//! For every user struct whose fields the synthesizer can classify
//! (primitives, growable arrays of supported types, nested user
//! structs), we synthesize per-type free functions
//! `__gos_serde_to_json_<T>` / `__gos_serde_from_json_<T>` (plus the
//! toml / yaml variants) as real Gossamer source, parse it, and merge
//! it into the program. The public surface is the generic call form
//! `to_json::<T>(value)` / `from_json::<T>(text)`, which
//! `rewrite_serde_generic_calls` rewrites into those names. There are
//! no `Type::to_json` methods — one spelling only.
//!
//! Because the synthesized functions are ordinary Gossamer code, they
//! compile through every tier (VM + Cranelift + LLVM) automatically.
//! There is no VM-only intercept; no runtime schema registry; no
//! per-call dispatch overhead.

#![forbid(unsafe_code)]

use std::collections::HashSet;

use gossamer_ast::{
    EnumDecl, EnumVariant, GenericArg, ItemKind, ModulePath, NodeId, SourceFile, StructBody,
    StructDecl, TypeKind, UseDecl, UseTarget,
};
use gossamer_lex::{FileId, SourceMap, Span};

use crate::ParseDiagnostic;

/// Classification of a struct field for the synthesizer. Anything
/// outside this set causes the struct to be skipped — we don't want
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
}

impl FieldKind {
    fn from_type(ty: &gossamer_ast::Type, structs: &HashSet<String>) -> Option<Self> {
        match &ty.kind {
            TypeKind::Path(path) if path.segments.len() == 1 => {
                let seg = &path.segments[0];
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
                if name == "Vec" && seg.generics.len() == 1 {
                    let GenericArg::Type(inner) = &seg.generics[0] else {
                        return None;
                    };
                    let inner_kind = Self::from_type(inner, structs)?;
                    return Some(Self::Vec(Box::new(inner_kind)));
                }
                None
            }
            TypeKind::Slice(inner) => {
                let inner_kind = Self::from_type(inner, structs)?;
                Some(Self::Vec(Box::new(inner_kind)))
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
        }
    }

    /// Gossamer source fragment that renders a single value bound to
    /// `expr` to JSON-syntax text. Returns an expression of type
    /// `String` — or, for nested structs, a `?`-propagating call.
    fn render_to_json(&self, expr: &str) -> String {
        match self {
            Self::I64 | Self::Int(_) | Self::F64 | Self::Bool => {
                format!("format!(\"{{}}\", {expr})")
            }
            Self::String => format!("format!(\"\\\"{{}}\\\"\", &{expr})"),
            Self::Vec(inner) => render_vec_to_json(expr, inner),
            Self::Struct(name) => format!("{}({expr})?", to_json_fn(name)),
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
        }
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

/// Walks `parsed` for struct definitions and synthesizes
/// serialization-method source for each eligible struct. Returns the
/// generated source text, ready to be parsed and merged.
#[must_use]
pub fn synthesize_serde_impls(parsed: &SourceFile) -> String {
    let mut out = String::new();
    out.push_str("// Synthesized by `gossamer-parse::autoderive`.\n");
    out.push('\n');

    let struct_names: HashSet<String> = parsed
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Struct(decl) if matches!(&decl.body, StructBody::Named(_)) => {
                Some(decl.name.name.clone())
            }
            _ => None,
        })
        .collect();

    for item in &parsed.items {
        let ItemKind::Struct(decl) = &item.kind else {
            continue;
        };
        let StructBody::Named(fields) = &decl.body else {
            continue;
        };
        if !decl.generics.params.is_empty() {
            continue;
        }
        let typed_fields: Option<Vec<(String, FieldKind)>> = fields
            .iter()
            .map(|f| FieldKind::from_type(&f.ty, &struct_names).map(|k| (f.name.name.clone(), k)))
            .collect();
        let Some(typed_fields) = typed_fields else {
            continue;
        };
        emit_impl(&mut out, decl, &typed_fields);
    }
    out
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
        out.push_str(&format!(
            "    let {fname} = match json::get(v, \"{fname}\") {{\n        Some(__child) => {extract},\n        None => return Err(errors::new(\"missing field `{fname}`\")),\n    }}\n"
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
pub fn synthesize_derive_impls(parsed: &SourceFile) -> String {
    let struct_names: HashSet<String> = parsed
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Struct(decl) if matches!(&decl.body, StructBody::Named(_)) => {
                Some(decl.name.name.clone())
            }
            _ => None,
        })
        .collect();
    let mut out = String::new();
    for item in &parsed.items {
        let derives = derive_list(&item.attrs);
        if derives.is_empty() {
            continue;
        }
        match &item.kind {
            ItemKind::Struct(decl) => {
                if let StructBody::Named(fields) = &decl.body {
                    emit_struct_derive_impl(&mut out, decl, fields, &derives, &struct_names);
                }
            }
            ItemKind::Enum(decl) if decl.generics.params.is_empty() => {
                emit_enum_derive_impl(&mut out, decl, &derives);
            }
            _ => {}
        }
    }
    out
}

/// The match pattern and the value-reconstruction for one enum variant,
/// binding each payload field to `{prefix}{i}` — e.g. for `V(a, b)` with prefix
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

fn emit_enum_derive_impl(out: &mut String, decl: &EnumDecl, derives: &[String]) {
    let name = &decl.name.name;
    let has = |t: &str| derives.iter().any(|d| d == t);
    let want_clone = has("Clone");
    let want_eq = has("PartialEq") || has("Eq");
    let want_default = has("Default");
    let want_debug = has("Debug");
    if !(want_clone || want_eq || want_default || want_debug) {
        return;
    }
    // Struct-payload variants (`Rect { w, h }`) are `Value::Struct` on the VM
    // walker, keyed by the bare variant name, so `==` / `{:?}` can't dispatch
    // to `Enum::eq` / `Enum::fmt` there. Derive only enums whose variants are
    // all tuple (`Circle(f64)`) or unit (`Point`) — those work on every tier.
    if decl
        .variants
        .iter()
        .any(|v| matches!(v.body, StructBody::Named(_)))
    {
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
    let want_default = has("Default");
    let want_debug = has("Debug");
    if !(want_clone || want_eq || want_default || want_debug) {
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
    let mut probe_map = SourceMap::new();
    let probe_file = probe_map.add_file("<autoderive-probe>", source.to_string());
    let (parsed, _) = crate::parse_source_file(source, probe_file);
    let serde = synthesize_serde_impls(&parsed);
    let derives = synthesize_derive_impls(&parsed);
    // Stdlib structs (pem::Block, …) are real Gossamer structs +
    // wrapper functions injected here; the wrappers call leaf
    // `gos_rt_*` intrinsics that return tuples/bytes, so the same
    // code compiles + runs on every tier. `rewrite_stdlib_struct_surface`
    // (in parse_with_autoderive) redirects the user's
    // `encoding::pem::*` call / literal / type sites onto these.
    let mut stdlib_wrappers = String::new();
    if source.contains("pem::") {
        stdlib_wrappers.push_str(PEM_WRAPPERS);
    }
    if source.contains("x509::") {
        stdlib_wrappers.push_str(X509_WRAPPERS);
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
    if synth_is_empty(&serde) && stdlib_wrappers.is_empty() && derives.is_empty() {
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
    combined
}

/// Real-struct + wrapper source for `std::encoding::pem`. The
/// wrappers fold the leaf intrinsics' tuple/byte returns into real
/// `__gos_pem_Block` structs, which lower natively on every tier.
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

/// Real-struct + wrapper source for `std::crypto::x509`.
const X509_WRAPPERS: &str = r"
struct __gos_x509_CertInfo { subject: String, issuer: String, serial: [u8], not_before_unix: i64, not_after_unix: i64, san_dns: [String], sha256: [u8] }
fn __gos_x509_parse_pem(s: &String) -> Result<__gos_x509_CertInfo, errors::Error> {
    let (subject, issuer, serial, nb, na, san, sha) = __gos_x509_parse_pem_raw(s)?
    Ok(__gos_x509_CertInfo { subject: subject, issuer: issuer, serial: serial, not_before_unix: nb, not_after_unix: na, san_dns: san, sha256: sha })
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

/// Convenience wrapper that augments `source` then parses the
/// result against `file`. Returns the merged `SourceFile` and any
/// parse diagnostics. Callers MUST have already added the augmented
/// source to their source map (see `augment_source`) for span
/// resolution to work.
#[must_use]
pub fn parse_with_autoderive(source: &str, file: FileId) -> (SourceFile, Vec<ParseDiagnostic>) {
    let (mut sf, diags) = crate::parse_source_file(source, file);
    rewrite_serde_generic_calls(&mut sf);
    rewrite_stdlib_struct_surface(&mut sf);
    inject_synthetic_uses(&mut sf, file);
    (sf, diags)
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
        _ => None,
    }
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
        // level — redirect to `errors::Error`.
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

/// Rewrites the generic serde call surface — `to_json::<T>(v)`,
/// `from_json::<T>(s)`, and the toml/yaml variants — into calls to the
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
                    let matched = matches!(
                        (head, tail),
                        ("json", "from_json" | "to_json")
                            | ("yaml", "from_yaml" | "to_yaml")
                            | ("toml", "from_toml" | "to_toml")
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
/// — checks for existing imports before inserting.
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
