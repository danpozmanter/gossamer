//! Runtime support library linked into every Gossamer program.
//! Commits the compiler and runtime to a single value layout.
//! will add the tracing GC on top of the allocator implied
//! here; the scheduler. For now this crate exposes the
//! layout descriptors in [`layout`] so the rest of the toolchain can
//! assume a stable representation.

// `c_abi` requires unsafe for `#[no_mangle] extern "C"` symbols and
// raw-pointer dispatch. The rest of the crate stays safe by
// scoping unsafe blocks inside that module.

// Process-wide allocator for the Gossamer toolchain and every compiled
// program. Replacing the platform default — notably musl's malloc on the
// static-musl release link path and Windows ucrt HeapAlloc, both slow under
// goroutine-heavy allocation contention — equalises allocator performance
// across Linux, macOS, and Windows. Defined here so the single definition
// covers both the `gos` binary (which links this crate as an rlib) and every
// program `gos build` links against libgossamer_runtime.a.
// ThreadSanitizer is incompatible with a custom global allocator:
// mimalloc's lazy global lock init (`mi_lock_init`) memcpys a shared
// lock with no synchronisation on a thread's first allocation, which
// TSan correctly flags as a data race, and an uninstrumented allocator
// blinds TSan to real heap races regardless. Under `-Zsanitizer=thread`
// fall back to the default system allocator, which TSan instruments —
// the standard practice for custom allocators under sanitizers
// (jemalloc / mimalloc document the same). Every non-TSan build —
// release, debug, ASan, every compiled program — keeps mimalloc.
#[cfg(not(tsan))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Configures the process allocator for a predictable memory footprint.
///
/// Sets mimalloc's purge delay to zero so freed pages return to the OS
/// promptly. mimalloc's default (1000 ms in v3) defers the `madvise`
/// purge to batch it; on a phase-structured program — build a large map,
/// drop it, build the next — every dropped phase's pages stay resident
/// until process exit, so peak RSS becomes the SUM of all phases instead
/// of the largest live set (measured: k-nucleotide `--release` 52.6 MB
/// -> 28.8 MB, wall-clock unchanged). This trades mimalloc's
/// throughput-favouring default for the predictable footprint a language
/// runtime wants; the lock-free allocation fast path that motivated
/// mimalloc is unaffected.
///
/// Compiled Gossamer programs reach this from their generated `main` via
/// `gos_rt_set_args` -> `runtime_init`; the `gos` binary (which links
/// this crate as an rlib and so shares the same mimalloc) calls it once
/// at startup so `gos run` and the toolchain benefit too. Safe wrapper so
/// callers under `#![forbid(unsafe_code)]` (the `gos` `main`) can invoke
/// it. No-op under `ThreadSanitizer`, where the system allocator is used.
///
/// `15` is mimalloc's `mi_option_purge_delay`. The `libmimalloc-sys`
/// binding exposes only a curated subset of the option enum that omits
/// this one, so it is pinned by value and guarded against a future
/// mimalloc enum shift by the `allocator_tests` unit test below, which
/// asserts index 15 still defaults to the 1000 ms purge delay.
pub fn init_process_allocator() {
    #[cfg(not(tsan))]
    {
        const MI_OPTION_PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;
        // SAFETY: `mi_option_set` is thread-safe and valid any time after
        // the allocator initialised, which it has by the time `main` runs.
        unsafe {
            libmimalloc_sys::mi_option_set(MI_OPTION_PURGE_DELAY, 0);
        }
    }
}

pub mod builtins;
pub mod c_abi;
pub mod coverage;
pub mod ffi;
pub mod gc;
pub mod http2_server;
pub mod layout;
pub mod preempt;
pub mod race;
pub mod replay;
pub mod safe_daemon;
pub mod safe_env;
pub mod sched;
pub mod sched_global;
pub mod sigquit;
pub mod sql;
pub mod stack_guard;
pub mod value;

pub use layout::{HEAP_ALIGN, ObjHeader, Ptr, TypeInfo, WORD_BYTES, header_align, header_size};
// Re-export preempt-check FFI symbols so JIT-side
// `rt::gos_rt_preempt_check{,_and_yield}` lookups resolve through
// the crate root rather than the `preempt` submodule path. The
// `#[unsafe(no_mangle)]` attribute on each function gives the
// linker a single canonical symbol; the re-export only affects the
// Rust-side path.
pub use preempt::{gos_rt_preempt_check, gos_rt_preempt_check_and_yield};
pub use value::{
    GossamerValue, SINGLETON_FALSE, SINGLETON_TRUE, SINGLETON_UNIT, TAG_FLOAT, TAG_HEAP,
    TAG_IMMEDIATE, TAG_MASK, TAG_SINGLETON, fits_i56, from_f64, from_heap_handle, from_i64,
    from_singleton, tag_of, to_f64, to_heap_handle, to_i64, to_singleton,
};

#[cfg(all(test, not(tsan)))]
mod allocator_tests {
    /// Guards the `mi_option_purge_delay` enum index (15) that
    /// [`super::init_process_allocator`] pins by value, since the
    /// `libmimalloc-sys` binding doesn't name it. mimalloc's purge-delay
    /// default is 1000 ms; if a mimalloc bump shifts the enum so index 15
    /// names a different option, this default check fails loudly — the
    /// signal to re-verify the index for the new version. Also confirms
    /// the setter actually drives it to 0 (return-pages-promptly).
    #[test]
    fn purge_delay_index_is_pinned_and_settable() {
        const MI_OPTION_PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;
        // SAFETY: option get/set are thread-safe; the global allocator is
        // mimalloc in a non-tsan build, so it is initialised here.
        let default = unsafe { libmimalloc_sys::mi_option_get(MI_OPTION_PURGE_DELAY) };
        assert_eq!(
            default, 1000,
            "mimalloc option 15 default is {default}, expected the 1000 ms \
             purge_delay default — the enum likely shifted; re-verify the \
             mi_option_purge_delay index for the current mimalloc version",
        );
        super::init_process_allocator();
        let after = unsafe { libmimalloc_sys::mi_option_get(MI_OPTION_PURGE_DELAY) };
        assert_eq!(after, 0, "init_process_allocator must set purge_delay to 0");
    }
}
