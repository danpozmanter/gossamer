#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::wildcard_imports)]

//! Compiled-tier `http::middleware::bearer_ok` — the minimal
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
//! `RwLock::with_read` combinators — `env[0]` is the callable address,
//! and the body is invoked as `f(env, token)` where `token` is a
//! freshly allocated gos String pointer (the `String`-parameter
//! lowering treats it exactly as a `String`). Bit-identical to the
//! interp `native_bearer_ok` in
//! `gossamer_interp::stdlib_builtins::http_middleware_bearer`.

use super::*;

/// `fn(env, token) -> bool` — the one-argument value-thunk shape, with
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

/// A composed-middleware handle: the wrapped handler's env pointer and
/// its serve fn-address.
pub struct GosMiddleware {
    inner_env: i64,
    inner_serve_addr: i64,
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
        }))
    })
}

/// `HandlerFn` for a composed middleware: runs the inner handler then
/// applies the response transform. Passed to `gos_rt_http_serve` as the
/// serve fn when the handler is a `GosMiddleware`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_middleware_serve(mw: *mut u8, req: *mut GosHttpRequest) -> i128 {
    ffi_entry!(0i128, {
        if mw.is_null() {
            let cs = std::ffi::CString::new("middleware: null handle").expect("static is NUL-free");
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return crate::c_abi::vec::pack_result(1, err as i64);
        }
        let m = unsafe { &*(mw as *const GosMiddleware) };
        if m.inner_serve_addr == 0 {
            let cs = std::ffi::CString::new("middleware: missing inner handler")
                .expect("static is NUL-free");
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
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
            None => unsafe {
                std::ffi::CStr::from_ptr(response.body.as_ptr())
                    .to_bytes()
                    .to_vec()
            },
        };
        let mut new_body = b"mw:".to_vec();
        new_body.extend_from_slice(&existing);
        response.body = SyncRawPtr::new(alloc_cstring(&new_body));
        response.body_bytes = Some(new_body);
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
        let header_str = unsafe {
            std::ffi::CStr::from_ptr(header)
                .to_string_lossy()
                .into_owned()
        };
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
