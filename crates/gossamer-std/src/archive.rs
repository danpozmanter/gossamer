//! Runtime support for `std::archive` - archive format readers and writers.

#![forbid(unsafe_code)]

/// Tar archive reader and writer.
pub mod tar;
/// ZIP archive reader and writer.
pub mod zip;
