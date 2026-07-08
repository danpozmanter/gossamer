//! Monomorphisation pass.
//! Walks every [`Body`] and materialises one specialised copy per
//! `(def, substs)` pair observed at a call site. The HIR lowering
//! upstream already stamps each MIR local with its concrete [`Ty`]
//! (no `TyKind::Param` escapes the type table's post-solve
//! projection), so a specialised copy is structurally identical to
//! its generic source under the flat-i64-per-slot layout - but the
//! copy is registered under a stable mangled name so each call site
//! can dispatch to its own specialisation.
//!

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use gossamer_resolve::DefId;
use gossamer_types::{GenericArg, Substs, Ty, TyCtxt, TyKind};

use crate::ir::{Body, ConstValue, Operand, Rvalue, StatementKind, Terminator};

/// Cap on the number of fixed-point iterations the monomorphiser
/// will run before bailing. Real workloads converge in ≤ 5; the
/// generous cap guards against a runaway generic that recursively
/// produces fresh specialisations.
const MAX_MONOMORPHISE_ITERATIONS: u32 = 32;

/// Monomorphises `bodies` by emitting one specialised copy per
/// distinct `(def, substs)` pair observed at a call site whose
/// substitution is non-empty. Monomorphic calls are untouched.
///
/// the pass is now **fixed-point**. The
/// previous implementation walked the original bodies once and
/// emitted copies after; specialisations that themselves called
/// other generics never had their inner calls specialised. We
/// loop until a pass produces no new copies - `fn map<T,U>(f:
/// fn(T)->U, xs)` calling `fn each<T>(f, xs)` now produces both
/// `map_i64_str` and `each_i64`. Cap at
/// `MAX_MONOMORPHISE_ITERATIONS` as a runaway guard.
pub fn monomorphise(bodies: &mut Vec<Body>, tcx: &mut TyCtxt) {
    let mut emitted: HashSet<String> = HashSet::new();
    // Source defs whose specialisation rewrote a trait-method call on a
    // type-parameter receiver. Only these need their call sites routed to
    // the mangled copy (and their now-dead template dropped): a scalar
    // generic keeps calling its template, which the compiled tiers lower
    // through the uniform pointer-width ABI. Routing a scalar generic to a
    // copy instead would mis-pass an `i64` argument as a pointer.
    let mut trait_specialised_defs: HashSet<u32> = HashSet::new();
    for iteration in 0..MAX_MONOMORPHISE_ITERATIONS {
        let mut needs: HashMap<DefId, Vec<Substs>> = HashMap::new();
        for body in bodies.iter() {
            for block in &body.blocks {
                for stmt in &block.stmts {
                    if let StatementKind::Assign { rvalue, .. } = &stmt.kind {
                        collect_from_rvalue(rvalue, &mut needs);
                    }
                }
                collect_from_terminator(&block.terminator, &mut needs);
            }
        }
        let sources: HashMap<u32, usize> = bodies
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.def.map(|d| (d.local, i)))
            .collect();
        let mut specialised: Vec<Body> = Vec::new();
        for (def, subst_list) in &needs {
            let Some(src_idx) = sources.get(&def.local) else {
                continue;
            };
            for substs in subst_list {
                if substs.is_empty() {
                    continue;
                }
                // A substitution made up only of const arguments needs no
                // specialised copy: a const generic array parameter is lowered
                // to a runtime-length sequence, so one body serves every value
                // of the const. The recorded const still keys this call's
                // `Substs` for typing; only the code copy is unnecessary.
                if substs
                    .as_slice()
                    .iter()
                    .all(|a| matches!(a, GenericArg::Const(_)))
                {
                    continue;
                }
                let name = mangled_name(*def, substs);
                if !emitted.insert(name.clone()) {
                    continue;
                }
                let mut copy = bodies[*src_idx].clone();
                copy.name = name;
                copy.def = None;
                let subst_tys: Vec<Option<Ty>> = substs
                    .as_slice()
                    .iter()
                    .map(|a| match a {
                        GenericArg::Type(t) => Some(*t),
                        GenericArg::Const(_) => None,
                    })
                    .collect();
                // Rewrite trait-method calls first, while the receiver locals
                // still carry the template's `Param` types: the rewrite keys on
                // a `Param` receiver to recognise a static-dispatch call and map
                // it to the concrete impl symbol. Substituting the locals first
                // would erase the `Param` and the call would stay an unresolved
                // bare method name.
                if rewrite_trait_method_calls(&mut copy, substs, tcx) {
                    trait_specialised_defs.insert(def.local);
                }
                // Then substitute the instantiation's concrete types for the
                // template's type parameters throughout the copy - local types
                // (so codegen sees the real struct/string/tuple/float layout
                // instead of a `Param` opaque slot) and internal call-site
                // generic args (so a recursive self-call resolves to this copy).
                for local in &mut copy.locals {
                    local.ty = subst_param_ty(tcx, local.ty, &subst_tys);
                }
                specialise_call_substs(&mut copy, &subst_tys, tcx);
                specialised.push(copy);
            }
        }
        let fn_progress = !specialised.is_empty();
        bodies.extend(specialised);
        let method_progress = specialise_methods_step(bodies, &mut emitted, tcx);
        if !fn_progress && !method_progress {
            // No new copies - fixed point reached.
            break;
        }
        assert!(
            iteration + 1 != MAX_MONOMORPHISE_ITERATIONS,
            "monomorphise: did not reach a fixed point in {MAX_MONOMORPHISE_ITERATIONS} iterations \
            - either there's a runaway generic that depends on its own specialisation, \
            or the cap needs to be raised after auditing the offending bodies"
        );
    }
    // Route every generic call whose argument is not an i64-slot scalar to its
    // specialised concrete copy. The template's flat-i64 ABI carries an
    // `i64`/`bool`/`char`/`()` argument correctly through the pointer-width
    // slot, so those keep calling the template; everything else (structs,
    // tuples, strings, `f64` - a float register class the i64 slot cannot hold)
    // is mishandled by the template and routes to its concrete copy, which uses
    // the real per-type ABI. Const-only instantiations have no copy.
    for body in bodies.iter_mut() {
        for block in &mut body.blocks {
            if let Terminator::Call { callee, .. } = &mut block.terminator
                && let Operand::FnRef { def, substs } = callee
                && !substs.is_empty()
                && substs_need_concrete_copy(substs, tcx)
            {
                let name = mangled_name(*def, substs);
                if emitted.contains(&name) {
                    *callee = Operand::Const(ConstValue::Str(name));
                }
            }
        }
    }
    // Trait-specialised templates carry an unresolved trait-method call in
    // their body; every caller now routes to a copy, so drop them.
    if !trait_specialised_defs.is_empty() {
        bodies.retain(|b| {
            b.def
                .is_none_or(|d| !trait_specialised_defs.contains(&d.local))
        });
    }
    // Resolve every local's type one last time so specialised
    // copies + originals share the resolved (no-Var) state.
    for body in bodies.iter_mut() {
        for local in &mut body.locals {
            local.ty = resolve(tcx, local.ty);
        }
    }
    // Register per-instantiation field-type tables for every generic
    // struct instantiation reachable from a (resolved) local type, so the
    // compiled tiers lay out `Wrapper<Point>` by its concrete field
    // (`Point`) instead of the declared `Param` slot.
    register_struct_instantiations(bodies, tcx);
}

