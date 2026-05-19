#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use std::sync::atomic::{AtomicI64, Ordering};

use super::*;

// ---------------------------------------------------------------
// Channel runtime — bounded MPMC via parking_lot Mutex<VecDeque>
// ---------------------------------------------------------------

use std::collections::VecDeque;

use parking_lot::{Condvar as PlCondvar, Mutex as PlMutex};

/// Channel payload storage. The 8-byte specialisation matches the
/// most common shape (every i64-class scalar plus pointer-sized
/// values fit) and avoids the per-message `Vec<u8>` allocation that
/// the byte-erased path needs. The codegen always knows
/// `elem_bytes` at the `gos_rt_chan_new` site, so the dispatch is
/// a one-time check at construction.
enum ChanStorage {
    /// 8-byte inline payloads. A 1M-message run with cap=100
    /// holds at most 100 * 8 = 800 B of payload here, vs ~3.2 MB
    /// of `Vec<u8>` headers + 8 B allocations under `Bytes`.
    I64(VecDeque<i64>),
    /// Erased byte storage for any other element size.
    Bytes(VecDeque<Vec<u8>>),
}

pub struct GosChan {
    pub elem_bytes: u32,
    pub cap: i64, // 0 = unbounded
    pub closed: PlMutex<bool>,
    buf: PlMutex<ChanStorage>,
    pub not_empty: PlCondvar,
    pub not_full: PlCondvar,
    /// Gids of goroutines parked on a recv (channel was empty). The
    /// next sender pops one and unparks it. Empty when no
    /// goroutines are waiting, in which case the OS-thread
    /// `not_empty` Condvar is the only waker path.
    parked_recv: parking_lot::Mutex<std::collections::VecDeque<crate::sched::Gid>>,
    /// Gids of goroutines parked on a send (buffer full). The next
    /// receiver pops one and unparks it.
    parked_send: parking_lot::Mutex<std::collections::VecDeque<crate::sched::Gid>>,
    /// Goroutine id of the most recent sender. Read by recv to
    /// record a happens-before edge into the race detector. `-1`
    /// means "no sender yet observed".
    pub last_sender: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_new(elem_bytes: u32, cap: i64) -> *mut GosChan {
    ffi_entry!(std::ptr::null_mut(), {
        let buf = if elem_bytes == 8 {
            ChanStorage::I64(VecDeque::new())
        } else {
            ChanStorage::Bytes(VecDeque::new())
        };
        Box::into_raw(Box::new(GosChan {
            elem_bytes,
            cap,
            closed: PlMutex::new(false),
            buf: PlMutex::new(buf),
            not_empty: PlCondvar::new(),
            not_full: PlCondvar::new(),
            parked_recv: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            parked_send: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            last_sender: AtomicI64::new(-1),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_send(c: *mut GosChan, val: *const u8) {
    ffi_entry!((), {
        if c.is_null() || val.is_null() {
            return;
        }
        let chan = unsafe { &*c };
        let bytes_len = chan.elem_bytes as usize;
        loop {
            let mut guard = chan.buf.lock();
            if chan.cap <= 0 || (storage_len(&guard) as i64) < chan.cap {
                push_back(&mut guard, val, bytes_len);
                drop(guard);
                chan.last_sender
                    .store(i64::from(crate::race::current_gid()), Ordering::Release);
                wake_one_recv(chan);
                return;
            }
            // Buffer full. Goroutines park; OS threads block.
            if gossamer_coro::in_goroutine() {
                drop(guard);
                crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                    chan.parked_send.lock().push_back(parker.gid);
                });
                // Cleanup: remove our gid from parked_send if still
                // present (e.g. a parallel close fired with pre_unpark
                // before any matching receive).
                if let Some(gid) = crate::sched_global::current_gid() {
                    chan.parked_send.lock().retain(|g| *g != gid);
                }
            } else {
                // Non-goroutine fallback: condvar-block the OS thread.
                // parking_lot's `wait` takes &mut guard and re-acquires
                // on wakeup; drop the guard explicitly after so clippy
                // doesn't flag a let-underscore-lock pattern.
                chan.not_full.wait(&mut guard);
                drop(guard);
            }
        }
    });
}

fn wake_one_recv(chan: &GosChan) {
    if let Some(gid) = chan.parked_recv.lock().pop_front() {
        crate::sched_global::scheduler().unpark(gid);
    }
    chan.not_empty.notify_one();
}

fn wake_one_send(chan: &GosChan) {
    if let Some(gid) = chan.parked_send.lock().pop_front() {
        crate::sched_global::scheduler().unpark(gid);
    }
    chan.not_full.notify_one();
}

fn wake_all(chan: &GosChan) {
    let recvs: Vec<_> = chan.parked_recv.lock().drain(..).collect();
    let sends: Vec<_> = chan.parked_send.lock().drain(..).collect();
    let sched = crate::sched_global::scheduler();
    for gid in recvs.into_iter().chain(sends) {
        sched.unpark(gid);
    }
    chan.not_empty.notify_all();
    chan.not_full.notify_all();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_try_send(c: *mut GosChan, val: *const u8) -> i32 {
    ffi_entry!(-1, {
        if c.is_null() || val.is_null() {
            return 0;
        }
        let chan = unsafe { &*c };
        let bytes_len = chan.elem_bytes as usize;
        let mut guard = chan.buf.lock();
        if chan.cap > 0 && storage_len(&guard) as i64 >= chan.cap {
            return 0;
        }
        push_back(&mut guard, val, bytes_len);
        drop(guard);
        chan.last_sender
            .store(i64::from(crate::race::current_gid()), Ordering::Release);
        chan.not_empty.notify_one();
        1
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_recv(c: *mut GosChan, out: *mut u8) -> i32 {
    ffi_entry!(-1, {
        if c.is_null() || out.is_null() {
            return 0;
        }
        let chan = unsafe { &*c };
        let bytes_len = chan.elem_bytes as usize;
        loop {
            let mut guard = chan.buf.lock();
            if pop_front(&mut guard, out, bytes_len) {
                drop(guard);
                record_chan_handoff(chan);
                wake_one_send(chan);
                return 1;
            }
            if *chan.closed.lock() {
                return 0;
            }
            // Empty channel. Goroutines park; OS threads block.
            if gossamer_coro::in_goroutine() {
                drop(guard);
                crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                    chan.parked_recv.lock().push_back(parker.gid);
                });
                if let Some(gid) = crate::sched_global::current_gid() {
                    chan.parked_recv.lock().retain(|g| *g != gid);
                }
            } else {
                chan.not_empty.wait(&mut guard);
                drop(guard);
            }
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_try_recv(c: *mut GosChan, out: *mut u8) -> i32 {
    ffi_entry!(-1, {
        if c.is_null() || out.is_null() {
            return 0;
        }
        let chan = unsafe { &*c };
        let bytes_len = chan.elem_bytes as usize;
        let mut guard = chan.buf.lock();
        if pop_front(&mut guard, out, bytes_len) {
            drop(guard);
            record_chan_handoff(chan);
            chan.not_full.notify_one();
            return 1;
        }
        0
    })
}

/// Single-argument wrapper for LLVM: calls `gos_rt_chan_recv` and
/// boxes the status + value into a `*mut GosResult` (disc=0 → Some,
/// disc=1 → None) so callers don't need to manage an out-pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_recv_option(c: *mut GosChan) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        let mut out = 0i64;
        let status = unsafe { gos_rt_chan_recv(c, std::ptr::addr_of_mut!(out).cast::<u8>()) };
        let disc = 1 - i64::from(status);
        let payload = if status == 1 { out } else { 0 };
        Box::into_raw(Box::new(GosResult { disc, payload }))
    })
}

/// Single-argument wrapper for LLVM: like `gos_rt_chan_recv_option`
/// but non-blocking (returns None immediately when the buffer is empty).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_try_recv_option(c: *mut GosChan) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        let mut out = 0i64;
        let status = unsafe { gos_rt_chan_try_recv(c, std::ptr::addr_of_mut!(out).cast::<u8>()) };
        let disc = 1 - i64::from(status);
        let payload = if status == 1 { out } else { 0 };
        Box::into_raw(Box::new(GosResult { disc, payload }))
    })
}

/// Cross-crate hooks installed by `gossamer-std` so the runtime
/// can observe a `Context` without depending on `gossamer-std`
/// itself. `ctx_handle` is the opaque pointer the caller passes
/// to `gos_rt_chan_recv_ctx_option` etc.; the installed callbacks
/// downcast it on their side. All three hooks must be installed
/// together via [`gos_rt_install_ctx_hooks`] before any
/// context-aware runtime entry point is called.
type CtxRegisterFn = unsafe extern "C" fn(ctx_handle: *const u8, gid: u32);
type CtxDeregisterFn = unsafe extern "C" fn(ctx_handle: *const u8, gid: u32);
type CtxIsCancelledFn = unsafe extern "C" fn(ctx_handle: *const u8) -> i32;

static CTX_REGISTER_HOOK: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static CTX_DEREGISTER_HOOK: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static CTX_IS_CANCELLED_HOOK: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Installs the cross-crate context hooks. Idempotent; calling
/// twice with the same fn pointers is a no-op. Calling with a
/// different fn pointer (an actual rebind) is undefined behaviour
/// — the caller (gossamer-std) installs exactly once at first
/// use of a context-aware runtime entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_install_ctx_hooks(
    register: CtxRegisterFn,
    deregister: CtxDeregisterFn,
    is_cancelled: CtxIsCancelledFn,
) {
    ffi_entry!((), {
        use std::sync::atomic::Ordering;
        CTX_REGISTER_HOOK.store(register as *mut (), Ordering::Release);
        CTX_DEREGISTER_HOOK.store(deregister as *mut (), Ordering::Release);
        CTX_IS_CANCELLED_HOOK.store(is_cancelled as *mut (), Ordering::Release);
    });
}

fn ctx_register_hook() -> Option<CtxRegisterFn> {
    let p = CTX_REGISTER_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        // SAFETY: `p` was stored via `CtxRegisterFn as *mut ()` in
        // `gos_rt_install_ctx_hooks` and is read back with the
        // same function-pointer type. The pointer itself is
        // immutable for the program's lifetime after install.
        Some(unsafe { std::mem::transmute::<*mut (), CtxRegisterFn>(p) })
    }
}

fn ctx_deregister_hook() -> Option<CtxDeregisterFn> {
    let p = CTX_DEREGISTER_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute::<*mut (), CtxDeregisterFn>(p) })
    }
}

