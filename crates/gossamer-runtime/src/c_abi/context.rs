#![allow(clippy::missing_safety_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::cast_sign_loss)]

//! Runtime support for `std::context` - request-scoped cancellation
//! and deadlines, modeled after Go's `context.Context`.
//!
//! This is the standalone all-tier handle surface:
//! `background` / `with_cancel` / `with_timeout` constructors plus the
//! `cancel` / `is_cancelled` / `done` methods. Cancellation is eager
//! down the tree (a `cancel` flips every descendant's flag) and
//! `is_cancelled` also walks up the parent chain and honours an
//! optional deadline. Deadlines use `std::time::Instant` only - no
//! OS-specific timer code, so the shim is identical on every target.
//!
//! The closure-returning `with_cancel -> (ctx, cancel)` shape from the
//! library `gossamer_std::context` is intentionally out of scope here:
//! this handle exposes `cancel` as a direct method on the context, and
//! `done` is a non-blocking cancellation check. `done_chan`
//! (`gos_rt_ctx_cancelled`) returns a channel the cancel walk closes,
//! so cancellation is observable from a `select` arm: a parked select
//! is unparked when the channel closes, and a closed channel's recv
//! arm is always ready. A deadline (`with_timeout`) flips
//! `is_cancelled` lazily on read but does not close the done channel,
//! so only explicit `cancel` (this context or an ancestor) drives the
//! selectable path.
//!
//! The handle is an opaque heap `Box<GosCtx>` carried as an `i64` on
//! compiled tiers; it leaks at process exit like the other runtime
//! handles. Parent / child links are stored as `usize` (not raw
//! pointers) so the struct is `Send + Sync` without an `unsafe impl`;
//! the pointee outlives every handle because none are ever freed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::chan::GosChan;

/// Opaque heap handle for a context node.
pub struct GosCtx {
    cancelled: AtomicBool,
    deadline: Option<Instant>,
    /// Parent node address as `usize`, or `0` for a root context.
    parent: usize,
    /// Child node addresses; `cancel` walks these depth-first.
    children: Mutex<Vec<usize>>,
    /// Address of this node's "done" channel as `usize`. `cancel`
    /// closes it so a `select { _ = ctx.done_chan().recv() => … }`
    /// arm becomes ready (closed-channel select readiness). Stored
    /// as `usize` (not a raw pointer) so `GosCtx` stays `Send + Sync`
    /// without an `unsafe impl`; the channel is leaked like the node.
    chan: usize,
}

fn ctx_at<'a>(addr: usize) -> Option<&'a GosCtx> {
    if addr == 0 {
        None
    } else {
        // SAFETY: `addr` came from `Box::into_raw`; the node is leaked
        // (never freed), so the reference is valid for the process
        // lifetime.
        Some(unsafe { &*(addr as *const GosCtx) })
    }
}

fn alloc_ctx(deadline: Option<Instant>, parent: usize) -> *mut GosCtx {
    // Every context carries an i64-element "done" channel. The cancel
    // walk closes it, making the channel's `select` recv arm ready.
    // SAFETY: `gos_rt_chan_new` returns a freshly boxed `GosChan` or
    // null on allocation failure; both are valid `usize` addresses.
    let chan = unsafe { super::chan::gos_rt_chan_new(8, 0) } as usize;
    let child = Box::into_raw(Box::new(GosCtx {
        cancelled: AtomicBool::new(false),
        deadline,
        parent,
        children: Mutex::new(Vec::new()),
        chan,
    }));
    if let Some(p) = ctx_at(parent) {
        p.children.lock().push(child as usize);
    }
    child
}

/// Closes the node's done channel idempotently. Called by the cancel
/// walk so a `select` recv arm on the channel becomes ready.
fn close_done_chan(node: &GosCtx) {
    if node.chan == 0 {
        return;
    }
    // SAFETY: `node.chan` came from `gos_rt_chan_new` (leaked, never
    // freed), so the pointer is valid for the process lifetime.
    let chan = unsafe { &*(node.chan as *const GosChan) };
    super::chan::chan_close_idempotent(chan);
}

/// `context::Context::background()` - a root context, never cancelled,
/// no deadline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ctx_background() -> *mut GosCtx {
    ffi_entry!(std::ptr::null_mut(), { alloc_ctx(None, 0) })
}

/// `context::Context::with_cancel(parent)` - a child whose
/// cancellation can be triggered via `cancel`; cancelling an ancestor
/// also cancels it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ctx_with_cancel(parent: *mut GosCtx) -> *mut GosCtx {
    ffi_entry!(std::ptr::null_mut(), { alloc_ctx(None, parent as usize) })
}

/// `context::Context::with_timeout(parent, millis)` - a child whose
/// `is_cancelled` flips `true` once `millis` have elapsed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ctx_with_timeout(parent: *mut GosCtx, millis: i64) -> *mut GosCtx {
    ffi_entry!(std::ptr::null_mut(), {
        let deadline = Instant::now() + Duration::from_millis(millis.max(0) as u64);
        alloc_ctx(Some(deadline), parent as usize)
    })
}

fn cancel_node(addr: usize) {
    let Some(node) = ctx_at(addr) else {
        return;
    };
    node.cancelled.store(true, Ordering::Release);
    close_done_chan(node);
    let kids: Vec<usize> = node.children.lock().clone();
    for k in kids {
        cancel_node(k);
    }
}

/// `ctx.cancel()` - cancel this context and every descendant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ctx_cancel(ctx: *mut GosCtx) {
    ffi_entry!((), {
        if !ctx.is_null() {
            cancel_node(ctx as usize);
        }
    });
}

fn node_is_cancelled(addr: usize) -> bool {
    let Some(node) = ctx_at(addr) else {
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
    node_is_cancelled(node.parent)
}

/// `ctx.is_cancelled()` - `1` when this context (or an ancestor) is
/// cancelled or its deadline has passed, else `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ctx_is_cancelled(ctx: *mut GosCtx) -> i64 {
    ffi_entry!(0, {
        if ctx.is_null() {
            return 0;
        }
        i64::from(node_is_cancelled(ctx as usize))
    })
}

/// `ctx.done()` - non-blocking cancellation check; identical to
/// `is_cancelled`. For the `select`-arm channel form, see
/// `gos_rt_ctx_cancelled` / `ctx.done_chan()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ctx_done(ctx: *mut GosCtx) -> i64 {
    ffi_entry!(0, {
        if ctx.is_null() {
            return 0;
        }
        i64::from(node_is_cancelled(ctx as usize))
    })
}

/// `ctx.done_chan()` - returns the context's "done" channel as a
/// receive endpoint. `cancel` (this context or any ancestor) closes
/// the channel, so a `select { _ = ctx.done_chan().recv() => … }` arm
/// fires on cancellation via closed-channel select readiness. Returns
/// the same channel on every call for a given context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ctx_cancelled(ctx: *mut GosCtx) -> *mut GosChan {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(node) = ctx_at(ctx as usize) else {
            return std::ptr::null_mut();
        };
        node.chan as *mut GosChan
    })
}
