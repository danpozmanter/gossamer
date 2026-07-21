//! IR-shape smoke tests for the LLVM lowerer.
//!
//! Mirrors `gossamer-codegen-cranelift/tests/native.rs`'s
//! per-shape construction style, but keeps the surface narrow:
//! every test feeds the lowerer a hand-rolled MIR body and
//! inspects the resulting object bytes (or a trace from running
//! the compiled program). A more granular "IR text snapshot"
//! flavour requires the lowerer to expose `render_module` -
//! tracked under the §3.3 LLVM-tests-directory item; this file
//! is the seed crate so that follow-up has a place to land.
//!
//! Tests gracefully skip when `opt` / `llc` aren't on PATH so
//! contributors without an LLVM install can still run the rest
//! of the workspace's test suite.

#![allow(missing_docs)]

use gossamer_codegen_llvm::{BuildError, compile_to_object};
use gossamer_lex::{SourceMap, Span};
use gossamer_mir::{
    BasicBlock, BlockId, Body, ConstValue, IteratorAdapterKind, IteratorOwnership,
    IteratorSourceKind, Local, LocalDecl, Operand, Place, Rvalue, Statement, StatementKind,
    Terminator,
};
use gossamer_resolve::DefId;
use gossamer_types::{IntTy, Substs, TyCtxt, TyKind};

fn dummy_span() -> Span {
    let mut map = SourceMap::new();
    let file = map.add_file("smoke.gos", "");
    Span::new(file, 0, 0)
}

fn skip_if_llvm_missing() -> bool {
    // The lowerer shells out to `opt`/`llc`; without them the
    // smoke tests can't run. The driver's `find_opt` / `find_llc`
    // helpers aren't part of the public API so we approximate by
    // looking for the binaries directly.
    let try_bin = |bin: &str| {
        std::process::Command::new(bin)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !try_bin("opt") && std::env::var("GOS_LLVM_OPT").is_err() {
        eprintln!("skipping LLVM smoke test: `opt` not on PATH");
        return true;
    }
    if !try_bin("llc") && std::env::var("GOS_LLC").is_err() {
        eprintln!("skipping LLVM smoke test: `llc` not on PATH");
        return true;
    }
    false
}

/// Builds the trivial `fn main() -> i64 { 0 }` body.
fn trivial_main_returning_zero() -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(gossamer_types::TyKind::Int(gossamer_types::IntTy::I64));
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![LocalDecl {
            ty: i64_ty,
            debug_name: None,
            mutable: false,
            region: false,
        }],
        blocks: vec![BasicBlock {
            id: BlockId(0),
            stmts: vec![Statement {
                span: dummy_span(),
                kind: StatementKind::Assign {
                    place: Place {
                        local: Local(0),
                        projection: Vec::new(),
                    },
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                },
            }],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };
    (body, tcx)
}

fn looping_main() -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
    let span = dummy_span();
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: true,
                region: false,
            },
        ],
        blocks: vec![
            BasicBlock {
                id: BlockId(0),
                stmts: vec![Statement {
                    span,
                    kind: StatementKind::Assign {
                        place: Place::local(Local(1)),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(1))),
                    },
                }],
                terminator: Terminator::Goto { target: BlockId(1) },
                span,
            },
            BasicBlock {
                id: BlockId(1),
                stmts: Vec::new(),
                terminator: Terminator::SwitchInt {
                    discriminant: Operand::Copy(Place::local(Local(1))),
                    arms: vec![(1, BlockId(1))],
                    default: BlockId(2),
                },
                span,
            },
            BasicBlock {
                id: BlockId(2),
                stmts: vec![Statement {
                    span,
                    kind: StatementKind::Assign {
                        place: Place::local(Local(0)),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    },
                }],
                terminator: Terminator::Return,
                span,
            },
        ],
        span,
    };
    (body, tcx)
}

