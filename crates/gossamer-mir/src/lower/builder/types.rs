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

use super::Builder;

impl<'a> Builder<'a> {
    pub(crate) fn new(
        _name: String,
        span: Span,
        tcx: &'a mut TyCtxt,
        structs: &'a HashMap<String, Vec<String>>,
        struct_defs: &'a HashMap<gossamer_resolve::DefId, String>,
        enums: &'a EnumIndex,
        impl_methods: &'a HashMap<String, Option<Ty>>,
        fn_returns: &'a HashMap<gossamer_resolve::DefId, Ty>,
        fn_inputs: &'a HashMap<gossamer_resolve::DefId, Vec<Ty>>,
        consts: &'a HashMap<gossamer_resolve::DefId, ConstValue>,
        region_unsafe: &'a std::collections::HashSet<gossamer_resolve::DefId>,
    ) -> Self {
        Self {
            tcx,
            locals: Vec::new(),
            blocks: Vec::new(),
            current: None,
            scopes: vec![HashMap::new()],
            fn_span: span,
            structs,
            struct_defs,
            enums,
            impl_methods,
            fn_returns,
            fn_inputs,
            consts,
            region_unsafe,
            local_struct: HashMap::new(),
            local_elem_struct: HashMap::new(),
            local_closure: HashMap::new(),
            local_fn_name: HashMap::new(),
            local_runtime_kind: HashMap::new(),
            local_define_layout: HashMap::new(),
            param_locals: std::collections::HashSet::new(),
            loop_stack: Vec::new(),
            payload_defer_block: None,
            grows_bindings: std::collections::HashSet::new(),
            grows_elem_ty: HashMap::new(),
            region_depth: 0,
            defer_stack: Vec::new(),
        }
    }

    /// Replaces unresolved (`Var` / `Error`) fields of a tuple type
    /// with `i64` so the codegen reads each slot as an integer
    /// instead of defaulting to a pointer. A multi-slot tuple element
    /// read out of a `Vec` (`v[i].0`) inherits its field types from
    /// the element type the binding was pinned to; when that element
    /// came from an unannotated `let mut xs = []` the int-literal
    /// fields can still be `Var`, and a `Var` field lowers to a `ptr`
    /// load — so `v[i].0` is reinterpreted as a string pointer. This
    /// mirrors the i64 fallback the for-loop tuple-destructuring path
    /// already applies. Non-tuple types and tuples with all-concrete
    /// fields pass through unchanged.
    /// Ensures `ty` renders as the 2-word by-value `i128` Result/Option
    /// representation. A `gos_rt_result_new` result is ALWAYS an i128
    /// Result/Option, but type inference sometimes leaves the binding's type
    /// an unresolved `Var` (e.g. a combinator-chain intermediate), which would
    /// render as `ptr` and TRUNCATE the i128 on store. Returns `ty` unchanged
    /// when it is already a Result/Option Adt, else a canonical `Option<i64>`
    /// (same i128 representation).
    pub(crate) fn result_repr_ty(&mut self, ty: Ty) -> Ty {
        use gossamer_types::TyKind;
        if matches!(
            self.tcx.kind_of(ty),
            TyKind::Adt { def, .. } if def.local == u32::MAX || def.local == u32::MAX - 1
        ) {
            return ty;
        }
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let substs = gossamer_types::Substs::from_types([i64_ty]);
        self.tcx.intern(TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn resolve_var_tuple_fields(&mut self, ty: Ty) -> Ty {
        use gossamer_types::TyKind;
        match self.tcx.kind_of(ty).clone() {
            TyKind::Tuple(fields) => {
                let needs_fix = fields
                    .iter()
                    .any(|f| matches!(self.tcx.kind_of(*f), TyKind::Var(_) | TyKind::Error));
                if !needs_fix {
                    return ty;
                }
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let resolved: Vec<Ty> = fields
                    .iter()
                    .map(|f| {
                        if matches!(self.tcx.kind_of(*f), TyKind::Var(_) | TyKind::Error) {
                            i64_ty
                        } else {
                            *f
                        }
                    })
                    .collect();
                self.tcx.intern(TyKind::Tuple(resolved))
            }
            // A `[T; N]` element whose `T` is still an inference
            // variable lowers its `Index` reads as `ptr` loads, the
            // same failure mode as a `Var` tuple field. Pin it to i64.
            TyKind::Array { elem, len }
                if matches!(self.tcx.kind_of(elem), TyKind::Var(_) | TyKind::Error) =>
            {
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                self.tcx.intern(TyKind::Array { elem: i64_ty, len })
            }
            _ => ty,
        }
    }

    pub(crate) fn struct_name_of(&self, ty: Ty) -> Option<String> {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Adt { def, .. } => {
                    if let Some(name) = self.struct_defs.get(def).cloned() {
                        return Some(name);
                    }
                    // Fallback: stdlib structs aren't user-
                    // declared so they don't appear in
                    // `struct_defs`, but their field layout
                    // lives in `stdlib_struct_shapes`. Match on
                    // the rendered type name (last `::`-segment).
                    let rendered = gossamer_types::printer::render_ty(self.tcx, cur);
                    let bare = rendered.rsplit("::").next().unwrap_or(&rendered);
                    if self.structs.contains_key(bare) {
                        return Some(bare.to_string());
                    }
                    return None;
                }
                TyKind::Ref { inner, .. } => cur = *inner,
                // The typechecker resolves stdlib types whose path
                // isn't declared in the resolver (e.g.
                // `&fs::DirInfo`) to `JsonValue` as a default. The
                // path information is lost in the typed `Ty`, but
                // the rendered form still reports the original
                // segment when the path matched a stdlib module
                // directly. Probe `stdlib_struct_shapes` against
                // the bare last segment to recover the layout.
                TyKind::JsonValue => {
                    let rendered = gossamer_types::printer::render_ty(self.tcx, cur);
                    let bare = rendered.rsplit("::").next().unwrap_or(&rendered);
                    if self.structs.contains_key(bare) {
                        return Some(bare.to_string());
                    }
                    return None;
                }
                _ => return None,
            }
        }
    }

