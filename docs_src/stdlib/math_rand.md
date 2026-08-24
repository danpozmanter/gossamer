# `std::math::rand`

Status: experimental

Deterministic pseudo-random number generation.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/mathrand.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Rng`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/mathrand.rs) | `type Rng` | SplitMix64-based RNG. |
