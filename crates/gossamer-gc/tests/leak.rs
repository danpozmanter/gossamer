//! Stream F.6 — heap-growth / leak regression coverage.
//! The test exercises an alloc-heavy loop that would leak if any
//! path kept referencing unrooted objects. After `collect`, the live
//! count must stay bounded regardless of how many objects were
//! produced.

use gossamer_gc::{Heap, ObjKind};

#[test]
fn churn_allocations_without_roots_leave_a_bounded_heap() {
    let mut heap = Heap::new();
    for _ in 0..10_000 {
        let _ = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 64);
    }
    assert!(heap.len() >= 10_000);
    heap.collect();
    assert_eq!(heap.len(), 0, "all unrooted churn should be reclaimed");
}

#[test]
fn rooted_objects_survive_every_collection_cycle() {
    let mut heap = Heap::new();
    let mut alive = Vec::new();
    for _ in 0..64 {
        let handle = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 32);
        heap.add_root(handle);
        alive.push(handle);
    }
    for _ in 0..10 {
        heap.collect();
    }
    assert_eq!(heap.len(), alive.len());
    for handle in alive {
        assert!(heap.is_live(handle));
    }
}

#[test]
fn pause_time_is_recorded_and_accumulates() {
    let mut heap = Heap::new();
    for _ in 0..1_000 {
        let _ = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 32);
    }
    heap.collect();
    heap.collect();
    let stats = heap.stats();
    assert!(stats.cycles >= 2);
    assert!(stats.total_pause_nanos >= u128::from(stats.last_pause_nanos));
    assert!(stats.max_pause_nanos >= stats.last_pause_nanos);
}

#[test]
fn steady_state_workload_does_not_grow_the_heap() {
    let mut heap = Heap::new();
    let mut root = None;
    for _ in 0..1_000 {
        if let Some(h) = root {
            heap.remove_root(h);
        }
        let handle = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 64);
        heap.add_root(handle);
        root = Some(handle);
        heap.collect();
    }
    assert_eq!(heap.len(), 1);
}
