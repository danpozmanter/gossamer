# `std::math::bits`

Status: experimental

Integer bit-manipulation operations (Go's math/bits shape).

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`add`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn add(x: u64, y: u64, carry: u64) -> (u64, u64)` | x + y + carry; returns (sum, carry_out). |
| [`count_ones`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn count_ones(x: u64) -> i64` | Number of set bits (popcount). |
| [`count_zeros`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn count_zeros(x: u64) -> i64` | Number of clear bits. |
| [`div`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn div(hi: u64, lo: u64, y: u64) -> (u64, u64)` | 128-bit dividend / 64-bit divisor; returns (quotient, remainder). |
| [`leading_zeros`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn leading_zeros(x: u64) -> i64` | Leading zero bit count. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn len(x: u64) -> i64` | Minimum bits required to represent x. |
| [`mul`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn mul(x: u64, y: u64) -> (u64, u64)` | Full 128-bit product; returns (hi, lo). |
| [`reverse_bits`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn reverse_bits(x: u64) -> i64` | Reverses bit order of x. |
| [`reverse_bytes`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn reverse_bytes(x: u64) -> i64` | Reverses byte order of x. |
| [`rotate_left`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn rotate_left(x: u64, n: i64) -> u64` | Rotates x left by n bits. |
| [`rotate_right`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn rotate_right(x: u64, n: i64) -> u64` | Rotates x right by n bits. |
| [`sub`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn sub(x: u64, y: u64, borrow: u64) -> (u64, u64)` | x - y - borrow; returns (diff, borrow_out). |
| [`trailing_zeros`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn trailing_zeros(x: u64) -> i64` | Trailing zero bit count. |
