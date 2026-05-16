#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicI64, Ordering};

use super::*;

// ---------------------------------------------------------------
// HTTP client — minimal Builder pattern returning Response with
// `status` (i64) + `body` (String). Backed by a small synchronous
// HTTP/1.1 implementation to avoid pulling a TLS stack into the
// runtime crate.
// ---------------------------------------------------------------

pub struct GosHttpClient {
    _placeholder: u8,
}

pub struct GosHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl GosHttpRequest {
    /// Builds a request from h2's parsed `(method, path?query,
    /// headers, body)` tuple. Mirrors the manually-parsed form
    /// `parse_request_into` produces for the h1 path.
    #[must_use]
    pub fn for_h2(
        method: String,
        path_and_query: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            method,
            url: path_and_query,
            headers,
            body,
        }
    }
}

pub struct GosHttpResponse {
    pub status: i64,
    pub body: SyncRawPtr<c_char>,
    pub headers: Vec<(String, String)>,
    /// Raw response bytes. Held so callers can access the binary
    /// payload (image downloads, gzipped content, etc.) via
    /// `resp.raw_bytes` without going through the UTF-8-lossy
    /// `body` field. `None` when the response came from a legacy
    /// constructor that didn't populate this — accessors should
    /// then fall back to the `body` c-string bytes.
    pub body_bytes: Option<Vec<u8>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_client_new() -> *mut GosHttpClient {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosHttpClient { _placeholder: 0 }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_client_get(
    _client: *mut GosHttpClient,
    url: *const c_char,
) -> *mut GosHttpRequest {
    ffi_entry!(std::ptr::null_mut(), {
        let url = if url.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(url).to_string_lossy().into_owned() }
        };
        Box::into_raw(Box::new(GosHttpRequest {
            method: "GET".to_string(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_client_post(
    _client: *mut GosHttpClient,
    url: *const c_char,
) -> *mut GosHttpRequest {
    ffi_entry!(std::ptr::null_mut(), {
        let url = if url.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(url).to_string_lossy().into_owned() }
        };
        Box::into_raw(Box::new(GosHttpRequest {
            method: "POST".to_string(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_header(
    req: *mut GosHttpRequest,
    name: *const c_char,
    value: *const c_char,
) -> *mut GosHttpRequest {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() {
            return req;
        }
        let n = if name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
        };
        let v = if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
        };
        unsafe { (*req).headers.push((n, v)) };
        req
    })
}

/// Mutating header insert used by the chained `req.headers.insert`
/// lowering (return-by-receiver kept off so the call has no value).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_set_header(
    req: *mut GosHttpRequest,
    name: *const c_char,
    value: *const c_char,
) {
    ffi_entry!((), {
        if req.is_null() {
            return;
        }
        let n = if name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
        };
        let v = if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
        };
        let req = unsafe { &mut *req };
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&n));
        req.headers.push((n, v));
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_get_header(
    req: *const GosHttpRequest,
    name: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() || name.is_null() {
            return alloc_cstring(b"");
        }
        let n = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
        let req = unsafe { &*req };
        let found = req
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&n))
            .map_or(String::new(), |(_, v)| v.clone());
        alloc_cstring(found.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_body(
    req: *mut GosHttpRequest,
    body: *const c_char,
) -> *mut GosHttpRequest {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() {
            return req;
        }
        let b = if body.is_null() {
            Vec::new()
        } else {
            unsafe { CStr::from_ptr(body).to_bytes().to_vec() }
        };
        unsafe { (*req).body = b };
        req
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_send(
    req: *mut GosHttpRequest,
) -> *mut GosHttpResponse {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() {
            return Box::into_raw(Box::new(GosHttpResponse {
                status: 0,
                body: SyncRawPtr::new(alloc_cstring(b"")),
                headers: Vec::new(),
                body_bytes: Some(Vec::new()),
            }));
        }
        let req = unsafe { Box::from_raw(req) };
        let (status, body_bytes) = http_request_ureq(&req).unwrap_or((0, Vec::new()));
        let body = alloc_cstring(&body_bytes);
        Box::into_raw(Box::new(GosHttpResponse {
            status,
            body: SyncRawPtr::new(body),
            headers: Vec::new(),
            body_bytes: Some(body_bytes),
        }))
    })
}

fn http_request_ureq(req: &GosHttpRequest) -> Option<(i64, Vec<u8>)> {
    if req.method.eq_ignore_ascii_case("GET") && req.headers.is_empty() && req.body.is_empty() {
        return http_get_follow_redirects(&req.url).ok();
    }
    None
}

fn http_get_follow_redirects(url: &str) -> Result<(i64, Vec<u8>), String> {
    let mut current = url.to_string();
    for _ in 0..6 {
        let (status, body, location) = if current.starts_with("https://") {
            http_get_tls(&current)?
        } else {
            http_get_plain(&current)?
        };
        if !(300..=399).contains(&status) || location.is_empty() {
            return Ok((status, body));
        }
        current = absolute_redirect(&current, &location);
    }
    Err(format!("too many redirects fetching `{url}`"))
}

fn absolute_redirect(from: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let scheme_end = from.find("://").map_or(0, |i| i + 3);
    let host_end = from[scheme_end..]
        .find('/')
        .map_or(from.len(), |i| scheme_end + i);
    if location.starts_with('/') {
        format!("{}{}", &from[..host_end], location)
    } else {
        format!("{}/{}", &from[..host_end], location)
    }
}

fn http_get_tls(url: &str) -> Result<(i64, Vec<u8>, String), String> {
    use gossamer_pkg::transport::{HttpsTransport, Transport};

    let transport = HttpsTransport::new_mozilla_roots();
    let body = transport.get(url).map_err(|e| format!("{e}"))?;
    Ok((200, body, String::new()))
}

fn http_get_plain(url: &str) -> Result<(i64, Vec<u8>, String), String> {
    let (host, path) = parse_http_get_url(url).ok_or_else(|| format!("unsupported URL: {url}"))?;
    let (host_part, port) = match host.split_once(':') {
        Some((h, p)) => (h, p),
        None => (host.as_str(), "80"),
    };
    let port_num = port
        .parse::<u16>()
        .map_err(|_| format!("bad port in URL: {url}"))?;
    let mut stream = connect_host_port(host_part, port_num)
        .map_err(|e| format!("connect {host_part}:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_part}\r\nUser-Agent: gos/{version}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        version = env!("CARGO_PKG_VERSION"),
    );
    std::io::Write::write_all(&mut stream, request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut response = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut response).map_err(|e| format!("read: {e}"))?;
    let response_str = String::from_utf8_lossy(&response);
    let Some((header_block, body)) = response_str.split_once("\r\n\r\n") else {
        return Err("invalid HTTP response".to_string());
    };
    let status_line = header_block.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let mut location = String::new();
    for hline in header_block.lines().skip(1) {
        if let Some((name, value)) = hline.split_once(':')
            && name.trim().eq_ignore_ascii_case("location")
        {
            location = value.trim().to_string();
            break;
        }
    }
    Ok((status, body.as_bytes().to_vec(), location))
}

fn parse_http_get_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h.to_string(), format!("/{p}")),
        None => (rest.to_string(), "/".to_string()),
    };
    Some((host, path))
}

#[cfg(unix)]
fn connect_host_port(host: &str, port: u16) -> std::io::Result<std::net::TcpStream> {
    use std::mem::MaybeUninit;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let host_c = std::ffi::CString::new(host)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "host contains NUL"))?;
    let port_c = std::ffi::CString::new(port.to_string())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "port contains NUL"))?;
    let hints = MaybeUninit::<libc::addrinfo>::zeroed();
    // SAFETY: zeroed `addrinfo` is a valid base to fill selected fields.
    let mut hints = unsafe { hints.assume_init() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut out: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: pointers stay valid for the call; `out` is written by libc.
    let rc = unsafe {
        libc::getaddrinfo(
            host_c.as_ptr(),
            port_c.as_ptr(),
            &raw const hints,
            &raw mut out,
        )
    };
    if rc != 0 {
        let msg = unsafe { CStr::from_ptr(libc::gai_strerror(rc)) }
            .to_string_lossy()
            .into_owned();
        return Err(std::io::Error::other(msg));
    }
    let mut cursor = out;
    let mut last_err = None;
    while !cursor.is_null() {
        // SAFETY: `cursor` comes from the valid `addrinfo` chain returned by libc.
        let ai = unsafe { &*cursor };
        let addr = match ai.ai_family {
            libc::AF_INET => {
                // SAFETY: ai_family says this is `sockaddr_in`.
                let sin = unsafe { &*(ai.ai_addr.cast::<libc::sockaddr_in>()) };
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                Some(SocketAddr::new(IpAddr::V4(ip), u16::from_be(sin.sin_port)))
            }
            libc::AF_INET6 => {
                // SAFETY: ai_family says this is `sockaddr_in6`.
                let sin6 = unsafe { &*(ai.ai_addr.cast::<libc::sockaddr_in6>()) };
                let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                Some(SocketAddr::new(
                    IpAddr::V6(ip),
                    u16::from_be(sin6.sin6_port),
                ))
            }
            _ => None,
        };
        if let Some(addr) = addr {
            match std::net::TcpStream::connect(addr) {
                Ok(stream) => {
                    // SAFETY: `out` was allocated by libc on successful `getaddrinfo`.
                    unsafe { libc::freeaddrinfo(out) };
                    return Ok(stream);
                }
                Err(err) => last_err = Some(err),
            }
        }
        cursor = ai.ai_next;
    }
    // SAFETY: `out` was allocated by libc on successful `getaddrinfo`.
    unsafe { libc::freeaddrinfo(out) };
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no socket addresses resolved")))
}