/// One fixed-point round of generic-method specialisation. Methods are
/// dispatched by name (`Const(Str("Wrapper::get"))`, no `DefId`), so they
/// never enter the `FnRef`-keyed function path and their `self: &Wrapper<T>`
/// / `-> T` stay `Param` - which codegen renders as an opaque `ptr` slot,
/// mismatching the caller for non-pointer / aggregate `T`. For each call to a
/// generic method whose receiver (the `self` argument's local type) is a
/// concrete struct instantiation, materialise a per-instantiation copy with
/// the concrete types substituted in and route the call to it - the same
/// shape as free-function monomorphisation, keyed by name. Returns `true`
/// when at least one new copy was created.
fn specialise_methods_step(
    bodies: &mut Vec<Body>,
    emitted: &mut HashSet<String>,
    tcx: &mut TyCtxt,
) -> bool {
    let method_bases: HashMap<String, usize> = bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| b.def.is_none() && b.name.contains("::") && body_has_param(b, tcx))
        .map(|(i, b)| (b.name.clone(), i))
        .collect();
    if method_bases.is_empty() {
        return false;
    }
    let mut rewrites: Vec<(usize, usize, String)> = Vec::new();
    let mut to_create: Vec<(usize, Substs, String)> = Vec::new();
    for (bi, body) in bodies.iter().enumerate() {
        for (blk, block) in body.blocks.iter().enumerate() {
            let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                ..
            } = &block.terminator
            else {
                continue;
            };
            let Some(&base_idx) = method_bases.get(name) else {
                continue;
            };
            let Some(Operand::Copy(p)) = args.first() else {
                continue;
            };
            let Some(recv_decl) = body.locals.get(p.local.0 as usize) else {
                continue;
            };
            let recv_ty = peel_ref(tcx, recv_decl.ty);
            let TyKind::Adt { substs, .. } = tcx.kind_of(recv_ty).clone() else {
                continue;
            };
            if substs.is_empty() || substs.types().iter().any(|t| ty_contains_param(tcx, *t)) {
                continue;
            }
            let spec_name = method_mangled_name(name, &substs);
            rewrites.push((bi, blk, spec_name.clone()));
            if emitted.insert(spec_name.clone()) {
                to_create.push((base_idx, substs, spec_name));
            }
        }
    }
    let made = !to_create.is_empty();
    for (base_idx, substs, spec_name) in to_create {
        let mut copy = bodies[base_idx].clone();
        copy.name = spec_name;
        copy.def = None;
        let subst_tys: Vec<Option<Ty>> = substs
            .as_slice()
            .iter()
            .map(|a| match a {
                GenericArg::Type(t) => Some(*t),
                GenericArg::Const(_) => None,
            })
            .collect();
        for local in &mut copy.locals {
            local.ty = subst_param_ty(tcx, local.ty, &subst_tys);
        }
        specialise_call_substs(&mut copy, &subst_tys, tcx);
        bodies.push(copy);
    }
    for (bi, blk, spec_name) in rewrites {
        if let Terminator::Call { callee, .. } = &mut bodies[bi].blocks[blk].terminator {
            *callee = Operand::Const(ConstValue::Str(spec_name));
        }
    }
    made
}

