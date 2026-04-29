//! Cross-file symbol index used by completion and auto-import.
//!
//! Tracks every top-level declaration that lives in any open document.
//! Built incrementally on `didOpen` / `didChange` (the resolver is fast
//! enough at file granularity that we just rebuild the entry for the
//! one document that changed). The completion path consults this map
//! after the stdlib index, so a `pub fn foo` declared in
//! `src/util.gos` becomes a candidate everywhere it's importable.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_resolve::DefKind;

use crate::session::DocumentAnalysis;

/// Single workspace entry. Stays in sync with the document's
/// definition index — kind, signature, and doc string are mirrored
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

/// Workspace-wide symbol index.
#[derive(Debug, Default)]
pub(crate) struct WorkspaceIndex {
    /// `name → list of declarations`. Multiple entries for the same
    /// name are possible across files; the LSP surfaces each as a
    /// distinct completion candidate.
    by_name: HashMap<String, Vec<WorkspaceItem>>,
    /// `uri → bare names declared by that document`. Used to drop
    /// stale entries on update.
    by_uri: HashMap<String, Vec<String>>,
}

impl WorkspaceIndex {
    /// Replaces the entries previously associated with `uri` with a
    /// fresh harvest of the document's top-level declarations.
    pub(crate) fn update(&mut self, uri: &str, doc: &DocumentAnalysis) {
        self.remove(uri);
        let mut names: Vec<String> = Vec::new();
        for (_, info) in doc.index_pairs() {
            // Skip locally-scoped synthetic kinds we don't want to
            // surface (TypeParam, Variant — the latter belongs to its
            // enum, not the file's surface).
            if matches!(info.kind, DefKind::TypeParam | DefKind::Variant) {
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
        self.by_uri.insert(uri.to_string(), names);
    }

    /// Drops every entry the document at `uri` previously contributed.
    pub(crate) fn remove(&mut self, uri: &str) {
        let Some(names) = self.by_uri.remove(uri) else {
            return;
        };
        for name in names {
            if let Some(entries) = self.by_name.get_mut(&name) {
                entries.retain(|item| item.uri != uri);
                if entries.is_empty() {
                    self.by_name.remove(&name);
                }
            }
        }
    }

    /// Returns every workspace item whose name starts with `prefix`,
    /// excluding entries from `current_uri` (the calling document
    /// already surfaces its own top-levels via the in-file path).
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
}
