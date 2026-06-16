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

use super::*;

// ---------------------------------------------------------------
// ---------------------------------------------------------------
// 0.4.0 HTTP-module bridges - compiled tier stateful + free-fn
// entry points. Matches the interp surface in
// `gossamer_interp::stdlib_builtins::install_http_*`.
// ---------------------------------------------------------------

// Router: stateful Box-allocated handle. Each route stores
// (method, parsed pattern, handler env+fn) so `Router.serve(req)`
// can walk the list and invoke the matching handler via the
// same fn-pointer ABI gos_rt_http_serve uses.

pub struct GosRouter {
    routes: Vec<GosRoute>,
}

struct GosRoute {
    method: String, // empty = any verb
    segments: Vec<RouteSegment>,
    env: usize,
    fn_addr: usize,
    /// `true` when the handler is a bare Gossamer `fn(http::Request) ->
    /// Result<http::Response, http::Error>` registered via
    /// `gos_rt_router_get_fn` (and friends). Dispatch calls the handler
    /// with a single `req` arg, no env. `false` for struct/closure
    /// handlers registered via `gos_rt_router_get`, which use the
    /// `fn(env, req)` closure ABI.
    bare: bool,
}

enum RouteSegment {
    Literal(String),
    Capture(String),    // `{name}` - captures one path segment
    CaptureAll(String), // `{name...}` - captures the rest
}

fn parse_route_pattern(pattern: &str) -> Vec<RouteSegment> {
    let mut out = Vec::new();
    for seg in pattern.split('/').filter(|s| !s.is_empty()) {
        if seg.starts_with('{') && seg.ends_with("...}") {
            out.push(RouteSegment::CaptureAll(seg[1..seg.len() - 4].to_string()));
        } else if seg.starts_with('{') && seg.ends_with('}') {
            out.push(RouteSegment::Capture(seg[1..seg.len() - 1].to_string()));
        } else {
            out.push(RouteSegment::Literal(seg.to_string()));
        }
    }
    out
}

/// Match `path` against a parsed route pattern, collecting `{name}`
/// captures. Returns `Some(params)` on a match (params empty when the
/// pattern is fully literal), `None` when the route does not match.
fn route_segments_match(segments: &[RouteSegment], path: &str) -> Option<Vec<(String, String)>> {
    let path_segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut params: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < segments.len() {
        match &segments[i] {
            RouteSegment::CaptureAll(name) => {
                params.push((name.clone(), path_segs[j..].join("/")));
                return Some(params);
            }
            RouteSegment::Capture(name) => {
                if j >= path_segs.len() {
                    return None;
                }
                params.push((name.clone(), path_segs[j].to_string()));
                i += 1;
                j += 1;
            }
            RouteSegment::Literal(lit) => {
                if j >= path_segs.len() || path_segs[j] != lit {
                    return None;
                }
                i += 1;
                j += 1;
            }
        }
    }
    if j == path_segs.len() {
        Some(params)
    } else {
        None
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_new() -> *mut GosRouter {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosRouter { routes: Vec::new() }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_add(
    router: *mut GosRouter,
    method: *const c_char,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        if router.is_null() {
            return;
        }
        let r = unsafe { &mut *router };
        let m = if method.is_null() {
            String::new()
        } else {
            unsafe {
                CStr::from_ptr(method)
                    .to_string_lossy()
                    .into_owned()
                    .to_ascii_uppercase()
            }
        };
        let pat = if pattern.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(pattern).to_string_lossy().into_owned() }
        };
        let segments = parse_route_pattern(&pat);
        super::fn_registry::register(fn_addr as usize, super::fn_registry::FnKind::HttpHandlerEnv);
        r.routes.push(GosRoute {
            method: m,
            segments,
            env: env as usize,
            fn_addr: fn_addr as usize,
            bare: false,
        });
    });
}

/// `router::add(router, method, pattern)` - registers a handler-less
/// route used purely for `router::lookup` pattern matching (the index
/// of the registered route is what `lookup` returns). `method` empty
/// matches any verb.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_add_pattern(
    router: *mut GosRouter,
    method: *const c_char,
    pattern: *const c_char,
) {
    ffi_entry!((), {
        if router.is_null() {
            return;
        }
        let r = unsafe { &mut *router };
        let m = if method.is_null() {
            String::new()
        } else {
            unsafe {
                CStr::from_ptr(method)
                    .to_string_lossy()
                    .into_owned()
                    .to_ascii_uppercase()
            }
        };
        let pat = if pattern.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(pattern).to_string_lossy().into_owned() }
        };
        let segments = parse_route_pattern(&pat);
        r.routes.push(GosRoute {
            method: m,
            segments,
            env: 0,
            fn_addr: 0,
            bare: false,
        });
    });
}

