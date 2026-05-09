//! GC stress tests.
//!
//! `concurrent.rs` and `heap.rs` cover the basic alloc / mark /
//! sweep semantics. This file pins the at-scale invariants:
//!
//!   - 100k allocations, then a single collect, drop everything.
//!   - 10k allocations, half rooted, single collect, exactly the
//!     rooted half survives.
//!   - 10 collect cycles in a tight loop, allocating fresh
//!     leaves each iteration — `cycles` stat counts each one,
//!     no live objects leak between iterations.
//!   - Reference graphs deeper than the `mark` recursion's
//!     informal stack guard — a 1 000-deep linear chain marks
//!     correctly without overflow.

#![allow(missing_docs)]

use gossamer_gc::{Heap, ObjKind};

#[test]
fn one_hundred_thousand_unrooted_allocations_collect_cleanly() {
    let mut heap = Heap::new();
    for i in 0..100_000 {
        let _ = heap.alloc(ObjKind::Leaf, Vec::new(), i64::from(i), 16);
    }
    assert_eq!(heap.len(), 100_000);
    let freed = heap.collect();
    assert_eq!(freed, 100_000);
    assert_eq!(heap.len(), 0);
    assert_eq!(heap.stats().cycles, 1);
}

#[test]
fn rooted_half_survives_a_single_collect_cycle() {
    let mut heap = Heap::new();
    let mut roots = Vec::with_capacity(5_000);
    for i in 0..10_000 {
        let r = heap.alloc(ObjKind::Leaf, Vec::new(), i, 16);
        if i % 2 == 0 {
            roots.push(r);
        }
    }
    for &r in &roots {
        heap.add_root(r);
    }
    assert_eq!(heap.len(), 10_000);
    heap.collect();
    assert_eq!(heap.len(), 5_000);
    for &r in &roots {
        assert!(heap.is_live(r), "rooted object died");
    }
}

#[test]
fn ten_collect_cycles_each_drop_their_iteration_garbage() {
    let mut heap = Heap::new();
    for _ in 0..10 {
        for j in 0..100 {
            let _ = heap.alloc(ObjKind::Leaf, Vec::new(), j, 16);
        }
        let freed = heap.collect();
        assert_eq!(freed, 100);
        assert_eq!(heap.len(), 0);
    }
    assert_eq!(heap.stats().cycles, 10);
}

#[test]
fn deep_linear_chain_marks_without_blowing_the_stack() {
    // 1 000-deep object chain, each linking to the next via the
    // `Obj::children` field. A naive recursive mark would
    // overflow the test thread's stack on chains this long;
    // the heap should iterate or use an explicit work list.
    let mut heap = Heap::new();
    let chain_len = 1_000;
    let mut prev = heap.alloc(ObjKind::Leaf, Vec::new(), 0, 16);
    for i in 1..chain_len {
        let r = heap.alloc(ObjKind::Aggregate, vec![prev], i, 16);
        prev = r;
    }
    heap.add_root(prev);
    heap.collect();
    assert_eq!(
        heap.len(),
        chain_len as usize,
        "expected entire chain to survive after rooting tail",
    );
}
