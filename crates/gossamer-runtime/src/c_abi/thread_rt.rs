//! Runtime support for `std::thread` on the compiled tiers.

#![allow(clippy::missing_safety_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_wrap)]
#![allow(unused_unsafe)]

/// `thread::num_cpus() -> i64` - logical CPU count, at least 1.
/// Mirrors `gossamer_std::thread::num_cpus` so the compiled tiers
/// agree bit-for-bit with the interpreter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_thread_num_cpus() -> i64 {
    ffi_entry!(1, {
        std::thread::available_parallelism().map_or(1, |n| n.get() as i64)
    })
}
