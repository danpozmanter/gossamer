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

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};

// ---------------------------------------------------------------
// Channel runtime - bounded MPMC via parking_lot Mutex<VecDeque>
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
    I64(VecDeque<(u64, i64)>),
    /// Erased byte storage for any other element size.
    Bytes(VecDeque<(u64, Vec<u8>)>),
}

pub struct GosChan {
    pub elem_bytes: u32,
    pub cap: i64, // 0 = unbuffered, >0 = bounded, <0 = unbounded
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
    next_send_id: AtomicU64,
    recv_waiters: AtomicUsize,
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
            next_send_id: AtomicU64::new(1),
            recv_waiters: AtomicUsize::new(0),
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
        let mut queued_unbuffered = false;
        let send_id = if chan.cap == 0 {
            chan.next_send_id.fetch_add(1, Ordering::Relaxed).max(1)
        } else {
            0
        };
        loop {
            let mut guard = chan.buf.lock();
            if chan.cap == 0 {
                if !queued_unbuffered {
                    push_back(&mut guard, send_id, val, bytes_len);
                    queued_unbuffered = true;
                    drop(guard);
                    chan.last_sender
                        .store(i64::from(crate::race::current_gid()), Ordering::Release);
                    wake_one_recv(chan);
                    continue;
                }
                if !storage_contains_id(&guard, send_id) {
                    return;
                }
            } else if chan.cap < 0 || (storage_len(&guard) as i64) < chan.cap {
                push_back(&mut guard, 0, val, bytes_len);
                drop(guard);
                chan.last_sender
                    .store(i64::from(crate::race::current_gid()), Ordering::Release);
                wake_one_recv(chan);
                return;
            }
            // Buffer full. Goroutines park; OS threads block.
            if gossamer_coro::in_goroutine() {
                // Publish our waiter entry before releasing `buf`. A receiver
                // can otherwise consume the last queued value between the
                // full-buffer check above and registration below, observe no
                // parked sender, and leave this goroutine asleep forever.
                // `park` records a pre-unpark wake that arrives after the
                // queue registration but before suspension.
                let mut guard = Some(guard);
                crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                    chan.parked_send.lock().push_back(parker.gid);
                    drop(guard.take());
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
        if chan.cap == 0 {
            let has_receiver = chan.recv_waiters.load(Ordering::Acquire) > 0
                || !chan.parked_recv.lock().is_empty();
            if !has_receiver {
                return 0;
            }
            push_back(&mut guard, 0, val, bytes_len);
        } else {
            if chan.cap > 0 && storage_len(&guard) as i64 >= chan.cap {
                return 0;
            }
            push_back(&mut guard, 0, val, bytes_len);
        }
        drop(guard);
        chan.last_sender
            .store(i64::from(crate::race::current_gid()), Ordering::Release);
        wake_one_recv(chan);
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
                // Register while still holding `buf`, pairing this empty
                // check with waiter publication. Without that pairing a
                // sender can queue a value in the gap, find no waiter to
                // wake, and strand both goroutines indefinitely.
                let mut guard = Some(guard);
                crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                    chan.recv_waiters.fetch_add(1, Ordering::AcqRel);
                    chan.parked_recv.lock().push_back(parker.gid);
                    drop(guard.take());
                });
                chan.recv_waiters.fetch_sub(1, Ordering::AcqRel);
                if let Some(gid) = crate::sched_global::current_gid() {
                    chan.parked_recv.lock().retain(|g| *g != gid);
                }
            } else {
                chan.recv_waiters.fetch_add(1, Ordering::AcqRel);
                chan.not_empty.wait(&mut guard);
                chan.recv_waiters.fetch_sub(1, Ordering::AcqRel);
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
            wake_one_send(chan);
            return 1;
        }
        0
    })
}

/// Single-argument wrapper for LLVM: calls `gos_rt_chan_recv` and
/// boxes the status + value into a `*mut GosResult` (disc=0 → Some,
/// disc=1 → None) so callers don't need to manage an out-pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_recv_option(c: *mut GosChan) -> i128 {
    ffi_entry!(0i128, {
        let mut out = 0i64;
        let status = unsafe { gos_rt_chan_recv(c, std::ptr::addr_of_mut!(out).cast::<u8>()) };
        let disc = 1 - i64::from(status);
        let payload = if status == 1 { out } else { 0 };
        crate::c_abi::vec::pack_result(disc, payload)
    })
}

