//! Structural-invariant tests for `gossamer_mir::verify`.
//!
//! Two halves:
//!
//! 1. Positive: every body produced by the lowerer for a corpus
//!    of representative programs passes `verify_body`. This is
//!    the property the production pipeline relies on; if it
//!    drifts the `optimise()` pass starts panicking under
//!    `debug_assertions`.
//! 2. Negative: hand-corrupt a body and assert the matching
//!    `VerifyError` fires. Pins each diagnostic shape so a
//!    refactor that loses an invariant check is a test failure,
//!    not a silent regression.

#![allow(missing_docs)]

use gossamer_hir::lower_source_file;
use gossamer_lex::SourceMap;
use gossamer_mir::verify::{VerifyError, verify_body};
use gossamer_mir::{
    BlockId, Body, Local, Operand, Place, Statement, StatementKind, Terminator, lower_program,
    optimise,
};
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

fn build(source: &str) -> (Vec<Body>, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file("verify.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(diags.is_empty(), "parse: {diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let bodies = lower_program(&hir, &mut tcx);
    (bodies, tcx)
}

#[test]
fn identity_body_passes_verify() {
    let (bodies, _) = build("fn id(x: i64) -> i64 { x }\n");
    for body in &bodies {
        verify_body(body).expect("identity body must verify");
    }
}

#[test]
fn binary_op_body_passes_verify() {
    let (bodies, _) = build("fn add(a: i64, b: i64) -> i64 { a + b }\n");
    for body in &bodies {
        verify_body(body).expect("binary-op body must verify");
    }
}

#[test]
fn match_body_passes_verify() {
    let source = r"
fn pick(b: bool) -> i64 {
    match b { true => 1, false => 0 }
}
";
    let (bodies, _) = build(source);
    for body in &bodies {
        verify_body(body).expect("match body must verify");
    }
}

#[test]
fn loop_body_passes_verify() {
    let source = r"
fn count(n: i64) -> i64 {
    let mut i = 0
    while i < n { i = i + 1 }
    i
}
";
    let (bodies, _) = build(source);
    for body in &bodies {
        verify_body(body).expect("loop body must verify");
    }
}

#[test]
fn optimise_preserves_verify_invariants() {
    // Several programs exercising arms of `optimise`. After
    // each pass `optimise` runs `debug_verify_body` itself; this
    // explicit second check pins the invariant at the
    // post-optimise resting state.
    let sources = [
        "fn id(x: i64) -> i64 { x }\n",
        "fn add(a: i64, b: i64) -> i64 { a + b }\n",
        "fn const_branch() -> i64 { if true { 1 } else { 0 } }\n",
        "fn loop_acc(n: i64) -> i64 { let mut s = 0; let mut i = 0; while i < n { s = s + i; i = i + 1 }; s }\n",
    ];
    for source in sources {
        let (mut bodies, tcx) = build(source);
        for body in &mut bodies {
            optimise(body, &tcx);
            verify_body(body).expect("optimise must preserve verify invariants");
        }
    }
}

// ----------------------------------------------------------------
// Negative cases — hand-corrupted bodies.
// ----------------------------------------------------------------

#[test]
fn block_id_mismatch_is_detected() {
    let (mut bodies, _) = build("fn id(x: i64) -> i64 { x }\n");
    let body = &mut bodies[0];
    // Stamp a wrong stored id on the entry block.
    body.blocks[0].id = BlockId(42);
    let errors = verify_body(body).expect_err("mismatched block id must fail");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, VerifyError::BlockIdMismatch { stored, .. } if stored.0 == 42)),
        "expected BlockIdMismatch in {errors:?}",
    );
}

#[test]
fn out_of_range_goto_target_is_detected() {
    let source = r"
fn pick(b: bool) -> i64 {
    if b { 1 } else { 0 }
}
";
    let (mut bodies, _) = build(source);
    let body = &mut bodies[0];
    // Locate any Goto and rewrite its target to a block past the
    // end of the CFG.
    let n_blocks = body.blocks.len() as u32;
    let mut rewrote_one = false;
    for block in &mut body.blocks {
        if let Terminator::Goto { target } = &mut block.terminator {
            *target = BlockId(n_blocks + 5);
            rewrote_one = true;
            break;
        }
    }
    assert!(rewrote_one, "test fixture: expected at least one Goto");
    let errors = verify_body(body).expect_err("out-of-range target must fail");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, VerifyError::BlockOutOfRange { .. })),
        "expected BlockOutOfRange in {errors:?}",
    );
}

#[test]
fn out_of_range_local_is_detected() {
    let (mut bodies, _) = build("fn id(x: i64) -> i64 { x }\n");
    let body = &mut bodies[0];
    let n_locals = body.locals.len() as u32;
    // Inject a StorageLive referencing a local past the end.
    body.blocks[0].stmts.insert(
        0,
        Statement {
            kind: StatementKind::StorageLive(Local(n_locals + 7)),
            span: body.span,
        },
    );
    let errors = verify_body(body).expect_err("out-of-range local must fail");
    assert!(
        errors.iter().any(
            |e| matches!(e, VerifyError::LocalOutOfRange { local, .. } if local.0 == n_locals + 7)
        ),
        "expected LocalOutOfRange in {errors:?}",
    );
}

#[test]
fn out_of_range_local_in_place_projection_is_detected() {
    // Index projection uses a Local; corrupt that one.
    let (mut bodies, _) = build("fn id(x: i64) -> i64 { x }\n");
    let body = &mut bodies[0];
    let n_locals = body.locals.len() as u32;
    body.blocks[0].stmts.insert(
        0,
        Statement {
            kind: StatementKind::Assign {
                place: Place::local(Local(0)),
                rvalue: gossamer_mir::Rvalue::Use(Operand::Copy(Place::local(Local(n_locals + 3)))),
            },
            span: body.span,
        },
    );
    let errors = verify_body(body).expect_err("out-of-range copy must fail");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, VerifyError::LocalOutOfRange { .. })),
        "expected LocalOutOfRange in {errors:?}",
    );
}

#[test]
fn empty_blocks_is_detected() {
    let (mut bodies, _) = build("fn id(x: i64) -> i64 { x }\n");
    let body = &mut bodies[0];
    body.blocks.clear();
    let errors = verify_body(body).expect_err("empty blocks must fail");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, VerifyError::EmptyBlocks { .. })),
        "expected EmptyBlocks in {errors:?}",
    );
}
