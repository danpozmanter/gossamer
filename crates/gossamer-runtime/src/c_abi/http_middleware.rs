#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::wildcard_imports)]

//! Compiled-tier `http::middleware::bearer_ok` - the minimal
//! cross-tier auth-check middleware primitive.
//!
//! `bearer_ok(req, verify)` extracts the `Authorization: Bearer
//! <token>` token from a request and runs the caller's Gossamer
//! `verify` closure on it, returning the closure's bool. A missing or
//! non-`Bearer` Authorization header returns `false` WITHOUT invoking
//! the closure (so the closure only ever sees a real token). This is
//! the Go `r.BasicAuth() -> (…, ok)` ok-bool idiom, specialised to
//! bearer tokens: the response shaping (401 vs handler body) stays in
//! Gossamer code, which keeps it trivially bit-identical across tiers.
//!
//! Closure ABI: the `verify` closure crosses the C-ABI through the
//! same env-thunk convention used by the `iter::*` / `Once::call` /
//! `RwLock::with_read` combinators - `env[0]` is the callable address,
//! and the body is invoked as `f(env, token)` where `token` is a
//! freshly allocated gos String pointer (the `String`-parameter
//! lowering treats it exactly as a `String`). Bit-identical to the
//! interp `native_bearer_ok` in
//! `gossamer_interp::stdlib_builtins::http_middleware_bearer`.

use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// `fn(env, token) -> bool` - the one-argument value-thunk shape, with
/// the closure's `String` parameter carried as a gos C-string pointer
/// (pointer-width, same slot the `i64` combinator thunks use).
type VerifyFn = unsafe extern "C" fn(env: *const u8, token: *mut std::os::raw::c_char) -> i64;

/// Callable address stored at `env[0]`, or `None` for a null/zero env.
fn env_fn_addr(env: *const u8) -> Option<*const ()> {
    if env.is_null() {
        return None;
    }
    // SAFETY: `env` is a live closure blob whose first word is the
    // callable address (codegen invariant shared with the combinator
    // and `iter::*` families).
    let addr = unsafe { (env.cast::<usize>()).read() };
    if addr == 0 {
        None
    } else {
        // Recover the address's exposed provenance so the pointer is
        // sound to call under strict provenance; a bare integer
        // transmute at the call site would carry none.
        Some(std::ptr::with_exposed_provenance::<()>(addr))
    }
}

/// Bearer token from an `Authorization` header value, or `None` when
/// the scheme is absent or not `Bearer` (case-insensitive). Shared
/// shape with the interp `bearer_token` so both tiers split the header
/// identically.
fn bearer_token(auth: &str) -> Option<String> {
    let (scheme, rest) = auth.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(rest.trim().to_string())
    } else {
        None
    }
}

/// `http::middleware::bearer_ok(req, verify) -> bool`. Returns whether
/// the request carries a `Bearer` token the `verify` closure accepts.
/// `false` (without calling `verify`) when no bearer header is present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_bearer_ok(req: *const GosHttpRequest, env: *const u8) -> i64 {
    ffi_entry!(0, {
        if req.is_null() {
            return 0;
        }
        let r = unsafe { &*req };
        let Some(auth) = r
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str())
        else {
            return 0;
        };
        let Some(token) = bearer_token(auth) else {
            return 0;
        };
        let Some(addr) = env_fn_addr(env) else {
            return 0;
        };
        // SAFETY: addr is the callable stored by the closure lowering;
        // a one-`String`-argument closure lowers to the
        // `fn(env, *mut c_char) -> i64` value-thunk shape.
        let f: VerifyFn = unsafe { std::mem::transmute(addr) };
        let token_cs = alloc_cstring(token.as_bytes());
        i64::from(unsafe { f(env, token_cs) } != 0)
    })
}

// ---------------------------------------------------------------
// Middleware composition `fn(inner: Handler) -> Handler` (Go-style
// wrap-and-return). A `GosMiddleware` handle carries the inner
// handler's env pointer and its serve fn-address; `gos_rt_middleware_serve`
// is itself a `HandlerFn` so `http::serve` calls it exactly like a
// struct handler's `::serve`. It runs the inner serve, then applies the
// deterministic response transform (prepend `mw:` to the body) so the
// composition is observable bit-identically across tiers. Chaining works
// because a chained middleware's inner serve address is itself
// `gos_rt_middleware_serve` and its inner env the nested handle.
// ---------------------------------------------------------------

