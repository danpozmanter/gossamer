#![allow(missing_docs)]

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;

fn example(name: &str) -> String {
    format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn hello_world_parses_cleanly() {
    let path = example("hello_world.gos");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut map = SourceMap::new();
    let file = map.add_file(&path, source.clone());
    let (sf, diags) = parse_source_file(&source, file);
    eprintln!("hello_world: {} uses, {} items, {} diags", sf.uses.len(), sf.items.len(), diags.len());
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(diags.is_empty(), "diagnostics should be empty");
}

#[test]
fn web_server_parses_cleanly() {
    let path = example("web_server.gos");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut map = SourceMap::new();
    let file = map.add_file(&path, source.clone());
    let (sf, diags) = parse_source_file(&source, file);
    eprintln!("web_server: {} uses, {} items, {} diags", sf.uses.len(), sf.items.len(), diags.len());
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(diags.is_empty(), "diagnostics should be empty");
}

#[test]
fn line_count_parses_cleanly() {
    let path = example("line_count.gos");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut map = SourceMap::new();
    let file = map.add_file(&path, source.clone());
    let (sf, diags) = parse_source_file(&source, file);
    eprintln!("line_count: {} uses, {} items, {} diags", sf.uses.len(), sf.items.len(), diags.len());
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(diags.is_empty(), "diagnostics should be empty");
}
