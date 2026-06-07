//! Object-level evidence for the Windows/COFF PIC string-reference fix.
//!
//! COFF has no GOT. Under `is_pic`, cranelift lowers a symbol address
//! to `movq sym@GOTPCREL(%rip), %dst` (a load *through* a GOT slot)
//! and emits a `Reloc::X86GOTPCRel4`. The object backend's COFF writer
//! rewrites `GotRelative` to a plain `Relative` reloc pointing at the
//! symbol itself, so the `movq` loads the symbol's first 8 bytes as if
//! they were its address — corrupting every string/data reference.
//!
//! `build_native_isa` therefore disables PIC on Windows. With PIC off
//! and a colocated (near) symbol, cranelift emits `leaq sym(%rip)` — a
//! direct relative reference that COFF/PE resolve correctly. The reloc
//! *type* is `IMAGE_REL_AMD64_REL32` either way, so the distinguishing
//! signal is the instruction opcode at the relocation site: `lea`
//! (0x8d) computes the address, `movq` (0x8b) loads through the slot.

// This suite cross-builds an `x86_64-pc-windows-msvc` COFF object in
// memory, which needs Cranelift's x86 backend. With the default
// `host-arch` feature set that backend is only compiled on x86_64
// hosts, so `isa::lookup("x86_64-…")` returns `Err` on the aarch64
// macOS runner and the test panics. The COFF/x86 PIC behaviour it
// checks is covered by the Linux and Windows x86_64 CI jobs.
#![cfg(target_arch = "x86_64")]

use cranelift_codegen::Context;
use cranelift_codegen::ir as cir;
use cranelift_codegen::ir::immediates::Imm64;
use cranelift_codegen::ir::{
    AbiParam, ExternalName, GlobalValueData, InstBuilder, UserExternalName, UserFuncName,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use object::read::{File, Object, ObjectSection};
use target_lexicon::Triple;

fn coff_isa(pic: bool) -> std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa> {
    let triple: Triple = "x86_64-pc-windows-msvc".parse().unwrap();
    let mut fb = settings::builder();
    fb.set("is_pic", if pic { "true" } else { "false" })
        .unwrap();
    fb.set("opt_level", "speed").unwrap();
    cranelift_codegen::isa::lookup(triple)
        .expect("windows-msvc isa")
        .finish(settings::Flags::new(fb))
        .expect("isa finish")
}

/// Emits a COFF function that takes the address of a colocated local
/// string and returns the opcode byte at the relocation site in
/// `.text` (the instruction reading the symbol).
fn symbol_ref_opcode(pic: bool) -> u8 {
    let isa = coff_isa(pic);
    let ptr_ty = isa.pointer_type();
    let call_conv = isa.default_call_conv();
    let builder = ObjectBuilder::new(
        isa,
        b"t".to_vec(),
        cranelift_module::default_libcall_names(),
    )
    .unwrap();
    let mut module = ObjectModule::new(builder);

    let data = module
        .declare_data(".Lstr_w", Linkage::Local, false, false)
        .unwrap();
    let mut desc = DataDescription::new();
    desc.define(b"w=\0".to_vec().into_boxed_slice());
    module.define_data(data, &desc).unwrap();

    let mut sig = module.make_signature();
    sig.call_conv = call_conv;
    sig.returns.push(AbiParam::new(ptr_ty));
    let func_id = module
        .declare_function("take_addr", Linkage::Export, &sig)
        .unwrap();

    let mut func = cir::Function::with_name_signature(UserFuncName::user(0, func_id.as_u32()), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut fb = FunctionBuilder::new(&mut func, &mut fb_ctx);
        let block = fb.create_block();
        fb.switch_to_block(block);
        fb.seal_block(block);
        let unr = fb.func.declare_imported_user_function(UserExternalName {
            namespace: 1,
            index: data.as_u32(),
        });
        let gv = fb.func.create_global_value(GlobalValueData::Symbol {
            name: ExternalName::user(unr),
            offset: Imm64::new(0),
            colocated: true,
            tls: false,
        });
        let addr = fb.ins().global_value(ptr_ty, gv);
        fb.ins().return_(&[addr]);
        fb.finalize();
    }
    let mut ctx = Context::for_function(func);
    module.define_function(func_id, &mut ctx).unwrap();

    let bytes = module.finish().emit().unwrap();
    let file = File::parse(&*bytes).unwrap();

    let text = file
        .sections()
        .find(|s| s.name() == Ok(".text"))
        .expect(".text section");
    let (off, _reloc) = text
        .relocations()
        .next()
        .expect("a relocation in .text referencing the string");
    let data = text.data().expect(".text bytes");
    // RIP-relative encoding at the reloc site: `REX 8x /05 <disp32>`.
    // The reloc points at the 4-byte disp32, so the opcode is two
    // bytes earlier (opcode, modrm, disp32...).
    let op_idx = (off as usize) - 2;
    data[op_idx]
}

#[test]
fn coff_symbol_reference_uses_lea_without_pic() {
    // The fix: non-PIC COFF computes the address with `lea` (0x8d),
    // not a GOT load. (x86_64 opcode check; the Windows CI is x64.)
    assert_eq!(
        symbol_ref_opcode(false),
        0x8d,
        "non-PIC COFF must reference the local string with lea, not a GOT load"
    );
}

#[test]
fn coff_symbol_reference_uses_got_load_with_pic() {
    // Documents the broken shape PIC produces on COFF: a `movq`
    // (0x8b) GOT load whose reloc COFF rewrites to a direct REL32 —
    // loading the string's bytes as if they were its address.
    assert_eq!(
        symbol_ref_opcode(true),
        0x8b,
        "PIC COFF emits a GOT load (movq), which COFF cannot represent correctly"
    );
}