/// Handler ABI shared with `gos_rt_http_serve`: `(env, request) -> packed
/// Result<Response, Error>`.
type HandlerAbi = unsafe extern "C" fn(env: *mut u8, req: *mut GosHttpRequest) -> i128;

/// A composed-middleware handle: the wrapped handler's env pointer, its
/// serve fn-address, which transform to apply, and that transform's
/// configuration string.
pub struct GosMiddleware {
    inner_env: i64,
    inner_serve_addr: i64,
    kind: i64,
    config: String,
}

/// Transform selector shared with `gossamer_std::http_middleware::kind`
/// and the interpreter's middleware table. The numbering is the ABI
/// between the MIR lowering and this runtime; append only.
pub mod middleware_kind {
    /// Prepend `mw:` to the body - the deterministic composition probe.
    pub const TAG: i64 = 0;
    /// Stamp `X-Request-Id` on the response.
    pub const REQUEST_ID: i64 = 1;
    /// CORS response headers; config is `origin|methods|headers|max_age`.
    pub const CORS: i64 = 2;
    /// Baseline security headers; config is the preset name.
    pub const SECURITY_HEADERS: i64 = 3;
    /// Strong `ETag` derived from the response body.
    pub const ETAG: i64 = 4;
    /// Token-bucket limiter; config is `capacity|refill_per_sec`.
    pub const RATE_LIMIT: i64 = 5;
    /// `Strict-Transport-Security`; config is the header value.
    pub const HSTS: i64 = 6;
    /// `Cache-Control`; config is the header value.
    pub const CACHE_CONTROL: i64 = 7;
    /// Reject bodies larger than the configured byte budget.
    pub const BODY_LIMIT: i64 = 8;
    /// Advertise gzip support via `Vary: Accept-Encoding`.
    pub const COMPRESS_GZIP: i64 = 9;
    /// One request line per response on stderr.
    pub const LOGGER: i64 = 10;
    /// Turn a handler `Err` into a 500 response.
    pub const RECOVERER: i64 = 11;
    /// Stamp the configured budget as `X-Timeout-Ms`.
    pub const TIMEOUT: i64 = 12;
    /// `WWW-Authenticate: Basic` on an unauthenticated response.
    pub const BASIC_AUTH: i64 = 13;
    /// `WWW-Authenticate: Bearer` on an unauthenticated response.
    pub const BEARER_AUTH: i64 = 14;
    /// Security headers plus HSTS plus a request id, in one wrapper.
    pub const SAFE_DEFAULTS: i64 = 15;
}

/// `middleware::<wrap>(inner) -> Handler` handle constructor. `inner_env`
/// is the wrapped handler's env pointer (a struct handle or a nested
/// `GosMiddleware`); `inner_serve_addr` is its serve fn-address resolved
/// at the call site via `gos_fn_addr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_middleware_new(
    inner_env: i64,
    inner_serve_addr: i64,
) -> *mut GosMiddleware {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosMiddleware {
            inner_env,
            inner_serve_addr,
            kind: middleware_kind::TAG,
            config: String::new(),
        }))
    })
}

/// `middleware::<name>(inner, config) -> Handler` handle constructor for
/// every transform beyond `tag`. `kind` selects the transform and
/// `config` carries its knobs as one deterministic string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_middleware_new_kind(
    inner_env: i64,
    inner_serve_addr: i64,
    kind: i64,
    config: *const std::os::raw::c_char,
) -> *mut GosMiddleware {
    ffi_entry!(std::ptr::null_mut(), {
        let config = if config.is_null() {
            String::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_string(config) }
        };
        Box::into_raw(Box::new(GosMiddleware {
            inner_env,
            inner_serve_addr,
            kind,
            config,
        }))
    })
}

// --- Response transforms -------------------------------------------
//
// Behavioural mirror of `gossamer_std::http_middleware`'s transform
// section. The crate graph keeps `gossamer-std` out of the runtime, so
// the logic is duplicated rather than shared; the two must stay
// identical or a chain answers differently under `gos` and `gos build`.

/// The mutable parts of a response a middleware transform may rewrite.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResponseParts {
    /// HTTP status code.
    pub status: i64,
    /// Response body bytes.
    pub body: Vec<u8>,
    /// Response headers in emission order.
    pub headers: Vec<(String, String)>,
}

/// Process-monotonic request counter behind `request_id`. Counting rather
/// than sampling a clock keeps a fixture's output identical on every tier.
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

