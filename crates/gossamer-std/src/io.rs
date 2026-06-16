#![allow(
    clippy::similar_names,
    clippy::needless_lifetimes,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::missing_errors_doc,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::doc_markdown,
    clippy::let_and_return,
    clippy::items_after_statements,
    clippy::missing_const_for_fn,
    clippy::extra_unused_lifetimes,
    clippy::elidable_lifetime_names,
    clippy::must_use_candidate
)]

//! Runtime support for `std::io`.

#![forbid(unsafe_code)]

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
    /// The operation was cancelled by a `std::context::Context`.
    #[error("cancelled: {0}")]
    Cancelled(String),
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

    /// Constructs a cancellation error from a context error.
    #[must_use]
    pub fn cancelled(err: crate::errors::Error) -> Self {
        Self::Cancelled(err.message().to_string())
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

// --- Composition primitives (Go's io::Copy / io::Pipe family) ------

/// Copies bytes from `src` to `dst` until `src` reports EOF (a
/// zero-length read). Returns the total number of bytes copied.
///
/// Mirrors Go's `io.Copy`. The caller picks the underlying types;
/// any `Reader`/`Writer` pair works.
pub fn copy<R: Reader + ?Sized, W: Writer + ?Sized>(
    dst: &mut W,
    src: &mut R,
) -> Result<u64, IoError> {
    let mut buf = [0u8; 8192];
    let mut total: u64 = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
}

/// Copies at most `n` bytes from `src` to `dst`. Returns the
/// number of bytes actually copied; less than `n` only when
/// `src` reports EOF first.
pub fn copy_n<R: Reader + ?Sized, W: Writer + ?Sized>(
    dst: &mut W,
    src: &mut R,
    n: u64,
) -> Result<u64, IoError> {
    let mut buf = [0u8; 8192];
    let mut copied: u64 = 0;
    while copied < n {
        let want = std::cmp::min((n - copied) as usize, buf.len());
        let got = src.read(&mut buf[..want])?;
        if got == 0 {
            return Ok(copied);
        }
        dst.write_all(&buf[..got])?;
        copied += got as u64;
    }
    Ok(copied)
}

/// Reads every byte from `src` until EOF and returns the
/// accumulated buffer.
pub fn read_all<R: Reader + ?Sized>(src: &mut R) -> Result<Vec<u8>, IoError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// Reader that stops returning bytes after at most `limit` have
/// been delivered.
pub struct LimitReader<'a, R: Reader + ?Sized> {
    inner: &'a mut R,
    remaining: u64,
}

impl<'a, R: Reader + ?Sized> LimitReader<'a, R> {
    /// Wraps `inner`, returning at most `limit` bytes.
    pub fn new(inner: &'a mut R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<'a, R: Reader + ?Sized> Reader for LimitReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let want = std::cmp::min(buf.len() as u64, self.remaining) as usize;
        let n = self.inner.read(&mut buf[..want])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Reader that pipes its input through a tee - every byte read
/// is also written to `tee`.
pub struct TeeReader<'a, R: Reader + ?Sized, W: Writer + ?Sized> {
    inner: &'a mut R,
    tee: &'a mut W,
}

impl<'a, R: Reader + ?Sized, W: Writer + ?Sized> TeeReader<'a, R, W> {
    /// Wraps `inner`, mirroring every read into `tee`.
    pub fn new(inner: &'a mut R, tee: &'a mut W) -> Self {
        Self { inner, tee }
    }
}

impl<'a, R: Reader + ?Sized, W: Writer + ?Sized> Reader for TeeReader<'a, R, W> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.tee.write_all(&buf[..n])?;
        }
        Ok(n)
    }
}

/// Reader that concatenates a sequence of underlying readers,
/// reading from each in turn until it reports EOF.
pub struct MultiReader<'a> {
    readers: Vec<&'a mut dyn Reader>,
    cursor: usize,
}

impl<'a> MultiReader<'a> {
    /// Constructs a multi-reader over `readers`.
    pub fn new(readers: Vec<&'a mut dyn Reader>) -> Self {
        Self { readers, cursor: 0 }
    }
}

impl<'a> Reader for MultiReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        while self.cursor < self.readers.len() {
            let n = self.readers[self.cursor].read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            self.cursor += 1;
        }
        Ok(0)
    }
}

/// In-memory pipe - a paired reader / writer where bytes written
/// to the writer become available on the reader. Buffered;
/// readers see whatever the writer has flushed so far.
///
/// Designed for the goroutine-friendly case where a producer
/// writes and a consumer reads on separate threads.
///
/// Create a pair with [`pipe()`].
#[derive(Debug)]
pub struct Pipe(());