/// Walks every body's local types and registers a per-instantiation field
/// table for each generic struct instantiation `Adt { def, substs }` whose
/// `substs` are concrete (no rigid `Param`). Recurses through the
/// substituted field types so a nested instantiation (`Outer<Inner<T>>`)
/// is registered too.
fn register_struct_instantiations(bodies: &[Body], tcx: &mut TyCtxt) {
    let mut done: HashSet<(DefId, Substs)> = HashSet::new();
    let mut stack: Vec<Ty> = Vec::new();
    for body in bodies {
        for local in &body.locals {
            stack.push(local.ty);
        }
    }
    while let Some(ty) = stack.pop() {
        match tcx.kind_of(ty).clone() {
            TyKind::Adt { def, substs } if !substs.is_empty() => {
                for t in substs.types() {
                    stack.push(t);
                }
                if substs.types().iter().any(|t| ty_contains_param(tcx, *t)) {
                    continue;
                }
                if !done.insert((def, substs.clone())) {
                    continue;
                }
                let Some(decl) = tcx.struct_field_tys(def).map(<[Ty]>::to_vec) else {
                    continue;
                };
                let subst_tys: Vec<Option<Ty>> = substs
                    .as_slice()
                    .iter()
                    .map(|a| match a {
                        GenericArg::Type(t) => Some(*t),
                        GenericArg::Const(_) => None,
                    })
                    .collect();
                let inst: Vec<Ty> = decl
                    .iter()
                    .map(|&f| subst_param_ty(tcx, f, &subst_tys))
                    .collect();
                for f in &inst {
                    stack.push(*f);
                }
                tcx.register_struct_fields_inst(def, substs, inst);
            }
            TyKind::Ref { inner, .. }
            | TyKind::Vec(inner)
            | TyKind::Slice(inner)
            | TyKind::Sender(inner)
            | TyKind::Receiver(inner)
            | TyKind::JoinHandle(inner) => stack.push(inner),
            TyKind::Array { elem, .. } => stack.push(elem),
            TyKind::Tuple(elems) => stack.extend(elems),
            TyKind::HashMap { key, value } => {
                stack.push(key);
                stack.push(value);
            }
            _ => {}
        }
    }
}

