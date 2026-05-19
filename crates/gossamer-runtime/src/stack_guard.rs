//! Stack-overflow guard.
//!
//! Recursive Gossamer code can blow the OS stack. Without a guard
//! the kernel delivers SIGSEGV on the bottom guard page and the
//! process dies silently — there is no chance to print a useful
//! diagnostic because the signal handler itself would run on the
//! already-exhausted stack and trigger a second fault.
//!
//! [`install_stack_guard`] fixes that on Unix by installing an
//! alternate signal stack via `sigaltstack(2)` and a
//! `SA_ONSTACK | SA_SIGINFO` SIGSEGV handler. The handler checks
//! the faulting address against the calling thread's known stack
//! bounds; if the fault is within a guard-page-sized neighbourhood
//! of the stack bottom it prints a structured stack-overflow
//! message via a raw `write(2)` syscall and calls
//! [`std::process::abort`]. Otherwise it restores the default
//! disposition and re-raises so the original SIGSEGV is observed
//! by the parent process / debugger.
//!
//! On Windows the same job is done by `SetThreadStackGuarantee` +
//! `SetUnhandledExceptionFilter`, which fires on
//! `EXCEPTION_STACK_OVERFLOW` from within a 64 KiB reserved tail
//! that the kernel keeps available for the filter to execute on.
//!
//! Every entry point in this module is async-signal-safe: the
//! Unix handler touches only the `libc::write` syscall, `itoa` on
//! a stack-resident scratch buffer, and `std::process::abort`. No
//! allocations, no locks, no Rust-side I/O.

#![allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]

/// Bytes reserved for the alternate signal stack. `SIGSTKSZ` on
/// glibc is 8 KiB which is too tight once symbolication is added;
/// 64 KiB matches Go's `sigaltstack` size and leaves room for the
/// signal-safe write path even on architectures with deep ABI
/// frames.
pub const ALT_STACK_BYTES: usize = 64 * 1024;

/// Distance from the bottom of the thread's stack within which a
/// fault is attributed to stack overflow. Linux's default guard is
/// one page (4 KiB); we widen to 16 KiB to absorb compiler-emitted
/// red-zone manipulation and to be robust against architectures
/// with larger pages.
pub const STACK_GUARD_PROXIMITY: usize = 16 * 1024;

/// Installs the per-thread stack-overflow guard. Idempotent on a
/// single thread; the scheduler calls this once at the start of
/// every worker. The main thread should also call it from program
/// entry.
pub fn install_stack_guard() {
    #[cfg(unix)]
    unix::install();

    #[cfg(windows)]
    windows::install();

    #[cfg(not(any(unix, windows)))]
    {
        // Other targets (notably wasm32) cannot deliver SIGSEGV;
        // stack exhaustion there manifests as a trap that the host
        // surfaces without our help.
    }
}

#[cfg(unix)]
mod unix {
    use std::cell::Cell;
    use std::mem::MaybeUninit;
    use std::sync::Once;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{ALT_STACK_BYTES, STACK_GUARD_PROXIMITY};

    // Per-thread alternate-signal stack. Kept thread-local so the
    // kernel always has a valid `ss_sp` for the calling thread —
    // `sigaltstack` is a per-thread setting, not process-wide.
    thread_local! {
        static ALT_STACK: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
        static STACK_LO: Cell<usize> = const { Cell::new(0) };
        static STACK_HI: Cell<usize> = const { Cell::new(0) };
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }

    // The `sigaction(SIGSEGV, ...)` installation is process-wide;
    // we only want to do it once. `sigaltstack` separately needs
    // running on every thread.
    static ACTION_INSTALLED: AtomicBool = AtomicBool::new(false);
    static ACTION_ONCE: Once = Once::new();

    pub(super) fn install() {
        // AddressSanitizer installs its own SIGSEGV handler + alt
        // signal stack to diagnose stack overflow itself. Installing
        // ours on top conflicts with ASan's runtime — when the test
        // process exits, ASan tries to munmap memory whose layout
        // our sigaltstack call disturbed, producing the
        // "Failed to munmap" abort in the sanitizers job. Detect
        // ASan via its standard option-variable contract (any ASan-
        // instrumented program reads `ASAN_OPTIONS` at startup; CI
        // sets it explicitly) and skip — ASan's diagnostics are
        // strictly better than ours for this case.
        if std::env::var_os("ASAN_OPTIONS").is_some() {
            return;
        }
        INSTALLED.with(|cell| {
            if cell.get() {
                return;
            }
            install_alt_stack();
            install_sigaction();
            record_stack_bounds();
            cell.set(true);
        });
    }

