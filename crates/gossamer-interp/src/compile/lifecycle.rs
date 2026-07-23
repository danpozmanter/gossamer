#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn new(
        name: &'static str,
        tcx: &'tcx TyCtxt,
        layouts: &'tcx StructLayouts,
        wrappers: &'tcx InlinableWrappers,
        inline_fns: &'tcx InlinableFns,
        fn_param_tys: &'tcx FnParamTypes,
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
            fn_param_tys,
            inlining: Vec::new(),
            inlined_nodes: 0,
            module_consts,
            method_muts,
            mut_statics,
            cov,
            flat_locals: std::collections::HashMap::new(),
            flat_int_locals: std::collections::HashSet::new(),
            flat_float_locals: std::collections::HashSet::new(),
            uint_display_locals: std::collections::HashSet::new(),
            reference_alias_regs: std::collections::HashSet::new(),
            escaped_reference_reg_floor: 0,
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
            max_reg: 0,
            max_float_reg: 0,
            max_int_reg: 0,
            scopes: vec![Scope::default()],
            loop_stack: Vec::new(),
            pending_loop_label: None,
            defer_stack: Vec::new(),
            closure_protos: Vec::new(),
            select_arms: Vec::new(),
            wide_ops: Vec::new(),
            next_cache_idx: 0,
            next_arith_cache_idx: 0,
            next_field_cache_idx: 0,
            mut_ref_params: Vec::new(),
            consumable: std::collections::HashSet::new(),
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

    pub(crate) fn finish(mut self, arity: u16) -> FnChunk {
        optimize_float_accumulator_moves(&mut self.instrs);
        optimize_i64_to_f64_divs(&mut self.instrs);
        let mut chunk = FnChunk {
            name: self.name,
            arity,
            register_count: self.max_reg.max(self.next_reg),
            float_count: self.max_float_reg.max(self.next_float_reg),
            int_count: self.max_int_reg.max(self.next_int_reg),
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
        // unconditionally - any code path that produces a chunk
        // via `finish` benefits, with no risk of a future caller
        // forgetting an explicit compact() call.
        chunk.compact();
        chunk
    }
}

fn optimize_i64_to_f64_divs(instrs: &mut Vec<Op>) {
    if instrs.len() < 2 {
        return;
    }

    let branch_targets = branch_targets(instrs);
    let mut remove = vec![false; instrs.len()];
    let mut idx = 0usize;
    while idx + 1 < instrs.len() {
        let Op::IntToFloatF64 {
            dst_f: cast_f,
            src_i,
        } = instrs[idx]
        else {
            idx += 1;
            continue;
        };
        let Op::DivF64 {
            dst_f,
            lhs_f,
            rhs_f,
        } = instrs[idx + 1]
        else {
            idx += 1;
            continue;
        };
        if rhs_f != cast_f
            || branch_targets[idx]
            || float_reg_read_before_write(&instrs[idx + 2..], cast_f)
        {
            idx += 1;
            continue;
        }
        instrs[idx + 1] = Op::DivF64ByI64 {
            dst_f,
            lhs_f,
            rhs_i: src_i,
        };
        remove[idx] = true;
        idx += 2;
    }

    if remove.iter().any(|drop| *drop) {
        compact_instrs(instrs, &remove);
    }
}

fn optimize_float_accumulator_moves(instrs: &mut Vec<Op>) {
    if instrs.len() < 2 {
        return;
    }

    let branch_targets = branch_targets(instrs);
    let mut remove = vec![false; instrs.len()];
    let mut idx = 0usize;
    while idx + 1 < instrs.len() {
        let Some((old_dst, a_f, b_f, c_f, is_sub)) = mul_fused_parts(instrs[idx]) else {
            idx += 1;
            continue;
        };
        let Op::MoveF64 { dst_f, src_f } = instrs[idx + 1] else {
            idx += 1;
            continue;
        };
        if src_f != old_dst
            || dst_f == old_dst
            || branch_targets[idx + 1]
            || float_reg_read_before_write(&instrs[idx + 2..], old_dst)
        {
            idx += 1;
            continue;
        }
        instrs[idx] = if is_sub {
            Op::MulSubF64 {
                dst_f,
                a_f,
                b_f,
                c_f,
            }
        } else {
            Op::MulAddF64 {
                dst_f,
                a_f,
                b_f,
                c_f,
            }
        };
        remove[idx + 1] = true;
        idx += 2;
    }

    if remove.iter().any(|drop| *drop) {
        compact_instrs(instrs, &remove);
    }
}

fn mul_fused_parts(op: Op) -> Option<(Reg, Reg, Reg, Reg, bool)> {
    match op {
        Op::MulAddF64 {
            dst_f,
            a_f,
            b_f,
            c_f,
        } => Some((dst_f, a_f, b_f, c_f, false)),
        Op::MulSubF64 {
            dst_f,
            a_f,
            b_f,
            c_f,
        } => Some((dst_f, a_f, b_f, c_f, true)),
        _ => None,
    }
}

fn float_reg_read_before_write(instrs: &[Op], reg: Reg) -> bool {
    for op in instrs {
        if op_reads_float(*op, reg) {
            return true;
        }
        if op_writes_float(*op, reg) {
            return false;
        }
    }
    false
}

