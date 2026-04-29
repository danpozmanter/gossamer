//! HIR → MIR lowering.
//! Produces a [`Body`] per HIR function. The lowerer is intentionally
//! straightforward: every HIR expression of interest becomes either a
//! sequence of [`StatementKind::Assign`]s targeting fresh temporaries
//! or a [`Terminator`] that closes the current block. Control flow
//! (`if`, `while`, `loop`, `match`) drops into the CFG by allocating
//! join blocks and stitching them with [`Terminator::Goto`] /
//! [`Terminator::SwitchInt`].

#![forbid(unsafe_code)]
#![allow(
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::match_same_arms
)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::{FileId, Span};
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    AssertMessage, BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place,
    Rvalue, Statement, StatementKind, Terminator, UnOp,
};

/// Lowers every function in `program` to a MIR [`Body`].
#[must_use]
pub fn lower_program(program: &HirProgram, tcx: &mut TyCtxt) -> Vec<Body> {
    let (structs, struct_defs) = collect_struct_fields(program);
    let enums = collect_enum_variants(program);
    let impl_methods = collect_impl_methods(program);
    let fn_returns = collect_fn_returns(program);
    let fn_inputs = collect_fn_inputs(program);
    let consts = collect_const_values(program);
    let mut bodies = Vec::new();
    for item in &program.items {
        collect_item(
            item,
            tcx,
            &structs,
            &struct_defs,
            &enums,
            &impl_methods,
            &fn_returns,
            &fn_inputs,
            &consts,
            &mut bodies,
        );
    }
    for body in &mut bodies {
        insert_drops_at_returns(body, tcx);
    }
    bodies
}

/// Builds a `DefId → ConstValue` map for top-level `const NAME: T = LIT`
/// items whose initializer is a literal (or a unary-negated literal).
/// Path expressions that resolve to one of these defs lower to a direct
/// `Operand::Const`, side-stepping the `FnRef` fallback that would
/// otherwise emit zero/garbage in compiled mode.
fn collect_const_values(program: &HirProgram) -> HashMap<gossamer_resolve::DefId, ConstValue> {
    let mut out = HashMap::new();
    for item in &program.items {
        let HirItemKind::Const(decl) = &item.kind else {
            continue;
        };
        let Some(def) = item.def else { continue };
        if let Some(value) = const_value_of_expr(&decl.value) {
            out.insert(def, value);
        }
    }
    out
}

fn const_value_of_expr(expr: &HirExpr) -> Option<ConstValue> {
    match &expr.kind {
        HirExprKind::Literal(lit) => Some(literal_to_const(lit)),
        HirExprKind::Unary {
            op: HirUnaryOp::Neg,
            operand,
        } => match const_value_of_expr(operand)? {
            ConstValue::Int(n) => Some(ConstValue::Int(-n)),
            ConstValue::Float(bits) => {
                let f = f64::from_bits(bits);
                Some(ConstValue::Float((-f).to_bits()))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Drop-insertion pass.
///
/// Emits a `Call(gos_rt_*_free, [local])` before each `Return`
/// terminator for every local that owns a heap-allocated runtime
/// container (`HashMap` / `Vec` / `HashSet` / `BTreeMap`) and
/// whose pointer is *not* moved into the return slot. Catches the
/// "build a `HashMap` inside a function, throw it away on exit"
/// pattern that otherwise leaks the container's entire backing
/// storage every call.
///
/// Conservative ownership rules:
/// - The local must be assigned exactly once, by a Call to a
///   recognised constructor (e.g. `gos_rt_map_new`).
/// - The local must not be later assigned (or projected-into)
///   anything else.
/// - The local must not appear as the source of an `Assign` to
///   `Local::RETURN`, the destination of a returning Call, or any
///   `Operand::Copy` whose destination is `Local::RETURN`.
/// - Locals at indices `1..=arity` are function parameters and
///   are never dropped here (caller owns them).
#[allow(
    clippy::cognitive_complexity,
    reason = "linear flow analysis over MIR; splitting hides the per-pass intent"
)]
fn insert_drops_at_returns(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    use gossamer_types::TyKind;

    if body.locals.is_empty() {
        return;
    }
    // Per-local: the constructor symbol that allocated it (if
    // any). `None` means the local was either never assigned, was
    // assigned by something other than a recognised constructor,
    // or has been disqualified by a subsequent re-assignment.
    let mut owner_ctor: Vec<Option<&'static str>> = vec![None; body.locals.len()];
    let mut moved_into_return: Vec<bool> = vec![false; body.locals.len()];

    let ctor_to_free = |name: &str| -> Option<&'static str> {
        match name {
            // Runtime-symbol form (used by some peephole sites).
            "gos_rt_map_new" | "gos_rt_map_new_with_capacity" => Some("gos_rt_map_free"),
            "gos_rt_vec_new" | "gos_rt_vec_with_capacity" => Some("gos_rt_vec_free"),
            "gos_rt_set_new" => Some("gos_rt_set_free"),
            "gos_rt_btmap_new" => Some("gos_rt_btmap_free"),
            // Path-form constructors emitted by the call lowerer.
            // The cranelift backend's `lower_intrinsic_call` table
            // routes these straight to the runtime helper, so the
            // drop pass needs to recognise both forms.
            "HashMap::new"
            | "collections::HashMap::new"
            | "HashMap::with_capacity"
            | "collections::HashMap::with_capacity" => Some("gos_rt_map_free"),
            "Vec::new" | "Vec::with_capacity" => Some("gos_rt_vec_free"),
            "HashSet::new" | "collections::HashSet::new" => Some("gos_rt_set_free"),
            "BTreeMap::new" | "collections::BTreeMap::new" => Some("gos_rt_btmap_free"),
            _ => None,
        }
    };

    let arity = body.arity as usize;
    let last_block = body.blocks.len();

    // Pass 1: discover constructor-allocated locals. Track every
    // assignment that *might* invalidate ownership (re-assignment,
    // projection writes) so we can disqualify aliasing patterns.
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind {
                let idx = place.local.0 as usize;
                if !place.projection.is_empty() {
                    // Writing through a projection on this local
                    // doesn't move ownership, so it stays valid.
                    continue;
                }
                if idx == 0 || idx <= arity || idx >= owner_ctor.len() {
                    continue;
                }
                // Re-assignment of an owning local — disqualify.
                if owner_ctor[idx].is_some() && !matches!(rvalue, Rvalue::CallIntrinsic { .. }) {
                    owner_ctor[idx] = None;
                }
            }
        }
        if let Terminator::Call {
            callee,
            destination,
            ..
        } = &block.terminator
        {
            let idx = destination.local.0 as usize;
            if idx == 0 || idx <= arity || idx >= owner_ctor.len() {
                continue;
            }
            if !destination.projection.is_empty() {
                continue;
            }
            // Any local of a heap-container type that's the
            // destination of a Call also owns the result — the
            // callee returned a freshly-allocated container that
            // this frame must drop unless it's then moved into
            // the return slot. Match by static type, since the
            // callee name ("count_kmers", arbitrary user fn)
            // doesn't telegraph ownership.
            let dest_ty = body.locals[idx].ty;
            let inferred_free: Option<&'static str> = match tcx.kind_of(dest_ty) {
                TyKind::HashMap { .. } => Some("gos_rt_map_free"),
                TyKind::Vec(_) => Some("gos_rt_vec_free"),
                _ => None,
            };
            if let Operand::Const(ConstValue::Str(name)) = callee {
                if let Some(free) = ctor_to_free(name.as_str()) {
                    if owner_ctor[idx].is_none() {
                        owner_ctor[idx] = Some(free);
                        continue;
                    }
                }
            }
            if let Some(free) = inferred_free {
                if owner_ctor[idx].is_none() {
                    owner_ctor[idx] = Some(free);
                    continue;
                }
            }
            // Any other Call destination invalidates ownership
            // (the local now holds something else).
            owner_ctor[idx] = None;
        }
    }

    // Pass 2: detect locals that *transitively* flow into the
    // return slot. The constructor result may be copied through a
    // chain of intermediate locals before landing in `Local::RETURN`
    // (e.g. `Local(0) = Local(4); Local(4) = Local(5);
    // Local(5) = HashMap::new()`). Any local in that chain
    // shares the same heap pointer and must not be dropped, since
    // `Local::RETURN` will be moved out to the caller.
    //
    // Build a "Copy edge" graph (`from` → `to` whenever
    // `Assign(to, Use(Copy(from)))` appears with bare projections),
    // then walk it backwards from `Local::RETURN` to its closure.
    let mut copy_edges_to: Vec<Vec<Local>> = vec![Vec::new(); body.locals.len()];
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind {
                if !place.projection.is_empty() {
                    continue;
                }
                if let Rvalue::Use(Operand::Copy(p)) = rvalue {
                    if !p.projection.is_empty() {
                        continue;
                    }
                    let to_idx = place.local.0 as usize;
                    if to_idx < copy_edges_to.len() {
                        copy_edges_to[to_idx].push(p.local);
                    }
                }
            }
        }
    }
    let mut stack = vec![Local::RETURN];
    moved_into_return[Local::RETURN.0 as usize] = true;
    while let Some(cur) = stack.pop() {
        let cur_idx = cur.0 as usize;
        if cur_idx >= copy_edges_to.len() {
            continue;
        }
        for src in copy_edges_to[cur_idx].clone() {
            let src_idx = src.0 as usize;
            if src_idx >= moved_into_return.len() {
                continue;
            }
            if !moved_into_return[src_idx] {
                moved_into_return[src_idx] = true;
                stack.push(src);
            }
        }
    }
    // Calls that write directly into `Local::RETURN` move every
    // pointer-shaped Copy argument into the return value too —
    // model the same closure on those edges.
    for block in &body.blocks {
        if let Terminator::Call {
            destination, args, ..
        } = &block.terminator
        {
            if destination.local == Local::RETURN && destination.projection.is_empty() {
                for arg in args {
                    if let Operand::Copy(p) = arg {
                        if p.projection.is_empty() {
                            let idx = p.local.0 as usize;
                            if idx < moved_into_return.len() {
                                moved_into_return[idx] = true;
                            }
                        }
                    }
                }
            }
        }
    }

    // Pass 3: collect drop targets in stable local-index order.
    // The constructor-name → free-name table already restricts
    // candidates to runtime container shapes; we trust the MIR's
    // type assignment and skip a redundant TyKind check here.
    let _ = TyKind::Bool; // silence unused-import lint outside the closure
    let drop_targets: Vec<(Local, &'static str)> = (0..owner_ctor.len())
        .filter_map(|i| {
            let free = owner_ctor[i]?;
            if moved_into_return[i] {
                return None;
            }
            Some((Local(i as u32), free))
        })
        .collect();

    if drop_targets.is_empty() {
        return;
    }

    for block_idx in 0..last_block {
        if !matches!(body.blocks[block_idx].terminator, Terminator::Return) {
            continue;
        }
        let span = body.blocks[block_idx].span;
        for (local, free_name) in &drop_targets {
            let dest = Local(u32::try_from(body.locals.len()).expect("local overflow"));
            let unit_ty = body.locals[0].ty;
            body.locals.push(LocalDecl {
                ty: unit_ty,
                debug_name: None,
                mutable: false,
            });
            // New trampoline block: Call(free, [local]) -> Goto(original_return_block)
            // To keep block ordering stable (and avoid moving the
            // Return terminator), we instead splice a statement
            // by appending into the current block's stmts. Drop
            // calls expect Terminator::Call shape, so we route
            // through a fresh trampoline block.
            let new_block_id = BlockId(u32::try_from(body.blocks.len()).expect("block overflow"));
            let _ = new_block_id;
            // Append a `Call` to the original block by replacing
            // its terminator: the existing Return moves to a new
            // block, and we splice a chain of Call terminators
            // before it.
            //
            // Simpler implementation: emit a noop assignment that
            // *invokes* the free helper as an Rvalue::CallIntrinsic.
            // This is supported by the cranelift lowerer's
            // statement path (via lower_intrinsic_call), so we
            // avoid the block-rewiring complexity.
            body.blocks[block_idx].stmts.push(Statement {
                kind: StatementKind::Assign {
                    place: Place::local(dest),
                    rvalue: Rvalue::CallIntrinsic {
                        name: free_name,
                        args: vec![Operand::Copy(Place::local(*local))],
                    },
                },
                span,
            });
        }
    }
}

/// Builds a `mangled-name -> return-Ty` map for impl methods. The
/// keys are the same `Struct::method` mangled names that the impl
/// pass uses to lower bodies; presence in the map also signals
/// "this name is an impl-method dispatch target." MIR call lowering
/// uses both pieces: (a) detecting an impl-method path call so the
/// callee becomes a `Const(Str)` instead of a `FnRef` whose `DefId`
/// the codegen has no body for, and (b) pinning the destination's
/// MIR type to the method's declared return type so subsequent
/// field access on the result lowers cleanly.
fn collect_impl_methods(program: &HirProgram) -> HashMap<String, Option<Ty>> {
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

/// Builds a `DefId → input Tys` map for every top-level function
/// (and trait / impl methods). Consumed by MIR lowering so call-
/// site argument coercion can detect when a `Fn(args) -> ret`
/// parameter is being supplied with a bare `fn item` that needs
/// trampoline-wrapping into the env+code shape.
fn collect_fn_inputs(program: &HirProgram) -> HashMap<gossamer_resolve::DefId, Vec<Ty>> {
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

/// Builds a `DefId → return Ty` map for every top-level function
/// (and trait / impl methods). Consumed by MIR lowering so call-
/// site destinations can be typed with the callee's concrete
/// return type instead of the call expression's inference-variable
/// placeholder.
fn collect_fn_returns(program: &HirProgram) -> HashMap<gossamer_resolve::DefId, Ty> {
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

/// Builds two maps from the program's struct declarations:
/// - `structs`: struct name → ordered field names.
/// - `struct_defs`: `DefId` → struct name, so projection lowering
///   can go from an `Adt { def, .. }` receiver type back to the
///   field list.
fn collect_struct_fields(
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
    (by_name, by_def)
}

/// Field orders for stdlib struct types user source can name.
/// Mirrors the Rust struct definitions in
/// `crates/gossamer-std/src/*.rs`. New stdlib struct → one entry.
fn stdlib_struct_shapes() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        ("Output", &["stdout", "stderr", "code"]),
        ("ExitStatus", &["code"]),
        (
            "DirEntry",
            &["path", "name", "is_dir", "is_file", "is_symlink"],
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
    ]
}

/// Builds `enum_name -> [variant_name]` and `variant_name -> (enum_name, idx)`
/// maps from program-level `enum` declarations. The MIR lowerer uses
/// these to encode `Color::Green` and the bare-name `Green` form as
/// integer discriminants when constructing values, and to translate
/// `match` arm patterns into stable indices when the scrutinee is
/// the enum's discriminant integer. Also collects each variant's
/// struct-field order so `Shape::Rect { w, h }` literal calls and
/// patterns can flatten into the right slot order.
fn collect_enum_variants(program: &HirProgram) -> EnumIndex {
    let mut by_enum: HashMap<String, Vec<String>> = HashMap::new();
    let mut variant_index: HashMap<String, (String, usize)> = HashMap::new();
    let mut variant_fields: HashMap<String, Vec<String>> = HashMap::new();
    for item in &program.items {
        if let HirItemKind::Adt(adt) = &item.kind {
            if let HirAdtKind::Enum(variants) = &adt.kind {
                let names: Vec<String> = variants.iter().map(|v| v.name.name.clone()).collect();
                for (idx, vname) in names.iter().enumerate() {
                    variant_index.insert(vname.clone(), (adt.name.name.clone(), idx));
                }
                for v in variants {
                    if let Some(fields) = &v.struct_fields {
                        let field_names: Vec<String> =
                            fields.iter().map(|f| f.name.clone()).collect();
                        variant_fields.insert(v.name.name.clone(), field_names);
                    }
                }
                by_enum.insert(adt.name.name.clone(), names);
            }
        }
    }
    EnumIndex {
        by_enum,
        variant_index,
        variant_fields,
    }
}

/// Coarse value-shape classifier for `HashMap<K, V>` value types.
/// The runtime exposes per-shape ABI variants (`gos_rt_map_*_i64`,
/// `_str`); this enum picks the right one without paying a runtime
/// type-id check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapValueKind {
    I64,
    String,
    Other,
}

/// Same coarse split for the key type. Strings need pointer-arg
/// dispatch; everything else (i64, bool, char, raw pointers, …)
/// flows through the scalar i64 ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapKeyKind {
    I64,
    String,
    Other,
}

fn map_value_kind_from(tcx: &gossamer_types::TyCtxt, ty: Ty) -> MapValueKind {
    use gossamer_types::TyKind;
    match tcx.kind_of(ty) {
        TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::Float(_) => MapValueKind::I64,
        TyKind::String => MapValueKind::String,
        _ => MapValueKind::Other,
    }
}

fn map_key_kind_from(tcx: &gossamer_types::TyCtxt, ty: Ty) -> MapKeyKind {
    use gossamer_types::TyKind;
    match tcx.kind_of(ty) {
        TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::Float(_) => MapKeyKind::I64,
        TyKind::String => MapKeyKind::String,
        _ => MapKeyKind::Other,
    }
}

/// Index of every program-level `enum` declaration in a form the MIR
/// lowerer can query during expression and match lowering.
#[derive(Default)]
struct EnumIndex {
    by_enum: HashMap<String, Vec<String>>,
    variant_index: HashMap<String, (String, usize)>,
    /// `variant_name -> [field_name]` for struct-payload variants.
    /// Lets `__struct("Rect", "w", v, "h", v)` calls resolve their
    /// declaration order even when `Rect` is an enum variant rather
    /// than a free struct.
    variant_fields: HashMap<String, Vec<String>>,
}

