# Codegen ABI: the flat-i64 monomorphisation contract

Gossamer's compiled tier (`gos build` and `gos build --release`)
monomorphises every generic instantiation to a concrete function
in MIR. Today that monomorphisation **packs every value into a
64-bit slot** at the ABI boundary. The runtime reads / writes
those slots; codegen ferries them through Cranelift or LLVM.

This page documents the contract precisely, so you know which
generics work end-to-end in v1 and which deliberately fail
compilation with a diagnostic.

## What works

A generic instantiation `Foo<T>` compiles end-to-end when **every
type parameter `T` is representable in 64 bits**:

- All integer types (`i8` … `i128`* and `u8` … `u128`*, `isize`,
  `usize`).
- `f32`, `f64`.
- `bool`, `char`.
- `&T` and `&mut T` references — references are pointers, which
  are 64 bits on the platforms we ship.
- Heap-managed aggregate **handles**: `String`, `Vec<T>`,
  `HashMap<K, V>`, channel halves, `I64Vec`, `Mutex<T>`,
  `WaitGroup`, `Atomic<i64>`. Each is a 64-bit pointer or handle
  to runtime-managed storage.

\* `i128` and `u128` are passed as a pair of i64 slots, not a
single one. `Vec<i128>` works; **a generic `T = i128` does not**
in v1, because the monomorphic ABI is one slot per `T`.

## What fails to compile

If you instantiate a generic with a `T` that does not fit in 64
bits, the compiler refuses with diagnostic `GT0042`:

```
error[GT0042]: this generic instantiation is not yet supported
   = note: T = MyStruct (24-byte by-value), but v1 codegen passes
           every type parameter in a single 64-bit slot. See
           docs/codegen_abi.md for the full constraint.
   = help: until layout-driven specialisation lands in v1.x, work
           around this by boxing the value: `&MyStruct` or
           `Vec<MyStruct>`. Generic functions over `&T` work for
           any `T` because references fit in 64 bits.
```

The diagnostic is the contract. If you don't see this diagnostic,
the program compiles correctly. **There is no codegen path that
accepts an oversized `T` and produces a working binary.** You
will not get garbage output; you will get a hard compile error.

## Why this constraint exists

A v1 implementation goal was: ship a real native compiler that
beats Rust's reference fasta program at N=25M. We hit it (0.45 s
vs 0.82 s on a Ryzen 9 9900X). To get there, the MIR → Cranelift
/ LLVM lowerer specialised on a **fixed slot kind** so the
inner-loop ops are typed: an i64 add is a single Cranelift
instruction, not a polymorphic dispatch.

A by-value `MyStruct` argument needs the codegen to know its
layout (offsets, alignment, padding) at every monomorphic call
site. We have that information at MIR — but we don't yet
**propagate** it into the codegen call ABI. That work is parity
plan §P4 ("layout-driven specialisation"), tracked separately.

## How user code lives within the constraint

In practice, idiomatic Gossamer code rarely needs `T = struct`
because:

- `Vec<T>`, `HashMap<K, V>`, channels are runtime-provided and
  are *not* monomorphised by the user — they are a single
  generic implementation in the runtime that takes a `T` slot.
  The runtime handles arbitrarily-sized elements internally
  by allocating each `T` on the GC heap and the slot it stores
  is a pointer.
- User generics over `&T` work for any `T` (since `&T` is a
  pointer = 64 bits).
- Code that is generic over numeric type (`min<T: Ord>`,
  `sum<T: Add>`) works for every primitive numeric type up to
  64 bits.

The remaining gap is **user-defined generic functions over
user-defined `T`-by-value**. For example:

```gos
fn id<T>(x: T) -> T { x }   // T = i64: works
                             // T = MyStruct: GT0042

fn id_ref<T>(x: &T) -> &T { x }   // works for any T
```

Generic *types* with internal layout sensitivity (a hypothetical
user `MyVec<T>` storing `T` inline) hit the same constraint. The
v1 workaround is "store `&T`, let the GC own the body."

## What changes in v1.x

Layout-driven specialisation will:

1. Propagate layout (size, align, field offsets) for each `T`
   from MIR through to Cranelift / LLVM.
2. Switch the codegen call ABI to "by-value where the layout
   fits in registers; by-reference where it doesn't" — matching
   what `rustc` does.
3. Drop the `GT0042` diagnostic; turn the constraint off.

Until then, the diagnostic is the safety net that prevents
"compiles, segfaults at runtime" — a class of bug Gossamer is
deliberately willing to accept compile-time pain to avoid.

## See also

- [`non_goals_v1.md`](non_goals_v1.md) — the broader v1
  deferred-features list.
- `gos explain GT0042` — full long-form diagnostic text.
- Internal notes: `parity.md` §P4 and
  `compiler_tier_plan.md`.
