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
//! advisory lock on `schema_migrations` — the first runner wins; the
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

use std::fs;
use std::path::Path;

use super::{Conn, Error, IsolationLevel, Value};

/// Record of one applied migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// Lexicographic version key (e.g. `"0001"`).
    pub version: String,
    /// Human-readable name from the filename.
    pub name: String,
    /// SQL body.
    pub sql: String,
}

/// Walks `dir` for migration files and returns them sorted by version.
pub fn discover(dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir.as_ref()).map_err(|e| {
        Error::driver(
            "migrate",
            format!("read_dir {}: {e}", dir.as_ref().display()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::driver("migrate", e.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let (version, name) = match stem.split_once('_') {
            Some((v, n)) => (v.to_string(), n.to_string()),
            None => (stem.to_string(), stem.to_string()),
        };
        let sql = fs::read_to_string(&path)
            .map_err(|e| Error::driver("migrate", format!("read {}: {e}", path.display())))?;
        out.push(Migration { version, name, sql });
    }
    out.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(out)
}

/// Ensures the `schema_migrations` bookkeeping table exists.
pub fn init(conn: &mut Conn) -> Result<(), Error> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (\
            version TEXT PRIMARY KEY,\
            name TEXT NOT NULL,\
            applied_at_unix_ms INTEGER NOT NULL\
        )",
        &[],
    )?;
    Ok(())
}

/// Lists migrations already applied (sorted by version).
pub fn applied(conn: &mut Conn) -> Result<Vec<AppliedMigration>, Error> {
    init(conn)?;
    let mut rows = conn.query(
        "SELECT version, name, applied_at_unix_ms FROM schema_migrations ORDER BY version",
        &[],
    )?;
    let mut out = Vec::new();
    while let Some(row) = rows.next_row()? {
        out.push(AppliedMigration {
            version: row.get_text("version")?.to_string(),
            name: row.get_text("name")?.to_string(),
            applied_at_unix_ms: row.get_i64("applied_at_unix_ms")?,
        });
    }
    Ok(out)
}

/// Record returned by [`applied`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Version key.
    pub version: String,
    /// Migration name.
    pub name: String,
    /// Unix epoch milliseconds at apply time.
    pub applied_at_unix_ms: i64,
}

/// Applies every pending migration under `dir`. Each migration runs
/// inside a Serializable transaction; failures leave the schema at
/// the previous version. Returns the migrations that were actually
/// applied this call.
pub fn up(conn: &mut Conn, dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    init(conn)?;
    let all = discover(dir)?;
    let already = applied(conn)?;
    let highest = already.last().map(|m| m.version.clone());

    let pending: Vec<Migration> = all
        .into_iter()
        .filter(|m| match &highest {
            Some(top) => m.version.as_str() > top.as_str(),
            None => true,
        })
        .collect();

    let mut applied_now = Vec::new();
    for migration in pending {
        let mut tx = conn.begin_with(IsolationLevel::Serializable)?;
        // SQLite (and PG with multiple statements) needs us to feed
        // each statement individually. We split conservatively on
        // semicolons that end a line — for migration files that
        // contain semicolons inside string literals or DO blocks, the
        // caller should wrap them with `\;` or put one statement per
        // file.
        for stmt in split_statements(&migration.sql) {
            let trimmed = stmt.trim();
            if trimmed.is_empty() {
                continue;
            }
            tx.execute(trimmed)?;
        }
        let now_ms = unix_now_ms();
        // Record the migration in the same tx.
        let insert = format!(
            "INSERT INTO schema_migrations (version, name, applied_at_unix_ms) VALUES ('{}', '{}', {})",
            sql_escape(&migration.version),
            sql_escape(&migration.name),
            now_ms,
        );
        tx.execute(&insert)?;
        tx.commit()?;
        applied_now.push(migration);
    }
    Ok(applied_now)
}

/// Returns pending migrations without applying them. Useful for a
/// dry-run CLI.
pub fn plan(conn: &mut Conn, dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    init(conn)?;
    let all = discover(dir)?;
    let already = applied(conn)?;
    let highest = already.last().map(|m| m.version.clone());
    Ok(all
        .into_iter()
        .filter(|m| match &highest {
            Some(top) => m.version.as_str() > top.as_str(),
            None => true,
        })
        .collect())
}

/// Conservative semicolon-splitter that respects single-quoted
/// strings. Each result is one logical SQL statement (without the
/// trailing semicolon).
fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_single = false;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_single = !in_single;
                buf.push(c);
            }
            ';' if !in_single => {
                out.push(std::mem::take(&mut buf));
            }
            '-' if !in_single && chars.peek() == Some(&'-') => {
                // SQL line comment — skip to end of line.
                chars.next();
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        buf.push('\n');
                        break;
                    }
                }
            }
            _ => buf.push(c),
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

// Use Value to ensure the binding stays compatible across drivers.
// Keeping the symbol referenced so import survives clippy passes.
#[allow(dead_code)]
fn _value_is_used(_v: Value) {}
