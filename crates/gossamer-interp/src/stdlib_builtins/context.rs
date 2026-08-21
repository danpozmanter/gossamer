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
//! `std::context` builtins for the bytecode VM - request-scoped
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
//! plus a small timer thread that drives the same cancellation path as
//! explicit cancel, so `done_chan()` is selectable on timeout.
//!
//! The closure-returning `with_cancel -> (ctx, cancel)` shape is a
//! documented follow-up. `done_chan()` returns the context's "done"
//! channel; `cancel` or deadline expiry closes it so a `select` recv arm
//! on the channel becomes ready (closed-channel select readiness).

use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use gossamer_ast::Ident;

use crate::builtins::{BuiltinFnPub, value_to_int};
use crate::value::{Channel, RuntimeError, RuntimeResult, Value};

struct CtxNode {
    cancelled: AtomicBool,
    deadline: Option<Instant>,
    parent: Option<i64>,
    /// A context outside this registry whose cancellation this node
    /// follows. A request's context takes the server's, so a peer that
    /// disconnects and a process that begins shutting down each reach the
    /// handler without a second watcher to keep in step.
    follows: Option<gossamer_std::context::Context>,
    children: parking_lot::Mutex<Vec<i64>>,
    /// The context's "done" channel. `cancel` closes it so a
    /// `select { _ = ctx.done_chan().recv() => … }` arm becomes ready
    /// via closed-channel select readiness. Closing is idempotent.
    chan: Channel,
}

static CTX_REGISTRY: LazyLock<parking_lot::Mutex<StdHashMap<i64, Arc<CtxNode>>>> =
    LazyLock::new(|| parking_lot::Mutex::new(StdHashMap::new()));
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

/// Deadlines waiting to fire, earliest last so the timer thread pops the back.
/// One thread serves every context: a deadline costs an entry here rather than
/// a thread parked on a sleep.
static DEADLINES: LazyLock<parking_lot::Mutex<Vec<(Instant, i64)>>> =
    LazyLock::new(|| parking_lot::Mutex::new(Vec::new()));
static DEADLINE_WAKE: LazyLock<parking_lot::Condvar> = LazyLock::new(parking_lot::Condvar::new);
static TIMER_THREAD: std::sync::Once = std::sync::Once::new();

/// Registers `id` to be cancelled at `deadline`, starting the shared timer
/// thread on first use.
fn schedule_deadline(deadline: Instant, id: i64) {
    {
        let mut queue = DEADLINES.lock();
        queue.push((deadline, id));
        // Latest first, so the earliest deadline is the last element.
        queue.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
    }
    TIMER_THREAD.call_once(|| {
        // A daemon thread: nothing joins it, and it exits with the process,
        // the same lifecycle the goroutine workers already have.
        let _ = std::thread::Builder::new()
            .name("gossamer-ctx-timer".to_string())
            .spawn(run_deadline_timer);
    });
    DEADLINE_WAKE.notify_all();
}

/// Whether any context deadline is still waiting to fire. A pending deadline
/// is an actor outside the goroutine set that will close a done channel, so
/// a program blocked on one is waiting rather than deadlocked.
pub(crate) fn deadline_pending() -> bool {
    !DEADLINES.lock().is_empty()
}

/// Cancels each context as its deadline arrives, sleeping until the earliest
/// one and waking early whenever a nearer deadline is registered.
fn run_deadline_timer() {
    loop {
        let mut queue = DEADLINES.lock();
        let Some(&(earliest, id)) = queue.last() else {
            DEADLINE_WAKE.wait(&mut queue);
            continue;
        };
        let now = Instant::now();
        if earliest > now {
            DEADLINE_WAKE.wait_for(&mut queue, earliest - now);
            continue;
        }
        queue.pop();
        drop(queue);
        cancel_node(id);
    }
}

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
        Arc::unwrap_or_clone(Arc::new(vec![("__ctx", Value::Int(id))])),
    )
}

