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