/// Next request id, formatted `req-<n>` from a process-monotonic counter.
/// Counting keeps a chain's output identical on every tier.
pub fn sequential_request_id() -> String {
    format!("req-{}", REQUEST_SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}

/// Remaining tokens per rate-limit configuration, keyed by the config
/// string so two limiters with different budgets never share a bucket.
static BUCKETS: parking_lot::Mutex<Option<std::collections::HashMap<String, i64>>> =
    parking_lot::Mutex::new(None);

/// Consumes one token from the bucket named by `config`
/// (`capacity|refill_per_sec`); false when the budget is exhausted.
pub fn rate_limit_allow(config: &str) -> bool {
    let capacity = config
        .split('|')
        .next()
        .and_then(|c| c.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let mut guard = BUCKETS.lock();
    let table = guard.get_or_insert_with(std::collections::HashMap::new);
    let remaining = table.entry(config.to_string()).or_insert(capacity);
    if *remaining <= 0 {
        return false;
    }
    *remaining -= 1;
    true
}

/// FNV-1a 64 of the body, the ETag validator both tiers compute.
fn etag_of(body: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("\"{hash:016x}\"")
}

/// Sets `name` to `value`, replacing any existing entry with that name so
/// a chained middleware never emits the header twice.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    match headers
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        Some((_, current)) => *current = value.to_string(),
        None => headers.push((name.to_string(), value.to_string())),
    }
}

fn security_header_set(preset: &str) -> &'static [(&'static str, &'static str)] {
    const BASELINE: &[(&str, &str)] = &[
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
    ];
    const STRICT: &[(&str, &str)] = &[
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        ("Content-Security-Policy", "default-src 'self'"),
        ("Cross-Origin-Opener-Policy", "same-origin"),
        ("Permissions-Policy", "geolocation=(), microphone=()"),
    ];
    match preset {
        "off" => &[],
        "strict" => STRICT,
        _ => BASELINE,
    }
}

/// `middleware::CorsConfig::new(origin, methods, headers, max_age)` -
/// the pipe-joined CORS configuration both tiers pass to `cors`.
pub fn cors_config(origin: &str, methods: &str, headers: &str, max_age: i64) -> String {
    format!("{origin}|{methods}|{headers}|{max_age}")
}

/// `middleware::CorsConfig::permissive()` - any origin, the common verbs.
pub fn cors_permissive() -> String {
    cors_config(
        "*",
        "GET, POST, PUT, DELETE, OPTIONS",
        "Content-Type, Authorization",
        86400,
    )
}

/// `middleware::HstsConfig::safe_default()` - one year, this host only.
pub fn hsts_safe_default() -> String {
    "max-age=31536000".to_string()
}

/// `middleware::HstsConfig::strict()` - two years, subdomains, preload.
pub fn hsts_strict() -> String {
    "max-age=63072000; includeSubDomains; preload".to_string()
}

/// `middleware::SecurityHeaders::strict()` - baseline plus CSP, COOP,
/// and Permissions-Policy.
pub fn security_headers_strict() -> String {
    "strict".to_string()
}

/// `middleware::SecurityHeaders::off()` - emit nothing.
pub fn security_headers_off() -> String {
    "off".to_string()
}

/// `middleware::CacheControl::no_store()` - never cache.
pub fn cache_control_no_store() -> String {
    "no-store".to_string()
}

/// `middleware::CacheControl::immutable_for(seconds)` - a content-hashed
/// asset that may be cached for `seconds` without revalidation.
pub fn cache_control_immutable_for(seconds: i64) -> String {
    format!("public, max-age={seconds}, immutable")
}

/// `middleware::RateLimit::per_ip(capacity, refill_per_sec)` - the
/// token-bucket configuration `rate_limit` consumes.
pub fn rate_limit_config(capacity: i64, refill_per_sec: i64) -> String {
    format!("{capacity}|{refill_per_sec}")
}

