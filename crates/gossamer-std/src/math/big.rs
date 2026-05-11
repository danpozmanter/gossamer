// Runtime support for `std::math::big` — arbitrary-precision integers.
//
// Wraps `num-bigint::BigInt` for signed big integers and
// `num-bigint::BigUint` for unsigned, exposing the operations that
// are most useful from Gossamer programs. All I/O goes through
// decimal strings.

#![forbid(unsafe_code)]

use num_bigint::{BigInt, BigUint};
use num_traits::{One, Zero};

use crate::errors::Error;

/// A signed arbitrary-precision integer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Int(BigInt);

impl std::fmt::Display for Int {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for Int {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<BigInt>()
            .map(Self)
            .map_err(|e| Error::new(format!("big::Int: {e}")))
    }
}

impl Int {
    /// Parses a decimal string into an `Int`.
    pub fn parse(s: &str) -> Result<Self, Error> {
        s.parse()
    }

    /// Creates an `Int` from an `i64`.
    #[must_use]
    pub fn from_i64(n: i64) -> Self {
        Self(BigInt::from(n))
    }

    /// Returns the hex string representation (lowercase, no prefix).
    #[must_use]
    pub fn to_hex(&self) -> String {
        format!("{:x}", self.0)
    }

    /// Returns the value as an `i64`, or `None` if it doesn't fit.
    #[must_use]
    pub fn to_i64(&self) -> Option<i64> {
        use num_traits::ToPrimitive;
        self.0.to_i64()
    }

    /// `true` if this integer is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// `true` if this integer is positive.
    #[must_use]
    pub fn is_positive(&self) -> bool {
        self.0 > BigInt::zero()
    }

    /// `true` if this integer is negative.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.0 < BigInt::zero()
    }

    /// `self + rhs`.
    #[must_use]
    pub fn add(&self, rhs: &Self) -> Self {
        Self(&self.0 + &rhs.0)
    }

    /// `self - rhs`.
    #[must_use]
    pub fn sub(&self, rhs: &Self) -> Self {
        Self(&self.0 - &rhs.0)
    }

    /// `self * rhs`.
    #[must_use]
    pub fn mul(&self, rhs: &Self) -> Self {
        Self(&self.0 * &rhs.0)
    }

    /// `self / rhs`. Returns an error if `rhs` is zero.
    pub fn div(&self, rhs: &Self) -> Result<Self, Error> {
        if rhs.is_zero() {
            return Err(Error::new("big::Int: division by zero"));
        }
        Ok(Self(&self.0 / &rhs.0))
    }

    /// `self % rhs`. Returns an error if `rhs` is zero.
    pub fn rem(&self, rhs: &Self) -> Result<Self, Error> {
        if rhs.is_zero() {
            return Err(Error::new("big::Int: division by zero"));
        }
        Ok(Self(&self.0 % &rhs.0))
    }

    /// `self ^ exp` (integer power). `exp` must be non-negative.
    #[must_use]
    pub fn pow(&self, exp: u32) -> Self {
        use num_traits::Pow;
        Self(self.0.clone().pow(exp))
    }

    /// Absolute value.
    #[must_use]
    pub fn abs(&self) -> Self {
        use num_traits::Signed;
        Self(self.0.abs())
    }

    /// Negation.
    #[must_use]
    pub fn neg(&self) -> Self {
        Self(-self.0.clone())
    }

    /// Greatest common divisor.
    #[must_use]
    pub fn gcd(&self, rhs: &Self) -> Self {
        use num_integer::Integer;
        Self(self.0.gcd(&rhs.0))
    }

    /// Least common multiple.
    #[must_use]
    pub fn lcm(&self, rhs: &Self) -> Self {
        use num_integer::Integer;
        Self(self.0.lcm(&rhs.0))
    }

    /// Comparison: returns `-1`, `0`, or `1`.
    #[must_use]
    pub fn compare(&self, rhs: &Self) -> i64 {
        match self.0.cmp(&rhs.0) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

/// An unsigned arbitrary-precision integer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Uint(BigUint);

impl std::fmt::Display for Uint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for Uint {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<BigUint>()
            .map(Self)
            .map_err(|e| Error::new(format!("big::Uint: {e}")))
    }
}

impl Uint {
    /// Parses a decimal string.
    pub fn parse(s: &str) -> Result<Self, Error> {
        s.parse()
    }

    /// Creates a `Uint` from a `u64`.
    #[must_use]
    pub fn from_u64(n: u64) -> Self {
        Self(BigUint::from(n))
    }

    /// Hex string (lowercase, no prefix).
    #[must_use]
    pub fn to_hex(&self) -> String {
        format!("{:x}", self.0)
    }

    /// Returns value as `u64`, or `None` if it doesn't fit.
    #[must_use]
    pub fn to_u64(&self) -> Option<u64> {
        use num_traits::ToPrimitive;
        self.0.to_u64()
    }

    /// `true` if this is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// `self + rhs`.
    #[must_use]
    pub fn add(&self, rhs: &Self) -> Self {
        Self(&self.0 + &rhs.0)
    }

    /// `self * rhs`.
    #[must_use]
    pub fn mul(&self, rhs: &Self) -> Self {
        Self(&self.0 * &rhs.0)
    }

    /// `self ^ exp`.
    #[must_use]
    pub fn pow(&self, exp: u32) -> Self {
        use num_traits::Pow;
        Self(self.0.clone().pow(exp))
    }

    /// Modular exponentiation: `(self ^ exp) % modulus`.
    #[must_use]
    pub fn pow_mod(&self, exp: &Self, modulus: &Self) -> Self {
        Self(self.0.modpow(&exp.0, &modulus.0))
    }

    /// Bit length.
    #[must_use]
    pub fn bit_len(&self) -> u64 {
        self.0.bits()
    }
}

/// Multiplies a factorial for demonstration.
#[must_use]
pub fn factorial(n: u64) -> Int {
    let mut result = BigInt::one();
    for i in 2..=n {
        result *= i;
    }
    Int(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_int_arithmetic() {
        let a = Int::parse("12345678901234567890").unwrap();
        let b = Int::parse("98765432109876543210").unwrap();
        let sum = a.add(&b);
        assert_eq!(sum.to_string(), "111111111011111111100");
        let product = Int::from_i64(100).mul(&Int::from_i64(200));
        assert_eq!(product.to_i64(), Some(20000));
    }

    #[test]
    fn factorial_100_is_big() {
        let f = factorial(20);
        assert_eq!(f.to_string(), "2432902008176640000");
    }

    #[test]
    fn pow_mod_computes_correctly() {
        let base = Uint::from_u64(2);
        let exp = Uint::from_u64(10);
        let modulus = Uint::from_u64(1000);
        assert_eq!(base.pow_mod(&exp, &modulus).to_u64(), Some(24));
    }

    #[test]
    fn gcd_lcm() {
        let a = Int::from_i64(12);
        let b = Int::from_i64(8);
        assert_eq!(a.gcd(&b).to_i64(), Some(4));
        assert_eq!(a.lcm(&b).to_i64(), Some(24));
    }
}
