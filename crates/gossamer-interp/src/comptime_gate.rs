//! Capability classification for compile-time evaluation.
//!
//! A `comptime` region folds on the bytecode VM, so the set of things
//! it can reach is exactly the set of builtins the VM resolves. This
//! module classifies every registered builtin by the capability class
//! it needs and answers, for a level, whether a name may be resolved
//! at all.
//!
//! The classification is derived from [`crate::registered_names`]
//! rather than hand-listed, so a builtin cannot arrive unclassified: a
//! module the tables do not name fails
//! `every_registered_module_is_classified`.

use std::sync::OnceLock;

use gossamer_runtime::comptime_policy::{Capability, ComptimeIo};
use rustc_hash::FxHashMap;

use crate::value::Value;

/// Modules whose members are classified name by name because the
/// module mixes pure helpers with capability-bearing calls.
#[cfg(test)]
const MIXED_MODULES: &[&str] = &["fs", "os", "env", "path", "metrics", "bufio"];

/// Capability every member of a module needs, or `None` when the whole
/// module is pure computation. Every module prefix that
/// [`crate::registered_names`] reports must appear here; a mixed
/// module is listed as pure and its capability-bearing members are
/// named in [`NAME_CAPABILITIES`].
const MODULE_CAPABILITIES: &[(&str, Option<Capability>)] = &[
    // Process and signal control.
    ("process", Some(Capability::Exec)),
    // Building a policy is pure, but the module exists to start a
    // process under it, and a compile-time region has no business
    // doing that. The whole module is denied rather than the one call
    // that spawns: over-denial here costs nothing that
    // `--comptime-io=full` does not restore.
    ("sandbox", Some(Capability::Exec)),
    ("exec", Some(Capability::Exec)),
    ("signal", Some(Capability::Exec)),
    ("Child", Some(Capability::Exec)),
    // Network. The whole surface is denied rather than the calls that
    // reach a socket: a compile-time region has no business assembling
    // a request either, and over-denial here costs nothing that
    // `--comptime-io=full` does not restore.
    ("net", Some(Capability::Network)),
    ("http", Some(Capability::Network)),
    ("http_h3", Some(Capability::Network)),
    ("httptest", Some(Capability::Network)),
    ("smtp", Some(Capability::Network)),
    ("websocket", Some(Capability::Network)),
    ("sse", Some(Capability::Network)),
    ("proxy", Some(Capability::Network)),
    ("static_files", Some(Capability::Network)),
    ("session", Some(Capability::Network)),
    ("csrf", Some(Capability::Network)),
    ("cookie", Some(Capability::Network)),
    ("chunked", Some(Capability::Network)),
    ("middleware", Some(Capability::Network)),
    ("Middleware", Some(Capability::Network)),
    ("router", Some(Capability::Network)),
    ("Router", Some(Capability::Network)),
    ("native_client", Some(Capability::Network)),
    ("Client", Some(Capability::Network)),
    ("ClientBuilder", Some(Capability::Network)),
    ("Server", Some(Capability::Network)),
    ("Request", Some(Capability::Network)),
    ("Response", Some(Capability::Network)),
    ("ResponseStream", Some(Capability::Network)),
    ("FileServer", Some(Capability::Network)),
    ("TcpListener", Some(Capability::Network)),
    ("TcpStream", Some(Capability::Network)),
    ("UdpSocket", Some(Capability::Network)),
    ("UnixListener", Some(Capability::Network)),
    ("UnixStream", Some(Capability::Network)),
    // Bare `File::` / `OpenOptions::` method spellings mirror the
    // `fs::File::` registrations one for one, so each inherits its
    // twin's capability by implementation identity.
    ("File", None),
    ("OpenOptions", None),
    // Bare `Policy::` method spellings mirror the `sandbox::Policy::`
    // registrations one for one and inherit their capability by
    // implementation identity.
    ("Policy", None),
    // Reads of host identity that are not path-shaped.
    ("user", Some(Capability::Read)),
    // Mixed modules: pure by default, members named individually.
    ("fs", None),
    ("os", None),
    ("env", None),
    ("path", None),
    ("metrics", None),
    ("bufio", None),
    // Pure computation over values the program already holds.
    ("adler32", None),
    ("aead", None),
    ("Arc", None),
    ("archive", None),
    ("ascii85", None),
    ("AtomicBool", None),
    ("AtomicI32", None),
    ("AtomicI64", None),
    ("AtomicU64", None),
    ("Barrier", None),
    ("base32", None),
    ("base64", None),
    ("big", None),
    ("binary", None),
    ("bits", None),
    ("blake3", None),
    ("Box", None),
    ("BTreeMap", None),
    ("BTreeSet", None),
    ("Buffer", None),
    ("Builder", None),
    ("bytes", None),
    ("bzip2", None),
    ("channel", None),
    ("Channel", None),
    ("collections", None),
    ("compress", None),
    ("context", None),
    ("Context", None),
    ("Counter", None),
    ("crc32", None),
    ("crypto", None),
    ("csv", None),
    ("deque", None),
    ("Deque", None),
    ("Duration", None),
    ("DynValue", None),
    ("ecdsa", None),
    ("ed25519", None),
    ("encoding", None),
    ("EndedSpan", None),
    ("errors", None),
    ("Errors", None),
    ("f32", None),
    ("f64", None),
    ("FieldError", None),
    ("flag", None),
    ("FlagMap", None),
    ("flate", None),
    ("fnv", None),
    ("Gauge", None),
    ("gzip", None),
    ("hash", None),
    ("HashSet", None),
    ("heap", None),
    ("hex", None),
    ("Histogram", None),
    ("hmac", None),
    ("html", None),
    ("i16", None),
    ("i32", None),
    ("i64", None),
    ("I64Vec", None),
    ("i8", None),
    ("image", None),
    ("insecure", None),
    ("Instant", None),
    ("io", None),
    ("ip", None),
    ("isize", None),
    ("iter", None),
    ("Iterator", None),
    ("json", None),
    ("jwt", None),
    ("kdf", None),
    ("lifecycle", None),
    ("Map", None),
    ("math", None),
    ("MaxHeap", None),
    ("mime", None),
    ("MinHeap", None),
    ("Mutex", None),
    ("netip", None),
    ("Once", None),
    ("option", None),
    ("Option", None),
    ("ordered_map", None),
    ("ordered_set", None),
    ("ordered_vec", None),
    ("password", None),
    ("pem", None),
    ("pprof", None),
    ("queue", None),
    ("Queue", None),
    ("rand", None),
    ("Rc", None),
    ("regex", None),
    ("Registry", None),
    ("result", None),
    ("Rng", None),
    ("runtime", None),
    ("RwLock", None),
    ("Scanner", None),
    ("Set", None),
    ("sha256", None),
    ("sha512", None),
    ("Shared", None),
    ("slog", None),
    ("sort", None),
    ("Span", None),
    ("stack", None),
    ("Stack", None),
    ("std", None),
    ("strconv", None),
    ("Stream", None),
    ("String", None),
    ("strings", None),
    ("subtle", None),
    ("sync", None),
    ("tar", None),
    ("template", None),
    ("testing", None),
    ("thread", None),
    ("time", None),
    ("toml", None),
    ("trace", None),
    ("Tracer", None),
    ("u16", None),
    ("u32", None),
    ("u64", None),
    ("u8", None),
    ("U8Vec", None),
    ("unicode", None),
    ("url", None),
    ("usize", None),
    ("utf16", None),
    ("utf8", None),
    ("uuid", None),
    ("validate", None),
    ("Vec", None),
    ("WaitGroup", None),
    ("x509", None),
    ("xml", None),
    ("yaml", None),
    ("zip", None),
    ("zlib", None),
    ("zstd", None),
];