/// `router::lookup(router, method, path) -> Option<i64>` - the index of
/// the first route whose method (empty = any) and pattern match, packed
/// as the 2-word Option (disc=0 Some, disc=1 None). Mirrors the interp
/// `router::lookup` matching exactly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_lookup(
    router: *const GosRouter,
    method: *const c_char,
    path: *const c_char,
) -> i128 {
    ffi_entry!(unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) }, {
        if router.is_null() {
            return unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) };
        }
        let r = unsafe { &*router };
        let m = if method.is_null() {
            String::new()
        } else {
            unsafe {
                CStr::from_ptr(method)
                    .to_string_lossy()
                    .into_owned()
                    .to_ascii_uppercase()
            }
        };
        let p = if path.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        for (i, route) in r.routes.iter().enumerate() {
            if (route.method.is_empty() || route.method == m)
                && route_segments_match(&route.segments, &p).is_some()
            {
                return unsafe { crate::c_abi::vec::gos_rt_result_new(0, i as i64) };
            }
        }
        unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) }
    })
}

/// Internal helper: bare-fn variant of `gos_rt_router_add`. Used by
/// `gos_rt_router_get_fn` / `_post_fn` / etc. when the registered
/// handler has no env (a top-level `fn`).
unsafe fn router_add_bare(
    router: *mut GosRouter,
    method: *const c_char,
    pattern: *const c_char,
    fn_addr: i64,
) {
    if router.is_null() {
        return;
    }
    let r = unsafe { &mut *router };
    let m = if method.is_null() {
        String::new()
    } else {
        unsafe {
            CStr::from_ptr(method)
                .to_string_lossy()
                .into_owned()
                .to_ascii_uppercase()
        }
    };
    let pat = if pattern.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(pattern).to_string_lossy().into_owned() }
    };
    let segments = parse_route_pattern(&pat);
    super::fn_registry::register(
        fn_addr as usize,
        super::fn_registry::FnKind::HttpHandlerBare,
    );
    r.routes.push(GosRoute {
        method: m,
        segments,
        env: 0,
        fn_addr: fn_addr as usize,
        bare: true,
    });
}

