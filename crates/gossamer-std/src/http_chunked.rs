//! HTTP/1.1 chunked transfer-encoding (RFC 7230 §4.1).
//!
//! Provides [`ChunkedReader`] for decoding inbound bodies and
//! [`ChunkedWriter`] for encoding outbound streams. Both honour
//! the spec strictly: hex-encoded chunk sizes, optional
//! chunk-extensions (ignored), CRLF-terminated framing, trailer
//! parsing, and a final zero-length chunk.
//!
//! The reader is a `Read` adapter — callers consume the decoded
//! payload bytes; the framing is invisible. The writer is a
//! `Write` adapter — callers write payload bytes; the framing is
//! emitted automatically. `finish()` flushes the terminating
//! zero-chunk and optional trailers.

use std::io::{self, BufRead, Read, Write};

/// Streaming decoder for `Transfer-Encoding: chunked` request /
/// response bodies. Implements `Read`; consumers see only the
/// decoded payload bytes.
///
/// State machine:
///
/// - `ReadSize`     — expect a hex size line.
/// - `ReadData(n)`  — `n` payload bytes still to read for the
///   current chunk.
/// - `ReadDataCrlf` — payload done; consume trailing `\r\n`.
/// - `ReadTrailers` — final zero-chunk seen; consume any trailer
///   headers up to the blank line.
/// - `Done`         — terminal; future reads return EOF.
///
/// Errors:
///
/// - Malformed size lines (non-hex, missing CRLF, oversize)
///   return `InvalidData`.
/// - Premature EOF mid-chunk returns `UnexpectedEof`.
pub struct ChunkedReader<R: BufRead> {
    inner: R,
    state: State,
    /// Captured trailer headers after the final zero-chunk.
    pub trailers: Vec<(String, String)>,
    /// Maximum hex size accepted on the size line (defensive).
    /// 16 hex digits = 2^64 bytes, far past the body cap; any
    /// value above this is treated as malformed.
    pub max_size_digits: usize,
}

#[derive(Debug)]
enum State {
    ReadSize,
    ReadData(u64),
    ReadDataCrlf,
    ReadTrailers,
    Done,
}

impl<R: BufRead> ChunkedReader<R> {
    /// Wraps `inner` with the chunked decoder.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            state: State::ReadSize,
            trailers: Vec::new(),
            max_size_digits: 16,
        }
    }

    /// Unwraps the reader, returning the underlying reader. The
    /// decoder must have reached `Done` (i.e. read a full chunked
    /// body) for the underlying reader to be positioned at the
    /// next request on a keep-alive connection.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Returns `true` once the trailing zero-chunk + trailer
    /// block have been consumed.
    pub fn is_done(&self) -> bool {
        matches!(self.state, State::Done)
    }

    fn read_size_line(&mut self) -> io::Result<u64> {
        let mut line = String::new();
        let n = self.inner.read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "chunked: EOF before chunk-size",
            ));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        // Strip chunk-extensions (after `;`).
        let head = trimmed.split(';').next().unwrap_or(trimmed).trim();
        if head.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunked: empty size line",
            ));
        }
        if head.len() > self.max_size_digits {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("chunked: size line too long: {} chars", head.len()),
            ));
        }
        u64::from_str_radix(head, 16).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("chunked: bad hex size: {head:?}"),
            )
        })
    }

    fn read_crlf(&mut self) -> io::Result<()> {
        let mut buf = [0u8; 2];
        self.inner.read_exact(&mut buf)?;
        if &buf != b"\r\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("chunked: expected CRLF, got {buf:?}"),
            ));
        }
        Ok(())
    }

    fn read_trailers_block(&mut self) -> io::Result<()> {
        loop {
            let mut line = String::new();
            let n = self.inner.read_line(&mut line)?;
            if n == 0 {
                // No trailing CRLF — be permissive (some peers
                // send the final 0\r\n then close without the
                // empty line).
                return Ok(());
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                return Ok(());
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                self.trailers
                    .push((name.trim().to_string(), value.trim().to_string()));
            }
        }
    }
}

impl<R: BufRead> Read for ChunkedReader<R> {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        loop {
            match &mut self.state {
                State::ReadSize => {
                    let size = self.read_size_line()?;
                    if size == 0 {
                        self.state = State::ReadTrailers;
                    } else {
                        self.state = State::ReadData(size);
                    }
                }
                State::ReadData(remaining) => {
                    if dst.is_empty() {
                        return Ok(0);
                    }
                    let want = std::cmp::min(*remaining, dst.len() as u64) as usize;
                    let n = self.inner.read(&mut dst[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "chunked: EOF mid-chunk",
                        ));
                    }
                    *remaining -= n as u64;
                    if *remaining == 0 {
                        self.state = State::ReadDataCrlf;
                    }
                    return Ok(n);
                }
                State::ReadDataCrlf => {
                    self.read_crlf()?;
                    self.state = State::ReadSize;
                }
                State::ReadTrailers => {
                    self.read_trailers_block()?;
                    self.state = State::Done;
                }
                State::Done => return Ok(0),
            }
        }
    }
}

