//! In-process Cranelift JIT used by `gos run --vm`.
//!
//! Reuses the [`super::native::lower_program`] HIR → MIR → CLIF
//! pipeline that the AOT object backend drives, swapping the
//! `ObjectModule` for a `JITModule`. The resulting raw fn pointers
//! are returned in a [`JitArtifact`] that the bytecode VM reads at
//! every `Op::Call` so hot user functions execute as native code
//! instead of dispatching through the bytecode loop.
//!
//! The VM's register-based dispatch maps cleanly onto SSA, so the
//! same MIR form the AOT path consumes drops straight in. Functions
//! whose codegen path can't lower a feature (closures, dynamic
//! shapes, …) are simply skipped; the VM's existing bytecode
//! interpreter still handles them.

#![allow(unsafe_code)]

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use cranelift_jit::{JITBuilder, JITModule};
use gossamer_mir::Body;
use gossamer_types::{Ty, TyCtxt, TyKind};

use crate::native::{build_native_isa, lower_program};

/// Cranelift register class for one parameter or return slot of a
/// JIT-compiled body. Used by the dispatch trampoline to pick the
/// right marshalling shape per slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitKind {
    /// A 64-bit signed integer (`i64`, `i32` widened, `usize`, …).
    I64,
    /// A 64-bit IEEE-754 float.
    F64,
    /// A 1-bit boolean represented as `i8` in the cranelift ABI.
    Bool,
    /// The unit value (no representation; the body has no return).
    Unit,
    /// A runtime [`GossamerValue`] — the u64-packed shape the
    /// codegen uses for any non-scalar type (String, Tuple, Array,
    /// Struct, Variant, Closure, Channel). Aggregate values cross
    /// the JIT boundary as `gossamer_runtime::GossamerValue`
    /// handles; the trampoline marshals via
    /// `Value::to_raw` / `Value::from_raw`.
    Value,
}

/// Raw handle for a JIT-compiled function: a fn pointer plus the
/// per-slot kinds that tell the dispatch trampoline how to marshal
/// arguments and the return value.
#[derive(Debug, Clone)]
pub struct JitFn {
    /// The Gossamer source name of the function. Mainly for
    /// `GOS_JIT_TRACE` diagnostics.
    pub name: String,
    /// Raw pointer to the entry of the compiled function. Valid for
    /// the lifetime of the owning [`JitArtifact`].
    pub ptr: *const u8,
    /// One [`JitKind`] per parameter, in source order.
    pub params: Vec<JitKind>,
    /// The return slot's kind.
    pub returns: JitKind,
}

// SAFETY: `ptr` is read-only from any thread, but the VM is
// single-threaded today. We do not implement Send/Sync for `JitFn`
// — anyone who copies it must keep it on the owning thread.

/// Owns a finalised [`JITModule`] and a name → [`JitFn`] map.
/// Dropping the artifact frees every page that backs the function
/// pointers it has handed out, so the VM must hold the artifact
/// for as long as any compiled fn is reachable.
pub struct JitArtifact {
    /// `Option` so [`Drop`] can call `JITModule::free_memory(self)`,
    /// which takes the module by value.
    module: Option<JITModule>,
    /// Compiled functions keyed by their Gossamer source name.
    pub functions: HashMap<String, JitFn>,
}

impl std::fmt::Debug for JitArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `module` field is intentionally omitted — its
        // pointer-shaped `Debug` output churns across runs and
        // adds no signal. `finish_non_exhaustive` documents the
        // skip in a clippy-blessed way.
        f.debug_struct("JitArtifact")
            .field("functions", &self.functions.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Drop for JitArtifact {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: we have unique ownership of the JITModule (the
            // `Option::take` above is single-threaded), and the VM
            // promises to drop the artifact only after every JitFn
            // copy in its globals table has been flushed.
            unsafe { module.free_memory() };
        }
    }
}

