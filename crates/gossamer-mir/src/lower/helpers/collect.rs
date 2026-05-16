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

pub(crate) fn collect_const_values(
    program: &HirProgram,
) -> HashMap<gossamer_resolve::DefId, ConstValue> {
    let mut out = HashMap::new();
    // One forward pass per item: consts written as `4.0 * PI * PI`
    // resolve their `PI` reference against the partial map of
    // already-folded entries, so item-order matters. The frontend
    // emits `const` / `static` items in source order, which is what
    // we want here. Without this, downstream expressions silently
    // default to 0 (the zero-value for their declared type) and
    // benchmarks like nbody print NaN because `4.0 * PI * PI` → 0.
    for item in &program.items {
        let Some(def) = item.def else { continue };
        let init = match &item.kind {
            HirItemKind::Const(decl) => &decl.value,
            // Inline both immutable and mutable statics as their
            // initial values in compiled mode. Writes to mutable
            // statics are no-ops in the compiled tier (lower_assign
            // can't resolve the place), so inlining the initial value
            // is the correct observable behaviour and fixes the
            // "start = " empty-print regression in compiled mode.
            HirItemKind::Static(decl) => &decl.value,
            _ => continue,
        };
        if let Some(value) = const_value_of_expr(init, &out) {
            out.insert(def, value);
        }
    }
    out
}

pub(crate) fn const_value_of_expr(
    expr: &HirExpr,
    known: &HashMap<gossamer_resolve::DefId, ConstValue>,
) -> Option<ConstValue> {
    match &expr.kind {
        HirExprKind::Literal(lit) => Some(literal_to_const(lit)),
        HirExprKind::Path { def: Some(def), .. } => known.get(def).cloned(),
        HirExprKind::Unary {
            op: HirUnaryOp::Neg,
            operand,
        } => match const_value_of_expr(operand, known)? {
            ConstValue::Int(n) => Some(ConstValue::Int(-n)),
            ConstValue::Float(bits) => {
                let f = f64::from_bits(bits);
                Some(ConstValue::Float((-f).to_bits()))
            }
            _ => None,
        },
        HirExprKind::Binary { op, lhs, rhs } => {
            let l = const_value_of_expr(lhs, known)?;
            let r = const_value_of_expr(rhs, known)?;
            fold_binary_const(*op, &l, &r)
        }
        _ => None,
    }
}

pub(crate) fn fold_binary_const(
    op: gossamer_hir::HirBinaryOp,
    lhs: &ConstValue,
    rhs: &ConstValue,
) -> Option<ConstValue> {
    use gossamer_hir::HirBinaryOp as Op;
    match (lhs, rhs) {
        (ConstValue::Int(a), ConstValue::Int(b)) => match op {
            Op::Add => Some(ConstValue::Int(a.checked_add(*b)?)),
            Op::Sub => Some(ConstValue::Int(a.checked_sub(*b)?)),
            Op::Mul => Some(ConstValue::Int(a.checked_mul(*b)?)),
            Op::Div if *b != 0 => Some(ConstValue::Int(a.checked_div(*b)?)),
            Op::Rem if *b != 0 => Some(ConstValue::Int(a.checked_rem(*b)?)),
            Op::BitAnd => Some(ConstValue::Int(a & b)),
            Op::BitOr => Some(ConstValue::Int(a | b)),
            Op::BitXor => Some(ConstValue::Int(a ^ b)),
            _ => None,
        },
        (ConstValue::Float(a), ConstValue::Float(b)) => {
            let af = f64::from_bits(*a);
            let bf = f64::from_bits(*b);
            let result = match op {
                Op::Add => af + bf,
                Op::Sub => af - bf,
                Op::Mul => af * bf,
                Op::Div => af / bf,
                _ => return None,
            };
            Some(ConstValue::Float(result.to_bits()))
        }
        _ => None,
    }
}

pub(crate) fn collect_impl_methods(program: &HirProgram) -> HashMap<String, Option<Ty>> {
    let mut out: HashMap<String, Option<Ty>> = HashMap::new();
    for item in &program.items {
        if let HirItemKind::Impl(decl) = &item.kind {
            if let Some(prefix) = decl.self_name.as_ref() {
                for method in &decl.methods {
                    let mangled = format!("{}::{}", prefix.name, method.name.name);
                    out.insert(mangled, method.ret);
                }
            }
        }
    }
    out
}

