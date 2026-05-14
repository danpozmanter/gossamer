//! Integration tests for the GC write-barrier insertion pass.

#![allow(missing_docs)]

use gossamer_hir::lower_source_file;
use gossamer_lex::SourceMap;
use gossamer_mir::{Body, StatementKind, insert_gc_barriers, lower_program};
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

fn build(source: &str) -> (Vec<Body>, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file("gc.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(diags.is_empty(), "parse: {diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let bodies = lower_program(&hir, &mut tcx);
    (bodies, tcx)
}

fn count_barriers(body: &Body) -> usize {
    body.blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter(|s| matches!(s.kind, StatementKind::GcWriteBarrier { .. }))
        .count()
}

const BAG_SOURCE: &str = "\
struct Bag { items: [String] }
fn build() -> Bag {
    let mut b = Bag { items: [].to_vec() }
    b.items = [\"hi\".to_string()].to_vec()
    b
}
";

const SCALAR_SOURCE: &str = "\
fn add(a: i64, b: i64) -> i64 { a + b }
fn main() { let _ = add(1, 2) }
";

const HOLDER_SOURCE: &str = "\
struct Holder { s: String }
fn replace(h: &mut Holder, new: String) { h.s = new }
";

#[test]
fn pass_is_idempotent_on_already_processed_bodies() {
    let (mut bodies, tcx) = build(BAG_SOURCE);
    for body in &mut bodies {
        insert_gc_barriers(body, &tcx);
    }
    let after_first: Vec<usize> = bodies.iter().map(count_barriers).collect();
    for body in &mut bodies {
        insert_gc_barriers(body, &tcx);
    }
    let after_second: Vec<usize> = bodies.iter().map(count_barriers).collect();
    assert_eq!(
        after_first, after_second,
        "running the pass twice must produce the same barrier count",
    );
}

#[test]
fn pass_runs_clean_on_scalar_only_programs() {
    let (mut bodies, tcx) = build(SCALAR_SOURCE);
    for body in &mut bodies {
        insert_gc_barriers(body, &tcx);
        assert_eq!(
            count_barriers(body),
            0,
            "scalar-only fn `{}` must not emit barriers",
            body.name,
        );
    }
}

#[test]
fn pass_emits_at_least_one_barrier_for_pointer_field_writes() {
    let (mut bodies, tcx) = build(HOLDER_SOURCE);
    let mut total_barriers = 0;
    for body in &mut bodies {
        insert_gc_barriers(body, &tcx);
        total_barriers += count_barriers(body);
    }
    assert!(
        total_barriers >= 1,
        "expected at least one barrier across the corpus, got {total_barriers}",
    );
}

#[test]
fn pass_does_not_crash_on_an_empty_program() {
    let (mut bodies, tcx) = build("fn main() {}\n");
    for body in &mut bodies {
        insert_gc_barriers(body, &tcx);
        assert_eq!(count_barriers(body), 0);
    }
}