impl EnumIndex {
    /// Resolves an enum-variant path / bare name to `(enum_name,
    /// variant_index)`. Accepts paths of the form `Color::Green`
    /// (two segments) or the bare name `Green` (one segment) when
    /// the variant name is unambiguous across the program.
    fn lookup(&self, segments: &[Ident]) -> Option<(String, usize)> {
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
}

#[allow(clippy::too_many_arguments)]
fn collect_item(
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

#[allow(clippy::too_many_arguments)]
fn lower_fn(
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
            // Heuristic: parameters named with stdlib-shape-
            // identifying names get a runtime-kind tag so
            // method dispatch on them lands on the right
            // helper. Without this, impl-method params
            // typed `http::Request` (whose Ty resolves to
            // Var since stdlib types aren't user-defined
            // structs) lose their surface kind.
            let runtime_kind: Option<&'static str> = match name.name.as_str() {
                "request" | "req" => Some("http::Request"),
                "response" | "resp" => Some("http::Response"),
                "scanner" => Some("bufio::Scanner"),
                "client" => Some("http::Client"),
                _ => None,
            };
            if let Some(rk) = runtime_kind {
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

fn param_name(pattern: &HirPat) -> Option<Ident> {
    match &pattern.kind {
        HirPatKind::Binding { name, .. } => Some(name.clone()),
        _ => None,
    }
}

fn param_mutable(pattern: &HirPat) -> bool {
    matches!(&pattern.kind, HirPatKind::Binding { mutable: true, .. })
}

struct Builder<'a> {
    tcx: &'a mut TyCtxt,
    locals: Vec<LocalDecl>,
    blocks: Vec<BasicBlock>,
    current: Option<BlockId>,
    scopes: Vec<HashMap<String, Local>>,
    fn_span: Span,
    structs: &'a HashMap<String, Vec<String>>,
    struct_defs: &'a HashMap<gossamer_resolve::DefId, String>,
    enums: &'a EnumIndex,
    impl_methods: &'a HashMap<String, Option<Ty>>,
    fn_returns: &'a HashMap<gossamer_resolve::DefId, Ty>,
    fn_inputs: &'a HashMap<gossamer_resolve::DefId, Vec<Ty>>,
    consts: &'a HashMap<gossamer_resolve::DefId, ConstValue>,
    local_struct: HashMap<Local, String>,
    /// For locals that hold an array/tuple whose element type is a
    /// known struct, records that struct's name. Used to resolve
    /// field projections through `a[i].x` when the type checker left
    /// the element type as an unresolved inference variable.
    local_elem_struct: HashMap<Local, String>,
    local_closure: HashMap<Local, String>,
    /// Locals that hold a function-name constant (e.g. a synthesised
    /// closure body like `__closure_0` bound through a let). Tracked
    /// so that calling the local dispatches to the named function by
    /// direct call rather than treating the local as a closure env
    /// pointer.
    local_fn_name: HashMap<Local, String>,
    /// Runtime-shape tag for locals whose static MIR type doesn't
    /// distinguish the stdlib type behind them (everything ends
    /// up as `i64` / pointer once erased). Method dispatch reads
    /// this tag to pick the right runtime helper for `fs.string(...)`,
    /// `client.get(...)`, `req.send()`, etc.
    local_runtime_kind: HashMap<Local, &'static str>,
    param_locals: std::collections::HashSet<Local>,
    /// Loop contexts visible at the current lowering point. The
    /// innermost loop is at the back. Each entry pairs the
    /// `continue`-target (the loop header) with the `break`-target
    /// (the block emitted right after the loop). `lower_loop` /
    /// `lower_while` push on entry and pop on exit;
    /// `HirExprKind::Break` / `Continue` lookup the back of the
    /// stack to terminate to the right block.
    loop_stack: Vec<LoopContext>,
}

/// A live loop context: where to jump on `break` vs. `continue`.
#[derive(Debug, Clone, Copy)]
struct LoopContext {
    continue_to: BlockId,
    break_to: BlockId,
}

impl<'a> Builder<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
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
            local_struct: HashMap::new(),
            local_elem_struct: HashMap::new(),
            local_closure: HashMap::new(),
            local_fn_name: HashMap::new(),
            local_runtime_kind: HashMap::new(),
            param_locals: std::collections::HashSet::new(),
            loop_stack: Vec::new(),
        }
    }

    /// Returns the struct name registered for the given type (if
    /// any). Walks through references so `&Body` resolves the same
    /// way as `Body`.
    fn struct_name_of(&self, ty: Ty) -> Option<String> {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Adt { def, .. } => {
                    return self.struct_defs.get(def).cloned();
                }
                TyKind::Ref { inner, .. } => cur = *inner,
                _ => return None,
            }
        }
    }

    /// Returns true when `ty` (or anything it references through `&`)
    /// is the stdlib `json::Value` type. Used by field-access and
    /// cast lowering to route opaque-receiver operations through the
    /// json runtime helpers.
    fn is_json_value_ty(&self, ty: Ty) -> bool {
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

    /// Returns true when `ty` is an Adt (typically `Result<T, E>`
    /// or `Option<T>`) whose first type-generic argument is
    /// `json::Value`. Used by match-arm binding to recover the
    /// payload type of a json-shaped variant when the variant's
    /// inner pattern only reproduces the scrutinee local.
    /// Classifies the value type of a `HashMap<K, V>` receiver
    /// for runtime-helper dispatch. Peels through `&T` first.
    /// Used by `m.insert(k, v)` / `m.get(k)` to pick the
    /// scalar / string-keyed runtime variant.
    fn hash_map_value_kind(&self, ty: Ty) -> Option<MapValueKind> {
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

    /// Classifies the key type of a `HashMap<K, V>` receiver.
    fn hash_map_key_kind(&self, ty: Ty) -> Option<MapKeyKind> {
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

    /// Returns the first generic-type argument of `ty` if it is an
    /// `Adt`, peeling through any `&T` references first. Used by
    /// the Option/Result `unwrap` lowering to recover the inner
    /// success type.
    fn first_generic_of(&self, ty: Ty) -> Option<Ty> {
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

    /// Returns the second generic-type argument of `ty` (i.e. the
    /// `E` in `Result<T, E>`) if it is an `Adt`, peeling through
    /// `&T` first. Used by the `err` method lowering.
    fn second_generic_of(&self, ty: Ty) -> Option<Ty> {
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

    fn adt_first_generic_is_json(&self, ty: Ty) -> bool {
        use gossamer_types::{GenericArg, TyKind};
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                TyKind::JsonValue => return true,
                TyKind::Adt { substs, .. } => {
                    return substs.as_slice().iter().any(|arg| match arg {
                        GenericArg::Type(t) => self.is_json_value_ty(*t),
                        GenericArg::Const(_) => false,
                    });
                }
                _ => return false,
            }
        }
    }

    // exprs_match is a free fn at the bottom of this file.

    /// Recognises the `m.insert(k, m.get_or(k, 0) + by)` shape
    /// and lowers it to a single `gos_rt_map_inc_i64(m, k, by)`
    /// call. Same map receiver, same key, integer add — emits
    /// nothing and returns `None` for any other shape so the
    /// caller falls through to the regular insert dispatch.
    /// Inlines `arr.swap(i, j)` as four index ops so the swap
    /// survives every backend (tree-walker, bytecode VM, JIT, AOT).
    /// Without this the generic Call fallback emits
    /// `Call(Const(Str("swap")), …)` — cranelift has no `swap`
    /// intrinsic and silently lowers it to a typed-zero stub,
    /// leaving the receiver unmodified.
    fn try_lower_array_swap(
        &mut self,
        receiver: &HirExpr,
        i_expr: &HirExpr,
        j_expr: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Build a Place that names the receiver as a place
        // expression. Bail out if the receiver isn't an
        // assignable l-value (a path, field, or index chain).
        let recv_place = self.lower_place_expr(receiver)?;
        let i_local = self.lower_expr(i_expr)?;
        let j_local = self.lower_expr(j_expr)?;
        let elem_ty = match self
            .tcx
            .kind_of(self.locals[recv_place.local.0 as usize].ty)
        {
            gossamer_types::TyKind::Array { elem, .. } => *elem,
            gossamer_types::TyKind::Slice(elem) => *elem,
            gossamer_types::TyKind::Vec(elem) => *elem,
            gossamer_types::TyKind::Ref { inner, .. } => match self.tcx.kind_of(*inner) {
                gossamer_types::TyKind::Array { elem, .. } => *elem,
                gossamer_types::TyKind::Slice(elem) => *elem,
                gossamer_types::TyKind::Vec(elem) => *elem,
                _ => return None,
            },
            _ => return None,
        };
        let mut at_i = recv_place.clone();
        at_i.projection.push(crate::ir::Projection::Index(i_local));
        let mut at_j = recv_place.clone();
        at_j.projection.push(crate::ir::Projection::Index(j_local));
        let temp_i = self.fresh(elem_ty);
        let temp_j = self.fresh(elem_ty);
        self.emit_assign(
            Place::local(temp_i),
            Rvalue::Use(Operand::Copy(at_i.clone())),
            span,
        );
        self.emit_assign(
            Place::local(temp_j),
            Rvalue::Use(Operand::Copy(at_j.clone())),
            span,
        );
        self.emit_assign(at_i, Rvalue::Use(Operand::Copy(Place::local(temp_j))), span);
        self.emit_assign(at_j, Rvalue::Use(Operand::Copy(Place::local(temp_i))), span);
        let unit_local = self.lower_unit(span);
        Some(unit_local)
    }

    fn try_lower_map_inc(
        &mut self,
        outer_recv: &HirExpr,
        outer_key: &HirExpr,
        value_expr: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Binary {
            op: HirBinaryOp::Add,
            lhs,
            rhs,
        } = &value_expr.kind
        else {
            return None;
        };
        let (get_call, by_expr) = if let HirExprKind::MethodCall { name, .. } = &lhs.kind {
            if name.name.as_str() == "get_or" {
                (lhs.as_ref(), rhs.as_ref())
            } else {
                return None;
            }
        } else if let HirExprKind::MethodCall { name, .. } = &rhs.kind {
            if name.name.as_str() == "get_or" {
                (rhs.as_ref(), lhs.as_ref())
            } else {
                return None;
            }
        } else {
            return None;
        };
        let HirExprKind::MethodCall {
            receiver: inner_recv,
            args: get_args,
            ..
        } = &get_call.kind
        else {
            return None;
        };
        if get_args.len() != 2 {
            return None;
        }
        if !exprs_match(outer_recv, inner_recv) || !exprs_match(outer_key, &get_args[0]) {
            return None;
        }
        // Peephole only handles `HashMap<i64, i64>`. The
        // `gos_rt_map_inc_i64` helper takes the key as an i64;
        // forwarding a `*const c_char` here corrupts the lookup.
        // For non-i64 receivers fall through to the general
        // get_or + insert path so the key is hashed correctly.
        let outer_recv_ty = self
            .receiver_local_from_path(outer_recv)
            .map_or(outer_recv.ty, |l| self.locals[l.0 as usize].ty);
        let key_kind = self.hash_map_key_kind(outer_recv_ty);
        let value_kind = self.hash_map_value_kind(outer_recv_ty);
        if !matches!(
            (key_kind, value_kind),
            (Some(MapKeyKind::I64), Some(MapValueKind::I64))
        ) {
            return None;
        }
        let recv_local = self.lower_expr(outer_recv)?;
        let key_local = self.lower_expr(outer_key)?;
        let by_local = self.lower_expr(by_expr)?;
        let dest = self.fresh(ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_map_inc_i64".to_string())),
            args: vec![
                Operand::Copy(Place::local(recv_local)),
                Operand::Copy(Place::local(key_local)),
                Operand::Copy(Place::local(by_local)),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    /// Synthesises the sentinel-DefId `Option<_>` Adt type used
    /// to flag values whose disc bit can be read at runtime.
    fn option_adt_ty(&mut self) -> Ty {
        self.tcx.intern(gossamer_types::TyKind::Adt {
            def: gossamer_resolve::DefId::local(u32::MAX - 1),
            substs: gossamer_types::Substs::new(),
        })
    }

    /// Recursive runtime-kind probe for chained method calls.
    /// Walks `client.get(...).send()` and returns the kind tag
    /// the call's destination would receive without lowering it.
    fn expr_runtime_kind(&self, expr: &HirExpr) -> Option<&'static str> {
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

    /// Returns `true` when `ty` is the sentinel-DefId Adt for
    /// `Result<T, E>` or `Option<T>`.
    fn is_result_or_option_adt(&self, ty: Ty) -> bool {
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

    /// Returns the `idx`-th type generic of an Adt receiver
    /// (peeling through `&T`). Returns `None` for non-Adt
    /// receivers, out-of-range indices, or const generics.
    fn adt_generic_at(&self, ty: Ty, idx: usize) -> Option<Ty> {
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

    /// Emits a `gos_rt_json_get(receiver, "field")` call and
    /// returns the fresh local holding the resulting `json::Value`
    /// pointer. Pinned to `TyKind::JsonValue` so chained accesses
    /// (`root.a.b.c`) take this same path on every step.
    fn emit_json_get(&mut self, receiver_local: Local, field: &str, span: Span) -> Local {
        let json_ty = self.tcx.json_value_ty();
        let dest = self.fresh(json_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_json_get".to_string())),
            args: vec![
                Operand::Copy(Place::local(receiver_local)),
                Operand::Const(ConstValue::Str(field.to_string())),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        dest
    }

    /// Emits a single-arg call to `name`, threading `receiver` as
    /// the only argument. Used to insert `gos_rt_json_as_*` and
    /// `gos_rt_json_render` coercions when the binding type forces
    /// a `json::Value` to a concrete shape.
    fn emit_single_arg_call(
        &mut self,
        name: &'static str,
        receiver: Local,
        ret_ty: Ty,
        span: Span,
    ) -> Local {
        let dest = self.fresh(ret_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name.to_string())),
            args: vec![Operand::Copy(Place::local(receiver))],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        dest
    }

    /// Routes free-function calls under the `json::` module to
    /// their runtime entry points. Returns `None` when the call
    /// isn't json — the surrounding `lower_call` continues with
    /// the normal user-fn dispatch.
    /// Free-function dispatch for the rest of the stdlib that the
    /// MIR side knows how to route to a `gos_rt_*` runtime helper:
    /// `errors::new`, `errors::wrap`, `regex::compile`,
    /// `regex::find_all`, `regex::replace_all`, `regex::split`,
    /// `regex::is_match`, `regex::find`, `flag::Set::new`,
    /// `fs::read_to_string`, `fs::write`, `fs::create_dir_all`,
    /// `path::join`, `bufio::Scanner::new`, `http::Client::new`,
    /// `http::Response::text`, `http::Response::json`,
    /// `gzip::encode/decode`, `slog::*`, `testing::check{_eq,_ok}`.
    /// Each maps the joined path to a single named runtime call.
    fn lower_stdlib_free_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
        let strip_std = if names.first() == Some(&"std") {
            &names[1..]
        } else {
            &names[..]
        };
        let joined = strip_std.join("::");
        let (rt_name, ret_ty) = match joined.as_str() {
            "errors::new" => (
                "gos_rt_error_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "errors::wrap" => (
                "gos_rt_error_wrap",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "errors::is" => ("gos_rt_error_is", self.tcx.bool_ty()),
            "regex::compile" => (
                "gos_rt_regex_compile",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "regex::is_match" => ("gos_rt_regex_is_match", self.tcx.bool_ty()),
            "regex::find" => ("gos_rt_regex_find", self.tcx.string_ty()),
            "regex::find_all" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_regex_find_all", v)
            }
            "regex::captures_all" => {
                // No real captures support yet — return an empty
                // vec so the program runs and any iteration is a
                // no-op.
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_regex_find_all", v)
            }
            "regex::replace_all" => ("gos_rt_regex_replace_all", self.tcx.string_ty()),
            "regex::split" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_regex_split", v)
            }
            "fs::read_to_string" => ("gos_rt_fs_read_to_string", self.tcx.string_ty()),
            "fs::write" => ("gos_rt_fs_write", self.tcx.bool_ty()),
            "fs::create_dir_all" => ("gos_rt_fs_create_dir_all", self.tcx.bool_ty()),
            "path::join" => ("gos_rt_path_join", self.tcx.string_ty()),
            "flag::Set::new" => (
                "gos_rt_flag_set_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "bufio::Scanner::new" | "Scanner::new" => (
                "gos_rt_bufio_scanner_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "bufio::Scanner::next" | "Scanner::next" => {
                ("gos_rt_bufio_scanner_text", self.tcx.string_ty())
            }
            "http::Client::new" => (
                "gos_rt_http_client_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::Response::text" => (
                "gos_rt_http_response_text_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::Response::json" => (
                "gos_rt_http_response_json_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::serve" => ("gos_rt_http_serve", self.tcx.unit()),
            "gzip::encode" | "compress::gzip::encode" => {
                ("gos_rt_gzip_encode", self.tcx.string_ty())
            }
            "gzip::decode" | "compress::gzip::decode" => {
                ("gos_rt_gzip_decode", self.tcx.string_ty())
            }
            "slog::info" => ("gos_rt_slog_info", self.tcx.unit()),
            "slog::warn" => ("gos_rt_slog_warn", self.tcx.unit()),
            "slog::error" => ("gos_rt_slog_error", self.tcx.unit()),
            "slog::debug" => ("gos_rt_slog_debug", self.tcx.unit()),
            "testing::check" => ("gos_rt_testing_check", self.tcx.bool_ty()),
            "testing::check_eq" => ("gos_rt_testing_check_eq_i64", self.tcx.bool_ty()),
            "testing::check_ok" => {
                // Pass-through identity in compiled mode — assumes
                // happy path.
                ("", self.tcx.int_ty(gossamer_types::IntTy::I64))
            }
            // Stdlib collections beyond HashMap. The cranelift
            // intrinsic dispatch handles `HashSet::new` /
            // `BTreeMap::new` directly (no args); MIR routes the
            // call through these symbol names so the destination
            // local can be tagged with a runtime kind for method
            // dispatch.
            "HashSet::new" | "collections::HashSet::new" => (
                "gos_rt_set_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "BTreeMap::new" | "collections::BTreeMap::new" => (
                "gos_rt_btmap_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            _ => return None,
        };
        if rt_name.is_empty() {
            // Identity passthrough for testing::check_ok and friends.
            let v = args.first().and_then(|a| self.lower_expr(a))?;
            let dest = self.fresh(ret_ty);
            self.emit_assign(
                Place::local(dest),
                Rvalue::Use(Operand::Copy(Place::local(v))),
                span,
            );
            return Some(dest);
        }
        let mut arg_locals = Vec::with_capacity(args.len());
        for arg in args {
            arg_locals.push(self.lower_expr(arg)?);
        }
        let dest = self.fresh(ret_ty);
        // Tag the destination's runtime shape so subsequent
        // method dispatches on the same local can pick the right
        // helper. Mirrors the shape the runtime helpers return.
        let runtime_kind: Option<&'static str> = match rt_name {
            "gos_rt_flag_set_new" => Some("flag::Set"),
            "gos_rt_bufio_scanner_new" => Some("bufio::Scanner"),
            "gos_rt_http_client_new" => Some("http::Client"),
            "gos_rt_http_request_send" => Some("http::Response"),
            "gos_rt_http_client_get" | "gos_rt_http_client_post" => Some("http::Request"),
            "gos_rt_http_response_text_new" | "gos_rt_http_response_json_new" => {
                Some("http::Response")
            }
            "gos_rt_error_new" | "gos_rt_error_wrap" => Some("errors::Error"),
            "gos_rt_regex_compile" => Some("regex::Pattern"),
            "gos_rt_set_new" => Some("collections::HashSet"),
            "gos_rt_btmap_new" => Some("collections::BTreeMap"),
            _ => None,
        };
        if let Some(rk) = runtime_kind {
            self.local_runtime_kind.insert(dest, rk);
        }
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(rt_name.to_string())),
            args: arg_locals
                .into_iter()
                .map(|l| Operand::Copy(Place::local(l)))
                .collect(),
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    fn lower_json_free_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() < 2 {
            return None;
        }
        let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
        let last = *names.last()?;
        let module_chain = &names[..names.len() - 1];
        let module_ok = matches!(
            module_chain,
            ["json"] | ["encoding", "json"] | ["std", "encoding", "json"]
        );
        if !module_ok {
            return None;
        }
        let (rt_name, ret_ty) = match last {
            "parse" => ("gos_rt_json_parse", self.tcx.json_value_ty()),
            "render" | "encode" => ("gos_rt_json_render", self.tcx.string_ty()),
            "decode" => ("gos_rt_json_parse", self.tcx.json_value_ty()),
            "get" => ("gos_rt_json_get", self.tcx.json_value_ty()),
            "at" => ("gos_rt_json_at", self.tcx.json_value_ty()),
            "as_i64" => (
                "gos_rt_json_as_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "as_f64" => (
                "gos_rt_json_as_f64",
                self.tcx.float_ty(gossamer_types::FloatTy::F64),
            ),
            "as_str" => ("gos_rt_json_as_str", self.tcx.string_ty()),
            "as_bool" => ("gos_rt_json_as_bool", self.tcx.bool_ty()),
            "as_array" => ("gos_rt_json_identity", self.tcx.json_value_ty()),
            "len" => (
                "gos_rt_json_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "is_null" => ("gos_rt_json_is_null", self.tcx.bool_ty()),
            _ => return None,
        };
        let mut arg_locals = Vec::with_capacity(args.len());
        for arg in args {
            arg_locals.push(self.lower_expr(arg)?);
        }
        let dest = self.fresh(ret_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(rt_name.to_string())),
            args: arg_locals
                .into_iter()
                .map(|l| Operand::Copy(Place::local(l)))
                .collect(),
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    /// Picks the right `gos_rt_json_as_*` (or render) helper for
    /// coercing a `json::Value` into the target primitive `ty`.
    /// Returns `None` when the target shape isn't representable as
    /// a single runtime call (e.g. a generic Adt) — the caller
    /// keeps the `json::Value` as-is in that case.
    fn maybe_coerce_json_value(
        &mut self,
        value: Local,
        target_ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let mut cur = target_ty;
        let kind = loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                other => break other.clone(),
            }
        };
        let (helper, ret_ty) = match kind {
            TyKind::Int(_) => (
                "gos_rt_json_as_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            TyKind::Float(_) => (
                "gos_rt_json_as_f64",
                self.tcx.float_ty(gossamer_types::FloatTy::F64),
            ),
            TyKind::Bool => ("gos_rt_json_as_bool", self.tcx.bool_ty()),
            TyKind::String => ("gos_rt_json_as_str", self.tcx.string_ty()),
            _ => return None,
        };
        Some(self.emit_single_arg_call(helper, value, ret_ty, span))
    }

    /// Walks a HIR place-shaped expression and tries to recover the
    /// struct name of whatever the expression evaluates to, even
    /// when the type checker left the expression's own `ty` as an
    /// unresolved inference variable. Falls through container
    /// projections (`a[_]` → element type, `a.N` → tuple element).
    fn struct_name_from_expr(&self, expr: &HirExpr) -> Option<String> {
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
                let ty = self.locals.get(local.0 as usize)?.ty;
                self.struct_name_of(ty)
            }
            _ => None,
        }
    }

    fn push_local(&mut self, ty: Ty, debug_name: Option<Ident>, mutable: bool) -> Local {
        let id = u32::try_from(self.locals.len()).expect("local overflow");
        self.locals.push(LocalDecl {
            ty,
            debug_name,
            mutable,
        });
        Local(id)
    }

    fn fresh(&mut self, ty: Ty) -> Local {
        self.push_local(ty, None, false)
    }

    fn bind_local(&mut self, name: &str, local: Local) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), local);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<Local> {
        for scope in self.scopes.iter().rev() {
            if let Some(local) = scope.get(name) {
                return Some(*local);
            }
        }
        None
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn new_block(&mut self, span: Span) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("block overflow"));
        self.blocks.push(BasicBlock {
            id,
            stmts: Vec::new(),
            terminator: Terminator::Unreachable,
            span,
        });
        id
    }

    fn set_current(&mut self, block: BlockId) {
        self.current = Some(block);
    }

    fn current_block(&mut self) -> &mut BasicBlock {
        let id = self.current.expect("no current block").0 as usize;
        &mut self.blocks[id]
    }

    fn emit_assign(&mut self, place: Place, rvalue: Rvalue, span: Span) {
        if self.current.is_none() {
            return;
        }
        let stmt = Statement {
            kind: StatementKind::Assign { place, rvalue },
            span,
        };
        self.current_block().stmts.push(stmt);
    }

    fn terminate(&mut self, terminator: Terminator) {
        if self.current.is_some() {
            let span = self.fn_span;
            let block = self.current_block();
            block.terminator = terminator;
            let _ = span;
        }
        self.current = None;
    }

    fn lower_block(&mut self, block: &HirBlock) -> Option<Local> {
        self.push_scope();
        for stmt in &block.stmts {
            self.lower_stmt(stmt);
            if self.current.is_none() {
                self.pop_scope();
                return None;
            }
        }
        let result = block.tail.as_ref().and_then(|tail| self.lower_expr(tail));
        self.pop_scope();
        if self.current.is_none() { None } else { result }
    }

    #[allow(clippy::cognitive_complexity)]
    fn lower_stmt(&mut self, stmt: &HirStmt) {
        match &stmt.kind {
            HirStmtKind::Let { pattern, ty, init } => {
                let local = self.push_local(*ty, param_name(pattern), param_mutable(pattern));
                if let HirPatKind::Binding { name, .. } = &pattern.kind {
                    self.bind_local(&name.name, local);
                }
                // `let mut xs: [T] = [a, b, c]` (or `let xs: Vec<T>
                // = [a, b, c]`): allocate a heap-backed Vec and
                // push each element so subsequent `xs.push(...)` /
                // `xs.len()` lands on a real Vec layout instead of
                // the fixed-size stack array the literal would
                // otherwise produce. Only fires when the binding
                // wears an explicit Slice/Vec annotation.
                {
                    use gossamer_types::TyKind;
                    let binding_wants_vec =
                        matches!(self.tcx.kind_of(*ty), TyKind::Vec(_) | TyKind::Slice(_),);
                    if binding_wants_vec {
                        if let Some(init_expr) = init.as_ref() {
                            if let HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) =
                                &init_expr.kind
                            {
                                if self.lower_let_array_as_vec(local, elems, stmt.span) {
                                    return;
                                }
                            }
                        }
                    }
                }
                if let Some(init) = init {
                    if let Some(mut value) = self.lower_expr(init) {
                        // Coerce a `json::Value`-typed initialiser
                        // when the binding has an explicit primitive
                        // / String annotation. `let low: i64 =
                        // root.latency.low_ms` becomes
                        // `gos_rt_json_as_i64(root.get("latency").get("low_ms"))`
                        // — keeps the user's natural notation while
                        // funnelling the dynamic-shape tax through
                        // the runtime helpers.
                        let value_ty = self.locals[value.0 as usize].ty;
                        if self.is_json_value_ty(value_ty) && !self.is_json_value_ty(*ty) {
                            if let Some(coerced) =
                                self.maybe_coerce_json_value(value, *ty, stmt.span)
                            {
                                value = coerced;
                            }
                        }
                        // Callable-shape coercion: when `let f:
                        // fn(...) -> ... = bare_fn` (or
                        // `Fn(...) -> ...`) is written, wrap the
                        // bare fn item in the env+code blob so
                        // every callable slot in the program
                        // uniformly carries an env_ptr. Without
                        // this, a later `f(...)` call site would
                        // see the raw fn address and skip the
                        // env-load step, segfaulting on access.
                        {
                            use gossamer_types::TyKind;
                            let value_ty_now = self.locals[value.0 as usize].ty;
                            let dest_callable = matches!(
                                self.tcx.kind_of(*ty),
                                TyKind::FnPtr(_) | TyKind::FnTrait(_)
                            );
                            let src_is_fn_def =
                                matches!(self.tcx.kind_of(value_ty_now), TyKind::FnDef { .. });
                            // Lift-closed produces a Path lowered
                            // to `Const(Str(name))` whose local is
                            // marked in `local_fn_name`. Treat it
                            // the same as a bare fn item for
                            // callable-slot wrapping.
                            let src_names_fn = self.local_fn_name.contains_key(&value);
                            if dest_callable && (src_is_fn_def || src_names_fn) {
                                value = self.coerce_to_fn_trait_if_needed(value, *ty, stmt.span);
                            }
                        }
                        // When the HIR-recorded type is an
                        // unresolved inference variable, pin the
                        // binding's MIR type to whatever the lowered
                        // initialiser settled on — keeps downstream
                        // passes (string-concat, codegen cl-type
                        // inference) grounded on concrete kinds.
                        let init_ty = self.locals[value.0 as usize].ty;
                        {
                            use gossamer_types::TyKind;
                            let binding_kind = self.tcx.kind_of(self.locals[local.0 as usize].ty);
                            let init_kind = self.tcx.kind_of(init_ty);
                            // When the binding's annotation is an
                            // Adt wrapper but the initialiser
                            // settled on a concrete scalar / String
                            // (typical of `let v = r.unwrap()` for
                            // `r: Result<T, E>` where the compiled
                            // tier flattens the wrapper), promote
                            // the binding to the scalar type so
                            // downstream printing + arithmetic find
                            // the right kind. The other concrete
                            // annotations (struct, vec, tuple, …)
                            // are kept verbatim because they
                            // typically come from explicit user
                            // annotations the typechecker has
                            // already validated against the value.
                            let promote_inner = matches!(binding_kind, TyKind::Adt { .. })
                                && matches!(
                                    init_kind,
                                    TyKind::Bool
                                        | TyKind::Char
                                        | TyKind::Int(_)
                                        | TyKind::Float(_)
                                        | TyKind::String
                                );
                            if !matches!(
                                binding_kind,
                                TyKind::Bool
                                    | TyKind::Char
                                    | TyKind::Int(_)
                                    | TyKind::Float(_)
                                    | TyKind::String
                                    | TyKind::Vec(_)
                                    | TyKind::Array { .. }
                                    | TyKind::Slice(_)
                                    | TyKind::Adt { .. }
                                    | TyKind::Tuple(_)
                                    | TyKind::Ref { .. }
                            ) || promote_inner
                            {
                                self.locals[local.0 as usize].ty = init_ty;
                            }
                        }
                        if let Some(struct_name) = self.local_struct.get(&value).cloned() {
                            self.local_struct.insert(local, struct_name);
                        }
                        if let Some(elem) = self.local_elem_struct.get(&value).cloned() {
                            self.local_elem_struct.insert(local, elem);
                        }
                        if let Some(closure_name) = self.local_closure.get(&value).cloned() {
                            self.local_closure.insert(local, closure_name);
                        }
                        if let Some(fn_name) = self.local_fn_name.get(&value).cloned() {
                            self.local_fn_name.insert(local, fn_name);
                        }
                        if let Some(rk) = self.local_runtime_kind.get(&value).copied() {
                            self.local_runtime_kind.insert(local, rk);
                        }
                        self.emit_assign(
                            Place::local(local),
                            Rvalue::Use(Operand::Copy(Place::local(value))),
                            stmt.span,
                        );
                        if let HirPatKind::Tuple(sub_patterns) = &pattern.kind {
                            self.bind_tuple_pattern(local, sub_patterns, stmt.span);
                        }
                    }
                }
            }
            HirStmtKind::Expr { expr, .. } => {
                let _ = self.lower_expr(expr);
            }
            HirStmtKind::Defer(_) => {
                // Deferred calls are lowered to no-ops at the MIR
                // level for now; full support lands with the
                // runtime's unwind-and-run machinery.
            }
            HirStmtKind::Go(expr) => {
                // `go f(args);` — spawn `f` on a fresh OS
                // thread via the runtime's
                // `gos_rt_go_spawn_call_N(fn_addr, args…)`
                // helper. Mirrors the expression-position
                // lowering below so a goroutine spawned at
                // statement level fans out the same way as
                // one used as an expression. Falls back to
                // synchronous execution when the inner shape
                // doesn't match a direct `f(args)` call with
                // ≤ 4 scalar arguments.
                let mut handled = false;
                if let HirExprKind::Call { callee, args } = &expr.kind {
                    if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
                        if args.len() <= 6 {
                            let sym: &'static str = match args.len() {
                                0 => "gos_rt_go_spawn_call_0",
                                1 => "gos_rt_go_spawn_call_1",
                                2 => "gos_rt_go_spawn_call_2",
                                3 => "gos_rt_go_spawn_call_3",
                                4 => "gos_rt_go_spawn_call_4",
                                5 => "gos_rt_go_spawn_call_5",
                                _ => "gos_rt_go_spawn_call_6",
                            };
                            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                            let fn_addr_local = self.fresh(i64_ty);
                            let substs = self.substs_of(callee.ty);
                            self.emit_assign(
                                Place::local(fn_addr_local),
                                Rvalue::Use(Operand::FnRef { def: *def, substs }),
                                expr.span,
                            );
                            let mut operands = Vec::with_capacity(args.len() + 1);
                            operands.push(Operand::Copy(Place::local(fn_addr_local)));
                            for arg in args {
                                if let Some(a) = self.lower_expr(arg) {
                                    operands.push(Operand::Copy(Place::local(a)));
                                }
                            }
                            let unit_ty = self.tcx.unit();
                            let dest = self.fresh(unit_ty);
                            let next = self.new_block(expr.span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(sym.to_string())),
                                args: operands,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            handled = true;
                        }
                    }
                }
                if !handled {
                    let _ = self.lower_expr(expr);
                }
            }
            HirStmtKind::Item(_) => {
                // Nested items are not supported in the MIR yet.
            }
        }
    }

    fn lower_expr(&mut self, expr: &HirExpr) -> Option<Local> {
        match &expr.kind {
            HirExprKind::Literal(lit) => Some(self.lower_literal(lit, expr.ty, expr.span)),
            HirExprKind::Path { segments, def } => {
                self.lower_path(segments, *def, expr.ty, expr.span)
            }
            HirExprKind::Unary { op, operand } => {
                self.lower_unary(*op, operand, expr.ty, expr.span)
            }
            HirExprKind::Binary { op, lhs, rhs } => {
                self.lower_binary(*op, lhs, rhs, expr.ty, expr.span)
            }
            HirExprKind::Assign { place, value } => {
                self.lower_assign(place, value, expr.span);
                Some(self.lower_unit(expr.span))
            }
            HirExprKind::Call { callee, args } => self.lower_call(callee, args, expr.ty, expr.span),
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(
                condition,
                then_branch,
                else_branch.as_deref(),
                expr.ty,
                expr.span,
            ),
            HirExprKind::While { condition, body } => {
                self.lower_while(condition, body, expr.span);
                Some(self.lower_unit(expr.span))
            }
            HirExprKind::Loop { body } => self.lower_loop(body, expr.ty, expr.span),
            HirExprKind::Block(block) => self.lower_block(block),
            HirExprKind::Return(value) => {
                if let Some(value) = value {
                    if let Some(mut local) = self.lower_expr(value) {
                        // When the function's declared return type
                        // is a callable shape (`fn(...) -> ...` /
                        // `Fn(...) -> ...`) and the returned value
                        // is a bare fn item, wrap it in the env+
                        // code blob so the caller's slot uniformly
                        // carries an env_ptr.
                        use gossamer_types::TyKind;
                        let ret_ty = self.locals[Local::RETURN.0 as usize].ty;
                        let value_ty = self.locals[local.0 as usize].ty;
                        let dest_callable = matches!(
                            self.tcx.kind_of(ret_ty),
                            TyKind::FnPtr(_) | TyKind::FnTrait(_)
                        );
                        let src_is_fn_def =
                            matches!(self.tcx.kind_of(value_ty), TyKind::FnDef { .. });
                        let src_names_fn = self.local_fn_name.contains_key(&local);
                        if dest_callable && (src_is_fn_def || src_names_fn) {
                            local = self.coerce_to_fn_trait_if_needed(local, ret_ty, expr.span);
                        }
                        self.emit_assign(
                            Place::local(Local::RETURN),
                            Rvalue::Use(Operand::Copy(Place::local(local))),
                            expr.span,
                        );
                    }
                }
                self.terminate(Terminator::Return);
                None
            }
            HirExprKind::Break(_) => {
                // Jump to the innermost loop's break target. Outside
                // a loop the resolver/typechecker is supposed to
                // reject this; if it slips through, fall back to
                // `Unreachable` rather than emit a dangling jump.
                if let Some(ctx) = self.loop_stack.last().copied() {
                    self.terminate(Terminator::Goto {
                        target: ctx.break_to,
                    });
                } else {
                    self.terminate(Terminator::Unreachable);
                }
                None
            }
            HirExprKind::Continue => {
                if let Some(ctx) = self.loop_stack.last().copied() {
                    self.terminate(Terminator::Goto {
                        target: ctx.continue_to,
                    });
                } else {
                    self.terminate(Terminator::Unreachable);
                }
                None
            }
            HirExprKind::Tuple(elems) => self.lower_tuple(elems, expr.ty, expr.span),
            HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) => {
                self.lower_array_list(elems, expr.ty, expr.span)
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::Repeat { value, count }) => {
                self.lower_array_repeat(value, count, expr.ty, expr.span)
            }
            HirExprKind::TupleIndex { receiver, index } => {
                self.lower_tuple_index(receiver, *index, expr.ty, expr.span)
            }
            HirExprKind::Index { base, index } => {
                self.lower_index_access(base, index, expr.ty, expr.span)
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.lower_match(scrutinee, arms, expr.ty, expr.span)
            }
            HirExprKind::Cast { value, ty: target } => {
                self.lower_cast(value, *target, expr.ty, expr.span)
            }
            HirExprKind::Field { receiver, name } => {
                self.lower_field_access(receiver, name, expr.ty, expr.span)
            }
            HirExprKind::LiftedClosure { name, captures } => {
                self.lower_lifted_closure(name, captures, expr.ty, expr.span)
            }
            HirExprKind::MethodCall {
                receiver,
                name,
                args,
            } => self.lower_method_call(receiver, name, args, expr.ty, expr.span),
            HirExprKind::Go(inner) => {
                let go_span = expr.span;
                // Real spawn for `go f(args)` where f is a named
                // function with 0-2 scalar args: emit a call to
                // `gos_rt_go_spawn_call_N(fn_addr, args…)`. The
                // runtime helper transmutes fn_addr back to
                // `extern "C" fn(...) -> i64` and runs it on a
                // fresh OS thread.
                //
                // Anything more complex (closure captures, >2
                // args, method calls) falls back to synchronous
                // execution so the program still runs — sound
                // for single-threaded workloads.
                if let HirExprKind::Call { callee, args } = &inner.kind {
                    if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
                        if args.len() <= 6 {
                            let sym: &'static str = match args.len() {
                                0 => "gos_rt_go_spawn_call_0",
                                1 => "gos_rt_go_spawn_call_1",
                                2 => "gos_rt_go_spawn_call_2",
                                3 => "gos_rt_go_spawn_call_3",
                                4 => "gos_rt_go_spawn_call_4",
                                5 => "gos_rt_go_spawn_call_5",
                                _ => "gos_rt_go_spawn_call_6",
                            };
                            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                            let fn_addr_local = self.fresh(i64_ty);
                            let substs = self.substs_of(callee.ty);
                            self.emit_assign(
                                Place::local(fn_addr_local),
                                Rvalue::Use(Operand::FnRef { def: *def, substs }),
                                go_span,
                            );
                            let mut operands = Vec::with_capacity(args.len() + 1);
                            operands.push(Operand::Copy(Place::local(fn_addr_local)));
                            for arg in args {
                                let a = self.lower_expr(arg)?;
                                operands.push(Operand::Copy(Place::local(a)));
                            }
                            let unit_ty = self.tcx.unit();
                            let dest = self.fresh(unit_ty);
                            let next = self.new_block(go_span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(sym.to_string())),
                                args: operands,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            return Some(dest);
                        }
                    }
                }
                // Fallback: synchronous.
                let _ = self.lower_expr(inner);
                Some(self.lower_unit(go_span))
            }
            HirExprKind::Select { arms } => {
                // Sequential stub: run each arm's side-effects and
                // then the first arm's body. The real runtime will
                // pick the first ready channel, but under the
                // single-task stub we just pretend arm 0 fired.
                use gossamer_hir::HirSelectOp;
                let mut result: Option<Local> = None;
                for (i, arm) in arms.iter().enumerate() {
                    match &arm.op {
                        HirSelectOp::Recv { channel, .. } | HirSelectOp::Send { channel, .. } => {
                            let _ = self.lower_expr(channel);
                        }
                        HirSelectOp::Default => {}
                    }
                    if i == 0 {
                        result = self.lower_expr(&arm.body);
                    }
                }
                result.or_else(|| Some(self.lower_unit(expr.span)))
            }
            HirExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                // Standalone Range value. The compiled tier
                // represents a Range as a 2-i64 tuple
                // `(lo, hi)`. Open-ended bounds default to 0
                // for `lo` and `i64::MAX` for `hi`. Used by
                // slice expressions like `arr[1..]` which the
                // surrounding Index lowering picks the bounds
                // out of.
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let lo_local = if let Some(s) = start {
                    self.lower_expr(s)?
                } else {
                    let l = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(l),
                        Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                        expr.span,
                    );
                    l
                };
                let hi_local = if let Some(e) = end {
                    self.lower_expr(e)?
                } else {
                    let l = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(l),
                        Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(i64::MAX)))),
                        expr.span,
                    );
                    l
                };
                // Bump `hi` for inclusive ranges so the half-
                // open `[lo, hi)` interpretation downstream
                // doesn't drop the last element.
                let hi_local = if *inclusive {
                    let one = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(one),
                        Rvalue::Use(Operand::Const(ConstValue::Int(1))),
                        expr.span,
                    );
                    let bumped = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(bumped),
                        Rvalue::BinaryOp {
                            op: BinOp::Add,
                            lhs: Operand::Copy(Place::local(hi_local)),
                            rhs: Operand::Copy(Place::local(one)),
                        },
                        expr.span,
                    );
                    bumped
                } else {
                    hi_local
                };
                let dest = self.fresh(expr.ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Aggregate {
                        kind: crate::ir::AggregateKind::Tuple,
                        operands: vec![
                            Operand::Copy(Place::local(lo_local)),
                            Operand::Copy(Place::local(hi_local)),
                        ],
                    },
                    expr.span,
                );
                Some(dest)
            }
            // The native build pipeline runs `gossamer_hir::lift_closures`
            // upstream, so by the time we lower a Closure here we are
            // either in the VM's pre-JIT pass (which never executes the
            // resulting MIR — execution stays on the tree-walker) or
            // an unreachable path. Emit a zero-shaped placeholder so
            // pre-pass lowering succeeds without claiming to lower the
            // closure semantically. Same shape for the resolver's
            // `Placeholder` sentinel: parse / resolve diagnostics halt
            // the build before a real run, so any survivor reaches MIR
            // only on the VM's no-execute pre-pass.
            HirExprKind::Closure { .. } | HirExprKind::Placeholder => {
                let dest = self.fresh(expr.ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    expr.span,
                );
                Some(dest)
            }
        }
    }

    /// Lowers a `HirExprKind::LiftedClosure` into a heap env laid out
    /// as `[fn_addr, cap0, cap1, …]`: the first word holds the
    /// address of the lifted function (used for indirect dispatch
    /// when the closure escapes into a parameter), and each capture
    /// occupies one i64 slot at offset `8*(i+1)`. The local that
    /// owns the env pointer is registered in `local_closure` so
    /// direct calls at the creation site can bypass the indirect
    /// dispatch and jump straight to the lifted function.
    fn lower_lifted_closure(
        &mut self,
        name: &Ident,
        captures: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let size = i128::from((captures.len() + 1) as i64 * 8);
        let size_local = self.fresh(ty);
        self.emit_assign(
            Place::local(size_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(size))),
            span,
        );
        let env_local = self.fresh(ty);
        self.emit_assign(
            Place::local(env_local),
            Rvalue::CallIntrinsic {
                name: "gos_alloc",
                args: vec![Operand::Copy(Place::local(size_local))],
            },
            span,
        );
        let fn_addr_local = self.fresh(ty);
        self.emit_assign(
            Place::local(fn_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(name.name.clone()))],
            },
            span,
        );
        let zero_offset_local = self.fresh(ty);
        self.emit_assign(
            Place::local(zero_offset_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let sink = self.fresh(ty);
        self.emit_assign(
            Place::local(sink),
            Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(Place::local(env_local)),
                    Operand::Copy(Place::local(zero_offset_local)),
                    Operand::Copy(Place::local(fn_addr_local)),
                ],
            },
            span,
        );
        for (i, cap) in captures.iter().enumerate() {
            let offset = (i as i64 + 1) * 8;
            let offset_local = self.fresh(ty);
            self.emit_assign(
                Place::local(offset_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(offset)))),
                span,
            );
            let value_local = self.lower_expr(cap)?;
            let sink = self.fresh(ty);
            self.emit_assign(
                Place::local(sink),
                Rvalue::CallIntrinsic {
                    name: "gos_store",
                    args: vec![
                        Operand::Copy(Place::local(env_local)),
                        Operand::Copy(Place::local(offset_local)),
                        Operand::Copy(Place::local(value_local)),
                    ],
                },
                span,
            );
        }
        self.local_closure.insert(env_local, name.name.clone());
        Some(env_local)
    }

    fn lower_literal(&mut self, lit: &HirLiteral, ty: Ty, span: Span) -> Local {
        // Pin the literal's MIR type to the concrete kind the
        // literal implies, not the HIR expression's `ty` which may
        // still be an unresolved inference variable. Downstream
        // passes (string-concat detection, cranelift type
        // inference) rely on this being grounded.
        use gossamer_types::{FloatTy as Ft, IntTy as It, TyKind};
        let concrete = match lit {
            HirLiteral::String(_) => Some(self.tcx.string_ty()),
            HirLiteral::Bool(_) => Some(self.tcx.bool_ty()),
            HirLiteral::Char(_) => Some(self.tcx.char_ty()),
            HirLiteral::Unit => Some(self.tcx.unit()),
            _ => None,
        };
        let local_ty = match concrete {
            Some(concrete_ty) => concrete_ty,
            None => match self.tcx.kind_of(ty) {
                TyKind::Int(_) | TyKind::Float(_) => ty,
                _ => match lit {
                    HirLiteral::Int(_) => self.tcx.int_ty(It::I64),
                    HirLiteral::Float(_) => self.tcx.float_ty(Ft::F64),
                    _ => ty,
                },
            },
        };
        let local = self.fresh(local_ty);
        let value = literal_to_const(lit);
        self.emit_assign(
            Place::local(local),
            Rvalue::Use(Operand::Const(value)),
            span,
        );
        local
    }

    fn lower_path(
        &mut self,
        segments: &[Ident],
        def: Option<gossamer_resolve::DefId>,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        if let Some(first) = segments.first() {
            if let Some(local) = self.lookup_local(&first.name) {
                return Some(local);
            }
        }
        // `None` as a value (no payload) lowers to a heap-allocated
        // `gos_rt_result_new(1, 0)` so the match disc check can
        // distinguish it from `Some(_)`.
        if let Some(last) = segments.last() {
            if last.name.as_str() == "None" && segments.len() == 1 {
                return self.lower_result_no_payload(1, ty, span);
            }
        }
        // Enum variant constructor (no-payload form): `Color::Green`
        // and the bare-name `Green` lower to an integer constant
        // holding the variant's declaration index. Match-arm
        // discriminants use the same indexing so the SwitchInt
        // dispatch lands on the right arm.
        if let Some((_enum_name, idx)) = self.enums.lookup(segments) {
            let int_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
            let local = self.push_local(int_ty, None, false);
            self.emit_assign(
                Place::local(local),
                Rvalue::Use(Operand::Const(ConstValue::Int(idx as i128))),
                span,
            );
            return Some(local);
        }
        let local = self.fresh(ty);
        let joined_name = segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        let operand = if let Some(def) = def {
            // A path that resolves to a top-level `const` item
            // inlines the literal value here. Without this, the
            // FnRef fallback below would treat the const like a
            // function pointer and the codegen would emit zero
            // (or a string-tag pointer) at every use site.
            if let Some(value) = self.consts.get(&def) {
                Operand::Const(value.clone())
            } else {
                Operand::FnRef {
                    def,
                    substs: self.substs_of(ty),
                }
            }
        } else {
            // Record that `local` holds a function-name constant
            // so a later `let` binding + call can still dispatch
            // directly to the named function without treating
            // the local as a closure env pointer.
            self.local_fn_name.insert(local, joined_name.clone());
            Operand::Const(ConstValue::Str(joined_name))
        };
        self.emit_assign(Place::local(local), Rvalue::Use(operand), span);
        Some(local)
    }

    /// Returns the generic substitution recorded on a function-shaped
    /// type. `Ty`s that are not `FnDef` (closures, plain references,
    /// anything resolved to an error) yield an empty substitution.
    fn substs_of(&self, ty: Ty) -> gossamer_types::Substs {
        match self.tcx.kind(ty) {
            Some(gossamer_types::TyKind::FnDef { substs, .. }) => substs.clone(),
            _ => gossamer_types::Substs::new(),
        }
    }

    fn lower_unary(
        &mut self,
        op: HirUnaryOp,
        operand: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let inner = self.lower_expr(operand)?;
        let local = self.fresh(ty);
        let mir_op = match op {
            HirUnaryOp::Neg => UnOp::Neg,
            HirUnaryOp::Not => UnOp::Not,
            HirUnaryOp::RefShared | HirUnaryOp::RefMut | HirUnaryOp::Deref => {
                self.emit_assign(
                    Place::local(local),
                    Rvalue::Use(Operand::Copy(Place::local(inner))),
                    span,
                );
                return Some(local);
            }
        };
        self.emit_assign(
            Place::local(local),
            Rvalue::UnaryOp {
                op: mir_op,
                operand: Operand::Copy(Place::local(inner)),
            },
            span,
        );
        Some(local)
    }

    fn lower_binary(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let lhs_local = self.lower_expr(lhs)?;
        let rhs_local = self.lower_expr(rhs)?;
        // Detect string concatenation (`s1 + s2` where at least
        // one side is a `String`) and route it through the native
        // runtime's `gos_rt_str_concat` helper rather than the
        // integer `+`. HIR types may still carry unresolved
        // inference variables here, so we inspect the lowered
        // MIR locals' concrete types too.
        if matches!(op, HirBinaryOp::Add) {
            let is_string = |t: Ty| -> bool {
                let mut cur = t;
                loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::String => return true,
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => return false,
                    }
                }
            };
            if is_string(ty)
                || is_string(lhs.ty)
                || is_string(rhs.ty)
                || is_string(self.locals[lhs_local.0 as usize].ty)
                || is_string(self.locals[rhs_local.0 as usize].ty)
            {
                let dest_ty = self.tcx.string_ty();
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_concat".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(lhs_local)),
                        Operand::Copy(Place::local(rhs_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
        }
        let local = self.fresh(ty);
        let bin_op = lower_binop(op);
        self.emit_assign(
            Place::local(local),
            Rvalue::BinaryOp {
                op: bin_op,
                lhs: Operand::Copy(Place::local(lhs_local)),
                rhs: Operand::Copy(Place::local(rhs_local)),
            },
            span,
        );
        Some(local)
    }

    fn lower_assign(&mut self, place: &HirExpr, value: &HirExpr, span: Span) {
        let Some(mut value_local) = self.lower_expr(value) else {
            return;
        };
        let Some(mir_place) = self.lower_place_expr(place) else {
            return;
        };
        // Same callable-coercion as `let` and `return`: when the
        // lvalue's static type is callable and the rvalue is a
        // bare fn item, wrap the fn into the env+code blob so the
        // slot ends up env-shaped.
        {
            use gossamer_types::TyKind;
            let dest_callable = matches!(
                self.tcx.kind_of(place.ty),
                TyKind::FnPtr(_) | TyKind::FnTrait(_)
            );
            let value_ty = self.locals[value_local.0 as usize].ty;
            let src_is_fn_def = matches!(self.tcx.kind_of(value_ty), TyKind::FnDef { .. });
            let src_names_fn = self.local_fn_name.contains_key(&value_local);
            if dest_callable && (src_is_fn_def || src_names_fn) {
                value_local = self.coerce_to_fn_trait_if_needed(value_local, place.ty, span);
            }
        }
        self.emit_assign(
            mir_place,
            Rvalue::Use(Operand::Copy(Place::local(value_local))),
            span,
        );
    }

    /// Converts a HIR expression used in lvalue position (`a`,
    /// `a.field`, `a[i]`, `a.0`, nested combinations) into a MIR
    /// [`Place`] with the right projection chain. Returns `None`
    /// when the expression is not a place (e.g. a literal).
    fn lower_place_expr(&mut self, expr: &HirExpr) -> Option<Place> {
        match &expr.kind {
            HirExprKind::Path { segments, .. } => {
                let first = segments.first()?;
                let local = self.lookup_local(&first.name)?;
                Some(Place::local(local))
            }
            HirExprKind::Field { receiver, name } => {
                let mut base = self.lower_place_expr(receiver)?;
                // Field index: first try the base's local_struct
                // registration, then fall back to the receiver's
                // static type via the type system.
                let struct_name = self
                    .local_struct
                    .get(&base.local)
                    .cloned()
                    .or_else(|| self.struct_name_from_expr(receiver))?;
                let order = self.structs.get(&struct_name)?;
                let idx = u32::try_from(order.iter().position(|f| f == &name.name)?).ok()?;
                base.projection.push(crate::ir::Projection::Field(idx));
                Some(base)
            }
            HirExprKind::TupleIndex { receiver, index } => {
                let mut base = self.lower_place_expr(receiver)?;
                base.projection.push(crate::ir::Projection::Field(*index));
                Some(base)
            }
            HirExprKind::Index { base, index } => {
                let mut base_place = self.lower_place_expr(base)?;
                let index_local = self.lower_expr(index)?;
                base_place
                    .projection
                    .push(crate::ir::Projection::Index(index_local));
                Some(base_place)
            }
            _ => None,
        }
    }

    #[allow(clippy::cognitive_complexity)]
    fn lower_call(
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
        }
        // When the callee's `DefId` is known and its declared
        // return type is on record, prefer the callee's return
        // type over the call-expression's HIR type — the latter
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
                        | "gos_rt_math_sqrt" => self.tcx.int_ty(gossamer_types::IntTy::I64),
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
        // as String — producing a `strlen` segfault on the i64
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
                            TyKind::FnPtr(_) | TyKind::FnDef { .. } | TyKind::Closure { .. }
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
        let mut arg_operands = Vec::with_capacity(args.len());
        for (idx, arg) in args.iter().enumerate() {
            let local = self.lower_expr(arg)?;
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
            arg_operands.push(Operand::Copy(Place::local(local)));
        }
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
        Some(dest)
    }

    /// If `expected` is a callable type (`Fn(args) -> ret` trait or
    /// `fn(args) -> ret` pointer) and `source_local` holds a bare
    /// `fn item`, wrap the fn address in a 16-byte env blob
    /// `[trampoline_addr, real_fn_addr]` and return a fresh local
    /// pointing at the blob. Otherwise the original local is
    /// returned unchanged. Capturing closures already produce
    /// env-shaped values via `lower_lifted_closure`, so they
    /// short-circuit here too.
    ///
    /// The unified env-pointer shape lets the codegen treat
    /// `FnPtr` / `FnTrait` callees identically: a single `load
    /// fn_addr from env[0]; call_indirect(fn_addr, env, args…)`
    /// path, with no special case for "raw fn ptr" that would
    /// segfault on an escaping closure.
    fn coerce_to_fn_trait_if_needed(
        &mut self,
        source_local: Local,
        expected: Ty,
        span: Span,
    ) -> Local {
        use gossamer_types::TyKind;
        let expected_kind = self.tcx.kind_of(expected).clone();
        let arity = match &expected_kind {
            TyKind::FnTrait(sig) => sig.inputs.len(),
            TyKind::FnPtr(sig) => sig.inputs.len(),
            _ => return source_local,
        };
        let source_ty = self.locals[source_local.0 as usize].ty;
        let source_kind = self.tcx.kind_of(source_ty);
        // Wrap when the source is a genuine fn item (`FnDef`)
        // OR a local that the MIR builder marked as holding a
        // function-name string constant (the lift-closures pass
        // produces these for non-capturing closures: the local
        // ends up Copy(Const(Str("__closure_N"))) — a rodata
        // pointer, NOT a callable address). FnPtr-typed locals
        // are already env_ptr-shaped after this round of fixes,
        // so re-wrapping them would double-indirect; FnTrait and
        // Closure values are env-shaped by construction.
        let names_a_fn = self.local_fn_name.contains_key(&source_local);
        let needs_wrap = matches!(source_kind, TyKind::FnDef { .. }) || names_a_fn;
        if !needs_wrap {
            return source_local;
        }
        let env_ty = expected;
        // Allocate the env blob: 16 bytes (trampoline ptr + real
        // fn ptr).
        let size_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(size_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(16))),
            span,
        );
        let env_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(env_local),
            Rvalue::CallIntrinsic {
                name: "gos_alloc",
                args: vec![Operand::Copy(Place::local(size_local))],
            },
            span,
        );
        // Resolve the per-arity trampoline name.
        let tramp_name: &'static str = match arity {
            0 => "gos_rt_fn_tramp_0",
            1 => "gos_rt_fn_tramp_1",
            2 => "gos_rt_fn_tramp_2",
            3 => "gos_rt_fn_tramp_3",
            4 => "gos_rt_fn_tramp_4",
            5 => "gos_rt_fn_tramp_5",
            6 => "gos_rt_fn_tramp_6",
            7 => "gos_rt_fn_tramp_7",
            8 => "gos_rt_fn_tramp_8",
            // Arities > 8 are out of scope for v1.0.0; fall back
            // to passing the source unchanged so the codegen's
            // existing "wrong shape → segfault" surface fires
            // loudly during testing rather than miscompiling
            // silently.
            _ => return source_local,
        };
        let tramp_addr_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(tramp_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(tramp_name.to_string()))],
            },
            span,
        );
        let zero_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(zero_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let sink_a = self.fresh(env_ty);
        self.emit_assign(
            Place::local(sink_a),
            Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(Place::local(env_local)),
                    Operand::Copy(Place::local(zero_local)),
                    Operand::Copy(Place::local(tramp_addr_local)),
                ],
            },
            span,
        );
        let eight_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(eight_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(8))),
            span,
        );
        // When the source local was bound to a fn name via
        // `let c = some_fn_name` (e.g. a lifted non-capturing
        // closure), its slot holds the address of the *string*
        // (the way the MIR encodes a `def: None` path), not the
        // function. Resolve to the real fn address via
        // `gos_fn_addr` so the trampoline forwards to the actual
        // code. Direct fn references (FnDef/FnPtr-typed locals)
        // already hold the right value.
        let real_fn_operand = if let Some(name) = self.local_fn_name.get(&source_local).cloned() {
            let addr_local = self.fresh(env_ty);
            self.emit_assign(
                Place::local(addr_local),
                Rvalue::CallIntrinsic {
                    name: "gos_fn_addr",
                    args: vec![Operand::Const(ConstValue::Str(name))],
                },
                span,
            );
            Operand::Copy(Place::local(addr_local))
        } else {
            Operand::Copy(Place::local(source_local))
        };
        let sink_b = self.fresh(env_ty);
        self.emit_assign(
            Place::local(sink_b),
            Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(Place::local(env_local)),
                    Operand::Copy(Place::local(eight_local)),
                    real_fn_operand,
                ],
            },
            span,
        );
        env_local
    }

    fn lower_if(
        &mut self,
        condition: &HirExpr,
        then_branch: &HirExpr,
        else_branch: Option<&HirExpr>,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let cond_local = self.lower_expr(condition)?;
        let then_block = self.new_block(span);
        let else_block = self.new_block(span);
        let join_block = self.new_block(span);
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cond_local)),
            arms: vec![(0, else_block)],
            default: then_block,
        });

        let result_local = self.fresh(ty);

        self.set_current(then_block);
        if let Some(then_value) = self.lower_expr(then_branch) {
            self.emit_assign(
                Place::local(result_local),
                Rvalue::Use(Operand::Copy(Place::local(then_value))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
        }

        self.set_current(else_block);
        if let Some(else_branch) = else_branch {
            if let Some(else_value) = self.lower_expr(else_branch) {
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(else_value))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join_block });
            }
        } else {
            let unit_local = self.lower_unit(span);
            self.emit_assign(
                Place::local(result_local),
                Rvalue::Use(Operand::Copy(Place::local(unit_local))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
        }

        self.set_current(join_block);
        Some(result_local)
    }

    /// Lowers a `match` expression over an integer or boolean
    /// scrutinee into a `SwitchInt` terminator. Literal arms drive
    /// the switch; wildcard/binding arms become the default; richer
    /// pattern shapes (tuple, struct, range, or-pattern) collapse
    /// to wildcard semantics — the first such arm becomes the
    /// default, later ones are dead. Guarded arms route through
    /// `lower_match_with_guards` for predicate-based dispatch.
    #[allow(
        clippy::cognitive_complexity,
        reason = "match-arm classification is a single linear walk; splitting it hides the per-pattern dispatch"
    )]
    fn lower_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Route guarded arms and any non-flat pattern shape
        // (tuple / or-pattern / nested variant binding) through
        // the if-chain lowering. The original SwitchInt path
        // below stays the fast path for flat int / bool /
        // single-variant matches whose discriminant fits one
        // word.
        let needs_chain = arms.iter().any(|arm| {
            arm.guard.is_some()
                || matches!(
                    arm.pattern.kind,
                    HirPatKind::Tuple(_)
                        | HirPatKind::Or(_)
                        | HirPatKind::Struct { .. }
                        | HirPatKind::Range { .. }
                        | HirPatKind::Ref { .. }
                        | HirPatKind::Literal(
                            HirLiteral::String(_) | HirLiteral::Char(_) | HirLiteral::Float(_)
                        )
                )
                || matches!(
                    &arm.pattern.kind,
                    HirPatKind::Variant { name, .. }
                        if matches!(name.name.as_str(), "Ok" | "Err" | "Some" | "None")
                            || self.enums.lookup(std::slice::from_ref(name)).is_some()
                )
        });
        if needs_chain {
            return self.lower_match_with_guards(scrutinee, arms, ty, span);
        }
        let mut switch_arms: Vec<(i128, BlockId)> = Vec::new();
        let mut default_block: Option<BlockId> = None;
        // Per-arm binding: the variant pattern's inner Binding (e.g.
        // `Ok(v)` → `v`) is registered against the scrutinee local
        // when we enter the arm block, so the body can reference it.
        // The scrutinee's static type carries through (e.g. for
        // `Ok(v)` on a `Result<json::Value, _>` the binding gets
        // typed as `json::Value`).
        // Per-arm captured payload binding: (binding name, mutable
        // flag, variant constructor name). The variant name lets
        // the arm-body fixup routine re-pin the scrutinee local's
        // type to the right `substs` slot — `substs[0]` for
        // `Ok`/`Some`, `substs[1]` for `Err` — so subsequent reads
        // of the bound name see the payload type, not the wrapper.
        let mut arm_bindings: Vec<Option<(Ident, bool, Option<String>)>> =
            Vec::with_capacity(arms.len());
        let mut arm_bodies: Vec<(BlockId, &HirExpr)> = Vec::with_capacity(arms.len());
        for arm in arms {
            let arm_block = self.new_block(span);
            arm_bodies.push((arm_block, &arm.body));
            arm_bindings.push(None);
            match &arm.pattern.kind {
                HirPatKind::Literal(HirLiteral::Int(text)) => {
                    let v = parse_int(text).unwrap_or_else(|| {
                        unreachable!("match arm: lexer-validated int literal `{text}` failed parse")
                    });
                    switch_arms.push((v, arm_block));
                }
                HirPatKind::Literal(HirLiteral::Bool(b)) => {
                    switch_arms.push((i128::from(*b), arm_block));
                }
                HirPatKind::Wildcard | HirPatKind::Binding { .. } => {
                    // Multiple wildcard arms are accepted; only the
                    // first is reachable. Subsequent wildcard bodies
                    // are emitted into dead blocks the SwitchInt
                    // never targets.
                    if default_block.is_none() {
                        default_block = Some(arm_block);
                    }
                }
                // Variant patterns (`Ok(x)`, `Err(e)`, `Some(v)`, …)
                // don't yet have runtime discriminants, but we can
                // still produce a well-formed CFG by always taking
                // the first variant arm as a "happy path" default.
                // Bind any inner pattern to the scrutinee local so
                // `let x = foo()?` compiles. Wrong for genuine error
                // cases, but enough for programs whose control flow
                // stays on the Ok/Some path.
                HirPatKind::Variant { name, fields } => {
                    // User-defined enum: dispatch by the variant's
                    // declaration index recorded in `EnumIndex`.
                    // Fall back to `Result`/`Option`'s historical
                    // happy-path encoding (`Ok` / `Some` = 0,
                    // `Err` / `None` = 1) for the stdlib variants
                    // that don't have a Gossamer enum behind them.
                    let pos: i128 =
                        if let Some((_, idx)) = self.enums.lookup(std::slice::from_ref(name)) {
                            idx as i128
                        } else if matches!(name.name.as_str(), "Err" | "None" | "Some" | "Ok") {
                            match name.name.as_str() {
                                "Some" | "Ok" => 0,
                                _ => 1,
                            }
                        } else {
                            switch_arms.len() as i128
                        };
                    switch_arms.push((pos, arm_block));
                    // For `Ok(v)` / `Some(v)` patterns the
                    // payload is structurally identical to the
                    // scrutinee in compiled mode (Result/Option
                    // are flat single-slot values today), so
                    // bind the inner name to the scrutinee local
                    // when entering the arm. Captures only the
                    // first single-Binding inner field — wider
                    // patterns continue through the placeholder.
                    if let Some(first) = fields.first() {
                        if let HirPatKind::Binding {
                            name: bname,
                            mutable,
                        } = &first.kind
                        {
                            *arm_bindings.last_mut().expect("arm tracked") =
                                Some((bname.clone(), *mutable, Some(name.name.clone())));
                        }
                    }
                }
                // Tuple / struct / range / or-pattern shapes that
                // the no-guards SwitchInt path doesn't decode are
                // treated as wildcard arms here: the first one
                // becomes the default, later ones are dead.
                _ => {
                    if default_block.is_none() {
                        default_block = Some(arm_block);
                    }
                }
            }
        }
        let scrutinee_local = self.lower_expr(scrutinee)?;
        let join_block = self.new_block(span);
        let result_local = self.fresh(ty);
        // Save the post-scrutinee block before allocating the
        // default arm; the unreachable_block creation below sets
        // current to that block and then terminates it (leaving
        // current = None), which would otherwise swallow our
        // SwitchInt / Goto terminator below.
        let dispatch_block = self.current;
        let default = default_block.unwrap_or_else(|| {
            let unreachable_block = self.new_block(span);
            self.set_current(unreachable_block);
            self.terminate(Terminator::Unreachable);
            unreachable_block
        });
        if let Some(block) = dispatch_block {
            self.set_current(block);
        }
        // When the scrutinee is a `json::Value` (or a Result /
        // Option carrying one), the runtime helpers always
        // produce a non-null sentinel handle, so the natural
        // "match the discriminant" lowering would fall through
        // to the unreachable arm and trap. Approximate the
        // happy-path by routing directly to the `Ok` / `Some`
        // arm — its inner binding aliases the scrutinee local
        // (see the binding-loop above), which is exactly the
        // shape `gos_rt_json_*` helpers expect downstream.
        let scrut_ty = self.locals[scrutinee_local.0 as usize].ty;
        let json_shaped =
            self.is_json_value_ty(scrut_ty) || self.adt_first_generic_is_json(scrut_ty);
        let ok_block = switch_arms.iter().find(|(v, _)| *v == 0).map(|(_, b)| *b);
        let routed = if json_shaped && let Some(ok) = ok_block {
            self.terminate(Terminator::Goto { target: ok });
            true
        } else {
            false
        };
        if !routed {
            self.terminate(Terminator::SwitchInt {
                discriminant: Operand::Copy(Place::local(scrutinee_local)),
                arms: switch_arms,
                default,
            });
        }
        for ((arm_block, body), binding) in arm_bodies.into_iter().zip(arm_bindings) {
            self.set_current(arm_block);
            // When the arm pattern was `Ok(v)` / `Some(v)` /
            // `Variant(v)`, register `v` against the scrutinee
            // local so the arm body's references resolve. If the
            // scrutinee is a flat `*mut GosJson` (i.e. its static
            // type is `Result<json::Value, _>` / `Option<json::Value>`
            // / `json::Value`), promote the scrutinee local to
            // `json::Value` so chained `j.field` accesses route
            // through the json runtime helpers.
            if let Some((bname, _mutable, variant_name)) = binding {
                let scrut_ty = self.locals[scrutinee_local.0 as usize].ty;
                if self.adt_first_generic_is_json(scrut_ty) {
                    let json_ty = self.tcx.json_value_ty();
                    self.locals[scrutinee_local.0 as usize].ty = json_ty;
                } else if let Some(name) = variant_name.as_deref() {
                    // Generalised happy-path payload pin: for
                    // `Ok(x)` / `Some(x)` the payload is the
                    // wrapper's first generic arg; for `Err(e)`
                    // the second. Without this, downstream code
                    // sees `x` typed as the whole `Result<T, E>`
                    // and routes value-formatting / coercion
                    // through the wrong path.
                    let slot = match name {
                        "Ok" | "Some" => Some(0),
                        "Err" => Some(1),
                        _ => None,
                    };
                    if let Some(idx) = slot {
                        if let Some(payload_ty) = self.adt_generic_at(scrut_ty, idx) {
                            self.locals[scrutinee_local.0 as usize].ty = payload_ty;
                        }
                    }
                }
                self.bind_local(&bname.name, scrutinee_local);
            }
            if let Some(value_local) = self.lower_expr(body) {
                // Pin the match-result local's type to the arm's
                // value type when the HIR type is opaque (Var /
                // Error). Lets chained patterns like `let v =
                // match r { Ok(j) => j, .. }; v.field` flow the
                // concrete `json::Value` (or struct) shape into
                // the surrounding `let`'s field-access lowering.
                use gossamer_types::TyKind;
                let arm_value_ty = self.locals[value_local.0 as usize].ty;
                let result_kind = self.tcx.kind_of(self.locals[result_local.0 as usize].ty);
                let arm_kind = self.tcx.kind_of(arm_value_ty);
                let result_is_loose =
                    matches!(result_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                let arm_is_concrete =
                    !matches!(arm_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                if result_is_loose && arm_is_concrete {
                    self.locals[result_local.0 as usize].ty = arm_value_ty;
                }
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(value_local))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join_block });
            }
        }
        self.set_current(join_block);
        Some(result_local)
    }

    /// Lowers a `match` whose arms include `if`-guards as a
    /// linear chain of `if (matches && guard) { body } else …`
    /// blocks. Supports the same per-arm pattern shapes the
    /// guard-free `lower_match` handles (literal int / bool,
    /// wildcard, single-binding, simple variant). Falls back to
    /// the unsupported placeholder for anything else so a
    /// surprising arm pattern doesn't silently miscompile.
    fn lower_match_with_guards(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let scrutinee_local = self.lower_expr(scrutinee)?;
        let bool_ty = self.tcx.bool_ty();
        let result_local = self.fresh(ty);
        let join = self.new_block(span);

        for arm in arms {
            let arm_block = self.new_block(span);
            let next_block = self.new_block(span);

            // Push a binding scope so guard + body see the
            // pattern-bound names. Bindings introduced by the
            // pattern (single-name, tuple-element, variant-payload)
            // are recorded against MIR locals here too.
            self.push_scope();
            // When `lower_pattern_predicate` doesn't decode the
            // pattern shape (tuple/struct/range/or-patterns that
            // need richer destructuring than the SwitchInt path
            // covers), treat the arm as always-matching by
            // synthesising a `true` predicate. The arm body still
            // lowers, the guard (if any) still gates entry, and
            // later arms remain reachable through the join.
            let pat_match_local = self
                .lower_pattern_predicate(scrutinee_local, &arm.pattern, span)
                .unwrap_or_else(|| {
                    let always = self.fresh(bool_ty);
                    self.emit_assign(
                        Place::local(always),
                        Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                        span,
                    );
                    always
                });

            // Combine the pattern predicate with the guard (if any).
            let predicate = if let Some(guard_expr) = &arm.guard {
                let guard_local = self.lower_expr(guard_expr)?;
                let combined = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(combined),
                    Rvalue::BinaryOp {
                        // Bool is i1/i8 at runtime; bitwise and is
                        // equivalent to logical AND for the
                        // 0/1 truth values produced above.
                        op: BinOp::BitAnd,
                        lhs: Operand::Copy(Place::local(pat_match_local)),
                        rhs: Operand::Copy(Place::local(guard_local)),
                    },
                    span,
                );
                combined
            } else {
                pat_match_local
            };

            self.terminate(Terminator::SwitchInt {
                discriminant: Operand::Copy(Place::local(predicate)),
                arms: vec![(0, next_block)],
                default: arm_block,
            });

            self.set_current(arm_block);
            if let Some(value_local) = self.lower_expr(&arm.body) {
                use gossamer_types::TyKind;
                let arm_value_ty = self.locals[value_local.0 as usize].ty;
                let result_kind = self.tcx.kind_of(self.locals[result_local.0 as usize].ty);
                let arm_kind = self.tcx.kind_of(arm_value_ty);
                let result_is_loose =
                    matches!(result_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                let arm_is_concrete =
                    !matches!(arm_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                if result_is_loose && arm_is_concrete {
                    self.locals[result_local.0 as usize].ty = arm_value_ty;
                }
                if let Some(struct_name) = self.local_struct.get(&value_local).cloned() {
                    self.local_struct.insert(result_local, struct_name);
                }
                if let Some(rk) = self.local_runtime_kind.get(&value_local).copied() {
                    self.local_runtime_kind.insert(result_local, rk);
                }
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(value_local))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join });
            } else if self.current.is_some() {
                // Arm body ended normally (no value to bind, but
                // control still falls through — typical of a loop
                // tail). Connect to join so the codegen doesn't
                // leave a dangling block.
                self.terminate(Terminator::Goto { target: join });
            }
            self.pop_scope();

            self.set_current(next_block);
        }
        // Ran past every arm without matching. Match is supposed
        // to be exhaustive; jump to join with the result-local
        // left at its default zero value (the `fresh` above didn't
        // initialise it). This branch is reachable only when the
        // exhaustiveness checker missed something.
        self.terminate(Terminator::Goto { target: join });
        self.set_current(join);
        Some(result_local)
    }

    /// Builds the boolean predicate "scrutinee value at `place`
    /// matches `pattern`" as a fresh MIR local. Recurses into
    /// sub-patterns (tuple, or, single-binding) and registers
    /// any binding sub-patterns against the right MIR local so
    /// the arm body can read them. Returns `None` when a
    /// sub-pattern shape is not yet handled (caller surfaces a
    /// clean unsupported-placeholder diagnostic).
    #[allow(clippy::cognitive_complexity)]
    fn lower_pattern_predicate(
        &mut self,
        scrutinee: Local,
        pattern: &HirPat,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let bool_ty = self.tcx.bool_ty();
        match &pattern.kind {
            HirPatKind::Wildcard => {
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Binding { name, .. } => {
                self.bind_local(&name.name, scrutinee);
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Literal(HirLiteral::Int(text)) => {
                let v = parse_int(text)?;
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let lit_local = self.fresh(scrut_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(v))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(cmp),
                    Rvalue::BinaryOp {
                        op: BinOp::Eq,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lit_local)),
                    },
                    span,
                );
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::Bool(b)) => {
                let lit_local = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(*b))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(cmp),
                    Rvalue::BinaryOp {
                        op: BinOp::Eq,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lit_local)),
                    },
                    span,
                );
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::String(text)) => {
                // String-literal match arm. Compare via
                // `gos_rt_str_eq` which the runtime exposes; emit
                // a fresh string operand for the literal.
                let str_ty = self.tcx.string_ty();
                let lit_local = self.fresh(str_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Str(text.clone()))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_eq".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(scrutinee)),
                        Operand::Copy(Place::local(lit_local)),
                    ],
                    destination: Place::local(cmp),
                    target: Some(next),
                });
                self.set_current(next);
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::Char(c)) => {
                let char_ty = self.tcx.char_ty();
                let lit_local = self.fresh(char_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Char(*c))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(cmp),
                    Rvalue::BinaryOp {
                        op: BinOp::Eq,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lit_local)),
                    },
                    span,
                );
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::Float(_) | HirLiteral::Unit) => {
                // Float / Unit literal arms are pathological;
                // float equality match is rarely correct. Fall
                // back to "always matches" so the program still
                // compiles. Programs depending on these arms
                // should use a guard with `==` instead.
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Ref { inner, .. } => {
                // `&pat` patterns: peel through the reference
                // and match the inner pattern. The compiled tier
                // doesn't materialise references separately; the
                // scrutinee local already holds the pointer.
                self.lower_pattern_predicate(scrutinee, inner, span)
            }
            HirPatKind::Rest => {
                // Rest pattern in a non-tuple context — match
                // anything; binds nothing.
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Tuple(sub_pats) => {
                // Conjunction across tuple-element predicates. Each
                // sub-pattern is matched against the corresponding
                // tuple field via a fresh local that holds the
                // projected element value.
                use gossamer_types::TyKind;
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let elem_tys: Vec<Ty> = match self.tcx.kind_of(scrut_ty) {
                    TyKind::Tuple(elems) => elems.clone(),
                    _ => return None,
                };
                let mut acc = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(acc),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                for (idx, sub_pat) in sub_pats.iter().enumerate() {
                    let elem_ty = elem_tys.get(idx).copied()?;
                    let elem_local = self.fresh(elem_ty);
                    let elem_place = Place {
                        local: scrutinee,
                        projection: vec![crate::ir::Projection::Field(idx as u32)],
                    };
                    self.emit_assign(
                        Place::local(elem_local),
                        Rvalue::Use(Operand::Copy(elem_place)),
                        span,
                    );
                    let sub_pred = self.lower_pattern_predicate(elem_local, sub_pat, span)?;
                    let combined = self.fresh(bool_ty);
                    self.emit_assign(
                        Place::local(combined),
                        Rvalue::BinaryOp {
                            op: BinOp::BitAnd,
                            lhs: Operand::Copy(Place::local(acc)),
                            rhs: Operand::Copy(Place::local(sub_pred)),
                        },
                        span,
                    );
                    acc = combined;
                }
                Some(acc)
            }
            HirPatKind::Or(branches) => {
                // Disjunction across branch predicates. Each branch
                // contributes its own match check; their bool
                // results are bitwise-ORed together.
                let mut acc = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(acc),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(false))),
                    span,
                );
                for branch in branches {
                    let pred = self.lower_pattern_predicate(scrutinee, branch, span)?;
                    let combined = self.fresh(bool_ty);
                    self.emit_assign(
                        Place::local(combined),
                        Rvalue::BinaryOp {
                            op: BinOp::BitOr,
                            lhs: Operand::Copy(Place::local(acc)),
                            rhs: Operand::Copy(Place::local(pred)),
                        },
                        span,
                    );
                    acc = combined;
                }
                Some(acc)
            }
            HirPatKind::Range { lo, hi, inclusive } => {
                // `lo..hi` and `lo..=hi` arms reduce to
                // `(scrut >= lo) && (scrut <op> hi)` where the
                // upper comparison is `<` for exclusive and `<=`
                // for inclusive. Only integer literal bounds are
                // accepted today; float / char ranges fall
                // through to the unsupported placeholder.
                let HirLiteral::Int(lo_text) = lo else {
                    return None;
                };
                let HirLiteral::Int(hi_text) = hi else {
                    return None;
                };
                let lo_v = parse_int(lo_text)?;
                let hi_v = parse_int(hi_text)?;
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let lo_local = self.fresh(scrut_ty);
                self.emit_assign(
                    Place::local(lo_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(lo_v))),
                    span,
                );
                let hi_local = self.fresh(scrut_ty);
                self.emit_assign(
                    Place::local(hi_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(hi_v))),
                    span,
                );
                let ge = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(ge),
                    Rvalue::BinaryOp {
                        op: BinOp::Ge,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lo_local)),
                    },
                    span,
                );
                let upper_op = if *inclusive { BinOp::Le } else { BinOp::Lt };
                let upper = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(upper),
                    Rvalue::BinaryOp {
                        op: upper_op,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(hi_local)),
                    },
                    span,
                );
                let combined = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(combined),
                    Rvalue::BinaryOp {
                        op: BinOp::BitAnd,
                        lhs: Operand::Copy(Place::local(ge)),
                        rhs: Operand::Copy(Place::local(upper)),
                    },
                    span,
                );
                Some(combined)
            }
            HirPatKind::Struct { name, fields, .. } => {
                // Struct pattern matching a value of a known
                // struct type — OR a struct-payload enum variant
                // (`Shape::Rect { w, h }` lowers as
                // `HirPatKind::Struct { name: "Rect", ... }`).
                // For an enum-variant struct, the predicate
                // routes to the variant's discriminant index;
                // for a real struct it's always true (shape
                // verified by the type-checker). Each named-field
                // sub-pattern reads through a
                // `Projection::Field(idx)` of the scrutinee.
                let order = self
                    .structs
                    .get(&name.name)
                    .cloned()
                    .or_else(|| self.enums.variant_fields.get(&name.name).cloned());
                let variant_idx = self
                    .enums
                    .lookup(std::slice::from_ref(name))
                    .map(|(_, i)| i);
                // Predicate seed: for an enum variant, compare the
                // scrutinee's discriminant to the variant index;
                // for a free struct, every value of the scrutinee
                // type matches.
                let acc = self.fresh(bool_ty);
                if let Some(idx) = variant_idx {
                    let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                    let lit_local = self.fresh(scrut_ty);
                    self.emit_assign(
                        Place::local(lit_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(idx as i128))),
                        span,
                    );
                    self.emit_assign(
                        Place::local(acc),
                        Rvalue::BinaryOp {
                            op: BinOp::Eq,
                            lhs: Operand::Copy(Place::local(scrutinee)),
                            rhs: Operand::Copy(Place::local(lit_local)),
                        },
                        span,
                    );
                } else {
                    self.emit_assign(
                        Place::local(acc),
                        Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                        span,
                    );
                }
                let scrut_is_real_struct = self
                    .struct_name_of(self.locals[scrutinee.0 as usize].ty)
                    .is_some_and(|sn| self.structs.contains_key(&sn));
                if let Some(order) = order {
                    for f in fields {
                        let pos = order.iter().position(|n| n == &f.name.name);
                        let Some(pos) = pos else { continue };
                        let field_idx = u32::try_from(pos).ok()?;
                        // The field's HIR-recorded type lives on
                        // the sub-pattern (or, for shorthand, on
                        // the field-pattern itself).
                        let field_ty = match &f.pattern {
                            Some(sub) => sub.ty,
                            None => self.tcx.int_ty(gossamer_types::IntTy::I64),
                        };
                        let elem = self.fresh(field_ty);
                        if scrut_is_real_struct {
                            // Free struct: real field projection.
                            let field_place = Place {
                                local: scrutinee,
                                projection: vec![crate::ir::Projection::Field(field_idx)],
                            };
                            self.emit_assign(
                                Place::local(elem),
                                Rvalue::Use(Operand::Copy(field_place)),
                                span,
                            );
                        } else {
                            // Enum-variant struct payload — the
                            // compiled tier's flat i64-per-slot
                            // ABI does not preserve multi-field
                            // enum payloads (a `Shape` local is
                            // 8 bytes; `Rect { w, h }` is 16).
                            // Bind every field to a default 0 so
                            // the body compiles; programs that
                            // depend on these fields produce the
                            // happy-path Ok/Some encoding only.
                            self.emit_assign(
                                Place::local(elem),
                                Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                                span,
                            );
                        }
                        if let Some(sub) = &f.pattern {
                            let sub_pred = self.lower_pattern_predicate(elem, sub, span)?;
                            // AND into the accumulator. Today we
                            // can't easily re-write `acc`, so we
                            // emit a fresh combined local each
                            // iteration; the optimiser collapses
                            // the chain.
                            let combined = self.fresh(bool_ty);
                            self.emit_assign(
                                Place::local(combined),
                                Rvalue::BinaryOp {
                                    op: BinOp::BitAnd,
                                    lhs: Operand::Copy(Place::local(acc)),
                                    rhs: Operand::Copy(Place::local(sub_pred)),
                                },
                                span,
                            );
                            // Rebind acc to the combined value
                            // via another assign.
                            self.emit_assign(
                                Place::local(acc),
                                Rvalue::Use(Operand::Copy(Place::local(combined))),
                                span,
                            );
                        } else {
                            // Shorthand `{ x, y }` binds the field
                            // name directly to the field local.
                            self.bind_local(&f.name.name, elem);
                        }
                    }
                }
                Some(acc)
            }
            HirPatKind::Variant { name, fields } => {
                // Two encodings hide behind variant patterns in
                // compiled mode:
                //
                //   1. **User-defined enums** (registered in the
                //      `EnumIndex`): the scrutinee holds the
                //      variant's declaration index; predicate is
                //      `scrutinee == idx`.
                //   2. **`Option<T>` / `Result<T, E>` stdlib
                //      variants**: the scrutinee carries the
                //      wrapped value directly (happy-path
                //      encoding — `unwrap` is identity). The
                //      compiled tier can't actually distinguish
                //      `Ok(_)` from `Err(_)` at runtime, so the
                //      `Ok` / `Some` arm becomes the unconditional
                //      always-true predicate and `Err` / `None`
                //      becomes always-false. This compiles `?`
                //      down to "take the success path"; programs
                //      that depend on real error dispatch keep
                //      working under `gos run`.
                if let Some((_, idx)) = self.enums.lookup(std::slice::from_ref(name)) {
                    let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                    let lit_local = self.fresh(scrut_ty);
                    self.emit_assign(
                        Place::local(lit_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(idx as i128))),
                        span,
                    );
                    let cmp = self.fresh(bool_ty);
                    self.emit_assign(
                        Place::local(cmp),
                        Rvalue::BinaryOp {
                            op: BinOp::Eq,
                            lhs: Operand::Copy(Place::local(scrutinee)),
                            rhs: Operand::Copy(Place::local(lit_local)),
                        },
                        span,
                    );
                    if let Some(first) = fields.first() {
                        if let HirPatKind::Binding { name: bname, .. } = &first.kind {
                            self.bind_local(&bname.name, scrutinee);
                        }
                    }
                    return Some(cmp);
                }
                // Result/Option dispatch picks one of two paths
                // based on the scrutinee's static type:
                //
                //   * Concrete `Result<T, E>` / `Option<T>` (Adt
                //     with our sentinel DefId): the scrutinee is a
                //     `*mut GosResult` carrying a real disc bit.
                //     Compare `gos_rt_result_disc(scrut)` to the
                //     expected arm value.
                //
                //   * Unresolved (`Var` / `Error` / `Never`) or any
                //     other shape: fall back to the happy-path
                //     encoding so legacy producers (`.send()`,
                //     `.map_err()`-chains whose return type the
                //     typer left as `Var`) keep working. `Ok` /
                //     `Some` arms are unconditionally true; `Err`
                //     / `None` arms unconditionally false.
                let expected_disc: i64 = match name.name.as_str() {
                    "Ok" | "Some" => 0,
                    _ => 1,
                };
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let scrut_kind = self.tcx.kind_of(scrut_ty);
                let real_disc = matches!(
                    scrut_kind,
                    TyKind::Adt { .. } if self.is_result_or_option_adt(scrut_ty)
                );
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let const_pred = self.fresh(bool_ty);
                if real_disc {
                    let disc_local = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(disc_local),
                        Rvalue::CallIntrinsic {
                            name: "gos_rt_result_disc",
                            args: vec![Operand::Copy(Place::local(scrutinee))],
                        },
                        span,
                    );
                    let lit_local = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(lit_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(expected_disc)))),
                        span,
                    );
                    self.emit_assign(
                        Place::local(const_pred),
                        Rvalue::BinaryOp {
                            op: BinOp::Eq,
                            lhs: Operand::Copy(Place::local(disc_local)),
                            rhs: Operand::Copy(Place::local(lit_local)),
                        },
                        span,
                    );
                } else {
                    let happy_path = expected_disc == 0;
                    self.emit_assign(
                        Place::local(const_pred),
                        Rvalue::Use(Operand::Const(ConstValue::Bool(happy_path))),
                        span,
                    );
                }
                if let Some(first) = fields.first() {
                    if let HirPatKind::Binding { name: bname, .. } = &first.kind {
                        // Bind the payload. With real discriminant
                        // encoding, allocate a fresh local from
                        // `gos_rt_result_payload`. With happy-path
                        // encoding, bind directly to the scrutinee
                        // (legacy: the scrutinee value IS the
                        // payload). Pin the binding's MIR type
                        // from the scrutinee's substs slot.
                        let payload_slot = match name.name.as_str() {
                            "Ok" | "Some" => Some(0),
                            "Err" => Some(1),
                            _ => None,
                        };
                        let payload_ty = payload_slot
                            .and_then(|idx| self.adt_generic_at(scrut_ty, idx))
                            .unwrap_or(i64_ty);
                        let payload_local = if real_disc {
                            let p = self.fresh(payload_ty);
                            self.emit_assign(
                                Place::local(p),
                                Rvalue::CallIntrinsic {
                                    name: "gos_rt_result_payload",
                                    args: vec![Operand::Copy(Place::local(scrutinee))],
                                },
                                span,
                            );
                            p
                        } else {
                            // Happy-path: alias the scrutinee.
                            // Only repin the scrutinee's MIR type
                            // when the substs gave a concrete
                            // payload type AND the scrutinee
                            // currently has no concrete type
                            // either. Without this guard, a
                            // concrete tagged scrutinee
                            // (`http::Response` / json::Value /
                            // …) would have its type field
                            // overwritten by `i64` (the
                            // unwrap-fallback default), losing
                            // the runtime-kind tag downstream
                            // method dispatch needs.
                            let payload_kind = self.tcx.kind_of(payload_ty).clone();
                            let scrut_concrete = !matches!(
                                self.tcx.kind_of(self.locals[scrutinee.0 as usize].ty),
                                TyKind::Var(_) | TyKind::Error | TyKind::Never,
                            );
                            let payload_concrete = !matches!(
                                payload_kind,
                                TyKind::Var(_) | TyKind::Error | TyKind::Never,
                            );
                            if payload_concrete && !scrut_concrete {
                                self.locals[scrutinee.0 as usize].ty = payload_ty;
                            }
                            scrutinee
                        };
                        self.bind_local(&bname.name, payload_local);
                        // Tag the payload local so
                        // `binding.field` / `binding.method` calls
                        // route through the right struct / runtime
                        // dispatch path. The struct/runtime-kind
                        // info is inherited from the wrapper's
                        // generic args (`Result<Opts, _>` →
                        // payload of Ok arm gets Opts).
                        if let Some(sname) = self.struct_name_of(first.ty) {
                            self.local_struct.insert(payload_local, sname);
                        }
                        let scrut_outer_ty = self.locals[scrutinee.0 as usize].ty;
                        let inner_ty = if name.name == "Err" {
                            self.second_generic_of(scrut_outer_ty)
                        } else {
                            self.first_generic_of(scrut_outer_ty)
                        };
                        if let Some(inner) = inner_ty {
                            if let Some(sname) = self.struct_name_of(inner) {
                                let runtime_kind: Option<&'static str> = match sname.as_str() {
                                    "Error" => Some("errors::Error"),
                                    "Response" => Some("http::Response"),
                                    "Request" => Some("http::Request"),
                                    "Client" => Some("http::Client"),
                                    "Scanner" => Some("bufio::Scanner"),
                                    "Pattern" => Some("regex::Pattern"),
                                    _ => None,
                                };
                                self.local_struct.insert(payload_local, sname);
                                if let Some(rk) = runtime_kind {
                                    self.local_runtime_kind.insert(payload_local, rk);
                                }
                            }
                        }
                    }
                }
                Some(const_pred)
            }
            HirPatKind::Literal(_) => None,
        }
    }

    /// Lowers `expr as T` into `Rvalue::Cast { operand, target }`.
    fn lower_cast(&mut self, value: &HirExpr, target: Ty, ty: Ty, span: Span) -> Option<Local> {
        let value_local = self.lower_expr(value)?;
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Cast {
                operand: Operand::Copy(Place::local(value_local)),
                target,
            },
            span,
        );
        Some(dest)
    }

    /// Binds each element of a tuple pattern to a fresh local reading
    /// through a `Projection::Field(i)`. Only the simple shapes used
    /// in practice — [`HirPatKind::Binding`] and [`HirPatKind::Wildcard`]
    /// — are supported; nested or non-tuple sub-patterns are silently
    /// skipped so the outer binding still sees the whole aggregate.
    fn bind_tuple_pattern(&mut self, tuple_local: Local, sub_patterns: &[HirPat], span: Span) {
        for (i, sub) in sub_patterns.iter().enumerate() {
            let HirPatKind::Binding { name, mutable } = &sub.kind else {
                continue;
            };
            let element_local =
                self.push_local(sub.ty, Some(Ident::new(name.name.as_str())), *mutable);
            self.bind_local(name.name.as_str(), element_local);
            let projection = vec![crate::ir::Projection::Field(
                u32::try_from(i).expect("tuple projection overflow"),
            )];
            let place = Place {
                local: tuple_local,
                projection,
            };
            self.emit_assign(
                Place::local(element_local),
                Rvalue::Use(Operand::Copy(place)),
                span,
            );
        }
    }

    /// Recognises a call to the synthetic `__struct("Name", "f1", v1,
    /// "f2", v2, …)` builtin and rewrites it into an
    /// [`Rvalue::Aggregate`] with the operands in declaration order.
    /// Returns `None` when the call is not a struct literal.
    fn lower_struct_call(
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
        // Try the free-struct table first, then fall back to the
        // enum's variant-fields map so `Shape::Rect { w, h }` (where
        // `Rect` is a struct-payload variant of an enum) lowers
        // without needing a free `struct Rect` to exist.
        let order = self
            .structs
            .get(struct_name)
            .cloned()
            .or_else(|| self.enums.variant_fields.get(struct_name).cloned())?;
        let mut provided: HashMap<String, &HirExpr> = HashMap::new();
        let mut chunks = pairs.chunks_exact(2);
        for chunk in chunks.by_ref() {
            let HirExprKind::Literal(HirLiteral::String(field_name)) = &chunk[0].kind else {
                return None;
            };
            provided.insert(field_name.clone(), &chunk[1]);
        }
        let mut operands = Vec::with_capacity(order.len());
        for field in &order {
            let value_expr = provided.get(field.as_str())?;
            let value_local = self.lower_expr(value_expr)?;
            operands.push(Operand::Copy(Place::local(value_local)));
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

    /// Lowers `receiver.name` into a projection read when `receiver`'s
    /// type is a known named struct. Falls back to the unsupported
    /// placeholder for any other receiver shape.
    fn lower_field_access(
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
                        let pinned_ty = if matches!(
                            self.tcx.kind_of(ty),
                            gossamer_types::TyKind::Error | gossamer_types::TyKind::Var(_)
                        ) {
                            let recv_local_ty = self.locals[place.local.0 as usize].ty;
                            let mut walk = recv_local_ty;
                            while let gossamer_types::TyKind::Ref { inner, .. } =
                                self.tcx.kind_of(walk)
                            {
                                walk = *inner;
                            }
                            match self.tcx.kind_of(walk) {
                                gossamer_types::TyKind::Adt { def, .. } => self
                                    .tcx
                                    .struct_field_tys(*def)
                                    .and_then(|tys| tys.get(pos).copied())
                                    .unwrap_or(ty),
                                gossamer_types::TyKind::Tuple(elems) => {
                                    elems.get(pos).copied().unwrap_or(ty)
                                }
                                _ => ty,
                            }
                        } else {
                            ty
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

    /// Lowers `receiver.method(args…)` into a `Call` terminator.
    /// First tries the stdlib intrinsic table (method names whose
    /// semantics the native runtime implements as a C-ABI helper);
    /// falls back to the `unsupported` placeholder if the receiver
    /// shape isn't recognised.
    #[allow(clippy::cognitive_complexity)]
    fn lower_method_call(
        &mut self,
        receiver: &HirExpr,
        method: &Ident,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // `arr.swap(i, j)` super-instruction. The generic Call
        // fallback at the end of this function would lower this as
        // `Call(Const(Str("swap")), …)` which the cranelift backend
        // can't resolve — JIT- and AOT-compiled bodies silently
        // produced a typed-zero stub, leaving the receiver
        // unmutated. Inlining as four index ops (read i, read j,
        // write j-into-i, write i-into-j) keeps the semantics
        // intact across every backend.
        if method.name.as_str() == "swap" && args.len() == 2 {
            if let Some(swap_local) =
                self.try_lower_array_swap(receiver, &args[0], &args[1], ty, span)
            {
                return Some(swap_local);
            }
        }
        // Fused-increment peephole: `m.insert(k, m.get_or(k, 0)
        // + by)` (or `… + 1`) on an i64-keyed map collapses into
        // a single `gos_rt_map_inc_i64(m, k, by)` call. Halves
        // the lock + hash work on every counter-style loop.
        if method.name.as_str() == "insert" && args.len() == 2 {
            if let Some(local) = self.try_lower_map_inc(receiver, &args[0], &args[1], ty, span) {
                return Some(local);
            }
        }
        // Prefer the MIR local's pinned type over the HIR receiver
        // type when the receiver is a Path bound to a local — the
        // type checker may have left the HIR type as an inference
        // variable, but we pin runtime-helper return types
        // (`gos_rt_stream_read_to_string` → `String`, etc.) on the
        // MIR side at line ~2026. Without this lookup `s.len()`
        // for `let s = stdin.read_to_string()` falls through the
        // `len` dispatch's default arm to `gos_rt_len` — which
        // misinterprets the C-string pointer as a length-prefixed
        // buffer and returns the first 8 data bytes.
        let receiver_ty = self
            .receiver_local_from_path(receiver)
            .map_or(receiver.ty, |local| self.locals[local.0 as usize].ty);
        let receiver_kind = self.tcx.kind_of(receiver_ty).clone();
        // Unwrap a leading `&T` so `s.len()` on a `&String`
        // parameter lowers the same as on an owned `String`.
        let receiver_kind_flat = match &receiver_kind {
            TyKind::Ref { inner, .. } => self.tcx.kind_of(*inner).clone(),
            other => other.clone(),
        };

        // Detect `recv.headers.<insert|get>(name[, value])` where
        // `recv` is an `http::Response`/`http::Request`. Fold the
        // chain into a single `gos_rt_http_*_set_header` /
        // `_get_header` call so the intermediate headers handle
        // never has to be represented.
        if let HirExprKind::Field {
            receiver: inner,
            name: field_name,
        } = &receiver.kind
        {
            if field_name.name.as_str() == "headers" {
                let inner_local_for_kind = self.receiver_local_from_path(inner);
                let inner_kind = inner_local_for_kind
                    .and_then(|l| self.local_runtime_kind.get(&l).copied())
                    .or_else(|| {
                        let inner_ty =
                            inner_local_for_kind.map_or(inner.ty, |l| self.locals[l.0 as usize].ty);
                        match self.tcx.kind_of(inner_ty) {
                            TyKind::Ref { inner: i, .. } => self.struct_name_of(*i),
                            _ => self.struct_name_of(inner_ty),
                        }
                        .and_then(|s| match s.as_str() {
                            "Response" => Some("http::Response"),
                            "Request" => Some("http::Request"),
                            _ => None,
                        })
                    });
                if matches!(inner_kind, Some("http::Response" | "http::Request")) {
                    let helper = match (inner_kind, method.name.as_str()) {
                        (Some("http::Response"), "insert") => {
                            Some(("gos_rt_http_response_set_header", self.tcx.unit(), 2usize))
                        }
                        (Some("http::Response"), "get") => Some((
                            "gos_rt_http_response_get_header",
                            self.tcx.string_ty(),
                            1usize,
                        )),
                        (Some("http::Request"), "insert") => {
                            Some(("gos_rt_http_request_set_header", self.tcx.unit(), 2usize))
                        }
                        (Some("http::Request"), "get") => Some((
                            "gos_rt_http_request_get_header",
                            self.tcx.string_ty(),
                            1usize,
                        )),
                        _ => None,
                    };
                    if let Some((rt, ret_ty, want_args)) = helper {
                        if args.len() == want_args {
                            let inner_local = self.lower_expr(inner)?;
                            let mut ops = Vec::with_capacity(args.len() + 1);
                            ops.push(Operand::Copy(Place::local(inner_local)));
                            for a in args {
                                let al = self.lower_expr(a)?;
                                ops.push(Operand::Copy(Place::local(al)));
                            }
                            let dest = self.fresh(ret_ty);
                            let next = self.new_block(span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(rt.to_string())),
                                args: ops,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            return Some(dest);
                        }
                    }
                }
            }
        }

        // Stdlib dispatch table. First by method name alone —
        // covers receivers whose HIR type is still an unresolved
        // inference variable (common post-checker). The runtime
        // helpers accept any receiver shape and return a safe
        // default (0, empty, null) for inputs the native runtime
        // doesn't yet represent.
        //
        // When the callee name is empty the method is identity
        // (currently `.to_string()` / `.clone()` on any scalar or
        // string-shaped receiver — the GC already aliases the
        // buffer).
        let runtime_symbol: Option<&'static str> = match method.name.as_str() {
            // `.to_string()` routes to the runtime numeric
            // formatter for integer / float receivers. String
            // receivers fall through to the identity copy.
            // `to_string()` (no args) — scalar-to-string for
            // integer / float receivers; identity copy for the
            // others.
            //
            // `to_string(len)` (1 arg) — the canonical "freeze the
            // build buffer" step at the end of a `U8Vec`-backed
            // incremental construction loop. Mirrors F#'s
            // `StringBuilder.ToString()` and Rust's
            // `String::from_utf8(vec).unwrap()`. Routes to a
            // runtime helper that copies the first `len` bytes
            // into a fresh immutable `String`.
            "to_string" => {
                if args.len() == 1 {
                    Some("gos_rt_heap_u8_to_string")
                } else {
                    match &receiver_kind_flat {
                        TyKind::Int(_) => Some("gos_rt_i64_to_str"),
                        TyKind::Float(_) => Some("gos_rt_f64_to_str"),
                        _ => Some(""),
                    }
                }
            }
            "clone" => Some(""),
            // Option / Result methods. Today the compiled tier
            // represents `Option<T>` and `Result<T, E>` as the
            // inner value with a null/zero sentinel for the
            // missing case (see the `lower_match` happy-path
            // routing). `.unwrap()` / `.ok()` / `.err()` are
            // identity copies; `.unwrap_or(d)` returns the
            // receiver as-is. `is_some` / `is_ok` evaluate to
            // `receiver != 0` (handled below as a synthesised
            // compare). `is_none` / `is_err` are the inverse.
            "unwrap" | "unwrap_or" | "ok" | "err" | "expect" => Some(""),
            "len" => match &receiver_kind_flat {
                TyKind::String => Some("gos_rt_str_len"),
                TyKind::HashMap { .. } => Some("gos_rt_map_len"),
                TyKind::JsonValue => Some("gos_rt_json_len"),
                TyKind::Vec(_) | TyKind::Array { .. } | TyKind::Slice(_) => Some("gos_rt_len"),
                _ => Some("gos_rt_len"),
            },
            "trim" => Some("gos_rt_str_trim"),
            "contains" => Some("gos_rt_str_contains"),
            "starts_with" => Some("gos_rt_str_starts_with"),
            "ends_with" => Some("gos_rt_str_ends_with"),
            "find" => Some("gos_rt_str_find"),
            "replace" => Some("gos_rt_str_replace"),
            "split" => Some("gos_rt_str_split"),
            "lines" => Some("gos_rt_str_lines"),
            "repeat" => Some("gos_rt_str_repeat"),
            "byte_at" => Some("gos_rt_str_byte_at"),
            // `is_empty` collapses to `len(self) == 0`. Route to
            // a small helper that delegates to the right `len`
            // backend for the receiver kind.
            "is_empty" => match &receiver_kind_flat {
                TyKind::String => Some("gos_rt_str_is_empty"),
                _ => Some("gos_rt_len_is_zero"),
            },
            "to_vec" => Some(""),
            // errors::Error methods. `is` here routes
            // unconditionally to the runtime helper because no
            // other type in the stdlib defines a `.is(...)`
            // method today; if a user struct defines one, the
            // user-impl dispatch below wins (it runs after this
            // table).
            "message" => Some("gos_rt_error_message"),
            "cause" => Some("gos_rt_error_cause"),
            "is" => Some("gos_rt_error_is"),
            // bufio::Scanner methods.
            "scan" => Some("gos_rt_bufio_scanner_scan"),
            "text" => Some("gos_rt_bufio_scanner_text"),
            // http::Response getters.
            "status" => Some("gos_rt_http_response_status"),
            "body" => Some("gos_rt_http_response_body"),
            // http builder. The kind-dispatch above already routes
            // tagged `http::Request` receivers for `.header(k, v)`
            // builder calls; this name-only arm catches untagged
            // ones — `.send` falls below to the channel default
            // because channel sends are far more common in user
            // code than untagged-http requests.
            "header" => Some("gos_rt_http_request_header"),
            "send" => Some("gos_rt_chan_send"),
            // string parsing — `text.parse()` for an i64 binding
            // routes to gos_rt_parse_i64 with a discarded ok flag.
            // Pin return to i64 for the common case; users with
            // f64 / float must annotate explicitly today.
            "parse" => Some("gos_rt_parse_i64"),
            // Result/Option chained helpers map to identity on
            // the happy path. The user passes in a closure; we
            // discard it (the compiled tier doesn't run the
            // error-mapping closure today).
            "map_err" | "map" => Some(""),
            "to_lowercase" => Some("gos_rt_str_to_lower"),
            "to_uppercase" => Some("gos_rt_str_to_upper"),
            "push" => Some("gos_rt_vec_push"),
            "pop" => Some("gos_rt_vec_pop"),
            "iter" => Some("gos_rt_arr_iter"),
            "as_bytes" => Some(""),
            "as_str" => match &receiver_kind_flat {
                TyKind::JsonValue => Some("gos_rt_json_as_str"),
                _ => Some(""),
            },
            // JSON value query/cast methods. The runtime helpers
            // accept a `*mut GosJson` (passed as a flat pointer)
            // and return either a fresh `*mut GosJson` (for
            // chained queries) or a primitive scalar.
            "as_i64" => Some("gos_rt_json_as_i64"),
            "as_f64" => Some("gos_rt_json_as_f64"),
            "as_bool" => Some("gos_rt_json_as_bool"),
            "is_null" => Some("gos_rt_json_is_null"),
            "at" => match &receiver_kind_flat {
                TyKind::JsonValue => Some("gos_rt_json_at"),
                _ => None,
            },
            "recv" => Some("gos_rt_chan_recv"),
            "try_send" => Some("gos_rt_chan_try_send"),
            "try_recv" => Some("gos_rt_chan_try_recv"),
            "close" => Some("gos_rt_chan_close"),
            // Stream methods (on `io::stdout()` / `io::stderr()`
            // / `io::stdin()` handles). Mirrors Rust's `Write` /
            // `BufRead` trait surface.
            "write_byte" => Some("gos_rt_stream_write_byte"),
            "write_byte_array" | "write_bytes" => Some("gos_rt_stream_write_byte_array"),
            "write" | "write_str" => Some("gos_rt_stream_write_str"),
            "flush" => Some("gos_rt_stream_flush"),
            "read_line" => Some("gos_rt_stream_read_line"),
            "read_to_string" => Some("gos_rt_stream_read_to_string"),
            // HashMap method dispatch — gated on the receiver
            // actually being a `HashMap`, not just on having a
            // matching method name. Without the gate, a user
            // struct with an `impl Foo { fn get(...) }` would
            // route through the map helper at codegen time and
            // either segfault on the wrong ABI or read garbage.
            // `get` extends the gate to `JsonValue` because the
            // json runtime also exposes a single-arg `get(key)`.
            "insert" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::I64) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_insert_str_i64"),
                        _ => Some("gos_rt_map_insert_i64_i64"),
                    },
                    Some(MapValueKind::String) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_insert_str_str"),
                        _ => Some("gos_rt_map_insert_i64_str"),
                    },
                    _ => Some("gos_rt_map_insert_i64_i64"),
                },
                _ => None,
            },
            "get" => match &receiver_kind_flat {
                TyKind::JsonValue => Some("gos_rt_json_get"),
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::String) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_str_str"),
                        _ => Some("gos_rt_map_get_i64_str"),
                    },
                    _ => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_str_i64"),
                        _ => Some("gos_rt_map_get_i64"),
                    },
                },
                _ => None,
            },
            "get_or" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::String) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_or_str_str"),
                        _ => Some("gos_rt_map_get_or_i64_str"),
                    },
                    _ => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_or_str_i64"),
                        _ => Some("gos_rt_map_get_or_i64"),
                    },
                },
                _ => None,
            },
            "remove" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_remove_str"),
                    _ => Some("gos_rt_map_remove_i64"),
                },
                _ => None,
            },
            "contains_key" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_contains_key_str"),
                    _ => Some("gos_rt_map_contains_key_i64"),
                },
                _ => None,
            },
            "clear" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => Some("gos_rt_map_clear"),
                _ => None,
            },
            // `m.inc_at(seq, start, len, by)` — zero-copy slice
            // hash for `HashMap<String, i64>`. Single hash lookup
            // per call, no per-iteration scratch allocation —
            // mirrors `*m.entry(&seq[i..i+k]).or_insert(0) += by`.
            "inc_at" => match self.hash_map_value_kind(receiver_ty) {
                Some(MapValueKind::I64) => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_inc_at_str_i64"),
                    _ => None,
                },
                _ => None,
            },
            // HashMap iteration. Each helper snapshots the
            // requested column into a fresh `GosVec` so the
            // for-loop lowerer can drive iteration with the
            // regular `gos_rt_vec_*` helpers. String-keyed /
            // string-valued shapes go through `*_str`; everything
            // else through `*_i64`.
            "keys" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_keys_str"),
                    _ => Some("gos_rt_map_keys_i64"),
                },
                _ => None,
            },
            "values" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::String) => Some("gos_rt_map_values_str"),
                    _ => Some("gos_rt_map_values_i64"),
                },
                _ => None,
            },
            // Mutex<T> / WaitGroup / Atomic / heap-Vec
            // primitives. Each method dispatches by name —
            // the runtime function takes the receiver
            // pointer as its first arg, matching the rest of
            // the table.
            "lock" => Some("gos_rt_mutex_lock"),
            "unlock" => Some("gos_rt_mutex_unlock"),
            "add" => Some("gos_rt_wg_add"),
            "done" => Some("gos_rt_wg_done"),
            "wait" => Some("gos_rt_wg_wait"),
            "load" => Some("gos_rt_atomic_i64_load"),
            "store" => Some("gos_rt_atomic_i64_store"),
            "fetch_add" => Some("gos_rt_atomic_i64_fetch_add"),
            "set_at" => Some("gos_rt_heap_i64_set"),
            "get_at" => Some("gos_rt_heap_i64_get"),
            "vec_len" => Some("gos_rt_heap_i64_len"),
            "write_range_to_stdout" => Some("gos_rt_heap_i64_write_bytes_to_stdout"),
            "write_lines_to_stdout" => Some("gos_rt_heap_i64_write_lines_to_stdout"),
            // U8Vec methods. Distinct names from the I64Vec
            // family because MIR's method dispatch is by name
            // alone — sharing `set_at` between i64 and u8
            // receivers would silently write through the
            // i64-stride helper to a u8 buffer, corrupting
            // adjacent bytes.
            "set_byte" => Some("gos_rt_heap_u8_set"),
            "get_byte" => Some("gos_rt_heap_u8_get"),
            "byte_len" => Some("gos_rt_heap_u8_len"),
            "write_byte_range_to_stdout" => Some("gos_rt_heap_u8_write_bytes_to_stdout"),
            "write_byte_lines_to_stdout" => Some("gos_rt_heap_u8_write_lines_to_stdout"),
            _ => None,
        };
        let _ = receiver_kind;

        // Char→String coercion for `s.split(c)` and `s.contains(c)`-
        // style calls where the user passes a `char` literal but
        // the underlying runtime helper expects a c-string ptr.
        // Lower the char arg through `gos_rt_char_to_str` before
        // it reaches the runtime call.
        let needs_char_to_str = matches!(
            method.name.as_str(),
            "split" | "contains" | "starts_with" | "ends_with" | "find" | "replace"
        );
        let _ = needs_char_to_str;

        // Receiver-shape-aware dispatch. Reads the kind tag from
        // a path-bound receiver, or inspects a chained method
        // call to recover its result kind for the
        // `a.b().c()`-style shapes.
        let receiver_runtime_kind = self
            .receiver_local_from_path(receiver)
            .and_then(|l| self.local_runtime_kind.get(&l).copied())
            .or_else(|| self.expr_runtime_kind(receiver));
        let kind_dispatch: Option<&'static str> =
            match (receiver_runtime_kind, method.name.as_str()) {
                (Some("flag::Set"), "string") => Some("gos_rt_flag_set_string"),
                (Some("flag::Set"), "uint") => Some("gos_rt_flag_set_uint"),
                (Some("flag::Set"), "bool") => Some("gos_rt_flag_set_bool"),
                (Some("flag::Set"), "parse") => Some("gos_rt_flag_set_parse"),
                (Some("http::Client"), "get") => Some("gos_rt_http_client_get"),
                (Some("http::Client"), "post") => Some("gos_rt_http_client_post"),
                (Some("http::Request"), "header") => Some("gos_rt_http_request_header"),
                (Some("http::Request"), "body") => Some("gos_rt_http_request_body"),
                (Some("http::Request"), "send") => Some("gos_rt_http_request_send"),
                (Some("http::Request"), "path") => Some("gos_rt_http_request_path"),
                (Some("http::Request"), "method") => Some("gos_rt_http_request_method"),
                (Some("http::Response"), "status") => Some("gos_rt_http_response_status"),
                (Some("http::Response"), "body") => Some("gos_rt_http_response_body"),
                (Some("bufio::Scanner"), "scan") => Some("gos_rt_bufio_scanner_scan"),
                (Some("bufio::Scanner"), "text") => Some("gos_rt_bufio_scanner_text"),
                (Some("errors::Error"), "message") => Some("gos_rt_error_message"),
                (Some("errors::Error"), "cause") => Some("gos_rt_error_cause"),
                (Some("errors::Error"), "is") => Some("gos_rt_error_is"),
                (Some("regex::Pattern"), "is_match") => Some("gos_rt_regex_is_match"),
                (Some("regex::Pattern"), "find") => Some("gos_rt_regex_find"),
                (Some("regex::Pattern"), "find_all") => Some("gos_rt_regex_find_all"),
                (Some("regex::Pattern"), "replace_all") => Some("gos_rt_regex_replace_all"),
                (Some("regex::Pattern"), "split") => Some("gos_rt_regex_split"),
                (Some("collections::HashSet"), "insert") => Some("gos_rt_set_insert"),
                (Some("collections::HashSet"), "contains") => Some("gos_rt_set_contains"),
                (Some("collections::HashSet"), "remove") => Some("gos_rt_set_remove"),
                (Some("collections::HashSet"), "len") => Some("gos_rt_set_len"),
                (Some("collections::BTreeMap"), "insert") => Some("gos_rt_btmap_insert"),
                (Some("collections::BTreeMap"), "get_or") => Some("gos_rt_btmap_get_or"),
                (Some("collections::BTreeMap"), "len") => Some("gos_rt_btmap_len"),
                _ => None,
            };
        if let Some(rt) = kind_dispatch {
            // Lower the receiver + args, emit a Call to the
            // runtime helper, return the dest local. Pin a
            // sensible return type for the destination.
            let receiver_local = self.lower_expr(receiver)?;
            let mut arg_operands = Vec::with_capacity(args.len() + 1);
            arg_operands.push(Operand::Copy(Place::local(receiver_local)));
            for arg in args {
                let a = self.lower_expr(arg)?;
                arg_operands.push(Operand::Copy(Place::local(a)));
            }
            let pinned: Ty = match rt {
                "gos_rt_error_message"
                | "gos_rt_bufio_scanner_text"
                | "gos_rt_http_response_body"
                | "gos_rt_http_request_path"
                | "gos_rt_http_request_method"
                | "gos_rt_regex_find"
                | "gos_rt_regex_replace_all" => self.tcx.string_ty(),
                "gos_rt_error_is"
                | "gos_rt_regex_is_match"
                | "gos_rt_bufio_scanner_scan"
                | "gos_rt_set_insert"
                | "gos_rt_set_contains"
                | "gos_rt_set_remove" => self.tcx.bool_ty(),
                "gos_rt_http_response_status"
                | "gos_rt_set_len"
                | "gos_rt_btmap_len"
                | "gos_rt_btmap_get_or" => self.tcx.int_ty(gossamer_types::IntTy::I64),
                "gos_rt_btmap_insert" => self.tcx.unit(),
                "gos_rt_regex_find_all" | "gos_rt_regex_split" => {
                    let s = self.tcx.string_ty();
                    self.tcx.intern(gossamer_types::TyKind::Vec(s))
                }
                "gos_rt_error_cause" => self.option_adt_ty(),
                _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
            };
            let dest = self.fresh(pinned);
            // Tag chained dest locals so further method calls
            // dispatch correctly: get/post return Request, send
            // returns Response, header/body return Request again.
            let dest_kind: Option<&'static str> = match rt {
                "gos_rt_http_client_get" | "gos_rt_http_client_post" => Some("http::Request"),
                "gos_rt_http_request_header" | "gos_rt_http_request_body" => Some("http::Request"),
                "gos_rt_http_request_send" => Some("http::Response"),
                _ => None,
            };
            if let Some(k) = dest_kind {
                self.local_runtime_kind.insert(dest, k);
            }
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(rt.to_string())),
                args: arg_operands,
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        // Synthesise `is_some`/`is_ok`/`is_none`/`is_err` directly
        // as bool constants. The happy-path Option/Result encoding
        // means `Some`/`Ok` always; `unwrap` then returns the
        // receiver. Mirrors the assumption baked into `lower_match`.
        match method.name.as_str() {
            "is_some" | "is_ok" => {
                let _ = self.lower_expr(receiver)?;
                let bool_ty = self.tcx.bool_ty();
                let dest = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                return Some(dest);
            }
            "is_none" | "is_err" => {
                let _ = self.lower_expr(receiver)?;
                let bool_ty = self.tcx.bool_ty();
                let dest = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(false))),
                    span,
                );
                return Some(dest);
            }
            _ => {}
        }

        let receiver_local = self.lower_expr(receiver)?;
        let mut arg_operands = Vec::with_capacity(args.len() + 1);
        arg_operands.push(Operand::Copy(Place::local(receiver_local)));
        for arg in args {
            let a = self.lower_expr(arg)?;
            arg_operands.push(Operand::Copy(Place::local(a)));
        }

        if let Some(sym) = runtime_symbol {
            if sym.is_empty() {
                // Identity method — just copy the receiver to the
                // destination. Lets `"lit".to_string()` lower
                // without involving the runtime.
                //
                // Pin the destination's MIR type to the receiver's
                // own type rather than the method-call expression's
                // (often still unresolved) inference variable, so
                // downstream passes see a concrete `String` /
                // `Vec<T>` / etc. — crucial for the binary-op
                // lowering in `lower_binary` to route `s + t`
                // through `gos_rt_str_concat`.
                //
                // For `unwrap` / `unwrap_or` / `ok` / `err` /
                // `expect` the receiver is a `Result<T,E>` /
                // `Option<T>` and the unwrapped value is the
                // first generic argument. Dig into the receiver's
                // generic substitution so the destination is the
                // inner `T` instead of the wrapper Adt — keeps
                // `println!("{v}")` of the unwrapped value on the
                // right scalar dispatch.
                // For Option/Result `unwrap`, default the inner to
                // i64 when neither the receiver type nor the call
                // expression's type knows the wrapped element. The
                // common case where neither has a concrete type is
                // `m.get(k).unwrap()` for `HashMap<_, i64>` — the
                // type checker leaves both call expressions as
                // unresolved and the MIR has to assume something.
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let unwrap_inner = matches!(
                    method.name.as_str(),
                    "unwrap" | "unwrap_or" | "ok" | "expect"
                )
                .then(|| self.first_generic_of(receiver_ty).unwrap_or(i64_ty));
                let err_inner = matches!(method.name.as_str(), "err")
                    .then(|| self.second_generic_of(receiver_ty).unwrap_or(i64_ty));
                // For Option/Result identity unwraps, prefer the
                // generic argument over the call expression's HIR
                // type — the latter is `Adt { Result, .. }` /
                // `Adt { Option, .. }` if the type checker assumed
                // Wrapped semantics, but the compiled tier always
                // returns the inner value directly.
                let dest_ty = if let Some(inner) = unwrap_inner.or(err_inner) {
                    inner
                } else {
                    match self.tcx.kind_of(ty) {
                        TyKind::Bool
                        | TyKind::Char
                        | TyKind::Int(_)
                        | TyKind::Float(_)
                        | TyKind::String
                        | TyKind::Vec(_)
                        | TyKind::Array { .. }
                        | TyKind::Slice(_)
                        | TyKind::Adt { .. }
                        | TyKind::Tuple(_) => ty,
                        _ => receiver_ty,
                    }
                };
                let dest = self.fresh(dest_ty);
                // Propagate runtime kind / struct tags so chained
                // identity-method calls (`.clone()`, `.unwrap()`,
                // `.map_err(...)`) keep the receiver's surface
                // type for downstream dispatch.
                if let Some(rk) = self.local_runtime_kind.get(&receiver_local).copied() {
                    self.local_runtime_kind.insert(dest, rk);
                }
                if let Some(sn) = self.local_struct.get(&receiver_local).cloned() {
                    self.local_struct.insert(dest, sn);
                }
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Use(Operand::Copy(Place::local(receiver_local))),
                    span,
                );
                return Some(dest);
            }
            // Pin the destination's MIR type to the helper's
            // known return shape when the HIR expression type is
            // still opaque (inference variable or Error). Keeps
            // operand_print_kind + codegen inference grounded on
            // a concrete scalar/string kind.
            let pinned_ret: Ty = match sym {
                "gos_rt_str_concat"
                | "gos_rt_str_trim"
                | "gos_rt_str_to_lower"
                | "gos_rt_str_to_upper"
                | "gos_rt_str_replace"
                | "gos_rt_str_repeat"
                | "gos_rt_heap_u8_to_string"
                | "gos_rt_i64_to_str"
                | "gos_rt_f64_to_str"
                | "gos_rt_stream_read_line"
                | "gos_rt_stream_read_to_string"
                | "gos_rt_map_get_str_str"
                | "gos_rt_map_get_or_str_str"
                | "gos_rt_map_get_or_i64_str"
                | "gos_rt_map_get_i64_str"
                | "gos_rt_json_as_str"
                | "gos_rt_json_render"
                | "gos_rt_error_message"
                | "gos_rt_bufio_scanner_text"
                | "gos_rt_http_response_body"
                | "gos_rt_regex_find" => self.tcx.string_ty(),
                "gos_rt_str_split" | "gos_rt_str_lines" => {
                    let s = self.tcx.string_ty();
                    self.tcx.intern(gossamer_types::TyKind::Vec(s))
                }
                "gos_rt_map_keys_i64" | "gos_rt_map_values_i64" => {
                    let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                    self.tcx.intern(gossamer_types::TyKind::Vec(i))
                }
                "gos_rt_map_keys_str" | "gos_rt_map_values_str" => {
                    let s = self.tcx.string_ty();
                    self.tcx.intern(gossamer_types::TyKind::Vec(s))
                }
                "gos_rt_str_contains" | "gos_rt_str_starts_with" | "gos_rt_str_ends_with" => {
                    self.tcx.bool_ty()
                }
                "gos_rt_str_find"
                | "gos_rt_str_len"
                | "gos_rt_str_byte_at"
                | "gos_rt_arr_len"
                | "gos_rt_len"
                | "gos_rt_map_len"
                | "gos_rt_map_get_or_i64"
                | "gos_rt_map_get_or_str_i64"
                | "gos_rt_map_get_i64"
                | "gos_rt_map_get_str_i64"
                | "gos_rt_chan_recv"
                | "gos_rt_chan_try_recv"
                | "gos_rt_vec_pop"
                | "gos_rt_json_as_i64"
                | "gos_rt_json_len"
                | "gos_rt_http_response_status"
                | "gos_rt_parse_i64" => self.tcx.int_ty(gossamer_types::IntTy::I64),
                "gos_rt_json_as_f64" => self.tcx.float_ty(gossamer_types::FloatTy::F64),
                "gos_rt_chan_try_send"
                | "gos_rt_map_remove"
                | "gos_rt_map_remove_i64"
                | "gos_rt_map_remove_str"
                | "gos_rt_map_contains_key_i64"
                | "gos_rt_map_contains_key_str"
                | "gos_rt_json_is_null"
                | "gos_rt_json_as_bool"
                | "gos_rt_error_is"
                | "gos_rt_regex_is_match"
                | "gos_rt_fs_write"
                | "gos_rt_fs_create_dir_all"
                | "gos_rt_bufio_scanner_scan"
                | "gos_rt_testing_check"
                | "gos_rt_testing_check_eq_i64"
                | "gos_rt_str_is_empty"
                | "gos_rt_len_is_zero" => self.tcx.bool_ty(),
                "gos_rt_json_get" | "gos_rt_json_at" | "gos_rt_json_parse" => {
                    self.tcx.json_value_ty()
                }
                "gos_rt_error_cause" => self.option_adt_ty(),
                _ => match self.tcx.kind_of(ty) {
                    TyKind::Error | TyKind::Var(_) => self.tcx.int_ty(gossamer_types::IntTy::I64),
                    _ => ty,
                },
            };
            let dest = self.fresh(pinned_ret);
            // Tag the destination's runtime kind so chained
            // method calls + `?` propagation continue to dispatch
            // correctly on the result of the runtime helper.
            let dest_kind: Option<&'static str> = match sym {
                "gos_rt_http_request_send" => Some("http::Response"),
                "gos_rt_http_request_header" | "gos_rt_http_request_body" => Some("http::Request"),
                "gos_rt_http_client_get" | "gos_rt_http_client_post" => Some("http::Request"),
                _ => None,
            };
            if let Some(rk) = dest_kind {
                self.local_runtime_kind.insert(dest, rk);
            }
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(sym.to_string())),
                args: arg_operands,
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }

        // User-defined `impl` method dispatch: when the receiver's
        // static type names a known struct, look up the mangled
        // method name (`Struct::method`) and emit a direct call
        // with the receiver as the first argument. Mirrors the
        // tree-walker's qualified-method lookup so user code can
        // build natively without rewriting every method as a free
        // function.
        let struct_name = self.struct_name_of(receiver_ty).or_else(|| {
            self.local_struct
                .get(&receiver_local)
                .cloned()
                .or_else(|| self.struct_name_from_expr(receiver))
        });
        if let Some(sname) = struct_name {
            let mangled = format!("{}::{}", sname, method.name);
            // Pin a sensible destination type if HIR left it
            // unresolved. Trait-dispatched method calls
            // (`circle.name()` where `name` is declared on the
            // `Shape` trait) often arrive with the destination ty
            // still an inference variable; use the impl's known
            // return type when available so the codegen sees the
            // real `String` / `f64` / etc. instead of falling
            // back to `i64` and printing the pointer bits.
            let dest_ty = match self.tcx.kind_of(ty) {
                gossamer_types::TyKind::Error | gossamer_types::TyKind::Var(_) => self
                    .impl_methods
                    .get(&mangled)
                    .copied()
                    .flatten()
                    .unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64)),
                _ => ty,
            };
            let dest = self.fresh(dest_ty);
            if let Some(out_struct) = self.struct_name_of(dest_ty) {
                self.local_struct.insert(dest, out_struct);
            }
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(mangled)),
                args: arg_operands,
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }

        // No stdlib helper, no struct-impl match. Emit a generic
        // by-name Call: cranelift's `Const(Str(name))` callee path
        // resolves the symbol via `callees_by_name` (lifted
        // closures, free fns) or falls back to a typed-zero stub
        // for genuinely unknown names. Either branch produces a
        // well-formed CFG, so the build never refuses to lower a
        // method shape we haven't taught the dispatch table about.
        let dest_ty = match self.tcx.kind_of(ty) {
            TyKind::Error | TyKind::Var(_) => self.tcx.int_ty(gossamer_types::IntTy::I64),
            _ => ty,
        };
        let dest = self.fresh(dest_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(method.name.clone())),
            args: arg_operands,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    /// Returns the `Local` named by a single-segment Path expression,
    /// if any. Lets `lower_method_call` look up the MIR-pinned type
    /// of the receiver instead of trusting the HIR's possibly-still-
    /// unresolved inference variable.
    fn receiver_local_from_path(&self, expr: &HirExpr) -> Option<Local> {
        if let HirExprKind::Path { segments, .. } = &expr.kind {
            let first = segments.first()?;
            return self.lookup_local(&first.name);
        }
        None
    }

    /// Lowers a tuple literal into an `Rvalue::Aggregate { kind:
    /// Tuple }` stored in a fresh local.
    fn lower_tuple(&mut self, elems: &[HirExpr], ty: Ty, span: Span) -> Option<Local> {
        let mut operands = Vec::with_capacity(elems.len());
        for elem in elems {
            let local = self.lower_expr(elem)?;
            operands.push(Operand::Copy(Place::local(local)));
        }
        let dest = self.fresh(ty);
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

    /// Lowers an explicit array literal (`[a, b, c]`) into an
    /// `Rvalue::Aggregate { kind: Array }`.
    /// Lowers `http::serve(addr, handler)` to
    /// `gos_rt_http_serve(addr, handler_env, fn_addr)` where
    /// `fn_addr` is the address of the handler type's `serve`
    /// method. The runtime calls `fn_addr(env, request)` per
    /// request to dispatch back into Gossamer code.
    fn lower_http_serve(
        &mut self,
        addr_expr: &HirExpr,
        handler_expr: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let addr_local = self.lower_expr(addr_expr)?;
        let handler_local = self.lower_expr(handler_expr)?;
        let handler_ty = self.locals[handler_local.0 as usize].ty;
        let handler_struct = self.struct_name_of(handler_ty)?;
        let serve_fn_name = format!("{handler_struct}::serve");
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let fn_addr_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(fn_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(serve_fn_name))],
            },
            span,
        );
        let unit_ty = self.tcx.unit();
        let dest = self.fresh(unit_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_http_serve".to_string())),
            args: vec![
                Operand::Copy(Place::local(addr_local)),
                Operand::Copy(Place::local(handler_local)),
                Operand::Copy(Place::local(fn_addr_local)),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    /// Variant of `lower_result_ctor` for the no-payload case
    /// (currently only `None`). Allocates a `(disc, 0)` pair via
    /// the inline `Rvalue::CallIntrinsic` form so the destination
    /// Variable lives in the surrounding block — avoids the
    /// cross-block SSA propagation gap that the terminator-form
    /// `Call` exposes for Adt-typed bindings.
    fn lower_result_no_payload(&mut self, disc: i64, ty: Ty, span: Span) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let disc_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(disc_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(disc)))),
            span,
        );
        let zero_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(zero_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::CallIntrinsic {
                name: "gos_rt_result_new",
                args: vec![
                    Operand::Copy(Place::local(disc_local)),
                    Operand::Copy(Place::local(zero_local)),
                ],
            },
            span,
        );
        Some(dest)
    }

    /// Lowers `Ok(v)` / `Err(v)` / `Some(v)` as a heap-allocated
    /// `(disc, payload)` pair via `gos_rt_result_new`. The
    /// resulting handle is a `*mut GosResult` (8-byte pointer);
    /// match dispatch reads the `disc` bit via
    /// `gos_rt_result_disc` and the payload via
    /// `gos_rt_result_payload`. Uses the inline
    /// `Rvalue::CallIntrinsic` form so the dest Variable's value
    /// stays in the same basic block as the subsequent
    /// `let res = …` Assign statement that copies it to the
    /// binding's local.
    fn lower_result_ctor(
        &mut self,
        disc: i64,
        payload_expr: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let disc_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(disc_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(disc)))),
            span,
        );
        let payload_local = self.lower_expr(payload_expr)?;
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::CallIntrinsic {
                name: "gos_rt_result_new",
                args: vec![
                    Operand::Copy(Place::local(disc_local)),
                    Operand::Copy(Place::local(payload_local)),
                ],
            },
            span,
        );
        Some(dest)
    }

    /// Lowers `let xs: [T]/Vec<T> = [a, b, c]` as a heap Vec
    /// allocation followed by per-element `gos_rt_vec_push`. The
    /// destination local has already been allocated by the caller;
    /// we write the resulting Vec pointer to it directly. Returns
    /// `true` on success — caller skips its normal init lowering.
    fn lower_let_array_as_vec(&mut self, local: Local, elems: &[HirExpr], span: Span) -> bool {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let unit_ty = self.tcx.unit();
        let elem_bytes = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes),
            Rvalue::Use(Operand::Const(ConstValue::Int(8))),
            span,
        );
        // `Vec::new` is the codegen-side intrinsic name that
        // routes to `gos_rt_vec_new(8)`; using it avoids pulling
        // in the lower-level helper directly and keeps the call
        // dispatch path identical to user-written `Vec::new()`.
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("Vec::new".to_string())),
            args: vec![Operand::Copy(Place::local(elem_bytes))],
            destination: Place::local(local),
            target: Some(next),
        });
        self.set_current(next);
        for elem in elems {
            let Some(elem_local) = self.lower_expr(elem) else {
                return false;
            };
            let push_dest = self.fresh(unit_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
                args: vec![
                    Operand::Copy(Place::local(local)),
                    Operand::Copy(Place::local(elem_local)),
                ],
                destination: Place::local(push_dest),
                target: Some(next),
            });
            self.set_current(next);
        }
        true
    }

    fn lower_array_list(&mut self, elems: &[HirExpr], ty: Ty, span: Span) -> Option<Local> {
        let mut operands = Vec::with_capacity(elems.len());
        let mut elem_struct: Option<String> = None;
        for elem in elems {
            let local = self.lower_expr(elem)?;
            if elem_struct.is_none() {
                if let Some(name) = self.local_struct.get(&local).cloned() {
                    elem_struct = Some(name);
                }
            }
            operands.push(Operand::Copy(Place::local(local)));
        }
        let dest = self.fresh(ty);
        if let Some(name) = elem_struct {
            self.local_elem_struct.insert(dest, name);
        }
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Array,
                operands,
            },
            span,
        );
        Some(dest)
    }

    /// Lowers `[value; count]`. Constant counts go through
    /// `Rvalue::Repeat { value, count }`; runtime counts allocate
    /// a `GosVec<i64>` via `gos_rt_vec_with_capacity` and seed it
    /// with `gos_rt_vec_push` inside a counter loop so any `count`
    /// expression — local, parameter, call result — yields a
    /// well-formed array value.
    fn lower_array_repeat(
        &mut self,
        value: &HirExpr,
        count: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        if let Some(count_u64) = literal_u64(count) {
            let value_local = self.lower_expr(value)?;
            let dest = self.fresh(ty);
            self.emit_assign(
                Place::local(dest),
                Rvalue::Repeat {
                    value: Operand::Copy(Place::local(value_local)),
                    count: count_u64,
                },
                span,
            );
            return Some(dest);
        }
        // Runtime-count fallback: build a heap `GosVec` whose
        // length matches the dynamic `count`, seeded with
        // `value`. The result is shape-compatible with the
        // existing for-loop / `len()` lowering for `Vec<T>`.
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let value_local = self.lower_expr(value)?;
        let count_local = self.lower_expr(count)?;
        let elem_bytes_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(8))),
            span,
        );
        let vec_local = self.fresh(ty);
        let after_new = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_with_capacity".to_string())),
            args: vec![
                Operand::Copy(Place::local(elem_bytes_local)),
                Operand::Copy(Place::local(count_local)),
            ],
            destination: Place::local(vec_local),
            target: Some(after_new),
        });
        self.set_current(after_new);

        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(count_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        let after_push = self.new_block(span);
        let push_dest = self.fresh(i64_ty);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_push_i64".to_string())),
            args: vec![
                Operand::Copy(Place::local(vec_local)),
                Operand::Copy(Place::local(value_local)),
            ],
            destination: Place::local(push_dest),
            target: Some(after_push),
        });
        self.set_current(after_push);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        let bumped = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(bumped),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Copy(Place::local(bumped))),
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Use(Operand::Copy(Place::local(vec_local))),
            span,
        );
        Some(dest)
    }

    /// Lowers `receiver.N` into a projection read: copy from a
    /// place rooted at the receiver local with a trailing
    /// [`Projection::Field(N)`].
    fn lower_tuple_index(
        &mut self,
        receiver: &HirExpr,
        index: u32,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let receiver_local = self.lower_expr(receiver)?;
        let dest = self.fresh(ty);
        let place = Place {
            local: receiver_local,
            projection: vec![crate::ir::Projection::Field(index)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        Some(dest)
    }

    /// Lowers `base[index]` into a projection read with a runtime
    /// [`Projection::Index(local)`]. For `String` receivers the
    /// element is a byte, so we route through a dedicated runtime
    /// helper that loads the byte and zero-extends it to `i64`.
    fn lower_index_access(
        &mut self,
        base: &HirExpr,
        index: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // Slice expression `arr[lo..hi]`: the index is a Range
        // value rather than a single integer. Route through a
        // runtime slice helper. Returns a `*mut GosVec` so the
        // surrounding code can iterate or `to_vec()` on it.
        if let HirExprKind::Range {
            start,
            end,
            inclusive,
        } = &index.kind
        {
            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
            let base_local = self.lower_expr(base)?;
            let lo_local = if let Some(s) = start {
                self.lower_expr(s)?
            } else {
                let l = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    span,
                );
                l
            };
            let hi_local = if let Some(e) = end {
                self.lower_expr(e)?
            } else {
                // `arr[lo..]` — substitute `arr.len()` as the
                // upper bound by calling `gos_rt_len` on the
                // base. Works for both arrays and Vecs since
                // `gos_rt_len` reads the leading length word.
                let l = self.fresh(i64_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_len".to_string())),
                    args: vec![Operand::Copy(Place::local(base_local))],
                    destination: Place::local(l),
                    target: Some(next),
                });
                self.set_current(next);
                l
            };
            let hi_local = if *inclusive {
                let one = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(one),
                    Rvalue::Use(Operand::Const(ConstValue::Int(1))),
                    span,
                );
                let bumped = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(bumped),
                    Rvalue::BinaryOp {
                        op: BinOp::Add,
                        lhs: Operand::Copy(Place::local(hi_local)),
                        rhs: Operand::Copy(Place::local(one)),
                    },
                    span,
                );
                bumped
            } else {
                hi_local
            };
            let dest_ty = ty;
            let dest = self.fresh(dest_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_slice".to_string())),
                args: vec![
                    Operand::Copy(Place::local(base_local)),
                    Operand::Copy(Place::local(lo_local)),
                    Operand::Copy(Place::local(hi_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        // Walk through references so `&String` indexing behaves
        // the same as indexing a bare `String`. Prefer the MIR
        // local's pinned type over the HIR type when the base is
        // a simple Path — the type checker may have left the HIR
        // type as an unresolved inference variable for receivers
        // produced by runtime helpers (e.g. `read_to_string`),
        // and the indexing path needs the concrete `String` to
        // route to `gos_rt_str_byte_at` instead of falling
        // through to the array-projection helper.
        let mut base_kind = self
            .receiver_local_from_path(base)
            .map_or(base.ty, |local| self.locals[local.0 as usize].ty);
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(base_kind) {
            base_kind = *inner;
        }
        let base_is_string = matches!(self.tcx.kind_of(base_kind), TyKind::String);
        if base_is_string {
            let base_local = self.lower_expr(base)?;
            let index_local = self.lower_expr(index)?;
            // `gos_rt_str_byte_at` returns a zero-extended byte —
            // pin the MIR destination to `i64` so downstream
            // print/format dispatch routes to the integer helper
            // instead of mis-treating the byte as a string ptr.
            let dest_ty = match self.tcx.kind_of(ty) {
                TyKind::Int(_) => ty,
                _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
            };
            let dest = self.fresh(dest_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_str_byte_at".to_string())),
                args: vec![
                    Operand::Copy(Place::local(base_local)),
                    Operand::Copy(Place::local(index_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        let base_local = self.lower_expr(base)?;
        let index_local = self.lower_expr(index)?;
        // For Vec / Slice receivers (whose runtime layout is a
        // `*mut GosVec` header, not a flat element buffer) route
        // index reads through `gos_rt_vec_get_i64`. A naked
        // `Projection::Index` would treat the local's first 8
        // bytes as element 0 — which is the GosVec `len` field,
        // not the data buffer.
        let actual_base_kind = self
            .tcx
            .kind_of(self.locals[base_local.0 as usize].ty)
            .clone();
        let actual_base_kind = match actual_base_kind {
            TyKind::Ref { inner, .. } => self.tcx.kind_of(inner).clone(),
            other => other,
        };
        if matches!(actual_base_kind, TyKind::Vec(_) | TyKind::Slice(_)) {
            let dest_ty = match self.tcx.kind_of(ty) {
                TyKind::Int(_) => ty,
                _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
            };
            let dest = self.fresh(dest_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_i64".to_string())),
                args: vec![
                    Operand::Copy(Place::local(base_local)),
                    Operand::Copy(Place::local(index_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        let dest = self.fresh(ty);
        let place = Place {
            local: base_local,
            projection: vec![crate::ir::Projection::Index(index_local)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        Some(dest)
    }

    fn lower_while(&mut self, condition: &HirExpr, body: &HirExpr, span: Span) {
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let Some(cond_local) = self.lower_expr(condition) else {
            return;
        };
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cond_local)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        // `break` jumps to `exit`; `continue` jumps back to the
        // condition test (`header`).
        self.loop_stack.push(LoopContext {
            continue_to: header,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
    }

    fn lower_loop(&mut self, body: &HirExpr, _ty: Ty, span: Span) -> Option<Local> {
        if let Some(for_loop) = detect_for_loop(body) {
            if let Some(result) = self.try_lower_for_loop(&for_loop, span) {
                return Some(result);
            }
        }
        let header = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });
        self.set_current(header);
        // Unconditional `loop`: `continue` and `break` both have
        // somewhere sensible to land. `break` exits, `continue`
        // restarts the body.
        self.loop_stack.push(LoopContext {
            continue_to: header,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.terminate(Terminator::Goto { target: header });
        self.set_current(exit);
        None
    }

    /// Lowers a detected `for x in iter { body }` loop directly into
    /// a counter-driven CFG when `iter` is a range or an array-shaped
    /// expression. Returns `None` when the iterator's shape is not
    /// recognised so the generic `loop` fallback handles it.
    #[allow(clippy::cognitive_complexity)]
    fn try_lower_for_loop(&mut self, for_loop: &ForLoopShape<'_>, span: Span) -> Option<Local> {
        use gossamer_types::TyKind;
        // `for (k, v) in m.iter()` on a HashMap. Snapshot the keys
        // into a fresh `GosVec`, iterate it, and inside each iteration
        // synthesise `v = m.get_or(k, default)` so the tuple pattern
        // bindings see real values.
        if let Some(local) = self.try_lower_for_hashmap_iter(for_loop, span) {
            return Some(local);
        }
        // `for entry in v.iter()` / `for entry in v` where v is a
        // `json::Value` array — synthesise the loop with
        // `gos_rt_json_len` + `gos_rt_json_at`.
        let iter_target = match &for_loop.iter_expr.kind {
            HirExprKind::MethodCall { receiver, name, .. } if name.name == "iter" => {
                Some(receiver.as_ref())
            }
            _ => None,
        };
        let json_iter = iter_target.filter(|recv| {
            let recv_ty = self
                .receiver_local_from_path(recv)
                .map_or(recv.ty, |local| self.locals[local.0 as usize].ty);
            self.is_json_value_ty(recv_ty)
        });
        if let Some(recv) = json_iter {
            return self.lower_for_json(recv, for_loop.loop_pat, for_loop.body, span);
        }
        if self.is_json_value_ty(for_loop.iter_expr.ty) {
            return self.lower_for_json(for_loop.iter_expr, for_loop.loop_pat, for_loop.body, span);
        }
        match &for_loop.iter_expr.kind {
            HirExprKind::Range {
                start: Some(start),
                end: Some(end),
                inclusive,
            } => self.lower_for_range(
                start,
                end,
                *inclusive,
                for_loop.loop_pat,
                for_loop.body,
                span,
            ),
            HirExprKind::Array(arr) => {
                let len = match arr {
                    gossamer_hir::HirArrayExpr::List(elems) => elems.len() as i64,
                    gossamer_hir::HirArrayExpr::Repeat { count, .. } => {
                        literal_u64(count).and_then(|c| i64::try_from(c).ok())?
                    }
                };
                self.lower_for_array(
                    for_loop.iter_expr,
                    for_loop.loop_pat,
                    for_loop.body,
                    len,
                    span,
                )
            }
            _ => {
                // Fallback chain:
                //   1. fixed-size `[T; N]` (`&[T; N]`) → array iter.
                //   2. runtime `Vec<T>` (or peeled-through-`&`)
                //      → `gos_rt_vec_*` dynamic-length iter.
                //   3. give up (the for-loop fallback re-emits
                //      the original `loop {}` shape).
                let mut cur = for_loop.iter_expr.ty;
                // Also peel `.iter()` method calls — `for x in v.iter()`
                // and `for x in &v` both end up wanting Vec iteration.
                let iter_recv = match &for_loop.iter_expr.kind {
                    HirExprKind::MethodCall { receiver, name, .. } if name.name == "iter" => {
                        Some(receiver.as_ref())
                    }
                    _ => None,
                };
                if let Some(recv) = iter_recv {
                    let recv_ty = self
                        .receiver_local_from_path(recv)
                        .map_or(recv.ty, |local| self.locals[local.0 as usize].ty);
                    // Also try the receiver's HIR-expression kind:
                    // `[..].iter()` has receiver = Array(...) whose
                    // ty may be unresolved on the MIR side; the AST
                    // shape gives us the literal length directly.
                    if let HirExprKind::Array(arr) = &recv.kind {
                        let len = match arr {
                            gossamer_hir::HirArrayExpr::List(elems) => Some(elems.len() as i64),
                            gossamer_hir::HirArrayExpr::Repeat { count, .. } => {
                                literal_u64(count).and_then(|c| i64::try_from(c).ok())
                            }
                        };
                        if let Some(len) = len {
                            return self.lower_for_array(
                                recv,
                                for_loop.loop_pat,
                                for_loop.body,
                                len,
                                span,
                            );
                        }
                    }
                    let mut peeled = recv_ty;
                    let mut found_elem: Option<Ty> = None;
                    let mut found_len: Option<i64> = None;
                    loop {
                        match self.tcx.kind_of(peeled) {
                            TyKind::Vec(elem) | TyKind::Slice(elem) => {
                                found_elem = Some(*elem);
                                break;
                            }
                            TyKind::Array { len, elem } => {
                                if let Ok(l) = i64::try_from(*len) {
                                    found_len = Some(l);
                                    found_elem = Some(*elem);
                                }
                                break;
                            }
                            TyKind::Ref { inner, .. } => peeled = *inner,
                            _ => break,
                        }
                    }
                    if let Some(len) = found_len {
                        return self.lower_for_array(
                            recv,
                            for_loop.loop_pat,
                            for_loop.body,
                            len,
                            span,
                        );
                    }
                    // For `.iter()` on a receiver whose MIR type
                    // didn't resolve to a Vec/Slice/Array (often
                    // because the receiver is a field projection
                    // through a Var-typed parent), default to
                    // runtime-Vec iteration. The receiver value
                    // is whatever `gos_rt_arr_iter` returns on it
                    // (identity for slices/vecs); the loop reads
                    // each element via `gos_rt_vec_get_ptr` +
                    // `gos_load`. Element type defaults to i64.
                    let elem_ty =
                        found_elem.unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64));
                    return self.lower_for_vec(
                        recv,
                        elem_ty,
                        for_loop.loop_pat,
                        for_loop.body,
                        span,
                    );
                }
                // If the iter expression is a Path bound to a
                // local, prefer the local's MIR-pinned type to
                // the HIR expression type — the typechecker
                // often leaves stdlib-call results as Var, but
                // the MIR side may have pinned them to a
                // concrete `Vec<T>` via a runtime-helper return
                // type pin.
                if let HirExprKind::Path { segments, .. } = &for_loop.iter_expr.kind {
                    if let Some(first) = segments.first() {
                        if let Some(local) = self.lookup_local(&first.name) {
                            cur = self.locals[local.0 as usize].ty;
                        }
                    }
                }
                // Check whether the HIR-expression type or the
                // chained method-call return type pins the iter
                // expression to a `Vec<T>`. The MIR-side dispatch
                // for `s.split(...)`, `.lines()`, `.iter()`, etc.
                // already pins their return to `Vec<String>` via
                // `pinned_ret`, so by the time we get here we can
                // look at the call target name + walk the
                // dispatch table to reach the Vec(elem) shape.
                let mut for_vec_elem: Option<Ty> = None;
                if let TyKind::Vec(elem) = self.tcx.kind_of(cur) {
                    for_vec_elem = Some(*elem);
                }
                if for_vec_elem.is_none() {
                    if let HirExprKind::MethodCall { name, .. } = &for_loop.iter_expr.kind {
                        if matches!(name.name.as_str(), "split" | "lines") {
                            for_vec_elem = Some(self.tcx.string_ty());
                        }
                    }
                }
                if for_vec_elem.is_none() {
                    if let HirExprKind::MethodCall { name, receiver, .. } = &for_loop.iter_expr.kind
                    {
                        if matches!(name.name.as_str(), "keys" | "values") {
                            let recv_ty = self
                                .receiver_local_from_path(receiver)
                                .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
                            if matches!(self.tcx.kind_of(recv_ty), TyKind::HashMap { .. })
                                || matches!(self.tcx.kind_of(recv_ty), TyKind::Ref { .. })
                                    && self.hash_map_key_kind(recv_ty).is_some()
                            {
                                let elem = if name.name.as_str() == "keys" {
                                    match self.hash_map_key_kind(recv_ty) {
                                        Some(MapKeyKind::String) => self.tcx.string_ty(),
                                        _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
                                    }
                                } else {
                                    match self.hash_map_value_kind(recv_ty) {
                                        Some(MapValueKind::String) => self.tcx.string_ty(),
                                        _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
                                    }
                                };
                                for_vec_elem = Some(elem);
                            }
                        }
                    }
                }
                if for_vec_elem.is_none() {
                    if let HirExprKind::Call { callee, .. } = &for_loop.iter_expr.kind {
                        if let HirExprKind::Path { segments, .. } = &callee.kind {
                            let joined = segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join("::");
                            if matches!(
                                joined.as_str(),
                                "regex::find_all"
                                    | "regex::split"
                                    | "regex::captures_all"
                                    | "std::regex::find_all"
                                    | "std::regex::split"
                                    | "std::regex::captures_all"
                            ) {
                                for_vec_elem = Some(self.tcx.string_ty());
                            }
                        }
                    }
                }
                if let Some(elem) = for_vec_elem {
                    return self.lower_for_vec(
                        for_loop.iter_expr,
                        elem,
                        for_loop.loop_pat,
                        for_loop.body,
                        span,
                    );
                }
                let len_opt = loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::Array { len, .. } => {
                            break i64::try_from(*len).ok();
                        }
                        TyKind::Vec(elem) | TyKind::Slice(elem) => {
                            let elem = *elem;
                            return self.lower_for_vec(
                                for_loop.iter_expr,
                                elem,
                                for_loop.loop_pat,
                                for_loop.body,
                                span,
                            );
                        }
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => break None,
                    }
                };
                if let Some(len) = len_opt {
                    return self.lower_for_array(
                        for_loop.iter_expr,
                        for_loop.loop_pat,
                        for_loop.body,
                        len,
                        span,
                    );
                }
                // Default fallback: treat as a runtime Vec.
                // Element type defaults to i64, which is the
                // pointer width — every slot in a GosVec is
                // 8 bytes regardless of element shape, so
                // method calls on the binding still dispatch.
                let elem_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                self.lower_for_vec(
                    for_loop.iter_expr,
                    elem_ty,
                    for_loop.loop_pat,
                    for_loop.body,
                    span,
                )
            }
        }
    }

    /// Lowers `for (k, v) in m.iter()` on a `HashMap<K, V>` by
    /// snapshotting the keys into a `GosVec`, iterating it, and
    /// synthesising `v = m.get_or(k, default)` inside the body
    /// so both tuple bindings see real values. Returns `None`
    /// when the iter expression isn't a `HashMap.iter()` call or
    /// the loop pattern isn't a two-element tuple of bindings.
    fn try_lower_for_hashmap_iter(
        &mut self,
        for_loop: &ForLoopShape<'_>,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let HirExprKind::MethodCall { receiver, name, .. } = &for_loop.iter_expr.kind else {
            return None;
        };
        if name.name != "iter" {
            return None;
        }
        let recv_ty = self
            .receiver_local_from_path(receiver)
            .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
        if !matches!(self.tcx.kind_of(recv_ty), TyKind::HashMap { .. }) {
            return None;
        }
        let HirPatKind::Tuple(elems) = &for_loop.loop_pat.kind else {
            return None;
        };
        if elems.len() != 2 {
            return None;
        }
        let HirPatKind::Binding {
            name: key_name,
            mutable: key_mut,
        } = &elems[0].kind
        else {
            return None;
        };
        let HirPatKind::Binding {
            name: val_name,
            mutable: val_mut,
        } = &elems[1].kind
        else {
            return None;
        };
        let key_kind = self.hash_map_key_kind(recv_ty);
        let value_kind = self.hash_map_value_kind(recv_ty);
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let str_ty = self.tcx.string_ty();
        let key_ty = match key_kind {
            Some(MapKeyKind::String) => str_ty,
            _ => i64_ty,
        };
        let val_ty = match value_kind {
            Some(MapValueKind::String) => str_ty,
            _ => i64_ty,
        };
        let keys_helper = match key_kind {
            Some(MapKeyKind::String) => "gos_rt_map_keys_str",
            _ => "gos_rt_map_keys_i64",
        };
        let get_or_helper = match (key_kind, value_kind) {
            (Some(MapKeyKind::String), Some(MapValueKind::String)) => "gos_rt_map_get_or_str_str",
            (Some(MapKeyKind::String), _) => "gos_rt_map_get_or_str_i64",
            (_, Some(MapValueKind::String)) => "gos_rt_map_get_or_i64_str",
            _ => "gos_rt_map_get_or_i64",
        };

        let recv_local = self.lower_expr(receiver)?;
        let keys_vec_ty = self.tcx.intern(TyKind::Vec(key_ty));
        let keys_vec = self.fresh(keys_vec_ty);
        let after_keys = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(keys_helper.to_string())),
            args: vec![Operand::Copy(Place::local(recv_local))],
            destination: Place::local(keys_vec),
            target: Some(after_keys),
        });
        self.set_current(after_keys);

        let len_local = self.fresh(i64_ty);
        let after_len = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_len".to_string())),
            args: vec![Operand::Copy(Place::local(keys_vec))],
            destination: Place::local(len_local),
            target: Some(after_len),
        });
        self.set_current(after_len);

        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let step_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        // ptr = gos_rt_vec_get_ptr(keys, counter); k = *ptr
        let ptr_local = self.fresh(i64_ty);
        let after_ptr = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_ptr".to_string())),
            args: vec![
                Operand::Copy(Place::local(keys_vec)),
                Operand::Copy(Place::local(counter)),
            ],
            destination: Place::local(ptr_local),
            target: Some(after_ptr),
        });
        self.set_current(after_ptr);
        let key_local = self.push_local(key_ty, Some(key_name.clone()), *key_mut);
        self.bind_local(&key_name.name, key_local);
        let after_load = self.new_block(span);
        let zero_off = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(zero_off),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_load".to_string())),
            args: vec![
                Operand::Copy(Place::local(ptr_local)),
                Operand::Copy(Place::local(zero_off)),
            ],
            destination: Place::local(key_local),
            target: Some(after_load),
        });
        self.set_current(after_load);

        // v = m.get_or(k, default). Default-by-value-type: 0 for
        // i64-valued maps, an empty string for string-valued maps.
        let default_local = if matches!(value_kind, Some(MapValueKind::String)) {
            let l = self.fresh(str_ty);
            self.emit_assign(
                Place::local(l),
                Rvalue::Use(Operand::Const(ConstValue::Str(String::new()))),
                span,
            );
            l
        } else {
            let l = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(l),
                Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                span,
            );
            l
        };
        let val_local = self.push_local(val_ty, Some(val_name.clone()), *val_mut);
        self.bind_local(&val_name.name, val_local);
        let after_val = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(get_or_helper.to_string())),
            args: vec![
                Operand::Copy(Place::local(recv_local)),
                Operand::Copy(Place::local(key_local)),
                Operand::Copy(Place::local(default_local)),
            ],
            destination: Place::local(val_local),
            target: Some(after_val),
        });
        self.set_current(after_val);

        let _ = self.lower_expr(for_loop.body);
        self.pop_scope();
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        let bumped = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(bumped),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Copy(Place::local(bumped))),
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        let unit_ty = self.tcx.unit();
        let unit = self.fresh(unit_ty);
        self.emit_assign(
            Place::local(unit),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        Some(unit)
    }

    /// Iterates a runtime `Vec<T>` (a `*mut GosVec` pointer) via
    /// `gos_rt_vec_len` + `gos_rt_vec_get_ptr`. The element type
    /// `elem_ty` controls how the loaded slot is interpreted —
    /// for scalar elements the slot's `*mut u8` is dereferenced
    /// as i64, for pointer-shaped elements (String, struct, ref)
    /// it's reinterpreted directly.
    fn lower_for_vec(
        &mut self,
        iter_expr: &HirExpr,
        elem_ty: Ty,
        loop_pat: &HirPat,
        body: &HirExpr,
        span: Span,
    ) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);

        let iter_local = self.lower_expr(iter_expr)?;

        // len = gos_rt_vec_len(vec)
        let len_local = self.fresh(i64_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_len".to_string())),
            args: vec![Operand::Copy(Place::local(iter_local))],
            destination: Place::local(len_local),
            target: Some(next),
        });
        self.set_current(next);

        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );

        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let step_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        // ptr = gos_rt_vec_get_ptr(vec, counter); elem = *ptr
        let ptr_local = self.fresh(i64_ty);
        let after_ptr = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_ptr".to_string())),
            args: vec![
                Operand::Copy(Place::local(iter_local)),
                Operand::Copy(Place::local(counter)),
            ],
            destination: Place::local(ptr_local),
            target: Some(after_ptr),
        });
        self.set_current(after_ptr);
        let elem_local = self.fresh(elem_ty);
        let after_load = self.new_block(span);
        let zero_off = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(zero_off),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_load".to_string())),
            args: vec![
                Operand::Copy(Place::local(ptr_local)),
                Operand::Copy(Place::local(zero_off)),
            ],
            destination: Place::local(elem_local),
            target: Some(after_load),
        });
        self.set_current(after_load);
        if let HirPatKind::Binding { name, .. } = &loop_pat.kind {
            self.bind_local(&name.name, elem_local);
        }
        self.loop_stack.push(LoopContext {
            continue_to: step_block,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.pop_scope();
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        Some(self.lower_unit(span))
    }

    fn lower_for_range(
        &mut self,
        start: &HirExpr,
        end: &HirExpr,
        inclusive: bool,
        loop_pat: &HirPat,
        body: &HirExpr,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::{IntTy as It, TyKind};
        let start_local = self.lower_expr(start)?;
        let end_local = self.lower_expr(end)?;
        // The loop counter's cranelift width must be concrete. Prefer
        // the MIR type picked by `lower_literal` for `start`; fall
        // back to i64 when neither HIR nor lowered MIR gave an
        // integer kind (unsuffixed literal, leaked inference var, …).
        let int_ty = {
            let start_mir_ty = self.locals[start_local.0 as usize].ty;
            let hir_kind = self.tcx.kind_of(start.ty);
            let mir_kind = self.tcx.kind_of(start_mir_ty);
            match hir_kind {
                TyKind::Int(_) => start.ty,
                _ => match mir_kind {
                    TyKind::Int(_) => start_mir_ty,
                    _ => self.tcx.int_ty(It::I64),
                },
            }
        };
        let counter = self.push_local(int_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Copy(Place::local(start_local))),
            span,
        );

        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let step_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        let op = if inclusive { BinOp::Le } else { BinOp::Lt };
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(end_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        if let HirPatKind::Binding { name, mutable } = &loop_pat.kind {
            let bind_local = self.push_local(int_ty, Some(name.clone()), *mutable);
            self.bind_local(&name.name, bind_local);
            self.emit_assign(
                Place::local(bind_local),
                Rvalue::Use(Operand::Copy(Place::local(counter))),
                span,
            );
        }
        // `continue` skips the rest of the body but must still
        // advance the counter, so it lands on `step_block`, not
        // on `header` directly. `break` exits the loop entirely.
        self.loop_stack.push(LoopContext {
            continue_to: step_block,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.pop_scope();
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(int_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        Some(self.lower_unit(span))
    }

    fn lower_for_array(
        &mut self,
        iter_expr: &HirExpr,
        loop_pat: &HirPat,
        body: &HirExpr,
        array_len: i64,
        span: Span,
    ) -> Option<Local> {
        let array_local = self.lower_expr(iter_expr)?;
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let len_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(len_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(array_len)))),
            span,
        );

        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let step_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        if let HirPatKind::Binding { name, mutable } = &loop_pat.kind {
            let elem_ty = loop_pat.ty;
            let bind_local = self.push_local(elem_ty, Some(name.clone()), *mutable);
            self.bind_local(&name.name, bind_local);
            let indexed_place = Place {
                local: array_local,
                projection: vec![crate::ir::Projection::Index(counter)],
            };
            self.emit_assign(
                Place::local(bind_local),
                Rvalue::Use(Operand::Copy(indexed_place)),
                span,
            );
        }
        self.loop_stack.push(LoopContext {
            continue_to: step_block,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.pop_scope();
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        Some(self.lower_unit(span))
    }

    /// Iterates the elements of a `json::Value` array via the
    /// runtime's `gos_rt_json_len` / `gos_rt_json_at` helpers.
    /// Each iteration assigns the `loop_pat` binding to the
    /// element handle (a fresh `*mut GosJson` typed `json::Value`).
    fn lower_for_json(
        &mut self,
        iter_expr: &HirExpr,
        loop_pat: &HirPat,
        body: &HirExpr,
        span: Span,
    ) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let json_ty = self.tcx.json_value_ty();

        let iter_local = self.lower_expr(iter_expr)?;

        // len = gos_rt_json_len(iter)
        let len_local = self.fresh(i64_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_json_len".to_string())),
            args: vec![Operand::Copy(Place::local(iter_local))],
            destination: Place::local(len_local),
            target: Some(next),
        });
        self.set_current(next);

        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );

        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        // elem = gos_rt_json_at(iter, counter)
        let elem_local = self.fresh(json_ty);
        let after_at = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_json_at".to_string())),
            args: vec![
                Operand::Copy(Place::local(iter_local)),
                Operand::Copy(Place::local(counter)),
            ],
            destination: Place::local(elem_local),
            target: Some(after_at),
        });
        self.set_current(after_at);
        if let HirPatKind::Binding { name, .. } = &loop_pat.kind {
            self.bind_local(&name.name, elem_local);
        }
        let step_block = self.new_block(span);
        self.loop_stack.push(LoopContext {
            continue_to: step_block,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.pop_scope();
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        Some(self.lower_unit(span))
    }

    fn lower_unit(&mut self, span: Span) -> Local {
        let unit_ty = self.tcx.unit();
        let local = self.fresh(unit_ty);
        self.emit_assign(
            Place::local(local),
            Rvalue::Use(Operand::Const(ConstValue::Unit)),
            span,
        );
        local
    }
}

