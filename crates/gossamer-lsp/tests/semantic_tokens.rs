//! Integration tests for `textDocument/semanticTokens/full`.
//!
//! The wire encoding is a flat array of `u32` quintuples:
//!   (`delta_line`, `delta_start`, `length`, `token_type`, `modifiers`).
//! Tests decode the array and assert known token kinds appear at the
//! expected source positions.

mod common;

use common::{field, server_with};
use gossamer_lsp::handle::document_params;
use gossamer_std::json::Value;

fn data_array(response: &Value) -> Vec<u32> {
    let Value::Array(items) = field(response, "data") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| match v {
            Value::Number(n) => Some(*n as u32),
            _ => None,
        })
        .collect()
}

/// Decoded view of one semantic-token quintuple.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct Token {
    line: u32,
    start: u32,
    length: u32,
    type_index: u32,
    modifiers: u32,
}

fn decode(data: &[u32]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut line = 0u32;
    let mut start = 0u32;
    for chunk in data.chunks(5) {
        if chunk.len() < 5 {
            break;
        }
        let dline = chunk[0];
        let dstart = chunk[1];
        if dline == 0 {
            start += dstart;
        } else {
            line += dline;
            start = dstart;
        }
        out.push(Token {
            line,
            start,
            length: chunk[2],
            type_index: chunk[3],
            modifiers: chunk[4],
        });
    }
    out
}

#[test]
fn semantic_tokens_empty_doc_has_empty_data() {
    let uri = "file:///e.gos";
    let server = server_with(uri, "");
    let response = server.semantic_tokens(&document_params(uri));
    let data = data_array(&response);
    assert!(
        data.is_empty(),
        "semantic tokens for empty file must be [], got {data:?}"
    );
}

#[test]
fn semantic_tokens_unknown_doc_returns_empty() {
    let server = server_with("file:///x.gos", "fn main() {}\n");
    let response = server.semantic_tokens(&document_params("file:///missing.gos"));
    let data = data_array(&response);
    assert!(
        data.is_empty(),
        "semantic tokens for unknown doc must be empty, got {data:?}"
    );
}

#[test]
fn semantic_tokens_has_function_decl() {
    let uri = "file:///f.gos";
    let server = server_with(uri, "fn greet() {}\n");
    let response = server.semantic_tokens(&document_params(uri));
    let data = data_array(&response);
    // Quintuples come in groups of 5; need at least one token.
    assert!(
        data.len() >= 5,
        "should emit at least one token for a fn decl, got {data:?}"
    );
    assert_eq!(
        data.len() % 5,
        0,
        "semanticTokens data array must be a multiple of 5, got {}",
        data.len()
    );
}

#[test]
fn semantic_tokens_function_index_for_fn_name() {
    let uri = "file:///fn.gos";
    let server = server_with(uri, "fn alpha() {}\n");
    let response = server.semantic_tokens(&document_params(uri));
    let tokens = decode(&data_array(&response));
    // Token-type indices follow the TOKEN_TYPES list in
    // src/semantic_tokens.rs: function = 6.
    let has_function_token = tokens.iter().any(|t| t.type_index == 6);
    assert!(
        has_function_token,
        "expected a `function` token (type_index 6) for `alpha`, got {tokens:?}"
    );
}

#[test]
fn semantic_tokens_struct_index_for_struct_name() {
    let uri = "file:///s.gos";
    let server = server_with(uri, "struct Widget {}\n");
    let response = server.semantic_tokens(&document_params(uri));
    let tokens = decode(&data_array(&response));
    // struct = 2 in TOKEN_TYPES.
    let has_struct_token = tokens.iter().any(|t| t.type_index == 2);
    assert!(
        has_struct_token,
        "expected a `struct` token (type_index 2) for `Widget`, got {tokens:?}"
    );
}

#[test]
fn semantic_tokens_enum_index_for_enum_name() {
    let uri = "file:///e.gos";
    let server = server_with(uri, "enum Color { Red }\n");
    let response = server.semantic_tokens(&document_params(uri));
    let tokens = decode(&data_array(&response));
    // enum = 3 in TOKEN_TYPES.
    let has_enum_token = tokens.iter().any(|t| t.type_index == 3);
    assert!(
        has_enum_token,
        "expected an `enum` token (type_index 3) for `Color`, got {tokens:?}"
    );
}

#[test]
fn semantic_tokens_emit_in_source_order() {
    let uri = "file:///o.gos";
    let server = server_with(uri, "fn a() {}\nfn b() {}\nfn c() {}\n");
    let response = server.semantic_tokens(&document_params(uri));
    let tokens = decode(&data_array(&response));
    // Each successive `fn` decl name must come after the previous.
    let fn_tokens: Vec<Token> = tokens.into_iter().filter(|t| t.type_index == 6).collect();
    for pair in fn_tokens.windows(2) {
        let prev = pair[0];
        let next = pair[1];
        assert!(
            (next.line, next.start) > (prev.line, prev.start),
            "function tokens out of order: {prev:?} then {next:?}"
        );
    }
}

#[test]
fn semantic_tokens_response_is_object() {
    let uri = "file:///r.gos";
    let server = server_with(uri, "fn main() {}\n");
    let response = server.semantic_tokens(&document_params(uri));
    assert!(
        matches!(response, Value::Object(_)),
        "semanticTokens/full must return an object with `data`, got {response:?}"
    );
}
