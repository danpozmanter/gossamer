//! Object-level evidence for the Mach-O `-dead_strip` literal fix.
//!
//! On macOS the release/debug link step passes `-Wl,-dead_strip`,
//! which strips at *atom* granularity. Cranelift lowers a string
//! literal to a named local read-only data symbol in `__const` and
//! references it with a GOT-relative relocation. ld64's dead-strip
//! does not reliably keep such a local data atom alive through that
//! reference, so the atom is removed or reordered and the reference
//! resolves into a neighbouring atom (the observed corruption: a
//! short literal printing as a stray control byte). The LLVM tier
//! avoids this by emitting `private unnamed_addr constant` globals,
//! which are referenced directly and merged by the linker.
//!
//! `cranelift-module` exposes `DataDescription::set_used`, which the
//! object backend lowers to `N_NO_DEAD_STRIP` on Mach-O — the
//! canonical "do not dead-strip this symbol" marker. The codegen now
//! sets it on every interned literal and RC-meta blob. This test
//! emits a Mach-O object on any host and asserts the retain flag is
//! present on a `used` literal and absent on a plain one.

use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::{DataDescription, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use object::SymbolFlags;
use object::macho::N_NO_DEAD_STRIP;
use object::read::{File, Object, ObjectSymbol};
use target_lexicon::Triple;

fn macho_isa() -> std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa> {
    // Target the *host* architecture's Apple/Mach-O triple so the
    // backend is always compiled in (an aarch64 macOS runner has no
    // x86 backend, and vice versa). Only the OS changes, which is all
    // the Mach-O object format + N_NO_DEAD_STRIP path depends on.
    let arch = std::env::consts::ARCH;
    let triple: Triple = format!("{arch}-apple-darwin").parse().unwrap();
    let mut fb = settings::builder();
    fb.set("is_pic", "true").unwrap();
    fb.set("opt_level", "speed").unwrap();
    let flags = settings::Flags::new(fb);
    cranelift_codegen::isa::lookup(triple)
        .expect("x86_64-apple-darwin isa")
        .finish(flags)
        .expect("isa finish")
}

fn define_str(module: &mut ObjectModule, symbol: &str, text: &str, used: bool) {
    let id = module
        .declare_data(symbol, Linkage::Local, false, false)
        .unwrap();
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let mut desc = DataDescription::new();
    desc.define(bytes.into_boxed_slice());
    if used {
        desc.set_used(true);
    }
    module.define_data(id, &desc).unwrap();
}

#[test]
fn used_literal_carries_no_dead_strip_on_macho() {
    let isa = macho_isa();
    let builder = ObjectBuilder::new(
        isa,
        b"test".to_vec(),
        cranelift_module::default_libcall_names(),
    )
    .unwrap();
    let mut module = ObjectModule::new(builder);

    // One literal emitted the way the codegen now does (used), one the
    // pre-fix way (plain local atom).
    define_str(&mut module, ".Lstr_used", "w=", true);
    define_str(&mut module, ".Lstr_plain", "ok", false);

    let bytes = module.finish().emit().unwrap();
    let file = File::parse(&*bytes).unwrap();

    // Mach-O prepends `_` to every symbol name.
    let flag_of = |name: &str| -> u16 {
        let sym = file
            .symbols()
            .find(|s| s.name().is_ok_and(|n| n.ends_with(name)))
            .unwrap_or_else(|| panic!("symbol {name} present"));
        match sym.flags() {
            SymbolFlags::MachO { n_desc } => n_desc,
            other => panic!("expected Mach-O symbol flags, got {other:?}"),
        }
    };

    assert_eq!(
        flag_of(".Lstr_used") & N_NO_DEAD_STRIP,
        N_NO_DEAD_STRIP,
        "used literal must carry N_NO_DEAD_STRIP so the macOS linker keeps the atom",
    );
    assert_eq!(
        flag_of(".Lstr_plain") & N_NO_DEAD_STRIP,
        0,
        "control: a plain local atom carries no retain flag",
    );
}
