//! End-to-end type-checker tests driven by parser + resolver output.

use gossamer_ast::{ExprKind, ItemKind, SourceFile, StmtKind};
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{
    IntTy, TyCtxt, TyKind, TypeError, TypeTable, typecheck_source_file,
    typecheck_source_file_with_lazy_iterators,
};

struct Checked {
    source: SourceFile,
    table: TypeTable,
    diagnostics: Vec<gossamer_types::TypeDiagnostic>,
    tcx: TyCtxt,
}

fn run(source: &str) -> Checked {
    run_with_lazy_iterators(source, false)
}

fn run_with_lazy_iterators(source: &str, lazy_iterators: bool) -> Checked {
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
    let (table, diagnostics) = if lazy_iterators {
        typecheck_source_file_with_lazy_iterators(&sf, &resolutions, &mut tcx, true)
    } else {
        typecheck_source_file(&sf, &resolutions, &mut tcx)
    };
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
fn range_values_are_lazy_iterators_in_every_edition() {
    for lazy_iterators in [false, true] {
        let checked = run_with_lazy_iterators("fn main() { let r = 10.. }\n", lazy_iterators);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
            panic!("expected fn");
        };
        let ExprKind::Block(block) = &decl.body.as_ref().expect("body").kind else {
            panic!("expected block");
        };
        let StmtKind::Let {
            init: Some(init), ..
        } = &block.stmts[0].kind
        else {
            panic!("expected initialized let");
        };
        let ty = checked.table.get(init.id).expect("range typed");
        assert!(matches!(checked.tcx.kind(ty), Some(TyKind::Iterator(_))));
    }
}

#[test]
fn formatting_a_range_reports_the_iterator_remedy() {
    let checked = run("fn main() { println!(\"{}\", 10..) }\n");
    let diagnostic = checked
        .diagnostics
        .iter()
        .find(|diagnostic| matches!(diagnostic.error, TypeError::IteratorStateFormatted))
        .expect("iterator formatting diagnostic")
        .to_diagnostic();
    assert_eq!(diagnostic.code.as_str(), "GT0041");
    assert!(
        diagnostic
            .helps
            .iter()
            .any(|help| help.contains("iter::collect"))
    );
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
fn function_argument_rejects_plain_tuple_for_tuple_struct() {
    let checked = run("struct RGB(i64, i64, i64)\n\
         fn print_color(color: RGB) { println!(\"{}\", color) }\n\
         fn main() { let three = (1, 500, -200); print_color(three) }\n");
    assert!(
        checked.diagnostics.iter().any(|d| matches!(
            &d.error,
            TypeError::TypeMismatch { expected, found }
                if expected == "RGB" && found == "(i64, i64, i64)"
        )),
        "plain tuple must not satisfy a nominal tuple-struct parameter: {:?}",
        checked.diagnostics
    );

    let checked = run("struct RGB(i64, i64, i64)\n\
         struct Triple(i64, i64, i64)\n\
         fn print_color(color: RGB) {}\n\
         fn main() { print_color(Triple(1, 500, -200)) }\n");
    assert!(
        checked.diagnostics.iter().any(|d| matches!(
            &d.error,
            TypeError::TypeMismatch { expected, found }
                if expected == "RGB" && found == "Triple"
        )),
        "distinct tuple structs should be named concisely: {:?}",
        checked.diagnostics
    );
}

#[test]
fn function_boundaries_preserve_nominal_struct_identity() {
    let rejected = [
        "struct A { value: i64 }\nstruct B { value: i64 }\nfn take(v: A) {}\nfn main() { take(B { value: 1 }) }\n",
        "struct A(i64, i64)\nstruct B(i64, i64)\nfn take(v: A) {}\nfn main() { take(B(1, 2)) }\n",
        "struct A(i64, i64)\nfn make() -> A { (1, 2) }\nfn main() {}\n",
        "struct A { value: i64 }\nstruct B { value: i64 }\nfn id<T>(v: T) -> T { v }\nfn main() { let _ = id::<A>(B { value: 1 }) }\n",
        "struct A(i64)\nstruct B(i64)\nfn id<T>(v: T) -> T { v }\nfn main() { let _ = id::<A, B>(A(1)) }\n",
    ];
    for source in rejected {
        let checked = run(source);
        assert!(
            checked.diagnostics.iter().any(|d| matches!(
                d.error,
                TypeError::TypeMismatch { .. } | TypeError::CallArityMismatch { .. }
            )),
            "nominal mismatch crossed a function boundary: {source}\n{:?}",
            checked.diagnostics
        );
    }

    let accepted = run("struct A(i64, i64)\n\
         fn take_struct(v: A) {}\n\
         fn take_tuple(v: (i64, i64)) {}\n\
         fn id<T>(v: T) -> T { v }\n\
         fn take_fn(f: Fn(i64) -> i64) { let _ = f(1) }\n\
         fn main() { take_struct(A(1, 2)); take_tuple((1, 2)); take_fn(id) }\n");
    assert!(
        accepted.diagnostics.is_empty(),
        "{:?}",
        accepted.diagnostics
    );
}

#[test]
fn string_values_coerce_to_borrowed_str_at_typed_boundaries() {
    let checked = run("static GREETING: &str = \"hello\"\n\
         fn classify(value: bool) -> &str { if value { \"yes\" } else { \"no\" } }\n\
         fn take(value: &str) {}\n\
         fn main() { take(\"text\"); let _ = classify(true) }\n");
    assert!(
        checked.diagnostics.is_empty(),
        "String to &str coercions must remain valid: {:?}",
        checked.diagnostics
    );
}

#[test]
fn function_boundaries_reject_wrong_float_callable_and_pipeline_types() {
    let rejected = [
        "fn take(v: i64) {}\nfn main() { take(1.5) }\n",
        "fn wrong(v: String) -> bool { true }\nfn take(f: Fn(i64) -> bool) {}\nfn main() { take(wrong) }\n",
        "fn invoke(f: Fn(i64) -> bool) { let _ = f(\"wrong\") }\nfn main() {}\n",
        "struct A(i64)\nstruct B(i64)\nfn take(v: A, n: i64) {}\nfn main() { 1 |> take(B(2)) }\n",
        "fn pair(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = 1 |> pair }\n",
    ];
    for source in rejected {
        let checked = run(source);
        assert!(
            checked.diagnostics.iter().any(|d| matches!(
                d.error,
                TypeError::TypeMismatch { .. } | TypeError::CallArityMismatch { .. }
            )),
            "invalid function call was accepted: {source}\n{:?}",
            checked.diagnostics
        );
    }
}

#[test]
fn methods_and_enum_constructors_check_declared_payload_types() {
    let method = run("struct A(i64)\n\
         struct B(i64)\n\
         impl A { fn take(&self, value: A) {} }\n\
         fn main() { let a = A(1); a.take(B(2)) }\n");
    assert!(
        method
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::TypeMismatch { .. })),
        "method parameter mismatch was accepted: {:?}",
        method.diagnostics
    );

    let variant = run("struct A(i64)\n\
         struct B(i64)\n\
         enum E { Value(A) }\n\
         fn main() { let _ = E::Value(B(2)) }\n");
    assert!(
        variant
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::TypeMismatch { .. })),
        "enum payload mismatch was accepted: {:?}",
        variant.diagnostics
    );

    let generic_method = run("struct A(i64)\n\
         struct B(i64)\n\
         struct Boxed<T> { value: T }\n\
         impl<T> Boxed<T> { fn take(&self, value: T) {} }\n\
         fn main() { let boxed = Boxed { value: A(1) }; boxed.take(B(2)) }\n");
    assert!(
        generic_method
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::TypeMismatch { .. })),
        "generic method parameter mismatch was accepted: {:?}",
        generic_method.diagnostics
    );

    let trait_method = run("struct A(i64)\n\
         struct B(i64)\n\
         trait Takes { fn take(&self, value: A); }\n\
         impl Takes for A { fn take(&self, value: A) {} }\n\
         fn call<T: Takes>(value: &T) { value.take(B(2)) }\n\
         fn main() {}\n");
    assert!(
        trait_method
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::TypeMismatch { .. })),
        "trait method parameter mismatch was accepted: {:?}",
        trait_method.diagnostics
    );

    let variant_arity = run("enum E { Pair(i64, i64) }\nfn main() { let _ = E::Pair(1) }\n");
    assert!(
        variant_arity
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::CallArityMismatch { .. })),
        "enum constructor arity mismatch was accepted: {:?}",
        variant_arity.diagnostics
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

fn has_immutable_assign(checked: &Checked) -> bool {
    checked
        .diagnostics
        .iter()
        .any(|d| matches!(d.error, TypeError::AssignToImmutable { .. }))
}

