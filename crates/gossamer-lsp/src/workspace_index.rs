//! Cross-file symbol index used by completion, references, and rename.
//!
//! Tracks every top-level declaration that lives in any open document
//! plus the byte spans of every workspace-significant occurrence (item
//! decls, field decls, method decls, plus every path-expression /
//! type-path resolved to one of those). Built incrementally on
//! `didOpen` / `didChange` (the resolver is fast enough at file
//! granularity that we just rebuild the entry for the one document
//! that changed).
//!
//! The cross-file bridge is name-based: per-file `DefId`s are not
//! comparable across files (the resolver runs once per document and
//! starts the counter at 0), so the index keys symbols by a
//! [`SymbolKey`] that combines a kind bucket with a fully-qualified
//! name. Two structs that both expose a `.bar` field never collide
//! because their keys are `Point.bar` and `Color.bar`.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_ast::{
    Expr, ExprKind, FieldSelector, ImplItem, Item, ItemKind, ModBody, Pattern, PatternKind,
    SourceFile, StructBody, Type, TypeKind, UseDecl, UseTarget, Visitor,
};
use gossamer_lex::Span;
use gossamer_resolve::{DefId, DefKind, Resolution, Resolutions};
use gossamer_types::{TyCtxt, TyKind, TypeTable};

use crate::session::DocumentAnalysis;

/// Single workspace entry. Stays in sync with the document's
/// definition index - kind, signature, and doc string are mirrored
/// here so completion can skip a hop when surfacing a cross-file item.
#[derive(Debug, Clone)]
pub(crate) struct WorkspaceItem {
    /// Bare item name, exactly as declared.
    pub name: String,
    /// Definition kind from the resolver.
    pub kind: DefKind,
    /// Pretty-printed single-line signature.
    pub signature: String,
    /// `///` doc block joined into one string. Empty when none.
    pub doc: String,
    /// URI of the document declaring the item.
    pub uri: String,
}

/// Bucket tag attached to a [`SymbolKey`]. Distinguishes the four
/// kinds of cross-file-renameable surface so two homonyms in
/// different name spaces never collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SymbolBucket {
    /// Top-level item (`fn`, `struct`, `enum`, `trait`, `const`,
    /// `static`, `type` alias). Key is the bare item name.
    Item,
    /// Enum variant. Key is `EnumName::VariantName`.
    Variant,
    /// Struct field. Key is `StructName.fieldName`.
    Field,
    /// Inherent or trait-impl method. Key is `TypeName::methodName`.
    Method,
}

/// Cross-file identity for a renameable symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SymbolKey {
    /// Which surface this symbol lives on.
    pub bucket: SymbolBucket,
    /// Fully-qualified name.
    pub name: String,
}

impl SymbolKey {
    /// Builds an item-bucket key for a bare top-level name. Used
    /// by tests and by external callers that already know the
    /// symbol they want to look up; the index also builds keys
    /// internally during `update` using the same shape.
    #[allow(
        dead_code,
        reason = "constructor used by tests and the public testing API; the lib path builds keys via the SymbolKey { bucket, name } shorthand"
    )]
    pub(crate) fn item(name: impl Into<String>) -> Self {
        Self {
            bucket: SymbolBucket::Item,
            name: name.into(),
        }
    }

    /// Builds a variant-bucket key (`EnumName::Variant`).
    pub(crate) fn variant(enum_name: &str, variant: &str) -> Self {
        Self {
            bucket: SymbolBucket::Variant,
            name: format!("{enum_name}::{variant}"),
        }
    }

    /// Builds a field-bucket key (`StructName.field`).
    pub(crate) fn field(struct_name: &str, field: &str) -> Self {
        Self {
            bucket: SymbolBucket::Field,
            name: format!("{struct_name}.{field}"),
        }
    }

    /// Builds a method-bucket key (`TypeName::method`).
    pub(crate) fn method(type_name: &str, method: &str) -> Self {
        Self {
            bucket: SymbolBucket::Method,
            name: format!("{type_name}::{method}"),
        }
    }

    /// Returns the leaf identifier - the substring after the last
    /// separator. Used by rename to emit edits that touch only the
    /// leaf, never the qualifier.
    pub(crate) fn leaf(&self) -> &str {
        match self.bucket {
            SymbolBucket::Item => &self.name,
            SymbolBucket::Variant | SymbolBucket::Method => {
                self.name.rsplit("::").next().unwrap_or(&self.name)
            }
            SymbolBucket::Field => self.name.rsplit('.').next().unwrap_or(&self.name),
        }
    }
}

