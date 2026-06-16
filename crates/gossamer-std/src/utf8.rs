//! Runtime support for `std::utf8` - UTF-8 validation, scalar decoding,
//! and encoding with Go-parity surface.

#![forbid(unsafe_code)]

/// Unicode replacement character (U+FFFD), returned on decoding errors.
pub const RUNE_ERROR: char = '\u{FFFD}';
/// Any rune below this value encodes as a single byte.
pub const RUNE_SELF: u32 = 0x80;
/// Maximum valid Unicode code point.
pub const MAX_RUNE: u32 = 0x10_FFFF;
/// Maximum bytes per encoded rune.
pub const UTF_MAX: usize = 4;

/// Returns `true` iff `input` is a well-formed UTF-8 byte stream.
#[must_use]
pub fn is_valid(input: &[u8]) -> bool {
    std::str::from_utf8(input).is_ok()
}

/// Returns `true` iff `s` is well-formed UTF-8.
#[must_use]
pub fn valid_string(s: &str) -> bool {
    std::str::from_utf8(s.as_bytes()).is_ok()
}

/// Returns `true` iff `r` is a valid Unicode code point (not a surrogate).
#[must_use]
pub fn valid_rune(r: u32) -> bool {
    r <= MAX_RUNE && !(0xD800..=0xDFFF).contains(&r)
}

/// Returns `true` iff `p` begins with a complete, valid UTF-8 sequence.
#[must_use]
pub fn full_rune(p: &[u8]) -> bool {
    if p.is_empty() {
        return false;
    }
    let first = p[0];
    let width = if first < 0x80 {
        1
    } else if first < 0xC0 {
        return false; // continuation byte - not a valid start
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    };
    p.len() >= width
}

/// Returns `true` iff `s` begins with a complete UTF-8-encoded rune.
#[must_use]
pub fn full_rune_in_string(s: &str) -> bool {
    full_rune(s.as_bytes())
}

/// Returns `true` iff `b` is the first byte of a valid UTF-8 sequence
/// (i.e., not a continuation byte).
#[must_use]
pub fn rune_start(b: u8) -> bool {
    b & 0xC0 != 0x80
}

/// Decodes the first UTF-8 scalar in `p`.  Returns `(RUNE_ERROR, 1)` on
/// any encoding error (matching Go's `utf8.DecodeRune` semantics).
/// Returns `(RUNE_ERROR, 0)` on empty input.
#[must_use]
pub fn decode_rune(p: &[u8]) -> (char, usize) {
    if p.is_empty() {
        return (RUNE_ERROR, 0);
    }
    match std::str::from_utf8(p) {
        Ok(s) => {
            if let Some(ch) = s.chars().next() {
                return (ch, ch.len_utf8());
            }
            (RUNE_ERROR, 1)
        }
        Err(e) => {
            let valid = e.valid_up_to();
            if valid == 0 {
                (RUNE_ERROR, 1)
            } else {
                let ch = std::str::from_utf8(&p[..valid])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .unwrap_or(RUNE_ERROR);
                (ch, ch.len_utf8())
            }
        }
    }
}

/// Decodes the first UTF-8 scalar in `s`.
#[must_use]
pub fn decode_rune_in_string(s: &str) -> (char, usize) {
    match s.chars().next() {
        Some(ch) => (ch, ch.len_utf8()),
        None => (RUNE_ERROR, 0),
    }
}

/// Decodes the last UTF-8 scalar in `p`. Returns `(RUNE_ERROR, 1)` on
/// error, `(RUNE_ERROR, 0)` on empty input.
#[must_use]
pub fn decode_last_rune(p: &[u8]) -> (char, usize) {
    if p.is_empty() {
        return (RUNE_ERROR, 0);
    }
    // Walk back to find the start of the last rune.
    let mut i = p.len();
    while i > 0 && i > p.len().saturating_sub(4) {
        i -= 1;
        if rune_start(p[i]) {
            break;
        }
    }
    let (ch, size) = decode_rune(&p[i..]);
    if i + size == p.len() {
        (ch, size)
    } else {
        (RUNE_ERROR, 1)
    }
}

/// Decodes the last UTF-8 scalar in `s`.
#[must_use]
pub fn decode_last_rune_in_string(s: &str) -> (char, usize) {
    match s.chars().next_back() {
        Some(ch) => (ch, ch.len_utf8()),
        None => (RUNE_ERROR, 0),
    }
}

