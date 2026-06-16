# Gossamer runtime value layout

This document is the authoritative source for the byte-level
representation of every runtime value that a Gossamer program can
observe. The tree-walking interpreter, the bytecode VM, and the native
backend all share the layouts recorded here. Any change to these
representations is an ABI break and requires a coordinated update
across all three consumers.

Layouts are expressed in terms of a machine word (`WORD_BYTES`). On the
primary 64-bit targets (`x86_64`, `aarch64`, `riscv64`) a word is
8 bytes and `HEAP_ALIGN` matches it. 32-bit targets (`wasm32`) use a
4-byte word; the layouts scale down uniformly.

## Object header

Every reference-counted heap allocation in the compiled tiers
(Cranelift, LLVM) begins with the fixed-size `RcHeader` defined in
`crates/gossamer-runtime/src/c_abi/rc.rs`. The pointer a compiled
program holds addresses the *payload*; the header sits
`RC_HEADER_SIZE` (8) bytes before it:

| Offset | Size | Field     | Purpose                                                                                          |
|--------|------|-----------|--------------------------------------------------------------------------------------------------|
| 0      | 4    | `strong`  | Strong reference count in the low 27 bits (`STRONG_COUNT_MASK`); the high bits hold the shared / region / buffered / cycle-color flags. Starts at 1. |
| 4      | 1    | `weak`    | Weak reference count (saturating); the allocation outlives `strong == 0` while this is non-zero. |
| 5      | 1    | `disc`    | Enum discriminant — codegen reads/writes the byte at `payload - 3`; 0 (and unread) for non-enum objects. |
| 6      | 2    | `meta_id` | Interned id of the child-layout descriptor blob; 0 for leaf objects with no RC-pointer children. |

Total: **8 bytes**, 8-byte-aligned (`RC_ALIGN`). There is no mark byte
and no tracing collector. An object is freed when its strong count
reaches zero — its RC-pointer children are released first — and
reference cycles are reclaimed on demand by the cycle collector
("Cycle-collector roots" below). The interpreter tier does not use this
header; it mirrors the same semantics with Rust `Arc`-shared values.

## Child-layout meta blob

`meta_id` interns a pointer to a per-type *child-layout blob* — the
descriptor the release path uses to find the RC-managed pointers inside
a payload. Codegen emits one flat `[i64]` blob per RC-managed ADT as a
single module constant; `meta_intern` / `meta_of` map between the blob
pointer and the 16-bit id stored in the header. The blob is
self-describing:

- `[0]` — kind tag (`RC_KIND_ENUM`, `RC_KIND_STRUCT`, … re-exported
  from `gossamer-abi`).
- `[1]` — variant count `V`.
- then `V` variant records, each `disc, child_count C, off_0 … off_C`,
  where each `off_i` is a payload word index (byte offset / 8) holding
  an RC-managed child pointer.

On release, `release_children` reads the live discriminant, finds the
matching record, and releases each child pointer. Leaf objects (no
RC-pointer children) carry `meta_id == 0` and free immediately at strong
count zero. No per-type `scan_fn` is walked by a collector — release is
driven entirely by this blob.

## Primitive values

Primitive types live inline and carry no header:

| Type        | Size (bytes) | Notes                                    |
|-------------|--------------|------------------------------------------|
| `bool`      | 1            | 0/1 only.                                |
| `char`      | 4            | Unicode scalar, NOT a surrogate half.    |
| `i8`/`u8`   | 1            |                                          |
| `i16`/`u16` | 2            |                                          |
| `i32`/`u32` | 4            |                                          |
| `i64`/`u64` | 8            |                                          |
| `i128`/`u128` | 16         | 8-byte aligned.                           |
| `isize`/`usize` | word     | Matches `WORD_BYTES`.                    |
| `f32`       | 4            |                                          |
| `f64`       | 8            |                                          |
| `()`        | 0            | Zero-sized.                              |

## Composite values

### `String`

A `String` value is a single pointer to the first content byte of a
heap buffer; the metadata lives in an inline prefix header just before
the content, so the program holds no separate length/capacity words.
For a growable string (`alloc_growable` in `c_abi/string.rs`) the
buffer is:

```
[ rc: u32 LE ][ cap: u32 LE ][ len: u32 LE ][ tag: u8 ][ content (cap bytes) ][ NUL ]
                                                        ^ ptr  (what the program holds)
```

