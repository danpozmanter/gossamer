//! The value range a declared integer width holds.
//!
//! A float-to-integer cast saturates at the target's own range, so the
//! bounds have to read the same in the bytecode VM, the JIT, and the AOT
//! back-end. They are stated once here.

/// Inclusive bounds of `width`-bit integers with the given signedness.
///
/// Both are returned as `i64`, the width every integer travels in at
/// runtime. A 64-bit unsigned target reports the signed range: an unsigned
/// value above `i64::MAX` has no separate representation in that model.
#[must_use]
pub const fn bounds(width: u32, signed: bool) -> (i64, i64) {
    if width >= 64 {
        return (i64::MIN, i64::MAX);
    }
    if signed {
        let half = 1i64 << (width - 1);
        (-half, half - 1)
    } else {
        (0, (1i64 << width) - 1)
    }
}

#[cfg(test)]
mod int_range_tests {
    use super::bounds;

    #[test]
    fn narrow_widths_report_their_own_range() {
        assert_eq!(bounds(8, false), (0, 255));
        assert_eq!(bounds(8, true), (-128, 127));
        assert_eq!(bounds(1, false), (0, 1));
        assert_eq!(bounds(16, true), (-32768, 32767));
        assert_eq!(bounds(32, false), (0, 4_294_967_295));
    }

    #[test]
    fn sixty_four_bits_report_the_runtime_word() {
        assert_eq!(bounds(64, true), (i64::MIN, i64::MAX));
        assert_eq!(bounds(64, false), (i64::MIN, i64::MAX));
    }
}
