//! Stream F.5 — weak references, finalisers, and string interning.
//! Weak references observe an object without keeping it alive; they
//! resolve to `None` once the target has been swept. Finalisers let
//! resource-backed objects (`File`, `Socket`, `TcpListener`)
//! deterministically release their OS handle before GC takes the
//! slot. Interning keys short strings by content so the lexer and
//! typechecker do not pay for duplicate heap allocations.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::heap::{GcRef, Heap};

/// Weak handle produced by [`WeakTable::downgrade`]. Calling
/// [`WeakTable::upgrade`] returns `Some(GcRef)` when the target is
/// still live, or `None` when it has been collected.
///
/// `WeakRef` does not track generations on its own; the table keeps a
/// generation counter per slot so upgraded handles always refer to
/// the original allocation, never a later reuse of the same slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WeakRef {
    /// Index inside the [`WeakTable`]'s slot vector.
    slot: u32,
    /// Generation captured when this weak handle was minted.
    generation: u32,
}

#[derive(Debug)]
struct WeakSlot {
    target: GcRef,
    generation: u32,
}

/// Collection of weak handles. Pair one instance with each [`Heap`];
/// call [`WeakTable::sweep`] after every GC cycle so collected
/// objects' weak handles stop resolving.
#[derive(Debug, Default)]
pub struct WeakTable {
    slots: Vec<Option<WeakSlot>>,
    free: Vec<u32>,
    next_generation: u32,
}

impl WeakTable {
    /// Empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Produces a [`WeakRef`] pointing at `handle`.
    pub fn downgrade(&mut self, handle: GcRef) -> WeakRef {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        let slot = if let Some(slot) = self.free.pop() {
            self.slots[slot as usize] = Some(WeakSlot {
                target: handle,
                generation,
            });
            slot
        } else {
            let slot = u32::try_from(self.slots.len()).expect("weak table overflow");
            self.slots.push(Some(WeakSlot {
                target: handle,
                generation,
            }));
            slot
        };
        WeakRef { slot, generation }
    }

    /// Resolves `weak` to a [`GcRef`] if the target is still live in
    /// `heap`.
    #[must_use]
    pub fn upgrade(&self, weak: WeakRef, heap: &Heap) -> Option<GcRef> {
        let entry = self.slots.get(weak.slot as usize)?.as_ref()?;
        if entry.generation != weak.generation {
            return None;
        }
        if !heap.is_live(entry.target) {
            return None;
        }
        Some(entry.target)
    }

    /// Drops weak entries whose target has been collected. Call after
    /// [`Heap::collect`] so dangling handles stop resolving.
    pub fn sweep(&mut self, heap: &Heap) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let should_free = matches!(slot.as_ref(), Some(entry) if !heap.is_live(entry.target));
            if should_free {
                *slot = None;
                if let Ok(idx) = u32::try_from(index) {
                    self.free.push(idx);
                }
            }
        }
    }

    /// Number of live weak entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Function called exactly once when a finalised object is swept.
pub type FinaliserFn = Rc<dyn Fn()>;

/// Registry that fires every registered finaliser whose `GcRef` has
/// stopped being live. Call [`FinalizerSet::run`] after [`Heap::collect`].
#[derive(Default)]
pub struct FinalizerSet {
    entries: Vec<(GcRef, FinaliserFn)>,
}

impl std::fmt::Debug for FinalizerSet {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "FinalizerSet {{ entries: {} }}", self.entries.len())
    }
}

impl FinalizerSet {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `finaliser` to run when `handle` is swept.
    pub fn register(&mut self, handle: GcRef, finaliser: FinaliserFn) {
        self.entries.push((handle, finaliser));
    }

    /// Number of registered finalisers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no finalisers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Fires every finaliser whose target is no longer live. Returns
    /// the number of finalisers fired.
    pub fn run(&mut self, heap: &Heap) -> usize {
        let mut fired = 0;
        self.entries.retain(|(handle, fin)| {
            if heap.is_live(*handle) {
                true
            } else {
                (fin)();
                fired += 1;
                false
            }
        });
        fired
    }
}

/// Intern table keyed by string contents. Dedupes equal strings so
/// the lexer, typechecker, and manifest parser share one
/// allocation per unique token.
#[derive(Debug, Default)]
pub struct InternTable {
    by_text: BTreeMap<String, GcRef>,
}

impl InternTable {
    /// Empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the intern handle for `text`, allocating it if first
    /// seen. `alloc` is invoked at most once per unique input.
    pub fn intern<F>(&mut self, text: &str, alloc: F) -> GcRef
    where
        F: FnOnce() -> GcRef,
    {
        if let Some(existing) = self.by_text.get(text) {
            return *existing;
        }
        let handle = alloc();
        self.by_text.insert(text.to_string(), handle);
        handle
    }

    /// Number of interned strings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_text.len()
    }

    /// Whether nothing has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_text.is_empty()
    }

    /// Clears every entry. Use only between collection cycles when
    /// the backing handles have been swept.
    pub fn clear(&mut self) {
        self.by_text.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::ObjKind;

    fn make_leaf(heap: &mut Heap) -> GcRef {
        heap.alloc(ObjKind::Leaf, Vec::new(), 0, 32)
    }

    #[test]
    fn weak_ref_resolves_while_target_is_rooted() {
        let mut heap = Heap::new();
        let mut weaks = WeakTable::new();
        let obj = make_leaf(&mut heap);
        heap.add_root(obj);
        let weak = weaks.downgrade(obj);
        assert_eq!(weaks.upgrade(weak, &heap), Some(obj));
    }

    #[test]
    fn weak_ref_is_none_after_target_collected() {
        let mut heap = Heap::new();
        let mut weaks = WeakTable::new();
        let obj = make_leaf(&mut heap);
        let weak = weaks.downgrade(obj);
        heap.collect();
        weaks.sweep(&heap);
        assert!(weaks.upgrade(weak, &heap).is_none());
    }

    #[test]
    fn finaliser_fires_exactly_once_when_target_is_collected() {
        let mut heap = Heap::new();
        let mut finals = FinalizerSet::new();
        let fired = Rc::new(std::cell::Cell::new(0usize));
        let obj = make_leaf(&mut heap);
        let fired_clone = Rc::clone(&fired);
        finals.register(obj, Rc::new(move || fired_clone.set(fired_clone.get() + 1)));
        heap.collect();
        assert_eq!(finals.run(&heap), 1);
        assert_eq!(fired.get(), 1);
        // Running again does nothing — the entry is gone.
        assert_eq!(finals.run(&heap), 0);
        assert_eq!(fired.get(), 1);
    }

    #[test]
    fn intern_table_deduplicates_equal_inputs() {
        let mut heap = Heap::new();
        let mut table = InternTable::new();
        let mut calls = 0;
        let a = table.intern("foo", || {
            calls += 1;
            make_leaf(&mut heap)
        });
        let b = table.intern("foo", || {
            calls += 1;
            make_leaf(&mut heap)
        });
        assert_eq!(a, b);
        assert_eq!(calls, 1);
    }
}
