//! Tests that checked-in docs assets keep volatile repo facts live.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn docs_repo_header_facts_are_live_and_loaded() {
    let root = workspace_root();
    let mkdocs = std::fs::read_to_string(root.join("mkdocs.yml")).expect("read mkdocs.yml");
    assert!(
        mkdocs.contains("  - js/repo_button.js"),
        "mkdocs.yml must load repo_button.js"
    );

    let script = std::fs::read_to_string(root.join("docs_src/js/repo_button.js"))
        .expect("read repo_button.js");
    assert!(
        script.contains("https://api.github.com/repos/danpozmanter/gossamer"),
        "repo header facts must come from GitHub at runtime"
    );
    assert!(
        script.contains("cache: \"no-store\""),
        "repo header fetches must not reuse stale browser cache entries"
    );
    assert!(
        script.contains("__source"),
        "Material source-fact session cache must be cleared"
    );
    assert!(
        script.contains("/releases/latest") && script.contains("/tags?per_page=1"),
        "the displayed version must be fetched from releases or tags"
    );
}
