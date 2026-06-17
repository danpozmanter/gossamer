//! End-to-end tests for AST → HIR lowering.

use gossamer_hir::{
    HirBinaryOp, HirExprKind, HirItemKind, HirLiteral, HirPatKind, HirStmtKind, lower_source_file,
};
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

fn lower(source: &str) -> (gossamer_hir::HirProgram, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    (program, tcx)
}

#[test]
fn simple_function_lowers_to_hir_fn() {
    let (program, _tcx) = lower("fn main() {}\n");
    assert_eq!(program.items.len(), 1);
    let HirItemKind::Fn(f) = &program.items[0].kind else {
        panic!("expected fn");
    };
    assert_eq!(f.name.name, "main");
    assert!(f.body.is_some());
    assert!(!f.has_self);
}

#[test]
fn pipe_rewrites_to_call_with_appended_argument() {
    let (program, _tcx) = lower(
        "fn wrap(a: i32, b: i32) -> i32 { a }\n\nfn caller(x: i32) -> i32 { x |> wrap(0i32) }\n",
    );
    let caller = program
        .items
        .iter()
        .find_map(|item| match &item.kind {
            HirItemKind::Fn(f) if f.name.name == "caller" => Some(f),
            _ => None,
        })
        .expect("caller lowered");
    let body = caller.body.as_ref().unwrap();
    let tail = body.block.tail.as_ref().expect("tail present");
    match &tail.kind {
        HirExprKind::Call { args, .. } => {
            assert_eq!(args.len(), 2, "expected appended pipe argument");
            match &args[0].kind {
                HirExprKind::Literal(HirLiteral::Int(text)) => assert!(text.starts_with('0')),
                other => panic!("unexpected first arg: {other:?}"),
            }
            match &args[1].kind {
                HirExprKind::Path { segments, .. } => assert_eq!(segments[0].name, "x"),
                other => panic!("unexpected second arg: {other:?}"),
            }
        }
        other => panic!("pipe did not rewrite to call: {other:?}"),
    }
}

#[test]
fn try_operator_lowers_to_match() {
    let (program, _tcx) =
        lower("fn main() -> i32 { let x = ok()?\n    x }\nfn ok() -> i32 { 0i32 }\n");
    let main = program
        .items
        .iter()
        .find_map(|item| match &item.kind {
            HirItemKind::Fn(f) if f.name.name == "main" => Some(f),
            _ => None,
        })
        .expect("main lowered");
    let body = main.body.as_ref().unwrap();
    let let_init = match &body.block.stmts[0].kind {
        HirStmtKind::Let { init, .. } => init.as_ref().unwrap(),
        other => panic!("expected let: {other:?}"),
    };
    match &let_init.kind {
        HirExprKind::Match { arms, .. } => {
            assert_eq!(arms.len(), 2);
            match &arms[0].pattern.kind {
                HirPatKind::Variant { name, .. } => assert_eq!(name.name, "Ok"),
                other => panic!("unexpected Ok arm: {other:?}"),
            }
            match &arms[1].pattern.kind {
                HirPatKind::Variant { name, .. } => assert_eq!(name.name, "Err"),
                other => panic!("unexpected Err arm: {other:?}"),
            }
        }
        other => panic!("try did not lower to match: {other:?}"),
    }
}

