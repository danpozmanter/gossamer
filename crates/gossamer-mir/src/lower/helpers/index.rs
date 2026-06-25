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

pub(crate) enum MapValueKind {
    I64,
    String,
    Other,
}

pub(crate) enum MapKeyKind {
    I64,
    String,
    Other,
}

pub(crate) fn map_value_kind_from(tcx: &gossamer_types::TyCtxt, ty: Ty) -> MapValueKind {
    use gossamer_types::TyKind;
    match tcx.kind_of(ty) {
        TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::Float(_) => MapValueKind::I64,
        TyKind::String => MapValueKind::String,
        _ => MapValueKind::Other,
    }
}

pub(crate) fn map_key_kind_from(tcx: &gossamer_types::TyCtxt, ty: Ty) -> MapKeyKind {
    use gossamer_types::TyKind;
    match tcx.kind_of(ty) {
        TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::Float(_) => MapKeyKind::I64,
        TyKind::String => MapKeyKind::String,
        _ => MapKeyKind::Other,
    }
}

pub(crate) struct EnumIndex {
    pub(crate) by_enum: HashMap<String, Vec<String>>,
    pub(crate) variant_index: HashMap<String, (String, usize)>,
    /// `variant_name -> [field_name]` for struct-payload variants.
    /// Lets `__struct("Rect", "w", v, "h", v)` calls resolve their
    /// declaration order even when `Rect` is an enum variant rather
    /// than a free struct.
    pub(crate) variant_fields: HashMap<String, Vec<String>>,
    /// `variant_name -> [field_ty]`, parallel to `variant_fields`.
    /// Used by struct-pattern matching so `Shape::Rect { w, h }`
    /// declares `w` / `h` MIR locals at the right MIR type - the
    /// generic `gos_load` helper returns i64 and a missing
    /// f64-typed binding made the cranelift codegen skip the
    /// I64→F64 bitcast in `define_var_to_with`.
    pub(crate) variant_field_tys: HashMap<String, Vec<Ty>>,
    /// `variant_name -> bool` - true when the variant carries any
    /// payload (struct fields OR tuple-payload constructor calls
    /// observed elsewhere in the program). Match dispatch keys off
    /// this to decide whether the scrutinee is a heap pointer.
    pub(crate) variant_has_payload: HashMap<String, bool>,
}

impl EnumIndex {
    /// Resolves an enum-variant path / bare name to `(enum_name,
    /// variant_index)`. Accepts paths of the form `Color::Green`
    /// (two segments) or the bare name `Green` (one segment) when
    /// the variant name is unambiguous across the program.
    pub(crate) fn lookup(&self, segments: &[Ident]) -> Option<(String, usize)> {
        match segments {
            [single] => self.variant_index.get(&single.name).cloned(),
            [enum_seg, variant_seg] => {
                let variants = self.by_enum.get(&enum_seg.name)?;
                let idx = variants.iter().position(|v| v == &variant_seg.name)?;
                Some((enum_seg.name.clone(), idx))
            }
            _ => None,
        }
    }

    /// Returns true when ANY variant of the enum that contains
    /// `segments` carries fields. Used by match dispatch to decide
    /// whether the scrutinee is a heap pointer (load disc from
    /// offset 0) or a flat i64 (variant index inline).
    pub(crate) fn has_any_payload(&self, segments: &[Ident]) -> bool {
        let Some((enum_name, _)) = self.lookup(segments) else {
            return false;
        };
        let Some(variants) = self.by_enum.get(&enum_name) else {
            return false;
        };
        variants
            .iter()
            .any(|v| self.variant_has_payload.get(v).copied().unwrap_or(false))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VecElemKind {
    /// Element / map-key type is `String` (or `&String`).
    Str,
    /// Default - anything else; treated as i64-shaped at the FFI.
    Int,
}

pub(crate) fn vec_element_kind(tcx: &gossamer_types::TyCtxt, ty: Ty) -> VecElemKind {
    use gossamer_types::TyKind;
    let inner = match tcx.kind_of(ty) {
        TyKind::Vec(inner) | TyKind::Slice(inner) => *inner,
        TyKind::Array { elem, .. } => *elem,
        _ => return VecElemKind::Int,
    };
    if matches!(tcx.kind_of(inner), TyKind::String) {
        VecElemKind::Str
    } else {
        VecElemKind::Int
    }
}

pub(crate) fn hashmap_key_kind(tcx: &gossamer_types::TyCtxt, ty: Ty) -> VecElemKind {
    use gossamer_types::TyKind;
    if let TyKind::HashMap { key, .. } = tcx.kind_of(ty) {
        if matches!(tcx.kind_of(*key), TyKind::String) {
            return VecElemKind::Str;
        }
    }
    VecElemKind::Int
}

pub(crate) fn arg_is_float(tcx: &gossamer_types::TyCtxt, expr: &HirExpr) -> bool {
    use gossamer_types::TyKind;
    matches!(tcx.kind_of(expr.ty), TyKind::Float(_))
}

/// True when `expr` types as `char` (peeling references). Used by the
/// `min`/`max`/`clamp` dispatch to keep the result `char`-typed - the
/// codepoint compares correctly as an i64, but the result must print as a
/// character, not its codepoint integer.
pub(crate) fn arg_is_char(tcx: &gossamer_types::TyCtxt, expr: &HirExpr) -> bool {
    use gossamer_types::TyKind;
    let mut walk = expr.ty;
    while let TyKind::Ref { inner, .. } = tcx.kind_of(walk) {
        walk = *inner;
    }
    matches!(tcx.kind_of(walk), TyKind::Char)
}

/// True when `expr` types as `Vec<u8>` / `&Vec<u8>` / `&[u8]` /
/// `[u8]` - used by the `os::write_file` dispatcher to pick the
/// bytes-shaped runtime helper for binary writes (preserves NUL
/// bytes that the c-string variant would truncate at).
pub(crate) fn is_vec_u8_arg(tcx: &gossamer_types::TyCtxt, expr: &HirExpr) -> bool {
    use gossamer_types::{IntTy, TyKind};
    let mut walk = expr.ty;
    while let TyKind::Ref { inner, .. } = tcx.kind_of(walk) {
        walk = *inner;
    }
    let inner = match tcx.kind_of(walk) {
        TyKind::Vec(t) | TyKind::Slice(t) => *t,
        _ => return false,
    };
    matches!(tcx.kind_of(inner), TyKind::Int(IntTy::U8))
}
