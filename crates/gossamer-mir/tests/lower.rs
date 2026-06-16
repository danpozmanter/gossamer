//! End-to-end tests for MIR lowering + optimisation passes.

use gossamer_hir::lower_source_file;
use gossamer_lex::SourceMap;
use gossamer_mir::{
    BinOp, ConstValue, Local, Operand, Rvalue, StatementKind, Terminator, const_value_of,
    lower_program, optimise,
};
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

fn build(source: &str) -> (Vec<gossamer_mir::Body>, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let bodies = lower_program(&hir, &mut tcx);
    (bodies, tcx)
}

#[test]
fn identity_function_produces_return_only_body() {
    let (bodies, _) = build("fn id(x: i64) -> i64 { x }\n");
    let body = &bodies[0];
    assert_eq!(body.name, "id");
    assert_eq!(body.arity, 1);
    // Return slot + 1 parameter = 2 locals before any temporaries.
    assert!(body.locals.len() >= 2);
    let entry = body.block(body.blocks[0].id);
    assert!(matches!(entry.terminator, Terminator::Return));
}

#[test]
fn binary_op_produces_binary_rvalue() {
    let (bodies, _) = build("fn add(a: i64, b: i64) -> i64 { a + b }\n");
    let body = &bodies[0];
    let stmts: Vec<_> = body.blocks.iter().flat_map(|b| b.stmts.iter()).collect();
    let binary_present = stmts.iter().any(|stmt| {
        matches!(
            &stmt.kind,
            StatementKind::Assign {
                rvalue: Rvalue::BinaryOp { op: BinOp::Add, .. },
                ..
            }
        )
    });
    assert!(binary_present, "expected Add BinaryOp in body");
}

#[test]
fn if_expression_produces_switchint_terminator() {
    let source = r"fn pick(b: bool) -> i64 { if b { 1i64 } else { 0i64 } }
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let has_switch = body
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::SwitchInt { .. }));
    assert!(has_switch, "expected a SwitchInt terminator");
}

#[test]
fn direct_call_produces_call_terminator() {
    let source = r"fn helper() -> i64 { 7i64 }
fn caller() -> i64 { helper() }
";
    let (bodies, _) = build(source);
    let caller = bodies
        .iter()
        .find(|b| b.name == "caller")
        .expect("caller body");
    let has_call = caller
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Call { .. }));
    assert!(has_call, "expected a Call terminator");
}

#[test]
fn while_loop_produces_cfg_with_back_edge() {
    let source = r"fn main() { let mut n = 3i64
    while n > 0i64 {
        n = n - 1i64
    }
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    // Header + body block both jump somewhere; at least one Goto
    // targets an earlier or equal block id (the back edge).
    let ids: Vec<_> = body.blocks.iter().map(|b| b.id.as_u32()).collect();
    let has_back_edge = body.blocks.iter().enumerate().any(|(i, b)| {
        if let Terminator::Goto { target } = b.terminator {
            target.as_u32() <= ids[i]
        } else {
            false
        }
    });
    assert!(has_back_edge, "expected a loop back-edge");
}

#[test]
fn constant_folding_eliminates_const_arithmetic() {
    let source = r"fn compute() -> i64 { 1i64 + 2i64 }
";
    let (mut bodies, tcx) = build(source);
    let body = &mut bodies[0];
    optimise(body, &tcx);
    // After const-fold, no BinaryOp should remain with two constants.
    let has_binary = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::BinaryOp { .. },
                ..
            }
        )
    });
    assert!(!has_binary, "constant BinaryOp survived folding");
    let folded_int = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(3))),
                ..
            }
        )
    });
    assert!(folded_int, "expected Int(3) const after folding");
}

#[test]
fn const_value_of_finds_literal_assignments() {
    let source = r"fn compute() -> i64 { 42i64 }
";
    let (mut bodies, tcx) = build(source);
    let body = &mut bodies[0];
    optimise(body, &tcx);
    // Find a local that holds Int(42). At minimum, the return slot
    // should eventually be assigned a const int after copy prop.
    let found = body.locals.iter().enumerate().any(|(i, _)| {
        let id = u32::try_from(i).expect("local index");
        const_value_of(body, Local(id)) == Some(ConstValue::Int(42))
    });
    assert!(found);
}

#[test]
fn dead_store_eliminates_unused_const_assignment() {
    let source = r"fn main() { let x = 99i64 }
";
    let (mut bodies, tcx) = build(source);
    let body = &mut bodies[0];
    let before = gossamer_mir::statement_count(body);
    optimise(body, &tcx);
    let after = gossamer_mir::statement_count(body);
    assert!(after <= before, "dead-store should not add statements");
}

#[test]
fn bare_loop_as_function_tail_lowers_without_panicking() {
    let source = "fn forever() { loop { } }\n";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    assert_eq!(body.name, "forever");
    assert!(!body.blocks.is_empty());
}

#[test]
fn loop_with_body_as_function_tail_does_not_emit_return_assign() {
    let source = "fn forever() -> i64 { loop { let _ = 1i64 } }\n";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    assert!(!body.blocks.is_empty());
    let assigns_to_return = body.blocks.iter().flat_map(|b| b.stmts.iter()).any(
        |s| matches!(&s.kind, StatementKind::Assign { place, .. } if place.local == Local::RETURN),
    );
    assert!(
        !assigns_to_return,
        "diverging loop tail must not produce a RETURN assign"
    );
}

#[test]
fn go_stmt_does_not_confuse_following_statements() {
    let source = "fn main() { go fn() { let x = 1i64 } let y = 2i64 }\n";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    assert_eq!(body.name, "main");
    assert!(!body.blocks.is_empty());
}

#[test]
fn const_branch_elim_collapses_if_true_branch() {
    let source = "fn answer() -> i64 { if true { 1i64 } else { 2i64 } }\n";
    let (mut bodies, tcx) = build(source);
    let body = &mut bodies[0];
    gossamer_mir::optimise(body, &tcx);
    let has_switch = body
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, gossamer_mir::Terminator::SwitchInt { .. }));
    assert!(
        !has_switch,
        "const_branch_elim should replace SwitchInt with Goto"
    );
}

#[test]
fn const_branch_elim_keeps_switch_for_conditionally_assigned_local() {
    // Regression: `let mut neg = false; if v < 0 { neg = true }; if neg
    // { ... }` was previously folded by const-branch-elim into an
    // unconditional jump to the `then` arm because the optimiser
    // remembered only the *last* constant assigned to `neg` rather than
    // detecting the multiple-store case. Both the runtime `if v < 0`
    // and `if neg` checks must survive optimisation.
    let source = r"fn pick(v: i64) -> i64 {
    let mut neg = false
    if v < 0i64 { neg = true }
    if neg { 1i64 } else { 0i64 }
}
";
    let (mut bodies, tcx) = build(source);
    let body = &mut bodies[0];
    gossamer_mir::optimise(body, &tcx);
    let switch_count = body
        .blocks
        .iter()
        .filter(|b| matches!(b.terminator, gossamer_mir::Terminator::SwitchInt { .. }))
        .count();
    assert_eq!(
        switch_count, 2,
        "both `if v < 0` and `if neg` SwitchInts must survive - \
         conditionally assigned locals are not constants"
    );
}

#[test]
fn escape_analysis_accepts_simple_leaf_body() {
    let (bodies, _) = build("fn leaf() -> i64 { 99i64 }\n");
    let set = gossamer_mir::analyse_escape(&bodies[0]);
    assert!(set.escapes(gossamer_mir::Local::RETURN));
}