#[cfg(not(unix))]
fn connect_host_port(host: &str, port: u16) -> std::io::Result<std::net::TcpStream> {
    std::net::TcpStream::connect((host, port))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_query(req: *const GosHttpRequest) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() {
            return alloc_cstring(b"");
        }
        // Naive query extraction: everything after the first `?`
        // in the URL (without the leading `?`).
        let url = &unsafe { &*req }.url;
        if let Some(pos) = url.find('?') {
            alloc_cstring(&url.as_bytes()[pos + 1..])
        } else {
            alloc_cstring(b"")
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_body_str(req: *const GosHttpRequest) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() {
            return alloc_cstring(b"");
        }
        alloc_cstring(&unsafe { &*req }.body)
    })
}

/// Returns the request's URL path (the part after the host).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_path(req: *const GosHttpRequest) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() {
            return alloc_cstring(b"");
        }
        let r = unsafe { &*req };
        let path = if let Some(rest) = r
            .url
            .strip_prefix("http://")
            .or_else(|| r.url.strip_prefix("https://"))
        {
            match rest.find('/') {
                Some(i) => &rest[i..],
                None => "/",
            }
        } else {
            r.url.as_str()
        };
        alloc_cstring(path.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_method(req: *const GosHttpRequest) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if req.is_null() {
            return alloc_cstring(b"");
        }
        alloc_cstring(unsafe { &*req }.method.as_bytes())
    })
}

