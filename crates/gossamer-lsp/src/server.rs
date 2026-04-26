//! LSP request-dispatch loop.
//! Reads JSON-RPC messages from the client, routes them by method,
//! and writes replies back. Only the handful of methods needed for
//! editor diagnostics + navigation are implemented:
//! - `initialize` / `initialized`
//! - `textDocument/didOpen` / `didChange` / `didClose`
//! - `textDocument/hover`
//! - `textDocument/definition`
//! - `textDocument/completion`
//! - `shutdown` / `exit`

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, BufWriter, Read, Write};

use gossamer_diagnostics::{Diagnostic as GossamerDiagnostic, Severity};
use gossamer_lex::Span;
use gossamer_std::json::Value;

use crate::inlay::{InlayHint, collect_inlays};
use crate::protocol::{Transport, field, field_str, field_u32, notification, response_ok};
use crate::session::{DocumentAnalysis, analyse};

/// Runs the server over the supplied reader/writer streams. Returns
/// `Ok(())` when the client sends `exit` after `shutdown`.
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
            "textDocument/completion" => {
                let result = state.completion(&params);
                transport.write_message(&response_ok(id, result))?;
            }
            "textDocument/references" => {
                let result = state.references(&params);
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
            "shutdown" => {
                transport.write_message(&response_ok(id, Value::Null))?;
            }
            "exit" => return Ok(()),
            _ => {
                // Unknown method: respond with null for requests (id
                // present), ignore notifications (no id). This keeps
                // pickier clients from flagging the server as broken.
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
    caps.insert("textDocumentSync".to_string(), Value::Number(1.0));
    caps.insert("hoverProvider".to_string(), Value::Bool(true));
    caps.insert("definitionProvider".to_string(), Value::Bool(true));
    caps.insert("referencesProvider".to_string(), Value::Bool(true));
    caps.insert("inlayHintProvider".to_string(), Value::Bool(true));
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
    caps.insert("completionProvider".to_string(), Value::Object(completion));
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
}

impl ServerState {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
        }
    }

    fn update(&mut self, uri: &str, text: &str) {
        let analysis = analyse(uri, text);
        self.documents.insert(uri.to_string(), analysis);
    }

    fn close(&mut self, uri: &str) {
        self.documents.remove(uri);
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

    fn definition(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let Some(word) = doc.word_at(offset) else {
            return Value::Null;
        };
        let Some(span) = doc.top_level_span(word) else {
            return Value::Null;
        };
        let mut location = BTreeMap::new();
        location.insert("uri".to_string(), Value::String(doc.uri.clone()));
        location.insert("range".to_string(), span_to_range(doc, span));
        Value::Object(location)
    }

    fn completion(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Array(Vec::new());
        };
        let prefix = doc.word_at(offset).unwrap_or("");
        let mut items: Vec<Value> = Vec::new();
        for (ident, _) in &doc.top_level {
            if ident.name.starts_with(prefix) {
                items.push(completion_item(&ident.name, 3));
            }
        }
        for name in KEYWORDS {
            if name.starts_with(prefix) {
                items.push(completion_item(name, 14));
            }
        }
        for name in BUILTIN_COMPLETIONS {
            if name.starts_with(prefix) {
                items.push(completion_item(name, 3));
            }
        }
        Value::Array(items)
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

    fn references(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Array(Vec::new());
        };
        let Some(word) = doc.word_at(offset) else {
            return Value::Array(Vec::new());
        };
        let spans = doc.find_references(word);
        let locations: Vec<Value> = spans
            .into_iter()
            .map(|span| {
                let mut location = BTreeMap::new();
                location.insert("uri".to_string(), Value::String(doc.uri.clone()));
                location.insert("range".to_string(), span_to_range(doc, span));
                Value::Object(location)
            })
            .collect();
        Value::Array(locations)
    }

    fn prepare_rename(&self, params: &Value) -> Value {
        let Some((doc, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let Some(word) = doc.word_at(offset) else {
            return Value::Null;
        };
        let bytes = doc.source.as_bytes();
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
        let Some(word) = doc.word_at(offset) else {
            return Value::Null;
        };
        let Some(new_name) = field_str(params, "newName") else {
            return Value::Null;
        };
        if new_name.is_empty() || !is_valid_identifier(new_name) {
            return Value::Null;
        }
        let edits: Vec<Value> = doc
            .find_references(word)
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
        // Honour the client-supplied range when present; fall back
        // to the whole document for clients that omit it.
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
    // `kind: 1` = `Type` per the LSP spec; renders the hint with
    // the same styling clients use for Rust-Analyzer's type
    // annotations.
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
    fn initialize_result_advertises_completion() {
        let v = initialize_result();
        let Value::Object(top) = v else {
            panic!("not object")
        };
        let Value::Object(caps) = top.get("capabilities").unwrap() else {
            panic!("no caps");
        };
        assert!(caps.contains_key("completionProvider"));
        assert!(caps.contains_key("hoverProvider"));
        assert!(caps.contains_key("definitionProvider"));
        assert!(caps.contains_key("referencesProvider"));
        assert!(caps.contains_key("renameProvider"));
        assert!(caps.contains_key("inlayHintProvider"));
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
        state.update(
            "file:///inlay.gos",
            // `n` has no annotation; the checker resolves the
            // unsuffixed literal default to `i64`.
            "fn main() {\n    let n = 42\n}\n",
        );
        let response = state.inlay_hints(&inlay_params("file:///inlay.gos"));
        let labels = extract_labels(&response);
        assert!(
            labels.iter().any(|l| l == ": i64"),
            "expected `: i64` hint; got {labels:?}"
        );
    }

    #[test]
    fn inlay_hints_skips_explicit_annotations() {
        let mut state = ServerState::new();
        state.update(
            "file:///inlay-skip.gos",
            // Both bindings carry explicit `: T`; nothing to add.
            "fn main() {\n    let a: i64 = 1\n    let b: bool = true\n}\n",
        );
        let response = state.inlay_hints(&inlay_params("file:///inlay-skip.gos"));
        let labels = extract_labels(&response);
        assert!(
            labels.is_empty(),
            "expected no hints when types are explicit; got {labels:?}"
        );
    }

    #[test]
    fn inlay_hints_skips_unit_typed_bindings() {
        let mut state = ServerState::new();
        state.update(
            "file:///inlay-unit.gos",
            // `_` doesn't bind; the let body returns `()`.
            "fn side_effect() { }\nfn main() {\n    let _ = side_effect()\n}\n",
        );
        let response = state.inlay_hints(&inlay_params("file:///inlay-unit.gos"));
        let labels = extract_labels(&response);
        assert!(
            !labels.iter().any(|l| l.contains("()")),
            "should not surface a `: ()` hint; got {labels:?}"
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
        assert_eq!(items.len(), 3, "expected 3 occurrences of `greet`");
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
    fn rename_produces_workspace_edit_with_one_edit_per_occurrence() {
        let mut state = ServerState::new();
        state.update(
            "file:///w.gos",
            "fn greet() { greet() }\nfn other() { greet() }\n",
        );
        let mut params = locate_params("file:///w.gos", 0, 4);
        if let Value::Object(fields) = &mut params {
            fields.insert("newName".to_string(), Value::String("hello".to_string()));
        }
        let response = state.rename(&params);
        let Value::Object(top) = response else {
            panic!("not object");
        };
        let Value::Object(changes) = top.get("changes").unwrap() else {
            panic!("no changes");
        };
        let Value::Array(edits) = changes.get("file:///w.gos").unwrap() else {
            panic!("no edits");
        };
        assert_eq!(edits.len(), 3);
        for edit in edits {
            let Value::Object(fields) = edit else {
                panic!("edit not object")
            };
            let Value::String(new_text) = fields.get("newText").unwrap() else {
                panic!("no newText");
            };
            assert_eq!(new_text, "hello");
        }
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
    fn completion_surfaces_keywords_on_short_prefix() {
        let mut state = ServerState::new();
        state.update("file:///k.gos", "fn main() { l }\n");
        let response = state.completion(&locate_params("file:///k.gos", 0, 13));
        let labels = extract_labels(&response);
        assert!(labels.iter().any(|l| l == "let"), "labels: {labels:?}");
        assert!(labels.iter().any(|l| l == "loop"), "labels: {labels:?}");
    }

    #[test]
    fn completion_surfaces_stdlib_builtins_by_prefix() {
        let mut state = ServerState::new();
        state.update("file:///b.gos", "fn main() { pr }\n");
        let response = state.completion(&locate_params("file:///b.gos", 0, 14));
        let labels = extract_labels(&response);
        assert!(labels.iter().any(|l| l == "println"), "labels: {labels:?}");
        assert!(labels.iter().any(|l| l == "print"), "labels: {labels:?}");
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
    fn hover_includes_identifier_in_markdown_body() {
        let mut state = ServerState::new();
        state.update("file:///h.gos", "fn helper() { }\nfn main() { helper() }\n");
        let response = state.hover(&locate_params("file:///h.gos", 1, 13));
        let Value::Object(fields) = response else {
            panic!("expected Hover");
        };
        let Value::Object(contents) = fields.get("contents").expect("contents") else {
            panic!("contents not object");
        };
        let Value::String(text) = contents.get("value").expect("value") else {
            panic!("value not string");
        };
        assert!(text.contains("helper"), "text: {text}");
    }
}
