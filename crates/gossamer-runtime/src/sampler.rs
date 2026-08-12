//! Signal-driven CPU sampler.
//!
//! A profiler cannot instrument: a runtime call at every function entry
//! costs 2.7x on call-heavy code, because it is an inlining barrier.
//! Instead a timer interrupts the running thread and the handler reads
//! the stack that is already there, which costs nothing until it fires.
//!
//! Everything the handler touches is async-signal-safe: it writes raw
//! program counters into a fixed array under an atomic index, allocating
//! nothing and taking no lock. Turning those addresses into names is
//! done by whoever drains the buffer, where allocation is allowed.
//!
//! The stack walk follows the frame-pointer chain, which is why compiled
//! bodies carry `"frame-pointer"="all"`. DWARF unwinding would be more
//! precise and is not async-signal-safe.

#![allow(clippy::missing_safety_doc)]

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Frames recorded per sample. Deep enough to reach through a recursive
/// program's hot region, small enough that the buffer stays a fixed,
/// signal-safe allocation.
pub const MAX_FRAMES: usize = 32;

/// Samples held before the buffer stops accepting more. At 100 Hz this
/// is 40 seconds, and each `cpu_profile` call drains it.
///
/// The storage is a zeroed static, so it costs address space rather than
/// resident memory until a sample is actually written - but two rings at
/// this size is already about 2 MiB in every binary, which is why it is
/// sized to a profiling window rather than to the largest one imaginable.
const CAPACITY: usize = 4_096;

/// One captured stack, as raw return addresses.
#[derive(Clone, Copy)]
pub struct RawSample {
    /// Return addresses, innermost first.
    pub frames: [usize; MAX_FRAMES],
    /// How many of `frames` are populated.
    pub len: u8,
}

impl RawSample {
    const EMPTY: Self = Self {
        frames: [0; MAX_FRAMES],
        len: 0,
    };
}

/// Fixed sample storage shared between a recorder and a drainer.
///
/// Not a lock: the recorder can be a signal handler, which may interrupt
/// code holding any lock in the process and so may not take one. Access
/// is serialized by the paired index instead - a recorder writes only the
/// slot its `fetch_add` claimed, and a drainer reads only slots below an
/// index it has swapped to zero.
struct SampleRing {
    slots: UnsafeCell<[RawSample; CAPACITY]>,
}

// SAFETY: the index protocol above gives every access a slot no other
// thread is touching; the cell only removes the compiler's aliasing
// assumption, it does not add sharing.
unsafe impl Sync for SampleRing {}

impl SampleRing {
    // The array lives in a static, not on a stack: `const fn` initialises
    // the storage in place.
    #[allow(
        clippy::large_stack_arrays,
        reason = "initialises a static, not a local"
    )]
    const fn new() -> Self {
        Self {
            slots: UnsafeCell::new([RawSample::EMPTY; CAPACITY]),
        }
    }

    /// Writes `sample` into `slot`, which the caller has claimed.
    fn write(&self, slot: usize, sample: &RawSample) {
        if slot >= CAPACITY {
            return;
        }
        // SAFETY: `slot` was claimed by the caller's `fetch_add` and is
        // in bounds, so no other writer addresses it.
        unsafe {
            self.slots
                .get()
                .cast::<RawSample>()
                .add(slot)
                .write(*sample);
        }
    }

    /// Reads `slot`, which no recorder is writing.
    fn read(&self, slot: usize) -> RawSample {
        // SAFETY: the drainer reset the index before reading, so slots
        // below `taken` are complete and unclaimed.
        unsafe { self.slots.get().cast::<RawSample>().add(slot).read() }
    }
}

/// The CPU sample ring.
static SAMPLES: SampleRing = SampleRing::new();

/// Next slot to write. Wraps; a drain reads up to the current index.
static WRITE_INDEX: AtomicUsize = AtomicUsize::new(0);

/// Whether the timer is armed. The handler returns immediately when it
/// is not, so a signal delivered during teardown records nothing.
static SAMPLING: AtomicBool = AtomicBool::new(false);

/// Whether sampling is currently armed.
#[must_use]
pub fn is_sampling() -> bool {
    SAMPLING.load(Ordering::Relaxed)
}

/// Starts sampling at `hz`, replacing any previous rate.
///
/// # Errors
///
/// Returns the OS error when the timer or handler cannot be installed.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
pub fn start(hz: u32) -> std::io::Result<()> {
    install_handler()?;
    WRITE_INDEX.store(0, Ordering::SeqCst);
    SAMPLING.store(true, Ordering::SeqCst);
    set_timer(hz)
}

