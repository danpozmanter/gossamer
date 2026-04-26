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

Every GC-managed heap allocation begins with the fixed-size
`ObjHeader` defined in `crates/gossamer-runtime/src/layout.rs`:

| Offset | Size | Field       | Purpose                                             |
|--------|------|-------------|-----------------------------------------------------|
| 0      | 8    | `type_info` | Pointer to the object's [`TypeInfo`] descriptor.    |
| 8      | 1    | `gc_mark`   | Set by the mark phase of the tracing GC.            |
| 9      | 1    | `flags`     | Reserved; future use for forwarding / pinning bits. |
| 10     | 6    | padding     | Zeroed on allocation.                               |

Total: **16 bytes**, word-aligned.

## TypeInfo descriptor

`TypeInfo` is a shared, read-only record. Every ADT emits a static
`TypeInfo` instance at compile time; the header points at it by
address, not by index. Fields:

- `size: usize` — size of the payload that follows the header.
- `align: usize` — alignment of the payload.
- `scan_fn: fn(*const u8, &mut dyn FnMut(*const u8))` — walks the
  payload and invokes the visitor on each GC-managed pointer it
  encounters. Primitives supply a no-op implementation.
- `drop_fn: Option<fn(*mut u8)>` — optional destructor for values that
  own native resources (files, sockets). Pure-Gossamer types leave this
  as `None`.

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

`String` is an immutable, growable UTF-8 string. The value layout is a
three-word record:

```
Repr {
  ptr: *const u8,    // GC-managed buffer
  len: usize,
  capacity: usize,
}
```

Total: **3 words**. The `ptr` field points at a GC-allocated byte
buffer; `String` values never own a non-GC allocation.

### `Vec<T>`

Three-word record identical in shape to `String`:

```
Repr {
  ptr: *const T,
  len: usize,
  capacity: usize,
}
```

### `HashMap<K, V>`

Swiss-table layout; four words at the top level plus a GC-managed
buckets array:

```
Repr {
  ctrl: *const u8,     // control-byte table
  buckets: *const u8,  // entry storage
  len: usize,
  capacity: usize,
}
```

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
x64 on Windows). GC-managed references travel in registers like any
other pointer. At every safepoint, the native backend emits a stack
map recording which registers and stack slots currently hold GC
references. The stack maps are consumed by the GC during marking (see
).

## Invariants enforced at compile time

The `_ASSERTIONS` constant in `layout.rs` encodes every invariant above
as a `const fn` check. Any change to the struct shapes that violates
those bounds produces a compile error, so the reproducibility guarantee
the three compilers rely on is not an aspiration — it is a precondition
for building the crate at all.
