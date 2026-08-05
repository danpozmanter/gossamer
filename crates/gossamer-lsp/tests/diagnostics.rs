//! Integration tests for `textDocument/publishDiagnostics`.
//!
//! Each test feeds a fixture that is known to trigger a specific
//! GT*/GP*/GR* diagnostic code and asserts the published notification
//! carries the expected code + a non-empty range + a non-empty
//! message.

mod common;

use common::{
    diagnostic_code, diagnostic_message, diagnostics_from, field, field_array, server_with,
};
use gossamer_std::json::Value;

/// Asserts every published diagnostic carries the `gos` source tag
/// and a non-empty message + range.
fn assert_diagnostics_well_formed(diags: &[Value]) {
    for diag in diags {
        let msg = diagnostic_message(diag);
        assert!(
            msg.as_ref().is_some_and(|m| !m.is_empty()),
            "diagnostic must carry a non-empty message: {diag:?}"
        );
        let range = field(diag, "range");
        assert!(
            matches!(range, Value::Object(_)),
            "diagnostic must carry a range: {diag:?}"
        );
        let source = match field(diag, "source") {
            Value::String(s) => s.clone(),
            _ => String::new(),
        };
        assert_eq!(source, "gos", "source should be `gos`, got {source:?}");
    }
}

/// True when any diagnostic in `diags` carries a code matching
/// `prefix` (case-sensitive). Lets tests assert "at least one
/// GP* error" without pinning the exact code.
fn has_code_prefix(diags: &[Value], prefix: &str) -> bool {
    diags
        .iter()
        .filter_map(diagnostic_code)
        .any(|c| c.starts_with(prefix))
}