fn ctx_is_cancelled_hook() -> Option<CtxIsCancelledFn> {
    let p = CTX_IS_CANCELLED_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute::<*mut (), CtxIsCancelledFn>(p) })
    }
}

/// Cancellation-aware variant of [`gos_rt_chan_recv_option`].
///
/// Behaves identically to `chan_recv_option` when the context
/// is uncancelled. If the context fires while the goroutine is
/// parked on the channel's `parked_recv` queue, the registered
/// `is_cancelled` hook's cancellation will be observed on the
/// next unpark cycle and the function returns `None` (disc=1).
///
/// `ctx_handle` is the opaque pointer the caller's
/// `gos_rt_install_ctx_hooks` callbacks know how to interpret;
/// the runtime never derefs it directly. Passing `null` falls
/// back to the unconditional [`gos_rt_chan_recv_option`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_recv_ctx_option(
    c: *mut GosChan,
    ctx_handle: *const u8,
) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        if ctx_handle.is_null() {
            return unsafe { gos_rt_chan_recv_option(c) };
        }
        let (Some(register), Some(deregister), Some(is_cancelled)) = (
            ctx_register_hook(),
            ctx_deregister_hook(),
            ctx_is_cancelled_hook(),
        ) else {
            return unsafe { gos_rt_chan_recv_option(c) };
        };
        // Check before parking: an already-cancelled context
        // short-circuits without touching the channel.
        if unsafe { is_cancelled(ctx_handle) } != 0 {
            return Box::into_raw(Box::new(GosResult {
                disc: 1,
                payload: 0,
            }));
        }
        if c.is_null() {
            return Box::into_raw(Box::new(GosResult {
                disc: 1,
                payload: 0,
            }));
        }
        let chan = unsafe { &*c };
        let bytes_len = chan.elem_bytes as usize;
        let gid = crate::sched_global::current_gid();
        if let Some(g) = gid {
            unsafe { register(ctx_handle, g.as_u32()) };
        }
        // Inline the recv loop with cancel polling on both the
        // goroutine park path and the OS-thread condvar path. The
        // 50 ms condvar timeout is the cancel-observation latency
        // for non-goroutine callers: short enough to feel responsive,
        // long enough not to hot-loop while idle.
        let mut out_val = 0i64;
        let out_ptr = std::ptr::addr_of_mut!(out_val).cast::<u8>();
        let (result_disc, result_payload) = loop {
            let mut guard = chan.buf.lock();
            if pop_front(&mut guard, out_ptr, bytes_len) {
                drop(guard);
                record_chan_handoff(chan);
                wake_one_send(chan);
                break (0i64, out_val);
            }
            if *chan.closed.lock() {
                break (1i64, 0i64);
            }
            if gossamer_coro::in_goroutine() {
                drop(guard);
                crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                    chan.parked_recv.lock().push_back(parker.gid);
                });
                if let Some(g) = crate::sched_global::current_gid() {
                    chan.parked_recv.lock().retain(|x| *x != g);
                }
                if unsafe { is_cancelled(ctx_handle) } != 0 {
                    break (1i64, 0i64);
                }
            } else {
                // OS-thread path: bounded condvar wait so the
                // cancel poll below can fire even when the channel
                // never gets a sender. Without the timeout, an
                // OS-thread caller would block forever on a
                // cancelled context.
                chan.not_empty
                    .wait_for(&mut guard, std::time::Duration::from_millis(50));
                drop(guard);
                if unsafe { is_cancelled(ctx_handle) } != 0 {
                    break (1i64, 0i64);
                }
            }
        };
        if let Some(g) = gid {
            unsafe { deregister(ctx_handle, g.as_u32()) };
        }
        Box::into_raw(Box::new(GosResult {
            disc: result_disc,
            payload: result_payload,
        }))
    })
}

