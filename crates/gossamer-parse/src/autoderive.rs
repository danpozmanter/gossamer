//! Compile-time codegen for serialization methods (serde / kotlinx
//! shape). For every user struct whose fields the synthesizer can
//! classify (primitives, growable arrays of supported types, nested
//! user structs), we synthesize
//! `pub fn to_json(self) -> Result<String, errors::Error>` and
//! `pub fn from_json(text: &String) -> Result<Self, errors::Error>`
//! as real Gossamer source, parse it, and merge into the program.
//!
//! Because the synthesized methods are ordinary Gossamer code, they
//! compile through every tier (VM + Cranelift + LLVM) automatically.
//! There is no VM-only intercept; no runtime schema registry; no
//! per-call dispatch overhead. Zero-cost abstraction in the same
//! sense as serde's `#[derive(Serialize, Deserialize)]`: the
//! compiler writes the code you would have written yourself.

#![forbid(unsafe_code)]

use std::collections::HashSet;

use gossamer_ast::{
    GenericArg, ItemKind, ModulePath, NodeId, SourceFile, StructBody, StructDecl, TypeKind,
    UseDecl, UseTarget,
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
            Self::Struct(name) => format!("{name}::to_json(&{expr})?"),
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
                "match {name}::from_json(&json::render({value_expr})) {{ Ok(__v) => __v, Err(__e) => return Err(errors::wrap(__e, \"{path}\")) }}"
            ),
        }
    }
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
    out.push_str(&format!("impl {name} {{\n"));
    emit_to_json(out, fields);
    emit_from_json(out, name, fields);
    emit_to_toml(out);
    emit_from_toml(out, name);
    emit_to_yaml(out);
    emit_from_yaml(out, name);
    out.push_str("}\n\n");
}

fn emit_to_json(out: &mut String, fields: &[(String, FieldKind)]) {
    out.push_str(
        "    /// Render `self` as a JSON object. Auto-derived by `gossamer-parse::autoderive`.\n",
    );
    out.push_str("    pub fn to_json(self) -> Result<String, errors::Error> {\n");
    out.push_str("        let mut out = \"\"\n");
    out.push_str("        out += \"{\"\n");
    for (i, (fname, kind)) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str("        out += \",\"\n");
        }
        out.push_str(&format!("        out += \"\\\"{fname}\\\":\"\n"));
        let lit = kind.render_to_json(&format!("self.{fname}"));
        out.push_str(&format!("        out += {lit}\n"));
    }
    out.push_str("        out += \"}\"\n");
    out.push_str("        Ok(out)\n");
    out.push_str("    }\n");
}

fn emit_to_toml(out: &mut String) {
    out.push_str("    /// Render `self` as TOML. Auto-derived (piggybacks on `to_json`).\n");
    out.push_str("    pub fn to_toml(self) -> Result<String, errors::Error> {\n");
    out.push_str("        let j = self.to_json()?\n");
    out.push_str("        toml::from_json(&j)\n");
    out.push_str("    }\n");
}

fn emit_from_toml(out: &mut String, name: &str) {
    out.push_str(
        "    /// Parse TOML text into `Self`. Auto-derived (composes\n    /// `toml::to_json` with `from_json`).\n",
    );
    out.push_str(&format!(
        "    pub fn from_toml(text: &String) -> Result<{name}, errors::Error> {{\n"
    ));
    out.push_str("        let j = toml::to_json(text)?\n");
    out.push_str(&format!("        {name}::from_json(&j)\n"));
    out.push_str("    }\n");
}

fn emit_to_yaml(out: &mut String) {
    out.push_str("    /// Render `self` as YAML. Auto-derived (piggybacks on `to_json`).\n");
    out.push_str("    pub fn to_yaml(self) -> Result<String, errors::Error> {\n");
    out.push_str("        let j = self.to_json()?\n");
    out.push_str("        yaml::from_json(&j)\n");
    out.push_str("    }\n");
}

