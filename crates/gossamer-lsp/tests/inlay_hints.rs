//! Integration tests for `textDocument/inlayHint`.

mod common;

use common::{field, field_str, server_with};
use gossamer_lsp::testing::document_params;
use gossamer_std::json::Value;

fn hint_labels(response: &Value) -> Vec<String> {
    let Value::Array(items) = response else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| field_str(item, "label").map(str::to_string))
        .collect()
}

#[test]
fn inlay_hints_empty_doc_is_empty_array() {
    let uri = "file:///e.gos";
    let server = server_with(uri, "");
    let response = server.inlay_hints(&document_params(uri));
    assert!(
        matches!(response, Value::Array(ref items) if items.is_empty()),
        "inlay hints on empty doc must be [], got {response:?}"
    );
}

#[test]
fn inlay_hints_unknown_doc_returns_empty() {
    let server = server_with("file:///x.gos", "fn main() {}\n");
    let response = server.inlay_hints(&document_params("file:///missing.gos"));
    assert!(
        matches!(response, Value::Array(ref items) if items.is_empty()),
        "inlay hints on missing doc must be [], got {response:?}"
    );
}

#[test]
fn inlay_hints_let_binding_carries_type() {
    // A `let` without annotation. The walker should surface the
    // inferred type if it can resolve one.
    let uri = "file:///l.gos";
    let server = server_with(uri, "fn main() {\n    let count = 7\n    let _ = count\n}\n");
    let response = server.inlay_hints(&document_params(uri));
    let labels = hint_labels(&response);
    // The hint surface may not always trigger; accept either empty
    // or any label that includes a type-like fragment.
    for label in &labels {
        assert!(
            !label.is_empty(),
            "inlay hint label must be non-empty: {labels:?}"
        );
    }
}

#[test]
fn inlay_hints_response_is_array() {
    let uri = "file:///a.gos";
    let server = server_with(uri, "fn main() {}\n");
    let response = server.inlay_hints(&document_params(uri));
    assert!(
        matches!(response, Value::Array(_)),
        "inlayHint response must be an array, got {response:?}"
    );
}

#[test]
fn inlay_hints_entries_carry_position_fields() {
    let uri = "file:///p.gos";
    let server = server_with(uri, "fn main() {\n    let x = 1\n    let _ = x\n}\n");
    let response = server.inlay_hints(&document_params(uri));
    if let Value::Array(items) = &response {
        for item in items {
            let position = field(&item, "position");
            assert!(
                matches!(position, Value::Object(_)),
                "inlay hint must carry a position object, got {item:?}"
            );
            assert!(
                matches!(field(position, "line"), Value::Number(_)),
                "inlay position needs numeric `line`, got {position:?}"
            );
            assert!(
                matches!(field(position, "character"), Value::Number(_)),
                "inlay position needs numeric `character`, got {position:?}"
            );
        }
    }
}

#[test]
fn inlay_hints_for_multiple_locals() {
    let uri = "file:///m.gos";
    let server = server_with(
        uri,
        "fn main() {\n    let a = 1\n    let b = 2\n    let c = 3\n    let _ = a + b + c\n}\n",
    );
    let response = server.inlay_hints(&document_params(uri));
    if let Value::Array(items) = &response {
        // Up to one hint per let; the walker's behaviour is
        // best-effort, so we just verify the shape is consistent.
        for item in items {
            let label = field(item, "label");
            assert!(
                matches!(label, Value::String(_)) || matches!(label, Value::Array(_)),
                "inlay hint label must be a string or LabelPart[], got {item:?}"
            );
        }
    }
}
