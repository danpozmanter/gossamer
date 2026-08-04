#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use std::collections::HashSet;

use gossamer_hir::{HirParam, collect_free_vars, collect_pattern_names};
use gossamer_types::{FloatTy, IntTy};

use super::*;
use crate::bytecode::{ClosureProto, FnChunk};

impl<'tcx> FnBuilder<'tcx> {
    /// Lowers a closure literal `|params| body` to a native
    /// [`Op::MakeClosure`]. Free variables that resolve to enclosing
    /// locals become captured upvalues (snapshotted into the closure
    /// value at construction); the body compiles to its own
    /// [`FnChunk`] whose leading parameters are those upvalues followed
    /// by the declared parameters. Free variables that resolve to
    /// globals stay unbound here and resolve through `vm.globals` when
    /// the body runs.
    pub(crate) fn compile_closure(
        &mut self,
        params: &[HirParam],
        body: &HirExpr,
    ) -> RuntimeResult<Reg> {
        // Free variables of the body, excluding the parameters it binds.
        let mut bound: HashSet<String> = HashSet::new();
        for param in params {
            collect_pattern_names(&param.pattern, &mut bound);
        }
        let free = collect_free_vars(body, &bound);

        // Capture only free vars that are live enclosing locals; the rest
        // resolve as globals inside the body. Box typed (i64/f64) locals
        // into the Value file first so each upvalue snapshot is a plain
        // `Value` in the closure's name/value capture list.
        let mut capture_regs: Vec<Reg> = Vec::new();
        let mut capture_names: Vec<String> = Vec::new();
        for name in &free {
            if let Some(tr) = self.lookup_local(name) {
                let value_reg = self.as_value(tr);
                capture_regs.push(value_reg);
                capture_names.push(name.clone());
            }
        }

        // The closure body always compiles to its own `FnChunk`: struct
        // `==` lowers natively to a `<Type>::eq` call and `defer` to
        // block-scoped LIFO emission, so every closure body lowers
        // natively.
        let chunk = self
            .build_closure_chunk(&capture_names, params, body)?
            .into_shared();

        let proto = ClosureProto {
            chunk,
            capture_regs,
        };
        let proto_idx = u32::try_from(self.closure_protos.len())
            .map_err(|_| RuntimeError::Unsupported("too many closures in one function"))?;
        self.closure_protos.push(proto);

        let dst = self.alloc_reg();
        self.emit(Op::MakeClosure {
            dst,
            proto: proto_idx,
        });
        Ok(dst)
    }

    /// Compiles a closure body into a standalone [`FnChunk`]. The
    /// leading `capture_names.len()` registers receive the captured
    /// upvalues, followed by one register per declared parameter; the VM
    /// places `capture_values ++ args` into them before running the
    /// chunk.
    fn build_closure_chunk(
        &self,
        capture_names: &[String],
        params: &[HirParam],
        body: &HirExpr,
    ) -> RuntimeResult<FnChunk> {
        let name = crate::value::intern_type_name("__closure");
        let mut b = FnBuilder::new(
            name,
            self.tcx,
            self.layouts,
            self.wrappers,
            self.inline_fns,
            self.fn_param_tys,
            self.module_consts,
            self.method_muts,
            self.mut_statics,
            self.source_map,
            self.cov,
        );
        // The closure body is a distinct function frame; carry the active
        // inline stack into it so a callee mid-inline in the enclosing
        // function is never re-inlined across the closure boundary.
        b.inlining.clone_from(&self.inlining);
        // Captured upvalues occupy the leading registers, bound by name so
        // body references resolve to them.
        for cname in capture_names {
            let reg = b.alloc_reg();
            b.bind_local(
                cname,
                TypedReg {
                    reg,
                    kind: RegKind::Value,
                },
            );
        }
        // Declared parameters follow, mirroring `compile_fn`'s param
        // binding: the `&mut Vec<T>` write-back protocol and the
        // typed-storage fast-path tracking both carry over.
        for param in params {
            let reg = b.alloc_reg();
            b.bind_param(&param.pattern, reg)?;
            if is_mut_ref_writeback(self.tcx, param.ty) {
                b.mut_ref_params.push(reg);
            }
            let elem_kind = b.unwrap_ref(param.ty);
            if let Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) =
                self.tcx.kind(elem_kind)
            {
                match self.tcx.kind(*elem) {
                    Some(TyKind::Float(FloatTy::F64)) => {
                        b.flat_float_locals.insert(reg);
                    }
                    Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize)) => {
                        b.flat_int_locals.insert(reg);
                    }
                    _ => {}
                }
            }
        }
        // A `Block` body mirrors `compile_fn`'s tail handling; a bare
        // expression compiles to a single Value reg returned directly.
        match &body.kind {
            HirExprKind::Block(block) => {
                if let BlockResult::ValueIn(reg) = b.compile_block(block)? {
                    b.emit(Op::Return { value: reg });
                } else {
                    b.emit(Op::ReturnUnit);
                }
            }
            _ => {
                let reg = b.compile_expr(body)?;
                b.emit(Op::Return { value: reg });
            }
        }
        let arity = u16::try_from(capture_names.len() + params.len())
            .map_err(|_| RuntimeError::Unsupported("closure arity exceeds 65535"))?;
        Ok(b.finish(arity))
    }
}
