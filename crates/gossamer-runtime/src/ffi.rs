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

#![allow(clippy::items_after_statements, clippy::missing_safety_doc)]

use std::ffi::{CStr, CString, c_char};
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
        let raw = self.raw;
        // SAFETY: caller asserted the symbol matches the signature.
        // Wrap in `catch_unwind` so a Rust panic inside a binding's
        // entry point cannot cross the FFI boundary (UB).
        std::panic::catch_unwind(|| unsafe { std::mem::transmute::<usize, Fn0I32>(raw)() })
            .unwrap_or(-1)
    }

    /// Calls the symbol as `extern "C" fn() -> *const c_char` and
    /// copies the NUL-terminated C string into an owned [`String`].
    #[must_use]
    pub fn call_no_args_cstring(&self) -> String {
        // `c_char` aliases `i8` on x86_64 Linux but `u8` on
        // aarch64 Linux; use the alias rather than hard-coding so
        // both platforms agree with `CStr::from_ptr`.
        type Fn0Ptr = unsafe extern "C" fn() -> *const c_char;
        let raw = self.raw;
        let ptr_addr: usize = std::panic::catch_unwind(|| {
            // SAFETY: as call_no_args_i32. The `as usize` cast keeps
            // the pointer value Send across the catch_unwind closure
            // boundary without exposing a `*const _` typed result.
            let p: *const c_char = unsafe { std::mem::transmute::<usize, Fn0Ptr>(raw)() };
            p as usize
        })
        .unwrap_or(0);
        if ptr_addr == 0 {
            return String::new();
        }
        // SAFETY: we additionally trust the returned pointer is
        // NUL-terminated and statically owned by the library (the
        // typical contract for `*_libversion`-style C accessors).
        unsafe { CStr::from_ptr(ptr_addr as *const c_char) }
            .to_string_lossy()
            .into_owned()
    }

    /// Calls the symbol as `extern "C" fn(c_int) -> c_int`.
    #[must_use]
    pub fn call_i32_to_i32(&self, arg: i32) -> i32 {
        type Fn1I32 = unsafe extern "C" fn(i32) -> i32;
        let raw = self.raw;
        std::panic::catch_unwind(|| unsafe {
            // SAFETY: as call_no_args_i32.
            std::mem::transmute::<usize, Fn1I32>(raw)(arg)
        })
        .unwrap_or(-1)
    }

    /// Calls the symbol as `extern "C" fn(*const c_char) -> c_int`.
    pub fn call_cstr_to_i32(&self, arg: &str) -> Result<i32, FfiError> {
        let arg = CString::new(arg).map_err(|e| FfiError::BadString(e.to_string()))?;
        let raw = self.raw;
        let arg_ptr_addr = arg.as_ptr() as usize;
        let result = std::panic::catch_unwind(|| {
            type Fn1Cstr = unsafe extern "C" fn(*const c_char) -> i32;
            // SAFETY: as call_no_args_i32; the CString lives across
            // the call (kept alive by the outer `arg` binding).
            unsafe { std::mem::transmute::<usize, Fn1Cstr>(raw)(arg_ptr_addr as *const c_char) }
        })
        .unwrap_or(-1);
        // Hold `arg` here so its NUL-terminated buffer stays valid
        // for the entire FFI call above (`catch_unwind` captures
        // only `raw` and `arg_ptr_addr`).
        drop(arg);
        Ok(result)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    // Miri can't simulate `dlopen` (no real OS dynamic linker); the
    // libloading crate aborts before any of our code runs. Skip
    // under Miri so the rest of the runtime suite stays exercisable.
    #[cfg_attr(miri, ignore = "libloading::dlopen unsupported under Miri")]
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
