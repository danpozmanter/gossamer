#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Real Cranelift-backed native codegen.
//! Lowers a slice of MIR [`Body`]s into a `cranelift-object` module
//! and serialises the result as ELF (or the host's equivalent object
//! format). Supported today:
//! - `fn main() -> i64` with integer arithmetic (`+`, `-`, `*`, `/`,
//!   `%`, `&`, `|`, `^`, `<<`, `>>`, unary `-`, `!`),
//! - integer constants,
//! - direct calls between lowered functions,
//! - `return` of an `i64`.
//!
//! A C-ABI shim `main(argc, argv) -> i32` is emitted automatically:
//! it calls the Gossamer `main` and truncates the `i64` result into
//! the process exit code, so the object file links through a
//! standard `cc` invocation.
//! Aggregates (tuples/arrays/structs), strings, closures, and
//! anything that needs a GC heap are not yet lowered - those
//! constructs fall back to [`crate::emit::emit_module`] for
//! inspection.

// Allow patterns the Cranelift lowering deliberately uses:
//   - `similar_names` fires on `print_str`/`print_i64`/etc.
//     intrinsic-name shadowing within the same arm. The
//     parallel naming makes the dispatch table readable.
//   - `many_single_char_names` fires on hot inner-loop locals
//     (`a`, `b`, `n`, `m`, `k`) where longer names would
//     overflow the 100-col limit.
//   - `items_after_statements` flags inline `extern "C"` decls
//     localised to the one helper that uses them. Hoisting them
//     to module scope spreads the FFI surface; localised wins.
//   - `too_many_lines` / `cognitive_complexity` fire on the
//     intrinsic-dispatch arm and the `lower_intrinsic_call`
//     match. Splitting either hides the one-arm-per-symbol
//     structure that makes the table grep-able.
//   - `unnecessary_wraps` flags helpers whose `Result` exists
//     so call sites can still `?` them once a future lowering
//     can fail.
//   - `if_chain_can_be_rewritten_with_match` would flatten
//     short `if let Some(x) = .. else if let Some(y) = ..`
//     chains into match-on-tuple-of-options that's strictly
//     uglier here.
//   - `doc_markdown` flags identifiers like `i64`, `f64`,
//     etc. in plain-prose docs. Backticking every numeric
//     type name in every comment is noise.
//   - `manual_debug_impl` flags `JitModule`'s `Debug` impl
//     (which deliberately omits the JIT module pointer to keep
//     debug output stable across runs).
#![forbid(unsafe_code)]
#![allow(clippy::comparison_chain)]

use std::collections::HashMap;

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, Function, GlobalValueData, InstBuilder, MemFlags, Signature,
    StackSlotData, StackSlotKind, UserExternalName, UserFuncName, condcodes::IntCC,
    immediates::Imm64, types,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::{Context, ir};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleDeclarations};
use cranelift_object::{ObjectBuilder, ObjectModule};
use gossamer_mir::{
    BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, StatementKind, Terminator,
    UnOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};
use rayon::prelude::*;

use super::*;