/// Single-argument wrapper for LLVM: like `gos_rt_chan_recv_option`
/// but non-blocking (returns None immediately when the buffer is empty).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_try_recv_option(c: *mut GosChan) -> i128 {
    ffi_entry!(0i128, {
        let mut out = 0i64;
        let status = unsafe { gos_rt_chan_try_recv(c, std::ptr::addr_of_mut!(out).cast::<u8>()) };
        let disc = 1 - i64::from(status);
        let payload = if status == 1 { out } else { 0 };
        crate::c_abi::vec::pack_result(disc, payload)
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
/// different fn pointer (an actual rebind) is undefined behaviour -
/// the caller (gossamer-std) installs exactly once at first
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
) -> i128 {
    ffi_entry!(0i128, {
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
            return crate::c_abi::vec::pack_result(1, 0);
        }
        if c.is_null() {
            return crate::c_abi::vec::pack_result(1, 0);
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
                // Keep the empty-buffer check and receiver registration in
                // one critical section, just like the unconditional recv
                // path. Cancellation does not change the channel wakeup
                // contract.
                let mut guard = Some(guard);
                crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                    chan.recv_waiters.fetch_add(1, Ordering::AcqRel);
                    chan.parked_recv.lock().push_back(parker.gid);
                    drop(guard.take());
                });
                chan.recv_waiters.fetch_sub(1, Ordering::AcqRel);
                if let Some(g) = crate::sched_global::current_gid() {
                    chan.parked_recv.lock().retain(|x| *x != g);
                }
                if unsafe { is_cancelled(ctx_handle) } != 0 {
                    break (1i64, 0i64);
                }
            } else {
                chan.recv_waiters.fetch_add(1, Ordering::AcqRel);
                chan.not_empty
                    .wait_for(&mut guard, std::time::Duration::from_millis(50));
                chan.recv_waiters.fetch_sub(1, Ordering::AcqRel);
                drop(guard);
                if unsafe { is_cancelled(ctx_handle) } != 0 {
                    break (1i64, 0i64);
                }
            }
        };
        if let Some(g) = gid {
            unsafe { deregister(ctx_handle, g.as_u32()) };
        }
        crate::c_abi::vec::pack_result(result_disc, result_payload)
    })
}

fn storage_len(storage: &ChanStorage) -> usize {
    match storage {
        ChanStorage::I64(d) => d.len(),
        ChanStorage::Bytes(d) => d.len(),
    }
}

fn storage_contains_id(storage: &ChanStorage, id: u64) -> bool {
    match storage {
        ChanStorage::I64(d) => d.iter().any(|(msg_id, _)| *msg_id == id),
        ChanStorage::Bytes(d) => d.iter().any(|(msg_id, _)| *msg_id == id),
    }
}

fn push_back(storage: &mut ChanStorage, id: u64, val: *const u8, bytes_len: usize) {
    match storage {
        ChanStorage::I64(deque) => {
            // Read 8 bytes from `val` into an i64 in a way that
            // doesn't assume natural alignment of the source.
            let mut tmp = [0u8; 8];
            unsafe {
                std::ptr::copy_nonoverlapping(val, tmp.as_mut_ptr(), 8);
            }
            deque.push_back((id, i64::from_ne_bytes(tmp)));
        }
        ChanStorage::Bytes(deque) => {
            let mut data = vec![0u8; bytes_len];
            unsafe {
                std::ptr::copy_nonoverlapping(val, data.as_mut_ptr(), bytes_len);
            }
            deque.push_back((id, data));
        }
    }
}

