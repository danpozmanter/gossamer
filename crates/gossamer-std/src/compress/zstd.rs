// Runtime support for `std::compress::zstd` - Zstandard encoding/decoding.
//
// Wraps the `zstd` crate (libzstd C library, vendored) in the Gossamer
// error shape. The user surface mirrors the sibling gzip / flate / zlib
// modules: one-shot byte-in / byte-out entry points returning the
// standard `IoError`.

#![forbid(unsafe_code)]

use std::io::{Read, Write};

use crate::io::IoError;

/// Default compression level (matches the `zstd` CLI default).
const DEFAULT_LEVEL: i32 = 3;
/// Minimum supported compression level.
const MIN_LEVEL: i32 = 1;
/// Maximum supported compression level.
const MAX_LEVEL: i32 = 22;

/// Compresses `input` with Zstandard at the default level (3).
pub fn encode(input: &[u8]) -> Result<Vec<u8>, IoError> {
    encode_level(input, DEFAULT_LEVEL)
}

/// Compresses `input` with Zstandard at the given `level` (1..=22).
pub fn encode_level(input: &[u8], level: i32) -> Result<Vec<u8>, IoError> {
    if !(MIN_LEVEL..=MAX_LEVEL).contains(&level) {
        return Err(IoError::Other(format!(
            "zstd level out of range (expected {MIN_LEVEL}..={MAX_LEVEL}): {level}"
        )));
    }
    let mut enc = zstd::stream::Encoder::new(Vec::with_capacity(input.len()), level)
        .map_err(|e| IoError::Other(format!("zstd encoder init: {e}")))?;
    enc.write_all(input)
        .map_err(|e| IoError::Other(format!("zstd encode write: {e}")))?;
    enc.finish()
        .map_err(|e| IoError::Other(format!("zstd encode finish: {e}")))
}

/// Decompresses a Zstandard-encoded payload.
pub fn decode(input: &[u8]) -> Result<Vec<u8>, IoError> {
    let mut dec = zstd::stream::Decoder::new(input)
        .map_err(|e| IoError::Other(format!("zstd decoder init: {e}")))?;
    let mut out = Vec::with_capacity(input.len() * 3);
    dec.read_to_end(&mut out)
        .map_err(|e| IoError::Other(format!("zstd decode: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_non_trivial_payload() {
        let plain: Vec<u8> = (0..2048u32).flat_map(u32::to_le_bytes).collect();
        let cipher = encode(&plain).unwrap();
        assert_ne!(cipher, plain);
        // Zstandard frame magic: 0x28 0xB5 0x2F 0xFD (little-endian).
        assert_eq!(cipher[0..4], [0x28, 0xB5, 0x2F, 0xFD]);
        let back = decode(&cipher).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn decode_rejects_garbage_input() {
        let result = decode(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05]);
        assert!(result.is_err(), "expected error from non-zstd input");
    }

    #[test]
    fn compression_actually_compresses_repetitive_input() {
        let plain: Vec<u8> = b"abcdefghij".repeat(1024);
        assert_eq!(plain.len(), 10_240);
        let cipher = encode(&plain).unwrap();
        assert!(
            cipher.len() < plain.len(),
            "expected encoded length < input length (got {} >= {})",
            cipher.len(),
            plain.len()
        );
        let back = decode(&cipher).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn encode_level_respects_bounds() {
        let plain = b"hello, zstd";
        assert!(encode_level(plain, 0).is_err());
        assert!(encode_level(plain, 23).is_err());
        let cipher = encode_level(plain, MAX_LEVEL).unwrap();
        assert_eq!(decode(&cipher).unwrap(), plain);
    }

    #[test]
    fn empty_input_round_trips() {
        let plain: &[u8] = b"";
        let cipher = encode(plain).unwrap();
        let back = decode(&cipher).unwrap();
        assert_eq!(back, plain);
    }
}
