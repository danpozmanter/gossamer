// End-to-end tests for MIR lowering + optimisation passes.

use gossamer_hir::{lower_source_file, lower_source_file_with_edition};
use gossamer_lex::SourceMap;
use gossamer_mir::{
    BinOp, ConstValue, Local, Operand, Rvalue, StatementKind, Terminator, const_value_of,
    lower_program, optimise,
};
use gossamer_parse::{autoderive::parse_with_autoderive, parse_source_file};
use gossamer_pkg::Edition;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file, typecheck_source_file_with_edition};

fn build(source: &str) -> (Vec<gossamer_mir::Body>, TyCtxt) {
    build_with_edition(source, Edition::E2026)
}

fn build_with_edition(source: &str, edition: Edition) -> (Vec<gossamer_mir::Body>, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_with_autoderive(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, diagnostics) =
        typecheck_source_file_with_edition(&sf, &resolutions, &mut tcx, edition);
    assert!(diagnostics.is_empty(), "typecheck: {diagnostics:?}");
    let hir = lower_source_file_with_edition(&sf, &resolutions, &table, &mut tcx, edition);
    let bodies = lower_program(&hir, &mut tcx);
    (bodies, tcx)
}

fn call_symbol_names(body: &gossamer_mir::Body) -> Vec<String> {
    let mut out = Vec::new();
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, .. },
                ..
            } = &stmt.kind
            {
                out.push((*name).to_string());
            }
        }
        if let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            ..
        } = &block.terminator
        {
            out.push(name.clone());
        }
    }
    out
}

#[test]
fn string_with_capacity_preserves_the_reservation_call() {
    let (bodies, _) = build(
        r#"
fn make() -> String {
    let mut out = String::with_capacity(4096)
    out.push_str("x")
    out
}
"#,
    );
    let body = bodies.iter().find(|body| body.name == "make").expect("body");
    assert!(
        call_symbol_names(body)
            .iter()
            .any(|name| name == "gos_rt_str_with_capacity"),
        "String::with_capacity must reach the native runtime instead of becoming an empty literal"
    );
}

#[test]
fn padded_integer_formatting_avoids_an_intermediate_string() {
    let (bodies, _) = build(
        r#"
fn key(i: i64) -> String {
    format!("key-{:08}", i)
}
"#,
    );
    let body = bodies.iter().find(|body| body.name == "key").expect("body");
    let symbols = call_symbol_names(body);
    assert!(
        symbols.iter().any(|name| name == "gos_rt_concat_pad_i64"),
        "integer padding and concatenation should use one native helper: {symbols:?}"
    );
    assert!(
        !symbols
            .iter()
            .any(|name| matches!(name.as_str(), "gos_rt_fmt_pad" | "gos_rt_fmt_pad_i64")),
        "integer padding must not allocate an intermediate string: {symbols:?}"
    );
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
fn edition_2027_lazy_iterator_pipelines_lower_to_runtime_handles() {
    let source = r"use std::iter

fn main() {
    let collected = iter::range(0, 10)
        |> iter::map(|x| x + 1)
        |> iter::filter(|x| x % 2 == 0)
        |> iter::take(3)
        |> iter::collect
    let chained = iter::chain(iter::range(0, 1), iter::range(1, 2)) |> iter::count
    let zipped = iter::zip(iter::range(0, 2), iter::range(2, 4)) |> iter::collect
    let enumerated = iter::range(3, 5) |> iter::enumerate |> iter::collect
    let found = iter::range(0, 4) |> iter::find(|x| x == 2) |> option::unwrap_or(-1)
    let folded = (2..=4).fold(1, |acc, x| acc * x)
    let borrowed = [1, 2, 3]
    let borrowed_out = borrowed |> iter::take(1) |> iter::collect
    let _ = collected
    let _ = chained
    let _ = zipped
    let _ = enumerated
    let _ = found
    let _ = folded
    let _ = borrowed_out
}

";
    let (bodies, _) = build_with_edition(source, Edition::E2027);
    let main = bodies.iter().find(|body| body.name == "main").expect("main");
    let names = call_symbol_names(main);
    for expected in [
        "gos_rt_lazy_iter_range_i64",
        "gos_rt_lazy_iter_map_i64",
        "gos_rt_lazy_iter_filter_i64",
        "gos_rt_lazy_iter_take_i64",
        "gos_rt_lazy_iter_collect_i64",
        "gos_rt_lazy_iter_chain_i64",
        "gos_rt_lazy_iter_count_i64",
        "gos_rt_lazy_iter_zip_i64",
        "gos_rt_lazy_iter_collect_pair_i64",
        "gos_rt_lazy_iter_enumerate_i64",
        "gos_rt_lazy_iter_find_i64",
        "gos_rt_lazy_iter_fold_i64",
        "gos_rt_lazy_iter_from_vec_i64",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "expected {expected} in MIR calls: {names:?}"
        );
    }
}