/// Capability an individual qualified name needs, overriding its
/// module's classification. Every member of a mixed module
/// that needs a capability is listed here; the rest are pure and are
/// asserted so by `every_mixed_module_member_is_classified`.
const NAME_CAPABILITIES: &[(&str, Capability)] = &[
    // fs - reads.
    ("fs::File::open", Capability::Read),
    ("fs::File::read", Capability::Read),
    ("fs::File::read_at", Capability::Read),
    ("fs::File::read_to_string", Capability::Read),
    ("fs::canonicalize", Capability::Read),
    ("fs::exists", Capability::Read),
    ("fs::file_size", Capability::Read),
    ("fs::is_dir", Capability::Read),
    ("fs::is_file", Capability::Read),
    ("fs::is_symlink", Capability::Read),
    ("fs::metadata", Capability::Read),
    ("fs::open", Capability::Read),
    ("fs::read", Capability::Read),
    ("fs::read_dir", Capability::Read),
    ("fs::read_to_string", Capability::Read),
    ("fs::walk_dir", Capability::Read),
    // fs - writes. `OpenOptions::open` counts as a write because the
    // options it carries may create or truncate the target.
    ("fs::File::create", Capability::Write),
    ("fs::File::set_len", Capability::Write),
    ("fs::File::sync_all", Capability::Write),
    ("fs::File::sync_data", Capability::Write),
    ("fs::File::write", Capability::Write),
    ("fs::File::write_all", Capability::Write),
    ("fs::File::write_at", Capability::Write),
    ("fs::File::write_bytes", Capability::Write),
    ("fs::OpenOptions::open", Capability::Write),
    ("fs::copy", Capability::Write),
    ("fs::create", Capability::Write),
    ("fs::create_dir", Capability::Write),
    ("fs::create_dir_all", Capability::Write),
    ("fs::remove_dir", Capability::Write),
    ("fs::remove_dir_all", Capability::Write),
    ("fs::remove_file", Capability::Write),
    ("fs::rename", Capability::Write),
    ("fs::sync_dir", Capability::Write),
    ("fs::temp_dir", Capability::Write),
    ("fs::temp_file", Capability::Write),
    ("fs::write", Capability::Write),
    // os.
    ("os::exec::kill", Capability::Exec),
    ("os::exec::kill_group", Capability::Exec),
    ("os::exec::pipeline_run", Capability::Exec),
    ("os::exec::run", Capability::Exec),
    ("os::exec::signal", Capability::Exec),
    ("os::exec::spawn", Capability::Exec),
    ("os::exec::spawn_piped", Capability::Exec),
    ("os::exec::wait_timeout", Capability::Exec),
    ("os::signal::on", Capability::Exec),
    ("os::signal::try_wait", Capability::Exec),
    ("os::signal::wait", Capability::Exec),
    // env. Reading a variable is I/O the `none` level denies and the
    // `confined` level permits; mutating the process environment or
    // the working directory is denied at both.
    ("env::args", Capability::Read),
    ("env::current_dir", Capability::Read),
    ("env::home_dir", Capability::Read),
    ("env::vars", Capability::Read),
    ("env::program_name", Capability::Read),
    ("env::temp_dir", Capability::Read),
    ("env::var", Capability::Read),
    ("env::set_current_dir", Capability::Env),
    ("env::set_var", Capability::Env),
    ("env::unset_var", Capability::Env),
    // path.
    ("path::glob", Capability::Read),
    ("path::walk", Capability::Read),
    // metrics.
    ("metrics::serve_metrics", Capability::Network),
    // bufio.
    ("bufio::read_lines", Capability::Read),
    ("bufio::read_lines_of", Capability::Read),
    ("bufio::read_to_string", Capability::Read),
    ("bufio::split_whitespace", Capability::Read),
];

