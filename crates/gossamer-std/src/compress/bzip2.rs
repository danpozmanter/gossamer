// Runtime support for `std::compress::bzip2` - bzip2 compress/decompress.

#![forbid(unsafe_code)]

use bzip2::Compression;
use bzip2::read::{BzDecoder, BzEncoder};

use crate::io::IoError;

/// Compresses `data` with bzip2 at the given level (0-9; 0 = fastest, 9 = best).
pub fn compress(data: &[u8], level: u32) -> Result<Vec<u8>, IoError> {
    use std::io::Read as _;
    let level = Compression::new(level.clamp(0, 9));
    let mut enc = BzEncoder::new(data, level);
    let mut out = Vec::new();
    enc.read_to_end(&mut out)
        .map_err(|e| IoError::from_std(e, "bzip2::compress"))?;
    Ok(out)
}

/// Decompresses bzip2 `data`.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, IoError> {
    use std::io::Read as _;
    let mut dec = BzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| IoError::from_std(e, "bzip2::decompress"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_compresses_and_restores() {
        let data = b"hello, gossamer lang! hello, gossamer lang!";
        let compressed = compress(data, 6).unwrap();
        assert!(compressed.len() < data.len() + 20);
        let restored = decompress(&compressed).unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn empty_input_round_trips() {
        let compressed = compress(b"", 6).unwrap();
        let restored = decompress(&compressed).unwrap();
        assert_eq!(restored, b"");
    }
}
