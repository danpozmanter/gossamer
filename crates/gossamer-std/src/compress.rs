//! Runtime support for `std::compress` - compression and decompression codecs.

#![forbid(unsafe_code)]

/// Bzip2 encoder and decoder.
pub mod bzip2;
/// Raw DEFLATE (RFC 1951) encoder and decoder.
pub mod flate;
/// Gzip (RFC 1952) encoder and decoder.
pub mod gzip;
/// Zlib (RFC 1950) encoder and decoder.
pub mod zlib;
/// Zstandard encoder and decoder (zstd crate, vendored libzstd).
pub mod zstd;
