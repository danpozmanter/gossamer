// Runtime support for `std::encoding::base32` - RFC 4648 Base32.
//
// Two alphabets are provided:
//   - Standard (A-Z 2-7, padded with `=`)
//   - Hex (0-9 A-V, same padding; sort-preserving)
//
// Both encode/decode operate on byte slices; `encode_string` and
// `decode_string` handle the UTF-8 string entry points.

#![forbid(unsafe_code)]

const ALPHA_STD: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const ALPHA_HEX: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";
const PAD: u8 = b'=';

fn encode_inner(data: &[u8], alpha: &[u8; 32], pad: bool) -> String {
    let mut out = Vec::with_capacity((data.len() * 8).div_ceil(5));
    let mut buf = 0u64;
    let mut bits = 0u32;
    for &b in data {
        buf = (buf << 8) | u64::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(alpha[((buf >> bits) & 0x1F) as usize]);
        }
    }
    if bits > 0 {
        out.push(alpha[((buf << (5 - bits)) & 0x1F) as usize]);
        if pad {
            let pad_count = match bits {
                1 => 4,
                2 => 1,
                3 => 6,
                4 => 3,
                _ => 0,
            };
            out.extend(std::iter::repeat_n(PAD, pad_count));
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn decode_inner(s: &str, alpha: &[u8; 32]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let mut buf = 0u64;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == PAD {
            break;
        }
        let Some(val) = alpha.iter().position(|&a| a == c.to_ascii_uppercase()) else {
            return Err(format!("base32: invalid character '{}'", c as char));
        };
        buf = (buf << 5) | val as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

/// Encodes `data` using the standard RFC 4648 Base32 alphabet (A-Z 2-7),
/// with `=` padding.
#[must_use]
pub fn encode(data: &[u8]) -> String {
    encode_inner(data, ALPHA_STD, true)
}

/// Decodes a standard Base32 string. Returns an error on invalid characters.
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    decode_inner(s, ALPHA_STD)
}

/// Encodes a UTF-8 string using standard Base32.
#[must_use]
pub fn encode_string(s: &str) -> String {
    encode(s.as_bytes())
}

/// Decodes a standard Base32 string into a UTF-8 string.
pub fn decode_string(s: &str) -> Result<String, String> {
    let bytes = decode(s)?;
    String::from_utf8(bytes).map_err(|e| format!("base32: {e}"))
}

/// Encodes `data` using the hex (extended) Base32 alphabet (0-9 A-V).
#[must_use]
pub fn encode_hex(data: &[u8]) -> String {
    encode_inner(data, ALPHA_HEX, true)
}

/// Decodes a hex-alphabet Base32 string.
pub fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    decode_inner(s, ALPHA_HEX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_test_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "MY======");
        assert_eq!(encode(b"fo"), "MZXQ====");
        assert_eq!(encode(b"foo"), "MZXW6===");
        assert_eq!(encode(b"foob"), "MZXW6YQ=");
        assert_eq!(encode(b"fooba"), "MZXW6YTB");
        assert_eq!(encode(b"foobar"), "MZXW6YTBOI======");
    }

    #[test]
    fn decode_round_trips() {
        for s in ["", "f", "foo", "hello world", "gossamer lang"] {
            let enc = encode(s.as_bytes());
            let dec = decode(&enc).unwrap();
            assert_eq!(dec, s.as_bytes());
        }
    }

    #[test]
    fn hex_alphabet_round_trips() {
        let enc = encode_hex(b"foobar");
        let dec = decode_hex(&enc).unwrap();
        assert_eq!(dec, b"foobar");
    }

    #[test]
    fn invalid_char_returns_err() {
        assert!(decode("!!!!").is_err());
    }
}
