//! Unit-test coverage per MIR shape for the LLVM lowerer.
//!
//! Each test hand-rolls a synthetic Body, runs `render_ir_to_string`
//! against it, and asserts substring properties on the resulting IR
//! rather than committing a full snapshot. This avoids the
//! string-pool / metadata non-determinism that plagued the earlier
//! `lower_snapshots` suite while still catching regressions in the
//! lowering of each MIR shape.

#![allow(missing_docs)]

use gossamer_codegen_llvm::render_ir_to_string;
use gossamer_lex::{SourceMap, Span};
use gossamer_mir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue,
    Statement, StatementKind, Terminator,
};
use gossamer_types::{ArrayLen, IntTy, TyCtxt, TyKind};

fn dummy_span() -> Span {
    let mut map = SourceMap::new();
    let file = map.add_file("shapes.gos", "");
    Span::new(file, 0, 0)
}

fn place(local: u32) -> Place {
    Place {
        local: Local(local),
        projection: Vec::new(),
    }
}

fn const_int_assign(local: u32, value: i64) -> Statement {
    Statement {
        span: dummy_span(),
        kind: StatementKind::Assign {
            place: place(local),
            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(value)))),
        },
    }
}

fn binop_assign(local: u32, op: BinOp, lhs: u32, rhs: u32) -> Statement {
    Statement {
        span: dummy_span(),
        kind: StatementKind::Assign {
            place: place(local),
            rvalue: Rvalue::BinaryOp {
                op,
                lhs: Operand::Copy(place(lhs)),
                rhs: Operand::Copy(place(rhs)),
            },
        },
    }
}

fn build_binop_main(op: BinOp, lhs: i64, rhs: i64) -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
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
                mutable: false,
                region: false,
            },
            LocalDecl {
                ty: i64_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
        ],
        blocks: vec![BasicBlock {
            id: BlockId(0),
            stmts: vec![
                const_int_assign(1, lhs),
                const_int_assign(2, rhs),
                binop_assign(0, op, 1, 2),
            ],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };
    (body, tcx)
}

fn build_const_int_main(value: i64) -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
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
            stmts: vec![const_int_assign(0, value)],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };
    (body, tcx)
}

#[test]
fn const_int_zero_emits_store_i64() {
    let (body, tcx) = build_const_int_main(0);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("define i64"), "IR was:\n{ir}");
    assert!(ir.contains("store i64 0"), "IR was:\n{ir}");
}

#[test]
fn const_int_max_emits_max_literal() {
    let (body, tcx) = build_const_int_main(i64::MAX);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(
        ir.contains(&format!("store i64 {}", i64::MAX)),
        "IR was:\n{ir}"
    );
}

#[test]
fn binop_add_i64_emits_add_instruction() {
    let (body, tcx) = build_binop_main(BinOp::Add, 3, 4);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("add i64"), "IR was:\n{ir}");
}

#[test]
fn binop_sub_i64_emits_sub_instruction() {
    let (body, tcx) = build_binop_main(BinOp::Sub, 10, 3);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("sub i64"), "IR was:\n{ir}");
}

#[test]
fn binop_mul_i64_emits_mul_instruction() {
    let (body, tcx) = build_binop_main(BinOp::Mul, 6, 7);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("mul i64"), "IR was:\n{ir}");
}

#[test]
fn binop_div_i64_emits_sdiv_instruction() {
    let (body, tcx) = build_binop_main(BinOp::Div, 20, 4);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("sdiv i64"), "IR was:\n{ir}");
}

#[test]
fn binop_bitand_i64_emits_and_instruction() {
    let (body, tcx) = build_binop_main(BinOp::BitAnd, 15, 9);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("and i64"), "IR was:\n{ir}");
}

#[test]
fn binop_bitor_i64_emits_or_instruction() {
    let (body, tcx) = build_binop_main(BinOp::BitOr, 12, 5);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("or i64"), "IR was:\n{ir}");
}

#[test]
fn binop_bitxor_i64_emits_xor_instruction() {
    let (body, tcx) = build_binop_main(BinOp::BitXor, 7, 3);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("xor i64"), "IR was:\n{ir}");
}

#[test]
fn binop_shl_i64_emits_shl_instruction() {
    let (body, tcx) = build_binop_main(BinOp::Shl, 1, 4);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("shl i64"), "IR was:\n{ir}");
}

#[test]
fn binop_shr_i64_emits_ashr_instruction() {
    let (body, tcx) = build_binop_main(BinOp::Shr, 64, 2);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    // Signed shift right lowers to ashr (arithmetic shift right).
    assert!(ir.contains("ashr i64"), "IR was:\n{ir}");
}

#[test]
fn binop_rem_i64_emits_srem_instruction() {
    let (body, tcx) = build_binop_main(BinOp::Rem, 17, 5);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("srem i64"), "IR was:\n{ir}");
}

#[test]
fn ir_contains_module_id_header() {
    let (body, tcx) = build_const_int_main(0);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("ModuleID"), "IR was:\n{ir}");
}

#[test]
fn ir_contains_target_triple() {
    let (body, tcx) = build_const_int_main(0);
    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(ir.contains("target triple"), "IR was:\n{ir}");
}

#[test]
fn large_fixed_array_local_spills_to_heap_storage() {
    let mut tcx = TyCtxt::new();
    let unit_ty = tcx.intern(TyKind::Unit);
    let i64_ty = tcx.intern(TyKind::Int(IntTy::I64));
    let arr_ty = tcx.intern(TyKind::Array {
        elem: i64_ty,
        len: ArrayLen::Concrete(100_000_000),
    });
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![
            LocalDecl {
                ty: unit_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
            LocalDecl {
                ty: arr_ty,
                debug_name: None,
                mutable: false,
                region: false,
            },
        ],
        blocks: vec![BasicBlock {
            id: BlockId(0),
            stmts: vec![Statement {
                span: dummy_span(),
                kind: StatementKind::Assign {
                    place: place(1),
                    rvalue: Rvalue::Repeat {
                        value: Operand::Const(ConstValue::Int(0)),
                        count: 100_000_000,
                    },
                },
            }],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };

    let ir = render_ir_to_string(&[body], &tcx, false).unwrap();
    assert!(
        ir.contains("%l1 = call ptr @gos_rt_aggr_alloc(i64 800000000)"),
        "IR was:\n{ir}"
    );
    assert!(!ir.contains("%l1 = alloca"), "IR was:\n{ir}");
    assert!(
        ir.contains("call void @\"gos_rt_aggr_free\"(ptr %l1, i64 800000000)"),
        "IR was:\n{ir}"
    );
}