/// Bare-name prefixes for leaf intrinsics the front end injects. A
/// leaf carries the capability of the surface it backs.
const BARE_PREFIX_CAPABILITIES: &[(&str, Capability)] = &[
    ("__gos_sql_", Capability::Network),
    ("__gos_fs_", Capability::Read),
];

/// Name-to-capability map over every registered builtin.
///
/// Qualified names are classified from the tables above. A bare name
/// is a method spelling of some qualified builtin, so it inherits that
/// builtin's capability by implementation identity - the two
/// registrations share one function pointer - which is what keeps
/// `"x".read_to_string()` from reaching what `fs::read_to_string` may
/// not.
fn capabilities() -> &'static FxHashMap<&'static str, Capability> {
    static TABLE: OnceLock<FxHashMap<&'static str, Capability>> = OnceLock::new();
    TABLE.get_or_init(|| build_table(crate::builtins::cached()))
}

/// Address of a builtin's implementation, tagged by which of the two
/// callable shapes it is so a plain and a dispatching builtin can
/// never collide.
fn implementation_of(value: &Value) -> Option<(u8, usize)> {
    match value {
        Value::Builtin(inner) => Some((0, inner.call as usize)),
        Value::Native(inner) => Some((1, inner.call as usize)),
        _ => None,
    }
}

