//! Semantic-navigation tools backed by the LSP analysis engine.

use std::collections::HashSet;
use std::path::Path;

use gossamer_lsp::handle::{ServerHandle, position_params, workspace_symbol_params};
use gossamer_std::json::{self, Value};

use crate::protocol::{field, field_str};
use crate::tools::text_result;

/// Lazily populated analysis session shared across nav tool calls.
pub(crate) struct NavSession {
    handle: ServerHandle,
    scanned_roots: HashSet<String>,
}

impl NavSession {
    /// Constructs an empty session.
    pub(crate) fn new() -> Self {
        Self {
            handle: ServerHandle::new(),
            scanned_roots: HashSet::new(),
        }
    }

    /// Reads `file`, (re)loads it into the analysis state, and returns
    /// its uri. Re-reading per call keeps the session in sync with the
    /// on-disk state the exec tools mutate.
    fn load(&mut self, file: &str) -> Result<String, String> {
        let abs = std::fs::canonicalize(file).map_err(|e| format!("{file}: {e}"))?;
        let text = std::fs::read_to_string(&abs).map_err(|e| format!("{file}: {e}"))?;
        let uri = path_to_uri(&abs);
        self.handle.update(&uri, &text);
        Ok(uri)
    }

    /// Runs `hover` / `definition` / `references` at a 1-based position.
    pub(crate) fn position_tool(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        let file = field_str(args, "file").ok_or("`file` is required")?;
        let line = required_u32(args, "line")?;
        let column = required_u32(args, "column")?;
        let uri = self.load(file)?;
        let params = position_params(&uri, line.saturating_sub(1), column.saturating_sub(1));
        let text = match tool {
            "hover" => hover_text(&self.handle.hover(&params)),
            "definition" => locations_text(&self.handle.definition(&params)),
            "references" => locations_text(&self.handle.references(&params)),
            // The tools table and this match are maintained together.
            _ => unreachable!("position_tool called with {tool}"),
        };
        Ok(text_result(&text, false))
    }

    /// Runs a workspace-wide symbol search under `root`.
    pub(crate) fn workspace_symbols(&mut self, args: &Value) -> Result<Value, String> {
        let query = field_str(args, "query").ok_or("`query` is required")?;
        let root = field_str(args, "root").unwrap_or(".");
        let abs = std::fs::canonicalize(root).map_err(|e| format!("{root}: {e}"))?;
        let key = abs.to_string_lossy().into_owned();
        if self.scanned_roots.insert(key.clone()) {
            self.handle.scan_workspace(&key);
        }
        let result = self
            .handle
            .workspace_symbols(&workspace_symbol_params(query));
        Ok(text_result(&symbols_text(&result), false))
    }
}

fn required_u32(args: &Value, key: &str) -> Result<u32, String> {
    json::as_i64(field(args, key))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("`{key}` must be a positive integer"))
}

fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

fn hover_text(result: &Value) -> String {
    let contents = field(result, "contents");
    if let Some(text) = json::as_str(field(contents, "value")).or_else(|| json::as_str(contents)) {
        return text.to_string();
    }
    "no hover information at this position".to_string()
}

/// Renders `null | Location | [Location]` as `path:line:col` lines
/// (1-based).
fn locations_text(result: &Value) -> String {
    let locations: Vec<&Value> = match result {
        Value::Array(items) => items.iter().collect(),
        Value::Null => Vec::new(),
        single => vec![single],
    };
    if locations.is_empty() {
        return "no results at this position".to_string();
    }
    locations
        .iter()
        .map(|l| location_line(l))
        .collect::<Vec<_>>()
        .join("\n")
}

fn location_line(location: &Value) -> String {
    let uri = field_str(location, "uri").unwrap_or("<unknown>");
    let start = field(field(location, "range"), "start");
    let line = json::as_i64(field(start, "line")).unwrap_or(0) + 1;
    let column = json::as_i64(field(start, "character")).unwrap_or(0) + 1;
    format!("{}:{line}:{column}", uri_to_path(uri))
}

fn symbols_text(result: &Value) -> String {
    let Some(items) = json::as_array(result) else {
        return "no matching symbols".to_string();
    };
    if items.is_empty() {
        return "no matching symbols".to_string();
    }
    items
        .iter()
        .map(|item| {
            let name = field_str(item, "name").unwrap_or("<unnamed>");
            format!("{name}  {}", location_line(field(item, "location")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
