//! Runtime value representation for the tree-walking interpreter.
//! Every shared aggregate is backed by [`Arc`] rather than
//! [`std::rc::Rc`] so a [`Value`] can cross thread boundaries — a
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

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

use gossamer_ast::Ident;
use gossamer_hir::{HirExpr, HirParam};
use gossamer_runtime::{
    GossamerValue, SINGLETON_FALSE, SINGLETON_TRUE, SINGLETON_UNIT, TAG_FLOAT, TAG_HEAP,
    TAG_IMMEDIATE, TAG_SINGLETON, fits_i56, from_f64, from_heap_handle, from_i64, from_singleton,
    tag_of, to_f64, to_heap_handle, to_i64, to_singleton,
};

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
/// `size_of::<Value>` to 48 bytes — every register-file slot
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
    /// Tuple aggregate.
    Tuple(Arc<Vec<Value>>),
    /// Array / Vec aggregate (interpreter treats both as `Vec`).
    Array(Arc<Vec<Value>>),
    /// Flat f64 storage for an array of a struct whose fields
    /// are all `f64`.
    FloatArray(Arc<FloatArrayInner>),
    /// Flat `i64` storage for a primitive integer array literal.
    IntArray(Arc<Vec<i64>>),
    /// Enum variant or tuple-struct constructor payload.
    Variant(Arc<VariantInner>),
    /// Struct-shaped aggregate.
    Struct(Arc<StructInner>),
    /// User-defined callable.
    Closure(Arc<Closure>),
    /// Built-in intrinsic callable.
    Builtin(Arc<BuiltinInner>),
    /// Built-in callable that can re-enter the interpreter through a
    /// [`NativeDispatch`] handle.
    Native(Arc<NativeInner>),
    /// Concurrent channel endpoint.
    Channel(Channel),
    /// Hash-map aggregate. Wrapped in `parking_lot::Mutex` for
    /// interior mutability so `m.insert(k, v)` is O(log N) instead
    /// of the O(N) clone the copy-on-write `Arc<BTreeMap>` shape
    /// would imply — k-nucleotide does ~250K inserts per length
    /// and would otherwise be quadratic in buffer size. The mutex
    /// (over a plain `RefCell`) keeps `Value: Send + Sync` so
    /// goroutines can pass maps through channels.
    Map(Arc<parking_lot::Mutex<std::collections::BTreeMap<MapKey, Value>>>),
    /// Poisoned / uninitialised sentinel.
    Void,
}

/// Ordered key type for [`Value::Map`]. Wraps a [`Value`] and
/// gives it a `(tag, content)` total order so any value the user
/// can hash (int / bool / char / string) sorts deterministically.
/// Aggregate values (arrays, structs, closures) collapse to a
/// single bucket — they're rejected at insert time, not here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
    /// String key.
    Str(String),
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
            Value::String(s) => Self::Str(s.as_str().to_string()),
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
            Self::Str(s) => Value::String(SmolStr::from(s.clone())),
            Self::NonHashable => Value::Unit,
        }
    }
}

/// Boxed payload of [`Value::FloatArray`]. Pre-B1 this lived
/// inline in the enum (~48 bytes); behind `Arc` it costs 8 in
/// the variant.
#[derive(Debug, Clone)]
pub struct FloatArrayInner {
    /// Element-struct name (e.g. `"Body"`).
    pub name: String,
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
    /// Variant name.
    pub name: String,
    /// Positional fields.
    pub fields: Arc<Vec<Value>>,
}

/// Boxed payload of [`Value::Struct`].
#[derive(Debug, Clone)]
pub struct StructInner {
    /// Struct name.
    pub name: String,
    /// Field name/value pairs in declaration order.
    pub fields: Arc<Vec<(Ident, Value)>>,
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
///   produced by `Arc::into_raw` for an `Arc<String>`. On
///   `x86_64` / aarch64, user-space pointers fit in 48 bits, so
///   masking the high bit is lossless.
///
/// **Why this matters.** Without SSO, every `Value::String(SmolStr::from("Ok"))`
/// allocates a `String` on the heap *and* an `Arc` header (~32
/// bytes total). Variant names like `"Ok"` / `"Err"` / `"Some"`
/// / `"None"`, single-char field names, and most stack tags fit
/// in 7 bytes — so a steady-state hot loop now does zero heap
/// allocation for those values.
///
/// **Safety.** All pointer arithmetic is contained in this type.
/// `Drop` and `Clone` decrement / increment the underlying Arc
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

impl SmolStr {
    /// Empty string (inline, len 0).
    #[must_use]
    pub const fn new() -> Self {
        Self { raw: 0 }
    }