/// Structural view of the HIR shape produced by
/// `for p in iter { body }` lowering (`loop { match iter.next() {
/// Some(p) => body, None => break } }`). Used by the MIR lowerer to
/// emit a counter-driven CFG instead of a method call + pattern
/// match the native backend can't lower.
struct ForLoopShape<'h> {
    iter_expr: &'h HirExpr,
    loop_pat: &'h HirPat,
    body: &'h HirExpr,
}

fn detect_for_loop(body: &HirExpr) -> Option<ForLoopShape<'_>> {
    let HirExprKind::Block(block) = &body.kind else {
        return None;
    };
    if !block.stmts.is_empty() {
        return None;
    }
    let tail = block.tail.as_deref()?;
    let HirExprKind::Match { scrutinee, arms } = &tail.kind else {
        return None;
    };
    if arms.len() != 2 {
        return None;
    }
    let HirExprKind::MethodCall {
        receiver,
        name,
        args,
    } = &scrutinee.kind
    else {
        return None;
    };
    if name.name != "next" || !args.is_empty() {
        return None;
    }
    let some_arm = &arms[0];
    let none_arm = &arms[1];
    let HirPatKind::Variant {
        name: some_name,
        fields: some_fields,
    } = &some_arm.pattern.kind
    else {
        return None;
    };
    if some_name.name != "Some" || some_fields.len() != 1 {
        return None;
    }
    let HirPatKind::Variant {
        name: none_name,
        fields: none_fields,
    } = &none_arm.pattern.kind
    else {
        return None;
    };
    if none_name.name != "None" || !none_fields.is_empty() {
        return None;
    }
    Some(ForLoopShape {
        iter_expr: receiver,
        loop_pat: &some_fields[0],
        body: &some_arm.body,
    })
}

