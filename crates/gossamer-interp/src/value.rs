//! Runtime value representation shared by the bytecode VM and focused
//! interpreter compatibility helpers.
//! Every shared aggregate is backed by [`Arc`] rather than
//! [`std::rc::Rc`] so a [`Value`] can cross thread boundaries - a
//! prerequisite for real goroutine parallelism per
//! the risks backlog.
//! Phase P1 introduces `to_raw` / `from_raw` so that the interpreter
//! and the native backend agree on a single `u64` value layout.
//! Heap objects are registered in a global side table and addressed
//! by `u32` handles; later phases will replace the `Arc` internals
//! with the GC heap directly.

// `SmolStr` (B2) does tagged-pointer arithmetic to keep
// `Value::String` at 8 bytes inline. The unsafe is confined to
// the few methods on `SmolStr`; everything else in the crate
// keeps the safe-Rust discipline.
#![allow(unsafe_code)]

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::cell::{Cell, UnsafeCell};
use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use smallvec::SmallVec;

use gossamer_runtime::{
    GossamerValue, SINGLETON_FALSE, SINGLETON_TRUE, SINGLETON_UNIT, TAG_FLOAT, TAG_HEAP,
    TAG_IMMEDIATE, TAG_SINGLETON, fits_i56, from_f64, from_heap_handle, from_i64, from_singleton,
    tag_of, to_f64, to_heap_handle, to_i64, to_singleton,
};

/// Dense-entry map backing interpreter `HashMap` values.
pub type DenseMap<K, V> = indexmap::IndexMap<K, V, rustc_hash::FxBuildHasher>;

/// Constructs an empty dense interpreter map with the VM's hash builder.
#[must_use]
pub fn dense_map<K, V>() -> DenseMap<K, V> {
    DenseMap::with_hasher(rustc_hash::FxBuildHasher)
}

/// Constructs a dense interpreter map with an initial entry capacity.
#[must_use]
pub fn dense_map_with_capacity<K, V>(capacity: usize) -> DenseMap<K, V> {
    DenseMap::with_capacity_and_hasher(capacity, rustc_hash::FxBuildHasher)
}

/// Shared JSON tree plus a stable view into one node of that tree.
///
/// `json::get` / `json::at` can return a child object or array by cloning this
/// lightweight handle instead of deep-cloning the selected subtree. Scalars are
/// still projected into ordinary interpreter values at the query boundary.
#[derive(Debug, Clone)]
pub struct JsonInner {
    tree: Arc<gossamer_std::json::Value>,
    view: usize,
}

impl JsonInner {
    /// Owns `value` as a new canonical JSON tree and views its root.
    #[must_use]
    pub fn new(value: gossamer_std::json::Value) -> Self {
        let tree = Arc::new(value);
        let view = Arc::as_ptr(&tree) as usize;
        Self { tree, view }
    }

    /// Borrows the JSON node viewed by this handle.
    #[must_use]
    pub fn as_value(&self) -> &gossamer_std::json::Value {
        // SAFETY: `view` is either the stable address of `tree`'s root from
        // `new`, or the address of a child borrowed from the same tree by
        // `child`. The `Arc` keeps the tree allocation alive for this handle.
        unsafe { &*(self.view as *const gossamer_std::json::Value) }
    }

    /// Builds a handle viewing `child`, which must be borrowed from this
    /// handle's tree.
    #[must_use]
    pub fn child(&self, child: &gossamer_std::json::Value) -> Self {
        Self {
            tree: Arc::clone(&self.tree),
            view: std::ptr::from_ref(child) as usize,
        }
    }

    /// Clones the viewed JSON node for APIs that genuinely need an owned DOM.
    #[must_use]
    pub fn to_owned_value(&self) -> gossamer_std::json::Value {
        self.as_value().clone()
    }
}

/// A mutable VM value that is confined to the OS thread that created it.
///
/// `MutCell` values are created only by `CellNew` / `CellNewMove` around an
/// immediate `&mut` call, then consumed by its matching `CellTake`. They do
/// not escape that call protocol or cross a goroutine boundary. Keeping their
/// `Arc` handle lets [`Value`] retain its process-wide transport properties,
/// while avoiding a mutex acquisition on each local read or write.
///
/// The owner check is a defensive boundary around the `UnsafeCell`: should a
/// future compiler path accidentally let a transient cell cross threads, it
/// panics before dereferencing the value instead of creating a data race.
/// The borrow flag preserves the mutex's exclusive-access contract and makes
/// accidental re-entrant access fail deterministically.
pub struct ThreadConfinedCell {
    owner: std::thread::ThreadId,
    borrowed: Cell<bool>,
    value: UnsafeCell<Value>,
}

// Access to `value` is permitted only after `lock` verifies that the current
// thread is `owner`. `ThreadConfinedCellGuard` is !Send, so a borrowed value
// cannot be moved to another thread. The Arc control block remains atomic,
// making handle clones and drops safe even when an invalid handle is moved.
unsafe impl Send for ThreadConfinedCell {}
unsafe impl Sync for ThreadConfinedCell {}

impl fmt::Debug for ThreadConfinedCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadConfinedCell")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl ThreadConfinedCell {
    #[must_use]
    pub(crate) fn new(value: Value) -> Self {
        Self {
            owner: std::thread::current().id(),
            borrowed: Cell::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Borrows the transient value on its owner thread.
    ///
    /// Panics if a foreign thread or a re-entrant caller attempts access,
    /// preserving the exclusive-access contract formerly provided by `Mutex`.
    pub fn lock(&self) -> ThreadConfinedCellGuard<'_> {
        assert_eq!(
            self.owner,
            std::thread::current().id(),
            "transient VM MutCell accessed from a different thread"
        );
        assert!(
            !self.borrowed.replace(true),
            "transient VM MutCell accessed re-entrantly"
        );
        ThreadConfinedCellGuard {
            cell: self,
            // A guard must remain on its owner thread: its Drop resets a
            // thread-local borrow flag and it may expose `&mut Value`.
            _not_send: PhantomData,
        }
    }

    fn into_inner(self) -> Value {
        self.value.into_inner()
    }
}

/// Exclusive, non-send access to a [`ThreadConfinedCell`] value.
pub struct ThreadConfinedCellGuard<'a> {
    cell: &'a ThreadConfinedCell,
    _not_send: PhantomData<std::rc::Rc<()>>,
}

impl std::ops::Deref for ThreadConfinedCellGuard<'_> {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `lock` checked the owning thread and set the exclusive
        // borrow flag before constructing this guard. The guard is !Send.
        unsafe { &*self.cell.value.get() }
    }
}

impl std::ops::DerefMut for ThreadConfinedCellGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: as above, and `&mut self` guarantees the caller has the
        // guard's unique mutable access for the duration of this borrow.
        unsafe { &mut *self.cell.value.get() }
    }
}

impl Drop for ThreadConfinedCellGuard<'_> {
    fn drop(&mut self) {
        self.cell.borrowed.set(false);
    }
}

/// One runtime value produced or consumed by the interpreter.
///
/// Unboxed integer / float / bool / char types sit inline; aggregates
/// (strings, tuples, arrays, structs) are reference-counted so that
/// assignment and argument passing share their backing storage, mirror-
/// ing the GC semantics described in SPEC §3.3.
///
/// **B1 layout (this commit).** Every variant payload is at most
/// one pointer / one scalar, so `size_of::<Value>() == 16` (one
/// 8-byte payload + 8-byte discriminant/padding). Pre-B1, the
/// `FloatArray` / `Variant` / `Struct` / `Builtin` / `Native`
/// variants inlined a `String` (24 bytes) plus an `Arc`, pushing
/// `size_of::<Value>` to 48 bytes - every register-file slot
/// paid the worst-case width even when holding `Int(i64)`. We
/// pull each heavy variant behind an `Arc<Inner>` so the enum
/// payload is one ptr; cloning a `Value` is now a refcount
/// bump in the worst case instead of a `String::clone`.
#[derive(Debug, Clone)]
pub enum Value {
    /// `()`.
    Unit,
    /// `bool`.
    Bool(bool),
    /// Signed 64-bit integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// `char`.
    Char(char),
    /// UTF-8 string. Stored inline when ≤ 7 bytes (no heap
    /// allocation); otherwise an `Arc<String>` behind a tag
    /// bit. See [`SmolStr`].
    String(SmolStr),
    /// A parsed JSON document retained in the stdlib's canonical tree.
    ///
    /// Keeping this behind an `Arc` lets `json::parse` hand a document
    /// directly to `json::render` without first allocating an interpreter
    /// array/map tree and then rebuilding the same JSON tree for encoding.
    /// JSON query builtins expose children lazily when a program actually
    /// traverses the document.
    Json(Arc<JsonInner>),
    /// Tuple aggregate.
    Tuple(Arc<Vec<Value>>),
    /// Array / Vec aggregate (interpreter treats both as `Vec`).
    Array(Arc<Vec<Value>>),
    /// Flat f64 storage for an array of a struct whose fields
    /// are all `f64`.
    FloatArray(Arc<FloatArrayInner>),
    /// Flat `i64` storage for a primitive integer array literal.
    IntArray(Arc<Vec<i64>>),
    /// Flat `f64` storage for a primitive float array literal /
    /// `Vec<f64>`. Avoids per-element `Value::Float` boxing on
    /// hot loops over numeric arrays (nbody's `dx`/`dy`/`dz`/`mag`
    /// scratch arrays read every f64 here straight into a typed
    /// register).
    FloatVec(Arc<Vec<f64>>),
    /// Opaque VM-only lazy iterator state handle. The concrete state lives in
    /// the stdlib `iter` registry so the `Value` enum does not recursively
    /// carry iterator closures and upstream states.
    LazyIter(i64),
    /// Enum variant or tuple-struct constructor payload.
    Variant(Arc<VariantInner>),
    /// Struct-shaped aggregate.
    Struct(Arc<StructInner>),
    /// Native (compiled-representation) enum value handed across the
    /// JIT boundary as a raw pointer. Structural access goes through
    /// the carried shape; drop of the last clone releases the
    /// reference through the runtime.
    NativeEnum(Arc<NativeEnumOwner>),
    /// User-defined callable.
    Closure(Arc<Closure>),
    /// Built-in intrinsic callable.
    Builtin(Arc<BuiltinInner>),
    /// Built-in callable that can re-enter the interpreter through a
    /// [`NativeDispatch`] handle.
    Native(Arc<NativeInner>),
    /// Concurrent channel endpoint.
    Channel(Channel),
    /// Hash-map aggregate. `IndexMap` keeps entries dense while retaining
    /// O(1) lookup through the Fx hasher; this avoids hashbrown's full
    /// `(K, V)` power-of-two bucket slack on map-heavy workloads. The mutex keeps
    /// `Value: Send + Sync` so goroutines can pass maps through
    /// channels.
    Map(Arc<parking_lot::Mutex<DenseMap<MapKey, Value>>>),
    /// Typed `HashMap<i64, i64>` aggregate. Skips the [`MapKey`]
    /// enum-tag dispatch on every op and avoids the [`Value`]
    /// box around each integer value. k-nucleotide's k-mer
    /// frequency tables ride this variant, dropping per-iteration
    /// hash + compare cost dramatically.
    IntMap(Arc<parking_lot::Mutex<DenseMap<i64, i64>>>),
    /// Typed `HashMap<String, i64>` aggregate. Drops both the
    /// [`MapKey`] enum tag and the [`Value`] box around each count:
    /// an entry is a bare `(SmolStr, i64)`, ~16 bytes lighter than
    /// the generic `Map`'s `(MapKey, Value)`. Because a `HashMap`
    /// keeps entries dense, that per-entry saving translates directly into
    /// lower peak RSS for string-frequency tables (k-mer / n-gram / token
    /// counts).
    StrIntMap(Arc<parking_lot::Mutex<DenseMap<SmolStr, i64>>>),
    /// Unsigned 64-bit integer - same bit pattern as `Int(n as i64)`
    /// but formats as an unsigned decimal value. Used exclusively for
    /// `x as u64` casts to preserve unsigned display semantics.
    Uint(u64),
    /// Non-owning weak reference produced by `x.downgrade()`. Observes
    /// the liveness of the referent's `Arc` without keeping it alive;
    /// `w.upgrade()` yields `Some` while a strong reference survives and
    /// `None` once the last one is dropped.
    Weak(WeakValue),
    /// Write-back cell carrying a `&mut Vec<T>` / `&mut [T]` call
    /// argument. The caller wraps the aggregate at the call site,
    /// the callee unwraps it at frame entry and stores the final
    /// parameter value back on return, and the caller then reads it
    /// out - write-through `&mut` parameter semantics on top of the
    /// VM's clone-on-write value model. Never escapes the call
    /// protocol: no user-visible op ever observes a `MutCell`.
    MutCell(Arc<ThreadConfinedCell>),
    /// Poisoned / uninitialised sentinel.
    Void,
}

impl Value {
    /// Renders this value as a source-like representation for interactive
    /// inspection. Unlike [`fmt::Display`], strings and chars are quoted.
    #[must_use]
    pub fn repr(&self) -> String {
        repr_value(self)
    }

    /// Borrows the elements of an `Array` or `Tuple` as a slice - both back
    /// onto `[Value]`, so read-only element access shares one path.
    #[must_use]
    pub(crate) fn as_value_slice(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            Value::Tuple(a) => Some(a),
            _ => None,
        }
    }
}

/// Iteratively reclaim a tree of owned child `Value`s with an explicit
/// worklist, so a depth-N recursive aggregate (linked list, tree, graph) tears
/// down in O(N) heap and O(1) native stack instead of overflowing the host
/// stack through nested `Arc` drop glue. Seeded by the `Drop` impls of the
/// recursive aggregate payloads ([`VariantInner`] / [`StructInner`]); once
/// teardown enters through one of those, the whole reachable owned-`Value`
/// subgraph is dismantled here.
///
/// For each popped value that uniquely owns children, the children are moved
/// onto the worklist and the now-childless shell drops shallowly. A
/// still-shared payload (`try_unwrap` returns `Err`) is just dereferenced. The
/// `Drop`-implementing payloads (`Variant`/`Struct`) are emptied with
/// `mem::take` rather than a field move, which a `Drop` type forbids; the
/// nested drop of the emptied shell re-enters this routine with nothing to do,
/// so the native recursion stays at most one frame deep.
///
/// Aggregate map *keys* (`MapKey::Agg`) keep their own drop glue: a deeply
/// nested aggregate used as a map key is neither a `Value` chain nor an
/// idiomatic shape, so it is out of scope here.
fn dismantle_children(mut stack: Vec<Value>) {
    while let Some(v) = stack.pop() {
        match v {
            Value::Variant(a) => {
                if let Ok(mut inner) = Arc::try_unwrap(a) {
                    stack.extend(std::mem::take(&mut inner.fields));
                }
            }
            Value::Struct(a) => {
                if let Ok(mut inner) = Arc::try_unwrap(a) {
                    let fields = std::mem::take(&mut inner.fields);
                    stack.extend(fields.into_vec().into_iter().map(|(_, val)| val));
                }
            }
            Value::Tuple(a) | Value::Array(a) => {
                if let Ok(vec) = Arc::try_unwrap(a) {
                    stack.extend(vec);
                }
            }
            Value::Closure(a) => {
                if let Ok(inner) = Arc::try_unwrap(a) {
                    stack.extend(inner.capture_values);
                }
            }
            Value::Map(a) => {
                if let Ok(m) = Arc::try_unwrap(a) {
                    stack.extend(m.into_inner().into_values());
                }
            }
            Value::MutCell(a) => {
                if let Ok(m) = Arc::try_unwrap(a) {
                    stack.push(m.into_inner());
                }
            }
            _ => {}
        }
    }
}

/// Native-stack recursion depth below which recursive aggregate teardown is
/// left to drop directly (cheap, allocation-free). At or above it a payload
/// switches to the iterative [`dismantle_children`] worklist, bounding the host
/// stack so a deep chain cannot overflow it. Comfortably below any thread's
/// stack budget while keeping the common shallow case off the worklist.
const DROP_RECURSION_LIMIT: u32 = 512;

