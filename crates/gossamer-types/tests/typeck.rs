//! End-to-end type-checker tests driven by parser + resolver output.

use gossamer_ast::{ExprKind, ItemKind, SourceFile, StmtKind};
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, TyKind, TypeError, TypeTable, typecheck_source_file};

struct Checked {
    source: SourceFile,
    table: TypeTable,
    diagnostics: Vec<gossamer_types::TypeDiagnostic>,
    tcx: TyCtxt,
}

fn run(source: &str) -> Checked {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse errors: {parse_diags:?}");
    let (resolutions, resolve_diags) = resolve_source_file(&sf);
    let unresolved: Vec<_> = resolve_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_resolve::ResolveError::UnresolvedName { .. }
            )
        })
        .collect();
    assert!(unresolved.is_empty(), "resolve errors: {unresolved:?}");
    let mut tcx = TyCtxt::new();
    let (table, diagnostics) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    Checked {
        source: sf,
        table,
        diagnostics,
        tcx,
    }
}

#[test]
fn suffixed_integer_literal_receives_declared_type() {
    let checked = run("fn main() { let x = 42i32 }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let body = decl.body.as_ref().unwrap();
    let ExprKind::Block(block) = &body.kind else {
        panic!("expected block");
    };
    let stmt = &block.stmts[0];
    let StmtKind::Let { init, .. } = &stmt.kind else {
        panic!("expected let");
    };
    let init = init.as_ref().unwrap();
    let ty = checked.table.get(init.id).expect("init typed");
    assert!(matches!(
        checked.tcx.kind(ty),
        Some(TyKind::Int(gossamer_types::IntTy::I32))
    ));
}

#[test]
fn string_literal_has_string_type() {
    let checked = run("fn main() { let s = \"hi\" }\n");
    assert!(checked.diagnostics.is_empty());
    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let body = decl.body.as_ref().unwrap();
    let ExprKind::Block(block) = &body.kind else {
        panic!("expected block");
    };
    let stmt = &block.stmts[0];
    let StmtKind::Let { init, .. } = &stmt.kind else {
        panic!("expected let");
    };
    let init = init.as_ref().unwrap();
    let ty = checked.table.get(init.id).unwrap();
    assert!(matches!(checked.tcx.kind(ty), Some(TyKind::String)));
}

#[test]
fn let_annotation_forces_concrete_type() {
    let checked = run("fn main() { let x: i32 = 1i32 }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn obvious_concrete_mismatch_is_reported() {
    let checked = run("fn main() { let x: bool = 42i32 }\n");
    assert!(!checked.diagnostics.is_empty());
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::TypeMismatch { .. })),
        "expected type mismatch diagnostic: {:?}",
        checked.diagnostics
    );
}

#[test]
fn if_branch_mismatch_is_reported() {
    let checked = run("fn main() { let y = if true { 1i32 } else { false } }\n");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::TypeMismatch { .. })),
        "expected branch-mismatch diagnostic: {:?}",
        checked.diagnostics
    );
}