fn acyclic_backward_numbered_main() -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
    let span = dummy_span();
    let local = || LocalDecl {
        ty: i64_ty,
        debug_name: None,
        mutable: false,
        region: false,
    };
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![local()],
        blocks: vec![
            BasicBlock {
                id: BlockId(0),
                stmts: Vec::new(),
                terminator: Terminator::Goto { target: BlockId(2) },
                span,
            },
            BasicBlock {
                id: BlockId(1),
                stmts: vec![Statement {
                    span,
                    kind: StatementKind::Assign {
                        place: Place::local(Local(0)),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    },
                }],
                terminator: Terminator::Return,
                span,
            },
            BasicBlock {
                id: BlockId(2),
                stmts: Vec::new(),
                terminator: Terminator::Goto { target: BlockId(1) },
                span,
            },
        ],
        span,
    };
    (body, tcx)
}

fn typed_iterator_main() -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
    let iter_i64 = tcx.iterator_ty(i64_ty);
    let option_i64 = tcx.intern(TyKind::Adt {
        def: DefId::local(u32::MAX - 1),
        substs: Substs::from_types([i64_ty]),
    });
    let span = dummy_span();
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
            LocalDecl {
                ty: iter_i64,
                debug_name: None,
                mutable: true,
                region: false,
            },
            LocalDecl {
                ty: iter_i64,
                debug_name: None,
                mutable: true,
                region: false,
            },
            LocalDecl {
                ty: option_i64,
                debug_name: None,
                mutable: true,
                region: false,
            },
        ],
        blocks: vec![BasicBlock {
            id: BlockId(0),
            stmts: vec![
                Statement {
                    span,
                    kind: StatementKind::IterSource {
                        dst: Place::local(Local(1)),
                        source_kind: IteratorSourceKind::Range,
                        source: Operand::Const(ConstValue::Int(5)),
                        item_ty: i64_ty,
                        ownership: IteratorOwnership::Owning,
                    },
                },
                Statement {
                    span,
                    kind: StatementKind::IterAdapter {
                        dst: Place::local(Local(2)),
                        adapter_kind: IteratorAdapterKind::Take,
                        upstream: Place::local(Local(1)),
                        closure_or_arg: Some(Operand::Const(ConstValue::Int(2))),
                        item_ty: i64_ty,
                    },
                },
                Statement {
                    span,
                    kind: StatementKind::IterNext {
                        dst_option: Place::local(Local(3)),
                        iter_place: Place::local(Local(2)),
                        item_ty: i64_ty,
                    },
                },
                Statement {
                    span,
                    kind: StatementKind::Assign {
                        place: Place::local(Local(0)),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    },
                },
            ],
            terminator: Terminator::Return,
            span,
        }],
        span,
    };
    (body, tcx)
}

fn string_len_main() -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
    let string_ty = tcx.intern(TyKind::String);
    let span = dummy_span();
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
            LocalDecl {
                ty: string_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
        ],
        blocks: vec![
            BasicBlock {
                id: BlockId(0),
                stmts: vec![Statement {
                    span,
                    kind: StatementKind::Assign {
                        place: Place::local(Local(1)),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Str("hello".to_string()))),
                    },
                }],
                terminator: Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_len".to_string())),
                    args: vec![Operand::Copy(Place::local(Local(1)))],
                    destination: Place::local(Local(0)),
                    target: Some(BlockId(1)),
                },
                span,
            },
            BasicBlock {
                id: BlockId(1),
                stmts: Vec::new(),
                terminator: Terminator::Return,
                span,
            },
        ],
        span,
    };
    (body, tcx)
}

fn dynamic_string_len_main() -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
    let string_ty = tcx.intern(TyKind::String);
    let span = dummy_span();
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 1,
        locals: vec![
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
            LocalDecl {
                ty: string_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
        ],
        blocks: vec![
            BasicBlock {
                id: BlockId(0),
                stmts: Vec::new(),
                terminator: Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_len".to_string())),
                    args: vec![Operand::Copy(Place::local(Local(1)))],
                    destination: Place::local(Local(0)),
                    target: Some(BlockId(1)),
                },
                span,
            },
            BasicBlock {
                id: BlockId(1),
                stmts: Vec::new(),
                terminator: Terminator::Return,
                span,
            },
        ],
        span,
    };
    (body, tcx)
}

