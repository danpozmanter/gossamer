//! Keeps the resolver's checked-in stdlib export table in sync with
//! the runtime builtin registry. If the stdlib surface changes, this
//! fails with the diff so the table in
//! `gossamer-resolve/src/stdlib_exports.rs` is regenerated — without
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
