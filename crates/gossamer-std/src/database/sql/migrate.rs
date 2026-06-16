//! Forward-only schema migrations for `std::database::sql`.
//!
//! A migration is an `*.sql` file whose name matches
//! `<version>_<slug>.sql`. Versions sort lexicographically; the
//! canonical shape is `0001_init.sql`, `0002_add_users.sql`, ....
//!
//! Calling [`up`] applies every migration whose version is greater
//! than the highest recorded in `schema_migrations`. Each migration
//! runs inside its own transaction so a failure leaves the schema
//! at the previous version. Concurrent runners are protected by an
//! advisory lock on `schema_migrations` - the first runner wins; the
//! others wait via the connection's busy-timeout.
//!
//! Usage:
//!
//! ```text
//! use std::database::sql::{open, migrate};
//! // a driver crate has been imported and called `register`.
//! let mut conn = open("postgres", &url)?;
//! migrate::up(&mut conn, "./migrations")?;
//! ```

#![forbid(unsafe_code)]

use std::path::Path;

pub use gossamer_runtime::sql_migrate::{AppliedMigration, Migration};

use super::{Conn, Error};

// The migration engine lives in `gossamer_runtime::sql_migrate` so
// the compiled tiers' C-ABI shims and the interpreter share one
// implementation; these wrappers adapt the `Conn` façade onto it.

/// Walks `dir` for migration files and returns them sorted by version.
pub fn discover(dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    gossamer_runtime::sql_migrate::discover(dir)
}

/// Ensures the `schema_migrations` bookkeeping table exists.
pub fn init(conn: &mut Conn) -> Result<(), Error> {
    gossamer_runtime::sql_migrate::init(conn.as_impl_mut())
}

/// Lists migrations already applied (sorted by version).
pub fn applied(conn: &mut Conn) -> Result<Vec<AppliedMigration>, Error> {
    gossamer_runtime::sql_migrate::applied(conn.as_impl_mut())
}

/// Applies every pending migration under `dir`. Each migration runs
/// inside a Serializable transaction; failures leave the schema at
/// the previous version. Returns the migrations applied this call.
pub fn up(conn: &mut Conn, dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    gossamer_runtime::sql_migrate::up(conn.as_impl_mut(), dir)
}

/// Returns pending migrations without applying them. Useful for a
/// dry-run CLI.
pub fn plan(conn: &mut Conn, dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    gossamer_runtime::sql_migrate::plan(conn.as_impl_mut(), dir)
}