#[test]
fn if_branches_with_matching_types_pass() {
    let checked = run("fn main() { let y = if true { 1i32 } else { 2i32 } }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn comparison_produces_bool() {
    let checked = run("fn main() { let b = 1i32 < 2i32 }\n");
    assert!(checked.diagnostics.is_empty());
    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let body = decl.body.as_ref().unwrap();
    let ExprKind::Block(block) = &body.kind else {
        panic!("expected block");
    };
    let stmt = &block.stmts[0];
    let StmtKind::Let { init, .. } = &stmt.kind else {
        panic!("expected let");
    };
    let init = init.as_ref().unwrap();
    let ty = checked.table.get(init.id).unwrap();
    assert!(matches!(checked.tcx.kind(ty), Some(TyKind::Bool)));
}

#[test]
fn every_expr_node_is_typed() {
    let checked = run("fn add(a: i32, b: i32) -> i32 { a + b }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let body = decl.body.as_ref().unwrap();
    assert!(checked.table.get(body.id).is_some());
}

#[test]
fn example_programs_typecheck_without_false_positives() {
    for name in ["hello_world.gos", "line_count.gos", "web_server.gos"] {
        let path = format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(&path).expect("read example");
        let mut map = SourceMap::new();
        let file = map.add_file(&path, source.clone());
        let (sf, parse_diags) = parse_source_file(&source, file);
        assert!(parse_diags.is_empty(), "{path}: {parse_diags:?}");
        let (resolutions, _resolve_diags) = resolve_source_file(&sf);
        let mut tcx = TyCtxt::new();
        let (_table, diagnostics) = typecheck_source_file(&sf, &resolutions, &mut tcx);
        assert!(
            diagnostics.is_empty(),
            "{path}: type diagnostics: {diagnostics:?}"
        );
    }
}

#[test]
fn cast_allows_numeric_to_numeric() {
    let src = "fn main() { let i: i32 = 1i32; let _ = i as i64; let _ = i as f64 }\n";
    let checked = run(src);
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics: {:?}",
        checked.diagnostics,
    );
}

#[test]
fn cast_allows_bool_and_char_to_integer_but_rejects_string() {
    let src = "fn main() { let b: bool = true; let _ = b as i64; let s: String = \"x\".to_string(); let _ = s as i64 }\n";
    let checked = run(src);
    assert_eq!(checked.diagnostics.len(), 1);
    assert!(
        matches!(&checked.diagnostics[0].error, TypeError::InvalidCast { from, to } if from == "String" && to == "i64"),
        "expected InvalidCast, got {:?}",
        checked.diagnostics[0].error,
    );
}

#[test]
fn cast_fails_soft_on_inference_variable_source() {
    let src = "fn main() { let s = \"x\".to_string(); let _ = s as i64 }\n";
    let checked = run(src);
    assert!(
        checked.diagnostics.is_empty(),
        "inference-var source should not trip the cast check: {:?}",
        checked.diagnostics,
    );
}

#[test]
fn cast_same_type_is_a_noop_and_passes() {
    let src = "fn main() { let i: i64 = 1i64; let _ = i as i64 }\n";
    let checked = run(src);
    assert!(
        checked.diagnostics.is_empty(),
        "same-type cast should be allowed: {:?}",
        checked.diagnostics,
    );
}

#[test]
fn cast_u8_to_char_allowed_other_ints_not() {
    let src = "fn main() { let b: u8 = 65u8; let _: char = b as char }\n";
    let ok = run(src);
    assert!(
        ok.diagnostics.is_empty(),
        "u8 -> char should pass: {:?}",
        ok.diagnostics,
    );
    let src = "fn main() { let i: i32 = 65i32; let _: char = i as char }\n";
    let bad = run(src);
    assert_eq!(bad.diagnostics.len(), 1);
    assert!(
        matches!(&bad.diagnostics[0].error, TypeError::InvalidCast { from, to } if from == "i32" && to == "char"),
        "expected i32 -> char rejection: {:?}",
        bad.diagnostics[0].error,
    );
}

#[test]
fn unsuffixed_integer_literal_takes_let_annotation_width() {
    let checked = run("fn main() { let x: u32 = 42 }\n");
    assert!(
        checked.diagnostics.is_empty(),
        "u32 annotation should soak up the literal: {:?}",
        checked.diagnostics,
    );
}

#[test]
fn unsuffixed_integer_literal_defaults_to_i64_when_unconstrained() {
    let checked = run("fn main() { let x = 42 }\n");
    assert!(
        checked.diagnostics.is_empty(),
        "orphan literal should default cleanly: {:?}",
        checked.diagnostics,
    );
    // Walk the AST and find the binding's type entry; it must
    // have resolved to a concrete i64 by the end of typecheck.
    let main = checked
        .source
        .items
        .iter()
        .find_map(|item| {
            if let ItemKind::Fn(f) = &item.kind {
                if f.name.name == "main" {
                    return Some(f);
                }
            }
            None
        })
        .expect("main fn");
    let body = main.body.as_ref().expect("main body");
    let ExprKind::Block(block) = &body.kind else {
        panic!("expected block body");
    };
    let StmtKind::Let { init, .. } = &block.stmts[0].kind else {
        panic!("expected let statement");
    };
    let init = init.as_ref().expect("let initializer");
    let init_id = match &init.kind {
        ExprKind::Literal(_) => init.id,
        other => panic!("expected literal initializer, got {other:?}"),
    };
    let ty = checked.table.get(init_id).expect("literal type");
    let kind = checked.tcx.kind(ty).expect("kind");
    assert!(
        matches!(kind, TyKind::Int(gossamer_types::IntTy::I64)),
        "unconstrained literal should default to i64, got {kind:?}",
    );
}

#[test]
fn unsuffixed_integer_literal_rejected_in_string_position() {
    let checked = run("fn main() { let x: String = 42 }\n");
    assert_eq!(
        checked.diagnostics.len(),
        1,
        "expected one mismatch diagnostic: {:?}",
        checked.diagnostics,
    );
    let TypeError::TypeMismatch { expected, found } = &checked.diagnostics[0].error else {
        panic!(
            "expected TypeMismatch, got {:?}",
            checked.diagnostics[0].error
        );
    };
    assert_eq!(expected, "String");
    assert_eq!(found, "{integer}");
}
