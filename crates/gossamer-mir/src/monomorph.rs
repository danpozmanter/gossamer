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
use gossamer_types::{GenericArg, Mutbl, Substs, Ty, TyCtxt, TyKind};

use crate::ir::{
    Body, ConstValue, Local, Operand, Place, Projection, Rvalue, StatementKind, Terminator,
};

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
    // A method's own body states the receiver type it was lowered with, which
    // is what says whether a specialised call site has to hand it an address.
    let receiver_is_ref: HashMap<String, bool> = bodies
        .iter()
        .filter(|b| b.arity >= 1 && b.name.contains("::"))
        .filter_map(|b| {
            b.locals.get(1).map(|recv| {
                (
                    b.name.clone(),
                    matches!(tcx.kind_of(recv.ty), TyKind::Ref { .. }),
                )
            })
        })
        .collect();
    let mut emitted: HashSet<String> = HashSet::new();
    let sources: HashMap<u32, usize> = bodies
        .iter()
        .enumerate()
        .filter_map(|(i, b)| b.def.map(|d| (d.local, i)))
        .collect();
    let method_bases: HashMap<String, usize> = bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| b.def.is_none() && b.name.contains("::") && body_has_param(b, tcx))
        .map(|(i, b)| (b.name.clone(), i))
        .collect();
    // Source defs whose specialisation rewrote a trait-method call on a
    // type-parameter receiver. Only these need their call sites routed to
    // the mangled copy (and their now-dead template dropped): a scalar
    // generic keeps calling its template, which the compiled tiers lower
    // through the uniform pointer-width ABI. Routing a scalar generic to a
    // copy instead would mis-pass an `i64` argument as a pointer.
    let mut trait_specialised_defs: HashSet<u32> = HashSet::new();
    // Method templates whose specialisation resolved a trait call through a
    // type parameter. The template keeps the unresolved bare callee, so once
    // every call site routes to a copy the template has to go with it.
    let mut trait_specialised_methods: HashSet<String> = HashSet::new();
    let mut function_scan_start = 0;
    let mut method_scan_start = 0;
    for iteration in 0..MAX_MONOMORPHISE_ITERATIONS {
        let function_scan_end = bodies.len();
        let specialised = specialise_functions_step(
            bodies,
            &sources,
            &mut emitted,
            &mut trait_specialised_defs,
            &receiver_is_ref,
            tcx,
            function_scan_start,
        );
        let fn_progress = !specialised.is_empty();
        bodies.extend(specialised);
        let (method_progress, method_scan_end) = specialise_methods_step(
            bodies,
            &method_bases,
            &mut emitted,
            &mut trait_specialised_methods,
            &receiver_is_ref,
            tcx,
            method_scan_start,
        );
        if !fn_progress && !method_progress {
            // No new copies - fixed point reached.
            break;
        }
        function_scan_start = function_scan_end;
        method_scan_start = method_scan_end;
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
                // A template whose trait call was resolved per instantiation
                // is dropped below, so every one of its call sites routes to
                // a copy - including a scalar instantiation, which would
                // otherwise keep pointing at a body that no longer exists.
                && (substs_need_concrete_copy(substs, tcx)
                    || trait_specialised_defs.contains(&def.local))
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
    if !trait_specialised_defs.is_empty() || !trait_specialised_methods.is_empty() {
        bodies.retain(|b| {
            b.def
                .is_none_or(|d| !trait_specialised_defs.contains(&d.local))
                && !trait_specialised_methods.contains(&b.name)
        });
    }
    // A `&self` method reads its receiver as an address on every tier, so
    // every call to one has to hand it an address. A generic template keeps
    // serving scalar instantiations directly, and its receiver travelled as
    // the opaque slot value the parameter had; settle the convention here,
    // where every body - template and copy alike - is in its final form.
    for body in bodies.iter_mut() {
        borrow_scalar_receivers_for_ref_methods(body, &receiver_is_ref, tcx);
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

/// Materialises free-function specialisations requested by newly discovered
/// bodies. Method calls use a separate name-keyed path below.
fn specialise_functions_step(
    bodies: &[Body],
    sources: &HashMap<u32, usize>,
    emitted: &mut HashSet<String>,
    trait_specialised_defs: &mut HashSet<u32>,
    receiver_is_ref: &HashMap<String, bool>,
    tcx: &mut TyCtxt,
    scan_start: usize,
) -> Vec<Body> {
    let mut needs: HashMap<DefId, Vec<Substs>> = HashMap::new();
    for body in &bodies[scan_start..] {
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { rvalue, .. } = &stmt.kind {
                    collect_from_rvalue(rvalue, &mut needs);
                }
            }
            collect_from_terminator(&block.terminator, &mut needs);
        }
    }
    let mut specialised = Vec::new();
    for (def, subst_list) in &needs {
        let Some(src_idx) = sources.get(&def.local) else {
            continue;
        };
        for substs in subst_list {
            if substs.is_empty() || substs_are_const_only(substs) {
                continue;
            }
            let name = mangled_name(*def, substs);
            if !emitted.insert(name.clone()) {
                continue;
            }
            let mut copy = bodies[*src_idx].clone();
            copy.name = name;
            copy.def = None;
            let subst_tys = subst_type_arguments(substs);
            // Do this while locals retain template parameters. The rewrite
            // recognises a parameter receiver and selects the concrete impl.
            if rewrite_trait_method_calls(&mut copy, substs, receiver_is_ref, tcx) {
                trait_specialised_defs.insert(def.local);
            }
            for local in &mut copy.locals {
                local.ty = subst_param_ty(tcx, local.ty, &subst_tys);
            }
            repair_generic_element_reads(&mut copy, tcx);
            borrow_scalar_receivers_for_ref_methods(&mut copy, receiver_is_ref, tcx);
            specialise_call_substs(&mut copy, &subst_tys, tcx);
            specialised.push(copy);
        }
    }
    specialised
}

