//! Runtime support for `std::utf16` - UTF-16 encoding and surrogate pair helpers.
//!
//! UTF-16 encodes Unicode scalar values in 16-bit code units. Scalars in the
//! Basic Multilingual Plane (U+0000..U+D7FF, U+E000..U+FFFF) map to one code
//! unit; supplementary scalars (U+10000..U+10FFFF) map to a high-low surrogate
//! pair.

#![forbid(unsafe_code)]

/// First code unit of a high surrogate pair.
pub const SURROGATE_MIN: u16 = 0xD800;
/// Last code unit of a low surrogate pair.
pub const SURROGATE_MAX: u16 = 0xDFFF;
/// First high (lead) surrogate.
pub const HIGH_SURROGATE_MIN: u16 = 0xD800;
/// Last high (lead) surrogate.
pub const HIGH_SURROGATE_MAX: u16 = 0xDBFF;
/// First low (trail) surrogate.
pub const LOW_SURROGATE_MIN: u16 = 0xDC00;
/// Last low (trail) surrogate.
pub const LOW_SURROGATE_MAX: u16 = 0xDFFF;

/// Returns `true` iff `r` falls in the surrogate range (U+D800..U+DFFF).
#[must_use]
pub fn is_surrogate(r: u16) -> bool {
    (SURROGATE_MIN..=SURROGATE_MAX).contains(&r)
}

/// Returns the number of UTF-16 code units needed to encode `r`.
/// Always 1 for BMP scalars, 2 for supplementary scalars.
#[must_use]
pub fn rune_len(r: char) -> usize {
    if (r as u32) < 0x10000 { 1 } else { 2 }
}

/// Encodes `r` into UTF-16, writing 1 or 2 code units into `buf`.
/// Returns the number of code units written.
/// Panics if `buf.len() < 2`.
pub fn encode_rune(r: char, buf: &mut [u16]) -> usize {
    let cp = r as u32;
    if cp < 0x10000 {
        buf[0] = cp as u16;
        1
    } else {
        let cp = cp - 0x10000;
        buf[0] = HIGH_SURROGATE_MIN + (cp >> 10) as u16;
        buf[1] = LOW_SURROGATE_MIN + (cp & 0x3FF) as u16;
        2
    }
}

/// Decodes a surrogate pair `(high, low)` into the corresponding scalar.
/// Returns `None` if the pair is not well-formed.
#[must_use]
pub fn decode_surrogate_pair(high: u16, low: u16) -> Option<char> {
    if !(HIGH_SURROGATE_MIN..=HIGH_SURROGATE_MAX).contains(&high) {
        return None;
    }
    if !(LOW_SURROGATE_MIN..=LOW_SURROGATE_MAX).contains(&low) {
        return None;
    }
    let cp =
        0x10000 + u32::from(high - HIGH_SURROGATE_MIN) * 0x400 + u32::from(low - LOW_SURROGATE_MIN);
    char::from_u32(cp)
}

/// Appends the UTF-16 encoding of `r` to `buf` and returns the extended `Vec`.
#[must_use]
pub fn append_rune(mut buf: Vec<u16>, r: char) -> Vec<u16> {
    let mut tmp = [0u16; 2];
    let n = encode_rune(r, &mut tmp);
    buf.extend_from_slice(&tmp[..n]);
    buf
}

/// Encodes a slice of `char` values into a `Vec<u16>`.
#[must_use]
pub fn encode(chars: &[char]) -> Vec<u16> {
    let mut out = Vec::with_capacity(chars.len());
    let mut tmp = [0u16; 2];
    for &ch in chars {
        let n = encode_rune(ch, &mut tmp);
        out.extend_from_slice(&tmp[..n]);
    }
    out
}

/// Decodes a `&[u16]` UTF-16 sequence into a `Vec<char>`.
/// Surrogate pairs are decoded to the corresponding supplementary scalar.
/// Unpaired surrogates are replaced with U+FFFD.
#[must_use]
pub fn decode(units: &[u16]) -> Vec<char> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (HIGH_SURROGATE_MIN..=HIGH_SURROGATE_MAX).contains(&u) {
            if i + 1 < units.len() {
                let lo = units[i + 1];
                if let Some(ch) = decode_surrogate_pair(u, lo) {
                    out.push(ch);
                    i += 2;
                    continue;
                }
            }
            out.push('\u{FFFD}');
        } else if (LOW_SURROGATE_MIN..=LOW_SURROGATE_MAX).contains(&u) {
            out.push('\u{FFFD}');
        } else if let Some(ch) = char::from_u32(u32::from(u)) {
            out.push(ch);
        } else {
            out.push('\u{FFFD}');
        }
        i += 1;
    }
    out
}

/// Encodes a Rust `&str` (UTF-8) directly to a `Vec<u16>`.
#[must_use]
pub fn encode_string(s: &str) -> Vec<u16> {
    s.chars().fold(Vec::with_capacity(s.len()), append_rune)
}

/// Decodes a `&[u16]` to a `String`, replacing unpaired surrogates with U+FFFD.
#[must_use]
pub fn decode_to_string(units: &[u16]) -> String {
    decode(units).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_roundtrip() {
        let mut buf = [0u16; 2];
        assert_eq!(encode_rune('A', &mut buf), 1);
        assert_eq!(buf[0], 0x0041);
    }

    #[test]
    fn supplementary_roundtrip() {
        let mut buf = [0u16; 2];
        let n = encode_rune('𝄞', &mut buf); // U+1D11E
        assert_eq!(n, 2);
        let ch = decode_surrogate_pair(buf[0], buf[1]).unwrap();
        assert_eq!(ch, '𝄞');
    }

    #[test]
    fn string_roundtrip() {
        let s = "hello, 世界! 𝄞";
        let encoded = encode_string(s);
        let decoded = decode_to_string(&encoded);
        assert_eq!(decoded, s);
    }

    #[test]
    fn unpaired_high_surrogate_replaced() {
        let units = [0xD800u16, 0x0041u16];
        let chars = decode(&units);
        assert_eq!(chars[0], '\u{FFFD}');
        assert_eq!(chars[1], 'A');
    }

    #[test]
    fn unpaired_low_surrogate_replaced() {
        let units = [0xDC00u16];
        let chars = decode(&units);
        assert_eq!(chars[0], '\u{FFFD}');
    }

    #[test]
    fn is_surrogate_detection() {
        assert!(is_surrogate(0xD800));
        assert!(is_surrogate(0xDFFF));
        assert!(!is_surrogate(0xD7FF));
        assert!(!is_surrogate(0xE000));
    }

    #[test]
    fn rune_len_bmp_and_supp() {
        assert_eq!(rune_len('A'), 1);
        assert_eq!(rune_len('𝄞'), 2);
    }

    #[test]
    fn encode_decode_slice() {
        let chars: Vec<char> = "café 𝄞".chars().collect();
        let units = encode(&chars);
        let back = decode(&units);
        assert_eq!(back, chars);
    }
}
