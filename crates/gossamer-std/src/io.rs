//! Runtime support for `std::io`.

#![forbid(unsafe_code)]
#![allow(clippy::needless_pass_by_value)]

use thiserror::Error;

/// Common errors surfaced by stdlib I/O operations.
#[derive(Debug, Error)]
pub enum IoError {
    /// The requested resource was not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// The caller did not have permission to perform the operation.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// An I/O operation failed at the OS layer.
    #[error("io: {0}")]
    Other(String),
}

impl IoError {
    /// Adapter that classifies a [`std::io::Error`] into our coarser
    /// error enum, attaching `context` for diagnostics.
    #[must_use]
    pub fn from_std(err: std::io::Error, context: &str) -> Self {
        use std::io::ErrorKind;
        match err.kind() {
            ErrorKind::NotFound => Self::NotFound(context.to_string()),
            ErrorKind::PermissionDenied => Self::PermissionDenied(context.to_string()),
            _ => Self::Other(format!("{context}: {err}")),
        }
    }
}

/// Convenience trait alias for the `Reader` interface presented to
/// Gossamer programs. The runtime wraps types implementing this with
/// the user-facing `Reader` GC type.
pub trait Reader {
    /// Reads up to `buf.len()` bytes into `buf` and returns the count.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError>;
}

/// Sink counterpart to [`Reader`].
pub trait Writer {
    /// Writes every byte in `buf` to the sink.
    fn write_all(&mut self, buf: &[u8]) -> Result<(), IoError>;
    /// Flushes any buffered bytes downstream.
    fn flush(&mut self) -> Result<(), IoError>;
}

/// In-memory sink used by tests and by the interpreter when a program
/// does not have a real OS stream available.
#[derive(Debug, Default)]
pub struct InMemoryWriter {
    /// Accumulated bytes.
    pub buffer: Vec<u8>,
}

impl Writer for InMemoryWriter {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), IoError> {
        self.buffer.extend_from_slice(buf);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}

/// In-memory source mirror of [`InMemoryWriter`].
#[derive(Debug, Default)]
pub struct InMemoryReader {
    /// Backing bytes.
    pub buffer: Vec<u8>,
    /// Read cursor.
    pub cursor: usize,
}

impl InMemoryReader {
    /// Constructs a reader wrapping `bytes`.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            buffer: bytes,
            cursor: 0,
        }
    }
}

impl Reader for InMemoryReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let remaining = &self.buffer[self.cursor..];
        let n = remaining.len().min(buf.len());
        buf[..n].copy_from_slice(&remaining[..n]);
        self.cursor += n;
        Ok(n)
    }
}
