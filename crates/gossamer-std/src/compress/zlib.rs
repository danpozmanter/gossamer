// Runtime support for `std::compress::zlib` — zlib (RFC 1950) encoding/decoding.
//
// Uses flate2 (pure-Rust miniz_oxide backend). The zlib format wraps raw DEFLATE
// with a two-byte header and an Adler-32 checksum trailer.

#![forbid(unsafe_code)]

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::io::IoError;

/// Compresses `input` using zlib at the given `level` (0–9).
/// `level = 0` is store-only; `level = 9` is maximum.
pub fn compress(input: &[u8], level: u32) -> Result<Vec<u8>, IoError> {
    let level = level.clamp(0, 9);
    let mut enc = ZlibEncoder::new(Vec::with_capacity(input.len()), Compression::new(level));
    enc.write_all(input)
        .map_err(|e| IoError::Other(e.to_string()))?;
    enc.finish().map_err(|e| IoError::Other(e.to_string()))
}

/// Decompresses zlib-encoded `input`.
pub fn decompress(input: &[u8]) -> Result<Vec<u8>, IoError> {
    let mut dec = ZlibDecoder::new(input);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| IoError::Other(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_short_text() {
        let src = b"hello, world!";
        let compressed = compress(src, 6).unwrap();
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, src);
    }

    #[test]
    fn level_none_is_lossless() {
        let src = b"data data data";
        let compressed = compress(src, 0).unwrap();
        assert_eq!(decompress(&compressed).unwrap(), src);
    }

    #[test]
    fn level_9_is_lossless() {
        let src = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let compressed = compress(src, 9).unwrap();
        assert!(compressed.len() < src.len());
        assert_eq!(decompress(&compressed).unwrap(), src);
    }

    #[test]
    fn zlib_header_magic() {
        // zlib streams start with 0x78 (deflate, window bits=15)
        let compressed = compress(b"hello", 6).unwrap();
        assert_eq!(compressed[0], 0x78, "expected zlib magic byte");
    }
}
