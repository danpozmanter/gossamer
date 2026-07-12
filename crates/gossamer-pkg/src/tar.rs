//! Minimal POSIX (USTAR) tar reader.
//!
//! Parses a concatenation of 512-byte headers + padded file payloads
//! as emitted by `tar cf out.tar dir/`. Enough to unpack a
//! dependency tarball into a `BTreeMap<path, bytes>`; anything
//! fancier (sparse files, pax extended attributes, symlinks,
//! gzipped `.tar.gz`) returns [`TarError::Unsupported`]. Pulled in
//! because every credible dependency tarball is a tar file, and the
//! package fetcher now needs to crack them open without linking a
//! C library.
//!
//! Implements the single-file strict-read half of
//! the risks backlog "Real package-registry transport +
//! signature verification" - the registry-server + publish-flow
//! half is deliberately out of scope per the plan's staged
//! recommendation.
//!
//! Safe Rust only; no `unsafe` blocks. Workspace pledge upheld.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{self, Read};

/// Error shape for tarball parsing.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TarError {
    /// Input ended mid-entry.
    #[error("truncated tar input at offset {0}")]
    Truncated(usize),
    /// Header's stored checksum did not match a recomputation over
    /// its bytes.
    #[error("tar header checksum mismatch for `{0}`")]
    BadChecksum(String),
    /// Size field was not parseable as an octal number.
    #[error("tar header size field malformed for `{0}`")]
    BadSize(String),
    /// Entry kind we do not yet unpack (symlink, hardlink, device
    /// node, sparse, pax extended attrs). Callers see the raw
    /// type-flag byte to decide whether to error or ignore.
    #[error("tar entry `{name}`: unsupported type flag {flag:?}")]
    Unsupported {
        /// Entry name as parsed.
        name: String,
        /// Byte value of the type flag field.
        flag: char,
    },
    /// Gzipped archive detected (first two bytes are the gzip magic).
    /// Callers that want `.tar.gz` support must decompress upstream.
    #[error(
        "gzipped archive detected - .tar.gz support is a follow-up; decompress before calling [`unpack`]"
    )]
    Gzipped,
    /// Entry name escapes the extraction directory: an absolute path,
    /// a `..` component, or a Windows drive/UNC prefix. Rejected so an
    /// archive cannot write outside the directory it is unpacked into
    /// (the classic tar-slip / zip-slip traversal).
    #[error("tar entry `{0}`: path escapes the extraction directory")]
    UnsafePath(String),
    /// Two members normalized to the same destination path. Rejecting this
    /// prevents archive readers, the cache digest, and filesystem extraction
    /// from disagreeing about which payload wins.
    #[error("tar contains duplicate destination path `{0}`")]
    DuplicateEntry(String),
    /// The archive contains more entries than its caller permits.
    #[error("tar contains more than {0} entries")]
    TooManyEntries(usize),
    /// One file exceeds the configured extraction budget.
    #[error("tar entry `{name}` has {size} bytes; limit is {limit}")]
    FileTooLarge {
        /// Entry name.
        name: String,
        /// Declared size.
        size: usize,
        /// Maximum accepted size.
        limit: usize,
    },
    /// Total extracted file payload exceeds the configured budget.
    #[error("tar expands beyond {limit} bytes")]
    TotalTooLarge {
        /// Maximum accepted aggregate payload.
        limit: usize,
    },
}

const BLOCK: usize = 512;
/// Largest raw package archive accepted by the built-in registry transport.
/// Keeping packing under this limit prevents producing an artifact that a
/// default `gos fetch` will later reject before it reaches the tar parser.
pub const MAX_PACKAGE_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// Extraction limits for a package tarball. The parser keeps a fully buffered
/// map today, so both the individual and aggregate payload must be bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackLimits {
    /// Maximum regular-file entries.
    pub max_entries: usize,
    /// Maximum size of a single regular file.
    pub max_file_bytes: usize,
    /// Maximum aggregate regular-file payload.
    pub max_total_bytes: usize,
}