fn storage_len(storage: &ChanStorage) -> usize {
    match storage {
        ChanStorage::I64(d) => d.len(),
        ChanStorage::Bytes(d) => d.len(),
    }
}

fn push_back(storage: &mut ChanStorage, val: *const u8, bytes_len: usize) {
    match storage {
        ChanStorage::I64(deque) => {
            // Read 8 bytes from `val` into an i64 in a way that
            // doesn't assume natural alignment of the source.
            let mut tmp = [0u8; 8];
            unsafe {
                std::ptr::copy_nonoverlapping(val, tmp.as_mut_ptr(), 8);
            }
            deque.push_back(i64::from_ne_bytes(tmp));
        }
        ChanStorage::Bytes(deque) => {
            let mut data = vec![0u8; bytes_len];
            unsafe {
                std::ptr::copy_nonoverlapping(val, data.as_mut_ptr(), bytes_len);
            }
            deque.push_back(data);
        }
    }
}

fn pop_front(storage: &mut ChanStorage, out: *mut u8, bytes_len: usize) -> bool {
    match storage {
        ChanStorage::I64(deque) => deque.pop_front().is_some_and(|n| {
            let bytes = n.to_ne_bytes();
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 8);
            }
            true
        }),
        ChanStorage::Bytes(deque) => deque.pop_front().is_some_and(|item| {
            unsafe {
                std::ptr::copy_nonoverlapping(item.as_ptr(), out, bytes_len);
            }
            true
        }),
    }
}

