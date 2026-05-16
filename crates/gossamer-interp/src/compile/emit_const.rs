#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn f64_const_idx(&mut self, value: f64) -> ConstIdx {
        let key = value.to_bits();
        if let Some(idx) = self.f64_const_cache.get(&key) {
            return *idx;
        }
        let idx = ConstIdx::try_from(self.f64_consts.len()).expect("f64 const pool overflow");
        self.f64_consts.push(value);
        self.f64_const_cache.insert(key, idx);
        idx
    }

    pub(crate) fn i64_const_idx(&mut self, value: i64) -> ConstIdx {
        if let Some(idx) = self.i64_const_cache.get(&value) {
            return *idx;
        }
        let idx = ConstIdx::try_from(self.i64_consts.len()).expect("i64 const pool overflow");
        self.i64_consts.push(value);
        self.i64_const_cache.insert(value, idx);
        idx
    }

    pub(crate) fn emit(&mut self, op: Op) -> InstrIdx {
        let idx = u32::try_from(self.instrs.len()).expect("instruction overflow");
        self.instrs.push(op);
        idx
    }

    pub(crate) fn cur_idx(&self) -> InstrIdx {
        u32::try_from(self.instrs.len()).expect("instruction overflow")
    }

    pub(crate) fn patch_jump(&mut self, idx: InstrIdx, target: InstrIdx) {
        match &mut self.instrs[idx as usize] {
            Op::Jump { target: t }
            | Op::BranchIf { target: t, .. }
            | Op::BranchIfNot { target: t, .. }
            | Op::BranchIfLtI64 { target: t, .. }
            | Op::BranchIfGeI64 { target: t, .. }
            | Op::BranchIfGtI64 { target: t, .. }
            | Op::BranchIfLtF64 { target: t, .. }
            | Op::BranchIfGeF64 { target: t, .. } => *t = target,
            other => panic!("cannot patch non-jump: {other:?}"),
        }
    }

    pub(crate) fn const_idx(&mut self, key: ConstKey, value: Value) -> ConstIdx {
        if let Some(idx) = self.const_cache.get(&key) {
            return *idx;
        }
        let idx = ConstIdx::try_from(self.consts.len()).expect("const pool overflow");
        self.consts.push(value);
        self.const_cache.insert(key, idx);
        idx
    }

    pub(crate) fn global_idx(&mut self, name: &str) -> GlobalIdx {
        if let Some(idx) = self.global_cache.get(name) {
            return *idx;
        }
        let idx = GlobalIdx::try_from(self.globals.len()).expect("global pool overflow");
        self.globals.push(name.to_string());
        self.global_cache.insert(name.to_string(), idx);
        idx
    }
}
