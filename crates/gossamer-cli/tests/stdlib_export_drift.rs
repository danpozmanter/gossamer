//! Keeps the resolver's checked-in stdlib export table in sync with
//! the runtime builtin registry. If the stdlib surface changes, this
//! fails with the diff so the table in
//! `gossamer-resolve/src/stdlib_exports.rs` is regenerated - without
//! it, a newly-added `module::fn` would be wrongly rejected by
//! `gos check` / the LSP as an unknown member.

#[test]
fn resolver_stdlib_table_matches_runtime() {
    let mut live: Vec<&str> = gossamer_interp::registered_names()
        .into_iter()
        .filter(|n| n.contains("::") && n.chars().next().is_some_and(char::is_lowercase))
        .collect();
    live.sort_unstable();
    live.dedup();

    let table: Vec<&str> = gossamer_resolve::STDLIB_QUALIFIED.to_vec();

    let missing: Vec<&str> = live
        .iter()
        .filter(|n| !table.contains(n))
        .copied()
        .collect();
    let extra: Vec<&str> = table
        .iter()
        .filter(|n| !live.contains(n))
        .copied()
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "stdlib export table drifted from runtime registry.\n  \
         missing from table (regenerate stdlib_exports.rs): {missing:?}\n  \
         extra in table (no longer registered): {extra:?}"
    );
}

/// Intentional deprecated re-exports the team keeps callable even
/// though their canonical spelling lives under a different module in
/// the manifest. This is a closed list, not a dumping ground - every
/// entry must be a deliberate alias, not an unmanifested member.
const ALLOWED_UNMANIFESTED: &[&str] = &[
    // Each entry's canonical spelling is the manifest member; these
    // are convenience / deprecated aliases the runtime keeps callable.
    "channel::new",                           // -> sync::channel
    "fs::create_dir",                         // -> fs::create_dir_all
    "fs::create_dir_all",                     // -> fs::create_dir_all
    "fs::read",                               // -> fs::read
    "math::rem",                              // -> math::rem
    "os::home",                               // -> env::home_dir
    "os::list_dir",                           // -> os::read_dir
    "os::set_cwd",                            // -> env::set_current_dir
    "path::walk",                             // -> fs::walk_dir
    "thread::sleep_ms",                       // -> time::sleep
    "encoding::utf16::is_surrogate",          // -> utf16::is_surrogate
    "encoding::utf16::rune_len",              // -> utf16::rune_len
    "encoding::utf16::decode_surrogate_pair", // -> utf16::decode_surrogate_pair
    "encoding::utf16::encode_string",         // -> utf16::encode_string
    "encoding::utf16::decode_to_string",      // -> utf16::decode_to_string
];