impl Default for UnpackLimits {
    fn default() -> Self {
        Self {
            max_entries: 4096,
            max_file_bytes: 16 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Unpacks `bytes` into a path → contents map. Directory entries
/// become empty-byte files so callers walking the map still see
/// them. Returns an empty map for a zero-length input.
pub fn unpack(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, TarError> {
    unpack_with_limits(bytes, UnpackLimits::default())
}

/// Bounded variant of [`unpack`]. Use a narrower limit for a registry or
/// sandbox policy; [`unpack`] uses the package-safe defaults above.
pub fn unpack_with_limits(
    bytes: &[u8],
    limits: UnpackLimits,
) -> Result<BTreeMap<String, Vec<u8>>, TarError> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        return Err(TarError::Gzipped);
    }
    let mut out = BTreeMap::new();
    let mut offset = 0;
    let mut regular_entries = 0usize;
    let mut total_payload = 0usize;
    while offset < bytes.len() {
        if offset + BLOCK > bytes.len() {
            return Err(TarError::Truncated(offset));
        }
        let header = &bytes[offset..offset + BLOCK];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let name = parse_name(header);
        let size = parse_size(header).ok_or_else(|| TarError::BadSize(name.clone()))?;
        verify_checksum(header, &name)?;
        let flag = header[156] as char;
        offset += BLOCK;
        let payload_end = offset
            .checked_add(size)
            .ok_or(TarError::Truncated(offset))?;
        if payload_end > bytes.len() {
            return Err(TarError::Truncated(offset));
        }
        match flag {
            '0' | '\0' => {
                regular_entries = regular_entries.saturating_add(1);
                if regular_entries > limits.max_entries {
                    return Err(TarError::TooManyEntries(limits.max_entries));
                }
                if size > limits.max_file_bytes {
                    return Err(TarError::FileTooLarge {
                        name,
                        size,
                        limit: limits.max_file_bytes,
                    });
                }
                total_payload = total_payload
                    .checked_add(size)
                    .ok_or(TarError::TotalTooLarge {
                        limit: limits.max_total_bytes,
                    })?;
                if total_payload > limits.max_total_bytes {
                    return Err(TarError::TotalTooLarge {
                        limit: limits.max_total_bytes,
                    });
                }
                let safe = safe_entry_name(&name).ok_or(TarError::UnsafePath(name.clone()))?;
                if out.contains_key(&safe) {
                    return Err(TarError::DuplicateEntry(safe));
                }
                let contents = bytes[offset..payload_end].to_vec();
                out.insert(safe, contents);
            }
            '5' => {
                // POSIX directory. Skip the payload (always zero)
                // and do not record the entry - our consumers walk
                // files only.
            }
            other => {
                return Err(TarError::Unsupported { name, flag: other });
            }
        }
        offset = payload_end;
        if size % BLOCK != 0 {
            offset += BLOCK - (size % BLOCK);
        }
    }
    Ok(out)
}

/// Reader-oriented counterpart to [`unpack_with_limits`]. It retains the
/// public map-shaped result for compatibility, but never needs a second
/// allocation for the raw archive. Package downloads use this after hashing
/// their temporary spool file.
pub fn unpack_reader<R: Read>(reader: R) -> Result<BTreeMap<String, Vec<u8>>, TarError> {
    unpack_reader_with_limits(reader, UnpackLimits::default())
}

/// Unpacks a USTAR stream with the same validation and limits as
/// [`unpack_with_limits`].
pub fn unpack_reader_with_limits<R: Read>(
    reader: R,
    limits: UnpackLimits,
) -> Result<BTreeMap<String, Vec<u8>>, TarError> {
    let mut reader = io::BufReader::with_capacity(BLOCK * 2, reader);
    let mut out = BTreeMap::new();
    let mut offset = 0usize;
    let mut regular_entries = 0usize;
    let mut total_payload = 0usize;
    loop {
        let mut header = [0u8; BLOCK];
        let read = read_block_or_eof(&mut reader, &mut header)
            .map_err(|()| TarError::Truncated(offset))?;
        if read == 0 {
            break;
        }
        if offset == 0 && read >= 2 && header[..2] == [0x1f, 0x8b] {
            return Err(TarError::Gzipped);
        }
        if read != BLOCK {
            return Err(TarError::Truncated(offset));
        }
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name = parse_name(&header);
        let size = parse_size(&header).ok_or_else(|| TarError::BadSize(name.clone()))?;
        verify_checksum(&header, &name)?;
        let flag = header[156] as char;
        offset = offset
            .checked_add(BLOCK)
            .ok_or(TarError::Truncated(offset))?;
        match flag {
            '0' | '\0' => {
                regular_entries = regular_entries.saturating_add(1);
                if regular_entries > limits.max_entries {
                    return Err(TarError::TooManyEntries(limits.max_entries));
                }
                if size > limits.max_file_bytes {
                    return Err(TarError::FileTooLarge {
                        name,
                        size,
                        limit: limits.max_file_bytes,
                    });
                }
                total_payload = total_payload
                    .checked_add(size)
                    .ok_or(TarError::TotalTooLarge {
                        limit: limits.max_total_bytes,
                    })?;
                if total_payload > limits.max_total_bytes {
                    return Err(TarError::TotalTooLarge {
                        limit: limits.max_total_bytes,
                    });
                }
                let safe = safe_entry_name(&name).ok_or(TarError::UnsafePath(name.clone()))?;
                if out.contains_key(&safe) {
                    return Err(TarError::DuplicateEntry(safe));
                }
                let mut contents = vec![0u8; size];
                reader
                    .read_exact(&mut contents)
                    .map_err(|_| TarError::Truncated(offset))?;
                out.insert(safe, contents);
            }
            '5' => skip_exact(&mut reader, size).map_err(|_| TarError::Truncated(offset))?,
            other => return Err(TarError::Unsupported { name, flag: other }),
        }
        offset = offset
            .checked_add(size)
            .ok_or(TarError::Truncated(offset))?;
        let padding = (BLOCK - size % BLOCK) % BLOCK;
        skip_exact(&mut reader, padding).map_err(|_| TarError::Truncated(offset))?;
        offset = offset
            .checked_add(padding)
            .ok_or(TarError::Truncated(offset))?;
    }
    Ok(out)
}

fn read_block_or_eof<R: Read>(reader: &mut R, block: &mut [u8; BLOCK]) -> Result<usize, ()> {
    let mut read = 0usize;
    while read != BLOCK {
        let n = reader.read(&mut block[read..]).map_err(|_| ())?;
        if n == 0 {
            return Ok(read);
        }
        read += n;
    }
    Ok(read)
}

fn skip_exact<R: Read>(reader: &mut R, mut count: usize) -> io::Result<()> {
    let mut scratch = [0u8; 8192];
    while count != 0 {
        let take = count.min(scratch.len());
        reader.read_exact(&mut scratch[..take])?;
        count -= take;
    }
    Ok(())
}

/// Returns a canonical slash-separated relative path if `name` stays within
/// the extraction directory, or `None` if it escapes it.
///
/// Rejects absolute paths, any `..`/root/drive component, and Windows
/// separators or drive/UNC prefixes (checked on the raw string because
/// `std::path::Component` does not split on `\` under Unix). This is
/// the single choke point: every consumer joins these names onto a
/// base directory, so guarding here prevents tar-slip writes in all of
/// them.
fn safe_entry_name(name: &str) -> Option<String> {
    use std::path::{Component, Path};
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return None;
    }
    // A leading drive letter (`C:`) or UNC-ish prefix never belongs in
    // a relative entry name.
    if name.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return None;
    }
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part.to_str()?.to_string()),
            // `./src/main.gos` is common in tar archives, but it must not
            // remain distinct from `src/main.gos` in the extracted tree.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!normalized.is_empty()).then(|| normalized.join("/"))
}