/// `true` if `ty` mentions a generic `Param` anywhere in its structure.
fn ty_contains_param(tcx: &TyCtxt, ty: Ty) -> bool {
    match tcx.kind_of(ty) {
        TyKind::Param { .. } => true,
        TyKind::Ref { inner, .. }
        | TyKind::Vec(inner)
        | TyKind::Slice(inner)
        | TyKind::Sender(inner)
        | TyKind::Receiver(inner)
        | TyKind::JoinHandle(inner) => ty_contains_param(tcx, *inner),
        TyKind::Array { elem, .. } => ty_contains_param(tcx, *elem),
        TyKind::Tuple(elems) => elems.iter().any(|t| ty_contains_param(tcx, *t)),
        TyKind::HashMap { key, value } => {
            ty_contains_param(tcx, *key) || ty_contains_param(tcx, *value)
        }
        TyKind::Adt { substs, .. } | TyKind::Alias { substs, .. } => {
            substs.types().iter().any(|t| ty_contains_param(tcx, *t))
        }
        _ => false,
    }
}

/// `true` if any of `body`'s locals carry a generic `Param`, marking it a
/// generic template (a method on a generic struct, or a generic function)
/// that needs a per-instantiation copy before codegen.
fn body_has_param(body: &Body, tcx: &TyCtxt) -> bool {
    body.locals.iter().any(|l| ty_contains_param(tcx, l.ty))
}

/// Peels a single layer of `&T` / `&mut T`, returning the pointee. A method
/// receiver is `&self`, so the receiver's struct type sits one reference
/// deep; values passed by value (a small aggregate) are returned unchanged.
fn peel_ref(tcx: &TyCtxt, ty: Ty) -> Ty {
    match tcx.kind_of(ty) {
        TyKind::Ref { inner, .. } => *inner,
        _ => ty,
    }
}

/// Mangled name of a generic method instantiation. Methods carry no `DefId`,
/// so the name keys the specialisation: the base `Type::method` name plus the
/// interned id of each concrete type argument (equal types share an id, so a
/// call site and the materialised copy agree).
fn method_mangled_name(base: &str, substs: &Substs) -> String {
    let mut out = format!("{base}$mono$");
    for (i, arg) in substs.as_slice().iter().enumerate() {
        if i > 0 {
            out.push('_');
        }
        match arg {
            GenericArg::Type(ty) => {
                out.push('t');
                out.push_str(&ty.as_u32().to_string());
            }
            GenericArg::Const(c) => {
                out.push('c');
                out.push_str(&c.to_string());
            }
        }
    }
    out
}

/// Static trait dispatch for a monomorphised generic body: a method
/// call on a type-parameter receiver (`x.describe()` where `x: &T`)
/// lowers to a bare `describe` callee the compiled tiers cannot link.
/// For this instantiation the receiver's parameter resolves to a concrete
/// type via `substs`, so rewrite the callee to that type's impl symbol
/// (`Dog::describe`), which already exists as a real function. The trait
/// bound checked at the call site guarantees the impl is present.
fn rewrite_trait_method_calls(copy: &mut Body, substs: &Substs, tcx: &TyCtxt) -> bool {
    let subst_tys: Vec<Option<Ty>> = substs
        .as_slice()
        .iter()
        .map(|a| match a {
            GenericArg::Type(t) => Some(*t),
            GenericArg::Const(_) => None,
        })
        .collect();
    let local_tys: Vec<Ty> = copy.locals.iter().map(|l| l.ty).collect();
    let mut rewrote = false;
    for block in &mut copy.blocks {
        let Terminator::Call { callee, args, .. } = &mut block.terminator else {
            continue;
        };
        let Operand::Const(ConstValue::Str(name)) = callee else {
            continue;
        };
        if name.contains("::") {
            continue;
        }
        let Some(Operand::Copy(recv)) = args.first() else {
            continue;
        };
        if !recv.projection.is_empty() {
            continue;
        }
        let Some(recv_ty) = local_tys.get(recv.local.0 as usize).copied() else {
            continue;
        };
        let Some(idx) = param_index(tcx, recv_ty) else {
            continue;
        };
        let Some(Some(concrete)) = subst_tys.get(idx) else {
            continue;
        };
        if let Some(cname) = adt_name(tcx, *concrete) {
            *callee = Operand::Const(ConstValue::Str(format!("{cname}::{name}")));
            rewrote = true;
        }
    }
    rewrote
}

