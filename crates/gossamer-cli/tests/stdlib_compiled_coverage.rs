//! Compiled-tier coverage gate for the stdlib free-function surface.
//!
//! A stdlib function is bound in the bytecode VM (place 1: the interp
//! builtin registry) AND lowered by the compiled tiers (place 5: a MIR
//! dispatch arm). When the second is missing a program calls the
//! function fine under `gos run` but fails `gos build` with
//! `opt: use of undefined value '@module::fn'`. Nothing used to
//! cross-check the two, so the drift accumulated silently.
//!
//! This test makes the compiled free-function dispatch *enumerable* and
//! asserts every advertised free function (`STDLIB_QUALIFIED`, which the
//! `resolver_stdlib_table_matches_runtime` test keeps equal to the
//! interp registry) is reachable by the compiled tier - either through a
//! MIR dispatch arm (the enumerable `::`-keyed match patterns in
//! `stdlib_free.rs` / `intrinsic.rs` / `expr_call.rs`) or through one of
//! the closed, documented non-pattern mechanisms below.
//!
//! Adding a VM builtin without wiring its compiled lowering makes this
//! test fail with the exact function name.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The MIR dispatch files whose `match` arms key on the joined stdlib
/// path. Their string-literal patterns are the enumerable set of
/// compiled-lowerable free-function paths.
const DISPATCH_SOURCES: &[&str] = &[
    "crates/gossamer-mir/src/lower/builder/stdlib_free.rs",
    "crates/gossamer-mir/src/lower/builder/intrinsic.rs",
    "crates/gossamer-mir/src/lower/builder/expr_call.rs",
];

/// Free functions the compiled tier reaches through a mechanism other
/// than a `::`-keyed dispatch arm. This is a closed, mechanism-annotated
/// list, not a dumping ground: every entry is a real function lowered by
/// a named compiled path (verified to build + run). A *new* VM-only
/// function must not be added here to silence the gate - wire its MIR
/// dispatch instead.
const COMPILED_VIA_SPECIAL_MECHANISM: &[&str] = &[
    // `lower_json_free_call` recognises the whole `json` / `encoding::json`
    // module by its segment chain (not per-function string patterns).
    "encoding::json::as_array",
    "encoding::json::as_bool",
    "encoding::json::as_f64",
    "encoding::json::as_i64",
    "encoding::json::as_str",
    "encoding::json::at",
    "encoding::json::decode",
    "encoding::json::encode",
    "encoding::json::encode_pretty",
    "encoding::json::get",
    "encoding::json::is_null",
    "encoding::json::keys",
    "encoding::json::len",
    "encoding::json::parse",
    "encoding::json::render",
    "encoding::json::set",
    "encoding::json::valid",
    "json::as_array",
    "json::as_bool",
    "json::as_f64",
    "json::as_i64",
    "json::as_str",
    "json::at",
    "json::decode",
    "json::encode",
    "json::get",
    "json::is_null",
    "json::keys",
    "json::len",
    "json::parse",
    "json::render",
    "json::set",
    // Autoderive-injected real Gossamer wrappers (compile to defined
    // functions; they fold the `__gos_*_raw` leaf intrinsics into structs).
    "archive::tar::read",
    "archive::zip::read",
    "crypto::x509::parse_pem",
    "crypto::x509::verify_server_certificate_with_crls",
    "encoding::pem::decode",
    "encoding::pem::decode_all",
    "encoding::pem::encode",
    "fs::metadata",
    // Civil-time wrappers carry the source-visible Location struct's compact
    // specification string into the scalar runtime leaves below.
    "time::add_date",
    "time::format_in",
    // `flag::define` expansion consumes these declarative builders
    // (`lower_flag_define`); they are only meaningful inside it.
    "flag::bool",
    "flag::int",
    "flag::parse",
    "flag::string",
    // Buffered std streams - lowered as fd-tagged stream handles.
    "io::stderr",
    "io::stdin",
    "io::stdout",
    // Process-control statements lowered directly to runtime calls.
    "process::abort",
    "process::exit",
    "process::id",
    // Channel construction destructured at the binding site.
    "channel::new",
    "channel::unbounded",
    "std::sync::channel",
    "std::sync::channel_unbounded",
    "sync::channel",
    "sync::channel_unbounded",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

/// Pulls every path-shaped string literal (`"a::b::c"`, lowercase head,
/// no `gos_rt_` / `__gos` prefix) out of a dispatch source file. These
/// are the match-arm patterns that route a joined path to its runtime
/// symbol; runtime symbols (`gos_rt_*`) carry no `::` and are excluded
/// by construction.
fn dispatch_paths(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, piece) in src.split('"').enumerate() {
        // Odd indices are the contents of a string literal.
        if i % 2 == 0 {
            continue;
        }
        if !piece.contains("::") {
            continue;
        }
        if piece.starts_with("gos_rt") || piece.starts_with("__gos") {
            continue;
        }
        let mut chars = piece.chars();
        let first_ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
        let rest_ok = piece
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
        if first_ok && rest_ok {
            out.insert(piece.to_string());
        }
    }
    out
}

