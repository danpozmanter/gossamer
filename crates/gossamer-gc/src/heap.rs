//! Stop-the-world mark-sweep heap.
//! SPEC discourages `unsafe` in library code, so the
//! collector models the heap as a safe arena indexed by [`GcRef`]
//! handles. Each live object stores its kind, its referenced children
//! (by `GcRef`), and a mark bit that the tracing pass flips during
//! collection. This is functionally equivalent to a mark-sweep GC over
//! raw pointers — the only loss is that we can't hand out stable
//! addresses, which safe code wouldn't observe anyway.

#![forbid(unsafe_code)]

use std::collections::HashSet;

/// Opaque handle to an object inside a [`Heap`]. Comparable and hashable
/// so callers can keep side tables keyed by `GcRef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GcRef(u32);

impl GcRef {
    /// Raw numeric index of this handle inside its owning heap.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Reconstructs a [`GcRef`] from its raw numeric index. Used by
    /// the runtime ABI bridge to round-trip handles through the
    /// `extern "C"` boundary; the compiled tier passes
    /// `GcRef::as_u32()` over the wire and resurrects the handle on
    /// the receiving side.
    #[must_use]
    pub const fn from_u32(raw: u32) -> Self {
        Self(raw)
    }
}

/// One entry in the GC arena.
#[derive(Debug, Clone)]
pub struct Obj {
    /// Classification tag that the scanner uses to decide which fields
    /// are GC references. Callers can attach any additional payload to
    /// the adjacent [`Payload`].
    pub kind: ObjKind,
    /// Child references the tracing pass should follow.
    ///
    /// Stored as `Option<Box<[GcRef]>>` rather than `Vec` because
    /// `ObjKind::Leaf` and `ObjKind::String` never carry children;
    /// boxed-slice + `None` cuts the per-object header from 24 B to
    /// 16 B and skips the empty-Vec allocation entirely for leaves.
    pub children: Option<Box<[GcRef]>>,
    /// Inline integer payload — the interpreter uses this for sizes,
    /// discriminants, etc. Kept alongside `kind` so small objects do
    /// not need a separate payload allocation.
    pub payload: i64,
    /// Allocation cost in bytes attributed to this object for GC-
    /// threshold accounting.
    pub size: usize,
    /// Mark bit toggled by the tracing pass. Never exposed outside the
    /// heap.
    marked: bool,
    /// `true` while the slot holds a live object; `false` when the
    /// sweep phase reclaimed it.
    alive: bool,
}

impl Obj {
    /// Returns the list of child references (empty when this object
    /// is a leaf).
    #[must_use]
    pub fn children(&self) -> &[GcRef] {
        self.children.as_deref().unwrap_or(&[])
    }

    /// Appends `child` to this object's GC-traced child list. Allocates
    /// a fresh boxed slice each call — intended for test setup and
    /// occasional mutation, not the hot path.
    pub fn add_child(&mut self, child: GcRef) {
        let mut buf: Vec<GcRef> = self.children.take().map_or_else(Vec::new, Vec::from);
        buf.push(child);
        self.children = Some(buf.into_boxed_slice());
    }
}

/// Classification attached to every GC object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjKind {
    /// Arbitrary opaque payload — no pointer scanning required.
    Leaf,
    /// Heterogeneous aggregate: the children list is authoritative.
    Aggregate,
    /// Array of homogeneous GC references.
    Array,
    /// String payload — treated like `Leaf` because the buffer is owned
    /// by Rust rather than managed via child refs.
    String,
    /// Closure record — children contain captured references, payload
    /// stores the code-chunk id.
    Closure,
}

/// Configuration knobs controlling how aggressively the GC runs.
#[derive(Debug, Clone, Copy)]
pub struct GcConfig {
    /// Cumulative allocated bytes required before the next automatic
    /// collection. Mirrors `GOGC`-style heuristics.
    pub threshold_bytes: usize,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            threshold_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Statistics surfaced to callers after each collection cycle.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcStats {
    /// Total bytes allocated across the heap's lifetime.
    pub bytes_allocated: usize,
    /// Number of completed `collect` calls.
    pub cycles: u64,
    /// Number of objects reclaimed by the most recent sweep.
    pub last_freed: usize,
    /// Number of live objects after the most recent sweep.
    pub live: usize,
    /// Wall-clock duration of the most recent `collect` (Stream F.1).
    pub last_pause_nanos: u64,
    /// Sum of every pause duration recorded so far (Stream F.1).
    pub total_pause_nanos: u128,
    /// Longest pause observed (Stream F.1).
    pub max_pause_nanos: u64,
}

/// Phase of a concurrent GC cycle driven by
/// [`Heap::concurrent_start`] / [`Heap::concurrent_step`] /
/// [`Heap::concurrent_finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrentPhase {
    /// No collection is in progress.
    Idle,
    /// Roots have been greyed; mutators may run concurrently. Calls to
    /// [`Heap::concurrent_step`] drain the grey set a chunk at a time.
    Marking,
    /// Marking finished; the next step is a sweep.
    ReadyToSweep,
}

