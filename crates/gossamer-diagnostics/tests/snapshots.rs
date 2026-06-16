//! Diagnostic-rendering snapshot tests.
//!
//! For every GT / GP / GR / GL code in the registry, build a
//! synthetic diagnostic seeded with the registry explanation,
//! render it through `render_plain` and the colour-free
//! `render`, and lock the output in via `insta::assert_snapshot!`.
//! The snapshots themselves live under `tests/snapshots/` and are
//! committed; CI runs `cargo insta test` to verify drift.

use gossamer_diagnostics::{
    Code, Diagnostic, Location, REGISTRY, RenderOptions, render, render_plain,
};
use gossamer_lex::{SourceMap, Span};

/// Codes the snapshot suite covers - parser, resolver, type
/// checker, lints. Runtime (`GX`) and exhaustiveness (`GM`) are
/// out of scope per the deliverable spec.
fn covers(code: &str) -> bool {
    let prefix = &code[..2];
    matches!(prefix, "GT" | "GP" | "GR" | "GL")
}

/// First non-empty line of `text`, trimmed. Used as the synthetic
/// diagnostic title so the snapshot anchors on real registry copy.
fn first_line(text: &str) -> &str {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    text
}

/// Builds a small in-memory source map with one file whose body is
/// long enough that the span we point at always falls inside a
/// real line. The contents are not surfaced in `render_plain`
/// snapshots, so the test stays stable regardless of source.
fn fixture_map() -> (SourceMap, gossamer_lex::FileId) {
    let mut map = SourceMap::new();
    let body = "fn main() {\n    let x = 1\n    let y = x + 2\n}\n";
    let file = map.add_file("snap.gos", body.to_string());
    (map, file)
}

fn synthesise(code: &'static str, explanation: &'static str) -> Diagnostic {
    let (map, file) = fixture_map();
    // Stable span pointing at the `let x = 1` line of the fixture
    // so the rendered carets are deterministic across runs.
    let loc = Location::new(file, Span::new(file, 16, 21));
    let _ = map; // map only constructed to mint the FileId.
    Diagnostic::error(Code(code), first_line(explanation).to_string()).with_primary(loc, "here")
}

fn render_for_snapshot(code: &'static str, explanation: &'static str) -> String {
    let (map, _) = fixture_map();
    let diag = synthesise(code, explanation);
    let plain = render_plain(&diag);
    let framed = render(&diag, &map, RenderOptions::default());
    assert!(
        !plain.trim().is_empty(),
        "render_plain returned empty for {code}"
    );
    assert!(
        !framed.trim().is_empty(),
        "render returned empty for {code}"
    );
    format!("--- plain ---\n{plain}\n--- framed ---\n{framed}")
}

#[test]
fn snapshot_every_covered_code() {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    let _guard = settings.bind_to_scope();

    let mut covered = 0_usize;
    for (code, explanation) in REGISTRY {
        if !covers(code) {
            continue;
        }
        let rendered = render_for_snapshot(code, explanation);
        // One snapshot per code, named after the code so a failure
        // tells you exactly which diagnostic drifted.
        insta::assert_snapshot!(*code, rendered);
        covered += 1;
    }
    assert!(
        covered >= 80,
        "expected to cover 80+ GT/GP/GR/GL codes, got {covered}"
    );
}
