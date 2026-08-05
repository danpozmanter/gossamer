//! Typed runtime ABI registry for the Gossamer compiler.
//!
//! A single source of truth for every `gos_rt_*` symbol's name and
//! C-ABI signature. Consumers (LLVM lowerer, Cranelift backend,
//! dispatch-consistency verifier) all derive their declarations from
//! this registry instead of maintaining parallel string arrays.

/// Reference-counting type-meta ABI (kind tags + blob layout) shared by
/// the MIR lowerer and the runtime.
pub mod rc;
/// ABI registry - the typed list of all `gos_rt_*` symbols.
pub mod registry;
/// Core ABI types: [`AbiType`], [`AbiSig`], [`RuntimeEntry`].
pub mod types;

/// Tag byte introducing a nested tuple in a `gos_rt_tuple_format` /
/// `gos_rt_tuple_cmp` tag stream. The next byte is the nested tuple's
/// element count, followed by that many tags, recursively. A nested
/// tuple's slots are flattened into the parent's buffer, so the stream
/// is walked with separate tag and slot cursors.
pub const TUPLE_TAG_NESTED: u8 = 8;

pub use registry::{REGISTRY, all_llvm_declarations, lookup};
pub use types::{AbiSig, AbiType, RuntimeEntry, Tier};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win64_marshals_fat_i128_across_the_ffi_boundary() {
        // A scalar `i128` by value has no stable `extern "C"` ABI on Win64
        // (llc uses a GP register pair; rustc uses a by-pointer arg + a
        // `<16 x i8>` return). The Windows declaration must therefore render a
        // Fat `i128` argument as `ptr` and a Fat `i128` return as `<16 x i8>`,
        // matching the call-site marshalling. SysV keeps bare `i128`.
        let entry =
            lookup("gos_rt_result_default_with").expect("gos_rt_result_default_with is registered");
        assert_eq!(entry.sig.ret, types::AbiType::I64);
        assert_eq!(entry.sig.params[0], types::AbiType::I128);

        let win = entry.llvm_declare_for(true);
        assert!(
            win.contains("@gos_rt_result_default_with(ptr,"),
            "Win64 must pass a Fat i128 argument by pointer: {win}"
        );
        assert!(
            !win.contains("i128"),
            "Win64 declaration must not contain a bare i128: {win}"
        );

        let sysv = lookup("gos_rt_result_new")
            .expect("gos_rt_result_new is registered")
            .llvm_declare_for(false);
        assert!(
            sysv.starts_with("declare i128 @gos_rt_result_new("),
            "SysV must keep the bare i128 return: {sysv}"
        );
        let win_ret = lookup("gos_rt_result_new").unwrap().llvm_declare_for(true);
        assert!(
            win_ret.starts_with("declare <16 x i8> @gos_rt_result_new("),
            "Win64 must return a Fat i128 in a 16-byte vector register: {win_ret}"
        );
    }

    #[test]
    fn registry_is_sorted() {
        let names: Vec<&str> = REGISTRY.iter().map(|e| e.name).collect();

        let mut sorted = names.clone();
        sorted.sort_unstable();

        if let Some((idx, (actual, expected))) = names
            .iter()
            .zip(sorted.iter())
            .enumerate()
            .find(|(_, (a, b))| a != b)
        {
            panic!("REGISTRY out of order at index {idx}: found {actual:?}, expected {expected:?}");
        }
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
                "declare does not contain symbol name for {0}: {1}",
                entry.name,
                decl
            );
        }
    }

    #[test]
    fn registry_size_sanity() {
        assert!(
            REGISTRY.len() > 200,
            "only {} entries - registry likely truncated",
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
                    "void return type must produce 'declare void' for {0}: {1}",
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
                "entry {0} has an empty docs field",
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