pub(crate) fn collect_fn_inputs(program: &HirProgram) -> HashMap<gossamer_resolve::DefId, Vec<Ty>> {
    let mut out = HashMap::new();
    for item in &program.items {
        if let HirItemKind::Fn(decl) = &item.kind {
            if let Some(def) = item.def {
                let inputs: Vec<Ty> = decl.params.iter().map(|p| p.ty).collect();
                out.insert(def, inputs);
            }
        }
    }
    out
}

pub(crate) fn collect_fn_returns(program: &HirProgram) -> HashMap<gossamer_resolve::DefId, Ty> {
    let mut out = HashMap::new();
    for item in &program.items {
        match &item.kind {
            HirItemKind::Fn(decl) => {
                if let Some(def) = item.def {
                    if let Some(ret) = decl.ret {
                        out.insert(def, ret);
                    }
                }
            }
            HirItemKind::Impl(decl) => {
                for method in &decl.methods {
                    if let Some(ret) = method.ret {
                        // Impl methods' def ids live on the
                        // method's name; use the resolver's id
                        // when available. Fallback to no entry.
                        let _ = method;
                        let _ = ret;
                    }
                }
            }
            HirItemKind::Trait(decl) => {
                let _ = decl;
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn collect_struct_fields(
    program: &HirProgram,
) -> (
    HashMap<String, Vec<String>>,
    HashMap<gossamer_resolve::DefId, String>,
) {
    let mut by_name = HashMap::new();
    let mut by_def = HashMap::new();
    for item in &program.items {
        if let HirItemKind::Adt(adt) = &item.kind {
            if let HirAdtKind::Struct(fields) = &adt.kind {
                by_name.insert(
                    adt.name.name.clone(),
                    fields.iter().map(|f| f.name.clone()).collect(),
                );
                if let Some(def) = item.def {
                    by_def.insert(def, adt.name.name.clone());
                }
            }
        }
    }
    for (name, fields) in stdlib_struct_shapes() {
        by_name
            .entry((*name).to_string())
            .or_insert_with(|| fields.iter().map(|f| (*f).to_string()).collect());
    }
    // Mirror the typechecker's sentinel-DefId minting for stdlib
    // structs (see `gossamer-types::checker::stdlib_struct_layout`).
    // Keeps `Adt { def, .. }`-shaped receivers from stdlib paths
    // (e.g. `&fs::DirInfo`) routable through `struct_defs[def] →
    // struct_name → field-name table`.
    for (name, offset) in stdlib_struct_def_offsets() {
        by_def.insert(
            gossamer_resolve::DefId::local(u32::MAX - offset),
            (*name).to_string(),
        );
    }
    (by_name, by_def)
}

pub(crate) fn stdlib_struct_def_offsets() -> &'static [(&'static str, u32)] {
    &[
        ("DirInfo", 2),
        ("Output", 3),
        ("ResponseStream", 4),
        ("Response", 5),
    ]
}

pub(crate) fn stdlib_struct_shapes() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        ("Output", &["stdout", "stderr", "code"]),
        ("ExitStatus", &["code"]),
        (
            "DirEntry",
            &["path", "name", "is_dir", "is_file", "is_symlink"],
        ),
        // `fs::list_dir` returns these — same field order as the
        // interp builtin's `Value::struct_("DirInfo", ...)` and
        // the runtime's `gos_rt_fs_list_dir` blob layout.
        (
            "DirInfo",
            &[
                "name",
                "path",
                "is_file",
                "is_dir",
                "is_symlink",
                "size",
                "modified_ms",
            ],
        ),
        (
            "Civil",
            &[
                "year",
                "month",
                "day",
                "hour",
                "minute",
                "second",
                "offset_seconds",
                "weekday",
            ],
        ),
        ("TestResult", &["name", "passed", "failure_message"]),
        ("Headers", &["pairs"]),
        ("StatusCode", &["code"]),
        ("FetchOptions", &["offline"]),
        ("IoError", &["kind", "message", "context"]),
        // `http::stream` returns these — same field order as the
        // interp's `builtin_http_stream` and the runtime's
        // `gos_rt_http_stream` blob layout.
        ("ResponseStream", &["__handle", "status", "content_type"]),
        // `http::get` / `http::post` return shape — fields are
        // accessed via per-name `gos_rt_http_response_*` helpers,
        // not flat-blob indexing, so adding `raw_bytes` here is
        // purely for source-level field-name lookup.
        (
            "Response",
            &["status", "body", "raw_bytes", "content_type", "location"],
        ),
    ]
}

