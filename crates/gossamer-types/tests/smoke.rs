//! End-to-end tests for the type interner and the unifier.

use gossamer_resolve::DefId;
use gossamer_types::{
    FloatTy, GenericArg, InferCtxt, IntTy, Mutbl, Substs, Ty, TyCtxt, TyKind, UnifyError, render_ty,
};

#[test]
fn interner_returns_same_handle_for_equal_types() {
    let mut tcx = TyCtxt::new();
    let a = tcx.int_ty(IntTy::I32);
    let b = tcx.int_ty(IntTy::I32);
    assert_eq!(a, b);
    let c = tcx.int_ty(IntTy::I64);
    assert_ne!(a, c);
}

#[test]
fn primitive_accessors_are_cached() {
    let mut tcx = TyCtxt::new();
    let unit_a = tcx.unit();
    let unit_b = tcx.unit();
    assert_eq!(unit_a, unit_b);
    let bool_a = tcx.bool_ty();
    let bool_b = tcx.bool_ty();
    assert_eq!(bool_a, bool_b);
    assert_ne!(unit_a, bool_a);
}

#[test]
fn structural_tuples_intern_structurally() {
    let mut tcx = TyCtxt::new();
    let i32_ = tcx.int_ty(IntTy::I32);
    let bool_ = tcx.bool_ty();
    let a = tcx.intern(TyKind::Tuple(vec![i32_, bool_]));
    let b = tcx.intern(TyKind::Tuple(vec![i32_, bool_]));
    assert_eq!(a, b);
    let c = tcx.intern(TyKind::Tuple(vec![bool_, i32_]));
    assert_ne!(a, c);
}

#[test]
fn ref_types_and_collections_render_correctly() {
    let mut tcx = TyCtxt::new();
    let i32_ = tcx.int_ty(IntTy::I32);
    let ref_i32 = tcx.intern(TyKind::Ref {
        mutability: Mutbl::Not,
        inner: i32_,
    });
    assert_eq!(render_ty(&tcx, ref_i32), "&i32");
    let mut_ref_i32 = tcx.intern(TyKind::Ref {
        mutability: Mutbl::Mut,
        inner: i32_,
    });
    assert_eq!(render_ty(&tcx, mut_ref_i32), "&mut i32");
    let vec_i32 = tcx.intern(TyKind::Vec(i32_));
    assert_eq!(render_ty(&tcx, vec_i32), "Vec<i32>");
    let string = tcx.string_ty();
    let map = tcx.intern(TyKind::HashMap {
        key: string,
        value: i32_,
    });
    assert_eq!(render_ty(&tcx, map), "HashMap<String, i32>");
}

#[test]
fn unify_binds_variable_to_concrete_type() {
    let mut tcx = TyCtxt::new();
    let mut infer = InferCtxt::new();
    let var = infer.fresh_var(&mut tcx);
    let i32_ = tcx.int_ty(IntTy::I32);
    assert!(infer.unify(&mut tcx, var, i32_).is_ok());
    let resolved = infer.resolve(&tcx, var);
    assert_eq!(resolved, i32_);
}

#[test]
fn unify_two_variables_then_concrete() {
    let mut tcx = TyCtxt::new();
    let mut infer = InferCtxt::new();
    let a = infer.fresh_var(&mut tcx);
    let b = infer.fresh_var(&mut tcx);
    infer.unify(&mut tcx, a, b).unwrap();
    let bool_ = tcx.bool_ty();
    infer.unify(&mut tcx, b, bool_).unwrap();
    assert_eq!(infer.resolve(&tcx, a), bool_);
    assert_eq!(infer.resolve(&tcx, b), bool_);
}

#[test]
fn unify_mismatch_reports_error() {
    let mut tcx = TyCtxt::new();
    let mut infer = InferCtxt::new();
    let i32_ = tcx.int_ty(IntTy::I32);
    let f64_ = tcx.float_ty(FloatTy::F64);
    let err = infer.unify(&mut tcx, i32_, f64_).unwrap_err();
    assert_eq!(err, UnifyError::Mismatch);
}

#[test]
fn unify_occurs_check_triggers_on_self_reference() {
    let mut tcx = TyCtxt::new();
    let mut infer = InferCtxt::new();
    let var = infer.fresh_var(&mut tcx);
    let tuple = tcx.intern(TyKind::Tuple(vec![var, var]));
    let err = infer.unify(&mut tcx, var, tuple).unwrap_err();
    assert!(matches!(err, UnifyError::Occurs { .. }));
}

#[test]
fn unify_structural_recurses_into_collections() {
    let mut tcx = TyCtxt::new();
    let mut infer = InferCtxt::new();
    let var = infer.fresh_var(&mut tcx);
    let i32_ = tcx.int_ty(IntTy::I32);
    let vec_var = tcx.intern(TyKind::Vec(var));
    let vec_i32 = tcx.intern(TyKind::Vec(i32_));
    infer.unify(&mut tcx, vec_var, vec_i32).unwrap();
    assert_eq!(infer.resolve(&tcx, var), i32_);
}

#[test]
fn never_unifies_with_anything() {
    let mut tcx = TyCtxt::new();
    let mut infer = InferCtxt::new();
    let never = tcx.never();
    let i32_ = tcx.int_ty(IntTy::I32);
    assert!(infer.unify(&mut tcx, never, i32_).is_ok());
    let bool_ = tcx.bool_ty();
    assert!(infer.unify(&mut tcx, bool_, never).is_ok());
}

#[test]
fn adt_with_substs_renders_with_generics() {
    let mut tcx = TyCtxt::new();
    let i32_ = tcx.int_ty(IntTy::I32);
    let bool_ = tcx.bool_ty();
    let substs = Substs::from_args(vec![GenericArg::Type(i32_), GenericArg::Type(bool_)]);
    let adt = tcx.intern(TyKind::Adt {
        def: DefId::local(42),
        substs,
    });
    assert_eq!(render_ty(&tcx, adt), "adt#42<i32, bool>");
}

#[test]
fn array_renders_with_length() {
    let mut tcx = TyCtxt::new();
    let u8_ = tcx.int_ty(IntTy::U8);
    let arr: Ty = tcx.intern(TyKind::Array { elem: u8_, len: 16 });
    assert_eq!(render_ty(&tcx, arr), "[u8; 16]");
}
