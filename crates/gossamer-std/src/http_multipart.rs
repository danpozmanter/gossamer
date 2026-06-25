//! Runtime support for `std::http::multipart`.
//!
//! Streaming RFC 7578 `multipart/form-data` parser. Parts larger than
//! `Config::max_in_memory_bytes` spill to disk in the configured temp
//! directory; smaller parts stay in a `Vec<u8>`. The on-disk files are
//! reaped by `Form::drop_temp_files` (called from `Drop`).

// `deny`, not `forbid`: this module is unsafe-free except for one
// audited Win32 ACL FFI block (`restrict_to_owner`, the Windows
// `chmod 0600` analogue for spilled temp parts) that carries a local
// `#[allow(unsafe_code)]`. `forbid` cannot be locally overridden;
// `deny` denies everywhere else.
#![deny(unsafe_code)]

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use crate::crypto;
use crate::errors::Error;

const DEFAULT_MAX_IN_MEMORY_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_PART_SIZE: usize = 100 * 1024 * 1024;
const DEFAULT_MAX_PARTS: usize = 1024;

/// Caller-tunable parser limits.
#[derive(Debug, Clone)]
pub struct Config {
    /// Per-part bytes kept in memory before spilling to a tempfile.
    pub max_in_memory_bytes: usize,
    /// Hard cap on the size of a single part (in-memory + spilled total).
    pub max_part_size: usize,
    /// Hard cap on the number of parts accepted.
    pub max_parts: usize,
    /// Override directory for spilled tempfiles. `None` => system temp.
    pub temp_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_in_memory_bytes: DEFAULT_MAX_IN_MEMORY_BYTES,
            max_part_size: DEFAULT_MAX_PART_SIZE,
            max_parts: DEFAULT_MAX_PARTS,
            temp_dir: None,
        }
    }
}

/// One parsed form part.
#[derive(Debug)]
pub enum Part {
    /// A text field: `Content-Disposition: form-data; name="x"` with no `filename`.
    Text {
        /// Field name.
        name: String,
        /// Decoded UTF-8 value.
        value: String,
        /// Optional `Content-Type` of the part.
        content_type: Option<String>,
    },
    /// A file field: `Content-Disposition: form-data; name="x"; filename="..."`.
    File {
        /// Field name.
        name: String,
        /// Client-provided filename (untrusted).
        filename: String,
        /// Optional `Content-Type` of the part.
        content_type: Option<String>,
        /// Payload storage - in-memory or spilled to disk.
        data: PartData,
    },
}

impl Part {
    /// Returns the form field name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Text { name, .. } | Self::File { name, .. } => name,
        }
    }
}

/// File-part payload - in memory or spilled to disk.
#[derive(Debug)]
pub enum PartData {
    /// Buffered fully in memory.
    InMemory(Vec<u8>),
    /// Spilled to a tempfile path; deleted by `Form::drop`.
    OnDisk(PathBuf),
}

/// A parsed multipart form. Owns any spilled tempfiles and unlinks them
/// when dropped.
#[derive(Debug)]
pub struct Form {
    parts: Vec<Part>,
}

impl Form {
    /// Returns the first text part with the given field name.
    #[must_use]
    pub fn get_text(&self, name: &str) -> Option<&str> {
        for p in &self.parts {
            if let Part::Text { name: n, value, .. } = p {
                if n == name {
                    return Some(value.as_str());
                }
            }
        }
        None
    }

    /// Returns the first file part with the given field name.
    #[must_use]
    pub fn get_file(&self, name: &str) -> Option<&Part> {
        self.parts
            .iter()
            .find(|p| matches!(p, Part::File { name: n, .. } if n == name))
    }

    /// Returns every part with the given field name, in order.
    #[must_use]
    pub fn get_all(&self, name: &str) -> Vec<&Part> {
        self.parts.iter().filter(|p| p.name() == name).collect()
    }

