//! Forward-only schema migrations for `std::database::sql`, shared
//! by every tier.
//!
//! A migration is an `*.sql` file whose name matches
//! `<version>_<slug>.sql`. Versions sort lexicographically; the
//! canonical shape is `0001_init.sql`, `0002_add_users.sql`, ....
//! [`up`] applies every migration whose version is greater than the
//! highest recorded in `schema_migrations`, each inside its own
//! Serializable transaction.
//!
//! Operates on the raw [`ConnectionImpl`] trait object so the C-ABI
//! shims and interpreter builtins can drive it; `gossamer-std`
//! adapts its `Conn` façade onto these functions.

#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use crate::sql::{ConnectionImpl, Error, IsolationLevel, Value};

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

fn exec(conn: &mut dyn ConnectionImpl, sql: &str) -> Result<u64, Error> {
    conn.prepare(sql)?.execute(&[])
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
pub fn init(conn: &mut dyn ConnectionImpl) -> Result<(), Error> {
    exec(
        conn,
        // BIGINT, not INTEGER: PostgreSQL's INTEGER is 32-bit and
        // epoch milliseconds overflow it (SQLite treats both as i64).
        "CREATE TABLE IF NOT EXISTS schema_migrations (\
            version TEXT PRIMARY KEY,\
            name TEXT NOT NULL,\
            applied_at_unix_ms BIGINT NOT NULL\
        )",
    )?;
    Ok(())
}

/// Lists migrations already applied (sorted by version).
pub fn applied(conn: &mut dyn ConnectionImpl) -> Result<Vec<AppliedMigration>, Error> {
    init(conn)?;
    let mut rows = conn
        .prepare(
            "SELECT version, name, applied_at_unix_ms FROM schema_migrations ORDER BY version",
        )?
        .query(&[])?;
    let mut out = Vec::new();
    while let Some(values) = rows.next_row()? {
        let text = |i: usize| match values.get(i) {
            Some(Value::Text(s)) => Ok(s.clone()),
            other => Err(Error::Type(format!(
                "schema_migrations column {i}: expected text, got {other:?}"
            ))),
        };
        let int = |i: usize| match values.get(i) {
            Some(Value::Int(n)) => Ok(*n),
            other => Err(Error::Type(format!(
                "schema_migrations column {i}: expected int, got {other:?}"
            ))),
        };
        out.push(AppliedMigration {
            version: text(0)?,
            name: text(1)?,
            applied_at_unix_ms: int(2)?,
        });
    }
    Ok(out)
}

fn pending(conn: &mut dyn ConnectionImpl, dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
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

/// Applies every pending migration under `dir`. Each migration runs
/// inside a Serializable transaction; failures leave the schema at
/// the previous version. Returns the migrations applied this call.
pub fn up(conn: &mut dyn ConnectionImpl, dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    let pending = pending(conn, dir)?;
    let mut applied_now = Vec::new();
    for migration in pending {
        let mut tx = conn.begin_with(IsolationLevel::Serializable)?;
        // Multi-statement migration files are fed one statement at a
        // time; the splitter respects single-quoted strings and `--`
        // comments. Files with semicolons inside DO blocks should
        // hold one statement per file.
        for stmt in split_statements(&migration.sql) {
            let trimmed = stmt.trim();
            if trimmed.is_empty() {
                continue;
            }
            tx.execute(trimmed)?;
        }
        let insert = format!(
            "INSERT INTO schema_migrations (version, name, applied_at_unix_ms) VALUES ('{}', '{}', {})",
            sql_escape(&migration.version),
            sql_escape(&migration.name),
            unix_now_ms(),
        );
        tx.execute(&insert)?;
        tx.commit()?;
        applied_now.push(migration);
    }
    Ok(applied_now)
}

/// Returns pending migrations without applying them. Useful for a
/// dry-run CLI.
pub fn plan(conn: &mut dyn ConnectionImpl, dir: impl AsRef<Path>) -> Result<Vec<Migration>, Error> {
    pending(conn, dir)
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
                // SQL line comment - skip to end of line.
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