fn emit_from_yaml(out: &mut String, name: &str) {
    out.push_str(
        "    /// Parse YAML text into `Self`. Auto-derived (composes\n    /// `yaml::to_json` with `from_json`).\n",
    );
    out.push_str(&format!(
        "    pub fn from_yaml(text: &String) -> Result<{name}, errors::Error> {{\n"
    ));
    out.push_str("        let j = yaml::to_json(text)?\n");
    out.push_str(&format!("        {name}::from_json(&j)\n"));
    out.push_str("    }\n");
}

fn emit_from_json(out: &mut String, name: &str, fields: &[(String, FieldKind)]) {
    out.push_str(
        "    /// Parse a JSON object into `Self`. Auto-derived by\n    /// `gossamer-parse::autoderive`. Returns `Err` when a required\n    /// field is missing or any field's value type does not match\n    /// the declaration; the error message names the offending field.\n",
    );
    out.push_str(&format!(
        "    pub fn from_json(text: &String) -> Result<{name}, errors::Error> {{\n"
    ));
    out.push_str("        let v = json::parse(text)?\n");
    for (fname, kind) in fields {
        let path = format!("field `{fname}`");
        let extract = kind.extract_strict("__child", &path);
        out.push_str(&format!(
            "        let {fname} = match json::get(v, \"{fname}\") {{\n            Some(__child) => {extract},\n            None => return Err(errors::new(\"missing field `{fname}`\")),\n        }}\n"
        ));
    }
    out.push_str(&format!("        Ok({name} {{ "));
    let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
    out.push_str(&names.join(", "));
    out.push_str(" })\n");
    out.push_str("    }\n");
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
    let synth = synthesize_serde_impls(&parsed);
    if synth_is_empty(&synth) {
        return source.to_string();
    }
    if std::env::var_os("GOS_AUTODERIVE_DEBUG").is_some() {
        eprintln!("=== autoderive synth ===\n{synth}=== /autoderive ===");
    }
    let mut combined = String::with_capacity(source.len() + synth.len() + 2);
    combined.push_str(source);
    if !combined.ends_with('\n') {
        combined.push('\n');
    }
    combined.push('\n');
    combined.push_str(&synth);
    combined
}

/// Convenience wrapper that augments `source` then parses the
/// result against `file`. Returns the merged `SourceFile` and any
/// parse diagnostics. Callers MUST have already added the augmented
/// source to their source map (see `augment_source`) for span
/// resolution to work.
#[must_use]
pub fn parse_with_autoderive(source: &str, file: FileId) -> (SourceFile, Vec<ParseDiagnostic>) {
    let (mut sf, diags) = crate::parse_source_file(source, file);
    inject_synthetic_uses(&mut sf, file);
    (sf, diags)
}

/// Adds `use std::json` and `use std::errors` to the parsed source
/// if it has synthesized impl blocks that depend on them. Idempotent
/// — checks for existing imports before inserting.
pub fn inject_synthetic_uses(sf: &mut SourceFile, file: FileId) {
    let has_synth = sf.items.iter().any(|item| {
        matches!(&item.kind, ItemKind::Impl(decl) if decl.items.iter().any(|it| {
            matches!(it, gossamer_ast::ImplItem::Fn(f) if {
                let n = f.name.name.as_str();
                n == "from_json" || n == "to_json"
            })
        }))
    });
    if !has_synth {
        return;
    }
    let dummy_span = Span::new(file, 0, 0);
    for segs in [
        &["std", "json"][..],
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

fn already_imports(uses: &[UseDecl], segs: &[&str]) -> bool {
    uses.iter().any(|u| match &u.target {
        UseTarget::Module(p) if p.segments.len() == segs.len() => p
            .segments
            .iter()
            .zip(segs.iter())
            .all(|(a, b)| a.name == *b),
        _ => false,
    })
}

fn synth_is_empty(synth: &str) -> bool {
    !synth.contains("impl ")
}