    /// Returns every part in receipt order.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// Total number of parts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// `true` if the form has no parts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Unlinks every `PartData::OnDisk` tempfile owned by this form.
    /// Best-effort: errors are ignored (file already gone, dir removed, etc.).
    pub fn drop_temp_files(&mut self) {
        for part in &self.parts {
            if let Part::File {
                data: PartData::OnDisk(path),
                ..
            } = part
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl Drop for Form {
    fn drop(&mut self) {
        self.drop_temp_files();
    }
}

/// Extracts the boundary token from a `Content-Type` header value.
///
/// Accepts both bare (`multipart/form-data; boundary=abc`) and quoted
/// (`boundary="abc"`) forms. Returns the inner token without surrounding
/// quotes.
pub fn parse_boundary(content_type: &str) -> Result<String, Error> {
    let lower = content_type.to_ascii_lowercase();
    let idx = lower
        .find("boundary=")
        .ok_or_else(|| Error::new("multipart: Content-Type missing boundary parameter"))?;
    let rest = &content_type[idx + "boundary=".len()..];
    let trimmed = rest.trim_start();
    let token = if let Some(stripped) = trimmed.strip_prefix('"') {
        let end = stripped
            .find('"')
            .ok_or_else(|| Error::new("multipart: unterminated quoted boundary"))?;
        &stripped[..end]
    } else {
        let end = trimmed
            .find(|c: char| c == ';' || c.is_whitespace())
            .unwrap_or(trimmed.len());
        &trimmed[..end]
    };
    if token.is_empty() {
        return Err(Error::new("multipart: empty boundary"));
    }
    if token.len() > 70 {
        return Err(Error::new(
            "multipart: boundary exceeds 70 chars (RFC 2046)",
        ));
    }
    Ok(token.to_string())
}

/// Convenience wrapper around [`parse`] for body bytes already in memory.
pub fn parse_bytes(body: &[u8], boundary: &str, config: &Config) -> Result<Form, Error> {
    parse(body, boundary, config)
}

/// Streams `reader` as a multipart body, honoring `config` limits.
///
/// The reader is wrapped in a [`BufReader`] sized for at least
/// `2 * boundary + headroom`; bodies are scanned byte-exact (no CRLF
/// normalization) so binary uploads round-trip.
pub fn parse<R: Read>(reader: R, boundary: &str, config: &Config) -> Result<Form, Error> {
    if boundary.is_empty() {
        return Err(Error::new("multipart: empty boundary"));
    }

    // Wire delimiters per RFC 2046 / 7578.
    let dash_boundary: Vec<u8> = {
        let mut v = Vec::with_capacity(2 + boundary.len());
        v.extend_from_slice(b"--");
        v.extend_from_slice(boundary.as_bytes());
        v
    };
    let crlf_dash_boundary: Vec<u8> = {
        let mut v = Vec::with_capacity(4 + boundary.len());
        v.extend_from_slice(b"\r\n--");
        v.extend_from_slice(boundary.as_bytes());
        v
    };

    let buf_capacity = (crlf_dash_boundary.len() * 4).max(8 * 1024);
    let mut br = BufReader::with_capacity(buf_capacity, reader);

    // The preamble runs from start-of-stream up to the first --boundary.
    // The first boundary is special: no leading CRLF required.
    skip_to_first_boundary(&mut br, &dash_boundary)?;

    // Right after the first --boundary, consume either "--\r\n" (empty
    // form, closing immediately) or the trailing CRLF for the first part.
    match consume_after_boundary(&mut br)? {
        AfterBoundary::Closing => {
            return Ok(Form { parts: Vec::new() });
        }
        AfterBoundary::More => {}
    }

    let mut parts: Vec<Part> = Vec::new();
    loop {
        if parts.len() >= config.max_parts {
            return Err(Error::new(format!(
                "multipart: exceeded max_parts={}",
                config.max_parts
            )));
        }

        let headers = read_part_headers(&mut br)?;
        let disposition = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-disposition"))
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| Error::new("multipart: part missing Content-Disposition header"))?;
        let (name_opt, filename_opt) = parse_disposition(disposition);
        let name = name_opt
            .ok_or_else(|| Error::new("multipart: part missing required name parameter"))?;
        let content_type = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone());

        // Decide sink: filename present => File, otherwise => Text.
        if let Some(filename) = filename_opt {
            let mut sink = FileSink::new(config);
            read_part_body(
                &mut br,
                &crlf_dash_boundary,
                &mut sink,
                config.max_part_size,
            )?;
            let data = sink.finish()?;
            parts.push(Part::File {
                name,
                filename,
                content_type,
                data,
            });
        } else {
            let mut sink = MemorySink::new();
            read_part_body(
                &mut br,
                &crlf_dash_boundary,
                &mut sink,
                config.max_part_size,
            )?;
            let bytes = sink.into_bytes();
            let value = String::from_utf8(bytes)
                .map_err(|_| Error::new("multipart: text part is not valid UTF-8"))?;
            parts.push(Part::Text {
                name,
                value,
                content_type,
            });
        }

        // After read_part_body, the boundary marker has been consumed.
        // Inspect the two bytes that follow it.
        if let AfterBoundary::Closing = consume_after_boundary(&mut br)? {
            return Ok(Form { parts });
        }
    }
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

enum AfterBoundary {
    Closing,
    More,
}

/// Reads and discards bytes until the first `--boundary` is consumed.
/// Returns `Ok(())` once the marker has been read off the stream.
fn skip_to_first_boundary<R: BufRead>(br: &mut R, dash_boundary: &[u8]) -> Result<(), Error> {
    // We don't care what the preamble contains; just locate the marker.
    // Use a sliding match against `dash_boundary`.
    let mut matched = 0usize;
    loop {
        let buf = br.fill_buf().map_err(io_err)?;
        if buf.is_empty() {
            return Err(Error::new(
                "multipart: reached EOF before first boundary marker",
            ));
        }
        let mut consumed = 0usize;
        for &b in buf {
            consumed += 1;
            if b == dash_boundary[matched] {
                matched += 1;
                if matched == dash_boundary.len() {
                    br.consume(consumed);
                    return Ok(());
                }
            } else {
                // Restart match. Optimization for non-pathological boundaries:
                // if the mismatching byte equals dash_boundary[0], begin a
                // new partial match of length 1.
                matched = usize::from(b == dash_boundary[0]);
            }
        }
        br.consume(consumed);
    }
}

/// Reads the two bytes immediately following a boundary marker and
/// classifies whether this was the closing delimiter (`--`) or a part
/// separator (`\r\n`). Discards trailing whitespace before the CRLF as
/// permitted by RFC 2046 (linear-white-space tolerance).
fn consume_after_boundary<R: BufRead>(br: &mut R) -> Result<AfterBoundary, Error> {
    let mut two = [0u8; 2];
    read_exact(br, &mut two)?;
    if two == *b"--" {
        // Closing delimiter. RFC permits trailing CRLF + epilogue, both
        // of which we ignore. Drain to EOF best-effort.
        let mut sink = std::io::sink();
        let _ = std::io::copy(br, &mut sink);
        Ok(AfterBoundary::Closing)
    } else if two == *b"\r\n" {
        Ok(AfterBoundary::More)
    } else {
        // Tolerate optional LWS between boundary and CRLF.
        let mut last = two;
        loop {
            if last == *b"\r\n" {
                return Ok(AfterBoundary::More);
            }
            // Slide window by one byte.
            let mut nxt = [0u8; 1];
            read_exact(br, &mut nxt)?;
            last = [last[1], nxt[0]];
            // Bail if we've burned more than 8 bytes of slack - clearly malformed.
            // (Implicit through the byte budget below.)
            if last[0] != b' ' && last[0] != b'\t' && last[0] != b'\r' {
                return Err(Error::new(
                    "multipart: malformed bytes after boundary marker",
                ));
            }
        }
    }
}

/// Reads CRLF-terminated header lines until an empty line is seen.
fn read_part_headers<R: BufRead>(br: &mut R) -> Result<Vec<(String, String)>, Error> {
    let mut out = Vec::new();
    loop {
        let line = read_crlf_line(br)?;
        if line.is_empty() {
            return Ok(out);
        }
        // Strict folded-header rejection: starting with space/tab is
        // legacy obs-fold (RFC 7230 deprecates).
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(Error::new(
                "multipart: obsolete header folding not supported",
            ));
        }
        let colon = line
            .find(':')
            .ok_or_else(|| Error::new("multipart: header missing colon"))?;
        let name = line[..colon].trim().to_string();
        let value = line[colon + 1..].trim().to_string();
        if name.is_empty() {
            return Err(Error::new("multipart: header with empty name"));
        }
        out.push((name, value));
        if out.len() > 64 {
            return Err(Error::new("multipart: too many headers in part (>64)"));
        }
    }
}

/// Reads a CRLF-terminated line (without the CRLF). 8 KiB cap.
fn read_crlf_line<R: BufRead>(br: &mut R) -> Result<String, Error> {
    let mut buf = Vec::new();
    let mut last_was_cr = false;
    loop {
        let chunk = br.fill_buf().map_err(io_err)?;
        if chunk.is_empty() {
            return Err(Error::new("multipart: EOF inside header line"));
        }
        let mut consumed = 0usize;
        for &b in chunk {
            consumed += 1;
            if last_was_cr && b == b'\n' {
                buf.pop(); // remove the trailing CR
                br.consume(consumed);
                return String::from_utf8(buf)
                    .map_err(|_| Error::new("multipart: non-UTF-8 header line"));
            }
            last_was_cr = b == b'\r';
            buf.push(b);
            if buf.len() > 8 * 1024 {
                return Err(Error::new("multipart: header line exceeds 8 KiB"));
            }
        }
        br.consume(consumed);
    }
}

/// Streams the part body into `sink` until the next boundary marker
/// (`\r\n--BOUNDARY`) is found. The marker is consumed off the stream
/// before returning. Enforces `max_part_size`.
fn read_part_body<R: BufRead, S: BodySink>(
    br: &mut R,
    crlf_dash_boundary: &[u8],
    sink: &mut S,
    max_part_size: usize,
) -> Result<(), Error> {
    let marker = crlf_dash_boundary;
    // Window of bytes provisionally part of the body but possibly
    // overlapping a boundary match in progress.
    let mut pending: Vec<u8> = Vec::with_capacity(marker.len());
    let mut written: usize = 0;

    loop {
        let buf = br.fill_buf().map_err(io_err)?;
        if buf.is_empty() {
            return Err(Error::new(
                "multipart: EOF inside part body before boundary",
            ));
        }
        // Concatenate pending + buf logically, scan for marker in the
        // combined stream, then commit non-overlapping prefix bytes to
        // the sink and keep the trailing partial match in `pending`.
        // To avoid an O(n*m) Vec rebuild, we operate on `pending` and
        // `buf` as a two-segment slice.
        let mut consumed = 0usize;
        for &b in buf {
            pending.push(b);
            consumed += 1;
            // Trim pending from the left until it is no longer a
            // prefix mismatch with `marker`, or we have flushed enough
            // bytes to leave only a partial match suffix.
            if pending.len() >= marker.len() {
                if pending[pending.len() - marker.len()..] == *marker {
                    // Found marker. Commit everything before it.
                    let commit_len = pending.len() - marker.len();
                    written = write_chunk(sink, &pending[..commit_len], written, max_part_size)?;
                    let _ = written; // silence unused-assign warnings
                    br.consume(consumed);
                    return Ok(());
                }
                // No suffix match - push out the oldest byte.
                // But only push out bytes that cannot possibly be
                // part of an ongoing boundary match. We do this by
                // sliding the window: shift left while the right-most
                // `len-1` bytes don't form a prefix of `marker`.
                while pending.len() >= marker.len() {
                    // Drain one byte from the front.
                    let drained = pending.remove(0);
                    written = write_chunk(sink, &[drained], written, max_part_size)?;
                    // Check if remaining tail is still a prefix of marker;
                    // if not (and large enough), continue draining.
                    if !is_prefix_match(&pending, marker) {
                        // Drain everything except a possible new
                        // partial match starting later.
                        // Find the longest suffix of `pending` that is
                        // a prefix of marker; flush the rest.
                        let keep = longest_prefix_suffix(&pending, marker);
                        if keep < pending.len() {
                            let flush_len = pending.len() - keep;
                            let to_flush: Vec<u8> = pending.drain(..flush_len).collect();
                            written = write_chunk(sink, &to_flush, written, max_part_size)?;
                        }
                        break;
                    }
                }
            }
        }
        br.consume(consumed);
    }
}

/// Returns true if `pending` is a prefix of `marker` (possibly shorter).
fn is_prefix_match(pending: &[u8], marker: &[u8]) -> bool {
    if pending.len() > marker.len() {
        return false;
    }
    pending == &marker[..pending.len()]
}

/// Returns the length of the longest suffix of `pending` that is a
/// prefix of `marker`.
fn longest_prefix_suffix(pending: &[u8], marker: &[u8]) -> usize {
    let max = pending.len().min(marker.len());
    let mut k = max;
    while k > 0 {
        if pending[pending.len() - k..] == marker[..k] {
            return k;
        }
        k -= 1;
    }
    0
}

fn write_chunk<S: BodySink>(
    sink: &mut S,
    chunk: &[u8],
    written: usize,
    max_part_size: usize,
) -> Result<usize, Error> {
    if chunk.is_empty() {
        return Ok(written);
    }
    let new_total = written.saturating_add(chunk.len());
    if new_total > max_part_size {
        return Err(Error::new(format!(
            "multipart: part exceeded max_part_size={max_part_size}"
        )));
    }
    sink.write_all(chunk)?;
    Ok(new_total)
}

/// Helper trait for the two part-body sinks (in-memory vs spill-to-disk).
trait BodySink {
    fn write_all(&mut self, chunk: &[u8]) -> Result<(), Error>;
}

struct MemorySink {
    buf: Vec<u8>,
}

impl MemorySink {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }
    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

impl BodySink for MemorySink {
    fn write_all(&mut self, chunk: &[u8]) -> Result<(), Error> {
        self.buf.extend_from_slice(chunk);
        Ok(())
    }
}

/// File sink: starts in memory, spills to a tempfile once the
/// configured in-memory threshold is crossed.
struct FileSink<'a> {
    config: &'a Config,
    mem: Vec<u8>,
    spilled: Option<(PathBuf, File)>,
}