/// Constructs a 200-style text response. Writes into the
/// per-thread response buffer (`RESPONSE_BUF`) — the previous
/// `Box::into_raw` per request was the dominant overhead at
/// conc=100. The body pointer is stored verbatim: it's already
/// valid arena/static memory (string literals live for the
/// program; `format!()` output lives until the next
/// `gos_rt_gc_reset`, which runs *after* the response is written
/// to the socket). Skipping the copy removes another two
/// allocations per request.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_text_new(
    status: i64,
    body: *const c_char,
) -> *mut GosHttpResponse {
    ffi_entry!(std::ptr::null_mut(), {
        // Box-allocate per request rather than reusing a per-thread
        // buffer. The thread-local optimization saved a malloc/free
        // pair, but exposed a subtle aliasing hazard under concurrent
        // load: when many connection threads exit in rapid succession,
        // the TLS-owned `headers: Vec<(String, String)>` had its drop
        // path running concurrently with whatever code happened to be
        // using the response pointer. Switching to Box::into_raw +
        // Box::from_raw makes ownership explicit — `drop_handler_result`
        // is the unique reclaim site.
        Box::into_raw(Box::new(GosHttpResponse {
            status,
            body: SyncRawPtr::new(body.cast_mut()),
            headers: Vec::new(),
            body_bytes: None,
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_json_new(
    status: i64,
    body: *const c_char,
) -> *mut GosHttpResponse {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe { gos_rt_http_response_text_new(status, body) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_status(resp: *const GosHttpResponse) -> i64 {
    ffi_entry!(-1, {
        if resp.is_null() {
            return 0;
        }
        unsafe { (*resp).status }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_body(resp: *const GosHttpResponse) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if resp.is_null() {
            return alloc_cstring(b"");
        }
        unsafe { (*resp).body.as_ptr() }
    })
}

/// Returns the raw response bytes as a freshly allocated `Vec<u8>`
/// (`*mut GosVec`), preserving the exact transport bytes including
/// non-UTF-8 sequences. Mirrors the interp tier's `resp.raw_bytes`
/// field used by binary-download callers (image fetch, gzip body,
/// etc.). When the response was built without bytes (`text_new` /
/// `json_new`), falls back to the c-string bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_raw_bytes(
    resp: *const GosHttpResponse,
) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes: Vec<u8> = if resp.is_null() {
            Vec::new()
        } else if let Some(stored) = unsafe { &(*resp).body_bytes } {
            stored.clone()
        } else {
            let cstr_ptr = unsafe { (*resp).body.as_ptr() };
            if cstr_ptr.is_null() {
                Vec::new()
            } else {
                unsafe { CStr::from_ptr(cstr_ptr).to_bytes().to_vec() }
            }
        };
        // Allocate the GosVec with capacity for all bytes and write
        // them directly into the backing buffer. The previous
        // per-byte `gos_rt_vec_push` path went through the slow
        // growth loop and triggered the JIT-cached helper's
        // single-shot-byte memcpy — which on some lowering paths
        // received `&b as *const u8` from a transient stack frame
        // that was clobbered between iterations, producing a
        // truncated 2-byte vec. Bulk memcpy is also simpler.
        let len_i64 = bytes.len() as i64;
        let v = unsafe { crate::c_abi::vec::gos_rt_vec_with_capacity(1, len_i64) };
        if !bytes.is_empty() {
            let vec_ref = unsafe { &mut *v };
            if !vec_ref.ptr.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        vec_ref.ptr.as_ptr(),
                        bytes.len(),
                    );
                }
                vec_ref.len = len_i64;
            }
        }
        v
    })
}

/// Sets `Header: Value` on a response, replacing any prior value
/// with the same case-insensitive name. Used by the chained
/// `r.headers.insert(name, value)` lowering.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_set_header(
    resp: *mut GosHttpResponse,
    name: *const c_char,
    value: *const c_char,
) {
    ffi_entry!((), {
        if resp.is_null() {
            return;
        }
        let n = if name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
        };
        let v = if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
        };
        let resp = unsafe { &mut *resp };
        resp.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&n));
        resp.headers.push((n, v));
    });
}