/// Streaming encoder for `Transfer-Encoding: chunked` outbound
/// bodies. Each `write` emits one chunk; callers should batch
/// writes if they care about per-chunk overhead. `finish()`
/// flushes the trailing zero-chunk plus an optional trailer
/// block.
///
/// Failure to call `finish()` before drop leaves the stream
/// without a terminator — peers will hang waiting for the
/// zero-chunk. The drop impl emits a best-effort terminator if
/// `finish()` was not called, but errors there are silently
/// dropped; production code MUST call `finish()` explicitly.
pub struct ChunkedWriter<W: Write> {
    inner: Option<W>,
    finished: bool,
}

impl<W: Write> ChunkedWriter<W> {
    /// Wraps `inner` with the chunked encoder.
    pub fn new(inner: W) -> Self {
        Self {
            inner: Some(inner),
            finished: false,
        }
    }

    /// Flushes the terminating zero-chunk and returns the
    /// underlying writer. After this call the writer is sealed.
    pub fn finish(mut self) -> io::Result<W> {
        self.finish_internal(&[])?;
        Ok(self.inner.take().expect("inner present"))
    }

    /// Same as [`Self::finish`] but writes the supplied trailer
    /// headers before the terminating CRLF. Each tuple is
    /// `(name, value)`; values are emitted verbatim and MUST NOT
    /// contain CR or LF.
    pub fn finish_with_trailers(mut self, trailers: &[(&str, &str)]) -> io::Result<W> {
        self.finish_internal(trailers)?;
        Ok(self.inner.take().expect("inner present"))
    }

    fn finish_internal(&mut self, trailers: &[(&str, &str)]) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let Some(w) = self.inner.as_mut() else {
            return Ok(());
        };
        w.write_all(b"0\r\n")?;
        for (name, value) in trailers {
            w.write_all(name.as_bytes())?;
            w.write_all(b": ")?;
            w.write_all(value.as_bytes())?;
            w.write_all(b"\r\n")?;
        }
        w.write_all(b"\r\n")?;
        w.flush()
    }
}

impl<W: Write> Write for ChunkedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.finished {
            return Err(io::Error::other("write after ChunkedWriter::finish"));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let w = self
            .inner
            .as_mut()
            .ok_or_else(|| io::Error::other("ChunkedWriter inner gone"))?;
        let header = format!("{:x}\r\n", buf.len());
        w.write_all(header.as_bytes())?;
        w.write_all(buf)?;
        w.write_all(b"\r\n")?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.inner.as_mut() {
            Some(w) => w.flush(),
            None => Ok(()),
        }
    }
}

/// One-shot encode helper: wraps `payload` in chunked
/// transfer-encoding bytes. Emits exactly one data chunk plus
/// the terminating zero-chunk, suitable for `Transfer-Encoding:
/// chunked` HTTP/1.1 bodies. The Gossamer surface bridges to
/// this rather than to the streaming `ChunkedWriter` because
/// the Gossamer language doesn't expose `Write` traits.
#[must_use]
pub fn encode_one(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 32);
    let header = format!("{:x}\r\n", payload.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\r\n0\r\n\r\n");
    out
}

/// One-shot decode helper: parses a complete chunked body
/// (including the terminating zero-chunk) and returns the
/// concatenated payload bytes. Trailers, if any, are discarded
/// — callers needing them should use `ChunkedReader` directly.
pub fn decode_all(payload: &[u8]) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let cursor = std::io::BufReader::new(payload);
    let mut reader = ChunkedReader::new(cursor);
    let mut out = Vec::with_capacity(payload.len());
    reader.read_to_end(&mut out)?;
    Ok(out)
}

