#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
#![forbid(unsafe_code)]
use super::*;
use std::collections::HashMap;
use std::fmt::Write as _;

use crate::BuildError;
use anyhow::Result;
use gossamer_abi as abi;
use gossamer_mir::{
    BasicBlock, BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, UnOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};

impl<'a> Lowerer<'a> {
    pub(crate) fn lower_unary(
        &mut self,
        op: UnOp,
        operand: &Operand,
        dest_local: Local,
    ) -> Result<String, BuildError> {
        let operand_v = self.lower_operand(operand)?;
        let dest_ty = self.body.local_ty(dest_local);
        let kind = numeric_kind(self.tcx, dest_ty);
        let tmp = self.fresh();
        match (op, kind) {
            (UnOp::Neg, NumericKind::Int(_)) => {
                writeln!(self.out, "  {tmp} = sub i64 0, {operand_v}").unwrap();
            }
            (UnOp::Neg, NumericKind::Float(_)) => {
                // Both f32 and f64 are represented as `double` at runtime.
                writeln!(self.out, "  {tmp} = fneg double {operand_v}").unwrap();
            }
            (UnOp::Not, _) => {
                // `Not` is bitwise on integers, logical on bool.
                // Both map to `xor` with an all-ones mask for the
                // operand's width - `-1` covers both `i1` and
                // wider integer types. The destination's MIR type
                // may be `Var`/`ptr` (typechecker left it
                // unresolved); use the operand's actual LLVM type
                // so `xor i1 %v, -1` is generated for bools rather
                // than the invalid `xor ptr`.
                let operand_llvm = self.operand_llvm_ty(operand);
                let ty = if operand_llvm == "ptr" || operand_llvm.is_empty() {
                    render_ty(self.tcx, dest_ty)
                } else {
                    operand_llvm
                };
                writeln!(self.out, "  {tmp} = xor {ty} {operand_v}, -1").unwrap();
            }
            _ => {
                return Err(BuildError::Unsupported("unary op on non-numeric type"));
            }
        }
        Ok(tmp)
    }