fn pop_front(storage: &mut ChanStorage, out: *mut u8, bytes_len: usize) -> bool {
    match storage {
        ChanStorage::I64(deque) => deque.pop_front().is_some_and(|(_, n)| {
            let bytes = n.to_ne_bytes();
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 8);
            }
            true
        }),
        ChanStorage::Bytes(deque) => deque.pop_front().is_some_and(|(_, item)| {
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

/// Closes `chan` if it is not already closed, returning `true` when
/// this call performed the close and `false` when it was already
/// closed. Never panics: the reclamation path (`gos_rt_chan_drop`)
/// closes through here so a channel the user already closed
/// explicitly is reclaimed without a spurious double-close panic.
pub(crate) fn chan_close_idempotent(chan: &GosChan) -> bool {
    {
        let mut guard = chan.closed.lock();
        if *guard {
            return false;
        }
        *guard = true;
    }
    wake_all(chan);
    true
}

/// Closes a channel. Returns `0` on success and `-1` if `c` is null.
/// Closing an already-closed channel panics with
/// `close of closed channel`, matching Go. The panic is
/// goroutine-scoped (via `gos_rt_panic`): it unwinds to the
/// coroutine boundary inside a spawned goroutine (isolating only
/// that goroutine) and exits 101 on the main goroutine - it never
/// aborts the whole process. Callers may ignore the return value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_close(c: *mut GosChan) -> i32 {
    // `None` = null channel, `Some(false)` = already closed. The close
    // itself runs under the FFI catch-guard; the double-close panic is
    // raised below, *outside* the guard, so a goroutine-scoped unwind
    // reaches the coroutine boundary instead of being swallowed and
    // converted to an FFI-entry sentinel.
    let outcome = ffi_entry!(None, {
        if c.is_null() {
            None
        } else {
            Some(chan_close_idempotent(unsafe { &*c }))
        }
    });
    match outcome {
        None => -1,
        Some(true) => 0,
        Some(false) => {
            let cs = std::ffi::CString::new("close of closed channel").unwrap();
            unsafe { super::gos_rt_panic(cs.as_ptr()) };
            0
        }
    }
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
            // Idempotent close for reclamation - must not panic if the
            // user already closed this channel explicitly (the
            // user-facing `gos_rt_chan_close` panics on double-close).
            chan_close_idempotent(&*c);
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

// ---------------------------------------------------------------
// select { } multiplexing
// ---------------------------------------------------------------
//
// The compiled tiers lower a `select` to a builder: one `gos_rt_select_new`,
// one `gos_rt_select_arm_*` per arm in source order, then `gos_rt_select_wait`
// (poll + park), `gos_rt_select_value` (the popped recv payload), and
// `gos_rt_select_free`. This sequence-of-scalar-calls shape keeps the MIR
// lowering free of array construction while the transfer stays atomic inside
// `wait` (no recheck-after-return TOCTOU). Semantics match the VM walker:
// ready arms are polled in pseudo-random order, a closed+drained recv arm is
// always ready (yielding the element-type zero value, matching Go), and the
// default arm fires only when nothing else is.

enum SelectArmRt {
    Recv(*mut GosChan),
    Send(*mut GosChan, i64),
    Default,
}

fn select_shuffle_indices(n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    if n <= 1 {
        return order;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let mut x = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0_u64, |d| d.as_nanos() as u64);
        nanos ^ 0xA076_1D64_78BD_642F
    };
    #[cfg(target_arch = "wasm32")]
    let mut x = {
        static SELECT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SELECT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) ^ 0xA076_1D64_78BD_642F
    };
    for i in (1..n).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let j = (x as usize) % (i + 1);
        order.swap(i, j);
    }
    order
}