/// One occurrence (declaration or reference) of a [`SymbolKey`] inside
/// a single document.
#[derive(Debug, Clone)]
pub(crate) struct SymbolOccurrence {
    /// Identity of the referenced symbol.
    pub key: SymbolKey,
    /// Span of the leaf identifier in the source (no qualifier).
    pub span: Span,
    /// True when this occurrence is the declaring identifier. Used
    /// by tests and downstream code that wants to distinguish
    /// declarations from references; the server itself currently
    /// emits both shapes uniformly.
    #[allow(
        dead_code,
        reason = "consumed by integration tests and reserved for callers that want declaration-vs-reference filtering"
    )]
    pub is_declaration: bool,
}

/// Per-document slice of the workspace index.
#[derive(Debug, Default, Clone)]
struct DocSlice {
    /// Top-level names contributed by this document (for `by_prefix`).
    names: Vec<String>,
    /// Every workspace-significant occurrence inside the document.
    occurrences: Vec<SymbolOccurrence>,
    /// `use` declarations parsed out of the source. Used by rename to
    /// rewrite import segments.
    use_occurrences: Vec<UseOccurrence>,
}

/// One leaf-segment inside a `use` declaration.
#[derive(Debug, Clone)]
pub(crate) struct UseOccurrence {
    /// Bare leaf identifier being imported (`foo` in `use util::foo`).
    pub leaf: String,
    /// Source span of the leaf identifier inside the `use` text.
    pub span: Span,
}

/// Workspace-wide symbol index.
#[derive(Debug, Default)]
pub(crate) struct WorkspaceIndex {
    /// `name → list of declarations`. Multiple entries for the same
    /// name are possible across files; the LSP surfaces each as a
    /// distinct completion candidate.
    by_name: HashMap<String, Vec<WorkspaceItem>>,
    /// Per-document slices keyed by URI.
    slices: HashMap<String, DocSlice>,
}

impl WorkspaceIndex {
    /// Replaces the entries previously associated with `uri` with a
    /// fresh harvest of the document's top-level declarations and
    /// per-document occurrence index.
    pub(crate) fn update(&mut self, uri: &str, doc: &DocumentAnalysis) {
        self.remove(uri);
        let mut names: Vec<String> = Vec::new();
        for (_, info) in doc.index_pairs() {
            if matches!(info.kind, DefKind::TypeParam | DefKind::Variant) {
                continue;
            }
            // The analysed unit is the whole package; a declaration in
            // another of its files is that file's own contribution.
            if !doc.span_in_document(info.name_span) {
                continue;
            }
            self.by_name
                .entry(info.name.clone())
                .or_default()
                .push(WorkspaceItem {
                    name: info.name.clone(),
                    kind: info.kind,
                    signature: info.signature.clone(),
                    doc: info.docs.clone(),
                    uri: uri.to_string(),
                });
            names.push(info.name.clone());
        }
        let occurrences = collect_occurrences(doc);
        let mut use_occurrences = collect_use_occurrences(&doc.sf, doc.source());
        use_occurrences.retain(|occurrence| doc.span_in_document(occurrence.span));
        self.slices.insert(
            uri.to_string(),
            DocSlice {
                names,
                occurrences,
                use_occurrences,
            },
        );
    }

