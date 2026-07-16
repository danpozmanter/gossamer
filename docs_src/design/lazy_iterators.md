# Lazy iterator protocol

Status: design accepted for staged implementation; public eager `std::iter`
helpers remain Experimental in 0.29.0.

## Goals

The public protocol must support lazy `map`, `filter`, `take`, `skip`,
`enumerate`, `chain`, and `zip` without allocating an intermediate `Vec`.
Terminal operations must stop the source as soon as their result is known and
must preserve the same closure call order in the VM, Cranelift JIT, and LLVM.

The protocol is internal in its first release. User implementations and dynamic
iterator trait objects are deferred until representation and optimization data
are available.

## Source ownership

An iterator is a linear state value. Calling `next(&mut self)` mutates its
cursor and returns `Option<Item>`.

- `RangeIter` owns integer bounds and the next index.
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
invalidate an outstanding borrowed iterator and produce a runtime error in
Experimental editions. A future Stable edition should reject these mutations
statically once region facts are available.

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

The VM stores iterator state in typed native handles and adds source-specific
`next` bytecodes for range, slice, and owning Vec. Adapter bytecodes call the
existing closure dispatcher only when an item reaches that adapter.

Cranelift and LLVM lower the same MIR states to stack or coroutine-frame
aggregates when they do not escape. `IterNext` becomes a small state-machine
branch. A runtime handle is permitted only when an iterator crosses an unknown
FFI boundary or is stored in an erased aggregate.

The cross-tier conformance fixture records result, closure side-effect order,
panic text, and allocation count. Promotion requires identical output and zero
intermediate Vec allocations for `range.map.filter.take.collect`.

## Migration

The existing eager signatures stay available under their current names for the
0.29 line and retain Experimental status. The lazy protocol first lands behind
the `edition = "2027"` project setting with iterator-returning signatures.
Projects on the prior edition keep eager behavior. The formatter and migration
tool can then rewrite explicitly eager code to `iter::eager_*` names before the
old signatures are deprecated.

## Work split

1. Type and MIR owner: add linear iterator types, the three MIR operations,
   validation, and drop elaboration.
2. VM owner: implement range, slice, and owning Vec sources plus `IterNext`.
3. Backend owners: lower the agreed MIR independently in Cranelift and LLVM.
4. Stdlib owner: add adapters and terminals only after all three source kinds
   pass parity tests.
5. Test owner: add early termination, infinite source, mutation invalidation,
   panic, captured state, and allocation-count fixtures.
6. Migration owner: edition gate, eager aliases, diagnostics, and automated
   rewrites.

Tracks 2, 3, and 5 can proceed in parallel after track 1 freezes the MIR data
layout. Tracks 4 and 6 depend on the first all-tier `next` fixture.
