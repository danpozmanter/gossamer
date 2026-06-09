#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::if_not_else)]
#![allow(clippy::single_match_else)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::redundant_else)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::single_match)]
#![allow(clippy::useless_conversion)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::Span;
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue,
    Statement, StatementKind, Terminator, UnOp,
};

use super::*;

pub(crate) struct Builder<'a> {
    pub(crate) tcx: &'a mut TyCtxt,
    pub(crate) locals: Vec<LocalDecl>,
    pub(crate) blocks: Vec<BasicBlock>,
    pub(crate) current: Option<BlockId>,
    pub(crate) scopes: Vec<HashMap<String, Local>>,
    pub(crate) fn_span: Span,
    pub(crate) structs: &'a HashMap<String, Vec<String>>,
    pub(crate) struct_defs: &'a HashMap<gossamer_resolve::DefId, String>,
    pub(crate) enums: &'a EnumIndex,
    pub(crate) impl_methods: &'a HashMap<String, Option<Ty>>,
    pub(crate) fn_returns: &'a HashMap<gossamer_resolve::DefId, Ty>,
    pub(crate) fn_inputs: &'a HashMap<gossamer_resolve::DefId, Vec<Ty>>,
    pub(crate) consts: &'a HashMap<gossamer_resolve::DefId, ConstValue>,
    /// Free functions that may let a value escape (spawn / channel / static
    /// write / param-stash). A loop calling any of these is never
    /// auto-regioned. See `collect_region_unsafe_fns`.
    pub(crate) region_unsafe: &'a std::collections::HashSet<gossamer_resolve::DefId>,
    pub(crate) local_struct: HashMap<Local, String>,
    /// For locals that hold an array/tuple whose element type is a
    /// known struct, records that struct's name. Used to resolve
    /// field projections through `a[i].x` when the type checker left
    /// the element type as an unresolved inference variable.
    pub(crate) local_elem_struct: HashMap<Local, String>,
    pub(crate) local_closure: HashMap<Local, String>,
    /// Locals that hold a function-name constant (e.g. a synthesised
    /// closure body like `__closure_0` bound through a let). Tracked
    /// so that calling the local dispatches to the named function by
    /// direct call rather than treating the local as a closure env
    /// pointer.
    pub(crate) local_fn_name: HashMap<Local, String>,
    /// Runtime-shape tag for locals whose static MIR type doesn't
    /// distinguish the stdlib type behind them (everything ends
    /// up as `i64` / pointer once erased). Method dispatch reads
    /// this tag to pick the right runtime helper for `fs.string(...)`,
    /// `client.get(...)`, `req.send()`, etc.
    pub(crate) local_runtime_kind: HashMap<Local, &'static str>,
    /// Per-local field layout for synthesised aggregates produced by
    /// the declarative `flag::define(...)` lowering. Maps the result
    /// local to a `Vec<(long_name, cell_kind)>` indexed by field
    /// position. Field access `flags.<long>` consults this table to
    /// emit the right `Field(idx)` projection plus the corresponding
    /// `flag::Cell::*` runtime kind tag.
    pub(crate) local_define_layout: HashMap<Local, Vec<(String, &'static str)>>,
    pub(crate) param_locals: std::collections::HashSet<Local>,
    /// Loop contexts visible at the current lowering point. The
    /// innermost loop is at the back. Each entry pairs the
    /// `continue`-target (the loop header) with the `break`-target
    /// (the block emitted right after the loop). `lower_loop` /
    /// `lower_while` push on entry and pop on exit;
    /// `HirExprKind::Break` / `Continue` lookup the back of the
    /// stack to terminate to the right block.
    pub(crate) loop_stack: Vec<LoopContext>,
    /// When set, `gos_rt_result_payload` + field/tuple-binding emissions
    /// for a Result/Option match arm are deferred into this block instead
    /// of the pre-branch header. Prevents unconditional null-deref when
    /// the scrutinee is None/Err on a subsequent loop iteration.
    pub(crate) payload_defer_block: Option<BlockId>,
    /// Binding names that are the receiver of a growth / in-place
    /// mutation method (`push`, `pop`, `insert`, `remove`, `sort`,
    /// `sort_by`, `extend`, `truncate`, `clear`, `retain`, `swap`)
    /// somewhere in the function body. A `let mut xs = [literal]`
    /// is only promoted to a heap `Vec` when its name is in this set
    /// — an explicitly-sized `let mut bodies: [Body; 5]` that is only
    /// indexed / field-mutated / passed to a `[T; N]`-typed parameter
    /// must stay a fixed inline array so the layout matches at every
    /// use site.
    pub(crate) grows_bindings: std::collections::HashSet<String>,
    /// Element type pushed into each growing binding, keyed by binding
    /// name, recovered from the first argument of `push` / `insert` /
    /// `append` call sites. An unannotated `let mut xs = []` carries no
    /// element type on its literal, so the promotion to a heap `Vec`
    /// would otherwise size elements at 8 bytes (i64) and silently
    /// truncate every multi-slot element (`[i64; 2]`, tuple, struct) on
    /// push. This map supplies the real element stride for that case.
    pub(crate) grows_elem_ty: std::collections::HashMap<String, Ty>,
    /// Nesting depth of `runtime::region_push` .. `region_pop` while
    /// lowering. Locals created at depth > 0 are arena-region-owned: the
    /// drop pass must not release them (the region frees them wholesale at
    /// pop; a post-pop release would be a use-after-free).
    pub(crate) region_depth: u32,
    /// One frame per lexical block currently being lowered; each frame holds
    /// that block's `defer`red expressions in registration order. Block-scoped
    /// `defer` (Swift/Zig semantics) emits a frame's expressions in LIFO order
    /// at every edge that leaves the block — normal fall-through, `return`
    /// (all frames), and `break`/`continue` (frames down to the loop's frame).
    pub(crate) defer_stack: Vec<Vec<gossamer_hir::HirExpr>>,
}

/// A live loop context: where to jump on `break` vs. `continue`,
/// plus the optional result local that `break <expr>` writes into
/// before jumping. `None` for loops whose result is unused.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LoopContext {
    pub(crate) continue_to: BlockId,
    pub(crate) break_to: BlockId,
    pub(crate) result: Option<Local>,
    /// Set to `true` when a `break` targeting this loop is lowered.
    /// Used so `lower_loop` can return `None` for purely divergent
    /// loops (no `break` at all), preventing a spurious `RETURN`
    /// assign at function-tail positions.
    pub(crate) break_used: bool,
    /// `defer_stack` length at loop entry. `break`/`continue` run the defers
    /// in frames at indices `>= defer_depth` (the blocks inside the loop body)
    /// before jumping, but not the loop's enclosing frames.
    pub(crate) defer_depth: usize,
}

mod ctrl;
mod expr;
mod intrinsic;
mod method_call;
mod misc;
mod scope;
mod stdlib;
mod stmt;
mod types;

mod expr_call;

mod expr_field;

mod expr_array;

mod stdlib_json;

mod stdlib_sql;

mod stdlib_free;

mod stdlib_binding;

mod method_call_dispatch;