#[test]
fn trait_impl_method_with_match_tail_lowers() {
    let source = r"
struct App { x: i64 }

trait Handler {
    fn serve(&self, n: i64) -> i64;
}

impl Handler for App {
    fn serve(&self, n: i64) -> i64 {
        match n {
            0i64 => 1i64,
            _ => 2i64,
        }
    }
}

fn main() { }
";
    let (bodies, _) = build(source);
    // Impl methods are mangled to `Type::method` so that two
    // impls with the same method name on different types do not
    // collide in the codegen's by-name dispatch table. Either
    // form should appear: the trait impl's mangled name keys on
    // the impl's `self_name` (`App`).
    assert!(
        bodies
            .iter()
            .any(|b| b.name == "serve" || b.name == "App::serve"),
        "expected the impl method body to be lowered (mangled or bare)"
    );
}

#[test]
fn match_on_int_literal_lowers_to_switchint() {
    let source = r"fn main() -> i64 {
    let n = 1i64
    match n {
        0i64 => 10i64,
        1i64 => 20i64,
        _ => 30i64,
    }
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let has_switch_with_two_arms = body.blocks.iter().any(|b| match &b.terminator {
        Terminator::SwitchInt { arms, .. } => arms.len() == 2,
        _ => false,
    });
    assert!(
        has_switch_with_two_arms,
        "match should lower into a SwitchInt with both literal arms"
    );
}

#[test]
fn optimise_preserves_match_result_local_across_blocks() {
    // Post-optimise each arm block must still write its const value
    // into the shared result local - a block-local dead-store-elim
    // would drop them because the only use is in a later join block.
    let source = r"fn main() -> i64 {
    let n = 1i64
    match n {
        0i64 => 10i64,
        1i64 => 20i64,
        _ => 30i64,
    }
}
";
    let (mut bodies, tcx) = build(source);
    let body = &mut bodies[0];
    optimise(body, &tcx);
    let const_20_retained = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(20))),
                ..
            }
        )
    });
    assert!(
        const_20_retained,
        "global dead-store-elim must keep the winning arm's Const(20) write"
    );
}

#[test]
fn match_with_guard_lowers_to_chained_branches() {
    // Guarded arms now compile to a sequential
    // `if pattern_predicate && guard { body } else next` chain
    // (see `lower_match_with_guards`), so the body must NOT
    // contain the unsupported placeholder anymore.
    let source = r"fn pick(n: i64) -> i64 {
    match n {
        x if x > 0i64 => 1i64,
        _ => 0i64,
    }
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let has_unsupported_call = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, .. },
                ..
            } if name.starts_with("unsupported")
        )
    });
    assert!(
        !has_unsupported_call,
        "guarded match arms should lower into a real if-chain, not the unsupported placeholder"
    );
    // Sanity: at least one SwitchInt terminator (the chain
    // emits one per arm) must be present.
    let has_switch = body
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::SwitchInt { .. }));
    assert!(
        has_switch,
        "guarded chain should produce SwitchInt branches"
    );
}

#[test]
fn tuple_destructuring_let_binds_each_element() {
    let source = r"fn main() -> i64 {
    let (a, b) = (11i64, 22i64)
    a + b
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    // Each binding is a fresh local read through a
    // Projection::Field(i) from the tuple local. Count how many
    // Field-projection reads land in the body.
    let field_projection_reads = body
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| match &s.kind {
            StatementKind::Assign {
                rvalue: Rvalue::Use(Operand::Copy(place)),
                ..
            } => place
                .projection
                .iter()
                .any(|p| matches!(p, gossamer_mir::Projection::Field(_))),
            _ => false,
        })
        .count();
    assert!(
        field_projection_reads >= 2,
        "tuple destructuring should emit two Field projection reads"
    );
}

#[test]
fn cast_expression_lowers_to_rvalue_cast() {
    let source = r"fn narrow(n: i64) -> i32 { n as i32 }
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let has_cast = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::Cast { .. },
                ..
            }
        )
    });
    assert!(has_cast, "cast expression should emit Rvalue::Cast");
}

#[test]
fn array_repeat_lowers_to_rvalue_repeat() {
    let source = r"fn main() -> i64 {
    let xs = [42i64; 3i64]
    xs[1i64]
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let has_repeat = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::Repeat { count: 3, .. },
                ..
            }
        )
    });
    assert!(has_repeat, "expected Rvalue::Repeat with count 3");
}

#[test]
fn monomorphise_emits_one_specialised_body_per_distinct_substitution() {
    let source = r"fn ident<T>(x: T) -> T { x }

fn main() -> i64 {
    let a = ident::<i64>(10i64)
    let b = ident::<i64>(32i64)
    a + b
}
";
    let (mut bodies, mut tcx) = build(source);
    // Before monomorphisation: one generic body + main.
    assert!(bodies.iter().any(|b| b.name == "ident"));
    let before_count = bodies.len();
    gossamer_mir::monomorphise(&mut bodies, &mut tcx);
    // After: at least one specialised `ident` copy registered under
    // a `fn#…__mono__…` name. Two call sites with the same substs
    // collapse into a single specialisation.
    let specialised_count = bodies
        .iter()
        .filter(|b| b.name.starts_with("fn#") && b.name.contains("__mono__"))
        .count();
    assert!(
        specialised_count >= 1,
        "expected at least one mangled specialised body; bodies: {:?}",
        bodies.iter().map(|b| &b.name).collect::<Vec<_>>()
    );
    assert!(
        bodies.len() > before_count,
        "specialisation should add bodies"
    );
}

#[test]
fn monomorphise_emits_distinct_bodies_for_distinct_type_arguments() {
    let source = r"fn first<T>(a: T, b: T) -> T { a }

fn main() -> i64 {
    let i = first::<i64>(10i64, 20i64)
    let b = first::<bool>(true, false)
    if b { i } else { 0i64 }
}
";
    let (mut bodies, mut tcx) = build(source);
    gossamer_mir::monomorphise(&mut bodies, &mut tcx);
    let specialised: Vec<&String> = bodies
        .iter()
        .map(|b| &b.name)
        .filter(|n| n.starts_with("fn#") && n.contains("__mono__"))
        .collect();
    assert!(
        specialised.len() >= 2,
        "expected two distinct specialisations (i64 and bool); got {specialised:?}"
    );
}

#[test]
fn for_loop_over_exclusive_range_lowers_to_counter_loop() {
    let source = r"fn main() -> i64 {
    let mut sum = 0i64
    for n in 0i64..5i64 {
        sum = sum + n
    }
    sum
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let has_method_call_remnant = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic {
                    name: "unsupported_match_with_guards"
                        | "unsupported_match_complex_pattern"
                        | "unsupported_match_multiple_wildcard_arms"
                        | "unsupported_match_int_literal_unparseable"
                        | "unsupported_expr_range"
                        | "unsupported_expr_closure"
                        | "unsupported_expr_placeholder"
                        | "unsupported_field_access_unknown_struct"
                        | "unsupported_field_access_unknown_field"
                        | "unsupported_array_repeat_dynamic_count"
                        | "unsupported",
                    ..
                },
                ..
            }
        )
    });
    assert!(
        !has_method_call_remnant,
        "for-range must lower through the counter-loop shortcut, not the unsupported placeholder"
    );
    let has_add_op = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::BinaryOp { op: BinOp::Add, .. },
                ..
            }
        )
    });
    assert!(has_add_op, "expected the counter increment BinaryOp");
}