    /// Constructs a [`SmolStr`] from a borrowed `&str`. Strings
    /// up to 7 bytes are stored inline; longer strings allocate
    /// a fresh `Arc<String>`.
    ///
    /// Intentionally not the [`std::str::FromStr`] trait method —
    /// `FromStr` returns `Result` to model fallible parsing and
    /// this conversion is infallible. Implementing the trait
    /// would force callers to `.unwrap()` an `Ok`-only path.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        if s.len() <= SMOL_INLINE_MAX {
            Self::new_inline(s.as_bytes())
        } else {
            Self::new_heap(Arc::new(s.to_string()))
        }
    }

    /// Constructs a [`SmolStr`] from an owned [`String`]. Avoids
    /// re-allocating when the string is heap-bound; for inline-
    /// fitting strings the owned string is dropped after copy.
    #[must_use]
    pub fn from_string(s: String) -> Self {
        if s.len() <= SMOL_INLINE_MAX {
            Self::new_inline(s.as_bytes())
        } else {
            Self::new_heap(Arc::new(s))
        }
    }

    /// Constructs from an existing `Arc<String>` — used by hot
    /// paths that have already paid the allocation. Always
    /// stores as heap (no inline-promotion to keep the
    /// constructor branch-free).
    #[must_use]
    pub fn from_arc(arc: Arc<String>) -> Self {
        Self::new_heap(arc)
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

    fn new_heap(arc: Arc<String>) -> Self {
        // SAFETY: `Arc::into_raw` returns a pointer obtained
        // from the global allocator; user-space pointers on
        // supported targets fit in the low 63 bits, so OR-ing
        // the tag bit is information-preserving.
        let ptr = Arc::into_raw(arc) as usize as u64;
        debug_assert!(
            ptr & SMOL_HEAP_TAG == 0,
            "Arc<String> pointer must have high bit clear"
        );
        Self {
            raw: ptr | SMOL_HEAP_TAG,
        }
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
            // Heap: dereference the Arc<String>.
            // SAFETY: only constructed via `Arc::into_raw`;
            // the strong count is at least 1 for the lifetime
            // of `self` (we hold one reference). We never give
            // out the raw pointer or call `from_raw` outside
            // `Drop` / `Clone`.
            let ptr = (self.raw & SMOL_PTR_MASK) as *const String;
            unsafe { (*ptr).as_str() }
        }
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
            let ptr = (self.raw & SMOL_PTR_MASK) as *const String;
            unsafe {
                Arc::increment_strong_count(ptr);
            }
        }
        Self { raw: self.raw }
    }
}

