//! Lifecycle status registry - declares whether each documented
//! stdlib module and language feature is `Stable`, `Shipped`,
//! `Experimental`, `Planned`, or `Removed`.
//!
//! Single source of truth for the `gos feature-status` subcommand
//! and the experimental markers emitted into the per-module docs
//! pages. `Experimental` is the default for manifest modules;
//! `Shipped` must be explicit and `Stable` additionally requires
//! all-tier contract evidence.
//!
//! Drift between this table and the rendered doc pages is gated
//! by `gos doc --emit-stdlib --check`.

#![forbid(unsafe_code)]

/// Lifecycle stage of a stdlib module or documented language feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// No fixture exercises this surface, so nothing is known about it
    /// beyond that it exists. Never authored: it is what an item's
    /// authored status is reduced to when no evidence supports it.
    /// Distinct from `Experimental`, which is a judgment someone made.
    Unproven,
    /// Compatibility-protected surface implemented across every
    /// supported tier. Doc page + cross-tier contract test required.
    Stable,
    /// Included in release artifacts and documented. Shipped means
    /// available, not yet protected by the Stable compatibility policy.
    Shipped,
    /// Surface is wired but has known gaps (partial implementation,
    /// platform-specific, or pending audit). Doc page required;
    /// tier-parity coverage optional.
    Experimental,
    /// Documented in the registry so consumers can see what's on
    /// the roadmap. No doc page or test required yet.
    Planned,
    /// Previously shipped, since withdrawn. Kept in the registry so
    /// tooling can answer "where did `foo` go?" with a deliberate
    /// removal note.
    Removed,
    /// Permanently declined (SPEC §17.5). The surface is rejected with
    /// a diagnostic naming the alternative, and no implementation is
    /// coming - the entry exists so the absence reads as a decision.
    Declined,
}

/// Execution implementation covered by an item contract.
///
/// This deliberately lives beside lifecycle status rather than in the test
/// harness: the contract says what is supported, while a sidecar says what was
/// observed in one particular run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvidenceTier {
    /// Bytecode VM execution.
    Vm,
    /// Cranelift JIT execution.
    Cranelift,
    /// LLVM AOT execution.
    Llvm,
}

impl EvidenceTier {
    /// Stable machine-readable name used in JSON evidence output.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Vm => "vm",
            Self::Cranelift => "cranelift",
            Self::Llvm => "llvm",
        }
    }
}

/// Item-level audit metadata derived from a canonical registry identifier.
///
/// Paths in `positive_tests` and `negative_tests` are deliberately IDs, not
/// prose. A later test-ledger generator can therefore reject a reference to a
/// non-existent item without changing the public registry model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemEvidence {
    /// Lifecycle state duplicated into the evidence payload for consumers that
    /// only ingest the ledger JSON.
    pub status: Status,
    /// Tiers claimed by this item.
    pub supported_tiers: &'static [EvidenceTier],
    /// Targets claimed by this item. `host` is intentionally conservative for
    /// surfaces without cross-target execution evidence.
    pub supported_targets: &'static [&'static str],
    /// Generated canonical documentation location.
    pub doc_path: Option<String>,
    /// Positive test IDs or paths associated with this item.
    pub positive_tests: Vec<String>,
    /// Negative test IDs or paths associated with this item.
    pub negative_tests: Vec<String>,
    /// Explicit limitations, empty only for a fully specified contract.
    pub known_limits: Vec<String>,
}

const ALL_TIERS: &[EvidenceTier] = &[
    EvidenceTier::Vm,
    EvidenceTier::Cranelift,
    EvidenceTier::Llvm,
];
const HOST_TARGET: &[&str] = &["host"];

/// Host families a registered tier-parity fixture is executed on. The CI
/// matrix runs that suite on each of these, so a fixture named in
/// [`ITEM_FIXTURES`] carries evidence from all of them rather than from
/// whichever host happened to build it.
const MATRIX_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