thread_local! {
    /// Current recursive-drop nesting depth for the aggregate payloads on this
    /// thread. Read once per `VariantInner` / `StructInner` drop to decide
    /// recurse-vs-iterate; never observed by user code.
    static DROP_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// RAII restore of [`DROP_DEPTH`], so the depth is correct even if a nested
/// drop unwinds.
struct DropDepthGuard(u32);

impl Drop for DropDepthGuard {
    fn drop(&mut self) {
        DROP_DEPTH.with(|d| d.set(self.0));
    }
}

impl Drop for VariantInner {
    fn drop(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        let depth = DROP_DEPTH.with(std::cell::Cell::get);
        if depth >= DROP_RECURSION_LIMIT {
            // Deep: flatten the remaining subgraph iteratively. `mem::take`
            // leaves the shell empty so its post-return field drop is a no-op,
            // and the worklist's own emptied shells re-enter as no-ops.
            dismantle_children(std::mem::take(&mut self.fields).into_vec());
            return;
        }
        // Shallow: take the fields (alloc-free for the inline arity) and drop
        // them ourselves with the depth raised, so a long chain trips the
        // iterative path before it can overflow the host stack.
        DROP_DEPTH.with(|d| d.set(depth + 1));
        let _guard = DropDepthGuard(depth);
        drop(std::mem::take(&mut self.fields));
    }
}

impl Drop for StructInner {
    fn drop(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        let depth = DROP_DEPTH.with(std::cell::Cell::get);
        if depth >= DROP_RECURSION_LIMIT {
            let fields = std::mem::take(&mut self.fields);
            dismantle_children(fields.into_vec().into_iter().map(|(_, v)| v).collect());
            return;
        }
        DROP_DEPTH.with(|d| d.set(depth + 1));
        let _guard = DropDepthGuard(depth);
        drop(std::mem::take(&mut self.fields));
    }
}

/// Type-erased weak handle backing [`Value::Weak`]. Each arm holds a
/// `std::sync::Weak` to the corresponding heap variant's `Arc`, so
/// upgrading reconstructs the original `Value` shape when the referent
/// is still alive. A downgrade of a non-heap (Copy) value records
/// [`WeakValue::Dead`] - there is no allocation to observe, so it never
/// upgrades.
#[derive(Debug)]
pub enum WeakValue {
    /// Weak reference to a [`Value::Variant`] payload.
    Variant(std::sync::Weak<VariantInner>),
    /// Weak reference to a [`Value::Struct`] payload.
    Struct(std::sync::Weak<StructInner>),
    /// Weak reference to a [`Value::Array`] payload.
    Array(std::sync::Weak<Vec<Value>>),
    /// Weak reference to a [`Value::Tuple`] payload.
    Tuple(std::sync::Weak<Vec<Value>>),
    /// Weak reference to a [`Value::NativeEnum`] node, observed through the
    /// runtime's intrusive weak count. Boxed so this variant is a single
    /// niche-bearing pointer like the others, keeping `WeakValue` (and thus the
    /// inline `Value::Weak`) within the 16-byte hot-`Value` budget. Kept inline
    /// in `Value` (not `Arc`-wrapped) so each `Value` clone/drop maps 1:1 to the
    /// intrusive weak retain/release below.
    NativeEnum(Box<NativeEnumWeakRef>),
    /// Downgrade of a value with no observable allocation; never upgrades.
    Dead,
}

/// The referent identity of a [`WeakValue::NativeEnum`]: the tagged native
/// pointer (disc bits intact) and the layout needed to rebuild a strong handle.
#[derive(Debug)]
pub struct NativeEnumWeakRef {
    /// Tagged native pointer of the referent.
    pub ptr: usize,
    /// Layout for the rebuilt handle.
    pub shape: Arc<NativeEnumShape>,
}

impl Clone for WeakValue {
    fn clone(&self) -> Self {
        match self {
            WeakValue::Variant(w) => WeakValue::Variant(w.clone()),
            WeakValue::Struct(w) => WeakValue::Struct(w.clone()),
            WeakValue::Array(w) => WeakValue::Array(w.clone()),
            WeakValue::Tuple(w) => WeakValue::Tuple(w.clone()),
            WeakValue::NativeEnum(r) => {
                let base = r.ptr & !7;
                if base != 0 {
                    // SAFETY: a copied weak handle observes the same node; bump
                    // the intrusive weak count so its drop is balanced.
                    unsafe { gossamer_runtime::c_abi::gos_rt_rc_weak_retain(base as *mut u8) };
                }
                WeakValue::NativeEnum(Box::new(NativeEnumWeakRef {
                    ptr: r.ptr,
                    shape: Arc::clone(&r.shape),
                }))
            }
            WeakValue::Dead => WeakValue::Dead,
        }
    }
}

impl Drop for WeakValue {
    fn drop(&mut self) {
        if let WeakValue::NativeEnum(r) = self {
            let base = r.ptr & !7;
            if base != 0 {
                // SAFETY: releasing the weak count this handle took at downgrade
                // / clone; frees the block once strong and weak both reach zero.
                unsafe { gossamer_runtime::c_abi::gos_rt_rc_weak_release(base as *mut u8) };
            }
        }
    }
}

impl WeakValue {
    /// Builds a weak handle from a strong value. Heap variants record a
    /// `std::sync::Weak` to their `Arc`; a native enum takes an intrusive weak
    /// count; everything else is `Dead`.
    #[must_use]
    pub fn downgrade(value: &Value) -> Self {
        match value {
            Value::Variant(a) => WeakValue::Variant(Arc::downgrade(a)),
            Value::Struct(a) => WeakValue::Struct(Arc::downgrade(a)),
            Value::Array(a) => WeakValue::Array(Arc::downgrade(a)),
            Value::Tuple(a) => WeakValue::Tuple(Arc::downgrade(a)),
            Value::NativeEnum(h) => {
                let base = h.ptr & !7;
                if base == 0 {
                    return WeakValue::Dead;
                }
                // SAFETY: bumps the referent's intrusive weak count; the block
                // outlives every strong reference until this weak is released.
                unsafe { gossamer_runtime::c_abi::gos_rt_rc_downgrade(base as *mut u8) };
                WeakValue::NativeEnum(Box::new(NativeEnumWeakRef {
                    ptr: h.ptr,
                    shape: Arc::clone(&h.shape),
                }))
            }
            _ => WeakValue::Dead,
        }
    }

    /// Reconstructs the strong [`Value`] if the referent is still alive.
    #[must_use]
    pub fn upgrade(&self) -> Option<Value> {
        match self {
            WeakValue::Variant(w) => w.upgrade().map(Value::Variant),
            WeakValue::Struct(w) => w.upgrade().map(Value::Struct),
            WeakValue::Array(w) => w.upgrade().map(Value::Array),
            WeakValue::Tuple(w) => w.upgrade().map(Value::Tuple),
            WeakValue::NativeEnum(r) => {
                let base = r.ptr & !7;
                // SAFETY: reading the strong count of a weak-pinned (still
                // allocated) node; > 0 means a strong owner survives.
                if base != 0
                    && unsafe { gossamer_runtime::c_abi::gos_rt_rc_strong_count(base as *mut u8) }
                        > 0
                {
                    // SAFETY: co-owning a live node; the returned borrowed handle
                    // releases this retain once on drop.
                    unsafe { gossamer_runtime::c_abi::gos_rt_rc_retain(base as *mut u8) };
                    Some(Value::NativeEnum(Arc::new(NativeEnumOwner {
                        ptr: r.ptr,
                        shape: Arc::clone(&r.shape),
                        owned: false,
                    })))
                } else {
                    None
                }
            }
            WeakValue::Dead => None,
        }
    }
}

/// Ordered key type for [`Value::Map`]. Wraps a [`Value`] and
/// gives it a `(tag, content)` total order so any value the user
/// can hash (int / bool / char / string) sorts deterministically.
/// Aggregate values (arrays, structs, closures) collapse to a
/// single bucket - they're rejected at insert time, not here.
///
/// String keys are stored as [`SmolStr`] (8 B inline for ≤ 7-byte
/// keys, otherwise an `Arc<str>` behind a tag bit) instead of an
/// owned `String`. For maps with many short string keys (k-mer
/// counts, tag dictionaries, …) this halves per-key residency
/// and removes one heap allocation per insert.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapKey {
    /// Sentinel for non-hashable inputs; all equal so their map
    /// degenerates to a single slot. Lets the runtime stay
    /// total even if user code passes an aggregate as a key.
    NonHashable,
    /// `bool` key.
    Bool(bool),
    /// `i64` key (every integer width converges here).
    Int(i64),
    /// `char` key.
    Char(char),
    /// String key (stored inline when ≤ 7 bytes - see [`SmolStr`]).
    Str(SmolStr),
    /// Aggregate key - struct / tuple / enum variant - hashed by *value*:
    /// the type/variant name plus each field's `MapKey`, recursively. Two
    /// equal-valued aggregates at distinct allocations produce equal keys, so
    /// `HashMap<Point, _>` keys by content the way the compiled tier does.
    /// Boxed so the rare aggregate-key case does not widen every `MapKey`
    /// (a scalar/string key stays 16 bytes instead of paying for two inline
    /// fat pointers).
    Agg(Box<AggKey>),
}

/// Boxed payload of [`MapKey::Agg`]: an aggregate map key hashed by value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AggKey {
    /// Type / variant name (`""` for a tuple, `"[]"` for an array).
    pub name: TypeTag,
    /// Each field's key, recursively.
    pub fields: Box<[MapKey]>,
}

impl MapKey {
    /// Builds a `MapKey` from any `Value`. Aggregates collapse
    /// to `NonHashable`.
    #[must_use]
    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Bool(b) => Self::Bool(*b),
            Value::Int(n) => Self::Int(*n),
            Value::Char(c) => Self::Char(*c),
            // Key floats by their bit pattern - matches the compiled tier,
            // which hashes the raw 8 bytes.
            Value::Float(f) => Self::Int(f.to_bits() as i64),
            Value::String(s) => Self::Str(s.clone()),
            Value::Tuple(vals) => Self::Agg(Box::new(AggKey {
                name: intern_type_tag(""),
                fields: vals.iter().map(Self::from_value).collect(),
            })),
            Value::Array(vals) => Self::Agg(Box::new(AggKey {
                name: intern_type_tag("[]"),
                fields: vals.iter().map(Self::from_value).collect(),
            })),
            Value::IntArray(ns) => Self::Agg(Box::new(AggKey {
                name: intern_type_tag("[]"),
                fields: ns.iter().map(|n| Self::Int(*n)).collect(),
            })),
            Value::Struct(inner) => Self::Agg(Box::new(AggKey {
                name: inner.name.clone(),
                fields: inner
                    .fields
                    .iter()
                    .map(|(_, fv)| Self::from_value(fv))
                    .collect(),
            })),
            Value::Variant(inner) => Self::Agg(Box::new(AggKey {
                name: inner.name.clone(),
                fields: inner.fields.iter().map(Self::from_value).collect(),
            })),
            // A native enum hashes through its boxed shape so a user enum used
            // as a map key keeps working after Step 8 (VM-built enums are
            // native) and hashes identically to a boxed one of the same value.
            Value::NativeEnum(owner) => Self::from_value(&native_enum_to_variant(owner)),
            _ => Self::NonHashable,
        }
    }

    /// Recovers the `Value` shape this key originally held. Used
    /// by `keys()` so iteration returns the user's original type.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Bool(b) => Value::Bool(*b),
            Self::Int(n) => Value::Int(*n),
            Self::Char(c) => Value::Char(*c),
            Self::Str(s) => Value::String(s.clone()),
            // Aggregate keys don't round-trip to their original typed shape
            // (field names / element types aren't retained); `keys()` over a
            // struct-keyed map is unsupported, matching the compiled tier.
            Self::NonHashable | Self::Agg(_) => Value::Unit,
        }
    }
}

/// Boxed payload of [`Value::FloatArray`]. Pre-B1 this lived
/// inline in the enum (~48 bytes); behind `Arc` it costs 8 in
/// the variant.
#[derive(Debug, Clone)]
pub struct FloatArrayInner {
    /// Element-struct name (e.g. `"Body"`). Interned via
    /// `intern_type_name` so identical names share a single
    /// `&'static` allocation (~24 B + heap save per aggregate).
    pub name: &'static str,
    /// Number of `f64` fields per element.
    pub stride: u16,
    /// Field names in declaration order.
    pub field_names: Arc<Vec<String>>,
    /// Flat f64 storage. Length equals `stride * elem_count`.
    pub data: Arc<Vec<f64>>,
}

/// Boxed payload of [`Value::Variant`].
#[derive(Debug, Clone)]
pub struct VariantInner {
    /// Variant name (interned, see `intern_type_tag`).
    pub name: TypeTag,
    /// Positional fields stored inline for the common arity (≤ 2):
    /// `Some(x)`, `Ok`/`Err`, and a two-child enum node (linked-list
    /// `Cons`, tree `Node`) keep their payload in the same heap block
    /// as the `Arc<VariantInner>` header - one allocation per value
    /// instead of two, and 64 bytes rather than 80 for a two-field
    /// node (it lands in a smaller `mimalloc` size class). Arity > 2
    /// spills to the heap. Sharing goes through the outer `Arc`.
    pub fields: SmallVec<[Value; 2]>,
}

/// Boxed payload of [`Value::Struct`].
#[derive(Debug, Clone)]
pub struct StructInner {
    /// Struct name (interned, see `intern_type_tag`).
    pub name: TypeTag,
    /// Field name/value pairs in declaration order, stored inline. The
    /// field name is an interned `&'static str` (shared across every
    /// instance of the type) rather than an owned `String`, so a struct
    /// instance no longer heap-allocates its field names - for a program
    /// holding millions of structs that removed millions of per-field
    /// allocations and shrank each slot from 40 to 32 bytes. A
    /// `Box<[_]>` (not `Vec`) drops the unused capacity word: a struct's
    /// field count is fixed at construction.
    pub fields: Box<[(&'static str, Value)]>,
}

/// Boxed payload of [`Value::Builtin`]. Builtins are constructed
/// once at VM init and shared by `Arc`; cloning a `Value::Builtin`
/// is one refcount inc.
#[derive(Debug, Clone)]
pub struct BuiltinInner {
    /// Display name.
    pub name: &'static str,
    /// Implementation pointer.
    pub call: fn(&[Value]) -> RuntimeResult<Value>,
}

/// Boxed payload of [`Value::Native`].
#[derive(Debug, Clone)]
pub struct NativeInner {
    /// Display name.
    pub name: &'static str,
    /// Implementation pointer.
    pub call: NativeCall,
}

/// Tagged-pointer string with 7-byte inline storage (B2).
///
/// **Encoding.** A single 8-byte word `raw`. The high bit
/// distinguishes inline from heap:
/// - `raw >> 63 == 0`: inline. The low 7 bytes hold UTF-8 content
///   (little-endian); the eighth byte (byte index 7, the high
///   byte) holds the length in `0..=7`.
/// - `raw >> 63 == 1`: heap. The low 63 bits hold a pointer
///   produced by the thin RC byte-buffer allocator. On
///   `x86_64` / aarch64, user-space pointers fit in 48 bits, so
///   masking the high bit is lossless.
///
/// **Why this matters.** Without SSO, every `Value::String(SmolStr::from("Ok"))`
/// allocates a `String` on the heap *and* an `Arc` header (~32
/// bytes total). Variant names like `"Ok"` / `"Err"` / `"Some"`
/// / `"None"`, single-char field names, and most stack tags fit
/// in 7 bytes - so a steady-state hot loop now does zero heap
/// allocation for those values.
///
/// **Safety.** All pointer arithmetic is contained in this type.
/// `Drop` and `Clone` decrement / increment the underlying heap string
/// only when the heap tag is set; inline values are pure `u64`
/// values that don't own anything. The unsafe block in
/// `as_str` casts the storage to `&[u8]`; the bytes are
/// guaranteed UTF-8 because `from_str` only stores valid UTF-8
/// inline.
pub struct SmolStr {
    raw: u64,
}

const SMOL_HEAP_TAG: u64 = 1u64 << 63;
const SMOL_PTR_MASK: u64 = !SMOL_HEAP_TAG;
const SMOL_INLINE_MAX: usize = 7;

#[repr(C)]
struct HeapSmolStr {
    strong: AtomicU32,
    len: u32,
    cap: u32,
}

impl HeapSmolStr {
    fn layout(cap: usize) -> Layout {
        let header = Layout::new::<Self>();
        let bytes = Layout::array::<u8>(cap).expect("SmolStr heap layout overflow");
        header
            .extend(bytes)
            .expect("SmolStr heap layout overflow")
            .0
            .pad_to_align()
    }

    fn alloc_with_capacity(bytes: &[u8], cap: usize) -> *const Self {
        debug_assert!(cap >= bytes.len());
        Self::alloc_with_fill(bytes.len(), cap, |dst| {
            // SAFETY: `alloc_with_fill` passes `bytes.len()` writable payload
            // bytes; the fresh allocation cannot overlap the source slice.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            }
        })
    }

    fn alloc_ascii_upper(bytes: &[u8]) -> *const Self {
        Self::alloc_with_fill(bytes.len(), bytes.len(), |dst| {
            for (i, &b) in bytes.iter().enumerate() {
                let upper = if b.is_ascii_lowercase() {
                    b - (b'a' - b'A')
                } else {
                    b
                };
                // SAFETY: `alloc_with_fill` passes `bytes.len()` writable
                // payload bytes and this loop writes each byte exactly once.
                unsafe {
                    *dst.add(i) = upper;
                }
            }
        })
    }

    #[allow(
        clippy::cast_ptr_alignment,
        reason = "alloc uses HeapSmolStr::layout, whose alignment is at least HeapSmolStr's alignment"
    )]
    fn alloc_with_fill<F>(len: usize, cap: usize, fill: F) -> *const Self
    where
        F: FnOnce(*mut u8),
    {
        let len_u32 = u32::try_from(len).expect("SmolStr heap string too large");
        let cap_u32 = u32::try_from(cap).expect("SmolStr heap string too large");
        let layout = Self::layout(cap);
        // SAFETY: `layout` is non-zero and was computed for the header +
        // payload.
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        let header = ptr.cast::<Self>();
        // SAFETY: `header` points to a fresh allocation large enough for the
        // header plus `len` payload bytes.
        unsafe {
            header.write(Self {
                strong: AtomicU32::new(1),
                len: len_u32,
                cap: cap_u32,
            });
            fill(Self::bytes_mut(header));
        }
        header
    }

    unsafe fn bytes_ptr(header: *const Self) -> *const u8 {
        // SAFETY: caller guarantees `header` points to a valid `HeapSmolStr`.
        unsafe { header.cast::<u8>().add(std::mem::size_of::<Self>()) }
    }

    unsafe fn bytes_mut(header: *mut Self) -> *mut u8 {
        // SAFETY: caller guarantees `header` points to a valid mutable allocation.
        unsafe { header.cast::<u8>().add(std::mem::size_of::<Self>()) }
    }