/// Convenience verb-specific entry points that map cleanly to
/// `Router.get(pattern, handler)` etc. in Gossamer source. Spelled
/// out one per verb so the `pub extern "C" fn` line parses through
/// the dispatch-consistency test's source scanner (macro-generated
/// fn names are invisible to a textual scan).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_get(
    router: *mut GosRouter,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("GET").expect("static verb");
        unsafe { gos_rt_router_add(router, verb_c.as_ptr(), pattern, env, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_post(
    router: *mut GosRouter,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("POST").expect("static verb");
        unsafe { gos_rt_router_add(router, verb_c.as_ptr(), pattern, env, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_put(
    router: *mut GosRouter,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("PUT").expect("static verb");
        unsafe { gos_rt_router_add(router, verb_c.as_ptr(), pattern, env, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_delete(
    router: *mut GosRouter,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("DELETE").expect("static verb");
        unsafe { gos_rt_router_add(router, verb_c.as_ptr(), pattern, env, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_patch(
    router: *mut GosRouter,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("PATCH").expect("static verb");
        unsafe { gos_rt_router_add(router, verb_c.as_ptr(), pattern, env, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_head(
    router: *mut GosRouter,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("HEAD").expect("static verb");
        unsafe { gos_rt_router_add(router, verb_c.as_ptr(), pattern, env, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_options(
    router: *mut GosRouter,
    pattern: *const c_char,
    env: *mut u8,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("OPTIONS").expect("static verb");
        unsafe { gos_rt_router_add(router, verb_c.as_ptr(), pattern, env, fn_addr) }
    });
}

/// Bare-fn variants: register a top-level Gossamer `fn(http::Request)
/// -> Result<http::Response, http::Error>` directly as a handler - no
/// env, no struct wrapper. Dispatch invokes the function with the
/// request as its single argument.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_get_fn(
    router: *mut GosRouter,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("GET").expect("static verb");
        unsafe { router_add_bare(router, verb_c.as_ptr(), pattern, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_post_fn(
    router: *mut GosRouter,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("POST").expect("static verb");
        unsafe { router_add_bare(router, verb_c.as_ptr(), pattern, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_put_fn(
    router: *mut GosRouter,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("PUT").expect("static verb");
        unsafe { router_add_bare(router, verb_c.as_ptr(), pattern, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_delete_fn(
    router: *mut GosRouter,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("DELETE").expect("static verb");
        unsafe { router_add_bare(router, verb_c.as_ptr(), pattern, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_patch_fn(
    router: *mut GosRouter,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("PATCH").expect("static verb");
        unsafe { router_add_bare(router, verb_c.as_ptr(), pattern, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_head_fn(
    router: *mut GosRouter,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("HEAD").expect("static verb");
        unsafe { router_add_bare(router, verb_c.as_ptr(), pattern, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_options_fn(
    router: *mut GosRouter,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        let verb_c = std::ffi::CString::new("OPTIONS").expect("static verb");
        unsafe { router_add_bare(router, verb_c.as_ptr(), pattern, fn_addr) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_add_fn(
    router: *mut GosRouter,
    method: *const c_char,
    pattern: *const c_char,
    fn_addr: i64,
) {
    ffi_entry!((), {
        unsafe { router_add_bare(router, method, pattern, fn_addr) }
    });
}

/// Dispatch a request through the router. Walks the route table,
/// invokes the first matching handler via fn-pointer ABI, and
/// returns its `*mut GosResult`. Returns a 404-shaped result when
/// nothing matches.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_router_serve(
    router: *const GosRouter,
    req: *mut GosHttpRequest,
) -> i128 {
    ffi_entry!(0i128, {
        if router.is_null() || req.is_null() {
            return router_404_result();
        }
        let r = unsafe { &*router };
        // Clone the request's path + method so the borrow ends before
        // we write captured params back through the `*mut req`.
        let path = unsafe { (*req).url_path_only().to_string() };
        let method = unsafe { (*req).method.clone() };
        for route in &r.routes {
            if !route.method.is_empty() && !route.method.eq_ignore_ascii_case(&method) {
                continue;
            }
            if let Some(params) = route_segments_match(&route.segments, &path) {
                unsafe { (*req).params = params };
                if route.bare {
                    super::fn_registry::verify(
                        route.fn_addr,
                        super::fn_registry::FnKind::HttpHandlerBare,
                    );
                    type BareFn = unsafe extern "C" fn(req: *mut GosHttpRequest) -> i128;
                    let handler: BareFn = unsafe { std::mem::transmute(route.fn_addr) };
                    return unsafe { handler(req) };
                }
                super::fn_registry::verify(
                    route.fn_addr,
                    super::fn_registry::FnKind::HttpHandlerEnv,
                );
                type HandlerFn =
                    unsafe extern "C" fn(env: *mut u8, req: *mut GosHttpRequest) -> i128;
                let handler: HandlerFn = unsafe { std::mem::transmute(route.fn_addr) };
                return unsafe { handler(route.env as *mut u8, req) };
            }
        }
        router_404_result()
    })
}

fn router_404_result() -> i128 {
    let resp = Box::into_raw(Box::new(GosHttpResponse {
        status: 404,
        body: SyncRawPtr::new(alloc_cstring(b"not found")),
        headers: Vec::new(),
        body_bytes: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        stream_handle: -1,
    }));
    crate::c_abi::vec::pack_result(0, resp as i64)
}

// ---------------------------------------------------------------
// Shared static-file Range (RFC 7233) handling. Both the compiled
// `gos_rt_file_server_serve` shim and the interp-tier
// `native_file_server_serve` native evaluate the request `Range:`
// header and build any `multipart/byteranges` body through these
// helpers, so partial-content responses are bit-identical across
// tiers.
// ---------------------------------------------------------------

/// Fixed `multipart/byteranges` boundary. Static rather than random so
/// a multi-range response is byte-deterministic across tiers; a public
/// server would randomise this, but the cross-tier parity gate requires
/// a stable wire image.
pub const BYTERANGES_BOUNDARY: &str = "gossamer_byteranges_boundary";

/// Outcome of evaluating a `Range:` header against a file length.
pub enum RangeOutcome {
    /// No (parseable) Range header - serve the whole file (200).
    Whole,
    /// One satisfiable range - 206 + Content-Range.
    Single { start: u64, end: u64 },
    /// Several satisfiable ranges - 206 multipart/byteranges.
    Multi(Vec<(u64, u64)>),
    /// A Range header naming no satisfiable range - 416.
    Unsatisfiable,
}

/// Parses an RFC 7233 `Range: bytes=...` header against the file length
/// `len`. Supports `N-M`, open-ended `N-`, and suffix `-N` specs,
/// single or comma-separated. A missing / syntactically invalid header
/// yields `Whole`; a well-formed header whose every spec is out of
/// range yields `Unsatisfiable`.
#[must_use]
pub fn evaluate_range(header: Option<&str>, len: u64) -> RangeOutcome {
    let Some(header) = header else {
        return RangeOutcome::Whole;
    };
    let Some(rest) = header.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Whole;
    };
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut saw_spec = false;
    for spec in rest.split(',') {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        saw_spec = true;
        let Some((start_s, end_s)) = spec.split_once('-') else {
            return RangeOutcome::Whole;
        };
        let (start_s, end_s) = (start_s.trim(), end_s.trim());
        let resolved = if start_s.is_empty() {
            // Suffix form `-N`: the last N bytes.
            match end_s.parse::<u64>() {
                Ok(n) if n > 0 && len > 0 => {
                    let n = n.min(len);
                    Some((len - n, len - 1))
                }
                Ok(_) => None,
                Err(_) => return RangeOutcome::Whole,
            }
        } else {
            match start_s.parse::<u64>() {
                Ok(start) if len > 0 && start < len => {
                    let end = if end_s.is_empty() {
                        len - 1
                    } else {
                        match end_s.parse::<u64>() {
                            Ok(e) => e.min(len - 1),
                            Err(_) => return RangeOutcome::Whole,
                        }
                    };
                    if start <= end {
                        Some((start, end))
                    } else {
                        None
                    }
                }
                Ok(_) => None,
                Err(_) => return RangeOutcome::Whole,
            }
        };
        if let Some(r) = resolved {
            ranges.push(r);
        }
    }
    if !saw_spec {
        return RangeOutcome::Whole;
    }
    match ranges.len() {
        0 => RangeOutcome::Unsatisfiable,
        1 => RangeOutcome::Single {
            start: ranges[0].0,
            end: ranges[0].1,
        },
        _ => RangeOutcome::Multi(ranges),
    }
}

/// Inclusive `[start, end]` slice of `file`, clamped to its bounds.
#[must_use]
pub fn range_slice(file: &[u8], start: u64, end: u64) -> Vec<u8> {
    let s = (start as usize).min(file.len());
    let e = (end as usize).saturating_add(1).min(file.len());
    file[s.min(e)..e].to_vec()
}

/// The `Content-Range` header value for a single 206 range.
#[must_use]
pub fn content_range_value(start: u64, end: u64, total: u64) -> String {
    format!("bytes {start}-{end}/{total}")
}

/// The `Content-Type` for a `multipart/byteranges` response.
#[must_use]
pub fn multipart_content_type() -> String {
    format!("multipart/byteranges; boundary={BYTERANGES_BOUNDARY}")
}

/// Builds the `multipart/byteranges` body for `ranges`.
#[must_use]
pub fn build_multipart_body(file: &[u8], ranges: &[(u64, u64)], mime: &str, total: u64) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for &(start, end) in ranges {
        out.extend_from_slice(b"--");
        out.extend_from_slice(BYTERANGES_BOUNDARY.as_bytes());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(format!("Content-Type: {mime}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Range: bytes {start}-{end}/{total}\r\n\r\n").as_bytes(),
        );
        out.extend_from_slice(&range_slice(file, start, end));
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"--");
    out.extend_from_slice(BYTERANGES_BOUNDARY.as_bytes());
    out.extend_from_slice(b"--\r\n");
    out
}

// FileServer: read-and-serve from a root directory with a path
// prefix strip. Mirrors `static_files::FileServer`'s common case.

pub struct GosFileServer {
    root: String,
    prefix: String,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_file_server_new(
    root: *const c_char,
    prefix: *const c_char,
) -> *mut GosFileServer {
    ffi_entry!(std::ptr::null_mut(), {
        let root_s = if root.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(root).to_string_lossy().into_owned() }
        };
        let prefix_s = if prefix.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(prefix).to_string_lossy().into_owned() }
        };
        Box::into_raw(Box::new(GosFileServer {
            root: root_s,
            prefix: prefix_s,
        }))
    })
}

/// `FileServer.serve(req) -> Result<Response, Error>`. Reads the
/// requested file from disk; rejects path traversal; returns 404
/// when missing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_file_server_serve(
    fs: *const GosFileServer,
    req: *const GosHttpRequest,
) -> i128 {
    ffi_entry!(0i128, {
        if fs.is_null() || req.is_null() {
            return router_404_result();
        }
        let server = unsafe { &*fs };
        let request = unsafe { &*req };
        let path = request.url_path_only();
        let rel = path.strip_prefix(&server.prefix).unwrap_or(path);
        let rel = rel.trim_start_matches('/');
        if rel.contains("..") {
            return crate::c_abi::vec::pack_result(
                0,
                Box::into_raw(Box::new(GosHttpResponse {
                    status: 403,
                    body: SyncRawPtr::new(alloc_cstring(b"forbidden")),
                    headers: Vec::new(),
                    body_bytes: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    stream_handle: -1,
                })) as i64,
            );
        }
        let full = std::path::PathBuf::from(&server.root).join(rel);
        match std::fs::read(&full) {
            Ok(bytes) => {
                let mime = mime_for_path_str(&full.to_string_lossy());
                let total = bytes.len() as u64;
                let range_header = request
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("range"))
                    .map(|(_, v)| v.as_str());
                let (status, body, content_type, mut headers) =
                    match evaluate_range(range_header, total) {
                        RangeOutcome::Whole => (
                            200,
                            bytes,
                            mime.to_string(),
                            vec![("accept-ranges".to_string(), "bytes".to_string())],
                        ),
                        RangeOutcome::Single { start, end } => {
                            let slice = range_slice(&bytes, start, end);
                            (
                                206,
                                slice,
                                mime.to_string(),
                                vec![
                                    ("accept-ranges".to_string(), "bytes".to_string()),
                                    (
                                        "content-range".to_string(),
                                        content_range_value(start, end, total),
                                    ),
                                ],
                            )
                        }
                        RangeOutcome::Multi(ranges) => {
                            let body = build_multipart_body(&bytes, &ranges, mime, total);
                            (
                                206,
                                body,
                                multipart_content_type(),
                                vec![("accept-ranges".to_string(), "bytes".to_string())],
                            )
                        }
                        RangeOutcome::Unsatisfiable => (
                            416,
                            Vec::new(),
                            "text/plain; charset=utf-8".to_string(),
                            vec![("content-range".to_string(), format!("bytes */{total}"))],
                        ),
                    };
                headers.insert(0, ("content-type".to_string(), content_type.clone()));
                let body_cstr = alloc_cstring(&body);
                crate::c_abi::vec::pack_result(
                    0,
                    Box::into_raw(Box::new(GosHttpResponse {
                        status,
                        body: SyncRawPtr::new(body_cstr),
                        headers,
                        body_bytes: Some(body),
                        content_type,
                        stream_handle: -1,
                    })) as i64,
                )
            }
            Err(_) => router_404_result(),
        }
    })
}

/// `static_files::serve_file(path) -> Result<Response, errors::Error>` -
/// one-shot read of a single file into a 200 Response (content-type from
/// the extension), or `Err` when the file cannot be read. Distinct from
/// `FileServer` (no prefix-strip / Range handling); mirrors the interp
/// `static_files::serve_file`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_static_serve_file(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let path_s = if path.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        match std::fs::read(&path_s) {
            Ok(bytes) => {
                let mime = mime_for_path_str(&path_s);
                let body_cstr = alloc_cstring(&bytes);
                crate::c_abi::vec::pack_result(
                    0,
                    Box::into_raw(Box::new(GosHttpResponse {
                        status: 200,
                        body: SyncRawPtr::new(body_cstr),
                        headers: vec![("content-type".to_string(), mime.to_string())],
                        body_bytes: Some(bytes),
                        content_type: mime.to_string(),
                        stream_handle: -1,
                    })) as i64,
                )
            }
            Err(e) => {
                let cs = std::ffi::CString::new(format!("{e}"))
                    .unwrap_or_else(|_| std::ffi::CString::new("read error").expect("NUL-free"));
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                crate::c_abi::vec::pack_result(1, err as i64)
            }
        }
    })
}

fn mime_for_path_str(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

// NativeClient: minimal stateful handle that round-trips through
// `gos_rt_http_get` / a tiny POST helper for the methods callers
// actually use in compiled mode. The full builder surface lives
// in gossamer-std for interp; the compiled handle is intentionally
// thin since most consumers go through `http::get` / `http::Client`.

pub struct GosNativeClient;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_native_client_new() -> *mut GosNativeClient {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosNativeClient))
    })
}

/// `NativeClient.get(url) -> Result<Response, Error>`. Delegates
/// to the existing one-shot GET helper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_native_client_get(
    _client: *const GosNativeClient,
    url: *const c_char,
) -> i128 {
    ffi_entry!(0i128, {
        unsafe { gos_rt_http_get(url, std::ptr::null_mut()) }
    })
}

// Proxy: stateful upstream-URL holder. `Proxy.forward(req)` issues
// a one-shot upstream request and returns the response.

pub struct GosProxy {
    upstream: String,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_proxy_new(upstream: *const c_char) -> *mut GosProxy {
    ffi_entry!(std::ptr::null_mut(), {
        let u = if upstream.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(upstream).to_string_lossy().into_owned() }
        };
        Box::into_raw(Box::new(GosProxy { upstream: u }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_proxy_forward(
    proxy: *const GosProxy,
    req: *const GosHttpRequest,
) -> i128 {
    ffi_entry!(0i128, {
        if proxy.is_null() {
            return router_404_result();
        }
        let p = unsafe { &*proxy };
        let request_path = if req.is_null() {
            "/".to_string()
        } else {
            unsafe { (&*req).url.clone() }
        };
        let full = format!("{}{request_path}", p.upstream.trim_end_matches('/'));
        let url_c = std::ffi::CString::new(full).unwrap_or_default();
        unsafe { gos_rt_http_get(url_c.as_ptr(), std::ptr::null_mut()) }
    })
}

// WebSocket: handshake/frame helpers. Full bidirectional framing
// needs a per-connection state machine that mostly lives in the
// existing gossamer-std `WebSocket` Rust impl; compiled-mode users
// drive it via `accept_key` + manual frame layout for now. The
// accept-key thunk is already declared above (gos_rt_ws_accept_key).
// gos_rt_ws_frame_text - encodes one text frame for outbound use.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ws_frame_text(payload: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if payload.is_null() {
            return alloc_cstring(b"");
        }
        let bytes = unsafe { CStr::from_ptr(payload).to_bytes() };
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 14);
        out.push(0x81); // FIN + text opcode
        let len = bytes.len();
        if len < 126 {
            out.push(len as u8);
        } else if len < 65536 {
            out.push(126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            out.push(127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
        out.extend_from_slice(bytes);
        alloc_cstring(&out)
    })
}

impl GosHttpRequest {
    fn url_path_only(&self) -> &str {
        match self.url.split('?').next() {
            Some(p) => p,
            None => self.url.as_str(),
        }
    }
}

/// chunked::encode - wrap one buffer in HTTP/1.1 chunked
/// transfer-encoding with a single data chunk + terminator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chunked_encode(data: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if data.is_null() {
            return alloc_cstring(b"");
        }
        let bytes = unsafe { CStr::from_ptr(data).to_bytes() };
        let out = format!("{:x}\r\n", bytes.len());
        let mut buf: Vec<u8> = Vec::with_capacity(bytes.len() + out.len() + 7);
        buf.extend_from_slice(out.as_bytes());
        buf.extend_from_slice(bytes);
        buf.extend_from_slice(b"\r\n0\r\n\r\n");
        alloc_cstring(&buf)
    })
}

/// chunked::decode - concat the data chunks from a complete
/// chunked body (trailers discarded).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chunked_decode(data: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if data.is_null() {
            return alloc_cstring(b"");
        }
        let bytes = unsafe { CStr::from_ptr(data).to_bytes() };
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0usize;
        while i < bytes.len() {
            // Read hex chunk size up to CRLF.
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\r' {
                j += 1;
            }
            let line = std::str::from_utf8(&bytes[i..j]).unwrap_or("");
            let size_str = line.split(';').next().unwrap_or(line).trim();
            let Ok(size) = u64::from_str_radix(size_str, 16) else {
                return alloc_cstring(b"");
            };
            // Skip CRLF.
            i = j + 2;
            if size == 0 {
                // Skip trailers up to terminating blank line.
                while i + 1 < bytes.len() && &bytes[i..i + 2] != b"\r\n" {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    i += 1;
                }
                break;
            }
            let take = size as usize;
            if i + take > bytes.len() {
                return alloc_cstring(b"");
            }
            out.extend_from_slice(&bytes[i..i + take]);
            i += take;
            // Skip data-trailing CRLF.
            if i + 1 < bytes.len() {
                i += 2;
            }
        }
        alloc_cstring(&out)
    })
}

/// sse::encode_event(name, data, id) - render one
/// `event:`/`data:` block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sse_encode_event(
    name: *const c_char,
    data: *const c_char,
    id: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let n = if name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
        };
        let d = if data.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(data).to_string_lossy().into_owned() }
        };
        let id_s = if id.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(id).to_string_lossy().into_owned() }
        };
        let mut out = String::new();
        if !id_s.is_empty() {
            out.push_str("id: ");
            out.push_str(&id_s);
            out.push('\n');
        }
        if !n.is_empty() {
            out.push_str("event: ");
            out.push_str(&n);
            out.push('\n');
        }
        for line in d.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        alloc_cstring(out.as_bytes())
    })
}

/// sse::encode_comment - render a `:`-prefixed keepalive line.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sse_encode_comment(text: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let t = if text.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(text).to_string_lossy().into_owned() }
        };
        alloc_cstring(format!(": {t}\n\n").as_bytes())
    })
}