/// Compiles every body in `bodies` through cranelift-jit and returns
/// the resulting handle table. Functions whose codegen path errors,
/// or whose ABI shape is not supported by the dispatch trampoline,
/// are silently skipped — the VM's existing bytecode dispatch picks
/// them up.
pub fn compile_to_jit(bodies: &[Body], tcx: &TyCtxt) -> Result<JitArtifact> {
    let isa = build_native_isa(false)?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    register_runtime_symbols(&mut builder);
    let mut module = JITModule::new(builder);

    // Rename the user's `main` to `gos_main` in the JIT's symbol
    // table. The host binary already exports `main` (the Rust
    // runtime's entry point); declaring a second `Linkage::Local`
    // `main` produced flaky SIGILLs on bring-up. The lookup map
    // we hand back to the VM keeps the original Gossamer name as
    // the key, so dispatch is unaffected.
    let lowered = lower_program(&mut module, bodies, tcx, Some("gos_main"))?;

    module
        .finalize_definitions()
        .map_err(|e| anyhow!("jit finalize: {e}"))?;

    let mut functions = HashMap::new();
    for body in bodies {
        let Some(id) = lowered.function_ids_by_name.get(&body.name).copied() else {
            continue;
        };
        let Some((params, returns)) = body_kinds(body, tcx) else {
            // Some param/return type isn't a primitive scalar — the
            // dispatch trampoline can't marshal it, so the VM will
            // fall back to bytecode for this fn.
            continue;
        };
        let ptr = module.get_finalized_function(id);
        functions.insert(
            body.name.clone(),
            JitFn {
                name: body.name.clone(),
                ptr,
                params,
                returns,
            },
        );
    }

    Ok(JitArtifact {
        module: Some(module),
        functions,
    })
}

fn body_kinds(body: &Body, tcx: &TyCtxt) -> Option<(Vec<JitKind>, JitKind)> {
    let mut params = Vec::with_capacity(body.arity as usize);
    for pidx in 1..=body.arity {
        let local = gossamer_mir::Local(pidx);
        let kind = ty_to_kind(tcx, body.local_ty(local))?;
        params.push(kind);
    }
    let returns = ty_to_kind(tcx, body.local_ty(gossamer_mir::Local::RETURN))?;
    Some((params, returns))
}

fn ty_to_kind(tcx: &TyCtxt, ty: Ty) -> Option<JitKind> {
    match tcx.kind_of(ty) {
        TyKind::Bool => Some(JitKind::Bool),
        TyKind::Int(_) => Some(JitKind::I64),
        TyKind::Float(_) => Some(JitKind::F64),
        TyKind::Unit => Some(JitKind::Unit),
        // Aggregate types (`String`, `Tuple`, `Adt`, channels …)
        // intentionally return `None` here. The codegen lowers
        // them as native struct-pointer ABIs (load/store at
        // computed offsets), but the trampoline can only marshal
        // through the runtime's `GossamerValue` u64 handle ABI.
        // Until the codegen and runtime agree on a uniform
        // aggregate calling convention, JIT-promoting these
        // shapes risks segfaults. `JitKind::Value` is reserved
        // for that future work.
        _ => None,
    }
}