    unsafe fn as_str<'a>(header: *const Self) -> &'a str {
        // SAFETY: caller guarantees `header` is live. Payload bytes came
        // from a `str`/`String`, so they are valid UTF-8.
        let len = unsafe { (*header).len as usize };
        let bytes = unsafe { std::slice::from_raw_parts(Self::bytes_ptr(header), len) };
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    unsafe fn inc(header: *const Self) {
        // SAFETY: caller owns a live strong reference to `header`.
        let prev = unsafe { (*header).strong.fetch_add(1, Ordering::Relaxed) };
        assert!(prev != u32::MAX, "SmolStr refcount overflow");
    }

    unsafe fn is_unique(header: *const Self) -> bool {
        // SAFETY: caller owns a live strong reference to `header`.
        unsafe { (*header).strong.load(Ordering::Acquire) == 1 }
    }

    unsafe fn append_unique(header: *mut Self, bytes: &[u8]) {
        // SAFETY: caller guarantees unique ownership and enough capacity.
        let len = unsafe { (*header).len as usize };
        let cap = unsafe { (*header).cap as usize };
        debug_assert!(len + bytes.len() <= cap);
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                Self::bytes_mut(header).add(len),
                bytes.len(),
            );
            (*header).len =
                u32::try_from(len + bytes.len()).expect("SmolStr heap string too large");
        }
    }

    unsafe fn dec(header: *const Self) {
        // SAFETY: caller owns one strong reference to `header`.
        if unsafe { (*header).strong.fetch_sub(1, Ordering::Release) } == 1 {
            std::sync::atomic::fence(Ordering::Acquire);
            let cap = unsafe { (*header).cap as usize };
            let layout = Self::layout(cap);
            // SAFETY: this is the final strong reference, so no other
            // thread can access the allocation after the release/acquire pair.
            unsafe {
                std::ptr::drop_in_place(header.cast_mut());
                dealloc(header.cast::<u8>().cast_mut(), layout);
            }
        }
    }
}

impl SmolStr {
    /// Empty string (inline, len 0).
    #[must_use]
    pub const fn new() -> Self {
        Self { raw: 0 }
    }

    /// Constructs an empty string with space reserved for at least `capacity`
    /// UTF-8 bytes.  Small hints stay inline; larger hints allocate the same
    /// thin, copy-on-write buffer used by appended strings so a mutable VM
    /// `String` can consume the reservation without reallocating.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        if capacity <= SMOL_INLINE_MAX {
            Self::new()
        } else {
            Self::new_heap_with_capacity(&[], capacity)
        }
    }

    /// Constructs a [`SmolStr`] from a borrowed `&str`. Strings
    /// up to 7 bytes are stored inline; longer strings allocate
    /// a fresh thin RC byte buffer.
    ///
    /// Intentionally not the [`std::str::FromStr`] trait method -
    /// `FromStr` returns `Result` to model fallible parsing and
    /// this conversion is infallible. Implementing the trait
    /// would force callers to `.unwrap()` an `Ok`-only path.
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "infallible conversion; FromStr would force callers to .unwrap()"
    )]
    pub fn from_str(s: &str) -> Self {
        if s.len() <= SMOL_INLINE_MAX {
            Self::new_inline(s.as_bytes())
        } else {
            Self::new_heap(s.as_bytes())
        }
    }

    /// Constructs a [`SmolStr`] from an owned [`String`]. Avoids
    /// re-allocating for inline-fitting strings; heap-bound strings
    /// move their bytes into the thin RC buffer.
    #[must_use]
    pub fn from_string(s: String) -> Self {
        if s.len() <= SMOL_INLINE_MAX {
            Self::new_inline(s.as_bytes())
        } else {
            Self::new_heap(s.as_bytes())
        }
    }

    /// Constructs an uppercase string, using a byte-wise fast path for ASCII
    /// and Rust's Unicode expansion for non-ASCII.
    #[must_use]
    pub fn to_uppercase_from(s: &str) -> Self {
        if !s.is_ascii() {
            return Self::from_string(s.to_uppercase());
        }
        let bytes = s.as_bytes();
        if bytes.len() <= SMOL_INLINE_MAX {
            let mut buf = [0u8; 8];
            for (i, &b) in bytes.iter().enumerate() {
                buf[i] = if b.is_ascii_lowercase() {
                    b - (b'a' - b'A')
                } else {
                    b
                };
            }
            buf[7] = bytes.len() as u8;
            Self {
                raw: u64::from_le_bytes(buf),
            }
        } else {
            let ptr = HeapSmolStr::alloc_ascii_upper(bytes) as usize as u64;
            debug_assert!(
                ptr & SMOL_HEAP_TAG == 0,
                "HeapSmolStr pointer must have high bit clear"
            );
            Self {
                raw: ptr | SMOL_HEAP_TAG,
            }
        }
    }

    /// Constructs from an existing `Arc<String>` - used by value
    /// registry paths that still expose strings that way.
    #[must_use]
    pub fn from_arc(arc: Arc<String>) -> Self {
        Self::from_str(arc.as_str())
    }

    fn new_inline(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= SMOL_INLINE_MAX);
        let mut buf = [0u8; 8];
        buf[..bytes.len()].copy_from_slice(bytes);
        // Length in the high byte (offset 7). High bit is 0,
        // so the heap tag is implicitly clear.
        buf[7] = bytes.len() as u8;
        Self {
            raw: u64::from_le_bytes(buf),
        }
    }

    fn new_heap(bytes: &[u8]) -> Self {
        Self::new_heap_with_capacity(bytes, bytes.len())
    }

    fn new_heap_with_capacity(bytes: &[u8], cap: usize) -> Self {
        debug_assert!(cap >= bytes.len());
        // SAFETY: `HeapSmolStr::alloc` returns an aligned allocation
        // obtained from the global allocator; user-space pointers on
        // supported targets fit in the low 63 bits, so OR-ing the tag
        // bit is information-preserving.
        let ptr = HeapSmolStr::alloc_with_capacity(bytes, cap) as usize as u64;
        debug_assert!(
            ptr & SMOL_HEAP_TAG == 0,
            "HeapSmolStr pointer must have high bit clear"
        );
        Self {
            raw: ptr | SMOL_HEAP_TAG,
        }
    }

    fn grown_capacity(current: usize, needed: usize) -> usize {
        debug_assert!(needed > current);
        let doubled = current.saturating_mul(2).max(16);
        doubled.max(needed)
    }

    /// Returns the borrowed string contents. Inline storage
    /// uses bytes from `self`; heap storage dereferences the
    /// underlying `String`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        if self.raw & SMOL_HEAP_TAG == 0 {
            // Inline: read length, return the prefix.
            // SAFETY: `new_inline` only writes valid UTF-8
            // bytes (since the input was a `&str`), so the
            // resulting prefix is valid UTF-8. The reference
            // ties its lifetime to `self`.
            let bytes: [u8; 8] = self.raw.to_le_bytes();
            let len = bytes[7] as usize;
            unsafe {
                let ptr = (&raw const self.raw).cast::<u8>();
                let slice = std::slice::from_raw_parts(ptr, len);
                std::str::from_utf8_unchecked(slice)
            }
        } else {
            // Heap: dereference the thin RC byte buffer.
            // SAFETY: only constructed via `HeapSmolStr::alloc`;
            // the strong count is at least 1 for the lifetime
            // of `self` (we hold one reference). We never give
            // out the raw pointer outside `Drop` / `Clone`.
            let ptr = (self.raw & SMOL_PTR_MASK) as *const HeapSmolStr;
            unsafe { HeapSmolStr::as_str(ptr) }
        }
    }

    /// Appends `s` in place. The heap variant keeps spare capacity and grows
    /// it when uniquely owned, so repeated appends to a `mut String` cost
    /// O(total length) instead of O(n^2). A shared heap string is copied once
    /// on the next append (copy-on-write). Inline storage appends in place
    /// until it exceeds the 7-byte window, then promotes to a heap string
    /// sized for both halves.
    pub fn push_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.raw & SMOL_HEAP_TAG == 0 {
            let len = self.raw.to_le_bytes()[7] as usize;
            if len + s.len() <= SMOL_INLINE_MAX {
                let mut buf = self.raw.to_le_bytes();
                buf[len..len + s.len()].copy_from_slice(s.as_bytes());
                buf[7] = (len + s.len()) as u8;
                self.raw = u64::from_le_bytes(buf);
            } else {
                let mut owned = String::with_capacity(len + s.len());
                owned.push_str(self.as_str());
                owned.push_str(s);
                *self = Self::new_heap_with_capacity(
                    owned.as_bytes(),
                    Self::grown_capacity(SMOL_INLINE_MAX, owned.len()),
                );
            }
        } else {
            let ptr = (self.raw & SMOL_PTR_MASK) as *mut HeapSmolStr;
            // SAFETY: `ptr` comes from a live heap SmolStr owned by `self`.
            let (len, cap, unique) = unsafe {
                (
                    (*ptr).len as usize,
                    (*ptr).cap as usize,
                    HeapSmolStr::is_unique(ptr),
                )
            };
            let needed = len + s.len();
            if unique && needed <= cap {
                // SAFETY: uniqueness and capacity are checked immediately above.
                unsafe { HeapSmolStr::append_unique(ptr, s.as_bytes()) };
                return;
            }
            // A VM builtin receives a cloned receiver before its write-back.
            // Copy-on-write therefore commonly takes this branch even when a
            // prior `String::with_capacity` reservation is large enough. Keep
            // that reservation on the replacement buffer; only grow when the
            // appended bytes exceed it.
            let new_cap = if needed <= cap {
                cap
            } else {
                Self::grown_capacity(cap, needed)
            };
            let mut owned = String::with_capacity(needed);
            owned.push_str(self.as_str());
            owned.push_str(s);
            let old = std::mem::replace(
                self,
                Self::new_heap_with_capacity(owned.as_bytes(), new_cap),
            );
            drop(old);
        }
    }

    /// Appends one Unicode scalar while retaining any reserved capacity.
    pub fn push(&mut self, ch: char) {
        let mut encoded = [0u8; 4];
        self.push_str(ch.encode_utf8(&mut encoded));
    }

    /// Returns the length in bytes (UTF-8 code units).
    #[must_use]
    pub fn len(&self) -> usize {
        if self.raw & SMOL_HEAP_TAG == 0 {
            (self.raw.to_le_bytes()[7]) as usize
        } else {
            self.as_str().len()
        }
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        if self.raw & SMOL_HEAP_TAG == 0 {
            SMOL_INLINE_MAX
        } else {
            let ptr = (self.raw & SMOL_PTR_MASK) as *const HeapSmolStr;
            // SAFETY: a heap-tagged SmolStr always owns a live HeapSmolStr.
            unsafe { (*ptr).cap as usize }
        }
    }

    /// Returns `true` iff the string has zero bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SmolStr {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SmolStr {
    fn clone(&self) -> Self {
        if self.raw & SMOL_HEAP_TAG != 0 {
            // SAFETY: we own a strong reference; reconstruct an
            // Arc to bump the count, then forget so we don't
            // drop our copy. The original raw stays valid.
            let ptr = (self.raw & SMOL_PTR_MASK) as *const HeapSmolStr;
            unsafe { HeapSmolStr::inc(ptr) };
        }
        Self { raw: self.raw }
    }
}

impl Drop for SmolStr {
    fn drop(&mut self) {
        if self.raw & SMOL_HEAP_TAG != 0 {
            // SAFETY: we own one strong reference produced by
            // `HeapSmolStr::alloc`. Decrementing releases it exactly once.
            let ptr = (self.raw & SMOL_PTR_MASK) as *const HeapSmolStr;
            unsafe { HeapSmolStr::dec(ptr) };
        }
    }
}

impl PartialEq for SmolStr {
    fn eq(&self, other: &Self) -> bool {
        // Fast path: both inline with same raw bits → equal.
        if self.raw == other.raw {
            return true;
        }
        self.as_str() == other.as_str()
    }
}

impl Eq for SmolStr {}

impl std::hash::Hash for SmolStr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialOrd for SmolStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SmolStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl fmt::Debug for SmolStr {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), out)
    }
}

impl fmt::Display for SmolStr {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.as_str())
    }
}

impl AsRef<str> for SmolStr {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::ops::Deref for SmolStr {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for SmolStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SmolStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl From<String> for SmolStr {
    fn from(s: String) -> Self {
        Self::from_string(s)
    }
}

impl From<&str> for SmolStr {
    fn from(s: &str) -> Self {
        Self::from_str(s)
    }
}

impl From<Arc<String>> for SmolStr {
    fn from(arc: Arc<String>) -> Self {
        Self::from_arc(arc)
    }
}

// SAFETY: heap storage is a thin atomic-refcounted immutable byte buffer.
// Inline storage is plain bytes copyable across threads.
unsafe impl Send for SmolStr {}
unsafe impl Sync for SmolStr {}

/// Compact integer identity for struct and enum-variant names.
///
/// The intern table owns one leaked `&'static str` per distinct name, while
/// each aggregate node stores only the numeric tag. Callers that need the text
/// recover it through [`Self::as_str`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeTag(Arc<str>);

/// Compatibility lookup only: it owns no type-name storage. Live values,
/// chunks, and VM sessions own the `Arc<str>` handles; stale entries are
/// replaced on the next lookup after their session is dropped.
static TYPE_TAGS: std::sync::LazyLock<
    parking_lot::Mutex<rustc_hash::FxHashMap<String, std::sync::Weak<str>>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(rustc_hash::FxHashMap::default()));

impl TypeTag {
    /// Returns the interned textual name for this tag.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the compact numeric identity stored in aggregate nodes.
    #[must_use]
    pub fn id(&self) -> u64 {
        Arc::as_ptr(&self.0).cast::<()>() as usize as u64
    }
}

impl fmt::Debug for TypeTag {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), out)
    }
}

impl fmt::Display for TypeTag {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.as_str())
    }
}

impl AsRef<str> for TypeTag {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for TypeTag {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for TypeTag {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// Returns a `&'static str` identity for `name`, allocating once
/// per distinct byte sequence. Used by [`Value::variant`],
/// [`Value::struct_`], and [`Value::float_array`] to deduplicate
/// type names across all values that share them - programs
/// typically have a fixed, small set of named types.
///
/// The leak is bounded by that set, not by call count.
#[must_use]
pub(crate) fn intern_type_name(name: &str) -> &'static str {
    static INTERNED: OnceLock<parking_lot::Mutex<rustc_hash::FxHashSet<&'static str>>> =
        OnceLock::new();
    let set = INTERNED.get_or_init(|| parking_lot::Mutex::new(rustc_hash::FxHashSet::default()));
    let mut guard = set.lock();
    if let Some(&s) = guard.get(name) {
        return s;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    guard.insert(leaked);
    leaked
}

/// Returns a compact identity for a type / variant name, allocating the name
/// text at most once through [`intern_type_name`].
#[must_use]
pub(crate) fn intern_type_tag(name: &str) -> TypeTag {
    let mut tags = TYPE_TAGS.lock();
    tags.retain(|_, weak| weak.strong_count() != 0);
    if let Some(existing) = tags.get(name).and_then(std::sync::Weak::upgrade) {
        return TypeTag(existing);
    }
    let owned: Arc<str> = Arc::from(name);
    tags.insert(name.to_owned(), Arc::downgrade(&owned));
    TypeTag(owned)
}

#[must_use]
fn type_tag_from_static(name: &'static str) -> TypeTag {
    intern_type_tag(name)
}

/// Closed integer range eligible for the small-variant cache, mirroring
/// the `CPython` small-int cache. Bounds the cache to
/// `names x (SMALL_INT_MAX - SMALL_INT_MIN + 1)` entries per thread.
const SMALL_VARIANT_INT_MIN: i64 = -128;
const SMALL_VARIANT_INT_MAX: i64 = 1024;
/// Max byte length of a single `String`-payload variant eligible for
/// interning. Bounded to the inline `SmolStr` range so the cache key never
/// holds a heap allocation, and so an unbounded space of distinct long
/// strings cannot grow the table. Covers the common case of a small set of
/// repeated string-payload variants (e.g. enum-like tags such as
/// `Str("alpha")` duplicated across many records).
const SMALL_VARIANT_STR_MAX: usize = 16;

/// Cache key for an interned single-small-scalar (or nullary) variant
/// node. `name` is an interned `&'static str`, unique per distinct
/// content, so it identifies the variant exactly.
#[derive(PartialEq, Eq, Hash)]
enum SmallVariantKey {
    Unit(TypeTag),
    Int(TypeTag, i64),
    Bool(TypeTag, bool),
    /// A single short `String` payload. The `SmolStr` is inline (≤ the
    /// `SMALL_VARIANT_STR_MAX` bound) so the key holds no heap allocation.
    Str(TypeTag, SmolStr),
}

thread_local! {
    /// Per-thread interning table for small immutable variant nodes
    /// (lever 3). Thread-local to avoid the cross-thread lock contention
    /// a shared global table would impose on per-connection VM threads.
    ///
    /// Holds a `Weak` rather than a strong reference so the cache never
    /// keeps a node alive on its own: identical small variants that are
    /// concurrently live share one allocation (every leaf of a tree), but
    /// once the last user reference drops, the node is freed and its
    /// liveness is observable through `downgrade()`/`upgrade()` exactly as
    /// for a non-interned node - preserving weak-reference tier parity.
    static SMALL_VARIANT_CACHE: std::cell::RefCell<
        rustc_hash::FxHashMap<SmallVariantKey, std::sync::Weak<VariantInner>>,
    > = std::cell::RefCell::new(rustc_hash::FxHashMap::default());
}

/// Returns the cache key if this `(name, fields)` pair is eligible for
/// small-variant interning: nullary, or a single `Int` in the cached
/// range, or a single `Bool`. Everything else (multi-field nodes,
/// large ints, aggregate payloads) returns `None` and allocates fresh.
fn small_variant_key(name: &TypeTag, fields: &[Value]) -> Option<SmallVariantKey> {
    match fields {
        [] => Some(SmallVariantKey::Unit(name.clone())),
        [Value::Int(n)] if (SMALL_VARIANT_INT_MIN..=SMALL_VARIANT_INT_MAX).contains(n) => {
            Some(SmallVariantKey::Int(name.clone(), *n))
        }
        [Value::Bool(b)] => Some(SmallVariantKey::Bool(name.clone(), *b)),
        // A single short string payload: immutable, so sharing one node
        // across all identical occurrences is sound exactly as for scalars.
        [Value::String(s)] if s.len() <= SMALL_VARIANT_STR_MAX => {
            Some(SmallVariantKey::Str(name.clone(), s.clone()))
        }
        _ => None,
    }
}

/// Returns a shared `Arc<VariantInner>` for an interning-eligible node:
/// reuses the cached node when a live one exists, otherwise allocates a
/// fresh one and records a `Weak` to it. The node is immutable, so all
/// aliases observe identical structure; the cache holding only a `Weak`
/// keeps liveness (and thus `Weak::upgrade`) faithful to the user's
/// references.
fn intern_small_variant(
    name: TypeTag,
    fields: Vec<Value>,
    key: SmallVariantKey,
) -> Arc<VariantInner> {
    SMALL_VARIANT_CACHE.with(|cache| {
        if let Some(existing) = cache.borrow().get(&key).and_then(std::sync::Weak::upgrade) {
            return existing;
        }
        let node = Arc::new(VariantInner {
            name,
            fields: variant_fields(fields),
        });
        cache.borrow_mut().insert(key, Arc::downgrade(&node));
        node
    })
}

/// Converts a constructor's temporary field `Vec` into the inline payload
/// storage used by ordinary enum nodes. `SmallVec::from_vec` only inlines when
/// the source Vec's capacity is <= the inline capacity; VM call-argument Vecs
/// are pooled and may carry a larger spare capacity from an unrelated call
/// site. For arity <= 2, force a move into inline storage so a two-field enum
/// node does not retain an accidental heap buffer.
fn variant_fields(fields: Vec<Value>) -> SmallVec<[Value; 2]> {
    if fields.len() <= 2 {
        fields.into_iter().collect()
    } else {
        SmallVec::from_vec(fields)
    }
}

/// Interns a struct field name to a `&'static str` with a leak
/// bounded by the program's fixed set of field names. Exposed for
/// `gossamer-binding`'s `#[derive(GosStruct)]` glue, which builds a
/// `Value::Struct` from runtime field-name strings.
#[must_use]
pub fn intern_field_name(name: &str) -> &'static str {
    intern_type_name(name)
}

/// Shared empty `Arc<Vec<Value>>` sentinel returned by every
/// constructor that would otherwise allocate a fresh empty `Vec`
/// plus Arc header (~32 B per call). All empty-payload variants
/// and arrays share this single allocation.
#[must_use]
pub(crate) fn empty_value_arc() -> Arc<Vec<Value>> {
    static EMPTY: OnceLock<Arc<Vec<Value>>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(Vec::new())))
}