/// A `STDLIB_QUALIFIED` entry is a free function when none of its
/// segments names a type (no segment starts uppercase) - the
/// `module::Type::method` forms are dispatched separately.
fn is_free_function(path: &str) -> bool {
    !path
        .split("::")
        .any(|seg| seg.chars().next().is_some_and(char::is_uppercase))
}

/// Edition-2027 migration aliases are deliberately dispatched through the
/// canonical eager lowering table. Keep the coverage gate aware of that
/// one-to-one path relationship instead of requiring duplicate match arms for
/// aliases that cannot diverge at runtime.
fn has_compiled_dispatch(path: &str, reachable: &BTreeSet<String>) -> bool {
    reachable.contains(path)
        || path
            .strip_prefix("iter::eager_")
            .is_some_and(|name| reachable.contains(&format!("iter::{name}")))
}

#[test]
fn stdlib_compiled_coverage() {
    let root = workspace_root();
    let mut reachable: BTreeSet<String> = BTreeSet::new();
    for rel in DISPATCH_SOURCES {
        let path = root.join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        reachable.extend(dispatch_paths(&src));
    }
    for entry in COMPILED_VIA_SPECIAL_MECHANISM {
        reachable.insert((*entry).to_string());
    }

    let vm_only: Vec<&str> = gossamer_resolve::STDLIB_QUALIFIED
        .iter()
        .copied()
        .filter(|p| is_free_function(p))
        .filter(|p| !has_compiled_dispatch(p, &reachable))
        .collect();

    assert!(
        vm_only.is_empty(),
        "{n} stdlib free function(s) are bound in the interp (VM) but have no \
         compiled-tier dispatch - they pass `gos run` and fail `gos build` with \
         `opt: use of undefined value`.\nWire a MIR dispatch arm in \
         crates/gossamer-mir/src/lower/builder/stdlib_free.rs (and the rt! \
         registry + gos_rt_* shim + symbol_table entry), or - if the function is \
         reached through a non-pattern mechanism - document it in \
         COMPILED_VIA_SPECIAL_MECHANISM:\n  {vm_only:#?}",
        n = vm_only.len()
    );

    // Guard against the allowlist rotting: an entry that is neither a
    // live free function nor actually special is dead weight that would
    // mask a future regression.
    let live_free: BTreeSet<&str> = gossamer_resolve::STDLIB_QUALIFIED
        .iter()
        .copied()
        .filter(|p| is_free_function(p))
        .collect();
    let stale: Vec<&&str> = COMPILED_VIA_SPECIAL_MECHANISM
        .iter()
        .filter(|p| !live_free.contains(**p))
        .collect();
    assert!(
        stale.is_empty(),
        "COMPILED_VIA_SPECIAL_MECHANISM entries no longer advertised as free \
         functions (remove them): {stale:#?}"
    );
}