#[test]
fn for_loop_over_array_literal_lowers_to_indexed_loop() {
    let source = r"fn main() -> i64 {
    let mut sum = 0i64
    for x in [10i64, 20i64, 30i64] {
        sum = sum + x
    }
    sum
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let has_unsupported = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic {
                    name: "unsupported_match_with_guards"
                        | "unsupported_match_complex_pattern"
                        | "unsupported_match_multiple_wildcard_arms"
                        | "unsupported_match_int_literal_unparseable"
                        | "unsupported_expr_range"
                        | "unsupported_expr_closure"
                        | "unsupported_expr_placeholder"
                        | "unsupported_field_access_unknown_struct"
                        | "unsupported_field_access_unknown_field"
                        | "unsupported_array_repeat_dynamic_count"
                        | "unsupported",
                    ..
                },
                ..
            }
        )
    });
    assert!(
        !has_unsupported,
        "for-array must lower to the indexed-loop shortcut"
    );
}

#[test]
fn struct_literal_lowers_to_aggregate_and_field_access_to_projection() {
    let source = r"
struct Point { x: i64, y: i64 }

fn main() -> i64 {
    let p = Point { x: 10i64, y: 32i64 }
    p.x + p.y
}
";
    let (bodies, _) = build(source);
    let body = bodies.iter().find(|b| b.name == "main").expect("main body");
    let has_aggregate = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign { rvalue: Rvalue::Aggregate { operands, .. }, .. }
                if operands.len() == 2
        )
    });
    assert!(
        has_aggregate,
        "struct literal should lower to Rvalue::Aggregate"
    );
    let field_reads = body
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| match &s.kind {
            StatementKind::Assign {
                rvalue: Rvalue::Use(Operand::Copy(place)),
                ..
            } => place
                .projection
                .iter()
                .any(|p| matches!(p, gossamer_mir::Projection::Field(_))),
            _ => false,
        })
        .count();
    assert!(
        field_reads >= 2,
        "expected two field projections for p.x and p.y"
    );
}

#[test]
fn struct_literal_respects_declaration_order_under_reordered_initialisers() {
    let source = r"
struct Pair { a: i64, b: i64 }

fn main() -> i64 {
    let p = Pair { b: 7i64, a: 3i64 }
    p.a
}
";
    let (bodies, _) = build(source);
    let body = bodies.iter().find(|b| b.name == "main").expect("main body");
    // Find the aggregate statement and capture the operand order.
    let aggregate_operands = body
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .find_map(|s| match &s.kind {
            StatementKind::Assign {
                rvalue: Rvalue::Aggregate { operands, .. },
                ..
            } => Some(operands.clone()),
            _ => None,
        })
        .expect("expected struct aggregate");
    assert_eq!(aggregate_operands.len(), 2);
    // Each operand is Copy(Local(N)); resolve each back to its
    // originating literal by walking the statement list.
    let find_const = |local: Local| -> Option<i128> {
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(n))),
                } = &stmt.kind
                {
                    if place.local == local {
                        return Some(*n);
                    }
                }
            }
        }
        None
    };
    let operand_constants: Vec<Option<i128>> = aggregate_operands
        .iter()
        .map(|op| match op {
            Operand::Copy(place) => find_const(place.local),
            _ => None,
        })
        .collect();
    assert_eq!(
        operand_constants,
        vec![Some(3), Some(7)],
        "operand[0] must be `a`'s value (3), operand[1] must be `b`'s value (7)"
    );
}

#[test]
fn optimise_preserves_index_const_behind_projection_read() {
    let source = r"fn main() -> i64 {
    let xs = [5i64, 7i64, 9i64]
    xs[2i64]
}
";
    let (mut bodies, tcx) = build(source);
    let body = &mut bodies[0];
    optimise(body, &tcx);
    let has_aggregate = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::Aggregate { .. },
                ..
            }
        )
    });
    assert!(has_aggregate, "array aggregate was eliminated");
    let has_index_const = body.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(2))),
                ..
            }
        )
    });
    assert!(
        has_index_const,
        "index-holding Const(2) was dropped by dead-store-elim - projection reads must count as a use of the index local"
    );
}

#[test]
fn monomorphise_rewrites_call_sites_to_reference_specialised_names() {
    // Verifies end-to-end: after monomorphise, the call sites inside
    // `main` reference the mangled specialised body names so the
    // native backend can dispatch directly through `callees_by_name`.
    let source = r"fn first<T>(a: T, b: T) -> T { a }

fn main() -> i64 {
    let x = first::<i64>(10i64, 20i64)
    let y = first::<i64>(30i64, 40i64)
    x + y
}
";
    let (mut bodies, mut tcx) = build(source);
    gossamer_mir::monomorphise(&mut bodies, &mut tcx);
    let main = bodies.iter().find(|b| b.name == "main").expect("main");
    // main has two call sites; both must resolve through FnRef with
    // a non-empty `Substs`. After monomorphise the bodies list must
    // contain a specialised body whose name is the mangled form for
    // the i64 substitution.
    let fnref_substs: Vec<_> = main
        .blocks
        .iter()
        .filter_map(|b| match &b.terminator {
            Terminator::Call {
                callee: Operand::FnRef { def, substs },
                ..
            } => Some((*def, substs.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        fnref_substs.len(),
        2,
        "expected two call sites to `first::<i64>`; got: {fnref_substs:?}"
    );
    assert!(
        fnref_substs.iter().all(|(_, s)| !s.is_empty()),
        "every call site must carry substs post-typecheck"
    );
    // The distinct (def, substs) pair deduplicates to one specialised
    // body, shared between the two call sites.
    let mangled: Vec<&String> = bodies
        .iter()
        .map(|b| &b.name)
        .filter(|n| n.starts_with("fn#") && n.contains("__mono__"))
        .collect();
    assert_eq!(
        mangled.len(),
        1,
        "two calls with identical substs should share one specialised body; got {mangled:?}"
    );
}

#[test]
fn monomorphise_leaves_calls_to_non_generic_functions_untouched() {
    // A fn with no type parameters must keep empty substs and never
    // emit a specialised copy - specialisation must be driven by
    // substs, not by every Call terminator.
    let source = r"fn double(n: i64) -> i64 { n * 2i64 }

fn main() -> i64 {
    double(21i64)
}
";
    let (mut bodies, mut tcx) = build(source);
    let before = bodies.len();
    gossamer_mir::monomorphise(&mut bodies, &mut tcx);
    let mangled_count = bodies
        .iter()
        .filter(|b| b.name.starts_with("fn#") && b.name.contains("__mono__"))
        .count();
    assert_eq!(
        mangled_count,
        0,
        "monomorphic call must not produce a specialised body; bodies: {:?}",
        bodies.iter().map(|b| &b.name).collect::<Vec<_>>()
    );
    assert_eq!(bodies.len(), before, "no extra bodies expected");
}

/// A `HashMap` allocated only inside an `if` arm is reclaimed
/// without ever freeing uninitialised memory.
///
/// The owning slot is zero-initialised at function entry, so every
/// `gos_rt_map_free` the drop pass schedules (the pre-overwrite
/// guard and the at-`Return` reclaim) is a null-safe no-op on the
/// `else` path that never allocated. The map must still be freed on
/// the `if` path (no leak), and every free must be dominated by the
/// entry zero-init so the conditional shape can never free a live
/// uninit slot.
#[test]
fn drop_pass_guards_conditionally_initialised_local() {
    let source = r"
fn maybe_build(flag: bool) -> i64 {
    if flag {
        let m: HashMap<i64, i64> = HashMap::new()
        m.insert(1, 2)
        m.len()
    } else {
        0
    }
}
";
    let (bodies, _) = build(source);
    let body = bodies
        .iter()
        .find(|b| b.name == "maybe_build")
        .expect("body");
    // Every local freed by `gos_rt_map_free`, by the slot the call
    // releases.
    let freed_locals: Vec<Local> = body
        .blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter_map(|stmt| match &stmt.kind {
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, args },
                ..
            } if *name == "gos_rt_map_free" => match args.first() {
                Some(Operand::Copy(p)) if p.projection.is_empty() => Some(p.local),
                _ => None,
            },
            _ => None,
        })
        .collect();

    // The conditionally-allocated map is reclaimed (no leak).
    assert!(
        !freed_locals.is_empty(),
        "conditionally-initialised map must still be freed (no leak)"
    );

    // Every freed slot is zero-initialised in the entry block, so the
    // free is a null-safe no-op on the path that never allocated.
    let entry = &body.blocks[0];
    for local in &freed_locals {
        let zero_init = entry.stmts.iter().any(|stmt| {
            matches!(
                &stmt.kind,
                StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                } if place.projection.is_empty() && place.local == *local
            )
        });
        assert!(
            zero_init,
            "freed local {local:?} must be zero-initialised at entry so its free is null-safe"
        );
    }
}