/// Shared empty `Arc<Vec<(&'static str, Value)>>` sentinel for
/// field-less struct constructors.
#[must_use]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn empty_struct_fields() -> Arc<Vec<(&'static str, Value)>> {
    static EMPTY: OnceLock<Arc<Vec<(&'static str, Value)>>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(Vec::new())))
}

impl Value {
    /// Empty `Value::Array(Arc::new(Vec::new()))` shared across
    /// callers. Avoids the per-call 32 B allocation for empty
    /// results.
    #[must_use]
    pub fn empty_array() -> Self {
        Self::Array(empty_value_arc())
    }

    /// Empty `Value::Tuple(Arc::from(Vec::new()))` shared across
    /// callers.
    #[must_use]
    pub fn empty_tuple() -> Self {
        Self::Tuple(empty_value_arc())
    }

    /// Constructs a [`Value::Variant`] from owned name + shared
    /// field list. Hides the `Arc::new(VariantInner { … })`
    /// boilerplate at every constructor site.
    ///
    /// A node whose payload is a single small immutable scalar
    /// (`None`/`Nil`, `Some(0)`, an enum leaf like `Num(7)`) is shared
    /// from a thread-local cache instead of allocated fresh - the
    /// interpreter analog of the `CPython` small-int cache. Variant fields
    /// are never mutated in place (no `Arc::make_mut` site touches a
    /// `VariantInner`), so the shared node is immutable and safe to
    /// alias. The cache is thread-local rather than a global table, so
    /// there is no lock contention across per-connection VM threads.
    #[must_use]
    pub fn variant(name: impl AsRef<str>, fields: Vec<Value>) -> Self {
        let name = intern_type_tag(name.as_ref());
        // Keep bytecode-VM enum construction in the compact boxed
        // representation. Earlier builds eagerly converted any variant with a
        // registered native enum shape into the compiled-tier RC layout here.
        // That made pure interpretation pay a native handle allocation plus an
        // `Arc<NativeEnumOwner>` for every recursive tree / JSON-DOM node; the
        // stress `ast-rewrite` and `json-serde` benchmarks ballooned into
        // multi-GB RSS. Native representation is still built lazily at the JIT
        // boundary by `jit_call::build_variant_to_native_enum`, where it is
        // actually needed.
        if let Some(key) = small_variant_key(&name, &fields) {
            return Self::Variant(intern_small_variant(name, fields, key));
        }
        Self::Variant(Arc::new(VariantInner {
            name,
            fields: variant_fields(fields),
        }))
    }

    /// Constructs the boxed `Variant` representation unconditionally, never the
    /// native form. Required where a genuine `Variant` is the contract - most
    /// importantly `native_enum_to_variant`, which converts a native handle to
    /// the boxed form for equality / display / serde; routing it back through
    /// [`Value::variant`] would rebuild a native handle and loop forever.
    #[must_use]
    pub(crate) fn variant_boxed(name: &'static str, fields: Vec<Value>) -> Self {
        let name = type_tag_from_static(name);
        if let Some(key) = small_variant_key(&name, &fields) {
            return Self::Variant(intern_small_variant(name, fields, key));
        }
        Self::Variant(Arc::new(VariantInner {
            name,
            fields: variant_fields(fields),
        }))
    }
    /// Constructs a [`Value::Struct`].
    #[must_use]
    pub fn struct_(name: impl AsRef<str>, fields: Vec<(&'static str, Value)>) -> Self {
        Self::struct_with_tag(intern_type_tag(name.as_ref()), fields)
    }

    /// Constructs a [`Value::Struct`] from an already-interned type tag.
    ///
    /// Positional constructors keep their zero-field sentinel in the global
    /// table, so cloning that tag avoids taking the global type-tag lock for
    /// every aggregate built in a hot loop.
    #[must_use]
    pub(crate) fn struct_with_tag(name: TypeTag, fields: Vec<(&'static str, Value)>) -> Self {
        Self::Struct(Arc::new(StructInner {
            name,
            fields: fields.into_boxed_slice(),
        }))
    }

    /// Constructs a two-field integer struct without a temporary argument
    /// vector. Used by the bytecode VM's typed positional-constructor opcode.
    #[must_use]
    pub(crate) fn struct_2_i64(
        name: &'static str,
        field0: &'static str,
        first: i64,
        field1: &'static str,
        second: i64,
    ) -> Self {
        Self::Struct(Arc::new(StructInner {
            name: type_tag_from_static(name),
            fields: Box::new([(field0, Self::Int(first)), (field1, Self::Int(second))]),
        }))
    }
    /// Constructs a [`Value::FloatArray`].
    #[must_use]
    pub fn float_array(
        name: impl AsRef<str>,
        stride: u16,
        field_names: Arc<Vec<String>>,
        data: Arc<Vec<f64>>,
    ) -> Self {
        Self::FloatArray(Arc::new(FloatArrayInner {
            name: intern_type_name(name.as_ref()),
            stride,
            field_names,
            data,
        }))
    }
    /// Constructs a [`Value::Builtin`].
    #[must_use]
    pub fn builtin(name: &'static str, call: fn(&[Value]) -> RuntimeResult<Value>) -> Self {
        Self::Builtin(Arc::new(BuiltinInner { name, call }))
    }
    /// Constructs a [`Value::Native`].
    #[must_use]
    pub fn native(name: &'static str, call: NativeCall) -> Self {
        Self::Native(Arc::new(NativeInner { name, call }))
    }
}

/// Shared channel backing a `(Sender<T>, Receiver<T>)` pair.
///
/// Capacity semantics mirror modern Go where `0` is an unbuffered
/// rendezvous channel, positive values are bounded buffers, and
/// `Channel::unbounded()` is the explicit queue form retained for
/// Gossamer code that wants non-blocking producer growth.
#[derive(Clone)]
pub struct Channel {
    inner: Arc<ChannelInner>,
}

struct ChannelInner {
    state: Mutex<ChannelState>,
    cv: parking_lot::Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelCapacity {
    Unbuffered,
    Unbounded,
    Bounded(usize),
}

struct ChannelMessage {
    id: u64,
    value: Value,
}

struct ChannelState {
    buf: VecDeque<ChannelMessage>,
    capacity: ChannelCapacity,
    closed: bool,
    next_send_id: u64,
    waiting_receivers: usize,
    select_waiters: Vec<Arc<SelectWaiter>>,
}

/// Wait handle used by the bytecode VM to park one `select` expression
/// across several channels and wake when any arm may be ready.
pub struct SelectWaiter {
    ready: Mutex<bool>,
    cv: parking_lot::Condvar,
}

impl SelectWaiter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ready: Mutex::new(false),
            cv: parking_lot::Condvar::new(),
        })
    }

    fn wake(&self) {
        let mut ready = self.ready.lock();
        *ready = true;
        self.cv.notify_all();
    }

    fn wait(&self) {
        let mut ready = self.ready.lock();
        while !*ready {
            self.cv.wait(&mut ready);
        }
    }
}

impl Channel {
    /// Constructs a new unbuffered channel.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Constructs an explicit unbounded queue channel.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::with_mode(ChannelCapacity::Unbounded)
    }

    /// Constructs a channel with the given buffered capacity. A
    /// `capacity` of `0` is unbuffered; a positive value bounds the
    /// buffer so a send parks once the buffer reaches capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        if capacity == 0 {
            Self::with_mode(ChannelCapacity::Unbuffered)
        } else {
            Self::with_mode(ChannelCapacity::Bounded(capacity))
        }
    }

    fn with_mode(capacity: ChannelCapacity) -> Self {
        Self {
            inner: Arc::new(ChannelInner {
                state: Mutex::new(ChannelState {
                    buf: VecDeque::new(),
                    capacity,
                    closed: false,
                    next_send_id: 1,
                    waiting_receivers: 0,
                    select_waiters: Vec::new(),
                }),
                cv: parking_lot::Condvar::new(),
            }),
        }
    }

    /// Pushes `value` onto the channel and notifies any parked
    /// receiver so it can re-check. On a bounded channel (positive
    /// capacity) the caller parks on the condvar while the buffer is at
    /// capacity, so a producer outrunning its consumer applies
    /// backpressure exactly as the compiled tier's `gos_rt_chan_send`.
    pub fn send(&self, value: Value) {
        let mut guard = self.inner.state.lock();
        match guard.capacity {
            ChannelCapacity::Unbuffered => {
                let id = guard.next_send_id;
                guard.next_send_id = guard.next_send_id.wrapping_add(1).max(1);
                guard.buf.push_back(ChannelMessage { id, value });
                self.notify_channel_changed(&mut guard);
                while guard.buf.iter().any(|msg| msg.id == id) && !guard.closed {
                    self.inner.cv.wait(&mut guard);
                }
            }
            ChannelCapacity::Unbounded => {
                guard.buf.push_back(ChannelMessage { id: 0, value });
                self.notify_channel_changed(&mut guard);
            }
            ChannelCapacity::Bounded(capacity) => {
                while guard.buf.len() >= capacity {
                    self.inner.cv.wait(&mut guard);
                }
                guard.buf.push_back(ChannelMessage { id: 0, value });
                self.notify_channel_changed(&mut guard);
            }
        }
    }

    /// Non-blocking send. Enqueues `value` and returns `true` when the
    /// operation can complete immediately; returns
    /// `false` without enqueueing when a bounded buffer is at capacity.
    /// Used by `select` so a full send arm reads as not-ready instead
    /// of blocking inside the readiness probe.
    #[must_use]
    pub fn try_send(&self, value: Value) -> bool {
        let mut guard = self.inner.state.lock();
        match guard.capacity {
            ChannelCapacity::Unbuffered => {
                if guard.waiting_receivers == 0 {
                    return false;
                }
                guard.buf.push_back(ChannelMessage { id: 0, value });
                self.notify_channel_changed(&mut guard);
                true
            }
            ChannelCapacity::Unbounded => {
                guard.buf.push_back(ChannelMessage { id: 0, value });
                self.notify_channel_changed(&mut guard);
                true
            }
            ChannelCapacity::Bounded(capacity) => {
                if guard.buf.len() >= capacity {
                    return false;
                }
                guard.buf.push_back(ChannelMessage { id: 0, value });
                self.notify_channel_changed(&mut guard);
                true
            }
        }
    }

    /// Marks the channel as closed and wakes every parked receiver
    /// so they observe the closed state and exit their wait. Returns
    /// `true` when this call performed the close and `false` when the
    /// channel was already closed - the caller turns the latter into
    /// a `close of closed channel` panic, matching Go. That panic is
    /// goroutine-scoped, so it ends only the offending goroutine
    /// (fatal on `main`) and never aborts the whole process.
    #[must_use]
    pub fn close(&self) -> bool {
        let mut guard = self.inner.state.lock();
        if guard.closed {
            return false;
        }
        guard.closed = true;
        self.notify_channel_changed(&mut guard);
        true
    }

    /// Non-blocking receive. Returns `None` when the channel is
    /// empty (regardless of close state - callers that need
    /// drain-aware semantics should use [`Channel::recv`]).
    #[must_use]
    pub fn try_recv(&self) -> Option<Value> {
        let mut guard = self.inner.state.lock();
        let value = guard.buf.pop_front().map(|msg| msg.value);
        if value.is_some() {
            self.notify_channel_changed(&mut guard);
        }
        value
    }

    /// Blocking receive. Parks until a value is available or the
    /// channel is closed AND drained. Returns `None` only after
    /// observing `closed = true && buf.is_empty()`. Mirrors Go's
    /// `v, ok := <-ch` shape so `while let Some(v) = rx.recv()`
    /// drains and exits cleanly when the producer closes.
    #[must_use]
    pub fn recv(&self) -> Option<Value> {
        let mut guard = self.inner.state.lock();
        loop {
            if let Some(msg) = guard.buf.pop_front() {
                self.notify_channel_changed(&mut guard);
                return Some(msg.value);
            }
            if guard.closed {
                return None;
            }
            guard.waiting_receivers += 1;
            self.wake_select_waiters(&guard);
            self.inner.cv.wait(&mut guard);
            guard.waiting_receivers = guard.waiting_receivers.saturating_sub(1);
        }
    }

    /// Blocking receive which also observes a caller-provided cancellation
    /// predicate. A queued value wins over cancellation, matching the native
    /// runtime's receive ordering. The bounded wait makes cancellation visible
    /// even though a Context does not share this channel's condvar.
    #[must_use]
    pub fn recv_with_cancel(&self, is_cancelled: impl Fn() -> bool) -> Option<Value> {
        let mut guard = self.inner.state.lock();
        loop {
            if let Some(msg) = guard.buf.pop_front() {
                self.notify_channel_changed(&mut guard);
                return Some(msg.value);
            }
            if guard.closed || is_cancelled() {
                return None;
            }
            guard.waiting_receivers += 1;
            self.wake_select_waiters(&guard);
            self.inner
                .cv
                .wait_for(&mut guard, Duration::from_millis(50));
            guard.waiting_receivers = guard.waiting_receivers.saturating_sub(1);
        }
    }

    /// Returns `true` when the channel currently has at least one
    /// pending value. Used by `select` to pick a ready arm.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.inner.state.lock().buf.is_empty()
    }

    /// `true` when both buffer drained and channel closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        let guard = self.inner.state.lock();
        guard.closed && guard.buf.is_empty()
    }

    /// Registers a `select` waiter that will be woken when this
    /// channel's readiness may have changed.
    pub fn register_select_waiter(&self, waiter: &Arc<SelectWaiter>) {
        let mut guard = self.inner.state.lock();
        if !guard.select_waiters.iter().any(|w| Arc::ptr_eq(w, waiter)) {
            guard.select_waiters.push(Arc::clone(waiter));
        }
    }

    /// Removes a previously registered `select` waiter.
    pub fn unregister_select_waiter(&self, waiter: &Arc<SelectWaiter>) {
        let mut guard = self.inner.state.lock();
        guard.select_waiters.retain(|w| !Arc::ptr_eq(w, waiter));
    }

    /// Constructs a waiter for a blocking `select`.
    #[must_use]
    pub fn select_waiter() -> Arc<SelectWaiter> {
        SelectWaiter::new()
    }

    /// Blocks until any registered channel wakes the waiter.
    pub fn wait_select(waiter: &SelectWaiter) {
        waiter.wait();
    }

    fn notify_channel_changed(&self, guard: &mut ChannelState) {
        self.inner.cv.notify_all();
        self.wake_select_waiters(guard);
    }

    fn wake_select_waiters(&self, guard: &ChannelState) {
        let waiters = guard.select_waiters.clone();
        for waiter in waiters {
            waiter.wake();
        }
    }
}