/// Items an executable fixture exercises, and the fixture that does it.
///
/// This is the audited subset: an entry means the named program calls the
/// item and asserts its result, and that the program is registered in the
/// tier-parity suite, so it runs on every tier and every host in the
/// matrix. An item absent here is not claimed to be unaudited - it is
/// simply not yet covered by this ledger, so [`item_evidence`] reports
/// no tier for it and [`derived_status`] reports it as
/// [`Status::Unproven`].
pub const ITEM_FIXTURES: &[(&str, &[&str])] = &[
    // BEGIN generated by `cargo xtask item-fixtures`
    (
        "std::bufio",
        &[
            "examples/grep.gos",
            "feature-testing-examples/winb_sys_misc.gos",
        ],
    ),
    (
        "std::bytes",
        &[
            "feature-testing-examples/bytes_builder.gos",
            "feature-testing-examples/winb_sys_bytes.gos",
        ],
    ),
    (
        "std::collections",
        &[
            "examples/collection_patterns.gos",
            "examples/data_structures.gos",
            "examples/map_hashable_keys.gos",
            "feature-testing-examples/aggregate_binding.gos",
            "feature-testing-examples/auto_regions_map_iter.gos",
            "feature-testing-examples/bench_shape_graph_and_list.gos",
            "feature-testing-examples/btreemap_i64_keys.gos",
            "feature-testing-examples/collection_iter_contracts.gos",
            "feature-testing-examples/container_display.gos",
            "feature-testing-examples/early_break_materializers.gos",
            "feature-testing-examples/elemty_btreemap_shapes.gos",
            "feature-testing-examples/fmt_tuple_map.gos",
            "feature-testing-examples/goroutine_deque_handle.gos",
            "feature-testing-examples/goroutine_set_handle.gos",
            "feature-testing-examples/goroutine_shared_map.gos",
            "feature-testing-examples/hashmap_counter_race.gos",
            "feature-testing-examples/hashmap_field_through_result.gos",
            "feature-testing-examples/hashmap_get_some_field.gos",
            "feature-testing-examples/hashmap_ref_param_iter.gos",
            "feature-testing-examples/hashset_algebra.gos",
            "feature-testing-examples/hashset_struct_keys.gos",
            "feature-testing-examples/inferred_map_dispatch.gos",
            "feature-testing-examples/map_entry_and_format_paths.gos",
            "feature-testing-examples/map_inc_at_oob.gos",
            "feature-testing-examples/map_iter_destructure.gos",
            "feature-testing-examples/map_iter_wildcard_destructure.gos",
            "feature-testing-examples/map_loop_control_flow.gos",
            "feature-testing-examples/map_pop_then_drop.gos",
            "feature-testing-examples/map_struct_value_access.gos",
            "feature-testing-examples/map_tuple_key_tuple_value.gos",
            "feature-testing-examples/map_value_heap_children.gos",
            "feature-testing-examples/misc_class_a.gos",
            "feature-testing-examples/mut_param_is_a_copy.gos",
            "feature-testing-examples/opaque_nominal_alias.gos",
            "feature-testing-examples/p7_substring_inc.gos",
            "feature-testing-examples/serde_more_field_kinds.gos",
            "feature-testing-examples/set_from_sequence.gos",
            "feature-testing-examples/set_literal_rendering.gos",
            "feature-testing-examples/single_field_struct_aggregate.gos",
            "feature-testing-examples/stdlib_expansion.gos",
            "feature-testing-examples/str_substring_kmer.gos",
            "feature-testing-examples/struct_container_reclaim.gos",
            "feature-testing-examples/struct_keyed_map_value_iter.gos",
            "feature-testing-examples/struct_map_keys.gos",
            "feature-testing-examples/struct_tuple_map_key.gos",
            "feature-testing-examples/temporary_method_dispatch.gos",
            "feature-testing-examples/tuple_destructuring_loop.gos",
            "feature-testing-examples/vec_deque.gos",
            "feature-testing-examples/vecdeque_element_typing.gos",
            "feature-testing-examples/vecdeque_full.gos",
            "feature-testing-examples/winb2_btreemap_keys.gos",
            "feature-testing-examples/winb2_hashset_i64.gos",
            "feature-testing-examples/winb2_hashset_to_vec.gos",
            "feature-testing-examples/winb2_map_contains.gos",
            "feature-testing-examples/winb_coll_map.gos",
            "feature-testing-examples/winb_coll_set.gos",
            "feature-testing-examples/winb_coll_vec.gos",
        ],
    ),
    (
        "std::collections::deque",
        &["examples/containers_seq_demo.gos"],
    ),
    ("std::collections::heap", &["examples/heap_demo.gos"]),
    (
        "std::collections::ordered_map",
        &["examples/containers_setmap_demo.gos"],
    ),
    (
        "std::collections::ordered_set",
        &["examples/containers_setmap_demo.gos"],
    ),
    (
        "std::collections::ordered_vec",
        &["examples/containers_ordered_demo.gos"],
    ),
    (
        "std::collections::queue",
        &["examples/containers_seq_demo.gos"],
    ),
    (
        "std::collections::stack",
        &["examples/containers_seq_demo.gos"],
    ),
    (
        "std::context",
        &[
            "feature-testing-examples/context_aware_waits.gos",
            "feature-testing-examples/context_cancel.gos",
            "feature-testing-examples/context_lifecycle.gos",
            "feature-testing-examples/select_ctx_cancel.gos",
        ],
    ),
    (
        "std::crypto::blake3",
        &["feature-testing-examples/nul_in_strings.gos"],
    ),
    (
        "std::crypto::sha256",
        &["feature-testing-examples/nul_in_strings.gos"],
    ),
    (
        "std::database::sql",
        &[
            "feature-testing-examples/sql_driverless.gos",
            "feature-testing-examples/sql_ident_quoting.gos",
            "feature-testing-examples/sql_native_driver.gos",
        ],
    ),
    (
        "std::encoding::base64",
        &[
            "feature-testing-examples/callback_shorthands.gos",
            "feature-testing-examples/nul_in_strings.gos",
            "feature-testing-examples/stdlib_leaf_calls_and_json_queries.gos",
        ],
    ),
    (
        "std::encoding::binary",
        &["feature-testing-examples/binary_offset_accessors.gos"],
    ),
    (
        "std::encoding::csv",
        &["feature-testing-examples/stdlib_leaf_calls_and_json_queries.gos"],
    ),
    (
        "std::encoding::hex",
        &["feature-testing-examples/nul_in_strings.gos"],
    ),
    (
        "std::encoding::json",
        &[
            "examples/file_io.gos",
            "feature-testing-examples/json_encode_aggregates.gos",
            "feature-testing-examples/json_null_rendering.gos",
            "feature-testing-examples/json_round_trip_fuzz.gos",
            "feature-testing-examples/json_set_update.gos",
            "feature-testing-examples/module_scoped_type_names.gos",
            "feature-testing-examples/serde_more_field_kinds.gos",
            "feature-testing-examples/stdlib_compiled_wiring.gos",
            "feature-testing-examples/stdlib_json_as_bool.gos",
            "feature-testing-examples/stdlib_leaf_calls_and_json_queries.gos",
            "feature-testing-examples/winb2_json_int_precision.gos",
            "feature-testing-examples/winb_data_encoding.gos",
        ],
    ),
    (
        "std::encoding::toml",
        &[
            "examples/toml_demo.gos",
            "feature-testing-examples/winb_data_encoding.gos",
        ],
    ),
    (
        "std::encoding::yaml",
        &["feature-testing-examples/winb_data_encoding.gos"],
    ),
    (
        "std::env",
        &[
            "examples/cli_args.gos",
            "examples/environment.gos",
            "examples/file_io.gos",
            "examples/grep.gos",
            "examples/list_dir.gos",
            "feature-testing-examples/env_vars.gos",
            "feature-testing-examples/fs_dir_ops.gos",
            "feature-testing-examples/fs_metadata.gos",
            "feature-testing-examples/fs_permission_modes.gos",
            "feature-testing-examples/fs_temp_file_lifecycle.gos",
            "feature-testing-examples/nul_in_strings.gos",
            "feature-testing-examples/os_args_clone_roundtrip.gos",
            "feature-testing-examples/process_run_in.gos",
            "feature-testing-examples/sandbox_env_allow_all.gos",
            "feature-testing-examples/stdlib_alias_wiring.gos",
            "feature-testing-examples/stdlib_env_portable.gos",
            "feature-testing-examples/stdlib_fs_portable.gos",
            "feature-testing-examples/stdlib_fs_rename.gos",
            "feature-testing-examples/winb_sys_misc.gos",
            "feature-testing-examples/write_file_bytes.gos",
        ],
    ),
    (
        "std::errors",
        &[
            "examples/errors.gos",
            "examples/file_io.gos",
            "examples/grep.gos",
            "examples/json_derive_test.gos",
            "examples/json_structs.gos",
            "examples/list_dir.gos",
            "examples/structured_concurrency.gos",
            "examples/testing.gos",
            "examples/toml_demo.gos",
            "feature-testing-examples/autoderive_int_widths.gos",
            "feature-testing-examples/binary_offset_accessors.gos",
            "feature-testing-examples/callable_carrier_return.gos",
            "feature-testing-examples/closure_payload_typing.gos",
            "feature-testing-examples/cohort_basics.gos",
            "feature-testing-examples/cohort_cancel.gos",
            "feature-testing-examples/combinator_sweep.gos",
            "feature-testing-examples/debugfmt_nested_adts.gos",
            "feature-testing-examples/entry_result_err.gos",
            "feature-testing-examples/entry_toplevel_err.gos",
            "feature-testing-examples/error_chain_inspection.gos",
            "feature-testing-examples/from_json_infer.gos",
            "feature-testing-examples/fs_file_positional_io.gos",
            "feature-testing-examples/fs_temp_file_lifecycle.gos",
            "feature-testing-examples/httptest_record_and_clock.gos",
            "feature-testing-examples/jit_admission_shapes.gos",
            "feature-testing-examples/json_parse_jit.gos",
            "feature-testing-examples/method_name_collision.gos",
            "feature-testing-examples/nested_struct_variant_payload.gos",
            "feature-testing-examples/operator_overloads.gos",
            "feature-testing-examples/option_unwrap_chain.gos",
            "feature-testing-examples/p7_deref_format.gos",
            "feature-testing-examples/process_run_in.gos",
            "feature-testing-examples/result_struct_payload.gos",
            "feature-testing-examples/router_closure_route.gos",
            "feature-testing-examples/shared_across_goroutines.gos",
            "feature-testing-examples/slice_methods.gos",
            "feature-testing-examples/sql_driverless.gos",
            "feature-testing-examples/sql_native_driver.gos",
            "feature-testing-examples/stdlib_env_portable.gos",
            "feature-testing-examples/stdlib_errors_chain.gos",
            "feature-testing-examples/stdlib_expansion.gos",
            "feature-testing-examples/stdlib_fs_portable.gos",
            "feature-testing-examples/stdlib_io_read_all.gos",
            "feature-testing-examples/stdlib_leaf_calls_and_json_queries.gos",
            "feature-testing-examples/struct_copy_followups.gos",
            "feature-testing-examples/top_level_question.gos",
            "feature-testing-examples/try_err_conversion.gos",
            "feature-testing-examples/winb2_parse_u64.gos",
            "feature-testing-examples/winb_coll_optres.gos",
            "feature-testing-examples/write_file_bytes.gos",
            "feature-testing-examples/yaml_autoderive.gos",
        ],
    ),
    (
        "std::flag",
        &[
            "examples/cli_args.gos",
            "examples/grep.gos",
            "feature-testing-examples/aggregate_binding.gos",
            "feature-testing-examples/flag_cell_duration.gos",
        ],
    ),
    (
        "std::fmt",
        &["feature-testing-examples/display_impl_dispatch.gos"],
    ),
    (
        "std::fs",
        &[
            "examples/file_io.gos",
            "examples/list_dir.gos",
            "feature-testing-examples/comptime_embedded_assets.gos",
            "feature-testing-examples/fs_dir_ops.gos",
            "feature-testing-examples/fs_error_text.gos",
            "feature-testing-examples/fs_file_positional_io.gos",
            "feature-testing-examples/fs_metadata.gos",
            "feature-testing-examples/fs_permission_modes.gos",
            "feature-testing-examples/fs_read_to_string_missing.gos",
            "feature-testing-examples/fs_temp_file_lifecycle.gos",
            "feature-testing-examples/fs_temp_resources.gos",
            "feature-testing-examples/net_unix_echo.gos",
            "feature-testing-examples/nul_in_strings.gos",
            "feature-testing-examples/process_run_in.gos",
            "feature-testing-examples/stdlib_alias_wiring.gos",
            "feature-testing-examples/stdlib_env_portable.gos",
            "feature-testing-examples/stdlib_fs_portable.gos",
            "feature-testing-examples/stdlib_fs_rename.gos",
            "feature-testing-examples/stdlib_path_glob.gos",
            "feature-testing-examples/write_file_bytes.gos",
        ],
    ),
    (
        "std::hash::adler32",
        &["feature-testing-examples/nul_in_strings.gos"],
    ),
    (
        "std::hash::crc32",
        &["feature-testing-examples/nul_in_strings.gos"],
    ),
    (
        "std::hash::fnv",
        &[
            "feature-testing-examples/nul_in_strings.gos",
            "feature-testing-examples/stdlib_leaf_calls_and_json_queries.gos",
        ],
    ),
    (
        "std::html",
        &[
            "feature-testing-examples/html_escape.gos",
            "feature-testing-examples/html_template_render_json.gos",
        ],
    ),
    (
        "std::http",
        &[
            "examples/http_diagnostics_transport.gos",
            "examples/web_server.gos",
            "feature-testing-examples/closure_payload_typing.gos",
            "feature-testing-examples/http3_serve_err_binding.gos",
            "feature-testing-examples/http_cookie.gos",
            "feature-testing-examples/http_csrf.gos",
            "feature-testing-examples/http_csrf_attach.gos",
            "feature-testing-examples/http_form_multipart.gos",
            "feature-testing-examples/http_form_urlencoded.gos",
            "feature-testing-examples/http_serve_err_binding.gos",
            "feature-testing-examples/http_session.gos",
            "feature-testing-examples/http_session_roundtrip.gos",
            "feature-testing-examples/http_surface.gos",
            "feature-testing-examples/httptest_record_and_clock.gos",
            "feature-testing-examples/httptest_static_server.gos",
            "feature-testing-examples/option_none_variant_collision.gos",
            "feature-testing-examples/router_closure_route.gos",
        ],
    ),
    (
        "std::http::router",
        &[
            "examples/web_server.gos",
            "feature-testing-examples/http_router_lookup.gos",
            "feature-testing-examples/router_closure_route.gos",
        ],
    ),
    (
        "std::http_h3",
        &["feature-testing-examples/http3_serve_err_binding.gos"],
    ),
    (
        "std::httptest",
        &[
            "examples/http_diagnostics_transport.gos",
            "feature-testing-examples/httptest_record_and_clock.gos",
            "feature-testing-examples/httptest_static_server.gos",
        ],
    ),
    (
        "std::io",
        &[
            "examples/grep.gos",
            "feature-testing-examples/stdlib_io_adapters.gos",
            "feature-testing-examples/stdlib_io_copy.gos",
            "feature-testing-examples/stdlib_io_read_all.gos",
        ],
    ),
    (
        "std::iter",
        &[
            "examples/caesar_cipher.gos",
            "examples/environment.gos",
            "examples/function_piping.gos",
            "examples/range_sum.gos",
            "examples/reverse_string.gos",
            "feature-testing-examples/aggr_enum_vec_combinators.gos",
            "feature-testing-examples/closure_env_container_capture.gos",
            "feature-testing-examples/closure_payload_typing.gos",
            "feature-testing-examples/combinator_sweep.gos",
            "feature-testing-examples/elemty_aggregate_elements.gos",
            "feature-testing-examples/elemty_float_eager.gos",
            "feature-testing-examples/for_over_free_call.gos",
            "feature-testing-examples/iter_extra.gos",
            "feature-testing-examples/iter_free_function_contracts.gos",
            "feature-testing-examples/iter_pipeline_fusion.gos",
            "feature-testing-examples/pipe_closure_step.gos",
            "feature-testing-examples/range_pipeline_iter.gos",
            "feature-testing-examples/seq_method_combinators.gos",
            "feature-testing-examples/stdlib_surface_join_parse_take.gos",
            "feature-testing-examples/string_method_surface.gos",
            "feature-testing-examples/temporary_wrap.gos",
            "feature-testing-examples/winb_coll_iter.gos",
            "feature-testing-examples/zip_pair_elements.gos",
        ],
    ),
    ("std::jwt", &["feature-testing-examples/jwt_roundtrip.gos"]),
    (
        "std::math",
        &[
            "examples/big_numbers.gos",
            "feature-testing-examples/callback_shorthands.gos",
            "feature-testing-examples/inline_scalar_kernel.gos",
            "feature-testing-examples/misc_class_a.gos",
            "feature-testing-examples/stdlib_math_bits.gos",
            "feature-testing-examples/stdlib_math_const.gos",
            "feature-testing-examples/stdlib_math_pred.gos",
            "feature-testing-examples/stdlib_math_scalar.gos",
            "feature-testing-examples/stdlib_parity_batch.gos",
            "feature-testing-examples/trait_object_dispatch.gos",
            "feature-testing-examples/winb_data_math.gos",
        ],
    ),
    (
        "std::math::bits",
        &["feature-testing-examples/combinator_element_kinds.gos"],
    ),
    (
        "std::math::rand",
        &[
            "feature-testing-examples/math_rand.gos",
            "feature-testing-examples/winb_data_math.gos",
        ],
    ),
    (
        "std::metrics",
        &["feature-testing-examples/metrics_observability.gos"],
    ),
    ("std::mime", &["examples/mime_demo.gos"]),
    (
        "std::net",
        &[
            "feature-testing-examples/net_ip.gos",
            "feature-testing-examples/net_smtp_send.gos",
            "feature-testing-examples/net_tcp_echo.gos",
            "feature-testing-examples/net_tcp_read_deadline.gos",
            "feature-testing-examples/net_tls_client.gos",
            "feature-testing-examples/net_tls_client_modes.gos",
            "feature-testing-examples/net_unix_echo.gos",
        ],
    ),
    ("std::net::netip", &["examples/netip_demo.gos"]),
    (
        "std::net::smtp",
        &["feature-testing-examples/net_smtp_send.gos"],
    ),
    ("std::net::url", &["examples/url_escape_demo.gos"]),
    (
        "std::option",
        &[
            "examples/function_piping.gos",
            "feature-testing-examples/callable_carrier_return.gos",
            "feature-testing-examples/closure_payload_typing.gos",
            "feature-testing-examples/combinator_sweep.gos",
            "feature-testing-examples/option_default.gos",
            "feature-testing-examples/temporary_wrap.gos",
            "feature-testing-examples/winb_coll_iter.gos",
            "feature-testing-examples/winb_coll_optres.gos",
        ],
    ),
    (
        "std::os",
        &[
            "feature-testing-examples/process_run_in.gos",
            "feature-testing-examples/stdlib_os_introspection.gos",
            "feature-testing-examples/winb_sys_misc.gos",
        ],
    ),
    (
        "std::os::exec",
        &[
            "feature-testing-examples/exec_pipeline.gos",
            "feature-testing-examples/exec_signal_group.gos",
            "feature-testing-examples/exec_wait_timeout.gos",
            "feature-testing-examples/process_spawn_pipe.gos",
        ],
    ),
    (
        "std::os::signal",
        &["feature-testing-examples/os_signal_subscribe.gos"],
    ),
    ("std::os::user", &["examples/os_user_demo.gos"]),
    (
        "std::path",
        &[
            "examples/file_io.gos",
            "feature-testing-examples/fast_string_path_scan.gos",
            "feature-testing-examples/fs_dir_ops.gos",
            "feature-testing-examples/fs_file_positional_io.gos",
            "feature-testing-examples/fs_metadata.gos",
            "feature-testing-examples/fs_permission_modes.gos",
            "feature-testing-examples/fs_temp_file_lifecycle.gos",
            "feature-testing-examples/fs_temp_resources.gos",
            "feature-testing-examples/nul_in_strings.gos",
            "feature-testing-examples/path_split.gos",
            "feature-testing-examples/path_value.gos",
            "feature-testing-examples/process_run_in.gos",
            "feature-testing-examples/stdlib_alias_wiring.gos",
            "feature-testing-examples/stdlib_env_portable.gos",
            "feature-testing-examples/stdlib_expansion.gos",
            "feature-testing-examples/stdlib_fs_portable.gos",
            "feature-testing-examples/stdlib_fs_rename.gos",
            "feature-testing-examples/stdlib_path_free.gos",
            "feature-testing-examples/stdlib_path_glob.gos",
            "feature-testing-examples/winb_sys_path.gos",
            "feature-testing-examples/write_file_bytes.gos",
        ],
    ),
    (
        "std::pprof",
        &["feature-testing-examples/pprof_profiles.gos"],
    ),
    (
        "std::process",
        &[
            "examples/grep.gos",
            "feature-testing-examples/process_run_in.gos",
            "feature-testing-examples/process_spawn_piped.gos",
            "feature-testing-examples/stdlib_process.gos",
            "feature-testing-examples/top_level_exit_code.gos",
        ],
    ),
    (
        "std::regex",
        &[
            "examples/regex.gos",
            "feature-testing-examples/early_break_materializers.gos",
            "feature-testing-examples/regex_unicode_categories.gos",
            "feature-testing-examples/stdlib_compiled_wiring.gos",
            "feature-testing-examples/winb2_regex_find_all_bound.gos",
            "feature-testing-examples/winb_data_regex.gos",
        ],
    ),
    (
        "std::result",
        &[
            "feature-testing-examples/callable_carrier_return.gos",
            "feature-testing-examples/closure_payload_typing.gos",
            "feature-testing-examples/combinator_sweep.gos",
            "feature-testing-examples/result_default.gos",
            "feature-testing-examples/winb_coll_optres.gos",
        ],
    ),
    (
        "std::runtime",
        &[
            "feature-testing-examples/arena_regions.gos",
            "feature-testing-examples/cohort_cancel.gos",
            "feature-testing-examples/cycle_collector.gos",
            "feature-testing-examples/cycle_reclaim.gos",
            "feature-testing-examples/cycle_shared_goroutines.gos",
            "feature-testing-examples/panic_hook.gos",
            "feature-testing-examples/weak_into_strong_cycle.gos",
        ],
    ),
    (
        "std::sandbox",
        &[
            "feature-testing-examples/sandbox.gos",
            "feature-testing-examples/sandbox_env_allow_all.gos",
        ],
    ),
    ("std::slog", &["feature-testing-examples/stdlib_slog.gos"]),
    (
        "std::sort",
        &[
            "feature-testing-examples/for_over_free_call.gos",
            "feature-testing-examples/stdlib_sort_module.gos",
        ],
    ),
    (
        "std::strconv",
        &[
            "feature-testing-examples/strconv_radix_quote.gos",
            "feature-testing-examples/struct_copy_reclaim.gos",
            "feature-testing-examples/winb2_parse_u64.gos",
            "feature-testing-examples/winb_text_strconv.gos",
        ],
    ),
    (
        "std::strings",
        &[
            "feature-testing-examples/callback_shorthands.gos",
            "feature-testing-examples/closure_payload_typing.gos",
            "feature-testing-examples/early_break_materializers.gos",
            "feature-testing-examples/encoding_xml.gos",
            "feature-testing-examples/enum_transform_jit.gos",
            "feature-testing-examples/fast_string_path_scan.gos",
            "feature-testing-examples/for_over_free_call.gos",
            "feature-testing-examples/go_stdlib_spawn.gos",
            "feature-testing-examples/method_dispatch_collisions.gos",
            "feature-testing-examples/metrics_observability.gos",
            "feature-testing-examples/misc_class_a.gos",
            "feature-testing-examples/nul_in_strings.gos",
            "feature-testing-examples/pipe_closure_step.gos",
            "feature-testing-examples/range_non_i64.gos",
            "feature-testing-examples/stdlib_expansion.gos",
            "feature-testing-examples/stdlib_strings_free.gos",
            "feature-testing-examples/trace_observability.gos",
            "feature-testing-examples/winb_text_strings.gos",
        ],
    ),
    (
        "std::sync",
        &[
            "examples/concurrency.gos",
            "feature-testing-examples/atomic_bool.gos",
            "feature-testing-examples/bounded_channel.gos",
            "feature-testing-examples/chan_select_struct_payload.gos",
            "feature-testing-examples/chan_struct_local_recv.gos",
            "feature-testing-examples/chan_struct_payload.gos",
            "feature-testing-examples/channel_close_drain.gos",
            "feature-testing-examples/channel_fan_in.gos",
            "feature-testing-examples/channel_progress_not_deadlock.gos",
            "feature-testing-examples/channel_semantics_conformance.gos",
            "feature-testing-examples/closure_goroutine.gos",
            "feature-testing-examples/concurrency_stress_shapes.gos",
            "feature-testing-examples/concurrent_atomic.gos",
            "feature-testing-examples/context_aware_waits.gos",
            "feature-testing-examples/context_lifecycle.gos",
            "feature-testing-examples/cycle_shared_goroutines.gos",
            "feature-testing-examples/go_stdlib_spawn.gos",
            "feature-testing-examples/goroutine_deque_handle.gos",
            "feature-testing-examples/goroutine_panic_isolation.gos",
            "feature-testing-examples/goroutine_set_handle.gos",
            "feature-testing-examples/goroutine_shared_map.gos",
            "feature-testing-examples/mutex_poison_recovery.gos",
            "feature-testing-examples/mutex_vs_channel_counter.gos",
            "feature-testing-examples/net_smtp_send.gos",
            "feature-testing-examples/net_tcp_echo.gos",
            "feature-testing-examples/net_tls_client.gos",
            "feature-testing-examples/net_tls_client_modes.gos",
            "feature-testing-examples/net_unix_echo.gos",
            "feature-testing-examples/os_signal_handler.gos",
            "feature-testing-examples/scheduler_drain.gos",
            "feature-testing-examples/select_closed_chan_ready.gos",
            "feature-testing-examples/select_default_timing.gos",
            "feature-testing-examples/select_multiplex.gos",
            "feature-testing-examples/shared_across_goroutines.gos",
            "feature-testing-examples/sync_extra.gos",
            "feature-testing-examples/sync_map_demo.gos",
            "feature-testing-examples/sync_rwlock.gos",
            "feature-testing-examples/tw_go_block.gos",
            "feature-testing-examples/unit_main_goroutine_drain.gos",
            "feature-testing-examples/waitgroup_many_waiters.gos",
        ],
    ),
    (
        "std::testing",
        &[
            "examples/defer_cleanup.gos",
            "examples/derive.gos",
            "examples/testing.gos",
            "examples/tuples.gos",
            "feature-testing-examples/array_bounds_probe.gos",
            "feature-testing-examples/array_literal_vec_methods.gos",
            "feature-testing-examples/channel_close_drain.gos",
            "feature-testing-examples/channel_fan_in.gos",
            "feature-testing-examples/closure_capture_mutation.gos",
            "feature-testing-examples/closure_lifetime_inference.gos",
            "feature-testing-examples/defer_unwind_order.gos",
            "feature-testing-examples/derive_traits.gos",
            "feature-testing-examples/doc_test_vs_unit_test_drift.gos",
            "feature-testing-examples/error_chain_inspection.gos",
            "feature-testing-examples/error_question_mark_propagation.gos",
            "feature-testing-examples/float_cast_drift.gos",
            "feature-testing-examples/for_lazy_iterator_source.gos",
            "feature-testing-examples/format_precision_padding.gos",
            "feature-testing-examples/format_spec.gos",
            "feature-testing-examples/fs_temp_file_lifecycle.gos",
            "feature-testing-examples/generic_function_monomorphization.gos",
            "feature-testing-examples/goroutine_panic_isolation.gos",
            "feature-testing-examples/hashmap_counter_race.gos",
            "feature-testing-examples/hashset_algebra.gos",
            "feature-testing-examples/iter_pipeline_fusion.gos",
            "feature-testing-examples/iterator_loop_sources.gos",
            "feature-testing-examples/iterator_param_argument.gos",
            "feature-testing-examples/json_round_trip_fuzz.gos",
            "feature-testing-examples/let_else_binding.gos",
            "feature-testing-examples/literal_forms.gos",
            "feature-testing-examples/loop_continue.gos",
            "feature-testing-examples/match_or_patterns.gos",
            "feature-testing-examples/method_dispatch_collision.gos",
            "feature-testing-examples/mutex_poison_recovery.gos",
            "feature-testing-examples/mutex_vs_channel_counter.gos",
            "feature-testing-examples/numeric_conversion_matrix.gos",
            "feature-testing-examples/option_unwrap_chain.gos",
            "feature-testing-examples/or_patterns.gos",
            "feature-testing-examples/os_signal_handler.gos",
            "feature-testing-examples/panic_recover_round_trip.gos",
            "feature-testing-examples/pattern_match_exhaustiveness.gos",
            "feature-testing-examples/pipe_closure_step.gos",
            "feature-testing-examples/pipe_operator_precedence.gos",
            "feature-testing-examples/process_spawn_pipe.gos",
            "feature-testing-examples/recursive_enum_walk.gos",
            "feature-testing-examples/reference_alias_mutation.gos",
            "feature-testing-examples/regex_unicode_categories.gos",
            "feature-testing-examples/select_closed_chan_ready.gos",
            "feature-testing-examples/select_ctx_cancel.gos",
            "feature-testing-examples/select_default_timing.gos",
            "feature-testing-examples/select_multiplex.gos",
            "feature-testing-examples/slice_subslicing.gos",
            "feature-testing-examples/sort_with_closure.gos",
            "feature-testing-examples/spawn_join.gos",
            "feature-testing-examples/static_items.gos",
            "feature-testing-examples/strconv_radix_quote.gos",
            "feature-testing-examples/string_char_needle.gos",
            "feature-testing-examples/string_concatenation_stress.gos",
            "feature-testing-examples/string_match_patterns.gos",
            "feature-testing-examples/string_method_surface.gos",
            "feature-testing-examples/string_unicode_boundaries.gos",
            "feature-testing-examples/struct_map_keys.gos",
            "feature-testing-examples/time_monotonic_vs_wall.gos",
            "feature-testing-examples/trait_object_dispatch.gos",
            "feature-testing-examples/tuple_destructuring_loop.gos",
            "feature-testing-examples/tuple_surface.gos",
            "feature-testing-examples/variable_shadowing_ladder.gos",
        ],
    ),
    (
        "std::thread",
        &[
            "feature-testing-examples/misc_class_a.gos",
            "feature-testing-examples/stdlib_thread_yield.gos",
        ],
    ),
    (
        "std::time",
        &[
            "examples/sleep_demo.gos",
            "examples/structured_concurrency.gos",
            "feature-testing-examples/channel_close_drain.gos",
            "feature-testing-examples/channel_fan_in.gos",
            "feature-testing-examples/channel_timers.gos",
            "feature-testing-examples/cohort_cancel.gos",
            "feature-testing-examples/context_aware_waits.gos",
            "feature-testing-examples/duration_methods.gos",
            "feature-testing-examples/flag_cell_duration.gos",
            "feature-testing-examples/go_stdlib_spawn.gos",
            "feature-testing-examples/goroutine_panic_isolation.gos",
            "feature-testing-examples/httptest_record_and_clock.gos",
            "feature-testing-examples/instant_methods.gos",
            "feature-testing-examples/mut_ref_container_params.gos",
            "feature-testing-examples/mutex_poison_recovery.gos",
            "feature-testing-examples/mutex_vs_channel_counter.gos",
            "feature-testing-examples/net_tcp_read_deadline.gos",
            "feature-testing-examples/os_signal_handler.gos",
            "feature-testing-examples/panic_hook.gos",
            "feature-testing-examples/select_default_timing.gos",
            "feature-testing-examples/stdlib_time_free.gos",
            "feature-testing-examples/stdlib_time_methods.gos",
            "feature-testing-examples/time_civil.gos",
            "feature-testing-examples/time_monotonic_vs_wall.gos",
            "feature-testing-examples/time_param_dispatch.gos",
            "feature-testing-examples/unit_main_goroutine_drain.gos",
            "feature-testing-examples/winb_sys_time.gos",
        ],
    ),
    (
        "std::trace",
        &["feature-testing-examples/trace_observability.gos"],
    ),
    (
        "std::unicode",
        &[
            "feature-testing-examples/stdlib_unicode_norm.gos",
            "feature-testing-examples/unicode_full.gos",
            "feature-testing-examples/winb_text_unicode.gos",
        ],
    ),
    (
        "std::utf8",
        &[
            "feature-testing-examples/stdlib_parity_batch.gos",
            "feature-testing-examples/stdlib_text_codec.gos",
            "feature-testing-examples/string_len_bytes.gos",
            "feature-testing-examples/unicode_full.gos",
            "feature-testing-examples/winb_text_utf8.gos",
        ],
    ),
    ("std::uuid", &["examples/uuid_demo.gos"]),
    (
        "std::validate",
        &[
            "feature-testing-examples/validate_errors.gos",
            "feature-testing-examples/validate_errors_return.gos",
        ],
    ),
    // END generated by `cargo xtask item-fixtures`
];