/// Reads `Header` value from a response, empty string when absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_get_header(
    resp: *const GosHttpResponse,
    name: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if resp.is_null() || name.is_null() {
            return alloc_cstring(b"");
        }
        let n = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
        let resp = unsafe { &*resp };
        let found = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&n))
            .map_or(String::new(), |(_, v)| v.clone());
        alloc_cstring(found.as_bytes())
    })
}

// ---------------------------------------------------------------
// http::stream — POST/GET that returns a line-by-line body reader
// keyed by an integer handle. Mirrors the interp's
// `builtin_http_stream` shape: the call returns a
// `Result<ResponseStream, errors::Error>` whose Ok payload is a
// 3-slot heap aggregate `[__handle: i64, status: i64, content_type:
// *c_char]`. Subsequent `.next_line()` calls dispatch to
// `gos_rt_http_stream_next_line`.
//
// Implementation: the wire reader stays open across FFI calls by
// living inside a process-global `Mutex<HashMap<i64, BufReader>>`
// keyed by the handle stashed in the ResponseStream blob.
// `next_line` calls `read_line` on the held reader so SSE bodies
// stream live (askq's `[thinking…]` dots arrive token-by-token
// rather than after the full LLM completion buffers up).
// ---------------------------------------------------------------

type StreamReader = std::io::BufReader<Box<dyn std::io::Read + Send + Sync>>;

static STREAM_REGISTRY: parking_lot::Mutex<
    Option<rustc_hash::FxHashMap<i64, std::sync::Arc<parking_lot::Mutex<StreamReader>>>>,
> = parking_lot::Mutex::new(None);
static NEXT_STREAM_HANDLE: AtomicI64 = AtomicI64::new(1);

fn stream_registry_register(reader: StreamReader) -> i64 {
    let handle = NEXT_STREAM_HANDLE.fetch_add(1, Ordering::SeqCst);
    let mut guard = STREAM_REGISTRY.lock();
    let map = guard.get_or_insert_with(rustc_hash::FxHashMap::default);
    map.insert(handle, std::sync::Arc::new(parking_lot::Mutex::new(reader)));
    handle
}