/// Applies the transform selected by `kind` to `parts`.
pub fn apply(kind_id: i64, config: &str, parts: &mut ResponseParts) {
    match kind_id {
        middleware_kind::TAG => {
            let mut body = b"mw:".to_vec();
            body.extend_from_slice(&parts.body);
            parts.body = body;
        }
        middleware_kind::REQUEST_ID => {
            set_header(&mut parts.headers, "X-Request-Id", &sequential_request_id());
        }
        middleware_kind::CORS => {
            let mut fields = config.split('|');
            let origin = fields.next().unwrap_or("*");
            let methods = fields.next().unwrap_or("GET, POST, OPTIONS");
            let allowed = fields.next().unwrap_or("Content-Type");
            let max_age = fields.next().unwrap_or("86400");
            set_header(&mut parts.headers, "Access-Control-Allow-Origin", origin);
            set_header(&mut parts.headers, "Access-Control-Allow-Methods", methods);
            set_header(&mut parts.headers, "Access-Control-Allow-Headers", allowed);
            set_header(&mut parts.headers, "Access-Control-Max-Age", max_age);
            if origin != "*" {
                set_header(&mut parts.headers, "Vary", "Origin");
            }
        }
        middleware_kind::SECURITY_HEADERS => {
            for (name, value) in security_header_set(config) {
                set_header(&mut parts.headers, name, value);
            }
        }
        middleware_kind::ETAG => {
            let tag = etag_of(&parts.body);
            set_header(&mut parts.headers, "ETag", &tag);
        }
        middleware_kind::RATE_LIMIT => {
            // The bucket is consumed exactly once per served response, so
            // the token draw stays out of the match guard.
            let allowed = rate_limit_allow(config);
            if !allowed {
                parts.status = 429;
                parts.body = b"rate limit exceeded".to_vec();
                set_header(&mut parts.headers, "Retry-After", "1");
            }
        }
        middleware_kind::HSTS => {
            set_header(&mut parts.headers, "Strict-Transport-Security", config);
        }
        middleware_kind::CACHE_CONTROL => set_header(&mut parts.headers, "Cache-Control", config),
        middleware_kind::BODY_LIMIT => {
            let max = config.parse::<usize>().unwrap_or(usize::MAX);
            if parts.body.len() > max {
                parts.status = 413;
                parts.body = b"payload too large".to_vec();
            }
        }
        middleware_kind::COMPRESS_GZIP => set_header(&mut parts.headers, "Vary", "Accept-Encoding"),
        middleware_kind::LOGGER => eprintln!("[http] {} {}b", parts.status, parts.body.len()),
        middleware_kind::RECOVERER if parts.status >= 500 => {
            parts.body = b"internal server error".to_vec();
        }
        middleware_kind::TIMEOUT => set_header(&mut parts.headers, "X-Timeout-Ms", config),
        middleware_kind::BASIC_AUTH if parts.status == 401 => {
            let realm = if config.is_empty() {
                "restricted"
            } else {
                config
            };
            set_header(
                &mut parts.headers,
                "WWW-Authenticate",
                &format!("Basic realm=\"{realm}\""),
            );
        }
        middleware_kind::BEARER_AUTH if parts.status == 401 => {
            set_header(&mut parts.headers, "WWW-Authenticate", "Bearer");
        }
        middleware_kind::SAFE_DEFAULTS => {
            for (name, value) in security_header_set("strict") {
                set_header(&mut parts.headers, name, value);
            }
            set_header(
                &mut parts.headers,
                "Strict-Transport-Security",
                "max-age=31536000; includeSubDomains",
            );
            set_header(&mut parts.headers, "X-Request-Id", &sequential_request_id());
        }
        _ => {}
    }
}

/// `HandlerFn` for a composed middleware: runs the inner handler then
/// applies the response transform. Passed to `gos_rt_http_serve` as the
/// serve fn when the handler is a `GosMiddleware`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_middleware_serve(mw: *mut u8, req: *mut GosHttpRequest) -> i128 {
    ffi_entry!(0i128, {
        if mw.is_null() {
            let err = crate::c_abi::errors::error_new_from_bytes(b"middleware: null handle");
            return crate::c_abi::vec::pack_result(1, err as i64);
        }
        let m = unsafe { &*(mw as *const GosMiddleware) };
        if m.inner_serve_addr == 0 {
            let err =
                crate::c_abi::errors::error_new_from_bytes(b"middleware: missing inner handler");
            return crate::c_abi::vec::pack_result(1, err as i64);
        }
        // SAFETY: inner_serve_addr came from `gos_fn_addr` over a
        // `HandlerFn`-shaped serve symbol ({Struct}::serve or a nested
        // gos_rt_middleware_serve); inner_env is its matching env.
        let inner: HandlerAbi = unsafe { std::mem::transmute(m.inner_serve_addr as usize) };
        let inner_result = unsafe { inner(m.inner_env as *mut u8, req) };
        if crate::c_abi::vec::gos_rt_result_disc(inner_result) != 0 {
            return inner_result;
        }
        let resp_ptr =
            crate::c_abi::vec::gos_rt_result_payload(inner_result) as *mut GosHttpResponse;
        if resp_ptr.is_null() {
            return inner_result;
        }
        let response = unsafe { &mut *resp_ptr };
        let existing = match &response.body_bytes {
            Some(b) => b.clone(),
            None if response.body.as_ptr().is_null() => Vec::new(),
            None => unsafe { crate::c_abi::gos_str_arg_bytes(response.body.as_ptr()) }.to_vec(),
        };
        let mut parts = ResponseParts {
            status: response.status,
            body: existing,
            headers: std::mem::take(&mut response.headers),
        };
        apply(m.kind, &m.config, &mut parts);
        response.status = parts.status;
        response.headers = parts.headers;
        response.body = SyncRawPtr::new(alloc_cstring(&parts.body));
        response.body_bytes = Some(parts.body);
        inner_result
    })
}