/// Builds a paired [`PipeReader`] / [`PipeWriter`].
#[must_use]
pub fn pipe() -> (PipeReader, PipeWriter) {
    let buf = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    (
        PipeReader {
            buf: std::sync::Arc::clone(&buf),
            closed: std::sync::Arc::clone(&closed),
        },
        PipeWriter { buf, closed },
    )
}

/// Reader half of a [`Pipe`].
pub struct PipeReader {
    buf: std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Writer half of a [`Pipe`].
pub struct PipeWriter {
    buf: std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Reader for PipeReader {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize, IoError> {
        loop {
            let mut g = self.buf.lock();
            if !g.is_empty() {
                let n = std::cmp::min(g.len(), dst.len());
                dst[..n].copy_from_slice(&g[..n]);
                g.drain(..n);
                return Ok(n);
            }
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(0);
            }
            drop(g);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

impl Writer for PipeWriter {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), IoError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(IoError::Other("pipe closed".into()));
        }
        self.buf.lock().extend_from_slice(buf);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}

impl PipeWriter {
    /// Signals end-of-stream to the reader.
    pub fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod io_compose_tests {
    use super::*;

    #[test]
    fn copy_drains_reader_into_writer() {
        let mut src = InMemoryReader::new(b"hello world".to_vec());
        let mut dst = InMemoryWriter::default();
        let n = copy(&mut dst, &mut src).unwrap();
        assert_eq!(n, 11);
        assert_eq!(dst.buffer, b"hello world");
    }

    #[test]
    fn copy_n_caps_at_n() {
        let mut src = InMemoryReader::new(b"hello world".to_vec());
        let mut dst = InMemoryWriter::default();
        let n = copy_n(&mut dst, &mut src, 5).unwrap();
        assert_eq!(n, 5);
        assert_eq!(dst.buffer, b"hello");
    }

    #[test]
    fn copy_n_stops_at_eof_below_n() {
        let mut src = InMemoryReader::new(b"hi".to_vec());
        let mut dst = InMemoryWriter::default();
        let n = copy_n(&mut dst, &mut src, 100).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn read_all_collects_full_buffer() {
        let mut src = InMemoryReader::new(b"abcdefgh".to_vec());
        let got = read_all(&mut src).unwrap();
        assert_eq!(got, b"abcdefgh");
    }

    #[test]
    fn limit_reader_caps_total_bytes() {
        let mut src = InMemoryReader::new(b"abcdef".to_vec());
        let mut limited = LimitReader::new(&mut src, 3);
        let got = read_all(&mut limited).unwrap();
        assert_eq!(got, b"abc");
    }

    #[test]
    fn tee_reader_mirrors_to_writer() {
        let mut src = InMemoryReader::new(b"payload".to_vec());
        let mut mirror = InMemoryWriter::default();
        let mut tee = TeeReader::new(&mut src, &mut mirror);
        let got = read_all(&mut tee).unwrap();
        assert_eq!(got, b"payload");
        assert_eq!(mirror.buffer, b"payload");
    }

    #[test]
    fn multi_reader_chains_inputs() {
        let mut a = InMemoryReader::new(b"foo".to_vec());
        let mut b = InMemoryReader::new(b"-bar".to_vec());
        let mut multi = MultiReader::new(vec![&mut a, &mut b]);
        let got = read_all(&mut multi).unwrap();
        assert_eq!(got, b"foo-bar");
    }

    #[test]
    fn pipe_round_trips_across_threads() {
        let (mut r, mut w) = pipe();
        let producer = std::thread::spawn(move || {
            w.write_all(b"hello").unwrap();
            w.write_all(b" world").unwrap();
            w.close();
        });
        let got = read_all(&mut r).unwrap();
        producer.join().unwrap();
        assert_eq!(got, b"hello world");
    }

    #[test]
    fn pipe_writer_close_signals_eof() {
        let (mut r, w) = pipe();
        w.close();
        drop(w);
        let mut buf = [0u8; 4];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn copy_returns_total_for_chunked_reads() {
        // Generate a 20 KiB stream so the 8 KiB internal buffer
        // cycles a few times.
        let payload: Vec<u8> = (0..20_480).map(|i| (i & 0xff) as u8).collect();
        let mut src = InMemoryReader::new(payload.clone());
        let mut dst = InMemoryWriter::default();
        let n = copy(&mut dst, &mut src).unwrap();
        assert_eq!(n, 20_480);
        assert_eq!(dst.buffer, payload);
    }
}