pub(crate) fn collect_enum_variants(program: &HirProgram) -> EnumIndex {
    let mut by_enum: HashMap<String, Vec<String>> = HashMap::new();
    let mut variant_index: HashMap<String, (String, usize)> = HashMap::new();
    let mut variant_fields: HashMap<String, Vec<String>> = HashMap::new();
    let mut variant_field_tys: HashMap<String, Vec<Ty>> = HashMap::new();
    let mut variant_has_payload: HashMap<String, bool> = HashMap::new();
    for item in &program.items {
        if let HirItemKind::Adt(adt) = &item.kind {
            if let HirAdtKind::Enum(variants) = &adt.kind {
                let names: Vec<String> = variants.iter().map(|v| v.name.name.clone()).collect();
                for (idx, vname) in names.iter().enumerate() {
                    variant_index.insert(vname.clone(), (adt.name.name.clone(), idx));
                    variant_has_payload.entry(vname.clone()).or_insert(false);
                }
                for v in variants {
                    if let Some(fields) = &v.struct_fields {
                        let field_names: Vec<String> =
                            fields.iter().map(|f| f.name.clone()).collect();
                        if !field_names.is_empty() {
                            variant_has_payload.insert(v.name.name.clone(), true);
                        }
                        variant_fields.insert(v.name.name.clone(), field_names);
                    }
                    if let Some(tys) = &v.struct_field_tys {
                        variant_field_tys.insert(v.name.name.clone(), tys.clone());
                    }
                }
                by_enum.insert(adt.name.name.clone(), names);
            }
        }
    }
    // Walk every fn body to discover tuple-payload variants. The
    // HIR strips tuple arity from `HirEnumVariant`, so we infer
    // it by counting args at every variant-constructor call site.
    #[allow(clippy::items_after_statements)]
    pub(crate) fn scan_expr(
        e: &gossamer_hir::HirExpr,
        idx: &HashMap<String, (String, usize)>,
        has_payload: &mut HashMap<String, bool>,
    ) {
        use gossamer_hir::{HirExpr, HirExprKind};
        let recurse = |e: &HirExpr, hp: &mut HashMap<String, bool>| {
            scan_expr(e, idx, hp);
        };
        match &e.kind {
            HirExprKind::Call { callee, args } => {
                recurse(callee, has_payload);
                for a in args {
                    recurse(a, has_payload);
                }
                if let HirExprKind::Path { segments, .. } = &callee.kind {
                    let last = segments.last().map(|s| s.name.as_str());
                    if let Some(name) = last {
                        if idx.contains_key(name) && !args.is_empty() {
                            has_payload.insert(name.to_string(), true);
                        }
                    }
                }
            }
            HirExprKind::Block(b) => {
                for s in &b.stmts {
                    if let gossamer_hir::HirStmtKind::Let { init: Some(i), .. } = &s.kind {
                        recurse(i, has_payload);
                    }
                    if let gossamer_hir::HirStmtKind::Expr { expr, .. } = &s.kind {
                        recurse(expr, has_payload);
                    }
                }
                if let Some(t) = &b.tail {
                    recurse(t, has_payload);
                }
            }
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                recurse(condition, has_payload);
                recurse(then_branch, has_payload);
                if let Some(e) = else_branch {
                    recurse(e, has_payload);
                }
            }
            HirExprKind::Match { scrutinee, arms } => {
                recurse(scrutinee, has_payload);
                for a in arms {
                    recurse(&a.body, has_payload);
                }
            }
            HirExprKind::Loop { body } => recurse(body, has_payload),
            HirExprKind::While { condition, body } => {
                recurse(condition, has_payload);
                recurse(body, has_payload);
            }
            HirExprKind::Binary { lhs, rhs, .. } => {
                recurse(lhs, has_payload);
                recurse(rhs, has_payload);
            }
            HirExprKind::Unary { operand, .. } => recurse(operand, has_payload),
            HirExprKind::Assign { place, value } => {
                recurse(place, has_payload);
                recurse(value, has_payload);
            }
            HirExprKind::MethodCall { receiver, args, .. } => {
                recurse(receiver, has_payload);
                for a in args {
                    recurse(a, has_payload);
                }
            }
            HirExprKind::Field { receiver, .. } => recurse(receiver, has_payload),
            HirExprKind::Index { base, index } => {
                recurse(base, has_payload);
                recurse(index, has_payload);
            }
            HirExprKind::TupleIndex { receiver, .. } => recurse(receiver, has_payload),
            HirExprKind::Tuple(elems) => {
                for e in elems {
                    recurse(e, has_payload);
                }
            }
            HirExprKind::Array(arr) => {
                use gossamer_hir::HirArrayExpr;
                match arr {
                    HirArrayExpr::List(elems) => {
                        for e in elems {
                            recurse(e, has_payload);
                        }
                    }
                    HirArrayExpr::Repeat { value, count } => {
                        recurse(value, has_payload);
                        recurse(count, has_payload);
                    }
                }
            }
            HirExprKind::Cast { value, .. } => recurse(value, has_payload),
            HirExprKind::Return(Some(v)) | HirExprKind::Break(Some(v)) => recurse(v, has_payload),
            _ => {}
        }
    }
    #[allow(clippy::items_after_statements)]
    pub(crate) fn scan_block(
        b: &gossamer_hir::HirBlock,
        idx: &HashMap<String, (String, usize)>,
        hp: &mut HashMap<String, bool>,
    ) {
        for s in &b.stmts {
            if let gossamer_hir::HirStmtKind::Let { init: Some(i), .. } = &s.kind {
                scan_expr(i, idx, hp);
            }
            if let gossamer_hir::HirStmtKind::Expr { expr, .. } = &s.kind {
                scan_expr(expr, idx, hp);
            }
        }
        if let Some(t) = &b.tail {
            scan_expr(t, idx, hp);
        }
    }
    for item in &program.items {
        match &item.kind {
            HirItemKind::Fn(decl) => {
                if let Some(body) = &decl.body {
                    scan_block(&body.block, &variant_index, &mut variant_has_payload);
                }
            }
            HirItemKind::Impl(impl_decl) => {
                for m in &impl_decl.methods {
                    if let Some(body) = &m.body {
                        scan_block(&body.block, &variant_index, &mut variant_has_payload);
                    }
                }
            }
            _ => {}
        }
    }
    EnumIndex {
        by_enum,
        variant_index,
        variant_fields,
        variant_field_tys,
        variant_has_payload,
    }
}