fn ctx_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "context::Context" {
            for (i, v) in &inner.fields {
                if (*i) == "__ctx" {
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
    alloc_node_following(deadline, parent, None)
}

fn alloc_node_following(
    deadline: Option<Instant>,
    parent: Option<i64>,
    follows: Option<gossamer_std::context::Context>,
) -> Value {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let node = Arc::new(CtxNode {
        cancelled: AtomicBool::new(false),
        deadline,
        parent,
        follows,
        children: parking_lot::Mutex::new(Vec::new()),
        chan: Channel::new(),
    });
    CTX_REGISTRY.lock().insert(id, node);
    if let Some(pid) = parent {
        if let Some(p) = node_of(pid) {
            p.children.lock().push(id);
        }
        // A parent that finished cancelling before this link was made
        // never reaches the child through its own walk, so the child
        // takes the ancestry's state at birth.
        if node_is_cancelled(pid) {
            cancel_node(id);
        }
    }
    if let Some(deadline) = deadline {
        schedule_deadline(deadline, id);
    }
    ctx_handle(id)
}

/// Contexts belonging to requests currently in flight.
///
/// Shutdown cancels every one, so a handler that watches its context
/// learns the process is going down at the same moment the accept loop
/// stops taking new work.
static LIVE_REQUESTS: parking_lot::Mutex<Vec<i64>> = parking_lot::Mutex::new(Vec::new());

/// Cancels every in-flight request's context.
pub(crate) fn cancel_live_requests() {
    for id in std::mem::take(&mut *LIVE_REQUESTS.lock()) {
        cancel_node(id);
    }
}

/// Allocates a request-scoped context and answers `(value, id)`.
///
/// The id is what the server cancels with when the request ends; the
/// value is what the handler reads off `request.context`.
pub(crate) fn request_context(
    deadline_ms: i64,
    follows: Option<gossamer_std::context::Context>,
) -> (Value, i64) {
    let deadline = (deadline_ms > 0)
        .then(|| Instant::now() + std::time::Duration::from_millis(deadline_ms as u64));
    let value = alloc_node_following(deadline, None, follows);
    let id = ctx_id_of(&value).unwrap_or(-1);
    LIVE_REQUESTS.lock().push(id);
    (value, id)
}

/// Cancels a context the server created, and every descendant.
pub(crate) fn cancel_request_context(id: i64) {
    if id >= 0 {
        LIVE_REQUESTS.lock().retain(|live| *live != id);
        cancel_node(id);
    }
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
    let millis = args.get(1).and_then(value_to_int).unwrap_or(0);
    if millis < 0 {
        return Err(RuntimeError::Type(
            "Context::with_timeout: timeout_ms must be non-negative".to_string(),
        ));
    }
    let millis = u64::try_from(millis).map_err(|_| {
        RuntimeError::Type("Context::with_timeout: timeout_ms is too large".to_string())
    })?;
    let deadline = Instant::now() + Duration::from_millis(millis);
    Ok(alloc_node(Some(deadline), parent))
}

/// Cancels `id` and every descendant. The walk carries its own stack so a deep
/// context chain costs heap rather than call frames, and each child list is
/// copied out before its node is cancelled so the tree lock is never held
/// across the cancel of a child. Each cancelled node is then retired from
/// the registry, and the walk's root from its parent's child list, so a
/// long-lived parent's list stays proportional to the children still live
/// under it.
fn cancel_node(id: i64) {
    if let Some(node) = node_of(id)
        && let Some(parent) = node.parent.and_then(node_of)
    {
        parent.children.lock().retain(|child| *child != id);
    }
    let mut pending = vec![id];
    while let Some(current) = pending.pop() {
        let Some(node) = node_of(current) else {
            continue;
        };
        node.cancelled.store(true, Ordering::Release);
        // Closing the done channel makes the node's `select` recv arm
        // ready; idempotent, so a repeated cancel is harmless.
        let _ = node.chan.close();
        let kids: Vec<i64> = node.children.lock().clone();
        pending.extend(kids);
        CTX_REGISTRY.lock().remove(&current);
    }
}

pub(crate) fn builtin_ctx_cancel(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(ctx_id_of) {
        cancel_node(id);
    }
    Ok(Value::Unit)
}

/// Whether `id` or any ancestor is cancelled or past its deadline. The
/// chain is walked iteratively so ancestry depth costs heap rather than
/// call frames, and an id the registry no longer holds names a context
/// whose cancellation already ran.
fn node_is_cancelled(id: i64) -> bool {
    let mut current = Some(id);
    while let Some(node_id) = current {
        let Some(node) = node_of(node_id) else {
            return true;
        };
        if node.cancelled.load(Ordering::Acquire) {
            return true;
        }
        if node.follows.as_ref().is_some_and(|ctx| ctx.is_cancelled()) {
            return true;
        }
        if let Some(deadline) = node.deadline
            && Instant::now() >= deadline
        {
            return true;
        }
        current = node.parent;
    }
    false
}

/// Returns whether a VM `Context` value has been cancelled or timed out.
/// Cancellation-aware VM primitives use this so they share the parent and
/// deadline semantics of `Context::is_cancelled`.
pub(crate) fn value_is_cancelled(value: &Value) -> bool {
    ctx_id_of(value).is_some_and(node_is_cancelled)
}

pub(crate) fn builtin_ctx_is_cancelled(args: &[Value]) -> RuntimeResult<Value> {
    let cancelled = args.first().is_some_and(value_is_cancelled);
    Ok(Value::Bool(cancelled))
}

pub(crate) fn builtin_ctx_done(args: &[Value]) -> RuntimeResult<Value> {
    builtin_ctx_is_cancelled(args)
}

/// `ctx.done_chan()` - the context's done channel as a `Receiver`.
/// `cancel` (this context or an ancestor) closes it, so a closed-channel
/// `select` recv arm fires on cancellation. Returns the same channel on
/// every call for a given context.
pub(crate) fn builtin_ctx_done_chan(args: &[Value]) -> RuntimeResult<Value> {
    match args.first().and_then(ctx_id_of).and_then(node_of) {
        Some(node) => Ok(Value::Channel(node.chan.clone())),
        // A context whose cancellation already ran reports exactly one
        // thing through this channel: readiness. A closed channel is that.
        None => {
            let chan = Channel::new();
            let _ = chan.close();
            Ok(Value::Channel(chan))
        }
    }
}
