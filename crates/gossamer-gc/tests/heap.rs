//! End-to-end tests for the mark-sweep heap.

use gossamer_gc::{GcConfig, Heap, ObjKind};

#[test]
fn allocation_returns_distinct_handles() {
    let mut heap = Heap::new();
    let a = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    let b = heap.alloc(ObjKind::Leaf, Vec::new(), 2, 16);
    assert_ne!(a, b);
    assert_eq!(heap.get(a).payload, 1);
    assert_eq!(heap.get(b).payload, 2);
    assert_eq!(heap.len(), 2);
}

#[test]
fn unrooted_objects_are_swept() {
    let mut heap = Heap::new();
    let _ = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 16);
    let _ = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 16);
    assert_eq!(heap.len(), 2);
    let freed = heap.collect();
    assert_eq!(freed, 2);
    assert_eq!(heap.len(), 0);
    assert_eq!(heap.stats().cycles, 1);
}

#[test]
fn rooted_objects_survive_collection() {
    let mut heap = Heap::new();
    let root = heap.alloc(ObjKind::Leaf, Vec::new(), 42, 16);
    let dead = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 16);
    heap.add_root(root);
    heap.collect();
    assert!(heap.is_live(root));
    assert!(!heap.is_live(dead));
    assert_eq!(heap.get(root).payload, 42);
}

#[test]
fn child_references_transitively_survive() {
    let mut heap = Heap::new();
    let leaf = heap.alloc(ObjKind::Leaf, Vec::new(), 7, 16);
    let aggregate = heap.alloc(ObjKind::Aggregate, vec![leaf], 0, 32);
    heap.add_root(aggregate);
    heap.collect();
    assert!(heap.is_live(leaf));
    assert!(heap.is_live(aggregate));
    assert_eq!(heap.get(leaf).payload, 7);
}

#[test]
fn cycles_are_reclaimed_when_unrooted() {
    let mut heap = Heap::new();
    let a = heap.alloc(ObjKind::Aggregate, Vec::new(), 1, 16);
    let b = heap.alloc(ObjKind::Aggregate, Vec::new(), 2, 16);
    heap.get_mut(a).children.push(b);
    heap.get_mut(b).children.push(a);
    heap.collect();
    assert!(!heap.is_live(a));
    assert!(!heap.is_live(b));
}

#[test]
fn removing_root_makes_objects_collectible() {
    let mut heap = Heap::new();
    let obj = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    heap.add_root(obj);
    heap.collect();
    assert!(heap.is_live(obj));
    heap.remove_root(obj);
    heap.collect();
    assert!(!heap.is_live(obj));
}

#[test]
fn free_slots_are_recycled_by_subsequent_allocations() {
    let mut heap = Heap::new();
    let first = heap.alloc(ObjKind::Leaf, Vec::new(), 1, 16);
    heap.collect();
    assert!(!heap.is_live(first));
    let reused = heap.alloc(ObjKind::Leaf, Vec::new(), 2, 16);
    assert_eq!(reused.as_u32(), first.as_u32());
    assert_eq!(heap.get(reused).payload, 2);
}

#[test]
fn maybe_collect_fires_when_threshold_crossed() {
    let mut heap = Heap::with_config(GcConfig {
        threshold_bytes: 64,
    });
    for _ in 0..10 {
        let _ = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 16);
    }
    assert!(heap.maybe_collect());
    assert!(heap.stats().cycles >= 1);
}

#[test]
fn stats_report_allocation_totals() {
    let mut heap = Heap::new();
    heap.alloc(ObjKind::Leaf, Vec::new(), 0, 128);
    heap.alloc(ObjKind::Leaf, Vec::new(), 0, 64);
    let stats = heap.stats();
    assert_eq!(stats.bytes_allocated, 192);
}