    fn install_alt_stack() {
        // SAFETY: `Box::into_raw` returns a valid heap allocation
        // of `ALT_STACK_BYTES`. The pointer is parked thread-local
        // and never freed — the thread either exits (kernel reaps
        // the alt stack first via SS_DISABLE on thread exit) or
        // lives until process abort.
        let buf: Box<[u8]> = vec![0_u8; ALT_STACK_BYTES].into_boxed_slice();
        let raw = Box::into_raw(buf).cast::<u8>();
        ALT_STACK.with(|cell| cell.set(raw));

        let ss = libc::stack_t {
            ss_sp: raw.cast::<libc::c_void>(),
            ss_flags: 0,
            ss_size: ALT_STACK_BYTES,
        };
        // SAFETY: `ss` is a valid `stack_t` describing an owned
        // buffer; the second argument is null to discard the old
        // alternate stack (we never need to restore it).
        let _ = unsafe { libc::sigaltstack(&raw const ss, std::ptr::null_mut()) };
    }

    fn install_sigaction() {
        ACTION_ONCE.call_once(|| {
            let mut action: libc::sigaction = unsafe { MaybeUninit::zeroed().assume_init() };
            // SAFETY: zero-init `sigaction` is a defined initial
            // state across every supported libc; we set the fields
            // we care about below.
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESTART;
            action.sa_sigaction = sigsegv_handler as *const () as usize;
            unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
            // SAFETY: `action` is fully initialised; passing null
            // for the old action is the standard "don't care"
            // call.
            let rc =
                unsafe { libc::sigaction(libc::SIGSEGV, &raw const action, std::ptr::null_mut()) };
            if rc == 0 {
                ACTION_INSTALLED.store(true, Ordering::Release);
            }
        });
    }