/// Repairs a container element read whose element type was a parameter.
///
/// The template lowered `xs[i]` for an opaque one-slot parameter, which is the
/// scalar read. Once the parameter is known to be an aggregate the element
/// occupies its slot inline and the address of that slot is the value, so the
/// read has to become the pointer form the concrete lowering emits. Leaving
/// the scalar read in place hands the callee the element's first bytes where
/// it expects the element's address.
///
/// Mirrors the element-representation predicate in the index/loop lowering:
/// a struct ADT is address-is-value at any width, and other aggregates are
/// once they exceed a single slot.
fn repair_generic_element_reads(copy: &mut Body, tcx: &TyCtxt) {
    let local_tys: Vec<Ty> = copy.locals.iter().map(|l| l.ty).collect();
    for block in &mut copy.blocks {
        let Terminator::Call {
            callee,
            destination,
            ..
        } = &mut block.terminator
        else {
            continue;
        };
        let Operand::Const(ConstValue::Str(name)) = callee else {
            continue;
        };
        if name != "gos_rt_vec_get_i64" && name != "gos_rt_vec_get_i64_unchecked" {
            continue;
        }
        if !destination.projection.is_empty() {
            continue;
        }
        let Some(elem_ty) = local_tys.get(destination.local.0 as usize).copied() else {
            continue;
        };
        let is_struct_adt = matches!(
            tcx.kind_of(elem_ty),
            TyKind::Adt { def, .. }
                if def.local < u32::MAX - 16 && tcx.struct_field_tys(*def).is_some()
        );
        let is_wide_aggregate = matches!(
            tcx.kind_of(elem_ty),
            TyKind::Tuple(_) | TyKind::Adt { .. } | TyKind::Array { .. }
        ) && tcx.slot_bytes(elem_ty) > 8;
        if is_struct_adt || is_wide_aggregate {
            *callee = Operand::Const(ConstValue::Str("gos_rt_vec_get_ptr".to_string()));
        }
    }
}

