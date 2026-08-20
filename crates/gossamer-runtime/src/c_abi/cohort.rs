#![allow(clippy::missing_safety_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]

//! Runtime support for `cohort { }` - structured concurrency on the
//! compiled tiers.
//!
//! A cohort owns the goroutines `spawn`ed while it is the running
//! goroutine's current cohort. The block cannot be left until every one
//! of them has finished, which is the guarantee the construct exists to
//! provide: a child cannot outlive the block that started it.
//!
//! State lives in a process-global registry keyed by a dense `i64` id, so
//! a cohort opened on one carrier resolves from the worker a child lands
//! on. The current cohort is per goroutine rather than per thread: a
//! coroutine migrates between carriers, so the mapping is keyed by `Gid`
//! and falls back to a thread-local for callers that are not goroutines
//! (the main thread, and any plain OS thread).
//!
//! Results are positional. A child is assigned its index when it is
//! registered, at the `spawn` call, and the error a fail-fast cohort
//! reports is the lowest-index failure rather than the first to arrive,
//! so a cohort's answer does not depend on completion order, worker
//! count, or which tier is running it.
//!
//! Cancellation is cooperative and observed as an operation's ordinary
//! "nothing more is coming" answer: a cancelled `recv` reports the same
//! `None` a closed channel does. Nothing is killed, so a cancelled
//! child leaves through its own normal exit path and its `defer` frames
//! and RC releases run in order.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::sched::Gid;

/// Completion policy, as spelled by `Policy::` in source.
pub const POLICY_FAIL_FAST: i64 = 0;
pub const POLICY_COLLECT_ALL: i64 = 1;
pub const POLICY_RACE: i64 = 2;

/// Execution context for children, as spelled by `Context::` in source.
pub const CONTEXT_DEFAULT: i64 = 0;
pub const CONTEXT_ISOLATED: i64 = 1;

/// Whether any cohort has ever been opened in this process. Every
/// cancellation point consults the cohort state, so the common case -
/// a program that uses no cohort at all - must cost one relaxed load.
static ANY_COHORT: AtomicBool = AtomicBool::new(false);

/// Whether this process has opened a cohort. Cancellation points call
/// this before doing any further cohort work.
#[inline]
pub fn any_cohort_live() -> bool {
    ANY_COHORT.load(Ordering::Relaxed)
}

/// Cohorts currently in the cancelled state. `main` runs inside a root
/// cohort, so `any_cohort_live` is true for every program and cannot be
/// the fast path any more; this counter is, because cancellation is rare
/// and a cancellation point that reads zero here has nothing to check.
static CANCELLED_COHORTS: AtomicI64 = AtomicI64::new(0);

/// One child's failure, and whether any joiner ever read it.
struct ChildFailure {
    index: i64,
    message: String,
    /// Set when the child's join handle was joined, so the failure
    /// reached the program rather than vanishing.
    observed: bool,
}

struct CohortState {
    next_index: i64,
    outstanding: i64,
    /// Failures by child index; the lowest index is the reported one.
    failures: Vec<ChildFailure>,
    /// Children that completed without failing.
    successes: i64,
    joined: bool,
    /// Set when the cohort's own deadline fired.
    timed_out: bool,
}

struct Cohort {
    /// Enclosing cohort on the goroutine that opened this one, or 0.
    parent: i64,
    policy: i64,
    context: i64,
    cancelled: AtomicBool,
    state: Mutex<CohortState>,
    /// Signalled whenever `outstanding` or the failure set changes.
    progress: Condvar,
    /// Goroutines parked waiting for this cohort to drain.
    joiners: Mutex<Vec<Gid>>,
    /// Goroutines parked inside a cancellation point under this cohort.
    /// Cancelling drains the list and unparks each, so every one of them
    /// re-checks its own condition.
    waiters: Mutex<Vec<Gid>>,
    /// Cohorts opened under this one, still live. A goroutine parked
    /// inside a nested cohort is on that cohort's waiter list, so
    /// cancelling reaches it by walking down this edge.
    children: Mutex<Vec<i64>>,
}

static COHORTS: LazyLock<Mutex<HashMap<i64, Arc<Cohort>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

/// Current cohort of each goroutine, keyed by `Gid`.
static CURRENT_BY_GID: LazyLock<Mutex<HashMap<u32, i64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    /// Current cohort for a caller that is not a scheduler goroutine -
    /// the main thread, and any isolated child running on its own thread.
    static CURRENT_ON_THREAD: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