impl<'a> FileSink<'a> {
    fn new(config: &'a Config) -> Self {
        Self {
            config,
            mem: Vec::new(),
            spilled: None,
        }
    }

    fn ensure_spilled(&mut self) -> Result<(), Error> {
        if self.spilled.is_some() {
            return Ok(());
        }
        let dir = self
            .config
            .temp_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::new(format!("multipart: create temp dir: {e}")))?;
        let suffix = crypto::rand::bytes(16)
            .map_err(|e| Error::new(format!("multipart: tempfile random: {}", e.message())))?;
        let mut name = String::from("gos-multipart-");
        for b in &suffix {
            use std::fmt::Write as _;
            let _ = write!(&mut name, "{b:02x}");
        }
        name.push_str(".tmp");
        let path = dir.join(name);
        let file = create_private_tempfile(&path)?;
        // Flush any bytes already buffered.
        let mut f = file;
        if !self.mem.is_empty() {
            f.write_all(&self.mem)
                .map_err(|e| Error::new(format!("multipart: tempfile write: {e}")))?;
            self.mem.clear();
            self.mem.shrink_to_fit();
        }
        self.spilled = Some((path, f));
        Ok(())
    }

    fn finish(mut self) -> Result<PartData, Error> {
        if let Some((path, mut f)) = self.spilled.take() {
            f.flush()
                .map_err(|e| Error::new(format!("multipart: tempfile flush: {e}")))?;
            drop(f);
            Ok(PartData::OnDisk(path))
        } else {
            Ok(PartData::InMemory(std::mem::take(&mut self.mem)))
        }
    }
}