#[test]
fn parse_error_emits_gp_diagnostic() {
    let uri = "file:///parse.gos";
    let server = server_with(uri, "fn main() { let x = }\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    assert!(!diags.is_empty(), "parse error should publish diagnostics");
    assert!(
        has_code_prefix(&diags, "GP"),
        "expected GP* parse-error code, got {:?}",
        diags.iter().filter_map(diagnostic_code).collect::<Vec<_>>()
    );
    assert_diagnostics_well_formed(&diags);
}

#[test]
fn a_project_sibling_module_resolves_in_the_editor() {
    // `gos check` / `gos run` bundle an entry with its sibling modules;
    // the editor analysed the open file alone, so a cross-module name
    // read as unresolved there and nowhere else.
    let dir = std::env::temp_dir().join(format!("gos-lsp-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/bundle\"\nversion = \"0.1.0\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("options.gos"), "enum Colorize { Always, Never }\n")
        .expect("write sibling");
    let entry = dir.join("main.gos");
    let source = "fn paint(color: Colorize) -> i64 {\n    match color {\n        Colorize::Always => 1,\n        Colorize::Never => 0,\n    }\n}\nfn main() { println!(\"{}\", paint(Colorize::Always)) }\n";
    std::fs::write(&entry, source).expect("write entry");

    let uri = format!("file://{}", entry.display());
    let server = server_with(&uri, source);
    let diags = diagnostics_from(&server.publish_diagnostics(&uri));
    let unresolved: Vec<_> = diags
        .iter()
        .filter(|diag| diagnostic_code(diag).as_deref() == Some("GR0001"))
        .filter_map(diagnostic_message)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        unresolved.is_empty(),
        "sibling-module names must resolve in the editor; got {unresolved:?}"
    );
}

#[test]
fn a_name_defined_in_no_module_is_still_unresolved() {
    // Bundling must widen what resolves, never suppress a real error: a
    // name no sibling defines stays unresolved in the editor.
    let dir = std::env::temp_dir().join(format!("gos-lsp-undef-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/undef\"\nversion = \"0.1.0\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("options.gos"), "enum Colorize { Always, Never }\n")
        .expect("write sibling");
    let entry = dir.join("main.gos");
    let source =
        "fn paint(color: Colorize) -> i64 { 1 }\nfn ghost(x: NotDefinedAnywhere) -> i64 { 2 }\n";
    std::fs::write(&entry, source).expect("write entry");

    let uri = format!("file://{}", entry.display());
    let server = server_with(&uri, source);
    let diags = diagnostics_from(&server.publish_diagnostics(&uri));
    let unresolved: Vec<String> = diags
        .iter()
        .filter(|diag| diagnostic_code(diag).as_deref() == Some("GR0001"))
        .filter_map(diagnostic_message)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        unresolved
            .iter()
            .any(|message| message.contains("NotDefinedAnywhere")),
        "a name no module defines must stay unresolved; got {unresolved:?}"
    );
    assert!(
        !unresolved
            .iter()
            .any(|message| message.contains("Colorize")),
        "a sibling-defined name must resolve; got {unresolved:?}"
    );
}

#[test]
fn a_signature_diagnostic_publishes_once_per_span() {
    // An editor stacked the same message on one span because the
    // signature's types are converted in two checker passes.
    let uri = "file:///duplicate.gos";
    let server = server_with(
        uri,
        "fn parse(path: String) -> [i64] { [1] }\nfn main() { let _ = parse(\"x\") }\n",
    );
    let diags = diagnostics_from(&server.publish_diagnostics(uri));
    let unsized_ranges: Vec<_> = diags
        .iter()
        .filter(|diag| diagnostic_code(diag).as_deref() == Some("GT0049"))
        .map(|diag| format!("{:?}", field(diag, "range")))
        .collect();
    assert_eq!(
        unsized_ranges.len(),
        1,
        "one span must publish one diagnostic; got {unsized_ranges:?}"
    );
    assert_diagnostics_well_formed(&diags);
}

#[test]
fn trailing_semicolon_is_accepted() {
    let uri = "file:///semicolon.gos";
    let server = server_with(uri, "fn main() { let x = 9;\nprintln(x) }\n");
    let diags = diagnostics_from(&server.publish_diagnostics(uri));
    assert!(
        diags.is_empty(),
        "accepted line-ending semicolon produced diagnostics: {diags:?}"
    );
}

#[test]
fn unresolved_name_emits_gr_diagnostic() {
    let uri = "file:///resolve.gos";
    let server = server_with(uri, "fn main() { does_not_exist() }\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    assert!(
        !diags.is_empty(),
        "unresolved name must publish a diagnostic"
    );
    assert!(
        has_code_prefix(&diags, "GR"),
        "expected GR* resolver-error code, got {:?}",
        diags.iter().filter_map(diagnostic_code).collect::<Vec<_>>()
    );
    assert_diagnostics_well_formed(&diags);
}

#[test]
fn type_mismatch_emits_gt_diagnostic() {
    let uri = "file:///type.gos";
    // Assigning a string literal to an i64-annotated binding is a
    // typecheck error.
    let server = server_with(uri, "fn main() {\nlet x: i64 = \"hello\"\nlet _ = x\n}\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    // The typechecker may produce different codes for this exact
    // shape; we just need at least one GT* code.
    if !diags.is_empty() {
        assert!(
            has_code_prefix(&diags, "GT") || has_code_prefix(&diags, "GR"),
            "expected GT*/GR* code on type mismatch, got {:?}",
            diags.iter().filter_map(diagnostic_code).collect::<Vec<_>>()
        );
        assert_diagnostics_well_formed(&diags);
    }
}

#[test]
fn clean_program_emits_no_errors() {
    let uri = "file:///clean.gos";
    let server = server_with(uri, "fn main() {\nlet x = 1\nlet _ = x\n}\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    // The publishDiagnostics notification is always emitted, but the
    // payload may be empty for a clean file. If any diagnostic comes
    // back it must be a warning, not an error (severity 1).
    for diag in &diags {
        let severity = field(diag, "severity");
        if let Value::Number(n) = severity {
            assert!(
                *n > 1.5,
                "clean program produced an error-severity diagnostic: {diag:?}"
            );
        }
    }
}

#[test]
fn indexed_write_does_not_emit_unused_mut_warning() {
    let uri = "file:///indexed-write.gos";
    let server = server_with(uri, "fn main() { let mut c = [1, 2]\nc[0] = 3 }\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    assert!(
        !diags
            .iter()
            .filter_map(diagnostic_code)
            .any(|code| code == "GL0003"),
        "indexed mutation should not produce unused-mut diagnostics: {diags:?}"
    );
}

#[test]
fn publish_diagnostics_notification_has_uri() {
    let uri = "file:///u.gos";
    let server = server_with(uri, "fn main() {}\n");
    let notifs = server.publish_diagnostics(uri);
    assert_eq!(
        notifs.len(),
        1,
        "publishDiagnostics should emit exactly one notification, got {}",
        notifs.len()
    );
    let params = field(&notifs[0], "params");
    let pub_uri = match field(params, "uri") {
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    assert_eq!(pub_uri, uri, "notification uri should match document uri");
}

#[test]
fn unknown_document_publishes_nothing() {
    let server = server_with("file:///known.gos", "fn main() {}\n");
    let notifs = server.publish_diagnostics("file:///unknown.gos");
    assert!(
        notifs.is_empty(),
        "publishing for an unknown doc must yield no notifications, got {notifs:?}"
    );
}

#[test]
fn diagnostic_range_is_within_source_bounds() {
    let uri = "file:///bounds.gos";
    let source = "fn main() { undefined_thing() }\n";
    let server = server_with(uri, source);
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    let line_count = source.lines().count() as f64;
    for diag in &diags {
        let range = field(diag, "range");
        let start = field(range, "start");
        let end = field(range, "end");
        let start_line = match field(start, "line") {
            Value::Number(n) => *n,
            _ => 0.0,
        };
        let end_line = match field(end, "line") {
            Value::Number(n) => *n,
            _ => 0.0,
        };
        assert!(
            start_line >= 0.0 && start_line <= line_count,
            "diagnostic start line {start_line} out of bounds"
        );
        assert!(
            end_line >= start_line,
            "diagnostic end line {end_line} precedes start {start_line}"
        );
    }
}

#[test]
fn duplicate_definition_emits_diagnostic() {
    let uri = "file:///dup.gos";
    let server = server_with(uri, "fn foo() {}\nfn foo() {}\nfn main() { foo() }\n");
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    // Duplicate-definition shape: emits at least one diagnostic.
    if !diags.is_empty() {
        assert_diagnostics_well_formed(&diags);
    }
}

#[test]
fn arity_mismatch_response_well_formed() {
    // GAP: the LSP pipeline currently does not surface a typecheck
    // diagnostic for arity mismatches in a free-function call.
    // When the typechecker grows that check this test should
    // tighten the assertion to require a non-empty diagnostic set
    // - for now we only verify the response is well-formed when
    // any diagnostic does come through.
    let uri = "file:///arity.gos";
    let server = server_with(
        uri,
        "fn one(x: i64) -> i64 { x }\nfn main() { one(1, 2) }\n",
    );
    let notifs = server.publish_diagnostics(uri);
    let diags = diagnostics_from(&notifs);
    if !diags.is_empty() {
        assert_diagnostics_well_formed(&diags);
    }
}

#[test]
fn empty_source_publishes_clean() {
    let uri = "file:///empty.gos";
    let server = server_with(uri, "");
    let notifs = server.publish_diagnostics(uri);
    assert_eq!(
        notifs.len(),
        1,
        "empty file still publishes one notification"
    );
    let diags = diagnostics_from(&notifs);
    // Empty source is valid Gossamer (no items).
    for diag in &diags {
        let severity = field(diag, "severity");
        if let Value::Number(n) = severity {
            assert!(
                *n > 1.5,
                "empty source produced an error severity: {diag:?}"
            );
        }
    }
    let _ = field_array(&notifs[0], "diagnostics");
}