impl Default for Channel {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "<channel len={}>", self.inner.state.lock().buf.len())
    }
}

/// Callback handed to [`Value::Native`] builtins. Exposes the subset
/// of the interpreter needed to dispatch back into Gossamer code.
pub trait NativeDispatch {
    /// Invokes a top-level function by name with the given arguments.
    fn call_fn(&mut self, name: &str, args: Vec<Value>) -> RuntimeResult<Value>;
    /// Invokes an arbitrary callable [`Value`]: builtin, native, or
    /// closure. Used by higher-order native builtins (e.g.
    /// `Option::map`) that receive a Gossamer closure as an argument.
    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> RuntimeResult<Value>;
    /// Spawns `callable` in a fresh worker thread with the supplied
    /// arguments. A panic in the spawned callable is isolated to the
    /// worker and does not propagate to the caller.
    fn spawn_callable(&mut self, callable: Value, args: Vec<Value>) -> RuntimeResult<()>;
    /// Spawns `callable` and returns a one-shot channel handle that
    /// `.join()` blocks on for the outcome (`Ok(value)`, or
    /// `Err(message)` if the callable panicked). Backs `spawn(f)`.
    fn spawn_join(&mut self, callable: Value, args: Vec<Value>) -> RuntimeResult<Value>;
}

/// Function pointer for [`Value::Native`] builtins.
pub type NativeCall = fn(&mut dyn NativeDispatch, &[Value]) -> RuntimeResult<Value>;

impl Value {
    /// Returns the unit value.
    #[must_use]
    pub const fn unit() -> Self {
        Self::Unit
    }

    /// Returns `true` when this value is `true` in boolean contexts.
    #[must_use]
    pub const fn is_truthy(&self) -> bool {
        matches!(self, Self::Bool(true))
    }

