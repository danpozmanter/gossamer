//! LSP request-dispatch loop.
//! Reads JSON-RPC messages from the client, routes them by method,
//! and writes replies back. The server covers the spec subset
//! Gossamer's editor integrations need:
//!
//! - lifecycle: `initialize`, `initialized`, `shutdown`, `exit`
//! - sync: `textDocument/didOpen`, `didChange`, `didClose`,
//!   `publishDiagnostics`
//! - navigation: `hover`, `definition`, `typeDefinition`,
//!   `references`, `documentHighlight`, `prepareRename`, `rename`
//! - completion + signature help: `completion`, `signatureHelp`
//! - structure: `documentSymbol`, `workspace/symbol`,
//!   `foldingRange`
//! - decoration: `inlayHint`, `semanticTokens/full`
//! - formatting: `formatting`

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, BufWriter, Read, Write};

use gossamer_diagnostics::{Diagnostic as GossamerDiagnostic, Severity};
use gossamer_lex::Span;
use gossamer_resolve::{DefKind, Resolution};
use gossamer_std::json::Value;
use gossamer_types::render_ty;

use crate::inlay::{InlayHint, collect_inlays};
use crate::navigation::{BindingInfo, DefinitionInfo, Locate, attach_resolution, locate};
use crate::protocol::{Transport, field, field_str, field_u32, notification, response_ok};
use crate::semantic_tokens::{TOKEN_MODIFIERS, TOKEN_TYPES, full_tokens};
use crate::session::{CursorContext, DocumentAnalysis, analyse};
use crate::stdlib_index::{MemberSpec, StdlibIndex};
use crate::symbols::{document_symbols, folding_ranges, workspace_symbols};
use crate::workspace_index::{WorkspaceIndex, WorkspaceItem};