/// `gos_rt_http_response_content_type` mints a fresh owned c-string
/// (`mints_owned_string`), so `let c = r.content_type` must move the
/// minted reference into the binding: no `gos_rt_rc_retain` anywhere
/// on the copy chain (move elision transfers the single reference)
/// and a `gos_rt_rc_release` on the binding. Without the
/// `mints_owned_string` entry the call temp is treated as a borrow,
/// the copy retains (+1), the binding releases (-1), and the minted
/// reference itself is never dropped - one leaked string per
/// `.content_type` read in compiled code.
#[test]
fn drop_pass_releases_http_response_content_type_string() {
    let source = r#"
use std::http

fn ct(url: &String) -> i64 {
    match http::get(url, []) {
        Ok(r) => {
            let c = r.content_type
            if c == "x" { 1 } else { 0 }
        }
        Err(_) => 0,
    }
}
"#;
    let (bodies, _) = build(source);
    let body = bodies.iter().find(|b| b.name == "ct").expect("body");

    let dest = body
        .blocks
        .iter()
        .find_map(|b| match &b.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                destination,
                ..
            } if name == "gos_rt_http_response_content_type" => Some(destination.local),
            _ => None,
        })
        .expect("content_type accessor call");

    // Move elision may transfer the minted reference along bare-Copy
    // chains (`let c = r.content_type`), so the release can land on
    // any alias of the call destination.
    let mut aliases = vec![dest];
    loop {
        let mut grew = false;
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Copy(p)),
                } = &stmt.kind
                    && place.projection.is_empty()
                    && p.projection.is_empty()
                    && aliases.contains(&p.local)
                    && !aliases.contains(&place.local)
                {
                    aliases.push(place.local);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }

    let alias_rc_calls = |wanted: &str| -> usize {
        body.blocks
            .iter()
            .flat_map(|b| b.stmts.iter())
            .filter(|stmt| {
                matches!(
                    &stmt.kind,
                    StatementKind::Assign {
                        rvalue: Rvalue::CallIntrinsic { name, args },
                        ..
                    } if *name == wanted
                        && matches!(
                            args.first(),
                            Some(Operand::Copy(p))
                                if p.projection.is_empty() && aliases.contains(&p.local)
                        )
                )
            })
            .count()
    };
    assert!(
        alias_rc_calls("gos_rt_rc_release") > 0,
        "minted content_type string must be released (aliases: {aliases:?})"
    );
    assert_eq!(
        alias_rc_calls("gos_rt_rc_retain"),
        0,
        "the minted reference must move into the binding, not be retained - \
         a retain here means the call temp was treated as a borrow and the \
         minted string leaks (aliases: {aliases:?})"
    );
}

/// C18 - drop pass keeps unconditional drops intact.
///
/// When a `HashMap` is allocated at the top of the function and
/// every path through the body keeps it owned by this frame, the
/// drop must still fire on `Return` to release the heap storage.
#[test]
fn drop_pass_keeps_unconditional_drop_intact() {
    let source = r"
fn build() -> i64 {
    let m: HashMap<i64, i64> = HashMap::new()
    m.insert(1, 2)
    m.len()
}
";
    let (bodies, _) = build(source);
    let body = bodies.iter().find(|b| b.name == "build").expect("body");
    let frees: Vec<_> = body
        .blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter_map(|stmt| match &stmt.kind {
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, .. },
                ..
            } => {
                if *name == "gos_rt_map_free" {
                    Some(*name)
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    assert!(
        !frees.is_empty(),
        "drop pass must free an unconditionally-allocated local"
    );
}

/// Destination local of a `X = Copy(tuple.0)` field-extract, the binding
/// produced when a tuple's first element is destructured.
fn field0_extract_dest(body: &gossamer_mir::Body) -> Option<Local> {
    body.blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .find_map(|stmt| match &stmt.kind {
            StatementKind::Assign {
                place,
                rvalue: Rvalue::Use(Operand::Copy(src)),
            } if place.projection.is_empty()
                && matches!(
                    src.projection.as_slice(),
                    [gossamer_mir::Projection::Field(0)]
                ) =>
            {
                Some(place.local)
            }
            _ => None,
        })
}

/// Count of `name` intrinsic calls whose first argument is `local`.
fn rc_calls_on(body: &gossamer_mir::Body, name: &str, local: Local) -> usize {
    body.blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter(|stmt| {
            matches!(
                &stmt.kind,
                StatementKind::Assign {
                    rvalue: Rvalue::CallIntrinsic { name: n, args },
                    ..
                } if *n == name
                    && matches!(
                        args.first(),
                        Some(Operand::Copy(p)) if p.projection.is_empty() && p.local == local
                    )
            )
        })
        .count()
}

/// A by-value tuple is a stack slot whose RC-managed elements are owned
/// per-field: `let (t, n) = make()` (where `make -> (String, i64)`) must
/// retain the extracted `String` at the field-0 copy - the binding holds
/// a fresh reference - and release it at end of life. Without it every
/// round of a tuple-returning allocator leaks one element.
#[test]
fn drop_pass_retains_and_releases_tuple_extracted_rc_field() {
    let source = r#"
fn make() -> (String, i64) {
    let s = "node"
    (s, 1)
}

fn use_it() -> i64 {
    let (t, n) = make()
    n + t.byte_at(0)
}
"#;
    let (bodies, _) = build(source);
    let body = bodies.iter().find(|b| b.name == "use_it").expect("body");
    let dest = field0_extract_dest(body).expect("field-0 tuple extract");

    assert!(
        rc_calls_on(body, "gos_rt_rc_retain", dest) > 0,
        "extracted tuple String must be retained at the field copy"
    );
    assert!(
        rc_calls_on(body, "gos_rt_rc_release", dest) > 0,
        "extracted tuple String must be released at end of life"
    );
}

/// A `Result` / `Option` tuple element is a 2-word by-value, never an RC
/// pointer, so per-field accounting must skip it: destructuring
/// `(Result<String, _>, i64)` emits no retain on the field-0 extract.
/// Treating the packed value as a pointer would corrupt the heap.
#[test]
fn drop_pass_skips_result_tuple_element() {
    let source = r#"
use std::errors

fn make() -> (Result<String, errors::Error>, i64) {
    (Ok("node"), 1)
}

fn use_it() -> i64 {
    let (_r, n) = make()
    n
}
"#;
    let (bodies, _) = build(source);
    let body = bodies.iter().find(|b| b.name == "use_it").expect("body");
    if let Some(dest) = field0_extract_dest(body) {
        assert_eq!(
            rc_calls_on(body, "gos_rt_rc_retain", dest),
            0,
            "a Result tuple element is by-value and must not be RC-retained"
        );
    }
}

fn optimised(source: &str) -> gossamer_mir::Body {
    let (mut bodies, tcx) = build(source);
    let mut body = bodies.remove(0);
    optimise(&mut body, &tcx);
    body
}

fn has_binary_op(body: &gossamer_mir::Body) -> bool {
    body.blocks.iter().flat_map(|b| b.stmts.iter()).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::BinaryOp { .. },
                ..
            }
        )
    })
}

