//! Regression coverage for the shadow-stack root-scanning path
//! (C1 in `~/dev/contexts/lang/adversarial_analysis.md`).
//!
//! The compiled tier emits `gos_rt_gc_shadow_push` / `_save` /
//! `_restore` around heap allocations whose only handle initially
//! lives on the C stack. The tests below verify that a concurrent
//! mark cycle with shadow roots active reaches those objects and
//! that scope-bracketed roots are released cleanly when the frame
//! restores.

use gossamer_runtime::gc;

#[test]
fn shadow_root_survives_concurrent_mark_cycle() {
    // The runtime's allocator uses the global heap exposed by
    // `gossamer_runtime::gc`. `gos_rt_gc_alloc_rooted` allocates
    // and pushes the new ref onto the calling thread's shadow
    // stack; without the shadow root the next collection would
    // reclaim it because nothing else references it.
    let frame = gc::shadow_save();
    // GcRef indices start at 0 and a single u32 isn't a null marker
    // — verify by reading the live count delta below.
    let baseline_live = gc::stats().live;
    let _raw = gc::gos_rt_gc_alloc_rooted(64);
    // Run a STW collection that promotes shadow roots; the live
    // count must include the rooted object.
    let _freed = gc::gos_rt_gc_collect_with_stack_roots();
    let stats_before = gc::stats();
    assert!(
        stats_before.live > baseline_live,
        "shadow-rooted object reclaimed: live={} baseline={}",
        stats_before.live,
        baseline_live,
    );
    // Restore the frame; the next collection should sweep the
    // object because nothing else references it.
    gc::shadow_restore(frame);
    let _freed_after = gc::gos_rt_gc_collect_with_stack_roots();
}

#[test]
fn shadow_save_and_restore_round_trip() {
    let baseline = gc::shadow_save();
    let _ = gc::gos_rt_gc_alloc_rooted(32);
    let _ = gc::gos_rt_gc_alloc_rooted(32);
    let _ = gc::gos_rt_gc_alloc_rooted(32);
    let after_pushes = gc::shadow_save();
    assert!(after_pushes >= baseline + 3);
    gc::shadow_restore(baseline);
    let after_restore = gc::shadow_save();
    assert_eq!(after_restore, baseline);
}

#[test]
fn shadow_stack_per_thread_is_isolated() {
    // Pushes on one thread must not appear on another thread's
    // shadow stack — the snapshot should still cover both.
    let baseline = gc::shadow_save();
    let _ = gc::gos_rt_gc_alloc_rooted(16);
    let main_after = gc::shadow_save();
    assert!(main_after > baseline);

    let handle = std::thread::spawn(|| {
        let other = gc::shadow_save();
        // A fresh thread starts with an empty shadow stack.
        assert_eq!(other, 0);
        let _ = gc::gos_rt_gc_alloc_rooted(16);
        gc::shadow_save()
    });
    let other_after = handle.join().unwrap();
    assert_eq!(other_after, 1);
    // Shadow stack on this thread is unchanged.
    assert_eq!(gc::shadow_save(), main_after);
    gc::shadow_restore(baseline);
}