/// Every registered `module::fn` must name a member the canonical
/// manifest advertises. Without this guard, a runtime builtin can
/// register an unmanifested alias that passes `gos check` and runs on
/// the VM, yet has no manifest entry - the structural hole that let a
/// drift of VM-only aliases accumulate. `module::Type::method` forms
/// (the segment before the member is uppercase) are type-associated
/// methods, not free functions, and are not manifest members.
#[test]
fn registry_members_match_manifest() {
    use std::collections::{HashMap, HashSet};

    // (canonical_path, member) every manifest module advertises.
    let mut pairs: HashSet<(&str, &str)> = HashSet::new();
    // Source-spelling binding -> the canonical paths it can resolve to.
    // Keyed by BOTH the full path (`encoding::json`) and its last
    // segment (`json`), so `json::parse` and `encoding::json::parse`
    // both reach `std::encoding::json`.
    let mut binding_to_paths: HashMap<&str, Vec<&str>> = HashMap::new();
    for m in gossamer_std::manifest::ALL_MODULES {
        let path = m.path.strip_prefix("std::").unwrap_or(m.path);
        for it in m.items {
            pairs.insert((path, it.name));
        }
        binding_to_paths.entry(path).or_default().push(path);
        if let Some(seg) = path.rsplit("::").next() {
            binding_to_paths.entry(seg).or_default().push(path);
        }
    }

    let unmatched: Vec<&str> = gossamer_interp::registered_names()
        .into_iter()
        .filter(|n| n.contains("::") && n.chars().next().is_some_and(char::is_lowercase))
        .filter(|n| !ALLOWED_UNMANIFESTED.contains(n))
        .filter(|n| {
            let mut segs: Vec<&str> = n.split("::").collect();
            // A leading `std` segment is just the crate root.
            if segs.first() == Some(&"std") {
                segs.remove(0);
            }
            let member = segs[segs.len() - 1];
            let binding_segs = &segs[..segs.len() - 1];
            // Type-associated methods are not manifest members.
            if binding_segs
                .last()
                .is_some_and(|s| s.chars().next().is_some_and(char::is_uppercase))
            {
                return false;
            }
            let binding = binding_segs.join("::");
            let matched = binding_to_paths
                .get(binding.as_str())
                .into_iter()
                .flatten()
                .any(|p| pairs.contains(&(*p, member)));
            !matched
        })
        .collect();

    assert!(
        unmatched.is_empty(),
        "{} registered member(s) have no canonical manifest entry. Add a \
         StdItem to the right manifest/*.rs module (or, if it is a \
         deliberate deprecated alias, to ALLOWED_UNMANIFESTED):\n  {unmatched:#?}",
        unmatched.len()
    );
}

/// Manifest `Function` items whose implementation is reached through a
/// parse-time call rewrite (`gossamer-parse`), so the public spelling is
/// absent from the interp builtin registry yet the call resolves on every
/// tier. A closed, mechanism-annotated list: each entry is rewritten /
/// injected by a named mechanism in `gossamer-parse` (verified to build +
/// run). The resolver never sees these names - the rewrite fires before
/// resolution - so the three-segment phantom gate cannot reject them.
const MANIFEST_IMPL_VIA_REWRITE: &[&str] = &[
    // `Parser::rewrite_errors_newf` desugars to `errors::new(format!(..))`.
    "errors::newf",
    // `rewrite_stdlib_struct_surface` maps these to injected
    // `__gos_http_*` wrappers (HTTP_SECURITY_WRAPPERS).
    "http::csrf::extract_token",
    "http::csrf::origin_allowed",
    "http::csrf::check",
    "http::csrf::attach_cookie",
    "http::session::with_session",
    "http::multipart::parse",
];