#[test]
fn hashset_intersection_iter_snapshots_directly_to_a_vec() {
    let source = r"use std::collections::HashSet

fn main() {
    let mut left: HashSet<i64> = HashSet::new()
    let mut right: HashSet<i64> = HashSet::new()
    left.insert(1)
    right.insert(1)
    let mut total = 0
    for value in left.intersection(right).iter() { total += value }
    let _ = total
}
";
    let (bodies, _) = build(source);
    let main = bodies.iter().find(|body| body.name == "main").expect("main");
    let names = call_symbol_names(main);
    assert!(
        names
            .iter()
            .any(|name| name == "gos_rt_set_intersection_to_vec_i64"),
        "intersection iteration should avoid a temporary set: {names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|name| name == "gos_rt_set_intersection"),
        "the eager intersection helper should not remain: {names:?}"
    );
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
fn counted_hashmap_insert_loop_reserves_proven_upper_bound() {
    let source = r"
use std::collections::HashMap

fn fill(n: i64) -> i64 {
    let mut m: HashMap<i64, i64> = HashMap::new()
    let mut i = 0i64
    while i < n {
        m.insert(i, i)
        i += 1i64
    }
    m.len()
}
";

    let (bodies, _) = build(source);
    let body = bodies
        .iter()
        .find(|body| body.name == "fill")
        .expect("fill body");
    let callees: Vec<_> = body
        .blocks
        .iter()
        .filter_map(|block| match &block.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                ..
            } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let call = body
        .blocks
        .iter()
        .find_map(|block| match &block.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                ..
            } if name == "gos_rt_map_new_with_capacity" => Some(args),
            _ => None,
        })
        .unwrap_or_else(|| panic!("counted map constructor must reserve; calls: {callees:?}"));
    assert_eq!(
        call.len(),
        1,
        "native lowering derives map layout from the destination"
    );
    assert!(
        matches!(call[0], Operand::Copy(_)),
        "capacity is the loop bound"
    );
}

#[test]
fn branch_guarded_heap_style_repeated_vec_reads_are_unchecked() {
    // Heap sift and BFS inner loops commonly validate an index once, then
    // inspect the same slot repeatedly. The MIR proof must carry only through
    // that straight-line access chain, never through a mutation or another
    // branch.
    let source = r"
fn probe(xs: Vec<i64>) -> i64 {
    let idx = 0i64
    let n = xs.len()
    if idx < n {
        let first = xs[idx]
        let second = xs[idx]
        first + second
    } else {
        0i64
    }
}
";
    let (mut bodies, tcx) = build(source);
    for body in &mut bodies {
        optimise(body, &tcx);
    }
    let probe = bodies
        .iter()
        .find(|body| body.name == "probe")
        .expect("probe");
    let unchecked_reads = probe
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                &block.terminator,
                Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(name)),
                    ..
                } if name == "gos_rt_vec_get_i64_unchecked"
            )
        })
        .count();
    let callees: Vec<_> = probe
        .blocks
        .iter()
        .filter_map(|block| match &block.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                ..
            } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        unchecked_reads, 2,
        "a single idx < len guard must cover its straight-line repeated reads; calls: {callees:?}"
    );
}

#[test]
fn counted_hashmap_reservation_rejects_a_skipped_insert_path() {
    let source = r"
use std::collections::HashMap

fn fill(n: i64) -> i64 {
    let mut m: HashMap<i64, i64> = HashMap::new()
    let mut i = 0i64
    while i < n {
        if i % 2i64 == 0i64 {
            m.insert(i, i)
        }
        i += 1i64
    }
    m.len()
}
";
    let (bodies, _) = build(source);
    let body = bodies
        .iter()
        .find(|body| body.name == "fill")
        .expect("fill body");
    assert!(body.blocks.iter().any(|block| {
        matches!(
            &block.terminator,
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                ..
            } if name == "HashMap::new"
        )
    }));
    assert!(!body.blocks.iter().any(|block| {
        matches!(
            &block.terminator,
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                ..
            } if name == "gos_rt_map_new_with_capacity"
        )
    }));
}

