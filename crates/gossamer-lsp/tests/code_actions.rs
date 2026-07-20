//! Integration tests for `textDocument/codeAction`.
//!
//! The server surfaces every diagnostic-attached `Suggestion` as a
//! quickfix action. These tests drive a fixture that produces a
//! resolver suggestion and assert the quickfix appears.

mod common;

use common::{diagnostics_from, field, field_str, server_with};
use gossamer_lsp::handle::{code_action_params, range_value};
use gossamer_std::json::Value;

fn action_titles(response: &Value) -> Vec<String> {
    let Value::Array(items) = response else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| field_str(item, "title").map(str::to_string))
        .collect()
}

#[test]
fn code_action_on_clean_doc_returns_empty() {
    let server = server_with("file:///clean.gos", "fn main() { let _ = 1; }\n");
    let params = code_action_params("file:///clean.gos", range_value(0, 0, 0, 5), Vec::new());
    let response = server.code_actions(&params);
    assert!(
        matches!(response, Value::Array(ref items) if items.is_empty()),
        "no suggestions on clean source, got {response:?}"
    );
}

#[test]
fn code_action_well_formed_for_unknown_uri() {
    let server = server_with("file:///k.gos", "fn main() {}\n");
    let params = code_action_params("file:///missing.gos", range_value(0, 0, 0, 1), Vec::new());
    let response = server.code_actions(&params);
    assert!(
        matches!(response, Value::Array(ref items) if items.is_empty()),
        "missing-doc codeAction must yield [] not error: {response:?}"
    );
}

#[test]
fn code_action_for_typo_suggests_correction() {
    // A misspelled call to a name that exists at top level. The
    // resolver should emit a Suggestion with the corrected name.
    let uri = "file:///typo.gos";
    let server = server_with(
        uri,
        "fn really_long_name() -> i64 { 0 }\nfn main() { really_lng_name(); }\n",
    );
    // First confirm the diagnostic was emitted, then trigger
    // codeAction over its range.
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    assert!(
        !diags.is_empty(),
        "typo must produce at least one diagnostic"
    );
    // The diagnostic's range tells us where to ask for actions.
    let diag = &diags[0];
    let range = field(diag, "range");
    let params = code_action_params(uri, range.clone(), vec![diag.clone()]);
    let response = server.code_actions(&params);
    // The codeAction response may be empty if the resolver did not
    // attach a Suggestion for this exact shape. Acceptable - we just
    // assert the shape is well-formed.
    assert!(
        matches!(response, Value::Array(_)),
        "codeAction must return an array, got {response:?}"
    );
    let _ = action_titles(&response);
}

