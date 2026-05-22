//! C-ABI runtime surface linked into every native Gossamer program.
//! Every symbol in this module is exported under the `gos_rt_*`
//! prefix so the Cranelift codegen can call them by name. All
//! `extern "C"` functions run in unsafe context — the compiler emits
//! raw pointers and trusts the contract described next to each
//! symbol. Failure modes are documented per symbol; they never
//! panic across the FFI boundary.

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
// FFI signatures must match the Cranelift / LLVM call sites
// exactly. Keep these allows at file scope rather than dotting
// per-call-site annotations across the C-ABI surface:
// `similar_names` covers `argc`/`argv` Unix convention;
// `many_single_char_names` covers `p`/`n`/`k` in tight memory
// helpers; `items_after_statements` permits inner helper fns
// alongside the call site they document; `same_length_and_capacity`
// fires on `Vec::from_raw_parts(p, n, n)` reconstructing exact
// allocations; `cast_lossless` would force `i64::from(x)` shapes
// that obscure hot-path arithmetic; `doc_markdown` would force
// backticks around every C-ABI symbol name in summary lines.
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
// Pointer casts in this file all reinterpret memory the runtime
// allocates through `gos_rt_gc_alloc`, which is 8-byte aligned, or
// `Vec`-backed buffers (whose alignment matches the elem type). The
// linter cannot see the upstream alignment guarantee and would fire
// on every cast; concentrating the allow at file scope keeps the
// individual sites readable.
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
// Mutable statics back the C-ABI / LLVM-inlined surface (`STDOUT_BUF`,
// `STDOUT_LEN`, etc. — see `stdout_buffer_globals.md`). The lowerer
// emits load/store directly against these symbols, so they have to
// remain `static mut`; the lint flags every read but the contract
// is documented at each declaration.
#![allow(static_mut_refs)]
// Several runtime helpers were `unsafe extern "C"` because they
// touched the (now-retired) bump arena's thread-local, mutated
// `Box::into_raw`-leaked storage, or wrapped `Vec::from_raw_parts`
// reclamation. The migration to `Box::into_raw`-only allocation
// (fix_architecture_ownership.md Stage 4) made several call paths
// safe at the function level — `gos_rt_result_new` is the
// loudest. Keep the existing `unsafe { ... }` wrappers in callers
// for now; the rustc warning is silenced here so the lint comes
// back when we tighten the fn-level `unsafe` story (Stage 6).
#![allow(unused_unsafe)]

/// Wraps an FFI body in `catch_unwind`, returning `$sentinel` on
/// panic. Without this, a panic inside the body crosses the
/// `extern "C"` boundary into compiled Gossamer code, which is UB.
macro_rules! ffi_entry {
    ($sentinel:expr, $body:block) => {{
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body));
        match result {
            Ok(v) => v,
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "(non-string panic payload)".to_string()
                };
                eprintln!(
                    "gossamer runtime: panic at FFI entry caught — {msg}; \
                     returning sentinel"
                );
                $sentinel
            }
        }
    }};
}

// ---------------------------------------------------------------
// SyncRawPtr<T> — `repr(transparent)` newtype around `*mut T` that
// is structurally `Send + Sync`. Used as the field type wherever a
// Gossamer runtime container holds a raw pointer that needs to
// cross goroutine / thread boundaries (channels, scheduler queues,
// shared static tables). Centralises the `unsafe impl Send + Sync`
// declaration into one audited site so individual container types
// (`GosVec`, `GosArrIter`, `GosError`, ...) auto-derive Send+Sync
// from their field composition without each one writing its own
// `unsafe impl`. Layout equals `*mut T` exactly — codegen field
// offsets are unchanged. Method surface mirrors `*mut T` and a
// `Deref<Target = *mut T>` impl makes most read-call sites compile
// unchanged. Pointer-validity / aliasing contracts remain the
// caller's responsibility — this newtype only declares thread-
// transferability, not freedom from data races.
#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct SyncRawPtr<T>(pub *mut T);

