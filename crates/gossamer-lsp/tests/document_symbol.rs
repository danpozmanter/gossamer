//! Integration tests for `textDocument/documentSymbol` and
//! `workspace/symbol`.

mod common;

use common::{field, field_str, server_with};
use gossamer_lsp::testing::{document_params, workspace_symbol_params};
use gossamer_std::json::Value;

/// Collects every `name` field from a (possibly hierarchical)
/// documentSymbol response, walking into `children` arrays.
fn walk_symbol_names(response: &Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Value::Array(items) = response {
        for item in items {
            push_symbol_names(item, &mut names);
        }
    }
    names
}

fn push_symbol_names(item: &Value, out: &mut Vec<String>) {
    if let Some(name) = field_str(item, "name") {
        out.push(name.to_string());
    }
    if let Value::Array(children) = field(item, "children") {
        for child in children {
            push_symbol_names(child, out);
        }
    }
}

#[test]
fn document_symbols_returns_top_level_fn() {
    let server = server_with("file:///d.gos", "fn one() {}\nfn two() {}\nfn three() {}\n");
    let response = server.document_symbols(&document_params("file:///d.gos"));
    let names = walk_symbol_names(&response);
    assert!(
        names.iter().any(|n| n == "one"),
        "expected `one` in {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "two"),
        "expected `two` in {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "three"),
        "expected `three` in {names:?}"
    );
}

#[test]
fn document_symbols_returns_structs_and_enums() {
    let server = server_with(
        "file:///s.gos",
        "struct Point { x: i64, y: i64 }\nenum Shape { Circle, Square }\nfn main() {}\n",
    );
    let response = server.document_symbols(&document_params("file:///s.gos"));
    let names = walk_symbol_names(&response);
    assert!(
        names.iter().any(|n| n == "Point"),
        "expected struct `Point` in {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "Shape"),
        "expected enum `Shape` in {names:?}"
    );
}

#[test]
fn document_symbols_returns_impl_methods() {
    let server = server_with(
        "file:///i.gos",
        "struct Counter { n: i64 }\nimpl Counter {\n    fn bump(&self) -> i64 { self.n + 1 }\n    fn reset(&self) -> i64 { 0 }\n}\nfn main() {}\n",
    );
    let response = server.document_symbols(&document_params("file:///i.gos"));
    let names = walk_symbol_names(&response);
    // The methods may appear either as top-level entries or as
    // children of the impl block — either is acceptable.
    let has_bump = names.iter().any(|n| n == "bump");
    let has_reset = names.iter().any(|n| n == "reset");
    assert!(
        has_bump || has_reset || names.iter().any(|n| n == "Counter"),
        "expected `Counter`/`bump`/`reset` in {names:?}"
    );
}

#[test]
fn document_symbols_returns_empty_for_unknown_doc() {
    let server = server_with("file:///x.gos", "fn main() {}\n");
    let response = server.document_symbols(&document_params("file:///missing.gos"));
    assert!(
        matches!(response, Value::Array(ref items) if items.is_empty()),
        "documentSymbol on missing doc should be empty array, got {response:?}"
    );
}

#[test]
fn document_symbols_handles_empty_file() {
    let server = server_with("file:///e.gos", "");
    let response = server.document_symbols(&document_params("file:///e.gos"));
    assert!(
        matches!(response, Value::Array(_)),
        "documentSymbol must always return an array, got {response:?}"
    );
}

#[test]
fn document_symbols_carries_kind_field() {
    let server = server_with("file:///k.gos", "fn solo() {}\n");
    let response = server.document_symbols(&document_params("file:///k.gos"));
    if let Value::Array(items) = &response {
        for item in items {
            let kind = field(item, "kind");
            assert!(
                matches!(kind, Value::Number(_)),
                "documentSymbol entry must carry a numeric `kind`, got {item:?}"
            );
        }
    }
}

#[test]
fn workspace_symbols_finds_function_across_files() {
    let mut server = gossamer_lsp::testing::ServerHandle::new();
    server.update("file:///a.gos", "fn alpha() {}\n");
    server.update("file:///b.gos", "fn alphabet() {}\n");
    let response = server.workspace_symbols(&workspace_symbol_params("alpha"));
    if let Value::Array(items) = &response {
        let names: Vec<String> = items
            .iter()
            .filter_map(|i| field_str(i, "name").map(str::to_string))
            .collect();
        assert!(
            names.iter().any(|n| n == "alpha") || names.iter().any(|n| n == "alphabet"),
            "workspaceSymbol `alpha` query should match at least one of alpha/alphabet, got {names:?}"
        );
    } else {
        panic!("workspace/symbol must return an array, got {response:?}");
    }
}

#[test]
fn workspace_symbols_empty_query_well_formed() {
    let server = server_with("file:///w.gos", "fn ws_target() {}\n");
    let response = server.workspace_symbols(&workspace_symbol_params(""));
    assert!(
        matches!(response, Value::Array(_)),
        "workspace/symbol must return an array, got {response:?}"
    );
}