/// Sampling is a no-op where there is no timer signal to drive it.
#[cfg(not(all(unix, not(miri), not(target_arch = "wasm32"))))]
pub fn start(_hz: u32) -> std::io::Result<()> {
    Ok(())
}

/// Stops sampling and disarms the timer.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
pub fn stop() {
    SAMPLING.store(false, Ordering::SeqCst);
    let _ = set_timer(0);
}

/// Stopping is a no-op where there is no timer to disarm.
#[cfg(not(all(unix, not(miri), not(target_arch = "wasm32"))))]
pub fn stop() {}

/// Takes everything recorded since the last drain.
#[must_use]
pub fn drain() -> Vec<RawSample> {
    let taken = WRITE_INDEX.swap(0, Ordering::SeqCst).min(CAPACITY);
    let mut out = Vec::with_capacity(taken);
    for slot in 0..taken {
        let sample = SAMPLES.read(slot);
        if sample.len > 0 {
            out.push(sample);
        }
    }
    out
}

#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
fn set_timer(hz: u32) -> std::io::Result<()> {
    // Written without naming `suseconds_t`: it is `i64` on glibc, `i32` on
    // Darwin, and deprecated on musl. An `i32` holds any microsecond count
    // this produces (at most 1_000_000), and `into()` widens it to
    // whichever width the platform's `tv_usec` actually is.
    let micros: i32 = i32::try_from(1_000_000 / hz.max(1)).unwrap_or(10_000);
    let interval = libc::timeval {
        tv_sec: 0,
        tv_usec: if hz == 0 { 0 } else { micros }.into(),
    };
    let spec = libc::itimerval {
        it_interval: interval,
        it_value: interval,
    };
    // SAFETY: `spec` is a fully-initialised itimerval we own; ITIMER_PROF
    // is a valid timer id.
    let rc = unsafe { libc::setitimer(libc::ITIMER_PROF, &raw const spec, std::ptr::null_mut()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
fn install_handler() -> std::io::Result<()> {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| {
        // SAFETY: zero-initialising a sigaction is valid; the mask is
        // emptied immediately below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_sigprof as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART | libc::SA_ONSTACK;
        // SAFETY: action is a stack-local sigaction we own.
        unsafe {
            libc::sigemptyset(&raw mut action.sa_mask);
            if libc::sigaction(libc::SIGPROF, &raw const action, std::ptr::null_mut()) != 0 {
                result = Err(std::io::Error::last_os_error());
            }
        }
    });
    result
}

/// SIGPROF handler. Async-signal-safe: no allocation, no locks, no
/// library calls beyond reading memory it has bounds-checked.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
extern "C" fn on_sigprof(
    _sig: libc::c_int,
    _info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    if !SAMPLING.load(Ordering::Relaxed) {
        return;
    }
    let mut sample = RawSample::EMPTY;
    let (pc, frame_pointer) = interrupted_registers(context);
    if pc != 0 {
        sample.frames[0] = pc;
        sample.len = 1;
    }
    walk_frames(frame_pointer, &mut sample);
    if sample.len == 0 {
        return;
    }
    let slot = WRITE_INDEX.fetch_add(1, Ordering::SeqCst);
    if slot >= CAPACITY {
        // Buffer full until the next drain. Dropping is better than
        // wrapping over samples the drainer is about to read.
        WRITE_INDEX.store(CAPACITY, Ordering::SeqCst);
        return;
    }
    SAMPLES.write(slot, &sample);
}

/// Program counter and frame pointer of the interrupted context.
#[cfg(all(
    unix,
    not(miri),
    not(target_arch = "wasm32"),
    target_arch = "x86_64",
    target_os = "linux"
))]
fn interrupted_registers(context: *mut libc::c_void) -> (usize, usize) {
    if context.is_null() {
        return (0, 0);
    }
    // SAFETY: the kernel hands a `ucontext_t` to an SA_SIGINFO handler.
    let ctx = unsafe { &*(context.cast::<libc::ucontext_t>()) };
    let regs = &ctx.uc_mcontext.gregs;
    (
        regs[libc::REG_RIP as usize] as usize,
        regs[libc::REG_RBP as usize] as usize,
    )
}

/// Other platforms record the frames the walk can reach without the
/// interrupted register file.
#[cfg(all(
    unix,
    not(miri),
    not(target_arch = "wasm32"),
    not(all(target_arch = "x86_64", target_os = "linux"))
))]
fn interrupted_registers(_context: *mut libc::c_void) -> (usize, usize) {
    (0, 0)
}