/// Generic-parameter index of a receiver type (`&T` / `T`), or `None`.
fn param_index(tcx: &TyCtxt, ty: Ty) -> Option<usize> {
    let mut t = ty;
    while let TyKind::Ref { inner, .. } = tcx.kind_of(t).clone() {
        t = inner;
    }
    match tcx.kind_of(t) {
        TyKind::Param { idx, .. } => Some(idx.0 as usize),
        _ => None,
    }
}

/// Source name of a concrete named type, or `None` for non-ADTs.
fn adt_name(tcx: &TyCtxt, ty: Ty) -> Option<String> {
    match tcx.kind_of(ty).clone() {
        TyKind::Adt { def, .. } => tcx.def_name(def).map(str::to_string),
        _ => None,
    }
}

fn collect_from_rvalue(rvalue: &Rvalue, out: &mut HashMap<DefId, Vec<Substs>>) {
    if let Rvalue::Use(operand) = rvalue {
        collect_from_operand(operand, out);
    }
}

fn collect_from_terminator(term: &Terminator, out: &mut HashMap<DefId, Vec<Substs>>) {
    if let Terminator::Call { callee, args, .. } = term {
        collect_from_operand(callee, out);
        for arg in args {
            collect_from_operand(arg, out);
        }
    }
}

fn collect_from_operand(operand: &Operand, out: &mut HashMap<DefId, Vec<Substs>>) {
    if let Operand::FnRef { def, substs } = operand {
        if !substs.is_empty() {
            let list = out.entry(*def).or_default();
            if !list.iter().any(|existing| existing == substs) {
                list.push(substs.clone());
            }
        }
    }
}

fn resolve(tcx: &mut TyCtxt, ty: Ty) -> Ty {
    let _ = tcx.kind(ty);
    ty
}

/// Whether any of `substs`' type arguments is a pointer-represented aggregate
/// that needs a concrete specialised copy. These types all pass through the
/// flat-i64 slot as a single pointer, so a routed call's pointer-width ABI
/// matches the concrete copy's ABI exactly - the copy then sees the real
/// layout (struct fields, tuple/string/vec contents) instead of an opaque
/// `Param`. Scalars with ABI-sensitive register classes (`Float`/`Bool`/`Char`)
/// also need concrete copies: keeping them behind an opaque `Param` leaves LLVM
/// to treat the payload as an i64 slot and loses the real operation/display
/// semantics.
fn substs_need_concrete_copy(substs: &Substs, tcx: &TyCtxt) -> bool {
    substs.as_slice().iter().any(|a| match a {
        GenericArg::Type(t) => matches!(
            tcx.kind_of(*t),
            TyKind::Float(_)
                | TyKind::Bool
                | TyKind::Char
                | TyKind::Adt { .. }
                | TyKind::Tuple(_)
                | TyKind::String
                | TyKind::Vec(_)
                | TyKind::Slice(_)
                | TyKind::Array { .. }
                | TyKind::HashMap { .. }
        ),
        GenericArg::Const(_) => false,
    })
}