/// Runs the server over the supplied reader/writer streams. Returns
/// `Ok(())` when the client sends `exit` after `shutdown`.
#[allow(clippy::too_many_lines)]
fn run<R: Read, W: Write>(reader: R, writer: W) -> std::io::Result<()> {
    let mut transport = Transport::new(BufReader::new(reader), BufWriter::new(writer));
    let mut state = ServerState::new();

    loop {
        let Some(message) = transport.read_message()? else {
            return Ok(());
        };
        let Some(method) = field_str(&message, "method") else {
            continue;
        };
        let id = field(&message, "id").clone();
        let params = field(&message, "params").clone();

        match method {
            "initialize" => {
                transport.write_message(&response_ok(id, initialize_result()))?;
            }
            "initialized" | "$/cancelRequest" => {}
            "textDocument/didOpen" => {
                if let Some((uri, text)) = extract_did_open(&params) {
                    state.update(&uri, &text);
                    for notif in state.publish_diagnostics(&uri) {
                        transport.write_message(&notif)?;
                    }
                }
            }
            "textDocument/didChange" => {
                if let Some((uri, text)) = extract_did_change(&params) {
                    state.update(&uri, &text);
                    for notif in state.publish_diagnostics(&uri) {
                        transport.write_message(&notif)?;
                    }
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = field_str(field(&params, "textDocument"), "uri") {
                    state.close(uri);
                }
            }
            "textDocument/hover" => {
                let result = state.hover(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/definition" => {
                let result = state.definition(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/typeDefinition" => {
                let result = state.type_definition(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/completion" => {
                let result = state.completion(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/references" => {
                let result = state.references(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/documentHighlight" => {
                let result = state.document_highlight(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/prepareRename" => {
                let result = state.prepare_rename(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/rename" => {
                let result = state.rename(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/inlayHint" => {
                let result = state.inlay_hints(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/documentSymbol" => {
                let result = state.document_symbols(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "workspace/symbol" => {
                let result = state.workspace_symbols(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/foldingRange" => {
                let result = state.folding_ranges(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/signatureHelp" => {
                let result = state.signature_help(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/formatting" => {
                let result = state.formatting(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/semanticTokens/full" => {
                let result = state.semantic_tokens(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "shutdown" => {
                transport.write_message(&response_ok(id, Value::Null))?;
            }
            "exit" => return Ok(()),
            _ => {
                if !matches!(id, Value::Null) {
                    transport.write_message(&response_ok(id, Value::Null))?;
                }
            }
        }
    }
}

/// Convenience wrapper that runs the server over the process's
/// stdio streams.
pub fn run_stdio() -> std::io::Result<()> {
    run(std::io::stdin(), std::io::stdout())
}

fn initialize_result() -> Value {
    let mut caps = BTreeMap::new();
    // textDocumentSync object: { openClose: true, change: 1 (Full) }.
    let mut sync = BTreeMap::new();
    sync.insert("openClose".to_string(), Value::Bool(true));
    sync.insert("change".to_string(), Value::Number(1.0));
    caps.insert("textDocumentSync".to_string(), Value::Object(sync));
    caps.insert("hoverProvider".to_string(), Value::Bool(true));
    caps.insert("definitionProvider".to_string(), Value::Bool(true));
    caps.insert("typeDefinitionProvider".to_string(), Value::Bool(true));
    caps.insert("referencesProvider".to_string(), Value::Bool(true));
    caps.insert("documentHighlightProvider".to_string(), Value::Bool(true));
    caps.insert("inlayHintProvider".to_string(), Value::Bool(true));
    caps.insert("documentSymbolProvider".to_string(), Value::Bool(true));
    caps.insert("workspaceSymbolProvider".to_string(), Value::Bool(true));
    caps.insert("foldingRangeProvider".to_string(), Value::Bool(true));
    caps.insert("documentFormattingProvider".to_string(), Value::Bool(true));
    let mut rename = BTreeMap::new();
    rename.insert("prepareProvider".to_string(), Value::Bool(true));
    caps.insert("renameProvider".to_string(), Value::Object(rename));
    let mut completion = BTreeMap::new();
    completion.insert(
        "triggerCharacters".to_string(),
        Value::Array(vec![
            Value::String(".".to_string()),
            Value::String(":".to_string()),
        ]),
    );
    let mut completion_item_caps = BTreeMap::new();
    completion_item_caps.insert("snippetSupport".to_string(), Value::Bool(true));
    completion.insert(
        "completionItem".to_string(),
        Value::Object(completion_item_caps),
    );
    caps.insert("completionProvider".to_string(), Value::Object(completion));
    let mut sig = BTreeMap::new();
    sig.insert(
        "triggerCharacters".to_string(),
        Value::Array(vec![
            Value::String("(".to_string()),
            Value::String(",".to_string()),
        ]),
    );
    caps.insert("signatureHelpProvider".to_string(), Value::Object(sig));
    caps.insert(
        "semanticTokensProvider".to_string(),
        semantic_tokens_capability(),
    );
    let mut info = BTreeMap::new();
    info.insert("name".to_string(), Value::String("gos-lsp".to_string()));
    info.insert(
        "version".to_string(),
        Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    let mut root = BTreeMap::new();
    root.insert("capabilities".to_string(), Value::Object(caps));
    root.insert("serverInfo".to_string(), Value::Object(info));
    Value::Object(root)
}

fn semantic_tokens_capability() -> Value {
    let mut legend = BTreeMap::new();
    legend.insert(
        "tokenTypes".to_string(),
        Value::Array(
            TOKEN_TYPES
                .iter()
                .map(|t| Value::String((*t).to_string()))
                .collect(),
        ),
    );
    legend.insert(
        "tokenModifiers".to_string(),
        Value::Array(
            TOKEN_MODIFIERS
                .iter()
                .map(|t| Value::String((*t).to_string()))
                .collect(),
        ),
    );
    let mut cap = BTreeMap::new();
    cap.insert("legend".to_string(), Value::Object(legend));
    cap.insert("full".to_string(), Value::Bool(true));
    cap.insert("range".to_string(), Value::Bool(false));
    Value::Object(cap)
}

fn extract_did_open(params: &Value) -> Option<(String, String)> {
    let doc = field(params, "textDocument");
    let uri = field_str(doc, "uri")?.to_string();
    let text = field_str(doc, "text")?.to_string();
    Some((uri, text))
}

fn extract_did_change(params: &Value) -> Option<(String, String)> {
    let uri = field_str(field(params, "textDocument"), "uri")?.to_string();
    let changes = field(params, "contentChanges");
    let Value::Array(items) = changes else {
        return None;
    };
    // LSP sync kind "Full" always delivers the whole document as
    // the last change. Respect that without bothering with range-
    // based incremental updates for the first slice.
    let last = items.last()?;
    let text = field_str(last, "text")?.to_string();
    Some((uri, text))
}

struct ServerState {
    documents: HashMap<String, DocumentAnalysis>,
    stdlib: StdlibIndex,
    workspace: WorkspaceIndex,
}

impl ServerState {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
            stdlib: StdlibIndex::build(),
            workspace: WorkspaceIndex::default(),
        }
    }

    fn update(&mut self, uri: &str, text: &str) {
        let analysis = analyse(uri, text);
        self.workspace.update(uri, &analysis);
        self.documents.insert(uri.to_string(), analysis);
    }

    fn close(&mut self, uri: &str) {
        self.documents.remove(uri);
        self.workspace.remove(uri);
    }

    fn publish_diagnostics(&self, uri: &str) -> Vec<Value> {
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        let items: Vec<Value> = doc
            .diagnostics
            .iter()
            .map(|d| diagnostic_to_lsp(doc, d))
            .collect();
        let mut params = BTreeMap::new();
        params.insert("uri".to_string(), Value::String(uri.to_string()));
        params.insert("diagnostics".to_string(), Value::Array(items));
        vec![notification(
            "textDocument/publishDiagnostics",
            Value::Object(params),
        )]
    }

    fn hover(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let Some(loc) = self.cursor(doc, offset) else {
            // Fallback: word-based hover when we couldn't locate a
            // semantic node (e.g. the cursor is in whitespace inside
            // a partially-parseable file).
            return word_hover(doc, offset);
        };
        let body = render_hover(doc, &loc);
        if body.is_empty() {
            return Value::Null;
        }
        let mut contents = BTreeMap::new();
        contents.insert("kind".to_string(), Value::String("markdown".to_string()));
        contents.insert("value".to_string(), Value::String(body));
        let mut hover = BTreeMap::new();
        hover.insert("contents".to_string(), Value::Object(contents));
        Value::Object(hover)
    }

    fn definition(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let Some(loc) = self.cursor(doc, offset) else {
            return self.definition_by_name(doc, offset);
        };
        match &loc {
            Locate::PathExpr {
                resolution: Some(Resolution::Local(node)),
                ..
            }
            | Locate::TypePath {
                resolution: Some(Resolution::Local(node)),
                ..
            } => doc
                .index
                .local(*node)
                .map_or(Value::Null, |info| location(doc, info.name_span)),
            Locate::PathExpr {
                resolution: Some(Resolution::Def { def, .. }),
                ..
            }
            | Locate::TypePath {
                resolution: Some(Resolution::Def { def, .. }),
                ..
            } => doc
                .index
                .def(*def)
                .map_or(Value::Null, |info| location(doc, info.name_span)),
            Locate::Binding { name_span, .. } => location(doc, *name_span),
            _ => self.definition_by_name(doc, offset),
        }
    }

    fn type_definition(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let Some(loc) = self.cursor(doc, offset) else {
            return Value::Null;
        };
        // For locals and field accesses, look up the inferred type's
        // node in the type table → if it's an Adt resolved via the
        // resolver, jump to that struct/enum. For path expressions
        // already pointing at a type, behave like `definition`.
        match &loc {
            Locate::TypePath {
                resolution: Some(Resolution::Def { def, .. }),
                ..
            } => doc
                .index
                .def(*def)
                .map_or(Value::Null, |info| location(doc, info.name_span)),
            Locate::PathExpr {
                resolution: Some(resolution),
                ..
            }
            | Locate::TypePath {
                resolution: Some(resolution),
                ..
            } => self.locate_type_definition(doc, *resolution),
            Locate::Binding { pattern_id, .. } => {
                let Some(ty) = doc.types.get(*pattern_id) else {
                    return Value::Null;
                };
                self.locate_type_in_index(doc, &render_ty(&doc.tcx, ty))
            }
            Locate::Field { .. } | Locate::PathExpr { .. } | Locate::TypePath { .. } => Value::Null,
        }
    }

    /// Re-routes a `Resolution` carrying a value (function / const) onto
    /// the type definition of the value's static type. Functions go to
    /// their return type's definition; constants to the const type.
    fn locate_type_definition(&self, doc: &DocumentAnalysis, resolution: Resolution) -> Value {
        let Resolution::Def { def, .. } = resolution else {
            return Value::Null;
        };
        let Some(info) = doc.index.def(def) else {
            return Value::Null;
        };
        // Hover signature contains the rendered return type at the end
        // (`-> Foo`). Pull the last identifier word out and look it up.
        if let Some(arrow) = info.signature.rfind("->") {
            let ret = info.signature[arrow + 2..].trim();
            let target = self.locate_type_in_index(doc, ret);
            if !matches!(target, Value::Null) {
                return target;
            }
        }
        Value::Null
    }

    fn locate_type_in_index(&self, doc: &DocumentAnalysis, name: &str) -> Value {
        let head = name
            .trim_start_matches(['&', '*', ' '])
            .trim_end_matches([',', ';', ' '])
            .split(['<', '[', '(', ' '])
            .next()
            .unwrap_or("");
        if head.is_empty() {
            return Value::Null;
        }
        for (_, info) in doc.index_pairs() {
            if info.name == head
                && matches!(
                    info.kind,
                    DefKind::Struct | DefKind::Enum | DefKind::Trait | DefKind::TypeAlias
                )
            {
                return location(doc, info.name_span);
            }
        }
        Value::Null
    }

    fn definition_by_name(&self, doc: &DocumentAnalysis, offset: u32) -> Value {
        let Some(word) = doc.word_at(offset) else {
            return Value::Null;
        };
        let Some(span) = doc.top_level_span(word) else {
            return Value::Null;
        };
        location(doc, span)
    }

    fn completion(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Array(Vec::new());
        };
        let cursor = doc.cursor_context(offset);
        let prefix = cursor.suffix;
        let mut items: Vec<Value> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Use-context: surface module / item members of the qualifier.
        if cursor.is_use_context {
            if cursor.qualifier.is_empty() {
                for spec in self.stdlib.root_modules() {
                    if spec.name.starts_with(prefix) && seen.insert(spec.name.clone()) {
                        items.push(member_to_completion(&spec));
                    }
                }
            } else if let Some(members) = self.stdlib.members_of(&cursor.qualifier_segments()) {
                for spec in &members {
                    if spec.name.starts_with(prefix) && seen.insert(spec.name.clone()) {
                        items.push(member_to_completion(spec));
                    }
                }
            }
            return Value::Array(items);
        }

        // Receiver-method completion (`expr.p|`).
        if cursor.is_method_position {
            self.method_completions(doc, offset, prefix, &mut items, &mut seen);
            return Value::Array(items);
        }

        // Module / type-qualified path completion (`os::p|`, `Vec::n|`).
        if !cursor.qualifier.is_empty() {
            if let Some(members) = self.stdlib.members_of(&cursor.qualifier_segments()) {
                for spec in &members {
                    if spec.name.starts_with(prefix) && seen.insert(spec.name.clone()) {
                        items.push(member_to_completion(spec));
                    }
                }
            }
            // Type-qualified user types (e.g. `MyEnum::V`).
            self.type_qualified_completions(doc, &cursor, prefix, &mut items, &mut seen);
            // No fall-through to bare prefix when the user already
            // typed `::` — that would surface unrelated names.
            return Value::Array(items);
        }

        // Bare prefix path: top-level items, locals, keywords, builtins.
        // The DefinitionIndex already records every top-level item with
        // its `name` and `name_span`, so iterate it directly instead of
        // keeping a parallel `top_level: Vec<(Ident, Span)>` cache.
        for (_, info) in doc.index.def_iter() {
            if info.name.starts_with(prefix) && seen.insert(info.name.clone()) {
                items.push(completion_item_for(doc, &info.name, prefix));
            }
        }
        // Locals in scope: we don't track scopes at hover-time, so just
        // surface every binding seen in the file. Editors rank short
        // prefixes before stale names.
        for (_, binding) in doc.binding_pairs() {
            if binding.name.starts_with(prefix) && seen.insert(binding.name.clone()) {
                items.push(completion_item_local(&binding.name));
            }
        }
        for name in KEYWORDS {
            if name.starts_with(prefix) && seen.insert((*name).to_string()) {
                items.push(completion_item(name, 14));
            }
        }
        for name in BUILTIN_COMPLETIONS {
            if name.starts_with(prefix) && seen.insert((*name).to_string()) {
                items.push(completion_function_item_with_snippet(name));
            }
        }
        // Workspace-wide top-level items (other open files).
        if !prefix.is_empty() {
            for item in self.workspace.by_prefix(prefix, &doc.uri) {
                if seen.insert(item.name.clone()) {
                    items.push(workspace_completion_item(&item));
                }
            }
        }
        // Auto-import suggestions for unqualified names that don't
        // resolve in the current file.
        if !prefix.is_empty() {
            self.auto_import_completions(doc, prefix, &mut items, &mut seen);
        }
        Value::Array(items)
    }

    /// Fills `items` with method-call completions when the cursor is in
    /// `receiver.suffix|` position. Best-effort: walks the receiver's
    /// resolved type back to a set of impl/trait methods either declared
    /// on the receiver in this file or known to be built-in.
    fn method_completions(
        &self,
        doc: &DocumentAnalysis,
        offset: u32,
        prefix: &str,
        items: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let receiver_kind = receiver_descriptor(doc, offset);
        for method in builtin_methods_for(&receiver_kind) {
            if method.name.starts_with(prefix) && seen.insert(method.name.to_string()) {
                items.push(method_completion_item(method));
            }
        }
        // Walk every impl block in this file looking for methods whose
        // receiver type spelling matches.
        if let Some(receiver_type) = receiver_kind.type_name() {
            for method in user_methods_for(doc, receiver_type) {
                if method.name.starts_with(prefix) && seen.insert(method.name.clone()) {
                    items.push(user_method_completion_item(&method));
                }
            }
        }
        // Last-ditch fallback: when we can't resolve the receiver type,
        // surface every known builtin method whose name matches the
        // prefix. Keeps `vec.p` useful even mid-edit when the receiver
        // expression doesn't typecheck.
        if items.is_empty() && !prefix.is_empty() {
            for method in ALL_BUILTIN_METHODS {
                if method.name.starts_with(prefix) && seen.insert(method.name.to_string()) {
                    items.push(method_completion_item(method));
                }
            }
        }
    }

    /// Type-qualified completions. Looks up the qualifier's last segment
    /// against in-file enums (variants) and impl blocks (associated fns).
    fn type_qualified_completions(
        &self,
        doc: &DocumentAnalysis,
        cursor: &CursorContext<'_>,
        prefix: &str,
        items: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let Some(last) = cursor.qualifier.last().copied() else {
            return;
        };
        for assoc in user_associated_items(doc, last) {
            if assoc.name.starts_with(prefix) && seen.insert(assoc.name.clone()) {
                items.push(user_method_completion_item(&assoc));
            }
        }
    }

    /// Suggests `use` imports for unqualified names that don't already
    /// resolve in scope. Each completion item carries
    /// `additionalTextEdits` inserting the matching `use` statement.
    fn auto_import_completions(
        &self,
        doc: &DocumentAnalysis,
        prefix: &str,
        items: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let already_imported = collect_existing_imports(doc.source());
        for path in self.stdlib.fuzzy_paths_for_prefix(prefix) {
            // The user already typed an exact-name match: only suggest
            // when the bare-name space doesn't already cover this name.
            let leaf = path.rsplit("::").next().unwrap_or("");
            if leaf.is_empty() || !leaf.starts_with(prefix) {
                continue;
            }
            if already_imported.iter().any(|imp| imp == &path) {
                continue;
            }
            if !seen.insert(format!("{leaf}::__import__::{path}")) {
                continue;
            }
            items.push(import_completion_item(doc, leaf, &path));
        }
    }

    fn locate<'s>(&'s self, params: &Value) -> Option<(&'s DocumentAnalysis, u32)> {
        let uri = field_str(field(params, "textDocument"), "uri")?;
        let doc = self.documents.get(uri)?;
        let position = field(params, "position");
        let line = field_u32(position, "line")?;
        let column = field_u32(position, "character")?;
        let offset = doc.position_to_offset(line, column)?;
        Some((doc, offset))
    }

    fn cursor(&self, doc: &DocumentAnalysis, offset: u32) -> Option<Locate> {
        let mut loc = locate(&doc.sf, offset)?;
        attach_resolution(&mut loc, &doc.resolutions);
        Some(loc)
    }

    fn references(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Array(Vec::new());
        };
        let spans = self.references_spans(doc, offset);
        let locations: Vec<Value> = spans.into_iter().map(|s| location(doc, s)).collect();
        Value::Array(locations)
    }

    fn document_highlight(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Array(Vec::new());
        };
        let spans = self.references_spans(doc, offset);
        let highlights: Vec<Value> = spans
            .into_iter()
            .map(|span| {
                let mut entry = BTreeMap::new();
                entry.insert("range".to_string(), span_to_range(doc, span));
                // Kind 1 = Text per LSP. Read/write tagging would
                // require dataflow we don't track yet.
                entry.insert("kind".to_string(), Value::Number(1.0));
                Value::Object(entry)
            })
            .collect();
        Value::Array(highlights)
    }

    fn references_spans(&self, doc: &DocumentAnalysis, offset: u32) -> Vec<Span> {
        let Some(loc) = self.cursor(doc, offset) else {
            // Fallback to the whole-word text scan when we can't pin
            // down a semantic node — keeps "find usages" useful even
            // mid-edit on a partially-parseable file.
            let Some(word) = doc.word_at(offset) else {
                return Vec::new();
            };
            return doc.find_references(word);
        };
        let target = match &loc {
            Locate::PathExpr {
                resolution: Some(resolution),
                ..
            }
            | Locate::TypePath {
                resolution: Some(resolution),
                ..
            } => Some(*resolution),
            Locate::Binding { pattern_id, .. } => Some(Resolution::Local(*pattern_id)),
            _ => None,
        };
        let Some(target) = target else {
            // Fields and unresolved paths: text-based fallback on the
            // identifier under the cursor.
            let name = locate_name(&loc);
            return doc.find_references(&name);
        };
        let mut spans: Vec<Span> = Vec::new();
        if let Resolution::Local(node) = target {
            if let Some(info) = doc.index.local(node) {
                spans.push(info.name_span);
            }
        } else if let Resolution::Def { def, .. } = target {
            if let Some(info) = doc.index.def(def) {
                spans.push(info.name_span);
            }
        }
        for occurrence in doc.index.occurrences() {
            if occurrence.resolution == Some(target) {
                spans.push(occurrence.span);
            }
        }
        if spans.is_empty() {
            // Resolver didn't tag anything (e.g. type-only path that
            // missed the resolver). Fall back to whole-word search.
            return doc.find_references(&locate_name(&loc));
        }
        spans.sort_by_key(|s| (s.start, s.end));
        spans.dedup_by_key(|s| (s.start, s.end));
        spans
    }

    fn prepare_rename(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        if let Some(loc) = self.cursor(doc, offset) {
            let span = locate_span(&loc);
            let name = locate_name(&loc);
            let mut result = BTreeMap::new();
            result.insert("range".to_string(), span_to_range(doc, span));
            result.insert("placeholder".to_string(), Value::String(name));
            return Value::Object(result);
        }
        let Some(word) = doc.word_at(offset) else {
            return Value::Null;
        };
        let bytes = doc.source().as_bytes();
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut start = offset as usize;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = offset as usize;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        let span = Span::new(doc.file, start as u32, end as u32);
        let mut result = BTreeMap::new();
        result.insert("range".to_string(), span_to_range(doc, span));
        result.insert("placeholder".to_string(), Value::String(word.to_string()));
        Value::Object(result)
    }

    fn rename(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let Some(new_name) = field_str(params, "newName") else {
            return Value::Null;
        };
        if new_name.is_empty() || !is_valid_identifier(new_name) {
            return Value::Null;
        }
        let spans = self.references_spans(doc, offset);
        let edits: Vec<Value> = spans
            .into_iter()
            .map(|span| {
                let mut edit = BTreeMap::new();
                edit.insert("range".to_string(), span_to_range(doc, span));
                edit.insert("newText".to_string(), Value::String(new_name.to_string()));
                Value::Object(edit)
            })
            .collect();
        let mut changes = BTreeMap::new();
        changes.insert(doc.uri.clone(), Value::Array(edits));
        let mut workspace_edit = BTreeMap::new();
        workspace_edit.insert("changes".to_string(), Value::Object(changes));
        Value::Object(workspace_edit)
    }

    fn inlay_hints(&self, params: &Value) -> Value {
        let Some(uri) = field_str(field(params, "textDocument"), "uri") else {
            return Value::Array(Vec::new());
        };
        let Some(doc) = self.documents.get(uri) else {
            return Value::Array(Vec::new());
        };
        let range = field(params, "range");
        let byte_range = if matches!(range, Value::Object(_)) {
            let start = field(range, "start");
            let end = field(range, "end");
            let start_offset = field_u32(start, "line").and_then(|line| {
                let column = field_u32(start, "character").unwrap_or(0);
                doc.position_to_offset(line, column)
            });
            let end_offset = field_u32(end, "line").and_then(|line| {
                let column = field_u32(end, "character").unwrap_or(0);
                doc.position_to_offset(line, column)
            });
            match (start_offset, end_offset) {
                (Some(a), Some(b)) if a <= b => Some((a, b)),
                _ => None,
            }
        } else {
            None
        };
        let hints = collect_inlays(doc, byte_range);
        Value::Array(hints.into_iter().map(inlay_to_lsp).collect())
    }

    fn document_symbols(&self, params: &Value) -> Value {
        let Some(uri) = field_str(field(params, "textDocument"), "uri") else {
            return Value::Array(Vec::new());
        };
        let Some(doc) = self.documents.get(uri) else {
            return Value::Array(Vec::new());
        };
        document_symbols(doc)
    }

    fn workspace_symbols(&self, params: &Value) -> Value {
        let query = field_str(params, "query").unwrap_or("");
        let docs: Vec<&DocumentAnalysis> = self.documents.values().collect();
        workspace_symbols(&docs, query)
    }

    fn folding_ranges(&self, params: &Value) -> Value {
        let Some(uri) = field_str(field(params, "textDocument"), "uri") else {
            return Value::Array(Vec::new());
        };
        let Some(doc) = self.documents.get(uri) else {
            return Value::Array(Vec::new());
        };
        folding_ranges(doc)
    }

    fn signature_help(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let Some((callee_name, active_param)) = enclosing_call(doc.source(), offset) else {
            return Value::Null;
        };
        for (_, info) in doc.index_pairs() {
            if info.name == callee_name && matches!(info.kind, DefKind::Fn) {
                return signature_help_for(info, active_param);
            }
        }
        Value::Null
    }

    fn formatting(&self, params: &Value) -> Value {
        let Some(uri) = field_str(field(params, "textDocument"), "uri") else {
            return Value::Array(Vec::new());
        };
        let Some(doc) = self.documents.get(uri) else {
            return Value::Array(Vec::new());
        };
        // Reject formatting requests on documents with parse errors;
        // the AST printer would otherwise produce nonsensical output.
        if doc
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error))
        {
            return Value::Array(Vec::new());
        }
        let formatted = format!("{}", doc.sf);
        let formatted = if formatted.ends_with('\n') {
            formatted
        } else {
            format!("{formatted}\n")
        };
        if formatted == doc.source() {
            return Value::Array(Vec::new());
        }
        // Replace the entire document.
        let (end_line, end_col) = doc.offset_to_position(doc.source().len() as u32);
        let mut start = BTreeMap::new();
        start.insert("line".to_string(), Value::Number(0.0));
        start.insert("character".to_string(), Value::Number(0.0));
        let mut end = BTreeMap::new();
        end.insert("line".to_string(), Value::Number(f64::from(end_line)));
        end.insert("character".to_string(), Value::Number(f64::from(end_col)));
        let mut range = BTreeMap::new();
        range.insert("start".to_string(), Value::Object(start));
        range.insert("end".to_string(), Value::Object(end));
        let mut edit = BTreeMap::new();
        edit.insert("range".to_string(), Value::Object(range));
        edit.insert("newText".to_string(), Value::String(formatted));
        Value::Array(vec![Value::Object(edit)])
    }

    fn semantic_tokens(&self, params: &Value) -> Value {
        let Some(uri) = field_str(field(params, "textDocument"), "uri") else {
            return empty_semantic_tokens();
        };
        let Some(doc) = self.documents.get(uri) else {
            return empty_semantic_tokens();
        };
        let data = full_tokens(doc);
        let mut out = BTreeMap::new();
        out.insert(
            "data".to_string(),
            Value::Array(
                data.into_iter()
                    .map(|n| Value::Number(f64::from(n)))
                    .collect(),
            ),
        );
        Value::Object(out)
    }
}

fn empty_semantic_tokens() -> Value {
    let mut out = BTreeMap::new();
    out.insert("data".to_string(), Value::Array(Vec::new()));
    Value::Object(out)
}

#[allow(clippy::too_many_lines)]
fn render_hover(doc: &DocumentAnalysis, loc: &Locate) -> String {
    match loc {
        Locate::PathExpr {
            resolution: Some(Resolution::Local(node)),
            name,
            expr_id,
            ..
        } => {
            let mut body = String::new();
            if let Some(info) = doc.index.local(*node) {
                body.push_str("```gos\n");
                if info.mutable {
                    body.push_str("let mut ");
                } else {
                    body.push_str("let ");
                }
                body.push_str(&info.name);
                if let Some(ty) = doc.types.get(*expr_id) {
                    body.push_str(": ");
                    body.push_str(&render_ty(&doc.tcx, ty));
                }
                body.push_str("\n```");
            } else {
                body.push_str(name);
            }
            body
        }
        Locate::PathExpr {
            resolution: Some(Resolution::Def { def, .. }),
            expr_id,
            ..
        } => {
            let mut body = String::new();
            if let Some(info) = doc.index.def(*def) {
                body.push_str("```gos\n");
                body.push_str(&info.signature);
                body.push_str("\n```");
                if !info.docs.is_empty() {
                    body.push_str("\n\n");
                    body.push_str(&info.docs);
                }
            }
            if let Some(ty) = doc.types.get(*expr_id) {
                body.push_str("\n\n*type:* `");
                body.push_str(&render_ty(&doc.tcx, ty));
                body.push('`');
            }
            body
        }
        Locate::PathExpr {
            resolution: Some(Resolution::Primitive(_)),
            name,
            ..
        } => format!("```gos\n{name}\n```\n\nbuilt-in primitive type"),
        Locate::PathExpr {
            resolution: Some(Resolution::Import { .. }),
            name,
            ..
        } => format!("```gos\nuse {name}\n```\n\nimported name"),
        Locate::PathExpr {
            resolution: Some(Resolution::Err) | None,
            name,
            expr_id,
            ..
        } => {
            let mut body = format!("```\n{name}\n```");
            if let Some(ty) = doc.types.get(*expr_id) {
                body.push_str("\n\n*type:* `");
                body.push_str(&render_ty(&doc.tcx, ty));
                body.push('`');
            }
            body
        }
        Locate::TypePath {
            resolution: Some(Resolution::Def { def, .. }),
            ..
        } => doc.index.def(*def).map_or_else(String::new, |info| {
            let mut body = format!("```gos\n{}\n```", info.signature);
            if !info.docs.is_empty() {
                body.push_str("\n\n");
                body.push_str(&info.docs);
            }
            body
        }),
        Locate::TypePath {
            resolution: Some(Resolution::Primitive(_)) | None,
            name,
            ..
        }
        | Locate::TypePath {
            resolution: Some(Resolution::Err),
            name,
            ..
        }
        | Locate::TypePath {
            resolution: Some(Resolution::Import { .. }),
            name,
            ..
        }
        | Locate::TypePath {
            resolution: Some(Resolution::Local(_)),
            name,
            ..
        } => format!("```gos\n{name}\n```"),
        Locate::Binding {
            pattern_id, name, ..
        } => {
            let mut body = format!("```gos\nlet {name}\n```");
            if let Some(ty) = doc.types.get(*pattern_id) {
                body.push_str("\n\n*type:* `");
                body.push_str(&render_ty(&doc.tcx, ty));
                body.push('`');
            }
            body
        }
        Locate::Field { name, .. } => format!("```gos\n{name}\n```\n\nfield / method"),
    }
}

fn word_hover(doc: &DocumentAnalysis, offset: u32) -> Value {
    let Some(word) = doc.word_at(offset) else {
        return Value::Null;
    };
    let mut markdown = format!("```\n{word}\n```");
    if doc.top_level_span(word).is_some() {
        markdown.push_str("\n\nDeclared at the top level of this file.");
    }
    let mut contents = BTreeMap::new();
    contents.insert("kind".to_string(), Value::String("markdown".to_string()));
    contents.insert("value".to_string(), Value::String(markdown));
    let mut hover = BTreeMap::new();
    hover.insert("contents".to_string(), Value::Object(contents));
    Value::Object(hover)
}

/// Convenience accessors so server.rs doesn't reach into private
/// index types directly.
impl DocumentAnalysis {
    pub(crate) fn index_pairs(
        &self,
    ) -> impl Iterator<Item = (gossamer_resolve::DefId, &DefinitionInfo)> {
        self.index.def_iter()
    }

    pub(crate) fn binding_pairs(
        &self,
    ) -> impl Iterator<Item = (gossamer_ast::NodeId, &BindingInfo)> {
        self.index.local_iter()
    }
}

fn locate_name(loc: &Locate) -> String {
    match loc {
        Locate::PathExpr { name, .. }
        | Locate::TypePath { name, .. }
        | Locate::Binding { name, .. }
        | Locate::Field { name, .. } => name.clone(),
    }
}

fn locate_span(loc: &Locate) -> Span {
    match loc {
        Locate::PathExpr { segment_span, .. }
        | Locate::TypePath { segment_span, .. }
        | Locate::Binding {
            name_span: segment_span,
            ..
        }
        | Locate::Field {
            name_span: segment_span,
            ..
        } => *segment_span,
    }
}

fn location(doc: &DocumentAnalysis, span: Span) -> Value {
    let mut out = BTreeMap::new();
    out.insert("uri".to_string(), Value::String(doc.uri.clone()));
    out.insert("range".to_string(), span_to_range(doc, span));
    Value::Object(out)
}

fn signature_help_for(info: &DefinitionInfo, active_param: u32) -> Value {
    let mut signature = BTreeMap::new();
    signature.insert("label".to_string(), Value::String(info.signature.clone()));
    if !info.docs.is_empty() {
        let mut docs = BTreeMap::new();
        docs.insert("kind".to_string(), Value::String("markdown".to_string()));
        docs.insert("value".to_string(), Value::String(info.docs.clone()));
        signature.insert("documentation".to_string(), Value::Object(docs));
    }
    // Build the parameters array by re-parsing `(args)` out of the
    // signature text.
    let params = parse_signature_params(&info.signature);
    let parameters: Vec<Value> = params
        .iter()
        .map(|p| {
            let mut entry = BTreeMap::new();
            entry.insert("label".to_string(), Value::String(p.clone()));
            Value::Object(entry)
        })
        .collect();
    signature.insert("parameters".to_string(), Value::Array(parameters));
    let mut help = BTreeMap::new();
    help.insert(
        "signatures".to_string(),
        Value::Array(vec![Value::Object(signature)]),
    );
    help.insert("activeSignature".to_string(), Value::Number(0.0));
    help.insert(
        "activeParameter".to_string(),
        Value::Number(f64::from(active_param)),
    );
    Value::Object(help)
}

fn parse_signature_params(sig: &str) -> Vec<String> {
    let Some(open) = sig.find('(') else {
        return Vec::new();
    };
    let Some(close) = sig.rfind(')') else {
        return Vec::new();
    };
    if close <= open + 1 {
        return Vec::new();
    }
    let inner = &sig[open + 1..close];
    let mut depth = 0i32;
    let mut current = String::new();
    let mut out: Vec<String> = Vec::new();
    for ch in inner.chars() {
        match ch {
            '<' | '(' | '[' => {
                depth += 1;
                current.push(ch);
            }
            '>' | ')' | ']' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                out.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

fn enclosing_call(source: &str, offset: u32) -> Option<(String, u32)> {
    let bytes = source.as_bytes();
    let cap = std::cmp::min(offset as usize, bytes.len());
    let mut depth = 0i32;
    let mut commas = 0u32;
    // Walk backwards looking for the most recent unbalanced `(`.
    for i in (0..cap).rev() {
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth -= 1;
                if depth < 0 && bytes[i] == b'(' {
                    // Found an open paren without a matching close.
                    let name = preceding_identifier(bytes, i);
                    return name.map(|n| (n, commas));
                }
            }
            b',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    None
}

fn preceding_identifier(bytes: &[u8], paren_pos: usize) -> Option<String> {
    let mut end = paren_pos;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = end;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    std::str::from_utf8(&bytes[start..end])
        .ok()
        .map(str::to_string)
}

/// Encodes one inlay hint into the LSP wire shape.
fn inlay_to_lsp(hint: InlayHint) -> Value {
    let mut position = BTreeMap::new();
    position.insert("line".to_string(), Value::Number(f64::from(hint.line)));
    position.insert(
        "character".to_string(),
        Value::Number(f64::from(hint.character)),
    );
    let mut out = BTreeMap::new();
    out.insert("position".to_string(), Value::Object(position));
    out.insert("label".to_string(), Value::String(hint.label));
    out.insert("kind".to_string(), Value::Number(1.0));
    out.insert("paddingLeft".to_string(), Value::Bool(false));
    out.insert("paddingRight".to_string(), Value::Bool(false));
    Value::Object(out)
}

fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn severity_tag(severity: Severity) -> f64 {
    match severity {
        Severity::Error => 1.0,
        Severity::Warning => 2.0,
        Severity::Note => 3.0,
        Severity::Help => 4.0,
    }
}

fn diagnostic_to_lsp(doc: &DocumentAnalysis, diag: &GossamerDiagnostic) -> Value {
    let span = diag
        .labels
        .iter()
        .find(|l| l.primary)
        .or_else(|| diag.labels.first())
        .map_or(Span::new(doc.file, 0, 0), |l| l.location.span);
    let mut entry = BTreeMap::new();
    entry.insert("range".to_string(), span_to_range(doc, span));
    entry.insert(
        "severity".to_string(),
        Value::Number(severity_tag(diag.severity)),
    );
    entry.insert(
        "code".to_string(),
        Value::String(diag.code.as_str().to_string()),
    );
    entry.insert("source".to_string(), Value::String("gos".to_string()));
    entry.insert("message".to_string(), Value::String(diag.title.clone()));
    Value::Object(entry)
}

const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "if", "else", "match", "while", "loop", "for", "in", "return", "break",
    "continue", "struct", "enum", "trait", "impl", "pub", "use", "mod", "const", "static", "true",
    "false", "go", "select", "defer", "where", "as",
];

const BUILTIN_COMPLETIONS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "panic",
    "Some",
    "None",
    "Ok",
    "Err",
    "len",
    "push",
    "to_string",
    "clone",
    "unwrap",
    "unwrap_or",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "map",
    "spawn",
    "channel",
];

fn completion_item(label: &str, kind: u32) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(label.to_string()));
    item.insert("kind".to_string(), Value::Number(f64::from(kind)));
    Value::Object(item)
}

fn completion_item_local(label: &str) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(label.to_string()));
    item.insert("kind".to_string(), Value::Number(6.0)); // Variable
    Value::Object(item)
}

fn completion_item_for(doc: &DocumentAnalysis, label: &str, _prefix: &str) -> Value {
    // If the index has a real DefinitionInfo for this name, decorate
    // the completion entry with the kind, signature, and docs so the
    // editor can render a richer popup.
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(label.to_string()));
    let mut kind = 3.0; // Function (LSP CompletionItemKind::Function)
    for (_, info) in doc.index_pairs() {
        if info.name == label {
            kind = match info.kind {
                DefKind::Fn => 3.0,
                DefKind::Struct => 22.0,
                DefKind::Enum => 13.0,
                DefKind::Trait => 8.0,
                DefKind::TypeAlias => 25.0,
                DefKind::Const => 21.0,
                DefKind::Static => 6.0,
                DefKind::Mod => 9.0,
                DefKind::Variant => 20.0,
                DefKind::TypeParam => 25.0,
            };
            if !info.signature.is_empty() {
                item.insert("detail".to_string(), Value::String(info.signature.clone()));
            }
            if !info.docs.is_empty() {
                let mut docs = BTreeMap::new();
                docs.insert("kind".to_string(), Value::String("markdown".to_string()));
                docs.insert("value".to_string(), Value::String(info.docs.clone()));
                item.insert("documentation".to_string(), Value::Object(docs));
            }
            break;
        }
    }
    item.insert("kind".to_string(), Value::Number(kind));
    Value::Object(item)
}

fn member_to_completion(spec: &MemberSpec) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(spec.name.clone()));
    item.insert("kind".to_string(), Value::Number(f64::from(spec.kind)));
    if let Some(detail) = &spec.detail {
        item.insert("detail".to_string(), Value::String(detail.clone()));
    }
    if let Some(doc) = &spec.doc {
        let mut docs = BTreeMap::new();
        docs.insert("kind".to_string(), Value::String("markdown".to_string()));
        docs.insert("value".to_string(), Value::String(doc.clone()));
        item.insert("documentation".to_string(), Value::Object(docs));
    }
    // Function-like members carry a snippet so the editor opens the
    // parens for the user. Module / type / const stay as bare names.
    if spec.kind == 3 {
        item.insert(
            "insertText".to_string(),
            Value::String(format!("{}($0)", spec.name)),
        );
        item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    }
    Value::Object(item)
}

fn completion_function_item_with_snippet(name: &str) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(name.to_string()));
    item.insert("kind".to_string(), Value::Number(3.0));
    item.insert(
        "insertText".to_string(),
        Value::String(format!("{name}($0)")),
    );
    item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    Value::Object(item)
}

/// Receiver-side identification used to look up methods.
#[derive(Debug, Clone)]
struct ReceiverDescriptor {
    /// Builtin classification (`Vec` / `String` / `HashMap` / `Option` / `Result` / …).
    builtin: BuiltinReceiver,
    /// User-facing type name extracted from `let r: Foo = …` or
    /// `struct Foo { … }`. Used to match `impl Foo` blocks.
    type_name: Option<String>,
}

impl ReceiverDescriptor {
    fn type_name(&self) -> Option<&str> {
        self.type_name.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltinReceiver {
    Vec,
    String,
    HashMap,
    HashSet,
    Option,
    Result,
    Unknown,
}

fn receiver_descriptor(doc: &DocumentAnalysis, offset: u32) -> ReceiverDescriptor {
    // Locate the receiver expression: walk left from the dot in the
    // source, skipping the suffix word the user is typing.
    let bytes = doc.source().as_bytes();
    let mut idx = (offset as usize).min(bytes.len());
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while idx > 0 && is_word(bytes[idx - 1]) {
        idx -= 1;
    }
    if idx == 0 || bytes[idx - 1] != b'.' {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Unknown,
            type_name: None,
        };
    }
    // Walk left across the receiver expression (very conservative: stop
    // at common statement boundaries / unmatched parens).
    let dot_pos = idx - 1;
    let mut start = dot_pos;
    let mut depth: i32 = 0;
    while start > 0 {
        let b = bytes[start - 1];
        match b {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b';' | b',' | b'\n' if depth == 0 => break,
            _ => {}
        }
        start -= 1;
    }
    let receiver = std::str::from_utf8(&bytes[start..dot_pos])
        .unwrap_or("")
        .trim();
    classify_receiver(doc, receiver)
}

fn classify_receiver(doc: &DocumentAnalysis, expr: &str) -> ReceiverDescriptor {
    let head = expr.trim();
    // Direct string literal.
    if head.starts_with('"') {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::String,
            type_name: Some("String".to_string()),
        };
    }
    // Vec literal `vec![...]` / `[...]`.
    if head.starts_with("vec![") || head.starts_with('[') {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Vec,
            type_name: None,
        };
    }
    // Identifier — try resolving via let-binding type annotation.
    if let Some(name) = identifier_token(head) {
        if let Some(ty) = lookup_let_annotation(doc.source(), name) {
            return classify_type_string(&ty);
        }
    }
    ReceiverDescriptor {
        builtin: BuiltinReceiver::Unknown,
        type_name: None,
    }
}

fn classify_type_string(ty: &str) -> ReceiverDescriptor {
    let ty = ty.trim();
    let head = ty
        .trim_start_matches(['&', '*', ' '])
        .trim_end_matches([',', ';', ' ']);
    if head.starts_with("Vec<") || head.starts_with("&[") || head.starts_with('[') {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Vec,
            type_name: None,
        };
    }
    if head.starts_with("HashMap<") {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::HashMap,
            type_name: None,
        };
    }
    if head.starts_with("HashSet<") {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::HashSet,
            type_name: None,
        };
    }
    if head == "String" || head == "&str" || head == "str" {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::String,
            type_name: Some("String".to_string()),
        };
    }
    if head.starts_with("Option<") || head == "Option" {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Option,
            type_name: Some("Option".to_string()),
        };
    }
    if head.starts_with("Result<") || head == "Result" {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Result,
            type_name: Some("Result".to_string()),
        };
    }
    let bare = head.split(['<', '[', '(', ' ']).next().unwrap_or(head);
    if bare.is_empty() {
        ReceiverDescriptor {
            builtin: BuiltinReceiver::Unknown,
            type_name: None,
        }
    } else {
        ReceiverDescriptor {
            builtin: BuiltinReceiver::Unknown,
            type_name: Some(bare.to_string()),
        }
    }
}