    /// Drops every entry the document at `uri` previously contributed.
    pub(crate) fn remove(&mut self, uri: &str) {
        let Some(slice) = self.slices.remove(uri) else {
            return;
        };
        for name in slice.names {
            if let Some(entries) = self.by_name.get_mut(&name) {
                entries.retain(|item| item.uri != uri);
                if entries.is_empty() {
                    self.by_name.remove(&name);
                }
            }
        }
    }

    /// Returns every workspace item whose name starts with `prefix`,
    /// excluding entries from `current_uri`.
    pub(crate) fn by_prefix(&self, prefix: &str, current_uri: &str) -> Vec<WorkspaceItem> {
        let mut out: Vec<WorkspaceItem> = Vec::new();
        for (name, entries) in &self.by_name {
            if !name.starts_with(prefix) {
                continue;
            }
            for item in entries {
                if item.uri == current_uri {
                    continue;
                }
                out.push(item.clone());
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.uri.cmp(&b.uri)));
        out
    }

    /// Returns every URI currently tracked by the index, in
    /// deterministic alphabetic order.
    pub(crate) fn uris(&self) -> Vec<String> {
        let mut out: Vec<String> = self.slices.keys().cloned().collect();
        out.sort();
        out
    }

    /// Returns every occurrence of `key` across every tracked document,
    /// grouped by URI in alphabetic order.
    pub(crate) fn occurrences_of(&self, key: &SymbolKey) -> Vec<(String, Vec<SymbolOccurrence>)> {
        let mut out: Vec<(String, Vec<SymbolOccurrence>)> = Vec::new();
        for uri in self.uris() {
            let slice = &self.slices[&uri];
            let matches: Vec<SymbolOccurrence> = slice
                .occurrences
                .iter()
                .filter(|o| &o.key == key)
                .cloned()
                .collect();
            if !matches.is_empty() {
                out.push((uri, matches));
            }
        }
        out
    }

    /// Returns every `use`-declaration leaf occurrence across the
    /// workspace whose identifier matches `leaf`.
    pub(crate) fn use_occurrences_of(&self, leaf: &str) -> Vec<(String, Vec<UseOccurrence>)> {
        let mut out: Vec<(String, Vec<UseOccurrence>)> = Vec::new();
        for uri in self.uris() {
            let slice = &self.slices[&uri];
            let matches: Vec<UseOccurrence> = slice
                .use_occurrences
                .iter()
                .filter(|o| o.leaf == leaf)
                .cloned()
                .collect();
            if !matches.is_empty() {
                out.push((uri, matches));
            }
        }
        out
    }
}