pub(super) fn lower_generic_rt_call(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    intrinsics: &mut IntrinsicContext,
    destination: &gossamer_mir::Place,
    name: &'static str,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    // Signature table: arg cl-types + return cl-type. `None`
    // return means void.
    let (params, ret): (&[ir::Type], Option<ir::Type>) = match name {
        // 0.7.0 string surface.
        "gos_rt_str_split_once" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_str_rsplit_once" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_str_count" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_str_chars" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_str_strip_chars" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_str_lstrip_chars" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_str_rstrip_chars" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_str_zfill" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_str_center" => (&[ptr_ty, types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_str_slice" => (&[ptr_ty, types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_str_rfind_opt" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_strings_join" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_io_copy" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_io_read_all" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_uuid_v4" => (&[], Some(ptr_ty)),
        "gos_rt_uuid_v7" => (&[], Some(ptr_ty)),
        "gos_rt_uuid_is_valid" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_uuid_normalize" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_uuid_simple" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_path_base" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_path_dir" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_path_ext" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_first" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_pop_opt" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_last" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_reversed" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_index_of_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_vec_index_of_str" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_count_of_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_vec_count_of_str" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_vec_contains_i64" => (&[ptr_ty, types::I64], Some(types::I8)),
        "gos_rt_vec_contains_str" => (&[ptr_ty, ptr_ty], Some(types::I8)),
        "gos_rt_vec_slice_result" => (&[ptr_ty, types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_vec_insert_safe" => (&[ptr_ty, types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_vec_remove_safe" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_map_keys_vec" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_map_values_vec" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_map_pop_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_map_pop_str" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_min_i64" => (&[types::I64, types::I64], Some(types::I64)),
        "gos_rt_max_i64" => (&[types::I64, types::I64], Some(types::I64)),
        "gos_rt_clamp_i64" => (&[types::I64, types::I64, types::I64], Some(types::I64)),
        "gos_rt_min_f64" => (&[types::F64, types::F64], Some(types::F64)),
        "gos_rt_max_f64" => (&[types::F64, types::F64], Some(types::F64)),
        "gos_rt_clamp_f64" => (&[types::F64, types::F64, types::F64], Some(types::F64)),
        "gos_rt_error_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_from" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_wrap" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_error_message" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_display" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_cause" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_is" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_regex_compile" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_is_match" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_regex_find" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_find_opt" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_captures" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_find_all" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_captures_all" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_replace" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_replace_all" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_split" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_fs_read_to_string" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_fs_write" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_fs_create_dir_all" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_path_join" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_string" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_int" => (&[ptr_ty, ptr_ty, types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_uint" => (&[ptr_ty, ptr_ty, types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_float" => (&[ptr_ty, ptr_ty, types::F64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_bool" => (&[ptr_ty, ptr_ty, types::I8, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_duration" => (&[ptr_ty, ptr_ty, types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_string_list" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_short" => (&[ptr_ty, types::I64], None),
        "gos_rt_flag_set_usage" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_parse" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_duration_from_secs" => (&[types::I64], Some(types::I64)),
        "gos_rt_duration_from_millis" => (&[types::I64], Some(types::I64)),
        "gos_rt_time_format_rfc3339" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_time_parse_rfc3339" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_parse" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_map_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_os_env" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_program_name" => (&[], Some(ptr_ty)),
        "gos_rt_env_temp_dir" => (&[], Some(ptr_ty)),
        "gos_rt_env_home_dir" => (&[], Some(ptr_ty)),
        "gos_rt_os_cwd" => (&[], Some(ptr_ty)),
        "gos_rt_fs_list_dir" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_fs_walk_dir" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_exec_run" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_exec_spawn" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_exec_kill" => (&[types::I64], Some(types::I64)),
        "gos_rt_exec_signal" => (&[types::I64, types::I64], Some(types::I64)),
        "gos_rt_exec_kill_group" => (&[types::I64], Some(types::I64)),
        "gos_rt_exec_wait_timeout" => (&[types::I64, types::I64], Some(types::I64)),
        "gos_rt_exec_pipeline_run" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_signal_on" => (&[types::I32], Some(types::I64)),
        "gos_rt_signal_wait" => (&[types::I64], None),
        "gos_rt_signal_try_wait" => (&[types::I64], Some(types::I32)),
        "gos_rt_os_set_env" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_os_unset_env" => (&[ptr_ty], None),
        "gos_rt_os_user_current_name" => (&[], Some(ptr_ty)),
        "gos_rt_os_user_current_uid" => (&[], Some(types::I64)),
        "gos_rt_os_user_current_gid" => (&[], Some(types::I64)),
        "gos_rt_os_user_current_home" => (&[], Some(ptr_ty)),
        "gos_rt_os_user_lookup_uid" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_os_user_lookup_name" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_is_valid" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_is_v4" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_is_v6" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_is_loopback" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_is_unspecified" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_is_multicast" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_is_private" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_normalize" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_netip_host_of" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_netip_port_of" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_netip_join_addr_port" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_mime_parse" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_top" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_sub" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_charset" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_boundary" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_param" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_type_by_extension" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_extension_by_type" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_mime_is_valid" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_toml_to_json" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_toml_from_json" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_toml_is_valid" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_toml_pretty" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_yaml_to_json" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_yaml_from_json" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_yaml_is_valid" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_sync_map_new" => (&[], Some(ptr_ty)),
        "gos_rt_sync_map_set" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_sync_map_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_sync_map_delete" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_sync_map_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_sync_map_contains" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_sync_map_keys" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_barrier_new" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_barrier_wait" => (&[ptr_ty], None),
        "gos_rt_once_new" => (&[], Some(ptr_ty)),
        "gos_rt_once_call" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_math_rng_new" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_math_rng_next_f64" => (&[ptr_ty], Some(types::F64)),
        "gos_rt_math_rng_next_u32" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_math_rng_next_u64" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_math_rng_range_u64" => (&[ptr_ty, types::I64, types::I64], Some(types::I64)),
        "gos_rt_bytes_builder_new" => (&[], Some(ptr_ty)),
        "gos_rt_bytes_builder_with_capacity" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_bytes_builder_write" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_bytes_builder_write_char" => (&[ptr_ty, types::I32], None),
        "gos_rt_bytes_builder_build" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bytes_builder_as_str" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bytes_builder_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_bytes_buffer_new" => (&[], Some(ptr_ty)),
        "gos_rt_bytes_buffer_with_capacity" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_bytes_buffer_write_str" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_bytes_buffer_push" => (&[ptr_ty, types::I64], None),
        "gos_rt_bytes_buffer_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_bytes_buffer_is_empty" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_bytes_buffer_clear" => (&[ptr_ty], None),
        "gos_rt_bytes_buffer_to_string" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bytes_split" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_bytes_replace" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_net_ip_octets" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_tcp_listener_close" => (&[types::I64], None),
        "gos_rt_tcp_stream_close" => (&[types::I64], None),
        "gos_rt_udp_close" => (&[types::I64], None),
        "gos_rt_field_error_new" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_field_error_path" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_field_error_message" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_field_error_code" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_validate_errors_new" => (&[], Some(ptr_ty)),
        "gos_rt_validate_errors_add" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_validate_errors_is_empty" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_validate_errors_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_validate_errors_count" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_validate_errors_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_validate_errors_collect" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_rwlock_new" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_rwlock_get" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_rwlock_set" => (&[ptr_ty, types::I64], None),
        "gos_rt_rwlock_with_read" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_rwlock_with_write" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_ctx_background" => (&[], Some(ptr_ty)),
        "gos_rt_ctx_with_cancel" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_ctx_with_timeout" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_ctx_cancel" => (&[ptr_ty], None),
        "gos_rt_ctx_cancelled" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_ctx_is_cancelled" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_ctx_done" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_metrics_counter_new" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_metrics_counter_inc" => (&[ptr_ty], None),
        "gos_rt_metrics_counter_value" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_metrics_gauge_new" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_metrics_gauge_set" => (&[ptr_ty, types::F64], None),
        "gos_rt_metrics_gauge_inc" => (&[ptr_ty], None),
        "gos_rt_metrics_gauge_dec" => (&[ptr_ty], None),
        "gos_rt_metrics_gauge_value" => (&[ptr_ty], Some(types::F64)),
        "gos_rt_metrics_histogram_new" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_metrics_histogram_observe" => (&[ptr_ty, types::F64], None),
        "gos_rt_metrics_histogram_sum" => (&[ptr_ty], Some(types::F64)),
        "gos_rt_metrics_histogram_count" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_metrics_registry_new" => (&[], Some(ptr_ty)),
        "gos_rt_metrics_registry_register" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_metrics_registry_render" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_middleware_new" => (&[types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_middleware_serve" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_trace_tracer_new" => (&[], Some(ptr_ty)),
        "gos_rt_trace_tracer_start_span" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_trace_span_set_attribute" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_trace_span_set_status" => (&[ptr_ty, types::I64, ptr_ty], None),
        "gos_rt_trace_span_end" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_trace_ended_to_otlp_json" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bheap_push_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_bheap_pop_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bheap_peek_i64" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_bheap_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_vec_first_i64" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_vec_last_i64" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_vec_pop_front_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_pop_back_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_push_front_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_vec_push_back_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_ovec_insert_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_ovec_remove_at_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_ovec_contains_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_ovec_index_of_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_oset_insert_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_oset_remove_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_oset_contains_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_omap_insert_i64" => (&[ptr_ty, types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_omap_remove_i64" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_omap_get_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_omap_contains_key_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_omap_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_url_query_escape" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_url_path_escape" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_url_query_unescape" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_url_path_unescape" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_exists" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_is_file" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_is_dir" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_is_symlink" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_file_size" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_remove_file" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_map_bare" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_result_map_err_bare" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_os_write_file_result" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_os_write_file_bytes_result" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_fs_read_bytes_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_mkdir_all_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_remove_file_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_remove_dir_all_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_stream" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_head" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_options" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_post" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_put" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_delete" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_bytes" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_stream_next_line" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_stream_next_chunk" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_bufio_scanner_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bufio_scanner_scan" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_bufio_scanner_text" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_new" => (&[], Some(ptr_ty)),
        "gos_rt_http_client_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_post" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_put" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_options" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_delete" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_head" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_builder_new" => (&[], Some(ptr_ty)),
        "gos_rt_http_client_builder_max_redirects" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_http_client_builder_timeout_ms" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_http_client_builder_cookie_jar" => (&[ptr_ty, types::I32], Some(ptr_ty)),
        "gos_rt_http_client_builder_proxy" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_builder_build" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_request" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_request_bytes" => {
            (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty))
        }
        "gos_rt_http_request_header" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_body" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_send" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_status" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_http_response_body" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_raw_bytes" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_headers" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_content_type" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_location" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_get_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_vec_set_i64" => (&[ptr_ty, types::I64, types::I64], None),
        "gos_rt_vec_format_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_chan_recv_option" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_chan_try_recv_option" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_result_new" => (&[types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_result_disc" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_payload" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_unwrap" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_unwrap_or" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_result_ok" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_err" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_ok_or" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_result_is_ok" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_is_err" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_select_new" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_select_arm_recv" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_select_arm_send" => (&[ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_select_arm_default" => (&[ptr_ty], None),
        "gos_rt_select_wait" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_select_value" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_select_free" => (&[ptr_ty], None),
        "gos_rt_set_new" => (&[], Some(ptr_ty)),
        "gos_rt_set_insert" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_set_contains" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_set_remove" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_set_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_set_union"
        | "gos_rt_set_intersection"
        | "gos_rt_set_difference"
        | "gos_rt_set_symmetric_difference" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_set_is_subset" | "gos_rt_set_is_superset" | "gos_rt_set_is_disjoint" => {
            (&[ptr_ty, ptr_ty], Some(types::I64))
        }
        "gos_rt_btmap_new" => (&[], Some(ptr_ty)),
        "gos_rt_btmap_insert" => (&[ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_btmap_get_or" => (&[ptr_ty, ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_btmap_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_btmap_keys" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_str_as_bytes" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_clone" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_map_inc_str_i64" => (&[ptr_ty, ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_map_or_insert_str_i64" => (&[ptr_ty, ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_map_or_insert_i64_i64" => (&[ptr_ty, types::I64, types::I64], Some(types::I64)),
        "gos_rt_errors_join_vec" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_errors_join" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_json_value_object_n" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_json_value_float" => (&[types::F64], Some(ptr_ty)),
        "gos_rt_http_response_set_header" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_http_response_set_content_type" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_http_response_set_body_bytes" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_http_response_with_header" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_get_header" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_set_header" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_http_request_get_header" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_path" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_method" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_query" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_headers" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_body_str" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_raw_body" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_text_new" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_json_new" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_stream_new" => (&[types::I64, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_gzip_encode" | "gos_rt_gzip_decode" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_sha256_hex" | "gos_rt_sha512_hex" | "gos_rt_blake3_hex" => {
            (&[ptr_ty], Some(ptr_ty))
        }
        "gos_rt_hmac_sha256_hex" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_chunked_encode" | "gos_rt_chunked_decode" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_sse_encode_event" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_sse_encode_comment" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_sse_encode_retry" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_mw_new_request_id" => (&[], Some(ptr_ty)),
        "gos_rt_mw_accepts_gzip" => (&[ptr_ty], Some(types::I32)),
        "gos_rt_mw_decode_basic_auth" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_ws_accept" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_ws_is_upgrade" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_ws_accept_key" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_static_mime_for_path" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_static_serve_file" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_router_new" => (&[], Some(ptr_ty)),
        "gos_rt_router_add" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_add_pattern" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_router_lookup" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_router_get"
        | "gos_rt_router_post"
        | "gos_rt_router_put"
        | "gos_rt_router_delete"
        | "gos_rt_router_patch"
        | "gos_rt_router_head"
        | "gos_rt_router_options" => (&[ptr_ty, ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_add_fn" => (&[ptr_ty, ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_get_fn"
        | "gos_rt_router_post_fn"
        | "gos_rt_router_put_fn"
        | "gos_rt_router_delete_fn"
        | "gos_rt_router_patch_fn"
        | "gos_rt_router_head_fn"
        | "gos_rt_router_options_fn" => (&[ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_serve" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_file_server_new" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_file_server_serve" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_native_client_new" => (&[], Some(ptr_ty)),
        "gos_rt_native_client_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_nc_get" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_nc_delete" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_nc_post" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_nc_put" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_proxy_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_proxy_forward" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_proxy_forward_url" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_ws_frame_text" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_slog_info" | "gos_rt_slog_warn" | "gos_rt_slog_error" | "gos_rt_slog_debug" => {
            (&[ptr_ty], None)
        }
        "gos_rt_testing_check" => (&[types::I8, ptr_ty], Some(types::I8)),
        "gos_rt_testing_check_eq_i64" => (&[types::I64, types::I64, ptr_ty], Some(types::I8)),
        "gos_rt_parse_i64_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_count_by_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_filter_map_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_find_map_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_flat_map_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_flat_map_arr_i64" => (&[ptr_ty, ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_iter_group_by_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_max_by_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_max_by_key_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_min_by_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_min_by_key_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_partition_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_chunk_by_size_i64" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_dedup_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_enumerate_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_flatten_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_pairwise_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_unzip_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_windowed_i64" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_zip_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_position_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_product_by_i64" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_iter_reduce_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_scan_i64" => (&[types::I64, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_skip_while_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_sorted_by_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_sorted_by_key_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_iter_take_while_i64" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_option_and_then" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_option_default_with" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_option_filter" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_option_flatten" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_option_iter" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_option_or" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_option_or_else" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_option_zip" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_result_and_then" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_result_or_else" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_result_to_opt_err" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_result_to_opt_ok" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_result_map_err" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_result_map" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_cell_load_str" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_cell_load_i64" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_flag_cell_load_bool" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_flag_cell_load_f64" => (&[ptr_ty], Some(types::F64)),
        "gos_rt_flag_cell_load_vec" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_sort_i64" => (&[ptr_ty], None),
        "gos_rt_arr_sort_by_i64" => (&[ptr_ty, types::I64, ptr_ty], None),
        "gos_rt_vec_sort_by_i64" => (&[ptr_ty, ptr_ty], None),
        // Aggregate-stride variants. Vec form reads `elem_bytes`
        // from the GosVec header so it has no extra arg; array
        // form takes `(buf, len, elem_bytes, env)`.
        "gos_rt_arr_sort_by_aggr" => (&[ptr_ty, types::I64, types::I64, ptr_ty], None),
        "gos_rt_vec_sort_by_aggr" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_json_set" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_arr_iter" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_arr_iter_next" => (&[ptr_ty], Some(ptr_ty)),
        _ => unreachable!("unhandled rt name {name}"),
    };
    let returns = ret.map(|t| vec![t]).unwrap_or_default();
    let func_id = intrinsics.extern_fn(module, name, params, &returns)?;
    let fref = module.declare_func_in_func(func_id, builder.func);
    let mut arg_values = Vec::with_capacity(params.len());
    for (i, param_ty) in params.iter().enumerate() {
        let v = match args.get(i) {
            Some(a) => {
                let hint = if *param_ty == ptr_ty {
                    Some(ptr_ty)
                } else {
                    None
                };
                lower_operand(module, builder, locals, body, tcx, a, hint, intrinsics)?
            }
            None => {
                if param_ty.is_int() {
                    builder.ins().iconst(*param_ty, 0)
                } else {
                    builder.ins().iconst(ptr_ty, 0)
                }
            }
        };
        let coerced = coerce_arg_to(builder, v, *param_ty)?;
        arg_values.push(coerced);
    }
    let call = builder.ins().call(fref, &arg_values);
    if let Some(_ret_ty) = ret {
        let v = builder.inst_results(call)[0];
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            v,
        );
    } else {
        let zero = builder.ins().iconst(types::I64, 0);
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            zero,
        );
    }
    Ok(())
}

pub(super) fn lower_external_binding_call(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    intrinsics: &mut IntrinsicContext,
    destination: &gossamer_mir::Place,
    name: &str,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();

    let dest_ty = body.local_ty(destination.local);
    let dest_cl_ty = mir_ty_to_cabi(tcx, dest_ty, ptr_ty);

    let mut params: Vec<ir::Type> = Vec::with_capacity(args.len());
    for arg in args {
        let ty = operand_cabi_ty(arg, body, tcx, ptr_ty);
        params.push(ty);
    }

    let returns: Vec<ir::Type> = match dest_cl_ty {
        Some(t) => vec![t],
        None => Vec::new(),
    };
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let extern_fn = intrinsics.extern_fn(module, static_name, &params, &returns)?;
    let fref = module.declare_func_in_func(extern_fn, builder.func);

    let mut arg_values: Vec<ir::Value> = Vec::with_capacity(args.len());
    for (arg, &param_ty) in args.iter().zip(params.iter()) {
        let v = lower_operand(
            module,
            builder,
            locals,
            body,
            tcx,
            arg,
            Some(param_ty),
            intrinsics,
        )?;
        let coerced = coerce_arg_to(builder, v, param_ty)?;
        arg_values.push(coerced);
    }

    let call = builder.ins().call(fref, &arg_values);
    if dest_cl_ty.is_some() {
        let v = builder.inst_results(call)[0];
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            v,
        );
    } else {
        let zero = builder.ins().iconst(types::I64, 0);
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            zero,
        );
    }
    Ok(())
}