/// The status to report for `path`, given what was authored for it.
///
/// An authored label is a statement of intent and acts as a ceiling, not
/// as a claim: a surface no fixture exercises reports [`Status::Unproven`]
/// however it was labelled. `Planned`, `Removed`, and `Declined` describe
/// surfaces that are not meant to run, so evidence is not what settles
/// them and they pass through unchanged.
#[must_use]
pub fn derived_status(path: &str, authored: Status) -> Status {
    match authored {
        Status::Planned | Status::Removed | Status::Declined | Status::Unproven => authored,
        Status::Stable | Status::Shipped | Status::Experimental => {
            if fixtures_for(path).is_empty() {
                Status::Unproven
            } else {
                authored
            }
        }
    }
}

/// Fixtures covering `path`, or the module `path` belongs to. An item
/// inherits its module's fixtures: a program that asserts `fs::write` and
/// `fs::read_to_string` round-trip is evidence for both, and naming the
/// module keeps the ledger from having to restate every call a fixture
/// makes.
fn fixtures_for(path: &str) -> &'static [&'static str] {
    let module = path.rsplit_once("::").map_or(path, |(head, _)| head);
    ITEM_FIXTURES
        .iter()
        .find(|(covered, _)| *covered == path || *covered == module)
        .map_or(&[][..], |(_, fixtures)| *fixtures)
}