/// Walks the doc's parsed AST + resolutions + type table and produces
/// every workspace-significant [`SymbolOccurrence`].
fn collect_occurrences(doc: &DocumentAnalysis) -> Vec<SymbolOccurrence> {
    let mut out: Vec<SymbolOccurrence> = Vec::new();
    let source = doc.source();
    for (_, info) in doc.index_pairs() {
        let bucket = match info.kind {
            DefKind::Fn
            | DefKind::Struct
            | DefKind::Enum
            | DefKind::Trait
            | DefKind::Const
            | DefKind::Static
            | DefKind::TypeAlias => Some(SymbolBucket::Item),
            DefKind::Mod | DefKind::Variant | DefKind::TypeParam => None,
        };
        if let Some(bucket) = bucket {
            out.push(SymbolOccurrence {
                key: SymbolKey {
                    bucket,
                    name: info.name.clone(),
                },
                span: info.name_span,
                is_declaration: true,
            });
        }
    }
    let document_items = doc.document_items();
    for item in &document_items {
        collect_item_decl_occurrences(item, source, &mut out);
    }
    for occurrence in doc.index.occurrences() {
        match occurrence.resolution {
            Some(Resolution::Def { def, kind }) => {
                let bucket = match kind {
                    DefKind::Fn
                    | DefKind::Struct
                    | DefKind::Enum
                    | DefKind::Trait
                    | DefKind::Const
                    | DefKind::Static
                    | DefKind::TypeAlias => SymbolBucket::Item,
                    DefKind::Variant => SymbolBucket::Variant,
                    DefKind::Mod | DefKind::TypeParam => continue,
                };
                let Some(info) = doc.index.def(def) else {
                    continue;
                };
                out.push(SymbolOccurrence {
                    key: SymbolKey {
                        bucket,
                        name: info.name.clone(),
                    },
                    span: occurrence.span,
                    is_declaration: false,
                });
            }
            // Cross-file references typically resolve to
            // `Import` (when imported via `use`) or `Err` (when the
            // name is referenced bare without a matching local
            // DefId). Record the bare name as an item-bucket
            // candidate - the workspace lookup narrows by
            // `(bucket, name)`, so unrelated identifiers harmlessly
            // join unrelated keys.
            Some(Resolution::Import { .. } | Resolution::Err) | None
                if !occurrence.name.is_empty() =>
            {
                out.push(SymbolOccurrence {
                    key: SymbolKey {
                        bucket: SymbolBucket::Item,
                        name: occurrence.name.clone(),
                    },
                    span: occurrence.span,
                    is_declaration: false,
                });
            }
            _ => {}
        }
    }
    let mut walker = MemberOccurrenceWalker {
        types: &doc.types,
        tcx: &doc.tcx,
        resolutions: &doc.resolutions,
        sf: &doc.sf,
        source,
        out: &mut out,
    };
    // The analysed unit is the whole package; the members another of its
    // files mentions are that file's own contribution, so the walk covers
    // this document's items only.
    for item in &document_items {
        walker.visit_item(item);
    }
    out.retain(|occurrence| doc.span_in_document(occurrence.span));
    out
}

fn collect_item_decl_occurrences(item: &Item, source: &str, out: &mut Vec<SymbolOccurrence>) {
    match &item.kind {
        ItemKind::Enum(decl) => {
            let enum_name = decl.name.name.as_str();
            for variant in &decl.variants {
                let span =
                    locate_in_item(source, item.span, &variant.name.name).unwrap_or(item.span);
                out.push(SymbolOccurrence {
                    key: SymbolKey::variant(enum_name, &variant.name.name),
                    span,
                    is_declaration: true,
                });
                out.push(SymbolOccurrence {
                    key: SymbolKey {
                        bucket: SymbolBucket::Variant,
                        name: variant.name.name.clone(),
                    },
                    span,
                    is_declaration: true,
                });
            }
        }
        ItemKind::Struct(decl) => {
            let struct_name = decl.name.name.as_str();
            if let StructBody::Named(fields) = &decl.body {
                for field in fields {
                    let span =
                        locate_in_item(source, item.span, &field.name.name).unwrap_or(item.span);
                    out.push(SymbolOccurrence {
                        key: SymbolKey::field(struct_name, &field.name.name),
                        span,
                        is_declaration: true,
                    });
                }
            }
        }
        ItemKind::Impl(decl) => {
            let TypeKind::Path(path) = &decl.self_ty.kind else {
                return;
            };
            let Some(last_seg) = path.segments.last() else {
                return;
            };
            let type_name = last_seg.name.name.as_str();
            for impl_item in &decl.items {
                let ImplItem::Fn(fn_decl) = impl_item else {
                    continue;
                };
                let span = find_method_decl_span(source, item.span, &fn_decl.name.name)
                    .unwrap_or(item.span);
                out.push(SymbolOccurrence {
                    key: SymbolKey::method(type_name, &fn_decl.name.name),
                    span,
                    is_declaration: true,
                });
            }
        }
        ItemKind::Mod(decl) => {
            if let ModBody::Inline(items) = &decl.body {
                for nested in items {
                    collect_item_decl_occurrences(nested, source, out);
                }
            }
        }
        _ => {}
    }
}