// SAFETY: see the type-level documentation above. All cross-thread
// transfer sites are audited by inspection of the containing
// struct's API — this impl declares the FFI handle can move
// between threads, not that the pointee can be mutated concurrently.
unsafe impl<T> Send for SyncRawPtr<T> {}
unsafe impl<T> Sync for SyncRawPtr<T> {}

impl<T> SyncRawPtr<T> {
    pub const NULL: Self = Self(std::ptr::null_mut());
    pub const fn new(p: *mut T) -> Self {
        Self(p)
    }
    pub const fn from_const(p: *const T) -> Self {
        Self(p.cast_mut())
    }
    pub fn as_ptr(&self) -> *mut T {
        self.0
    }
    pub fn as_const_ptr(&self) -> *const T {
        self.0.cast_const()
    }
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl<T> std::ops::Deref for SyncRawPtr<T> {
    type Target = *mut T;
    fn deref(&self) -> &*mut T {
        &self.0
    }
}

impl<T> Default for SyncRawPtr<T> {
    fn default() -> Self {
        Self::NULL
    }
}

impl<T> From<*mut T> for SyncRawPtr<T> {
    fn from(p: *mut T) -> Self {
        Self(p)
    }
}

impl<T> From<*const T> for SyncRawPtr<T> {
    fn from(p: *const T) -> Self {
        Self::from_const(p)
    }
}

pub mod archive;
pub mod args;
pub mod atomic;
pub mod btmap;
pub mod bufio;
pub mod chan;
pub mod concat;
pub mod container_heap;
pub mod container_seq;
pub mod container_set;
pub mod coverage;
pub mod crypto;
pub mod csv;
pub mod encoding;
pub mod errors;
pub mod exec;
pub mod flag;
pub mod fn_registry;
pub mod fs;
pub mod gc;
pub mod go;
pub mod gzip;
pub mod hash;
pub mod heap_i64;
pub mod heap_u8;
pub mod http_bridges;
pub mod http_client;
pub mod http_server;
pub mod json;
pub mod lcg;
pub mod ledger;
pub mod len;
pub mod map;
pub mod math;
pub mod math_big;
pub mod mime;
pub mod mutex;
pub mod netip;
pub mod os_user;
pub mod panic;
pub mod print;
pub mod rc;
pub mod regex;
pub mod set;
pub mod signal;
pub mod slog;
pub mod sql;
pub mod strconv;
pub mod stream;
pub mod string;
pub mod sync_map;
pub mod sync_vec;
pub mod testing;
pub mod time;
pub mod toml_enc;
pub mod unicode;
pub mod url;
pub mod utf16;
pub mod utf8;
pub mod uuid;
pub mod vec;
pub mod wg;
pub mod x509;
pub mod yaml_enc;

pub mod symbol_table;
pub use symbol_table::runtime_symbol_addrs;

pub use encoding::*;
pub use sql::*;
pub use unicode::*;
pub use utf8::*;

pub use archive::*;
pub use args::*;
pub use atomic::*;
pub use btmap::*;
pub use bufio::*;
pub use chan::*;
pub use concat::*;
pub use container_heap::*;
pub use container_seq::*;
pub use container_set::*;
pub use coverage::*;
pub use crypto::*;
pub use csv::*;
pub use errors::*;
pub use exec::*;
pub use flag::*;
pub use fs::*;
pub use gc::*;
pub use go::*;
pub use gzip::*;
pub use hash::*;
pub use heap_i64::*;
pub use heap_u8::*;
pub use http_bridges::*;
pub use http_client::*;
pub use http_server::*;
pub use json::*;
pub use lcg::*;
pub use len::*;
pub use map::*;
pub use math::*;
pub use math_big::*;
pub use mime::*;
pub use mutex::*;
pub use netip::*;
pub use os_user::*;
pub use panic::*;
pub use print::*;
pub use rc::*;
pub use regex::*;
pub use set::*;
pub use signal::*;
pub use slog::*;
pub use strconv::*;
pub use stream::*;
pub use string::*;
pub use sync_map::*;
pub use sync_vec::*;
pub use testing::*;
pub use time::*;
pub use toml_enc::*;
pub use url::*;
pub use utf16::*;
pub use uuid::*;
pub use vec::*;
pub use wg::*;
pub use x509::*;
pub use yaml_enc::*;