#[test]
fn identity_fold_add_zero_either_side() {
    let body = optimised("fn f(x: i64) -> i64 { x + 0 }\n");
    assert!(!has_binary_op(&body), "x + 0 must fold to x");
    let body = optimised("fn f(x: i64) -> i64 { 0 + x }\n");
    assert!(!has_binary_op(&body), "0 + x must fold to x");
}

#[test]
fn identity_fold_sub_zero_rhs_only() {
    let body = optimised("fn f(x: i64) -> i64 { x - 0 }\n");
    assert!(!has_binary_op(&body), "x - 0 must fold to x");
    let body = optimised("fn f(x: i64) -> i64 { 0 - x }\n");
    assert!(has_binary_op(&body), "0 - x is a negation, not an identity");
}

#[test]
fn identity_fold_mul_one_either_side() {
    let body = optimised("fn f(x: i64) -> i64 { x * 1 }\n");
    assert!(!has_binary_op(&body), "x * 1 must fold to x");
    let body = optimised("fn f(x: i64) -> i64 { 1 * x }\n");
    assert!(!has_binary_op(&body), "1 * x must fold to x");
}

#[test]
fn absorbing_fold_mul_zero_to_const_zero() {
    let body = optimised("fn f(x: i64) -> i64 { x * 0 }\n");
    assert!(!has_binary_op(&body), "x * 0 must fold to 0");
    assert_eq!(
        const_value_of(&body, Local::RETURN),
        Some(ConstValue::Int(0)),
        "return slot must hold the absorbed 0"
    );
}

#[test]
fn identity_fold_div_rem_one_rhs() {
    let body = optimised("fn f(x: i64) -> i64 { x / 1 }\n");
    assert!(!has_binary_op(&body), "x / 1 must fold to x");
    let body = optimised("fn f(x: i64) -> i64 { x % 1 }\n");
    assert!(!has_binary_op(&body), "x % 1 must fold to 0");
    assert_eq!(
        const_value_of(&body, Local::RETURN),
        Some(ConstValue::Int(0))
    );
}

#[test]
fn no_fold_for_nonconstant_divisor() {
    let body = optimised("fn f(x: i64) -> i64 { 0 / x }\n");
    assert!(
        has_binary_op(&body),
        "0 / x must keep its runtime division (x may be zero)"
    );
    let body = optimised("fn f(x: i64) -> i64 { 0 % x }\n");
    assert!(
        has_binary_op(&body),
        "0 % x must keep its runtime remainder (x may be zero)"
    );
}

#[test]
fn identity_fold_bitwise_zero() {
    let body = optimised("fn f(x: i64) -> i64 { x | 0 }\n");
    assert!(!has_binary_op(&body), "x | 0 must fold to x");
    let body = optimised("fn f(x: i64) -> i64 { x ^ 0 }\n");
    assert!(!has_binary_op(&body), "x ^ 0 must fold to x");
    let body = optimised("fn f(x: i64) -> i64 { x & 0 }\n");
    assert!(!has_binary_op(&body), "x & 0 must fold to 0");
    assert_eq!(
        const_value_of(&body, Local::RETURN),
        Some(ConstValue::Int(0))
    );
}

#[test]
fn identity_fold_shift_zero_amount() {
    let body = optimised("fn f(x: i64) -> i64 { x << 0 }\n");
    assert!(!has_binary_op(&body), "x << 0 must fold to x");
    let body = optimised("fn f(x: i64) -> i64 { x >> 0 }\n");
    assert!(!has_binary_op(&body), "x >> 0 must fold to x");
}

#[test]
fn identity_fold_bool_operands() {
    let body = optimised("fn f(b: bool) -> bool { b & true }\n");
    assert!(!has_binary_op(&body), "b & true must fold to b");
    let body = optimised("fn f(b: bool) -> bool { b | false }\n");
    assert!(!has_binary_op(&body), "b | false must fold to b");
    let body = optimised("fn f(b: bool) -> bool { b ^ false }\n");
    assert!(!has_binary_op(&body), "b ^ false must fold to b");
    let body = optimised("fn f(b: bool) -> bool { b & false }\n");
    assert!(!has_binary_op(&body), "b & false must fold to false");
    assert_eq!(
        const_value_of(&body, Local::RETURN),
        Some(ConstValue::Bool(false))
    );
    let body = optimised("fn f(b: bool) -> bool { b | true }\n");
    assert!(!has_binary_op(&body), "b | true must fold to true");
    assert_eq!(
        const_value_of(&body, Local::RETURN),
        Some(ConstValue::Bool(true))
    );
}

#[test]
fn no_identity_fold_for_floats() {
    let body = optimised("fn f(y: f64) -> f64 { y + 0.0 }\n");
    assert!(
        has_binary_op(&body),
        "y + 0.0 is not an identity under IEEE-754 (-0.0 + 0.0 == +0.0)"
    );
    let body = optimised("fn f(y: f64) -> f64 { y * 1.0 }\n");
    assert!(has_binary_op(&body), "float ops stay unfolded");
}

#[test]
fn no_identity_fold_for_nonidentity_constant() {
    let body = optimised("fn f(x: i64) -> i64 { x + 1 }\n");
    assert!(has_binary_op(&body), "x + 1 must stay a runtime add");
}

// ----------------------------------------------------------------
// Bare-`http::Response` handler thunk synthesis.
//
// The HTTP runtime invokes every registered handler through the
// packed-Result i128 C-ABI, so a serve method (or router fn) that
// declares a bare `http::Response` return gets a synthesized
// `::__ok_wrap` body that calls the real handler and packs its
// return into `Ok` via `gos_rt_result_new`. The registration site
// must point `gos_fn_addr` at the thunk.
// ----------------------------------------------------------------

const BARE_SERVE_SOURCE: &str = r#"
use std::http

struct App { }

impl http::Handler for App {
    fn serve(&self, _r: http::Request) -> http::Response {
        http::Response::text(200, "ok")
    }
}

fn main() {
    let _ = http::serve("127.0.0.1:8080", App { })
}
"#;