fn cohort_at(id: i64) -> Option<Arc<Cohort>> {
    if id == 0 {
        return None;
    }
    COHORTS.lock().get(&id).cloned()
}

/// The running goroutine's current cohort id, or 0.
pub fn current_cohort() -> i64 {
    if !any_cohort_live() {
        return 0;
    }
    match crate::sched_global::current_gid() {
        Some(gid) => CURRENT_BY_GID.lock().get(&gid.0).copied().unwrap_or(0),
        None => CURRENT_ON_THREAD.with(std::cell::Cell::get),
    }
}

fn set_current_cohort(id: i64) {
    match crate::sched_global::current_gid() {
        Some(gid) => {
            let mut map = CURRENT_BY_GID.lock();
            if id == 0 {
                map.remove(&gid.0);
            } else {
                map.insert(gid.0, id);
            }
        }
        None => CURRENT_ON_THREAD.with(|cell| cell.set(id)),
    }
}

/// Whether `id` or any enclosing cohort is cancelled. The chain is
/// walked iteratively so nesting depth costs heap rather than frames.
fn chain_is_cancelled(id: i64) -> bool {
    let mut current = id;
    while current != 0 {
        let Some(node) = cohort_at(current) else {
            // A retired cohort has already been joined; nothing under it
            // has further work to do.
            return true;
        };
        if node.cancelled.load(Ordering::Acquire) {
            return true;
        }
        current = node.parent;
    }
    false
}

/// Whether the running goroutine's cohort chain is cancelled. This is
/// the predicate every cancellation point consults.
pub fn current_is_cancelled() -> bool {
    if CANCELLED_COHORTS.load(Ordering::Relaxed) == 0 {
        return false;
    }
    let id = current_cohort();
    id != 0 && chain_is_cancelled(id)
}

/// Registers `gid` as parked under the running goroutine's cohort, so
/// cancelling it wakes the goroutine to re-check its condition. Returns
/// the cohort the registration landed on, for the matching deregister.
///
/// A cancellation point checks the cohort before it parks, and this
/// registration happens after that check. Cancelling in the gap between
/// the two would find no waiter on record and wake nobody, leaving the
/// goroutine parked on a condition that has already been decided - so a
/// cancellation observed here unparks the goroutine on the spot. The
/// scheduler records a wake that arrives before the coroutine has
/// actually suspended, which is the same guarantee `select` relies on
/// when an arm becomes ready while it is registering.
pub fn register_waiter(gid: Gid) -> i64 {
    let id = current_cohort();
    if let Some(node) = cohort_at(id) {
        node.waiters.lock().push(gid);
    }
    if id != 0 && chain_is_cancelled(id) {
        crate::sched_global::scheduler().unpark(gid);
    }
    id
}

/// Drops `gid` from `id`'s parked set.
pub fn deregister_waiter(id: i64, gid: Gid) {
    if let Some(node) = cohort_at(id) {
        node.waiters.lock().retain(|x| *x != gid);
    }
}

/// Marks `id` cancelled and wakes everything waiting under it: the
/// goroutines parked at a cancellation point, any joiner, and the same
/// again for every cohort nested inside it.
///
/// A goroutine parked inside a nested cohort is registered on that
/// cohort's waiter list, and a parked goroutine cannot consult the
/// parent chain on its own, so the wake has to travel down every edge
/// rather than rely on the walk each cancellation point does before it
/// parks. The descendants are visited iteratively, so nesting depth
/// costs heap rather than frames.
fn cancel(id: i64) {
    let mut pending = vec![id];
    while let Some(current) = pending.pop() {
        let Some(node) = cohort_at(current) else {
            continue;
        };
        {
            // The flag changes and the condvar wake are issued under the
            // lock a joining OS thread holds while it tests `outstanding`,
            // so a cancellation landing between one waiter's test and its
            // wait still reaches it.
            let _state = node.state.lock();
            if node.cancelled.swap(true, Ordering::AcqRel) {
                continue;
            }
            CANCELLED_COHORTS.fetch_add(1, Ordering::AcqRel);
            node.progress.notify_all();
        }
        for gid in std::mem::take(&mut *node.waiters.lock()) {
            crate::sched_global::scheduler().unpark(gid);
        }
        wake_joiners(&node);
        pending.extend(node.children.lock().iter().copied());
    }
}