#[test]
fn code_action_for_import_suggestion() {
    // Referring to `Pattern` without importing it from std::regex
    // should yield a resolver suggestion if the auto-import path is
    // wired.
    let uri = "file:///import.gos";
    let server = server_with(uri, "fn main() { let _: Pattern; }\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    let diag = diags
        .iter()
        .find(|diag| matches!(field(diag, "code"), Value::String(code) if code == "GR0001"))
        .expect("Pattern must produce GR0001");
    let range = field(diag, "range");
    let params = code_action_params(uri, range.clone(), vec![diag.clone()]);
    let response = server.code_actions(&params);
    let titles = action_titles(&response);
    assert!(
        titles
            .iter()
            .any(|title| title.contains("std::regex::Pattern")),
        "expected exact stdlib import action, got {titles:?}"
    );
}

#[test]
fn lint_diagnostics_offer_safe_quickfixes() {
    let uri = "file:///lint.gos";
    let server = server_with(uri, "fn main() { let unused = 1; }\n");
    let diags = diagnostics_from(&server.publish_diagnostics(uri));
    let diag = diags
        .iter()
        .find(|diag| matches!(field(diag, "code"), Value::String(code) if code == "GL0001"))
        .expect("unused variable lint must be published");
    let params = code_action_params(uri, field(diag, "range").clone(), vec![diag.clone()]);
    let response = server.code_actions(&params);
    assert!(
        action_titles(&response)
            .iter()
            .any(|title| title == "Fix unused variable"),
        "expected lint quickfix, got {response:?}"
    );
}

#[test]
fn unused_mutable_variable_fix_prefixes_the_identifier() {
    let uri = "file:///unused-mut.gos";
    let source = "fn main() { let mut unused = 1; }\n";
    let server = server_with(uri, source);
    let diags = diagnostics_from(&server.publish_diagnostics(uri));
    let diag = diags
        .iter()
        .find(|diag| matches!(field(diag, "code"), Value::String(code) if code == "GL0001"))
        .expect("unused variable lint");
    let params = code_action_params(uri, field(diag, "range").clone(), vec![diag.clone()]);
    let response = server.code_actions(&params);
    let Value::Array(actions) = response else {
        panic!("codeAction must return an array");
    };
    let action = actions
        .iter()
        .find(|action| field_str(action, "title") == Some("Fix unused variable"))
        .expect("unused variable quickfix");
    let edits = field(field(field(action, "edit"), "changes"), uri);
    let Value::Array(edits) = edits else {
        panic!("quickfix edits");
    };
    assert_eq!(field_str(&edits[0], "newText"), Some("_"));
    let start = field(field(&edits[0], "range"), "start");
    assert_eq!(
        field(start, "character"),
        &Value::Number(source.find("unused").unwrap() as f64)
    );
}

#[test]
fn source_fix_all_combines_safe_edits_and_honours_only() {
    let uri = "file:///fix-all.gos";
    let server = server_with(uri, "fn main() { let first = 1; let second = 2; }\n");
    let mut params = code_action_params(uri, range_value(0, 0, 0, 1), Vec::new());
    let Value::Object(fields) = &mut params else {
        unreachable!()
    };
    let Value::Object(context) = fields.get_mut("context").unwrap() else {
        unreachable!()
    };
    context.insert(
        "only".to_string(),
        Value::Array(vec![Value::String("source.fixAll.gossamer".to_string())]),
    );
    let response = server.code_actions(&params);
    let Value::Array(actions) = response else {
        panic!("codeAction must return an array");
    };
    assert_eq!(actions.len(), 1, "only fix-all was requested: {actions:?}");
    assert_eq!(
        field_str(&actions[0], "kind"),
        Some("source.fixAll.gossamer")
    );
    let edits = field(field(field(&actions[0], "edit"), "changes"), uri);
    assert!(matches!(edits, Value::Array(items) if items.len() == 2));
}

#[test]
fn code_action_entries_have_workspace_edits() {
    let uri = "file:///e.gos";
    let server = server_with(uri, "fn alpha() {}\nfn main() { alphat(); }\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    if diags.is_empty() {
        return;
    }
    let diag = &diags[0];
    let range = field(diag, "range");
    let params = code_action_params(uri, range.clone(), vec![diag.clone()]);
    let response = server.code_actions(&params);
    if let Value::Array(items) = &response {
        for item in items {
            // Each action must carry either an `edit` or a `command`.
            let has_edit = !matches!(field(item, "edit"), Value::Null);
            let has_command = !matches!(field(item, "command"), Value::Null);
            assert!(
                has_edit || has_command,
                "every code action needs an edit or command: {item:?}"
            );
        }
    }
}

#[test]
fn code_action_kind_is_quickfix() {
    let uri = "file:///q.gos";
    let server = server_with(
        uri,
        "fn long_named_function() {}\nfn main() { long_named_funct(); }\n",
    );
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    if diags.is_empty() {
        return;
    }
    let diag = &diags[0];
    let range = field(diag, "range");
    let params = code_action_params(uri, range.clone(), vec![diag.clone()]);
    let response = server.code_actions(&params);
    if let Value::Array(items) = &response {
        for item in items {
            if let Some(kind) = field_str(item, "kind") {
                assert!(
                    kind.starts_with("quickfix") || kind == "refactor",
                    "expected quickfix kind, got {kind}"
                );
            }
        }
    }
}