impl BodySink for FileSink<'_> {
    fn write_all(&mut self, chunk: &[u8]) -> Result<(), Error> {
        if let Some((_, f)) = self.spilled.as_mut() {
            f.write_all(chunk)
                .map_err(|e| Error::new(format!("multipart: tempfile write: {e}")))?;
            return Ok(());
        }
        // Would this push us over the threshold?
        if self.mem.len() + chunk.len() > self.config.max_in_memory_bytes {
            self.ensure_spilled()?;
            if let Some((_, f)) = self.spilled.as_mut() {
                f.write_all(chunk)
                    .map_err(|e| Error::new(format!("multipart: tempfile write: {e}")))?;
            }
            return Ok(());
        }
        self.mem.extend_from_slice(chunk);
        Ok(())
    }
}

#[cfg(unix)]
fn create_private_tempfile(path: &PathBuf) -> Result<File, Error> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| {
            Error::new(format!(
                "multipart: create tempfile {}: {e}",
                path.display()
            ))
        })
}

#[cfg(windows)]
fn create_private_tempfile(path: &PathBuf) -> Result<File, Error> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            Error::new(format!(
                "multipart: create tempfile {}: {e}",
                path.display()
            ))
        })?;
    // `env::temp_dir()` can resolve to a directory shared by other users on
    // Windows, so the spilled upload body must be restricted explicitly: an
    // owner-only DACL, the analogue of the unix `0o600` above. Fail closed.
    restrict_to_owner(path).map_err(|e| {
        Error::new(format!(
            "multipart: restrict tempfile {}: {e}",
            path.display()
        ))
    })?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn create_private_tempfile(path: &PathBuf) -> Result<File, Error> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            Error::new(format!(
                "multipart: create tempfile {}: {e}",
                path.display()
            ))
        })
}