fn wake_joiners(node: &Cohort) {
    for gid in std::mem::take(&mut *node.joiners.lock()) {
        crate::sched_global::scheduler().unpark(gid);
    }
}

/// Opens a cohort on the running goroutine and makes it current.
fn push(policy: i64, timeout_ms: i64, context: i64) -> i64 {
    ANY_COHORT.store(true, Ordering::Relaxed);
    let parent = current_cohort();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let node = Arc::new(Cohort {
        parent,
        policy,
        context,
        cancelled: AtomicBool::new(false),
        state: Mutex::new(CohortState {
            next_index: 0,
            outstanding: 0,
            failures: Vec::new(),
            successes: 0,
            joined: false,
            timed_out: false,
        }),
        progress: Condvar::new(),
        joiners: Mutex::new(Vec::new()),
        waiters: Mutex::new(Vec::new()),
        children: Mutex::new(Vec::new()),
    });
    COHORTS.lock().insert(id, node);
    if let Some(enclosing) = cohort_at(parent) {
        enclosing.children.lock().push(id);
    }
    set_current_cohort(id);
    // An enclosing cohort cancelled between reading `parent` and linking
    // this one in never reaches the new cohort through that edge, so a
    // cohort opened under a cancelled chain starts cancelled itself.
    if parent != 0 && chain_is_cancelled(parent) {
        cancel(id);
    }
    if timeout_ms > 0 {
        // The deadline rides the scheduler's timer wheel, so a bounded
        // cohort costs an entry there rather than a thread parked on a
        // sleep.
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        let timer_gid = crate::sched_global::add_timer(deadline);
        crate::sched_global::register_waker(
            timer_gid,
            Box::new(move || {
                if let Some(node) = cohort_at(id) {
                    node.state.lock().timed_out = true;
                    cancel(id);
                }
            }),
        );
    }
    id
}

/// Reserves a positional slot for a child about to be spawned into
/// `id`, and counts it as outstanding. Returns the child's index.
pub fn register_child(id: i64) -> i64 {
    let Some(node) = cohort_at(id) else {
        return -1;
    };
    let mut state = node.state.lock();
    let index = state.next_index;
    state.next_index += 1;
    state.outstanding += 1;
    index
}

/// Called on the child goroutine before its body runs.
pub fn enter_child(id: i64) {
    if id != 0 {
        set_current_cohort(id);
    }
}

/// Called on the child goroutine after its body finishes, however it
/// finished. `failure` carries the message when the child panicked or
/// returned an `Err`.
pub fn leave_child(id: i64, index: i64, failure: Option<String>) {
    set_current_cohort(0);
    let Some(node) = cohort_at(id) else {
        return;
    };
    let cancel_now;
    {
        let mut state = node.state.lock();
        state.outstanding -= 1;
        if let Some(message) = failure {
            state.failures.push(ChildFailure {
                index,
                message,
                observed: false,
            });
            // Fail-fast winds the siblings down as soon as one child
            // fails; race does the same as soon as one succeeds.
            cancel_now = node.policy == POLICY_FAIL_FAST;
        } else {
            state.successes += 1;
            cancel_now = node.policy == POLICY_RACE;
        }
        node.progress.notify_all();
    }
    wake_joiners(&node);
    if cancel_now {
        cancel(id);
    }
}

/// Blocks until `id` has no outstanding children. A goroutine gives its
/// carrier back while it waits; a plain OS thread keeps the condvar.
fn wait_for_drain(node: &Arc<Cohort>) {
    loop {
        {
            let state = node.state.lock();
            if state.outstanding <= 0 {
                return;
            }
        }
        if crate::sched_global::current_gid().is_some() {
            let mut parked_as = None;
            let state = node.state.lock();
            if state.outstanding <= 0 {
                return;
            }
            let mut guard = Some(state);
            crate::sched_global::park(crate::sched::ParkReason::Sync, |parker| {
                parked_as = Some(parker.gid);
                node.joiners.lock().push(parker.gid);
                drop(guard.take());
            });
            if let Some(gid) = parked_as {
                node.joiners.lock().retain(|x| *x != gid);
            }
        } else {
            let mut state = node.state.lock();
            if state.outstanding > 0 {
                node.progress.wait(&mut state);
            }
        }
    }
}

