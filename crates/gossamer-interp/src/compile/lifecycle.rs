#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn new(
        name: &'static str,
        tcx: &'tcx TyCtxt,
        layouts: &'tcx StructLayouts,
        wrappers: &'tcx InlinableWrappers,
        inline_fns: &'tcx InlinableFns,
        module_consts: &'tcx ConstValues,
        method_muts: &'tcx MutSelfMethods,
        mut_statics: &'tcx MutStatics,
        cov: Option<&'tcx gossamer_lex::SourceMap>,
    ) -> Self {
        Self {
            name,
            tcx,
            layouts,
            wrappers,
            inline_fns,
            inlining: Vec::new(),
            inlined_nodes: 0,
            module_consts,
            method_muts,
            mut_statics,
            cov,
            flat_locals: std::collections::HashMap::new(),
            flat_int_locals: std::collections::HashSet::new(),
            flat_float_locals: std::collections::HashSet::new(),
            collection_locals: std::collections::HashSet::new(),
            flag_set_locals: std::collections::HashSet::new(),
            duration_cell_locals: std::collections::HashSet::new(),
            instrs: Vec::new(),
            consts: Vec::new(),
            const_cache: HashMap::new(),
            f64_consts: Vec::new(),
            f64_const_cache: HashMap::new(),
            i64_consts: Vec::new(),
            i64_const_cache: HashMap::new(),
            globals: Vec::new(),
            shape_names: Vec::new(),
            global_cache: HashMap::new(),
            next_reg: 0,
            next_float_reg: 0,
            next_int_reg: 0,
            scopes: vec![Scope::default()],
            loop_stack: Vec::new(),
            defer_stack: Vec::new(),
            closure_protos: Vec::new(),
            select_arms: Vec::new(),
            wide_ops: Vec::new(),
            next_cache_idx: 0,
            next_arith_cache_idx: 0,
            next_field_cache_idx: 0,
            mut_ref_params: Vec::new(),
        }
    }

    pub(crate) fn bind_param(&mut self, pattern: &HirPat, reg: Reg) {
        if let HirPatKind::Binding { name, .. } = &pattern.kind {
            self.bind_local(
                &name.name,
                TypedReg {
                    reg,
                    kind: RegKind::Value,
                },
            );
        }
    }

    pub(crate) fn finish(self, arity: u16) -> FnChunk {
        let mut chunk = FnChunk {
            name: self.name,
            arity,
            register_count: self.next_reg,
            float_count: self.next_float_reg,
            int_count: self.next_int_reg,
            instrs: self.instrs,
            consts: self.consts,
            f64_consts: self.f64_consts,
            i64_consts: self.i64_consts,
            globals: self.globals,
            shape_names: self.shape_names,
            closure_protos: self.closure_protos,
            select_arms: self.select_arms,
            wide_ops: self.wide_ops,
            // The actual cache `Vec`s live in per-`Vm`
            // `ChunkState` and are sized from these counts. See
            // `vm::Vm::chunk_state_for`.
            call_cache_count: self.next_cache_idx,
            arith_cache_count: self.next_arith_cache_idx,
            field_cache_count: self.next_field_cache_idx,
            mut_ref_params: self.mut_ref_params,
        };
        // Release growth-by-doubling slack on every Vec field
        // unconditionally — any code path that produces a chunk
        // via `finish` benefits, with no risk of a future caller
        // forgetting an explicit compact() call.
        chunk.compact();
        chunk
    }
}