    pub(crate) fn lower_binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        dest_local: Local,
    ) -> Result<String, BuildError> {
        // String comparisons must use `gos_rt_str_compare` - pointer
        // equality on C strings is address comparison, not content.
        let operand_ty_raw = self.operand_ty(lhs);
        let is_str_cmp = matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        ) && {
            // For Ref types, check that the inner type is String.
            if let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(operand_ty_raw) {
                matches!(self.tcx.kind(*inner), Some(TyKind::String))
            } else {
                matches!(self.tcx.kind(operand_ty_raw), Some(TyKind::String))
            }
        };
        if is_str_cmp {
            let lhs_v = self.lower_operand(lhs)?;
            let rhs_v = self.lower_operand(rhs)?;
            declare_rt(&mut self.runtime_refs, "gos_rt_str_compare");
            let cmp_tmp = self.fresh();
            writeln!(
                self.out,
                "  {cmp_tmp} = call i32 @gos_rt_str_compare(ptr {lhs_v}, ptr {rhs_v})"
            )
            .unwrap();
            let pred = match op {
                BinOp::Eq => "eq",
                BinOp::Ne => "ne",
                BinOp::Lt => "slt",
                BinOp::Le => "sle",
                BinOp::Gt => "sgt",
                BinOp::Ge => "sge",
                _ => unreachable!(),
            };
            let bool_tmp = self.fresh();
            writeln!(self.out, "  {bool_tmp} = icmp {pred} i32 {cmp_tmp}, 0").unwrap();
            let dest_ty = self.body.local_ty(dest_local);
            let dest_llvm = render_ty(self.tcx, dest_ty);
            if dest_llvm == "i1" {
                return Ok(bool_tmp);
            }
            let widened = self.fresh();
            writeln!(self.out, "  {widened} = zext i1 {bool_tmp} to {dest_llvm}").unwrap();
            return Ok(widened);
        }
        let mut lhs_v = self.lower_operand(lhs)?;
        let mut rhs_v = self.lower_operand(rhs)?;
        // Comparisons return `i1`; everything else returns the
        // operands' shared type. Pick the operand type off
        // either side - both are the same kind by MIR
        // invariant.
        let operand_ty = self.operand_ty(lhs);
        let mut kind = numeric_kind(self.tcx, operand_ty);
        let mut operand_llvm = render_ty(self.tcx, operand_ty);
        // Width mismatch correction: when one operand is the
        // narrower `i1` form (most often a `Copy(bool_place)`) but
        // `operand_ty(lhs)` resolved to `i64` (typical when the
        // other operand is a Const fall-through), widen the i1
        // side to match. Without this fix, `and i64 1, %i1_val`
        // hits opt's "operand type mismatch" verifier.
        let lhs_llvm = self.operand_llvm_ty(lhs);
        let rhs_llvm = self.operand_llvm_ty(rhs);
        // `char` is the only type that renders as `i32` here, but
        // `operand_ty` can fail to classify a bare char constant when
        // the body has no char local to borrow, leaving `operand_llvm`
        // as the unit return type (`void`). The operand LLVM types are
        // authoritative: an `i32` operand is a char, so reclassify the
        // operation as an `i32`-width `Other` so a char comparison
        // emits `icmp ... i32` instead of the invalid `icmp ... void`.
        if operand_llvm != "i32" && (lhs_llvm == "i32" || rhs_llvm == "i32") {
            operand_llvm = "i32".to_string();
            kind = NumericKind::Other;
        }
        // A bare float constant is carried as an f64 bit pattern and
        // renders as `double`; `operand_ty` cannot classify it (and
        // yields the unit return type when no float local exists to
        // borrow), so a float operation on two constants reaches here as
        // `Other` / `void`. Reclassify as a float op at the operand's
        // LLVM width - which `rvalue_llvm_ty` and the store path also
        // key off the lhs for - and coerce the operands so a mixed
        // `float` / `double` pair (an f32 place next to an f64-bit
        // constant) shares the operation's type via fptrunc / fpext.
        let is_float_llvm = |t: &str| t == "float" || t == "double";
        if is_float_llvm(&lhs_llvm) || is_float_llvm(&rhs_llvm) {
            if matches!(kind, NumericKind::Other) {
                operand_llvm = if is_float_llvm(&lhs_llvm) {
                    lhs_llvm.clone()
                } else {
                    rhs_llvm.clone()
                };
                kind = NumericKind::Float(if operand_llvm == "float" {
                    FloatTy::F32
                } else {
                    FloatTy::F64
                });
            }
            if matches!(kind, NumericKind::Float(_)) {
                if is_float_llvm(&lhs_llvm) {
                    lhs_v = self.coerce_llvm_value(&lhs_v, &lhs_llvm, &operand_llvm);
                }
                if is_float_llvm(&rhs_llvm) {
                    rhs_v = self.coerce_llvm_value(&rhs_v, &rhs_llvm, &operand_llvm);
                }
            }
        }
        if operand_llvm == "i64" {
            if lhs_llvm == "i1" {
                let zlhs = self.fresh();
                writeln!(self.out, "  {zlhs} = zext i1 {lhs_v} to i64").unwrap();
                lhs_v = zlhs;
            }
            if rhs_llvm == "i1" {
                let zrhs = self.fresh();
                writeln!(self.out, "  {zrhs} = zext i1 {rhs_v} to i64").unwrap();
                rhs_v = zrhs;
            }
            // LLVM 18 rejects mixing ptr and integer operands in any
            // instruction. When a local was typed as ptr (e.g. an enum
            // discriminant stored via inttoptr) but the MIR operand type
            // is i64, convert the ptr operand before the operation.
            if lhs_llvm == "ptr" {
                let plhs = self.fresh();
                writeln!(self.out, "  {plhs} = ptrtoint ptr {lhs_v} to i64").unwrap();
                lhs_v = plhs;
            }
            if rhs_llvm == "ptr" {
                let prhs = self.fresh();
                writeln!(self.out, "  {prhs} = ptrtoint ptr {rhs_v} to i64").unwrap();
                rhs_v = prhs;
            }
        }
        // When the MIR operand type is `ptr` (e.g. a closure parameter
        // whose type was not pinned to a concrete integer during inference),
        // the LLVM locals hold integer bit-patterns stored via inttoptr.
        // Arithmetic on such values requires ptrtoint → integer op →
        // inttoptr back so the result slot (also ptr-typed) stays coherent.
        let mut result_needs_inttoptr = false;
        if operand_llvm == "ptr"
            && matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Rem
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor
                    | BinOp::Shl
                    | BinOp::Shr
            )
        {
            if lhs_llvm == "ptr" {
                let plhs = self.fresh();
                writeln!(self.out, "  {plhs} = ptrtoint ptr {lhs_v} to i64").unwrap();
                lhs_v = plhs;
            } else if lhs_llvm == "double" || lhs_llvm == "float" {
                let blhs = self.fresh();
                writeln!(self.out, "  {blhs} = bitcast {lhs_llvm} {lhs_v} to i64").unwrap();
                lhs_v = blhs;
            }
            if rhs_llvm == "ptr" {
                let prhs = self.fresh();
                writeln!(self.out, "  {prhs} = ptrtoint ptr {rhs_v} to i64").unwrap();
                rhs_v = prhs;
            } else if rhs_llvm == "double" || rhs_llvm == "float" {
                let brhs = self.fresh();
                writeln!(self.out, "  {brhs} = bitcast {rhs_llvm} {rhs_v} to i64").unwrap();
                rhs_v = brhs;
            }
            operand_llvm = "i64".to_string();
            kind = NumericKind::Int(gossamer_types::IntTy::I64);
            result_needs_inttoptr = true;
        }
        // Similarly, ptr-typed comparison operands need ptrtoint.
        if operand_llvm == "ptr"
            && matches!(
                op,
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
            )
        {
            if lhs_llvm == "ptr" {
                let plhs = self.fresh();
                writeln!(self.out, "  {plhs} = ptrtoint ptr {lhs_v} to i64").unwrap();
                lhs_v = plhs;
            } else if lhs_llvm == "double" || lhs_llvm == "float" {
                let blhs = self.fresh();
                writeln!(self.out, "  {blhs} = bitcast {lhs_llvm} {lhs_v} to i64").unwrap();
                lhs_v = blhs;
            }
            if rhs_llvm == "ptr" {
                let prhs = self.fresh();
                writeln!(self.out, "  {prhs} = ptrtoint ptr {rhs_v} to i64").unwrap();
                rhs_v = prhs;
            } else if rhs_llvm == "double" || rhs_llvm == "float" {
                let brhs = self.fresh();
                writeln!(self.out, "  {brhs} = bitcast {rhs_llvm} {rhs_v} to i64").unwrap();
                rhs_v = brhs;
            }
            operand_llvm = "i64".to_string();
            kind = NumericKind::Int(gossamer_types::IntTy::I64);
        }
        // The MIR lowering of `||` / `&&` evaluates both
        // operands eagerly and folds into `Add` / similar
        // arithmetic on `i1`, with a `SwitchInt(0, false_arm)`
        // in the default branch acting as the boolean reduce.
        // On `i1`, `1 + 1` wraps to `0` and breaks the
        // semantics. Widen both operands to `i64` for any
        // non-bitwise / non-comparison arith on `i1` so the
        // Cranelift backend's `i8`-style "extend then add"
        // shape is preserved.
        if operand_llvm == "i1"
            && matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
            )
        {
            let zlhs = self.fresh();
            writeln!(self.out, "  {zlhs} = zext i1 {lhs_v} to i64").unwrap();
            let zrhs = self.fresh();
            writeln!(self.out, "  {zrhs} = zext i1 {rhs_v} to i64").unwrap();
            lhs_v = zlhs;
            rhs_v = zrhs;
            operand_llvm = "i64".to_string();
            kind = NumericKind::Int(gossamer_types::IntTy::I64);
        }
        // `Div` / `Rem` keep the signed-i64 runtime model for every
        // ≤64-bit type (the VM uses `wrapping_div` / `wrapping_rem` on
        // `i64`), so the declared signedness only selects `udiv`/`urem`
        // for the 128-bit types.
        let op_signed = |i: gossamer_types::IntTy| int_width(i) <= 64 || int_signed(i);
        // `< <= > >=` and `>>` use unsigned instructions (`icmp u*` /
        // `lshr`) only for the unsigned families that can exceed
        // `i64::MAX` - `u64` / `usize` / `u128`; every other ≤64-bit type
        // (including `u8`/`u16`/`u32`, which mask below 2^63) keeps the
        // signed `icmp s*` / `ashr`. This matches the VM, which routes
        // only `u64`/`usize` operands through its unsigned compare/shift
        // opcodes. A constant operand carries no signedness, so derive it
        // from the place operand when one side is constant; two constants
        // default to signed.
        let cmp_shift_signed = {
            let pick = |o: &Operand| -> Option<gossamer_types::IntTy> {
                if let Operand::Copy(_) = o
                    && let NumericKind::Int(i) = numeric_kind(self.tcx, self.operand_ty(o))
                {
                    return Some(i);
                }
                None
            };
            match pick(lhs).or_else(|| pick(rhs)) {
                Some(i) => int_signed(i) || int_width(i) < 64,
                // Both operands are constants (a const-folded `u64`, e.g.
                // `a >> 1` where `a` folded to a literal, carries no operand
                // signedness). Fall back to the operation's own int type so a
                // `u64`/`usize` still compares and shifts unsigned per its
                // declared type instead of defaulting to signed.
                None => match kind {
                    NumericKind::Int(i) => int_signed(i) || int_width(i) < 64,
                    _ => true,
                },
            }
        };
        // LLVM `shl`/`ashr` produce poison for shift amounts >= the
        // operand width; the VM masks the amount with `& 63`. Mask
        // i64 shift amounts the same way so `1 << 70` is `1 << 6`
        // on every tier.
        if matches!(op, BinOp::Shl | BinOp::Shr) && operand_llvm == "i64" {
            let masked = self.fresh();
            writeln!(self.out, "  {masked} = and i64 {rhs_v}, 63").unwrap();
            rhs_v = masked;
        }
        let tmp = self.fresh();
        let instr = match (op, kind) {
            (BinOp::Add, NumericKind::Int(_)) => format!("add {operand_llvm}"),
            (BinOp::Sub, NumericKind::Int(_)) => format!("sub {operand_llvm}"),
            (BinOp::Mul, NumericKind::Int(_)) => format!("mul {operand_llvm}"),
            (BinOp::Div, NumericKind::Int(i)) => {
                if op_signed(i) {
                    format!("sdiv {operand_llvm}")
                } else {
                    format!("udiv {operand_llvm}")
                }
            }
            (BinOp::Rem, NumericKind::Int(i)) => {
                if op_signed(i) {
                    format!("srem {operand_llvm}")
                } else {
                    format!("urem {operand_llvm}")
                }
            }
            (BinOp::BitAnd, _) => format!("and {operand_llvm}"),
            (BinOp::BitOr, _) => format!("or {operand_llvm}"),
            (BinOp::BitXor, _) => format!("xor {operand_llvm}"),
            (BinOp::Shl, _) => format!("shl {operand_llvm}"),
            (BinOp::Shr, NumericKind::Int(_)) => {
                if cmp_shift_signed {
                    format!("ashr {operand_llvm}")
                } else {
                    format!("lshr {operand_llvm}")
                }
            }
            (BinOp::Add, NumericKind::Float(_)) => format!("fadd {operand_llvm}"),
            (BinOp::Sub, NumericKind::Float(_)) => format!("fsub {operand_llvm}"),
            (BinOp::Mul, NumericKind::Float(_)) => format!("fmul {operand_llvm}"),
            (BinOp::Div, NumericKind::Float(_)) => format!("fdiv {operand_llvm}"),
            (BinOp::Rem, NumericKind::Float(_)) => format!("frem {operand_llvm}"),
            (cmp, NumericKind::Int(_)) if is_cmp(cmp) => {
                let pred = int_cmp_pred(cmp, cmp_shift_signed);
                format!("icmp {pred} {operand_llvm}")
            }
            (cmp, NumericKind::Float(_)) if is_cmp(cmp) => {
                let pred = float_cmp_pred(cmp);
                format!("fcmp {pred} {operand_llvm}")
            }
            (cmp, NumericKind::Other) if is_cmp(cmp) && operand_llvm == "i32" => {
                // `char` comparison: compare codepoints as `i32`. Valid
                // scalar values (0..=0x10FFFF) are positive, so the signed
                // predicates give the natural codepoint ordering, and
                // equality is exact.
                let pred = int_cmp_pred(cmp, true);
                format!("icmp {pred} {operand_llvm}")
            }
            (cmp, _) if matches!(cmp, BinOp::Eq | BinOp::Ne) => {
                let pred = if matches!(cmp, BinOp::Eq) { "eq" } else { "ne" };
                if operand_llvm == "ptr" {
                    // LLVM 18 rejects `icmp eq ptr %t, 0`. When comparing
                    // enum discriminants (stored as tagged pointers via
                    // inttoptr) against integer constants or other ptr
                    // operands, convert pointer operands to i64 first.
                    let lhs_int = self.fresh();
                    writeln!(self.out, "  {lhs_int} = ptrtoint ptr {lhs_v} to i64").unwrap();
                    lhs_v = lhs_int;
                    if rhs_llvm == "ptr" {
                        let rhs_int = self.fresh();
                        writeln!(self.out, "  {rhs_int} = ptrtoint ptr {rhs_v} to i64").unwrap();
                        rhs_v = rhs_int;
                    }
                    operand_llvm = "i64".to_string();
                    format!("icmp {pred} i64")
                } else {
                    // Equality on non-numeric types (bool, char) uses icmp.
                    format!("icmp {pred} {operand_llvm}")
                }
            }
            _ => {
                if std::env::var("GOS_LLVM_TRACE").is_ok() {
                    eprintln!(
                        "llvm backend: binop fallback: op={op:?} kind={kind:?} \
                         operand_ty={operand_llvm}"
                    );
                }
                return Err(BuildError::Unsupported(
                    "binary op / operand-type combination",
                ));
            }
        };
        writeln!(self.out, "  {tmp} = {instr} {lhs_v}, {rhs_v}").unwrap();
        // Coerce the result back to the destination type.
        //
        // * Comparison ops (Eq/Ne/Lt/Le/Gt/Ge): result is
        //   always `i1`. If the destination is wider, `zext`
        //   to its width.
        // * Arithmetic ops on `i1`-widened operands (the
        //   `&&` / `||` shape): result is `i64`. If the
        //   destination is `i1`, narrow via `icmp ne 0`.
        // Other (operand_llvm == dest_llvm): no coercion.
        let dest_ty = self.body.local_ty(dest_local);
        let dest_llvm = render_ty(self.tcx, dest_ty);
        let is_cmp = matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        );
        if is_cmp {
            // Result is `i1`.
            if dest_llvm == "i1" {
                return Ok(tmp);
            }
            let widened = self.fresh();
            writeln!(self.out, "  {widened} = zext i1 {tmp} to {dest_llvm}").unwrap();
            return Ok(widened);
        }
        // Arithmetic - result type matches `operand_llvm`.
        if operand_llvm == "i64" && dest_llvm == "i1" {
            let narrowed = self.fresh();
            writeln!(self.out, "  {narrowed} = icmp ne i64 {tmp}, 0").unwrap();
            return Ok(narrowed);
        }
        // When the operands were ptr-typed in MIR but we treated them as
        // integer by ptrtoint-ing, the i64 result must be converted back to
        // ptr to match the dest slot's expected type.
        if result_needs_inttoptr && dest_llvm == "ptr" {
            let p = self.fresh();
            writeln!(self.out, "  {p} = inttoptr i64 {tmp} to ptr").unwrap();
            return Ok(p);
        }
        Ok(tmp)
    }

    pub(crate) fn lower_cast(
        &mut self,
        operand: &Operand,
        target: gossamer_types::Ty,
        _dest_local: Local,
    ) -> Result<String, BuildError> {
        let src_v = self.lower_operand(operand)?;
        // Numeric constants classify directly: `operand_ty` resolves
        // a const's type by borrowing a same-kinded local's Ty, and
        // a body with no integer local would misclassify an Int
        // const as the unit return type (non-numeric).
        let (src_kind, src_llvm) = match operand {
            Operand::Const(ConstValue::Int(_)) => (
                NumericKind::Int(gossamer_types::IntTy::I64),
                "i64".to_string(),
            ),
            Operand::Const(ConstValue::Float(_)) => {
                (NumericKind::Float(FloatTy::F64), "double".to_string())
            }
            // Bool / char consts classify directly for the same
            // reason as Int / Float above - `true as i64` in a body
            // with no bool local must not misclassify.
            Operand::Const(ConstValue::Bool(_)) => (NumericKind::Other, "i1".to_string()),
            Operand::Const(ConstValue::Char(_)) => (NumericKind::Other, "i32".to_string()),
            _ => {
                let src_ty = self.operand_ty(operand);
                (numeric_kind(self.tcx, src_ty), render_ty(self.tcx, src_ty))
            }
        };
        let dst_kind = numeric_kind(self.tcx, target);
        let dst_llvm = render_ty(self.tcx, target);
        // Cast to `f32`: although f32 is represented as `double`, an
        // explicit `as f32` must round the value to f32 precision (the VM
        // rounds: `0.1 as f32` is `0.100000001…`). Bring the source to a
        // double, then round-trip through `float` (fptrunc + fpext),
        // leaving the result as the double the rest of the pipeline
        // expects. A cast to `f64` from a double-represented source needs
        // no rounding and falls through to the generic paths below.
        if matches!(dst_kind, NumericKind::Float(FloatTy::F32)) {
            let as_double = match src_kind {
                NumericKind::Float(_) => self.coerce_llvm_value(&src_v, &src_llvm, "double"),
                NumericKind::Int(i) => {
                    let op = if int_signed(i) || int_width(i) <= 64 {
                        "sitofp"
                    } else {
                        "uitofp"
                    };
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = {op} {src_llvm} {src_v} to double").unwrap();
                    tmp
                }
                NumericKind::Other => {
                    let wide = self.fresh();
                    writeln!(self.out, "  {wide} = zext {src_llvm} {src_v} to i64").unwrap();
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = sitofp i64 {wide} to double").unwrap();
                    tmp
                }
            };
            let narrowed = self.fresh();
            writeln!(
                self.out,
                "  {narrowed} = fptrunc double {as_double} to float"
            )
            .unwrap();
            let widened = self.fresh();
            writeln!(self.out, "  {widened} = fpext float {narrowed} to double").unwrap();
            return Ok(widened);
        }
        // Int → narrow int under the i64 runtime model: both sides
        // render as i64, but the cast is the language's single
        // masking point (VM parity: `300 as u8` == 44, `200 as i8`
        // == -56). Truncate to the declared width and extend back
        // by the target's signedness.
        if let (NumericKind::Int(_), NumericKind::Int(b)) = (src_kind, dst_kind)
            && src_llvm == "i64"
            && dst_llvm == "i64"
            && int_width(b) < 64
        {
            return Ok(self.mask_to_int_width(&src_v, b));
        }
        // Float → int is saturating at i64 width with no narrow
        // mask, on every tier: the VM and Cranelift (`fcvt_to_
        // sint_sat`) both produce `300.7 as u8 == 300`, `-1.5 as
        // u8 == -1`, `1e20 as i64 == i64::MAX`. A plain `fptosi`
        // is poison for out-of-range inputs, which `opt -O3`
        // folds into garbage.
        if let (NumericKind::Float(f), NumericKind::Int(_)) = (src_kind, dst_kind)
            && dst_llvm == "i64"
        {
            let src_float = match f {
                FloatTy::F32 => "float",
                FloatTy::F64 => "double",
            };
            let intrinsic = match f {
                FloatTy::F32 => "llvm.fptosi.sat.i64.f32",
                FloatTy::F64 => "llvm.fptosi.sat.i64.f64",
            };
            self.runtime_refs
                .insert(format!("declare i64 @{intrinsic}({src_float})"));
            let v = if src_llvm == src_float {
                src_v
            } else {
                self.coerce_llvm_value(&src_v, &src_llvm, src_float)
            };
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = call i64 @{intrinsic}({src_float} {v})").unwrap();
            return Ok(tmp);
        }
        if src_llvm == dst_llvm {
            return Ok(src_v);
        }
        // `bool` / `char` → integer and `u8` → `char` complete the
        // GT0005 whitelist on this tier (bool = i1, char = i32, every
        // ≤64-bit int = i64 at runtime). The source zero-extends to
        // the 64-bit runtime value; a narrow declared target then
        // masks like any int → int cast. `u8 as char` masks to the
        // declared u8 width before narrowing into the char's i32
        // code-point slot - matching the VM's `cast_scalar`.
        if let (NumericKind::Other, NumericKind::Int(b)) = (src_kind, dst_kind)
            && (src_llvm == "i1" || src_llvm == "i32")
        {
            let wide = self.fresh();
            writeln!(self.out, "  {wide} = zext {src_llvm} {src_v} to i64").unwrap();
            if int_width(b) < 64 {
                return Ok(self.mask_to_int_width(&wide, b));
            }
            return Ok(wide);
        }
        if let (NumericKind::Int(_), NumericKind::Other) = (src_kind, dst_kind)
            && dst_llvm == "i32"
        {
            let masked = self.fresh();
            writeln!(self.out, "  {masked} = and i64 {src_v}, 255").unwrap();
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = trunc i64 {masked} to i32").unwrap();
            return Ok(tmp);
        }
        let tmp = self.fresh();
        let instr = match (src_kind, dst_kind) {
            (NumericKind::Int(a), NumericKind::Int(b)) => {
                let aw = int_width(a);
                let bw = int_width(b);
                if bw == aw {
                    // Same width, different signedness → bitcast.
                    format!("bitcast {src_llvm} {src_v} to {dst_llvm}")
                } else if bw < aw {
                    format!("trunc {src_llvm} {src_v} to {dst_llvm}")
                } else if int_signed(a) {
                    format!("sext {src_llvm} {src_v} to {dst_llvm}")
                } else {
                    format!("zext {src_llvm} {src_v} to {dst_llvm}")
                }
            }
            (NumericKind::Int(i), NumericKind::Float(_)) => {
                // Every ≤64-bit int (u64/usize included) lives as a
                // signed i64 value at runtime, so the conversion is
                // signed (VM parity: `(0u64 - 1) as f64 == -1.0`).
                // Only u128 takes the unsigned conversion.
                if int_signed(i) || int_width(i) <= 64 {
                    format!("sitofp {src_llvm} {src_v} to {dst_llvm}")
                } else {
                    format!("uitofp {src_llvm} {src_v} to {dst_llvm}")
                }
            }
            (NumericKind::Float(_), NumericKind::Int(i)) => {
                if int_signed(i) {
                    format!("fptosi {src_llvm} {src_v} to {dst_llvm}")
                } else {
                    format!("fptoui {src_llvm} {src_v} to {dst_llvm}")
                }
            }
            (NumericKind::Float(FloatTy::F32), NumericKind::Float(FloatTy::F64)) => {
                format!("fpext {src_llvm} {src_v} to {dst_llvm}")
            }
            (NumericKind::Float(FloatTy::F64), NumericKind::Float(FloatTy::F32)) => {
                format!("fptrunc {src_llvm} {src_v} to {dst_llvm}")
            }
            _ => {
                return Err(BuildError::Unsupported("cast between non-numeric types"));
            }
        };
        writeln!(self.out, "  {tmp} = {instr}").unwrap();
        Ok(tmp)
    }

    /// Lowers a `__concat(...)` call by appending each arg to
    /// the runtime's thread-local concat buffer, then storing
    /// the finished string pointer in `destination`. Mirrors the
    /// Cranelift backend so `format!(...)` produces a real value
    /// the caller can store / return; the previous inline-print
    /// shortcut printed pieces eagerly and reordered output
    /// whenever a `format!` result outlived its emission point.
    pub(crate) fn lower_concat_call(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "__concat destination cannot have projections",
            ));
        }
        for sym in [
            "gos_rt_concat_init",
            "gos_rt_concat_str",
            "gos_rt_concat_i64",
            "gos_rt_concat_u64",
            "gos_rt_concat_f64",
            "gos_rt_concat_bool",
            "gos_rt_concat_char",
            "gos_rt_concat_finish",
        ] {
            declare_rt(&mut self.runtime_refs, sym);
        }
        writeln!(self.out, "  call void @gos_rt_concat_init()").unwrap();
        for arg in args {
            let kind = self.concat_print_kind(arg);
            if matches!(kind, ConcatKind::Unsupported) {
                return Err(BuildError::Unsupported(
                    "println/format of aggregate or variant types",
                ));
            }
            let value = self.lower_operand(arg)?;
            match kind {
                ConcatKind::StrPtr => {
                    writeln!(self.out, "  call void @gos_rt_concat_str(ptr {value})").unwrap();
                }
                ConcatKind::Int => {
                    let widened = self.widen_to_i64(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_i64(i64 {widened})").unwrap();
                }
                ConcatKind::Uint => {
                    let widened = self.widen_to_u64(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_u64(i64 {widened})").unwrap();
                }
                ConcatKind::Float => {
                    let widened = self.widen_to_f64(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_f64(double {widened})").unwrap();
                }
                ConcatKind::Bool => {
                    let widened = self.widen_bool_to_i32(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_bool(i32 {widened})").unwrap();
                }
                ConcatKind::Char => {
                    let widened = self.widen_char_to_i32(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_char(i32 {widened})").unwrap();
                }
                kind @ (ConcatKind::VecI64
                | ConcatKind::VecF64
                | ConcatKind::VecBool
                | ConcatKind::VecString
                | ConcatKind::VecVecI64
                | ConcatKind::VecVecString
                | ConcatKind::ArrI64(_)
                | ConcatKind::ArrF64(_)
                | ConcatKind::ArrBool(_)
                | ConcatKind::ArrString(_)
                | ConcatKind::JsonValue
                | ConcatKind::ErrorMessage
                | ConcatKind::Tuple
                | ConcatKind::Option(_)
                | ConcatKind::Result(_, _)
                | ConcatKind::Map) => {
                    let str_ptr = self.emit_concat_aggregate(arg, kind, &value)?;
                    writeln!(self.out, "  call void @gos_rt_concat_str(ptr {str_ptr})").unwrap();
                }
                ConcatKind::Unsupported => unreachable!("checked above"),
            }
        }
        let result = self.fresh();
        writeln!(self.out, "  {result} = call ptr @gos_rt_concat_finish()").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_ty} {result}, ptr {slot}").unwrap();
        }
        match target {
            Some(t) => {
                writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap();
            }
            None => {
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        Ok(())
    }

    /// Lowers `__fmt_prec(value, prec)` as a call into
    /// `gos_rt_f64_prec_to_str`. The value is widened to `f64` and
    /// the precision to `i64` to match the runtime ABI; the returned
    /// pointer becomes the destination's String value.
    pub(crate) fn lower_fmt_prec_call(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "__fmt_prec destination cannot have projections",
            ));
        }
        if args.len() != 2 {
            return Err(BuildError::Unsupported(
                "__fmt_prec expects exactly two arguments",
            ));
        }
        declare_rt(&mut self.runtime_refs, "gos_rt_f64_prec_to_str");
        let value_raw = self.lower_operand(&args[0])?;
        let value = self.coerce_to_f64(&args[0], &value_raw);
        let prec_raw = self.lower_operand(&args[1])?;
        let prec = self.widen_to_i64(&args[1], &prec_raw);
        let result = self.fresh();
        writeln!(
            self.out,
            "  {result} = call ptr @gos_rt_f64_prec_to_str(double {value}, i64 {prec})"
        )
        .unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_ty} {result}, ptr {slot}").unwrap();
        }
        match target {
            Some(t) => {
                writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap();
            }
            None => {
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        Ok(())
    }

    /// Single-arg LLVM math intrinsic dispatch: emits the call
    /// + result store + outgoing terminator branch.
    pub(crate) fn lower_math_intrinsic(
        &mut self,
        intrinsic_name: &str,
        arg: &Operand,
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let arg_v = self.lower_operand(arg)?;
        let arg_llvm = self.operand_llvm_ty(arg);
        // When an intermediate f64 value was stored through the ptr-arithmetic
        // path (e.g. result of `s * (s - a) * ...` where operand types are ptr),
        // the loaded value is ptr-typed but holds a double bit-pattern. Convert
        // ptr → i64 → double before passing to the llvm intrinsic.
        let arg_v = if arg_llvm == "ptr" {
            let i = self.fresh();
            let d = self.fresh();
            writeln!(self.out, "  {i} = ptrtoint ptr {arg_v} to i64").unwrap();
            writeln!(self.out, "  {d} = bitcast i64 {i} to double").unwrap();
            d
        } else {
            arg_v
        };
        let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
        self.runtime_refs
            .insert(format!("declare double @{intrinsic_name}(double)"));
        let tmp = self.fresh();
        writeln!(
            self.out,
            "  {tmp} = call {dest_ty} @{intrinsic_name}(double {arg_v})"
        )
        .unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_ty} {tmp}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }
}