/// sse::encode_retry - render a `retry:` directive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sse_encode_retry(ms: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(format!("retry: {ms}\n\n").as_bytes())
    })
}

/// middleware::new_request_id - process-monotonic id with nanos
/// prefix.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_new_request_id() -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        alloc_cstring(format!("{nanos:x}-{n:x}").as_bytes())
    })
}

/// middleware::accepts_gzip - comma-split the header, look for a
/// gzip token.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_accepts_gzip(header: *const c_char) -> i32 {
    ffi_entry!(-1, {
        if header.is_null() {
            return 0;
        }
        let h = unsafe { CStr::from_ptr(header).to_string_lossy() };
        let accepts = h
            .split(',')
            .any(|tok| tok.trim().eq_ignore_ascii_case("gzip"));
        i32::from(accepts)
    })
}

/// websocket::accept_key - RFC 6455 Sec-WebSocket-Accept
/// derivation: base64(sha1(client_key + GUID)).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ws_accept_key(client_key: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
        if client_key.is_null() {
            return alloc_cstring(b"");
        }
        let k = unsafe { CStr::from_ptr(client_key).to_bytes() };
        let mut input: Vec<u8> = Vec::with_capacity(k.len() + WS_GUID.len());
        input.extend_from_slice(k);
        input.extend_from_slice(WS_GUID);
        let digest = sha1_oneshot(&input);
        let encoded = base64_oneshot(&digest);
        alloc_cstring(encoded.as_bytes())
    })
}

