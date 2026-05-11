// Runtime support for `std::compress::flate` — raw DEFLATE encoding/decoding.
//
// Uses flate2 (pure-Rust miniz_oxide backend). Go's `compress/flate` package
// is mirrored here with Gossamer's error shape. Raw DEFLATE is the building
// block for gzip, zlib, and zip.

#![forbid(unsafe_code)]

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;

use crate::io::IoError;

/// Compresses `input` using raw DEFLATE at the given `level` (0–9).
/// `level = 0` is store-only (no compression); `level = 9` is maximum.
pub fn compress(input: &[u8], level: u32) -> Result<Vec<u8>, IoError> {
    let level = level.clamp(0, 9);
    let mut enc = DeflateEncoder::new(Vec::with_capacity(input.len()), Compression::new(level));
    enc.write_all(input)
        .map_err(|e| IoError::Other(e.to_string()))?;
    enc.finish().map_err(|e| IoError::Other(e.to_string()))
}

/// Decompresses raw DEFLATE-encoded `input`.
pub fn decompress(input: &[u8]) -> Result<Vec<u8>, IoError> {
    let mut dec = DeflateDecoder::new(input);
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
}