/// Materializes the audit ledger fields for one canonical item ID.
///
/// The function is also used for flattened stdlib exports, which makes the
/// item ledger complete even where a module has no hand-written lifecycle
/// override. Test arrays start empty until a fixture explicitly claims the
/// item; this is honest metadata rather than an inferred passing result.
#[must_use]
pub fn item_evidence(path: &str, status: Status) -> ItemEvidence {
    let doc_path = if let Some(rest) = path.strip_prefix("std::") {
        Some(format!("docs_src/stdlib/{}.md", rest.replace("::", "_")))
    } else if let Some(rest) = path.strip_prefix("lang::") {
        Some(format!("docs_src/language/{}.md", rest.replace("::", "_")))
    } else {
        Some(format!("docs_src/misc/{}.md", path.replace("::", "_")))
    };
    let known_limits = match status {
        Status::Unproven => {
            vec!["No fixture exercises this surface; nothing is claimed about it.".to_string()]
        }
        Status::Stable | Status::Shipped => Vec::new(),
        Status::Experimental => vec![
            "Experimental surface; consult the item documentation before relying on it."
                .to_string(),
        ],
        Status::Planned => vec!["Planned surface; no implementation contract.".to_string()],
        Status::Removed => {
            vec!["Removed surface; retained only for migration guidance.".to_string()]
        }
        Status::Declined => {
            vec!["Declined; see the item documentation for the reasoning.".to_string()]
        }
    };

    // Tiers come from a fixture that ran on them, never from the status.
    // A lifecycle label is a statement of intent; deriving the tiers from
    // it would make this field restate the claim it exists to support.
    // With no fixture there is no tier evidence, and the honest answer is
    // an empty list.
    let fixtures = fixtures_for(path);
    let (supported_tiers, supported_targets) = if fixtures.is_empty() {
        (&[][..], HOST_TARGET)
    } else {
        (ALL_TIERS, MATRIX_TARGETS)
    };
    ItemEvidence {
        status,
        supported_tiers,
        supported_targets,
        doc_path,
        positive_tests: fixtures.iter().map(|f| (*f).to_string()).collect(),
        negative_tests: Vec::new(),
        known_limits,
    }
}