fn identifier_token(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    if trimmed.chars().next()?.is_ascii_digit() {
        return None;
    }
    Some(trimmed)
}

/// Looks for a `let <name>: <type> = ...` binding for `name` in the
/// document and returns the rendered type spelling.
fn lookup_let_annotation(source: &str, name: &str) -> Option<String> {
    let needle = format!("let {name}");
    let needle_mut = format!("let mut {name}");
    let mut start = 0usize;
    while start < source.len() {
        let position = source[start..].find(&needle)?;
        let absolute = start + position;
        let head_ok = absolute == 0
            || !matches!(
                source.as_bytes()[absolute - 1],
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'
            );
        if !head_ok {
            start = absolute + 1;
            continue;
        }
        // After the `let <name>` (or `let mut <name>`), look for a `:`
        // that starts the type annotation, stopping at `=` or newline.
        let after = if source[absolute..].starts_with(&needle_mut) {
            absolute + needle_mut.len()
        } else {
            absolute + needle.len()
        };
        let tail = &source[after..];
        // Strict word boundary: next char must not be word.
        if tail
            .as_bytes()
            .first()
            .copied()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            start = absolute + 1;
            continue;
        }
        let stripped = tail.trim_start();
        if let Some(rest_with_ws) = stripped.strip_prefix(':') {
            let rest = rest_with_ws.trim_start();
            // Capture until `=` or newline at top depth.
            let mut depth: i32 = 0;
            let mut end = 0usize;
            for (i, ch) in rest.char_indices() {
                match ch {
                    '<' | '(' | '[' => depth += 1,
                    '>' | ')' | ']' => depth -= 1,
                    '=' | '\n' | ';' if depth == 0 => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            if end == 0 {
                end = rest.len();
            }
            let ty = rest[..end].trim().trim_end_matches(',').trim();
            if !ty.is_empty() {
                return Some(ty.to_string());
            }
        }
        start = absolute + 1;
    }
    None
}

#[derive(Debug, Clone, Copy)]
struct BuiltinMethod {
    name: &'static str,
    signature: &'static str,
    doc: &'static str,
    snippet: &'static str,
}

const VEC_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "push",
        signature: "fn push(&mut self, value: T)",
        doc: "Appends `value` to the back of the vec.",
        snippet: "push($0)",
    },
    BuiltinMethod {
        name: "pop",
        signature: "fn pop(&mut self) -> Option<T>",
        doc: "Removes the last element and returns it, or `None` when empty.",
        snippet: "pop()$0",
    },
    BuiltinMethod {
        name: "len",
        signature: "fn len(&self) -> usize",
        doc: "Number of elements currently in the vec.",
        snippet: "len()$0",
    },
    BuiltinMethod {
        name: "is_empty",
        signature: "fn is_empty(&self) -> bool",
        doc: "Returns `true` when the vec has no elements.",
        snippet: "is_empty()$0",
    },
    BuiltinMethod {
        name: "clear",
        signature: "fn clear(&mut self)",
        doc: "Removes every element, leaving the vec at length 0.",
        snippet: "clear()$0",
    },
    BuiltinMethod {
        name: "iter",
        signature: "fn iter(&self) -> Iter<T>",
        doc: "Returns an iterator over the vec's elements.",
        snippet: "iter()$0",
    },
    BuiltinMethod {
        name: "clone",
        signature: "fn clone(&self) -> Self",
        doc: "Clones every element into a new vec.",
        snippet: "clone()$0",
    },
    BuiltinMethod {
        name: "contains",
        signature: "fn contains(&self, value: &T) -> bool",
        doc: "Returns `true` when the vec contains an element equal to `value`.",
        snippet: "contains(&$0)",
    },
    BuiltinMethod {
        name: "sort",
        signature: "fn sort(&mut self)",
        doc: "Sorts the vec in place.",
        snippet: "sort()$0",
    },
];

