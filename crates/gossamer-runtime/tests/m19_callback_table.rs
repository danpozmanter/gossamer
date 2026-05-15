//! Audit M19 (0.6.0): callback handle table round-trip.

#![allow(
    clippy::cast_ptr_alignment,
    reason = "Counter is `#[repr(align(8))]`; the cast is safe"
)]

use std::sync::atomic::{AtomicU32, Ordering};

// Pinned 8-byte-aligned box so the `*const u8 → AtomicU32` cast
// in the callback is safe under stricter alignment lints.
#[repr(align(8))]
struct Counter(AtomicU32);

extern "C" fn callback_fn(
    ctx: *const u8,
    _args: *const u8,
    _args_len: u32,
    _result: *mut u8,
) -> i32 {
    // ctx is a `*const Counter` we passed at register time.
    // SAFETY: the registering thread is alive for the duration
    // of the test; the AtomicU32 stays on its stack.
    let counter = unsafe { &*ctx.cast::<Counter>() };
    counter.0.fetch_add(1, Ordering::AcqRel);
    0
}

#[test]
fn callback_register_invoke_unregister_round_trip() {
    let counter = Counter(AtomicU32::new(0));
    let ctx: *const u8 = std::ptr::from_ref(&counter).cast::<u8>();

    let handle = gossamer_runtime::c_abi::gos_rt_callback_register(ctx, callback_fn);
    assert!(handle != 0, "register must return a non-zero handle");

    // First invocation: callback fires, counter increments to 1.
    let mut result = [0u8; 16];
    let rc = unsafe {
        gossamer_runtime::c_abi::gos_rt_callback_invoke(
            handle,
            std::ptr::null(),
            0,
            result.as_mut_ptr(),
        )
    };
    assert_eq!(rc, 0, "callback returned 0 on success");
    assert_eq!(counter.0.load(Ordering::Acquire), 1);

    // Unregister: subsequent invocations return -1.
    gossamer_runtime::c_abi::gos_rt_callback_unregister(handle);
    let rc2 = unsafe {
        gossamer_runtime::c_abi::gos_rt_callback_invoke(
            handle,
            std::ptr::null(),
            0,
            result.as_mut_ptr(),
        )
    };
    assert_eq!(rc2, -1, "invoke must return -1 for an unregistered handle");
    // Counter unchanged after unregister.
    assert_eq!(counter.0.load(Ordering::Acquire), 1);
}

#[test]
fn callback_invoke_unknown_handle_returns_minus_one() {
    let mut result = [0u8; 16];
    let rc = unsafe {
        gossamer_runtime::c_abi::gos_rt_callback_invoke(
            0xDEAD_BEEF_DEAD_BEEF,
            std::ptr::null(),
            0,
            result.as_mut_ptr(),
        )
    };
    assert_eq!(rc, -1);
}

#[test]
fn callback_invoke_zero_handle_short_circuits() {
    let rc = unsafe {
        gossamer_runtime::c_abi::gos_rt_callback_invoke(
            0,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, -1);
}
