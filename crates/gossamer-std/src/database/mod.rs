//! Database access — SQL today, room for a future NoSQL surface.

#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

#[cfg(feature = "sql")]
pub mod sql;
