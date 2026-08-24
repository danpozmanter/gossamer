# `std::math::big`

Status: experimental

Arbitrary-precision integers (num-bigint).

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Int`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `type Int` | Arbitrary-precision signed integer. |
| [`Uint`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `type Uint` | Arbitrary-precision unsigned integer. |
| [`factorial`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn factorial(n: i64) -> big::Uint` | Computes n! as an Int. |
| [`int_abs`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_abs(value: big::Int) -> big::Int` | Absolute value of an Int. |
| [`int_add`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_add(a: big::Int, b: big::Int) -> big::Int` | Sum of two Ints. |
| [`int_cmp`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_cmp(a: big::Int, b: big::Int) -> i64` | Three-way comparison of two Ints (-1, 0, 1). |
| [`int_div`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_div(a: big::Int, b: big::Int) -> big::Int` | Truncated quotient of two Ints. |
| [`int_from_i64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_from_i64(value: i64) -> big::Int` | Converts an i64 into an Int. |
| [`int_from_str`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_from_str(text: String) -> Result<big::Int, errors::Error>` | Parses a decimal string into an Int. |
| [`int_gcd`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_gcd(a: big::Int, b: big::Int) -> big::Int` | Greatest common divisor of two Ints. |
| [`int_is_negative`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_is_negative(value: big::Int) -> bool` | Reports whether the Int is less than zero. |
| [`int_is_positive`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_is_positive(value: big::Int) -> bool` | Reports whether the Int is greater than zero. |
| [`int_is_zero`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_is_zero(value: big::Int) -> bool` | Reports whether the Int is zero. |
| [`int_lcm`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_lcm(a: big::Int, b: big::Int) -> big::Int` | Least common multiple of two Ints. |
| [`int_mul`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_mul(a: big::Int, b: big::Int) -> big::Int` | Product of two Ints. |
| [`int_neg`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_neg(value: big::Int) -> big::Int` | Negation of an Int. |
| [`int_pow`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_pow(value: big::Int, exp: i64) -> big::Int` | Int raised to a non-negative power. |
| [`int_rem`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_rem(a: big::Int, b: big::Int) -> big::Int` | Remainder of two Ints. |
| [`int_sub`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_sub(a: big::Int, b: big::Int) -> big::Int` | Difference of two Ints. |
| [`int_to_hex`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_to_hex(value: big::Int) -> String` | Hexadecimal string form of an Int. |
| [`int_to_i64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_to_i64(value: big::Int) -> Result<i64, errors::Error>` | Narrows an Int to i64 where it fits. |
| [`int_to_str`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn int_to_str(value: big::Int) -> String` | Decimal string form of an Int. |
| [`uint_add`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_add(a: big::Uint, b: big::Uint) -> big::Uint` | Sum of two Uints. |
| [`uint_bit_len`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_bit_len(value: big::Uint) -> i64` | Number of significant bits in a Uint. |
| [`uint_from_str`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_from_str(text: String) -> Result<big::Uint, errors::Error>` | Parses a decimal string into a Uint. |
| [`uint_from_u64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_from_u64(value: u64) -> big::Uint` | Converts a u64 into a Uint. |
| [`uint_is_zero`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_is_zero(value: big::Uint) -> bool` | Reports whether the Uint is zero. |
| [`uint_mul`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_mul(a: big::Uint, b: big::Uint) -> big::Uint` | Product of two Uints. |
| [`uint_pow`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_pow(value: big::Uint, exp: i64) -> big::Uint` | Uint raised to a non-negative power. |
| [`uint_pow_mod`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_pow_mod(value: big::Uint, exp: big::Uint, modulus: big::Uint) -> big::Uint` | Modular exponentiation of a Uint. |
| [`uint_to_hex`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_to_hex(value: big::Uint) -> String` | Hexadecimal string form of a Uint. |
| [`uint_to_str`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_to_str(value: big::Uint) -> String` | Decimal string form of a Uint. |
| [`uint_to_u64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/math/big.rs) | `fn uint_to_u64(value: big::Uint) -> Result<u64, errors::Error>` | Narrows a Uint to u64 where it fits. |