impl Status {
    /// Returns the short lowercase tag printed in the table and
    /// embedded in doc pages (`"shipped"`, `"experimental"`, ...).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Status::Unproven => "unproven",
            Status::Stable => "stable",
            Status::Shipped => "shipped",
            Status::Experimental => "experimental",
            Status::Planned => "planned",
            Status::Removed => "removed",
            Status::Declined => "declined",
        }
    }

    /// Parses the inverse of [`Status::tag`]. Returns `None` for any
    /// unrecognised tag so `--status=foo` can surface a CLI error
    /// instead of silently matching nothing.
    #[must_use]
    pub fn parse(tag: &str) -> Option<Status> {
        match tag {
            "stable" => Some(Status::Stable),
            "shipped" => Some(Status::Shipped),
            "experimental" => Some(Status::Experimental),
            "planned" => Some(Status::Planned),
            "removed" => Some(Status::Removed),
            "declined" => Some(Status::Declined),
            _ => None,
        }
    }
}

/// One entry in the lifecycle registry - qualified path, status,
/// brief description.
#[derive(Debug, Clone, Copy)]
pub struct FeatureStatus {
    /// Canonical path. Stdlib modules use the `std::foo::bar`
    /// shape; language features use the `lang::if_let` shape so
    /// the two namespaces never collide.
    pub path: &'static str,
    /// Lifecycle stage.
    pub status: Status,
    /// One-line description surfaced in `gos feature-status`.
    pub doc: &'static str,
}

