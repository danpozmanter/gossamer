//! End-to-end test that the cranelift-jit backend produces native
//! code we can call through a function pointer in-process. The
//! body shapes here parallel the smallest cases the bytecode VM
//! hands to the JIT trampoline.

#![allow(missing_docs)]
#![allow(unsafe_code)]

use std::mem;

use gossamer_codegen_cranelift::compile_to_jit;
use gossamer_lex::SourceMap;
use gossamer_mir::{
    BasicBlock, BinOp, Body, ConstValue, LocalDecl, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator,
};
use gossamer_types::{IntTy, TyCtxt};

fn dummy_span() -> gossamer_lex::Span {
    let mut map = SourceMap::new();
    let file = map.add_file("jit.gos", "");
    gossamer_lex::Span::new(file, 0, 0)
}

fn place(local: u32) -> Place {
    Place {
        local: gossamer_mir::Local(local),
        projection: Vec::<Projection>::new(),
    }
}

#[test]
fn jit_compiles_const_int_returning_main() {
    // fn main() -> i64 { 42 }
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.int_ty(IntTy::I64);
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![LocalDecl {
            ty: i64_ty,
            debug_name: None,
            mutable: false,
        }],
        blocks: vec![BasicBlock {
            id: gossamer_mir::BlockId(0),
            stmts: vec![Statement {
                span: dummy_span(),
                kind: StatementKind::Assign {
                    place: place(0),
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(42))),
                },
            }],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };
    let artifact = compile_to_jit(&[body], &tcx).expect("compile");
    let main_fn = artifact.functions.get("main").expect("main present");
    // SAFETY: the test only invokes `main_fn` while `artifact` is
    // live, matching the trampoline's lifetime contract.
    let result: i64 = unsafe {
        let f: extern "C" fn() -> i64 = mem::transmute(main_fn.ptr);
        f()
    };
    assert_eq!(result, 42);
}

#[test]
fn jit_compiles_simple_arithmetic_function() {
    // fn add(a: i64, b: i64) -> i64 { a + b }
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.int_ty(IntTy::I64);
    let body = Body {
        name: "add".to_string(),
        def: None,
        arity: 2,
        locals: vec![
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
            },
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
            },
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
            },
        ],
        blocks: vec![BasicBlock {
            id: gossamer_mir::BlockId(0),
            stmts: vec![Statement {
                span: dummy_span(),
                kind: StatementKind::Assign {
                    place: place(0),
                    rvalue: Rvalue::BinaryOp {
                        op: BinOp::Add,
                        lhs: Operand::Copy(place(1)),
                        rhs: Operand::Copy(place(2)),
                    },
                },
            }],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };
    let artifact = compile_to_jit(&[body], &tcx).expect("compile");
    let add_fn = artifact.functions.get("add").expect("add present");
    let result: i64 = unsafe {
        let f: extern "C" fn(i64, i64) -> i64 = mem::transmute(add_fn.ptr);
        f(7, 35)
    };
    assert_eq!(result, 42);
}

#[test]
fn jit_artifact_drops_without_panic() {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.int_ty(IntTy::I64);
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![LocalDecl {
            ty: i64_ty,
            debug_name: None,
            mutable: false,
        }],
        blocks: vec![BasicBlock {
            id: gossamer_mir::BlockId(0),
            stmts: vec![Statement {
                span: dummy_span(),
                kind: StatementKind::Assign {
                    place: place(0),
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                },
            }],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };
    let artifact = compile_to_jit(&[body], &tcx).expect("compile");
    drop(artifact);
}
