# Lazy iterator protocol

Integer range expressions produce lazy iterators, and adapters applied to those
iterator values remain lazy. A collection already holds its values, so it
traverses them eagerly: `xs.map(f)` and `iter::map(xs, f)` answer a `Vec`, and
`xs.iter()` is how a caller asks for the lazy walk instead.

## Goals

The public protocol supports lazy `map`, `filter`, `take`, `skip`,
`enumerate`, `chain`, and `zip` without allocating an intermediate `Vec`.
Terminal operations stop the source as soon as their result is known and
preserve the same closure call order in the VM, Cranelift JIT, and LLVM.

The built-in protocol and supported adapters are public in this release.
User-defined iterator implementations, extra adapters such as `filter_map` and
`flat_map`, and dynamic iterator trait objects are deferred until
representation and optimization data are available.

## Source ownership

An iterator is a linear state value. Calling `next(&mut self)` mutates its
cursor and returns `Option<Item>`.

- `RangeIter` owns its integer cursor and optional bounds. An open upper bound
  follows Rust overflow behavior: debug builds panic before yielding
  `i64::MAX`; release builds yield `i64::MAX`, wrap to `i64::MIN`, and
  continue.
- `SliceIter<'a, T>` borrows an immutable slice. Structural mutation of the
  source `Vec` is rejected while this iterator is live.
- `VecIntoIter<T>` owns the source allocation and moves each element out once.
- Adapter states own their upstream iterator and closure. They do not expose
  the upstream state separately.
- `chain` and `zip` own both inputs. `zip` stops when either input ends.

Dropping an iterator drops its closure, any buffered item, and every still-owned
source element exactly once. Exhausted iterators remain exhausted. Calling
`next` after exhaustion returns `None` without invoking user code.

## Invalidation and mutation

A borrowed `Vec` iterator records the source allocation identity and structural
generation. Element replacement that preserves length and capacity is visible
on the next read. Push, pop, insert, remove, clear, reserve, and reallocation
invalidate an outstanding borrowed iterator and produce a runtime error. A
future release should reject these mutations statically once region facts are
available.

An owning iterator is unaffected by later bindings that referred to the moved
source, because those bindings are unavailable after the move. Captured mutable
closure state is evaluated in source order and is published after every closure
call, including calls that panic.

## Terminal operations

- `fold`, `count`, `sum`, and `collect` consume to exhaustion unless a closure
  panics.
- `any`, `all`, and `find` stop after the first decisive item.
- `collect::<Vec<T>>` is the only required adapter pipeline allocation other
  than allocations performed by user closures or an owning source itself.
- A terminal consumes the iterator. Reuse after a terminal is a type error.
- Panics unwind through adapter states and run their drops once.

Infinite sources are valid only with a short-circuiting terminal or a bounding
adapter such as `take`. Tests must use a watchdog when deliberately constructing
an unbounded terminal.

## Typed MIR representation

The first implementation adds three internal operations rather than lowering
adapters to ordinary dynamic calls:

```text
IterSource { dst, source_kind, source, item_ty, ownership }
IterAdapter { dst, adapter_kind, upstream, closure_or_arg, item_ty }
IterNext { dst_option, iter_place, item_ty }
```

`iter_place` is mutable and non-copy. MIR validation rejects a second consumer,
use after terminal, and borrowed-source structural mutation. Drop elaboration
owns cleanup for source and adapter state. The representation carries concrete
item and state types, so no boxed `dyn Iterator` or per-item heap allocation is
required.

## Backend lowering

The VM stores iterator state in native handles and pulls from sources and
adapters only when a terminal requests an item. Adapter closures are called only
for items that reach the adapter.

Cranelift JIT and LLVM AOT use the same public contract through opaque runtime
handles for the supported 0.30 surface. Scalar `Iterator<i64>` pipelines cover
sources, `map`, `filter`, `take`, `skip`, `chain`, and scalar terminals. Pair
iterator handles cover `enumerate`, `zip`, `collect`, and `count` for
`(i64, i64)` items. The internal `IterSource`, `IterAdapter`, and `IterNext`
MIR statements are verified and preserved by optimization passes, but strict
lowering rejects them if a future lowering path emits them before executable
backend lowering is added.

The cross-tier conformance fixture records result and closure side-effect
order. Promotion requires identical output and no intermediate Vec allocation
for `range.map.filter.take.collect`.

The runnable `examples/projects/lazy_iterators` project shows the normal
materialization boundary. `benchmarks/lazy_iterators` compares a lazy pipeline
with the eager collection surface and pins equal output.

## Reaching the lazy surface

A lazy pipeline starts from a value that is already an iterator: a range
literal (`0..n`, `10..`), or `xs.iter()` over a collection. Every adapter
applied to one stays lazy until a terminal (`collect`, `sum`, `count`, ...)
pulls it. Code that wants eager materialization traverses the collection
through its own methods, which answer a `Vec` directly.
