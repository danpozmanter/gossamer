#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::if_not_else)]
#![allow(clippy::single_match_else)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::redundant_else)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::single_match)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::let_and_return)]
#![allow(clippy::needless_collect)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::Span;
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue,
    Statement, StatementKind, Terminator, UnOp,
};

use super::*;

pub(crate) fn shape_char(tcx: &gossamer_types::TyCtxt, ty: gossamer_types::Ty) -> char {
    use gossamer_types::{FloatTy as Ft, IntTy as It, TyKind};
    match tcx.kind_of(ty) {
        TyKind::Bool => 'b',
        TyKind::Char => 'c',
        TyKind::Int(int) => match int {
            It::I8 | It::U8 => 'y',
            It::I16 | It::U16 => 'k',
            It::I32 | It::U32 => 'j',
            It::I64 | It::U64 | It::Isize | It::Usize | It::I128 | It::U128 => 'i',
        },
        TyKind::Float(f) => match f {
            Ft::F32 => 'g',
            Ft::F64 => 'f',
        },
        TyKind::Unit | TyKind::Never => 'u',
        // Pointer-shaped on 64-bit; refs / strings / aggregates
        // / opaque handles all share the same i64 register slot.
        _ => 'i',
    }
}

#[must_use]
pub fn mangle_callable_shape(tcx: &gossamer_types::TyCtxt, sig: &gossamer_types::FnSig) -> String {
    let mut name = String::with_capacity("__fn_thunk_".len() + sig.inputs.len() + 2);
    name.push_str("__fn_thunk_");
    for input in &sig.inputs {
        name.push(shape_char(tcx, *input));
    }
    name.push('_');
    name.push(shape_char(tcx, sig.output));
    name
}
