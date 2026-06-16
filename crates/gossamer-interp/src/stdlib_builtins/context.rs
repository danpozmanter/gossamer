#![allow(
    unused_imports,
    dead_code,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::missing_errors_doc,
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value
)]
//! `std::context` builtins for the bytecode VM — request-scoped
//! cancellation and deadlines. The handle is a struct carrying an
//! `id`; the node state lives in a process-global registry keyed by
//! `id` (mirrors `sync::Map` / `math::rand::Rng`), so a context minted
//! on one goroutine worker thread resolves on another.
//!
//! Constructors `background` / `with_cancel` / `with_timeout` plus the
//! `cancel` / `is_cancelled` / `done` methods are the bit-identical VM
//! mirror of the compiled `gos_rt_ctx_*` shims. Cancellation is eager
//! down the child tree; `is_cancelled` also walks up the parent chain
//! and honours an optional deadline. Deadlines use `std::time::Instant`
//! so behaviour is identical on every target.
//!
//! The closure-returning `with_cancel -> (ctx, cancel)` shape is a
//! documented follow-up. `done_chan()` returns the context's "done"
//! channel; `cancel` closes it so a `select` recv arm on the channel
//! becomes ready (closed-channel select readiness). A deadline
//! (`with_timeout`) flips `is_cancelled` lazily but does not close the
//! channel, so only explicit `cancel` drives the selectable path.

use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use gossamer_ast::Ident;

use crate::builtins::{BuiltinFnPub, value_to_int};
use crate::value::{Channel, RuntimeResult, Value};

struct CtxNode {
    cancelled: AtomicBool,
    deadline: Option<Instant>,
    parent: Option<i64>,
    children: parking_lot::Mutex<Vec<i64>>,
    /// The context's "done" channel. `cancel` closes it so a
    /// `select { _ = ctx.done_chan().recv() => … }` arm becomes ready
    /// via closed-channel select readiness. Closing is idempotent.
    chan: Channel,
}

static CTX_REGISTRY: LazyLock<parking_lot::Mutex<StdHashMap<i64, Arc<CtxNode>>>> =
    LazyLock::new(|| parking_lot::Mutex::new(StdHashMap::new()));
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

pub(crate) fn install_context(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("Context::background", builtin_ctx_background),
        ("context::Context::background", builtin_ctx_background),
        ("Context::with_cancel", builtin_ctx_with_cancel),
        ("context::Context::with_cancel", builtin_ctx_with_cancel),
        ("Context::with_timeout", builtin_ctx_with_timeout),
        ("context::Context::with_timeout", builtin_ctx_with_timeout),
        // Methods (dispatched via the `context::Context` struct-name
        // key that `qualified_method_key` forms).
        ("context::Context::cancel", builtin_ctx_cancel),
        ("context::Context::is_cancelled", builtin_ctx_is_cancelled),
        ("context::Context::done", builtin_ctx_done),
        ("context::Context::done_chan", builtin_ctx_done_chan),
    ];
    for (name, call) in entries {
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

fn ctx_handle(id: i64) -> Value {
    Value::struct_(
        "context::Context",
        Arc::unwrap_or_clone(Arc::new(vec![(Ident::new("__ctx"), Value::Int(id))])),
    )
}

fn ctx_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "context::Context" {
            for (i, v) in &inner.fields {
                if i.name == "__ctx" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

fn node_of(id: i64) -> Option<Arc<CtxNode>> {
    CTX_REGISTRY.lock().get(&id).cloned()
}

fn alloc_node(deadline: Option<Instant>, parent: Option<i64>) -> Value {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let node = Arc::new(CtxNode {
        cancelled: AtomicBool::new(false),
        deadline,
        parent,
        children: parking_lot::Mutex::new(Vec::new()),
        chan: Channel::new(),
    });
    CTX_REGISTRY.lock().insert(id, node);
    if let Some(pid) = parent {
        if let Some(p) = node_of(pid) {
            p.children.lock().push(id);
        }
    }
    ctx_handle(id)
}

pub(crate) fn builtin_ctx_background(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(alloc_node(None, None))
}

pub(crate) fn builtin_ctx_with_cancel(args: &[Value]) -> RuntimeResult<Value> {
    let parent = args.first().and_then(ctx_id_of);
    Ok(alloc_node(None, parent))
}

pub(crate) fn builtin_ctx_with_timeout(args: &[Value]) -> RuntimeResult<Value> {
    let parent = args.first().and_then(ctx_id_of);
    let millis = args.get(1).and_then(value_to_int).unwrap_or(0).max(0) as u64;
    let deadline = Instant::now() + Duration::from_millis(millis);
    Ok(alloc_node(Some(deadline), parent))
}

fn cancel_node(id: i64) {
    let Some(node) = node_of(id) else {
        return;
    };
    node.cancelled.store(true, Ordering::Release);
    // Closing the done channel makes the node's `select` recv arm
    // ready; idempotent, so a repeated cancel is harmless.
    let _ = node.chan.close();
    let kids: Vec<i64> = node.children.lock().clone();
    for k in kids {
        cancel_node(k);
    }
}

pub(crate) fn builtin_ctx_cancel(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(ctx_id_of) {
        cancel_node(id);
    }
    Ok(Value::Unit)
}

fn node_is_cancelled(id: i64) -> bool {
    let Some(node) = node_of(id) else {
        return false;
    };
    if node.cancelled.load(Ordering::Acquire) {
        return true;
    }
    if let Some(deadline) = node.deadline {
        if Instant::now() >= deadline {
            return true;
        }
    }
    node.parent.is_some_and(node_is_cancelled)
}

pub(crate) fn builtin_ctx_is_cancelled(args: &[Value]) -> RuntimeResult<Value> {
    let cancelled = args
        .first()
        .and_then(ctx_id_of)
        .is_some_and(node_is_cancelled);
    Ok(Value::Bool(cancelled))
}

pub(crate) fn builtin_ctx_done(args: &[Value]) -> RuntimeResult<Value> {
    builtin_ctx_is_cancelled(args)
}

/// `ctx.done_chan()` — the context's done channel as a `Receiver`.
/// `cancel` (this context or an ancestor) closes it, so a closed-channel
/// `select` recv arm fires on cancellation. Returns the same channel on
/// every call for a given context.
pub(crate) fn builtin_ctx_done_chan(args: &[Value]) -> RuntimeResult<Value> {
    match args.first().and_then(ctx_id_of).and_then(node_of) {
        Some(node) => Ok(Value::Channel(node.chan.clone())),
        None => Ok(Value::Channel(Channel::new())),
    }
}
