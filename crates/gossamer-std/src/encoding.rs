//! Runtime support for `std::encoding::{base64, hex, binary}`.
//! Pure-Rust, allocation-conscious one-shot encode/decode helpers.
//! The `binary` submodule wraps endianness packing, the `base64` and
//! `hex` submodules handle byte-string conversion.

#![forbid(unsafe_code)]

pub mod base64 {
    //! RFC 4648 base64 with the standard alphabet.

    use crate::errors::Error;

    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// Encodes `input` to a base64 string (with `=` padding).
    #[must_use]
    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        let mut chunks = input.chunks_exact(3);
        for chunk in chunks.by_ref() {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(chunk[1]) << 8)
                | u32::from(chunk[2]);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        }
        let rem = chunks.remainder();
        match rem.len() {
            1 => {
                let n = u32::from(rem[0]) << 16;
                out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
                out.push('=');
                out.push('=');
            }
            2 => {
                let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
                out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
                out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
                out.push('=');
            }
            _ => {}
        }
        out
    }

    /// Decodes a base64 string, tolerating whitespace between
    /// characters.
    pub fn decode(input: &str) -> Result<Vec<u8>, Error> {
        let filtered: Vec<u8> = input
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        if filtered.len() % 4 != 0 {
            return Err(Error::new("base64 input length must be a multiple of 4"));
        }
        let mut out = Vec::with_capacity(filtered.len() / 4 * 3);
        for chunk in filtered.chunks(4) {
            let mut values = [0u32; 4];
            let mut pad = 0;
            for (i, byte) in chunk.iter().enumerate() {
                if *byte == b'=' {
                    pad += 1;
                    values[i] = 0;
                } else {
                    values[i] = index(*byte)
                        .ok_or_else(|| Error::new(format!("bad base64 character `{}`", *byte as char)))?
                        .into();
                }
            }
            let n =
                (values[0] << 18) | (values[1] << 12) | (values[2] << 6) | values[3];
            out.push((n >> 16) as u8);
            if pad < 2 {
                out.push((n >> 8) as u8);
            }
            if pad < 1 {
                out.push(n as u8);
            }
        }
        Ok(out)
    }

    fn index(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
}

pub mod hex {
    //! Lowercase hex encoding.

    use crate::errors::Error;

    /// Encodes `input` as lowercase hex.
    #[must_use]
    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len() * 2);
        for byte in input {
            out.push(nibble(*byte >> 4));
            out.push(nibble(*byte & 0xf));
        }
        out
    }

    /// Decodes a hex string, rejecting non-hex bytes and odd length.
    pub fn decode(input: &str) -> Result<Vec<u8>, Error> {
        if input.len() % 2 != 0 {
            return Err(Error::new("hex input must have even length"));
        }
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks(2) {
            let hi = value(pair[0]).ok_or_else(|| Error::new("bad hex digit"))?;
            let lo = value(pair[1]).ok_or_else(|| Error::new("bad hex digit"))?;
            out.push((hi << 4) | lo);
        }
        Ok(out)
    }

    const fn nibble(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'a' + n - 10) as char,
            _ => '?',
        }
    }

    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
}

pub mod binary {
    //! Endianness helpers.

    /// Writes `value` big-endian into `out[..2]`. Panics if `out` is
    /// too small.
    pub fn put_u16_be(out: &mut [u8], value: u16) {
        out[..2].copy_from_slice(&value.to_be_bytes());
    }

    /// Writes `value` little-endian into `out[..2]`.
    pub fn put_u16_le(out: &mut [u8], value: u16) {
        out[..2].copy_from_slice(&value.to_le_bytes());
    }

    /// Reads a big-endian `u16` from `input[..2]`.
    #[must_use]
    pub fn get_u16_be(input: &[u8]) -> u16 {
        u16::from_be_bytes([input[0], input[1]])
    }

    /// Reads a little-endian `u16` from `input[..2]`.
    #[must_use]
    pub fn get_u16_le(input: &[u8]) -> u16 {
        u16::from_le_bytes([input[0], input[1]])
    }

    /// Writes `value` big-endian into `out[..4]`.
    pub fn put_u32_be(out: &mut [u8], value: u32) {
        out[..4].copy_from_slice(&value.to_be_bytes());
    }

    /// Writes `value` little-endian into `out[..4]`.
    pub fn put_u32_le(out: &mut [u8], value: u32) {
        out[..4].copy_from_slice(&value.to_le_bytes());
    }

    /// Reads a big-endian `u32` from `input[..4]`.
    #[must_use]
    pub fn get_u32_be(input: &[u8]) -> u32 {
        u32::from_be_bytes([input[0], input[1], input[2], input[3]])
    }

    /// Reads a little-endian `u32` from `input[..4]`.
    #[must_use]
    pub fn get_u32_le(input: &[u8]) -> u32 {
        u32::from_le_bytes([input[0], input[1], input[2], input[3]])
    }

    /// Writes `value` big-endian into `out[..8]`.
    pub fn put_u64_be(out: &mut [u8], value: u64) {
        out[..8].copy_from_slice(&value.to_be_bytes());
    }

    /// Reads a big-endian `u64` from `input[..8]`.
    #[must_use]
    pub fn get_u64_be(input: &[u8]) -> u64 {
        u64::from_be_bytes([
            input[0], input[1], input[2], input[3], input[4], input[5], input[6], input[7],
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_canonical_vectors() {
        let cases = [
            (b"".as_slice(), ""),
            (b"f".as_slice(), "Zg=="),
            (b"fo".as_slice(), "Zm8="),
            (b"foo".as_slice(), "Zm9v"),
            (b"foob".as_slice(), "Zm9vYg=="),
            (b"fooba".as_slice(), "Zm9vYmE="),
            (b"foobar".as_slice(), "Zm9vYmFy"),
        ];
        for (raw, encoded) in cases {
            assert_eq!(base64::encode(raw), encoded, "encode {raw:?}");
            assert_eq!(base64::decode(encoded).unwrap(), raw);
        }
    }

    #[test]
    fn hex_round_trips_canonical_vectors() {
        assert_eq!(hex::encode(b"abc"), "616263");
        assert_eq!(hex::decode("616263").unwrap(), b"abc");
        assert!(hex::decode("zzz").is_err());
    }

    #[test]
    fn binary_u32_round_trip() {
        let mut buf = [0u8; 4];
        binary::put_u32_be(&mut buf, 0xDEADBEEF);
        assert_eq!(binary::get_u32_be(&buf), 0xDEADBEEF);
        let mut buf = [0u8; 4];
        binary::put_u32_le(&mut buf, 0xCAFEBABE);
        assert_eq!(binary::get_u32_le(&buf), 0xCAFEBABE);
    }
}