    /// Rehydrates a [`Value::FloatArray`] into the boxed
    /// [`Value::Array`] of [`Value::Struct`] representation.
    /// Used at every code path where a flat aggregate meets
    /// code that expects the generic shape - ABI crossings,
    /// `EvalDeferred`, `Display`, etc.
    ///
    /// # Panics
    ///
    /// Panics if `self` is not a [`Value::FloatArray`].
    #[must_use]
    pub fn float_array_to_value_array(&self) -> Value {
        let Self::FloatArray(inner) = self else {
            panic!("float_array_to_value_array: not a FloatArray");
        };
        let stride = inner.stride as usize;
        let elem_count = inner.data.len().checked_div(stride).unwrap_or(0);
        let mut out = Vec::with_capacity(elem_count);
        for i in 0..elem_count {
            let base = i * stride;
            let mut fields: Vec<(&'static str, Value)> =
                Vec::with_capacity(inner.field_names.len());
            for (j, fname) in inner.field_names.iter().enumerate() {
                fields.push((
                    crate::value::intern_type_name(fname.as_str()),
                    Value::Float(inner.data[base + j]),
                ));
            }
            out.push(Value::struct_(
                inner.name,
                Arc::unwrap_or_clone(Arc::new(fields)),
            ));
        }
        Value::Array(Arc::new(out))
    }

    /// Convenience wrapper that returns the rehydrated element
    /// vector of a [`Value::FloatArray`] so callers that just
    /// need to iterate struct elements don't have to match the
    /// outer [`Value::Array`].
    #[must_use]
    pub fn float_array_elems(&self) -> Vec<Value> {
        let Value::Array(a) = self.float_array_to_value_array() else {
            unreachable!()
        };
        a.as_ref().clone()
    }

    /// Serialises `self` into the canonical `u64` value layout.
    ///
    /// Inline scalars encode directly; heap objects are stored in the
    /// global side table and the returned word carries their handle.
    #[must_use]
    pub fn to_raw(&self) -> GossamerValue {
        match self {
            Self::NativeEnum(o) => native_enum_to_variant(o).to_raw(),
            // Write-back cells never escape the call protocol; if a
            // boundary serialises one anyway, its current inner value
            // is the only meaningful payload.
            Self::MutCell(c) => c.lock().to_raw(),
            Self::Unit => from_singleton(SINGLETON_UNIT),
            Self::Bool(false) => from_singleton(SINGLETON_FALSE),
            Self::Bool(true) => from_singleton(SINGLETON_TRUE),
            Self::Int(n) => {
                if fits_i56(*n) {
                    from_i64(*n)
                } else {
                    let id = register_heap(RegistryEntry::Int(*n));
                    from_heap_handle(id)
                }
            }
            Self::Float(f) => from_f64(*f),
            Self::Char(c) => {
                let payload = ((*c as u64) << 2) | 3;
                from_singleton(payload)
            }
            Self::String(s) => {
                // Preserve the VM string allocation in the raw side table.
                // `SmolStr::clone` is a refcount bump for heap strings and a
                // word copy for inline strings, so this boundary neither
                // materialises an `Arc<String>` nor copies its bytes.
                let id = register_heap(RegistryEntry::String(s.clone()));
                from_heap_handle(id)
            }
            // The compact raw ABI has no JSON-tree representation. JSON
            // values are intentionally interpreter-local; callers crossing
            // this boundary receive the same sentinel as other opaque values.
            Self::Json(_) => from_singleton(SINGLETON_UNIT),
            Self::Tuple(t) => {
                let id = register_heap(RegistryEntry::Tuple(Arc::clone(t)));
                from_heap_handle(id)
            }
            Self::Array(a) => {
                let id = register_heap(RegistryEntry::Array(Arc::clone(a)));
                from_heap_handle(id)
            }
            Self::FloatArray(data) => {
                // The tagged word only names a VM-owned side-table entry, so
                // retain the typed storage instead of rehydrating every
                // element into a boxed array at the JIT boundary.
                let id = register_heap(RegistryEntry::FloatArray(Arc::clone(data)));
                from_heap_handle(id)
            }
            Self::IntArray(data) => {
                // Keep the compact typed storage shared across the boundary.
                let id = register_heap(RegistryEntry::IntArray(Arc::clone(data)));
                from_heap_handle(id)
            }
            Self::FloatVec(data) => {
                // Keep the compact typed storage shared across the boundary.
                let id = register_heap(RegistryEntry::FloatVec(Arc::clone(data)));
                from_heap_handle(id)
            }
            Self::Variant(inner) => {
                let id = register_heap(RegistryEntry::Variant(Arc::clone(inner)));
                from_heap_handle(id)
            }
            Self::Struct(inner) => {
                let id = register_heap(RegistryEntry::Struct(Arc::clone(inner)));
                from_heap_handle(id)
            }
            Self::Closure(c) => {
                let id = register_heap(RegistryEntry::Closure(Arc::clone(c)));
                from_heap_handle(id)
            }
            Self::Channel(ch) => {
                let id = register_heap(RegistryEntry::Channel(ch.clone()));
                from_heap_handle(id)
            }
            Self::Uint(n) => {
                let n_i = *n as i64;
                if fits_i56(n_i) {
                    from_i64(n_i)
                } else {
                    let id = register_heap(RegistryEntry::Int(n_i));
                    from_heap_handle(id)
                }
            }
            Self::Map(_)
            | Self::IntMap(_)
            | Self::StrIntMap(_)
            | Self::LazyIter(_)
            | Self::Builtin(_)
            | Self::Native(_)
            | Self::Weak(_)
            | Self::Void => {
                // Unencodable in the raw layout - return a sentinel
                // that `from_raw` maps back to `Void`.
                from_singleton(SINGLETON_UNIT)
            }
        }
    }

    /// Deserialises a [`GossamerValue`] into the interpreter's
    /// convenience wrapper.  The inverse of [`Self::to_raw`].
    #[must_use]
    pub fn from_raw(raw: GossamerValue) -> Self {
        match tag_of(raw) {
            TAG_IMMEDIATE => Self::Int(to_i64(raw)),
            TAG_FLOAT => Self::Float(to_f64(raw)),
            TAG_SINGLETON => {
                let disc = to_singleton(raw);
                match disc {
                    SINGLETON_UNIT => Self::Unit,
                    SINGLETON_FALSE => Self::Bool(false),
                    SINGLETON_TRUE => Self::Bool(true),
                    _ => {
                        let low = disc & 3;
                        if low == 3 {
                            let codepoint = (disc >> 2) as u32;
                            Self::Char(char::from_u32(codepoint).unwrap_or('\0'))
                        } else {
                            Self::Void
                        }
                    }
                }
            }
            TAG_HEAP => {
                let id = to_heap_handle(raw);
                match take_heap(id) {
                    Some(RegistryEntry::Int(n)) => Self::Int(n),
                    Some(RegistryEntry::String(s)) => Self::String(s),
                    Some(RegistryEntry::Tuple(t)) => Self::Tuple(t),
                    Some(RegistryEntry::Array(a)) => Self::Array(a),
                    Some(RegistryEntry::FloatArray(a)) => Self::FloatArray(a),
                    Some(RegistryEntry::IntArray(a)) => Self::IntArray(a),
                    Some(RegistryEntry::FloatVec(a)) => Self::FloatVec(a),
                    Some(RegistryEntry::Variant(inner)) => Self::Variant(inner),
                    Some(RegistryEntry::Struct(inner)) => Self::Struct(inner),
                    Some(RegistryEntry::Closure(c)) => Self::Closure(c),
                    Some(RegistryEntry::Channel(ch)) => Self::Channel(ch),
                    None => Self::Void,
                }
            }
            _ => Self::Void,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Primitive formatting delegates to the shared runtime
        // helpers so the interpreter and the native backend produce
        // byte-identical text. the parity plan.
        match self {
            Self::NativeEnum(o) => fmt::Display::fmt(&native_enum_to_variant(o), out),
            // Cells render as their inner value - they are a call-
            // protocol artifact, never a user-visible shape.
            Self::MutCell(c) => {
                let inner = c.lock().clone();
                fmt::Display::fmt(&inner, out)
            }
            Self::Unit => out.write_str(gossamer_runtime::builtins::format_unit()),
            Self::Bool(b) => out.write_str(gossamer_runtime::builtins::format_bool(*b)),
            Self::Int(i) => out.write_str(&gossamer_runtime::builtins::format_int(*i)),
            Self::Float(f) => out.write_str(&gossamer_runtime::builtins::format_float(*f)),
            Self::Char(c) => write!(out, "{c}"),
            Self::String(s) => out.write_str(s),
            Self::Json(value) => out.write_str(&gossamer_std::json::encode(value.as_value())),
            Self::Tuple(parts) => write_tuple(out, parts),
            Self::Array(parts) => write_array(out, parts),
            Self::FloatArray(_) => write_array(out, &self.float_array_elems()),
            Self::IntArray(data) => {
                let elems: Vec<Value> = data.iter().copied().map(Value::Int).collect();
                write_array(out, &elems)
            }
            Self::FloatVec(data) => {
                let elems: Vec<Value> = data.iter().copied().map(Value::Float).collect();
                write_array(out, &elems)
            }
            Self::LazyIter(id) => match crate::stdlib_builtins::iter::lazy_iter_repr(*id) {
                Some(range) => out.write_str(&range),
                None => out.write_str("<iterator>"),
            },
            Self::Variant(inner) => write_variant(out, inner.name.as_str(), &inner.fields),
            Self::Struct(inner) => {
                // Placeholder expressions evaluate to this sentinel in the VM;
                // the compiled tiers emit "<value>" for the same cases.
                if inner.name == "<stub>" {
                    return out.write_str("<value>");
                }
                // `errors::Error` prints Go-style as its colon-joined
                // cause chain ("outer: mid: root") so `format!("{}", e)`
                // and `?`-surfaced errors match the compiled tiers'
                // `gos_rt_error_display` path. `.message()` stays
                // top-level-only. Other structs keep the default
                // `Name { f: v, … }` shape used everywhere else.
                if inner.name == "errors::Error" {
                    if let Some(msg) = inner
                        .fields
                        .iter()
                        .find(|(n, _)| (*n) == "message")
                        .map(|(_, v)| v.clone())
                    {
                        write!(out, "{msg}")?;
                        let mut cursor = inner
                            .fields
                            .iter()
                            .find(|(n, _)| (*n) == "cause")
                            .map(|(_, v)| v.clone());
                        while let Some(Self::Variant(link)) = cursor {
                            if link.name != "Some" || link.fields.is_empty() {
                                break;
                            }
                            let Self::Struct(cause) = &link.fields[0] else {
                                break;
                            };
                            if cause.name != "errors::Error" {
                                break;
                            }
                            let Some((_, m)) = cause.fields.iter().find(|(n, _)| (*n) == "message")
                            else {
                                break;
                            };
                            write!(out, ": {m}")?;
                            cursor = cause
                                .fields
                                .iter()
                                .find(|(n, _)| (*n) == "cause")
                                .map(|(_, v)| v.clone());
                        }
                        return Ok(());
                    }
                }
                write_struct(out, inner.name.as_str(), &inner.fields)
            }
            Self::Closure(_) => out.write_str("<closure>"),
            Self::Builtin(inner) => write!(out, "<builtin {}>", inner.name),
            Self::Native(inner) => write!(out, "<native {}>", inner.name),
            Self::Channel(ch) => write!(out, "{ch:?}"),
            Self::Map(map) => write_map(out, &map.lock()),
            Self::IntMap(map) => write_int_map(out, &map.lock()),
            Self::StrIntMap(map) => write_str_int_map(out, &map.lock()),
            Self::Uint(n) => write!(out, "{n}"),
            Self::Weak(_) => out.write_str("<weak>"),
            Self::Void => out.write_str("<void>"),
        }
    }
}

fn repr_value(value: &Value) -> String {
    match value {
        Value::String(text) => format!("{:?}", text.as_str()),
        Value::Char(ch) => format!("{ch:?}"),
        Value::Tuple(parts) => {
            let mut rendered: Vec<String> = parts.iter().map(repr_value).collect();
            if rendered.len() == 1 {
                rendered[0].push(',');
            }
            format!("({})", rendered.join(", "))
        }
        Value::Array(parts) => format!(
            "[{}]",
            parts.iter().map(repr_value).collect::<Vec<_>>().join(", ")
        ),
        Value::FloatArray(_) => repr_value(&Value::Array(Arc::new(value.float_array_elems()))),
        Value::IntArray(data) => format!("{:?}", data.as_slice()),
        Value::FloatVec(data) => format!("{:?}", data.as_slice()),
        Value::LazyIter(id) => crate::stdlib_builtins::iter::lazy_iter_repr(*id)
            .unwrap_or_else(|| "<iterator>".to_string()),
        Value::Variant(inner) => {
            let fields = inner.fields.iter().map(repr_value).collect::<Vec<_>>();
            if fields.is_empty() {
                inner.name.as_str().to_string()
            } else {
                format!("{}({})", inner.name.as_str(), fields.join(", "))
            }
        }
        Value::Struct(inner) => repr_struct(inner.name.as_str(), &inner.fields),
        Value::Map(map) => {
            let map = map.lock();
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(key, item)| format!(
                        "{}: {}",
                        repr_value(&key.to_value()),
                        repr_value(item)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::StrIntMap(map) => {
            let map = map.lock();
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(key, item)| format!("{:?}: {item}", key.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::MutCell(cell) => repr_value(&cell.lock()),
        Value::NativeEnum(owner) => repr_value(&native_enum_to_variant(owner)),
        _ => value.to_string(),
    }
}

fn repr_struct(name: &str, fields: &[(&'static str, Value)]) -> String {
    let is_tuple_struct = !fields.is_empty()
        && fields
            .iter()
            .enumerate()
            .all(|(i, (n, _))| n.parse::<usize>() == Ok(i));
    if is_tuple_struct {
        let fields = fields
            .iter()
            .map(|(_, field)| repr_value(field))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("{name}({fields})");
    }
    format!(
        "{name} {{ {} }}",
        fields
            .iter()
            .map(|(field_name, field)| format!("{field_name}: {}", repr_value(field)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn write_tuple(out: &mut fmt::Formatter<'_>, parts: &[Value]) -> fmt::Result {
    out.write_str("(")?;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{part}")?;
    }
    if parts.len() == 1 {
        out.write_str(",")?;
    }
    out.write_str(")")
}

/// Renders a `HashMap` as `{k: v, …}` with entries sorted by key so
/// the output is deterministic and byte-identical to the compiled
/// tiers' `gos_rt_map_format` (native map storage has its own
/// implementation-defined order).
fn write_map(out: &mut fmt::Formatter<'_>, map: &DenseMap<MapKey, Value>) -> fmt::Result {
    out.write_str("{")?;
    let mut entries: Vec<(&MapKey, &Value)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{}: {v}", k.to_value())?;
    }
    out.write_str("}")
}

/// Key-sorted rendering of an `i64`-keyed, `i64`-valued map. Mirrors
/// [`write_map`] for the [`Value::IntMap`] storage shape.
fn write_int_map(out: &mut fmt::Formatter<'_>, map: &DenseMap<i64, i64>) -> fmt::Result {
    out.write_str("{")?;
    let mut entries: Vec<(&i64, &i64)> = map.iter().collect();
    entries.sort_by_key(|&(k, _)| *k);
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{k}: {v}")?;
    }
    out.write_str("}")
}

/// Key-sorted rendering of a `String`-keyed, `i64`-valued map.
/// Mirrors [`write_map`] for the [`Value::StrIntMap`] storage shape,
/// quoting keys exactly as the generic map's string keys render.
fn write_str_int_map(out: &mut fmt::Formatter<'_>, map: &DenseMap<SmolStr, i64>) -> fmt::Result {
    out.write_str("{")?;
    let mut entries: Vec<(&SmolStr, &i64)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{}: {v}", k.as_str())?;
    }
    out.write_str("}")
}

fn write_array(out: &mut fmt::Formatter<'_>, parts: &[Value]) -> fmt::Result {
    out.write_str("[")?;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{part}")?;
    }
    out.write_str("]")
}

fn write_variant(out: &mut fmt::Formatter<'_>, name: &str, fields: &[Value]) -> fmt::Result {
    out.write_str(name)?;
    if fields.is_empty() {
        return Ok(());
    }
    out.write_str("(")?;
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{field}")?;
    }
    out.write_str(")")
}

fn write_struct(
    out: &mut fmt::Formatter<'_>,
    name: &str,
    fields: &[(&'static str, Value)],
) -> fmt::Result {
    out.write_str(name)?;
    // A tuple struct's fields are named "0".."N-1"; render it as
    // `Name(v0, v1)` to match the derived `fmt` on the compiled tiers.
    let is_tuple_struct = !fields.is_empty()
        && fields
            .iter()
            .enumerate()
            .all(|(i, (n, _))| n.parse::<usize>() == Ok(i));
    if is_tuple_struct {
        out.write_str("(")?;
        for (i, (_, value)) in fields.iter().enumerate() {
            if i > 0 {
                out.write_str(", ")?;
            }
            out.write_str(&repr_value(value))?;
        }
        return out.write_str(")");
    }
    out.write_str(" { ")?;
    for (i, (ident, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{}: {}", (*ident), repr_value(value))?;
    }
    out.write_str(" }")
}

/// Concrete closure representation.
///
/// The bytecode VM compiles the closure body to its own `FnChunk`
/// whose leading parameters are the captured upvalues, followed by the
/// declared parameters. [`Self::chunk`] holds that body and
/// [`Self::capture_values`] the snapshotted upvalue `Value`s; the VM
/// invokes the closure by running the chunk with `capture_values ++
/// args` in the leading registers. The chunk's `arity` minus
/// `capture_values.len()` is the closure's declared parameter count.
#[derive(Debug, Clone)]
pub struct Closure {
    /// Native bytecode body run by the VM. Its leading registers hold
    /// the captured upvalues, then the declared parameters.
    pub chunk: Arc<crate::bytecode::FnChunk>,
    /// Upvalue snapshot, positionally aligned with the chunk's leading
    /// parameters. Scalars are by-value snapshots; aggregates share
    /// their `Arc` backing, so a mutation through the closure is visible
    /// to the original binding.
    pub capture_values: Vec<Value>,
}

/// Replaces any `MutCell` argument with a clone of its inner value.
/// Builtins and natives have no parameter table, so they receive the
/// plain aggregate; user functions keep the cell for write-back.
pub(crate) fn unwrap_mut_cells(mut args: Vec<Value>) -> Vec<Value> {
    for arg in &mut args {
        if let Value::MutCell(cell) = arg {
            let inner = cell.lock().clone();
            *arg = inner;
        }
    }
    args
}

/// Result type used throughout the interpreter for operations that can
/// abort with a runtime error.
pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// Top-level interpreter errors. Each variant carries a stable
/// diagnostic code (`GX0001` …) that both the interpreter and the
/// native backend use when reporting the same failure - the
/// "unified error code catalogue" half of
/// the parity plan.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeError {
    /// An operation was applied to a value of the wrong kind.
    #[error("error[GX0001]: type error at runtime: {0}")]
    Type(String),
    /// A name lookup failed when interpreting a path expression.
    #[error("error[GX0002]: name `{0}` is not bound in this scope")]
    UnresolvedName(String),
    /// A call site supplied the wrong number of arguments.
    #[error("error[GX0003]: wrong number of arguments: expected {expected}, found {found}")]
    Arity {
        /// Declared arity.
        expected: usize,
        /// Supplied argument count.
        found: usize,
    },
    /// Integer division by zero or arithmetic overflow.
    #[error("error[GX0004]: arithmetic error: {0}")]
    Arithmetic(String),
    /// `panic!(...)` invoked from user code or an exhausted match.
    #[error("error[GX0005]: panic: {0}")]
    Panic(String),
    /// A `match` expression failed to match any arm.
    #[error("error[GX0006]: no match for scrutinee at runtime")]
    MatchFailure,
    /// An unimplemented construct was reached while walking the tree.
    #[error("error[GX0007]: interpreter does not yet support {0}")]
    Unsupported(&'static str),
    /// Goroutine call depth exceeded the VM limit.
    #[error("error[GX0008]: stack overflow - call depth exceeded {0} frames")]
    StackOverflow(usize),
    /// Execution budget exhausted (the playground caps loop iterations so an
    /// unbounded loop fails cleanly instead of hanging). Only exists under the
    /// `fuel` feature; native `gos run` has no budget and never raises it.
    #[cfg(feature = "fuel")]
    #[error("error[GX0009]: execution limit reached - the program ran too long")]
    FuelExhausted,
}

impl RuntimeError {
    /// Returns the stable `GXNNNN` diagnostic code for this runtime
    /// error. The code is the same in every execution path and is
    /// rendered by `gos explain` for long-form help.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Type(_) => "GX0001",
            Self::UnresolvedName(_) => "GX0002",
            Self::Arity { .. } => "GX0003",
            Self::Arithmetic(_) => "GX0004",
            Self::Panic(_) => "GX0005",
            Self::MatchFailure => "GX0006",
            Self::Unsupported(_) => "GX0007",
            Self::StackOverflow(_) => "GX0008",
            #[cfg(feature = "fuel")]
            Self::FuelExhausted => "GX0009",
        }
    }
}

// ------------------------------------------------------------------
// Global heap side table (Phase P1)
//
// Heap-backed `Value` variants are registered here before being
// encoded as `TAG_HEAP` u64 words.  In later phases this side table
// will be replaced by direct GC-arena storage.

/// One heap-allocated payload stored in the global side table.
#[derive(Clone)]
enum RegistryEntry {
    /// Integer that did not fit in the i56 immediate range.
    Int(i64),
    /// VM string storage. Heap strings stay in their original compact
    /// allocation; inline strings remain a single copied word.
    String(SmolStr),
    /// Tuple aggregate.
    Tuple(Arc<Vec<Value>>),
    /// Array / Vec aggregate.
    Array(Arc<Vec<Value>>),
    /// Flat f64 struct-array storage.
    FloatArray(Arc<FloatArrayInner>),
    /// Flat i64 array storage.
    IntArray(Arc<Vec<i64>>),
    /// Flat f64 vector storage.
    FloatVec(Arc<Vec<f64>>),
    /// Enum variant or tuple-struct constructor payload.
    Variant(Arc<VariantInner>),
    /// Struct-shaped aggregate.
    Struct(Arc<StructInner>),
    /// User-defined callable.
    Closure(Arc<Closure>),
    /// Concurrent channel endpoint.
    Channel(Channel),
}

/// Global registry mapping `u32` handles to [`RegistryEntry`] values.
/// Protected by a [`Mutex`] so it is safe to access from goroutine
/// threads.
///
/// The companion `FREE_SLOTS` free-list keeps slot reuse O(1):
/// `register_heap` pops a known-empty index off the stack instead of
/// linearly scanning every slot for `None` (which was O(n) per
/// registration on long-running programs).
static REGISTRY: Mutex<RegistryStorage> = Mutex::new(RegistryStorage {
    slots: Vec::new(),
    free: Vec::new(),
});

struct RegistryStorage {
    slots: Vec<Option<RegistryEntry>>,
    free: Vec<u32>,
}

/// Stores `entry` in the global side table and returns its stable
/// handle. Reuses a previously-released slot when one is available
/// so the registry stays bounded by the in-flight raw-value count
/// instead of growing monotonically with cumulative `to_raw` calls.
fn register_heap(entry: RegistryEntry) -> u32 {
    let mut reg = REGISTRY.lock();
    if let Some(idx) = reg.free.pop() {
        reg.slots[idx as usize] = Some(entry);
        return idx;
    }
    let id = reg.slots.len();
    reg.slots.push(Some(entry));
    u32::try_from(id).expect("registry handle overflow")
}

/// Removes `handle` from the global side table and returns the
/// stored entry. The slot is recycled onto the free-list so the next
/// `register_heap` can reuse it. Returns `None` when the slot is
/// empty (the object was already taken or never registered).
fn take_heap(handle: u32) -> Option<RegistryEntry> {
    let mut reg = REGISTRY.lock();
    let entry = reg.slots.get_mut(handle as usize).and_then(Option::take)?;
    reg.free.push(handle);
    Some(entry)
}

/// Returns `(slots, occupied)` where `slots` is the size of the
/// registry's slot vector and `occupied` is the count of currently
/// non-empty slots. Test-only - exposed so the value-roundtrip suite
/// can assert that the registry stays bounded under repeated
/// `to_raw`/`from_raw` cycles.
#[doc(hidden)]
#[must_use]
pub fn registry_stats_for_test() -> (usize, usize) {
    let reg = REGISTRY.lock();
    let occupied = reg.slots.iter().filter(|s| s.is_some()).count();
    (reg.slots.len(), occupied)
}

#[cfg(test)]
mod size_assertions {
    use super::Value;

    #[test]
    fn value_size_at_most_16_bytes() {
        // Assertion lock-down for the `Value` enum size. Each
        // non-trivial variant must keep its body behind a
        // single pointer / 8-byte payload (e.g. `Arc<...>`,
        // `SmolStr`). Adding a wider payload (raw `Vec<...>`,
        // raw `String`) will fail this test.
        //
        // The natural fit on 64-bit is 16 bytes (8 disc + 8
        // payload). A future D9 NaN-box pass can collapse this
        // further to 8 by encoding the tag inside the payload -
        // see `gossamer_runtime::GossamerValue` for the layout
        // the LLVM lowerer already speaks. Until then this
        // assertion is the regression guard.
        let n = std::mem::size_of::<Value>();
        assert!(n <= 16, "Value grew to {n} bytes (target ≤16)");
    }

    #[test]
    fn report_value_size_for_visibility() {
        let n = std::mem::size_of::<Value>();
        eprintln!("Value size: {n} bytes");
    }
}

// ---------------------------------------------------------------
// Native enum handles (JIT interop).
// ---------------------------------------------------------------

/// Field classification for one positional payload slot of a native
/// enum variant, used to convert raw payload words into [`Value`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeFieldKind {
    /// 64-bit integer (all integer widths occupy one slot).
    I64,
    /// 64-bit float (stored as raw bits in the slot).
    F64,
    /// Boolean (non-zero slot = true).
    Bool,
    /// Heap string (slot is a tagged c-string body pointer).
    Str,
    /// Unicode scalar value (the compiled layout stores it in the
    /// low 32 bits of the payload slot).
    Char,
    /// Another supported heap enum; index into the program's shape
    /// table.
    Enum(u32),
    /// A `Vec<E>` field where `E` is a supported heap enum: the payload
    /// slot holds a `*mut GosVec` of 8-byte PRIMITIVE slots, each a native
    /// enum pointer of the shape-table index carried here (the AOT layout a
    /// `JsonVal::Arr(Vec<JsonVal>)` variant uses).
    VecEnum(u32),
    /// A `Vec<(String, E)>` field where `E` is a supported heap enum: the
    /// payload slot holds a `*mut GosVec` of 16-byte PRIMITIVE slots laid out
    /// `[*c_char @ +0][native-enum ptr @ +8]` (the AOT layout a
    /// `JsonVal::Obj(Vec<(String, JsonVal)>)` variant uses). The index is the
    /// element enum's shape-table index.
    VecStrEnumTuple(u32),
}

/// One variant of a native enum shape.
#[derive(Debug)]
pub struct NativeVariantShape {
    /// Variant name (interned, pointer-comparable with `VariantIs`).
    pub name: &'static str,
    /// Positional field kinds.
    pub fields: Vec<NativeFieldKind>,
}

/// Layout description of a heap enum whose values may cross the JIT
/// boundary as raw native pointers. Built once per program load from
/// the HIR and shared by native value handles.
#[derive(Debug)]
pub struct NativeEnumShape {
    /// Enum name (diagnostics).
    pub enum_name: &'static str,
    /// Index of this shape in the program's shape table.
    pub index: u32,
    /// True when the discriminant lives in pointer bits 1-2 (at most
    /// 4 variants); false = header byte at `payload - 3`.
    pub tagged: bool,
    /// Variants in declaration order.
    pub variants: Vec<NativeVariantShape>,
}

/// Owning handle for a native (compiled-representation) enum value
/// produced by a JIT-compiled body. Holds one strong reference;
/// dropping the last clone releases it through the runtime.
#[derive(Debug)]
pub struct NativeEnumOwner {
    /// Tagged native pointer (compiled-tier representation).
    pub ptr: usize,
    /// Layout for VM-side structural access.
    pub shape: Arc<NativeEnumShape>,
    /// `true` when this handle exclusively owns the whole tree it roots (a
    /// value returned from a JIT body to the VM). Its drop frees the tree via
    /// the shape walk, tolerating the caller-cleans over-retention the native
    /// code leaves. `false` for a borrowed handle read out of a parent's field
    /// (`native_enum_field`), whose drop balances a single retain and must not
    /// touch the parent-owned subtree.
    pub owned: bool,
}

/// Releases one reference to a native enum value, also reclaiming the `Vec`
/// and string payloads the node-meta release does not reach (a `Vec<Enum>` /
/// `Vec<(String, Enum)>` field is a separate `GosVec` the node's child-layout
/// meta does not list). Native nodes are refcounted - the VM retains a child
/// when it reads a field - so this releases each owned reference exactly once:
/// only when a node is the *last* owner (strong count <= 1) are its `Vec` /
/// string children reclaimed. Enum-pointer children are left to the runtime's
/// own meta cascade, which is iterative and so safe for deep recursive trees
/// (an explicit walk here would overflow the native stack on a depth-20 tree).
fn release_native_enum_tree(ptr: usize, shape: &NativeEnumShape) {
    use gossamer_runtime::c_abi as rt;
    let base = ptr & !7;
    if base == 0 {
        return;
    }
    // SAFETY: `base` is a live runtime-managed node; reading its strong count
    // is valid. Single-threaded per VM, so the count is stable across the
    // check-then-reclaim below.
    let last = unsafe { rt::gos_rt_rc_strong_count(base as *mut u8) } <= 1;
    if last {
        let disc = native_enum_disc(ptr, shape);
        if let Some(variant) = shape.variants.get(disc) {
            for (i, kind) in variant.fields.iter().enumerate() {
                let slot = (base + i * 8) as *mut i64;
                // SAFETY: payload slot inside the node's allocation.
                let word = unsafe { *slot };
                match kind {
                    NativeFieldKind::Str => {
                        if word != 0 {
                            // SAFETY: a live owned cstring body.
                            unsafe { rt::gos_rt_str_free(word as *mut std::os::raw::c_char) };
                            // SAFETY: writing a slot we own.
                            unsafe { *slot = 0 };
                        }
                    }
                    NativeFieldKind::VecEnum(eidx) => {
                        release_native_vec_enum(word, *eidx);
                        // SAFETY: writing a slot we own.
                        unsafe { *slot = 0 };
                    }
                    NativeFieldKind::VecStrEnumTuple(eidx) => {
                        release_native_vec_str_enum(word, *eidx);
                        // SAFETY: writing a slot we own.
                        unsafe { *slot = 0 };
                    }
                    // Enum-pointer children are reclaimed by the runtime's meta
                    // cascade on the release below (deep-safe). Scalars own
                    // nothing.
                    NativeFieldKind::Enum(_)
                    | NativeFieldKind::I64
                    | NativeFieldKind::F64
                    | NativeFieldKind::Bool
                    | NativeFieldKind::Char => {}
                }
            }
        }
    }
    // SAFETY: `base` is a live node; releasing balances one owning reference.
    // When the count reaches zero the runtime frees the node and cascades to
    // its (still-live) enum-pointer children.
    unsafe { rt::gos_rt_rc_release(base as *mut u8) };
}

/// Releases one reference to each element of a native `Vec<Enum>` and frees the
/// buffer. Called only from the last-owner path of [`release_native_enum_tree`].
fn release_native_vec_enum(word: i64, eidx: u32) {
    use gossamer_runtime::c_abi as rt;
    if word == 0 {
        return;
    }
    let v = word as *mut rt::vec::GosVec;
    if let Some(eshape) = native_shape(eidx) {
        // SAFETY: live `GosVec` of 8-byte native-enum pointer slots.
        let len = unsafe { rt::gos_rt_vec_len(v) }.max(0);
        for i in 0..len {
            let elem = unsafe { rt::gos_rt_vec_get_i64(v, i) };
            release_native_enum_tree(elem as usize, &eshape);
        }
    }
    // SAFETY: owns this `PRIMITIVE` vec; its elements were released above.
    unsafe { rt::gos_rt_vec_free(v) };
}

/// Releases one reference to each `(String, Enum)` element of a native
/// `Vec<(String, Enum)>` (freeing key cstrings, releasing enum values) and
/// frees the buffer.
fn release_native_vec_str_enum(word: i64, eidx: u32) {
    use gossamer_runtime::c_abi as rt;
    if word == 0 {
        return;
    }
    let v = word as *mut rt::vec::GosVec;
    let eshape = native_shape(eidx);
    // SAFETY: live `GosVec` of 16-byte `[cstr][enum ptr]` slots.
    let len = unsafe { rt::gos_rt_vec_len(v) }.max(0);
    for i in 0..len {
        let p = unsafe { rt::gos_rt_vec_get_ptr(v, i) };
        if p.is_null() {
            continue;
        }
        // SAFETY: 16-byte slot: cstring word at +0, enum pointer at +8.
        let key_word = unsafe { p.cast::<i64>().read_unaligned() };
        if key_word != 0 {
            // SAFETY: a live owned key cstring.
            unsafe { rt::gos_rt_str_free(key_word as *mut std::os::raw::c_char) };
        }
        if let Some(s) = eshape.as_ref() {
            let val_word = unsafe { p.add(8).cast::<i64>().read_unaligned() };
            release_native_enum_tree(val_word as usize, s);
        }
        // SAFETY: writing slots of a vec we own; the vec's own free then
        // reclaims nothing twice.
        unsafe {
            p.cast::<i64>().write_unaligned(0);
            p.add(8).cast::<i64>().write_unaligned(0);
        }
    }
    // SAFETY: owns this vec; slots nulled above.
    unsafe { rt::gos_rt_vec_free(v) };
}

impl Drop for NativeEnumOwner {
    fn drop(&mut self) {
        let base = self.ptr & !7;
        if self.owned && base != 0 {
            free_exclusive_enum_tree(self.ptr, Arc::clone(&self.shape));
        } else {
            release_native_enum_tree(self.ptr, &self.shape);
        }
    }
}

/// Completely frees an exclusively-owned native enum tree via its VM-side
/// shape. Discovers every reachable node once (a shared node in a DAG is
/// visited a single time), reclaims each node's `String` / `Vec` payloads and
/// clears its enum-pointer slots so a release cannot re-enter the runtime
/// cascade, then drains each node's strong count to zero. Iterative worklist,
/// so a deep tree does not overflow the native stack. Sound only for an
/// exclusively-owned root (guaranteed by the caller's `strong_count <= 1`
/// gate): each node's whole reference count belongs to this tree, so draining
/// it frees exactly once - a shared subtree still held elsewhere is never
/// routed here.
fn free_exclusive_enum_tree(root_ptr: usize, root_shape: Arc<NativeEnumShape>) {
    use gossamer_runtime::c_abi as rt;
    let root_base = root_ptr & !7;
    if root_base == 0 {
        return;
    }
    // (base, full pointer for discriminant reads, shape) for each node.
    let mut seen: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut nodes: Vec<(usize, usize, Arc<NativeEnumShape>)> = Vec::new();
    seen.insert(root_base);
    let mut work = vec![(root_ptr, root_shape)];
    while let Some((ptr, shape)) = work.pop() {
        let base = ptr & !7;
        if base == 0 {
            continue;
        }
        nodes.push((base, ptr, Arc::clone(&shape)));
        let disc = native_enum_disc(ptr, &shape);
        let Some(variant) = shape.variants.get(disc) else {
            continue;
        };
        for (i, kind) in variant.fields.iter().enumerate() {
            if let NativeFieldKind::Enum(eidx) = kind
                && let Some(cshape) = native_shape(*eidx)
            {
                // SAFETY: payload slot inside the node's allocation.
                let cword = unsafe { *((base + i * 8) as *const i64) } as usize;
                let cbase = cword & !7;
                if cbase != 0 && seen.insert(cbase) {
                    work.push((cword, cshape));
                }
            }
        }
    }
    // Reclaim `String` / `Vec` payloads and clear every enum-pointer slot so the
    // strong-count drain below cannot re-enter the runtime cascade.
    for (base, ptr, shape) in &nodes {
        let disc = native_enum_disc(*ptr, shape);
        let Some(variant) = shape.variants.get(disc) else {
            continue;
        };
        for (i, kind) in variant.fields.iter().enumerate() {
            let slot = (*base + i * 8) as *mut i64;
            // SAFETY: payload slot inside the node's allocation.
            let payload_word = unsafe { *slot };
            match kind {
                NativeFieldKind::Str => {
                    if payload_word != 0 {
                        // SAFETY: a live owned cstring body.
                        unsafe { rt::gos_rt_str_free(payload_word as *mut std::os::raw::c_char) };
                        // SAFETY: writing a slot we own.
                        unsafe { *slot = 0 };
                    }
                }
                NativeFieldKind::VecEnum(eidx) => {
                    release_native_vec_enum(payload_word, *eidx);
                    // SAFETY: writing a slot we own.
                    unsafe { *slot = 0 };
                }
                NativeFieldKind::VecStrEnumTuple(eidx) => {
                    release_native_vec_str_enum(payload_word, *eidx);
                    // SAFETY: writing a slot we own.
                    unsafe { *slot = 0 };
                }
                NativeFieldKind::Enum(_) => {
                    // SAFETY: writing a slot we own; the child is freed below.
                    unsafe { *slot = 0 };
                }
                NativeFieldKind::I64
                | NativeFieldKind::F64
                | NativeFieldKind::Bool
                | NativeFieldKind::Char => {}
            }
        }
    }
    // Drain each node's strong count to zero and free it. Slots are cleared, so
    // no release re-enters the cascade; the no-buffer release reclaims each node
    // immediately instead of leaving an over-retained interior node parked as a
    // cycle-collection candidate.
    for (base, _, _) in &nodes {
        // SAFETY: `base` is a live runtime-managed node reached from the root.
        let rc = unsafe { rt::gos_rt_rc_strong_count(*base as *mut u8) };
        for _ in 0..rc.max(0) {
            // Re-check before each release so a node already driven to zero by
            // an earlier iteration (a shared node reached along two paths whose
            // count this teardown already drained) is never released past zero
            // into freed memory.
            if unsafe { rt::gos_rt_rc_strong_count(*base as *mut u8) } <= 0 {
                break;
            }
            // SAFETY: exclusively owned and count still positive.
            unsafe { rt::gos_rt_rc_release_no_buffer(*base as *mut u8) };
        }
    }
}

/// Process-global weak compatibility table of registered native enum shapes.
/// A loaded VM owns the strong descriptor handles; this table exists only for
/// legacy shape-index operands and JIT trampoline metadata while the VM
/// migrates to fully program-owned shape sessions. Dead programs leave no
/// descriptor allocation alive through the compatibility path.
static NATIVE_SHAPES: std::sync::LazyLock<
    parking_lot::RwLock<rustc_hash::FxHashMap<u32, std::sync::Weak<NativeEnumShape>>>,
> = std::sync::LazyLock::new(|| parking_lot::RwLock::new(rustc_hash::FxHashMap::default()));
static NEXT_NATIVE_SHAPE_INDEX: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Maps a variant name to the single native enum shape that declares it, so
/// the bytecode enum constructor ([`Value::variant`]) can build the native
/// representation directly instead of a boxed `Variant` that later marshals
/// across the JIT boundary (Step 8: one representation, no marshalling copy).
/// A name declared by more than one shape maps to `None` (ambiguous - the
/// constructor cannot pick a shape from the variant name alone and falls back
/// to the boxed form).
type VariantShapeMap =
    rustc_hash::FxHashMap<&'static str, Option<std::sync::Weak<NativeEnumShape>>>;

static VARIANT_NAME_TO_SHAPE: std::sync::LazyLock<parking_lot::RwLock<VariantShapeMap>> =
    std::sync::LazyLock::new(|| parking_lot::RwLock::new(VariantShapeMap::default()));

/// Bumped after every shape registration. A later program can make a variant
/// name that was unique become ambiguous, so thread-local positive caches must
/// be discarded across registrations.
static NATIVE_SHAPE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// Positive-only constructor-shape cache. Negative results cannot be
    /// cached because a later program load may register the name; once a shape
    /// is found, however, the append-only registry makes it immutable.
    static NATIVE_SHAPE_CACHE: std::cell::RefCell<(
        u64,
        rustc_hash::FxHashMap<TypeTag, Arc<NativeEnumShape>>
    )> = std::cell::RefCell::new((0, rustc_hash::FxHashMap::default()));
}

/// The native enum shape that uniquely declares a variant named `name`, or
/// `None` if no native shape declares it or more than one does.
#[must_use]
#[allow(dead_code)]
pub(crate) fn native_shape_for_variant(tag: TypeTag, name: &str) -> Option<Arc<NativeEnumShape>> {
    use std::sync::atomic::Ordering;
    let generation = NATIVE_SHAPE_GENERATION.load(Ordering::Acquire);
    if let Some(shape) = NATIVE_SHAPE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.0 != generation {
            cache.0 = generation;
            cache.1.clear();
        }
        cache.1.get(&tag).cloned()
    }) {
        return Some(shape);
    }
    let shape = VARIANT_NAME_TO_SHAPE
        .read()
        .get(name)
        .cloned()
        .flatten()?
        .upgrade()?;
    NATIVE_SHAPE_CACHE.with(|cache| {
        cache.borrow_mut().1.insert(tag, Arc::clone(&shape));
    });
    Some(shape)
}

/// Atomically reserves a contiguous block of shape indices and
/// registers the shapes that `build` produces under them.
///
/// The reserve (reading the base index) and the inserts happen under a
/// single write lock, so concurrent program loads can never interleave
/// a reserve with another load's register - the indices a shape is
/// built against are guaranteed to be the indices it lands at.
///
/// `build` is handed the base index the block will occupy and must
/// return the shapes in index order, each carrying `index == base +
/// offset`. Returns `build`'s second value (typically the `DefId ->
/// index` map the shapes were built against).
pub fn register_native_shapes<R>(build: impl FnOnce(u32) -> (Vec<Arc<NativeEnumShape>>, R)) -> R {
    let mut t = NATIVE_SHAPES.write();
    t.retain(|_, weak| weak.strong_count() != 0);
    // The builder needs its base before it can reveal the batch length. Reserve
    // a fixed, intentionally generous block; shape batches are tiny and the
    // opaque compatibility ids need only be unique, not dense.
    let base = NEXT_NATIVE_SHAPE_INDEX.fetch_add(1024, std::sync::atomic::Ordering::AcqRel);
    let (shapes, result) = build(base);
    let mut names = VARIANT_NAME_TO_SHAPE.write();
    for (offset, shape) in shapes.into_iter().enumerate() {
        debug_assert_eq!(
            shape.index,
            base + u32::try_from(offset).unwrap_or(0),
            "shape table index drift",
        );
        // Step 8 builds native for every registered shape, including
        // `Vec`-bearing enums (e.g. a JSON-like `List(Vec<Node>)`). A marshalled
        // `Vec` element is a fresh, exclusively-owned native copy - never an
        // alias of a live VM node - so construction and teardown stay uniform
        // (drain-to-zero) with no mixed-ownership double free.
        for variant in &shape.variants {
            names
                .entry(variant.name)
                .and_modify(
                    |entry| match entry.as_ref().and_then(std::sync::Weak::upgrade) {
                        // The old program has gone away. Reuse the compatibility
                        // entry instead of leaving a stale ambiguity behind.
                        None => *entry = Some(Arc::downgrade(&shape)),
                        Some(existing) if Arc::ptr_eq(&existing, &shape) => {}
                        // A live second shape declaring this variant name makes
                        // the constructor ambiguous; fall back to `Variant`.
                        Some(_) => *entry = None,
                    },
                )
                .or_insert_with(|| Some(Arc::downgrade(&shape)));
        }
        t.insert(shape.index, Arc::downgrade(&shape));
    }
    NATIVE_SHAPE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
    result
}

/// Looks up a registered shape by global index.
#[must_use]
pub fn native_shape(idx: u32) -> Option<Arc<NativeEnumShape>> {
    NATIVE_SHAPES.read().get(&idx)?.upgrade()
}

/// The discriminant of a native enum pointer under `shape`.
#[must_use]
pub fn native_enum_disc(ptr: usize, shape: &NativeEnumShape) -> usize {
    if shape.tagged {
        (ptr >> 1) & 3
    } else {
        // SAFETY: header-repr values carry the disc byte at payload-3
        // by the compiled-tier layout contract.
        unsafe { *((ptr - 3) as *const u8) as usize }
    }
}

/// Reads positional field `idx` of a native enum value and converts
/// it to a [`Value`] per the variant's field kind. Returns
/// `Value::Unit` for out-of-range access (mirrors `VariantField`).
#[must_use]
pub fn native_enum_field(owner: &NativeEnumOwner, idx: usize) -> Value {
    let disc = native_enum_disc(owner.ptr, &owner.shape);
    let Some(variant) = owner.shape.variants.get(disc) else {
        return Value::Unit;
    };
    let Some(kind) = variant.fields.get(idx) else {
        return Value::Unit;
    };
    let base = owner.ptr & !7;
    if base == 0 {
        return Value::Unit;
    }
    // SAFETY: payload slot reads inside an allocation sized for the
    // variant's field count (compiled-tier layout contract).
    let word = unsafe { *((base + idx * 8) as *const i64) };
    match kind {
        NativeFieldKind::I64 => Value::Int(word),
        NativeFieldKind::F64 => Value::Float(f64::from_bits(word as u64)),
        NativeFieldKind::Bool => Value::Bool(word != 0),
        NativeFieldKind::Char => Value::Char(char::from_u32(word as u32).unwrap_or('\u{0}')),
        NativeFieldKind::Str => {
            if word == 0 {
                Value::String(SmolStr::default())
            } else {
                // SAFETY: string payload slots hold NUL-terminated
                // tagged c-string bodies.
                let c = unsafe { std::ffi::CStr::from_ptr(word as *const std::os::raw::c_char) };
                Value::String(SmolStr::from(c.to_string_lossy().as_ref()))
            }
        }
        NativeFieldKind::Enum(sidx) => {
            let Some(shape) = native_shape(*sidx) else {
                return Value::Unit;
            };
            // The VM takes its own reference to the child.
            // SAFETY: retain of a live runtime-managed value (or a
            // tagged-null, which the entry treats as null).
            unsafe {
                gossamer_runtime::c_abi::gos_rt_rc_retain(word as usize as *mut u8);
            }
            Value::NativeEnum(Arc::new(NativeEnumOwner {
                ptr: word as usize,
                shape: Arc::clone(&shape),
                owned: false,
            }))
        }
        NativeFieldKind::VecEnum(eidx) => native_vec_enum_to_array(word, *eidx),
        NativeFieldKind::VecStrEnumTuple(eidx) => native_vec_str_enum_to_array(word, *eidx),
    }
}

/// Moves an enum-pointer field out of a uniquely owned native node without a
/// retain/release round trip.  Returns `None` when the node is shared or the
/// field is not itself an enum, in which case the VM must use
/// [`native_enum_field`] and preserve ordinary clone semantics.
///
/// Clearing the payload slot transfers the parent's one child reference to
/// the returned handle: the parent's metadata-driven drop sees null and does
/// not release it a second time.  This is the native counterpart of draining a
/// slot from `VariantInner::fields` in `VariantFieldConsume`.
#[must_use]
pub fn native_enum_field_consume(owner: &mut NativeEnumOwner, idx: usize) -> Option<Value> {
    let base = owner.ptr & !7;
    if base == 0 {
        return None;
    }
    // Arc uniqueness only proves the Rust handle is unique.  Native nodes have
    // their own RC domain, so require its count to be one before mutating a
    // payload slot that another native alias could observe.
    let unique = unsafe { gossamer_runtime::c_abi::gos_rt_rc_strong_count(base as *mut u8) == 1 };
    if !unique {
        return None;
    }
    let disc = native_enum_disc(owner.ptr, &owner.shape);
    let kind = owner.shape.variants.get(disc)?.fields.get(idx)?;
    let NativeFieldKind::Enum(shape_idx) = kind else {
        return None;
    };
    let shape = native_shape(*shape_idx)?;
    let slot = (base + idx * 8) as *mut i64;
    // SAFETY: `slot` is a field of the uniquely owned allocation.  Reading and
    // zeroing it atomically transfers the parent's reference to the new owner.
    let word = unsafe { std::ptr::replace(slot, 0) };
    Some(Value::NativeEnum(Arc::new(NativeEnumOwner {
        ptr: word as usize,
        shape: Arc::clone(&shape),
        owned: false,
    })))
}

/// Reads a native `Vec<E>` payload word (a `*mut GosVec` of 8-byte native
/// enum pointer slots) into a `Value::Array` of `Value::NativeEnum` children,
/// each retained so the array owns its own reference. An empty / null vec
/// yields an empty array.
#[must_use]
pub(crate) fn native_vec_enum_to_array(word: i64, eidx: u32) -> Value {
    if word == 0 {
        return Value::Array(Arc::new(Vec::new()));
    }
    let Some(eshape) = native_shape(eidx) else {
        return Value::Array(Arc::new(Vec::new()));
    };
    let v = word as *const gossamer_runtime::c_abi::vec::GosVec;
    // SAFETY: `v` is a live `GosVec` of 8-byte pointer slots (the AOT
    // `Vec<Enum>` layout); `len`/`get_i64` read initialised in-bounds slots.
    let len = unsafe { gossamer_runtime::c_abi::gos_rt_vec_len(v) }.max(0);
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let elem = unsafe { gossamer_runtime::c_abi::gos_rt_vec_get_i64(v, i) };
        if (elem as usize) & !7 == 0 {
            out.push(Value::Unit);
            continue;
        }
        // SAFETY: co-own the child (the parent vec keeps its own share); the
        // returned `NativeEnumOwner` releases it on drop.
        unsafe { gossamer_runtime::c_abi::gos_rt_rc_retain(elem as usize as *mut u8) };
        out.push(Value::NativeEnum(Arc::new(NativeEnumOwner {
            ptr: elem as usize,
            shape: Arc::clone(&eshape),
            owned: false,
        })));
    }
    Value::Array(Arc::new(out))
}

/// Reads a native `Vec<(String, E)>` payload word (a `*mut GosVec` of 16-byte
/// `[*c_char][native-enum ptr]` slots) into a `Value::Array` of 2-tuples
/// `(Value::String, Value::NativeEnum)`. Strings are copied; enum children are
/// retained so the array owns its own reference.
#[must_use]
pub(crate) fn native_vec_str_enum_to_array(word: i64, eidx: u32) -> Value {
    if word == 0 {
        return Value::Array(Arc::new(Vec::new()));
    }
    let Some(eshape) = native_shape(eidx) else {
        return Value::Array(Arc::new(Vec::new()));
    };
    let v = word as *const gossamer_runtime::c_abi::vec::GosVec;
    // SAFETY: `v` is a live `GosVec` of 16-byte slots (the AOT
    // `Vec<(String, Enum)>` layout); `len`/`get_ptr` read in-bounds slots.
    let len = unsafe { gossamer_runtime::c_abi::gos_rt_vec_len(v) }.max(0);
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let p = unsafe { gossamer_runtime::c_abi::gos_rt_vec_get_ptr(v, i) };
        if p.is_null() {
            out.push(Value::Tuple(Arc::from(vec![
                Value::String(SmolStr::default()),
                Value::Unit,
            ])));
            continue;
        }
        // SAFETY: each 16-byte slot holds a cstring word at +0 and a native
        // enum pointer word at +8.
        let key_word = unsafe { p.cast::<i64>().read_unaligned() };
        let val_word = unsafe { p.add(8).cast::<i64>().read_unaligned() };
        let key = if key_word == 0 {
            Value::String(SmolStr::default())
        } else {
            // SAFETY: cstring words point at NUL-terminated tagged bodies.
            let c = unsafe { std::ffi::CStr::from_ptr(key_word as *const std::os::raw::c_char) };
            Value::String(SmolStr::from(c.to_string_lossy().as_ref()))
        };
        let val = if (val_word as usize) & !7 == 0 {
            Value::Unit
        } else {
            // SAFETY: co-own the child enum; the tuple's `NativeEnumOwner`
            // releases it on drop.
            unsafe { gossamer_runtime::c_abi::gos_rt_rc_retain(val_word as usize as *mut u8) };
            Value::NativeEnum(Arc::new(NativeEnumOwner {
                ptr: val_word as usize,
                shape: Arc::clone(&eshape),
                owned: false,
            }))
        };
        out.push(Value::Tuple(Arc::from(vec![key, val])));
    }
    Value::Array(Arc::new(out))
}

/// Deep-converts a native enum value into the boxed
/// [`Value::Variant`] representation - the safety valve for paths
/// that need structural `Value`s (FFI bridging, fallback equality).
#[must_use]
pub fn native_enum_to_variant(owner: &NativeEnumOwner) -> Value {
    let disc = native_enum_disc(owner.ptr, &owner.shape);
    let Some(variant) = owner.shape.variants.get(disc) else {
        return Value::Unit;
    };
    let fields: Vec<Value> = (0..variant.fields.len())
        .map(|i| deep_native_value(native_enum_field(owner, i)))
        .collect();
    Value::variant_boxed(variant.name, fields)
}

/// Deep-converts any `Value::NativeEnum` reachable through a value (directly,
/// or inside an `Array` / `Tuple` produced by a `Vec<Enum>` / `Vec<(String,
/// Enum)>` field) into the boxed `Value::Variant` representation.
fn deep_native_value(v: Value) -> Value {
    match v {
        Value::NativeEnum(child) => native_enum_to_variant(&child),
        Value::Array(arc) => Value::Array(Arc::new(
            arc.iter().cloned().map(deep_native_value).collect(),
        )),
        Value::Tuple(arc) => Value::Tuple(Arc::from(
            arc.iter()
                .cloned()
                .map(deep_native_value)
                .collect::<Vec<_>>(),
        )),
        other => other,
    }
}

// ---------------------------------------------------------------
// Native struct shapes (JIT interop).
// ---------------------------------------------------------------

/// Layout description of a user struct whose values may cross the JIT
/// boundary. Built once per program load from the HIR. Unlike a heap enum, a struct in the compiled tier is a
/// flat field-slot block with NO RC header: field `i` lives at byte
/// offset `i * 8` and `&self` / `&mut self` point at field 0.
///
/// Only all-scalar structs (every field `I64` / `F64` / `Bool` / `Char`,
/// one 8-byte slot each) are registered: those marshal in O(field count)
/// with no heap children, so the trampoline can build / write back / free
/// the block with no reference-counting and no aliasing surface.
#[derive(Debug)]
pub struct NativeStructShape {
    /// Struct name (interned, matches `StructInner::name`).
    pub struct_name: &'static str,
    /// Index of this shape in the program's struct-shape table.
    pub index: u32,
    /// Field name + scalar kind, in declaration order. Field `i` is at
    /// byte offset `i * 8` in the native flat block.
    pub fields: Vec<(&'static str, NativeFieldKind)>,
}

/// Process-global weak compatibility table of registered native struct shapes.
/// Loaded VMs retain descriptors; dead program descriptors are releasable even
/// though legacy shape-index slots remain append-only for now.
static NATIVE_STRUCT_SHAPES: std::sync::LazyLock<
    parking_lot::RwLock<rustc_hash::FxHashMap<u32, std::sync::Weak<NativeStructShape>>>,
> = std::sync::LazyLock::new(|| parking_lot::RwLock::new(rustc_hash::FxHashMap::default()));
static NEXT_NATIVE_STRUCT_SHAPE_INDEX: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// Atomically reserves a contiguous block of struct-shape indices and
/// registers the shapes `build` produces under them. Same contract as
/// [`register_native_shapes`].
pub fn register_native_struct_shapes<R>(
    build: impl FnOnce(u32) -> (Vec<Arc<NativeStructShape>>, R),
) -> R {
    let mut t = NATIVE_STRUCT_SHAPES.write();
    t.retain(|_, weak| weak.strong_count() != 0);
    let base = NEXT_NATIVE_STRUCT_SHAPE_INDEX.fetch_add(1024, std::sync::atomic::Ordering::AcqRel);
    let (shapes, result) = build(base);
    for (offset, shape) in shapes.into_iter().enumerate() {
        debug_assert_eq!(
            shape.index,
            base + u32::try_from(offset).unwrap_or(0),
            "struct shape table index drift",
        );
        t.insert(shape.index, Arc::downgrade(&shape));
    }
    result
}

/// Looks up a registered struct shape by global index.
#[must_use]
pub fn native_struct_shape(idx: u32) -> Option<Arc<NativeStructShape>> {
    NATIVE_STRUCT_SHAPES.read().get(&idx)?.upgrade()
}

#[cfg(test)]
mod mapkey_size_tests {
    use super::MapKey;
    // 0.18.1: boxing the rare aggregate-key arm keeps the common
    // scalar/string keys at 16 bytes instead of 40.
    #[test]
    fn mapkey_is_two_words() {
        assert_eq!(std::mem::size_of::<MapKey>(), 16);
    }
}

#[cfg(test)]
mod smolstr_tests {
    use super::SmolStr;

