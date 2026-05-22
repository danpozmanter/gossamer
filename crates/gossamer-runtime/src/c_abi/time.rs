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

// ---------------------------------------------------------------
// Time (seconds since UNIX epoch as f64 — interpreter parity)
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_now() -> f64 {
    ffi_entry!(f64::NAN, {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    })
}

// Process-wide monotonic base, initialised on first use. Mirrors
// the interpreter's per-thread `MONOTONIC_BASE` in
// `gossamer-interp`; a single process-global base gives identical
// `monotonic_ms` / `monotonic_nanos` deltas across the compiled
// tiers without the thread-local indirection.
fn monotonic_base() -> std::time::Instant {
    static BASE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *BASE.get_or_init(std::time::Instant::now)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_monotonic_ms() -> i64 {
    ffi_entry!(-1, {
        i64::try_from(monotonic_base().elapsed().as_millis()).unwrap_or(i64::MAX)
    })
}

/// `time::now_nanos() -> i64` — nanoseconds since the UNIX epoch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_now_nanos() -> i64 {
    ffi_entry!(-1, {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        i64::try_from(nanos).unwrap_or(i64::MAX)
    })
}

/// `time::since_ms(start) -> i64` — monotonic milliseconds elapsed
/// since the `start` value previously returned by `monotonic_ms`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_since_ms(start: i64) -> i64 {
    ffi_entry!(-1, {
        let now = i64::try_from(monotonic_base().elapsed().as_millis()).unwrap_or(i64::MAX);
        now.saturating_sub(start)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_monotonic_nanos() -> i64 {
    ffi_entry!(-1, {
        i64::try_from(monotonic_base().elapsed().as_nanos()).unwrap_or(i64::MAX)
    })
}

// `time::Duration` accessors — Duration is stored as i64
// milliseconds in the compiled tier (matches the existing
// `gos_rt_duration_from_secs`/`from_millis` constructors in
// `string.rs`). These accessors complete the surface so callers
// can round-trip a Duration through `from_secs(n)` and recover
// the same `n` via `as_secs`.

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_from_micros(us: i64) -> i64 {
    us / 1_000
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_as_millis(ms: i64) -> i64 {
    ms
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_as_secs(ms: i64) -> i64 {
    ms / 1_000
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_as_micros(ms: i64) -> i64 {
    ms.saturating_mul(1_000)
}
