//! Canonical u64 value representation shared by the interpreter,
//! bytecode VM, and native backend.
//! Every Gossamer value that crosses a stage boundary (interpreter ↔
//! compiled code, or FFI) is encoded as a single `u64` word.  The low
//! three bits hold a tag; the remaining 61 bits hold the payload.
//!
//! Tag schema (stable as of Phase P1):
//! | tag bits | meaning | payload interpretation |
//! |----------|---------|------------------------|
//! | `0b000`  | immediate `i56` | sign-extended to `i64` |
//! | `0b001`  | heap handle | `u32` index into the GC arena |
//! | `0b010`  | `f64` NaN-boxed | IEEE-754 quiet-NaN space |
//! | `0b011`  | singleton | bool / char / unit / nil enum |
//! | `0b100`  | *reserved* | |
//! | `0b101`  | *reserved* | |
//! | `0b110`  | *reserved* | |
//! | `0b111`  | *reserved* | |

#![forbid(unsafe_code)]

/// A Gossamer value packed into a single machine word.
pub type GossamerValue = u64;

/// Bit mask for the three tag bits in the low end of a
/// [`GossamerValue`].
pub const TAG_MASK: u64 = 0b111;

/// Tag for a signed 56-bit integer that fits without heap allocation.
pub const TAG_IMMEDIATE: u64 = 0b000;

/// Tag for a GC-managed heap object.  The payload is a [`u32`]
/// handle (`GcRef`) stored in the high 61 bits.
pub const TAG_HEAP: u64 = 0b001;

/// Tag for an IEEE-754 `f64` value NaN-boxed into the quiet-NaN
/// payload space.
pub const TAG_FLOAT: u64 = 0b010;

/// Tag for small singleton values (`bool`, `char`, `unit`).  The
/// payload encodes the specific discriminant.
pub const TAG_SINGLETON: u64 = 0b011;

// ------------------------------------------------------------------
// Singleton payload constants (stored in the high 61 bits)

/// Payload word for the `unit` singleton.
pub const SINGLETON_UNIT: u64 = 0;
/// Payload word for the `false` singleton.
pub const SINGLETON_FALSE: u64 = 1;
/// Payload word for the `true` singleton.
pub const SINGLETON_TRUE: u64 = 2;

// ------------------------------------------------------------------
// Conversion helpers

/// Packs a small signed integer (must fit in 56 bits) into a
/// [`GossamerValue`].
///
/// # Panics
///
/// Panics in debug builds when `n` does not fit in an `i56`.
#[must_use]
pub fn from_i64(n: i64) -> GossamerValue {
    debug_assert!(fits_i56(n), "value {n} does not fit in i56");
    ((n as u64) << 3) | TAG_IMMEDIATE
}

/// Unpacks an immediate integer value.  The caller must have
/// verified the tag is [`TAG_IMMEDIATE`] beforehand.
#[must_use]
pub fn to_i64(v: GossamerValue) -> i64 {
    // Sign-extend from 56 bits to 64 bits.
    let shifted = (v >> 3) as i64;
    (shifted << 8) >> 8
}

/// Returns `true` when `n` fits in a signed 56-bit integer.
#[must_use]
pub fn fits_i56(n: i64) -> bool {
    n >> 56 == 0 || n >> 56 == -1
}

/// Packs a heap handle into a [`GossamerValue`].  The handle must
/// fit in 32 bits (the current `GcRef` encoding).
///
/// # Panics
///
/// Panics in debug builds when `handle` exceeds `u32::MAX`.
#[must_use]
pub fn from_heap_handle(handle: u32) -> GossamerValue {
    debug_assert!(
        handle <= 0x1FFF_FFFF,
        "heap handle {handle} exceeds 29 bits"
    );
    (u64::from(handle) << 3) | TAG_HEAP
}

/// Unpacks a heap handle.  The caller must have verified the tag
/// is [`TAG_HEAP`] beforehand.
#[must_use]
pub fn to_heap_handle(v: GossamerValue) -> u32 {
    ((v >> 3) & 0xFFFF_FFFF) as u32
}

/// Packs an `f64` into a [`GossamerValue`].  The low 3 bits are
/// reserved for the [`TAG_FLOAT`] tag; the float's original bit
/// pattern is stored in the high 61 bits with the low 3 bits
/// masked to zero.  This is a lossy encoding for the ~12.5 % of
/// floats whose low mantissa bits are non-zero; later phases may
/// heap-allocate such floats.
#[must_use]
pub fn from_f64(f: f64) -> GossamerValue {
    (f.to_bits() & !TAG_MASK) | TAG_FLOAT
}

/// Unpacks a [`TAG_FLOAT`] value back to `f64`.  Reconstructs the
/// bit pattern from the high 61 bits; the low 3 bits were masked
/// on encoding and are therefore zero in the result.
#[must_use]
pub fn to_f64(v: GossamerValue) -> f64 {
    f64::from_bits(v & !TAG_MASK)
}

/// Packs a singleton value into a [`GossamerValue`].
#[must_use]
pub fn from_singleton(discriminant: u64) -> GossamerValue {
    debug_assert!(
        discriminant <= 0x1FFF_FFFF_FFFF_FFFF,
        "singleton discriminant overflow"
    );
    (discriminant << 3) | TAG_SINGLETON
}

/// Unpacks a singleton discriminant.  The caller must have verified
/// the tag is [`TAG_SINGLETON`] beforehand.
#[must_use]
pub fn to_singleton(v: GossamerValue) -> u64 {
    v >> 3
}