/// `http::middleware::decode_basic_auth(header) -> Option<(String, String)>`.
/// Decodes a `Basic <base64(user:pass)>` Authorization header value (the
/// `Basic ` scheme prefix is optional) into `(user, password)`, or `None`
/// for malformed / non-decodable input. Packed as the 2-word `Option` the
/// compiled tiers read (disc=0 Some, disc=1 None), the same shape
/// `gos_rt_http_request_basic_auth` returns; mirrors the interp
/// `builtin_mw_decode_basic_auth`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_decode_basic_auth(header: *const std::os::raw::c_char) -> i128 {
    ffi_entry!(unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) }, {
        if header.is_null() {
            return unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) };
        }
        let header_str = unsafe { crate::c_abi::gos_str_arg_string(header) };
        let token = header_str.strip_prefix("Basic ").unwrap_or(&header_str);
        let Ok(decoded) = crate::c_abi::encoding::base64_decode(token.trim()) else {
            return unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) };
        };
        let Ok(decoded) = String::from_utf8(decoded) else {
            return unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) };
        };
        let Some((user, pass)) = decoded.split_once(':') else {
            return unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) };
        };
        #[repr(C)]
        struct Pair {
            a: i64,
            b: i64,
        }
        let pair = Box::into_raw(Box::new(Pair {
            a: alloc_cstring(user.as_bytes()) as i64,
            b: alloc_cstring(pass.as_bytes()) as i64,
        }));
        unsafe { crate::c_abi::vec::gos_rt_result_new(0, pair as i64) }
    })
}

// --- Config constructors -------------------------------------------

/// `middleware::CorsConfig::permissive() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_cors_permissive() -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(cors_permissive().as_bytes())
    })
}

/// `middleware::CorsConfig::new(origin, methods, headers, max_age) -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_cors_new(
    origin: *const std::os::raw::c_char,
    methods: *const std::os::raw::c_char,
    headers: *const std::os::raw::c_char,
    max_age: i64,
) -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let text = cors_config(
            &unsafe { mw_cstr(origin) },
            &unsafe { mw_cstr(methods) },
            &unsafe { mw_cstr(headers) },
            max_age,
        );
        alloc_cstring(text.as_bytes())
    })
}

/// `middleware::HstsConfig::safe_default() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_hsts_safe_default() -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(hsts_safe_default().as_bytes())
    })
}

/// `middleware::HstsConfig::strict() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_hsts_strict() -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(hsts_strict().as_bytes())
    })
}

/// `middleware::SecurityHeaders::strict() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_security_strict() -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(security_headers_strict().as_bytes())
    })
}

/// `middleware::SecurityHeaders::off() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_security_off() -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(security_headers_off().as_bytes())
    })
}

/// `middleware::CacheControl::no_store() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_cache_no_store() -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(cache_control_no_store().as_bytes())
    })
}

/// `middleware::CacheControl::immutable_for(seconds) -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_cache_immutable_for(seconds: i64) -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(cache_control_immutable_for(seconds).as_bytes())
    })
}

/// `middleware::RateLimit::per_ip(capacity, refill_per_sec) -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mw_rate_limit_per_ip(
    capacity: i64,
    refill_per_sec: i64,
) -> *mut std::os::raw::c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(rate_limit_config(capacity, refill_per_sec).as_bytes())
    })
}

/// Owned copy of a nul-terminated C string; the empty string for null.
unsafe fn mw_cstr(p: *const std::os::raw::c_char) -> String {
    unsafe { crate::c_abi::gos_str_arg_string(p) }
}
