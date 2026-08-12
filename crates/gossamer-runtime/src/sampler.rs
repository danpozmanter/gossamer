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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Frames recorded per sample. Deep enough to reach through a recursive
/// program's hot region, small enough that the buffer stays a fixed,
/// signal-safe allocation.
pub const MAX_FRAMES: usize = 32;

/// Samples held before the buffer wraps. At 100 Hz this is over two
/// minutes of profiling, and a drain empties it.
const CAPACITY: usize = 16_384;

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

/// The sample ring. A `static mut` rather than a lock: the handler may
/// interrupt any code, including code holding any lock in the process,
/// so it may not take one.
static mut SAMPLES: [RawSample; CAPACITY] = [RawSample::EMPTY; CAPACITY];

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
        // SAFETY: slots below the index the handler published are fully
        // written, and sampling is stopped or the index reset before a
        // drain, so no handler is writing these entries concurrently.
        // Indexed rather than iterated: taking a reference to a `static
        // mut` a signal handler may write is what the raw pointer avoids.
        let sample = unsafe { (&raw const SAMPLES).cast::<RawSample>().add(slot).read() };
        if sample.len > 0 {
            out.push(sample);
        }
    }
    out
}

#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
fn set_timer(hz: u32) -> std::io::Result<()> {
    let interval = if hz == 0 {
        libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        }
    } else {
        libc::timeval {
            tv_sec: 0,
            tv_usec: i64::from(1_000_000 / hz.max(1)),
        }
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
    // SAFETY: `slot` is this handler's exclusive index into the array,
    // claimed by the atomic increment above, and is in bounds.
    unsafe {
        (&raw mut SAMPLES[slot]).write(sample);
    }
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

/// Walks the frame-pointer chain, appending return addresses.
///
/// Every step is validated: the chain must climb, stay aligned, and move
/// by a plausible amount. A frame pointer in a function compiled without
/// one is whatever that function left in the register, so the walk has
/// to treat it as untrusted input.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32")))]
fn walk_frames(mut frame_pointer: usize, sample: &mut RawSample) {
    /// A frame larger than this is a sign the chain is not a chain.
    const MAX_FRAME_BYTES: usize = 1 << 20;

    while (sample.len as usize) < MAX_FRAMES {
        if frame_pointer == 0 || !frame_pointer.is_multiple_of(std::mem::align_of::<usize>()) {
            return;
        }
        // SAFETY: the address is aligned and non-null, and a frame
        // pointer addresses two readable words by the ABI's contract.
        // A corrupt chain that survives the checks above can still fault,
        // which is why the walk stops at the first implausible step
        // rather than trusting an arbitrary depth.
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

/// Heap samples, sharing `RawSample`'s shape.
static mut HEAP_SAMPLES: [RawSample; CAPACITY] = [RawSample::EMPTY; CAPACITY];

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
pub fn record_allocation(bytes: usize) {
    if !HEAP_SAMPLING.load(Ordering::Relaxed) {
        return;
    }
    let total = HEAP_ACCUMULATOR.fetch_add(bytes, Ordering::Relaxed) + bytes;
    if total < HEAP_SAMPLE_BYTES {
        return;
    }
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
    // SAFETY: `slot` is this caller's exclusive index, claimed by the
    // atomic increment, and is in bounds.
    unsafe {
        (&raw mut HEAP_SAMPLES[slot]).write(sample);
    }
}

/// This frame's frame pointer, as the starting point for a walk.
// `inline(always)` because an out-of-line call would make this frame the
// walk's starting point instead of the allocating caller's.
#[cfg(all(unix, not(miri), not(target_arch = "wasm32"), target_arch = "x86_64"))]
#[allow(clippy::inline_always, reason = "the caller's frame is the subject")]
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
}
