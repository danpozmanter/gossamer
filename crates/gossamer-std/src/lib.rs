//! Standard library for Gossamer (Phases 22 → 26).
//! Introduces this crate as the manifest + Rust-side runtime
//! support for every stdlib module. Subsequent phases extend the
//! manifest with their own module entries while reusing the shared
//! infrastructure exposed here. The Gossamer source files that
//! eventually compile via `gos build` will live alongside this crate
//! in `crates/gossamer-std/std/*.gos` and call into the helpers here
//! for primitives the language can't yet express in itself.

#![forbid(unsafe_code)]

pub mod bufio;
pub mod bytes;
pub mod collections;
pub mod context;
#[cfg(feature = "crypto")]
pub mod crypto;
pub mod encoding;
#[cfg(feature = "compress")]
pub mod compress;
pub mod errors;
pub mod exec;
pub mod flag;
pub mod fmt;
pub mod fs;
pub mod http;
pub mod io;
pub mod json;
pub mod manifest;
pub mod mathrand;
pub mod net;
pub mod os;
pub mod panic;
pub mod path;
pub mod signal;
#[cfg(feature = "regex")]
pub mod regex;
pub mod registry;
pub mod runtime;
pub mod slog;
pub mod sort;
pub mod strconv;
pub mod strings;
pub mod sync;
pub mod testing;
pub mod time;
#[cfg(feature = "tls")]
pub mod tls;
pub mod url;
pub mod utf8;

pub use registry::{StdItem, StdItemKind, StdModule, item, module, modules};
