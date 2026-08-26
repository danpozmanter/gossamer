//! Pins the resolver's checked-in manifest-item table to this crate's
//! manifest. The resolver rejects `use std::module::Item` when no module
//! exports `Item`, so a manifest export missing from that table would be
//! reported as a nonexistent item.

#[test]
fn resolver_manifest_item_table_matches_manifest() {
    let mut manifest: Vec<String> = Vec::new();
    for module in gossamer_std::manifest::ALL_MODULES {
        let path = module.path.strip_prefix("std::").unwrap_or(module.path);
        for item in module.items {
            manifest.push(format!("{path}::{}", item.name));
        }
    }
    manifest.sort();
    manifest.dedup();

    let table: Vec<&str> = gossamer_resolve::STDLIB_MANIFEST_ITEMS.to_vec();
    // The lookups below are binary searches, so an unsorted table makes
    // every entry past the first inversion unreachable and reports it as
    // missing. Name the inversion instead of the entries it hides.
    let inversion = table.windows(2).find(|pair| pair[1] < pair[0]);
    assert!(
        inversion.is_none(),
        "STDLIB_MANIFEST_ITEMS is not sorted: {:?} precedes {:?}. \
         Entries are looked up by binary search; insert in sorted position.",
        inversion.map(|p| p[0]),
        inversion.map(|p| p[1]),
    );
    let missing: Vec<&String> = manifest
        .iter()
        .filter(|path| table.binary_search(&path.as_str()).is_err())
        .collect();
    let extra: Vec<&&str> = table
        .iter()
        .filter(|path| manifest.binary_search(&(*path).to_string()).is_err())
        .collect();

    assert!(
        missing.is_empty() && extra.is_empty(),
        "STDLIB_MANIFEST_ITEMS drifted from the stdlib manifest.\n  \
         missing from the table (regenerate gossamer-resolve/src/stdlib_exports.rs): \
         {missing:?}\n  extra in the table (no longer a manifest export): {extra:?}"
    );
}

#[test]
fn every_manifest_export_is_importable() {
    for module in gossamer_std::manifest::ALL_MODULES {
        let path = module.path.strip_prefix("std::").unwrap_or(module.path);
        for item in module.items {
            let qualified = format!("{path}::{}", item.name);
            assert!(
                gossamer_resolve::is_stdlib_item_path(&qualified),
                "`use std::{qualified}` would be rejected despite being a manifest export"
            );
        }
    }
}

#[test]
fn resolver_macro_item_table_matches_manifest() {
    let mut manifest: Vec<String> = Vec::new();
    for module in gossamer_std::manifest::ALL_MODULES {
        let path = module.path.strip_prefix("std::").unwrap_or(module.path);
        for item in module.items {
            if matches!(item.kind, gossamer_std::registry::StdItemKind::Builtin) {
                manifest.push(format!("{path}::{}", item.name));
            }
        }
    }
    manifest.sort();
    manifest.dedup();

    let table: Vec<String> = gossamer_resolve::STDLIB_MACRO_ITEMS
        .iter()
        .map(|path| (*path).to_string())
        .collect();
    assert_eq!(
        table, manifest,
        "STDLIB_MACRO_ITEMS drifted from the manifest's macro exports; \
         the resolver reports a call to one of these as GR0018, so a missing \
         entry lets that call pass `gos check` and fail at run time"
    );
}
