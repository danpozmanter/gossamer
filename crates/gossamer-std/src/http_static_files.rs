#![allow(
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::let_and_return,
    clippy::map_unwrap_or,
    clippy::doc_markdown,
    clippy::cast_possible_wrap,
    clippy::manual_let_else,
    clippy::missing_errors_doc,
    clippy::missing_const_for_fn,
    clippy::bind_instead_of_map,
    clippy::or_then_unwrap
)]

//! Static file server.
//!
//! `FileServer` serves files from a root directory. Mirrors
//! Go's `http.FileServer` semantics:
//!
//! - `Last-Modified` + `ETag` headers with conditional GET
//!   (`If-Modified-Since`, `If-None-Match`) returning `304`.
//! - `Range: bytes=N-M` with `206 Partial Content` + correct
//!   `Content-Range`.
//! - MIME-sniff via extension table (covers the common web
//!   asset types).
//! - Path-traversal protection: rejects requests whose resolved
//!   filesystem path escapes the configured root.
//!
//! Wire as a `Router` handler:
//!
//! ```no_run
//! use gossamer_std::http_router::Router;
//! use gossamer_std::http_static_files::FileServer;
//!
//! let fs = FileServer::new("/var/www");
//! let mut router = Router::new();
//! router.get("/assets/{path...}", move |req, params| {
//!     let path = params.get("path").unwrap_or("");
//!     fs.serve_path(path, req)
//! });
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use crate::http::{Headers, Request, Response, StatusCode};

/// Configuration for [`FileServer`].
#[derive(Debug, Clone)]
pub struct FileServer {
    root: PathBuf,
    /// Emit `Last-Modified` headers and honour
    /// `If-Modified-Since`.
    pub last_modified: bool,
    /// Emit `ETag` headers and honour `If-None-Match`.
    pub etag: bool,
    /// Honour `Range:` requests with 206 responses.
    pub range_support: bool,
    /// Maximum file size to serve, in bytes. Files larger than
    /// this return 404 to avoid OOM on attacker-controlled
    /// paths. Set to 0 to allow any size.
    pub max_file_bytes: u64,
}

impl FileServer {
    /// Creates a server rooted at `root`. All file paths are
    /// resolved beneath this directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            last_modified: true,
            etag: true,
            range_support: true,
            max_file_bytes: 100 * 1024 * 1024,
        }
    }

    /// Serves `rel_path` (relative to the configured root) for
    /// `request`. Returns an appropriate 200 / 206 / 304 / 404 /
    /// 416 response per HTTP semantics.
    #[must_use]
    pub fn serve_path(&self, rel_path: &str, request: &Request) -> Response {
        // Path-traversal guard: canonicalize the requested path and
        // verify it is still under the canonical root. Canonicalize
        // must succeed - a path that does not resolve cannot be served
        // anyway, and falling back to the raw `candidate` would leave
        // `..` segments that the component-wise `starts_with` accepts.
        // Canonicalizing both sides also normalises platform prefixes
        // consistently (macOS `/private`, Windows `\\?\`), so the
        // containment check holds cross-platform.
        let candidate = self.root.join(rel_path);
        let canonical = match fs::canonicalize(&candidate) {
            Ok(c) => c,
            Err(_) => return not_found(),
        };
        let root_canonical = fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        if !canonical.starts_with(&root_canonical) {
            return not_found();
        }
        let meta = match fs::metadata(&canonical) {
            Ok(m) => m,
            Err(_) => return not_found(),
        };
        if meta.is_dir() {
            // Try to serve `index.html` under the directory.
            let idx = canonical.join("index.html");
            if let Ok(im) = fs::metadata(&idx) {
                return self.serve_file(&idx, &im, request);
            }
            return not_found();
        }
        if !meta.is_file() {
            return not_found();
        }
        if self.max_file_bytes > 0 && meta.len() > self.max_file_bytes {
            return not_found();
        }
        self.serve_file(&canonical, &meta, request)
    }

    fn serve_file(&self, path: &Path, meta: &fs::Metadata, request: &Request) -> Response {
        let mtime = meta.modified().ok();
        let len = meta.len();

        // Build ETag from mtime + size.
        let etag = mtime.as_ref().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| format!("\"{:x}-{:x}\"", d.as_secs(), len))
        });

        // Conditional GET: If-None-Match.
        if self.etag
            && let (Some(etag_val), Some(inm)) = (&etag, request.headers.get("if-none-match"))
            && inm.trim() == etag_val
        {
            return not_modified(etag_val.as_str(), mtime);
        }
        // Conditional GET: If-Modified-Since.
        if self.last_modified
            && let (Some(t), Some(ims)) = (&mtime, request.headers.get("if-modified-since"))
            // Browsers send the RFC 1123 (HTTP-date) form they got back
            // in `Last-Modified`; accept that first, falling back to
            // RFC 3339 for non-browser clients.
            && let Ok(client) =
                crate::time::parse_rfc1123_gmt(ims).or_else(|_| crate::time::parse_rfc3339(ims))
            && let Ok(d) = t.duration_since(std::time::UNIX_EPOCH)
            && client.unix_seconds() >= d.as_secs() as i64
        {
            return not_modified(etag.as_deref().unwrap_or(""), mtime);
        }

        // Read body. For Range requests, only read the slice.
        let range = if self.range_support {
            request.headers.get("range").and_then(parse_range)
        } else {
            None
        };

        let (status, body, content_range): (StatusCode, Vec<u8>, Option<String>) = match range {
            Some((start, end_inclusive)) if start <= end_inclusive && end_inclusive < len => {
                let want = (end_inclusive - start + 1) as usize;
                let mut buf = vec![0u8; want];
                let mut file = match std::fs::File::open(path) {
                    Ok(f) => f,
                    Err(_) => return not_found(),
                };
                use std::io::{Read, Seek, SeekFrom};
                if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut buf).is_err()
                {
                    return not_found();
                }
                let cr = format!("bytes {start}-{end_inclusive}/{len}");
                (StatusCode(206), buf, Some(cr))
            }
            Some(_) => {
                // Unsatisfiable range.
                let mut headers = Headers::new();
                headers.insert("content-range", &format!("bytes */{len}"));
                return Response {
                    status: StatusCode(416),
                    headers,
                    body: Vec::new(),
                    raw_header_pairs: Vec::new(),
                    body_stream: None,
                };
            }
            None => match fs::read(path) {
                Ok(b) => (StatusCode(200), b, None),
                Err(_) => return not_found(),
            },
        };

        let mut headers = Headers::new();
        headers.insert("content-type", mime_for_path(path));
        let cl = body.len().to_string();
        headers.insert("content-length", &cl);
        if self.range_support {
            headers.insert("accept-ranges", "bytes");
        }
        if let Some(cr) = content_range {
            headers.insert("content-range", &cr);
        }
        if self.etag
            && let Some(e) = etag
        {
            headers.insert("etag", &e);
        }
        if self.last_modified
            && let Some(t) = mtime
            && let Ok(stamp) = crate::time::format_rfc1123_gmt(crate::time::SystemTime::from_std(t))
        {
            headers.insert("last-modified", &stamp);
        }
        Response {
            status,
            headers,
            body,
            raw_header_pairs: Vec::new(),
            body_stream: None,
        }
    }
}