so `ptr[-1]` is the provenance tag, `ptr[-5..-1]` the length,
`ptr[-9..-5]` the capacity, and `ptr[-13..-9]` the front reference
count. The buffer is reference counted: `gos_rt_str_retain` /
`gos_rt_str_free` adjust the front count and free at zero, refusing to
reclaim a pointer whose tag does not match (a foreign pointer leaks
rather than corrupting the heap). Static literals emitted into rodata
and arena-region strings carry distinct tags and are never individually
freed (a region frees its bytes wholesale at `arena_pop`). No tracing
collector scans the buffer.

### `Vec<T>`

A `Vec<T>` value is a pointer to a heap-allocated `GosVec` header
(`c_abi/vec.rs`):

```
GosVec {
  len:        i64,
  cap:        i64,
  elem_bytes: u32,
  elem_kind:  u8,         // tag driving deep-free of pointer-bearing elements
  _reserved:  [u8; 3],    // padding; _reserved[0] flags an arena-region vec
  ptr:        *mut u8,    // element buffer
}
```

The header is reference counted (`vec_retain_header`) and reclaimed by
`gos_rt_vec_free`, which uses `elem_kind` to recursively free
string / vec / map / RC-node elements; an arena-region vec is freed
wholesale at `arena_pop` instead. No tracing collector scans it.

### `HashMap<K, V>`

A `HashMap<K, V>` value is a pointer to a heap-allocated `GosMap`
header (`c_abi/map.rs`), which wraps a mutex-guarded typed-storage enum
rather than an inline swiss table:

```
GosMap {
  len_cache:   i64,
  storage:     parking_lot::Mutex<MapStorage>,
  blob_values: AtomicBool,   // true when stored values are owned RC copy-blobs
}

enum MapStorage {            // auto-promoted from Empty on first insert
  Empty,
  I64I64 (FxHashMap<i64, i64>),
  StrI64 (FxHashMap<Box<[u8]>, i64>),
  StrStr (FxHashMap<Box<[u8]>, Box<[u8]>>),
  I64Str (FxHashMap<i64, Box<[u8]>>),
  Bytes  (FxHashMap<Box<[u8]>, Box<[u8]>>),
  SkeyVal(FxHashMap<Box<[u8]>, i64>),   // aggregate keys (flat content bytes)
}
```

The backing maps are `rustc-hash` `FxHashMap`s. The header is reclaimed
by `gos_rt_map_free` (which releases any RC copy-blob values it owns);
there is no separately collected buckets array.

### Fat pointers (`dyn Trait`, closure)

Two words in every case:

```
dyn_ref::Repr     { data: *const (), vtable: *const Vtable }
closure::Repr     { code: *const fn, env: *const Obj }
```

### Struct

Inline C-style layout using each field's declared alignment. Fields
are emitted in declaration order; the compiler does **not** reorder to
minimize padding. Stable-layout reorderings would be visible to
`#[repr(C)]` FFI code.

### Enum

Tagged-union representation:

```
[ discriminant: uN ][ padding ][ payload: variant data ]
```

The discriminant width is the smallest integer type that fits the
variant count (`u8` through `u32`). Niche optimisations apply to
`Option<&T>` and `Option<NonZeroU*>`: they elide the discriminant and
reuse the pointee's zero-bit pattern.

## Function ABI

Function calls follow the target's native C ABI (System V on unix, MS
x64 on Windows). Reference-counted pointers travel in registers like
any other pointer. The compiler inserts balanced retain/release calls
(`gos_rt_rc_retain` / `gos_rt_rc_release`) around the points where a
reference is copied or dropped, so there are no safepoints, no stack
maps, and no register/stack-slot root scanning. The cycle collector
discovers its roots from a candidate buffer (next section) instead of
walking the stack.

## Cycle-collector roots

Reference counting alone cannot reclaim cycles (`A -> B -> A` never
reaches zero), so a synchronous Bacon-Rajan trial-deletion collector
backs it up (`collect_cycles` in `c_abi/rc.rs`). It needs no stack scan
and no compiler root map: its candidate roots are exactly the objects
whose strong count was decremented to a *non-zero* value
(`possible_root`), recorded in a thread-local buffer (`ROOTS`) and
deduplicated by the header's buffered bit. When the buffer crosses
`DEFAULT_COLLECT_THRESHOLD` (10 000) — or when user code calls
`runtime::collect_cycles()` (`gos_rt_collect_cycles`) — the collector
traces only the subgraph reachable from those candidates and frees any
confirmed garbage cycle. Objects shared across goroutines and
arena-region objects are excluded.

## Invariants enforced at compile time

The header size is pinned by a `const` assertion in `c_abi/rc.rs`
(`assert!(RC_HEADER_SIZE == 8, "RcHeader must remain 8 bytes")`), so any
field change that would grow the per-object header fails the build
rather than silently regressing memory or breaking the `payload - 3`
discriminant offset the compiled tiers rely on.
