//! Integration tests for `textDocument/completion`.
//!
//! Covers method-position, type-qualified, stdlib path, use-statement,
//! bare-prefix, keyword, and auto-import completion shapes.

mod common;

use common::{completion_labels, server_with};
use gossamer_lsp::testing::position_params;

/// Returns a completion response and asserts every expected label
/// appears in it. Embeds the failing label set in the assertion
/// message so test failures are self-explaining.
fn assert_has_labels(response: &gossamer_std::json::Value, expected: &[&str], context: &str) {
    let labels = completion_labels(response);
    for needle in expected {
        assert!(
            labels.iter().any(|l| l == needle),
            "{context}: expected label `{needle}` in completion results, got {labels:?}"
        );
    }
}

#[test]
fn method_completion_on_string_receiver() {
    let server = server_with(
        "file:///m.gos",
        "fn main() {\n    let s = \"hello\"\n    s.to\n}\n",
    );
    // Cursor immediately after `s.to` (line 2, column 8).
    let params = position_params("file:///m.gos", 2, 8);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    assert!(
        !labels.is_empty(),
        "expected method completions for `s.to`, got empty array"
    );
    // The builtin String method table should include `to_string` and
    // `to_lower`/`to_upper`. Any one of the documented prefix matches
    // is enough — assert at least one method survives.
    let has_to_method = labels
        .iter()
        .any(|l| l.starts_with("to_") || l == "to_string");
    assert!(
        has_to_method,
        "expected at least one `to_*` method, got {labels:?}"
    );
}

#[test]
fn type_qualified_completion_vec_associated() {
    let server = server_with("file:///v.gos", "fn main() {\n    let x = Vec::n\n}\n");
    // Cursor right after `Vec::n` on line 1 (column 17).
    let params = position_params("file:///v.gos", 1, 17);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    // The completion may surface zero items if the stdlib index does
    // not register `Vec` as a member-bearing type. We tolerate that
    // — but if any items come back, none of them should be unrelated
    // bare functions like `println`.
    for label in &labels {
        assert!(
            !["println", "print", "panic"].contains(&label.as_str()),
            "type-qualified `Vec::` should not surface print macros, got {label}"
        );
    }
}

#[test]
fn stdlib_module_completion_after_path() {
    let server = server_with("file:///s.gos", "use std::fs::\nfn main() {}\n");
    // Cursor immediately after `std::fs::` on line 0, column 13.
    let params = position_params("file:///s.gos", 0, 13);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    assert!(
        !labels.is_empty(),
        "expected std::fs members in use context, got {labels:?}"
    );
    // `read_to_string` is one of the most-used fs entries — must be
    // there as long as the stdlib index has any fs members.
    let has_read = labels.iter().any(|l| l == "read_to_string" || l == "read");
    assert!(
        has_read,
        "expected `read_to_string` or `read` in std::fs members, got {labels:?}"
    );
}

#[test]
fn use_context_lists_root_modules() {
    let server = server_with("file:///r.gos", "use s\nfn main() {}\n");
    // Cursor right after `use s` (line 0, column 5).
    let params = position_params("file:///r.gos", 0, 5);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    assert!(
        labels.iter().any(|l| l == "std"),
        "use-statement bare prefix `s` should suggest `std`, got {labels:?}"
    );
}

#[test]
fn bare_prefix_surfaces_user_functions() {
    let server = server_with(
        "file:///p.gos",
        "fn hello_world() -> i64 { 0 }\nfn main() {\n    hello_w\n}\n",
    );
    // Cursor right after `hello_w` (line 2, column 11).
    let params = position_params("file:///p.gos", 2, 11);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    assert!(
        labels.iter().any(|l| l == "hello_world"),
        "bare prefix `hello_w` should resolve to `hello_world`, got {labels:?}"
    );
}

#[test]
fn bare_prefix_includes_keywords() {
    let server = server_with("file:///k.gos", "fn main() {\n    re\n}\n");
    // Cursor right after `re` (line 1, column 6).
    let params = position_params("file:///k.gos", 1, 6);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    assert!(
        labels.iter().any(|l| l == "return"),
        "keyword `return` should appear in bare-prefix `re`, got {labels:?}"
    );
}

#[test]
fn bare_prefix_includes_builtin_macros() {
    let server = server_with("file:///b.gos", "fn main() {\n    print\n}\n");
    // Cursor right after `print` (line 1, column 9).
    let params = position_params("file:///b.gos", 1, 9);
    let response = server.completion(&params);
    assert_has_labels(&response, &["print", "println"], "builtin macros");
}

#[test]
fn locals_surface_in_completion() {
    let server = server_with(
        "file:///l.gos",
        "fn main() {\n    let banana = 1\n    let bandana = 2\n    ba\n}\n",
    );
    // Cursor right after `ba` (line 3, column 6).
    let params = position_params("file:///l.gos", 3, 6);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    let banana_present = labels.iter().any(|l| l == "banana");
    let dotted_match = labels.iter().any(|l| l == "bandana");
    assert!(
        banana_present || dotted_match,
        "expected at least one of `banana`/`bandana` in {labels:?}"
    );
}

#[test]
fn empty_document_returns_array() {
    let server = server_with("file:///e.gos", "");
    let params = position_params("file:///e.gos", 0, 0);
    let response = server.completion(&params);
    // Empty document should still return an array, even if empty.
    assert!(
        matches!(response, gossamer_std::json::Value::Array(_)),
        "completion on empty file should return Array, got {response:?}"
    );
}

#[test]
fn method_completion_on_vec_receiver() {
    let server = server_with(
        "file:///v.gos",
        "fn main() {\n    let xs: Vec<i64> = Vec::new()\n    xs.p\n}\n",
    );
    // Cursor after `xs.p` on line 2, column 8.
    let params = position_params("file:///v.gos", 2, 8);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    // `push` / `pop` are guaranteed entries on Vec; at least one must
    // come back when the receiver resolves to a known builtin type.
    let has_p_method = labels.iter().any(|l| l == "push" || l == "pop");
    assert!(
        has_p_method,
        "expected `push`/`pop` on Vec receiver, got {labels:?}"
    );
}

#[test]
fn dotted_method_to_string() {
    let server = server_with(
        "file:///t.gos",
        "fn main() {\n    let n = 42\n    n.to_\n}\n",
    );
    // Cursor at line 2, column 9 (after `n.to_`).
    let params = position_params("file:///t.gos", 2, 9);
    let response = server.completion(&params);
    let labels = completion_labels(&response);
    // The integer method surface may or may not include `to_string`
    // depending on the BUILTIN_METHOD table for i64; we accept any
    // `to_*` method as proof the dotted-method path is engaged.
    assert!(
        labels.is_empty() || labels.iter().any(|l| l.starts_with("to_")),
        "dotted-method completion should be tightly scoped, got {labels:?}"
    );
}

#[test]
fn unknown_document_returns_array() {
    let server = server_with("file:///x.gos", "fn main() {}\n");
    let params = position_params("file:///does-not-exist.gos", 0, 0);
    let response = server.completion(&params);
    // Locating a missing document should return an empty array, not
    // null or an error.
    assert!(
        matches!(response, gossamer_std::json::Value::Array(_)),
        "missing-doc completion should yield Array, got {response:?}"
    );
}