fn not_found() -> Response {
    let mut headers = Headers::new();
    headers.insert("content-type", "text/plain; charset=utf-8");
    Response {
        status: StatusCode(404),
        headers,
        body: b"not found".to_vec(),
        raw_header_pairs: Vec::new(),
        body_stream: None,
    }
}

fn not_modified(etag: &str, mtime: Option<std::time::SystemTime>) -> Response {
    let mut headers = Headers::new();
    if !etag.is_empty() {
        headers.insert("etag", etag);
    }
    if let Some(t) = mtime
        && let Ok(stamp) = crate::time::format_rfc1123_gmt(crate::time::SystemTime::from_std(t))
    {
        headers.insert("last-modified", &stamp);
    }
    Response {
        status: StatusCode(304),
        headers,
        body: Vec::new(),
        raw_header_pairs: Vec::new(),
        body_stream: None,
    }
}

fn parse_range(header: &str) -> Option<(u64, u64)> {
    // Accept the single-range form: `bytes=START-END`. The
    // multi-range case (`bytes=0-100,200-300`) is uncommon for
    // static assets and intentionally unsupported in v1.
    let rest = header.trim().strip_prefix("bytes=")?;
    let (start_s, end_s) = rest.split_once('-')?;
    let start: u64 = start_s.trim().parse().ok()?;
    let end: u64 = end_s.trim().parse().ok()?;
    if start > end {
        return None;
    }
    Some((start, end))
}

fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::http::Method;

    fn req(path: &str) -> Request {
        Request {
            method: Method::Get,
            path: path.to_string(),
            query: String::new(),
            headers: Headers::new(),
            body: Vec::new(),
            context: Context::background(),
            trailers: None,
        }
    }

    /// Minimal RAII tempdir without the `tempfile` crate. Each
    /// call returns a fresh directory under the OS temp dir;
    /// the directory is removed when the guard is dropped.
    struct TmpDir(std::path::PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            // Process-global so two tests creating a tmpdir within the
            // same clock tick get distinct names. A per-call local
            // counter is always 0; on coarse-clock platforms
            // (Windows/macOS) parallel tests then collide and one
            // test's Drop deletes another's directory mid-run.
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!("gos-static-{tag}-{pid}-{nanos:x}-{n}"));
            std::fs::create_dir_all(&p).expect("create tmpdir");
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmpdir() -> TmpDir {
        TmpDir::new("test")
    }

    #[test]
    fn serves_existing_file_with_correct_content_type() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("hello.html"), b"<h1>hi</h1>").unwrap();
        let fs = FileServer::new(dir.path());
        let resp = fs.serve_path("hello.html", &req("/hello.html"));
        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"<h1>hi</h1>");
        assert_eq!(
            resp.headers.get("content-type"),
            Some("text/html; charset=utf-8")
        );
        assert!(resp.headers.get("etag").is_some());
        assert!(resp.headers.get("last-modified").is_some());
        assert_eq!(resp.headers.get("accept-ranges"), Some("bytes"));
    }

    #[test]
    fn returns_404_for_missing_file() {
        let dir = tmpdir();
        let fs = FileServer::new(dir.path());
        let resp = fs.serve_path("nonexistent.txt", &req("/x"));
        assert_eq!(resp.status, StatusCode(404));
    }

    #[test]
    fn path_traversal_above_root_is_rejected() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("ok.txt"), b"ok").unwrap();
        let fs = FileServer::new(dir.path());
        let resp = fs.serve_path("../etc/passwd", &req("/x"));
        assert_eq!(resp.status, StatusCode(404));
    }

    #[test]
    fn returns_304_on_if_none_match() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let fs = FileServer::new(dir.path());
        let first = fs.serve_path("a.txt", &req("/x"));
        let etag = first.headers.get("etag").unwrap().to_string();
        let mut r = req("/x");
        r.headers.insert("if-none-match", &etag);
        let second = fs.serve_path("a.txt", &r);
        assert_eq!(second.status, StatusCode(304));
        assert!(second.body.is_empty());
    }

    #[test]
    fn returns_304_on_if_modified_since_rfc1123() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let fs = FileServer::new(dir.path());
        // The server emits Last-Modified in RFC 1123; a browser echoes
        // that exact wire format in If-Modified-Since.
        let first = fs.serve_path("a.txt", &req("/x"));
        let last_modified = first.headers.get("last-modified").unwrap().to_string();
        let mut r = req("/x");
        r.headers.insert("if-modified-since", &last_modified);
        let second = fs.serve_path("a.txt", &r);
        assert_eq!(second.status, StatusCode(304));
        assert!(second.body.is_empty());
    }

    #[test]
    fn rfc1123_is_modified_returns_200_for_older_client_copy() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let fs = FileServer::new(dir.path());
        let mut r = req("/x");
        // A timestamp well in the past: the file is newer, so serve it.
        r.headers
            .insert("if-modified-since", "Sun, 06 Nov 1994 08:49:37 GMT");
        let resp = fs.serve_path("a.txt", &r);
        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"hello");
    }

    #[test]
    fn range_returns_206_with_slice() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("big.bin"), b"abcdefghijklmnopqrstuvwxyz").unwrap();
        let fs = FileServer::new(dir.path());
        let mut r = req("/x");
        r.headers.insert("range", "bytes=2-5");
        let resp = fs.serve_path("big.bin", &r);
        assert_eq!(resp.status, StatusCode(206));
        assert_eq!(resp.body, b"cdef");
        assert_eq!(resp.headers.get("content-range"), Some("bytes 2-5/26"));
        assert_eq!(resp.headers.get("content-length"), Some("4"));
    }

    #[test]
    fn range_beyond_eof_returns_416() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("x.bin"), b"abc").unwrap();
        let fs = FileServer::new(dir.path());
        let mut r = req("/x");
        r.headers.insert("range", "bytes=10-20");
        let resp = fs.serve_path("x.bin", &r);
        assert_eq!(resp.status, StatusCode(416));
        assert_eq!(resp.headers.get("content-range"), Some("bytes */3"));
    }

    #[test]
    fn directory_with_index_html_is_served() {
        let dir = tmpdir();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("index.html"), b"INDEX").unwrap();
        let fs = FileServer::new(dir.path());
        let resp = fs.serve_path("sub", &req("/sub"));
        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"INDEX");
    }

    #[test]
    fn directory_without_index_returns_404() {
        let dir = tmpdir();
        let sub = dir.path().join("empty");
        std::fs::create_dir(&sub).unwrap();
        let fs = FileServer::new(dir.path());
        let resp = fs.serve_path("empty", &req("/empty"));
        assert_eq!(resp.status, StatusCode(404));
    }

    #[test]
    fn oversize_file_returns_404_when_capped() {
        let dir = tmpdir();
        std::fs::write(dir.path().join("big.bin"), vec![0u8; 1024]).unwrap();
        let mut fs = FileServer::new(dir.path());
        fs.max_file_bytes = 100;
        let resp = fs.serve_path("big.bin", &req("/x"));
        assert_eq!(resp.status, StatusCode(404));
    }

    #[test]
    fn mime_extension_table_covers_common_types() {
        assert_eq!(mime_for_path(Path::new("a.css")), "text/css; charset=utf-8");
        assert_eq!(
            mime_for_path(Path::new("a.js")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(mime_for_path(Path::new("a.png")), "image/png");
        assert_eq!(mime_for_path(Path::new("a.svg")), "image/svg+xml");
        assert_eq!(mime_for_path(Path::new("a.wasm")), "application/wasm");
        assert_eq!(
            mime_for_path(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }
}