/// Substitutes a specialisation's concrete types for the template's type
/// parameters within `ty`, recursing through composite types. A `Param` whose
/// position holds a const argument (`subst_tys[i] == None`) is left unchanged.
fn subst_param_ty(tcx: &mut TyCtxt, ty: Ty, subst_tys: &[Option<Ty>]) -> Ty {
    let kind = tcx.kind_of(ty).clone();
    match kind {
        TyKind::Param { idx, .. } => subst_tys
            .get(idx.0 as usize)
            .copied()
            .flatten()
            .unwrap_or(ty),
        TyKind::Ref { inner, mutability } => {
            let inner = subst_param_ty(tcx, inner, subst_tys);
            tcx.intern(TyKind::Ref { inner, mutability })
        }
        TyKind::Vec(elem) => {
            let elem = subst_param_ty(tcx, elem, subst_tys);
            tcx.intern(TyKind::Vec(elem))
        }
        TyKind::Slice(elem) => {
            let elem = subst_param_ty(tcx, elem, subst_tys);
            tcx.intern(TyKind::Slice(elem))
        }
        TyKind::Array { elem, len } => {
            let elem = subst_param_ty(tcx, elem, subst_tys);
            tcx.intern(TyKind::Array { elem, len })
        }
        TyKind::Tuple(elems) => {
            let elems = elems
                .iter()
                .map(|&e| subst_param_ty(tcx, e, subst_tys))
                .collect();
            tcx.intern(TyKind::Tuple(elems))
        }
        TyKind::HashMap { key, value } => {
            let key = subst_param_ty(tcx, key, subst_tys);
            let value = subst_param_ty(tcx, value, subst_tys);
            tcx.intern(TyKind::HashMap { key, value })
        }
        TyKind::Adt { def, substs } => {
            let new_args = substs
                .as_slice()
                .iter()
                .map(|a| match a {
                    GenericArg::Type(t) => GenericArg::Type(subst_param_ty(tcx, *t, subst_tys)),
                    GenericArg::Const(c) => GenericArg::Const(*c),
                })
                .collect();
            tcx.intern(TyKind::Adt {
                def,
                substs: Substs::from_args(new_args),
            })
        }
        _ => ty,
    }
}

/// Rewrites every internal call site's `FnRef` generic args in `copy`,
/// substituting the specialisation's concrete types for the template's type
/// parameters. Mirrors the operand set `collect_from_*` inspects.
fn specialise_call_substs(copy: &mut Body, subst_tys: &[Option<Ty>], tcx: &mut TyCtxt) {
    fn subst_operand(op: &mut Operand, subst_tys: &[Option<Ty>], tcx: &mut TyCtxt) {
        if let Operand::FnRef { substs, .. } = op
            && !substs.is_empty()
        {
            let new_args = substs
                .as_slice()
                .iter()
                .map(|a| match a {
                    GenericArg::Type(t) => GenericArg::Type(subst_param_ty(tcx, *t, subst_tys)),
                    GenericArg::Const(c) => GenericArg::Const(*c),
                })
                .collect();
            *substs = Substs::from_args(new_args);
        }
    }
    for block in &mut copy.blocks {
        for stmt in &mut block.stmts {
            if let StatementKind::Assign {
                rvalue: Rvalue::Use(op),
                ..
            } = &mut stmt.kind
            {
                subst_operand(op, subst_tys, tcx);
            }
        }
        if let Terminator::Call { callee, args, .. } = &mut block.terminator {
            subst_operand(callee, subst_tys, tcx);
            for arg in args.iter_mut() {
                subst_operand(arg, subst_tys, tcx);
            }
        }
    }
}