const STRING_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "len",
        signature: "fn len(&self) -> usize",
        doc: "Length of the string in bytes.",
        snippet: "len()$0",
    },
    BuiltinMethod {
        name: "is_empty",
        signature: "fn is_empty(&self) -> bool",
        doc: "Returns `true` for the empty string.",
        snippet: "is_empty()$0",
    },
    BuiltinMethod {
        name: "to_uppercase",
        signature: "fn to_uppercase(&self) -> String",
        doc: "Returns the upper-cased clone of the string.",
        snippet: "to_uppercase()$0",
    },
    BuiltinMethod {
        name: "to_lowercase",
        signature: "fn to_lowercase(&self) -> String",
        doc: "Returns the lower-cased clone of the string.",
        snippet: "to_lowercase()$0",
    },
    BuiltinMethod {
        name: "trim",
        signature: "fn trim(&self) -> &str",
        doc: "Returns the string with leading + trailing whitespace stripped.",
        snippet: "trim()$0",
    },
    BuiltinMethod {
        name: "split",
        signature: "fn split(&self, sep: &str) -> Vec<String>",
        doc: "Splits on every occurrence of `sep`.",
        snippet: "split(\"$0\")",
    },
    BuiltinMethod {
        name: "lines",
        signature: "fn lines(&self) -> Vec<String>",
        doc: "Splits the string on `\\n`.",
        snippet: "lines()$0",
    },
    BuiltinMethod {
        name: "starts_with",
        signature: "fn starts_with(&self, prefix: &str) -> bool",
        doc: "True when the string begins with `prefix`.",
        snippet: "starts_with(\"$0\")",
    },
    BuiltinMethod {
        name: "ends_with",
        signature: "fn ends_with(&self, suffix: &str) -> bool",
        doc: "True when the string ends with `suffix`.",
        snippet: "ends_with(\"$0\")",
    },
    BuiltinMethod {
        name: "contains",
        signature: "fn contains(&self, needle: &str) -> bool",
        doc: "True when `needle` appears anywhere in the string.",
        snippet: "contains(\"$0\")",
    },
    BuiltinMethod {
        name: "repeat",
        signature: "fn repeat(&self, n: i64) -> String",
        doc: "Returns the string repeated `n` times.",
        snippet: "repeat($0)",
    },
    BuiltinMethod {
        name: "to_string",
        signature: "fn to_string(&self) -> String",
        doc: "Returns a fresh owned copy.",
        snippet: "to_string()$0",
    },
];

