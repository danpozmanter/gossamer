# `std::math`

Status: experimental

Mathematical constants and f64 functions (Go's math package shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `PI` | const | Archimedes' constant π. |
| `E` | const | Euler's number e. |
| `SQRT_2` | const | √2. |
| `LN_2` | const | Natural log of 2. |
| `LN_10` | const | Natural log of 10. |
| `PHI` | const | Golden ratio φ. |
| `INF` | const | Positive infinity. |
| `NAN` | const | Not-a-number value. |
| `abs` | fn | Absolute value of x. |
| `sqrt` | fn | Square root. |
| `cbrt` | fn | Cube root. |
| `floor` | fn | Largest integer ≤ x. |
| `ceil` | fn | Smallest integer ≥ x. |
| `round` | fn | Nearest integer, half away from zero. |
| `trunc` | fn | Integer part of x. |
| `sin` | fn | Sine (radians). |
| `cos` | fn | Cosine (radians). |
| `tan` | fn | Tangent (radians). |
| `asin` | fn | Arcsine (radians). |
| `acos` | fn | Arccosine (radians). |
| `atan` | fn | Arctangent (radians). |
| `atan2` | fn | Four-quadrant arctangent of y/x. |
| `exp` | fn | e^x. |
| `exp2` | fn | 2^x. |
| `ln` | fn | Natural logarithm. |
| `log2` | fn | Base-2 logarithm. |
| `log10` | fn | Base-10 logarithm. |
| `log` | fn | Logarithm with given base. |
| `pow` | fn | x raised to the power y. |
| `hypot` | fn | Euclidean distance √(x²+y²). |
| `rem` | fn | Floating-point remainder x%y. |
| `is_nan` | fn | Reports whether x is NaN. |
| `is_inf` | fn | Reports whether x is infinite. |
| `copysign` | fn | Magnitude of x with sign of y. |
| `positive_diff` | fn | max(x-y, 0). |
| `sinh` | fn | Hyperbolic sine. |
| `cosh` | fn | Hyperbolic cosine. |
| `tanh` | fn | Hyperbolic tangent. |
| `min` | fn | Lesser of two values. |
| `max` | fn | Greater of two values. |
| `clamp` | fn | Constrain x to the inclusive range [lo, hi]. |
| `LOG2_E` | const | Base-2 logarithm of e. |
| `LOG10_E` | const | Base-10 logarithm of e. |
| `MAX_F64` | const | Largest finite f64 value. |
| `MIN_POSITIVE_F64` | const | Smallest positive normal f64 value. |
| `NEG_INF` | const | Negative infinity. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`E`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Euler's number e. |
| [`INF`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Positive infinity. |
| [`LN_10`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Natural log of 10. |
| [`LN_2`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Natural log of 2. |
| [`LOG10_E`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Base-10 logarithm of e. |
| [`LOG2_E`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Base-2 logarithm of e. |
| [`MAX_F64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Largest finite f64 value. |
| [`MIN_POSITIVE_F64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Smallest positive normal f64 value. |
| [`NAN`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Not-a-number value. |
| [`NEG_INF`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Negative infinity. |
| [`PHI`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Golden ratio φ. |
| [`PI`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | Archimedes' constant π. |
| [`SQRT_2`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `const` — see the source declaration | √2. |
| [`abs`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn abs(x: f64) -> f64` | Absolute value of x. |
| [`acos`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn acos(x: f64) -> f64` | Arccosine (radians). |
| [`asin`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn asin(x: f64) -> f64` | Arcsine (radians). |
| [`atan`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn atan(x: f64) -> f64` | Arctangent (radians). |
| [`atan2`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn atan2(y: f64, x: f64) -> f64` | Four-quadrant arctangent of y/x. |
| [`Int`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `type` — see the source declaration | Arbitrary-precision signed integer. |
| [`Uint`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `type` — see the source declaration | Arbitrary-precision unsigned integer. |
| [`factorial`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn factorial(n: i64) -> big::Uint` | Computes n! as an Int. |
| [`int_abs`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_abs(value: big::Int) -> big::Int` | Absolute value of an Int. |
| [`int_add`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_add(a: big::Int, b: big::Int) -> big::Int` | Sum of two Ints. |
| [`int_cmp`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_cmp(a: big::Int, b: big::Int) -> i64` | Three-way comparison of two Ints (-1, 0, 1). |
| [`int_div`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_div(a: big::Int, b: big::Int) -> big::Int` | Truncated quotient of two Ints. |
| [`int_from_i64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_from_i64(value: i64) -> big::Int` | Converts an i64 into an Int. |
| [`int_from_str`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_from_str(text: String) -> Result<big::Int, errors::Error>` | Parses a decimal string into an Int. |
| [`int_gcd`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_gcd(a: big::Int, b: big::Int) -> big::Int` | Greatest common divisor of two Ints. |
| [`int_is_negative`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_is_negative(value: big::Int) -> bool` | Reports whether the Int is less than zero. |
| [`int_is_positive`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_is_positive(value: big::Int) -> bool` | Reports whether the Int is greater than zero. |
| [`int_is_zero`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_is_zero(value: big::Int) -> bool` | Reports whether the Int is zero. |
| [`int_lcm`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_lcm(a: big::Int, b: big::Int) -> big::Int` | Least common multiple of two Ints. |
| [`int_mul`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_mul(a: big::Int, b: big::Int) -> big::Int` | Product of two Ints. |
| [`int_neg`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_neg(value: big::Int) -> big::Int` | Negation of an Int. |
| [`int_pow`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_pow(value: big::Int, exp: i64) -> big::Int` | Int raised to a non-negative power. |
| [`int_rem`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_rem(a: big::Int, b: big::Int) -> big::Int` | Remainder of two Ints. |
| [`int_sub`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_sub(a: big::Int, b: big::Int) -> big::Int` | Difference of two Ints. |
| [`int_to_hex`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_to_hex(value: big::Int) -> String` | Hexadecimal string form of an Int. |
| [`int_to_i64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_to_i64(value: big::Int) -> Result<i64, errors::Error>` | Narrows an Int to i64 where it fits. |
| [`int_to_str`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn int_to_str(value: big::Int) -> String` | Decimal string form of an Int. |
| [`uint_add`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_add(a: big::Uint, b: big::Uint) -> big::Uint` | Sum of two Uints. |
| [`uint_bit_len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_bit_len(value: big::Uint) -> i64` | Number of significant bits in a Uint. |
| [`uint_from_str`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_from_str(text: String) -> Result<big::Uint, errors::Error>` | Parses a decimal string into a Uint. |
| [`uint_from_u64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_from_u64(value: u64) -> big::Uint` | Converts a u64 into a Uint. |
| [`uint_is_zero`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_is_zero(value: big::Uint) -> bool` | Reports whether the Uint is zero. |
| [`uint_mul`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_mul(a: big::Uint, b: big::Uint) -> big::Uint` | Product of two Uints. |
| [`uint_pow`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_pow(value: big::Uint, exp: i64) -> big::Uint` | Uint raised to a non-negative power. |
| [`uint_pow_mod`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_pow_mod(value: big::Uint, exp: big::Uint, modulus: big::Uint) -> big::Uint` | Modular exponentiation of a Uint. |
| [`uint_to_hex`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_to_hex(value: big::Uint) -> String` | Hexadecimal string form of a Uint. |
| [`uint_to_str`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_to_str(value: big::Uint) -> String` | Decimal string form of a Uint. |
| [`uint_to_u64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn uint_to_u64(value: big::Uint) -> Result<u64, errors::Error>` | Narrows a Uint to u64 where it fits. |
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
| [`cbrt`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn cbrt(x: f64) -> f64` | Cube root. |
| [`ceil`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn ceil(x: f64) -> f64` | Smallest integer ≥ x. |
| [`clamp`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn clamp(x: f64, min: f64, max: f64) -> f64` | Constrain x to the inclusive range [lo, hi]. |
| [`copysign`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn copysign(x: f64, y: f64) -> f64` | Magnitude of x with sign of y. |
| [`cos`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn cos(x: f64) -> f64` | Cosine (radians). |
| [`cosh`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn cosh(x: f64) -> f64` | Hyperbolic cosine. |
| [`exp`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn exp(x: f64) -> f64` | e^x. |
| [`exp2`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn exp2(x: f64) -> f64` | 2^x. |
| [`floor`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn floor(x: f64) -> f64` | Largest integer ≤ x. |
| [`hypot`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn hypot(x: f64, y: f64) -> f64` | Euclidean distance √(x²+y²). |
| [`is_inf`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn is_inf(x: f64, sign: i64) -> bool` | Reports whether x is infinite. |
| [`is_nan`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn is_nan(x: f64) -> bool` | Reports whether x is NaN. |
| [`ln`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn ln(x: f64) -> f64` | Natural logarithm. |
| [`log`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn log(x: f64, y: f64) -> f64` | Logarithm with given base. |
| [`log10`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn log10(x: f64) -> f64` | Base-10 logarithm. |
| [`log2`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn log2(x: f64) -> f64` | Base-2 logarithm. |
| [`max`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn max(x: f64, y: f64) -> f64` | Greater of two values. |
| [`min`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn min(x: f64, y: f64) -> f64` | Lesser of two values. |
| [`positive_diff`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn positive_diff(x: f64, y: f64) -> f64` | max(x-y, 0). |
| [`pow`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn pow(x: f64, y: f64) -> f64` | x raised to the power y. |
| [`Rng`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `type` — see the source declaration | SplitMix64-based RNG. |
| [`rem`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn rem(x: f64, y: f64) -> f64` | Floating-point remainder x%y. |
| [`round`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn round(x: f64) -> f64` | Nearest integer, half away from zero. |
| [`sin`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn sin(x: f64) -> f64` | Sine (radians). |
| [`sinh`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn sinh(x: f64) -> f64` | Hyperbolic sine. |
| [`sqrt`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn sqrt(x: f64) -> f64` | Square root. |
| [`tan`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn tan(x: f64) -> f64` | Tangent (radians). |
| [`tanh`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn tanh(x: f64) -> f64` | Hyperbolic tangent. |
| [`trunc`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/math.rs) | `fn trunc(x: f64) -> f64` | Integer part of x. |