/// Mark-sweep heap. Owns every live `Obj` inside a `Vec` so allocation
/// and iteration stay cache friendly without needing raw pointers.
#[derive(Debug)]
pub struct Heap {
    objects: Vec<Obj>,
    free_list: Vec<u32>,
    roots: HashSet<GcRef>,
    bytes_since_collect: usize,
    config: GcConfig,
    stats: GcStats,
    grey: Vec<GcRef>,
    phase: ConcurrentPhase,
}

impl Heap {
    /// Returns a heap using the default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(GcConfig::default())
    }

    /// Returns a heap tuned by the provided configuration.
    #[must_use]
    pub fn with_config(config: GcConfig) -> Self {
        Self {
            objects: Vec::new(),
            free_list: Vec::new(),
            roots: HashSet::new(),
            bytes_since_collect: 0,
            config,
            stats: GcStats::default(),
            grey: Vec::new(),
            phase: ConcurrentPhase::Idle,
        }
    }

    /// Allocates a fresh object of the given kind. The caller provides
    /// the initial child list, payload word, and payload-size hint
    /// used by the GC-trigger heuristic.
    pub fn alloc(
        &mut self,
        kind: ObjKind,
        children: Vec<GcRef>,
        payload: i64,
        size: usize,
    ) -> GcRef {
        self.stats.bytes_allocated = self.stats.bytes_allocated.saturating_add(size);
        self.bytes_since_collect = self.bytes_since_collect.saturating_add(size);
        let children = if children.is_empty() {
            None
        } else {
            Some(children.into_boxed_slice())
        };
        if let Some(slot) = self.free_list.pop() {
            self.objects[slot as usize] = Obj {
                kind,
                children,
                payload,
                size,
                marked: false,
                alive: true,
            };
            return GcRef(slot);
        }
        let index = u32::try_from(self.objects.len()).expect("heap slot overflow");
        self.objects.push(Obj {
            kind,
            children,
            payload,
            size,
            marked: false,
            alive: true,
        });
        GcRef(index)
    }

    /// Registers `handle` as a GC root. Rooted objects (and their
    /// transitive children) survive every collection.
    pub fn add_root(&mut self, handle: GcRef) {
        self.roots.insert(handle);
    }

    /// Removes `handle` from the root set. The object becomes eligible
    /// for collection once no other roots reach it.
    pub fn remove_root(&mut self, handle: GcRef) {
        self.roots.remove(&handle);
    }

    /// Returns `true` when `handle` is currently rooted.
    #[must_use]
    pub fn is_rooted(&self, handle: GcRef) -> bool {
        self.roots.contains(&handle)
    }

    /// Borrows an object by handle. Panics if the handle is stale or
    /// refers to a reclaimed slot.
    #[must_use]
    pub fn get(&self, handle: GcRef) -> &Obj {
        let obj = &self.objects[handle.0 as usize];
        assert!(obj.alive, "use-after-free on GcRef {handle:?}");
        obj
    }

    /// Mutably borrows an object by handle.
    pub fn get_mut(&mut self, handle: GcRef) -> &mut Obj {
        let obj = &mut self.objects[handle.0 as usize];
        assert!(obj.alive, "use-after-free on GcRef {handle:?}");
        obj
    }

    /// Returns `true` when the slot backing `handle` is still live.
    #[must_use]
    pub fn is_live(&self, handle: GcRef) -> bool {
        self.objects
            .get(handle.0 as usize)
            .is_some_and(|obj| obj.alive)
    }

    /// Current GC statistics.
    #[must_use]
    pub fn stats(&self) -> GcStats {
        self.stats
    }

    /// Runs a full mark-sweep collection. Returns the number of
    /// reclaimed objects.
    pub fn collect(&mut self) -> usize {
        let started = std::time::Instant::now();
        self.mark();
        let freed = self.sweep();
        self.bytes_since_collect = 0;
        self.stats.cycles = self.stats.cycles.saturating_add(1);
        self.stats.last_freed = freed;
        self.stats.live = self.live_count();
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.stats.last_pause_nanos = elapsed;
        self.stats.total_pause_nanos = self
            .stats
            .total_pause_nanos
            .saturating_add(u128::from(elapsed));
        if elapsed > self.stats.max_pause_nanos {
            self.stats.max_pause_nanos = elapsed;
        }
        freed
    }

    /// Collects only when the allocation-since-last-GC threshold has
    /// been exceeded. Returns whether a collection ran.
    pub fn maybe_collect(&mut self) -> bool {
        if self.bytes_since_collect >= self.config.threshold_bytes {
            self.collect();
            return true;
        }
        false
    }

    fn mark(&mut self) {
        for obj in &mut self.objects {
            obj.marked = false;
        }
        let mut stack: Vec<GcRef> = self.roots.iter().copied().collect();
        while let Some(handle) = stack.pop() {
            let slot = handle.0 as usize;
            let Some(obj) = self.objects.get_mut(slot) else {
                continue;
            };
            if obj.marked || !obj.alive {
                continue;
            }
            obj.marked = true;
            if let Some(children) = obj.children.as_deref() {
                stack.extend(children.iter().copied());
            }
        }
    }

    fn sweep(&mut self) -> usize {
        let mut freed = 0;
        for (index, obj) in self.objects.iter_mut().enumerate() {
            if !obj.alive {
                continue;
            }
            if !obj.marked {
                obj.alive = false;
                obj.children = None;
                self.free_list
                    .push(u32::try_from(index).expect("heap index overflow"));
                freed += 1;
            }
        }
        freed
    }

    fn live_count(&self) -> usize {
        self.objects.iter().filter(|obj| obj.alive).count()
    }

    /// Current concurrent-GC phase.
    #[must_use]
    pub fn concurrent_phase(&self) -> ConcurrentPhase {
        self.phase
    }

    /// Begins a concurrent mark cycle. Greys the current root set
    /// synchronously (a short STW pause) and returns. Subsequent
    /// [`Self::concurrent_step`] calls process the grey stack in
    /// chunks.
    pub fn concurrent_start(&mut self) {
        for obj in &mut self.objects {
            obj.marked = false;
        }
        self.grey = self.roots.iter().copied().collect();
        self.phase = ConcurrentPhase::Marking;
    }

    /// Drains up to `budget` entries from the grey stack, marking
    /// them and shading their children. Returns the number of
    /// objects actually marked.
    pub fn concurrent_step(&mut self, budget: usize) -> usize {
        if !matches!(self.phase, ConcurrentPhase::Marking) {
            return 0;
        }
        let mut marked = 0;
        for _ in 0..budget {
            let Some(handle) = self.grey.pop() else {
                break;
            };
            let slot = handle.0 as usize;
            let Some(obj) = self.objects.get_mut(slot) else {
                continue;
            };
            if obj.marked || !obj.alive {
                continue;
            }
            obj.marked = true;
            marked += 1;
            if let Some(children) = obj.children.as_deref() {
                self.grey.extend(children.iter().copied());
            }
        }
        if self.grey.is_empty() {
            self.phase = ConcurrentPhase::ReadyToSweep;
        }
        marked
    }

    /// Completes the concurrent cycle: a final short STW re-scan of
    /// the grey set followed by a sweep. Returns the number of freed
    /// objects.
    pub fn concurrent_finish(&mut self) -> usize {
        // Final remark pass: drain any grey work the write barrier
        // has pushed while the mutator ran concurrently.
        self.phase = ConcurrentPhase::Marking;
        while !self.grey.is_empty() {
            self.concurrent_step(64);
        }
        self.phase = ConcurrentPhase::ReadyToSweep;
        let freed = self.sweep();
        self.bytes_since_collect = 0;
        self.phase = ConcurrentPhase::Idle;
        self.stats.cycles = self.stats.cycles.saturating_add(1);
        self.stats.last_freed = freed;
        self.stats.live = self.live_count();
        freed
    }

    /// Write barrier: shade `target` so any mutator-visible write of
    /// a reference while a concurrent cycle is active re-greys the
    /// newly stored reference. Has no effect between collections.
    pub fn write_barrier(&mut self, target: GcRef) {
        if matches!(self.phase, ConcurrentPhase::Idle) {
            return;
        }
        if self.is_live(target) {
            self.grey.push(target);
        }
    }

    /// Returns the total number of live objects. Convenience wrapper
    /// around [`GcStats::live`] for call sites that want the current
    /// value rather than the post-sweep snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live_count()
    }

    /// Returns `true` when the heap holds no live objects.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live_count() == 0
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}