/// `(lo, hi)` bounds of the stack the calling code is running on, cached
/// per thread.
///
/// A goroutine runs on its own allocated stack, so its window comes from
/// the coroutine guard rather than from the OS thread. That window changes
/// as goroutines are scheduled onto a worker, so only the OS-thread bounds
/// - fixed for the life of the thread - are worth caching.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
fn current_stack_bounds() -> Option<(usize, usize)> {
    thread_local! {
        static THREAD_STACK: std::cell::Cell<Option<(usize, usize)>> =
            const { std::cell::Cell::new(None) };
    }
    if let Some(window) = gossamer_coro::goroutine_stack_bounds() {
        return Some(window);
    }
    // `Cell<Option<_>>` distinguishes "not yet asked" from "the platform
    // cannot tell us", so a platform that cannot report bounds is asked
    // once per thread rather than on every sample.
    THREAD_STACK.with(|cached| {
        if let Some(window) = cached.get() {
            return (window.1 > window.0).then_some(window);
        }
        let window = crate::stack_guard::current_thread_stack_bounds();
        cached.set(Some(window.unwrap_or((0, 0))));
        window
    })
}

/// Walks the frame-pointer chain, appending return addresses.
///
/// Every step is validated against the stack the walk is running on: a
/// frame pointer in a function compiled without one is whatever that
/// function left in the register, so every link is untrusted input and
/// must be proven to address readable stack memory before it is read.
/// Without bounds the walk cannot be made safe, so it does not run.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
fn walk_frames(mut frame_pointer: usize, sample: &mut RawSample) {
    /// A frame larger than this is a sign the chain is not a chain.
    const MAX_FRAME_BYTES: usize = 1 << 20;

    let Some((lo, hi)) = current_stack_bounds() else {
        return;
    };
    // Both words of the link must lie inside the window.
    let readable = |fp: usize| {
        fp != 0
            && fp.is_multiple_of(std::mem::align_of::<usize>())
            && fp >= lo
            && fp.saturating_add(2 * std::mem::size_of::<usize>()) <= hi
    };

    while (sample.len as usize) < MAX_FRAMES {
        if !readable(frame_pointer) {
            return;
        }
        // SAFETY: `readable` proved the address is aligned and that both
        // words it addresses lie inside the running stack's mapped bounds.
        let (saved_fp, return_address) = unsafe {
            let base = frame_pointer as *const usize;
            (base.read_volatile(), base.add(1).read_volatile())
        };
        if return_address == 0 {
            return;
        }
        sample.frames[sample.len as usize] = return_address;
        sample.len += 1;
        // The chain climbs toward the stack base by a bounded amount.
        if saved_fp <= frame_pointer || saved_fp - frame_pointer > MAX_FRAME_BYTES {
            return;
        }
        frame_pointer = saved_fp;
    }
}

#[cfg(not(all(unix, not(miri), not(target_arch = "wasm32"))))]
fn walk_frames(_frame_pointer: usize, _sample: &mut RawSample) {}

/// Bytes allocated between heap samples. Go's default; large enough that
/// the counter check is lost in allocation cost, small enough that a
/// profile of a short program is not empty.
pub const HEAP_SAMPLE_BYTES: usize = 512 * 1024;

/// Whether heap sampling is armed.
static HEAP_SAMPLING: AtomicBool = AtomicBool::new(false);

/// Bytes allocated since the last heap sample.
static HEAP_ACCUMULATOR: AtomicUsize = AtomicUsize::new(0);

/// The allocation sample ring.
static HEAP_SAMPLES: SampleRing = SampleRing::new();

/// Next heap slot to write.
static HEAP_INDEX: AtomicUsize = AtomicUsize::new(0);

/// Arms allocation sampling.
pub fn start_heap() {
    HEAP_INDEX.store(0, Ordering::SeqCst);
    HEAP_ACCUMULATOR.store(0, Ordering::SeqCst);
    HEAP_SAMPLING.store(true, Ordering::SeqCst);
}

/// Disarms allocation sampling.
pub fn stop_heap() {
    HEAP_SAMPLING.store(false, Ordering::SeqCst);
}

/// Takes every heap sample recorded since the last drain.
#[must_use]
pub fn drain_heap() -> Vec<RawSample> {
    let taken = HEAP_INDEX.swap(0, Ordering::SeqCst).min(CAPACITY);
    let mut out = Vec::with_capacity(taken);
    for slot in 0..taken {
        // SAFETY: as for `drain` - slots below the published index are
        // fully written and no recorder is writing them concurrently.
        let sample = unsafe {
            (&raw const HEAP_SAMPLES)
                .cast::<RawSample>()
                .add(slot)
                .read()
        };
        if sample.len > 0 {
            out.push(sample);
        }
    }
    out
}