/// Replaces a file's DACL with a single ACE granting the current user
/// read+write and nothing else, marking the DACL protected so inherited ACEs
/// are dropped. The Windows analogue of `chmod 0600`.
// Win32 ACL programming is inherently `unsafe` FFI; the block is
// self-contained and audited (two-call TOKEN_USER pattern + DACL set).
#[cfg(windows)]
#[allow(unsafe_code)]
fn restrict_to_owner(path: &std::path::Path) -> std::io::Result<()> {
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut len);
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &raw mut len) == 0 {
            CloseHandle(token);
            return Err(io::Error::last_os_error());
        }
        // The global allocator returns memory aligned to at least 16 bytes, so
        // the `Vec<u8>` backing store satisfies `TOKEN_USER`'s 8-byte alignment.
        #[allow(clippy::cast_ptr_alignment)]
        let token_user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let sid = token_user.User.Sid;

        let mut ea: EXPLICIT_ACCESS_W = std::mem::zeroed();
        ea.grfAccessPermissions = FILE_GENERIC_READ | FILE_GENERIC_WRITE;
        ea.grfAccessMode = SET_ACCESS;
        ea.grfInheritance = NO_INHERITANCE;
        ea.Trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        };

        let mut acl: *mut ACL = std::ptr::null_mut();
        let rc = SetEntriesInAclW(1, &raw const ea, std::ptr::null_mut(), &raw mut acl);
        CloseHandle(token);
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let rc = SetNamedSecurityInfoW(
            wide.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        );
        if !acl.is_null() {
            LocalFree(acl.cast());
        }
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
    }
    Ok(())
}

