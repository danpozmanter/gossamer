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
    // An unannotated closure parameter stays an unresolved inference
    // variable, so the cast check must stay soft on it. (A concrete
    // `String` source is correctly rejected - see
    // `cast_allows_bool_and_char_to_integer_but_rejects_string`.)
    let src = "fn main() { let f = |x| x as i64; let _ = f }\n";
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

#[test]
fn array_literal_coerces_to_vec_annotation() {
    let checked = run("fn main() { let xs: Vec<String> = [\"a\", \"b\"] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn array_literal_coerces_to_slice_annotation() {
    let checked = run("fn main() { let xs: [String] = [\"a\", \"b\"] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn repeat_literal_coerces_to_vec_annotation() {
    let checked = run("fn main() { let xs: Vec<i64> = [0; 4] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn array_literal_return_coerces_to_vec() {
    let checked = run("fn make() -> Vec<String> { [\"x\", \"y\"] }\nfn main() { make(); }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn vec_literal_rejects_wrong_element_type() {
    let checked = run("fn main() { let xs: Vec<String> = [1, 2] }\n");
    assert!(
        !checked.diagnostics.is_empty(),
        "assigning integer literals to Vec<String> must error",
    );
}

#[test]
fn if_branches_of_differing_array_length_join_to_vec() {
    // Differing lengths can only co-type as a Vec; this must check for any
    // element type, not only integer literals.
    let checked =
        run("fn main() { let v: Vec<String> = if true { [\"a\", \"b\"] } else { [\"c\"] } }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn nested_vec_literal_with_differing_inner_lengths_checks() {
    let checked = run("fn main() { let g: Vec<Vec<i64>> = [[1, 2], [3]] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn assignment_value_array_literal_adopts_vec_shape() {
    // `v = [2, 3]` where `v: Vec<i64>` must record the literal as a
    // heap Vec - a fixed `[i64; 2]` record desyncs the value layout
    // from the Vec-typed slot on the compiled tiers.
    let checked = run("fn main() { let mut v: Vec<i64> = [1]\n v = [2, 3] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let body = decl.body.as_ref().unwrap();
    let ExprKind::Block(block) = &body.kind else {
        panic!("expected block");
    };
    let assign = block.tail.as_ref().expect("assign tail");
    let ExprKind::Assign { value, .. } = &assign.kind else {
        panic!("expected assign");
    };
    let ty = checked.table.get(value.id).expect("assign value typed");
    assert!(
        matches!(checked.tcx.kind(ty), Some(TyKind::Vec(_))),
        "assign-value literal must adopt the place's Vec shape, got {:?}",
        checked.tcx.kind(ty)
    );
}

#[test]
fn some_payload_array_literal_adopts_vec_shape() {
    // `Some([1, 2])` bound to `Option<Vec<i64>>` must record the
    // payload literal as a Vec, not a fixed `[i64; 2]`.
    let checked = run("fn main() { let x: Option<Vec<i64>> = Some([1, 2]) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let body = decl.body.as_ref().unwrap();
    let ExprKind::Block(block) = &body.kind else {
        panic!("expected block");
    };
    let StmtKind::Let { init, .. } = &block.stmts[0].kind else {
        panic!("expected let");
    };
    let init = init.as_ref().unwrap();
    let ExprKind::Call { args, .. } = &init.kind else {
        panic!("expected Some(..) call");
    };
    let ty = checked.table.get(args[0].id).expect("payload typed");
    assert!(
        matches!(checked.tcx.kind(ty), Some(TyKind::Vec(_))),
        "Some(..) payload literal must adopt the expected Vec shape, got {:?}",
        checked.tcx.kind(ty)
    );
}

/// Recursively finds the first closure expression nested in `expr`.
fn find_closure(expr: &gossamer_ast::Expr) -> Option<&gossamer_ast::Expr> {
    match &expr.kind {
        ExprKind::Closure { .. } => Some(expr),
        ExprKind::Call { callee, args } => {
            find_closure(callee).or_else(|| args.iter().find_map(find_closure))
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            find_closure(receiver).or_else(|| args.iter().find_map(find_closure))
        }
        ExprKind::Binary { lhs, rhs, .. } => find_closure(lhs).or_else(|| find_closure(rhs)),
        _ => None,
    }
}

/// Init expression of the `stmt_idx`-th statement (a `let`) in `fn_name`.
fn let_init<'a>(checked: &'a Checked, fn_name: &str, stmt_idx: usize) -> &'a gossamer_ast::Expr {
    for item in &checked.source.items {
        if let ItemKind::Fn(decl) = &item.kind {
            if decl.name.name == fn_name {
                let body = decl.body.as_ref().expect("fn body");
                let ExprKind::Block(block) = &body.kind else {
                    panic!("expected block body");
                };
                let StmtKind::Let { init, .. } = &block.stmts[stmt_idx].kind else {
                    panic!("expected let statement");
                };
                return init.as_ref().expect("let init");
            }
        }
    }
    panic!("fn `{fn_name}` not found");
}

/// Resolved first-parameter type of the first closure nested in `root`.
fn closure_param_kind(checked: &Checked, root: &gossamer_ast::Expr) -> TyKind {
    let closure = find_closure(root).expect("closure expr");
    let ty = checked.table.get(closure.id).expect("closure typed");
    match checked.tcx.kind(ty).expect("closure ty kind") {
        TyKind::FnPtr(sig) | TyKind::FnTrait(sig) => {
            let input = *sig.inputs.first().expect("closure has a param");
            checked.tcx.kind(input).expect("param ty kind").clone()
        }
        other => panic!("closure typed as {other:?}, expected a fn type"),
    }
}

#[test]
fn map_err_method_closure_param_pins_to_err_payload_type() {
    let checked = run("fn fail() -> Result<i64, String> { Err(\"boom\") }\n\
         fn main() { let r = fail().map_err(|e| format!(\"w: {e}\")) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 0);
    assert!(
        matches!(closure_param_kind(&checked, init), TyKind::String),
        "map_err closure param must pin to the Err payload String"
    );
    // The call itself types as Result<i64, String> (format! output).
    let call_ty = checked.table.get(init.id).expect("call typed");
    let Some(TyKind::Adt { def, substs }) = checked.tcx.kind(call_ty) else {
        panic!("map_err call must type as a Result Adt");
    };
    assert_eq!(checked.tcx.def_name(*def), Some("Result"));
    let payloads = substs.types();
    assert!(matches!(
        checked.tcx.kind(payloads[0]),
        Some(TyKind::Int(gossamer_types::IntTy::I64))
    ));
    assert!(matches!(
        checked.tcx.kind(payloads[1]),
        Some(TyKind::String)
    ));
}

#[test]
fn option_map_method_closure_param_pins_to_payload_type() {
    let checked = run("fn main() { let o: Option<String> = Some(\"x\")\n\
         let m = o.map(|s| format!(\"<{s}>\")) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 1);
    assert!(
        matches!(closure_param_kind(&checked, init), TyKind::String),
        "Option::map closure param must pin to the payload String"
    );
    let call_ty = checked.table.get(init.id).expect("call typed");
    let Some(TyKind::Adt { def, substs }) = checked.tcx.kind(call_ty) else {
        panic!("Option::map call must type as an Option Adt");
    };
    assert_eq!(checked.tcx.def_name(*def), Some("Option"));
    assert!(matches!(
        checked.tcx.kind(substs.types()[0]),
        Some(TyKind::String)
    ));
}

#[test]
fn iter_map_free_fn_closure_param_pins_to_elem_type() {
    let checked = run("use std::iter\n\
         fn main() { let xs: Vec<String> = [\"a\"]\n\
         let ys = iter::map(|s| format!(\"[{s}]\"), xs) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 1);
    assert!(
        matches!(closure_param_kind(&checked, init), TyKind::String),
        "iter::map closure param must pin to the Vec element String"
    );
    let call_ty = checked.table.get(init.id).expect("call typed");
    let Some(TyKind::Vec(elem)) = checked.tcx.kind(call_ty) else {
        panic!("iter::map call must type as Vec");
    };
    assert!(matches!(checked.tcx.kind(*elem), Some(TyKind::String)));
}

#[test]
fn piped_iter_map_closure_param_pins_to_elem_type() {
    let checked = run("use std::iter\n\
         fn main() { let xs: Vec<String> = [\"a\"]\n\
         let ys = xs |> iter::map(|s| format!(\"({s})\")) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 1);
    assert!(
        matches!(closure_param_kind(&checked, init), TyKind::String),
        "piped iter::map closure param must pin to the Vec element String"
    );
    let pipe_ty = checked.table.get(init.id).expect("pipe typed");
    let Some(TyKind::Vec(elem)) = checked.tcx.kind(pipe_ty) else {
        panic!("piped iter::map must type as Vec");
    };
    assert!(matches!(checked.tcx.kind(*elem), Some(TyKind::String)));
}

#[test]
fn piped_result_default_with_closure_param_pins_to_err_type() {
    let checked = run("use std::result\n\
         fn fail() -> Result<i64, String> { Err(\"boom\") }\n\
         fn main() { let v = fail() |> result::default_with(|e| println!(\"{e}\")) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 0);
    assert!(
        matches!(closure_param_kind(&checked, init), TyKind::String),
        "result::default_with closure param must pin to the Err payload String"
    );
}

#[test]
fn unknown_std_combinator_with_closure_errors_loudly() {
    // `iter::mystery` has no checker signature row. A closure passed
    // there is uninferrable, which the compiled tiers would render as
    // a pointer-formatting bug - the checker must reject loudly.
    // (The resolver flags the unknown name separately; this test
    // bypasses the resolve assertion on purpose.)
    let source = "use std::iter\n\
                  fn main() { let xs: Vec<String> = [\"a\"]\n\
                  let ys = iter::mystery(|x| x, xs) }\n";
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse errors: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (_, diagnostics) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    assert!(
        diagnostics.iter().any(|d| matches!(
            &d.error,
            TypeError::ClosureParamUninferred { combinator } if combinator == "iter::mystery"
        )),
        "expected ClosureParamUninferred for iter::mystery, got {diagnostics:?}"
    );
}

#[test]
fn i128_type_annotation_rejected_with_gt0014() {
    let checked = run("fn main() { let x: i128 = 1 }\n");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(&d.error, TypeError::Int128Unsupported { ty } if ty == "i128")),
        "expected Int128Unsupported for `i128`, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn u128_param_and_return_rejected_with_gt0014() {
    let checked = run("fn f(x: u128) -> u128 { x }\nfn main() { }\n");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(&d.error, TypeError::Int128Unsupported { ty } if ty == "u128")),
        "expected Int128Unsupported for `u128`, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn i128_literal_suffix_rejected_with_gt0014() {
    let checked = run("fn main() { let y = 1i128 }\n");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(&d.error, TypeError::Int128Unsupported { ty } if ty == "i128")),
        "expected Int128Unsupported for the `1i128` suffix, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn cast_target_i128_rejected_with_gt0014() {
    let checked = run("fn main() { let z = 1 as i128 }\n");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(&d.error, TypeError::Int128Unsupported { ty } if ty == "i128")),
        "expected Int128Unsupported for `as i128`, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn bool_and_char_casts_pass_the_whitelist() {
    let checked = run("fn main() {\n\
         let a = true as i64\n\
         let b = 'a' as u8\n\
         let c = 65 as u8 as char\n\
         let d = false as u64\n\
         }\n");
    assert!(
        checked.diagnostics.is_empty(),
        "whitelisted bool/char casts must typecheck: {:?}",
        checked.diagnostics,
    );
}

// ---------------------------------------------------------------
// Task 22 - std fns as first-class values (GT0015 + tabled set).
// ---------------------------------------------------------------

fn diagnostics_for(source: &str) -> Vec<gossamer_types::TypeDiagnostic> {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse errors: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (_, diagnostics) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    diagnostics
}

#[test]
fn untabled_std_fn_as_value_errors_with_gt0015() {
    let source = "use std::{iter, strings}\n\
                  fn main() { let out = [\"ab\"] |> iter::map(strings::repeat)\n\
                  let _ = out }\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics.iter().any(|d| matches!(
            &d.error,
            TypeError::StdFnValueUnsupported { path } if path == "strings::repeat"
        )),
        "expected StdFnValueUnsupported for strings::repeat, got {diagnostics:?}"
    );
}

#[test]
fn tabled_std_fn_as_value_typechecks_clean() {
    let source = "use std::errors\n\
                  fn main() { let r: Result<i64, String> = Err(\"boom\")\n\
                  let m = r.map_err(errors::new)\nlet _ = m }\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics.is_empty(),
        "tabled std fn value must typecheck clean, got {diagnostics:?}"
    );
}

#[test]
fn json_encode_of_an_enum_errors_with_gt0016() {
    // The classic missing-`?`: `json::parse` returns a Result, so
    // encoding it (instead of the unwrapped Value) is rejected.
    let source = "use std::encoding::json\n\
                  fn main() { let v = json::parse(\"{}\")\n\
                  let _ = json::encode(&v) }\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics.iter().any(
            |d| matches!(&d.error, TypeError::JsonNotSerializable { op, .. } if op == "encode")
        ),
        "expected JsonNotSerializable for json::encode of a Result, got {diagnostics:?}"
    );
}

#[test]
fn json_encode_of_value_scalar_and_struct_typechecks_clean() {
    // json::Value, scalars, and structs are all valid encode inputs.
    let source = "use std::encoding::json\n\
                  struct P { x: i64 }\n\
                  fn main() {\n\
                  let _ = json::encode(&42)\n\
                  let _ = json::encode(&P { x: 1 })\n\
                  let v = json::parse(\"{}\").unwrap()\n\
                  let _ = json::encode(&v) }\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        !diagnostics
            .iter()
            .any(|d| matches!(&d.error, TypeError::JsonNotSerializable { .. })),
        "valid json::encode inputs must not trip GT0016, got {diagnostics:?}"
    );
}

#[test]
fn std_fn_in_callee_position_stays_legal() {
    // Call and pipe-rhs positions are the normal stdlib call shapes;
    // GT0015 only fires on genuine value positions.
    let source = "use std::strings\n\
                  fn main() { let a = strings::repeat(\"ab\", 2)\n\
                  let b = \"x\" |> strings::repeat(2)\nlet _ = a\nlet _ = b }\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        !diagnostics
            .iter()
            .any(|d| matches!(&d.error, TypeError::StdFnValueUnsupported { .. })),
        "callee positions must not trip GT0015, got {diagnostics:?}"
    );
}

#[test]
fn bare_std_path_as_pipe_rhs_stays_legal() {
    let source = "use std::option\n\
                  fn main() { let o: Option<i64> = Some(1)\n\
                  let v = o |> option::is_some\nlet _ = v }\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        !diagnostics
            .iter()
            .any(|d| matches!(&d.error, TypeError::StdFnValueUnsupported { .. })),
        "pipe-rhs bare std paths must not trip GT0015, got {diagnostics:?}"
    );
}