/// Explicit lifecycle entries for documented language features and
/// audited stdlib module statuses. Manifest modules default to
/// `Experimental` when materialized from `manifest::ALL_MODULES`;
/// `Shipped` must be explicit here.
pub const FEATURE_STATUS: &[FeatureStatus] = &[
    // -----------------------------------------------------------------
    // Language features. All `lang::*` so the namespace never collides
    // with the `std::*` stdlib paths.
    // -----------------------------------------------------------------
    lang("lang::let", "Immutable binding."),
    lang(
        "lang::let_mut",
        "Mutable bindings can be reassigned and can be the source of `&mut`.",
    ),
    lang("lang::if", "Conditional expression."),
    lang("lang::match", "Exhaustive pattern match expression."),
    lang("lang::if_let", "Single-variant pattern sugar."),
    lang(
        "lang::while_let",
        "Loop that drains while a pattern matches.",
    ),
    lang("lang::for", "Iterator-driven loop."),
    lang("lang::loop", "Unconditional loop with `break value`."),
    lang(
        "lang::break",
        "Exit the innermost loop, optionally with a value.",
    ),
    lang(
        "lang::continue",
        "Skip to the next iteration of the innermost loop.",
    ),
    lang("lang::return", "Exit the enclosing function with a value."),
    lang(
        "lang::question_mark",
        "Short-circuit Result / Option propagation operator.",
    ),
    lang(
        "lang::pipe",
        "Forward-pipe operator `|>`, for composing free functions in a functional style. A step is either a bare callable (`x |> f`) or a closure whose parameter is the piped value (`x |> |v| f(a, v)`). Methods chain on their own and are the shorter spelling; a method chain can feed a pipe.",
    ),
    lang("lang::closure", "Lambda expression `|args| body`."),
    lang(
        "lang::callback_shorthand",
        "A callback written without `|v|`: a std free function named in value position stands for the closure that calls it, as in `xs.map(math::abs)`.",
    ),
    lang("lang::fn", "Function declaration."),
    lang("lang::struct", "Product type declaration."),
    lang(
        "lang::enum",
        "Sum type declaration with payload-carrying variants.",
    ),
    lang("lang::trait", "Behaviour interface declaration."),
    lang("lang::impl", "Inherent and trait implementation blocks."),
    lang(
        "lang::generics",
        "Type parameters on functions / impls / structs.",
    ),
    lang("lang::go", "Goroutine spawn, detached."),
    lang(
        "lang::cohort",
        "Structured concurrency: `cohort { }` owns the goroutines `spawn`ed inside it, joins them on every exit path, and reports the first failure as its `Result`.",
    ),
    lang(
        "lang::triple_quoted_string",
        "`\"\"\"` string literal whose body is dedented by the indentation it shares with its closing delimiter; `gos fmt` moves the block with the line that opens it.",
    ),
    lang("lang::select", "Channel multiplex select expression."),
    lang("lang::channel", "Typed channel via `std::sync::channel`."),
    FeatureStatus {
        path: "lang::weak_references",
        status: Status::Experimental,
        doc: "`Weak<T>` downgrade/upgrade handles. Native collection is thread-local only and the bytecode VM has no cycle collector, so cross-tier cyclic reclamation is not yet a Stable guarantee.",
    },
    lang(
        "lang::spawn",
        "Goroutine join handle: `spawn(f)` -> `JoinHandle<T>`, `.join()` -> `Result<T, String>`.",
    ),
    lang(
        "lang::macros",
        "Built-in macros only - no user-defined macros: the format family (print/println/eprint/eprintln/format/panic), the desugar macros (matches!/todo!/unimplemented!/unreachable!/dbg!), and the build-time regex!/sql!/codegen!.",
    ),
    lang(
        "lang::doctest",
        "Fenced code in `//` doc comments runs under `gos test`.",
    ),
    lang("lang::cfg", "Conditional compilation attribute."),
    lang(
        "lang::attribute",
        "Built-in attributes (`#[cfg]`, `#[test]`, `#[bench]`, `#[derive]`).",
    ),
    lang("lang::const", "Compile-time constant binding."),
    lang(
        "lang::static",
        "Module-level mutable or immutable static slot.",
    ),
    lang(
        "lang::opaque_nominal_alias",
        "`type Name = new Repr` declares a distinct nominal type over an unchanged runtime representation, erased before lowering so no tier sees one. It inherits equality, ordering, hashing, and formatting - which describe the value both sides share - and nothing else: arithmetic needs the alias's own `impl Add`, and the representation's methods are not in scope. `.into()` converts to and from its own representation; any other pair needs `impl From`.",
    ),
    lang(
        "lang::slicing",
        "A range in index position takes a subsequence: `xs[1..3]`, `xs[..k]`, `xs[k..]`, `xs[..]`, `xs[a..=b]`, over fixed arrays, slices, `Vec`, and `String`. Bounds clamp rather than panic, matching `substring`; a `String` slice takes byte offsets and snaps to codepoint boundaries.",
    ),
    lang(
        "lang::visibility",
        "Three visibilities: private by default (the declaring module and its descendants), `pub(package)` (every module of the declaring package), and `pub` (the package's public API). Declared per item, per method, and per struct field; `pub(crate)` / `pub(super)` / `pub(in path)` are rejected (`GP0038`).",
    ),
    lang(
        "lang::type_alias",
        "Transparent type alias: `type X = T` (and generic `type Pair<A> = (A, A)`) is interchangeable with its target everywhere; a cyclic alias is rejected (`GT0024`).",
    ),
    lang(
        "lang::mut_ref_params",
        "Local `&mut` aliases write through; `&mut Vec<T>` / `&mut [T]` parameters write through on every tier.",
    ),
    // Identifier rules - Unicode XID_Start / XID_Continue (UAX #31).
    lang(
        "lang::unicode_identifiers",
        "Identifiers follow UAX #31 (matches Rust 2024).",
    ),
    // Compile-time evaluation. Folds to a literal before the tiers split.
    lang(
        "lang::comptime",
        "Zig-style compile-time evaluation: `comptime { ... }` blocks, `comptime fn` calls, and `comptime` parameters run on the bytecode VM during compilation and fold to a literal, so every tier compiles the identical constant. `typeInfo::<T>()` reflects a struct's fields, a tuple struct's positions, or an enum's variants - substituting the arguments for a generic instantiation - and a `for (name, ty) in typeInfo::<T>()` loop unrolls into native per-field code, and `codegen!(...)` splices a `comptime fn`'s `String` back as source. Includes the `regex!` / `sql!` build-time validation macros.",
    ),
    // Caller-side argument spellings. Rewritten into declared order before
    // type checking, so every tier compiles the same positional call.
    lang(
        "lang::keyword_arguments",
        "Keyword arguments and constant parameter defaults: a call may name any parameter (`volume(depth = 4, width = 2)`), and a parameter may declare a constant default (`fn volume(width: i64, height: i64 = 2)`) that is spliced into every call omitting it. Positional arguments come first, then names. Both are caller-side spellings rewritten into the callee's declared order before type checking, so the calling convention is unchanged. A name on a method call is matched when every type declaring that method name would rewrite the call identically; when they disagree the call is reported (GR0013) rather than guessed.",
    ),
    // Planned / partial language surface.
    FeatureStatus {
        path: "lang::move_keyword",
        status: Status::Declined,
        doc: "`move` closure capture keyword - declined permanently (SPEC 17.5). Capture is automatic and the runtime manages ownership, so `move` would annotate a decision the language does not make.",
    },
    FeatureStatus {
        path: "lang::async_await",
        status: Status::Declined,
        doc: "`async fn` / `.await` - declined permanently (SPEC 17.5). Goroutines and channels cover the same shape without colored functions.",
    },
    FeatureStatus {
        path: "lang::lifetimes",
        status: Status::Declined,
        doc: "Explicit lifetime annotations and a borrow checker - declined permanently (SPEC 17.5). References have implicit lexical lifetimes ending at the closing brace, and the lexical `&mut` check is the intended ceiling.",
    },
    // -----------------------------------------------------------------
    // Stdlib status overrides. Modules are shipped library surface; these
    // entries retain their specific documentation and namespace contracts.
    // -----------------------------------------------------------------
    FeatureStatus {
        path: "std::crypto::x509",
        status: Status::Shipped,
        doc: "All-tier private-root X.509 parsing and fail-closed server verification with mandatory CRLs. System roots, revocation retrieval, issuance, and mutable source-level TLS configuration are deliberately out of scope.",
    },
    FeatureStatus {
        path: "std::tls",
        status: Status::Shipped,
        doc: "TLS surface (rustls-backed) - handshake and host-configured mTLS work. The all-tier x509 verifier exposes fail-closed CRL-backed server-chain validation; public TLS connection configuration remains in progress.",
    },
    FeatureStatus {
        path: "std::runtime::collect_cycles",
        status: Status::Experimental,
        doc: "Explicit cycle-collection hook. It returns `()`; native collection covers thread-local RC graphs, while the bytecode VM currently treats it as a no-op.",
    },
    FeatureStatus {
        path: "std::database::sql",
        status: Status::Experimental,
        doc: "Driver-pluggable SQL access (Conn, Tx, Stmt, Rows, Pool, migrate_up, query::Select). Host drivers register at startup via gossamer_runtime::sql::register; Gossamer-native drivers use sql::register_native. No driver ships in the box.",
    },
    FeatureStatus {
        path: "std::html::template",
        status: Status::Shipped,
        doc: "Context-aware HTML template engine - auto-escape works (text/attr/URL/JS), pipeline operator set still expanding. Heuristic classifier, NOT a content-security-policy substitute; the `html::escape` primitive (wired on every tier) is the supported cross-tier escape.",
    },
    // Namespace decisions document one spelling instead of growing aliases.
    FeatureStatus {
        path: "std::process",
        status: Status::Shipped,
        doc: "Canonical current-process and child-process API.",
    },
    FeatureStatus {
        path: "std::os::exec",
        status: Status::Shipped,
        doc: "Deprecated compatibility facade for pre-0.27 child-process code; use `std::process`. It remains wired during the 0.x line but receives no new API.",
    },
    FeatureStatus {
        path: "std::path",
        status: Status::Shipped,
        doc: "Lexical filesystem-path API. It uses platform path grammar and never parses, escapes, or resolves network URLs.",
    },
    FeatureStatus {
        path: "std::fs",
        status: Status::Shipped,
        doc: "Filesystem reading, writing, and traversal. The portable surface - create, write, read, copy, rename, remove, list, walk, canonicalize, and the temp-directory helpers - is exercised on every tier and on each supported host family. Symbolic links, permission bits, and ownership are platform-specific: they report `Unsupported` where the host has no equivalent, and creating a link needs privilege on Windows.",
    },
    FeatureStatus {
        path: "std::env",
        status: Status::Shipped,
        doc: "Process environment, arguments, and working directory. The portable surface is exercised on every tier and on each supported host family. Which variable backs `home_dir`, and whether the environment block compares names case-insensitively, is the host's business.",
    },
    FeatureStatus {
        path: "std::net::url",
        status: Status::Shipped,
        doc: "Network URL parser and component escaper. Do not pass filesystem paths or HTTP route matching through this API.",
    },
    FeatureStatus {
        path: "std::http_h3",
        status: Status::Experimental,
        doc: "HTTP/3 over QUIC with bounded connections, streams, headers, bodies, and wire I/O. Public handler/client bodies remain fully buffered; streaming and backpressure parity with HTTP/2 are not yet shipped. `std::http::h3` is not an alias.",
    },
    FeatureStatus {
        path: "std::thread",
        status: Status::Shipped,
        doc: "OS-thread yield and CPU-count helpers only. `go`/`spawn` plus channels are the language concurrency model; there is no user-facing `thread::spawn` API.",
    },
    // -----------------------------------------------------------------
    // Sub-module stdlib feature entries. Not manifest modules (the
    // implicit-Experimental walk never synthesises them), so the 0.13.0
    // HTTP tier-parity surface is registered explicitly.
    // -----------------------------------------------------------------
    shipped(
        "std::http::client_request_native",
        "`http::request` / `http::request_bytes` native on the compiled tiers through one ureq engine.",
    ),
    shipped(
        "std::http::response_headers",
        "Client `Response.headers` (lowercase, wire order) plus honored server response headers with chainable `with_header`.",
    ),
    shipped(
        "std::http::redirect_policy",
        "`Client::builder().max_redirects(n).timeout_ms(ms).build()`; `max_redirects(0)` returns the raw 3xx.",
    ),
    shipped(
        "std::http::binary_bodies",
        "`Response.raw_bytes` / `Request.raw_body` packed byte bodies, NUL-safe on every tier.",
    ),
    shipped(
        "std::http::streaming_responses",
        "`Response::stream` chunked server streaming plus `ResponseStream::next_chunk` client byte reads.",
    ),
    experimental(
        "std::http::request_streaming",
        "HTTP/2 request bodies can be consumed incrementally by the Rust-side RequestStreamingHandler scaffold; the public Gossamer handler ABI still receives bounded complete Request bodies on VM and AOT.",
    ),
    shipped(
        "std::http::server_request_headers",
        "Inbound `Request.headers` populated on every tier; `path` strips the query string.",
    ),
    // -----------------------------------------------------------------
    // Tooling features. `tooling::*` mirrors the `lang::*` namespace
    // convention for surface that is neither language nor stdlib.
    // -----------------------------------------------------------------
    shipped(
        "tooling::faithful_fmt",
        "Token-stream `gos fmt`: comments and macros preserved verbatim, idempotent, no-destruction self-check.",
    ),
];