#[test]
fn counted_string_hashmap_insert_loop_reserves_with_typed_capacity_backend() {
    // The MIR planner carries only the proven count. The native backend reads
    // this map's destination type and selects string-keyed capacity storage.
    let source = r#"
use std::collections::HashMap

fn fill(n: i64) -> i64 {
    let mut m: HashMap<String, i64> = HashMap::new()
    let mut i = 0i64
    while i < n {
        m.insert(format!("key-{}", i), i)
        i += 1i64
    }
    m.len()
}
"#;
    let (bodies, _) = build(source);
    let body = bodies
        .iter()
        .find(|body| body.name == "fill")
        .expect("fill body");
    assert!(
        body.blocks.iter().any(|block| matches!(
            &block.terminator,
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                ..
            } if name == "gos_rt_map_new_with_capacity"
        )),
        "string map must carry its proven capacity to the typed backend"
    );
}

#[test]
fn container_insert_keeps_balanced_source_and_container_vec_ownership() {
    // An insert must acquire a share for the map and still release the source
    // binding at scope exit. The old post-drop MIR pass removed every source
    // release after an insert, which made this ordinary overwrite/remove shape
    // leak each value. Keeping both operations also makes the map's overwrite
    // and teardown releases exactly balance the inserted share.
    let source = r"
use std::collections::HashMap

fn replace() -> i64 {
    let mut m: HashMap<i64, Vec<i64>> = HashMap::new()
    let value: Vec<i64> = [1i64, 2i64]
    m.insert(1, value)
    m.insert(1, [3i64, 4i64])
    m.remove(1)
    0i64
}

";
    let (bodies, _) = build(source);
    let body = bodies
        .iter()
        .find(|body| body.name == "replace")
        .expect("replace body");
    let inserted_source = body
        .blocks
        .iter()
        .find_map(|block| match &block.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                ..
            } if name == "gos_rt_map_insert_i64_i64" => args.get(2).and_then(|arg| match arg {
                Operand::Copy(place) if place.projection.is_empty() => Some(place.local),
                _ => None,
            }),
            _ => None,
        })
        .expect("typed Vec-map insertion of the named source");

    let releases = body
        .blocks
        .iter()
        .flat_map(|block| &block.stmts)
        .filter(|stmt| {
            matches!(
                &stmt.kind,
                StatementKind::Assign {
                    rvalue: Rvalue::CallIntrinsic { name, args },
                    ..
                } if *name == "gos_rt_vec_free"
                    && matches!(args.first(), Some(Operand::Copy(place))
                        if place.projection.is_empty() && place.local == inserted_source)
            )
        })
        .count();
    assert!(
        releases >= 1,
        "the source share must be released after insertion; body: {body:#?}"
    );

    let retains = body
        .blocks
        .iter()
        .flat_map(|block| &block.stmts)
        .filter(|stmt| {
            matches!(
                &stmt.kind,
                StatementKind::Assign {
                    rvalue: Rvalue::CallIntrinsic { name, args },
                    ..
                } if *name == "gos_rt_vec_retain"
                    && matches!(args.first(), Some(Operand::Copy(place))
                        if place.projection.is_empty() && place.local == inserted_source)
            )
        })
        .count();
    assert!(
        retains >= 1,
        "the map must receive an explicit retained share; body: {body:#?}"
    );
}

#[test]
fn container_or_insert_retains_map_vec_share_and_returns_a_borrow() {
    // `or_insert` consumes its default only on an absent key. The lowering
    // must still mint the map's Vec share before the call; its result is an
    // interior borrow and is intentionally not an owning call destination.
    let source = r#"
use std::collections::HashMap

fn insert_default() -> i64 {
    let mut m: HashMap<String, Vec<i64>> = HashMap::new()
    let stored = m.or_insert("key", [1i64, 2i64])
    stored.len()
}
"#;
    let (bodies, _) = build(source);
    let body = bodies
        .iter()
        .find(|body| body.name == "insert_default")
        .expect("insert_default body");
    let default = body
        .blocks
        .iter()
        .find_map(|block| match &block.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                ..
            } if name == "gos_rt_map_or_insert_str_i64" => args.get(2).and_then(|arg| match arg {
                Operand::Copy(place) if place.projection.is_empty() => Some(place.local),
                _ => None,
            }),
            _ => None,
        })
        .expect("or_insert default Vec argument");
    assert!(
        body.blocks
            .iter()
            .flat_map(|block| &block.stmts)
            .any(|stmt| {
                matches!(
                    &stmt.kind,
                    StatementKind::Assign {
                        rvalue: Rvalue::CallIntrinsic { name, args },
                        ..
                    } if *name == "gos_rt_vec_retain"
                        && matches!(args.first(), Some(Operand::Copy(place))
                            if place.projection.is_empty() && place.local == default)
                )
            }),
        "or_insert must retain the map's Vec share; body: {body:#?}"
    );
}