/// Best-effort `fn methodName` span finder inside the impl block at
/// `impl_span`. Returns the byte span of the identifier itself.
fn find_method_decl_span(source: &str, impl_span: Span, method: &str) -> Option<Span> {
    let start = impl_span.start as usize;
    let end = std::cmp::min(impl_span.end as usize, source.len());
    if start >= end {
        return None;
    }
    let slice = &source[start..end];
    let needle = "fn ";
    let mut cursor = 0;
    while let Some(pos) = slice[cursor..].find(needle) {
        let id_start = cursor + pos + needle.len();
        let bytes = slice.as_bytes();
        let mut id_end = id_start;
        while id_end < bytes.len()
            && (bytes[id_end].is_ascii_alphanumeric() || bytes[id_end] == b'_')
        {
            id_end += 1;
        }
        let candidate = &slice[id_start..id_end];
        if candidate == method {
            let abs_start = start + id_start;
            let abs_end = start + id_end;
            return Some(Span::new(impl_span.file, abs_start as u32, abs_end as u32));
        }
        cursor = id_end;
    }
    None
}

/// Locates a whole-word occurrence of `name` inside the byte range
/// `[item_span.start, item_span.end)`. Returns the absolute span.
fn locate_in_item(source: &str, item_span: Span, name: &str) -> Option<Span> {
    let start = item_span.start as usize;
    let end = std::cmp::min(item_span.end as usize, source.len());
    if start >= end || name.is_empty() {
        return None;
    }
    let slice = &source[start..end];
    let bytes = slice.as_bytes();
    let needle = name.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut cursor = 0;
    while cursor + needle.len() <= bytes.len() {
        if &bytes[cursor..cursor + needle.len()] != needle {
            cursor += 1;
            continue;
        }
        let before_ok = cursor == 0 || !is_word(bytes[cursor - 1]);
        let after_ok =
            cursor + needle.len() == bytes.len() || !is_word(bytes[cursor + needle.len()]);
        if before_ok && after_ok {
            let abs_start = start + cursor;
            let abs_end = abs_start + needle.len();
            return Some(Span::new(item_span.file, abs_start as u32, abs_end as u32));
        }
        cursor += 1;
    }
    None
}

/// AST walker that emits a [`SymbolOccurrence`] for every field
/// access, method-call receiver, and struct-literal/pattern field
/// whose type the type-checker has resolved to a concrete ADT.
struct MemberOccurrenceWalker<'a> {
    types: &'a TypeTable,
    tcx: &'a TyCtxt,
    resolutions: &'a Resolutions,
    sf: &'a SourceFile,
    source: &'a str,
    out: &'a mut Vec<SymbolOccurrence>,
}

impl Visitor for MemberOccurrenceWalker<'_> {
    fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::FieldAccess {
                receiver,
                field: FieldSelector::Named(name),
            } => {
                if let Some(adt_name) = self.adt_name_of_expr(receiver) {
                    let span =
                        find_trailing_word(self.source, expr.span, &name.name).unwrap_or(expr.span);
                    self.out.push(SymbolOccurrence {
                        key: SymbolKey::field(&adt_name, &name.name),
                        span,
                        is_declaration: false,
                    });
                }
            }
            ExprKind::MethodCall { receiver, name, .. } => {
                if let Some(adt_name) = self.adt_name_of_expr(receiver) {
                    let span =
                        find_trailing_word(self.source, expr.span, &name.name).unwrap_or(expr.span);
                    self.out.push(SymbolOccurrence {
                        key: SymbolKey::method(&adt_name, &name.name),
                        span,
                        is_declaration: false,
                    });
                }
            }
            ExprKind::Struct { path, fields, .. } => {
                if let Some(adt_name) = path.segments.last().map(|s| s.name.name.clone()) {
                    for field in fields {
                        let span = locate_in_item(self.source, expr.span, &field.name.name)
                            .unwrap_or(expr.span);
                        self.out.push(SymbolOccurrence {
                            key: SymbolKey::field(&adt_name, &field.name.name),
                            span,
                            is_declaration: false,
                        });
                    }
                }
            }
            _ => {}
        }
        gossamer_ast::visitor::walk_expr(self, expr);
    }

    fn visit_pattern(&mut self, pattern: &Pattern) {
        if let PatternKind::Struct { path, fields, .. } = &pattern.kind {
            if let Some(adt_name) = path.segments.last().map(|s| s.name.name.clone()) {
                for field in fields {
                    let span = locate_in_item(self.source, pattern.span, &field.name.name)
                        .unwrap_or(pattern.span);
                    self.out.push(SymbolOccurrence {
                        key: SymbolKey::field(&adt_name, &field.name.name),
                        span,
                        is_declaration: false,
                    });
                }
            }
        }
        gossamer_ast::visitor::walk_pattern(self, pattern);
    }

    fn visit_type(&mut self, ty: &Type) {
        gossamer_ast::visitor::walk_type(self, ty);
    }
}