fn gos_fn_addr_targets(body: &gossamer_mir::Body) -> Vec<String> {
    body.blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter_map(|stmt| match &stmt.kind {
            StatementKind::Assign {
                rvalue:
                    Rvalue::CallIntrinsic {
                        name: "gos_fn_addr",
                        args,
                    },
                ..
            } => match args.first() {
                Some(Operand::Const(ConstValue::Str(s))) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn bare_response_serve_method_synthesizes_ok_wrap_thunk() {
    let (bodies, _) = build(BARE_SERVE_SOURCE);
    let wrap = bodies
        .iter()
        .find(|b| b.name == "App::serve::__ok_wrap")
        .expect("synthesized ::__ok_wrap body for bare-Response serve");
    assert_eq!(wrap.arity, 2, "env thunk forwards (self, request)");
    let calls_serve = wrap.blocks.iter().any(|b| {
        matches!(
            &b.terminator,
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(s)),
                ..
            } if s == "App::serve"
        )
    });
    assert!(calls_serve, "thunk must call the wrapped App::serve");
    let packs_ok = wrap.blocks.iter().flat_map(|b| b.stmts.iter()).any(|stmt| {
        matches!(
            &stmt.kind,
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic {
                    name: "gos_rt_result_new",
                    ..
                },
                ..
            }
        )
    });
    assert!(packs_ok, "thunk must pack the Response into Ok");
}

#[test]
fn response_stream_lowers_to_three_arg_stream_new_call() {
    let source = r#"
use std::http

struct App { }

impl http::Handler for App {
    fn serve(&self, _r: http::Request) -> Result<http::Response, http::Error> {
        match http::stream("GET", "http://127.0.0.1:1/x", "", []) {
            Ok(up) => Ok(http::Response::stream(up.status, up.content_type, up)),
            Err(e) => Err(e),
        }
    }
}

fn main() {
    let _ = http::serve("127.0.0.1:8080", App { })
}
"#;
    let (bodies, _) = build(source);
    let serve = bodies
        .iter()
        .find(|b| b.name == "App::serve")
        .expect("serve body");
    let arg_count = serve.blocks.iter().find_map(|b| match &b.terminator {
        Terminator::Call {
            callee: Operand::Const(ConstValue::Str(s)),
            args,
            ..
        } if s == "gos_rt_http_response_stream_new" => Some(args.len()),
        _ => None,
    });
    assert_eq!(
        arg_count,
        Some(3),
        "Response::stream must lower to the (status, content_type, rs) shim call"
    );
}

#[test]
fn bare_response_serve_registration_dispatches_through_thunk() {
    let (bodies, _) = build(BARE_SERVE_SOURCE);
    let main_body = bodies.iter().find(|b| b.name == "main").expect("main body");
    assert_eq!(
        gos_fn_addr_targets(main_body),
        vec!["App::serve::__ok_wrap".to_string()],
        "http::serve must register the Ok-packing thunk"
    );
}

#[test]
fn result_serve_method_keeps_direct_dispatch() {
    let source = r#"
use std::http

struct App { }

impl http::Handler for App {
    fn serve(&self, _r: http::Request) -> Result<http::Response, http::Error> {
        Ok(http::Response::text(200, "ok"))
    }
}

fn main() {
    let _ = http::serve("127.0.0.1:8080", App { })
}
"#;
    let (bodies, _) = build(source);
    assert!(
        !bodies.iter().any(|b| b.name.ends_with("::__ok_wrap")),
        "Result-returning serve needs no thunk"
    );
    let main_body = bodies.iter().find(|b| b.name == "main").expect("main body");
    assert_eq!(
        gos_fn_addr_targets(main_body),
        vec!["App::serve".to_string()],
        "Result-returning serve dispatches directly"
    );
}

#[test]
fn bare_response_router_fn_registers_ok_wrap_thunk() {
    let source = r#"
use std::http
use std::http::router

fn hello(_r: http::Request) -> http::Response {
    http::Response::text(200, "ok")
}

fn main() {
    let r = router::Router::new()
    r.get("/hello", hello)
    let _ = http::serve("127.0.0.1:8080", r)
}
"#;
    let (bodies, _) = build(source);
    assert!(
        bodies.iter().any(|b| b.name == "hello::__ok_wrap"),
        "bare-Response router fn gets a thunk"
    );
    let main_body = bodies.iter().find(|b| b.name == "main").expect("main body");
    assert!(
        gos_fn_addr_targets(main_body).contains(&"hello::__ok_wrap".to_string()),
        "router registration must point gos_fn_addr at the thunk"
    );
}

/// Lowers `source` through the native pipeline shape: HIR lowering
/// plus the closure-lift pass, mirroring what `gos build` runs before
/// MIR. Needed for assertions about lifted closure bodies.
fn build_with_lift(source: &str) -> (Vec<gossamer_mir::Body>, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    assert!(type_diags.is_empty(), "typecheck: {type_diags:?}");
    let hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let hir = gossamer_hir::lift_closures(hir, &mut tcx);
    let bodies = lower_program(&hir, &mut tcx);
    (bodies, tcx)
}

#[test]
fn lifted_map_err_closure_param_keeps_string_type() {
    // The Err payload is a String; the lifted closure body's param
    // local must stay String after the lift pass. Before the checker
    // grew Result-combinator signatures the param reached the lift
    // unresolved and was pinned to i64, so `format!("{e}")` rendered
    // the payload pointer as an integer on the compiled tiers.
    let source = "fn fail() -> Result<i64, String> { Err(\"boom\") }\n\
                  fn main() { let r = fail().map_err(|e| format!(\"w: {e}\"))\n\
                  let _ = r }\n";
    let (bodies, tcx) = build_with_lift(source);
    let lifted = bodies
        .iter()
        .find(|b| b.name.starts_with("__closure_"))
        .expect("lifted closure body");
    assert_eq!(lifted.arity, 1, "non-capturing closure takes one param");
    let param_ty = lifted.locals[1].ty;
    assert!(
        matches!(tcx.kind_of(param_ty), gossamer_types::TyKind::String),
        "lifted map_err closure param must be String, got {:?}",
        tcx.kind_of(param_ty)
    );
}

#[test]
fn lifted_iter_map_closure_param_keeps_string_type() {
    let source = "use std::iter\n\
                  fn main() { let xs: Vec<String> = [\"a\", \"b\"]\n\
                  let ys = iter::map(|s| format!(\"[{s}]\"), xs)\n\
                  let _ = ys }\n";
    let (bodies, tcx) = build_with_lift(source);
    let lifted = bodies
        .iter()
        .find(|b| b.name.starts_with("__closure_"))
        .expect("lifted closure body");
    let param_ty = lifted.locals[1].ty;
    assert!(
        matches!(tcx.kind_of(param_ty), gossamer_types::TyKind::String),
        "lifted iter::map closure param must be String, got {:?}",
        tcx.kind_of(param_ty)
    );
}

#[test]
fn result_map_err_free_call_lowers_to_runtime_shim() {
    // `result::map_err(f, r)` (the piped/free form) must lower to the
    // `gos_rt_result_map_err` shim instead of an undefined
    // `@result::map_err` symbol that fails the native link.
    let source = "use std::result\n\
                  fn fail() -> Result<i64, String> { Err(\"boom\") }\n\
                  fn main() { let r = fail() |> result::map_err(|e| format!(\"p: {e}\"))\n\
                  let _ = r }\n";
    let (bodies, _) = build_with_lift(source);
    let main = bodies.iter().find(|b| b.name == "main").expect("main");
    let calls_shim = main.blocks.iter().any(|b| {
        matches!(
            &b.terminator,
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(sym)),
                ..
            } if sym == "gos_rt_result_map_err"
        )
    });
    assert!(
        calls_shim,
        "expected a gos_rt_result_map_err call terminator in main"
    );
}

// ---------------------------------------------------------------
// http::Response struct literals - must lower to the runtime
// constructor + setter chain on compiled tiers, never to the
// undefined `__struct` symbol (which fails the native build).
// ---------------------------------------------------------------