    #[test]
    fn repeated_appends_preserve_contents() {
        let mut s = SmolStr::new();
        for _ in 0..10_000 {
            s.push_str("abc");
        }
        assert_eq!(s.len(), 30_000);
        assert!(s.as_str().starts_with("abcabc"));
        assert!(s.as_str().ends_with("abc"));
    }

    #[test]
    fn append_to_shared_heap_string_is_copy_on_write() {
        let mut left = SmolStr::from("abcdefgh");
        let right = left.clone();

        left.push_str("-mutated");

        assert_eq!(right.as_str(), "abcdefgh");
        assert_eq!(left.as_str(), "abcdefgh-mutated");
    }

    #[test]
    fn reserved_empty_string_reuses_vm_builder_capacity() {
        let mut text = SmolStr::with_capacity(64);
        assert_eq!(text.capacity(), 64);
        text.push_str("reserved text");
        assert_eq!(text.as_str(), "reserved text");
        assert_eq!(text.capacity(), 64);
    }
}

#[cfg(test)]
mod repr_tests {
    use std::sync::Arc;

    use smallvec::smallvec;

    use super::{StructInner, Value, VariantInner, intern_type_tag};

    #[test]
    fn repr_quotes_strings_and_chars_recursively() {
        let list = Value::Array(Arc::new(vec![
            Value::String("wow".into()),
            Value::Char('a'),
        ]));
        let variant = Value::Variant(Arc::new(VariantInner {
            name: intern_type_tag("Ok"),
            fields: smallvec![list],
        }));
        let record = Value::Struct(Arc::new(StructInner {
            name: intern_type_tag("Message"),
            fields: Box::new([("text", Value::String("hello".into())), ("value", variant)]),
        }));

        assert_eq!(
            record.repr(),
            "Message { text: \"hello\", value: Ok([\"wow\", 'a']) }"
        );
        assert_eq!(Value::String("wow".into()).to_string(), "wow");
    }
}

#[cfg(test)]
mod thread_confined_cell_tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;