/// Accounts `bytes` against the sample threshold, recording a stack when
/// it is crossed.
///
/// Called from the global allocator, so it must not allocate: doing so
/// would re-enter the allocator that called it. The frame walk writes
/// into a fixed array for exactly that reason.
///
/// `#[inline]` with the recording body out of line so a disarmed process
/// pays a relaxed load and a predicted branch inside `__rust_alloc`
/// rather than a call per allocation.
#[inline]
pub fn record_allocation(bytes: usize) {
    if !HEAP_SAMPLING.load(Ordering::Relaxed) {
        return;
    }
    let total = HEAP_ACCUMULATOR.fetch_add(bytes, Ordering::Relaxed) + bytes;
    if total < HEAP_SAMPLE_BYTES {
        return;
    }
    capture_heap_sample();
}

/// Out of line so the walk starts from a frame the ABI guarantees has a
/// frame pointer. Read inside the inlined caller, `rbp` is whatever
/// `__rust_alloc` left there, which is not a frame chain.
#[inline(never)]
fn capture_heap_sample() {
    HEAP_ACCUMULATOR.store(0, Ordering::Relaxed);
    let mut sample = RawSample::EMPTY;
    walk_frames(current_frame_pointer(), &mut sample);
    if sample.len == 0 {
        return;
    }
    let slot = HEAP_INDEX.fetch_add(1, Ordering::SeqCst);
    if slot >= CAPACITY {
        HEAP_INDEX.store(CAPACITY, Ordering::SeqCst);
        return;
    }
    HEAP_SAMPLES.write(slot, &sample);
}

/// The running frame pointer, as the starting point for a walk.
///
/// `inline(always)`: Rust code that keeps no frame pointer of its own also
/// does not clobber `rbp`, so the register still holds the innermost frame
/// that does keep one - a compiled Gossamer body, which carries
/// `"frame-pointer"="all"`. Reading it from an out-of-line frame instead
/// would read that frame's own `rbp`, which such a function never
/// establishes, and the walk would start from a register holding
/// unrelated data.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32"), target_arch = "x86_64"))]
#[allow(clippy::inline_always, reason = "reads the caller's live register")]
#[inline(always)]
fn current_frame_pointer() -> usize {
    let fp: usize;
    // SAFETY: reading the frame-pointer register clobbers nothing.
    unsafe {
        std::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    fp
}

#[cfg(not(all(unix, not(miri), not(target_arch = "wasm32"), target_arch = "x86_64")))]
fn current_frame_pointer() -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draining_an_idle_sampler_yields_nothing() {
        stop();
        let _ = drain();
        assert!(drain().is_empty());
    }

    /// The walk treats the chain as untrusted: a null or misaligned
    /// frame pointer ends it rather than dereferencing.
    #[test]
    fn the_walk_refuses_an_implausible_chain() {
        let mut sample = RawSample::EMPTY;
        walk_frames(0, &mut sample);
        assert_eq!(sample.len, 0);
        walk_frames(1, &mut sample);
        assert_eq!(sample.len, 0, "a misaligned frame pointer is not walked");
    }

    /// Armed sampling records a stack once the accumulator crosses the
    /// interval. Guards the split between the inlined arm check and the
    /// out-of-line recorder: the recorder owns the frame-pointer read, so
    /// the walk starts from a frame that has one.
    #[test]
    fn armed_sampling_records_a_stack_with_frames() {
        stop_heap();
        let _ = drain_heap();
        start_heap();
        record_allocation(HEAP_SAMPLE_BYTES);
        record_allocation(HEAP_SAMPLE_BYTES);
        stop_heap();
        let samples = drain_heap();
        assert!(
            !samples.is_empty(),
            "an armed sampler records past the interval"
        );
        assert!(
            samples.iter().all(|s| s.len > 0),
            "every recorded sample carries at least one frame"
        );
    }

    /// A disarmed sampler is the default and records nothing, so a program
    /// that never asks for a heap profile keeps an empty ring.
    #[test]
    fn a_disarmed_sampler_records_nothing() {
        stop_heap();
        let _ = drain_heap();
        record_allocation(HEAP_SAMPLE_BYTES * 4);
        assert!(drain_heap().is_empty());
    }
}