/// Registers every `gos_rt_*` C-ABI symbol the codegen may emit
/// against the JIT builder so that compiled bodies can call into the
/// runtime in-process. Kept in lock-step with the symbol set the
/// AOT object backend imports — anything the codegen knows how to
/// emit must resolve here.
#[allow(clippy::too_many_lines)]
fn register_runtime_symbols(builder: &mut JITBuilder) {
    use gossamer_runtime::c_abi as rt;
    use gossamer_runtime::gc;
    use gossamer_runtime::preempt;
    macro_rules! reg {
        ($($name:literal => $f:path),* $(,)?) => {
            $(
                builder.symbol($name, $f as *const u8);
            )*
        };
    }
    reg! {
        "gos_rt_set_args"            => rt::gos_rt_set_args,
        "gos_rt_os_args"             => rt::gos_rt_os_args,
        "gos_rt_arr_len"             => rt::gos_rt_arr_len,
        "gos_rt_len"                 => rt::gos_rt_len,
        "gos_rt_str_len"             => rt::gos_rt_str_len,
        "gos_rt_str_byte_at"         => rt::gos_rt_str_byte_at,
        "gos_rt_str_concat"          => rt::gos_rt_str_concat,
        "gos_rt_str_trim"            => rt::gos_rt_str_trim,
        "gos_rt_str_to_upper"        => rt::gos_rt_str_to_upper,
        "gos_rt_str_to_lower"        => rt::gos_rt_str_to_lower,
        "gos_rt_str_contains"        => rt::gos_rt_str_contains,
        "gos_rt_str_starts_with"     => rt::gos_rt_str_starts_with,
        "gos_rt_str_ends_with"       => rt::gos_rt_str_ends_with,
        "gos_rt_str_find"            => rt::gos_rt_str_find,
        "gos_rt_str_replace"         => rt::gos_rt_str_replace,
        "gos_rt_str_split"           => rt::gos_rt_str_split,
        "gos_rt_str_lines"           => rt::gos_rt_str_lines,
        "gos_rt_str_repeat"          => rt::gos_rt_str_repeat,
        "gos_rt_str_eq"              => rt::gos_rt_str_eq,
        "gos_rt_str_is_empty"        => rt::gos_rt_str_is_empty,
        "gos_rt_len_is_zero"         => rt::gos_rt_len_is_zero,
        "gos_rt_error_new"           => rt::gos_rt_error_new,
        "gos_rt_error_wrap"          => rt::gos_rt_error_wrap,
        "gos_rt_error_message"       => rt::gos_rt_error_message,
        "gos_rt_error_cause"         => rt::gos_rt_error_cause,
        "gos_rt_error_is"            => rt::gos_rt_error_is,
        "gos_rt_regex_compile"       => rt::gos_rt_regex_compile,
        "gos_rt_regex_is_match"      => rt::gos_rt_regex_is_match,
        "gos_rt_regex_find"          => rt::gos_rt_regex_find,
        "gos_rt_regex_find_all"      => rt::gos_rt_regex_find_all,
        "gos_rt_regex_replace_all"   => rt::gos_rt_regex_replace_all,
        "gos_rt_regex_split"         => rt::gos_rt_regex_split,
        "gos_rt_fs_read_to_string"   => rt::gos_rt_fs_read_to_string,
        "gos_rt_fs_write"            => rt::gos_rt_fs_write,
        "gos_rt_fs_create_dir_all"   => rt::gos_rt_fs_create_dir_all,
        "gos_rt_path_join"           => rt::gos_rt_path_join,
        "gos_rt_flag_set_new"        => rt::gos_rt_flag_set_new,
        "gos_rt_flag_set_string"     => rt::gos_rt_flag_set_string,
        "gos_rt_flag_set_uint"       => rt::gos_rt_flag_set_uint,
        "gos_rt_flag_set_bool"       => rt::gos_rt_flag_set_bool,
        "gos_rt_flag_set_parse"      => rt::gos_rt_flag_set_parse,
        "gos_rt_bufio_scanner_new"   => rt::gos_rt_bufio_scanner_new,
        "gos_rt_bufio_scanner_scan"  => rt::gos_rt_bufio_scanner_scan,
        "gos_rt_bufio_scanner_text"  => rt::gos_rt_bufio_scanner_text,
        "gos_rt_http_client_new"     => rt::gos_rt_http_client_new,
        "gos_rt_http_client_get"     => rt::gos_rt_http_client_get,
        "gos_rt_http_client_post"    => rt::gos_rt_http_client_post,
        "gos_rt_http_request_header" => rt::gos_rt_http_request_header,
        "gos_rt_http_request_body"   => rt::gos_rt_http_request_body,
        "gos_rt_http_request_send"   => rt::gos_rt_http_request_send,
        "gos_rt_http_response_status" => rt::gos_rt_http_response_status,
        "gos_rt_http_response_body"  => rt::gos_rt_http_response_body,
        "gos_rt_vec_get_i64"         => rt::gos_rt_vec_get_i64,
        "gos_rt_vec_set_i64"         => rt::gos_rt_vec_set_i64,
        "gos_rt_vec_format_i64"      => rt::gos_rt_vec_format_i64,
        "gos_rt_concat_init"         => rt::gos_rt_concat_init,
        "gos_rt_concat_str"          => rt::gos_rt_concat_str,
        "gos_rt_concat_i64"          => rt::gos_rt_concat_i64,
        "gos_rt_concat_f64"          => rt::gos_rt_concat_f64,
        "gos_rt_concat_f64_prec"     => rt::gos_rt_concat_f64_prec,
        "gos_rt_concat_bool"         => rt::gos_rt_concat_bool,
        "gos_rt_concat_char"         => rt::gos_rt_concat_char,
        "gos_rt_concat_finish"       => rt::gos_rt_concat_finish,
        "gos_rt_main_exit_code"      => rt::gos_rt_main_exit_code,
        "gos_rt_result_new"          => rt::gos_rt_result_new,
        "gos_rt_result_disc"         => rt::gos_rt_result_disc,
        "gos_rt_result_payload"      => rt::gos_rt_result_payload,
        "gos_rt_set_new"             => rt::gos_rt_set_new,
        "gos_rt_set_insert"          => rt::gos_rt_set_insert,
        "gos_rt_set_contains"        => rt::gos_rt_set_contains,
        "gos_rt_set_remove"          => rt::gos_rt_set_remove,
        "gos_rt_set_len"             => rt::gos_rt_set_len,
        "gos_rt_btmap_new"           => rt::gos_rt_btmap_new,
        "gos_rt_btmap_insert"        => rt::gos_rt_btmap_insert,
        "gos_rt_btmap_get_or"        => rt::gos_rt_btmap_get_or,
        "gos_rt_btmap_len"           => rt::gos_rt_btmap_len,
        "gos_rt_http_response_set_header" => rt::gos_rt_http_response_set_header,
        "gos_rt_http_response_get_header" => rt::gos_rt_http_response_get_header,
        "gos_rt_http_request_set_header" => rt::gos_rt_http_request_set_header,
        "gos_rt_http_request_get_header" => rt::gos_rt_http_request_get_header,
        "gos_rt_http_request_path"   => rt::gos_rt_http_request_path,
        "gos_rt_http_request_method" => rt::gos_rt_http_request_method,
        "gos_rt_http_request_query"  => rt::gos_rt_http_request_query,
        "gos_rt_http_request_body_str" => rt::gos_rt_http_request_body_str,
        "gos_rt_http_response_text_new" => rt::gos_rt_http_response_text_new,
        "gos_rt_http_response_json_new" => rt::gos_rt_http_response_json_new,
        "gos_rt_gzip_encode"         => rt::gos_rt_gzip_encode,
        "gos_rt_gzip_decode"         => rt::gos_rt_gzip_decode,
        "gos_rt_slog_info"           => rt::gos_rt_slog_info,
        "gos_rt_slog_warn"           => rt::gos_rt_slog_warn,
        "gos_rt_slog_error"          => rt::gos_rt_slog_error,
        "gos_rt_slog_debug"          => rt::gos_rt_slog_debug,
        "gos_rt_testing_check"       => rt::gos_rt_testing_check,
        "gos_rt_testing_check_eq_i64" => rt::gos_rt_testing_check_eq_i64,
        "gos_rt_parse_i64"           => rt::gos_rt_parse_i64,
        "gos_rt_parse_f64"           => rt::gos_rt_parse_f64,
        "gos_rt_i64_to_str"          => rt::gos_rt_i64_to_str,
        "gos_rt_f64_to_str"          => rt::gos_rt_f64_to_str,
        "gos_rt_f64_prec_to_str"     => rt::gos_rt_f64_prec_to_str,
        "gos_rt_flush_stdout"        => rt::gos_rt_flush_stdout,
        "gos_rt_print_str"           => rt::gos_rt_print_str,
        "gos_rt_print_i64"           => rt::gos_rt_print_i64,
        "gos_rt_print_f64"           => rt::gos_rt_print_f64,
        "gos_rt_print_bool"          => rt::gos_rt_print_bool,
        "gos_rt_print_char"          => rt::gos_rt_print_char,
        "gos_rt_io_stdin"            => rt::gos_rt_io_stdin,
        "gos_rt_io_stdout"           => rt::gos_rt_io_stdout,
        "gos_rt_io_stderr"           => rt::gos_rt_io_stderr,
        "gos_rt_stream_write_byte"   => rt::gos_rt_stream_write_byte,
        "gos_rt_stream_write_str"    => rt::gos_rt_stream_write_str,
        "gos_rt_stream_flush"        => rt::gos_rt_stream_flush,
        "gos_rt_stream_read_line"    => rt::gos_rt_stream_read_line,
        "gos_rt_stream_read_to_string" => rt::gos_rt_stream_read_to_string,
        "gos_rt_println"             => rt::gos_rt_println,
        "gos_rt_stdout_acquire"      => rt::gos_rt_stdout_acquire,
        "gos_rt_stdout_release"      => rt::gos_rt_stdout_release,
        "gos_rt_vec_new"             => rt::gos_rt_vec_new,
        "gos_rt_vec_with_capacity"   => rt::gos_rt_vec_with_capacity,
        "gos_rt_vec_len"             => rt::gos_rt_vec_len,
        "gos_rt_vec_push"            => rt::gos_rt_vec_push,
        "gos_rt_vec_push_i64"        => rt::gos_rt_vec_push_i64,
        "gos_rt_vec_get_ptr"         => rt::gos_rt_vec_get_ptr,
        "gos_rt_vec_pop"             => rt::gos_rt_vec_pop,
        "gos_rt_vec_slice"           => rt::gos_rt_vec_slice,
        "gos_rt_map_new"             => rt::gos_rt_map_new,
        "gos_rt_map_len"             => rt::gos_rt_map_len,
        "gos_rt_map_insert"          => rt::gos_rt_map_insert,
        "gos_rt_map_get"             => rt::gos_rt_map_get,
        "gos_rt_map_get_or_i64"      => rt::gos_rt_map_get_or_i64,
        "gos_rt_map_inc_i64"         => rt::gos_rt_map_inc_i64,
        "gos_rt_map_remove"          => rt::gos_rt_map_remove,
        "gos_rt_map_insert_i64_i64"  => rt::gos_rt_map_insert_i64_i64,
        "gos_rt_map_get_i64"         => rt::gos_rt_map_get_i64,
        "gos_rt_map_remove_i64"      => rt::gos_rt_map_remove_i64,
        "gos_rt_map_contains_key_i64" => rt::gos_rt_map_contains_key_i64,
        "gos_rt_map_insert_str_i64"  => rt::gos_rt_map_insert_str_i64,
        "gos_rt_map_get_str_i64"     => rt::gos_rt_map_get_str_i64,
        "gos_rt_map_insert_str_str"  => rt::gos_rt_map_insert_str_str,
        "gos_rt_map_get_str_str"     => rt::gos_rt_map_get_str_str,
        "gos_rt_map_contains_key_str" => rt::gos_rt_map_contains_key_str,
        "gos_rt_map_remove_str"      => rt::gos_rt_map_remove_str,
        "gos_rt_map_clear"           => rt::gos_rt_map_clear,
        "gos_rt_map_inc_at_str_i64"  => rt::gos_rt_map_inc_at_str_i64,
        "gos_rt_map_free"            => rt::gos_rt_map_free,
        "gos_rt_vec_free"            => rt::gos_rt_vec_free,
        "gos_rt_set_free"            => rt::gos_rt_set_free,
        "gos_rt_btmap_free"          => rt::gos_rt_btmap_free,
        "gos_rt_map_keys_i64"        => rt::gos_rt_map_keys_i64,
        "gos_rt_map_values_i64"      => rt::gos_rt_map_values_i64,
        "gos_rt_map_keys_str"        => rt::gos_rt_map_keys_str,
        "gos_rt_map_values_str"      => rt::gos_rt_map_values_str,
        "gos_rt_map_get_or_str_i64"  => rt::gos_rt_map_get_or_str_i64,
        "gos_rt_map_get_or_str_str"  => rt::gos_rt_map_get_or_str_str,
        "gos_rt_map_get_or_i64_str"  => rt::gos_rt_map_get_or_i64_str,
        "gos_rt_map_insert_i64_str"  => rt::gos_rt_map_insert_i64_str,
        "gos_rt_map_get_i64_str"     => rt::gos_rt_map_get_i64_str,
        "gos_rt_json_parse"          => rt::gos_rt_json_parse,
        "gos_rt_json_render"         => rt::gos_rt_json_render,
        "gos_rt_json_get"            => rt::gos_rt_json_get,
        "gos_rt_json_at"             => rt::gos_rt_json_at,
        "gos_rt_json_len"            => rt::gos_rt_json_len,
        "gos_rt_json_is_null"        => rt::gos_rt_json_is_null,
        "gos_rt_json_as_i64"         => rt::gos_rt_json_as_i64,
        "gos_rt_json_as_f64"         => rt::gos_rt_json_as_f64,
        "gos_rt_json_as_str"         => rt::gos_rt_json_as_str,
        "gos_rt_json_as_bool"        => rt::gos_rt_json_as_bool,
        "gos_rt_json_parsed_ok"      => rt::gos_rt_json_parsed_ok,
        "gos_rt_json_identity"       => rt::gos_rt_json_identity,
        "gos_rt_chan_new"            => rt::gos_rt_chan_new,
        "gos_rt_chan_send"           => rt::gos_rt_chan_send,
        "gos_rt_chan_try_send"       => rt::gos_rt_chan_try_send,
        "gos_rt_chan_recv"           => rt::gos_rt_chan_recv,
        "gos_rt_chan_try_recv"       => rt::gos_rt_chan_try_recv,
        "gos_rt_chan_close"          => rt::gos_rt_chan_close,
        "gos_rt_go_spawn"            => rt::gos_rt_go_spawn,
        "gos_rt_go_spawn_call_0"     => rt::gos_rt_go_spawn_call_0,
        "gos_rt_go_spawn_call_1"     => rt::gos_rt_go_spawn_call_1,
        "gos_rt_go_spawn_call_2"     => rt::gos_rt_go_spawn_call_2,
        "gos_rt_go_yield"            => rt::gos_rt_go_yield,
        "gos_rt_sleep_ns"            => rt::gos_rt_sleep_ns,
        "gos_rt_now_ns"              => rt::gos_rt_now_ns,
        "gos_rt_gc_alloc"            => rt::gos_rt_gc_alloc,
        "gos_rt_gc_alloc_rooted"     => gc::gos_rt_gc_alloc_rooted,
        "gos_rt_gc_shadow_push"      => gc::gos_rt_gc_shadow_push,
        "gos_rt_gc_shadow_save"      => gc::gos_rt_gc_shadow_save,
        "gos_rt_gc_shadow_restore"   => gc::gos_rt_gc_shadow_restore,
        "gos_rt_gc_collect_with_stack_roots"
                                     => gc::gos_rt_gc_collect_with_stack_roots,
        "gos_rt_gc_reset"            => rt::gos_rt_gc_reset,
        "gos_rt_http_serve"          => rt::gos_rt_http_serve,
        "gos_rt_panic"               => rt::gos_rt_panic,
        "gos_rt_exit"                => rt::gos_rt_exit,
        "gos_rt_time_now"            => rt::gos_rt_time_now,
        "gos_rt_math_sqrt"           => rt::gos_rt_math_sqrt,
        "gos_rt_math_pow"            => rt::gos_rt_math_pow,
        "gos_rt_math_sin"            => rt::gos_rt_math_sin,
        "gos_rt_math_cos"            => rt::gos_rt_math_cos,
        "gos_rt_math_log"            => rt::gos_rt_math_log,
        "gos_rt_math_exp"            => rt::gos_rt_math_exp,
        "gos_rt_math_abs"            => rt::gos_rt_math_abs,
        "gos_rt_math_floor"          => rt::gos_rt_math_floor,
        "gos_rt_math_ceil"           => rt::gos_rt_math_ceil,
        "gos_rt_time_now_ms"         => rt::gos_rt_time_now_ms,
        // Fn-trait coercion trampolines (closure_fn_trait_plan.md).
        // Emitted by the cranelift codegen when a bare `fn`/`fn item`
        // value is wrapped into a `Fn(args) -> ret` slot — the env
        // blob's offset 0 holds one of these, offset 8 holds the
        // real fn ptr.
        "gos_rt_fn_tramp_0"          => rt::gos_rt_fn_tramp_0,
        "gos_rt_fn_tramp_1"          => rt::gos_rt_fn_tramp_1,
        "gos_rt_fn_tramp_2"          => rt::gos_rt_fn_tramp_2,
        "gos_rt_fn_tramp_3"          => rt::gos_rt_fn_tramp_3,
        "gos_rt_fn_tramp_4"          => rt::gos_rt_fn_tramp_4,
        "gos_rt_fn_tramp_5"          => rt::gos_rt_fn_tramp_5,
        "gos_rt_fn_tramp_6"          => rt::gos_rt_fn_tramp_6,
        "gos_rt_fn_tramp_7"          => rt::gos_rt_fn_tramp_7,
        "gos_rt_fn_tramp_8"          => rt::gos_rt_fn_tramp_8,
        // Stringification helpers for compound `println!` /
        // `format!`. The codegen emits these whenever an arg's
        // print-kind is bool or char.
        "gos_rt_bool_to_str"         => rt::gos_rt_bool_to_str,
        "gos_rt_char_to_str"         => rt::gos_rt_char_to_str,
        // Block-write helpers used by `Stream::write_byte_array`
        // (the codegen emits this in fasta's repeat-fasta loop
        // for the bulk per-line dump).
        "gos_rt_stream_write_byte_array" => rt::gos_rt_stream_write_byte_array,
        // Heap-allocated i64 vector — `I64Vec` in source. Used
        // by fasta's section-TWO/THREE workers as the shared
        // scratch buffer.
        "gos_rt_heap_i64_new"        => rt::gos_rt_heap_i64_new,
        "gos_rt_heap_i64_free"       => rt::gos_rt_heap_i64_free,
        "gos_rt_heap_i64_get"        => rt::gos_rt_heap_i64_get,
        "gos_rt_heap_i64_set"        => rt::gos_rt_heap_i64_set,
        "gos_rt_heap_i64_len"        => rt::gos_rt_heap_i64_len,
        "gos_rt_heap_i64_write_lines_to_stdout"
                                     => rt::gos_rt_heap_i64_write_lines_to_stdout,
        "gos_rt_heap_i64_write_bytes_to_stdout"
                                     => rt::gos_rt_heap_i64_write_bytes_to_stdout,
        // U8Vec — 1-byte-per-element heap vec for fasta-shape
        // scratch buffers. Same shape as the i64 family but
        // with byte-aligned storage.
        "gos_rt_heap_u8_new"         => rt::gos_rt_heap_u8_new,
        "gos_rt_heap_u8_free"        => rt::gos_rt_heap_u8_free,
        "gos_rt_heap_u8_get"         => rt::gos_rt_heap_u8_get,
        "gos_rt_heap_u8_set"         => rt::gos_rt_heap_u8_set,
        "gos_rt_heap_u8_len"         => rt::gos_rt_heap_u8_len,
        "gos_rt_heap_u8_to_string"   => rt::gos_rt_heap_u8_to_string,
        "gos_rt_heap_u8_write_lines_to_stdout"
                                     => rt::gos_rt_heap_u8_write_lines_to_stdout,
        "gos_rt_heap_u8_write_bytes_to_stdout"
                                     => rt::gos_rt_heap_u8_write_bytes_to_stdout,
        // Sync primitives + LCG jump used by the goroutine
        // worker pattern in fasta / nbody.
        "gos_rt_mutex_new"           => rt::gos_rt_mutex_new,
        "gos_rt_mutex_lock"          => rt::gos_rt_mutex_lock,
        "gos_rt_mutex_unlock"        => rt::gos_rt_mutex_unlock,
        "gos_rt_wg_new"              => rt::gos_rt_wg_new,
        "gos_rt_wg_add"              => rt::gos_rt_wg_add,
        "gos_rt_wg_done"             => rt::gos_rt_wg_done,
        "gos_rt_wg_wait"             => rt::gos_rt_wg_wait,
        "gos_rt_wg_error"            => rt::gos_rt_wg_error,
        "gos_rt_wg_error_clear"      => rt::gos_rt_wg_error_clear,
        "gos_rt_atomic_i64_new"      => rt::gos_rt_atomic_i64_new,
        "gos_rt_atomic_i64_load"     => rt::gos_rt_atomic_i64_load,
        "gos_rt_atomic_i64_store"    => rt::gos_rt_atomic_i64_store,
        "gos_rt_atomic_i64_fetch_add"=> rt::gos_rt_atomic_i64_fetch_add,
        "gos_rt_atomic_i64_load_acquire"
                                     => rt::gos_rt_atomic_i64_load_acquire,
        "gos_rt_atomic_i64_store_release"
                                     => rt::gos_rt_atomic_i64_store_release,
        "gos_rt_atomic_i64_load_relaxed"
                                     => rt::gos_rt_atomic_i64_load_relaxed,
        "gos_rt_atomic_i64_store_relaxed"
                                     => rt::gos_rt_atomic_i64_store_relaxed,
        "gos_rt_atomic_i64_fetch_add_acqrel"
                                     => rt::gos_rt_atomic_i64_fetch_add_acqrel,
        "gos_rt_atomic_i64_cas"      => rt::gos_rt_atomic_i64_cas,
        "gos_rt_atomic_i64_cas_acq_rel"
                                     => rt::gos_rt_atomic_i64_cas_acq_rel,
        "gos_rt_atomic_i64_swap"     => rt::gos_rt_atomic_i64_swap,
        "gos_rt_preempt_check"       => preempt::gos_rt_preempt_check,
        "gos_rt_preempt_check_and_yield"
                                     => preempt::gos_rt_preempt_check_and_yield,
        "gos_rt_stdout_acquire"      => rt::gos_rt_stdout_acquire,
        "gos_rt_stdout_release"      => rt::gos_rt_stdout_release,
        "gos_rt_sync_i64_new"        => rt::gos_rt_sync_i64_new,
        "gos_rt_sync_i64_drop"       => rt::gos_rt_sync_i64_drop,
        "gos_rt_sync_i64_len"        => rt::gos_rt_sync_i64_len,
        "gos_rt_sync_i64_get"        => rt::gos_rt_sync_i64_get,
        "gos_rt_sync_i64_set"        => rt::gos_rt_sync_i64_set,
        "gos_rt_sync_i64_push"       => rt::gos_rt_sync_i64_push,
        "gos_rt_sync_i64_add"        => rt::gos_rt_sync_i64_add,
        "gos_rt_sync_u8_new"         => rt::gos_rt_sync_u8_new,
        "gos_rt_sync_u8_drop"        => rt::gos_rt_sync_u8_drop,
        "gos_rt_sync_u8_len"         => rt::gos_rt_sync_u8_len,
        "gos_rt_sync_u8_get"         => rt::gos_rt_sync_u8_get,
        "gos_rt_sync_u8_set"         => rt::gos_rt_sync_u8_set,
        "gos_rt_sync_u8_push"        => rt::gos_rt_sync_u8_push,
        "gos_rt_lcg_jump"            => rt::gos_rt_lcg_jump,
        "gos_rt_go_spawn_call_3"     => rt::gos_rt_go_spawn_call_3,
        "gos_rt_go_spawn_call_4"     => rt::gos_rt_go_spawn_call_4,
        "gos_rt_go_spawn_call_5"     => rt::gos_rt_go_spawn_call_5,
        "gos_rt_go_spawn_call_6"     => rt::gos_rt_go_spawn_call_6,
    }
}
