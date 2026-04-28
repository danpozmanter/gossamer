//! Foreign-function interface support.
//!
//! Wraps `libloading` to give Gossamer programs a way to load shared
//! libraries (`.so`, `.dylib`, `.dll`) at runtime, resolve symbols by
//! name, and invoke them through per-arity calling shims. The FFI
//! design lives in `~/dev/contexts/lang/ffi_design.md`; the user-
//! facing `std::ffi` module is a thin safe wrapper around this one.
//!
//! Unsafe is contained inside this module: every entry point either
//! returns a typed wrapper (so the unsafety stops at the boundary)
//! or invokes a fixed-shape `extern "C"` function pointer that the
//! caller has audited at the type-system level.

#![allow(
    clippy::missing_errors_doc,
    clippy::items_after_statements,
    clippy::missing_safety_doc
)]

use std::ffi::{CStr, CString};
use std::path::Path;
use std::sync::Arc;

/// Errors produced by FFI loading and calling.
#[derive(Debug)]
pub enum FfiError {
    /// Could not open the library.
    Open(String, String),
    /// Could not resolve a symbol.
    Resolve(String, String),
    /// String contained an interior NUL byte.
    BadString(String),
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(name, cause) => write!(f, "ffi: open `{name}`: {cause}"),
            Self::Resolve(sym, cause) => write!(f, "ffi: resolve `{sym}`: {cause}"),
            Self::BadString(s) => write!(f, "ffi: NUL in string: {s}"),
        }
    }
}

impl std::error::Error for FfiError {}

/// Dynamically loaded shared library.
#[derive(Clone)]
pub struct Library {
    inner: Arc<libloading::Library>,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Library(...)")
    }
}

impl Library {
    /// Loads a shared object by file name.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FfiError> {
        let path = path.as_ref();
        // SAFETY: dlopen is inherently unsafe; the loaded module is
        // trusted by virtue of the caller naming it. This matches
        // `libloading::Library::new` which itself documents the
        // contract.
        let library = unsafe { libloading::Library::new(path) }
            .map_err(|e| FfiError::Open(path.display().to_string(), e.to_string()))?;
        Ok(Self {
            inner: Arc::new(library),
        })
    }

    /// Resolves a symbol by name. Returns a typed [`Symbol`] handle.
    pub fn symbol(&self, name: &str) -> Result<Symbol, FfiError> {
        // SAFETY: libloading::Library::get returns a typed wrapper
        // whose lifetime is tied to the library handle. We capture
        // the raw pointer immediately and store it alongside an
        // `Arc<Library>` clone so the dlopen handle outlives every
        // outstanding `Symbol`.
        let raw = unsafe {
            let symbol: libloading::Symbol<*mut std::ffi::c_void> = self
                .inner
                .get(name.as_bytes())
                .map_err(|e| FfiError::Resolve(name.to_string(), e.to_string()))?;
            *symbol as usize
        };
        Ok(Symbol {
            _library: self.clone(),
            raw,
            name: name.to_string(),
        })
    }
}

/// Resolved C function pointer with library-tied lifetime.
#[derive(Clone)]
pub struct Symbol {
    _library: Library,
    raw: usize,
    name: String,
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Symbol({})", self.name)
    }
}

impl Symbol {
    /// Returns the symbol's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Calls the symbol as `extern "C" fn() -> i32`.
    #[must_use]
    pub fn call_no_args_i32(&self) -> i32 {
        type Fn0I32 = unsafe extern "C" fn() -> i32;
        // SAFETY: caller asserted the symbol matches the signature.
        unsafe { std::mem::transmute::<usize, Fn0I32>(self.raw)() }
    }

    /// Calls the symbol as `extern "C" fn() -> *const c_char` and
    /// copies the NUL-terminated C string into an owned [`String`].
    #[must_use]
    pub fn call_no_args_cstring(&self) -> String {
        type Fn0Ptr = unsafe extern "C" fn() -> *const i8;
        // SAFETY: as call_no_args_i32; we additionally trust the
        // returned pointer is NUL-terminated and statically owned by
        // the library (the typical contract for `*_libversion`-style
        // C accessors).
        let ptr: *const i8 = unsafe { std::mem::transmute::<usize, Fn0Ptr>(self.raw)() };
        if ptr.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    /// Calls the symbol as `extern "C" fn(c_int) -> c_int`.
    #[must_use]
    pub fn call_i32_to_i32(&self, arg: i32) -> i32 {
        type Fn1I32 = unsafe extern "C" fn(i32) -> i32;
        // SAFETY: as call_no_args_i32.
        unsafe { std::mem::transmute::<usize, Fn1I32>(self.raw)(arg) }
    }

    /// Calls the symbol as `extern "C" fn(*const c_char) -> c_int`.
    pub fn call_cstr_to_i32(&self, arg: &str) -> Result<i32, FfiError> {
        let arg = CString::new(arg).map_err(|e| FfiError::BadString(e.to_string()))?;
        type Fn1Cstr = unsafe extern "C" fn(*const i8) -> i32;
        // SAFETY: as call_no_args_i32; the CString lives until the
        // call returns.
        Ok(unsafe { std::mem::transmute::<usize, Fn1Cstr>(self.raw)(arg.as_ptr()) })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn opens_libc_and_calls_strlen() {
        let Ok(lib) = Library::open("libc.so.6") else {
            return;
        };
        let symbol = lib.symbol("strlen").unwrap();
        let result = symbol.call_cstr_to_i32("hello").unwrap();
        assert!(result == 5 || result < 0);
    }
}