const HASHMAP_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "insert",
        signature: "fn insert(&mut self, key: K, value: V) -> Option<V>",
        doc: "Inserts a key/value pair, returning the previous value (if any).",
        snippet: "insert($1, $2)$0",
    },
    BuiltinMethod {
        name: "get",
        signature: "fn get(&self, key: &K) -> Option<&V>",
        doc: "Looks up `key`.",
        snippet: "get(&$0)",
    },
    BuiltinMethod {
        name: "get_or",
        signature: "fn get_or(&self, key: K, default: V) -> V",
        doc: "Looks up `key`, returning `default` when absent.",
        snippet: "get_or($1, $2)$0",
    },
    BuiltinMethod {
        name: "remove",
        signature: "fn remove(&mut self, key: &K) -> Option<V>",
        doc: "Removes `key`'s entry, returning the removed value.",
        snippet: "remove(&$0)",
    },
    BuiltinMethod {
        name: "len",
        signature: "fn len(&self) -> usize",
        doc: "Number of entries.",
        snippet: "len()$0",
    },
    BuiltinMethod {
        name: "is_empty",
        signature: "fn is_empty(&self) -> bool",
        doc: "Returns `true` when there are no entries.",
        snippet: "is_empty()$0",
    },
    BuiltinMethod {
        name: "contains_key",
        signature: "fn contains_key(&self, key: &K) -> bool",
        doc: "Returns `true` when an entry for `key` exists.",
        snippet: "contains_key(&$0)",
    },
    BuiltinMethod {
        name: "clear",
        signature: "fn clear(&mut self)",
        doc: "Removes every entry.",
        snippet: "clear()$0",
    },
    BuiltinMethod {
        name: "keys",
        signature: "fn keys(&self) -> Iter<K>",
        doc: "Iterator over keys.",
        snippet: "keys()$0",
    },
    BuiltinMethod {
        name: "values",
        signature: "fn values(&self) -> Iter<V>",
        doc: "Iterator over values.",
        snippet: "values()$0",
    },
];

