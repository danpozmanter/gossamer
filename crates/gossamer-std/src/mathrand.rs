//! Runtime support for `std::math::rand` — deterministic pseudo-
//! random numbers suitable for simulation, testing, and
//! non-security work.
//! Uses `SplitMix64` for scalar generation — tiny, passable
//! statistical quality, and trivially seedable.

#![forbid(unsafe_code)]

/// Deterministic RNG. Identical seeds always produce identical
/// sequences.
#[derive(Debug, Clone, Copy)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Builds an RNG from `seed`. A `seed` of 0 is permitted — the
    /// first [`next_u64`] still returns a non-zero value.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Returns the next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Returns the next `u32` output.
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Returns a uniform `u64` in `[low, high)`. Panics when
    /// `high <= low`.
    pub fn range_u64(&mut self, low: u64, high: u64) -> u64 {
        assert!(high > low, "empty range");
        low + self.next_u64() % (high - low)
    }

    /// Returns a uniform `f64` in `[0.0, 1.0)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    /// Shuffles `slice` in place using a Fisher-Yates swap.
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        if slice.len() <= 1 {
            return;
        }
        for i in (1..slice.len()).rev() {
            let j = (self.next_u64() % (i as u64 + 1)) as usize;
            slice.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_seeds_produce_identical_streams() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge_quickly() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        let mut differ = false;
        for _ in 0..8 {
            if a.next_u64() != b.next_u64() {
                differ = true;
                break;
            }
        }
        assert!(differ);
    }

    #[test]
    fn range_u64_never_escapes_bounds() {
        let mut rng = Rng::new(0);
        for _ in 0..10_000 {
            let v = rng.range_u64(10, 20);
            assert!((10..20).contains(&v));
        }
    }

    #[test]
    fn next_f64_is_inside_unit_interval() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            let f = rng.next_f64();
            assert!((0.0..1.0).contains(&f));
        }
    }

    #[test]
    fn shuffle_permutes_slice_deterministically() {
        let mut items = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut rng = Rng::new(123);
        rng.shuffle(&mut items);
        let mut replay = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut rng2 = Rng::new(123);
        rng2.shuffle(&mut replay);
        assert_eq!(items, replay);
    }
}
