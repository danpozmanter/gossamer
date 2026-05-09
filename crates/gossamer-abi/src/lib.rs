//! Typed runtime ABI registry for the Gossamer compiler.
//!
//! A single source of truth for every `gos_rt_*` symbol's name and
//! C-ABI signature. Consumers (LLVM lowerer, Cranelift backend,
//! dispatch-consistency verifier) all derive their declarations from
//! this registry instead of maintaining parallel string arrays.

/// ABI registry — the typed list of all `gos_rt_*` symbols.
pub mod registry;
/// Core ABI types: [`AbiType`], [`AbiSig`], [`RuntimeEntry`].
pub mod types;

pub use registry::{REGISTRY, all_llvm_declarations, lookup};
pub use types::{AbiSig, AbiType, RuntimeEntry, Tier};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted() {
        let names: Vec<&str> = REGISTRY.iter().map(|e| e.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "REGISTRY must be sorted alphabetically by name for lookup correctness"
        );
    }

    #[test]
    fn registry_no_duplicates() {
        let mut names: Vec<&str> = REGISTRY.iter().map(|e| e.name).collect();
        let orig_len = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(orig_len, names.len(), "REGISTRY contains duplicate entries");
    }

    #[test]
    fn all_names_have_gos_rt_prefix() {
        for entry in REGISTRY {
            assert!(
                entry.name.starts_with("gos_rt_"),
                "entry {:?} does not start with gos_rt_",
                entry.name
            );
        }
    }

    #[test]
    fn llvm_declare_round_trips() {
        for entry in REGISTRY {
            let decl = entry.llvm_declare();
            assert!(
                decl.starts_with("declare "),
                "malformed declare for {}: {}",
                entry.name,
                decl
            );
            assert!(
                decl.contains(&format!("@{}", entry.name)),
                "declare does not contain symbol name for {}: {}",
                entry.name,
                decl
            );
        }
    }

    #[test]
    fn registry_size_sanity() {
        assert!(
            REGISTRY.len() > 200,
            "only {} entries — registry likely truncated",
            REGISTRY.len()
        );
    }

    #[test]
    fn void_return_only_for_void_type() {
        use AbiType::Void;
        for entry in REGISTRY {
            if entry.sig.ret == Void {
                let decl = entry.llvm_declare();
                assert!(
                    decl.starts_with("declare void "),
                    "void return type must produce 'declare void' for {}: {}",
                    entry.name,
                    decl
                );
            }
        }
    }

    #[test]
    fn docs_field_is_non_empty() {
        for entry in REGISTRY {
            assert!(
                !entry.docs.is_empty(),
                "entry {} has an empty docs field",
                entry.name
            );
        }
    }

    #[test]
    fn tier_field_coverage() {
        use types::Tier;
        let both = REGISTRY.iter().filter(|e| e.tier == Tier::Both).count();
        let cl = REGISTRY
            .iter()
            .filter(|e| e.tier == Tier::Cranelift)
            .count();
        let ll = REGISTRY.iter().filter(|e| e.tier == Tier::Llvm).count();
        assert!(both >= 30, "expected >=30 Both-tier entries, got {both}");
        assert!(cl >= 100, "expected >=100 Cranelift-tier entries, got {cl}");
        assert!(ll >= 1, "expected >=1 Llvm-tier entries, got {ll}");
        assert_eq!(
            both + cl + ll,
            REGISTRY.len(),
            "every entry must have a tier"
        );
    }
}
