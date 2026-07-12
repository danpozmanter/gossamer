# `std::math::bits`

Status: experimental

Integer bit-manipulation operations (Go's math/bits shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `count_ones` | fn | Number of set bits (popcount). |
| `count_zeros` | fn | Number of clear bits. |
| `leading_zeros` | fn | Leading zero bit count. |
| `trailing_zeros` | fn | Trailing zero bit count. |
| `rotate_left` | fn | Rotates x left by n bits. |
| `rotate_right` | fn | Rotates x right by n bits. |
| `reverse_bits` | fn | Reverses bit order of x. |
| `reverse_bytes` | fn | Reverses byte order of x. |
| `len` | fn | Minimum bits required to represent x. |
| `add` | fn | x + y + carry; returns (sum, carry_out). |
| `sub` | fn | x - y - borrow; returns (diff, borrow_out). |
| `mul` | fn | Full 128-bit product; returns (hi, lo). |
| `div` | fn | 128-bit dividend / 64-bit divisor; returns (quotient, remainder). |