/// Borrows a receiver whose concrete type turned out to be a scalar for a
/// method whose impl declares `&self`.
///
/// A type parameter is one opaque slot to the template, so the receiver
/// travels by value. When the parameter resolves to a struct the slot already
/// holds the address; when it resolves to a scalar the slot holds the value,
/// and the impl - which declares a reference - reads it as an address. The
/// declared convention is read off the callee's own body, which states the
/// receiver type it was lowered with.
fn borrow_scalar_receivers_for_ref_methods(
    copy: &mut Body,
    receiver_is_ref: &HashMap<String, bool>,
    tcx: &mut TyCtxt,
) {
    let local_tys: Vec<Ty> = copy.locals.iter().map(|l| l.ty).collect();
    let mut work: Vec<(usize, Local, Ty)> = Vec::new();
    for (block_index, block) in copy.blocks.iter().enumerate() {
        let Terminator::Call { callee, args, .. } = &block.terminator else {
            continue;
        };
        let Operand::Const(ConstValue::Str(name)) = callee else {
            continue;
        };
        if receiver_is_ref.get(name) != Some(&true) {
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
        // Only a receiver still typed as a parameter is settled here. A
        // template serving scalar instantiations carries the value in that
        // slot, and lowering could not have chosen the convention because the
        // concrete type was not yet known. A receiver that already has a
        // concrete type was lowered against the impl it resolves to, and
        // overriding it here would break a convention that already holds.
        if !matches!(tcx.kind_of(recv_ty), TyKind::Param { .. }) {
            continue;
        }
        work.push((block_index, recv.local, recv_ty));
    }
    for (block_index, recv_local, recv_ty) in work {
        let ref_ty = tcx.intern(TyKind::Ref {
            mutability: Mutbl::Not,
            inner: recv_ty,
        });
        let tmp = Local(u32::try_from(copy.locals.len()).expect("local index fits"));
        copy.locals.push(crate::ir::LocalDecl {
            ty: ref_ty,
            debug_name: None,
            mutable: false,
            region: false,
        });
        let span = copy.span;
        let block = &mut copy.blocks[block_index];
        block.stmts.push(crate::ir::Statement {
            kind: StatementKind::Assign {
                place: Place::local(tmp),
                rvalue: Rvalue::Ref {
                    mutable: false,
                    place: Place::local(recv_local),
                },
            },
            span,
        });
        if let Terminator::Call { args, .. } = &mut block.terminator
            && let Some(first) = args.first_mut()
        {
            *first = Operand::Copy(Place::local(tmp));
        }
    }
}

fn substs_are_const_only(substs: &Substs) -> bool {
    // A const-generic array parameter is lowered to a runtime-length sequence,
    // so one body serves every const value and a specialised copy is wasted.
    substs
        .as_slice()
        .iter()
        .all(|arg| matches!(arg, GenericArg::Const(_)))
}

fn subst_type_arguments(substs: &Substs) -> Vec<Option<Ty>> {
    substs
        .as_slice()
        .iter()
        .map(|arg| match arg {
            GenericArg::Type(ty) => Some(*ty),
            GenericArg::Const(_) => None,
        })
        .collect()
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
    method_bases: &HashMap<String, usize>,
    emitted: &mut HashSet<String>,
    trait_specialised_methods: &mut HashSet<String>,
    receiver_is_ref: &HashMap<String, bool>,
    tcx: &mut TyCtxt,
    scan_start: usize,
) -> (bool, usize) {
    if method_bases.is_empty() {
        return (false, bodies.len());
    }
    let mut rewrites: Vec<(usize, usize, String)> = Vec::new();
    let mut to_create: Vec<(usize, Substs, String, String)> = Vec::new();
    let scan_end = bodies.len();
    for (bi, body) in bodies.iter().enumerate().take(scan_end).skip(scan_start) {
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
                to_create.push((base_idx, substs, spec_name, name.clone()));
            }
        }
    }
    // A method may carry its own type parameters on a type that has none -
    // `impl Cmd { fn arg<T: Arg>(self, v: T) }`. The receiver's substs say
    // nothing about `T`, so the instantiation is read off the argument types
    // at the call site, exactly as a generic free function's is.
    for (bi, body) in bodies.iter().enumerate().take(scan_end).skip(scan_start) {
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
            let Some(substs) = method_param_substs(&bodies[base_idx], body, args, tcx) else {
                continue;
            };
            let spec_name = method_mangled_name(name, &substs);
            rewrites.push((bi, blk, spec_name.clone()));
            if emitted.insert(spec_name.clone()) {
                to_create.push((base_idx, substs, spec_name, name.clone()));
            }
        }
    }
    let made = !to_create.is_empty();
    for (base_idx, substs, spec_name, base_name) in to_create {
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
        // A method on a bounded `impl<T: Trait>` block calls the trait method
        // through its type parameter, so the same receiver rewrite a generic
        // free function needs applies here. It runs while the locals still
        // carry the template parameter, which is what identifies the receiver.
        if rewrite_trait_method_calls(&mut copy, &substs, receiver_is_ref, tcx) {
            trait_specialised_methods.insert(base_name);
        }
        reference_aggregate_trait_receivers(&mut copy, &subst_tys, tcx);
        for local in &mut copy.locals {
            local.ty = subst_param_ty(tcx, local.ty, &subst_tys);
        }
        repair_generic_element_reads(&mut copy, tcx);
        borrow_scalar_receivers_for_ref_methods(&mut copy, receiver_is_ref, tcx);
        specialise_call_substs(&mut copy, &subst_tys, tcx);
        bodies.push(copy);
    }
    for (bi, blk, spec_name) in rewrites {
        if let Terminator::Call { callee, .. } = &mut bodies[bi].blocks[blk].terminator {
            *callee = Operand::Const(ConstValue::Str(spec_name));
        }
    }
    (made, scan_end)
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
            TyKind::HashMap { key, value, .. } => {
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
        TyKind::HashMap { key, value, .. } => {
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
/// The instantiation a call site gives a method's own type parameters.
///
/// The template's parameter locals still carry their `Param`s; each is paired
/// with the type the call actually passes, so `cmd.arg(1)` reads `T = i64`.
/// `None` when the method declares no parameters of its own, or when a
/// parameter's instantiation is not concrete at this site.
fn method_param_substs(
    template: &Body,
    caller: &Body,
    args: &[Operand],
    tcx: &TyCtxt,
) -> Option<Substs> {
    let mut resolved: Vec<Option<Ty>> = Vec::new();
    let mut saw_param = false;
    for (index, arg) in args.iter().enumerate() {
        let Some(decl) = template.locals.get(index + 1) else {
            break;
        };
        let TyKind::Param { idx, .. } = tcx.kind_of(peel_ref(tcx, decl.ty)) else {
            continue;
        };
        let param = idx.0 as usize;
        let Operand::Copy(place) = arg else {
            return None;
        };
        let actual = peel_ref(tcx, caller.locals.get(place.local.0 as usize)?.ty);
        if ty_contains_param(tcx, actual) {
            return None;
        }
        saw_param = true;
        if resolved.len() <= param {
            resolved.resize(param + 1, None);
        }
        resolved[param] = Some(actual);
    }
    if !saw_param {
        return None;
    }
    let types: Option<Vec<Ty>> = resolved.into_iter().collect();
    Some(Substs::from_types(types?))
}

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
fn rewrite_trait_method_calls(
    copy: &mut Body,
    substs: &Substs,
    known_methods: &HashMap<String, bool>,
    tcx: &TyCtxt,
) -> bool {
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
        let Some(recv_ty) = place_ty(tcx, &local_tys, recv) else {
            continue;
        };
        let Some(idx) = param_index(tcx, recv_ty) else {
            continue;
        };
        let Some(Some(concrete)) = subst_tys.get(idx) else {
            continue;
        };
        if let Some(cname) = adt_name(tcx, *concrete) {
            let resolved = format!("{cname}::{name}");
            // A primitive's trait surface is mostly builtin - `__debug` and
            // friends have no body of their own - so name one only when the
            // program actually declares it. A declared type keeps resolving
            // by name, which is how its derived methods are reached.
            let primitive_target = !matches!(tcx.kind_of(*concrete), TyKind::Adt { .. });
            if !primitive_target || known_methods.contains_key(&resolved) {
                *callee = Operand::Const(ConstValue::Str(resolved));
                rewrote = true;
            }
        }
    }
    rewrote
}

/// Repoints a trait-method receiver that reached the callee by value at the
/// reference the concrete impl expects.
///
/// A method body written against `T` copies the receiver out of its slot,
/// because a type parameter is one opaque slot to the generic template. The
/// impl it resolves to declares `&self`, so once `T` is known to be an
/// aggregate the copy has to become the address of that place, and the local
/// holding it has to be typed as a reference so the backend keeps a pointer
/// rather than an aggregate. A scalar receiver already travels correctly in
/// its slot and is left alone.
///
/// Runs while the locals still carry their template parameters, which is what
/// identifies the receiver.
fn reference_aggregate_trait_receivers(
    copy: &mut Body,
    subst_tys: &[Option<Ty>],
    tcx: &mut TyCtxt,
) {
    let mut retarget: Vec<(Local, Place, Ty)> = Vec::new();
    for block in &copy.blocks {
        let Terminator::Call { callee, args, .. } = &block.terminator else {
            continue;
        };
        let Operand::Const(ConstValue::Str(name)) = callee else {
            continue;
        };
        if !name.contains("::") {
            continue;
        }
        let Some(Operand::Copy(recv)) = args.first() else {
            continue;
        };
        if !recv.projection.is_empty() {
            continue;
        }
        let Some(decl) = copy.locals.get(recv.local.0 as usize) else {
            continue;
        };
        let TyKind::Param { idx, .. } = tcx.kind_of(decl.ty) else {
            continue;
        };
        let Some(Some(concrete)) = subst_tys.get(idx.0 as usize).copied() else {
            continue;
        };
        if !matches!(tcx.kind_of(concrete), TyKind::Adt { .. } | TyKind::Tuple(_)) {
            continue;
        }
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.local == recv.local
                && place.projection.is_empty()
                && let Rvalue::Use(Operand::Copy(source)) = rvalue
            {
                retarget.push((recv.local, source.clone(), concrete));
            }
        }
    }
    for (local, source, concrete) in retarget {
        let referenced = tcx.intern(TyKind::Ref {
            mutability: Mutbl::Not,
            inner: concrete,
        });
        if let Some(decl) = copy.locals.get_mut(local.0 as usize) {
            decl.ty = referenced;
        }
        for block in &mut copy.blocks {
            for stmt in &mut block.stmts {
                if let StatementKind::Assign { place, rvalue } = &mut stmt.kind
                    && place.local == local
                    && place.projection.is_empty()
                    && matches!(rvalue, Rvalue::Use(Operand::Copy(_)))
                {
                    *rvalue = Rvalue::Ref {
                        mutable: false,
                        place: source.clone(),
                    };
                }
            }
        }
    }
}