#[test]
fn unknown_trait_bound_emits_diagnostic() {
    // `fn f<T: Hashabel>(...)` declares a bound on an unknown
    // trait. The typechecker should surface a `GT0011
    // unknown-trait-bound` diagnostic at declaration time so the
    // typo doesn't slip past to a runtime "no method" error.
    let mut map = SourceMap::new();
    let source = "fn need_hash<T: Hashabel>(x: T) -> T { x }\n";
    let file = map.add_file("bound.gos", source.to_string());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(source, file);
    assert!(parse_diags.is_empty());
    let (resolutions, _) = gossamer_resolve::resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (_table, diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    let found = diags.iter().any(|d| d.error.code() == "GT0011");
    assert!(
        found,
        "expected GT0011 unknown-trait-bound for `Hashabel`, got: {diags:?}",
    );
}

#[test]
fn known_builtin_trait_bound_is_accepted() {
    // `Iterator` is a built-in trait name; a bound on it must
    // NOT produce an unknown-trait diagnostic.
    let mut map = SourceMap::new();
    let source = "fn collect<T: Iterator>(it: T) -> T { it }\n";
    let file = map.add_file("bound.gos", source.to_string());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(source, file);
    assert!(parse_diags.is_empty());
    let (resolutions, _) = gossamer_resolve::resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (_table, diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    let any_unknown_bound = diags.iter().any(|d| d.error.code() == "GT0011");
    assert!(
        !any_unknown_bound,
        "Iterator bound should be accepted, got: {diags:?}",
    );
}

#[test]
fn try_on_option_lowers_to_some_none_match() {
    // `?` applied to an Option-returning expression desugars to
    // `Some(v) => v, None => return None`. Pre-fix this fell
    // through to the Ok/Err arms and produced nonsense.
    let (program, _tcx) = lower(
        "fn maybe() -> Option<i64> { Some(7) }\n\
         fn caller() -> Option<i64> { let x = maybe()?\n    Some(x) }\n",
    );
    let caller = program
        .items
        .iter()
        .find_map(|item| match &item.kind {
            HirItemKind::Fn(f) if f.name.name == "caller" => Some(f),
            _ => None,
        })
        .expect("caller lowered");
    let body = caller.body.as_ref().unwrap();
    let let_init = match &body.block.stmts[0].kind {
        HirStmtKind::Let { init, .. } => init.as_ref().unwrap(),
        other => panic!("expected let: {other:?}"),
    };
    match &let_init.kind {
        HirExprKind::Match { arms, .. } => {
            assert_eq!(arms.len(), 2);
            match &arms[0].pattern.kind {
                HirPatKind::Variant { name, .. } => {
                    assert_eq!(name.name, "Some", "first arm should be Some");
                }
                other => panic!("unexpected first arm: {other:?}"),
            }
            match &arms[1].pattern.kind {
                HirPatKind::Variant { name, .. } => {
                    assert_eq!(name.name, "None", "second arm should be None");
                }
                other => panic!("unexpected second arm: {other:?}"),
            }
        }
        other => panic!("`?` on Option did not lower to Some/None match: {other:?}"),
    }
}

#[test]
fn for_loop_lowers_to_loop_plus_match() {
    let (program, _tcx) = lower("fn main() { for x in 0..10 { let y = x } }\n");
    let main = program
        .items
        .iter()
        .find_map(|item| match &item.kind {
            HirItemKind::Fn(f) if f.name.name == "main" => Some(f),
            _ => None,
        })
        .expect("main lowered");
    let body = main.body.as_ref().unwrap();
    let tail = body.block.tail.as_ref().expect("tail present");
    match &tail.kind {
        HirExprKind::Loop { body, .. } => match &body.kind {
            HirExprKind::Block(block) => {
                let inner_tail = block.tail.as_ref().expect("loop tail");
                match &inner_tail.kind {
                    HirExprKind::Match { arms, .. } => {
                        assert_eq!(arms.len(), 2);
                        match &arms[1].body.kind {
                            HirExprKind::Break { .. } => {}
                            other => panic!("None arm should break: {other:?}"),
                        }
                    }
                    other => panic!("expected match in loop: {other:?}"),
                }
            }
            other => panic!("expected block: {other:?}"),
        },
        other => panic!("for did not lower to loop: {other:?}"),
    }
}

#[test]
fn binary_ops_round_trip_through_lowering() {
    let (program, _tcx) = lower("fn add(a: i32, b: i32) -> i32 { a + b }\n");
    let add = program
        .items
        .iter()
        .find_map(|item| match &item.kind {
            HirItemKind::Fn(f) if f.name.name == "add" => Some(f),
            _ => None,
        })
        .expect("add lowered");
    let tail = add
        .body
        .as_ref()
        .and_then(|body| body.block.tail.as_ref())
        .expect("tail present");
    match &tail.kind {
        HirExprKind::Binary { op, .. } => assert_eq!(*op, HirBinaryOp::Add),
        other => panic!("unexpected expr kind: {other:?}"),
    }
}

#[test]
fn every_lowered_expr_has_a_type() {
    let (program, tcx) = lower("fn add(a: i32, b: i32) -> i32 { a + b }\n");
    let add = program
        .items
        .iter()
        .find_map(|item| match &item.kind {
            HirItemKind::Fn(f) if f.name.name == "add" => Some(f),
            _ => None,
        })
        .expect("add lowered");
    let tail = add
        .body
        .as_ref()
        .and_then(|body| body.block.tail.as_ref())
        .expect("tail present");
    assert!(
        tcx.kind(tail.ty).is_some(),
        "tail ty was not interned by this ctx"
    );
}

#[test]
fn example_programs_lower_without_panics() {
    for name in ["hello_world.gos", "line_count.gos", "web_server.gos"] {
        let path = format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(&path).expect("read example");
        let (program, _tcx) = lower(&source);
        assert!(!program.items.is_empty(), "{path}: no items lowered");
    }
}