fn has_shared_reference_assign(checked: &Checked) -> bool {
    checked
        .diagnostics
        .iter()
        .any(|d| matches!(d.error, TypeError::AssignThroughSharedReference { .. }))
}

fn has_mutable_reference_to_immutable(checked: &Checked) -> bool {
    checked
        .diagnostics
        .iter()
        .any(|d| matches!(d.error, TypeError::MutableReferenceToImmutable { .. }))
}

#[test]
fn compound_assign_to_immutable_let_is_rejected() {
    let checked = run("fn main() { let total: i64 = 0\n total += 5 }\n");
    assert!(
        has_immutable_assign(&checked),
        "expected GT0030: {:?}",
        checked.diagnostics
    );
}

#[test]
fn plain_assign_to_immutable_let_is_rejected() {
    let checked = run("fn main() { let x: i64 = 0\n x = 5 }\n");
    assert!(has_immutable_assign(&checked), "{:?}", checked.diagnostics);
}

#[test]
fn assign_to_mut_let_is_accepted() {
    let checked = run("fn main() { let mut total: i64 = 0\n total += 5 }\n");
    assert!(
        !has_immutable_assign(&checked),
        "unexpected GT0030: {:?}",
        checked.diagnostics
    );
}

#[test]
fn assign_to_immutable_parameter_is_rejected() {
    let checked = run("fn f(x: i64) -> i64 { x += 1\n x }\n");
    assert!(has_immutable_assign(&checked), "{:?}", checked.diagnostics);
}

#[test]
fn assign_to_mut_parameter_is_accepted() {
    let checked = run("fn f(mut x: i64) -> i64 { x += 1\n x }\n");
    assert!(
        !has_immutable_assign(&checked),
        "unexpected GT0030: {:?}",
        checked.diagnostics
    );
}

#[test]
fn field_assign_through_immutable_binding_is_rejected() {
    let checked = run("struct P { x: i64 }\nfn main() { let p = P { x: 1 }\n p.x = 5 }\n");
    assert!(has_immutable_assign(&checked), "{:?}", checked.diagnostics);
}

#[test]
fn field_assign_through_mut_binding_is_accepted() {
    let checked = run("struct P { x: i64 }\nfn main() { let mut p = P { x: 1 }\n p.x = 5 }\n");
    assert!(
        !has_immutable_assign(&checked),
        "unexpected GT0030: {:?}",
        checked.diagnostics
    );
}

#[test]
fn write_through_mut_reference_parameter_is_accepted() {
    // The `p` binding is not itself `mut`, but a `&mut P` reference makes
    // the pointed-to place writable - matching Rust and not a false GT0030.
    let checked = run("struct P { x: i64 }\nfn bump(p: &mut P) { p.x = 9 }\n");
    assert!(
        !has_immutable_assign(&checked),
        "unexpected GT0030: {:?}",
        checked.diagnostics
    );
}

#[test]
fn mutable_reference_to_immutable_binding_is_rejected() {
    let checked = run("fn main() { let a = [1, 2]\n let c = &mut a }\n");
    assert!(
        has_mutable_reference_to_immutable(&checked),
        "expected GT0032: {:?}",
        checked.diagnostics
    );
}

#[test]
fn mutable_reference_to_mutable_binding_is_accepted() {
    let checked = run("fn main() { let mut a = [1, 2]\n let c = &mut a\n c[0] = 0 }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn mutable_reference_respects_static_mutability() {
    let immutable = run("static X: i64 = 1\nfn main() { let p = &mut X }\n");
    assert!(
        has_mutable_reference_to_immutable(&immutable),
        "expected GT0032: {:?}",
        immutable.diagnostics
    );

    let mutable = run("static mut X: i64 = 1\nfn main() { let p = &mut X }\n");
    assert!(mutable.diagnostics.is_empty(), "{:?}", mutable.diagnostics);
}

#[test]
fn assignment_through_shared_reference_is_rejected_precisely() {
    let checked = run("fn main() { let a = [1, 2]\n let mut d = &a\n d[0] = 0 }\n");
    assert!(
        has_shared_reference_assign(&checked),
        "expected GT0031: {:?}",
        checked.diagnostics
    );
    assert!(
        !has_immutable_assign(&checked),
        "shared-reference write must not be reported as GT0030: {:?}",
        checked.diagnostics
    );
}

#[test]
fn mutable_reference_binding_cannot_be_rebound_to_its_referent() {
    // `mut` permits rebinding `x`, but it does not erase the `&[i64; 2]`
    // type inferred for that binding. A new reference is required.
    let checked = run("fn main() { let mut x = &[1, 2]\n x = [2, 3] }\n");
    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let ExprKind::Block(block) = &decl.body.as_ref().expect("body").kind else {
        panic!("expected block");
    };
    let ExprKind::Assign { place, value, .. } = &block.tail.as_ref().expect("assignment").kind
    else {
        panic!("expected assignment");
    };
    let place_ty = checked.table.get(place.id).expect("place typed");
    let value_ty = checked.table.get(value.id).expect("value typed");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::TypeMismatch { .. })),
        "expected reference-rebinding type mismatch: {:?}; place={}, value={}",
        checked.diagnostics,
        gossamer_types::render_ty(&checked.tcx, place_ty),
        gossamer_types::render_ty(&checked.tcx, value_ty),
    );
}

#[test]
fn mutable_reference_mismatch_renders_resolved_referent_type() {
    let checked = run("fn main() { let mut a = 12\n let mut b = &mut a\n b = 16 }\n");
    assert!(
        checked.diagnostics.iter().any(|diagnostic| matches!(
            &diagnostic.error,
            TypeError::TypeMismatch { expected, found }
                if expected == "&mut i64" && found == "{integer}"
        )),
        "expected concrete reference mismatch, got {:?}",
        checked.diagnostics
    );
    assert!(
        checked
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.to_string().contains('?')),
        "inference variables leaked into diagnostics: {:?}",
        checked.diagnostics
    );
}

#[test]
fn return_reference_mismatch_renders_public_referent_type() {
    let checked = run("fn main() {\n\
         let value = no_dangle()\n\
         println!(\"{}\", value)\n\
         }\n\
         fn no_dangle() -> String {\n\
         let s = String::from(\"hello\")\n\
         &s\n\
         }\n");
    assert!(
        checked.diagnostics.iter().any(|diagnostic| matches!(
            &diagnostic.error,
            TypeError::TypeMismatch { expected, found }
                if expected == "String" && found == "&String"
        )),
        "expected String/&String mismatch, got {:?}",
        checked.diagnostics
    );
    assert!(
        checked
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.to_string().contains('?')),
        "inference variables leaked into diagnostics: {:?}",
        checked.diagnostics
    );
}

#[test]
fn vec_constructors_have_public_container_types() {
    let checked = run("fn main() {\n\
             let empty = Vec::new()\n\
             let reserved = Vec::with_capacity(4)\n\
             let values = Vec::from([1, 2])\n\
             let map = HashMap::with_capacity(4)\n\
         }\n");
    assert!(checked.diagnostics.is_empty(), "{:#?}", checked.diagnostics);

    let gossamer_ast::ItemKind::Fn(main) = &checked.source.items[0].kind else {
        panic!("expected main function");
    };
    let body = main.body.as_ref().expect("main body");
    let gossamer_ast::ExprKind::Block(body) = &body.kind else {
        panic!("expected main block");
    };
    let types = body
        .stmts
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            gossamer_ast::StmtKind::Let {
                init: Some(expr), ..
            } => checked.table.get(expr.id),
            _ => None,
        })
        .map(|ty| gossamer_types::render_public_ty(&checked.tcx, ty))
        .collect::<Vec<_>>();
    assert_eq!(types, ["Vec<_>", "Vec<_>", "Vec<i64>", "HashMap<_, _>"]);
}

#[test]
fn contextual_integer_literals_must_fit_their_declared_width() {
    let checked = run("struct ByteHolder { value: i8 }\n\
         fn takes_byte(value: i8) {}\n\
         fn byte() -> i8 { 567 }\n\
         fn main() {\n\
             let scalar: i8 = 567\n\
             let values: Vec<i8> = [1, 567]\n\
             let holder = ByteHolder { value: 567 }\n\
             takes_byte(567)\n\
             let negative: i8 = -129\n\
             let unsigned: u8 = 256\n\
             let signed_minimum: i8 = -128\n\
         }\n");
    let overflows = checked
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                &diagnostic.error,
                TypeError::IntLiteralOverflow { literal, ty }
                    if literal == "567" && ty == "i8"
            )
        })
        .count();
    assert_eq!(
        overflows, 5,
        "every contextual i8 literal should be checked: {:#?}",
        checked.diagnostics
    );
    assert!(checked.diagnostics.iter().any(|diagnostic| matches!(
        &diagnostic.error,
        TypeError::IntLiteralOverflow { literal, ty }
            if literal == "-129" && ty == "i8"
    )));
    assert!(checked.diagnostics.iter().any(|diagnostic| matches!(
        &diagnostic.error,
        TypeError::IntLiteralOverflow { literal, ty }
            if literal == "256" && ty == "u8"
    )));
}