/// Walks every call site that supplies generic arguments and
/// rejects substitutions whose `T` does not fit the codegen's
/// flat-i64 ABI. Returns one human-readable error per offending
/// site; the empty `Vec` means every generic instantiation is
/// representable.
///
/// The flat-i64 ABI passes every generic parameter through a
/// single `i64` register slot. Layout-driven specialisation
/// (parity plan §P4) is the long-term fix; until then any `T`
/// wider than 8 bytes by value (tuples, fixed arrays, named ADTs,
/// strings, vecs, hashmaps, function references, closures) will
/// either corrupt memory at runtime (compiled tier) or produce a
/// runtime type error (interp). This check shifts that failure
/// to compile time.
///
/// The allowed set is intentionally narrow:
/// `Bool`, `Char`, `Int(_)`, `Float(_)`, `Unit`, `Never`. Anything
/// else flips the diagnostic on. `Sender<T>`, `Receiver<T>`,
/// `Ref<T>`, and pointer-shaped runtime handles do round-trip
/// through `i64` in some paths but are conservatively refused
/// here so generic code that "happens to work today" doesn't
/// silently break when a user instantiates it with an
/// incompatible `T` next month.
///
/// Doc pointer the diagnostic cites: `docs/codegen_abi.md`.
#[must_use]
pub fn check_generic_layouts(bodies: &[Body], tcx: &TyCtxt) -> Vec<String> {
    let mut needs: HashMap<DefId, Vec<Substs>> = HashMap::new();
    for body in bodies {
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { rvalue, .. } = &stmt.kind {
                    collect_from_rvalue(rvalue, &mut needs);
                }
            }
            collect_from_terminator(&block.terminator, &mut needs);
        }
    }
    let mut errors: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (def, subst_list) in &needs {
        for substs in subst_list {
            if substs.is_empty() {
                continue;
            }
            for (i, arg) in substs.as_slice().iter().enumerate() {
                let GenericArg::Type(ty) = arg else { continue };
                if !fits_flat_i64_abi(tcx, *ty) {
                    let key = format!("{}|{}|{}", def.local, i, ty.as_u32());
                    if !seen.insert(key) {
                        continue;
                    }
                    let render = render_ty_for_diagnostic(tcx, *ty);
                    errors.push(format!(
                        "error[GM0001]: generic parameter at position {i} of fn#{} \
                         instantiated with `{render}`, which is not representable in \
                         the flat-i64 ABI used by codegen.\n  \
                         Until layout-driven specialisation lands (parity plan §P4), \
                         only primitive scalars (`bool`, `char`, integer / float \
                         types, `()`) are permitted as generic arguments. See \
                         docs/codegen_abi.md.",
                        def.local
                    ));
                }
            }
        }
    }
    errors
}

/// Predicate matching the set of types the codegen can plumb
/// through a generic parameter. The original ABI restricted this
/// to scalars (Bool/Char/Int/Float/Unit/Never); the widened ABI
/// allows aggregate types as generics by passing them through a
/// single-pointer environment slot, mirroring the closure
/// strategy already in use (see `lowering_bugs_round2.md`).
///
/// Permitted today:
///
/// - Scalars: `bool`, `char`, integer / float, `()`, `!`.
/// - `String`, `Vec<T>`, `HashMap<K, V>`, `HashSet<T>`,
///   `BTreeMap<K, V>` - by-pointer in the flat ABI.
/// - Tuples and named ADTs (struct/enum) - by-pointer.
/// - Function references and channel handles (`Sender<T>` /
///   `Receiver<T>`) - already round-trip through `i64` in the
///   compiled tier.
/// - Refs (`&T`).
///
/// Still rejected:
/// - `TyKind::Closure` - needs explicit env pointer wiring at
///   the call site that monomorphisation doesn't yet rewrite.
/// - `TyKind::Alias` (unresolved type alias) - should never
///   reach codegen, but flagged here defensively.
fn fits_flat_i64_abi(tcx: &TyCtxt, ty: Ty) -> bool {
    match tcx.kind_of(ty) {
        TyKind::Bool
        | TyKind::Char
        | TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Unit
        | TyKind::Never
        | TyKind::String
        | TyKind::Vec(_)
        | TyKind::HashMap { .. }
        | TyKind::Sender(_)
        | TyKind::Receiver(_)
        | TyKind::JoinHandle(_)
        | TyKind::Ref { .. }
        | TyKind::FnDef { .. }
        | TyKind::FnPtr(_)
        | TyKind::Adt { .. }
        | TyKind::Tuple(_)
        | TyKind::Array { .. }
        | TyKind::Slice(_) => true,
        // A `Param`-typed generic argument is a template-internal call site -
        // a recursive generic's self-call (`fn rec<T>(..) { rec(..) }`) carries
        // `substs = [T]`, or a scalar generic keeps calling its template. It is
        // not a concrete instantiation; the real instantiations are checked
        // when their own (concrete) substs are observed. Rejecting it was a
        // false positive that blocked recursive generics from compiling.
        TyKind::Param { .. } => true,
        TyKind::Closure { .. } | TyKind::Alias { .. } => false,
        _ => false,
    }
}