#[test]
fn manifest_functions_have_implementations() {
    use std::collections::{HashMap, HashSet};

    use gossamer_std::manifest::feature_status::{Status, lookup};

    // Canonical path (no `std::`) -> itself, plus last-segment -> path,
    // so a registered `json::parse` and a manifest `encoding::json` both
    // reach the same canonical module. Mirrors the binding map in
    // `registry_members_match_manifest`, inverted.
    let mut binding_to_paths: HashMap<&str, Vec<&str>> = HashMap::new();
    for m in gossamer_std::manifest::ALL_MODULES {
        let path = m.path.strip_prefix("std::").unwrap_or(m.path);
        binding_to_paths.entry(path).or_default().push(path);
        if let Some(seg) = path.rsplit("::").next() {
            binding_to_paths.entry(seg).or_default().push(path);
        }
    }

    // (canonical_path, member) pairs the interp actually binds, reached
    // by reverse-mapping every registered free-function name through its
    // binding spelling. `module::Type::method` names are type-associated
    // methods, never manifest free-function members, so they are skipped.
    let mut implemented: HashSet<(&str, &str)> = HashSet::new();
    for name in gossamer_interp::registered_names() {
        if !name.contains("::") || !name.chars().next().is_some_and(char::is_lowercase) {
            continue;
        }
        let mut segs: Vec<&str> = name.split("::").collect();
        if segs.first() == Some(&"std") {
            segs.remove(0);
        }
        let member = segs[segs.len() - 1];
        let binding_segs = &segs[..segs.len() - 1];
        if binding_segs
            .last()
            .is_some_and(|s| s.chars().next().is_some_and(char::is_uppercase))
        {
            continue;
        }
        let binding = binding_segs.join("::");
        if let Some(paths) = binding_to_paths.get(binding.as_str()) {
            for p in paths {
                implemented.insert((*p, member));
            }
        }
    }

    let phantoms: Vec<String> = gossamer_std::manifest::ALL_MODULES
        .iter()
        // A module whose lifecycle status departs from `Shipped`
        // (Experimental / Planned) is honestly disclosed by
        // `gos feature-status`; its surface is allowed to have gaps.
        .filter(|m| lookup(m.path).is_none_or(|e| e.status == Status::Shipped))
        .flat_map(|m| {
            let path = m.path.strip_prefix("std::").unwrap_or(m.path);
            m.items
                .iter()
                .filter(|it| it.kind == gossamer_std::registry::StdItemKind::Function)
                .map(move |it| (path, it.name))
        })
        .filter(|(path, name)| !implemented.contains(&(*path, *name)))
        .map(|(path, name)| format!("{path}::{name}"))
        .filter(|p| !MANIFEST_IMPL_VIA_REWRITE.contains(&p.as_str()))
        .collect();

    assert!(
        phantoms.is_empty(),
        "{n} manifest Function item(s) advertise a function that resolves to NO \
         implementation - they are listed by `gos doc` and default to \"Shipped\" in \
         the feature-status registry, pass `gos check` (for 3-segment paths), then \
         fail at runtime with GX0002 / a compiled build error.\nEither wire the \
         function on all three tiers (interp builtin + c_abi shim + cranelift + llvm \
         dispatch) and add a tier-parity fixture, or remove the StdItem from its \
         manifest/*.rs module:\n  {phantoms:#?}",
        n = phantoms.len()
    );

    // Guard the allowlist against rot: every rewrite-backed entry must
    // still be an advertised manifest Function, else it is dead weight
    // masking a future regression.
    let manifest_fns: std::collections::HashSet<String> = gossamer_std::manifest::ALL_MODULES
        .iter()
        .flat_map(|m| {
            let path = m.path.strip_prefix("std::").unwrap_or(m.path);
            m.items
                .iter()
                .filter(|it| it.kind == gossamer_std::registry::StdItemKind::Function)
                .map(move |it| format!("{path}::{}", it.name))
        })
        .collect();
    let stale: Vec<&&str> = MANIFEST_IMPL_VIA_REWRITE
        .iter()
        .filter(|p| !manifest_fns.contains(**p))
        .collect();
    assert!(
        stale.is_empty(),
        "MANIFEST_IMPL_VIA_REWRITE entries no longer advertised as manifest \
         Functions (remove them): {stale:#?}"
    );
}

#[test]
fn stdlib_module_paths_match_manifest() {
    let mut live: Vec<&str> = gossamer_std::manifest::ALL_MODULES
        .iter()
        .map(|m| m.path.strip_prefix("std::").unwrap_or(m.path))
        .collect();
    live.sort_unstable();
    live.dedup();

    let table = gossamer_resolve::STDLIB_MODULE_PATHS;
    assert!(
        table.windows(2).all(|w| w[0] < w[1]),
        "STDLIB_MODULE_PATHS must be sorted for binary search"
    );
    let missing: Vec<&str> = live
        .iter()
        .filter(|n| !table.contains(n))
        .copied()
        .collect();
    let extra: Vec<&str> = table
        .iter()
        .filter(|n| !live.contains(n))
        .copied()
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "module path table drifted from the std manifest.\n  \
         missing from table: {missing:?}\n  extra in table: {extra:?}"
    );
}
