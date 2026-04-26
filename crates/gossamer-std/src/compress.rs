//! Runtime support for `std::compress::gzip`.
//!
//! Wraps the `flate2` crate's gzip encoder/decoder in the Gossamer
//! `IoError` shape. The user-facing surface is two builders:
//!
//! - `gzip::Encoder::new(level)` returns an encoder; call
//!   `encoder.encode(bytes)` to produce the compressed payload, or
//!   `encoder.write(stream, bytes)` to drain into a writer.
//! - `gzip::Decoder::new()` returns a decoder; call
//!   `decoder.decode(bytes)` to expand a payload, or
//!   `decoder.read_all(stream)` to drain a reader.
//!
//! Compression levels: `0` (none) → `9` (best). `Default` (`6`) is
//! the recommended general-purpose tradeoff and matches `gzip(1)`'s
//! default.

#![forbid(unsafe_code)]

pub mod gzip;