/// The cohort's outcome message, or `None` when it succeeded.
fn outcome_message(node: &Arc<Cohort>) -> Option<String> {
    let mut state = node.state.lock();
    state.joined = true;
    // Lowest index first, so the reported failure does not depend on
    // which child happened to finish first.
    state.failures.sort_by_key(|failure| failure.index);
    match node.policy {
        POLICY_COLLECT_ALL if !state.failures.is_empty() => Some(
            state
                .failures
                .iter()
                .map(|failure| failure.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        ),
        POLICY_RACE => {
            if state.successes > 0 {
                None
            } else {
                state
                    .failures
                    .first()
                    .map(|failure| failure.message.clone())
                    .or_else(|| state.timed_out.then(|| "cohort timed out".to_string()))
            }
        }
        _ => state
            .failures
            .first()
            .map(|failure| failure.message.clone())
            .or_else(|| state.timed_out.then(|| "cohort timed out".to_string())),
    }
}

/// Joins the running goroutine's cohort: waits for every child, then
/// reports the cohort's outcome. Leaves the cohort current, so the
/// matching pop still runs.
fn join_current() -> Option<String> {
    let id = current_cohort();
    let node = cohort_at(id)?;
    wait_for_drain(&node);
    outcome_message(&node)
}

/// Closes the running goroutine's cohort: cancels anything still
/// running, waits for it, retires the cohort, and restores the
/// enclosing one. Runs from the block's `defer`, so it covers every
/// exit edge - including a `return` or a `?` out of the middle of the
/// block.
fn pop_current() {
    let id = current_cohort();
    let Some(node) = cohort_at(id) else {
        return;
    };
    let already_joined = node.state.lock().joined;
    if !already_joined {
        cancel(id);
        wait_for_drain(&node);
    }
    set_current_cohort(node.parent);
    if node.cancelled.load(Ordering::Acquire) {
        CANCELLED_COHORTS.fetch_sub(1, Ordering::AcqRel);
    }
    // A handle nobody joined has no one left to mark it observed, so its
    // entry retires with the cohort rather than living for the process.
    CHILD_HANDLES.lock().retain(|_, (cohort, _)| *cohort != id);
    if let Some(enclosing) = cohort_at(node.parent) {
        enclosing.children.lock().retain(|child| *child != id);
    }
    COHORTS.lock().remove(&id);
}

/// Closes every cohort still open on the running goroutine. The
/// goroutine spawn wrappers call this as their last act, so a body that
/// left through a path `defer` does not cover - a panic - still cannot
/// leave its children running.
pub fn unwind_open_cohorts() {
    if !any_cohort_live() {
        return;
    }
    while current_cohort() != 0 {
        pop_current();
    }
}

/// `runtime::cohort_push(policy, timeout_ms, context)` - opens a cohort.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_cohort_push(policy: i64, timeout_ms: i64, context: i64) -> i64 {
    ffi_entry!(0, { push(policy, timeout_ms, context) })
}

/// `runtime::cohort_join()` - waits for the cohort's children and
/// answers `Result<(), errors::Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_cohort_join() -> i128 {
    ffi_entry!(super::vec::pack_result(0, 0), {
        match join_current() {
            None => super::vec::pack_result(0, 0),
            Some(message) => {
                let err = super::errors::error_new_from_bytes(message.as_bytes());
                super::vec::pack_result(1, err as i64)
            }
        }
    })
}

/// `runtime::cohort_pop()` - closes the cohort.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_cohort_pop() {
    ffi_entry!((), { pop_current() });
}

/// `runtime::cohort_cancelled()` - whether the running goroutine's
/// cohort has been cancelled. Source-visible so a CPU-bound child can
/// cooperate at a point of its own choosing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_cohort_cancelled() -> i64 {
    ffi_entry!(0, { i64::from(current_is_cancelled()) })
}

/// `runtime::cohort_cancel()` - cancels the running goroutine's cohort
/// from inside it, so a child that finds its own answer can wind the
/// others down without failing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_cohort_cancel() {
    ffi_entry!((), {
        let id = current_cohort();
        if id != 0 {
            cancel(id);
        }
    });
}

/// Whether children of the running goroutine's cohort run on dedicated
/// OS threads. Read by the spawn path.
pub fn current_context() -> i64 {
    let id = current_cohort();
    let mut current = id;
    while current != 0 {
        let Some(node) = cohort_at(current) else {
            return CONTEXT_DEFAULT;
        };
        if node.context != CONTEXT_DEFAULT {
            return node.context;
        }
        current = node.parent;
    }
    CONTEXT_DEFAULT
}