/// Encodes `scalar` as UTF-8 into `out`. Returns the number of bytes
/// written (1-4). Panics if `out` is shorter than `UTF_MAX`.
pub fn encode_rune(scalar: char, out: &mut [u8]) -> usize {
    scalar.encode_utf8(out).len()
}

/// Appends the UTF-8 encoding of `r` to `buf` and returns the extended slice.
#[must_use]
pub fn append_rune(mut buf: Vec<u8>, r: char) -> Vec<u8> {
    let mut tmp = [0u8; 4];
    let n = r.encode_utf8(&mut tmp).len();
    buf.extend_from_slice(&tmp[..n]);
    buf
}

/// Returns the number of Unicode scalar values in `input`.
#[must_use]
pub fn rune_count(input: &[u8]) -> usize {
    std::str::from_utf8(input).map_or(0, |s| s.chars().count())
}

/// Returns the number of Unicode scalar values in `s`.
#[must_use]
pub fn rune_count_in_string(s: &str) -> usize {
    s.chars().count()
}

/// Returns the number of bytes needed to encode `r` in UTF-8.
/// Returns -1 if `r` is not a valid scalar (Go returns `utf8.RuneError`
/// width, here we return -1 to be usable from i64-typed Gossamer code).
#[must_use]
pub fn rune_len(r: char) -> i64 {
    r.len_utf8() as i64
}

/// Returns the number of bytes needed to encode the codepoint `r`.
/// Returns -1 for invalid codepoints (surrogates or out-of-range).
#[must_use]
pub fn rune_len_u32(r: u32) -> i64 {
    char::from_u32(r).map_or(-1, |ch| ch.len_utf8() as i64)
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

/// Encodes `scalar` into `out`, returning the number of bytes written.
/// Panics if `out` is shorter than 4.
pub fn encode(scalar: char, out: &mut [u8]) -> usize {
    scalar.encode_utf8(out).len()
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
    fn valid_rune_rejects_surrogate() {
        assert!(!valid_rune(0xD800));
        assert!(!valid_rune(0xDFFF));
        assert!(valid_rune(0xE000));
        assert!(valid_rune(MAX_RUNE));
        assert!(!valid_rune(MAX_RUNE + 1));
    }

    #[test]
    fn decode_rune_ascii() {
        let (ch, n) = decode_rune(b"hello");
        assert_eq!(ch, 'h');
        assert_eq!(n, 1);
    }

    #[test]
    fn decode_rune_multibyte() {
        let (ch, n) = decode_rune("你好".as_bytes());
        assert_eq!(ch, '你');
        assert_eq!(n, 3);
    }

    #[test]
    fn decode_rune_empty() {
        let (ch, n) = decode_rune(b"");
        assert_eq!(ch, RUNE_ERROR);
        assert_eq!(n, 0);
    }

    #[test]
    fn decode_rune_invalid() {
        let (ch, n) = decode_rune(&[0xff]);
        assert_eq!(ch, RUNE_ERROR);
        assert_eq!(n, 1);
    }

    #[test]
    fn decode_last_rune_multibyte() {
        let s = "hello你";
        let (ch, n) = decode_last_rune(s.as_bytes());
        assert_eq!(ch, '你');
        assert_eq!(n, 3);
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

    #[test]
    fn rune_count_in_string_matches_rune_count() {
        let s = "café";
        assert_eq!(rune_count_in_string(s), 4);
    }

    #[test]
    fn rune_len_ascii_and_multibyte() {
        assert_eq!(rune_len('a'), 1);
        assert_eq!(rune_len('é'), 2);
        assert_eq!(rune_len('你'), 3);
        assert_eq!(rune_len('𝄞'), 4);
    }

    #[test]
    fn full_rune_on_complete_sequence() {
        assert!(full_rune("你好".as_bytes()));
        assert!(!full_rune(&"你好".as_bytes()[..2]));
        assert!(full_rune(b"a"));
    }

    #[test]
    fn rune_start_identifies_starts() {
        assert!(rune_start(b'a'));
        assert!(rune_start(0xE4)); // start of 3-byte sequence
        assert!(!rune_start(0x80)); // continuation byte
    }

    #[test]
    fn append_rune_extends_buffer() {
        let buf = append_rune(Vec::new(), '你');
        assert_eq!(buf, "你".as_bytes());
    }
}