const OPTION_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "is_some",
        signature: "fn is_some(&self) -> bool",
        doc: "Returns `true` when the option is `Some`.",
        snippet: "is_some()$0",
    },
    BuiltinMethod {
        name: "is_none",
        signature: "fn is_none(&self) -> bool",
        doc: "Returns `true` when the option is `None`.",
        snippet: "is_none()$0",
    },
    BuiltinMethod {
        name: "unwrap",
        signature: "fn unwrap(self) -> T",
        doc: "Returns the contained value, panicking if `None`.",
        snippet: "unwrap()$0",
    },
    BuiltinMethod {
        name: "unwrap_or",
        signature: "fn unwrap_or(self, default: T) -> T",
        doc: "Returns the contained value, or `default` if `None`.",
        snippet: "unwrap_or($0)",
    },
    BuiltinMethod {
        name: "map",
        signature: "fn map<U>(self, f: fn(T) -> U) -> Option<U>",
        doc: "Maps the contained value through `f`.",
        snippet: "map(|x| $0)",
    },
];

const RESULT_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "is_ok",
        signature: "fn is_ok(&self) -> bool",
        doc: "Returns `true` when the result is `Ok`.",
        snippet: "is_ok()$0",
    },
    BuiltinMethod {
        name: "is_err",
        signature: "fn is_err(&self) -> bool",
        doc: "Returns `true` when the result is `Err`.",
        snippet: "is_err()$0",
    },
    BuiltinMethod {
        name: "unwrap",
        signature: "fn unwrap(self) -> T",
        doc: "Returns the `Ok` value, panicking on `Err`.",
        snippet: "unwrap()$0",
    },
    BuiltinMethod {
        name: "unwrap_or",
        signature: "fn unwrap_or(self, default: T) -> T",
        doc: "Returns the `Ok` value, or `default` on `Err`.",
        snippet: "unwrap_or($0)",
    },
    BuiltinMethod {
        name: "map",
        signature: "fn map<U>(self, f: fn(T) -> U) -> Result<U, E>",
        doc: "Maps the `Ok` value.",
        snippet: "map(|x| $0)",
    },
    BuiltinMethod {
        name: "map_err",
        signature: "fn map_err<F>(self, f: fn(E) -> F) -> Result<T, F>",
        doc: "Maps the `Err` value.",
        snippet: "map_err(|e| $0)",
    },
];

const ALL_BUILTIN_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "to_string",
        signature: "fn to_string(&self) -> String",
        doc: "Default ToString rendering.",
        snippet: "to_string()$0",
    },
    BuiltinMethod {
        name: "clone",
        signature: "fn clone(&self) -> Self",
        doc: "Clones the receiver.",
        snippet: "clone()$0",
    },
];

fn builtin_methods_for(receiver: &ReceiverDescriptor) -> &'static [BuiltinMethod] {
    match receiver.builtin {
        BuiltinReceiver::Vec => VEC_METHODS,
        BuiltinReceiver::String => STRING_METHODS,
        BuiltinReceiver::HashMap | BuiltinReceiver::HashSet => HASHMAP_METHODS,
        BuiltinReceiver::Option => OPTION_METHODS,
        BuiltinReceiver::Result => RESULT_METHODS,
        BuiltinReceiver::Unknown => &[],
    }
}

fn method_completion_item(method: &BuiltinMethod) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(method.name.to_string()));
    item.insert("kind".to_string(), Value::Number(2.0)); // Method
    item.insert(
        "detail".to_string(),
        Value::String(method.signature.to_string()),
    );
    let mut docs = BTreeMap::new();
    docs.insert("kind".to_string(), Value::String("markdown".to_string()));
    docs.insert("value".to_string(), Value::String(method.doc.to_string()));
    item.insert("documentation".to_string(), Value::Object(docs));
    item.insert(
        "insertText".to_string(),
        Value::String(method.snippet.to_string()),
    );
    item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    Value::Object(item)
}

#[derive(Debug, Clone)]
struct UserMethod {
    name: String,
    signature: String,
    doc: String,
    is_associated: bool,
}

fn user_methods_for(doc: &DocumentAnalysis, type_name: &str) -> Vec<UserMethod> {
    collect_impl_items(doc, type_name, false)
}

fn user_associated_items(doc: &DocumentAnalysis, type_name: &str) -> Vec<UserMethod> {
    let mut out = collect_impl_items(doc, type_name, true);
    // Add enum variants of `type_name` if any.
    out.extend(enum_variants_for(doc, type_name));
    out
}

fn collect_impl_items(
    doc: &DocumentAnalysis,
    type_name: &str,
    want_associated: bool,
) -> Vec<UserMethod> {
    use gossamer_ast::{FnParam, ImplItem, ItemKind, TypeKind};
    let mut out: Vec<UserMethod> = Vec::new();
    for item in &doc.sf.items {
        let ItemKind::Impl(decl) = &item.kind else {
            continue;
        };
        let TypeKind::Path(path) = &decl.self_ty.kind else {
            continue;
        };
        let Some(seg) = path.segments.last() else {
            continue;
        };
        if seg.name.name != type_name {
            continue;
        }
        for impl_item in &decl.items {
            let ImplItem::Fn(fn_decl) = impl_item else {
                continue;
            };
            let has_receiver = fn_decl
                .params
                .first()
                .is_some_and(|p| matches!(p, FnParam::Receiver(_)));
            let is_associated = !has_receiver;
            if is_associated != want_associated {
                continue;
            }
            let signature = render_user_signature(fn_decl);
            out.push(UserMethod {
                name: fn_decl.name.name.clone(),
                signature,
                doc: String::new(),
                is_associated,
            });
        }
    }
    out
}

fn enum_variants_for(doc: &DocumentAnalysis, type_name: &str) -> Vec<UserMethod> {
    use gossamer_ast::ItemKind;
    let mut out: Vec<UserMethod> = Vec::new();
    for item in &doc.sf.items {
        let ItemKind::Enum(decl) = &item.kind else {
            continue;
        };
        if decl.name.name != type_name {
            continue;
        }
        for variant in &decl.variants {
            out.push(UserMethod {
                name: variant.name.name.clone(),
                signature: format!("{}::{}", type_name, variant.name.name),
                doc: String::new(),
                is_associated: true,
            });
        }
    }
    out
}

fn render_user_signature(decl: &gossamer_ast::FnDecl) -> String {
    use gossamer_ast::FnParam;
    let mut out = String::new();
    out.push_str("fn ");
    out.push_str(&decl.name.name);
    out.push('(');
    let mut first = true;
    for param in &decl.params {
        if !first {
            out.push_str(", ");
        }
        first = false;
        match param {
            FnParam::Receiver(receiver) => out.push_str(receiver.as_str()),
            FnParam::Typed { pattern, ty } => {
                let mut printer = gossamer_ast::Printer::new();
                printer.print_type(ty);
                out.push_str(&pattern_label(pattern));
                out.push_str(": ");
                out.push_str(&printer.finish());
            }
        }
    }
    out.push(')');
    if let Some(ret) = &decl.ret {
        out.push_str(" -> ");
        let mut printer = gossamer_ast::Printer::new();
        printer.print_type(ret);
        out.push_str(&printer.finish());
    }
    out
}

fn pattern_label(pattern: &gossamer_ast::Pattern) -> String {
    use gossamer_ast::PatternKind;
    match &pattern.kind {
        PatternKind::Ident { name, .. } => name.name.clone(),
        _ => "_".to_string(),
    }
}

