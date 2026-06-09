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
    pub(crate) fn lower_struct_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        let last = segments.last()?;
        if last.name != "__struct" {
            return None;
        }
        let (name_expr, pairs) = args.split_first()?;
        let HirExprKind::Literal(HirLiteral::String(struct_name)) = &name_expr.kind else {
            return None;
        };
        if pairs.len() % 2 != 0 {
            return None;
        }
        // Try the free-struct table first. Then check whether the
        // name is a struct-payload enum variant (`Shape::Rect { w,
        // h }`); if so, route through `lower_user_enum_ctor` so
        // the runtime layout matches what the variant-pattern
        // match expects (`[disc, p0, p1, …]` heap aggregate).
        // Mixing the flat-tuple lowering with the disc-prefixed
        // match was the root cause of the
        // control_flow / data_structures crash — the match read
        // disc from offset 0 of a value that didn't have one.
        let is_free_struct = self.structs.contains_key(struct_name);
        let mut variant_idx: Option<usize> = None;
        let mut variant_enum_name: Option<String> = None;
        if !is_free_struct {
            // The variant-fields map is keyed by the bare variant
            // name; look up its declaration index in the parent
            // enum so the constructor writes the right disc.
            let variant_ident = Ident::new(struct_name.clone());
            if let Some((enum_name, idx)) = self.enums.lookup(std::slice::from_ref(&variant_ident))
            {
                variant_idx = Some(idx);
                variant_enum_name = Some(enum_name);
            }
        }
        let order = self
            .structs
            .get(struct_name)
            .cloned()
            .or_else(|| self.enums.variant_fields.get(struct_name).cloned())?;
        let mut provided: HashMap<String, &HirExpr> = HashMap::new();
        let mut base_expr: Option<&HirExpr> = None;
        let mut chunks = pairs.chunks_exact(2);
        for chunk in chunks.by_ref() {
            let HirExprKind::Literal(HirLiteral::String(field_name)) = &chunk[0].kind else {
                return None;
            };
            // Functional-update sentinel: `..base` is encoded by HIR
            // as a `"__base"` key whose value is the base expression.
            // Stash it and fill in any unprovided fields by projecting
            // `base.field` below.
            if field_name == "__base" {
                base_expr = Some(&chunk[1]);
                continue;
            }
            provided.insert(field_name.clone(), &chunk[1]);
        }
        if let Some(idx) = variant_idx {
            // Enum struct-variant: build a `[disc, w, h, …]` heap
            // aggregate. Args are HirExpr in field-declaration
            // order so the load offsets in
            // `lower_pattern_predicate`'s Struct arm line up.
            let arg_exprs: Vec<HirExpr> = order
                .iter()
                .filter_map(|f| provided.get(f.as_str()).map(|e| (*e).clone()))
                .collect();
            let enum_name = variant_enum_name.as_deref().unwrap_or("");
            let result = self.lower_user_enum_ctor(
                enum_name,
                u32::try_from(idx).unwrap_or(0),
                &arg_exprs,
                ty,
                span,
            );
            // Tag the struct-variant literal with its ENUM name (not the bare
            // variant) so `==` / `{:?}` route to `Enum::eq` / `Enum::fmt`.
            if let Some(local) = result {
                self.local_struct.insert(local, enum_name.to_string());
            }
            return result;
        }
        // Resolve the base expression once (when present) so missing
        // fields can be filled by projecting `base.field`. Lowering
        // the base into a fresh local also lets us register its
        // struct identity for nested-field projection lookups.
        let base_local: Option<Local> = if let Some(b) = base_expr {
            let local = self.lower_expr(b)?;
            self.local_struct
                .entry(local)
                .or_insert_with(|| struct_name.clone());
            Some(local)
        } else {
            None
        };
        // Declared field types (when the struct's `ty` carries a real
        // DefId) let us coerce a flat `[T; N]` array-literal value into
        // a heap `GosVec` when the field is declared `[T]` (Vec) /
        // `[T]`-slice. Without this, `Q { bytes: [1, 2, 3] }` stores the
        // 3-slot inline array straight into the 1-slot Vec field — the
        // struct alloca overflows and a later `q.bytes.len()` reads
        // element[0] as the Vec pointer (misaligned-deref crash).
        let field_tys: Option<Vec<Ty>> = match self.tcx.kind_of(ty) {
            gossamer_types::TyKind::Adt { def, .. } => {
                self.tcx.struct_field_tys(*def).map(<[Ty]>::to_vec)
            }
            _ => None,
        };
        let mut operands = Vec::with_capacity(order.len());
        for (idx, field) in order.iter().enumerate() {
            if let Some(value_expr) = provided.get(field.as_str()) {
                let mut value_local = self.lower_expr(value_expr)?;
                // Coerce an array-literal value to a Vec when the field
                // is declared as a growable `[T]` / slice.
                if let Some(field_ty) = field_tys.as_ref().and_then(|t| t.get(idx)).copied() {
                    use gossamer_types::TyKind;
                    let val_ty = self.locals[value_local.0 as usize].ty;
                    if let TyKind::Array { elem, len } = self.tcx.kind_of(val_ty).clone()
                        && matches!(
                            self.tcx.kind_of(field_ty),
                            TyKind::Vec(_) | TyKind::Slice(_)
                        )
                    {
                        value_local = self.coerce_array_to_vec(value_local, elem, len, span);
                    }
                }
                operands.push(Operand::Copy(Place::local(value_local)));
            } else if let Some(base) = base_local {
                let projection_idx = u32::try_from(idx).ok()?;
                let mut place = Place::local(base);
                place
                    .projection
                    .push(crate::ir::Projection::Field(projection_idx));
                operands.push(Operand::Copy(place));
            } else {
                return None;
            }
        }
        let dest = self.fresh(ty);
        self.local_struct.insert(dest, struct_name.clone());
        // Adt requires a DefId we don't have handy at this layer.
        // The native codegen treats every aggregate as a flat i64-per
        // slot stack slot regardless of kind, so `Tuple` is a safe
        // structural stand-in until monomorphisation wires real DefIds
        // through.
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Tuple,
                operands,
            },
            span,
        );
        Some(dest)
    }

    pub(crate) fn lower_field_access(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Try the place-expression path first: for `a.x`, `a[i].x`,
        // and other lvalue-shaped receivers this builds a direct
        // projected place read without materialising the intermediate
        // struct copy. That lets `a[i].x` lower to `copy a[i].x`
        // instead of `tmp = a[i]; tmp.x` (and the latter's
        // lost-struct-name fallback to the unsupported placeholder).
        if let Some(mut place) = self.lower_place_expr(receiver) {
            if let Some(rk) = self.local_runtime_kind.get(&place.local).copied() {
                let helper: Option<(&'static str, Ty)> = match (rk, name.name.as_str()) {
                    ("http::Response", "status") => Some((
                        "gos_rt_http_response_status",
                        self.tcx.int_ty(gossamer_types::IntTy::I64),
                    )),
                    ("http::Response", "body") => {
                        Some(("gos_rt_http_response_body", self.tcx.string_ty()))
                    }
                    ("http::Response", "raw_bytes") => {
                        let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
                        Some((
                            "gos_rt_http_response_raw_bytes",
                            self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty)),
                        ))
                    }
                    ("http::Request", "method") => {
                        Some(("gos_rt_http_request_method", self.tcx.string_ty()))
                    }
                    ("http::Request", "path") => {
                        Some(("gos_rt_http_request_path", self.tcx.string_ty()))
                    }
                    ("http::Request", "query") => {
                        Some(("gos_rt_http_request_query", self.tcx.string_ty()))
                    }
                    ("http::Request", "body") => {
                        Some(("gos_rt_http_request_body_str", self.tcx.string_ty()))
                    }
                    ("errors::Error", "message") => {
                        Some(("gos_rt_error_message", self.tcx.string_ty()))
                    }
                    ("errors::Error", "cause") => Some((
                        "gos_rt_error_cause",
                        self.tcx.int_ty(gossamer_types::IntTy::I64),
                    )),
                    _ => None,
                };
                if let Some((rt_name, ret_ty)) = helper {
                    let dest = self.fresh(ret_ty);
                    let next = self.new_block(span);
                    self.terminate(Terminator::Call {
                        callee: Operand::Const(ConstValue::Str(rt_name.to_string())),
                        args: vec![Operand::Copy(Place::local(place.local))],
                        destination: Place::local(dest),
                        target: Some(next),
                    });
                    self.set_current(next);
                    return Some(dest);
                }
            }
            let struct_name = self
                .local_struct
                .get(&place.local)
                .cloned()
                .or_else(|| self.struct_name_from_expr(receiver));
            if let Some(sname) = struct_name {
                if let Some(order) = self.structs.get(&sname).cloned() {
                    if let Some(pos) = order.iter().position(|f| f == &name.name) {
                        let idx = u32::try_from(pos).ok()?;
                        // The HIR-recorded `ty` for a field projection
                        // can be an unresolved inference variable when
                        // the receiver's type only crystallised at MIR
                        // pinning time (e.g. `body_f.value` after
                        // `let body_f = body.to_fahrenheit()`). Fall
                        // through to the struct's declared field type
                        // — looked up via the receiver local's MIR
                        // `Adt` def — so downstream printing and
                        // temp-local typing see the real `f64` /
                        // `String` / etc. Without this, the lower
                        // tier alloca's the temp as `ptr` and stores
                        // an `f64` through it, producing invalid IR.
                        // Resolve the field's concrete type. The HIR
                        // `ty` is unreliable for generic-struct fields:
                        // it can be an unresolved inference variable, or
                        // the field's bound `Param(n)` (`third: C` on
                        // `Triple<A, B, C>`). In both cases fall through
                        // to the struct's declared field type looked up
                        // via the receiver's MIR `Adt` def, then
                        // substitute any `Param` against the receiver's
                        // concrete type arguments. Without this the field
                        // local defaults to i64/ptr and the `println!`
                        // arg dispatcher mis-stringifies an `f64` (prints
                        // its bit pattern) or strlen's a non-pointer.
                        // The struct's *declared* field type (looked up
                        // via the receiver local's MIR `Adt` def) is
                        // authoritative — the HIR-recorded `ty` is
                        // unreliable here: it can be an unresolved
                        // inference var, a bound `Param(n)`, OR a
                        // wrongly-resolved concrete type (e.g. a
                        // `match Ok(q) => q.bytes` binding where `q`'s
                        // struct type didn't fully propagate and the
                        // field came back as `String` instead of
                        // `[u8]`, sending `.len()` to strlen). Prefer
                        // the declared type whenever the receiver is an
                        // Adt / Tuple with a known field at `pos`,
                        // substituting any generic `Param` against the
                        // receiver's concrete type arguments.
                        let recv_local_ty = self.locals[place.local.0 as usize].ty;
                        let mut walk = recv_local_ty;
                        while let gossamer_types::TyKind::Ref { inner, .. } = self.tcx.kind_of(walk)
                        {
                            walk = *inner;
                        }
                        let pinned_ty = match self.tcx.kind_of(walk).clone() {
                            gossamer_types::TyKind::Adt { def, substs } => {
                                match self
                                    .tcx
                                    .struct_field_tys(def)
                                    .and_then(|tys| tys.get(pos).copied())
                                {
                                    Some(field_ty) => match self.tcx.kind_of(field_ty) {
                                        gossamer_types::TyKind::Param { idx, .. } => substs
                                            .types()
                                            .get(idx.0 as usize)
                                            .copied()
                                            .unwrap_or(field_ty),
                                        _ => field_ty,
                                    },
                                    None => ty,
                                }
                            }
                            gossamer_types::TyKind::Tuple(elems) => {
                                elems.get(pos).copied().unwrap_or(ty)
                            }
                            _ => ty,
                        };
                        place.projection.push(crate::ir::Projection::Field(idx));
                        let dest = self.fresh(pinned_ty);
                        self.emit_assign(
                            Place::local(dest),
                            Rvalue::Use(Operand::Copy(place)),
                            span,
                        );
                        return Some(dest);
                    }
                }
            }
        }

        // Fallback: recurse into the receiver and use its local's
        // recorded struct name (the original path, kept for cases
        // where the receiver is an expression rather than a place
        // — e.g. a call that returns a struct).
        let receiver_local = self.lower_expr(receiver)?;

        // `flags.<long>` for the synthesised `flag::define(...)`
        // aggregate. The per-local layout table records each spec's
        // field index + cell kind, so dispatch lowers to a plain
        // `Field(idx)` projection plus a `flag::Cell::*` runtime tag
        // that the deref `*` later picks up.
        if let Some(field_local) = self.lookup_define_field(receiver_local, &name.name, span) {
            return Some(field_local);
        }

        // Stdlib struct via `local_struct` tag (e.g. `fs::DirInfo`
        // returned from `fs::list_dir`). The receiver was tagged
        // when its element-struct propagated through the
        // `entries[i]` index. Resolve the field name to a
        // positional `Field(idx)` against the registered shape
        // BEFORE the JsonValue fallback fires — otherwise a
        // typechecker-opaque ADT routes through `gos_rt_json_get`
        // and crashes inside serde_json.
        if let Some(struct_name) = self.local_struct.get(&receiver_local).cloned() {
            if let Some(order) = self.structs.get(&struct_name).cloned() {
                if let Some(idx) = order.iter().position(|f| f == name.name.as_str()) {
                    let dest = self.fresh(ty);
                    self.emit_assign(
                        Place::local(dest),
                        Rvalue::Use(Operand::Copy(Place {
                            local: receiver_local,
                            projection: vec![crate::ir::Projection::Field(
                                u32::try_from(idx).unwrap_or(0),
                            )],
                        })),
                        span,
                    );
                    return Some(dest);
                }
            }
        }

        // `value.field` on a `json::Value` receiver — rewrite to a
        // runtime `gos_rt_json_get(value, "field")` call. The
        // result is itself a `json::Value` that downstream code
        // chains further field access / cast through.
        if self.is_json_value_ty(receiver.ty)
            || self.is_json_value_ty(self.locals[receiver_local.0 as usize].ty)
        {
            return Some(self.emit_json_get(receiver_local, &name.name, span));
        }

        // Runtime-kind-aware field access: stdlib types
        // (`http::Response`, `errors::Error`, …) expose
        // `.status`, `.body`, `.message`, `.cause` as
        // field-style access in source even though they're
        // runtime-helper calls under the hood.
        let runtime_kind = self
            .receiver_local_from_path(receiver)
            .and_then(|l| self.local_runtime_kind.get(&l).copied())
            .or_else(|| self.local_runtime_kind.get(&receiver_local).copied());
        if let Some(rk) = runtime_kind {
            let helper: Option<(&'static str, Ty)> = match (rk, name.name.as_str()) {
                ("http::Response", "status") => Some((
                    "gos_rt_http_response_status",
                    self.tcx.int_ty(gossamer_types::IntTy::I64),
                )),
                ("http::Response", "body") => {
                    Some(("gos_rt_http_response_body", self.tcx.string_ty()))
                }
                ("http::Response", "raw_bytes") => {
                    let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
                    Some((
                        "gos_rt_http_response_raw_bytes",
                        self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty)),
                    ))
                }
                ("http::Request", "method") => {
                    Some(("gos_rt_http_request_method", self.tcx.string_ty()))
                }
                ("http::Request", "path") => {
                    Some(("gos_rt_http_request_path", self.tcx.string_ty()))
                }
                ("http::Request", "query") => {
                    Some(("gos_rt_http_request_query", self.tcx.string_ty()))
                }
                ("http::Request", "body") => {
                    Some(("gos_rt_http_request_body_str", self.tcx.string_ty()))
                }
                ("errors::Error", "message") => {
                    Some(("gos_rt_error_message", self.tcx.string_ty()))
                }
                ("errors::Error", "cause") => Some((
                    "gos_rt_error_cause",
                    self.tcx.int_ty(gossamer_types::IntTy::I64),
                )),
                _ => None,
            };
            if let Some((rt_name, ret_ty)) = helper {
                let dest = self.fresh(ret_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(rt_name.to_string())),
                    args: vec![Operand::Copy(Place::local(receiver_local))],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
        }

        let struct_name = self
            .local_struct
            .get(&receiver_local)
            .cloned()
            .or_else(|| self.struct_name_of(receiver.ty))
            .or_else(|| {
                // Last-resort lookup: if exactly one struct in
                // the program defines a field named `name`,
                // assume the receiver is that struct. This
                // recovers field access on receivers whose MIR
                // type was left as Var by the type checker
                // (common for results of `parse_opts()?` /
                // similar patterns where the typer didn't
                // propagate the wrapper's inner generic).
                let mut candidates: Vec<&String> = self
                    .structs
                    .iter()
                    .filter(|(_, fields)| fields.iter().any(|f| f == &name.name))
                    .map(|(n, _)| n)
                    .collect();
                if candidates.len() == 1 {
                    Some(candidates.pop().unwrap().clone())
                } else {
                    None
                }
            });
        let field_order = struct_name
            .as_ref()
            .and_then(|n| self.structs.get(n))
            .cloned();
        let Some(order) = field_order else {
            // Last-resort fallback for opaque receivers: when the
            // receiver type is an unresolved inference variable
            // (`Var`), `Never`, or `Error`, we can't tell what
            // struct it would have been. The single most common
            // shape that lands here is field access on a
            // `json::Value` whose carrier type wasn't pinned by the
            // Type-checker validated this access already. When the
            // MIR receiver type stays opaque (Var / Never / Error)
            // we fall back to the JSON-get path; that produces the
            // right answer for json::Value carriers and a null for
            // genuinely missing fields. Other receiver kinds reach
            // here only on a checker bug — promote to a JSON-get
            // soft fallback so the build still succeeds.
            return Some(self.emit_json_get(receiver_local, &name.name, span));
        };
        if let Some(sname) = struct_name {
            // Tag the receiver so subsequent field accesses
            // and method calls hit the same fallback.
            self.local_struct.insert(receiver_local, sname);
        }
        let idx = order
            .iter()
            .position(|f| f == &name.name)
            .map(|i| u32::try_from(i).expect("field index fits u32"));
        // The type-checker rejects accesses to unknown field
        // names, so this lookup must succeed for any program that
        // reaches MIR. If a future refactor relaxes that check,
        // route the read through `gos_rt_json_get` so the build
        // still produces a value — null for absent fields — rather
        // than refusing to lower.
        let Some(idx) = idx else {
            return Some(self.emit_json_get(receiver_local, &name.name, span));
        };
        let dest = self.fresh(ty);
        let place = Place {
            local: receiver_local,
            projection: vec![crate::ir::Projection::Field(idx)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        Some(dest)
    }
}
