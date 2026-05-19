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
//! anything that needs a GC heap are not yet lowered — those
//! constructs fall back to [`super::emit::emit_module`] for
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

use super::*;

pub(super) fn name_to_static(name: &str, set: &[&'static str]) -> Option<&'static str> {
    for s in set {
        if *s == name {
            return Some(*s);
        }
    }
    None
}

pub(super) fn generic_rt_static_name(name: &str) -> Option<&'static str> {
    if let Some(s) = name_to_static(
        name,
        &[
            "gos_rt_http_request_path",
            "gos_rt_http_request_method",
            "gos_rt_http_request_query",
            "gos_rt_http_request_body_str",
            "gos_rt_http_response_text_new",
            "gos_rt_http_response_json_new",
        ],
    ) {
        return Some(s);
    }
    match name {
        // 0.7.0 string surface.
        "gos_rt_str_split_once" => Some("gos_rt_str_split_once"),
        "gos_rt_str_rsplit_once" => Some("gos_rt_str_rsplit_once"),
        "gos_rt_str_count" => Some("gos_rt_str_count"),
        "gos_rt_str_strip_chars" => Some("gos_rt_str_strip_chars"),
        "gos_rt_str_lstrip_chars" => Some("gos_rt_str_lstrip_chars"),
        "gos_rt_str_rstrip_chars" => Some("gos_rt_str_rstrip_chars"),
        "gos_rt_str_zfill" => Some("gos_rt_str_zfill"),
        "gos_rt_str_center" => Some("gos_rt_str_center"),
        "gos_rt_str_slice" => Some("gos_rt_str_slice"),
        "gos_rt_str_rfind_opt" => Some("gos_rt_str_rfind_opt"),
        "gos_rt_strings_join" => Some("gos_rt_strings_join"),
        "gos_rt_uuid_v4" => Some("gos_rt_uuid_v4"),
        "gos_rt_uuid_v7" => Some("gos_rt_uuid_v7"),
        "gos_rt_uuid_is_valid" => Some("gos_rt_uuid_is_valid"),
        "gos_rt_uuid_normalize" => Some("gos_rt_uuid_normalize"),
        "gos_rt_uuid_simple" => Some("gos_rt_uuid_simple"),
        "gos_rt_path_base" => Some("gos_rt_path_base"),
        "gos_rt_path_dir" => Some("gos_rt_path_dir"),
        "gos_rt_path_ext" => Some("gos_rt_path_ext"),
        "gos_rt_vec_first" => Some("gos_rt_vec_first"),
        "gos_rt_vec_last" => Some("gos_rt_vec_last"),
        "gos_rt_vec_reversed" => Some("gos_rt_vec_reversed"),
        "gos_rt_vec_index_of_i64" => Some("gos_rt_vec_index_of_i64"),
        "gos_rt_vec_index_of_str" => Some("gos_rt_vec_index_of_str"),
        "gos_rt_vec_count_of_i64" => Some("gos_rt_vec_count_of_i64"),
        "gos_rt_vec_count_of_str" => Some("gos_rt_vec_count_of_str"),
        "gos_rt_vec_contains_i64" => Some("gos_rt_vec_contains_i64"),
        "gos_rt_vec_contains_str" => Some("gos_rt_vec_contains_str"),
        "gos_rt_vec_slice_result" => Some("gos_rt_vec_slice_result"),
        "gos_rt_vec_insert_safe" => Some("gos_rt_vec_insert_safe"),
        "gos_rt_vec_remove_safe" => Some("gos_rt_vec_remove_safe"),
        "gos_rt_map_keys_vec" => Some("gos_rt_map_keys_vec"),
        "gos_rt_map_values_vec" => Some("gos_rt_map_values_vec"),
        "gos_rt_map_pop_i64" => Some("gos_rt_map_pop_i64"),
        "gos_rt_map_pop_str" => Some("gos_rt_map_pop_str"),
        "gos_rt_min_i64" => Some("gos_rt_min_i64"),
        "gos_rt_max_i64" => Some("gos_rt_max_i64"),
        "gos_rt_clamp_i64" => Some("gos_rt_clamp_i64"),
        "gos_rt_min_f64" => Some("gos_rt_min_f64"),
        "gos_rt_max_f64" => Some("gos_rt_max_f64"),
        "gos_rt_clamp_f64" => Some("gos_rt_clamp_f64"),
        "gos_rt_error_new" => Some("gos_rt_error_new"),
        "gos_rt_error_from" => Some("gos_rt_error_from"),
        "gos_rt_error_wrap" => Some("gos_rt_error_wrap"),
        "gos_rt_error_message" => Some("gos_rt_error_message"),
        "gos_rt_error_cause" => Some("gos_rt_error_cause"),
        "gos_rt_error_is" => Some("gos_rt_error_is"),
        "gos_rt_regex_compile" => Some("gos_rt_regex_compile"),
        "gos_rt_regex_is_match" => Some("gos_rt_regex_is_match"),
        "gos_rt_regex_find" => Some("gos_rt_regex_find"),
        "gos_rt_regex_find_opt" => Some("gos_rt_regex_find_opt"),
        "gos_rt_regex_captures" => Some("gos_rt_regex_captures"),
        "gos_rt_regex_find_all" => Some("gos_rt_regex_find_all"),
        "gos_rt_regex_captures_all" => Some("gos_rt_regex_captures_all"),
        "gos_rt_regex_replace" => Some("gos_rt_regex_replace"),
        "gos_rt_regex_replace_all" => Some("gos_rt_regex_replace_all"),
        "gos_rt_regex_split" => Some("gos_rt_regex_split"),
        "gos_rt_fs_read_to_string" => Some("gos_rt_fs_read_to_string"),
        "gos_rt_fs_write" => Some("gos_rt_fs_write"),
        "gos_rt_fs_create_dir_all" => Some("gos_rt_fs_create_dir_all"),
        "gos_rt_path_join" => Some("gos_rt_path_join"),
        "gos_rt_flag_set_new" => Some("gos_rt_flag_set_new"),
        "gos_rt_flag_set_string" => Some("gos_rt_flag_set_string"),
        "gos_rt_flag_set_int" => Some("gos_rt_flag_set_int"),
        "gos_rt_flag_set_uint" => Some("gos_rt_flag_set_uint"),
        "gos_rt_flag_set_float" => Some("gos_rt_flag_set_float"),
        "gos_rt_flag_set_bool" => Some("gos_rt_flag_set_bool"),
        "gos_rt_flag_set_duration" => Some("gos_rt_flag_set_duration"),
        "gos_rt_flag_set_string_list" => Some("gos_rt_flag_set_string_list"),
        "gos_rt_flag_set_short" => Some("gos_rt_flag_set_short"),
        "gos_rt_flag_set_usage" => Some("gos_rt_flag_set_usage"),
        "gos_rt_flag_set_parse" => Some("gos_rt_flag_set_parse"),
        "gos_rt_duration_from_secs" => Some("gos_rt_duration_from_secs"),
        "gos_rt_duration_from_millis" => Some("gos_rt_duration_from_millis"),
        "gos_rt_time_format_rfc3339" => Some("gos_rt_time_format_rfc3339"),
        "gos_rt_time_parse_rfc3339" => Some("gos_rt_time_parse_rfc3339"),
        "gos_rt_flag_parse" => Some("gos_rt_flag_parse"),
        "gos_rt_flag_map_get" => Some("gos_rt_flag_map_get"),
        "gos_rt_os_env" => Some("gos_rt_os_env"),
        "gos_rt_os_program_name" => Some("gos_rt_os_program_name"),
        "gos_rt_env_temp_dir" => Some("gos_rt_env_temp_dir"),
        "gos_rt_env_home_dir" => Some("gos_rt_env_home_dir"),
        "gos_rt_os_cwd" => Some("gos_rt_os_cwd"),
        "gos_rt_os_exists" => Some("gos_rt_os_exists"),
        "gos_rt_os_is_file" => Some("gos_rt_os_is_file"),
        "gos_rt_os_is_dir" => Some("gos_rt_os_is_dir"),
        "gos_rt_os_is_symlink" => Some("gos_rt_os_is_symlink"),
        "gos_rt_os_file_size" => Some("gos_rt_os_file_size"),
        "gos_rt_os_remove_file" => Some("gos_rt_os_remove_file"),
        "gos_rt_result_map_bare" => Some("gos_rt_result_map_bare"),
        "gos_rt_result_map_err_bare" => Some("gos_rt_result_map_err_bare"),
        "gos_rt_os_write_file_result" => Some("gos_rt_os_write_file_result"),
        "gos_rt_os_write_file_bytes_result" => Some("gos_rt_os_write_file_bytes_result"),
        "gos_rt_fs_read_bytes_result" => Some("gos_rt_fs_read_bytes_result"),
        "gos_rt_os_mkdir_all_result" => Some("gos_rt_os_mkdir_all_result"),
        "gos_rt_os_remove_file_result" => Some("gos_rt_os_remove_file_result"),
        "gos_rt_os_remove_dir_all_result" => Some("gos_rt_os_remove_dir_all_result"),
        "gos_rt_http_stream" => Some("gos_rt_http_stream"),
        "gos_rt_http_get" => Some("gos_rt_http_get"),
        "gos_rt_http_stream_next_line" => Some("gos_rt_http_stream_next_line"),
        "gos_rt_fs_list_dir" => Some("gos_rt_fs_list_dir"),
        "gos_rt_fs_walk_dir" => Some("gos_rt_fs_walk_dir"),
        "gos_rt_exec_run" => Some("gos_rt_exec_run"),
        "gos_rt_exec_spawn" => Some("gos_rt_exec_spawn"),
        "gos_rt_exec_kill" => Some("gos_rt_exec_kill"),
        "gos_rt_signal_on" => Some("gos_rt_signal_on"),
        "gos_rt_signal_wait" => Some("gos_rt_signal_wait"),
        "gos_rt_signal_try_wait" => Some("gos_rt_signal_try_wait"),
        "gos_rt_os_set_env" => Some("gos_rt_os_set_env"),
        "gos_rt_os_unset_env" => Some("gos_rt_os_unset_env"),
        "gos_rt_os_user_current_name" => Some("gos_rt_os_user_current_name"),
        "gos_rt_os_user_current_uid" => Some("gos_rt_os_user_current_uid"),
        "gos_rt_os_user_current_gid" => Some("gos_rt_os_user_current_gid"),
        "gos_rt_os_user_current_home" => Some("gos_rt_os_user_current_home"),
        "gos_rt_os_user_lookup_uid" => Some("gos_rt_os_user_lookup_uid"),
        "gos_rt_os_user_lookup_name" => Some("gos_rt_os_user_lookup_name"),
        "gos_rt_netip_is_valid" => Some("gos_rt_netip_is_valid"),
        "gos_rt_netip_is_v4" => Some("gos_rt_netip_is_v4"),
        "gos_rt_netip_is_v6" => Some("gos_rt_netip_is_v6"),
        "gos_rt_netip_is_loopback" => Some("gos_rt_netip_is_loopback"),
        "gos_rt_netip_is_unspecified" => Some("gos_rt_netip_is_unspecified"),
        "gos_rt_netip_is_multicast" => Some("gos_rt_netip_is_multicast"),
        "gos_rt_netip_is_private" => Some("gos_rt_netip_is_private"),
        "gos_rt_netip_normalize" => Some("gos_rt_netip_normalize"),
        "gos_rt_netip_host_of" => Some("gos_rt_netip_host_of"),
        "gos_rt_netip_port_of" => Some("gos_rt_netip_port_of"),
        "gos_rt_netip_join_addr_port" => Some("gos_rt_netip_join_addr_port"),
        "gos_rt_mime_parse" => Some("gos_rt_mime_parse"),
        "gos_rt_mime_top" => Some("gos_rt_mime_top"),
        "gos_rt_mime_sub" => Some("gos_rt_mime_sub"),
        "gos_rt_mime_charset" => Some("gos_rt_mime_charset"),
        "gos_rt_mime_boundary" => Some("gos_rt_mime_boundary"),
        "gos_rt_mime_param" => Some("gos_rt_mime_param"),
        "gos_rt_mime_type_by_extension" => Some("gos_rt_mime_type_by_extension"),
        "gos_rt_mime_extension_by_type" => Some("gos_rt_mime_extension_by_type"),
        "gos_rt_mime_is_valid" => Some("gos_rt_mime_is_valid"),
        "gos_rt_toml_to_json" => Some("gos_rt_toml_to_json"),
        "gos_rt_toml_from_json" => Some("gos_rt_toml_from_json"),
        "gos_rt_toml_is_valid" => Some("gos_rt_toml_is_valid"),
        "gos_rt_toml_pretty" => Some("gos_rt_toml_pretty"),
        "gos_rt_yaml_to_json" => Some("gos_rt_yaml_to_json"),
        "gos_rt_yaml_from_json" => Some("gos_rt_yaml_from_json"),
        "gos_rt_yaml_is_valid" => Some("gos_rt_yaml_is_valid"),
        "gos_rt_sync_map_new" => Some("gos_rt_sync_map_new"),
        "gos_rt_sync_map_set" => Some("gos_rt_sync_map_set"),
        "gos_rt_sync_map_get" => Some("gos_rt_sync_map_get"),
        "gos_rt_sync_map_delete" => Some("gos_rt_sync_map_delete"),
        "gos_rt_sync_map_len" => Some("gos_rt_sync_map_len"),
        "gos_rt_sync_map_contains" => Some("gos_rt_sync_map_contains"),
        "gos_rt_sync_map_keys" => Some("gos_rt_sync_map_keys"),
        "gos_rt_bheap_push_i64" => Some("gos_rt_bheap_push_i64"),
        "gos_rt_bheap_pop_i64" => Some("gos_rt_bheap_pop_i64"),
        "gos_rt_bheap_peek_i64" => Some("gos_rt_bheap_peek_i64"),
        "gos_rt_bheap_len" => Some("gos_rt_bheap_len"),
        "gos_rt_vec_first_i64" => Some("gos_rt_vec_first_i64"),
        "gos_rt_vec_last_i64" => Some("gos_rt_vec_last_i64"),
        "gos_rt_vec_pop_front_i64" => Some("gos_rt_vec_pop_front_i64"),
        "gos_rt_vec_pop_back_i64" => Some("gos_rt_vec_pop_back_i64"),
        "gos_rt_vec_push_front_i64" => Some("gos_rt_vec_push_front_i64"),
        "gos_rt_vec_push_back_i64" => Some("gos_rt_vec_push_back_i64"),
        "gos_rt_ovec_insert_i64" => Some("gos_rt_ovec_insert_i64"),
        "gos_rt_ovec_remove_at_i64" => Some("gos_rt_ovec_remove_at_i64"),
        "gos_rt_ovec_contains_i64" => Some("gos_rt_ovec_contains_i64"),
        "gos_rt_ovec_index_of_i64" => Some("gos_rt_ovec_index_of_i64"),
        "gos_rt_oset_insert_i64" => Some("gos_rt_oset_insert_i64"),
        "gos_rt_oset_remove_i64" => Some("gos_rt_oset_remove_i64"),
        "gos_rt_oset_contains_i64" => Some("gos_rt_oset_contains_i64"),
        "gos_rt_omap_insert_i64" => Some("gos_rt_omap_insert_i64"),
        "gos_rt_omap_remove_i64" => Some("gos_rt_omap_remove_i64"),
        "gos_rt_omap_get_i64" => Some("gos_rt_omap_get_i64"),
        "gos_rt_omap_contains_key_i64" => Some("gos_rt_omap_contains_key_i64"),
        "gos_rt_omap_len" => Some("gos_rt_omap_len"),
        "gos_rt_url_query_escape" => Some("gos_rt_url_query_escape"),
        "gos_rt_url_path_escape" => Some("gos_rt_url_path_escape"),
        "gos_rt_url_query_unescape" => Some("gos_rt_url_query_unescape"),
        "gos_rt_url_path_unescape" => Some("gos_rt_url_path_unescape"),
        "gos_rt_bufio_scanner_new" => Some("gos_rt_bufio_scanner_new"),
        "gos_rt_bufio_scanner_scan" => Some("gos_rt_bufio_scanner_scan"),
        "gos_rt_bufio_scanner_text" => Some("gos_rt_bufio_scanner_text"),
        "gos_rt_http_client_new" => Some("gos_rt_http_client_new"),
        "gos_rt_http_client_get" => Some("gos_rt_http_client_get"),
        "gos_rt_http_client_post" => Some("gos_rt_http_client_post"),
        "gos_rt_http_request_header" => Some("gos_rt_http_request_header"),
        "gos_rt_http_request_body" => Some("gos_rt_http_request_body"),
        "gos_rt_http_request_send" => Some("gos_rt_http_request_send"),
        "gos_rt_http_response_status" => Some("gos_rt_http_response_status"),
        "gos_rt_http_response_body" => Some("gos_rt_http_response_body"),
        "gos_rt_http_response_raw_bytes" => Some("gos_rt_http_response_raw_bytes"),
        "gos_rt_vec_get_i64" => Some("gos_rt_vec_get_i64"),
        "gos_rt_vec_set_i64" => Some("gos_rt_vec_set_i64"),
        "gos_rt_vec_format_i64" => Some("gos_rt_vec_format_i64"),
        "gos_rt_chan_recv_option" => Some("gos_rt_chan_recv_option"),
        "gos_rt_chan_try_recv_option" => Some("gos_rt_chan_try_recv_option"),
        "gos_rt_result_new" => Some("gos_rt_result_new"),
        "gos_rt_result_disc" => Some("gos_rt_result_disc"),
        "gos_rt_result_payload" => Some("gos_rt_result_payload"),
        "gos_rt_result_unwrap" => Some("gos_rt_result_unwrap"),
        "gos_rt_result_unwrap_or" => Some("gos_rt_result_unwrap_or"),
        "gos_rt_result_ok" => Some("gos_rt_result_ok"),
        "gos_rt_result_err" => Some("gos_rt_result_err"),
        "gos_rt_result_ok_or" => Some("gos_rt_result_ok_or"),
        "gos_rt_result_is_ok" => Some("gos_rt_result_is_ok"),
        "gos_rt_result_is_err" => Some("gos_rt_result_is_err"),
        "gos_rt_set_new" => Some("gos_rt_set_new"),
        "gos_rt_set_insert" => Some("gos_rt_set_insert"),
        "gos_rt_set_contains" => Some("gos_rt_set_contains"),
        "gos_rt_set_remove" => Some("gos_rt_set_remove"),
        "gos_rt_set_len" => Some("gos_rt_set_len"),
        "gos_rt_btmap_new" => Some("gos_rt_btmap_new"),
        "gos_rt_btmap_insert" => Some("gos_rt_btmap_insert"),
        "gos_rt_btmap_get_or" => Some("gos_rt_btmap_get_or"),
        "gos_rt_btmap_len" => Some("gos_rt_btmap_len"),
        "gos_rt_btmap_keys" => Some("gos_rt_btmap_keys"),
        "gos_rt_str_as_bytes" => Some("gos_rt_str_as_bytes"),
        "gos_rt_vec_clone" => Some("gos_rt_vec_clone"),
        "gos_rt_map_inc_str_i64" => Some("gos_rt_map_inc_str_i64"),
        "gos_rt_map_or_insert_str_i64" => Some("gos_rt_map_or_insert_str_i64"),
        "gos_rt_map_or_insert_i64_i64" => Some("gos_rt_map_or_insert_i64_i64"),
        "gos_rt_errors_join_vec" => Some("gos_rt_errors_join_vec"),
        "gos_rt_errors_join" => Some("gos_rt_errors_join"),
        "gos_rt_json_value_object_n" => Some("gos_rt_json_value_object_n"),
        "gos_rt_http_response_set_header" => Some("gos_rt_http_response_set_header"),
        "gos_rt_http_response_get_header" => Some("gos_rt_http_response_get_header"),
        "gos_rt_http_request_set_header" => Some("gos_rt_http_request_set_header"),
        "gos_rt_http_request_get_header" => Some("gos_rt_http_request_get_header"),
        "gos_rt_gzip_encode" => Some("gos_rt_gzip_encode"),
        "gos_rt_gzip_decode" => Some("gos_rt_gzip_decode"),
        "gos_rt_sha256_hex" => Some("gos_rt_sha256_hex"),
        "gos_rt_sha512_hex" => Some("gos_rt_sha512_hex"),
        "gos_rt_blake3_hex" => Some("gos_rt_blake3_hex"),
        "gos_rt_hmac_sha256_hex" => Some("gos_rt_hmac_sha256_hex"),
        "gos_rt_chunked_encode" => Some("gos_rt_chunked_encode"),
        "gos_rt_chunked_decode" => Some("gos_rt_chunked_decode"),
        "gos_rt_sse_encode_event" => Some("gos_rt_sse_encode_event"),
        "gos_rt_sse_encode_comment" => Some("gos_rt_sse_encode_comment"),
        "gos_rt_sse_encode_retry" => Some("gos_rt_sse_encode_retry"),
        "gos_rt_mw_new_request_id" => Some("gos_rt_mw_new_request_id"),
        "gos_rt_mw_accepts_gzip" => Some("gos_rt_mw_accepts_gzip"),
        "gos_rt_ws_accept_key" => Some("gos_rt_ws_accept_key"),
        "gos_rt_static_mime_for_path" => Some("gos_rt_static_mime_for_path"),
        "gos_rt_router_new" => Some("gos_rt_router_new"),
        "gos_rt_router_add" => Some("gos_rt_router_add"),
        "gos_rt_router_get" => Some("gos_rt_router_get"),
        "gos_rt_router_post" => Some("gos_rt_router_post"),
        "gos_rt_router_put" => Some("gos_rt_router_put"),
        "gos_rt_router_delete" => Some("gos_rt_router_delete"),
        "gos_rt_router_patch" => Some("gos_rt_router_patch"),
        "gos_rt_router_head" => Some("gos_rt_router_head"),
        "gos_rt_router_options" => Some("gos_rt_router_options"),
        "gos_rt_router_add_fn" => Some("gos_rt_router_add_fn"),
        "gos_rt_router_get_fn" => Some("gos_rt_router_get_fn"),
        "gos_rt_router_post_fn" => Some("gos_rt_router_post_fn"),
        "gos_rt_router_put_fn" => Some("gos_rt_router_put_fn"),
        "gos_rt_router_delete_fn" => Some("gos_rt_router_delete_fn"),
        "gos_rt_router_patch_fn" => Some("gos_rt_router_patch_fn"),
        "gos_rt_router_head_fn" => Some("gos_rt_router_head_fn"),
        "gos_rt_router_options_fn" => Some("gos_rt_router_options_fn"),
        "gos_rt_router_serve" => Some("gos_rt_router_serve"),
        "gos_rt_file_server_new" => Some("gos_rt_file_server_new"),
        "gos_rt_file_server_serve" => Some("gos_rt_file_server_serve"),
        "gos_rt_native_client_new" => Some("gos_rt_native_client_new"),
        "gos_rt_native_client_get" => Some("gos_rt_native_client_get"),
        "gos_rt_proxy_new" => Some("gos_rt_proxy_new"),
        "gos_rt_proxy_forward" => Some("gos_rt_proxy_forward"),
        "gos_rt_ws_frame_text" => Some("gos_rt_ws_frame_text"),
        "gos_rt_slog_info" => Some("gos_rt_slog_info"),
        "gos_rt_slog_warn" => Some("gos_rt_slog_warn"),
        "gos_rt_slog_error" => Some("gos_rt_slog_error"),
        "gos_rt_slog_debug" => Some("gos_rt_slog_debug"),
        "gos_rt_testing_check" => Some("gos_rt_testing_check"),
        "gos_rt_testing_check_eq_i64" => Some("gos_rt_testing_check_eq_i64"),
        "gos_rt_parse_i64_result" => Some("gos_rt_parse_i64_result"),
        "gos_rt_result_map_err" => Some("gos_rt_result_map_err"),
        "gos_rt_result_map" => Some("gos_rt_result_map"),
        "gos_rt_flag_cell_load_str" => Some("gos_rt_flag_cell_load_str"),
        "gos_rt_flag_cell_load_i64" => Some("gos_rt_flag_cell_load_i64"),
        "gos_rt_flag_cell_load_bool" => Some("gos_rt_flag_cell_load_bool"),
        "gos_rt_flag_cell_load_f64" => Some("gos_rt_flag_cell_load_f64"),
        "gos_rt_flag_cell_load_vec" => Some("gos_rt_flag_cell_load_vec"),
        // Plain ascending sort for Vec<i64>.
        "gos_rt_vec_sort_i64" => Some("gos_rt_vec_sort_i64"),
        // Sort-by callbacks for fixed-array / Vec receivers.
        "gos_rt_arr_sort_by_i64" => Some("gos_rt_arr_sort_by_i64"),
        "gos_rt_vec_sort_by_i64" => Some("gos_rt_vec_sort_by_i64"),
        // Stride-aware sort_by for multi-slot aggregate elements
        // (Tuple / struct). The comparator receives element
        // pointers; the cranelift ABI passes aggregates that way
        // already so the user closure body works unchanged.
        "gos_rt_arr_sort_by_aggr" => Some("gos_rt_arr_sort_by_aggr"),
        "gos_rt_vec_sort_by_aggr" => Some("gos_rt_vec_sort_by_aggr"),
        "gos_rt_json_set" => Some("gos_rt_json_set"),
        "gos_rt_arr_iter" => Some("gos_rt_arr_iter"),
        "gos_rt_arr_iter_next" => Some("gos_rt_arr_iter_next"),
        _ => None,
    }
}
