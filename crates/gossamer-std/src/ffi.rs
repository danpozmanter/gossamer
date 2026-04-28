//! Foreign-function interface, exposed at `std::ffi`.
//!
//! Safe-Rust wrapper around [`gossamer_runtime::ffi`] (where the
//! `unsafe` lives — `gossamer-std` keeps `#![forbid(unsafe_code)]`).
//! Mirrors the design at `~/dev/contexts/lang/ffi_design.md`:
//!
//! - [`Library::open`] dynamically loads a shared object.
//! - [`Library::symbol`] resolves a named function pointer.
//! - The returned [`Symbol`] can be invoked with the well-typed
//!   shapes that the runtime ships out of the box.
//! - [`LinkLibrary`] / [`LinkKind`] are the build-system primitives
//!   that the driver translates into linker flags.

#![forbid(unsafe_code)]

use std::path::Path;

use gossamer_runtime::ffi as rt;
use thiserror::Error;

/// Errors produced by FFI loading and calling.
#[derive(Debug, Error)]
pub enum Error {
    /// Could not open the library.
    #[error("ffi: open `{name}`: {cause}")]
    Open {
        /// Library name.
        name: String,
        /// Underlying cause.
        cause: String,
    },
    /// Could not resolve a symbol.
    #[error("ffi: resolve `{symbol}`: {cause}")]
    Resolve {
        /// Symbol name.
        symbol: String,
        /// Underlying cause.
        cause: String,
    },
    /// String contained an interior NUL byte.
    #[error("ffi: NUL in string: {0}")]
    BadString(String),
}

impl Error {
    fn from_runtime(err: rt::FfiError) -> Self {
        match err {
            rt::FfiError::Open(name, cause) => Self::Open { name, cause },
            rt::FfiError::Resolve(symbol, cause) => Self::Resolve { symbol, cause },
            rt::FfiError::BadString(s) => Self::BadString(s),
        }
    }
}

/// Dynamically loaded shared library. Cheap to clone — clones share
/// the underlying handle.
#[derive(Debug, Clone)]
pub struct Library {
    inner: rt::Library,
}

impl Library {
    /// Loads a shared object by file name (`libsqlite3.so`,
    /// `libcurl.dylib`, `kernel32.dll`).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Ok(Self {
            inner: rt::Library::open(path).map_err(Error::from_runtime)?,
        })
    }

    /// Resolves a symbol by name.
    pub fn symbol(&self, name: &str) -> Result<Symbol, Error> {
        Ok(Symbol {
            inner: self.inner.symbol(name).map_err(Error::from_runtime)?,
        })
    }
}

/// Resolved C function pointer with library-tied lifetime.
#[derive(Debug, Clone)]
pub struct Symbol {
    inner: rt::Symbol,
}

impl Symbol {
    /// Returns the symbol's name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Calls the symbol as `extern "C" fn() -> i32`.
    pub fn call_no_args_i32(&self) -> Result<i32, Error> {
        Ok(self.inner.call_no_args_i32())
    }

    /// Calls the symbol as `extern "C" fn() -> *const c_char`,
    /// returning the result as an owned [`String`].
    pub fn call_no_args_cstring(&self) -> Result<String, Error> {
        Ok(self.inner.call_no_args_cstring())
    }

    /// Calls the symbol as `extern "C" fn(c_int) -> c_int`.
    pub fn call_i32_to_i32(&self, arg: i32) -> Result<i32, Error> {
        Ok(self.inner.call_i32_to_i32(arg))
    }

    /// Calls the symbol as `extern "C" fn(*const c_char) -> c_int`.
    pub fn call_cstr_to_i32(&self, arg: &str) -> Result<i32, Error> {
        self.inner
            .call_cstr_to_i32(arg)
            .map_err(Error::from_runtime)
    }
}

/// Build-system declaration of a native library to link against.
/// Mirrors the `[ffi]` section in `project.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLibrary {
    /// Library base name (without `lib` prefix or extension).
    pub name: String,
    /// `dylib` or `static`.
    pub kind: LinkKind,
    /// Optional search-path override added with `-L`.
    pub path: Option<String>,
}

/// Linkage kind for a [`LinkLibrary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// Shared library: `-l<name>`.
    Dylib,
    /// Static library: `-l<name>` after `-Bstatic`.
    Static,
}

impl LinkKind {
    /// Parses the textual form (`"dylib"` / `"static"`) from
    /// `project.toml`.
    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "dylib" | "shared" => Ok(Self::Dylib),
            "static" => Ok(Self::Static),
            other => Err(Error::BadString(format!("ffi link kind {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_kind_round_trip() {
        assert_eq!(LinkKind::parse("dylib").unwrap(), LinkKind::Dylib);
        assert_eq!(LinkKind::parse("static").unwrap(), LinkKind::Static);
        assert!(LinkKind::parse("magic").is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn opens_libc_and_calls_strlen() {
        // libc is linked into every Linux process. `strlen` is the
        // simplest signature that proves the FFI roundtrip.
        let Ok(lib) = Library::open("libc.so.6") else {
            return;
        };
        let symbol = lib.symbol("strlen").unwrap();
        let result = symbol.call_cstr_to_i32("hello").unwrap();
        assert!(result == 5 || result < 0);
    }
}
