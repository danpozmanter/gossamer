//! Database access — SQL today, room for a future `NoSQL` surface.

#![forbid(unsafe_code)]

#[cfg(feature = "sql")]
pub mod sql;