/// Records the sender->receiver synchronisation edge into the race
/// detector. Called immediately after a successful recv. No-op
/// when the race detector is disabled.
fn record_chan_handoff(chan: &GosChan) {
    let from = chan.last_sender.load(Ordering::Acquire);
    if from < 0 {
        return;
    }
    let to = crate::race::current_gid();
    crate::race::record_sync(u32::try_from(from).unwrap_or(0), to);
}

/// Closes a channel. Returns `0` on success, `-1` if `c` is null,
/// `-2` if the channel was already closed (double-close used to
/// abort the process; the runtime now returns an error code so a
/// stray double-close in user code becomes a recoverable
/// diagnostic instead of a process-wide crash). Callers may
/// ignore the return value — the prior `()` signature is binary-
/// compatible with the new `i32` one under SysV (callee fills
/// `%rax`, caller ignores).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_close(c: *mut GosChan) -> i32 {
    ffi_entry!(-1, {
        if c.is_null() {
            return -1;
        }
        let chan = unsafe { &*c };
        {
            let mut guard = chan.closed.lock();
            if *guard {
                eprintln!("gossamer runtime: channel already closed (ignored)");
                return -2;
            }
            *guard = true;
        }
        wake_all(chan);
        0
    })
}

/// Drops a channel created with `gos_rt_chan_new`.
/// Closes the channel first so any thread parked on `not_empty` /
/// `not_full` wakes with `RecvResult::Closed` / `SendResult::Closed`
/// before the underlying storage is reclaimed. Calling this on a
/// channel that other threads are still using is a logic error;
/// the codegen emits the call at the channel's last live use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_drop(c: *mut GosChan) {
    ffi_entry!((), {
        if c.is_null() {
            return;
        }
        // Close + notify before reclamation so parked threads observe
        // the closed flag rather than racing the Box drop. The Drop
        // impl on `GosChan` repeats the close+notify, harmlessly,
        // because callers may also drop a `Box<GosChan>` directly in
        // tests without going through this entry point.
        unsafe {
            // Discard the close result — double-close is now an error
            // code, not a process abort. Drop still runs.
            let _ = gos_rt_chan_close(c);
            drop(Box::from_raw(c));
        }
    });
}

impl Drop for GosChan {
    fn drop(&mut self) {
        *self.closed.lock() = true;
        self.not_empty.notify_all();
        self.not_full.notify_all();
    }
}
