//! Runtime support for `std::utf8` — UTF-8 validation and scalar
//! decoding.

#![forbid(unsafe_code)]

/// Returns `true` iff `input` is a well-formed UTF-8 byte stream.
#[must_use]
pub fn is_valid(input: &[u8]) -> bool {
    std::str::from_utf8(input).is_ok()
}

/// Decodes the first UTF-8 scalar in `input`. Returns
/// `(scalar, byte_length)` or `None` at the start of an ill-formed
/// sequence / empty input.
#[must_use]
pub fn decode_first(input: &[u8]) -> Option<(char, usize)> {
    let text = std::str::from_utf8(input).ok()?;
    let ch = text.chars().next()?;
    Some((ch, ch.len_utf8()))
}

/// Encodes `scalar` into `out`, returning the number of bytes
/// written. Panics if `out` is shorter than 4.
pub fn encode(scalar: char, out: &mut [u8]) -> usize {
    scalar.encode_utf8(out).len()
}

/// Returns the number of scalar values in `input`.
#[must_use]
pub fn rune_count(input: &[u8]) -> usize {
    std::str::from_utf8(input)
        .map_or(0, |s| s.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_accepts_ascii() {
        assert!(is_valid(b"hello"));
    }

    #[test]
    fn is_valid_accepts_multibyte() {
        assert!(is_valid("héllo".as_bytes()));
    }

    #[test]
    fn is_valid_rejects_broken_sequence() {
        let bad = [0xff, 0xfe];
        assert!(!is_valid(&bad));
    }

    #[test]
    fn decode_first_returns_scalar_and_length() {
        let (ch, n) = decode_first("你好".as_bytes()).unwrap();
        assert_eq!(ch, '你');
        assert_eq!(n, 3);
    }

    #[test]
    fn rune_count_counts_scalars() {
        assert_eq!(rune_count("abc".as_bytes()), 3);
        assert_eq!(rune_count("αβγ".as_bytes()), 3);
    }
}