const fn lang(path: &'static str, doc: &'static str) -> FeatureStatus {
    shipped(path, doc)
}

const fn shipped(path: &'static str, doc: &'static str) -> FeatureStatus {
    FeatureStatus {
        path,
        status: Status::Shipped,
        doc,
    }
}

const fn experimental(path: &'static str, doc: &'static str) -> FeatureStatus {
    FeatureStatus {
        path,
        status: Status::Experimental,
        doc,
    }
}

/// Returns the registered status for `path`, falling back to
/// `Experimental` when `path` is a stdlib module present in
/// `manifest::ALL_MODULES` and to `None` otherwise. Callers wanting
/// the synthesised full stdlib + language surface should iterate
/// `all_entries` instead.
#[must_use]
pub fn lookup(path: &str) -> Option<FeatureStatus> {
    if let Some(entry) = FEATURE_STATUS.iter().find(|e| e.path == path) {
        return Some(*entry);
    }
    if let Some(module) = super::ALL_MODULES.iter().find(|m| m.path == path) {
        return Some(FeatureStatus {
            path: module.path,
            status: Status::Experimental,
            doc: module.summary,
        });
    }
    None
}

/// Returns the lifecycle contract for one canonical manifest item.
///
/// A module's lifecycle entry describes the module index and must never
/// silently promote each exported item. Item promotion therefore requires an
/// exact, qualified registry entry such as `std::runtime::collect_cycles`.
/// Unlisted manifest items deliberately remain Experimental until their own
/// evidence is recorded.
#[must_use]
pub fn item_status(path: &str) -> Status {
    FEATURE_STATUS
        .iter()
        .find(|entry| entry.path == path)
        .map_or(Status::Experimental, |entry| entry.status)
}

