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
    heap.get_mut(aggregate).children.push(leaf_a);
    heap.add_root(aggregate);
    heap.concurrent_start();
    // Drain enough work to mark the aggregate and leaf_a.
    heap.concurrent_step(16);
    // Mutator installs leaf_b into the aggregate after its slot
    // was already marked. Without a barrier the sweep would drop
    // leaf_b. With the barrier it survives.
    heap.get_mut(aggregate).children.push(leaf_b);
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