#[test]
fn nested_vec_push_retains_inner_vec_once_for_container_share() {
    // `outer.push(inner)` needs one retained share for the outer Vec's element.
    // The local `inner` binding keeps its original share and is freed at scope
    // exit. A second compiler-inserted retain leaves the inner Vec alive after
    // both frees, leaking nested Vec<String> stress cases.
    let source = r#"
fn main() {
    let mut outer: [[String]] = []
    let mut inner: [String] = []
    inner.push("value".to_string())
    outer.push(inner)
    println(outer[0][0])
}
"#;
    let (bodies, tcx) = build(source);
    let body = bodies
        .iter()
        .find(|body| body.name == "main")
        .expect("main");
    let pushed_inner = body
        .blocks
        .iter()
        .find_map(|block| match &block.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                ..
            } if name == "gos_rt_vec_push" => args.get(1).and_then(|arg| match arg {
                Operand::Copy(place)
                    if place.projection.is_empty()
                        && matches!(
                            tcx.kind_of(body.locals[place.local.0 as usize].ty),
                            gossamer_types::TyKind::Vec(_) | gossamer_types::TyKind::Slice(_)
                        ) =>
                {
                    Some(place.local)
                }
                _ => None,
            }),
            _ => None,
        })
        .expect("nested Vec push argument");

    let retains = body
        .blocks
        .iter()
        .flat_map(|block| &block.stmts)
        .filter(|stmt| {
            matches!(
                &stmt.kind,
                StatementKind::Assign {
                    rvalue: Rvalue::CallIntrinsic { name, args },
                    ..
                } if *name == "gos_rt_vec_retain"
                    && matches!(args.first(), Some(Operand::Copy(place))
                        if place.projection.is_empty() && place.local == pushed_inner)
            )
        })
        .count();
    assert_eq!(
        retains, 1,
        "nested Vec push must mint exactly one container share; body: {body:#?}"
    );
}

#[test]
fn map_insert_registers_structural_children_for_aggregate_values() {
    let source = r#"
use std::collections::HashMap

struct Item { name: String, tags: [String], n: i64 }

fn insert_item() {
    let mut m: HashMap<i64, Item> = HashMap::new()
    m.insert(1i64, Item { name: "item", tags: [], n: 1i64 })
}
"#;
    let (bodies, tcx) = build(source);
    let body = bodies
        .iter()
        .find(|body| body.name == "insert_item")
        .expect("insert_item body");
    let value_ty = body
        .locals
        .iter()
        .find_map(|local| match tcx.kind_of(local.ty) {
            gossamer_types::TyKind::HashMap { value, .. } => Some(*value),
            _ => None,
        })
        .expect("HashMap value type");
    let symbol = format!("gos_rc_meta_boxaggr_{}", value_ty.as_u32());
    let meta = tcx
        .rc_meta(&symbol)
        .expect("aggregate map values need structural copy metadata");
    assert_eq!(meta[0], gossamer_abi::rc::RC_KIND_STRUCT);
    assert!(
        meta[4..].contains(&0),
        "String child word missing: {meta:?}"
    );
    assert!(
        meta[4..].contains(
            &((gossamer_abi::rc::RC_CHILD_VEC << gossamer_abi::rc::RC_CHILD_KIND_SHIFT) | 1)
        ),
        "Vec child word missing: {meta:?}"
    );
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
fn runtime_array_repeat_builds_directly_in_let_binding() {
    let source = r"fn make(n: i64) -> i64 {
    let mut xs = [0i64; n]
    xs[0] = 7
    xs.len()
}
";
    let (bodies, _) = build(source);
    let body = &bodies[0];
    let names = call_symbol_names(body);
    assert!(
        names
            .iter()
            .any(|name| name == "gos_rt_vec_repeat_primitive"),
        "primitive runtime-sized repeat must construct initialized storage directly"
    );
    assert!(
        !names.iter().any(|name| name == "gos_rt_vec_push"),
        "primitive runtime-sized repeat must not execute a checked push loop"
    );
    assert!(
        !names.iter().any(|name| name == "gos_rt_vec_clone"),
        "fresh repeat storage must move directly into the binding"
    );
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
    let p = Pair { a: 3i64, b: 7i64 }
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
