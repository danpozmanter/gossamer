// Runtime support for `std::archive::zip` — ZIP archive reading and writing.
//
// Wraps the `zip` crate. The read API extracts files into memory; the write API
// builds an in-memory ZIP archive from name/content pairs. Both return IoError on
// failure so callers can use `?` without a separate error conversion.

#![forbid(unsafe_code)]

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;

use crate::io::IoError;

/// A single entry extracted from a ZIP archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// Path inside the archive.
    pub name: String,
    /// Decompressed file content. Empty for directory entries.
    pub data: Vec<u8>,
    /// `true` for directory entries (no data).
    pub is_dir: bool,
}

/// Reads all file entries from a ZIP archive stored in `data`.
///
/// Directory entries are included with an empty `data` field and
/// `is_dir = true`. Returns an error if the bytes are not a valid ZIP
/// archive.
pub fn read(data: &[u8]) -> Result<Vec<ZipEntry>, IoError> {
    let cursor = Cursor::new(data);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| IoError::Other(format!("zip read: {e}")))?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| IoError::Other(format!("zip entry {i}: {e}")))?;
        let name = file.name().to_owned();
        let is_dir = file.is_dir();
        let mut buf = Vec::new();
        if !is_dir {
            std::io::Read::read_to_end(&mut file, &mut buf)
                .map_err(|e| IoError::Other(format!("zip read entry {name}: {e}")))?;
        }
        entries.push(ZipEntry {
            name,
            data: buf,
            is_dir,
        });
    }
    Ok(entries)
}

/// Builds an in-memory ZIP archive from `files` — a list of `(name, data)` pairs.
///
/// Files are stored with deflate compression at the default level. Returns the
/// raw ZIP bytes on success.
pub fn write(files: &[(&str, &[u8])]) -> Result<Vec<u8>, IoError> {
    let buf = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for &(name, data) in files {
        zip.start_file(name, opts)
            .map_err(|e| IoError::Other(format!("zip start_file {name}: {e}")))?;
        zip.write_all(data)
            .map_err(|e| IoError::Other(format!("zip write {name}: {e}")))?;
    }
    let finished = zip
        .finish()
        .map_err(|e| IoError::Other(format!("zip finish: {e}")))?;
    Ok(finished.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_single_file() {
        let content = b"hello from zip";
        let zip_bytes = write(&[("hello.txt", content)]).unwrap();
        let entries = read(&zip_bytes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.txt");
        assert_eq!(entries[0].data, content);
        assert!(!entries[0].is_dir);
    }

    #[test]
    fn roundtrip_multiple_files() {
        let zip_bytes = write(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]).unwrap();
        let entries = read(&zip_bytes).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[test]
    fn invalid_bytes_return_error() {
        let result = read(b"not a zip");
        assert!(result.is_err());
    }

    #[test]
    fn empty_archive() {
        let zip_bytes = write(&[]).unwrap();
        let entries = read(&zip_bytes).unwrap();
        assert!(entries.is_empty());
    }
}