pub struct SelectBuilder {
    arms: Vec<SelectArmRt>,
    last_value: i64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_select_new(n: i64) -> *mut SelectBuilder {
    ffi_entry!(std::ptr::null_mut(), {
        let cap = usize::try_from(n).unwrap_or(0);
        Box::into_raw(Box::new(SelectBuilder {
            arms: Vec::with_capacity(cap),
            last_value: 0,
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_select_arm_recv(b: *mut SelectBuilder, c: *mut GosChan) {
    ffi_entry!((), {
        if b.is_null() {
            return;
        }
        unsafe { &mut *b }.arms.push(SelectArmRt::Recv(c));
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_select_arm_send(b: *mut SelectBuilder, c: *mut GosChan, val: i64) {
    ffi_entry!((), {
        if b.is_null() {
            return;
        }
        unsafe { &mut *b }.arms.push(SelectArmRt::Send(c, val));
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_select_arm_default(b: *mut SelectBuilder) {
    ffi_entry!((), {
        if b.is_null() {
            return;
        }
        unsafe { &mut *b }.arms.push(SelectArmRt::Default);
    });
}

/// Checks whether a select arm became ready while the caller was publishing
/// its waiter registrations. The check happens after every arm is registered,
/// so a readiness transition cannot be lost between the main select poll and
/// suspension.
fn select_arm_is_ready(kind: u8, chan: &GosChan) -> bool {
    let guard = chan.buf.lock();
    match kind {
        // Receive is ready for a queued value or a closed-and-drained channel.
        0 => storage_len(&guard) != 0 || *chan.closed.lock(),
        // Send is ready when buffer space exists. An unbuffered send instead
        // needs a receiver that has already published its interest.
        1 if chan.cap == 0 => {
            chan.recv_waiters.load(Ordering::Acquire) > 0 || !chan.parked_recv.lock().is_empty()
        }
        1 => chan.cap < 0 || (storage_len(&guard) as i64) < chan.cap,
        _ => false,
    }
}

/// Polls the registered arms in pseudo-random order, parking the goroutine until one
/// is ready when there is no default arm. Returns the chosen arm's source
/// index (the default arm's index when nothing else is ready), or -1 on a null
/// builder. The popped value of a chosen recv arm is stored for retrieval via
/// `gos_rt_select_value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_select_wait(b: *mut SelectBuilder) -> i64 {
    ffi_entry!(-1, {
        if b.is_null() {
            return -1;
        }
        let builder = unsafe { &mut *b };
        // Snapshot (kind, chan, send_val) so the poll/park loops don't hold a
        // borrow of `builder` across the `last_value` write. 0=recv, 1=send,
        // 2=default. Raw pointers are Copy; the snapshot is cheap.
        let arms: Vec<(u8, *mut GosChan, i64)> = builder
            .arms
            .iter()
            .map(|a| match a {
                SelectArmRt::Recv(c) => (0u8, *c, 0i64),
                SelectArmRt::Send(c, v) => (1u8, *c, *v),
                SelectArmRt::Default => (2u8, std::ptr::null_mut(), 0i64),
            })
            .collect();
        let default_index = arms.iter().position(|(k, _, _)| *k == 2);
        loop {
            for i in select_shuffle_indices(arms.len()) {
                let (kind, c, v) = arms[i];
                if kind == 0 {
                    if c.is_null() {
                        continue;
                    }
                    let chan = unsafe { &*c };
                    let mut tmp = 0i64;
                    let mut guard = chan.buf.lock();
                    if pop_front(
                        &mut guard,
                        std::ptr::addr_of_mut!(tmp).cast::<u8>(),
                        chan.elem_bytes as usize,
                    ) {
                        drop(guard);
                        record_chan_handoff(chan);
                        wake_one_send(chan);
                        builder.last_value = tmp;
                        return i as i64;
                    }
                    // Go semantics: a closed+drained recv arm is always
                    // ready, yielding the element-type zero value. Hold the
                    // `buf` lock while reading `closed` (same lock order as
                    // `gos_rt_chan_recv`) so an empty-then-closed transition
                    // is observed atomically.
                    if *chan.closed.lock() {
                        drop(guard);
                        builder.last_value = 0;
                        return i as i64;
                    }
                } else if kind == 1 {
                    if c.is_null() {
                        continue;
                    }
                    let send_val = v;
                    if unsafe { gos_rt_chan_try_send(c, std::ptr::addr_of!(send_val).cast::<u8>()) }
                        == 1
                    {
                        return i as i64;
                    }
                }
            }
            if let Some(idx) = default_index {
                return idx as i64;
            }
            // Nothing ready, no default: block until a channel changes, then
            // re-poll. Mirrors the single-channel recv/send park discipline,
            // registering on every arm's queue so any sender/receiver wakes us.
            if gossamer_coro::in_goroutine() {
                crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                    for (kind, c, _) in &arms {
                        if c.is_null() {
                            continue;
                        }
                        let chan = unsafe { &**c };
                        if *kind == 0 {
                            chan.recv_waiters.fetch_add(1, Ordering::AcqRel);
                            chan.parked_recv.lock().push_back(parker.gid);
                        } else if *kind == 1 {
                            chan.parked_send.lock().push_back(parker.gid);
                        }
                    }
                    // An arm may have become ready after the initial poll but
                    // before its waiter entry was published. Recheck after
                    // all entries exist; `unpark` records a pre-unpark wake
                    // until this coroutine has actually suspended.
                    if arms.iter().any(|(kind, c, _)| {
                        !c.is_null() && select_arm_is_ready(*kind, unsafe { &**c })
                    }) {
                        crate::sched_global::scheduler().unpark(parker.gid);
                    }
                });
                if let Some(gid) = crate::sched_global::current_gid() {
                    for (kind, c, _) in &arms {
                        if c.is_null() {
                            continue;
                        }
                        let chan = unsafe { &**c };
                        if *kind == 0 {
                            chan.recv_waiters.fetch_sub(1, Ordering::AcqRel);
                            chan.parked_recv.lock().retain(|g| *g != gid);
                        } else if *kind == 1 {
                            chan.parked_send.lock().retain(|g| *g != gid);
                        }
                    }
                }
            } else {
                // OS-thread fallback: a select waits on several channels, but a
                // single condvar wait tracks only one. Bounded-wait on the first
                // operable channel and re-poll; the 50 ms bound is the
                // missed-notify backstop, matching the walker's park cadence.
                let first = arms
                    .iter()
                    .find(|(kind, c, _)| *kind != 2 && !c.is_null())
                    .map(|(_, c, _)| *c);
                if let Some(c) = first {
                    let chan = unsafe { &*c };
                    let mut guard = chan.buf.lock();
                    chan.not_empty
                        .wait_for(&mut guard, std::time::Duration::from_millis(50));
                    drop(guard);
                } else {
                    return -1;
                }
            }
        }
    })
}

/// Returns the value popped by the most recent `gos_rt_select_wait` recv
/// outcome on this builder (0 for a send/default outcome or a null builder).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_select_value(b: *mut SelectBuilder) -> i64 {
    ffi_entry!(0, {
        if b.is_null() {
            return 0;
        }
        unsafe { &*b }.last_value
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_select_free(b: *mut SelectBuilder) {
    ffi_entry!((), {
        if b.is_null() {
            return;
        }
        unsafe {
            drop(Box::from_raw(b));
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn cap_zero_channel_send_waits_for_receiver() {
        let chan = unsafe { gos_rt_chan_new(8, 0) };
        assert!(!chan.is_null());
        let done = Arc::new(AtomicBool::new(false));
        let done_tx = Arc::clone(&done);
        let addr = chan as usize;
        let sender = std::thread::spawn(move || {
            let value = 77_i64;
            unsafe {
                gos_rt_chan_send(addr as *mut GosChan, std::ptr::addr_of!(value).cast());
            }
            done_tx.store(true, Ordering::Release);
        });
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !done.load(Ordering::Acquire),
            "unbuffered send returned before recv"
        );
        let mut out = 0_i64;
        let ok = unsafe { gos_rt_chan_recv(chan, std::ptr::addr_of_mut!(out).cast()) };
        assert_eq!(ok, 1);
        assert_eq!(out, 77);
        sender.join().expect("sender");
        assert!(done.load(Ordering::Acquire));
        unsafe { gos_rt_chan_drop(chan) };
    }

    #[test]
    fn cap_negative_channel_is_explicitly_unbounded() {
        let chan = unsafe { gos_rt_chan_new(8, -1) };
        assert!(!chan.is_null());
        let value = 11_i64;
        let sent = unsafe { gos_rt_chan_try_send(chan, std::ptr::addr_of!(value).cast()) };
        assert_eq!(sent, 1);
        let mut out = 0_i64;
        let got = unsafe { gos_rt_chan_try_recv(chan, std::ptr::addr_of_mut!(out).cast()) };
        assert_eq!(got, 1);
        assert_eq!(out, 11);
        unsafe { gos_rt_chan_drop(chan) };
    }
}