fn parse_name(header: &[u8]) -> String {
    // USTAR splits long names across `prefix` (offset 345, 155 bytes)
    // and `name` (offset 0, 100 bytes). Old tar tools emit only
    // `name`; GNU/BSD tar uses the split when names exceed 100
    // bytes. We honour both.
    let name = null_terminated(&header[0..100]);
    let prefix = if header.len() >= 500 {
        null_terminated(&header[345..500])
    } else {
        String::new()
    };
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

fn parse_size(header: &[u8]) -> Option<usize> {
    let field = &header[124..136];
    let text = std::str::from_utf8(field).ok()?;
    let trimmed = text.trim_end_matches('\0').trim();
    if trimmed.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(trimmed, 8).ok()
}

fn verify_checksum(header: &[u8], name: &str) -> Result<(), TarError> {
    let stored_text = std::str::from_utf8(&header[148..156]).unwrap_or("");
    let stored_trimmed = stored_text.trim_end_matches(['\0', ' ']).trim();
    let Some(stored) = u32::from_str_radix(stored_trimmed, 8).ok() else {
        return Err(TarError::BadChecksum(name.to_string()));
    };
    let mut sum: u32 = 0;
    for (i, byte) in header.iter().enumerate() {
        if (148..156).contains(&i) {
            sum += u32::from(b' ');
        } else {
            sum += u32::from(*byte);
        }
    }
    if sum == stored {
        Ok(())
    } else {
        Err(TarError::BadChecksum(name.to_string()))
    }
}

fn null_terminated(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Errors raised by [`pack`].
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PackError {
    /// Streaming archive input/output failed.
    #[error("archive I/O: {0}")]
    Io(String),
    /// Path was longer than the (USTAR `prefix` + `name`) split can express.
    #[error("path too long for USTAR (>= 256 bytes): {0}")]
    PathTooLong(String),
    /// Payload exceeded the 8 GiB ceiling representable in the 12-byte
    /// octal `size` field.
    #[error("file too large for USTAR (>= 8 GiB): {0}")]
    FileTooLarge(String),
    /// The entry name is not a canonical relative package path.
    #[error("unsafe or non-canonical archive path: {0}")]
    UnsafePath(String),
    /// More regular files were supplied than the pack policy permits.
    #[error("archive contains more than {0} entries")]
    TooManyEntries(usize),
    /// A regular file exceeds the configured packing budget.
    #[error("archive entry `{path}` has {size} bytes; limit is {limit}")]
    EntryTooLarge {
        /// Entry path.
        path: String,
        /// Entry length in bytes.
        size: usize,
        /// Configured per-entry ceiling.
        limit: usize,
    },
    /// Aggregate regular-file bytes exceed the packing budget.
    #[error("archive payload exceeds {limit} bytes")]
    TotalTooLarge {
        /// Configured aggregate payload ceiling.
        limit: usize,
    },
    /// The encoded tar including headers, padding, and end marker is too big.
    #[error("archive encoding exceeds {limit} bytes")]
    ArchiveTooLarge {
        /// Configured final archive ceiling.
        limit: usize,
    },
}

/// Resource limits for deterministic archive creation. These mirror the
/// unpacker limits and additionally cap the final wire-format size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackLimits {
    /// Maximum regular-file entries.
    pub max_entries: usize,
    /// Maximum bytes in one regular file.
    pub max_file_bytes: usize,
    /// Maximum aggregate regular-file payload bytes.
    pub max_total_bytes: usize,
    /// Maximum total tar bytes, including headers and padding.
    pub max_archive_bytes: usize,
}

impl Default for PackLimits {
    fn default() -> Self {
        let unpack = UnpackLimits::default();
        Self {
            max_entries: unpack.max_entries,
            max_file_bytes: unpack.max_file_bytes,
            max_total_bytes: unpack.max_total_bytes,
            max_archive_bytes: MAX_PACKAGE_ARCHIVE_BYTES,
        }
    }
}

/// Builds a deterministic USTAR-format tar buffer from `entries`.
/// Entries are emitted in lexicographic order, modification times
/// are zero, and ownership is set to root:root - so two runs over
/// the same input produce byte-identical output. Used by `gos
/// publish` so the published sha256 is stable across machines.
pub fn pack(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, PackError> {
    pack_with_limits(entries, PackLimits::default())
}

/// Bounded variant of [`pack`]. It validates paths before creating the output
/// allocation and computes the exact encoded size up front.
pub fn pack_with_limits(
    entries: &BTreeMap<String, Vec<u8>>,
    limits: PackLimits,
) -> Result<Vec<u8>, PackError> {
    let encoded_len = checked_pack_size(entries, limits)?;
    let mut out = Vec::with_capacity(encoded_len);
    for (path, body) in entries {
        let header = pack_header(path, body)?;
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        let pad = (BLOCK - body.len() % BLOCK) % BLOCK;
        out.resize(out.len() + pad, 0);
    }
    // USTAR end marker: two zero blocks.
    out.extend_from_slice(&[0u8; BLOCK * 2]);
    Ok(out)
}

/// Validates a sequence of package file names and lengths and returns the
/// exact USTAR encoding length. This is the planning half of streamed archive
/// creation: callers can reject a project before opening any file contents.
pub(crate) fn checked_pack_file_sizes(
    entries: &[(String, usize)],
    limits: PackLimits,
) -> Result<usize, PackError> {
    let mut total_payload = 0usize;
    let mut archive_bytes = BLOCK * 2;
    for (index, (path, size)) in entries.iter().enumerate() {
        if index >= limits.max_entries {
            return Err(PackError::TooManyEntries(limits.max_entries));
        }
        if safe_entry_name(path).as_deref() != Some(path.as_str()) {
            return Err(PackError::UnsafePath(path.clone()));
        }
        if *size > 0o7777_7777_7777 {
            return Err(PackError::FileTooLarge(path.clone()));
        }
        if *size > limits.max_file_bytes {
            return Err(PackError::EntryTooLarge {
                path: path.clone(),
                size: *size,
                limit: limits.max_file_bytes,
            });
        }
        total_payload = total_payload
            .checked_add(*size)
            .ok_or(PackError::TotalTooLarge {
                limit: limits.max_total_bytes,
            })?;
        if total_payload > limits.max_total_bytes {
            return Err(PackError::TotalTooLarge {
                limit: limits.max_total_bytes,
            });
        }
        let padding = (BLOCK - size % BLOCK) % BLOCK;
        archive_bytes = archive_bytes
            .checked_add(BLOCK)
            .and_then(|length| length.checked_add(*size))
            .and_then(|length| length.checked_add(padding))
            .ok_or(PackError::ArchiveTooLarge {
                limit: limits.max_archive_bytes,
            })?;
        if archive_bytes > limits.max_archive_bytes {
            return Err(PackError::ArchiveTooLarge {
                limit: limits.max_archive_bytes,
            });
        }
    }
    Ok(archive_bytes)
}

/// Writes one validated regular-file entry directly to `out`. The reader must
/// yield exactly `size` bytes; this makes a changed project file fail instead
/// of silently producing a tar whose header and payload disagree.
pub(crate) fn write_file_entry<W: std::io::Write, R: Read>(
    out: &mut W,
    path: &str,
    size: usize,
    reader: &mut R,
) -> Result<(), PackError> {
    let header = pack_header_size(path, size)?;
    out.write_all(&header)
        .map_err(|error| PackError::Io(error.to_string()))?;
    let mut remaining = size;
    let mut buffer = [0u8; 8192];
    while remaining != 0 {
        let wanted = remaining.min(buffer.len());
        let read = reader
            .read(&mut buffer[..wanted])
            .map_err(|error| PackError::Io(error.to_string()))?;
        if read == 0 {
            return Err(PackError::Io(format!(
                "file {path} ended before its planned {size}-byte size"
            )));
        }
        out.write_all(&buffer[..read])
            .map_err(|error| PackError::Io(error.to_string()))?;
        remaining -= read;
    }
    let mut trailing = [0u8; 1];
    if reader
        .read(&mut trailing)
        .map_err(|error| PackError::Io(error.to_string()))?
        != 0
    {
        return Err(PackError::Io(format!(
            "file {path} grew after archive planning"
        )));
    }
    let padding = (BLOCK - size % BLOCK) % BLOCK;
    if padding != 0 {
        out.write_all(&[0u8; BLOCK][..padding])
            .map_err(|error| PackError::Io(error.to_string()))?;
    }
    Ok(())
}

/// Appends the mandatory USTAR end marker to a streamed archive.
pub(crate) fn write_end_marker<W: std::io::Write>(out: &mut W) -> Result<(), PackError> {
    out.write_all(&[0u8; BLOCK * 2])
        .map_err(|error| PackError::Io(error.to_string()))
}

fn checked_pack_size(
    entries: &BTreeMap<String, Vec<u8>>,
    limits: PackLimits,
) -> Result<usize, PackError> {
    let mut total_payload = 0usize;
    let mut archive_bytes = BLOCK * 2;
    for (index, (path, body)) in entries.iter().enumerate() {
        if index >= limits.max_entries {
            return Err(PackError::TooManyEntries(limits.max_entries));
        }
        if safe_entry_name(path).as_deref() != Some(path.as_str()) {
            return Err(PackError::UnsafePath(path.clone()));
        }
        if body.len() > 0o7777_7777_7777 {
            return Err(PackError::FileTooLarge(path.clone()));
        }
        if body.len() > limits.max_file_bytes {
            return Err(PackError::EntryTooLarge {
                path: path.clone(),
                size: body.len(),
                limit: limits.max_file_bytes,
            });
        }
        total_payload = total_payload
            .checked_add(body.len())
            .ok_or(PackError::TotalTooLarge {
                limit: limits.max_total_bytes,
            })?;
        if total_payload > limits.max_total_bytes {
            return Err(PackError::TotalTooLarge {
                limit: limits.max_total_bytes,
            });
        }
        let padding = (BLOCK - body.len() % BLOCK) % BLOCK;
        archive_bytes = archive_bytes
            .checked_add(BLOCK)
            .and_then(|size| size.checked_add(body.len()))
            .and_then(|size| size.checked_add(padding))
            .ok_or(PackError::ArchiveTooLarge {
                limit: limits.max_archive_bytes,
            })?;
        if archive_bytes > limits.max_archive_bytes {
            return Err(PackError::ArchiveTooLarge {
                limit: limits.max_archive_bytes,
            });
        }
    }
    Ok(archive_bytes)
}

fn pack_header(path: &str, body: &[u8]) -> Result<[u8; BLOCK], PackError> {
    pack_header_size(path, body.len())
}

fn pack_header_size(path: &str, size: usize) -> Result<[u8; BLOCK], PackError> {
    let mut header = [0u8; BLOCK];
    write_path_into(&mut header, path)?;
    write_octal(&mut header[100..108], 0o644);
    write_octal(&mut header[108..116], 0);
    write_octal(&mut header[116..124], 0);
    write_octal(&mut header[124..136], size as u64);
    write_octal(&mut header[136..148], 0);
    // Checksum field initialised to 8 spaces during sum computation.
    for cell in &mut header[148..156] {
        *cell = b' ';
    }
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    let cs = format!("{sum:06o}\0 ");
    let cs_bytes = cs.as_bytes();
    for (i, b) in cs_bytes.iter().take(8).enumerate() {
        header[148 + i] = *b;
    }
    Ok(header)
}

fn write_path_into(header: &mut [u8; BLOCK], path: &str) -> Result<(), PackError> {
    let bytes = path.as_bytes();
    if bytes.len() <= 100 {
        for (i, b) in bytes.iter().enumerate() {
            header[i] = *b;
        }
        return Ok(());
    }
    if bytes.len() > 100 + 1 + 155 {
        return Err(PackError::PathTooLong(path.to_string()));
    }
    // Find a `/` split where the prefix fits in 155 bytes and the
    // suffix fits in 100. Walk backwards from the latest possible
    // split point.
    let max_prefix = bytes.len() - 1;
    let mut split: Option<usize> = None;
    for i in (1..=max_prefix.min(155)).rev() {
        if bytes[i] == b'/' && bytes.len() - i - 1 <= 100 {
            split = Some(i);
            break;
        }
    }
    let Some(split) = split else {
        return Err(PackError::PathTooLong(path.to_string()));
    };
    for (i, b) in bytes[..split].iter().enumerate() {
        header[345 + i] = *b;
    }
    for (i, b) in bytes[split + 1..].iter().enumerate() {
        header[i] = *b;
    }
    Ok(())
}

fn write_octal(field: &mut [u8], value: u64) {
    let width = field.len();
    let formatted = format!("{value:0width$o}", width = width - 1);
    let bytes = formatted.as_bytes();
    for (i, b) in bytes.iter().take(width - 1).enumerate() {
        field[i] = *b;
    }
    field[width - 1] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a single-entry tar buffer in memory. USTAR layout:
    /// 100 name | 8 mode | 8 uid | 8 gid | 12 size | 12 mtime |
    /// 8 chksum | 1 typeflag | 100 linkname | 6 magic | 2 version |
    /// 32 uname | 32 gname | 8 devmajor | 8 devminor | 155 prefix |
    /// 12 pad - 512 bytes total.
    fn build_tar(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        for (i, b) in name.as_bytes().iter().take(100).enumerate() {
            header[i] = *b;
        }
        let mode = b"0000644\0";
        for (i, b) in mode.iter().enumerate() {
            header[100 + i] = *b;
        }
        let size_octal = format!("{:011o}\0", body.len());
        for (i, b) in size_octal.as_bytes().iter().take(12).enumerate() {
            header[124 + i] = *b;
        }
        let mtime = b"00000000000\0";
        for (i, b) in mtime.iter().enumerate() {
            header[136 + i] = *b;
        }
        for cell in &mut header[148..156] {
            *cell = b' ';
        }
        header[156] = b'0';
        let magic = b"ustar\0";
        for (i, b) in magic.iter().enumerate() {
            header[257 + i] = *b;
        }
        let version = b"00";
        header[263] = version[0];
        header[264] = version[1];
        let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        let cs_str = format!("{checksum:06o}\0 ");
        for (i, b) in cs_str.as_bytes().iter().take(8).enumerate() {
            header[148 + i] = *b;
        }
        let mut out = Vec::with_capacity(1024);
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        let pad = (512 - body.len() % 512) % 512;
        out.resize(out.len() + pad, 0);
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    #[test]
    fn unpack_reads_a_single_normal_file() {
        let tar = build_tar("src/lib.gos", b"fn main() {}\n");
        let files = unpack(&tar).expect("unpack");
        assert_eq!(files.len(), 1);
        assert_eq!(
            files.get("src/lib.gos").map(Vec::as_slice),
            Some(b"fn main() {}\n" as &[u8])
        );
    }

    #[test]
    fn reader_unpack_matches_slice_unpack() {
        let tar = build_tar("src/streamed.gos", b"fn main() {}\n");
        let from_slice = unpack(&tar).unwrap();
        let from_reader = unpack_reader(std::io::Cursor::new(tar)).unwrap();
        assert_eq!(from_reader, from_slice);
    }

    #[test]
    fn unpack_refuses_gzipped_archives_with_a_clear_error() {
        let bytes = [0x1f, 0x8b, 0x00, 0x00, 0x00, 0x00];
        let err = unpack(&bytes).unwrap_err();
        assert!(matches!(err, TarError::Gzipped));
    }

    #[test]
    fn unpack_reports_checksum_mismatch_on_tampered_header() {
        let mut tar = build_tar("a.gos", b"hi");
        tar[148] = b'9';
        let err = unpack(&tar).unwrap_err();
        assert!(matches!(err, TarError::BadChecksum(_)));
    }

    #[test]
    fn unpack_handles_an_empty_archive() {
        let empty_blocks = vec![0u8; 1024];
        let files = unpack(&empty_blocks).expect("unpack empty");
        assert!(files.is_empty());
    }

    #[test]
    fn pack_round_trips_through_unpack() {
        let mut input: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        input.insert("src/main.gos".to_string(), b"fn main() {}\n".to_vec());
        input.insert("README.md".to_string(), b"# project\n".to_vec());
        input.insert("project.toml".to_string(), b"[project]\n".to_vec());
        let bytes = pack(&input).expect("pack");
        let back = unpack(&bytes).expect("unpack");
        assert_eq!(input, back);
    }

    #[test]
    fn unpack_limits_bound_file_count_and_payload() {
        let mut entries = BTreeMap::new();
        entries.insert("a.gos".to_string(), b"abc".to_vec());
        entries.insert("b.gos".to_string(), b"def".to_vec());
        let archive = pack(&entries).unwrap();

        let count_err = unpack_with_limits(
            &archive,
            UnpackLimits {
                max_entries: 1,
                max_file_bytes: 10,
                max_total_bytes: 10,
            },
        )
        .unwrap_err();
        assert!(matches!(count_err, TarError::TooManyEntries(1)));

        let file_err = unpack_with_limits(
            &archive,
            UnpackLimits {
                max_entries: 2,
                max_file_bytes: 2,
                max_total_bytes: 10,
            },
        )
        .unwrap_err();
        assert!(matches!(file_err, TarError::FileTooLarge { .. }));

        let total_err = unpack_with_limits(
            &archive,
            UnpackLimits {
                max_entries: 2,
                max_file_bytes: 10,
                max_total_bytes: 5,
            },
        )
        .unwrap_err();
        assert!(matches!(total_err, TarError::TotalTooLarge { limit: 5 }));
    }

    #[test]
    fn unpack_rejects_parent_dir_traversal() {
        let tar = build_tar("../../../../home/victim/.bashrc", b"evil\n");
        let err = unpack(&tar).unwrap_err();
        assert!(matches!(err, TarError::UnsafePath(_)), "got {err:?}");
    }

    #[test]
    fn unpack_rejects_absolute_paths() {
        let tar = build_tar("/etc/cron.d/x", b"evil\n");
        let err = unpack(&tar).unwrap_err();
        assert!(matches!(err, TarError::UnsafePath(_)), "got {err:?}");
    }

    #[test]
    fn unpack_rejects_backslash_separators() {
        let tar = build_tar("..\\..\\win.ini", b"evil\n");
        let err = unpack(&tar).unwrap_err();
        assert!(matches!(err, TarError::UnsafePath(_)), "got {err:?}");
    }

    #[test]
    fn unpack_allows_curdir_prefixed_relative_paths() {
        let tar = build_tar("./src/main.gos", b"ok\n");
        let files = unpack(&tar).expect("unpack");
        assert_eq!(
            files.get("src/main.gos").map(Vec::as_slice),
            Some(b"ok\n" as &[u8])
        );
    }

    #[test]
    fn unpack_rejects_duplicate_canonical_paths() {
        let first = build_tar("./src/main.gos", b"first\n");
        let second = build_tar("src/main.gos", b"second\n");
        let mut combined = first[..first.len() - 1024].to_vec();
        combined.extend_from_slice(&second[..second.len() - 1024]);
        combined.extend_from_slice(&[0u8; 1024]);
        let err = unpack(&combined).unwrap_err();
        assert!(matches!(err, TarError::DuplicateEntry(path) if path == "src/main.gos"));
    }

    #[test]
    fn pack_is_byte_deterministic() {
        let mut input: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        input.insert("a.txt".to_string(), b"alpha".to_vec());
        input.insert("b.txt".to_string(), b"beta".to_vec());
        let a = pack(&input).expect("pack a");
        let b = pack(&input).expect("pack b");
        assert_eq!(
            a, b,
            "two pack calls on identical input must produce identical bytes"
        );
    }

    #[test]
    fn pack_rejects_unsafe_paths_and_honours_resource_limits() {
        let mut unsafe_path = BTreeMap::new();
        unsafe_path.insert("../outside.gos".to_string(), b"no".to_vec());
        assert!(matches!(pack(&unsafe_path), Err(PackError::UnsafePath(_))));

        let mut entries = BTreeMap::new();
        entries.insert("a.gos".to_string(), b"abc".to_vec());
        entries.insert("b.gos".to_string(), b"def".to_vec());
        let limits = PackLimits {
            max_entries: 1,
            max_file_bytes: 10,
            max_total_bytes: 10,
            max_archive_bytes: 4 * BLOCK,
        };
        assert!(matches!(
            pack_with_limits(&entries, limits),
            Err(PackError::TooManyEntries(1))
        ));

        let limits = PackLimits {
            max_entries: 2,
            max_file_bytes: 2,
            max_total_bytes: 10,
            max_archive_bytes: 4 * BLOCK,
        };
        assert!(matches!(
            pack_with_limits(&entries, limits),
            Err(PackError::EntryTooLarge { .. })
        ));
    }
}