/// The join handle of every cohort child, by handle address, so joining
/// a handle can mark that child's failure as one the program saw.
static CHILD_HANDLES: LazyLock<Mutex<HashMap<usize, (i64, i64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records which cohort child a join handle belongs to.
pub fn note_child_handle(handle: usize, cohort: i64, index: i64) {
    if handle == 0 || cohort == 0 {
        return;
    }
    CHILD_HANDLES.lock().insert(handle, (cohort, index));
}

/// Marks the child behind `handle` as observed: its outcome reached the
/// program, so a failure it reported is not an orphaned one.
pub fn mark_handle_observed(handle: usize) {
    if handle == 0 {
        return;
    }
    let entry = CHILD_HANDLES.lock().remove(&handle);
    let Some((cohort, index)) = entry else {
        return;
    };
    if let Some(node) = cohort_at(cohort) {
        let mut state = node.state.lock();
        for failure in &mut state.failures {
            if failure.index == index {
                failure.observed = true;
            }
        }
    }
}

/// Opens the process-wide root cohort that `main` runs inside.
///
/// Every `spawn` is a child of some cohort, so a goroutine cannot outlive
/// the program and a failure cannot vanish unread. The root's policy is
/// collect-all: it exists to bound lifetimes and surface failures, not to
/// impose fail-fast on a program that never asked for it, so one child's
/// `Err` never cancels another's work.
pub fn open_root() {
    if current_cohort() != 0 {
        return;
    }
    push(POLICY_COLLECT_ALL, 0, CONTEXT_DEFAULT);
}

/// Closes the root cohort: waits for every child `main` left running, then
/// reports any failure that nothing in the program ever read.
///
/// A failure whose join handle was joined has already reached the program
/// through that handle, so only the unobserved ones are reported here.
pub fn close_root() {
    let id = current_cohort();
    let Some(node) = cohort_at(id) else {
        return;
    };
    wait_for_drain(&node);
    let orphaned: Vec<String> = {
        let state = node.state.lock();
        state
            .failures
            .iter()
            .filter(|failure| !failure.observed)
            .map(|failure| failure.message.clone())
            .collect()
    };
    for message in orphaned {
        eprintln!("gossamer: spawned goroutine failed with nobody to observe it: {message}");
    }
    node.state.lock().joined = true;
    pop_current();
}

/// Renders a panic payload the way the join handle does, for a child
/// whose failure reaches the cohort through the unwinding guard.
pub fn panic_failure_message(raw: Option<String>) -> String {
    raw.unwrap_or_else(|| "spawned goroutine panicked".to_string())
}

/// Runs an isolated child on an OS thread of its own, for the whole of
/// its life. Blocking there stalls nothing else, which is the point of
/// the isolated context: synchronous Rust and CPU-bound work have
/// somewhere to run that no other goroutine shares.
#[cfg(target_arch = "wasm32")]
pub fn spawn_isolated(body: Box<dyn FnOnce() + Send + 'static>) {
    // wasm32 has one thread, so isolation has nowhere to go: the body
    // runs where every other goroutine runs.
    crate::sched_global::spawn(body);
}

#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_isolated(body: Box<dyn FnOnce() + Send + 'static>) {
    let cell = Arc::new(Mutex::new(Some(body)));
    let on_thread = Arc::clone(&cell);
    let spawned = std::thread::Builder::new()
        .name("gos-isolated".to_string())
        .stack_size(ISOLATED_STACK_BYTES)
        .spawn(move || {
            gossamer_coro::arm_stack_guard(
                ISOLATED_STACK_BYTES - gossamer_coro::STACK_GUARD_MARGIN,
            );
            let body = on_thread.lock().take();
            if let Some(body) = body {
                body();
            }
        });
    if spawned.is_err() {
        // The OS refused a thread. The child still has to run, and a
        // shared carrier is the only place left for it.
        let body = cell.lock().take();
        if let Some(body) = body {
            crate::sched_global::spawn(body);
        }
    }
}

/// Stack reserve for an isolated child. It runs the same bodies a
/// goroutine does, so it gets the same reserve a scheduler carrier has.
#[cfg(not(target_arch = "wasm32"))]
const ISOLATED_STACK_BYTES: usize = 8 * 1024 * 1024;