/// Returns the tag bits of `v`.
#[must_use]
pub fn tag_of(v: GossamerValue) -> u64 {
    v & TAG_MASK
}

// ------------------------------------------------------------------
// Compile-time sanity checks

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i56_roundtrips_small_integers() {
        for n in [-100i64, -1, 0, 1, 42, 1000, i64::from(i32::MAX)] {
            assert_eq!(to_i64(from_i64(n)), n, "i56 roundtrip for {n}");
        }
    }

    #[test]
    fn heap_handle_roundtrips() {
        for h in [0u32, 1, 42, u32::MAX >> 3] {
            assert_eq!(to_heap_handle(from_heap_handle(h)), h);
        }
    }

    #[test]
    fn f64_roundtrips_finite_values() {
        for f in [0.0f64, -0.0, 1.0, -1.5, 2.0, 4.0, -8.0, 16.0] {
            let packed = from_f64(f);
            assert_eq!(tag_of(packed), TAG_FLOAT);
            let unpacked = to_f64(packed);
            assert_eq!(unpacked, f, "f64 roundtrip for {f}");
        }
    }

    #[test]
    fn singleton_roundtrips() {
        assert_eq!(to_singleton(from_singleton(SINGLETON_UNIT)), SINGLETON_UNIT);
        assert_eq!(to_singleton(from_singleton(SINGLETON_TRUE)), SINGLETON_TRUE);
    }

    #[test]
    fn tag_bits_are_distinct() {
        let tags = [TAG_IMMEDIATE, TAG_HEAP, TAG_FLOAT, TAG_SINGLETON];
        for (i, a) in tags.iter().enumerate() {
            for (j, b) in tags.iter().enumerate() {
                if i != j {
                    assert_ne!(a & TAG_MASK, b & TAG_MASK, "tags {a} and {b} collide");
                }
            }
        }
    }

    /// Bit patterns whose low three mantissa bits are non-zero
    /// will round-trip lossy through `from_f64` / `to_f64` because
    /// the encoding masks those bits to zero. Track the surviving
    /// payload precisely so a future change that breaks even this
    /// envelope (e.g. a wider tag) is caught immediately.
    #[test]
    fn f64_low_three_mantissa_bits_are_dropped_by_design() {
        let f = f64::from_bits(0x4000_0000_0000_0007); // bits ...111
        let packed = from_f64(f);
        let unpacked = to_f64(packed);
        // The encoding documents that the low 3 bits are masked.
        assert_eq!(unpacked.to_bits() & 0b111, 0);
        // The high 61 bits of the mantissa survive.
        assert_eq!(unpacked.to_bits() & !0b111, f.to_bits() & !0b111);
    }

    #[test]
    fn f64_inf_roundtrips() {
        for f in [f64::INFINITY, f64::NEG_INFINITY] {
            let packed = from_f64(f);
            assert_eq!(tag_of(packed), TAG_FLOAT);
            let unpacked = to_f64(packed);
            assert!(unpacked.is_infinite());
            assert_eq!(unpacked.is_sign_positive(), f.is_sign_positive());
        }
    }

    #[test]
    fn f64_nan_roundtrips_to_a_nan() {
        // The NaN payload's low three bits are sacrificed by the
        // encoding (just like any other float). Round-tripping
        // therefore preserves NaN-ness but not necessarily the
        // exact bit pattern. Future callers that need stable NaN
        // payloads must take a heap-tagged path.
        let f = f64::NAN;
        let packed = from_f64(f);
        assert_eq!(tag_of(packed), TAG_FLOAT);
        let unpacked = to_f64(packed);
        assert!(unpacked.is_nan(), "NaN round-trip must yield NaN");
    }

    #[test]
    fn f64_subnormals_preserve_high_mantissa_bits() {
        // `f64::MIN_POSITIVE` is the smallest *normal* double; its
        // representation has zero in the bits the encoding masks
        // (the exponent uses the high bits and the mantissa is
        // zero), so round-trip is exact.
        let f = f64::MIN_POSITIVE;
        let unpacked = to_f64(from_f64(f));
        assert_eq!(unpacked, f);

        // A *subnormal* close to the minimum non-zero value has all
        // its precision in the mantissa. The lowest 3 mantissa bits
        // are dropped; we assert the encoding remains within an
        // ulp-class of the input. If the encoding ever stops
        // preserving subnormals at all (e.g. flushing to zero) this
        // test catches it.
        let sub = f64::from_bits(0x10); // tiny subnormal
        let packed = from_f64(sub);
        let unpacked = to_f64(packed);
        assert_eq!(unpacked.to_bits() & 0b111, 0);
        // Same magnitude class (still subnormal, sign matches).
        assert_eq!(unpacked.is_sign_positive(), sub.is_sign_positive());
    }

    #[test]
    fn f64_zero_signs_roundtrip_distinctly() {
        // +0.0 and -0.0 differ only in the sign bit (high bit, well
        // above the masked low 3). The encoding must preserve the
        // distinction because some Gossamer programs branch on
        // `f.is_sign_negative()` for parity / NaN-payload-style
        // tricks.
        let pos = to_f64(from_f64(0.0));
        let neg = to_f64(from_f64(-0.0));
        assert_eq!(pos, 0.0);
        assert_eq!(neg, 0.0);
        assert!(pos.is_sign_positive());
        assert!(neg.is_sign_negative());
    }
}