fn op_reads_float(op: Op, reg: Reg) -> bool {
    match op {
        Op::AddF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::SubF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::MulF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::DivF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::LtF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::LeF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::GtF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::GeF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::EqF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::NeF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::BranchIfLtF64 {
            lhs_f: a, rhs_f: b, ..
        }
        | Op::BranchIfGeF64 {
            lhs_f: a, rhs_f: b, ..
        } => a == reg || b == reg,
        Op::DivF64ByI64 { lhs_f, .. } => lhs_f == reg,
        Op::NegF64 { src_f, .. }
        | Op::BoxF64 { src_f, .. }
        | Op::SqrtF64 { src_f, .. }
        | Op::SinF64 { src_f, .. }
        | Op::CosF64 { src_f, .. }
        | Op::AbsF64 { src_f, .. }
        | Op::FloorF64 { src_f, .. }
        | Op::CeilF64 { src_f, .. }
        | Op::ExpF64 { src_f, .. }
        | Op::LnF64 { src_f, .. }
        | Op::FloatToIntI64 { src_f, .. }
        | Op::MoveF64 { src_f, .. } => src_f == reg,
        Op::MulAddF64 { a_f, b_f, c_f, .. } | Op::MulSubF64 { a_f, b_f, c_f, .. } => {
            a_f == reg || b_f == reg || c_f == reg
        }
        Op::FloatVecSetF64 { value_f, .. }
        | Op::FlatSetF64 { value_f, .. }
        | Op::FlatSetF64I { value_f, .. }
        | Op::IndexedFieldSetF64 { value_f, .. }
        | Op::IndexedFieldSetF64ByOffset { value_f, .. } => value_f == reg,
        _ => false,
    }
}

fn op_writes_float(op: Op, reg: Reg) -> bool {
    match op {
        Op::LoadConstF64 { dst_f, .. }
        | Op::AddF64 { dst_f, .. }
        | Op::SubF64 { dst_f, .. }
        | Op::MulF64 { dst_f, .. }
        | Op::DivF64 { dst_f, .. }
        | Op::DivF64ByI64 { dst_f, .. }
        | Op::NegF64 { dst_f, .. }
        | Op::UnboxF64 { dst_f, .. }
        | Op::SqrtF64 { dst_f, .. }
        | Op::SinF64 { dst_f, .. }
        | Op::CosF64 { dst_f, .. }
        | Op::AbsF64 { dst_f, .. }
        | Op::FloorF64 { dst_f, .. }
        | Op::CeilF64 { dst_f, .. }
        | Op::ExpF64 { dst_f, .. }
        | Op::LnF64 { dst_f, .. }
        | Op::MulAddF64 { dst_f, .. }
        | Op::MulSubF64 { dst_f, .. }
        | Op::MoveF64 { dst_f, .. }
        | Op::FieldGetF64 { dst_f, .. }
        | Op::IndexedFieldGetF64 { dst_f, .. }
        | Op::IndexedFieldGetF64ByOffset { dst_f, .. }
        | Op::FieldGetF64ByOffset { dst_f, .. }
        | Op::FlatGetF64 { dst_f, .. }
        | Op::FlatGetF64I { dst_f, .. }
        | Op::IntToFloatF64 { dst_f, .. }
        | Op::FloatVecGetF64 { dst_f, .. } => dst_f == reg,
        _ => false,
    }
}

fn branch_targets(instrs: &[Op]) -> Vec<bool> {
    let mut targets = vec![false; instrs.len()];
    for op in instrs {
        if let Some(target) = op_target(*op)
            && let Some(slot) = targets.get_mut(target as usize)
        {
            *slot = true;
        }
    }
    targets
}

fn compact_instrs(instrs: &mut Vec<Op>, remove: &[bool]) {
    let mut remap = vec![0u32; instrs.len() + 1];
    let mut next = 0u32;
    for (idx, drop) in remove.iter().copied().enumerate() {
        remap[idx] = next;
        if !drop {
            next += 1;
        }
    }
    remap[instrs.len()] = next;

    let mut compacted = Vec::with_capacity(next as usize);
    for (idx, op) in instrs.iter().copied().enumerate() {
        if !remove[idx] {
            compacted.push(remap_op_target(op, &remap));
        }
    }
    *instrs = compacted;
}

fn op_target(op: Op) -> Option<InstrIdx> {
    match op {
        Op::Jump { target }
        | Op::BranchIf { target, .. }
        | Op::BranchIfNot { target, .. }
        | Op::BranchIfLtI64 { target, .. }
        | Op::BranchIfGeI64 { target, .. }
        | Op::BranchIfGtI64 { target, .. }
        | Op::BranchIfLtF64 { target, .. }
        | Op::BranchIfGeF64 { target, .. }
        | Op::IncJumpIfLtI64 { target, .. }
        | Op::IncJumpIfLeI64 { target, .. } => Some(target),
        _ => None,
    }
}

fn remap_op_target(mut op: Op, remap: &[InstrIdx]) -> Op {
    let target = match &mut op {
        Op::Jump { target }
        | Op::BranchIf { target, .. }
        | Op::BranchIfNot { target, .. }
        | Op::BranchIfLtI64 { target, .. }
        | Op::BranchIfGeI64 { target, .. }
        | Op::BranchIfGtI64 { target, .. }
        | Op::BranchIfLtF64 { target, .. }
        | Op::BranchIfGeF64 { target, .. }
        | Op::IncJumpIfLtI64 { target, .. }
        | Op::IncJumpIfLeI64 { target, .. } => target,
        _ => return op,
    };
    *target = remap[*target as usize];
    op
}