/// Best-effort one-line spelling of a `Ty` for the diagnostic.
/// Intentionally terse - full type printing lives in
/// `gossamer-types::printer`; we don't want to drag the printer
/// crate's full dependency surface into the MIR diagnostic path.
fn render_ty_for_diagnostic(tcx: &TyCtxt, ty: Ty) -> String {
    match tcx.kind_of(ty) {
        TyKind::Bool => "bool".to_string(),
        TyKind::Char => "char".to_string(),
        TyKind::String => "String".to_string(),
        TyKind::Int(_) => "int".to_string(),
        TyKind::Float(_) => "float".to_string(),
        TyKind::Unit => "()".to_string(),
        TyKind::Never => "!".to_string(),
        TyKind::Tuple(_) => "tuple".to_string(),
        TyKind::Array { .. } => "array".to_string(),
        TyKind::Slice(_) => "slice".to_string(),
        TyKind::Vec(_) => "Vec<...>".to_string(),
        TyKind::HashMap { .. } => "HashMap<...>".to_string(),
        TyKind::Sender(_) => "Sender<...>".to_string(),
        TyKind::Receiver(_) => "Receiver<...>".to_string(),
        TyKind::JoinHandle(_) => "JoinHandle<...>".to_string(),
        TyKind::Ref { .. } => "&T".to_string(),
        TyKind::FnDef { .. } => "fn-item".to_string(),
        TyKind::FnPtr(_) => "fn-pointer".to_string(),
        TyKind::Closure { .. } => "closure".to_string(),
        TyKind::Adt { .. } => "named struct/enum".to_string(),
        TyKind::Alias { .. } => "alias".to_string(),
        _ => "<unrenderable>".to_string(),
    }
}

/// Returns the stable mangled name for a specialised copy of
/// function `def` at substitution `substs`. Callers (MIR codegen,
/// native backend) use this name as the symbol the specialised body
/// is registered under.
#[must_use]
pub fn mangled_name(def: DefId, substs: &Substs) -> String {
    let mut out = format!("fn#{}__mono__", def.local);
    for (i, arg) in substs.as_slice().iter().enumerate() {
        if i > 0 {
            out.push('_');
        }
        match arg {
            GenericArg::Type(ty) => {
                out.push('t');
                out.push_str(&ty.as_u32().to_string());
            }
            GenericArg::Const(c) => {
                out.push('c');
                out.push_str(&c.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monomorphise_is_idempotent_on_a_concrete_body() {
        // Smoke test: running the pass twice over the same body must
        // produce identical structural output - the pass is
        // deliberately a fixpoint.
        let mut tcx = TyCtxt::new();
        let unit = tcx.unit();
        let recorded = unit;
        let body = Body {
            name: "f".to_string(),
            def: None,
            arity: 0,
            locals: vec![
                crate::ir::LocalDecl {
                    ty: unit,
                    debug_name: None,
                    mutable: false,
                    region: false,
                },
                crate::ir::LocalDecl {
                    ty: recorded,
                    debug_name: None,
                    mutable: false,
                    region: false,
                },
            ],
            blocks: Vec::new(),
            span: gossamer_lex::Span::new(
                {
                    let mut map = gossamer_lex::SourceMap::new();
                    map.add_file("t.gos", "")
                },
                0,
                0,
            ),
        };
        let before = body.locals[1].ty;
        let mut bodies = vec![body];
        monomorphise(&mut bodies, &mut tcx);
        assert_eq!(bodies[0].locals[1].ty, before);
        monomorphise(&mut bodies, &mut tcx);
        assert_eq!(bodies[0].locals[1].ty, before);
    }
}