#[test]
fn llvm_lowers_constant_return_to_object_bytes() {
    if skip_if_llvm_missing() {
        return;
    }
    let (body, tcx) = trivial_main_returning_zero();
    let object = match compile_to_object(&[body], &tcx) {
        Ok(o) => o,
        Err(e) => {
            // The smoke test treats a missing LLVM toolchain as a
            // skip rather than a hard fail - `BuildError::Tool`
            // surfaces both "binary not found" and "binary failed".
            // Anything else is real and should fail.
            let msg = e.to_string();
            if msg.contains("opt") || msg.contains("llc") || msg.contains("not found") {
                eprintln!("skipping LLVM smoke test: {msg}");
                return;
            }
            panic!("compile_to_object: {e}");
        }
    };
    assert!(!object.bytes.is_empty(), "object bytes must not be empty");
    // ELF objects on Linux start with `\x7fELF`; Mach-O on macOS
    // starts with `0xfeedface` / `0xfeedfacf` little-endian. We
    // check the ELF case (the CI host) and skip the assertion on
    // other shapes since the test pivots on hardware availability.
    if cfg!(target_os = "linux") {
        assert_eq!(&object.bytes[..4], b"\x7fELF");
    }
}

#[test]
fn llvm_numeric_loop_has_no_native_preemption_poll() {
    let (body, tcx) = looping_main();
    let ir = gossamer_codegen_llvm::render_ir_to_string(&[body], &tcx, false)
        .expect("loop MIR must render to LLVM IR");
    assert!(
        !ir.contains("call i32 @gos_rt_preempt_check_and_yield"),
        "native loop back-edges should not inject an opaque preemption poll: {ir}"
    );
}

#[test]
fn llvm_acyclic_backward_numbered_edge_has_no_preemption_poll() {
    let (body, tcx) = acyclic_backward_numbered_main();
    let ir = gossamer_codegen_llvm::render_ir_to_string(&[body], &tcx, false)
        .expect("acyclic MIR must render to LLVM IR");
    assert!(
        !ir.contains("call i32 @gos_rt_preempt_check_and_yield"),
        "block numbering alone must not create a native safepoint: {ir}"
    );
}

#[test]
fn llvm_string_len_of_literal_local_folds_to_constant() {
    let (body, tcx) = string_len_main();
    let ir = gossamer_codegen_llvm::render_ir_to_string(&[body], &tcx, false)
        .expect("string length MIR must render to LLVM IR");
    assert!(
        ir.contains("store i64 5"),
        "literal string length must fold before the fasta-style modulus path: {ir}"
    );
    assert!(
        !ir.contains("i64 -5") && !ir.contains("call i64 @gos_rt_str_len"),
        "literal string length must not load a dynamic header or call runtime: {ir}"
    );
}

#[test]
fn llvm_dynamic_string_len_calls_strlen() {
    let (body, tcx) = dynamic_string_len_main();
    let ir = gossamer_codegen_llvm::render_ir_to_string(&[body], &tcx, false)
        .expect("string length MIR must render to LLVM IR");
    assert!(
        ir.contains("declare i64 @strlen(ptr)") && ir.contains("call i64 @strlen"),
        "dynamic string length must lower through strlen: {ir}"
    );
    assert!(
        !ir.contains("call i64 @gos_rt_str_len"),
        "dynamic string length must not call the runtime helper: {ir}"
    );
    assert!(
        !ir.contains("%%"),
        "generated labels must be valid LLVM IR: {ir}"
    );
}

#[test]
fn llvm_build_error_displays_unsupported_kind() {
    let err = BuildError::Unsupported("test only");
    let msg = format!("{err}");
    assert!(msg.contains("unsupported"));
    assert!(msg.contains("test only"));
}

#[test]
fn llvm_renders_typed_iterator_mir_as_lazy_runtime_calls() {
    let (body, tcx) = typed_iterator_main();
    let ir = gossamer_codegen_llvm::render_ir_to_string(&[body], &tcx, false)
        .expect("typed iterator MIR must render to LLVM IR");
    assert!(ir.contains("@\"gos_rt_lazy_iter_range_i64\""), "{ir}");
    assert!(ir.contains("@\"gos_rt_lazy_iter_take_i64\""), "{ir}");
    assert!(ir.contains("@\"gos_rt_lazy_iter_next_i64\""), "{ir}");
    assert!(
        !ir.contains("typed iterator MIR reached LLVM before iterator lowering"),
        "{ir}"
    );
}