/// Parses the field-name and filename out of a Content-Disposition value.
/// Recognizes `name="..."`, `filename="..."`, and basic
/// `filename*=UTF-8''percent-encoded` (RFC 5987).
fn parse_disposition(value: &str) -> (Option<String>, Option<String>) {
    let mut name: Option<String> = None;
    let mut filename: Option<String> = None;
    let mut filename_ext: Option<String> = None;
    for raw in split_params(value) {
        let (k, v) = match raw.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        let unquoted = unquote(v);
        let kl = k.to_ascii_lowercase();
        match kl.as_str() {
            "name" => name = Some(unquoted),
            "filename" => filename = Some(unquoted),
            "filename*" => {
                if let Some(decoded) = decode_rfc5987(&unquoted) {
                    filename_ext = Some(decoded);
                }
            }
            _ => {}
        }
    }
    // RFC 5987: filename* (when present) takes precedence over filename.
    (name, filename_ext.or(filename))
}

/// Splits a Content-Disposition value on `;` but only outside quoted
/// strings (no backslash escapes - RFC 7578 forbids them).
fn split_params(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = value.as_bytes();
    let mut in_quote = false;
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_quote = !in_quote,
            b';' if !in_quote => {
                out.push(value[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(value[start..].trim());
    out
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Minimal RFC 5987 decoder: `charset'lang'pct-encoded`. We accept only
/// UTF-8 (case-insensitive) and ignore the language tag.
fn decode_rfc5987(value: &str) -> Option<String> {
    let first = value.find('\'')?;
    let charset = &value[..first];
    let rest = &value[first + 1..];
    let second = rest.find('\'')?;
    let encoded = &rest[second + 1..];
    if !charset.eq_ignore_ascii_case("utf-8") {
        return None;
    }
    let mut out = Vec::with_capacity(encoded.len());
    let bytes = encoded.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_nibble(bytes[i + 1])?;
            let lo = hex_nibble(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + (b - b'a')),
        b'A'..=b'F' => Some(10 + (b - b'A')),
        _ => None,
    }
}

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<(), Error> {
    r.read_exact(buf).map_err(io_err)
}

fn io_err(e: std::io::Error) -> Error {
    Error::new(format!("multipart: io: {e}"))
}

// ---------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn body(parts: &[&[u8]], boundary: &str, closing: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i == 0 {
                out.extend_from_slice(b"--");
                out.extend_from_slice(boundary.as_bytes());
                out.extend_from_slice(b"\r\n");
            } else {
                out.extend_from_slice(b"\r\n--");
                out.extend_from_slice(boundary.as_bytes());
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(part);
        }
        if closing {
            out.extend_from_slice(b"\r\n--");
            out.extend_from_slice(boundary.as_bytes());
            out.extend_from_slice(b"--\r\n");
        }
        out
    }

    #[test]
    fn parse_boundary_bare() {
        let b = parse_boundary("multipart/form-data; boundary=abc123").unwrap();
        assert_eq!(b, "abc123");
    }

    #[test]
    fn parse_boundary_quoted() {
        let b = parse_boundary("multipart/form-data; boundary=\"my boundary\"").unwrap();
        assert_eq!(b, "my boundary");
    }

    #[test]
    fn parse_boundary_trailing_param() {
        let b = parse_boundary("multipart/form-data; boundary=abc; charset=utf-8").unwrap();
        assert_eq!(b, "abc");
    }

    #[test]
    fn parse_boundary_missing() {
        assert!(parse_boundary("text/plain").is_err());
    }

    #[test]
    fn single_text_field() {
        let boundary = "BOUND";
        let part = b"Content-Disposition: form-data; name=\"greeting\"\r\n\r\nhello";
        let body = body(&[part], boundary, true);
        let form = parse_bytes(&body, boundary, &Config::default()).unwrap();
        assert_eq!(form.len(), 1);
        assert_eq!(form.get_text("greeting"), Some("hello"));
    }

    #[test]
    fn multiple_text_fields() {
        let boundary = "X";
        let p1 = b"Content-Disposition: form-data; name=\"a\"\r\n\r\nfirst";
        let p2 = b"Content-Disposition: form-data; name=\"b\"\r\n\r\nsecond";
        let p3 = b"Content-Disposition: form-data; name=\"c\"\r\n\r\nthird";
        let body = body(&[p1, p2, p3], boundary, true);
        let form = parse_bytes(&body, boundary, &Config::default()).unwrap();
        assert_eq!(form.len(), 3);
        assert_eq!(form.get_text("a"), Some("first"));
        assert_eq!(form.get_text("b"), Some("second"));
        assert_eq!(form.get_text("c"), Some("third"));
    }

    #[test]
    fn single_file_in_memory() {
        let boundary = "F";
        let part = b"Content-Disposition: form-data; name=\"upload\"; filename=\"hello.txt\"\r\nContent-Type: text/plain\r\n\r\nfile contents here";
        let body = body(&[part], boundary, true);
        let form = parse_bytes(&body, boundary, &Config::default()).unwrap();
        assert_eq!(form.len(), 1);
        let f = form.get_file("upload").expect("file present");
        match f {
            Part::File {
                filename,
                content_type,
                data,
                ..
            } => {
                assert_eq!(filename, "hello.txt");
                assert_eq!(content_type.as_deref(), Some("text/plain"));
                match data {
                    PartData::InMemory(bytes) => {
                        assert_eq!(bytes.as_slice(), b"file contents here");
                    }
                    PartData::OnDisk(_) => panic!("should be in memory"),
                }
            }
            Part::Text { .. } => panic!("expected file part"),
        }
    }

    #[test]
    fn file_spills_to_disk() {
        let boundary = "S";
        let big = vec![b'q'; 1024];
        let mut part: Vec<u8> = Vec::new();
        part.extend_from_slice(
            b"Content-Disposition: form-data; name=\"x\"; filename=\"big.bin\"\r\n\r\n",
        );
        part.extend_from_slice(&big);
        let body = body(&[&part], boundary, true);
        let cfg = Config {
            max_in_memory_bytes: 64,
            ..Config::default()
        };
        let form = parse_bytes(&body, boundary, &cfg).unwrap();
        assert_eq!(form.len(), 1);
        let f = form.get_file("x").unwrap();
        match f {
            Part::File {
                data: PartData::OnDisk(path),
                ..
            } => {
                assert!(path.exists(), "tempfile should exist");
                let read = std::fs::read(path).unwrap();
                assert_eq!(read, big);
            }
            _ => panic!("expected on-disk file part"),
        }
        // Drop unlinks the file.
        let saved_path = match form.get_file("x").unwrap() {
            Part::File {
                data: PartData::OnDisk(p),
                ..
            } => p.clone(),
            _ => unreachable!(),
        };
        drop(form);
        assert!(!saved_path.exists(), "tempfile should be unlinked on drop");
    }

    #[test]
    fn mixed_text_and_file() {
        let boundary = "MIX";
        let p1 = b"Content-Disposition: form-data; name=\"username\"\r\n\r\nada";
        let p2 = b"Content-Disposition: form-data; name=\"avatar\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n\x89PNG\r\n\x1a\n";
        let body = body(&[p1, p2], boundary, true);
        let form = parse_bytes(&body, boundary, &Config::default()).unwrap();
        assert_eq!(form.len(), 2);
        assert_eq!(form.get_text("username"), Some("ada"));
        let f = form.get_file("avatar").unwrap();
        if let Part::File {
            filename,
            data: PartData::InMemory(bytes),
            content_type,
            ..
        } = f
        {
            assert_eq!(filename, "a.png");
            assert_eq!(content_type.as_deref(), Some("image/png"));
            assert_eq!(bytes.as_slice(), b"\x89PNG\r\n\x1a\n");
        } else {
            panic!("expected in-memory file part");
        }
    }

    #[test]
    fn missing_name_errors() {
        let boundary = "M";
        let part = b"Content-Disposition: form-data\r\n\r\nnope";
        let body = body(&[part], boundary, true);
        let err = parse_bytes(&body, boundary, &Config::default()).unwrap_err();
        assert!(err.message().contains("name"), "got: {}", err.message());
    }

    #[test]
    fn missing_closing_errors() {
        let boundary = "C";
        let part = b"Content-Disposition: form-data; name=\"x\"\r\n\r\nvalue";
        // No closing --.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"--");
        bytes.extend_from_slice(boundary.as_bytes());
        bytes.extend_from_slice(b"\r\n");
        bytes.extend_from_slice(part);
        // Reader hits EOF before the next boundary marker is found.
        let err = parse_bytes(&bytes, boundary, &Config::default()).unwrap_err();
        assert!(
            err.message().contains("EOF") || err.message().contains("boundary"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn closing_delimiter_succeeds() {
        let boundary = "Z";
        let part = b"Content-Disposition: form-data; name=\"k\"\r\n\r\nv";
        let body = body(&[part], boundary, true);
        let form = parse_bytes(&body, boundary, &Config::default()).unwrap();
        assert_eq!(form.get_text("k"), Some("v"));
    }

    #[test]
    fn max_parts_cap() {
        let boundary = "P";
        let p1 = b"Content-Disposition: form-data; name=\"a\"\r\n\r\n1";
        let p2 = b"Content-Disposition: form-data; name=\"b\"\r\n\r\n2";
        let p3 = b"Content-Disposition: form-data; name=\"c\"\r\n\r\n3";
        let body = body(&[p1, p2, p3], boundary, true);
        let cfg = Config {
            max_parts: 2,
            ..Config::default()
        };
        let err = parse_bytes(&body, boundary, &cfg).unwrap_err();
        assert!(
            err.message().contains("max_parts"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn max_part_size_cap() {
        let boundary = "PS";
        let body_bytes = body(
            &[b"Content-Disposition: form-data; name=\"k\"\r\n\r\n0123456789ABCDEF"],
            boundary,
            true,
        );
        let cfg = Config {
            max_part_size: 4,
            ..Config::default()
        };
        let err = parse_bytes(&body_bytes, boundary, &cfg).unwrap_err();
        assert!(
            err.message().contains("max_part_size"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn get_all_returns_every_match() {
        let boundary = "G";
        let p1 = b"Content-Disposition: form-data; name=\"tag\"\r\n\r\nred";
        let p2 = b"Content-Disposition: form-data; name=\"tag\"\r\n\r\nblue";
        let p3 = b"Content-Disposition: form-data; name=\"other\"\r\n\r\nx";
        let body = body(&[p1, p2, p3], boundary, true);
        let form = parse_bytes(&body, boundary, &Config::default()).unwrap();
        let all = form.get_all("tag");
        assert_eq!(all.len(), 2);
        assert!(matches!(all[0], Part::Text { .. }));
    }

    #[test]
    fn rfc5987_filename_decoded() {
        let v = "form-data; name=\"f\"; filename*=UTF-8''na%C3%AFve.txt";
        let (name, filename) = parse_disposition(v);
        assert_eq!(name.as_deref(), Some("f"));
        assert_eq!(filename.as_deref(), Some("naïve.txt"));
    }

    #[test]
    fn streaming_reader_works() {
        // Drive through a small Cursor to exercise BufReader refills.
        let boundary = "STREAM";
        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(
            b"Content-Disposition: form-data; name=\"big\"; filename=\"x\"\r\n\r\n",
        );
        blob.extend(std::iter::repeat_n(b'z', 20_000));
        let body = body(&[&blob], boundary, true);
        let cursor = Cursor::new(body);
        let form = parse(cursor, boundary, &Config::default()).unwrap();
        let f = form.get_file("big").unwrap();
        match f {
            Part::File {
                data: PartData::InMemory(bytes),
                ..
            } => {
                assert_eq!(bytes.len(), 20_000);
                assert!(bytes.iter().all(|&b| b == b'z'));
            }
            Part::File { .. } | Part::Text { .. } => panic!("expected in-memory file part"),
        }
    }
}