    /// Bare type name of an Adt for method dispatch (`Type::method`), seeing
    /// through `&`. Unlike `struct_name_of` this also names user enums (which
    /// aren't in `struct_defs`); the caller gates on the mangled method
    /// actually existing in `impl_methods`, so naming a stdlib Adt is harmless.
    pub(crate) fn adt_dispatch_name(&self, ty: Ty) -> Option<String> {
        use gossamer_types::TyKind;
        if let Some(name) = self.struct_name_of(ty) {
            return Some(name);
        }
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Adt { .. } => {
                    let rendered = gossamer_types::printer::render_ty(self.tcx, cur);
                    let bare = rendered.rsplit("::").next().unwrap_or(&rendered);
                    // `adt#N` is the debug placeholder for an Adt whose name the
                    // tcx never registered (user enums). Reject it so the caller
                    // falls back to the `local_struct` tag, which has the name.
                    if bare.starts_with("adt#") {
                        return None;
                    }
                    return Some(bare.to_string());
                }
                TyKind::Ref { inner, .. } => cur = *inner,
                _ => return None,
            }
        }
    }

    pub(crate) fn router_bare_variant(symbol: &str) -> Option<&'static str> {
        match symbol {
            "gos_rt_router_get" => Some("gos_rt_router_get_fn"),
            "gos_rt_router_post" => Some("gos_rt_router_post_fn"),
            "gos_rt_router_put" => Some("gos_rt_router_put_fn"),
            "gos_rt_router_delete" => Some("gos_rt_router_delete_fn"),
            "gos_rt_router_patch" => Some("gos_rt_router_patch_fn"),
            "gos_rt_router_head" => Some("gos_rt_router_head_fn"),
            "gos_rt_router_options" => Some("gos_rt_router_options_fn"),
            "gos_rt_router_add" => Some("gos_rt_router_add_fn"),
            _ => None,
        }
    }

    pub(crate) fn emit_router_handler_abi(
        &mut self,
        handler_local: Local,
        span: Span,
    ) -> RouterHandlerAbi {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        if let Some(fn_name) = self.local_fn_name.get(&handler_local).cloned() {
            let fn_addr_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(fn_addr_local),
                Rvalue::CallIntrinsic {
                    name: "gos_fn_addr",
                    args: vec![Operand::Const(ConstValue::Str(fn_name))],
                },
                span,
            );
            return RouterHandlerAbi::Bare(Operand::Copy(Place::local(fn_addr_local)));
        }
        let handler_ty = self.locals[handler_local.0 as usize].ty;
        let handler_struct = self
            .struct_name_of(handler_ty)
            .unwrap_or_else(|| "Handler".to_string());
        let serve_fn_name = format!("{handler_struct}::serve");
        let fn_addr_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(fn_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(serve_fn_name))],
            },
            span,
        );
        RouterHandlerAbi::WithEnv {
            env: Operand::Copy(Place::local(handler_local)),
            fn_addr: Operand::Copy(Place::local(fn_addr_local)),
        }
    }

    pub(crate) fn lookup_define_field(
        &mut self,
        receiver_local: Local,
        long_name: &str,
        span: Span,
    ) -> Option<Local> {
        let layout = self.local_define_layout.get(&receiver_local)?.clone();
        let (idx, &(_, cell_kind)) = layout
            .iter()
            .enumerate()
            .find(|(_, (n, _))| n == long_name)?;
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let dest = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Use(Operand::Copy(Place {
                local: receiver_local,
                projection: vec![crate::Projection::Field(idx as u32)],
            })),
            span,
        );
        self.local_runtime_kind.insert(dest, cell_kind);
        Some(dest)
    }

    pub(crate) fn is_json_value_ty(&self, ty: Ty) -> bool {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::JsonValue => return true,
                TyKind::Ref { inner, .. } => cur = *inner,
                _ => return false,
            }
        }
    }

    pub(crate) fn hash_map_value_kind(&self, ty: Ty) -> Option<MapValueKind> {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::HashMap { value, .. } => {
                    return Some(map_value_kind_from(self.tcx, *value));
                }
                _ => return None,
            }
        }
    }

    /// `(K, V)` of a `HashMap<K, V>` (seeing through a leading `&`).
    pub(crate) fn hash_map_kv_tys(&self, ty: Ty) -> Option<(Ty, Ty)> {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::HashMap { key, value } => return Some((*key, *value)),
                _ => return None,
            }
        }
    }

    /// True when `ty` (through a leading `&`) is a struct or tuple — the only
    /// shapes that route through the content-hashing map key path. Bare
    /// scalars / `String` / enums keep their own paths.
    pub(crate) fn is_aggregate_key(&self, ty: Ty) -> bool {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::Tuple(_) => return true,
                TyKind::Adt { .. } => return self.struct_name_of(cur).is_some(),
                _ => return false,
            }
        }
    }

    /// Per-slot layout descriptor for a struct / tuple used as a map key, or
    /// `None` if the type can't be content-keyed. Each character describes one
    /// 8-byte slot of the aggregate's flat buffer: `'s'` = scalar (read
    /// inline), `'S'` = `String` pointer (dereferenced and folded by content).
    /// Nested all-scalar / String structs inline their slots, so the
    /// descriptor flattens them. `Vec` / nested-enum fields aren't keyable.
    pub(crate) fn key_descriptor(&self, ty: Ty) -> Option<String> {
        let mut out = String::new();
        if self.append_key_descriptor(ty, &mut out) && !out.is_empty() {
            Some(out)
        } else {
            None
        }
    }

    fn append_key_descriptor(&self, ty: Ty, out: &mut String) -> bool {
        use gossamer_types::TyKind;
        match self.tcx.kind_of(ty) {
            TyKind::Ref { inner, .. } => self.append_key_descriptor(*inner, out),
            TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::Float(_) => {
                out.push('s');
                true
            }
            TyKind::String => {
                out.push('S');
                true
            }
            TyKind::Tuple(elems) => {
                let elems = elems.clone();
                !elems.is_empty() && elems.iter().all(|e| self.append_key_descriptor(*e, out))
            }
            TyKind::Adt { def, .. } => {
                if self.struct_name_of(ty).is_none() {
                    return false;
                }
                let fields = self.tcx.struct_field_tys(*def).map(<[Ty]>::to_vec);
                match fields {
                    Some(fields) if !fields.is_empty() => {
                        fields.iter().all(|f| self.append_key_descriptor(*f, out))
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    pub(crate) fn hash_map_key_kind(&self, ty: Ty) -> Option<MapKeyKind> {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::HashMap { key, .. } => {
                    return Some(map_key_kind_from(self.tcx, *key));
                }
                _ => return None,
            }
        }
    }

    pub(crate) fn first_generic_of(&self, ty: Ty) -> Option<Ty> {
        use gossamer_types::{GenericArg, TyKind};
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::Adt { substs, .. } => {
                    for arg in substs.as_slice() {
                        if let GenericArg::Type(t) = arg {
                            return Some(*t);
                        }
                    }
                    return None;
                }
                _ => return None,
            }
        }
    }

    pub(crate) fn second_generic_of(&self, ty: Ty) -> Option<Ty> {
        use gossamer_types::{GenericArg, TyKind};
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::Adt { substs, .. } => {
                    let types: Vec<Ty> = substs
                        .as_slice()
                        .iter()
                        .filter_map(|arg| match arg {
                            GenericArg::Type(t) => Some(*t),
                            GenericArg::Const(_) => None,
                        })
                        .collect();
                    return types.get(1).copied();
                }
                _ => return None,
            }
        }
    }

    pub(crate) fn option_adt_ty(&mut self) -> Ty {
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs: gossamer_types::Substs::new(),
        })
    }

    pub(crate) fn option_tuple3_i64_i64_str_ty(&mut self) -> Ty {
        let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let s = self.tcx.string_ty();
        let tup = self
            .tcx
            .intern(gossamer_types::TyKind::Tuple(vec![i, i, s]));
        let substs = gossamer_types::Substs::from_types([tup]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_string_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let substs = gossamer_types::Substs::from_types([s]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_vec_option_string_ty(&mut self) -> Ty {
        let opt_s = self.option_string_ty();
        let v = self.tcx.intern(gossamer_types::TyKind::Vec(opt_s));
        let substs = gossamer_types::Substs::from_types([v]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_vec_string_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
        let substs = gossamer_types::Substs::from_types([v]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_string_adt_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let substs = gossamer_types::Substs::from_types([s]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_i64_adt_ty(&mut self) -> Ty {
        let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let substs = gossamer_types::Substs::from_types([i]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_f64_adt_ty(&mut self) -> Ty {
        let f = self.tcx.float_ty(gossamer_types::FloatTy::F64);
        let substs = gossamer_types::Substs::from_types([f]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn result_string_error_adt_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([s, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_vec_u8_error_ty(&mut self) -> Ty {
        let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
        let vec = self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty));
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([vec, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    /// `Result<inner, errors::Error>` for an arbitrary Ok type.
    pub(crate) fn result_of(&mut self, inner: Ty) -> Ty {
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([inner, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    /// The x509 `CertInfo` leaf tuple: `(String, String, [u8], i64,
    /// i64, [String], [u8])`.
    pub(crate) fn tuple_cert_info_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
        let vec_u8 = self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty));
        let vec_str = self.tcx.intern(gossamer_types::TyKind::Vec(s));
        self.tcx.intern(gossamer_types::TyKind::Tuple(vec![
            s, s, vec_u8, i, i, vec_str, vec_u8,
        ]))
    }

    /// The archive entry leaf tuple `(String, [u8], bool)`.
    pub(crate) fn tuple_entry_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
        let vec_u8 = self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty));
        let b = self.tcx.bool_ty();
        self.tcx
            .intern(gossamer_types::TyKind::Tuple(vec![s, vec_u8, b]))
    }

    /// The tuple type `(String, [u8])`.
    pub(crate) fn tuple_str_bytes_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
        let vec_u8 = self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty));
        self.tcx
            .intern(gossamer_types::TyKind::Tuple(vec![s, vec_u8]))
    }

    pub(crate) fn result_pair_i64_error_ty(&mut self) -> Ty {
        let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let tup = self.tcx.intern(gossamer_types::TyKind::Tuple(vec![i, i]));
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([tup, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_vec_vec_string_error_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let inner = self.tcx.intern(gossamer_types::TyKind::Vec(s));
        let outer = self.tcx.intern(gossamer_types::TyKind::Vec(inner));
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([outer, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_vec_string_error_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let vec = self.tcx.intern(gossamer_types::TyKind::Vec(s));
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([vec, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_unit_error_adt_ty(&mut self) -> Ty {
        let u = self.tcx.unit();
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([u, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    /// `Result<ok, String>` — the shape `gos_rt_join` produces (Ok
    /// value, or Err panic message as a String).
    pub(crate) fn result_payload_string_error_ty(&mut self, ok: Ty) -> Ty {
        let s = self.tcx.string_ty();
        let substs = gossamer_types::Substs::from_types([ok, s]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_i64_error_adt_ty(&mut self) -> Ty {
        let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([i, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_f64_error_adt_ty(&mut self) -> Ty {
        let f = self.tcx.float_ty(gossamer_types::FloatTy::F64);
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([f, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_bool_error_adt_ty(&mut self) -> Ty {
        let b = self.tcx.bool_ty();
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([b, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn result_json_value_error_adt_ty(&mut self) -> Ty {
        let j = self.tcx.json_value_ty();
        let e = self.tcx.dyn_error_ty();
        let substs = gossamer_types::Substs::from_types([j, e]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX),
            substs,
        })
    }

    pub(crate) fn option_json_value_adt_ty(&mut self) -> Ty {
        let j = self.tcx.json_value_ty();
        let substs = gossamer_types::Substs::from_types([j]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_json_array_adt_ty(&mut self) -> Ty {
        let j = self.tcx.json_value_ty();
        let vec_ty = self.tcx.intern(gossamer_types::TyKind::Vec(j));
        let substs = gossamer_types::Substs::from_types([vec_ty]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn option_string_vec_adt_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let vec_ty = self.tcx.intern(gossamer_types::TyKind::Vec(s));
        let substs = gossamer_types::Substs::from_types([vec_ty]);
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs,
        })
    }

    pub(crate) fn expr_runtime_kind(&self, expr: &HirExpr) -> Option<&'static str> {
        let HirExprKind::MethodCall { receiver, name, .. } = &expr.kind else {
            return None;
        };
        let receiver_kind = self
            .receiver_local_from_path(receiver)
            .and_then(|l| self.local_runtime_kind.get(&l).copied())
            .or_else(|| self.expr_runtime_kind(receiver))?;
        match (receiver_kind, name.name.as_str()) {
            ("http::Client", "get" | "post") => Some("http::Request"),
            ("http::Request", "header" | "body") => Some("http::Request"),
            ("http::Request", "send") => Some("http::Response"),
            _ => None,
        }
    }

    pub(crate) fn is_result_or_option_adt(&self, ty: Ty) -> bool {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::Adt { def, .. } => {
                    return def.local == u32::MAX || def.local == u32::MAX - 1;
                }
                _ => return false,
            }
        }
    }

    pub(crate) fn adt_generic_at(&self, ty: Ty, idx: usize) -> Option<Ty> {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::Adt { substs, .. } => {
                    return substs.types().get(idx).copied();
                }
                _ => return None,
            }
        }
    }

    pub(crate) fn elem_bytes_of(&self, ty: Ty) -> u32 {
        use gossamer_types::TyKind;
        match self.tcx.kind_of(ty) {
            TyKind::Bool => 1,
            TyKind::Char => 4,
            TyKind::Int(_) | TyKind::Float(_) => 8,
            TyKind::String => 8,
            // Tuples / aggregate ADTs occupy `slot_count * 8` bytes
            // in the flat-stack representation the native codegen
            // uses (mirrors `type_slot_count` in cranelift's
            // native.rs). A `(String, String)` tuple is two i64
            // slots = 16 bytes; treating it as 8 like any other
            // compound type would make `[(a, b), (c, d)].to_vec()`
            // copy only half of each pair.
            TyKind::Tuple(_) | TyKind::Array { .. } | TyKind::Adt { .. } => {
                self.type_slot_bytes(ty)
            }
            // Default to pointer-sized for everything else (refs,
            // enums-as-handles, channels, …).
            _ => 8,
        }
    }

    /// Builds the `Weak<T>` sentinel Adt (def `u32::MAX - 6`), matching
    /// the typechecker's `weak_adt_ty`. Used to pin the result of
    /// `x.downgrade()` so the drop pass releases it via the weak helpers.
    pub(crate) fn weak_adt_ty(&mut self, payload: Ty) -> Ty {
        use gossamer_types::TyKind;
        let def = gossamer_resolve::DefId::local(u32::MAX - 6);
        let substs = gossamer_types::Substs::from_types([payload]);
        self.tcx.intern(TyKind::Adt { def, substs })
    }

    /// Extracts `T` from a `Weak<T>` sentinel Adt; returns `None` for any
    /// other type.
    pub(crate) fn weak_payload_ty(&self, ty: Ty) -> Option<Ty> {
        use gossamer_types::TyKind;
        match self.tcx.kind_of(ty) {
            TyKind::Adt { def, substs } if def.local == u32::MAX - 6 => {
                substs.types().first().copied()
            }
            _ => None,
        }
    }

    /// Builds the `Option<T>` sentinel Adt (def `u32::MAX - 1`) carrying
    /// a concrete payload subst. Used to pin the result of `w.upgrade()`
    /// so the standard match/if-let machinery reads the discriminant and
    /// binds the payload at the right type.
    pub(crate) fn option_payload_adt_ty(&mut self, payload: Ty) -> Ty {
        use gossamer_types::TyKind;
        let def = gossamer_resolve::DefId::local(u32::MAX - 1);
        let substs = gossamer_types::Substs::from_types([payload]);
        self.tcx.intern(TyKind::Adt { def, substs })
    }

    pub(crate) fn type_slot_bytes(&self, ty: Ty) -> u32 {
        use gossamer_types::TyKind;
        match self.tcx.kind_of(ty) {
            TyKind::Tuple(elems) => {
                let total: u32 = elems
                    .iter()
                    .map(|t| self.type_slot_bytes(*t).max(8) / 8)
                    .sum();
                total.max(1) * 8
            }
            TyKind::Array { elem, len } => {
                let elem_bytes = self.type_slot_bytes(*elem).max(8);
                u32::try_from(*len).unwrap_or(1).saturating_mul(elem_bytes)
            }
            TyKind::Adt { def, .. } => {
                // `Result<T,E>` / `Option<T>` (u32::MAX, u32::MAX - 1) are the
                // 2-word by-value `i128` representation: 16 bytes per element
                // (so `Vec<Option<T>>` reserves 16 bytes/elem and push/read
                // move the full payload, not just the discriminant).
                if def.local == u32::MAX || def.local == u32::MAX - 1 {
                    return 16;
                }
                // The other stdlib struct sentinels (DirInfo, Output,
                // ResponseStream, Response — u32::MAX-2 .. u32::MAX-5) are
                // heap-allocated by `gos_rt_*` helpers and passed by pointer,
                // and `Weak<T>` (u32::MAX-6) is a weak-counted pointer — one
                // slot each.
                if def.local >= u32::MAX - 6 {
                    return 8;
                }
                // For user-defined structs with a registered field
                // layout, sum each field's slot width (rounded up to
                // 8 bytes per slot). A `Projection { a: i64, b: i64 }`
                // is two slots = 16 bytes, so a `Vec<Projection>`
                // created with `gos_rt_vec_new(elem_bytes)` reserves
                // 16 bytes per element and the push-site memcpy
                // copies the full inline struct rather than truncating
                // to the first field.
                if let Some(field_tys) = self.tcx.struct_field_tys(*def) {
                    let total_slots: u32 = field_tys
                        .iter()
                        .map(|t| (self.type_slot_bytes(*t).max(8)) / 8)
                        .sum();
                    return total_slots.max(1) * 8;
                }
                8
            }
            TyKind::Bool => 1,
            TyKind::Char => 4,
            TyKind::Int(_) | TyKind::Float(_) | TyKind::String => 8,
            _ => 8,
        }
    }

    pub(crate) fn binding_type_to_mir(&mut self, t: &gossamer_resolve::BindingType) -> Ty {
        use gossamer_resolve::BindingType as B;
        use gossamer_types::TyKind;
        match t {
            B::Unit => self.tcx.unit(),
            B::Bool => self.tcx.bool_ty(),
            B::I64 => self.tcx.int_ty(gossamer_types::IntTy::I64),
            B::F64 => self.tcx.float_ty(gossamer_types::FloatTy::F64),
            B::Char => self.tcx.char_ty(),
            B::String => self.tcx.string_ty(),
            B::Bytes => {
                // Bytes is `[u8]` at the source level; the runtime
                // represents it through the same IntArray path as
                // `[i64]` so the MIR shape is `Vec<i64>`.
                let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                self.tcx.intern(TyKind::Vec(u8_ty))
            }
            B::Vec(inner) => {
                let inner_ty = self.binding_type_to_mir(inner);
                self.tcx.intern(TyKind::Vec(inner_ty))
            }
            // Option / Result / Variant map to the runtime's
            // tagged-union pointer; the codegen treats them as
            // ptr-sized.
            B::Option(_) | B::Result(_, _) | B::Variant(_) => {
                self.tcx.int_ty(gossamer_types::IntTy::I64)
            }
            // Map / Callback / Tuple / Opaque / Any all flow as
            // ptr-sized values (handles or untyped passthroughs).
            B::Map(_, _) | B::Callback(_, _) | B::Tuple(_) | B::Opaque(_) | B::Any => {
                self.tcx.int_ty(gossamer_types::IntTy::I64)
            }
        }
    }

    pub(crate) fn struct_name_from_expr(&self, expr: &HirExpr) -> Option<String> {
        use gossamer_types::TyKind;
        if let Some(name) = self.struct_name_of(expr.ty) {
            return Some(name);
        }
        match &expr.kind {
            HirExprKind::Index { base, .. } => {
                // Prefer the element-type registration (survives
                // inference-variable leakage) before walking the
                // base's static type.
                if let HirExprKind::Path { segments, .. } = &base.kind {
                    if let Some(first) = segments.first() {
                        if let Some(local) = self.lookup_local(&first.name) {
                            if let Some(name) = self.local_elem_struct.get(&local).cloned() {
                                return Some(name);
                            }
                        }
                    }
                }
                let mut cur = base.ty;
                loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                            return self.struct_name_of(*elem);
                        }
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => return self.struct_name_from_expr(base),
                    }
                }
            }
            HirExprKind::TupleIndex { receiver, index } => {
                let mut cur = receiver.ty;
                loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::Tuple(elems) => {
                            let elem = *elems.get(*index as usize)?;
                            return self.struct_name_of(elem);
                        }
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => return self.struct_name_from_expr(receiver),
                    }
                }
            }
            HirExprKind::Path { segments, .. } => {
                let first = segments.first()?;
                let local = self.lookup_local(&first.name)?;
                // A variant constructor's type is left a `Var`, so recover the
                // struct / enum name from the `local_struct` tag first.
                if let Some(name) = self.local_struct.get(&local).cloned() {
                    return Some(name);
                }
                let ty = self.locals.get(local.0 as usize)?.ty;
                self.struct_name_of(ty)
            }
            _ => None,
        }
    }

    pub(crate) fn substs_of(&self, ty: Ty) -> gossamer_types::Substs {
        match self.tcx.kind(ty) {
            Some(gossamer_types::TyKind::FnDef { substs, .. }) => substs.clone(),
            _ => gossamer_types::Substs::new(),
        }
    }

    pub(crate) fn iter_element_kind(&self, ty: Ty) -> Option<gossamer_types::TyKind> {
        use gossamer_types::TyKind;
        let kind = self.tcx.kind_of(ty).clone();
        let kind = match kind {
            TyKind::Ref { inner, .. } => self.tcx.kind_of(inner).clone(),
            other => other,
        };
        match kind {
            TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                Some(self.tcx.kind_of(elem).clone())
            }
            _ => None,
        }
    }
}
