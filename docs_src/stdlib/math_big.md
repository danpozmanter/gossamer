# `std::math::big`

Status: shipped

Arbitrary-precision integers (num-bigint).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Int` | type | Arbitrary-precision signed integer. |
| `Uint` | type | Arbitrary-precision unsigned integer. |
| `factorial` | fn | Computes n! as an Int. |
| `int_from_str` | fn | Parses a decimal string into an Int. |
| `int_from_i64` | fn | Converts an i64 into an Int. |
| `int_to_str` | fn | Decimal string form of an Int. |
| `int_to_hex` | fn | Hexadecimal string form of an Int. |
| `int_to_i64` | fn | Narrows an Int to i64 where it fits. |
| `int_is_zero` | fn | Reports whether the Int is zero. |
| `int_is_positive` | fn | Reports whether the Int is greater than zero. |
| `int_is_negative` | fn | Reports whether the Int is less than zero. |
| `int_add` | fn | Sum of two Ints. |
| `int_sub` | fn | Difference of two Ints. |
| `int_mul` | fn | Product of two Ints. |
| `int_div` | fn | Truncated quotient of two Ints. |
| `int_rem` | fn | Remainder of two Ints. |
| `int_pow` | fn | Int raised to a non-negative power. |
| `int_abs` | fn | Absolute value of an Int. |
| `int_neg` | fn | Negation of an Int. |
| `int_gcd` | fn | Greatest common divisor of two Ints. |
| `int_lcm` | fn | Least common multiple of two Ints. |
| `int_cmp` | fn | Three-way comparison of two Ints (-1, 0, 1). |
| `uint_from_str` | fn | Parses a decimal string into a Uint. |
| `uint_from_u64` | fn | Converts a u64 into a Uint. |
| `uint_to_str` | fn | Decimal string form of a Uint. |
| `uint_to_hex` | fn | Hexadecimal string form of a Uint. |
| `uint_to_u64` | fn | Narrows a Uint to u64 where it fits. |
| `uint_is_zero` | fn | Reports whether the Uint is zero. |
| `uint_add` | fn | Sum of two Uints. |
| `uint_mul` | fn | Product of two Uints. |
| `uint_pow` | fn | Uint raised to a non-negative power. |
| `uint_pow_mod` | fn | Modular exponentiation of a Uint. |
| `uint_bit_len` | fn | Number of significant bits in a Uint. |