/// Returns every entry in the registry merged with the implicit
/// stdlib defaults. Stdlib modules that don't appear in
/// `FEATURE_STATUS` are synthesised as `Experimental`. Entries are
/// returned in a stable order: registry entries first (declaration
/// order), then the synthesised stdlib defaults (manifest order).
#[must_use]
pub fn all_entries() -> Vec<FeatureStatus> {
    let mut out: Vec<FeatureStatus> = FEATURE_STATUS.to_vec();
    for module in super::ALL_MODULES {
        if FEATURE_STATUS.iter().any(|e| e.path == module.path) {
            continue;
        }
        if out.iter().any(|e| e.path == module.path) {
            continue;
        }
        out.push(FeatureStatus {
            path: module.path,
            status: Status::Experimental,
            doc: module.summary,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_tag_round_trips() {
        for tag in ["stable", "shipped", "experimental", "planned", "removed"] {
            let parsed = Status::parse(tag).expect("known tag");
            assert_eq!(parsed.tag(), tag);
        }
    }

    #[test]
    fn lookup_returns_explicit_entry() {
        let entry = lookup("std::tls").expect("tls registered");
        assert_eq!(entry.status, Status::Shipped);
    }

    #[test]
    fn x509_private_root_contract_is_explicitly_shipped() {
        let entry = lookup("std::crypto::x509").expect("x509 registered");
        assert_eq!(entry.status, Status::Shipped);
        assert!(entry.doc.contains("private-root"));
    }

    #[test]
    fn weak_references_remain_explicitly_experimental() {
        let entry = lookup("lang::weak_references").expect("weak-reference status");
        assert_eq!(entry.status, Status::Experimental);
    }

    /// An item a fixture covers reports that fixture, on every tier and
    /// host the matrix runs, rather than the tiers its status implies.
    #[test]
    fn item_evidence_reports_the_fixture_that_exercises_the_item() {
        let evidence = item_evidence("std::fs::read_to_string", Status::Shipped);
        assert!(
            evidence
                .positive_tests
                .contains(&"feature-testing-examples/stdlib_fs_portable.gos".to_string()),
            "an item inherits its module's fixtures: {:?}",
            evidence.positive_tests
        );
        assert_eq!(evidence.supported_tiers, ALL_TIERS);
        assert_eq!(evidence.supported_targets, MATRIX_TARGETS);
        // An item outside the ledger claims no tiers at all. A lifecycle
        // label is a statement of intent; letting it imply a tier would
        // make the evidence restate the claim it exists to support.
        let underived = item_evidence("std::lifecycle::on_shutdown", Status::Shipped);
        assert!(underived.positive_tests.is_empty());
        assert!(
            underived.supported_tiers.is_empty(),
            "an item with no fixture must claim no tier"
        );
        assert_eq!(underived.supported_targets, HOST_TARGET);
    }

    /// Every fixture the ledger cites must exist. A renamed or deleted
    /// program would otherwise leave the ledger claiming evidence from a
    /// file nothing runs.
    #[test]
    fn every_ledger_fixture_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root is two levels above this crate");
        for (item, fixtures) in ITEM_FIXTURES {
            for fixture in *fixtures {
                assert!(
                    root.join(fixture).is_file(),
                    "{item} cites {fixture}, which does not exist"
                );
            }
        }
    }

    #[test]
    fn item_evidence_has_all_audit_fields_and_canonical_doc_location() {
        let evidence = item_evidence("std::lifecycle::on_shutdown", Status::Experimental);
        assert_eq!(evidence.status, Status::Experimental);
        assert!(evidence.supported_tiers.is_empty());
        assert_eq!(evidence.supported_targets, HOST_TARGET);
        assert_eq!(
            evidence.doc_path.as_deref(),
            Some("docs_src/stdlib/lifecycle_on_shutdown.md")
        );
        assert!(evidence.positive_tests.is_empty());
        assert!(evidence.negative_tests.is_empty());
        assert!(!evidence.known_limits.is_empty());
    }

    #[test]
    fn namespace_boundaries_are_explicit_and_lifecycle_accurate() {
        let expected = [
            ("std::process", "Canonical"),
            ("std::os::exec", "Deprecated"),
            ("std::path", "filesystem-path"),
            ("std::net::url", "Network URL"),
            ("std::http_h3", "fully buffered"),
            ("std::thread", "no user-facing `thread::spawn`"),
        ];
        for (path, contract) in expected {
            let entry = lookup(path).unwrap_or_else(|| panic!("missing status for {path}"));
            let expected_status = if path == "std::http_h3" {
                Status::Experimental
            } else {
                Status::Shipped
            };
            assert_eq!(entry.status, expected_status, "{path}");
            assert!(entry.doc.contains(contract), "{path}: {}", entry.doc);
        }
    }

    #[test]
    fn thread_surface_does_not_advertise_unavailable_os_thread_spawn() {
        let module = super::super::ALL_MODULES
            .iter()
            .find(|module| module.path == "std::thread")
            .expect("std::thread manifest module");
        let items: Vec<&str> = module.items.iter().map(|item| item.name).collect();
        assert_eq!(items, ["yield_now", "num_cpus"]);
    }

    #[test]
    fn lookup_defaults_stdlib_modules_to_experimental() {
        let entry = lookup("std::fmt").expect("fmt in manifest");
        assert_eq!(entry.status, Status::Experimental);
    }

    #[test]
    fn module_promotion_does_not_promote_unlisted_items() {
        assert_eq!(lookup("std::process").unwrap().status, Status::Shipped);
        assert_eq!(item_status("std::process::run"), Status::Experimental);
        assert_eq!(
            item_status("std::runtime::collect_cycles"),
            Status::Experimental
        );
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("std::does::not::exist").is_none());
    }

    #[test]
    fn all_entries_covers_every_stdlib_module() {
        let entries = all_entries();
        for module in super::super::ALL_MODULES {
            assert!(
                entries.iter().any(|e| e.path == module.path),
                "missing default-Experimental entry for {}",
                module.path,
            );
        }
    }

    #[test]
    fn unaudited_manifest_modules_are_not_synthesized_as_shipped() {
        let entries = all_entries();
        let fmt = entries
            .iter()
            .find(|entry| entry.path == "std::fmt")
            .expect("std::fmt synthesized");
        assert_eq!(fmt.status, Status::Experimental);
    }

    #[test]
    fn language_features_present() {
        let entries = all_entries();
        for path in ["lang::if_let", "lang::pipe", "lang::go", "lang::select"] {
            assert!(
                entries.iter().any(|e| e.path == path),
                "missing language entry {path}",
            );
        }
    }
}