impl Drop for SmolStr {
    fn drop(&mut self) {
        if self.raw & SMOL_HEAP_TAG != 0 {
            // SAFETY: we own one strong reference produced by
            // `Arc::into_raw`. Recovering and dropping decrements
            // the count exactly once.
            let ptr = (self.raw & SMOL_PTR_MASK) as *const String;
            unsafe {
                drop(Arc::from_raw(ptr));
            }
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

// SAFETY: heap storage holds an `Arc<String>` (Send + Sync).
// Inline storage is plain bytes copyable across threads.
unsafe impl Send for SmolStr {}
unsafe impl Sync for SmolStr {}

impl Value {
    /// Constructs a [`Value::Variant`] from owned name + shared
    /// field list. Hides the `Arc::new(VariantInner { … })`
    /// boilerplate at every constructor site.
    #[must_use]
    pub fn variant(name: String, fields: Arc<Vec<Value>>) -> Self {
        Self::Variant(Arc::new(VariantInner { name, fields }))
    }
    /// Constructs a [`Value::Struct`].
    #[must_use]
    pub fn struct_(name: String, fields: Arc<Vec<(Ident, Value)>>) -> Self {
        Self::Struct(Arc::new(StructInner { name, fields }))
    }
    /// Constructs a [`Value::FloatArray`].
    #[must_use]
    pub fn float_array(
        name: String,
        stride: u16,
        field_names: Arc<Vec<String>>,
        data: Arc<Vec<f64>>,
    ) -> Self {
        Self::FloatArray(Arc::new(FloatArrayInner {
            name,
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

/// Shared buffered channel backing a `(Sender<T>, Receiver<T>)` pair.
///
/// Buffered semantics: `send` pushes, `recv` pops. `recv` returns
/// `Some(value)` when a value is available and `None` when the
/// buffer is empty. `Value::Channel` is `Send + Sync` so it can
/// travel across goroutine boundaries once the scheduler backing
/// `go expr` ships.
#[derive(Clone)]
pub struct Channel {
    inner: Arc<Mutex<VecDeque<Value>>>,
}

impl Channel {
    /// Constructs a new empty channel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Pushes `value` onto the channel.
    pub fn send(&self, value: Value) {
        self.inner.lock().push_back(value);
    }

    /// Non-blocking receive. Returns `None` when the channel is
    /// empty; once a blocking runtime exists it will park the caller
    /// instead.
    #[must_use]
    pub fn try_recv(&self) -> Option<Value> {
        self.inner.lock().pop_front()
    }

    /// Returns `true` when the channel currently has at least one
    /// pending value. Used by `select` to pick a ready arm.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.inner.lock().is_empty()
    }
}

impl Default for Channel {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "<channel len={}>", self.inner.lock().len())
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
}

/// Function pointer for [`Value::Native`] builtins.
pub(crate) type NativeCall = fn(&mut dyn NativeDispatch, &[Value]) -> RuntimeResult<Value>;

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
    /// code that expects the generic shape — ABI crossings,
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
            let mut fields: Vec<(Ident, Value)> = Vec::with_capacity(inner.field_names.len());
            for (j, fname) in inner.field_names.iter().enumerate() {
                fields.push((
                    Ident::new(fname.as_str()),
                    Value::Float(inner.data[base + j]),
                ));
            }
            out.push(Value::struct_(inner.name.clone(), Arc::new(fields)));
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
                // Materialise into an `Arc<String>` for the
                // raw-layout side table. Inline `SmolStr` content
                // is copied; heap content is reference-bumped via
                // a re-Arc.
                let id = register_heap(RegistryEntry::String(Arc::new(s.as_str().to_string())));
                from_heap_handle(id)
            }
            Self::Tuple(t) => {
                let id = register_heap(RegistryEntry::Tuple(Arc::clone(t)));
                from_heap_handle(id)
            }
            Self::Array(a) => {
                let id = register_heap(RegistryEntry::Array(Arc::clone(a)));
                from_heap_handle(id)
            }
            Self::FloatArray(_) => {
                // Rehydrate into a `Value::Array<Value::Struct>`
                // before handing across the ABI boundary — the raw
                // representation has no slot for flat f64 aggregates.
                let arr = self.float_array_to_value_array();
                let Value::Array(a) = arr else { unreachable!() };
                let id = register_heap(RegistryEntry::Array(a));
                from_heap_handle(id)
            }
            Self::IntArray(data) => {
                // Same idea: rehydrate the flat-i64 representation
                // into the boxed `Vec<Value::Int>` shape so the
                // raw-layout consumers see a regular array.
                let boxed: Vec<Value> = data.iter().copied().map(Value::Int).collect();
                let id = register_heap(RegistryEntry::Array(Arc::new(boxed)));
                from_heap_handle(id)
            }
            Self::Variant(inner) => {
                let id = register_heap(RegistryEntry::Variant {
                    name: inner.name.clone(),
                    fields: Arc::clone(&inner.fields),
                });
                from_heap_handle(id)
            }
            Self::Struct(inner) => {
                let id = register_heap(RegistryEntry::Struct {
                    name: inner.name.clone(),
                    fields: Arc::clone(&inner.fields),
                });
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
            Self::Map(_) | Self::Builtin(_) | Self::Native(_) | Self::Void => {
                // Unencodable in the raw layout — return a sentinel
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
                match lookup_heap(id) {
                    Some(RegistryEntry::Int(n)) => Self::Int(n),
                    Some(RegistryEntry::String(s)) => Self::String(SmolStr::from_arc(s)),
                    Some(RegistryEntry::Tuple(t)) => Self::Tuple(t),
                    Some(RegistryEntry::Array(a)) => Self::Array(a),
                    Some(RegistryEntry::Variant { name, fields }) => Self::variant(name, fields),
                    Some(RegistryEntry::Struct { name, fields }) => Self::struct_(name, fields),
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
            Self::Unit => out.write_str(gossamer_runtime::builtins::format_unit()),
            Self::Bool(b) => out.write_str(gossamer_runtime::builtins::format_bool(*b)),
            Self::Int(i) => out.write_str(&gossamer_runtime::builtins::format_int(*i)),
            Self::Float(f) => out.write_str(&gossamer_runtime::builtins::format_float(*f)),
            Self::Char(c) => write!(out, "{c}"),
            Self::String(s) => out.write_str(s),
            Self::Tuple(parts) => write_tuple(out, parts),
            Self::Array(parts) => write_array(out, parts),
            Self::FloatArray(_) => write_array(out, &self.float_array_elems()),
            Self::IntArray(data) => {
                let elems: Vec<Value> = data.iter().copied().map(Value::Int).collect();
                write_array(out, &elems)
            }
            Self::Variant(inner) => write_variant(out, &inner.name, &inner.fields),
            Self::Struct(inner) => write_struct(out, &inner.name, &inner.fields),
            Self::Closure(_) => out.write_str("<closure>"),
            Self::Builtin(inner) => write!(out, "<builtin {}>", inner.name),
            Self::Native(inner) => write!(out, "<native {}>", inner.name),
            Self::Channel(ch) => write!(out, "{ch:?}"),
            Self::Map(map) => {
                out.write_str("{")?;
                for (i, (k, v)) in map.lock().iter().enumerate() {
                    if i > 0 {
                        out.write_str(", ")?;
                    }
                    write!(out, "{}: {v}", k.to_value())?;
                }
                out.write_str("}")
            }
            Self::Void => out.write_str("<void>"),
        }
    }
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
    fields: &[(Ident, Value)],
) -> fmt::Result {
    out.write_str(name)?;
    out.write_str(" { ")?;
    for (i, (ident, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{}: {value}", ident.name)?;
    }
    out.write_str(" }")
}

/// Concrete closure representation: captured environment plus the HIR
/// body to interpret on invocation.
#[derive(Debug, Clone)]
pub struct Closure {
    /// Parameters declared at the lowering stage.
    pub params: Vec<HirParam>,
    /// Body expression lowered from AST.
    pub body: HirExpr,
    /// Captured lexical bindings at closure-construction time. Stored
    /// as flat name/value pairs so the interpreter can splice them into
    /// a fresh frame on each call.
    pub captures: Vec<(String, Value)>,
}

/// Result type used throughout the interpreter for operations that can
/// abort with a runtime error.
pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// Top-level interpreter errors. Each variant carries a stable
/// diagnostic code (`GX0001` …) that both the interpreter and the
/// native backend use when reporting the same failure — the
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
        }
    }
}

/// Control-flow signal propagated through nested expressions.
#[derive(Debug, Clone)]
pub(crate) enum Flow {
    /// Normal evaluation produced a value.
    Value(Value),
    /// `return expr;` unwinds to the nearest call frame.
    Return(Value),
    /// `break expr;` unwinds to the nearest loop.
    Break(Value),
    /// `continue;` skips to the next loop iteration.
    Continue,
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
    /// GC-managed UTF-8 string.
    String(Arc<String>),
    /// Tuple aggregate.
    Tuple(Arc<Vec<Value>>),
    /// Array / Vec aggregate.
    Array(Arc<Vec<Value>>),
    /// Enum variant or tuple-struct constructor payload.
    Variant {
        /// Variant name.
        name: String,
        /// Positional fields.
        fields: Arc<Vec<Value>>,
    },
    /// Struct-shaped aggregate.
    Struct {
        /// Struct name.
        name: String,
        /// Field name/value pairs in declaration order.
        fields: Arc<Vec<(Ident, Value)>>,
    },
    /// User-defined callable.
    Closure(Arc<Closure>),
    /// Concurrent channel endpoint.
    Channel(Channel),
}

/// Global registry mapping `u32` handles to [`RegistryEntry`] values.
/// Protected by a [`Mutex`] so it is safe to access from goroutine
/// threads.
static REGISTRY: Mutex<Vec<Option<RegistryEntry>>> = Mutex::new(Vec::new());

/// Stores `entry` in the global side table and returns its stable
/// handle.
fn register_heap(entry: RegistryEntry) -> u32 {
    let mut reg = REGISTRY.lock();
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(entry);
            return u32::try_from(i).expect("registry index overflow");
        }
    }
    let id = reg.len();
    reg.push(Some(entry));
    u32::try_from(id).expect("registry handle overflow")
}

/// Looks up `handle` in the global side table.  Returns `None` when
/// the slot is empty (the object was GC'd or never registered).
fn lookup_heap(handle: u32) -> Option<RegistryEntry> {
    let reg = REGISTRY.lock();
    reg.get(handle as usize).and_then(std::clone::Clone::clone)
}