fn stream_registry_lookup(handle: i64) -> Option<std::sync::Arc<parking_lot::Mutex<StreamReader>>> {
    let guard = STREAM_REGISTRY.lock();
    guard.as_ref()?.get(&handle).cloned()
}

fn stream_registry_drop(handle: i64) {
    let mut guard = STREAM_REGISTRY.lock();
    if let Some(map) = guard.as_mut() {
        map.remove(&handle);
    }
}

/// Builds a 3-slot ResponseStream blob `[__handle, status,
/// content_type]`. Field order matches `stdlib_struct_shapes`.
/// Box-allocated so the pointer outlives any LLVM
/// `arena_save`/`arena_restore` window the caller's compiled code
/// emits — see fix_architecture_ownership.md Stage 4.
fn alloc_response_stream_blob(handle: i64, status: i64, content_type: &str) -> *mut i64 {
    let ct_cs = alloc_cstring(content_type.as_bytes()) as i64;
    Box::into_raw(Box::new([handle, status, ct_cs])).cast::<i64>()
}

fn err_result_with_msg(msg: &str) -> *mut GosResult {
    let cs = std::ffi::CString::new(msg).unwrap_or_default();
    let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
    gos_rt_result_new(1, err as i64)
}

/// `http::get(url, headers) -> Result<http::Response, errors::Error>`.
/// One-shot GET. Ok payload is a `*mut GosHttpResponse` so field
/// access (`r.status`, `r.body`) routes through the existing
/// `gos_rt_http_response_*` dispatch.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_http_get(url: *const c_char, headers: *mut GosVec) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        let url_str = if url.is_null() {
            return unsafe { err_result_with_msg("http::get: url is null") };
        } else {
            unsafe { CStr::from_ptr(url).to_string_lossy().into_owned() }
        };
        let mut header_pairs: Vec<(String, String)> = Vec::new();
        if !headers.is_null() {
            let v = unsafe { &*headers };
            let elem_bytes = v.elem_bytes as usize;
            if elem_bytes != 0 && !v.ptr.is_null() {
                for i in 0..v.len {
                    let slot = unsafe { v.ptr.add((i as usize) * elem_bytes) };
                    let key_ptr = unsafe { (slot as *const *const c_char).read_unaligned() };
                    let val_ptr = unsafe { (slot.add(8) as *const *const c_char).read_unaligned() };
                    let key = if key_ptr.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(key_ptr).to_string_lossy().into_owned() }
                    };
                    let val = if val_ptr.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(val_ptr).to_string_lossy().into_owned() }
                    };
                    header_pairs.push((key, val));
                }
            }
        }
        let agent = ureq::config::Config::builder()
            .http_status_as_error(false)
            .timeout_connect(Some(std::time::Duration::from_secs(15)))
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .new_agent();
        let mut req = agent.get(&url_str);
        for (k, v) in &header_pairs {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(e) => return unsafe { err_result_with_msg(&format!("http::get: {e}")) },
        };
        let status = i64::from(resp.status().as_u16());
        let mut hdrs: Vec<(String, String)> = Vec::new();
        for (name, value) in resp.headers() {
            hdrs.push((
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            ));
        }
        let body_bytes = {
            use std::io::Read;
            let mut buf: Vec<u8> = Vec::new();
            let mut reader = resp.into_body().into_reader();
            if let Err(e) = reader.read_to_end(&mut buf) {
                return unsafe { err_result_with_msg(&format!("http::get: read body: {e}")) };
            }
            buf
        };
        // `body` is a UTF-8-lossy c-string view so existing text-shaped
        // `.body` callers continue to work. Binary payloads (images,
        // gzip, …) flow through `.raw_bytes` which preserves the
        // exact source bytes.
        let body_cs = alloc_cstring(body_bytes.as_slice());
        let resp_box = Box::into_raw(Box::new(GosHttpResponse {
            status,
            body: SyncRawPtr::new(body_cs),
            headers: hdrs,
            body_bytes: Some(body_bytes),
        }));
        gos_rt_result_new(0, resp_box as i64)
    })
}