    // Records [lo, hi) byte range of the calling thread's stack
    // into thread-local storage. Best effort: if discovery fails
    // the handler simply propagates the signal instead of
    // diagnosing overflow.
    fn record_stack_bounds() {
        let (lo, hi) = discover_stack_bounds();
        STACK_LO.with(|c| c.set(lo));
        STACK_HI.with(|c| c.set(hi));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn discover_stack_bounds() -> (usize, usize) {
        // SAFETY: pthread_getattr_np / pthread_attr_getstack /
        // pthread_attr_destroy are documented async-signal-safe
        // wrappers over the current thread's stack metadata. The
        // attr is zero-initialised before the get call; the get
        // call populates sp/size; destroy releases attr resources.
        // All raw pointers we dereference (&attr, &sp, &size) are
        // stack-locals owned by this fn.
        unsafe {
            let mut attr: libc::pthread_attr_t = MaybeUninit::zeroed().assume_init();
            if libc::pthread_getattr_np(libc::pthread_self(), &raw mut attr) != 0 {
                return (0, 0);
            }
            let mut sp: *mut libc::c_void = std::ptr::null_mut();
            let mut size: libc::size_t = 0;
            let rc = libc::pthread_attr_getstack(&raw const attr, &raw mut sp, &raw mut size);
            libc::pthread_attr_destroy(&raw mut attr);
            if rc != 0 || sp.is_null() {
                return (0, 0);
            }
            let lo = sp as usize;
            (lo, lo + size)
        }
    }

    #[cfg(target_os = "macos")]
    fn discover_stack_bounds() -> (usize, usize) {
        // Darwin uses pthread_get_stackaddr_np / _get_stacksize_np.
        // `stackaddr` is the high end of the stack (grows down).
        // SAFETY: both calls are documented as safe for the
        // current thread; they read libpthread-internal state and
        // return scalars.
        unsafe {
            let me = libc::pthread_self();
            let hi = libc::pthread_get_stackaddr_np(me) as usize;
            let size = libc::pthread_get_stacksize_np(me) as usize;
            if hi == 0 || size == 0 {
                return (0, 0);
            }
            (hi.saturating_sub(size), hi)
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    fn discover_stack_bounds() -> (usize, usize) {
        // Other Unix (FreeBSD, illumos, etc.): no portable bounds
        // discovery. The handler still installs but every fault is
        // forwarded unmodified — equivalent to the pre-guard
        // behaviour but with the alt stack in place so the kernel
        // doesn't double-fault.
        (0, 0)
    }

    extern "C" fn sigsegv_handler(
        sig: libc::c_int,
        info: *mut libc::siginfo_t,
        _ctx: *mut libc::c_void,
    ) {
        // The handler runs on the alternate signal stack
        // (`SA_ONSTACK`). Only async-signal-safe operations are
        // permitted: raw syscalls, atomics, and stack-only state.
        let fault_addr = if info.is_null() {
            0
        } else {
            // SAFETY: kernel guarantees `info` is non-null when
            // SA_SIGINFO is set. `si_addr` is a populated field
            // across every supported libc.
            unsafe { (*info).si_addr() as usize }
        };
        if is_stack_overflow(fault_addr) {
            report_overflow_and_abort(fault_addr);
        }
        // Not a stack overflow: restore the default disposition
        // and re-raise so the original SIGSEGV is observed.
        propagate_signal(sig);
    }

    fn is_stack_overflow(addr: usize) -> bool {
        if addr == 0 {
            return false;
        }
        let lo = STACK_LO.with(Cell::get);
        let hi = STACK_HI.with(Cell::get);
        if lo == 0 || hi == 0 || hi <= lo {
            return false;
        }
        // Faults inside the stack range are an upper-stack write
        // gone wrong; only faults near (or just below) the bottom
        // guard page count as overflow.
        let guard_top = lo;
        let guard_bottom = lo.saturating_sub(STACK_GUARD_PROXIMITY);
        addr < guard_top && addr >= guard_bottom
    }

    fn report_overflow_and_abort(addr: usize) -> ! {
        // Compose the message on a stack scratch buffer. We can't
        // use `format!` or `eprintln!` — both allocate / take
        // locks. `itoa::Buffer` writes into a stack-resident
        // array.
        let prefix = b"gossamer: stack overflow at 0x";
        let suffix = b"; aborting\n";
        let mut scratch = [0_u8; 96];
        let mut len = 0;
        len += copy_into(&mut scratch[len..], prefix);
        len += hex_into(&mut scratch[len..], addr);
        len += copy_into(&mut scratch[len..], suffix);
        // SAFETY: `write(2)` is async-signal-safe. fd 2 is
        // stderr; `scratch` outlives the call.
        let _ = unsafe {
            libc::write(
                libc::STDERR_FILENO,
                scratch.as_ptr().cast::<libc::c_void>(),
                len,
            )
        };
        // `_exit(134)` would also work; abort gives core-dump
        // semantics matching the underlying SIGSEGV.
        std::process::abort();
    }

    fn propagate_signal(sig: libc::c_int) {
        // Restore SIG_DFL and re-raise so the parent / debugger
        // sees the original signal.
        // SAFETY: zero-initialising a POSIX sigaction is valid; the
        // struct's only invariant is sa_mask being a well-formed
        // sigset, which sigemptyset establishes immediately after.
        let mut action: libc::sigaction = unsafe { MaybeUninit::zeroed().assume_init() };
        action.sa_sigaction = libc::SIG_DFL;
        // SAFETY: action is a stack-local sigaction we own; the
        // pointer is well-aligned and exclusive.
        unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
        action.sa_flags = 0;
        // SAFETY: sigaction is async-signal-safe (POSIX guarantee);
        // action is a fully-initialised sigaction we own.
        unsafe {
            let _ = libc::sigaction(sig, &raw const action, std::ptr::null_mut());
            let _ = libc::raise(sig);
        }
    }

    fn copy_into(dst: &mut [u8], src: &[u8]) -> usize {
        let n = src.len().min(dst.len());
        dst[..n].copy_from_slice(&src[..n]);
        n
    }

    fn hex_into(dst: &mut [u8], mut value: usize) -> usize {
        if value == 0 {
            if !dst.is_empty() {
                dst[0] = b'0';
                return 1;
            }
            return 0;
        }
        // Render high-nibble first into a scratch slot, then copy.
        let mut tmp = [0_u8; 16];
        let mut idx = tmp.len();
        while value != 0 {
            idx -= 1;
            let nibble = (value & 0xF) as u8;
            tmp[idx] = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            };
            value >>= 4;
        }
        let written = tmp.len() - idx;
        let copy_len = written.min(dst.len());
        dst[..copy_len].copy_from_slice(&tmp[idx..idx + copy_len]);
        copy_len
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hex_zero() {
            let mut buf = [0_u8; 4];
            let n = hex_into(&mut buf, 0);
            assert_eq!(&buf[..n], b"0");
        }

        #[test]
        fn hex_arbitrary() {
            let mut buf = [0_u8; 16];
            let n = hex_into(&mut buf, 0xdead_beef);
            assert_eq!(&buf[..n], b"deadbeef");
        }

        #[test]
        fn copy_truncates() {
            let mut buf = [0_u8; 3];
            let n = copy_into(&mut buf, b"hello");
            assert_eq!(n, 3);
            assert_eq!(&buf, b"hel");
        }

        #[test]
        fn proximity_check_inside_stack_is_not_overflow() {
            STACK_LO.with(|c| c.set(0x1_0000_0000));
            STACK_HI.with(|c| c.set(0x1_0010_0000));
            // Address inside the live stack — not an overflow.
            assert!(!is_stack_overflow(0x1_0005_0000));
            // Address just below the bottom — the overflow case.
            assert!(is_stack_overflow(0x0_FFFF_F000));
            // Address far below the bottom — propagate.
            assert!(!is_stack_overflow(0x0_0000_1000));
            // Address above the top — propagate.
            assert!(!is_stack_overflow(0x1_0020_0000));
            STACK_LO.with(|c| c.set(0));
            STACK_HI.with(|c| c.set(0));
        }

        #[test]
        fn install_is_idempotent() {
            install();
            install();
            assert!(INSTALLED.with(Cell::get));
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::sync::Once;

    use windows_sys::Win32::Foundation::EXCEPTION_STACK_OVERFLOW;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::Threading::SetThreadStackGuarantee;

    /// 64 KiB reserved tail kept available so the unhandled
    /// exception filter can execute even when the user stack is
    /// almost exhausted.
    const STACK_GUARANTEE_BYTES: u32 = 64 * 1024;

    static FILTER_ONCE: Once = Once::new();

    pub(super) fn install() {
        let mut guarantee = STACK_GUARANTEE_BYTES;
        // SAFETY: SetThreadStackGuarantee writes through the
        // pointer; `guarantee` is a stack-resident u32.
        unsafe {
            let _ = SetThreadStackGuarantee(&mut guarantee);
        }
        FILTER_ONCE.call_once(|| {
            // SAFETY: installing a process-wide filter pointer is
            // documented safe to call from any thread.
            unsafe {
                SetUnhandledExceptionFilter(Some(handler));
            }
        });
    }

    unsafe extern "system" fn handler(info: *const EXCEPTION_POINTERS) -> i32 {
        if info.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // SAFETY: the kernel guarantees `ExceptionRecord` is
        // populated when this filter fires.
        let record = unsafe { (*info).ExceptionRecord };
        if record.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let code = unsafe { (*record).ExceptionCode };
        if code != EXCEPTION_STACK_OVERFLOW {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Write a single short line via the C runtime's stderr
        // handle. Using `eprintln!` here is unsafe (panic crossing
        // an FFI boundary inside an SEH handler); the message
        // length is fixed so a raw WriteFile call is enough.
        write_message();
        std::process::abort();
    }

    fn write_message() {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        const MSG: &[u8] = b"gossamer: stack overflow; aborting\n";
        // SAFETY: GetStdHandle returns a process-owned handle.
        // WriteFile against an inheritable stderr handle is safe
        // from any thread, including SEH context.
        unsafe {
            let h = GetStdHandle(STD_ERROR_HANDLE);
            if !h.is_null() {
                let mut written: u32 = 0;
                let _ = WriteFile(
                    h,
                    MSG.as_ptr(),
                    MSG.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_does_not_panic() {
        install_stack_guard();
        // Installing twice on the same thread is a no-op on Unix
        // and a redundant ThreadStackGuarantee call on Windows;
        // neither path should panic.
        install_stack_guard();
    }
}
