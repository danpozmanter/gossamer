# `std::math`

Status: shipped

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
| `min_f64` | fn | Lesser of two f64 values. |
| `max_f64` | fn | Greater of two f64 values. |
| `min_i64` | fn | Lesser of two i64 values. |
| `max_i64` | fn | Greater of two i64 values. |
| `abs_i64` | fn | Absolute value of an i64. |
| `fmod` | fn | Floating-point remainder x%y. |
| `is_nan` | fn | Reports whether x is NaN. |
| `is_inf` | fn | Reports whether x is infinite. |
| `nan` | fn | Returns the IEEE 754 NaN value. |
| `inf` | fn | Returns ±infinity based on sign. |
| `copysign` | fn | Magnitude of x with sign of y. |
| `dim` | fn | max(x-y, 0) — Go's math.Dim. |

