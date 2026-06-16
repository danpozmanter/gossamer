//! Text-template support.
//!
//! The engine itself lives in the leaf crate `gossamer-template` (below
//! `gossamer-runtime`) so the compiled tier can render templates without
//! a dependency cycle; this module re-exports it unchanged.

#![forbid(unsafe_code)]

pub use gossamer_template::text as template;