#[test]
fn mutable_reference_binding_can_be_rebound_with_a_reference() {
    let checked = run("fn main() { let mut x = &[1, 2]\n x = &[2, 3] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn overlapping_mutable_references_are_rejected() {
    let checked = run(
        "fn main() { let mut a = [1, 2]\n let x = &mut a\n let y = &mut a\n x[0] = 0\n y[1] = 3 }\n",
    );
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TypeError::MutableReferenceConflict { .. })),
        "expected overlapping mutable-reference diagnostic: {:?}",
        checked.diagnostics
    );
}

#[test]
fn mutable_reference_rebinding_releases_the_previous_root() {
    let checked = run(
        "fn main() { let mut a = [1, 2]\n let mut b = [3, 4]\n let mut r = &mut a\n r = &mut b\n let s = &mut a\n s[0] = 0\n r[0] = 5 }\n",
    );
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
fn int_to_char_casts_allowed_float_rejected() {
    // Every int width casts to char by reading its low byte (the
    // masking `u8 as char` always applied), so `s[i] as char` works
    // without an `as u8` intermediate.
    for src in [
        "fn main() { let b: u8 = 65u8; let _: char = b as char }\n",
        "fn main() { let i: i32 = 65i32; let _: char = i as char }\n",
        "fn main() { let s = \"hi\"; let _: char = s[0] as char }\n",
    ] {
        let ok = run(src);
        assert!(
            ok.diagnostics.is_empty(),
            "int -> char should pass for {src:?}: {:?}",
            ok.diagnostics,
        );
    }
    let src = "fn main() { let f: f64 = 65.0; let _: char = f as char }\n";
    let bad = run(src);
    assert_eq!(bad.diagnostics.len(), 1);
    assert!(
        matches!(&bad.diagnostics[0].error, TypeError::InvalidCast { from, to } if from == "f64" && to == "char"),
        "expected f64 -> char rejection: {:?}",
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
fn later_narrow_assignments_do_not_retype_an_inferred_source_binding() {
    let checked = run("fn main() {
            let a = 256
            let mut b: i8 = 1
            let mut v: Vec<i8> = [1, 2]
            b = a
            v[0] = a
        }\n");

    let mismatches: Vec<_> = checked
        .diagnostics
        .iter()
        .filter_map(|diag| match &diag.error {
            TypeError::TypeMismatch { expected, found } => {
                Some((expected.as_str(), found.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(mismatches, vec![("i8", "i64"), ("i8", "i64")]);

    let ItemKind::Fn(decl) = &checked.source.items[0].kind else {
        panic!("expected fn");
    };
    let ExprKind::Block(block) = &decl.body.as_ref().expect("body").kind else {
        panic!("expected block");
    };
    let StmtKind::Let { pattern, .. } = &block.stmts[0].kind else {
        panic!("expected source binding");
    };
    let source_ty = checked.table.get(pattern.id).expect("source binding typed");
    assert_eq!(checked.tcx.kind(source_ty), Some(&TyKind::Int(IntTy::I64)));
}

#[test]
fn later_use_sites_cannot_narrow_established_numeric_bindings() {
    let checked = run("use std::collections::HashMap
        fn takes_i8(value: i8) {}
        fn bad_return() -> i8 {
            let value = 256
            value
        }
        fn main() {
            let value = 256
            let values = [256, 257]
            let optional = Some(256)
            let mut bytes: Vec<i8> = []
            let mut map: HashMap<String, i8> = HashMap::new()
            takes_i8(value)
            bytes.push(value)
            map.insert(\"key\", value)
            let narrowed: [i8; 2] = values
            let narrowed_option: Option<i8> = optional
        }\n");

    let mismatches: Vec<_> = checked
        .diagnostics
        .iter()
        .filter_map(|diag| match &diag.error {
            TypeError::TypeMismatch { expected, found } => {
                Some((expected.as_str(), found.as_str()))
            }
            _ => None,
        })
        .collect();
    assert!(
        mismatches.len() >= 6,
        "every later narrowing use must fail: {:?}",
        checked.diagnostics
    );
    assert!(
        mismatches
            .iter()
            .all(|(expected, found)| expected.contains("i8") && found.contains("i64")),
        "narrowing diagnostics must preserve established types: {mismatches:?}"
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
fn vec_get_returns_option_of_element_type() {
    let checked = run("fn main() { let got: Option<i64> = Vec::from([1, 2]).get(0) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn vec_rejects_methods_without_lowering() {
    for method in ["retain", "drain"] {
        let source = format!("fn main() {{ let mut xs = Vec::from([1, 2]); xs.{method}(0) }}\n");
        let checked = run(&source);
        assert!(
            checked.diagnostics.iter().any(|diag| matches!(
                &diag.error,
                TypeError::UnresolvedMethod { name, .. } if name == method
            )),
            "Vec::{method} should be rejected until it has VM and compiled lowering: {:?}",
            checked.diagnostics
        );
    }
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
fn lazy_iter_map_types_as_iterator_when_enabled() {
    let checked = run_with_lazy_iterators(
        "use std::iter\n\
         fn main() { let xs: Vec<String> = [\"a\"]\n\
         let ys = xs |> iter::map(|s| format!(\"({s})\")) }\n",
        true,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 1);
    assert!(
        matches!(closure_param_kind(&checked, init), TyKind::String),
        "lazy iter::map closure param must pin to the Vec element String"
    );
    let pipe_ty = checked.table.get(init.id).expect("pipe typed");
    let Some(TyKind::Iterator(elem)) = checked.tcx.kind(pipe_ty) else {
        panic!("lazy iter::map must type as Iterator");
    };
    assert!(matches!(checked.tcx.kind(*elem), Some(TyKind::String)));
}

#[test]
fn eager_iter_migration_aliases_preserve_combinator_types() {
    let checked = run("use std::iter\n\
         fn main() { let xs: Vec<String> = [\"a\", \"bb\"]\n\
         let ys = xs |> iter::eager_map(|s| format!(\"[{s}]\"))\n\
         let kept = ys |> iter::eager_filter(|s| s.len() > 2)\n\
         let found = kept |> iter::eager_find(|s| s.len() > 3)\n\
         let _ = found }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    let mapped = let_init(&checked, "main", 1);
    assert!(
        matches!(closure_param_kind(&checked, mapped), TyKind::String),
        "iter::eager_map must pin its closure parameter to the Vec element type"
    );
    let Some(TyKind::Vec(elem)) = checked.tcx.kind(checked.table.get(mapped.id).unwrap()) else {
        panic!("iter::eager_map must type as Vec");
    };
    assert!(matches!(checked.tcx.kind(*elem), Some(TyKind::String)));
}

#[test]
fn lazy_mode_keeps_eager_iter_aliases_as_vec() {
    let checked = run_with_lazy_iterators(
        "use std::iter\n\
         fn main() { let xs: Vec<String> = [\"a\", \"bb\"]\n\
         let ys = xs |> iter::eager_map(|s| format!(\"[{s}]\"))\n\
         let _ = ys }\n",
        true,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    let mapped = let_init(&checked, "main", 1);
    let Some(TyKind::Vec(elem)) = checked.tcx.kind(checked.table.get(mapped.id).unwrap()) else {
        panic!("iter::eager_map must type as Vec in lazy mode");
    };
    assert!(matches!(checked.tcx.kind(*elem), Some(TyKind::String)));
}

#[test]
fn lazy_range_take_enumerate_collect_pipeline_types() {
    let checked = run_with_lazy_iterators(
        "use std::iter\n\
         fn main() { let xs = iter::range(0, 10) |> iter::take(3) |> iter::enumerate()\n\
         let out = iter::collect(xs)\n\
         let _ = out }\n",
        true,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    let lazy_stage = let_init(&checked, "main", 0);
    let Some(TyKind::Iterator(pair)) = checked.tcx.kind(checked.table.get(lazy_stage.id).unwrap())
    else {
        panic!("range/take/enumerate pipeline must type as Iterator");
    };
    let Some(TyKind::Tuple(parts)) = checked.tcx.kind(*pair) else {
        panic!("enumerate item must be a tuple");
    };
    assert_eq!(parts.len(), 2);
    assert!(matches!(checked.tcx.kind(parts[0]), Some(TyKind::Int(_))));
    assert!(matches!(checked.tcx.kind(parts[1]), Some(TyKind::Int(_))));

    let collected = let_init(&checked, "main", 1);
    let Some(TyKind::Vec(_)) = checked.tcx.kind(checked.table.get(collected.id).unwrap()) else {
        panic!("iter::collect must materialize a Vec");
    };
}

#[test]
fn lazy_source_and_scalar_terminal_rows_type() {
    let checked = run_with_lazy_iterators(
        "use std::iter\n\
         fn main() { let one = iter::once(7)\n\
         let total = iter::sum(one)\n\
         let xs = iter::range(0, 3)\n\
         let best = iter::max(xs)\n\
         let _ = total\n\
         let _ = best }\n",
        true,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    let one = let_init(&checked, "main", 0);
    let Some(TyKind::Iterator(elem)) = checked.tcx.kind(checked.table.get(one.id).unwrap()) else {
        panic!("iter::once must type as Iterator in lazy mode");
    };
    assert!(matches!(checked.tcx.kind(*elem), Some(TyKind::Int(_))));

    let total = let_init(&checked, "main", 1);
    assert!(matches!(
        checked.tcx.kind(checked.table.get(total.id).unwrap()),
        Some(TyKind::Int(_))
    ));
}

#[test]
fn piped_result_default_with_closure_param_pins_to_err_type() {
    let checked = run("use std::result\n\
         fn fail() -> Result<i64, String> { Err(\"boom\") }\n\
         fn main() { let v = fail() |> result::unwrap_or_else(|e| println!(\"{e}\")) }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 0);
    assert!(
        matches!(closure_param_kind(&checked, init), TyKind::String),
        "result::unwrap_or_else closure param must pin to the Err payload String"
    );
}

#[test]
fn result_rejects_option_only_methods() {
    let checked = run("fn main() { let v = \"12\".parse::<i64>().ok_or(\"missing\") }\n");
    assert!(
        checked.diagnostics.iter().any(|d| matches!(
            d.error,
            TypeError::UnresolvedMethod { ref ty, ref name }
                if ty == "Result" && name == "ok_or"
        )),
        "{:?}",
        checked.diagnostics
    );
}

#[test]
fn raw_stdlib_result_helpers_support_question_mark() {
    let checked = run("use std::errors\n\
         fn load(path: String) -> Result<i64, errors::Error> {\n\
             let (size, is_file, is_dir, is_symlink, readonly, modified) = __gos_fs_metadata_raw(path)?\n\
             Ok(size)\n\
         }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
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
    diagnostics_for_with_lazy_iterators(source, false)
}

fn diagnostics_for_with_lazy_iterators(
    source: &str,
    lazy_iterators: bool,
) -> Vec<gossamer_types::TypeDiagnostic> {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse errors: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (_, diagnostics) = if lazy_iterators {
        typecheck_source_file_with_lazy_iterators(&sf, &resolutions, &mut tcx, true)
    } else {
        typecheck_source_file(&sf, &resolutions, &mut tcx)
    };
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

fn has_code(diags: &[gossamer_types::TypeDiagnostic], code: &str) -> bool {
    diags.iter().any(|d| d.error.code() == code)
}

// 0.18.1 authoritativeness fixes: each program below passed `gos check`
// on 0.18.0 and then SIGSEGV'd, printed garbage, or failed to build on
// the compiled tier. The checker now rejects them so "if it builds it
// runs" holds. Each test also has a valid sibling that must NOT trip.

#[test]
fn index_on_scalar_is_rejected() {
    // 0.18.0: compiled tier read through the i64 as a pointer (SIGSEGV).
    let d = diagnostics_for("fn main() { let x = 5; let y = x[0]; println!(\"{}\", y) }\n");
    assert!(has_code(&d, "GT0021"), "{d:?}");
}

#[test]
fn index_on_lazy_iterator_is_rejected() {
    let d = diagnostics_for_with_lazy_iterators(
        "use std::iter\nfn main() { let xs = iter::range(0, 3); let y = xs[0]; let _ = y }\n",
        true,
    );
    assert!(has_code(&d, "GT0021"), "{d:?}");
}

#[test]
fn formatting_lazy_iterator_is_rejected() {
    let d = diagnostics_for_with_lazy_iterators(
        "use std::iter\nfn main() { let xs = iter::range(0, 3); println!(\"{}\", xs) }\n",
        true,
    );
    assert!(has_code(&d, "GT0041"), "{d:?}");
}

#[test]
fn unsupported_lazy_iterator_input_adapter_is_rejected() {
    let d = diagnostics_for_with_lazy_iterators(
        "use std::iter\nfn main() { let xs = iter::range(0, 9) |> iter::step_by(2); let _ = xs }\n",
        true,
    );
    assert!(has_code(&d, "GT0001"), "{d:?}");
}

#[test]
fn lazy_terminal_rejects_materialized_vec_without_eager_alias() {
    let d = diagnostics_for_with_lazy_iterators(
        "use std::iter\nfn main() { let xs = [1, 2, 3]; let n = iter::sum(xs); let _ = n }\n",
        true,
    );
    assert!(has_code(&d, "GT0001"), "{d:?}");
}

#[test]
fn reusing_consumed_lazy_iterator_is_rejected() {
    let d = diagnostics_for_with_lazy_iterators(
        "use std::iter\nfn main() { let xs = iter::range(0, 3); let out = iter::collect(xs); let _ = xs; let _ = out }\n",
        true,
    );
    assert!(has_code(&d, "GT0042"), "{d:?}");
}

#[test]
fn reusing_pipe_consumed_lazy_iterator_is_rejected() {
    let d = diagnostics_for_with_lazy_iterators(
        "use std::iter\nfn main() { let xs = iter::range(0, 3); let out = xs |> iter::take(1); let _ = xs; let _ = out }\n",
        true,
    );
    assert!(has_code(&d, "GT0042"), "{d:?}");
}

#[test]
fn index_on_vec_and_string_is_accepted() {
    let d = diagnostics_for(
        "fn main() { let xs = [1, 2, 3]; let s = \"hi\"; println!(\"{} {}\", xs[0], s.byte_at(0)) }\n",
    );
    assert!(!has_code(&d, "GT0021"), "{d:?}");
}

#[test]
fn reasonable_fixed_array_is_accepted() {
    let d = diagnostics_for("fn main() { let a: [i64; 16] = [0; 16]; println!(\"{}\", a[0]) }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn benchmark_sized_fixed_array_is_accepted() {
    let d = diagnostics_for(
        "fn main() { let a: [f64; 40000] = [0.0; 40000]; println!(\"{}\", a[0]) }\n",
    );
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn very_large_fixed_array_is_accepted() {
    let d = diagnostics_for("fn main() { let a: [i64; 100000000] = [0; 100000000]; let _ = a }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn oversized_repeat_into_vec_is_accepted() {
    let d = diagnostics_for("fn main() { let v: [i64] = [0; 100000000]; let _ = v.len() }\n");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn call_of_scalar_value_is_rejected() {
    // 0.18.0: compiled tier emitted a call through a non-function symbol.
    let d = diagnostics_for("fn main() { let x = 5; let y = x(3); println!(\"{}\", y) }\n");
    assert!(has_code(&d, "GT0022"), "{d:?}");
}

#[test]
fn qualified_associated_call_is_not_flagged_as_non_callable() {
    // `String::new()` types its callee as `String`; it must not trip GT0022.
    let d = diagnostics_for("fn main() { let s = String::new(); println!(\"{}\", s) }\n");
    assert!(!has_code(&d, "GT0022"), "{d:?}");
}

#[test]
fn constructor_calls_are_not_flagged_as_non_callable() {
    let d = diagnostics_for("fn main() { let o = Some(5); let r = Ok(1); println!(\"ok\") }\n");
    assert!(!has_code(&d, "GT0022"), "{d:?}");
}

#[test]
fn named_struct_associated_function_is_not_checked_as_constructor() {
    let checked = run(
        "struct Pt { x: i64, y: i64 }\nimpl Pt { fn origin() -> Pt { Pt { x: 0, y: 0 } } }\nfn main() { let p = Pt::origin() }\n",
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
}

#[test]
fn struct_destructuring_requires_the_nominal_name() {
    let d = diagnostics_for(
        "struct Point { x: i64, y: i64 }\nfn main() { let p = Point { x: 1, y: 2 }; let (x, y) = p }\n",
    );
    assert!(has_code(&d, "GT0033"), "{d:?}");

    let d = diagnostics_for("fn main() { let pair = (1, 2); let (x, y) = pair }\n");
    assert!(!has_code(&d, "GT0033"), "{d:?}");
}

#[test]
fn named_struct_construction_requires_braces() {
    let d = diagnostics_for("struct Point { x: i64, y: i64 }\nfn main() { let _ = Point(1, 2) }\n");
    assert!(has_code(&d, "GT0034"), "{d:?}");

    let d = diagnostics_for(
        "struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { x: 1, y: 2 } }\n",
    );
    assert!(!has_code(&d, "GT0034"), "{d:?}");

    let d =
        diagnostics_for("struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { 1, 2 } }\n");
    assert!(!has_code(&d, "GT0034"), "{d:?}");

    let d = diagnostics_for(
        "struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { y: 2, 1 } }\n",
    );
    assert!(
        !has_code(&d, "GT0034") && !has_code(&d, "GT0035") && !has_code(&d, "GT0037"),
        "{d:?}"
    );
}

#[test]
fn tuple_struct_construction_requires_parentheses() {
    let d = diagnostics_for("struct Point(i64, i64)\nfn main() { let _ = Point { x: 1, y: 2 } }\n");
    assert!(has_code(&d, "GT0034"), "{d:?}");

    let d = diagnostics_for("struct Point(i64, i64)\nfn main() { let _ = Point(1, 2) }\n");
    assert!(!has_code(&d, "GT0034"), "{d:?}");
}

#[test]
fn named_struct_literal_fields_are_checked() {
    let d =
        diagnostics_for("struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { x: 1 } }\n");
    assert!(has_code(&d, "GT0035"), "{d:?}");

    let d = diagnostics_for(
        "struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { x: 1, x: 2, y: 3 } }\n",
    );
    assert!(has_code(&d, "GT0036"), "{d:?}");

    let d = diagnostics_for(
        "struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { x: 1, y: 2, z: 3 } }\n",
    );
    assert!(has_code(&d, "GT0006"), "{d:?}");

    let d = diagnostics_for("struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { 1 } }\n");
    assert!(has_code(&d, "GT0035"), "{d:?}");

    let d = diagnostics_for(
        "struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { 1, 2, 3 } }\n",
    );
    assert!(has_code(&d, "GT0037"), "{d:?}");

    let d = diagnostics_for(
        "struct Point { x: i64, y: i64 }\nfn main() { let _ = Point { x: 1, y: 2, 3 } }\n",
    );
    assert!(has_code(&d, "GT0037"), "{d:?}");
}

#[test]
fn out_of_range_tuple_index_is_rejected() {
    // 0.18.0: compiled tier read out-of-object memory (garbage / leak).
    let d = diagnostics_for("fn main() { let t = (1, 2); let x = t.5; println!(\"{}\", x) }\n");
    assert!(has_code(&d, "GT0023"), "{d:?}");
}

#[test]
fn positional_index_on_struct_is_rejected() {
    let d = diagnostics_for(
        "struct P { x: i64, y: i64 }\nfn main() { let p = P { x: 1, y: 2 }; let v = p.0; println!(\"{}\", v) }\n",
    );
    assert!(has_code(&d, "GT0023"), "{d:?}");
}

#[test]
fn in_range_tuple_index_is_accepted() {
    let d = diagnostics_for("fn main() { let t = (1, 2, 3); println!(\"{} {}\", t.0, t.2) }\n");
    assert!(!has_code(&d, "GT0023"), "{d:?}");
}

#[test]
fn method_call_with_wrong_arity_is_rejected() {
    // 0.18.0: VM aborted (GX0003) but the compiled tier zero-filled the
    // missing argument and returned a wrong result (tier divergence).
    let d = diagnostics_for(
        "struct A { x: i64 }\nimpl A { fn add(&self, a: i64, b: i64) -> i64 { self.x + a + b } }\nfn main() { let a = A { x: 1 }; println!(\"{}\", a.add(2)) }\n",
    );
    assert!(has_code(&d, "GT0018"), "{d:?}");
}

#[test]
fn method_call_with_correct_arity_is_accepted() {
    let d = diagnostics_for(
        "struct A { x: i64 }\nimpl A { fn add(&self, a: i64, b: i64) -> i64 { self.x + a + b } }\nfn main() { let a = A { x: 1 }; println!(\"{}\", a.add(2, 3)) }\n",
    );
    assert!(!has_code(&d, "GT0018"), "{d:?}");
}

#[test]
fn piped_method_call_counts_the_implicit_argument() {
    // `5 |> a.add(2)` desugars to `a.add(2, 5)`: arity is satisfied.
    let d = diagnostics_for(
        "struct A { x: i64 }\nimpl A { fn add(&self, a: i64, b: i64) -> i64 { self.x + a + b } }\nfn main() { let a = A { x: 1 }; println!(\"{}\", 5 |> a.add(2)) }\n",
    );
    assert!(!has_code(&d, "GT0018"), "{d:?}");
}

#[test]
fn nonexistent_method_on_user_struct_is_rejected() {
    // 0.18.0: a typo passed check; the compiled build failed on an
    // undefined `@A::bogus` symbol.
    let d = diagnostics_for(
        "struct A { x: i64 }\nfn main() { let a = A { x: 1 }; let y = a.bogus(); println!(\"{}\", y) }\n",
    );
    assert!(has_code(&d, "GT0002"), "{d:?}");
}

#[test]
fn real_method_on_user_struct_is_accepted() {
    let d = diagnostics_for(
        "struct A { x: i64 }\nimpl A { fn get(&self) -> i64 { self.x } }\nfn main() { let a = A { x: 1 }; println!(\"{}\", a.get()) }\n",
    );
    assert!(!has_code(&d, "GT0002"), "{d:?}");
}

#[test]
fn hashmap_keys_with_aggregate_key_is_rejected_before_lowering() {
    // Native maps hash aggregate keys as flat bytes and cannot reconstruct a
    // `Vec<K>` snapshot. This used to pass checking and return Unit-shaped
    // values in the compiled runtime.
    let d = diagnostics_for(
        "use std::collections::HashMap\nstruct Point { x: i64, y: i64 }\nfn main() { let m: HashMap<Point, i64> = HashMap::new(); let _ = m.keys(); }\n",
    );
    assert!(
        d.iter().any(|diagnostic| matches!(
            &diagnostic.error,
            TypeError::UnresolvedMethod { name, .. }
                if name == "keys for aggregate HashMap keys"
        )),
        "expected aggregate HashMap keys rejection, got {d:?}"
    );
}

#[test]
fn hashmap_keys_with_scalar_key_remains_available() {
    let d = diagnostics_for(
        "use std::collections::HashMap\nfn main() { let m: HashMap<i64, i64> = HashMap::new(); let _ = m.keys(); }\n",
    );
    assert!(d.is_empty(), "scalar HashMap keys should typecheck: {d:?}");
}

#[test]
fn strings_free_fn_rejects_integer_in_string_slot() {
    // 0.18.x: an integer in a `String` parameter of a `strings::` free
    // function passed check, then the compiled string shim dereferenced
    // it as a pointer (SIGSEGV the VM masked).
    let d = diagnostics_for(
        "use std::strings\nfn main() { let r = strings::contains(&\"hello\", 5)\nprintln!(\"{}\", r) }\n",
    );
    assert!(
        d.iter().any(
            |x| matches!(&x.error, TypeError::ArgumentTypeMismatch { callee, parameter, expected, found, .. }
                if callee == "strings::contains" && parameter == "needle"
                    && expected == "String | char" && found == "{integer}")
        ),
        "expected String/{{integer}} mismatch, got {d:?}"
    );
}

#[test]
fn strings_free_fn_rejects_misordered_integer_argument() {
    // `splitn(text, n, sep)`: an integer landing in the `sep` slot is a
    // mis-ordered call that the compiled tier would crash on.
    let d = diagnostics_for(
        "use std::strings\nfn main() { let p = strings::splitn(&\"a,b\", 2, 5)\nprintln!(\"{}\", p.len()) }\n",
    );
    assert!(
        d.iter()
            .any(|x| matches!(&x.error, TypeError::ArgumentTypeMismatch { .. })),
        "expected a type mismatch for the integer separator, got {d:?}"
    );
}

#[test]
fn strings_free_fn_rejects_float_in_string_slot() {
    // Preserve the source-facing `{float}` spelling for this mismatch.
    let d = diagnostics_for(
        "use std::strings\nfn main() { let r = strings::contains(&\"hi\", 1.5)\nprintln!(\"{}\", r) }\n",
    );
    assert!(
        d.iter().any(
            |x| matches!(&x.error, TypeError::ArgumentTypeMismatch { callee, parameter, expected, found, .. }
                if callee == "strings::contains" && parameter == "needle"
                    && expected == "String | char" && found == "{float}")
        ),
        "expected String/{{float}} mismatch, got {d:?}"
    );
}

#[test]
fn user_fn_rejects_float_in_string_parameter() {
    let d = diagnostics_for(
        "fn f(s: &String) -> i64 { s.len() }\nfn main() { println!(\"{}\", f(1.5)) }\n",
    );
    assert!(
        d.iter().any(
            |x| matches!(&x.error, TypeError::TypeMismatch { expected, found }
                if expected == "String" && found == "{float}")
        ),
        "expected String/{{float}} mismatch, got {d:?}"
    );
}

#[test]
fn string_method_rejects_integer_in_string_slot() {
    // The same crash via method form: `s.contains(5)` dispatches to the
    // string shim with the receiver as the implicit first argument.
    let d = diagnostics_for(
        "fn main() { let s = \"hi\"\nlet r = s.contains(5)\nprintln!(\"{}\", r) }\n",
    );
    assert!(
        d.iter().any(
            |x| matches!(&x.error, TypeError::ArgumentTypeMismatch { callee, parameter, expected, found, .. }
                if callee == "String::contains" && parameter == "needle"
                    && expected == "String | char" && found == "{integer}")
        ),
        "expected String/{{integer}} mismatch, got {d:?}"
    );
}

#[test]
fn string_method_accepts_string_and_char_patterns() {
    let d = diagnostics_for(
        "fn main() {\n\
         let s = \"hello world\"\n\
         let _ = s.contains(&\"world\")\n\
         let _ = s.contains('w')\n\
         let _ = s.replace(&\"o\", &\"0\")\n\
         let _ = s.splitn(2, &\" \")\n\
         }\n",
    );
    assert!(
        !d.iter()
            .any(|x| matches!(&x.error, TypeError::TypeMismatch { .. })),
        "valid string-method calls must type clean, got {d:?}"
    );
}

#[test]
fn string_method_surface_covers_receiver_shaped_strings_functions() {
    let d = diagnostics_for(
        "fn main() {\n\
         let s = \" hello world \"\n\
         let _ = s.bytes()\n\
         let _ = s.center(15, ' ')\n\
         let _ = s.chars()\n\
         let _ = s.contains(\"world\")\n\
         let _ = s.contains_any(\"aeiou\")\n\
         let _ = s.count('l')\n\
         let _ = s.ends_with(\" \")\n\
         let _ = s.equal_fold(\" HELLO WORLD \")\n\
         let _ = s.find(\"world\")\n\
         let _ = s.find_any(\"od\")\n\
         let _ = s.lines()\n\
         let _ = s.pad_left(16, '.')\n\
         let _ = s.pad_right(16, '.')\n\
         let _ = s.repeat(2)\n\
         let _ = s.replace(\"l\", \"L\")\n\
         let _ = s.replacen(\"l\", \"L\", 1)\n\
         let _ = s.rfind(\"l\")\n\
         let _ = s.rfind_any(\"le\")\n\
         let _ = s.rsplit_once(\" \")\n\
         let _ = s.slice(1, 5)\n\
         let _ = s.split(\" \")\n\
         let _ = s.split_once(\" \")\n\
         let _ = s.split_whitespace()\n\
         let _ = s.splitn(2, \" \")\n\
         let _ = s.starts_with(\" \")\n\
         let _ = s.strip_prefix(\" \")\n\
         let _ = s.strip_suffix(\" \")\n\
         let _ = s.to_bool()\n\
         let _ = s.to_f64()\n\
         let _ = s.to_i64()\n\
         let _ = s.to_lowercase()\n\
         let _ = s.to_title()\n\
         let _ = s.to_uppercase()\n\
         let _ = s.trim()\n\
         let _ = s.trim_end()\n\
         let _ = s.trim_end_matches(\" \")\n\
         let _ = s.trim_matches(\" \")\n\
         let _ = s.trim_start()\n\
         let _ = s.trim_start_matches(\" \")\n\
         }\n",
    );
    assert!(
        !d.iter().any(|x| matches!(
            &x.error,
            TypeError::UnresolvedMethod { ty, .. } if ty == "String"
        )),
        "all receiver-shaped strings functions must work as String methods: {d:?}"
    );
    assert!(
        d.is_empty(),
        "valid string method surface must type clean: {d:?}"
    );
}

#[test]
fn strings_join_is_not_a_string_method() {
    let d = diagnostics_for("fn main() { let _ = \"a\".join(\"-\") }\n");
    assert!(
        d.iter().any(|x| matches!(
            &x.error,
            TypeError::UnresolvedMethod { ty, name } if ty == "String" && name == "join"
        )),
        "`strings::join(parts, sep)` belongs to Vec, not String: {d:?}"
    );
}

#[test]
fn strings_free_fn_accepts_string_and_char_patterns() {
    // A real string needle, a `char` needle, and a `char` pad all type
    // cleanly - the validation must not reject the legitimate shapes.
    let d = diagnostics_for(
        "use std::strings\nfn main() {\n\
         let s = \"hello\"\n\
         let _ = strings::contains(&s, &\"ell\")\n\
         let _ = strings::contains(&s, 'e')\n\
         let _ = strings::replace(&s, &\"l\", &\"L\")\n\
         let _ = strings::pad_left(&\"7\", 4, '0')\n\
         let _ = strings::repeat(&\"ab\", 3)\n\
         }\n",
    );
    assert!(
        !d.iter()
            .any(|x| matches!(&x.error, TypeError::TypeMismatch { .. })),
        "valid string-function calls must type clean, got {d:?}"
    );
}

#[test]
fn strings_free_fn_accepts_inferred_borrowed_text_values() {
    let d = diagnostics_for(
        "use std::{metrics, strings, trace}\n\
         fn main() {\n\
         let c = metrics::Counter::new(\"requests_total\", \"total requests\")\n\
         let r = metrics::Registry::new()\n\
         r.register(c)\n\
         let text = r.render()\n\
         let _ = strings::contains(&text, \"requests_total\")\n\
         let tracer = trace::Tracer::new()\n\
         let span = tracer.start_span(\"checkout\")\n\
         let ended = span.end()\n\
         let json = ended.to_otlp_json()\n\
         let _ = strings::contains(&json, \"checkout\")\n\
         }\n",
    );
    assert!(
        !d.iter().any(|x| matches!(
            &x.error,
            TypeError::ArgumentTypeMismatch { callee, parameter, found, .. }
                if callee == "strings::contains" && parameter == "text" && found.starts_with('?')
        )),
        "borrowed inferred String values must not be reported as unresolved variables: {d:?}"
    );
    assert!(
        d.is_empty(),
        "valid inferred string calls must type clean: {d:?}"
    );
}

#[test]
fn string_slice_rejects_missing_or_non_integer_bounds() {
    let d = diagnostics_for(
        "fn main() {\n\
         let s = \"abcde\"\n\
         let _ = s.slice(1)\n\
         let _ = s.slice(1..3)\n\
         let _ = s.slice(\"a\")\n\
         }\n",
    );
    assert!(
        d.iter().any(|x| matches!(
            x.error,
            TypeError::CallArityMismatch { ref callee, expected: 2, found: 1 }
                if callee == "strings::slice"
        )),
        "missing string-method argument must be rejected: {d:?}"
    );
    assert_eq!(
        d.iter()
            .filter(|x| matches!(x.error, TypeError::CallArityMismatch { .. }))
            .count(),
        3,
        "every one-argument slice spelling must be rejected: {d:?}"
    );
    assert!(
        d.iter()
            .any(|x| matches!(x.error, TypeError::TypeMismatch { .. })),
        "a string bound must be rejected as non-integer: {d:?}"
    );
}

#[test]
fn string_range_index_has_string_type() {
    let checked = run("fn main() { let piece = \"abcd\"[1..3] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 0);
    let ty = checked.table.get(init.id).expect("range index typed");
    assert!(
        matches!(checked.tcx.kind(ty), Some(TyKind::String)),
        "String range index should produce String, got {:?}",
        checked.tcx.kind(ty)
    );
}

#[test]
fn fixed_array_range_index_has_vec_type() {
    let checked = run("fn main() { let piece = [1, 2, 3, 4][1..3] }\n");
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let init = let_init(&checked, "main", 0);
    let ty = checked.table.get(init.id).expect("range index typed");
    let Some(TyKind::Vec(elem)) = checked.tcx.kind(ty) else {
        panic!(
            "fixed array range index should produce Vec, got {:?}",
            checked.tcx.kind(ty)
        );
    };
    assert!(
        matches!(
            checked.tcx.kind(*elem),
            Some(TyKind::Int(gossamer_types::IntTy::I64))
        ),
        "fixed array range element should be i64, got {:?}",
        checked.tcx.kind(*elem)
    );
}

#[test]
fn fixed_array_rejects_vec_only_methods() {
    let d = diagnostics_for(
        "fn main() {\n\
         let mut a = [1; 3]\n\
         a.push(4)\n\
         let _ = a.pop()\n\
         a.insert(1, 9)\n\
         a.truncate(1)\n\
         a.reserve(8)\n\
         }\n",
    );
    for name in ["push", "pop", "insert", "truncate", "reserve"] {
        assert!(
            d.iter().any(|diagnostic| matches!(
                &diagnostic.error,
                TypeError::UnresolvedMethod { ty, name: method }
                    if ty == "[i64; 3]" && method == name
            )),
            "expected fixed-array `{name}` rejection, got {d:?}"
        );
    }
}

#[test]
fn string_slice_rejects_a_duplicate_receiver_argument() {
    let d = diagnostics_for(
        "fn main() {\n\
         let s = \"world\"\n\
         let _ = s.slice(s, 1, 3)\n\
         let _ = s |> |s| s.slice(s, 1, 3)\n\
         }\n",
    );
    assert!(
        d.iter().any(|x| matches!(
            x.error,
            TypeError::CallArityMismatch { ref callee, expected: 2, found: 3 }
                if callee == "strings::slice"
        )),
        "a repeated String receiver must be an arity error, not silently ignored: {d:?}"
    );
    assert_eq!(
        d.iter()
            .filter(|x| matches!(
                x.error,
                TypeError::CallArityMismatch { ref callee, expected: 2, found: 3 }
                    if callee == "strings::slice"
            ))
            .count(),
        2,
        "the closure form must reject the duplicate receiver too: {d:?}"
    );
}

#[test]
fn strings_free_calls_enforce_complete_arity() {
    let d = diagnostics_for(
        "use std::strings\n\
         fn main() {\n\
         let _ = strings::count(\"abc\")\n\
         let _ = strings::slice(\"abc\", 0, 2)\n\
         }\n",
    );
    assert!(
        d.iter().any(|x| matches!(
            x.error,
            TypeError::CallArityMismatch { ref callee, expected: 2, found: 1 }
                if callee == "strings::count"
        )),
        "missing free-function argument must be rejected: {d:?}"
    );
    assert!(
        !d.iter().any(|x| matches!(
            x.error,
            TypeError::CallArityMismatch { ref callee, .. } if callee == "strings::slice"
        )),
        "valid string slice must retain its three-argument contract: {d:?}"
    );
}

#[test]
fn string_parse_requires_payload_type_when_result_is_unexpected() {
    let d = diagnostics_for(
        "use std::strings\n\
         use std::errors\n\
         fn main() {\n\
         let good_a: Result<i64, errors::Error> = \"12\".parse()\n\
         let good_b: Result<i64, errors::Error> = strings::parse(\"34\")\n\
         let missing_a = \"56\".parse()\n\
         let missing_b = strings::parse(\"78\")\n\
         }\n",
    );
    let uninferred = d
        .iter()
        .filter(|diag| matches!(diag.error, TypeError::GenericReturnTypeUninferred { .. }))
        .count();
    assert_eq!(
        uninferred, 2,
        "untyped parse calls must require a concrete payload type: {d:?}"
    );
}

#[test]
fn string_parse_missing_question_mark_uses_assignment_type_as_payload_hint() {
    let d = diagnostics_for(
        "use std::strings\n\
         fn main() {\n\
         let bad_a: u8 = \"12\".parse()\n\
         let bad_b: u8 = strings::parse(\"34\")\n\
         }\n",
    );
    let mismatches = d
        .iter()
        .filter(|diag| {
            matches!(
                &diag.error,
                TypeError::TypeMismatch { expected, found }
                    if expected == "u8" && found == "Result<u8, errors::Error>"
            )
        })
        .count();
    assert_eq!(
        mismatches, 2,
        "missing-question-mark parse diagnostics must use the concrete target payload: {d:?}"
    );
}

#[test]
fn question_mark_requires_result_or_option_context() {
    let invalid_operand = diagnostics_for(
        "use std::errors\n\
         fn main() -> Result<i64, errors::Error> {\n\
         \"12\"?\n\
         }\n",
    );
    assert!(
        invalid_operand
            .iter()
            .any(|diag| matches!(diag.error, TypeError::QuestionMarkUnsupported { .. })),
        "question mark on a string must be rejected before lowering: {invalid_operand:?}"
    );

    let unit_function = diagnostics_for(
        "use std::errors\n\
         fn main() {\n\
         let bad: u8 = \"12\".parse()?\n\
         }\n",
    );
    assert!(
        unit_function
            .iter()
            .any(|diag| matches!(diag.error, TypeError::QuestionMarkUnsupported { .. })),
        "question mark in a unit-returning function must be rejected: {unit_function:?}"
    );

    let valid = diagnostics_for(
        "use std::errors\n\
         fn parse_one() -> Result<u8, errors::Error> {\n\
         let value: u8 = \"12\".parse()?\n\
         Ok(value)\n\
         }\n",
    );
    assert!(
        valid.is_empty(),
        "valid result propagation failed: {valid:?}"
    );
}

#[test]
fn strings_count_rejects_every_non_string_or_char_argument_with_parameter_names() {
    let d = diagnostics_for(
        "use std::strings\n\
         fn main() {\n\
         let _ = strings::count(1, \"a\")\n\
         let _ = strings::count(\"abc\", [1, 2])\n\
         let _ = strings::count((1, 2), \"a\")\n\
         let _ = strings::count(1..2, \"a\")\n\
         let _ = \"abc\".count([1, 2])\n\
         }\n",
    );
    let named: Vec<_> = d
        .iter()
        .filter_map(|diag| match &diag.error {
            TypeError::ArgumentTypeMismatch {
                callee, parameter, ..
            } => Some((callee.as_str(), parameter.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        named,
        vec![
            ("strings::count", "text"),
            ("strings::count", "needle"),
            ("strings::count", "text"),
            ("strings::count", "text"),
            ("String::count", "needle"),
        ],
        "every invalid count parameter must be rejected and identified: {d:?}"
    );
}

#[test]
fn named_string_argument_mismatch_includes_the_actual_literal_value() {
    let d = diagnostics_for("use std::strings\nfn main() { let _ = strings::count(\"ab\", 1) }\n");
    let Some(error) = d.iter().find_map(|diag| match &diag.error {
        TypeError::ArgumentTypeMismatch {
            callee,
            parameter,
            found,
            actual,
            ..
        } if callee == "strings::count" && parameter == "needle" => Some((found, actual)),
        _ => None,
    }) else {
        panic!("missing named argument mismatch: {d:?}");
    };
    assert_eq!(error.0, "{integer}");
    assert_eq!(error.1, "1");
    assert_eq!(
        d.len(),
        1,
        "one invalid parameter must produce exactly one error: {d:?}"
    );
}

#[test]
fn strings_count_reports_exact_parameter_types_and_names() {
    let d = diagnostics_for(
        "use std::strings\n\
         fn main() {\n\
         let _ = strings::count([1, 2], \"a\")\n\
         let _ = strings::count('a', \"a\")\n\
         let _ = strings::count(\"a\", 1)\n\
         }\n",
    );
    let mismatches: Vec<_> = d
        .iter()
        .filter_map(|diag| match &diag.error {
            TypeError::ArgumentTypeMismatch {
                callee,
                parameter,
                expected,
                found,
                actual,
            } => Some((
                callee.as_str(),
                parameter.as_str(),
                expected.as_str(),
                found.as_str(),
                actual.as_str(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        mismatches,
        vec![
            ("strings::count", "text", "String", "array", "[1, 2]"),
            ("strings::count", "text", "String", "char", "'a'"),
            (
                "strings::count",
                "needle",
                "String | char",
                "{integer}",
                "1"
            ),
        ],
        "diagnostics must match the source-facing signature: {d:?}"
    );
}

#[test]
fn named_string_argument_mismatch_uses_a_user_facing_container_type() {
    let d = diagnostics_for(
        "use std::strings\nfn main() { let _ = strings::slice([1, 2, 3], 1, 2) }\n",
    );
    assert!(
        d.iter().any(|diag| matches!(
            &diag.error,
            TypeError::ArgumentTypeMismatch { parameter, found, actual, .. }
                if parameter == "text" && found == "array" && actual == "[1, 2, 3]"
        )),
        "array mismatch must not expose an inference variable: {d:?}"
    );
}

#[test]
fn string_bytes_method_is_typed_as_byte_vector() {
    let d =
        diagnostics_for("fn main() { let bytes: Vec<u8> = \"ab\".bytes(); let _ = bytes[1] }\n");
    assert!(
        d.is_empty(),
        "String::bytes must typecheck as Vec<u8>: {d:?}"
    );
}

#[test]
fn stdlib_signature_catalog_rejects_non_string_arity_mismatch() {
    let d = diagnostics_for(
        "use std::math\n\
         fn main() { let _ = math::sqrt() }\n",
    );
    assert!(
        d.iter().any(|x| matches!(
            x.error,
            TypeError::CallArityMismatch { ref callee, expected: 1, found: 0 }
                if callee == "math::sqrt"
        )),
        "stdlib signature arity must reject missing math::sqrt arg: {d:?}"
    );
}

#[test]
fn stdlib_signature_catalog_shapes_non_string_argument_types() {
    let d = diagnostics_for(
        "use std::math\n\
         fn main() { let _ = math::pow(\"x\", 2.0) }\n",
    );
    assert!(
        d.iter().any(|x| matches!(
            &x.error,
            TypeError::TypeMismatch { expected, found }
                if expected == "f64" && found == "String"
        )),
        "stdlib signature argument expectations must reject math::pow(String, f64): {d:?}"
    );
}

#[test]
fn stdlib_signature_catalog_supplies_non_specialized_return_type() {
    let d = diagnostics_for(
        "use std::math\n\
         fn f() -> String { math::sqrt(4.0) }\n",
    );
    assert!(
        d.iter().any(|x| matches!(
            &x.error,
            TypeError::TypeMismatch { expected, found }
                if expected == "String" && found == "f64"
        )),
        "stdlib signature return type must reject returning math::sqrt as String: {d:?}"
    );
}

#[test]
fn json_value_variant_pattern_is_rejected() {
    // `json::Value` is an opaque dynamic-document handle with no matchable
    // discriminant; matching its variants silently falls through on the VM
    // and faults on the compiled tiers, so it is rejected at check.
    let d = diagnostics_for(
        "use std::encoding::json\n\
         fn f(v: json::Value) -> i64 {\n\
         match v {\n\
         json::Value::Int(n) => n,\n\
         json::Value::Object(pairs) => 1,\n\
         _ => 0,\n\
         }\n\
         }\n",
    );
    let hits = d
        .iter()
        .filter(|x| matches!(&x.error, TypeError::JsonValuePatternUnsupported { .. }))
        .count();
    assert_eq!(
        hits, 2,
        "both json variant patterns must be rejected, got {d:?}"
    );
}

#[test]
fn json_value_if_let_pattern_is_rejected() {
    let d = diagnostics_for(
        "use std::encoding::json\n\
         fn f(v: json::Value) {\n\
         if let json::Value::Object(pairs) = v { let _ = pairs }\n\
         }\n",
    );
    assert!(
        d.iter()
            .any(|x| matches!(&x.error, TypeError::JsonValuePatternUnsupported { .. })),
        "if-let json variant pattern must be rejected, got {d:?}"
    );
}

#[test]
fn json_value_constructor_expression_is_not_rejected() {
    // Constructing a `json::Value` (path/call position, not a pattern) is
    // the supported programmatic-build API and must stay clean.
    let d = diagnostics_for(
        "use std::encoding::json\n\
         fn f() -> json::Value { json::Value::Int(7) }\n",
    );
    assert!(
        !d.iter()
            .any(|x| matches!(&x.error, TypeError::JsonValuePatternUnsupported { .. })),
        "json::Value constructor must not be rejected, got {d:?}"
    );
}

#[test]
fn downgrade_on_scalar_is_rejected() {
    // `.downgrade()` needs a runtime RC pointer; a by-value scalar has no
    // header, so `Weak` of it faults on the compiled tiers. Rejected at check.
    let d = diagnostics_for("fn main() { let x: i64 = 5\nlet w = x.downgrade() }\n");
    assert!(
        d.iter()
            .any(|x| matches!(&x.error, TypeError::WeakDowngradeNonRc { .. })),
        "downgrade on a scalar must be rejected, got {d:?}"
    );
}

#[test]
fn downgrade_on_struct_is_accepted() {
    // A struct is a reference-counted aggregate with a real header, so
    // `.downgrade()` on it is valid and must type clean.
    let d = diagnostics_for(
        "struct Node { x: i64 }\n\
         fn main() { let n = Node { x: 1 }\nlet w = n.downgrade()\nlet _ = w }\n",
    );
    assert!(
        !d.iter()
            .any(|x| matches!(&x.error, TypeError::WeakDowngradeNonRc { .. })),
        "downgrade on a struct must be accepted, got {d:?}"
    );
}

#[test]
fn unary_neg_without_impl_is_rejected() {
    // `-a` on a struct with no `impl Neg` previously passed check and
    // faulted GX0002 at runtime; it must be a clean GT0003 error.
    let d = diagnostics_for(
        "struct P { x: i64 }\n\
         fn main() { let a = P { x: 1 }\nlet n = -a\nlet _ = n }\n",
    );
    assert!(has_code(&d, "GT0003"), "{d:?}");
}

#[test]
fn unary_neg_with_impl_is_accepted() {
    let d = diagnostics_for(
        "struct P { x: i64 }\n\
         impl Neg for P { fn neg(self) -> P { P { x: 0 - self.x } } }\n\
         fn main() { let a = P { x: 1 }\nlet n = -a\nprintln!(\"{}\", n.x) }\n",
    );
    assert!(!has_code(&d, "GT0003"), "{d:?}");
}

#[test]
fn unary_neg_on_enum_with_impl_is_accepted() {
    let d = diagnostics_for(
        "enum Sign { Pos(i64), Neg(i64) }\n\
         impl Neg for Sign { fn neg(self) -> Sign {\n\
         match self { Sign::Pos(n) => Sign::Neg(n), Sign::Neg(n) => Sign::Pos(n) } } }\n\
         fn main() { let p = Sign::Pos(7)\nlet s = -p\nlet _ = s }\n",
    );
    assert!(!has_code(&d, "GT0003"), "{d:?}");
}

#[test]
fn compound_assign_without_impl_is_rejected() {
    // `a += b` desugars through `Add`; a struct place with no impl
    // previously passed check and faulted at runtime.
    let d = diagnostics_for(
        "struct P { x: i64 }\n\
         fn main() { let mut a = P { x: 1 }\na += P { x: 2 }\nlet _ = a }\n",
    );
    assert!(has_code(&d, "GT0003"), "{d:?}");
}

#[test]
fn compound_assign_with_impl_is_accepted() {
    // Includes the heterogeneous shape `v *= 2.0` (impl Mul takes `f64`).
    let d = diagnostics_for(
        "struct V { x: f64 }\n\
         impl Add for V { fn add(self, o: V) -> V { V { x: self.x + o.x } } }\n\
         impl Mul for V { fn mul(self, s: f64) -> V { V { x: self.x * s } } }\n\
         fn main() { let mut v = V { x: 1.0 }\nv += V { x: 2.0 }\nv *= 2.0\nprintln!(\"{}\", v.x) }\n",
    );
    assert!(!has_code(&d, "GT0003"), "{d:?}");
}

#[test]
fn adt_on_rhs_of_scalar_operator_is_rejected() {
    // Operator dispatch is receiver-first: `2.0 * v` would call
    // `V::mul(2.0, v)` with a scalar `self`, so it is rejected rather
    // than miscompiled with swapped operands.
    let d = diagnostics_for(
        "struct V { x: f64 }\n\
         impl Mul for V { fn mul(self, s: f64) -> V { V { x: self.x * s } } }\n\
         fn main() { let v = V { x: 1.0 }\nlet w = 2.0 * v\nlet _ = w }\n",
    );
    assert!(has_code(&d, "GT0003"), "{d:?}");
}

#[test]
fn generic_impl_operator_is_accepted() {
    // `impl<T> Add for Wrap<T>` types `a + b` per instantiation via the
    // generic impl's declared return with substituted arguments.
    let d = diagnostics_for(
        "struct Wrap<T> { v: T }\n\
         impl<T> Add for Wrap<T> { fn add(self, o: Wrap<T>) -> Wrap<T> { Wrap { v: self.v + o.v } } }\n\
         fn main() { let a = Wrap { v: 3 }\nlet b = Wrap { v: 4 }\nprintln!(\"{}\", (a + b).v) }\n",
    );
    assert!(!has_code(&d, "GT0003"), "{d:?}");
}

#[test]
fn index_on_struct_without_impl_is_rejected() {
    let d = diagnostics_for(
        "struct P { x: i64 }\n\
         fn main() { let a = P { x: 1 }\nprintln!(\"{}\", a[0]) }\n",
    );
    assert!(has_code(&d, "GT0021"), "{d:?}");
}

#[test]
fn index_on_struct_with_impl_is_accepted() {
    let d = diagnostics_for(
        "struct P { x: i64 }\n\
         impl Index for P { fn index(self, i: i64) -> i64 { self.x + i } }\n\
         fn main() { let a = P { x: 1 }\nprintln!(\"{}\", a[0]) }\n",
    );
    assert!(!has_code(&d, "GT0021"), "{d:?}");
}

#[test]
fn flag_set_parse_supports_question_mark() {
    let d = diagnostics_for(
        "use std::env\n\
         use std::errors\n\
         use std::flag\n\
         fn main() -> Result<(), errors::Error> {\n\
         let mut fs = flag::Set::new(\"demo\")\n\
         let rest = fs.parse(env::args())?\n\
         println!(\"{}\", rest.len())\n\
         Ok(())\n\
         }\n",
    );
    assert!(!has_code(&d, "GT0045"), "{d:?}");
}