impl MemberOccurrenceWalker<'_> {
    fn adt_name_of_expr(&self, receiver: &Expr) -> Option<String> {
        if let Some(ty) = self.types.get(receiver.id) {
            if let Some(def) = resolve_to_adt(self.tcx, ty) {
                if let Some(name) = self.adt_def_name(def) {
                    return Some(name);
                }
            }
            let rendered = gossamer_types::render_ty(self.tcx, ty);
            let trimmed = rendered.trim_start_matches(['&', '*', ' ']);
            let head = trimmed.split(['<', '[', '(', ' ']).next().unwrap_or("");
            if !head.is_empty() && head.chars().next().is_some_and(char::is_uppercase) {
                return Some(head.to_string());
            }
        }
        if let ExprKind::Path(path) = &receiver.kind {
            if let Some(seg) = path.segments.last() {
                let head = seg.name.name.as_str();
                if head.chars().next().is_some_and(char::is_uppercase) {
                    return Some(head.to_string());
                }
            }
        }
        None
    }

    fn adt_def_name(&self, def: DefId) -> Option<String> {
        for item in &self.sf.items {
            let Some(item_def) = self.resolutions.definition_of(item.id) else {
                continue;
            };
            if item_def != def {
                continue;
            }
            return match &item.kind {
                ItemKind::Struct(decl) => Some(decl.name.name.clone()),
                ItemKind::Enum(decl) => Some(decl.name.name.clone()),
                _ => None,
            };
        }
        None
    }
}

fn resolve_to_adt(tcx: &TyCtxt, ty: gossamer_types::Ty) -> Option<DefId> {
    let kind = tcx.kind(ty)?;
    match kind {
        TyKind::Adt { def, .. } | TyKind::Alias { def, .. } => Some(*def),
        TyKind::Ref { inner, .. } => {
            let inner_kind = tcx.kind(*inner)?;
            match inner_kind {
                TyKind::Adt { def, .. } | TyKind::Alias { def, .. } => Some(*def),
                _ => None,
            }
        }
        _ => None,
    }
}

fn find_trailing_word(source: &str, span: Span, name: &str) -> Option<Span> {
    let start = span.start as usize;
    let end = std::cmp::min(span.end as usize, source.len());
    if start >= end {
        return None;
    }
    let slice = &source[start..end];
    let pos = slice.rfind(name)?;
    let bytes = slice.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let before_ok = pos == 0 || !is_word(bytes[pos - 1]);
    let after_ok = pos + name.len() == bytes.len() || !is_word(bytes[pos + name.len()]);
    if !before_ok || !after_ok {
        return None;
    }
    let abs_start = start + pos;
    let abs_end = abs_start + name.len();
    Some(Span::new(span.file, abs_start as u32, abs_end as u32))
}