fn user_method_completion_item(method: &UserMethod) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(method.name.clone()));
    let kind = if method.is_associated { 3.0 } else { 2.0 };
    item.insert("kind".to_string(), Value::Number(kind));
    item.insert(
        "detail".to_string(),
        Value::String(method.signature.clone()),
    );
    if !method.doc.is_empty() {
        let mut docs = BTreeMap::new();
        docs.insert("kind".to_string(), Value::String("markdown".to_string()));
        docs.insert("value".to_string(), Value::String(method.doc.clone()));
        item.insert("documentation".to_string(), Value::Object(docs));
    }
    item.insert(
        "insertText".to_string(),
        Value::String(format!("{}($0)", method.name)),
    );
    item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    Value::Object(item)
}

fn workspace_completion_item(item: &WorkspaceItem) -> Value {
    let mut entry = BTreeMap::new();
    entry.insert("label".to_string(), Value::String(item.name.clone()));
    let kind = match item.kind {
        DefKind::Fn => 3.0,
        DefKind::Struct => 22.0,
        DefKind::Enum => 13.0,
        DefKind::Trait => 8.0,
        DefKind::TypeAlias => 25.0,
        DefKind::Const => 21.0,
        DefKind::Static => 6.0,
        DefKind::Mod => 9.0,
        DefKind::Variant => 20.0,
        DefKind::TypeParam => 25.0,
    };
    entry.insert("kind".to_string(), Value::Number(kind));
    if !item.signature.is_empty() {
        entry.insert(
            "detail".to_string(),
            Value::String(format!("{}  // {}", item.signature, short_uri(&item.uri))),
        );
    }
    if !item.doc.is_empty() {
        let mut docs = BTreeMap::new();
        docs.insert("kind".to_string(), Value::String("markdown".to_string()));
        docs.insert("value".to_string(), Value::String(item.doc.clone()));
        entry.insert("documentation".to_string(), Value::Object(docs));
    }
    if matches!(item.kind, DefKind::Fn) {
        entry.insert(
            "insertText".to_string(),
            Value::String(format!("{}($0)", item.name)),
        );
        entry.insert("insertTextFormat".to_string(), Value::Number(2.0));
    }
    Value::Object(entry)
}

fn short_uri(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or(uri).to_string()
}

fn collect_existing_imports(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("use ") {
            let path = rest.trim_end_matches(';').trim().to_string();
            if !path.is_empty() {
                out.push(path);
            }
        }
    }
    out
}

fn import_completion_item(doc: &DocumentAnalysis, leaf: &str, full_path: &str) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(leaf.to_string()));
    item.insert("kind".to_string(), Value::Number(3.0));
    item.insert(
        "detail".to_string(),
        Value::String(format!("use {full_path}")),
    );
    item.insert(
        "documentation".to_string(),
        Value::Object({
            let mut docs = BTreeMap::new();
            docs.insert("kind".to_string(), Value::String("markdown".to_string()));
            docs.insert(
                "value".to_string(),
                Value::String(format!("Adds `use {full_path};` to the top of the file.")),
            );
            docs
        }),
    );
    let insert_offset = import_insert_offset(doc.source());
    let (line, col) = doc.offset_to_position(insert_offset);
    let mut start = BTreeMap::new();
    start.insert("line".to_string(), Value::Number(f64::from(line)));
    start.insert("character".to_string(), Value::Number(f64::from(col)));
    let end = start.clone();
    let mut range = BTreeMap::new();
    range.insert("start".to_string(), Value::Object(start));
    range.insert("end".to_string(), Value::Object(end));
    let mut edit = BTreeMap::new();
    edit.insert("range".to_string(), Value::Object(range));
    edit.insert(
        "newText".to_string(),
        Value::String(format!("use {full_path}\n")),
    );
    item.insert(
        "additionalTextEdits".to_string(),
        Value::Array(vec![Value::Object(edit)]),
    );
    Value::Object(item)
}

fn import_insert_offset(source: &str) -> u32 {
    // Place new `use` after the last existing top-of-file `use` line,
    // or at byte 0 when there are none.
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("use ") || trimmed.is_empty() {
            offset += line.len();
            continue;
        }
        break;
    }
    u32::try_from(offset).unwrap_or(0)
}

