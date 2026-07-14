# `std::runtime::collect_cycles`

Status: shipped

Requests collection of unreachable reference cycles.

## Signature

```gos
fn collect_cycles() -> ()
```

## Behavior

On compiled tiers, `collect_cycles()` runs the native runtime's cycle
collector for thread-local reference-counted graphs. It can reclaim cycles
that are unreachable except through their own strong references.

The bytecode VM exposes the same function so programs typecheck and run
consistently. VM heap values are `Arc`-backed, so the call does not provide
collection-driven weak invalidation there.

Values that have crossed goroutine boundaries are excluded from the native
collector's thread-local pass. Break cross-goroutine cycles explicitly with
`Weak<T>`.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/runtime.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

This library currently declares no additional public Gossamer-language items. Its runtime integration is documented in the implementation source above.