pub(crate) fn collect_item(
    item: &HirItem,
    tcx: &mut TyCtxt,
    structs: &HashMap<String, Vec<String>>,
    struct_defs: &HashMap<gossamer_resolve::DefId, String>,
    enums: &EnumIndex,
    impl_methods: &HashMap<String, Option<Ty>>,
    fn_returns: &HashMap<gossamer_resolve::DefId, Ty>,
    fn_inputs: &HashMap<gossamer_resolve::DefId, Vec<Ty>>,
    consts: &HashMap<gossamer_resolve::DefId, ConstValue>,
    out: &mut Vec<Body>,
) {
    match &item.kind {
        HirItemKind::Fn(decl) => {
            // Cross-module callers route through `Operand::FnRef`
            // keyed by `DefId` (the resolver registers
            // `other::greet` as a `DefKind::Fn` directly), so a
            // single bare-name lowering covers both `greet()` and
            // `other::greet()` call sites. The module-qualified
            // duplicate body is unnecessary.
            if let Some(body) = lower_fn(
                decl,
                item.def,
                item.span,
                tcx,
                structs,
                struct_defs,
                enums,
                impl_methods,
                fn_returns,
                fn_inputs,
                consts,
            ) {
                out.push(body);
            }
        }
        HirItemKind::Impl(decl) => {
            // Mangle each method name to `Struct::method` so calls
            // from `c.bump()` (where `c: Counter`) can dispatch via
            // a stable name without colliding with another impl's
            // identically-named method on a different struct.
            let prefix = decl.self_name.as_ref().map(|n| n.name.clone());
            for method in &decl.methods {
                let mangled: HirFn = if let Some(p) = prefix.clone() {
                    let mut renamed = method.clone();
                    renamed.name = Ident::new(format!("{}::{}", p, method.name.name));
                    renamed
                } else {
                    method.clone()
                };
                if let Some(body) = lower_fn(
                    &mangled,
                    None,
                    item.span,
                    tcx,
                    structs,
                    struct_defs,
                    enums,
                    impl_methods,
                    fn_returns,
                    fn_inputs,
                    consts,
                ) {
                    out.push(body);
                }
            }
        }
        HirItemKind::Trait(decl) => {
            for method in &decl.methods {
                if method.body.is_some() {
                    if let Some(body) = lower_fn(
                        method,
                        None,
                        item.span,
                        tcx,
                        structs,
                        struct_defs,
                        enums,
                        impl_methods,
                        fn_returns,
                        fn_inputs,
                        consts,
                    ) {
                        out.push(body);
                    }
                }
            }
        }
        HirItemKind::Adt(_) | HirItemKind::Const(_) | HirItemKind::Static(_) => {}
    }
}

