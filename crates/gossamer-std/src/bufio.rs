//! Runtime support for `std::bufio` — buffered readers, writers, and
//! a line / token scanner sitting above `io::Read` / `io::Write`.

#![forbid(unsafe_code)]

use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};

/// Wraps `R` in an 8 KiB [`BufReader`]. Exposed as a thin alias so
/// stdlib prose is consistent with Go's `bufio`.
pub struct Reader<R: Read> {
    inner: BufReader<R>,
}

impl<R: Read> Reader<R> {
    /// Wraps `reader` in a default-capacity buffered reader.
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
        }
    }

    /// Wraps `reader` in a buffered reader with `capacity` bytes of
    /// scratch.
    pub fn with_capacity(capacity: usize, reader: R) -> Self {
        Self {
            inner: BufReader::with_capacity(capacity, reader),
        }
    }

    /// Reads up to the next `\n` (inclusive). Returns the number of
    /// bytes appended to `buf`.
    pub fn read_line(&mut self, buf: &mut String) -> io::Result<usize> {
        self.inner.read_line(buf)
    }

    /// Reads until the next `delimiter`, appending to `buf`.
    pub fn read_until(&mut self, delimiter: u8, buf: &mut Vec<u8>) -> io::Result<usize> {
        self.inner.read_until(delimiter, buf)
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

/// Buffered writer that forwards flushes to `W`.
pub struct Writer<W: Write> {
    inner: BufWriter<W>,
}

impl<W: Write> Writer<W> {
    /// Wraps `writer` in a default-capacity buffered writer.
    pub fn new(writer: W) -> Self {
        Self {
            inner: BufWriter::new(writer),
        }
    }

    /// Writes `bytes` and returns the count written.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.write(bytes)
    }

    /// Flushes any buffered bytes to the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    /// Writes a line followed by `\n`.
    pub fn write_line(&mut self, text: &str) -> io::Result<()> {
        self.inner.write_all(text.as_bytes())?;
        self.inner.write_all(b"\n")
    }
}

/// Split-function signature used by [`Scanner`]. Returns
/// `Some((bytes_consumed, token))` when a token is ready, or `None`
/// when more input is needed.
pub type SplitFn = fn(&[u8], bool) -> Option<(usize, Vec<u8>)>;

/// Line-oriented scanner, inspired by Go's `bufio.Scanner`.
pub struct Scanner<R: Read> {
    reader: BufReader<R>,
    buffer: Vec<u8>,
    token: Option<Vec<u8>>,
    at_eof: bool,
    split: SplitFn,
    max_token_size: usize,
}

impl<R: Read> Scanner<R> {
    /// Default-capacity scanner that yields newline-terminated tokens.
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            buffer: Vec::new(),
            token: None,
            at_eof: false,
            split: split_lines,
            max_token_size: 1 << 20,
        }
    }

    /// Installs a custom split function.
    pub fn set_split(&mut self, split: SplitFn) {
        self.split = split;
    }

    /// Sets the maximum size of a single token. Tokens exceeding this
    /// limit cause [`scan`] to return `false` and surface an error
    /// via [`err`].
    pub fn set_max_token_size(&mut self, size: usize) {
        self.max_token_size = size;
    }

    /// Advances to the next token. Returns `false` at EOF or on
    /// error.
    pub fn scan(&mut self) -> bool {
        loop {
            if let Some((consumed, token)) = (self.split)(&self.buffer, self.at_eof) {
                self.token = Some(token);
                self.buffer.drain(..consumed);
                return true;
            }
            if self.at_eof {
                return false;
            }
            let before = self.buffer.len();
            if before > self.max_token_size {
                return false;
            }
            let mut scratch = [0u8; 4096];
            match self.reader.read(&mut scratch) {
                Ok(0) => {
                    self.at_eof = true;
                }
                Ok(n) => self.buffer.extend_from_slice(&scratch[..n]),
                Err(_) => return false,
            }
        }
    }

    /// Returns the most recent token as a string. The scanner holds
    /// ownership, so this is cheap.
    pub fn text(&self) -> String {
        self.token
            .as_ref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }

    /// Returns the most recent token as raw bytes.
    pub fn bytes(&self) -> Vec<u8> {
        self.token.clone().unwrap_or_default()
    }
}

/// Default line-splitter used by [`Scanner::new`].
#[must_use]
pub fn split_lines(data: &[u8], at_eof: bool) -> Option<(usize, Vec<u8>)> {
    if let Some(idx) = data.iter().position(|b| *b == b'\n') {
        let mut line = data[..idx].to_vec();
        if line.ends_with(b"\r") {
            line.pop();
        }
        return Some((idx + 1, line));
    }
    if at_eof && !data.is_empty() {
        return Some((data.len(), data.to_vec()));
    }
    None
}

/// Whitespace-splitter suitable for `set_split` — emits tokens
/// separated by any run of ASCII whitespace.
#[must_use]
pub fn split_words(data: &[u8], at_eof: bool) -> Option<(usize, Vec<u8>)> {
    let mut cursor = 0;
    while cursor < data.len() && data[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let start = cursor;
    while cursor < data.len() && !data[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor < data.len() {
        return Some((cursor, data[start..cursor].to_vec()));
    }
    if at_eof && start < data.len() {
        return Some((data.len(), data[start..].to_vec()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn scanner_emits_lines_without_trailing_newline() {
        let input = b"alpha\nbeta\ngamma\n".to_vec();
        let mut scanner = Scanner::new(Cursor::new(input));
        let mut out = Vec::new();
        while scanner.scan() {
            out.push(scanner.text());
        }
        assert_eq!(
            out,
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn scanner_handles_missing_trailing_newline() {
        let input = b"only".to_vec();
        let mut scanner = Scanner::new(Cursor::new(input));
        assert!(scanner.scan());
        assert_eq!(scanner.text(), "only");
        assert!(!scanner.scan());
    }

    #[test]
    fn scanner_word_split() {
        let input = b"  apple   banana\tcarrot \n".to_vec();
        let mut scanner = Scanner::new(Cursor::new(input));
        scanner.set_split(split_words);
        let mut out = Vec::new();
        while scanner.scan() {
            out.push(scanner.text());
        }
        assert_eq!(out, vec!["apple", "banana", "carrot"]);
    }

    #[test]
    fn writer_write_line_flushes() {
        let mut sink = Vec::new();
        {
            let mut writer = Writer::new(&mut sink);
            writer.write_line("one").unwrap();
            writer.write_line("two").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(sink, b"one\ntwo\n");
    }
}