/// `http::stream(method, url, body, headers) -> Result<ResponseStream, errors::Error>`.
///
/// `headers` is a `Vec<(String, String)>` whose backing storage is
/// a tight array of 16-byte tuples `(*c_char, *c_char)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_stream(
    method: *const c_char,
    url: *const c_char,
    body: *const c_char,
    headers: *mut GosVec,
) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        let method_str = if method.is_null() {
            "GET".to_string()
        } else {
            unsafe { CStr::from_ptr(method).to_string_lossy().into_owned() }
        };
        let url_str = if url.is_null() {
            return unsafe { err_result_with_msg("http::stream: url is null") };
        } else {
            unsafe { CStr::from_ptr(url).to_string_lossy().into_owned() }
        };
        let body_str = if body.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(body).to_string_lossy().into_owned() }
        };
        let mut header_pairs: Vec<(String, String)> = Vec::new();
        if !headers.is_null() {
            let v = unsafe { &*headers };
            let elem_bytes = v.elem_bytes as usize;
            if elem_bytes != 0 && !v.ptr.is_null() {
                for i in 0..v.len {
                    let slot = unsafe { v.ptr.add((i as usize) * elem_bytes) };
                    // Each tuple slot is two i64-shaped pointers laid
                    // out back-to-back: key at +0, value at +8.
                    let key_ptr = unsafe { (slot as *const *const c_char).read_unaligned() };
                    let val_ptr = unsafe { (slot.add(8) as *const *const c_char).read_unaligned() };
                    let key = if key_ptr.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(key_ptr).to_string_lossy().into_owned() }
                    };
                    let val = if val_ptr.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(val_ptr).to_string_lossy().into_owned() }
                    };
                    header_pairs.push((key, val));
                }
            }
        }

        // Build an agent with no read timeout — SSE / chunked
        // chat-completion bodies can have multi-second gaps between
        // tokens (askq's reasoning phase) and the default 30s read
        // timeout would tear the connection mid-stream.
        // http_status_as_error(false) so 4xx/5xx bodies are surfaced
        // to the caller as a live ResponseStream rather than dropped.
        let agent = ureq::config::Config::builder()
            .http_status_as_error(false)
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .build()
            .new_agent();
        let mut builder = ureq::http::Request::builder()
            .method(method_str.as_str())
            .uri(url_str.as_str());
        for (k, v) in &header_pairs {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let body_bytes = if body_str.is_empty() {
            Vec::new()
        } else {
            body_str.into_bytes()
        };
        let request = match builder.body(body_bytes) {
            Ok(r) => r,
            Err(e) => {
                return unsafe {
                    err_result_with_msg(&format!("http::stream: build request: {e}"))
                };
            }
        };
        let resp = match agent.run(request) {
            Ok(r) => r,
            Err(e) => return unsafe { err_result_with_msg(&format!("http::stream: {e}")) },
        };
        let status = i64::from(resp.status().as_u16());
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .to_string();
        let reader = std::io::BufReader::new(
            Box::new(resp.into_body().into_reader()) as Box<dyn std::io::Read + Send + Sync>
        );
        let handle = stream_registry_register(reader);
        let blob = unsafe { alloc_response_stream_blob(handle, status, &content_type) };
        if blob.is_null() {
            stream_registry_drop(handle);
            return unsafe { err_result_with_msg("http::stream: arena alloc failed") };
        }
        unsafe { gos_rt_result_new(0, blob as i64) }
    })
}

/// `ResponseStream::next_line() -> Option<String>`.
///
/// `rs` points at the 3-slot blob `[handle, status, content_type]`
/// returned by `gos_rt_http_stream`. Returns a `*mut GosResult`
/// shaped as `Option<String>` (disc 0 = Some, 1 = None). EOF or
/// I/O failure drops the stream from the registry and returns
/// None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_stream_next_line(rs: *const i64) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        if rs.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let handle = unsafe { *rs };
        let Some(arc) = stream_registry_lookup(handle) else {
            return unsafe { gos_rt_result_new(1, 0) };
        };
        use std::io::BufRead;
        let mut buf = String::new();
        let read_result = arc.lock().read_line(&mut buf);
        match read_result {
            Ok(0) => {
                stream_registry_drop(handle);
                unsafe { gos_rt_result_new(1, 0) }
            }
            Ok(_) => {
                if buf.ends_with('\n') {
                    buf.pop();
                    if buf.ends_with('\r') {
                        buf.pop();
                    }
                }
                let cs = alloc_cstring(buf.as_bytes()) as i64;
                unsafe { gos_rt_result_new(0, cs) }
            }
            Err(_) => {
                stream_registry_drop(handle);
                unsafe { gos_rt_result_new(1, 0) }
            }
        }
    })
}