impl<W: Write> Drop for ChunkedWriter<W> {
    fn drop(&mut self) {
        if !self.finished {
            // Best-effort terminator on drop. Errors swallowed —
            // production paths must call `finish()` explicitly.
            let _ = self.finish_internal(&[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn writer_emits_single_chunk_plus_terminator() {
        let mut buf = Vec::new();
        {
            let mut w = ChunkedWriter::new(&mut buf);
            w.write_all(b"hello").unwrap();
            w.finish().unwrap();
        }
        assert_eq!(buf, b"5\r\nhello\r\n0\r\n\r\n");
    }

    #[test]
    fn writer_emits_multi_chunk() {
        let mut buf = Vec::new();
        let mut w = ChunkedWriter::new(&mut buf);
        w.write_all(b"hello").unwrap();
        w.write_all(b" ").unwrap();
        w.write_all(b"world!").unwrap();
        w.finish().unwrap();
        assert_eq!(
            buf,
            b"5\r\nhello\r\n1\r\n \r\n6\r\nworld!\r\n0\r\n\r\n".to_vec()
        );
    }

    #[test]
    fn writer_emits_trailer_block() {
        let mut buf = Vec::new();
        let mut w = ChunkedWriter::new(&mut buf);
        w.write_all(b"payload").unwrap();
        w.finish_with_trailers(&[("Trailer-One", "alpha"), ("Trailer-Two", "beta")])
            .unwrap();
        let expected = b"7\r\npayload\r\n0\r\nTrailer-One: alpha\r\nTrailer-Two: beta\r\n\r\n";
        assert_eq!(buf, expected);
    }

    #[test]
    fn writer_drop_emits_terminator() {
        let mut buf = Vec::new();
        {
            let mut w = ChunkedWriter::new(&mut buf);
            w.write_all(b"abc").unwrap();
            // No explicit finish — drop terminator kicks in.
        }
        // Should end with the zero-chunk + blank line.
        assert!(buf.ends_with(b"0\r\n\r\n"), "got: {buf:?}");
    }

    #[test]
    fn reader_consumes_single_chunk() {
        let body = b"5\r\nhello\r\n0\r\n\r\n";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello");
        assert!(r.is_done());
    }

    #[test]
    fn reader_consumes_multi_chunk() {
        let body = b"5\r\nhello\r\n1\r\n \r\n6\r\nworld!\r\n0\r\n\r\n";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello world!");
    }

    #[test]
    fn reader_captures_trailers() {
        let body = b"3\r\nabc\r\n0\r\nFoo: bar\r\nBaz: qux\r\n\r\n";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "abc");
        assert_eq!(r.trailers.len(), 2);
        assert_eq!(r.trailers[0], ("Foo".to_string(), "bar".to_string()));
        assert_eq!(r.trailers[1], ("Baz".to_string(), "qux".to_string()));
    }

    #[test]
    fn reader_skips_chunk_extensions() {
        let body = b"5;name=value\r\nhello\r\n0\r\n\r\n";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn reader_round_trips_writer_output() {
        // Producer.
        let mut wire = Vec::new();
        let mut w = ChunkedWriter::new(&mut wire);
        for chunk in &[b"alpha".as_ref(), b"beta", b"gamma", b"delta"] {
            w.write_all(chunk).unwrap();
        }
        w.finish().unwrap();
        // Consumer.
        let mut r = ChunkedReader::new(Cursor::new(&wire));
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"alphabetagammadelta");
        assert!(r.is_done());
    }

    #[test]
    fn reader_rejects_bad_hex_size() {
        let body = b"xyz\r\nhello\r\n0\r\n\r\n";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn reader_rejects_premature_eof_mid_chunk() {
        let body = b"5\r\nhel";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn reader_rejects_missing_crlf_after_data() {
        // 5 bytes payload "hello" but missing trailing CRLF.
        let body = b"5\r\nhello0\r\n\r\n";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn reader_rejects_oversize_hex() {
        // 17 hex chars exceeds the 16-digit guard.
        let body = b"11111111111111111\r\n...";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn reader_handles_zero_chunk_without_trailer_block_blank_line() {
        // Some peers terminate `0\r\n` without the final empty
        // line — be permissive.
        let body = b"3\r\nabc\r\n0\r\n";
        let mut r = ChunkedReader::new(Cursor::new(body));
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "abc");
    }

    #[test]
    fn fuzz_writer_round_trips_random_payloads() {
        // Deterministic SplitMix64 seed; covers a spread of
        // payload sizes including 0, 1, alignment boundaries,
        // page-sized.
        let sizes = [0_usize, 1, 7, 8, 9, 31, 32, 33, 4095, 4096, 16_383, 65_535];
        let mut state: u64 = 0xDEADBEEF;
        for &size in &sizes {
            let mut payload = Vec::with_capacity(size);
            for _ in 0..size {
                state = state.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z ^= z >> 31;
                payload.push((z & 0xff) as u8);
            }
            let mut wire = Vec::new();
            let mut w = ChunkedWriter::new(&mut wire);
            if !payload.is_empty() {
                w.write_all(&payload).unwrap();
            }
            w.finish().unwrap();
            let mut r = ChunkedReader::new(Cursor::new(&wire));
            let mut decoded = Vec::new();
            r.read_to_end(&mut decoded).unwrap();
            assert_eq!(decoded, payload, "round-trip failed for size {size}");
        }
    }
}