fn call_names(body: &gossamer_mir::Body) -> Vec<String> {
    body.blocks
        .iter()
        .filter_map(|b| match &b.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(n)),
                ..
            } => Some(n.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn http_response_literal_full_lowers_to_constructor_and_setters() {
    let source = "use std::http\n\
                  fn h() -> http::Response {\n\
                  http::Response { status: 201, body: \"x\", content_type: \"t\",\n\
                  headers: [(\"a\", \"b\"), (\"c\", \"d\")] } }\n";
    let (bodies, _) = build(source);
    let h = bodies.iter().find(|b| b.name == "h").expect("h body");
    let names = call_names(h);
    assert!(
        !names.iter().any(|n| n == "__struct"),
        "literal must not lower to __struct: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "gos_rt_http_response_text_new"),
        "expected text_new constructor: {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n == "gos_rt_http_response_set_content_type"),
        "expected content-type setter: {names:?}"
    );
    let with_header_count = names
        .iter()
        .filter(|n| n.as_str() == "gos_rt_http_response_with_header")
        .count();
    assert_eq!(
        with_header_count, 2,
        "literal header arrays unroll one with_header per pair: {names:?}"
    );
}

#[test]
fn http_response_literal_omitted_fields_use_constructor_defaults() {
    let source = "use std::http\n\
                  fn h() -> http::Response { http::Response { } }\n";
    let (bodies, _) = build(source);
    let h = bodies.iter().find(|b| b.name == "h").expect("h body");
    let names = call_names(h);
    assert!(
        !names.iter().any(|n| n == "__struct"),
        "literal must not lower to __struct: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "gos_rt_http_response_text_new"),
        "expected text_new constructor: {names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|n| n == "gos_rt_http_response_set_content_type"),
        "omitted content_type keeps the text_new default: {names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|n| n == "gos_rt_http_response_with_header"),
        "omitted headers attach nothing: {names:?}"
    );
}

#[test]
fn http_response_literal_dynamic_headers_emit_vec_loop() {
    let source = "use std::http\n\
                  fn h(hs: [(String, String)]) -> http::Response {\n\
                  http::Response { status: 200, body: \"x\", headers: hs } }\n";
    let (bodies, _) = build(source);
    let h = bodies.iter().find(|b| b.name == "h").expect("h body");
    let names = call_names(h);
    assert!(
        !names.iter().any(|n| n == "__struct"),
        "literal must not lower to __struct: {names:?}"
    );
    for expected in [
        "gos_rt_vec_len",
        "gos_rt_vec_get_ptr",
        "gos_rt_http_response_with_header",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "dynamic header arrays loop via {expected}: {names:?}"
        );
    }
    let has_back_edge = h.blocks.iter().enumerate().any(|(i, b)| {
        matches!(&b.terminator, Terminator::Goto { target } if target.0 < u32::try_from(i).unwrap_or(0))
    });
    assert!(
        has_back_edge,
        "expected a loop back-edge over the header vec"
    );
}

#[test]
fn http_response_literal_byte_body_routes_through_set_body_bytes() {
    let source = "use std::http\n\
                  fn h() -> http::Response {\n\
                  http::Response { status: 200, body: [104u8, 105u8] } }\n";
    let (bodies, _) = build(source);
    let h = bodies.iter().find(|b| b.name == "h").expect("h body");
    let names = call_names(h);
    assert!(
        names
            .iter()
            .any(|n| n == "gos_rt_http_response_set_body_bytes"),
        "byte-array bodies route through set_body_bytes: {names:?}"
    );
}

#[test]
fn user_defined_response_struct_still_lowers_as_aggregate() {
    let source = "struct Response { status: i64 }\n\
                  fn h() -> Response { Response { status: 7 } }\n";
    let (bodies, _) = build(source);
    let h = bodies.iter().find(|b| b.name == "h").expect("h body");
    let names = call_names(h);
    assert!(
        !names.iter().any(|n| n == "gos_rt_http_response_text_new"),
        "a user Response struct must keep the aggregate lowering: {names:?}"
    );
    let has_aggregate = h.blocks.iter().flat_map(|b| b.stmts.iter()).any(|s| {
        matches!(
            &s.kind,
            StatementKind::Assign {
                rvalue: Rvalue::Aggregate { .. },
                ..
            }
        )
    });
    assert!(
        has_aggregate,
        "expected an Aggregate assign for the user struct"
    );
}

// ---------------------------------------------------------------
// Task 22 - per-name combinator matrix: every closure-taking std
// combinator the checker has a signature row for must lower its
// free data-last call to a concrete gos_rt_* shim, never to an
// undefined `@module::name` symbol.
// ---------------------------------------------------------------