fn build_table(entries: &'static [(&'static str, Value)]) -> FxHashMap<&'static str, Capability> {
    let mut by_name: FxHashMap<&'static str, Capability> = FxHashMap::default();
    let mut by_implementation: FxHashMap<(u8, usize), Capability> = FxHashMap::default();

    for (name, value) in entries {
        let Some(capability) = classify_qualified(name) else {
            continue;
        };
        by_name.insert(*name, capability);
        if let Some(key) = implementation_of(value) {
            let entry = by_implementation.entry(key).or_insert(capability);
            *entry = strictest(*entry, capability);
        }
    }

    for (name, value) in entries {
        if by_name.contains_key(name) {
            continue;
        }
        let inherited = implementation_of(value).and_then(|key| by_implementation.get(&key));
        if let Some(capability) = inherited {
            by_name.insert(*name, *capability);
        }
    }
    by_name
}

/// The stricter of two capabilities, ordered by how much of the host
/// they expose.
fn strictest(left: Capability, right: Capability) -> Capability {
    fn rank(capability: Capability) -> u8 {
        match capability {
            Capability::Read => 0,
            Capability::Env => 1,
            Capability::Write => 2,
            Capability::Network => 3,
            Capability::Exec => 4,
        }
    }
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

/// Capability `name` needs, for a name the tables classify directly.
fn classify_qualified(name: &str) -> Option<Capability> {
    if let Some((_, capability)) = NAME_CAPABILITIES.iter().find(|(key, _)| *key == name) {
        return Some(*capability);
    }
    if let Some((_, capability)) = BARE_PREFIX_CAPABILITIES
        .iter()
        .find(|(prefix, _)| name.starts_with(prefix))
    {
        return Some(*capability);
    }
    let module = name.split("::").next()?;
    if module == name {
        return None;
    }
    MODULE_CAPABILITIES
        .iter()
        .find(|(key, _)| *key == module)
        .and_then(|(_, capability)| *capability)
}

/// The capability `name` needs when the level in force would refuse
/// it, or `None` when the level permits the call.
///
/// `Full` permits everything, `Confined` permits reads (the path is
/// checked where the read happens), and `None` permits no I/O at all.
#[must_use]
pub(crate) fn denied(name: &str, level: ComptimeIo) -> Option<Capability> {
    if level == ComptimeIo::Full {
        return None;
    }
    let capability = *capabilities().get(name)?;
    if level == ComptimeIo::Confined && capability == Capability::Read {
        return None;
    }
    Some(capability)
}

/// Refuses a compile-time read of `path` that leaves the confinement
/// root.
///
/// The capability itself is decided when the builtin's name resolves;
/// this is the second half of `confined`, where the read is permitted
/// but only under the tree that holds the source doing the reading.
pub(crate) fn guard_read(operation: &str, path: &str) -> crate::value::RuntimeResult<()> {
    gossamer_runtime::comptime_policy::check_path(operation, Capability::Read, path)
        .map_err(|denied| crate::value::RuntimeError::ComptimeDenied(denied.to_string()))
}

#[cfg(test)]
mod comptime_gate_tests {
    use super::*;

    #[test]
    fn every_registered_module_is_classified() {
        let mut unclassified: Vec<&str> = crate::registered_names()
            .into_iter()
            .filter_map(|name| name.split("::").next().filter(|module| *module != name))
            .filter(|module| !MODULE_CAPABILITIES.iter().any(|(key, _)| key == module))
            .collect();
        unclassified.sort_unstable();
        unclassified.dedup();
        assert!(
            unclassified.is_empty(),
            "these builtin modules have no compile-time capability class: {unclassified:?}"
        );
    }

    #[test]
    fn every_mixed_module_member_is_classified() {
        let pure_members: &[&str] = &[
            "fs::File::close",
            "fs::File::flush",
            "fs::File::len",
            "fs::File::seek",
            "fs::File::try_lock_exclusive",
            "fs::File::try_lock_range",
            "fs::File::try_lock_shared",
            "fs::File::unlock",
            "fs::File::unlock_range",
            "fs::OpenOptions::append",
            "fs::OpenOptions::create",
            "fs::OpenOptions::create_new",
            "fs::OpenOptions::new",
            "fs::OpenOptions::read",
            "fs::OpenOptions::truncate",
            "fs::OpenOptions::write",
            "fs::SEEK_CUR",
            "fs::SEEK_END",
            "fs::SEEK_SET",
            "os::arch",
            "os::family",
            "path::components",
            "path::extension",
            "path::file_name",
            "path::file_stem",
            "path::is_absolute",
            "path::join",
            "path::matches",
            "path::normalize",
            "path::parent",
            "path::prefixes",
            "path::split",
            "path::starts_with",
            "path::unique_prefixes",
            "metrics::Counter::inc",
            "metrics::Counter::new",
            "metrics::Counter::value",
            "metrics::Gauge::dec",
            "metrics::Gauge::inc",
            "metrics::Gauge::new",
            "metrics::Gauge::set",
            "metrics::Gauge::value",
            "metrics::Histogram::count",
            "metrics::Histogram::new",
            "metrics::Histogram::observe",
            "metrics::Histogram::sum",
            "metrics::Registry::new",
            "metrics::Registry::register",
            "metrics::Registry::render",
            "bufio::Scanner::new",
            "bufio::Scanner::next",
            "bufio::Scanner::scan",
            "bufio::Scanner::text",
        ];
        let mut unclassified: Vec<&str> = crate::registered_names()
            .into_iter()
            .filter(|name| {
                MIXED_MODULES
                    .iter()
                    .any(|module| name.starts_with(&format!("{module}::")))
            })
            .filter(|name| {
                !NAME_CAPABILITIES.iter().any(|(key, _)| key == name)
                    && !pure_members.contains(name)
            })
            .collect();
        unclassified.sort_unstable();
        unclassified.dedup();
        assert!(
            unclassified.is_empty(),
            "these members of a mixed module are neither capability-bearing nor \
             reviewed as pure: {unclassified:?}"
        );
    }

    #[test]
    fn confined_denies_writes_and_permits_reads() {
        assert_eq!(
            denied("fs::write", ComptimeIo::Confined),
            Some(Capability::Write)
        );
        assert_eq!(denied("fs::read_to_string", ComptimeIo::Confined), None);
        assert_eq!(
            denied("process::run", ComptimeIo::Confined),
            Some(Capability::Exec)
        );
        assert_eq!(
            denied("http::get", ComptimeIo::Confined),
            Some(Capability::Network)
        );
        assert_eq!(
            denied("env::set_var", ComptimeIo::Confined),
            Some(Capability::Env)
        );
    }

    #[test]
    fn none_denies_reads_too_and_full_denies_nothing() {
        assert_eq!(
            denied("fs::read_to_string", ComptimeIo::None),
            Some(Capability::Read)
        );
        assert_eq!(denied("fs::write", ComptimeIo::Full), None);
        assert_eq!(denied("format", ComptimeIo::None), None);
    }

    #[test]
    fn a_bare_method_spelling_inherits_its_qualified_capability() {
        assert_eq!(
            denied("File::write", ComptimeIo::Confined),
            Some(Capability::Write)
        );
        assert_eq!(
            denied("spawn_piped", ComptimeIo::Confined),
            Some(Capability::Exec)
        );
        assert_eq!(
            denied("remove_file", ComptimeIo::Confined),
            Some(Capability::Write)
        );
    }
}
