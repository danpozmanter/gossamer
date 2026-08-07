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