/// static_files::mime_for_path - extension-driven MIME lookup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_static_mime_for_path(path: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if path.is_null() {
            return alloc_cstring(b"application/octet-stream");
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let ext = std::path::Path::new(&p)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mime = match ext.as_str() {
            "html" | "htm" => "text/html; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "js" | "mjs" => "application/javascript",
            "json" => "application/json",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "wasm" => "application/wasm",
            "pdf" => "application/pdf",
            "txt" | "md" => "text/plain; charset=utf-8",
            "xml" => "application/xml",
            _ => "application/octet-stream",
        };
        alloc_cstring(mime.as_bytes())
    })
}

// Minimal sha1 + base64 used by gos_rt_ws_accept_key. Inlined
// here to avoid pulling in another dep - the runtime crate
// stays self-contained for these tiny one-shots.
fn sha1_oneshot(input: &[u8]) -> [u8; 20] {
    // FIPS 180-4 SHA-1.
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded: Vec<u8> = Vec::with_capacity(input.len() + 72);
    padded.extend_from_slice(input);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks_exact(64) {
        let mut w: [u32; 80] = [0; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999_u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1_u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC_u32),
                _ => (b ^ c ^ d, 0xCA62_C1D6_u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64_oneshot(input: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[((b0 & 0b11) << 4 | b1 >> 4) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((b1 & 0b1111) << 2 | b2 >> 6) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b111111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
