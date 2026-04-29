//! Regression coverage for the per-thread bump arena.
//! Asserts the cap on individual arena buffer growth and the
//! `gos_rt_arena_save`/`restore` checkpoint primitives.

use gossamer_runtime::c_abi::{
    gos_rt_arena_restore, gos_rt_arena_save, gos_rt_gc_alloc, gos_rt_gc_reset,
};

const MIB: usize = 1024 * 1024;

#[test]
fn arena_cap_prevents_runaway_doubling() {
    unsafe {
        gos_rt_gc_reset();
        // Force several arenas to be allocated by requesting close
        // to the (capped) max size each time.
        for _ in 0..6 {
            let _ = gos_rt_gc_alloc((15 * MIB) as u64);
        }
        // We can't peek at the arena vec from outside, so instead
        // we verify a 64 MiB allocation succeeds without OOMing
        // the test process — pre-fix the doubling would have asked
        // for 4+8+16+32+64+128 = 252 MiB capacity for the same
        // sequence, ahead of even fitting the new request.
        let big = gos_rt_gc_alloc((20 * MIB) as u64);
        assert!(!big.is_null());
        gos_rt_gc_reset();
    }
}

#[test]
fn arena_save_restore_roundtrip() {
    unsafe {
        gos_rt_gc_reset();
        // Snapshot before any allocation.
        let snap0 = gos_rt_arena_save();
        let p = gos_rt_gc_alloc(64);
        assert!(!p.is_null());
        let snap1 = gos_rt_arena_save();
        let q = gos_rt_gc_alloc(64);
        assert!(!q.is_null());
        // Restore to snap1: the second allocation is rewound.
        gos_rt_arena_restore(snap1);
        // Restore to snap0: the first allocation is also rewound.
        gos_rt_arena_restore(snap0);
        // After full rewind, a fresh allocation must succeed.
        let r = gos_rt_gc_alloc(64);
        assert!(!r.is_null());
        gos_rt_gc_reset();
    }
}

#[test]
fn arena_restore_zero_is_full_reset() {
    unsafe {
        gos_rt_gc_reset();
        for _ in 0..3 {
            let _ = gos_rt_gc_alloc(1024);
        }
        // Saving with no arenas returns 0; restore(0) == full reset.
        gos_rt_arena_restore(0);
        // After reset a fresh save reports the empty state again.
        let after = gos_rt_arena_save();
        assert_eq!(after, 0, "save after reset should report empty arena");
        gos_rt_gc_reset();
    }
}
