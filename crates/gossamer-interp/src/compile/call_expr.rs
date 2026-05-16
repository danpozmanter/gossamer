#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn compile_path(&mut self, segments: &[Ident]) -> RuntimeResult<Reg> {
        let Some(first) = segments.first() else {
            return Err(RuntimeError::UnresolvedName(String::new()));
        };
        if segments.len() == 1 {
            if let Some(tr) = self.lookup_local(&first.name) {
                return Ok(self.as_value(tr));
            }
            // Top-level `const` items inline through the constant
            // pool (single-index fetch) instead of `LoadGlobal`
            // (string-keyed HashMap lookup). Hot loops that close
            // over module-level consts pay only a register move
            // per access.
            if let Some(value) = self.module_consts.get(first.name.as_str()) {
                let key = const_key_for_value(value);
                let idx = self.const_idx(key, value.clone());
                let dst = self.alloc_reg();
                self.emit(Op::LoadConst { dst, idx });
                return Ok(dst);
            }
        }
        // For multi-segment paths (`fmt::println`,
        // `http::Response::text`, ...) the VM has two builtins to
        // pick between: one registered under the tail name
        // (`text`) and one under the fully-qualified path
        // (`http::Response::text`). Emit a LoadGlobal keyed on the
        // full join — the global table has entries for both, and
        // the qualified key is unambiguous.
        let name = if segments.len() > 1 {
            segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::")
        } else {
            first.name.clone()
        };
        let idx = self.global_idx(&name);
        let dst = self.alloc_reg();
        self.emit(Op::LoadGlobal { dst, idx });
        Ok(dst)
    }
}