/// Type of the value `place` denotes, walking its projection chain from the
/// root local's declared type. A receiver reached through a field - the shape
/// `self.value.method()` produces inside a generic `impl` block - carries its
/// type parameter on the projected field rather than on the local, so the
/// trait-dispatch rewrite has to resolve the whole chain to find it.
/// Returns `None` for any step whose type is not statically resolvable here.
fn place_ty(tcx: &TyCtxt, local_tys: &[Ty], place: &Place) -> Option<Ty> {
    let mut ty = local_tys.get(place.local.0 as usize).copied()?;
    for step in &place.projection {
        ty = match step {
            Projection::Deref => match tcx.kind_of(ty) {
                TyKind::Ref { inner, .. } => *inner,
                _ => return None,
            },
            Projection::Field(index) => {
                let mut base = ty;
                while let TyKind::Ref { inner, .. } = tcx.kind_of(base) {
                    base = *inner;
                }
                match tcx.kind_of(base) {
                    TyKind::Adt { def, substs } => *tcx
                        .adt_field_tys(*def, substs)
                        .and_then(|fields| fields.get(*index as usize))?,
                    TyKind::Tuple(elems) => *elems.get(*index as usize)?,
                    _ => return None,
                }
            }
            Projection::Index(_) | Projection::Downcast(_) | Projection::Discriminant => {
                return None;
            }
        };
    }
    Some(ty)
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
/// Name the impl block for `ty` registers its methods under.
///
/// A trait is implementable for a primitive as much as for a declared type,
/// and such an impl keys its methods by the primitive's spelling. Resolving
/// only ADTs left a trait call on a parameter that turned out to be `i64`
/// pointing at the unqualified trait name, which names no body.
fn adt_name(tcx: &TyCtxt, ty: Ty) -> Option<String> {
    match tcx.kind_of(ty).clone() {
        TyKind::Adt { def, .. } => tcx.def_name(def).map(str::to_string),
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char | TyKind::String => {
            Some(gossamer_types::printer::render_ty(tcx, ty))
        }
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
        TyKind::HashMap {
            key,
            value,
            ordered,
        } => {
            let key = subst_param_ty(tcx, key, subst_tys);
            let value = subst_param_ty(tcx, value, subst_tys);
            tcx.intern(TyKind::HashMap {
                key,
                value,
                ordered,
            })
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
        | TyKind::Iterator(_)
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
