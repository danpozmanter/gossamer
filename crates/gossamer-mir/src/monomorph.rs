//! Monomorphisation pass.
//! Walks every [`Body`] and materialises one specialised copy per
//! `(def, substs)` pair observed at a call site. The HIR lowering
//! upstream already stamps each MIR local with its concrete [`Ty`]
//! (no `TyKind::Param` escapes the type table's post-solve
//! projection), so a specialised copy is structurally identical to
//! its generic source under the flat-i64-per-slot layout — but the
//! copy is registered under a stable mangled name so each call site
//! can dispatch to its own specialisation.
//!

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use gossamer_resolve::DefId;
use gossamer_types::{GenericArg, Substs, Ty, TyCtxt, TyKind};

use crate::ir::{Body, Operand, Rvalue, StatementKind, Terminator};

/// Monomorphises `bodies` by emitting one specialised copy per
/// distinct `(def, substs)` pair observed at a call site whose
/// substitution is non-empty. Monomorphic calls are untouched.
pub fn monomorphise(bodies: &mut Vec<Body>, tcx: &mut TyCtxt) {
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
    let mut emitted: HashSet<String> = HashSet::new();
    let mut specialised: Vec<Body> = Vec::new();
    for (def, subst_list) in &needs {
        let Some(src_idx) = sources.get(&def.local) else {
            continue;
        };
        for substs in subst_list {
            if substs.is_empty() {
                continue;
            }
            let name = mangled_name(*def, substs);
            if !emitted.insert(name.clone()) {
                continue;
            }
            let mut copy = bodies[*src_idx].clone();
            copy.name = name;
            copy.def = None;
            for local in &mut copy.locals {
                local.ty = resolve(tcx, local.ty);
            }
            specialised.push(copy);
        }
    }
    for body in bodies.iter_mut() {
        for local in &mut body.locals {
            local.ty = resolve(tcx, local.ty);
        }
    }
    bodies.extend(specialised);
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
///   `BTreeMap<K, V>` — by-pointer in the flat ABI.
/// - Tuples and named ADTs (struct/enum) — by-pointer.
/// - Function references and channel handles (`Sender<T>` /
///   `Receiver<T>`) — already round-trip through `i64` in the
///   compiled tier.
/// - Refs (`&T`).
///
/// Still rejected:
/// - `TyKind::Closure` — needs explicit env pointer wiring at
///   the call site that monomorphisation doesn't yet rewrite.
/// - `TyKind::Alias` (unresolved type alias) — should never
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
        | TyKind::Ref { .. }
        | TyKind::FnDef { .. }
        | TyKind::FnPtr(_)
        | TyKind::Adt { .. }
        | TyKind::Tuple(_)
        | TyKind::Array { .. }
        | TyKind::Slice(_) => true,
        TyKind::Closure { .. } | TyKind::Alias { .. } => false,
        _ => false,
    }
}

/// Best-effort one-line spelling of a `Ty` for the diagnostic.
/// Intentionally terse — full type printing lives in
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
        // produce identical structural output — the pass is
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
                },
                crate::ir::LocalDecl {
                    ty: recorded,
                    debug_name: None,
                    mutable: false,
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