/// (label, source, expected shim) rows for the per-name matrix.
const COMBINATOR_MATRIX: &[(&str, &str, &str)] = &[
    (
        "result::and_then",
        "use std::{errors, result}\nfn main() { let r: Result<i64, errors::Error> = Ok(2)\n\
             let m = r |> result::and_then(|x: i64| if x > 0 { Ok(x) } else { Err(errors::new(\"n\")) })\nlet _ = m }",
        "gos_rt_result_and_then",
    ),
    (
        "result::or_else",
        "use std::{errors, result}\nfn main() { let r: Result<i64, errors::Error> = Err(errors::new(\"b\"))\n\
             let m = r |> result::or_else(|_e| Ok(7))\nlet _ = m }",
        "gos_rt_result_or_else",
    ),
    (
        "result::ok",
        "use std::{errors, result}\nfn main() { let r: Result<i64, errors::Error> = Ok(4)\n\
             let m = r |> result::ok\nlet _ = m }",
        "gos_rt_result_to_opt_ok",
    ),
    (
        "result::err",
        "use std::{errors, result}\nfn main() { let r: Result<i64, errors::Error> = Ok(4)\n\
             let m = r |> result::err\nlet _ = m }",
        "gos_rt_result_to_opt_err",
    ),
    (
        "result::is_ok",
        "use std::{errors, result}\nfn main() { let r: Result<i64, errors::Error> = Ok(4)\n\
             let m = r |> result::is_ok\nlet _ = m }",
        "gos_rt_result_is_ok",
    ),
    (
        "result::is_err",
        "use std::{errors, result}\nfn main() { let r: Result<i64, errors::Error> = Ok(4)\n\
             let m = r |> result::is_err\nlet _ = m }",
        "gos_rt_result_is_err",
    ),
    (
        "option::and_then",
        "use std::option\nfn main() { let o: Option<i64> = Some(3)\n\
             let m = o |> option::and_then(|x: i64| if x > 2 { Some(x) } else { None })\nlet _ = m }",
        "gos_rt_option_and_then",
    ),
    (
        "option::filter",
        "use std::option\nfn main() { let o: Option<i64> = Some(3)\n\
             let m = o |> option::filter(|x: i64| x > 2)\nlet _ = m }",
        "gos_rt_option_filter",
    ),
    (
        "option::or",
        "use std::option\nfn main() { let o: Option<i64> = None\n\
             let m = o |> option::or(Some(8))\nlet _ = m }",
        "gos_rt_option_or",
    ),
    (
        "option::or_else",
        "use std::option\nfn main() { let o: Option<i64> = None\n\
             let m = o |> option::or_else(|| Some(8))\nlet _ = m }",
        "gos_rt_option_or_else",
    ),
    (
        "option::default_with",
        "use std::option\nfn main() { let o: Option<i64> = None\n\
             let v = o |> option::default_with(|| 6)\nlet _ = v }",
        "gos_rt_option_default_with",
    ),
    (
        "option::zip",
        "use std::option\nfn main() { let a: Option<i64> = Some(1)\n\
             let b: Option<i64> = Some(2)\nlet m = a |> option::zip(b)\nlet _ = m }",
        "gos_rt_option_zip",
    ),
    (
        "option::flatten",
        "use std::option\nfn main() { let o: Option<Option<i64>> = Some(Some(4))\n\
             let m = o |> option::flatten\nlet _ = m }",
        "gos_rt_option_flatten",
    ),
    (
        "option::iter",
        "use std::option\nfn main() { let o: Option<i64> = Some(9)\n\
             let xs = o |> option::iter\nlet _ = xs }",
        "gos_rt_option_iter",
    ),
    (
        "option::is_some",
        "use std::option\nfn main() { let o: Option<i64> = Some(9)\n\
             let v = o |> option::is_some\nlet _ = v }",
        "gos_rt_option_is_some",
    ),
    (
        "iter::filter_map",
        "use std::iter\nfn main() { let xs = [1, 2] |> iter::filter_map(|x: i64| if x > 1 { Some(x) } else { None })\nlet _ = xs }",
        "gos_rt_iter_filter_map_i64",
    ),
    (
        "iter::flat_map (array literal)",
        "use std::iter\nfn main() { let xs = [1, 2] |> iter::flat_map(|x: i64| [x, x * 10])\nlet _ = xs }",
        "gos_rt_iter_flat_map_arr_i64",
    ),
    (
        "iter::reduce",
        "use std::iter\nfn main() { let v = [1, 2] |> iter::reduce(|a: i64, b: i64| a + b)\nlet _ = v }",
        "gos_rt_iter_reduce_i64",
    ),
    (
        "iter::scan",
        "use std::iter\nfn main() { let xs = [1, 2] |> iter::scan(0, |a: i64, x: i64| a + x)\nlet _ = xs }",
        "gos_rt_iter_scan_i64",
    ),
    (
        "iter::product_by",
        "use std::iter\nfn main() { let v = [1, 2] |> iter::product_by(|x: i64| x + 1)\nlet _ = v }",
        "gos_rt_iter_product_by_i64",
    ),
    (
        "iter::position",
        "use std::iter\nfn main() { let v = [5, 6] |> iter::position(|x: i64| x == 6)\nlet _ = v }",
        "gos_rt_iter_position_i64",
    ),
    (
        "iter::find_map",
        "use std::iter\nfn main() { let v = [1, 2] |> iter::find_map(|x: i64| if x > 1 { Some(x) } else { None })\nlet _ = v }",
        "gos_rt_iter_find_map_i64",
    ),
    (
        "iter::take_while",
        "use std::iter\nfn main() { let xs = [1, 9] |> iter::take_while(|x: i64| x < 5)\nlet _ = xs }",
        "gos_rt_iter_take_while_i64",
    ),
    (
        "iter::skip_while",
        "use std::iter\nfn main() { let xs = [1, 9] |> iter::skip_while(|x: i64| x < 5)\nlet _ = xs }",
        "gos_rt_iter_skip_while_i64",
    ),
    (
        "iter::partition",
        "use std::iter\nfn main() { let (a, b) = [1, 2] |> iter::partition(|x: i64| x % 2 == 0)\nlet _ = a\nlet _ = b }",
        "gos_rt_iter_partition_i64",
    ),
    (
        "iter::sort_by",
        "use std::iter\nfn main() { let xs = [3, 1] |> iter::sort_by(|a: i64, b: i64| a - b)\nlet _ = xs }",
        "gos_rt_iter_sorted_by_i64",
    ),
    (
        "iter::sort_by_key",
        "use std::iter\nfn main() { let xs = [3, 1] |> iter::sort_by_key(|x: i64| 0 - x)\nlet _ = xs }",
        "gos_rt_iter_sorted_by_key_i64",
    ),
    (
        "iter::min_by",
        "use std::iter\nfn main() { let v = [3, 1] |> iter::min_by(|a: i64, b: i64| a - b)\nlet _ = v }",
        "gos_rt_iter_min_by_i64",
    ),
    (
        "iter::max_by",
        "use std::iter\nfn main() { let v = [3, 1] |> iter::max_by(|a: i64, b: i64| a - b)\nlet _ = v }",
        "gos_rt_iter_max_by_i64",
    ),
    (
        "iter::min_by_key",
        "use std::iter\nfn main() { let v = [3, 1] |> iter::min_by_key(|x: i64| 0 - x)\nlet _ = v }",
        "gos_rt_iter_min_by_key_i64",
    ),
    (
        "iter::max_by_key",
        "use std::iter\nfn main() { let v = [3, 1] |> iter::max_by_key(|x: i64| 0 - x)\nlet _ = v }",
        "gos_rt_iter_max_by_key_i64",
    ),
    (
        "iter::group_by",
        "use std::iter\nfn main() { let m = [1, 2] |> iter::group_by(|x: i64| x % 2)\nlet _ = m }",
        "gos_rt_iter_group_by_i64",
    ),
    (
        "iter::count_by",
        "use std::iter\nfn main() { let m = [1, 2] |> iter::count_by(|x: i64| x % 2)\nlet _ = m }",
        "gos_rt_iter_count_by_i64",
    ),
];

#[test]
fn combinator_free_calls_lower_to_runtime_shims() {
    for (label, source, shim) in COMBINATOR_MATRIX {
        let (bodies, _) = build_with_lift(source);
        let main = bodies
            .iter()
            .find(|b| b.name == "main")
            .unwrap_or_else(|| panic!("{label}: missing main body"));
        let names = call_names(main);
        assert!(
            names.iter().any(|n| n == shim),
            "{label}: expected `{shim}` call, got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("::")),
            "{label}: undefined high-level callee leaked into MIR: {names:?}"
        );
    }
}

// ---------------------------------------------------------------
// Task 22 - std fns as values (eta-expansion): a tabled std fn in
// a callable slot must resolve to its runtime symbol; the source
// path must not survive into MIR (it has no native symbol).
// ---------------------------------------------------------------

fn const_strings(body: &gossamer_mir::Body) -> Vec<String> {
    let mut out = Vec::new();
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { rvalue, .. } = &stmt.kind {
                match rvalue {
                    Rvalue::Use(Operand::Const(ConstValue::Str(s))) => out.push(s.clone()),
                    Rvalue::CallIntrinsic { args, .. } => {
                        for arg in args {
                            if let Operand::Const(ConstValue::Str(s)) = arg {
                                out.push(s.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

#[test]
fn std_fn_value_map_err_resolves_to_runtime_symbol() {
    let source = "use std::errors\n\
                  fn main() { let r: Result<i64, String> = Err(\"boom\")\n\
                  let m = r.map_err(errors::new)\nlet _ = m }";
    let (bodies, _) = build_with_lift(source);
    let main = bodies.iter().find(|b| b.name == "main").expect("main");
    let strings = const_strings(main);
    assert!(
        strings.iter().any(|s| s == "gos_rt_error_new"),
        "expected the runtime symbol in MIR: {strings:?}"
    );
    assert!(
        !strings.iter().any(|s| s == "errors::new"),
        "source path must not leak into MIR: {strings:?}"
    );
}

#[test]
fn std_fn_value_iter_map_resolves_to_runtime_symbol() {
    let source = "use std::{iter, strings}\n\
                  fn main() { let out = [\"ab\"] |> iter::map(strings::to_upper)\nlet _ = out }";
    let (bodies, _) = build_with_lift(source);
    let main = bodies.iter().find(|b| b.name == "main").expect("main");
    let strings = const_strings(main);
    assert!(
        strings.iter().any(|s| s == "gos_rt_str_to_upper"),
        "expected the runtime symbol in MIR: {strings:?}"
    );
    assert!(
        !strings.iter().any(|s| s == "strings::to_upper"),
        "source path must not leak into MIR: {strings:?}"
    );
}