pub(crate) fn lower_fn(
    decl: &HirFn,
    def: Option<gossamer_resolve::DefId>,
    span: Span,
    tcx: &mut TyCtxt,
    structs: &HashMap<String, Vec<String>>,
    struct_defs: &HashMap<gossamer_resolve::DefId, String>,
    enums: &EnumIndex,
    impl_methods: &HashMap<String, Option<Ty>>,
    fn_returns: &HashMap<gossamer_resolve::DefId, Ty>,
    fn_inputs: &HashMap<gossamer_resolve::DefId, Vec<Ty>>,
    consts: &HashMap<gossamer_resolve::DefId, ConstValue>,
) -> Option<Body> {
    let body = decl.body.as_ref()?;
    let mut builder = Builder::new(
        decl.name.name.clone(),
        span,
        tcx,
        structs,
        struct_defs,
        enums,
        impl_methods,
        fn_returns,
        fn_inputs,
        consts,
    );
    let return_ty = decl.ret.unwrap_or_else(|| builder.tcx.unit());
    builder.push_local(return_ty, None, false);
    let arity = u32::try_from(decl.params.len()).expect("arity overflow");
    for param in &decl.params {
        let local = builder.push_local(
            param.ty,
            param_name(&param.pattern),
            param_mutable(&param.pattern),
        );
        builder.param_locals.insert(local);
        if let HirPatKind::Binding { name, .. } = &param.pattern.kind {
            builder.bind_local(&name.name, local);
            // First-priority signal for the runtime-kind tag: the
            // rendered type of the parameter. Stdlib types like
            // `http::Request` resolve to a `TyKind` whose printer
            // form retains the path, even when the type isn't a
            // user-declared struct (so `struct_name_of` below
            // can't pick it up). Match on the last `::`-segment
            // so the fully-qualified `http::Request` and the
            // short-name `Request` both light up.
            let rendered = gossamer_types::printer::render_ty(builder.tcx, param.ty);
            let last_segment = rendered.rsplit("::").next().unwrap_or(&rendered);
            let runtime_kind_from_type: Option<&'static str> = match last_segment {
                "Request" => Some("http::Request"),
                "Response" => Some("http::Response"),
                "Scanner" => Some("bufio::Scanner"),
                "Client" => Some("http::Client"),
                _ => None,
            };
            // Secondary fallback: parameters named with stdlib-
            // shape-identifying names get the same tag. Covers
            // the case where the type renders as a Var (e.g.
            // when type inference hasn't pinned the parameter to
            // a concrete shape).
            let runtime_kind_from_name: Option<&'static str> = match name.name.as_str() {
                "request" | "req" | "r" | "rq" => Some("http::Request"),
                "response" | "resp" | "rsp" => Some("http::Response"),
                "scanner" => Some("bufio::Scanner"),
                "client" => Some("http::Client"),
                _ => None,
            };
            if let Some(rk) = runtime_kind_from_type.or(runtime_kind_from_name) {
                builder.local_runtime_kind.insert(local, rk);
            }
        }
        // Pre-populate `local_struct` for parameters whose static
        // type resolves to a known named struct so `self.field`
        // (and other `param.field`) accesses inside the body find
        // the struct name without falling through to the
        // unsupported placeholder. The HIR lowerer leaves `self`'s
        // type as Error today, so we also try the impl receiver
        // by inspecting parameter names: a binding called `self`
        // gets the receiver type when `param.ty` doesn't already
        // resolve to one.
        if let Some(struct_name) = builder.struct_name_of(param.ty) {
            // Tag well-known stdlib types via the runtime-kind
            // map so method dispatch on parameters picks the
            // right helper. Maps by struct name; any user struct
            // sharing one of these names overrides this — out
            // of scope for now.
            let runtime_kind: Option<&'static str> = match struct_name.as_str() {
                "Error" => Some("errors::Error"),
                "Response" => Some("http::Response"),
                "Request" => Some("http::Request"),
                "Client" => Some("http::Client"),
                "Scanner" => Some("bufio::Scanner"),
                "Pattern" => Some("regex::Pattern"),
                _ => None,
            };
            builder.local_struct.insert(local, struct_name);
            if let Some(rk) = runtime_kind {
                builder.local_runtime_kind.insert(local, rk);
            }
        }
    }
    let entry = builder.new_block(span);
    builder.set_current(entry);
    let result_local = builder.lower_block(&body.block);
    if let Some(mut result) = result_local {
        if builder.current.is_some() {
            // Same callable-coercion as the explicit `return`
            // arm: a tail-expression that yields a bare fn item
            // when the function declares a callable-shape return
            // gets wrapped into the env+code blob so the caller's
            // slot is uniformly env-shaped.
            use gossamer_types::TyKind;
            let ret_ty = builder.locals[Local::RETURN.0 as usize].ty;
            let value_ty = builder.locals[result.0 as usize].ty;
            let dest_callable = matches!(
                builder.tcx.kind_of(ret_ty),
                TyKind::FnPtr(_) | TyKind::FnTrait(_)
            );
            let src_is_fn_def = matches!(builder.tcx.kind_of(value_ty), TyKind::FnDef { .. });
            let src_names_fn = builder.local_fn_name.contains_key(&result);
            if dest_callable && (src_is_fn_def || src_names_fn) {
                result = builder.coerce_to_fn_trait_if_needed(result, ret_ty, span);
            }
            builder.emit_assign(
                Place::local(Local::RETURN),
                Rvalue::Use(Operand::Copy(Place::local(result))),
                span,
            );
        }
    }
    builder.terminate(Terminator::Return);
    Some(Body {
        name: decl.name.name.clone(),
        def,
        arity,
        locals: builder.locals,
        blocks: builder.blocks,
        span,
    })
}

pub(crate) fn param_name(pattern: &HirPat) -> Option<Ident> {
    match &pattern.kind {
        HirPatKind::Binding { name, .. } => Some(name.clone()),
        _ => None,
    }
}

pub(crate) fn param_mutable(pattern: &HirPat) -> bool {
    matches!(&pattern.kind, HirPatKind::Binding { mutable: true, .. })
}

pub(crate) fn pattern_kind_label(pattern: &HirPat) -> &'static str {
    match &pattern.kind {
        HirPatKind::Wildcard => "wildcard",
        HirPatKind::Binding { .. } => "binding",
        HirPatKind::Literal(_) => "literal",
        HirPatKind::Tuple(_) => "tuple",
        HirPatKind::Or(_) => "or-pattern",
        HirPatKind::Range { .. } => "range",
        HirPatKind::Struct { .. } => "struct",
        HirPatKind::Variant { .. } => "variant",
        HirPatKind::Ref { .. } => "reference",
        HirPatKind::Rest => "rest",
        HirPatKind::At { .. } => "at-binding",
    }
}