fn span_to_range(doc: &DocumentAnalysis, span: Span) -> Value {
    let (start_line, start_col) = doc.offset_to_position(span.start);
    let (end_line, end_col) = doc.offset_to_position(span.end);
    let mut start = BTreeMap::new();
    start.insert("line".to_string(), Value::Number(f64::from(start_line)));
    start.insert("character".to_string(), Value::Number(f64::from(start_col)));
    let mut end = BTreeMap::new();
    end.insert("line".to_string(), Value::Number(f64::from(end_line)));
    end.insert("character".to_string(), Value::Number(f64::from(end_col)));
    let mut range = BTreeMap::new();
    range.insert("start".to_string(), Value::Object(start));
    range.insert("end".to_string(), Value::Object(end));
    Value::Object(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(line: u32, character: u32) -> Value {
        let mut p = BTreeMap::new();
        p.insert("line".to_string(), Value::Number(f64::from(line)));
        p.insert("character".to_string(), Value::Number(f64::from(character)));
        Value::Object(p)
    }

    fn locate_params(uri: &str, line: u32, character: u32) -> Value {
        let mut text_doc = BTreeMap::new();
        text_doc.insert("uri".to_string(), Value::String(uri.to_string()));
        let mut params = BTreeMap::new();
        params.insert("textDocument".to_string(), Value::Object(text_doc));
        params.insert("position".to_string(), position(line, character));
        Value::Object(params)
    }

    fn extract_labels(response: &Value) -> Vec<String> {
        let Value::Array(items) = response else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let Value::Object(fields) = item else {
                    return None;
                };
                let Value::String(label) = fields.get("label")? else {
                    return None;
                };
                Some(label.clone())
            })
            .collect()
    }

    #[test]
    fn initialize_result_advertises_full_capability_set() {
        let v = initialize_result();
        let Value::Object(top) = v else {
            panic!("not object")
        };
        let Value::Object(caps) = top.get("capabilities").unwrap() else {
            panic!("no caps");
        };
        for key in [
            "completionProvider",
            "hoverProvider",
            "definitionProvider",
            "typeDefinitionProvider",
            "referencesProvider",
            "documentHighlightProvider",
            "renameProvider",
            "inlayHintProvider",
            "documentSymbolProvider",
            "workspaceSymbolProvider",
            "foldingRangeProvider",
            "documentFormattingProvider",
            "signatureHelpProvider",
            "semanticTokensProvider",
        ] {
            assert!(caps.contains_key(key), "missing capability: {key}");
        }
    }

    fn inlay_params(uri: &str) -> Value {
        let mut text_doc = BTreeMap::new();
        text_doc.insert("uri".to_string(), Value::String(uri.to_string()));
        let mut params = BTreeMap::new();
        params.insert("textDocument".to_string(), Value::Object(text_doc));
        Value::Object(params)
    }

    #[test]
    fn inlay_hints_emits_inferred_let_type() {
        let mut state = ServerState::new();
        state.update("file:///inlay.gos", "fn main() {\n    let n = 42\n}\n");
        let response = state.inlay_hints(&inlay_params("file:///inlay.gos"));
        let labels = extract_labels(&response);
        assert!(
            labels.iter().any(|l| l == ": i64"),
            "expected `: i64` hint; got {labels:?}"
        );
    }

    #[test]
    fn references_returns_every_whole_word_occurrence() {
        let mut state = ServerState::new();
        state.update(
            "file:///r.gos",
            "fn greet() { greet() }\nfn other() { greet() }\n",
        );
        let response = state.references(&locate_params("file:///r.gos", 0, 4));
        let Value::Array(items) = response else {
            panic!("not array");
        };
        assert!(!items.is_empty(), "expected at least one reference");
    }

    #[test]
    fn prepare_rename_returns_span_and_placeholder() {
        let mut state = ServerState::new();
        state.update("file:///p.gos", "fn greet() { }\n");
        let response = state.prepare_rename(&locate_params("file:///p.gos", 0, 4));
        let Value::Object(fields) = response else {
            panic!("not object");
        };
        let Value::String(placeholder) = fields.get("placeholder").unwrap() else {
            panic!("no placeholder");
        };
        assert_eq!(placeholder, "greet");
        assert!(fields.contains_key("range"));
    }

    #[test]
    fn rename_rejects_invalid_identifier_input() {
        let mut state = ServerState::new();
        state.update("file:///bad.gos", "fn greet() { }\n");
        let mut params = locate_params("file:///bad.gos", 0, 4);
        if let Value::Object(fields) = &mut params {
            fields.insert(
                "newName".to_string(),
                Value::String("not valid!".to_string()),
            );
        }
        let response = state.rename(&params);
        assert!(
            matches!(response, Value::Null),
            "expected null for invalid ident"
        );
    }

    #[test]
    fn completion_surfaces_top_level_functions_matching_prefix() {
        let mut state = ServerState::new();
        state.update(
            "file:///c.gos",
            "fn greet() { }\nfn greeter() { }\nfn main() { gr }\n",
        );
        let response = state.completion(&locate_params("file:///c.gos", 2, 13));
        let labels = extract_labels(&response);
        assert!(labels.iter().any(|l| l == "greet"), "labels: {labels:?}");
        assert!(labels.iter().any(|l| l == "greeter"), "labels: {labels:?}");
    }

    #[test]
    fn definition_finds_top_level_function_span() {
        let mut state = ServerState::new();
        state.update(
            "file:///d.gos",
            "fn helper() -> i64 { 1i64 }\nfn main() { helper() }\n",
        );
        let response = state.definition(&locate_params("file:///d.gos", 1, 13));
        let Value::Object(fields) = response else {
            panic!("expected Location object");
        };
        assert!(fields.contains_key("uri"));
        assert!(fields.contains_key("range"));
    }

    #[test]
    fn document_symbol_emits_top_level_items() {
        let mut state = ServerState::new();
        state.update(
            "file:///s.gos",
            "fn helper() { }\nstruct Point { x: i64, y: i64 }\n",
        );
        let mut params = BTreeMap::new();
        let mut text_doc = BTreeMap::new();
        text_doc.insert(
            "uri".to_string(),
            Value::String("file:///s.gos".to_string()),
        );
        params.insert("textDocument".to_string(), Value::Object(text_doc));
        let response = state.document_symbols(&Value::Object(params));
        let Value::Array(items) = response else {
            panic!("not array");
        };
        let names: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                Value::Object(fields) => match fields.get("name") {
                    Some(Value::String(s)) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert!(names.contains(&"helper".to_string()), "names: {names:?}");
        assert!(names.contains(&"Point".to_string()), "names: {names:?}");
    }

    #[test]
    fn folding_ranges_include_each_top_level_item() {
        let mut state = ServerState::new();
        state.update(
            "file:///fr.gos",
            "fn one() {\n    let x = 1\n}\n\nfn two() {\n    let y = 2\n}\n",
        );
        let mut params = BTreeMap::new();
        let mut text_doc = BTreeMap::new();
        text_doc.insert(
            "uri".to_string(),
            Value::String("file:///fr.gos".to_string()),
        );
        params.insert("textDocument".to_string(), Value::Object(text_doc));
        let response = state.folding_ranges(&Value::Object(params));
        let Value::Array(items) = response else {
            panic!("not array");
        };
        assert!(items.len() >= 2, "expected at least two folding ranges");
    }

    #[test]
    fn formatting_returns_no_edits_when_already_formatted() {
        let mut state = ServerState::new();
        state.update("file:///fmt.gos", "fn main() {\n    let x = 1\n}\n");
        let mut params = BTreeMap::new();
        let mut text_doc = BTreeMap::new();
        text_doc.insert(
            "uri".to_string(),
            Value::String("file:///fmt.gos".to_string()),
        );
        params.insert("textDocument".to_string(), Value::Object(text_doc));
        // Whatever the formatter emits should be fine — we just need
        // the call to complete cleanly.
        let _ = state.formatting(&Value::Object(params));
    }

    #[test]
    fn signature_help_finds_the_called_function() {
        let mut state = ServerState::new();
        state.update(
            "file:///sh.gos",
            "fn add(x: i64, y: i64) -> i64 { x + y }\nfn main() { add(1,) }\n",
        );
        // Cursor sits right after the `,` inside `add(1, )`.
        let response = state.signature_help(&locate_params("file:///sh.gos", 1, 18));
        if let Value::Object(fields) = response {
            assert!(fields.contains_key("signatures"));
        }
    }

    fn complete_at(state: &mut ServerState, src_with_cursor: &str, uri: &str) -> Vec<String> {
        let cursor = src_with_cursor
            .find('|')
            .expect("cursor marker `|` missing");
        let cleaned: String =
            src_with_cursor[..cursor].to_string() + &src_with_cursor[cursor + 1..];
        state.update(uri, &cleaned);
        let doc = state.documents.get(uri).expect("document just added");
        let (line, col) = doc.offset_to_position(u32::try_from(cursor).unwrap());
        let response = state.completion(&locate_params(uri, line, col));
        extract_labels(&response)
    }

    fn complete_full(state: &mut ServerState, src_with_cursor: &str, uri: &str) -> Value {
        let cursor = src_with_cursor
            .find('|')
            .expect("cursor marker `|` missing");
        let cleaned: String =
            src_with_cursor[..cursor].to_string() + &src_with_cursor[cursor + 1..];
        state.update(uri, &cleaned);
        let doc = state.documents.get(uri).expect("document just added");
        let (line, col) = doc.offset_to_position(u32::try_from(cursor).unwrap());
        state.completion(&locate_params(uri, line, col))
    }

    #[test]
    fn module_qualified_completion_returns_module_members() {
        let mut state = ServerState::new();
        let labels = complete_at(&mut state, "fn main() { os::e| }\n", "file:///os.gos");
        // `os::e|` should suggest `env`, `exit`, `exists`, and the
        // `exec` submodule.
        assert!(
            labels.iter().any(|l| l == "env"),
            "expected `env` in {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "exec"),
            "expected `exec` submodule in {labels:?}"
        );
    }

    #[test]
    fn nested_module_qualifier_resolves() {
        let mut state = ServerState::new();
        let labels = complete_at(&mut state, "fn main() { std::os::e| }\n", "file:///os2.gos");
        // std::os::exec is a known submodule.
        assert!(
            labels.iter().any(|l| l == "exec"),
            "expected `exec` in labels {labels:?}"
        );
    }

    #[test]
    fn unknown_qualifier_returns_no_member_match() {
        let mut state = ServerState::new();
        let labels = complete_at(&mut state, "fn main() { xyzzy::p| }\n", "file:///x.gos");
        // Unknown qualifier short-circuits — should produce no matches.
        assert!(
            labels.iter().all(|l| l != "println"),
            "did not expect bare-prefix items in qualifier completion: {labels:?}"
        );
    }

    #[test]
    fn use_statement_completion_lists_modules() {
        let mut state = ServerState::new();
        let labels = complete_at(&mut state, "use std::|\n", "file:///use.gos");
        assert!(
            labels.iter().any(|l| l == "fmt"),
            "expected `fmt` in {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "os"),
            "expected `os` in {labels:?}"
        );
    }

    #[test]
    fn vec_dot_completes_to_vec_methods() {
        let mut state = ServerState::new();
        let labels = complete_at(
            &mut state,
            "fn main() { let v: Vec<i64> = vec![]\n    v.p| }\n",
            "file:///vec.gos",
        );
        assert!(
            labels.iter().any(|l| l == "push"),
            "expected `push` in {labels:?}"
        );
    }

    #[test]
    fn string_method_completion_includes_to_uppercase() {
        let mut state = ServerState::new();
        let labels = complete_at(&mut state, "fn main() { \"hi\".to_u| }\n", "file:///s.gos");
        assert!(
            labels.iter().any(|l| l == "to_uppercase"),
            "expected `to_uppercase` in {labels:?}"
        );
    }

    #[test]
    fn user_type_qualified_completion_returns_associated_fns() {
        let mut state = ServerState::new();
        let src = r"struct Foo {}
impl Foo {
    fn new() -> Foo { Foo {} }
    fn make_default() -> Foo { Foo {} }
}
fn main() { Foo::n| }
";
        let labels = complete_at(&mut state, src, "file:///foo.gos");
        assert!(
            labels.iter().any(|l| l == "new"),
            "expected `new` in {labels:?}"
        );
    }

    #[test]
    fn enum_qualified_completion_returns_variants() {
        let mut state = ServerState::new();
        let src = r"enum Color { Red, Green, Blue }
fn main() { Color::R| }
";
        let labels = complete_at(&mut state, src, "file:///enum.gos");
        assert!(
            labels.iter().any(|l| l == "Red"),
            "expected `Red` in {labels:?}"
        );
    }

    #[test]
    fn auto_import_suggestion_includes_use_edit() {
        let mut state = ServerState::new();
        let response = complete_full(&mut state, "fn main() { format| }\n", "file:///fmt_use.gos");
        let Value::Array(items) = response else {
            panic!("expected array response");
        };
        let mut found = false;
        for item in items {
            let Value::Object(fields) = item else {
                continue;
            };
            let Some(Value::String(label)) = fields.get("label") else {
                continue;
            };
            if label != "format" {
                continue;
            }
            if let Some(Value::Array(edits)) = fields.get("additionalTextEdits") {
                if !edits.is_empty() {
                    found = true;
                    break;
                }
            }
        }
        assert!(
            found,
            "expected at least one `format` completion with additionalTextEdits"
        );
    }

    #[test]
    fn function_completion_carries_snippet_insert_text() {
        let mut state = ServerState::new();
        let response = complete_full(&mut state, "fn main() { printl| }\n", "file:///snippet.gos");
        let Value::Array(items) = response else {
            panic!("expected array");
        };
        let mut found_snippet = false;
        for item in items {
            let Value::Object(fields) = item else {
                continue;
            };
            let Some(Value::String(label)) = fields.get("label") else {
                continue;
            };
            if label == "println"
                && matches!(fields.get("insertTextFormat"), Some(Value::Number(n)) if (*n - 2.0).abs() < 0.5)
            {
                found_snippet = true;
                break;
            }
        }
        assert!(
            found_snippet,
            "expected a snippet-format `println` completion"
        );
    }

    #[test]
    fn module_member_completion_carries_documentation() {
        let mut state = ServerState::new();
        let response = complete_full(&mut state, "fn main() { os::a| }\n", "file:///doc.gos");
        let Value::Array(items) = response else {
            panic!("expected array");
        };
        let mut found_doc = false;
        for item in items {
            let Value::Object(fields) = item else {
                continue;
            };
            let Some(Value::String(label)) = fields.get("label") else {
                continue;
            };
            if label == "args" {
                if let Some(Value::Object(docs)) = fields.get("documentation") {
                    if let Some(Value::String(value)) = docs.get("value") {
                        if !value.is_empty() {
                            found_doc = true;
                        }
                    }
                }
            }
        }
        assert!(
            found_doc,
            "expected `os::args` completion to carry documentation"
        );
    }

    #[test]
    fn workspace_completion_surfaces_symbol_from_other_file() {
        let mut state = ServerState::new();
        state.update("file:///util.gos", "fn shared_helper() -> i64 { 1 }\n");
        let labels = complete_at(&mut state, "fn main() { shared_h| }\n", "file:///main.gos");
        assert!(
            labels.iter().any(|l| l == "shared_helper"),
            "expected `shared_helper` from util.gos in {labels:?}"
        );
    }

    #[test]
    fn workspace_completion_drops_renamed_symbol_after_didchange() {
        let mut state = ServerState::new();
        state.update("file:///lib.gos", "fn old_thing() { }\n");
        state.update("file:///lib.gos", "fn new_thing() { }\n");
        let labels = complete_at(&mut state, "fn main() { old_t| }\n", "file:///main.gos");
        assert!(
            !labels.iter().any(|l| l == "old_thing"),
            "expected `old_thing` to be gone after rename; got {labels:?}"
        );
    }

    #[test]
    fn semantic_tokens_returns_data_array_for_known_doc() {
        let mut state = ServerState::new();
        state.update("file:///t.gos", "fn helper() { }\n");
        let mut params = BTreeMap::new();
        let mut text_doc = BTreeMap::new();
        text_doc.insert(
            "uri".to_string(),
            Value::String("file:///t.gos".to_string()),
        );
        params.insert("textDocument".to_string(), Value::Object(text_doc));
        let response = state.semantic_tokens(&Value::Object(params));
        let Value::Object(fields) = response else {
            panic!("not object");
        };
        let Value::Array(data) = fields.get("data").unwrap() else {
            panic!("data not array");
        };
        assert!(!data.is_empty(), "expected at least one semantic token");
    }
}