    use super::{ThreadConfinedCell, Value};

    #[test]
    fn thread_confined_cell_rejects_foreign_access_before_deref() {
        let cell = Arc::new(ThreadConfinedCell::new(Value::Int(7)));
        assert!(matches!(&*cell.lock(), Value::Int(7)));

        let foreign = Arc::clone(&cell);
        let rejected = std::thread::spawn(move || {
            catch_unwind(AssertUnwindSafe(|| {
                let _guard = foreign.lock();
            }))
            .is_err()
        })
        .join()
        .expect("foreign thread did not panic");
        assert!(rejected, "foreign access must not reach the UnsafeCell");
    }
}

#[cfg(test)]
mod deep_drop_tests {
    use super::{StructInner, Value, VariantInner, intern_type_tag};
    use smallvec::SmallVec;
    use std::sync::Arc;

    // A chain far deeper than the native stack could hold recursive drop
    // frames. The structures are built iteratively (construction was never the
    // problem) and dropped at the end of each test; before the iterative
    // teardown these drops overflowed the default test-thread stack.
    const DEPTH: usize = 1_000_000;

    #[test]
    fn deep_variant_chain_drops_without_stack_overflow() {
        let mut v = Value::Variant(Arc::new(VariantInner {
            name: intern_type_tag("Nil"),
            fields: SmallVec::new(),
        }));
        for _ in 0..DEPTH {
            let mut fields: SmallVec<[Value; 2]> = SmallVec::new();
            fields.push(Value::Int(0));
            fields.push(v);
            v = Value::Variant(Arc::new(VariantInner {
                name: intern_type_tag("Cons"),
                fields,
            }));
        }
        drop(v);
    }

    #[test]
    fn deep_struct_chain_drops_without_stack_overflow() {
        let mut v = Value::Unit;
        for _ in 0..DEPTH {
            let fields: Box<[(&'static str, Value)]> = Box::new([("next", v)]);
            v = Value::Struct(Arc::new(StructInner {
                name: intern_type_tag("Link"),
                fields,
            }));
        }
        drop(v);
    }

    #[test]
    fn deep_array_nested_in_variant_drops_iteratively() {
        // A Variant whose child is an Array whose child is a Variant ...: the
        // mixed chain must flatten through the worklist once teardown enters
        // via the Variant payload.
        let mut v = Value::Unit;
        for _ in 0..DEPTH {
            let arr = Value::Array(Arc::new(vec![v]));
            let mut fields: SmallVec<[Value; 2]> = SmallVec::new();
            fields.push(arr);
            v = Value::Variant(Arc::new(VariantInner {
                name: intern_type_tag("Wrap"),
                fields,
            }));
        }
        drop(v);
    }
}

#[cfg(test)]
mod native_consume_tests {
    use std::sync::Arc;

    use super::{
        NativeEnumOwner, NativeEnumShape, NativeFieldKind, NativeStructShape, NativeVariantShape,
        Value, intern_type_name, intern_type_tag, native_enum_field_consume, native_shape,
        native_shape_for_variant, native_struct_shape, register_native_shapes,
        register_native_struct_shapes,
    };

    #[test]
    fn dead_type_tag_storage_is_not_retained_by_compatibility_lookup() {
        let name = "SessionOwnedTypeTagRegression";
        let tag = intern_type_tag(name);
        let weak = Arc::downgrade(&tag.0);
        drop(tag);
        assert!(weak.upgrade().is_none());
        let _ = intern_type_tag("SessionOwnedTypeTagSweep");
        assert!(
            !super::TYPE_TAGS.lock().contains_key(name),
            "compatibility lookup retained a dead type tag"
        );
    }

    #[test]
    fn compatibility_shape_indices_do_not_keep_descriptors_alive() {
        let (enum_index, enum_weak) = register_native_shapes(|base| {
            let shape = Arc::new(NativeEnumShape {
                enum_name: intern_type_name("WeakCompatibilityEnum"),
                index: base,
                tagged: true,
                variants: Vec::new(),
            });
            let weak = Arc::downgrade(&shape);
            (vec![shape], (base, weak))
        });
        assert!(enum_weak.upgrade().is_none());
        assert!(native_shape(enum_index).is_none());

        let (struct_index, struct_weak) = register_native_struct_shapes(|base| {
            let shape = Arc::new(NativeStructShape {
                struct_name: intern_type_name("WeakCompatibilityStruct"),
                index: base,
                fields: Vec::new(),
            });
            let weak = Arc::downgrade(&shape);
            (vec![shape], (base, weak))
        });
        assert!(struct_weak.upgrade().is_none());
        assert!(native_struct_shape(struct_index).is_none());
    }

    #[test]
    fn native_shape_cache_invalidates_when_name_becomes_ambiguous() {
        const VARIANT: &str = "ShapeCacheAmbiguousVariant";
        let first = register_native_shapes(|base| {
            let shape = Arc::new(NativeEnumShape {
                enum_name: intern_type_name("ShapeCacheFirst"),
                index: base,
                tagged: true,
                variants: vec![NativeVariantShape {
                    name: intern_type_name(VARIANT),
                    fields: Vec::new(),
                }],
            });
            (vec![Arc::clone(&shape)], shape)
        });
        let tag = intern_type_tag(VARIANT);
        assert!(Arc::ptr_eq(
            &native_shape_for_variant(tag.clone(), VARIANT).expect("initial unique shape"),
            &first
        ));

        register_native_shapes(|base| {
            let shape = Arc::new(NativeEnumShape {
                enum_name: intern_type_name("ShapeCacheSecond"),
                index: base,
                tagged: true,
                variants: vec![NativeVariantShape {
                    name: intern_type_name(VARIANT),
                    fields: Vec::new(),
                }],
            });
            (vec![shape], ())
        });
        assert!(
            native_shape_for_variant(tag, VARIANT).is_none(),
            "registration generation must invalidate the cached unique shape"
        );
    }

    #[test]
    fn consuming_unique_native_child_transfers_without_retain() {
        let (child_shape, parent_shape) = register_native_shapes(|base| {
            let child = Arc::new(NativeEnumShape {
                enum_name: intern_type_name("ConsumeChild"),
                index: base,
                tagged: false,
                variants: vec![NativeVariantShape {
                    name: intern_type_name("ConsumeLeaf"),
                    fields: vec![NativeFieldKind::I64],
                }],
            });
            let parent = Arc::new(NativeEnumShape {
                enum_name: intern_type_name("ConsumeParent"),
                index: base + 1,
                tagged: false,
                variants: vec![NativeVariantShape {
                    name: intern_type_name("ConsumeNode"),
                    fields: vec![NativeFieldKind::Enum(base)],
                }],
            });
            (
                vec![Arc::clone(&child), Arc::clone(&parent)],
                (child, parent),
            )
        });

        // Null metadata is sufficient here: the test explicitly transfers the
        // only child slot before either allocation drops.
        let child = unsafe { gossamer_runtime::c_abi::gos_rt_rc_alloc(8, std::ptr::null()) };
        let parent = unsafe { gossamer_runtime::c_abi::gos_rt_rc_alloc(8, std::ptr::null()) };
        assert!(!child.is_null() && !parent.is_null());
        unsafe {
            *((child as usize - 3) as *mut u8) = 0;
            child.cast::<i64>().write_unaligned(7);
            *((parent as usize - 3) as *mut u8) = 0;
            parent.cast::<i64>().write_unaligned(child as i64);
        }
        let before = unsafe { gossamer_runtime::c_abi::gos_rt_rc_strong_count(child) };
        let mut owner = NativeEnumOwner {
            ptr: parent as usize,
            shape: Arc::clone(&parent_shape),
            owned: false,
        };
        let moved = native_enum_field_consume(&mut owner, 0).expect("unique child moves");
        assert_eq!(
            unsafe { parent.cast::<i64>().read_unaligned() },
            0,
            "parent slot cleared"
        );
        let Value::NativeEnum(child_owner) = moved else {
            panic!("consume returned non-enum")
        };
        assert_eq!(child_owner.ptr, child as usize);
        assert!(Arc::ptr_eq(&child_owner.shape, &child_shape));
        assert_eq!(
            unsafe { gossamer_runtime::c_abi::gos_rt_rc_strong_count(child) },
            before,
            "moving a child must not retain it"
        );
        drop(child_owner);
        drop(owner);
    }
}
