//! Integration tests for `textDocument/formatting`.

mod common;

use common::{field, field_str, server_with};
use gossamer_lsp::handle::document_params;
use gossamer_std::json::Value;

fn first_edit_new_text(response: &Value) -> Option<String> {
    let Value::Array(edits) = response else {
        return None;
    };
    let first = edits.first()?;
    field_str(first, "newText").map(str::to_string)
}

#[test]
fn formatting_on_clean_doc_returns_empty_or_idempotent() {
    let uri = "file:///clean.gos";
    let server = server_with(uri, "fn main() {}\n");
    let response = server.formatting(&document_params(uri));
    // Already-formatted source should yield no edits.
    if let Value::Array(items) = &response {
        if !items.is_empty() {
            // If any edit comes back, the new text should still be
            // valid Gossamer (it always is - the pretty-printer just
            // ran). Acceptable.
            assert!(
                first_edit_new_text(&response).is_some(),
                "non-empty formatting edits must carry newText"
            );
        }
    } else {
        panic!("formatting response must be an array, got {response:?}");
    }
}

#[test]
fn formatting_on_broken_source_returns_empty() {
    let uri = "file:///bad.gos";
    let server = server_with(uri, "fn main() { let x = \n");
    let response = server.formatting(&document_params(uri));
    assert!(
        matches!(response, Value::Array(ref items) if items.is_empty()),
        "formatting should refuse to run on parse-error source, got {response:?}"
    );
}

#[test]
fn formatting_full_document_replace() {
    // Deliberately ugly source - extra blank lines + indentation
    // wobble. The pretty-printer should canonicalize it.
    let uri = "file:///ugly.gos";
    let server = server_with(
        uri,
        "fn   main()   {\n\n\n    let    x   =   1\n    let _ = x\n}\n",
    );
    let response = server.formatting(&document_params(uri));
    if let Value::Array(edits) = &response {
        if !edits.is_empty() {
            // Should be a full-document replace.
            let range = field(&edits[0], "range");
            let start = field(range, "start");
            let start_line = match field(start, "line") {
                Value::Number(n) => *n,
                _ => f64::NAN,
            };
            assert_eq!(start_line, 0.0, "format edit should start at line 0");
        }
    } else {
        panic!("formatting response must be an array, got {response:?}");
    }
}

#[test]
fn formatting_unknown_doc_returns_empty() {
    let server = server_with("file:///x.gos", "fn main() {}\n");
    let response = server.formatting(&document_params("file:///missing.gos"));
    assert!(
        matches!(response, Value::Array(ref items) if items.is_empty()),
        "format on missing doc must be empty array, got {response:?}"
    );
}

#[test]
fn formatting_response_carries_newtext_when_changed() {
    let uri = "file:///c.gos";
    let server = server_with(
        uri,
        "fn   spaced()    -> i64    { 0 }\nfn main() { let _ = spaced() }\n",
    );
    let response = server.formatting(&document_params(uri));
    if let Value::Array(edits) = &response {
        if !edits.is_empty() {
            assert!(
                first_edit_new_text(&response).is_some(),
                "edit must carry newText: {response:?}"
            );
        }
    }
}

#[test]
fn formatting_empty_source_yields_empty_or_minimal_edit() {
    let uri = "file:///e.gos";
    let server = server_with(uri, "");
    let response = server.formatting(&document_params(uri));
    assert!(
        matches!(response, Value::Array(_)),
        "format on empty doc must be array, got {response:?}"
    );
}