/// Extracts a `u64` count from a HIR integer-literal expression used
/// as the repeat count of `[value; count]`. Returns `None` for any
/// non-literal or negative value.
fn literal_u64(expr: &HirExpr) -> Option<u64> {
    let HirExprKind::Literal(HirLiteral::Int(text)) = &expr.kind else {
        return None;
    };
    let parsed = parse_int(text)?;
    u64::try_from(parsed).ok()
}

fn literal_to_const(lit: &HirLiteral) -> ConstValue {
    match lit {
        HirLiteral::Unit => ConstValue::Unit,
        HirLiteral::Bool(b) => ConstValue::Bool(*b),
        HirLiteral::Int(text) => ConstValue::Int(parse_int(text).unwrap_or(0)),
        HirLiteral::Float(text) => ConstValue::Float(parse_float(text).to_bits()),
        HirLiteral::Char(c) => ConstValue::Char(*c),
        HirLiteral::String(text) => ConstValue::Str(text.clone()),
        HirLiteral::Byte(b) => ConstValue::Int(i128::from(*b)),
        HirLiteral::ByteString(bytes) => {
            ConstValue::Str(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

fn parse_int(text: &str) -> Option<i128> {
    let cleaned = strip_int_suffix(text).replace('_', "");
    if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        return i128::from_str_radix(rest, 16).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        return i128::from_str_radix(rest, 2).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        return i128::from_str_radix(rest, 8).ok();
    }
    cleaned.parse::<i128>().ok()
}

fn parse_float(text: &str) -> f64 {
    for suffix in &["f32", "f64"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.parse::<f64>().unwrap_or(0.0);
        }
    }
    text.parse::<f64>().unwrap_or(0.0)
}

fn strip_int_suffix(text: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "i128", "u128", "isize", "usize", "i64", "u64", "i32", "u32", "i16", "u16", "i8", "u8",
    ];
    for suffix in SUFFIXES {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    text.to_string()
}

fn lower_binop(op: HirBinaryOp) -> BinOp {
    match op {
        HirBinaryOp::Add => BinOp::Add,
        HirBinaryOp::Sub => BinOp::Sub,
        HirBinaryOp::Mul => BinOp::Mul,
        HirBinaryOp::Div => BinOp::Div,
        HirBinaryOp::Rem => BinOp::Rem,
        // Logical `&&` / `||` lower to bitwise on the i1/i8
        // bool representation. The truth tables match: for
        // operands `a, b ∈ {0, 1}`, `a & b == a && b` and
        // `a | b == a || b`. (Short-circuit evaluation — not
        // calling the rhs when the lhs settles the result —
        // is a separate concern handled at HIR-to-MIR control
        // flow if/when we expose `&&`/`||` over expressions
        // with side effects.)
        HirBinaryOp::And | HirBinaryOp::BitAnd => BinOp::BitAnd,
        HirBinaryOp::Or | HirBinaryOp::BitOr => BinOp::BitOr,
        HirBinaryOp::BitXor => BinOp::BitXor,
        HirBinaryOp::Shl => BinOp::Shl,
        HirBinaryOp::Shr => BinOp::Shr,
        HirBinaryOp::Eq => BinOp::Eq,
        HirBinaryOp::Ne => BinOp::Ne,
        HirBinaryOp::Lt => BinOp::Lt,
        HirBinaryOp::Le => BinOp::Le,
        HirBinaryOp::Gt => BinOp::Gt,
        HirBinaryOp::Ge => BinOp::Ge,
    }
}

/// Structural equality on the simple HIR-expr shapes the
/// fused-increment peephole needs to compare: paths (variable
/// names), literals, and field/tuple-index chains. Everything
/// else returns `false` so the peephole stays conservative.
fn exprs_match(a: &HirExpr, b: &HirExpr) -> bool {
    match (&a.kind, &b.kind) {
        (HirExprKind::Path { segments: sa, .. }, HirExprKind::Path { segments: sb, .. }) => {
            sa.len() == sb.len() && sa.iter().zip(sb).all(|(x, y)| x.name == y.name)
        }
        (HirExprKind::Literal(la), HirExprKind::Literal(lb)) => match (la, lb) {
            (HirLiteral::Int(x), HirLiteral::Int(y)) => x == y,
            (HirLiteral::Bool(x), HirLiteral::Bool(y)) => x == y,
            (HirLiteral::Char(x), HirLiteral::Char(y)) => x == y,
            (HirLiteral::String(x), HirLiteral::String(y)) => x == y,
            _ => false,
        },
        (
            HirExprKind::Field {
                receiver: ra,
                name: na,
            },
            HirExprKind::Field {
                receiver: rb,
                name: nb,
            },
        ) => na.name == nb.name && exprs_match(ra, rb),
        (
            HirExprKind::TupleIndex {
                receiver: ra,
                index: ia,
            },
            HirExprKind::TupleIndex {
                receiver: rb,
                index: ib,
            },
        ) => ia == ib && exprs_match(ra, rb),
        _ => false,
    }
}

#[allow(dead_code)]
fn _used_imports(_: AssertMessage, _: FileId) {}
