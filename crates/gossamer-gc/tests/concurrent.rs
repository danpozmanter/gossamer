//! Concurrent-mark + write-barrier tests layered on top of the Phase
//! 14 stop-the-world tests.

use gossamer_gc::{ConcurrentPhase, Heap, ObjKind};

#[test]
fn concurrent_cycle_preserves_rooted_objects() {
    let mut heap = Heap::new();
    let root = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    let junk = heap.alloc(ObjKind::Leaf, Vec::new(), 2, 16);
    heap.add_root(root);
    heap.concurrent_start();
    assert_eq!(heap.concurrent_phase(), ConcurrentPhase::Marking);
    while heap.concurrent_phase() == ConcurrentPhase::Marking {
        heap.concurrent_step(1);
    }
    let freed = heap.concurrent_finish();
    assert_eq!(freed, 1);
    assert!(heap.is_live(root));
    assert!(!heap.is_live(junk));
}

#[test]
fn write_barrier_re_greys_target_during_marking() {
    let mut heap = Heap::new();
    let aggregate = heap.alloc(ObjKind::Aggregate, Vec::new(), 0, 32);
    let leaf_a = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    let leaf_b = heap.alloc(ObjKind::Leaf, Vec::new(), 2, 16);
    heap.get_mut(aggregate).add_child(leaf_a);
    heap.add_root(aggregate);
    heap.concurrent_start();
    // Drain enough work to mark the aggregate and leaf_a.
    heap.concurrent_step(16);
    // Mutator installs leaf_b into the aggregate after its slot
    // was already marked. Without a barrier the sweep would drop
    // leaf_b. With the barrier it survives.
    heap.get_mut(aggregate).add_child(leaf_b);
    heap.write_barrier(leaf_b);
    heap.concurrent_finish();
    assert!(heap.is_live(aggregate));
    assert!(heap.is_live(leaf_a));
    assert!(heap.is_live(leaf_b));
}

#[test]
fn concurrent_phase_transitions_idle_marking_ready_idle() {
    let mut heap = Heap::new();
    let root = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    heap.add_root(root);
    assert_eq!(heap.concurrent_phase(), ConcurrentPhase::Idle);
    heap.concurrent_start();
    assert_eq!(heap.concurrent_phase(), ConcurrentPhase::Marking);
    while heap.concurrent_phase() == ConcurrentPhase::Marking {
        heap.concurrent_step(4);
    }
    assert_eq!(heap.concurrent_phase(), ConcurrentPhase::ReadyToSweep);
    heap.concurrent_finish();
    assert_eq!(heap.concurrent_phase(), ConcurrentPhase::Idle);
}

#[test]
fn write_barrier_outside_marking_is_a_noop() {
    let mut heap = Heap::new();
    let obj = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 16);
    // No concurrent cycle is in progress — barrier should not change
    // reachability.
    heap.write_barrier(obj);
    assert_eq!(heap.concurrent_phase(), ConcurrentPhase::Idle);
    heap.collect();
    assert!(!heap.is_live(obj));
}

#[test]
fn alloc_during_marking_is_born_black_and_survives_sweep() {
    // Allocation shading: the mutator allocates a fresh object after
    // marking has begun. Without allocation shading the object would
    // be born white and the sweep would reclaim it even though the
    // mutator still holds it on its (unscanned) stack.
    let mut heap = Heap::new();
    let rooted = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    heap.add_root(rooted);
    heap.concurrent_start();
    let fresh = heap.alloc(ObjKind::Leaf, Vec::new(), 2, 16);
    while heap.concurrent_phase() == ConcurrentPhase::Marking {
        heap.concurrent_step(8);
    }
    heap.concurrent_finish();
    assert!(heap.is_live(rooted));
    assert!(
        heap.is_live(fresh),
        "allocation-shaded object reclaimed mid-cycle",
    );
}

#[test]
fn write_barrier_with_source_re_greys_a_marked_source() {
    // Defensive Yuasa-style barrier variant: re-greying the source
    // forces a re-traversal even if the children list was mutated
    // after the source was marked black.
    let mut heap = Heap::new();
    let aggregate = heap.alloc(ObjKind::Aggregate, Vec::new(), 0, 32);
    let leaf = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    heap.add_root(aggregate);
    heap.concurrent_start();
    // Drain so the aggregate has been marked.
    while heap.concurrent_phase() == ConcurrentPhase::Marking {
        heap.concurrent_step(8);
    }
    // Mutator publishes a new child into the (now black) aggregate
    // and uses the source-aware barrier to re-grey the aggregate.
    heap.get_mut(aggregate).add_child(leaf);
    heap.write_barrier_with_source(aggregate, leaf);
    heap.concurrent_finish();
    assert!(heap.is_live(aggregate));
    assert!(heap.is_live(leaf));
}
