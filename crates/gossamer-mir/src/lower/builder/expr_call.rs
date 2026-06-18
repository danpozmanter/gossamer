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
    pub(crate) fn lower_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // `http::serve(addr, handler)` shortcut: pass the handler's
        // serve method address as a third argument so the runtime
        // can dispatch back into Gossamer code per request.
        if let HirExprKind::Path { segments, .. } = &callee.kind {
            let joined: String = segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::");
            if joined == "http::serve" && args.len() == 2 {
                if let Some(local) = self.lower_http_serve(&args[0], &args[1], ty, span) {
                    return Some(local);
                }
            }
            // `http::serve_tls(addr, cert_pem, key_pem, handler)` - the
            // TLS-terminating variant. Same handler-fn-ptr dispatch as
            // `http::serve`, with the cert + key PEM threaded ahead of
            // the handler.
            if joined == "http::serve_tls" && args.len() == 4 {
                if let Some(local) =
                    self.lower_http_serve_tls(&args[0], &args[1], &args[2], &args[3], ty, span)
                {
                    return Some(local);
                }
            }
            // `websocket::serve(addr, handler)` - same handler-fn-ptr
            // dispatch as `http::serve`, but resolves the handler's
            // `handle(&self, ws: i64)` method and emits `gos_rt_ws_serve`.
            if matches!(
                joined.as_str(),
                "websocket::serve" | "http::websocket::serve"
            ) && args.len() == 2
            {
                if let Some(local) = self.lower_websocket_serve(&args[0], &args[1], ty, span) {
                    return Some(local);
                }
            }
            // `http_h3::serve(addr, cert_path, key_path, handler)` -
            // same handler-fn-ptr dispatch as `http::serve` with two
            // extra leading string args (the TLS keypair file paths).
            if joined == "http_h3::serve" && args.len() == 4 {
                if let Some(local) =
                    self.lower_http3_serve(&args[0], &args[1], &args[2], &args[3], ty, span)
                {
                    return Some(local);
                }
            }
            // `sql::register_native(name, driver)` (autoderive-mangled
            // to `__gos_sql_register_native`): capture the driver's env
            // + `gos_fn_addr("<Type>::dispatch")` so the runtime can
            // dispatch back into the `.gos` driver per op. Same Rust ->
            // Gossamer bridge as `http::serve`.
            if joined == "__gos_sql_register_native" && args.len() == 2 {
                if let Some(local) = self.lower_sql_register_native(&args[0], &args[1], ty, span) {
                    return Some(local);
                }
            }
            // `http::serve_h2c(addr, handler, config)` - ignore the
            // config argument in compiled mode and use the runtime
            // default; reuses the same handler-fn-ptr dispatch as
            // http::serve. Renamed from the original `http2::*`
            // spelling when HTTP/2 was folded into std::http per
            // the Go model (0.4.0).
            if joined == "http::serve_h2c" && args.len() >= 2 {
                if let Some(local) = self.lower_http2_bind_and_run_h2c(&args[0], &args[1], ty, span)
                {
                    return Some(local);
                }
            }
            // `flag::define(name, [flag::int(...), flag::string(...),
            // flag::bool(...)])` - declarative one-shot construction.
            // Expand to the imperative `flag::Set` builder pattern at
            // MIR level so the compiled tier reuses the now-working
            // cell-load helpers.
            if joined == "flag::define" && args.len() == 2 {
                if let Some(local) = self.lower_flag_define(&args[0], &args[1], ty, span) {
                    return Some(local);
                }
            }
            // `Box::new(x)` / `Arc::new(x)` / `Rc::new(x)` are
            // identity wrappers in a fully GC'd language - every
            // value already lives on the GC heap. Without this
            // identity passthrough the call lands on a generic
            // dispatch that returns a typed-zero stub, which then
            // landed as the `rest` payload of a `Cons(_, rest)`
            // and segfaulted the next match arm reading disc off
            // a null pointer (the linked_list reproducer).
            if matches!(joined.as_str(), "Box::new" | "Arc::new" | "Rc::new") && args.len() == 1 {
                return self.lower_expr(&args[0]);
            }
            // `String::new()` / `String::with_capacity(_)` materialise
            // an empty owned String. Gos `String` is the runtime's
            // C-string representation, and the empty literal is the
            // canonical zero value. Without this passthrough the
            // call ends up as a typed-zero stub at codegen and any
            // downstream `push_str` (rewritten to `__concat`) sees
            // a null/garbage receiver instead of a real empty bytes
            // pointer.
            if matches!(joined.as_str(), "String::new" | "String::with_capacity") && args.len() <= 1
            {
                let str_ty = self.tcx.string_ty();
                let dest = self.fresh(str_ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Use(Operand::Const(ConstValue::Str(String::new()))),
                    span,
                );
                return Some(dest);
            }
        }
        // Variant constructor shortcut for `Result<T, E>` and
        // `Option<T>`: `Ok(v)` / `Err(v)` / `Some(v)` lower to
        // a `gos_rt_result_new(disc, payload)` call so the
        // resulting handle carries a real discriminant. Match
        // dispatch and `?`-propagation rely on the disc bit being
        // present at runtime.
        if let HirExprKind::Path { segments, .. } = &callee.kind {
            let last = segments.last().map(|s| s.name.as_str());
            let disc = match last {
                Some("Ok" | "Some") => Some(0),
                Some("Err") => Some(1),
                _ => None,
            };
            if let Some(disc) = disc {
                if args.len() == 1 {
                    return self.lower_result_ctor(disc, &args[0], ty, span);
                }
            }
            // User-defined enum variants with payloads: `List::Cons
            // (v, rest)`. Allocate `[disc, p0, p1, ...]` on the heap
            // and return the pointer. Match dispatch reads disc from
            // offset 0 via `gos_rt_enum_disc`.
            if !args.is_empty()
                && let Some((enum_name, idx)) = self.enums.lookup(segments)
            {
                let result = self.lower_user_enum_ctor(
                    &enum_name,
                    u32::try_from(idx).unwrap_or(0),
                    args,
                    ty,
                    span,
                );
                // Tag the result local with its enum name. The checker leaves a
                // variant constructor's type a `Var`, so `==` / `.method()` /
                // `{:?}` on the value (or a `let`-bound copy of it, propagated
                // by the Let lowering) recover the enum from `local_struct`.
                if let Some(local) = result {
                    self.local_struct.insert(local, enum_name);
                }
                return result;
            }
            // F#-style iter / option / result combinator surface
            // (SPEC §10.4 / §10.4a / §10.4b). Data-last; closures
            // pass through `coerce_to_fn_trait_if_needed` so the
            // unified callable infra builds a real env pointer.
            let joined: String = segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(local) = self.try_lower_iter_call(&joined, args, ty, span) {
                return Some(local);
            }
            if let Some(local) = self.try_lower_option_call(&joined, args, ty, span) {
                return Some(local);
            }
            if let Some(local) = self.try_lower_combinator_call(&joined, args, ty, span) {
                return Some(local);
            }
        }
        // When the callee's `DefId` is known and its declared
        // return type is on record, prefer the callee's return
        // type over the call-expression's HIR type - the latter
        // may still be an inference variable.
        let ty = if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
            // Prefer the callee's declared return type over the
            // call-expression's HIR type when available; the
            // checker often leaves the latter as an inference
            // variable.
            use gossamer_types::TyKind;
            if let Some(registered) = self.fn_returns.get(def).copied() {
                if matches!(self.tcx.kind_of(registered), TyKind::Error) {
                    ty
                } else {
                    registered
                }
            } else {
                ty
            }
        } else {
            ty
        };
        // Pin the call's dest type for known stdlib path callees
        // whose return kind is fixed. The typechecker leaves most
        // stdlib call-expression types as `Var` because no impl
        // index tracks them; the codegen then defaults to pointer-
        // or int-typed registers. Fix the printable kind here.
        let ty = {
            use gossamer_types::TyKind;
            if let HirExprKind::Path {
                segments,
                def: None,
                ..
            } = &callee.kind
            {
                let joined = segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                if matches!(self.tcx.kind_of(ty), TyKind::Error | TyKind::Var(_)) {
                    match joined.as_str() {
                        "math::sqrt" | "math::sin" | "math::cos" | "math::ln" | "math::log"
                        | "math::exp" | "math::abs" | "math::floor" | "math::ceil"
                        | "math::pow" | "time::now" => {
                            self.tcx.float_ty(gossamer_types::FloatTy::F64)
                        }
                        "time::now_ns" | "time::now_ms" | "strconv::parse_i64"
                        | "strconv::parse_int" | "strconv::atoi" | "gos_rt_math_sqrt" => {
                            self.tcx.int_ty(gossamer_types::IntTy::I64)
                        }
                        // String-returning stdlib helpers. The
                        // runtime returns a `*mut c_char` which the
                        // codegen needs to know is a String so
                        // `.len()` / `.trim()` / `.as_bytes()` etc.
                        // dispatch to the `gos_rt_str_*` family
                        // instead of the generic `gos_rt_len`. The
                        // typechecker leaves these as `Var` because
                        // it doesn't index stdlib free functions;
                        // pin here as the last grounding step.
                        "fs::read_to_string"
                        | "std::fs::read_to_string"
                        | "path::join"
                        | "std::path::join"
                        | "io::read_line"
                        | "std::io::read_line"
                        | "format" => self.tcx.string_ty(),
                        _ => ty,
                    }
                } else {
                    ty
                }
            } else {
                ty
            }
        };
        // When the callee is a single-segment Path bound to a
        // local whose static type is a callable (`FnPtr` /
        // `FnTrait`), and the call expression's HIR type is
        // unresolved, extract the return type from the callee
        // signature directly. Without this, `add5(3)` (for
        // `add5: fn(i64) -> i64`) leaves the result as an
        // inference variable, which the print path then treats
        // as String - producing a `strlen` segfault on the i64
        // bit pattern returned from the closure body.
        let ty = {
            use gossamer_types::TyKind;
            if matches!(self.tcx.kind_of(ty), TyKind::Error | TyKind::Var(_)) {
                if let HirExprKind::Path {
                    segments,
                    def: None,
                    ..
                } = &callee.kind
                {
                    if segments.len() == 1 {
                        if let Some(local) = self.lookup_local(&segments[0].name) {
                            let local_ty = self.locals[local.0 as usize].ty;
                            match self.tcx.kind_of(local_ty) {
                                TyKind::FnPtr(sig) | TyKind::FnTrait(sig) => sig.output,
                                TyKind::FnDef { def, substs } => {
                                    let _ = substs;
                                    self.fn_returns.get(def).copied().unwrap_or(ty)
                                }
                                _ => ty,
                            }
                        } else {
                            ty
                        }
                    } else {
                        ty
                    }
                } else {
                    ty
                }
            } else {
                ty
            }
        };
        if let Some(local) = self.lower_struct_call(callee, args, ty, span) {
            return Some(local);
        }
        // Free-function `json::*` calls that route to runtime
        // helpers. Detect by joined path so the same lowering fires
        // whether the user wrote `use std::encoding::json` and
        // `json::parse(...)` or the fully-qualified
        // `std::encoding::json::parse(...)` form.
        if let Some(local) = self.lower_json_free_call(callee, args, span) {
            return Some(local);
        }
        // External Rust binding (`tuigoose::layout::rect`, etc.).
        // Resolves through `gossamer_resolve::external` populated
        // either by the runner's `install_all` or the build-time
        // `ensure_signatures` pass.
        if let Some(local) = self.lower_external_binding_call(callee, args, span) {
            return Some(local);
        }
        // Same for the rest of the stdlib that maps cleanly to
        // a single runtime helper (errors, regex, fs, path,
        // bufio, http, gzip, slog, testing, …).
        if let Some(local) = self.lower_stdlib_free_call(callee, args, span) {
            return Some(local);
        }
        // If the callee is a bare path that resolves to a local
        // previously registered as a lifted closure, dispatch
        // statically to that closure's top-level function and pass
        // the env pointer as the implicit first argument.
        if let HirExprKind::Path {
            segments,
            def: None,
            ..
        } = &callee.kind
        {
            if segments.len() == 1 {
                if let Some(local) = self.lookup_local(&segments[0].name) {
                    if let Some(fn_name) = self.local_closure.get(&local).cloned() {
                        let mut arg_operands = Vec::with_capacity(args.len() + 1);
                        arg_operands.push(Operand::Copy(Place::local(local)));
                        for arg in args {
                            let a = self.lower_expr(arg)?;
                            let a = self.auto_deref_cell(a, span);
                            arg_operands.push(Operand::Copy(Place::local(a)));
                        }
                        let dest = self.fresh(ty);
                        let next = self.new_block(span);
                        self.terminate(Terminator::Call {
                            callee: Operand::Const(ConstValue::Str(fn_name)),
                            args: arg_operands,
                            destination: Place::local(dest),
                            target: Some(next),
                        });
                        self.set_current(next);
                        return Some(dest);
                    }
                }
            }
        }
        // Pre-compute the joined path name for impl-method detection
        // and destination-type pinning (used twice below).
        let joined_path = match &callee.kind {
            HirExprKind::Path { segments, .. } => Some(
                segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::"),
            ),
            _ => None,
        };
        // If the callee is an impl-method path, pin `ty` to the
        // method's declared return type (the resolver doesn't track
        // impl methods, so the call expression's HIR type is often
        // an unresolved variable for this case).
        let ty = if let Some(name) = joined_path.as_ref() {
            if let Some(Some(ret)) = self.impl_methods.get(name).copied() {
                use gossamer_types::TyKind;
                if matches!(self.tcx.kind_of(ty), TyKind::Error | TyKind::Var(_)) {
                    ret
                } else {
                    ty
                }
            } else {
                ty
            }
        } else {
            ty
        };
        let callee_operand = match &callee.kind {
            HirExprKind::Path { def: Some(def), .. }
                if joined_path
                    .as_ref()
                    .is_some_and(|n| self.impl_methods.contains_key(n)) =>
            {
                let _ = def;
                let name = joined_path
                    .clone()
                    .expect("joined_path guarded by `is_some_and` above");
                Operand::Const(ConstValue::Str(name))
            }
            HirExprKind::Path { def: Some(def), .. } => Operand::FnRef {
                def: *def,
                substs: self.substs_of(callee.ty),
            },
            HirExprKind::Path {
                segments,
                def: None,
                ..
            } => {
                // Only treat a bare local as an indirect closure
                // callee when it came from a function parameter.
                // Other locals (e.g. bound to `Const(Str(name))`
                // by a `let f = bare_name`) still flow through the
                // by-name callee lookup so the direct dispatch path
                // resolves them to the named function body.
                if segments.len() == 1 {
                    if let Some(local) = self.lookup_local(&segments[0].name) {
                        use gossamer_types::TyKind;
                        // Prefer the recorded function-name binding
                        // when the local holds a `Const(Str(name))`
                        // (e.g. `let plus = __closure_0; plus(...)`).
                        // Falling back to the segment name alone
                        // loses the pointer to the synthesised body.
                        if let Some(name) = self.local_fn_name.get(&local).cloned() {
                            Operand::Const(ConstValue::Str(name))
                        } else if self.param_locals.contains(&local) {
                            Operand::Copy(Place::local(local))
                        } else if matches!(
                            self.tcx.kind_of(self.locals[local.0 as usize].ty),
                            TyKind::FnPtr(_)
                                | TyKind::FnDef { .. }
                                | TyKind::Closure { .. }
                                | TyKind::FnTrait(_)
                        ) {
                            // Local bound to a function-typed value
                            // (e.g. returned from `make_counter()`).
                            // Call it indirectly through the local.
                            Operand::Copy(Place::local(local))
                        } else {
                            Operand::Const(ConstValue::Str(segments[0].name.clone()))
                        }
                    } else {
                        Operand::Const(ConstValue::Str(segments[0].name.clone()))
                    }
                } else {
                    Operand::Const(ConstValue::Str(
                        segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join("::"),
                    ))
                }
            }
            _ => {
                let local = self.lower_expr(callee)?;
                Operand::Copy(Place::local(local))
            }
        };
        // Look up the callee's parameter types so we can apply
        // Fn-trait coercions per arg position. The call site of
        // `apply(f: Fn(i64) -> i64, ...)` with `f = bare_fn` needs
        // to wrap `bare_fn`'s code address into the env+code
        // shape; the call site of `apply(f, ...)` with `f` already
        // a closure (env-shaped) is a no-op.
        let callee_param_tys: Option<Vec<Ty>> = match &callee.kind {
            HirExprKind::Path { def: Some(def), .. } => self.fn_inputs.get(def).cloned(),
            _ => None,
        };
        // `__concat` carries every `println!` / `format!` argument. A struct
        // argument with a derived `Type::fmt` is rendered to a String first so
        // the compiled tiers can format it (they cannot print an aggregate).
        let callee_is_concat = matches!(
            &callee.kind,
            HirExprKind::Path { segments, .. }
                if segments.len() == 1 && segments[0].name.as_str() == "__concat"
        );
        let mut arg_operands = Vec::with_capacity(args.len());
        // `&mut <bare-local>` arguments of a writeback type (scalar / String)
        // lower to a by-slot-address `Rvalue::Ref`. After the call the callee
        // may have stored a new value through that address; reload the local
        // from `*ref` so the caller's binding sees it. This is mandatory on the
        // Cranelift tier - its locals are SSA Variables with no machine
        // address, so the Ref materialises a throwaway stack slot the callee
        // writes into; without the reload the round-trip is lost. On the LLVM
        // tier the local is alloca-backed and the reload re-reads the same
        // alloca (a harmless no-op). Recorded as (place_local, ref_local).
        let mut mut_ref_reloads: Vec<(Local, Local)> = Vec::new();
        for (idx, arg) in args.iter().enumerate() {
            // Detect a `&mut <bare local>` of a writeback type before lowering;
            // the matching `Rvalue::Ref` emission lives in `lower_unary`.
            let reload_target = self.mut_ref_reload_target(arg);
            let local = self.lower_expr(arg)?;
            if let Some(place_local) = reload_target {
                mut_ref_reloads.push((place_local, local));
            }
            // 0.7.0 flag::Cell auto-deref at the user-fn call
            // boundary - matches the VM tier's behaviour so
            // `f(flags.output)` passes the unwrapped value.
            let local = self.auto_deref_cell(local, span);
            // Wrap when the source MIR local holds a raw code
            // address (named fn item, lifted closure name, or a
            // `let f = some_fn`). Capturing closures registered
            // in `local_closure` are env_ptr-shaped already and
            // skip this path.
            let in_closure_map = self.local_closure.contains_key(&local);
            let in_fn_name_map = self.local_fn_name.contains_key(&local);
            let local_ty = self.locals[local.0 as usize].ty;
            let local_kind_is_fn = matches!(
                self.tcx.kind_of(local_ty),
                gossamer_types::TyKind::FnDef { .. } | gossamer_types::TyKind::FnPtr(_)
            );
            let arg_is_fn_item = !in_closure_map
                && (in_fn_name_map
                    || local_kind_is_fn
                    || matches!(&arg.kind, HirExprKind::Path { def: Some(_), .. }));
            let local = if arg_is_fn_item {
                if let Some(params) = callee_param_tys.as_ref() {
                    if let Some(expected) = params.get(idx).copied() {
                        self.coerce_to_fn_trait_if_needed(local, expected, span)
                    } else {
                        local
                    }
                } else {
                    local
                }
            } else {
                local
            };
            // Coerce flat Array { T, N } to a proper GosVec when the
            // callee's parameter expects Vec<T> / Slice<T>.  Without this
            // a literal like `[1,2,3]` bound as `Array{i64,3}` passes a
            // flat-buffer pointer to a function that calls gos_rt_vec_get
            // on it, reading element[0]=1 as the GosVec length and then
            // going out of bounds or segfaulting.
            let local = {
                use gossamer_types::TyKind;
                let local_ty = self.locals[local.0 as usize].ty;
                let expected_opt = callee_param_tys.as_ref().and_then(|p| p.get(idx).copied());
                // See through a `&` borrow on both sides. In the GC aliasing
                // model `&[T]` and `[T]` share a representation, so a `&[T]` /
                // `&Vec<T>` parameter is `Ref { Slice/Vec }`; and an inline
                // `[T; N]` array borrowed as `&[T]` (e.g. `f(&xs)` where
                // `let xs = [1,2,3]`) reaches here as `Ref { Array }`. Both
                // must still trigger the array→GosVec coercion, or the callee
                // reads the inline flat buffer as a GosVec header (garbage /
                // segfault). Without unwrapping the ref, the coercion fired
                // only for by-value `[T]`/`Vec` params and silently skipped
                // every `&[T]` parameter.
                let deref = |b: &Self, t: Ty| match b.tcx.kind_of(t) {
                    TyKind::Ref { inner, .. } => *inner,
                    _ => t,
                };
                let local_inner = deref(self, local_ty);
                if let TyKind::Array { elem, len } = self.tcx.kind_of(local_inner).clone() {
                    if let Some(expected) = expected_opt {
                        let expected_inner = deref(self, expected);
                        // A const generic array parameter (`[T; N]`) is carried
                        // by the callee as a runtime-length sequence, so a
                        // concrete-length array argument is coerced to a GosVec
                        // just like a `Vec<T>` / `[T]` parameter.
                        let expected_is_const_array = matches!(
                            self.tcx.kind_of(expected_inner),
                            TyKind::Array {
                                len: gossamer_types::ArrayLen::Param(_),
                                ..
                            }
                        );
                        if matches!(
                            self.tcx.kind_of(expected_inner),
                            TyKind::Vec(_) | TyKind::Slice(_)
                        ) || expected_is_const_array
                        {
                            // A `&[T]` parameter borrows: the caller's array
                            // outlives the call and reclaims its element
                            // children at its own drop. Build a non-owning
                            // view so the coerced slice never deep-frees
                            // those children. A by-value `Vec<T>` / `[T]`
                            // parameter takes ownership, so keep the owning
                            // copy.
                            let param_is_borrow =
                                matches!(self.tcx.kind_of(expected), TyKind::Ref { .. });
                            if param_is_borrow {
                                self.coerce_borrow_array_to_vec(local, elem, len, span)
                            } else {
                                self.coerce_array_to_vec(local, elem, len, span)
                            }
                        } else {
                            local
                        }
                    } else {
                        local
                    }
                } else {
                    local
                }
            };
            // Debug routing: render a struct/enum `__concat` argument through
            // its derived `Type::fmt` (a String), so a `println!("{:?}", s)`
            // compiles on Cranelift / LLVM instead of bailing on an aggregate.
            let local = if callee_is_concat {
                let arg_ty = self.locals[local.0 as usize].ty;
                match self
                    .adt_dispatch_name(arg_ty)
                    .or_else(|| self.local_struct.get(&local).cloned())
                {
                    Some(sname) if self.impl_methods.contains_key(&format!("{sname}::fmt")) => {
                        let str_ty = self.tcx.string_ty();
                        let dest = self.fresh(str_ty);
                        let next = self.new_block(span);
                        self.terminate(Terminator::Call {
                            callee: Operand::Const(ConstValue::Str(format!("{sname}::fmt"))),
                            args: vec![Operand::Copy(Place::local(local))],
                            destination: Place::local(dest),
                            target: Some(next),
                        });
                        self.set_current(next);
                        dest
                    }
                    _ => local,
                }
            } else {
                local
            };
            arg_operands.push(Operand::Copy(Place::local(local)));
        }
        // When an inline closure literal is the callee (e.g. `x |> |s| f(s)`),
        // `ty` may still be the FnPtr type of the closure rather than its
        // return type. Resolve the return type from the callee operand's sig.
        let ty = {
            use gossamer_types::TyKind;
            if matches!(
                self.tcx.kind_of(ty),
                TyKind::Var(_) | TyKind::Error | TyKind::FnPtr(_) | TyKind::FnTrait(_)
            ) {
                if let Operand::Copy(Place {
                    local: callee_local,
                    projection,
                }) = &callee_operand
                {
                    if projection.is_empty() {
                        let callee_local_ty = self.locals[callee_local.0 as usize].ty;
                        match self.tcx.kind_of(callee_local_ty) {
                            TyKind::FnPtr(sig) | TyKind::FnTrait(sig) => {
                                let out = sig.output;
                                if matches!(self.tcx.kind_of(out), TyKind::Var(_) | TyKind::Error) {
                                    ty
                                } else {
                                    out
                                }
                            }
                            _ => ty,
                        }
                    } else {
                        ty
                    }
                } else {
                    ty
                }
            } else {
                ty
            }
        };
        let dest = self.fresh(ty);
        // Pre-register the destination's struct name so subsequent
        // `dest.field` projections resolve to a concrete struct
        // even when the type checker leaves the call's HIR type
        // partially elaborated.
        if let Some(sname) = self.struct_name_of(ty) {
            self.local_struct.insert(dest, sname);
        }
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: callee_operand,
            args: arg_operands,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        for (place_local, ref_local) in mut_ref_reloads {
            self.emit_assign(
                Place::local(place_local),
                Rvalue::Use(Operand::Copy(Place {
                    local: ref_local,
                    projection: vec![crate::ir::Projection::Deref],
                })),
                span,
            );
        }
        Some(dest)
    }

    /// For a `&mut <bare local>` argument of a writeback type (scalar /
    /// `String`), returns the borrowed local - the destination of the
    /// post-call `place = *ref` reload. Mirrors the `Rvalue::Ref` emission
    /// gate in `lower_unary`: only `&mut` over a place-expr of a scalar or
    /// `String` operand takes a slot address. A bare path that *forwards* an
    /// existing `&mut` parameter is already a pointer and needs no reload, so
    /// only the explicit `&mut <local>` form qualifies.
    fn mut_ref_reload_target(&self, arg: &HirExpr) -> Option<Local> {
        use gossamer_types::TyKind;
        let HirExprKind::Unary {
            op: HirUnaryOp::RefMut,
            operand,
        } = &arg.kind
        else {
            return None;
        };
        let writeback = matches!(
            self.tcx.kind_of(operand.ty),
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char | TyKind::String
        );
        if !writeback {
            return None;
        }
        let HirExprKind::Path { segments, .. } = &operand.kind else {
            return None;
        };
        let [seg] = segments.as_slice() else {
            return None;
        };
        self.lookup_local(&seg.name)
    }
}