fn collect_use_occurrences(sf: &SourceFile, source: &str) -> Vec<UseOccurrence> {
    let mut out: Vec<UseOccurrence> = Vec::new();
    for decl in &sf.uses {
        let span = decl.span;
        let start = span.start as usize;
        let end = std::cmp::min(span.end as usize, source.len());
        if start >= end {
            continue;
        }
        let slice = &source[start..end];
        match &decl.list {
            Some(list) => {
                for entry in list {
                    if let Some(rel) = locate_word_in(slice, &entry.name.name) {
                        out.push(UseOccurrence {
                            leaf: entry.name.name.clone(),
                            span: Span::new(
                                decl.span.file,
                                (start as u32) + rel.0,
                                (start as u32) + rel.1,
                            ),
                        });
                    }
                }
            }
            None => {
                if let Some(leaf) = leaf_name_of(decl) {
                    if let Some(rel) = locate_word_in(slice, &leaf) {
                        out.push(UseOccurrence {
                            leaf,
                            span: Span::new(
                                decl.span.file,
                                (start as u32) + rel.0,
                                (start as u32) + rel.1,
                            ),
                        });
                    }
                }
            }
        }
    }
    out
}

fn leaf_name_of(decl: &UseDecl) -> Option<String> {
    match &decl.target {
        UseTarget::Module(path) => path.segments.last().map(|s| s.name.clone()),
        UseTarget::Project { module, .. } => module
            .as_ref()
            .and_then(|m| m.segments.last().map(|s| s.name.clone())),
    }
}

fn locate_word_in(text: &str, name: &str) -> Option<(u32, u32)> {
    let bytes = text.as_bytes();
    let needle = name.as_bytes();
    if needle.is_empty() {
        return None;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut cursor = 0;
    while cursor + needle.len() <= bytes.len() {
        if &bytes[cursor..cursor + needle.len()] != needle {
            cursor += 1;
            continue;
        }
        let before_ok = cursor == 0 || !is_word(bytes[cursor - 1]);
        let after_ok =
            cursor + needle.len() == bytes.len() || !is_word(bytes[cursor + needle.len()]);
        if before_ok && after_ok {
            return Some((cursor as u32, (cursor + needle.len()) as u32));
        }
        cursor += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::analyse;

    #[test]
    fn workspace_index_surfaces_other_files_top_levels() {
        let mut idx = WorkspaceIndex::default();
        let util = analyse("file:///util.gos", "fn shared() -> i64 { 0 }\n");
        let main = analyse("file:///main.gos", "fn main() {}\n");
        idx.update("file:///util.gos", &util);
        idx.update("file:///main.gos", &main);
        let hits = idx.by_prefix("sh", "file:///main.gos");
        assert!(
            hits.iter().any(|i| i.name == "shared"),
            "expected shared from util in {hits:?}"
        );
    }

    #[test]
    fn workspace_index_removes_stale_entries_on_update() {
        let mut idx = WorkspaceIndex::default();
        let v1 = analyse("file:///lib.gos", "fn old_name() { }\n");
        idx.update("file:///lib.gos", &v1);
        let v2 = analyse("file:///lib.gos", "fn new_name() { }\n");
        idx.update("file:///lib.gos", &v2);
        let from_other = idx.by_prefix("old", "file:///main.gos");
        assert!(
            from_other.is_empty(),
            "old_name should be gone after update; got {from_other:?}"
        );
        let new_hits = idx.by_prefix("new", "file:///main.gos");
        assert!(new_hits.iter().any(|i| i.name == "new_name"));
    }

    #[test]
    fn workspace_index_records_item_declaration_occurrence() {
        let mut idx = WorkspaceIndex::default();
        let util = analyse("file:///util.gos", "fn shared() -> i64 { 0 }\n");
        idx.update("file:///util.gos", &util);
        let key = SymbolKey::item("shared");
        let hits = idx.occurrences_of(&key);
        assert!(!hits.is_empty(), "expected an occurrence for `shared`");
        let (_, occs) = &hits[0];
        assert!(occs.iter().any(|o| o.is_declaration));
    }
}
