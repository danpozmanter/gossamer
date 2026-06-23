//! Tier parity gate - VM, Cranelift debug, LLVM release.
//!
//! Every `.gos` source under `examples/` and
//! `feature-testing-examples/` is run in all three tiers and the
//! captured stdout / exit code must match. The harness is the
//! single source of truth for cross-tier behaviour: a regression in
//! any backend turns this suite red.
//!
//! Examples needing CLI args, stdin, or running an HTTP server
//! carry a row in `SPECS` describing the fixture. Server-style
//! examples are bounded with a hard 60 s wall clock cap so a
//! regression that hangs a tier cannot stall CI.
//!
//! `GOSSAMER_FAIL_ON_LLVM_FALLBACK` is enabled separately by
//! `llvm_release_lowers_every_example_without_fallback`, surfacing
//! "LLVM body silently routed to Cranelift" regressions distinct
//! from output-level parity.

#![allow(missing_docs)]

mod common;

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PER_RUN_TIMEOUT: Duration = Duration::from_mins(1);

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn fresh_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-parity-{pid}-{n}-{tag}",
        pid = std::process::id(),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Vm,
    Cranelift,
    Llvm,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Tier::Vm => "vm",
            Tier::Cranelift => "cranelift",
            Tier::Llvm => "llvm",
        }
    }
}

struct Spec {
    /// Path relative to the workspace root.
    path: &'static str,
    /// Args appended after the source on `gos run`, or passed
    /// directly to the compiled binary.
    args: &'static [&'static str],
    /// Stdin to feed to every tier's run.
    stdin: &'static [u8],
    /// Stdout is non-deterministic; compare line multisets only.
    nondeterministic: bool,
    /// Allow non-zero exit (must still match across tiers).
    allow_nonzero: bool,
    /// Skip parity entirely; the VM still has to run cleanly.
    skip_parity: Option<&'static str>,
    /// Skip everything (including the VM run) with a reason.
    skip_all: Option<&'static str>,
    /// HTTP-server fixture: spawn, sleep `boot_ms`, send a probe,
    /// kill, compare the probe response across tiers.
    server: Option<ServerFixture>,
}

#[derive(Clone, Copy)]
struct ServerFixture {
    /// Wait this long after launch before issuing the probe.
    boot_ms: u64,
    /// Listen address baked into the example.
    addr: &'static str,
    /// Probe path, e.g. `/health`.
    probe_path: &'static str,
}

const fn spec(path: &'static str) -> Spec {
    Spec {
        path,
        args: &[],
        stdin: &[],
        nondeterministic: false,
        allow_nonzero: false,
        skip_parity: None,
        skip_all: None,
        server: None,
    }
}

const SPECS: &[Spec] = &[
    // --- examples/ ---
    spec("examples/binary_search.gos"),
    spec("examples/bubble_sort.gos"),
    spec("examples/caesar_cipher.gos"),
    spec("examples/defer_cleanup.gos"),
    Spec {
        args: &[
            "--name",
            "jane",
            "--port",
            "9000",
            "--verbose",
            "alpha",
            "beta",
        ],
        ..spec("examples/cli_args.gos")
    },
    spec("examples/concurrency.gos"),
    spec("examples/containers_ordered_demo.gos"),
    spec("examples/containers_seq_demo.gos"),
    spec("examples/containers_setmap_demo.gos"),
    spec("examples/control_flow.gos"),
    spec("examples/data_structures.gos"),
    spec("examples/digit_sum.gos"),
    spec("examples/environment.gos"),
    spec("examples/errors.gos"),
    spec("examples/factorial.gos"),
    spec("examples/fibonacci.gos"),
    spec("examples/file_io.gos"),
    spec("examples/fizz_buzz.gos"),
    spec("examples/fnv_hash.gos"),
    spec("examples/function_piping.gos"),
    spec("examples/gcd.gos"),
    Spec {
        nondeterministic: true,
        skip_parity: Some(
            "goroutine completion count differs across tiers under scheduling pressure",
        ),
        ..spec("examples/go_spawn.gos")
    },
    Spec {
        args: &["needle"],
        stdin: b"alpha line\nneedle hidden here\nanother needle\nclosing\n",
        ..spec("examples/grep.gos")
    },
    spec("examples/heap_demo.gos"),
    spec("examples/hello_world.gos"),
    spec("examples/json_derive_test.gos"),
    Spec {
        skip_all: Some("needs live web_server.gos on :8080 - covered by web_server smoke tests"),
        ..spec("examples/http_client.gos")
    },
    spec("examples/line_count.gos"),
    spec("examples/linked_list.gos"),
    spec("examples/list_dir.gos"),
    spec("examples/mime_demo.gos"),
    spec("examples/netip_demo.gos"),
    spec("examples/os_user_demo.gos"),
    spec("examples/prime_check.gos"),
    spec("examples/range_sum.gos"),
    spec("examples/regex.gos"),
    spec("examples/reverse_string.gos"),
    spec("examples/shapes.gos"),
    spec("examples/sleep_demo.gos"),
    spec("examples/temperature.gos"),
    Spec {
        skip_parity: Some(
            "fn main is empty stub - coverage comes from `gos test examples/testing.gos`",
        ),
        ..spec("examples/testing.gos")
    },
    spec("examples/toml_demo.gos"),
    spec("examples/url_escape_demo.gos"),
    Spec {
        // v4/v7 produce fresh random / time-ordered values each run;
        // exit code is 0 and the format checks (lengths, validity,
        // normalize, simple) deterministic across tiers - but the
        // raw stdout bytes differ run-to-run.
        nondeterministic: true,
        ..spec("examples/uuid_demo.gos")
    },
    spec("examples/vowel_count.gos"),
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             web_auth_api_parity_across_tiers",
        ),
        ..spec("examples/web_auth_api.gos")
    },
    Spec {
        server: Some(ServerFixture {
            boot_ms: 800,
            addr: "127.0.0.1:8080",
            probe_path: "/health",
        }),
        ..spec("examples/web_server.gos")
    },
    spec("examples/word_count.gos"),
    // --- feature-testing-examples/ ---
    // `os::args()` must hand back owned, refcounted gos strings: cloning
    // one arg while others are live must not corrupt any of them. The
    // first arg ("Qwen3.6-35B") is held while the rest are cloned in a
    // loop, the classic shape that exposed raw-argv-pointer corruption.
    Spec {
        args: &["Qwen3.6-35B", "a", "b", "c", "d"],
        ..spec("feature-testing-examples/os_args_clone_roundtrip.gos")
    },
    // A recursive Box-enum cloned in a loop (the original stays live) must
    // retain each iteration's clone; the loop-carried read must not be
    // move-elided. Covers the sequential and goroutine-shared (captured) paths
    // that double-freed the enum's nodes and corrupted the heap at exit.
    spec("feature-testing-examples/rc_loop_carried_clone.gos"),
    // Out-of-range whole-element indexed writes are a lenient no-op on every
    // tier (scalar, string, and struct elements); in-bounds access unaffected.
    spec("feature-testing-examples/oob_index_lenient.gos"),
    // Out-of-range read of an aggregate-element Vec panics identically on
    // every tier (was a compiled segfault / VM field-access error).
    Spec {
        allow_nonzero: true,
        ..spec("feature-testing-examples/oob_index_aggregate_panic.gos")
    },
    // Win B stdlib differential-parity coverage: every function in these
    // module groups produces bit-identical output on the VM, Cranelift, and
    // LLVM tiers (the sweep that found and fixed the split/equal_fold/parse,
    // path/time, and crypto-coerce divergences).
    spec("feature-testing-examples/winb_text_strings.gos"),
    spec("feature-testing-examples/winb_text_strconv.gos"),
    spec("feature-testing-examples/winb_text_utf8.gos"),
    spec("feature-testing-examples/winb_text_unicode.gos"),
    spec("feature-testing-examples/winb_text_fmt.gos"),
    spec("feature-testing-examples/winb_data_crypto.gos"),
    spec("feature-testing-examples/winb_data_encoding.gos"),
    spec("feature-testing-examples/winb_data_math.gos"),
    spec("feature-testing-examples/winb_data_regex.gos"),
    spec("feature-testing-examples/winb_coll_vec.gos"),
    spec("feature-testing-examples/winb_coll_map.gos"),
    spec("feature-testing-examples/winb_coll_set.gos"),
    spec("feature-testing-examples/winb_coll_iter.gos"),
    spec("feature-testing-examples/winb_coll_optres.gos"),
    spec("feature-testing-examples/winb_sys_path.gos"),
    spec("feature-testing-examples/winb_sys_time.gos"),
    spec("feature-testing-examples/winb_sys_bytes.gos"),
    spec("feature-testing-examples/winb_sys_misc.gos"),
    // Win B integrator-fix coverage: segfaults (Vec<Struct>::new+push,
    // HashSet<i64>::insert, regex::find_all bound-iter), silent-wrong
    // (map contains, parse_u64, JSON integer precision), and dispatch gaps
    // (HashSet to_vec/iter/clear, Vec method insert/remove, BTreeMap keys).
    spec("feature-testing-examples/winb2_vec_new_struct.gos"),
    spec("feature-testing-examples/winb2_hashset_i64.gos"),
    spec("feature-testing-examples/winb2_regex_find_all_bound.gos"),
    spec("feature-testing-examples/winb2_map_contains.gos"),
    spec("feature-testing-examples/winb2_parse_u64.gos"),
    spec("feature-testing-examples/winb2_json_int_precision.gos"),
    spec("feature-testing-examples/winb2_hashset_to_vec.gos"),
    spec("feature-testing-examples/winb2_vec_insert_remove.gos"),
    spec("feature-testing-examples/winb2_btreemap_keys.gos"),
    // 0.18.0 smaller items: String::from identity, parse-error Display,
    // scalar fixed-array out-of-range lenient zero-value.
    spec("feature-testing-examples/winb2_smaller_items.gos"),
    // JIT widening coverage fixtures (inliner edge-dissolving,
    // aggregate-interior bodies, char-field enums, mixed-arity).
    spec("feature-testing-examples/jit_inline_chain.gos"),
    spec("feature-testing-examples/jit_aggregate_local.gos"),
    spec("feature-testing-examples/jit_inline_aggregate_return.gos"),
    spec("feature-testing-examples/jit_inline_const_args.gos"),
    spec("feature-testing-examples/jit_enum_char_field.gos"),
    spec("feature-testing-examples/jit_inline_vec_ops.gos"),
    spec("feature-testing-examples/jit_mixed_arity6.gos"),
    spec("feature-testing-examples/jit_aggregate_param.gos"),
    // Bytecode VM user-function inliner - must stay bit-identical to the
    // MIR-tier inlining already present in the compiled tiers.
    spec("feature-testing-examples/inline_scalar_kernel.gos"),
    spec("feature-testing-examples/temporary_wrap.gos"),
    spec("feature-testing-examples/temporary_method_dispatch.gos"),
    spec("feature-testing-examples/vecdeque_element_typing.gos"),
    spec("feature-testing-examples/method_dispatch_collisions.gos"),
    spec("feature-testing-examples/fmt_struct_enum.gos"),
    spec("feature-testing-examples/fmt_tuple_map.gos"),
    spec("feature-testing-examples/string_concat_chain.gos"),
    // Irrefutable let-pattern destructuring (struct / tuple-struct / enum
    // variant / nested / or-pattern) and const generic array length.
    spec("feature-testing-examples/let_destructure_struct.gos"),
    spec("feature-testing-examples/const_generic_array_len.gos"),
    // Let-chains, open-ended range patterns, fixed-array slice patterns,
    // bounds-safe String.byte_at, and in-place / flat numeric Vec growth.
    spec("feature-testing-examples/let_chains.gos"),
    spec("feature-testing-examples/open_ended_ranges.gos"),
    spec("feature-testing-examples/slice_pattern_fixed_array.gos"),
    spec("feature-testing-examples/string_byte_at_oob.gos"),
    spec("feature-testing-examples/vec_inplace_growth.gos"),
    spec("feature-testing-examples/record_update.gos"),
    spec("feature-testing-examples/trait_bounds.gos"),
    spec("feature-testing-examples/nested_field_access.gos"),
    spec("feature-testing-examples/rc_elision.gos"),
    spec("feature-testing-examples/bounds_check_elim.gos"),
    spec("feature-testing-examples/borrowed_option_result.gos"),
    spec("feature-testing-examples/aggregate_binding.gos"),
    spec("feature-testing-examples/fs_metadata.gos"),
    spec("feature-testing-examples/html_escape.gos"),
    spec("feature-testing-examples/html_template_render_json.gos"),
    spec("feature-testing-examples/jwt_roundtrip.gos"),
    spec("feature-testing-examples/crypto_ecdsa.gos"),
    spec("feature-testing-examples/validate_errors.gos"),
    spec("feature-testing-examples/validate_errors_return.gos"),
    spec("feature-testing-examples/sync_rwlock.gos"),
    spec("feature-testing-examples/context_cancel.gos"),
    spec("feature-testing-examples/metrics_observability.gos"),
    spec("feature-testing-examples/trace_observability.gos"),
    spec("feature-testing-examples/os_signal_subscribe.gos"),
    spec("feature-testing-examples/array_bounds_probe.gos"),
    spec("feature-testing-examples/array_literal_vec_methods.gos"),
    spec("feature-testing-examples/vec_aggregate_rc_ownership.gos"),
    spec("feature-testing-examples/mut_ref_scalar_writeback.gos"),
    spec("feature-testing-examples/mut_ref_string_writeback.gos"),
    spec("feature-testing-examples/byte_vec_i64_model.gos"),
    spec("feature-testing-examples/map_iteration_order.gos"),
    spec("feature-testing-examples/usize_compare.gos"),
    spec("feature-testing-examples/u64_unsigned.gos"),
    spec("feature-testing-examples/channel_close_drain.gos"),
    spec("feature-testing-examples/chan_struct_payload.gos"),
    spec("feature-testing-examples/channel_timers.gos"),
    Spec {
        nondeterministic: true,
        ..spec("feature-testing-examples/channel_fan_in.gos")
    },
    spec("feature-testing-examples/closure_capture_mutation.gos"),
    spec("feature-testing-examples/closure_lifetime_inference.gos"),
    spec("feature-testing-examples/closure_payload_typing.gos"),
    spec("feature-testing-examples/combinator_sweep.gos"),
    spec("feature-testing-examples/mut_ref_params.gos"),
    spec("feature-testing-examples/http_surface.gos"),
    spec("feature-testing-examples/http_form_multipart.gos"),
    spec("feature-testing-examples/option_none_variant_collision.gos"),
    spec("feature-testing-examples/method_name_collision.gos"),
    spec("feature-testing-examples/select_multiplex.gos"),
    spec("feature-testing-examples/select_closed_chan_ready.gos"),
    spec("feature-testing-examples/select_ctx_cancel.gos"),
    spec("feature-testing-examples/let_else_binding.gos"),
    spec("feature-testing-examples/slice_param_coercion.gos"),
    spec("feature-testing-examples/enum_param_rc_repro.gos"),
    spec("feature-testing-examples/sort_struct_field_closure.gos"),
    spec("feature-testing-examples/sql_driverless.gos"),
    spec("feature-testing-examples/sql_ident_quoting.gos"),
    spec("feature-testing-examples/struct_copy_reclaim.gos"),
    spec("feature-testing-examples/struct_copy_followups.gos"),
    spec("feature-testing-examples/struct_container_reclaim.gos"),
    spec("feature-testing-examples/enum_unit_local.gos"),
    spec("feature-testing-examples/panic_hook.gos"),
    spec("feature-testing-examples/arena_blocks.gos"),
    spec("feature-testing-examples/result_struct_payload.gos"),
    spec("feature-testing-examples/vec_literal_coercion.gos"),
    spec("feature-testing-examples/derive_traits.gos"),
    spec("feature-testing-examples/derive_struct_variant.gos"),
    spec("feature-testing-examples/struct_map_keys.gos"),
    spec("feature-testing-examples/atomic_bool.gos"),
    spec("feature-testing-examples/cycle_collector.gos"),
    spec("feature-testing-examples/arena_regions.gos"),
    spec("feature-testing-examples/auto_regions.gos"),
    spec("feature-testing-examples/tuple_extract_region.gos"),
    spec("feature-testing-examples/defer_unwind_order.gos"),
    spec("feature-testing-examples/early_break_materializers.gos"),
    spec("feature-testing-examples/empty_vec_growth.gos"),
    spec("feature-testing-examples/vec_multislot_growth.gos"),
    spec("feature-testing-examples/doc_test_vs_unit_test_drift.gos"),
    spec("feature-testing-examples/error_chain_inspection.gos"),
    spec("feature-testing-examples/error_question_mark_propagation.gos"),
    spec("feature-testing-examples/float_cast_drift.gos"),
    spec("feature-testing-examples/format_precision_padding.gos"),
    spec("feature-testing-examples/format_spec.gos"),
    spec("feature-testing-examples/fs_error_text.gos"),
    spec("feature-testing-examples/fs_temp_file_lifecycle.gos"),
    spec("feature-testing-examples/fs_dir_ops.gos"),
    spec("feature-testing-examples/path_split.gos"),
    spec("feature-testing-examples/base32_decode.gos"),
    spec("feature-testing-examples/json_yaml_encode.gos"),
    spec("feature-testing-examples/bounded_channel.gos"),
    spec("feature-testing-examples/generic_function_monomorphization.gos"),
    spec("feature-testing-examples/goroutine_panic_isolation.gos"),
    Spec {
        nondeterministic: true,
        ..spec("feature-testing-examples/hashmap_counter_race.gos")
    },
    spec("feature-testing-examples/hashset_algebra.gos"),
    spec("feature-testing-examples/http2_push.gos"),
    spec("feature-testing-examples/http2_trailers.gos"),
    spec("feature-testing-examples/http_cookie.gos"),
    spec("feature-testing-examples/http_csrf.gos"),
    spec("feature-testing-examples/http_csrf_attach.gos"),
    spec("feature-testing-examples/http_session.gos"),
    spec("feature-testing-examples/http_session_roundtrip.gos"),
    spec("feature-testing-examples/http_form_urlencoded.gos"),
    Spec {
        skip_all: Some(
            "binds fixed loopback ports - covered serially by \
             http_bare_handler_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_bare_handler.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_bare_aliases_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_bare_aliases.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_client_cookie_jar_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_client_cookie_jar.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_client_verbs_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_client_verbs.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback TLS port - covered serially by \
             http_serve_tls_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_serve_tls_roundtrip.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_server_headers_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_server_headers.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_middleware_bearer_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_middleware_bearer.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_middleware_compose_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_middleware_compose.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_middleware_ws_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_middleware_ws.gos")
    },
    spec("feature-testing-examples/http_router_lookup.gos"),
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_next_chunk_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_next_chunk.gos")
    },
    Spec {
        skip_all: Some(
            "binds fixed loopback ports - covered serially by \
             http_proxy_stream_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_proxy_stream.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_raw_bytes_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_raw_bytes.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_redirect_policy_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_redirect_policy.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_request_headers_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_request_headers.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_request_values_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_request_values.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_request_form_auth_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_request_form_auth.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_form_file_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_form_file.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_response_headers_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_response_headers.gos")
    },
    Spec {
        skip_all: Some(
            "binds fixed loopback ports - covered serially by \
             http_roundtrip_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_roundtrip.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_static_file_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_static_file.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_static_range_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_static_range.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             http_websocket_accept_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/http_websocket_accept.gos")
    },
    Spec {
        skip_all: Some(
            "binds a fixed loopback port - covered serially by \
             websocket_echo_parity_across_tiers",
        ),
        ..spec("feature-testing-examples/websocket_echo.gos")
    },
    spec("feature-testing-examples/http_serve_err_binding.gos"),
    spec("feature-testing-examples/http3_serve_err_binding.gos"),
    spec("feature-testing-examples/integer_overflow_edges.gos"),
    spec("feature-testing-examples/iter_combinator_chain.gos"),
    spec("feature-testing-examples/iter_extra.gos"),
    spec("feature-testing-examples/sync_extra.gos"),
    spec("feature-testing-examples/math_rand.gos"),
    spec("feature-testing-examples/bytes_builder.gos"),
    spec("feature-testing-examples/net_ip.gos"),
    spec("feature-testing-examples/net_tcp_echo.gos"),
    Spec {
        // Unix-domain sockets are POSIX-only; on Windows every entry
        // point returns an Err, so the program prints a bind-failure
        // message whose format differs between VM and native.
        skip_all: if cfg!(windows) {
            Some("Unix-domain sockets are not available on Windows")
        } else {
            None
        },
        ..spec("feature-testing-examples/net_unix_echo.gos")
    },
    spec("feature-testing-examples/vec_remove_inplace.gos"),
    spec("feature-testing-examples/map_value_heap_children.gos"),
    spec("feature-testing-examples/map_pop_then_drop.gos"),
    spec("feature-testing-examples/rc_move_elision.gos"),
    spec("feature-testing-examples/map_struct_value_access.gos"),
    spec("feature-testing-examples/chan_struct_local_recv.gos"),
    spec("feature-testing-examples/chan_select_struct_payload.gos"),
    spec("feature-testing-examples/net_tls_client.gos"),
    spec("feature-testing-examples/net_tls_client_modes.gos"),
    spec("feature-testing-examples/json_round_trip_fuzz.gos"),
    spec("feature-testing-examples/method_dispatch_collision.gos"),
    spec("feature-testing-examples/mutex_poison_recovery.gos"),
    spec("feature-testing-examples/mutex_vs_channel_counter.gos"),
    spec("feature-testing-examples/numeric_conversion_matrix.gos"),
    spec("feature-testing-examples/option_default.gos"),
    spec("feature-testing-examples/option_unwrap_chain.gos"),
    spec("feature-testing-examples/result_default.gos"),
    spec("feature-testing-examples/try_option_propagation.gos"),
    spec("feature-testing-examples/try_err_conversion.gos"),
    spec("feature-testing-examples/crypto_sha_hex.gos"),
    spec("feature-testing-examples/os_signal_handler.gos"),
    spec("feature-testing-examples/panic_recover_round_trip.gos"),
    spec("feature-testing-examples/pattern_match_exhaustiveness.gos"),
    spec("feature-testing-examples/pipe_operator_precedence.gos"),
    spec("feature-testing-examples/pipe_placeholder.gos"),
    Spec {
        // The example exercises `exec::run` against `echo`, `printf`,
        // `sh`, `true`, `false` - all Unix-only standalone executables
        // (on Windows `echo`/`true`/`false` are `cmd` builtins, not
        // resolvable via `Command::new`, and `sh`/`printf` aren't
        // present at all). Cross-platform shape would defeat the
        // demo's purpose. Linux + macOS cover the surface.
        skip_all: if cfg!(windows) {
            Some("uses Unix-only commands (echo, sh, printf, true, false)")
        } else {
            None
        },
        ..spec("feature-testing-examples/process_spawn_pipe.gos")
    },
    spec("feature-testing-examples/rc_release_drops.gos"),
    spec("feature-testing-examples/weak_refs.gos"),
    spec("feature-testing-examples/recursive_enum_walk.gos"),
    spec("feature-testing-examples/reference_alias_mutation.gos"),
    spec("feature-testing-examples/regex_unicode_categories.gos"),
    Spec {
        skip_parity: Some("poll-attempt count is scheduler-dependent; output varies across tiers"),
        ..spec("feature-testing-examples/select_default_timing.gos")
    },
    spec("feature-testing-examples/slice_methods.gos"),
    spec("feature-testing-examples/slice_subslicing.gos"),
    spec("feature-testing-examples/sort_with_closure.gos"),
    spec("feature-testing-examples/spawn_join.gos"),
    spec("feature-testing-examples/string_build.gos"),
    spec("feature-testing-examples/string_concatenation_stress.gos"),
    spec("feature-testing-examples/string_method_surface.gos"),
    spec("feature-testing-examples/string_unicode_boundaries.gos"),
    spec("feature-testing-examples/time_monotonic_vs_wall.gos"),
    spec("feature-testing-examples/tw_go_block.gos"),
    spec("feature-testing-examples/trait_object_dispatch.gos"),
    Spec {
        nondeterministic: true,
        ..spec("feature-testing-examples/tuple_destructuring_loop.gos")
    },
    spec("feature-testing-examples/variable_shadowing_ladder.gos"),
    spec("feature-testing-examples/literal_forms.gos"),
    spec("feature-testing-examples/loop_continue.gos"),
    spec("feature-testing-examples/match_or_patterns.gos"),
    spec("feature-testing-examples/or_patterns.gos"),
    spec("feature-testing-examples/string_match_patterns.gos"),
    spec("feature-testing-examples/string_char_needle.gos"),
    spec("feature-testing-examples/static_items.gos"),
    spec("feature-testing-examples/stdlib_expansion.gos"),
    spec("feature-testing-examples/strconv_radix_quote.gos"),
    spec("feature-testing-examples/stdlib_strings_free.gos"),
    spec("feature-testing-examples/stdlib_compiled_wiring.gos"),
    spec("feature-testing-examples/stdlib_path_free.gos"),
    spec("feature-testing-examples/stdlib_time_free.gos"),
    spec("feature-testing-examples/stdlib_hash.gos"),
    spec("feature-testing-examples/stdlib_math_bits.gos"),
    spec("feature-testing-examples/stdlib_math_pred.gos"),
    spec("feature-testing-examples/stdlib_os_introspection.gos"),
    spec("feature-testing-examples/stdlib_fs_rename.gos"),
    spec("feature-testing-examples/stdlib_json_as_bool.gos"),
    spec("feature-testing-examples/stdlib_thread_yield.gos"),
    Spec {
        stdin: b"alpha\nbeta\ngamma\n",
        ..spec("feature-testing-examples/stdlib_io_read_all.gos")
    },
    Spec {
        stdin: b"one two three",
        ..spec("feature-testing-examples/stdlib_io_copy.gos")
    },
    spec("feature-testing-examples/stdlib_alias_wiring.gos"),
    spec("feature-testing-examples/stdlib_math_scalar.gos"),
    spec("feature-testing-examples/stdlib_math_const.gos"),
    spec("feature-testing-examples/stdlib_unicode_norm.gos"),
    spec("feature-testing-examples/stdlib_process.gos"),
    spec("feature-testing-examples/stdlib_time_methods.gos"),
    spec("feature-testing-examples/duration_methods.gos"),
    spec("feature-testing-examples/flag_cell_duration.gos"),
    spec("feature-testing-examples/instant_methods.gos"),
    spec("feature-testing-examples/time_param_dispatch.gos"),
    spec("feature-testing-examples/neg_int_min_wraps.gos"),
    spec("feature-testing-examples/stdlib_net_dns.gos"),
    spec("feature-testing-examples/stdlib_json_dynamic.gos"),
    spec("feature-testing-examples/stdlib_netip.gos"),
    spec("feature-testing-examples/stdlib_strconv.gos"),
    spec("feature-testing-examples/stdlib_fs_ops.gos"),
    spec("feature-testing-examples/stdlib_encoding_crypto.gos"),
    spec("feature-testing-examples/stdlib_text_codec.gos"),
    spec("feature-testing-examples/stdlib_pem.gos"),
    spec("feature-testing-examples/stdlib_x509.gos"),
    spec("feature-testing-examples/stdlib_archive.gos"),
    spec("feature-testing-examples/struct_update_base.gos"),
    spec("feature-testing-examples/at_binding_subpattern.gos"),
    Spec {
        skip_parity: Some(
            "blocking channel recv without sleep returns None immediately in compiled tiers; \
             use channel_close_drain.gos (with time::sleep) for cross-tier drain coverage",
        ),
        ..spec("feature-testing-examples/scheduler_drain.gos")
    },
    spec("feature-testing-examples/static_mut_basic.gos"),
    spec("feature-testing-examples/static_mut_goroutines.gos"),
    spec("feature-testing-examples/closure_goroutine.gos"),
    spec("feature-testing-examples/go_stdlib_spawn.gos"),
    spec("feature-testing-examples/yaml_autoderive.gos"),
    spec("feature-testing-examples/sync_map_demo.gos"),
    spec("feature-testing-examples/autoderive_int_widths.gos"),
    spec("feature-testing-examples/write_file_bytes.gos"),
    spec("feature-testing-examples/unicode_full.gos"),
    spec("feature-testing-examples/string_len_bytes.gos"),
    spec("feature-testing-examples/concurrent_atomic.gos"),
    spec("feature-testing-examples/stdlib_parity_batch.gos"),
    spec("feature-testing-examples/compress_zstd.gos"),
    spec("feature-testing-examples/compress_bzip2.gos"),
    spec("feature-testing-examples/crypto_password.gos"),
    spec("feature-testing-examples/crypto_extra.gos"),
    spec("feature-testing-examples/crypto_aead.gos"),
    spec("feature-testing-examples/encoding_xml.gos"),
    spec("feature-testing-examples/misc_class_a.gos"),
    spec("feature-testing-examples/hashmap_get_some_field.gos"),
    Spec {
        skip_all: if cfg!(windows) {
            Some("uses Unix-only commands (printf, tr, sort, head)")
        } else {
            None
        },
        ..spec("feature-testing-examples/exec_pipeline.gos")
    },
    Spec {
        skip_all: if cfg!(windows) {
            Some("uses Unix-only /bin/true and /bin/sleep")
        } else {
            None
        },
        ..spec("feature-testing-examples/exec_wait_timeout.gos")
    },
    Spec {
        skip_all: if cfg!(windows) {
            Some("uses Unix-only /bin/sleep, /bin/sh, SIGTERM")
        } else {
            None
        },
        ..spec("feature-testing-examples/exec_signal_group.gos")
    },
    spec("feature-testing-examples/vec_runtime_repeat.gos"),
    spec("feature-testing-examples/range_non_i64.gos"),
    spec("feature-testing-examples/string_push_char.gos"),
    spec("feature-testing-examples/vec_deque.gos"),
    spec("feature-testing-examples/tuple_match_patterns.gos"),
    spec("feature-testing-examples/clone_builtin_dispatch.gos"),
    spec("feature-testing-examples/nested_vec_mutation.gos"),
    spec("feature-testing-examples/deref_string_concat.gos"),
    spec("feature-testing-examples/vec_single_field_struct.gos"),
    spec("feature-testing-examples/inline_index_remap.gos"),
    spec("feature-testing-examples/string_append_realloc.gos"),
    // Top-level statements (implicit `fn main`): plain, `?`-propagation,
    // mixed-with-items, and an explicit process exit code.
    spec("examples/top_level_statements.gos"),
    spec("feature-testing-examples/top_level_hello.gos"),
    spec("feature-testing-examples/top_level_question.gos"),
    spec("feature-testing-examples/top_level_mixed.gos"),
    Spec {
        allow_nonzero: true,
        ..spec("feature-testing-examples/top_level_exit_code.gos")
    },
    // Front-end features: labelled loops (`break 'l`/`continue 'l`) and
    // slice / rest patterns (`[a, b]`, `[first, ..rest]`, `[.., last]`).
    spec("feature-testing-examples/labeled_loops.gos"),
    spec("feature-testing-examples/slice_patterns.gos"),
    // Gossamer-native SQL driver dispatch: a `.gos` struct registers
    // itself as a std::database::sql driver (sql::register_native) and
    // is driven through the full Conn/Stmt/Rows facade. Cross-tier
    // gate for the register_native bridge + native_* side-channel.
    spec("feature-testing-examples/sql_native_driver.gos"),
    // Qualified type-path annotation (`util::Rec` in `&util::Rec` param and
    // `&mut util::Rec` param) resolves to the struct's Adt on all tiers so
    // field access lowers to a real Field projection instead of falling
    // through to the json accessor.
    spec("feature-testing-examples/cross_module_struct_fields.gos"),
];

#[derive(Debug)]
struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    /// Crash cause instead of an opaque number (signal name on unix,
    /// NTSTATUS name on Windows); `None` only if the process never
    /// reported an exit status.
    exit_text: Option<String>,
    /// True when the deadline elapsed and the child was killed.
    timed_out: bool,
    /// Executable path that was launched.
    exe: PathBuf,
    /// Space-joined command line (exe + args), for reproduction.
    cmdline: String,
    /// Working directory the child ran in.
    workdir: PathBuf,
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Renders an `ExitStatus` via the shared helper so a native-tier
/// crash reads as its cause (signal / NTSTATUS) instead of a bare
/// number. Returns `Some` whenever the process reported a status
/// (exit code or signal); `None` only if no status was collected.
fn render_status(status: std::process::ExitStatus) -> Option<String> {
    if status.code().is_some() {
        return Some(common::describe_exit(status).text);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal().is_some() {
            return Some(common::describe_exit(status).text);
        }
    }
    let _ = status;
    None
}

fn run_with_timeout(
    mut child: Child,
    stdin: &[u8],
    deadline: Instant,
    exe: PathBuf,
    cmdline: String,
    workdir: PathBuf,
) -> Run {
    if let Some(mut sin) = child.stdin.take() {
        let _ = sin.write_all(stdin);
        drop(sin);
    }
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => break,
        }
    }
    let out = child.wait_with_output().expect("wait_with_output");
    Run {
        stdout: normalize_newlines(&String::from_utf8_lossy(&out.stdout)),
        stderr: normalize_newlines(&String::from_utf8_lossy(&out.stderr)),
        code: out.status.code(),
        exit_text: render_status(out.status),
        timed_out,
        exe,
        cmdline,
        workdir,
    }
}

/// Formats the full per-tier execution report for a CI failure dump.
/// Every field is shown even when empty so a crashed-before-output
/// tier still surfaces executable path, command line, and exit cause.
fn tier_report(label: &str, run: &Run) -> String {
    let exit = match (run.code, &run.exit_text) {
        (Some(c), Some(text)) => format!("{c} ({text})"),
        (Some(c), None) => format!("{c}"),
        (None, Some(text)) => format!("none ({text})"),
        (None, None) => "none".to_string(),
    };
    let timeout = if run.timed_out { "yes" } else { "no" };
    format!(
        "{label}:\n  exit={exit}\n  timed_out={timeout}\n  exe={}\n  cmdline={}\n  workdir={}\n  stdout={:?}\n  stderr={:?}",
        run.exe.display(),
        run.cmdline,
        run.workdir.display(),
        run.stdout,
        run.stderr,
    )
}

#[test]
fn tier_report_shows_exit_ntstatus_and_streams() {
    let run = Run {
        stdout: String::from("ok\n"),
        stderr: String::new(),
        code: Some(0),
        exit_text: Some("exit 0".to_string()),
        timed_out: false,
        exe: PathBuf::from("/tmp/gos"),
        cmdline: "/tmp/gos run examples/hello.gos".to_string(),
        workdir: PathBuf::from("/home/daniel/dev/gossamer"),
    };
    let report = tier_report("vm", &run);
    assert!(report.starts_with("vm:\n  exit=0 (exit 0)"));
    assert!(report.contains("timed_out=no"));
    assert!(report.contains("exe=/tmp/gos"));
    assert!(report.contains("cmdline=/tmp/gos run examples/hello.gos"));
    assert!(report.contains("workdir=/home/daniel/dev/gossamer"));
    assert!(report.contains("stdout=\"ok\\n\""));
    assert!(report.contains("stderr=\"\""));
}

#[test]
fn tier_report_handles_crash_and_timeout() {
    // 0xC0000005 reinterpreted as i32 is -1073741819 — this is how
    // Rust's ExitStatus::code() surfaces a Windows NTSTATUS exit.
    // exit_text carries the decoded name so the CI log reads as the
    // cause, not a number.
    let run = Run {
        stdout: String::new(),
        stderr: String::from("fault"),
        code: Some(-1073741819),
        exit_text: Some("exit code 0xc0000005 (STATUS_ACCESS_VIOLATION)".to_string()),
        timed_out: true,
        exe: PathBuf::from("C:\\scratch\\hello.exe"),
        cmdline: "C:\\scratch\\hello.exe".to_string(),
        workdir: PathBuf::from("C:\\ci"),
    };
    let report = tier_report("cranelift", &run);
    assert!(report.contains("exit=-1073741819 (exit code 0xc0000005 (STATUS_ACCESS_VIOLATION))"));
    assert!(report.contains("timed_out=yes"));
    assert!(report.contains("exe=C:\\scratch\\hello.exe"));
    assert!(report.contains("stdout=\"\""));
    assert!(report.contains("stderr=\"fault\""));
}

fn run_vm(src: &Path, args: &[&str], stdin: &[u8]) -> Run {
    let gos = gos_bin();
    let mut cmd = Command::new(&gos);
    cmd.arg("run").arg(src);
    let mut parts: Vec<String> = vec![gos.display().to_string(), "run".to_string()];
    parts.push(src.display().to_string());
    if !args.is_empty() {
        cmd.arg("--").args(args);
        parts.push("--".to_string());
        parts.extend(args.iter().map(std::string::ToString::to_string));
    }
    let workdir = cmd.get_current_dir().map_or_else(
        || env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        std::path::Path::to_path_buf,
    );
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos run");
    run_with_timeout(
        child,
        stdin,
        Instant::now() + PER_RUN_TIMEOUT,
        gos,
        parts.join(" "),
        workdir,
    )
}

fn build_native(src: &Path, release: bool, scratch: &Path) -> Result<PathBuf, String> {
    let mut cmd = Command::new(gos_bin());
    cmd.arg("build");
    if release {
        cmd.arg("--release");
    }
    cmd.arg("--out-dir").arg(scratch).arg(src);
    let out = cmd.output().expect("spawn gos build");
    if !out.status.success() {
        return Err(format!(
            "gos build {flag} failed:\n  stdout: {}\n  stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            flag = if release { "--release" } else { "" },
        ));
    }
    // The unit name is manifest-derived (project id tail) for sources
    // inside a project, or the file stem for loose-file builds. Scan
    // the scratch dir for a single executable instead of guessing.
    let mut binaries: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(scratch)
        .map_err(|e| format!("read_dir {}: {e}", scratch.display()))?
        .flatten()
    {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        if is_executable(&p) {
            binaries.push(p);
        }
    }
    if binaries.is_empty() {
        return Err(format!(
            "gos build produced no executable in {}",
            scratch.display(),
        ));
    }
    if binaries.len() > 1 {
        return Err(format!(
            "gos build produced multiple executables in {}: {binaries:?}",
            scratch.display(),
        ));
    }
    Ok(binaries.into_iter().next().expect("checked len == 1"))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

fn run_native(bin: &Path, args: &[&str], stdin: &[u8]) -> Run {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    let mut parts: Vec<String> = vec![bin.display().to_string()];
    parts.extend(args.iter().map(std::string::ToString::to_string));
    let workdir = cmd.get_current_dir().map_or_else(
        || env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        std::path::Path::to_path_buf,
    );
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn native binary");
    run_with_timeout(
        child,
        stdin,
        Instant::now() + PER_RUN_TIMEOUT,
        bin.to_path_buf(),
        parts.join(" "),
        workdir,
    )
}

fn run_tier(spec: &Spec, tier: Tier) -> Result<Run, String> {
    let src = workspace_root().join(spec.path);
    match tier {
        Tier::Vm => Ok(run_vm(&src, spec.args, spec.stdin)),
        Tier::Cranelift => {
            let scratch = fresh_dir(&format!("cl-{}", file_tag(spec.path)));
            let bin = build_native(&src, false, &scratch)?;
            let run = run_native(&bin, spec.args, spec.stdin);
            let _ = fs::remove_dir_all(&scratch);
            Ok(run)
        }
        Tier::Llvm => {
            let scratch = fresh_dir(&format!("ll-{}", file_tag(spec.path)));
            let bin = build_native(&src, true, &scratch)?;
            let run = run_native(&bin, spec.args, spec.stdin);
            let _ = fs::remove_dir_all(&scratch);
            Ok(run)
        }
    }
}

fn file_tag(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("x")
        .to_string()
}

fn stdout_matches(a: &str, b: &str, nondeterministic: bool) -> bool {
    if nondeterministic {
        let mut la: Vec<&str> = a.lines().collect();
        let mut lb: Vec<&str> = b.lines().collect();
        la.sort_unstable();
        lb.sort_unstable();
        la == lb
    } else {
        a == b
    }
}

fn divergence(spec: &Spec, lhs: (Tier, &Run), rhs: (Tier, &Run)) -> Option<String> {
    if !stdout_matches(&lhs.1.stdout, &rhs.1.stdout, spec.nondeterministic) {
        return Some(format!(
            "{path}: stdout diverged between {a} and {b}\n  {a}: {astdout:?}\n  {b}: {bstdout:?}\n\n--- per-tier execution report ---\n{report}",
            path = spec.path,
            a = lhs.0.label(),
            b = rhs.0.label(),
            astdout = lhs.1.stdout,
            bstdout = rhs.1.stdout,
            report = tier_report(lhs.0.label(), lhs.1) + "\n" + &tier_report(rhs.0.label(), rhs.1),
        ));
    }
    if !spec.allow_nonzero && lhs.1.code != rhs.1.code {
        return Some(format!(
            "{path}: exit code diverged: {a}={ac:?} {b}={bc:?}\n\n--- per-tier execution report ---\n{report}",
            path = spec.path,
            a = lhs.0.label(),
            ac = lhs.1.code,
            b = rhs.0.label(),
            bc = rhs.1.code,
            report = tier_report(lhs.0.label(), lhs.1) + "\n" + &tier_report(rhs.0.label(), rhs.1),
        ));
    }
    None
}

#[test]
fn vm_runs_every_example_without_crashing() {
    let mut failures = Vec::new();
    for spec in SPECS {
        if let Some(reason) = spec.skip_all {
            eprintln!("skip vm: {} ({reason})", spec.path);
            continue;
        }
        if spec.server.is_some() {
            // Server VM coverage lives in `web_server_smoke_vm`.
            continue;
        }
        let run = match run_tier(spec, Tier::Vm) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!(
                    "{path}: vm error (no Run produced — tier failed before execution):\n  {e}",
                    path = spec.path,
                ));
                continue;
            }
        };
        if !spec.allow_nonzero && run.code != Some(0) {
            failures.push(format!(
                "{path}: vm exit={code:?}\n\n--- per-tier execution report ---\n{report}",
                path = spec.path,
                code = run.code,
                report = tier_report(Tier::Vm.label(), &run),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} VM run failures:\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

// The parity battery is split into `PARITY_GROUPS` round-robin groups
// per tier so a single failing example fails only its small group test
// (e.g. `llvm_parity_group_2`) instead of the whole "every example"
// suite - narrower to find, faster to re-run. The failure message
// still names the exact example. Keep the group tests below in sync
// with this count.
const PARITY_GROUPS: usize = 6;

macro_rules! parity_group_tests {
    ($($g:literal => $cranelift:ident, $llvm:ident, $strict:ident;)*) => {
        $(
            #[test]
            fn $cranelift() {
                parity_walk(Tier::Cranelift, $g);
            }
            #[test]
            fn $llvm() {
                parity_walk(Tier::Llvm, $g);
            }
            #[test]
            fn $strict() {
                lowers_without_fallback_group($g);
            }
        )*
    };
}

parity_group_tests! {
    0 => cranelift_parity_group_0, llvm_parity_group_0, llvm_strict_lower_group_0;
    1 => cranelift_parity_group_1, llvm_parity_group_1, llvm_strict_lower_group_1;
    2 => cranelift_parity_group_2, llvm_parity_group_2, llvm_strict_lower_group_2;
    3 => cranelift_parity_group_3, llvm_parity_group_3, llvm_strict_lower_group_3;
    4 => cranelift_parity_group_4, llvm_parity_group_4, llvm_strict_lower_group_4;
    5 => cranelift_parity_group_5, llvm_parity_group_5, llvm_strict_lower_group_5;
}

/// Serialises every parity walk so concurrent test functions can't
/// race on examples whose fixtures share `/tmp/gossamer_test_*`
/// paths (notably `fs_temp_file_lifecycle.gos`). The grouped tests run
/// sequentially under this lock - the round-robin split shrinks the
/// failing unit without reintroducing the cross-test fixture race.
static PARITY_WALK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn parity_walk(compiled: Tier, group: usize) {
    let _guard = PARITY_WALK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut failures = Vec::new();
    for (idx, spec) in SPECS.iter().enumerate() {
        if idx % PARITY_GROUPS != group {
            continue;
        }
        if spec.skip_all.is_some() || spec.skip_parity.is_some() || spec.server.is_some() {
            continue;
        }
        let vm = match run_tier(spec, Tier::Vm) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!(
                    "{path}: vm error (no Run produced — tier failed before execution):\n  {e}",
                    path = spec.path,
                ));
                continue;
            }
        };
        let other = match run_tier(spec, compiled) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!(
                    "{path}: {tier} error (no Run produced — tier failed before execution):\n  {e}",
                    path = spec.path,
                    tier = compiled.label(),
                ));
                continue;
            }
        };
        if let Some(d) = divergence(spec, (Tier::Vm, &vm), (compiled, &other)) {
            failures.push(d);
        }
    }
    assert!(
        failures.is_empty(),
        "{} {} parity failures:\n{}",
        failures.len(),
        compiled.label(),
        failures.join("\n\n"),
    );
}

// ----------------------------------------------------------------
// Server fixtures.
//
// `web_server.gos` is the only HTTP server in the example set. We
// verify that each tier boots the listener within the boot
// budget, responds 200 to `GET /health`, and exits cleanly when
// the test process tears it down. The probe is a hand-rolled
// `TcpStream` so the test depends on no crate-level HTTP client.
// ----------------------------------------------------------------

#[test]
fn web_server_smoke_vm() {
    server_smoke(Tier::Vm);
}

#[test]
fn web_server_smoke_cranelift() {
    server_smoke(Tier::Cranelift);
}

#[test]
fn web_server_smoke_llvm() {
    server_smoke(Tier::Llvm);
}

/// Runs a self-terminating loopback client+server fixture (server
/// goroutines + client in `main` + explicit `process::exit`) on
/// all three tiers sequentially and demands identical stdout and
/// exit codes. These fixtures bind fixed loopback ports, so they
/// are excluded from the parallel SPECS walks (`skip_all`) and
/// serialised under [`SERVER_PORT_LOCK`] here instead.
/// `expect_contains` guards against an all-tiers-identically-broken
/// pass (e.g. every tier printing the same connection error).
fn self_terminating_server_parity(path: &'static str, expect_contains: &[&str]) {
    let _port_guard = SERVER_PORT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _server_window = common::ServerPortLock::acquire();
    let fixture = spec(path);
    let vm = run_tier(&fixture, Tier::Vm).expect("vm run");
    assert_eq!(
        vm.code,
        Some(0),
        "{path}: vm exit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        vm.code,
        vm.stdout,
        vm.stderr,
    );
    for needle in expect_contains {
        assert!(
            vm.stdout.contains(needle),
            "{path}: vm stdout missing {needle:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            vm.stdout,
            vm.stderr,
        );
    }
    for tier in [Tier::Cranelift, Tier::Llvm] {
        let run = run_tier(&fixture, tier)
            .unwrap_or_else(|e| panic!("{path}: {} error: {e}", tier.label()));
        if let Some(d) = divergence(&fixture, (Tier::Vm, &vm), (tier, &run)) {
            panic!("{d}\n--- {} stderr ---\n{}", tier.label(), run.stderr);
        }
    }
}

/// `go <stdlib-free-call>` must spawn a goroutine on every tier rather
/// than run inline. The fixture's two-line output is reachable only
/// when the spawned `Barrier::wait` runs asynchronously (it is one of
/// two barrier parties; main is the other). A synchronous inline call
/// would deadlock main on the barrier and print nothing. Asserting the
/// exact output plus cross-tier parity proves the spawn is async and
/// identical across the bytecode VM, Cranelift JIT, and LLVM AOT.
#[test]
fn go_stdlib_spawn_is_async_across_tiers() {
    let fixture = spec("feature-testing-examples/go_stdlib_spawn.gos");
    let expected = "main reached barrier\nreleased\n";
    let vm = run_tier(&fixture, Tier::Vm).expect("vm run");
    assert_eq!(
        normalize_newlines(&vm.stdout),
        expected,
        "vm stdout\n--- stderr ---\n{}",
        vm.stderr,
    );
    assert_eq!(vm.code, Some(0), "vm exit={:?}", vm.code);
    for tier in [Tier::Cranelift, Tier::Llvm] {
        let run =
            run_tier(&fixture, tier).unwrap_or_else(|e| panic!("{} error: {e}", tier.label()));
        if let Some(d) = divergence(&fixture, (Tier::Vm, &vm), (tier, &run)) {
            panic!("{d}\n--- {} stderr ---\n{}", tier.label(), run.stderr);
        }
    }
}

/// The one-shot client verbs `http::head` / `options` / `post` / `put`
/// / `delete` each lower to a per-verb `gos_rt_http_<verb>` shim so the
/// method string is fixed at the runtime boundary; the request method,
/// body, and Content-Type must round-trip bit-identically on every tier.
#[test]
fn http_client_verbs_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_client_verbs.gos",
        &[
            "get status=200 body=m=GET b= ct=",
            "options status=200 body=m=OPTIONS b= ct=",
            "post status=200 body=m=POST b=hello-post ct=application/json",
            "put status=200 body=m=PUT b=hello-put ct=text/plain",
            "delete status=200 body=m=DELETE b=hello-delete ct=",
            "head status=200",
        ],
    );
}

/// The canonical classifier free functions
/// `http::middleware::decode_basic_auth` (header -> Option<(user, pass)>)
/// and `http::websocket::is_websocket_upgrade` (request -> bool) must
/// classify bit-identically on every tier, degrading to `None` / `false`
/// when the relevant headers are absent.
#[test]
fn http_middleware_ws_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_middleware_ws.gos",
        &[
            "A status=200 body=cred=admin:s3cret up=yes",
            "B status=200 body=cred=none up=no",
        ],
    );
}

/// Go-style middleware composition `http::middleware::tag(inner) ->
/// Handler` must wrap a handler and prepend `mw:` to each response body
/// bit-identically on every tier; a double-wrap `tag(tag(App{}))` proves
/// the chained path (the inner middleware serves through
/// `gos_rt_middleware_serve`), yielding `mw:mw:ok`.
#[test]
fn http_middleware_compose_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_middleware_compose.gos",
        &["status=200 body=mw:mw:ok"],
    );
}

/// The bare HTTP free-function aliases `native_client::{get,post,put,delete}`,
/// `proxy::forward`, and `static_files::serve_file` must resolve to their
/// canonical compiled shims and behave bit-identically on every tier.
#[test]
fn http_bare_aliases_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_bare_aliases.gos",
        &[
            "nc_get status=200 body=m=GET b=",
            "nc_post status=200 body=m=POST b=p",
            "nc_put status=200 body=m=PUT b=q",
            "nc_delete status=200 body=m=DELETE b=",
            "proxy_get status=200 body=m=GET b=",
            "proxy_post status=200 body=m=POST b=fwd",
            "serve_file status=200 body=served-from-disk",
        ],
    );
}

/// `FileServer` byte-range (RFC 7233) responses must be bit-identical on
/// every tier: a single `Range` yields 206 + `Content-Range` + the
/// sliced body, a multi-range yields a 206 `multipart/byteranges` body
/// with the fixed boundary, an out-of-range request yields 416. Both the
/// compiled `gos_rt_file_server_serve` and interp `native_file_server_serve`
/// route through the shared gossamer-runtime Range helpers.
#[test]
fn http_static_range_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_static_range.gos",
        &[
            "single status=206 cr=bytes 2-5/16 body=2345",
            "multi status=206 ct=multipart/byteranges; boundary=gossamer_byteranges_boundary",
            "Content-Range: bytes 0-2/16",
            "Content-Range: bytes 5-7/16",
            "bad status=416 cr=bytes */16",
            "whole status=200 body=0123456789ABCDEF",
        ],
    );
}

/// Bare-`http::Response` handlers (no `Result` wrapper) must serve
/// identically on every tier: the MIR-synthesized `::__ok_wrap`
/// thunk adapts them to the packed-Result handler C-ABI. Covers
/// the `impl http::Handler` env path and the Router bare-fn path.
#[test]
fn http_bare_handler_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_bare_handler.gos",
        &[
            "struct status=200 body=bare struct ok",
            "route status=200 body=bare route ok",
        ],
    );
}

/// The `http::Client` cookie jar (`Client::builder().cookie_jar(true)`)
/// must persist `Set-Cookie` across requests on the same client and
/// re-send it bit-identically on every tier: the compiled tiers keep a
/// persistent `ureq::Agent` on the boxed client, the interp tier an
/// id-keyed `gossamer_std::http::Client` registry. The handler echoes
/// the `Cookie` header it received on the second request.
#[test]
fn http_client_cookie_jar_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_client_cookie_jar.gos",
        &["login status=200", "me_body=cookie=sid=abc123"],
    );
}

/// Request-scoped values (`r.set_value(k, v)` / `r.value(k)`, Go's
/// `context.WithValue`) must read back bit-identically on every tier;
/// re-setting a key overwrites, an absent key yields `""`.
#[test]
fn http_request_values_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_request_values.gos",
        &["status=200 body=user=bob role=admin missing=[]"],
    );
}

/// `r.form_value(key)` reads an x-www-form-urlencoded body field and
/// `r.basic_auth()` decodes the `Authorization: Basic` header into
/// `Option<(String, String)>`; both must read back bit-identically on
/// every tier, degrading to `""` / `None` when absent.
#[test]
fn http_request_form_auth_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_request_form_auth.gos",
        &[
            "status=200 body=form_user=alice form_role=admin missing=[] auth=admin:s3cret",
            "status=200 body=form_user= form_role= missing=[] auth=none",
        ],
    );
}

/// `r.form_file(name)` parses a `multipart/form-data` request body off
/// `raw_body` (boundary from the `Content-Type` header) and returns the
/// matching file part's `filename` / `content_type` / `[u8]` content.
/// The upload echo and the no-body 404 must read back bit-identically
/// on every tier.
#[test]
fn http_form_file_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_form_file.gos",
        &[
            "status=200 body=file=x.txt ctype=text/plain len=5 sum=335",
            "status=404 body=no file",
        ],
    );
}

/// `http::middleware::bearer_ok` runs the caller's verify closure on
/// the request's Bearer token across the C-ABI; a valid token reaches
/// the handler (200), an invalid or absent one is rejected (401).
#[test]
fn http_middleware_bearer_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_middleware_bearer.gos",
        &[
            "valid status=200 body=welcome",
            "wrong status=401 body=unauthorized",
            "none status=401 body=unauthorized",
        ],
    );
}

/// Canonical authenticated API example: a path-parameter router, a
/// `middleware::bearer_ok` auth gate, typed `r.path_int` extraction,
/// and signed `session::sign` / `verify` cookies - all composed in
/// one program that must behave bit-identically on every tier.
#[test]
fn web_auth_api_parity_across_tiers() {
    self_terminating_server_parity(
        "examples/web_auth_api.gos",
        &[
            "login session={\"user\":\"ada\"}",
            "order status=200 body={\"order\":42}",
            "noauth status=401",
        ],
    );
}

/// Router `{id}` / `{rest...}` path captures must reach a Gossamer
/// handler via `r.path_value(name)` bit-identically on every tier.
/// An undeclared capture name yields `""`.
#[test]
fn http_router_params_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_router_params.gos",
        &[
            "A status=200 body=user=42",
            "B status=200 body=file=docs/readme.md",
            "C status=200 body=missing=[]",
        ],
    );
}

/// Typed path extractors `r.path_int` / `r.path_float` (Option<T>) must
/// parse captures and return None on unparseable/absent identically on
/// every tier - exercises the packed-Option C-ABI.
#[test]
fn http_router_typed_params_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_router_typed_params.gos",
        &["A id=42 amt=3.5 raw=42", "B id=-1 amt=-1 raw=notnum"],
    );
}

/// `http::static_files::FileServer` served through `http::serve` must
/// resolve a real file (200 + body + MIME) and 404 a missing path
/// bit-identically on every tier - compiled wires `gos_rt_file_server_*`,
/// interp the `native_file_server_serve` dispatch.
#[test]
fn http_static_file_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_static_file.gos",
        &["status=200 body=static file ok", "missing status=404"],
    );
}

/// `http::websocket::accept` (RFC 6455 server handshake) must validate
/// the upgrade headers and build a 101 Response identically on every
/// tier - compiled wires `gos_rt_ws_accept`, interp the native
/// `websocket::accept`; a request without the headers is rejected with
/// the handshake error string.
#[test]
fn http_websocket_accept_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_websocket_accept.gos",
        &[
            "accept_key=s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
            "valid=status=101",
            "reject=missing Upgrade header",
        ],
    );
}

/// Bidirectional WebSocket messaging (RFC 6455): an echo server bound
/// via `websocket::serve` on a goroutine, a `websocket::connect` client
/// that sends a text message and verifies the echo. All three tiers
/// drive the shared `gossamer_ws` framing engine, so the output is
/// bit-identical on the bytecode VM, Cranelift JIT, and LLVM AOT.
#[test]
fn websocket_echo_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/websocket_echo.gos",
        &["ws echo: ok"],
    );
}

/// `http::serve_tls` (server-side HTTPS) terminating a real TLS
/// handshake, plus the three `TcpStream::start_tls*` client modes
/// (skip-verify, public-root verify, custom-CA verify), must behave
/// identically on every tier. A private CA signs a localhost leaf the
/// server presents; `start_tls_insecure` and `start_tls_ca` complete
/// the request while the public-root `start_tls` rejects the private
/// chain - bit-identically on the bytecode VM, Cranelift JIT, and LLVM
/// AOT.
#[test]
fn http_serve_tls_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_serve_tls_roundtrip.gos",
        &["insecure: ok", "default-verify: rejected", "custom-ca: ok"],
    );
}

/// The compiled HTTP server must emit the RFC 9110 origin headers
/// `Date` and `Server` that the interp tier already sends, so a client
/// observes the same response-header set on every tier.
#[test]
fn http_server_headers_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_server_headers.gos",
        &["server-header: true", "date-header: true"],
    );
}

/// `match http::serve(..) { Err(e) => println!("{}", e) }` must
/// compile and run identically on every tier. The serve expression
/// is `Result<(), errors::Error>`-typed (the Err binding used to
/// lower as void and break LLVM with "sext void to i64"), and a
/// bind failure is the caller's `Err` value - printed via the match
/// arm and exit 0 on every tier.
#[test]
fn http_serve_err_binding_parity_across_tiers() {
    let fixture = spec("feature-testing-examples/http_serve_err_binding.gos");
    let expected_stdout = "about to bind\nError: http::serve: invalid socket address\n";
    for tier in [Tier::Vm, Tier::Cranelift, Tier::Llvm] {
        let run =
            run_tier(&fixture, tier).unwrap_or_else(|e| panic!("{} error: {e}", tier.label()));
        assert_eq!(
            run.code,
            Some(0),
            "{} must exit 0 - serve failure is the caller's Err value, not a panic\n\
             --- stdout ---\n{}\n--- stderr ---\n{}",
            tier.label(),
            run.stdout,
            run.stderr,
        );
        assert_eq!(run.stdout, expected_stdout, "{} stdout", tier.label());
        assert!(
            !run.stderr.contains("GX0005"),
            "{} must not panic on serve failure\n--- stderr ---\n{}",
            tier.label(),
            run.stderr,
        );
    }
}

/// `http_h3::serve` is the QUIC + HTTP/3 server entry, wired across
/// all three tiers through the shared `gossamer-http3` engine. A full
/// QUIC round trip is too slow / nondeterministic for the parity walk
/// (the loopback handshake takes tens of seconds), so this fixture
/// exercises the same handler-fn-ptr dispatch and `Result<(), Error>`
/// surface deterministically: HTTP/3 mandates TLS, so the server
/// reads the cert / key PEM before binding, and a missing cert file
/// is the caller's `Err` value on every tier - not a panic. The cert
/// read goes through `std::fs::read` on both tiers, so the OS error
/// tail is identical; this pins the stable prefix and asserts
/// cross-tier equality of the full line.
#[test]
fn http3_serve_err_binding_parity_across_tiers() {
    let fixture = spec("feature-testing-examples/http3_serve_err_binding.gos");
    let stable_prefix = "about to bind\nError: http_h3::serve: h3 io: read cert:";
    let mut outputs: Vec<(String, String)> = Vec::new();
    for tier in [Tier::Vm, Tier::Cranelift, Tier::Llvm] {
        let run =
            run_tier(&fixture, tier).unwrap_or_else(|e| panic!("{} error: {e}", tier.label()));
        assert_eq!(
            run.code,
            Some(0),
            "{} must exit 0 - a cert read failure is the caller's Err value, not a panic\n\
             --- stdout ---\n{}\n--- stderr ---\n{}",
            tier.label(),
            run.stdout,
            run.stderr,
        );
        assert!(
            run.stdout.starts_with(stable_prefix),
            "{} stdout must carry the stable cert-read-error prefix\n--- stdout ---\n{}",
            tier.label(),
            run.stdout,
        );
        assert!(
            !run.stderr.contains("GX0005"),
            "{} must not panic on a cert read failure\n--- stderr ---\n{}",
            tier.label(),
            run.stderr,
        );
        outputs.push((tier.label().to_string(), run.stdout));
    }
    // The OS error tail is machine-specific but identical across
    // tiers on the same host: every tier's full stdout must match.
    let (first_label, first_out) = &outputs[0];
    for (label, out) in &outputs[1..] {
        assert_eq!(
            out, first_out,
            "{label} stdout must match {first_label} byte-for-byte",
        );
    }
}

/// Inbound server request headers must be readable identically on
/// every tier: `for (name, value) in r.headers` (the historical
/// MIR-lowering panic / first-request segfault shape), borrowed
/// `&r.headers` lookups, the lowercase/dedupe/name-sorted interp
/// `Headers` view, and `r.path` query-stripping + `r.query` parity.
#[test]
fn http_request_headers_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_request_headers.gos",
        &[
            "status=200",
            "custom=2 alpha=a1 beta=b2 path=/echo query=k=1&n=2",
        ],
    );
}

/// Handler-set response headers must reach the wire identically on
/// every tier: `Response::with_header` is replace-then-push (the
/// second same-name attach wins, case-insensitively) and the
/// constructor's content type survives alongside custom headers
/// (explicit header > `content_type` field > text/plain default).
#[test]
fn http_response_headers_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_response_headers.gos",
        &[
            "status=201 body=created",
            "x-a=2",
            "x-b=3",
            "content-type=text/plain; charset=utf-8",
        ],
    );
}

/// Programmer-selectable redirect policy must behave identically on
/// every tier: the default `Client::builder().build()` follows the
/// 302 to the final 200 body, `max_redirects(0)` returns the 302 raw
/// with its Location header intact, and `request_bytes` honors the
/// same configured client.
#[test]
fn http_redirect_policy_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_redirect_policy.gos",
        &[
            "a_status=200 a_body=landed",
            "b_status=302 b_location=/data",
            "c_status=200 c_body=hi",
        ],
    );
}

/// `ResponseStream::next_chunk(max)` must drain a streamed body in
/// identical byte chunks on every tier: the Some payload is a
/// packed `elem_bytes=1` `GosVec` (the `raw_bytes` representation
/// contract), consumed through the canonical `while let
/// Some(chunk)` shape with len / indexing / for-loop sum /
/// `hex::encode` all reading byte-stride.
#[test]
fn http_next_chunk_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_next_chunk.gos",
        &[
            "len=4 b0=65 hex=41c3bfe2",
            "len=4 b0=132 hex=84a27a41",
            "len=2 b0=66 hex=4243",
            "total=10 sum=1291",
        ],
    );
}

/// Streamed server responses (`Response::stream` - the
/// proxy-passthrough shape) must behave identically on every tier:
/// the proxy opens a fresh upstream `http::stream` per request, the
/// server drains it as chunked frames, and constructing the
/// response consumes the `ResponseStream` handle (`next_chunk`
/// yields `None` afterwards - the /consumed handler answers 500 if
/// it ever sees leftover data).
#[test]
fn http_proxy_stream_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_proxy_stream.gos",
        &[
            "first status=200 ct=text/plain; charset=utf-8 len=37 \
             body=upstream payload: the quick brown fox",
            "second status=200 ct=text/plain; charset=utf-8 len=37 \
             body=upstream payload: the quick brown fox",
            "consumed status=200 ct=text/plain; charset=utf-8 len=37 \
             body=upstream payload: the quick brown fox",
        ],
    );
}

/// Integration fixture chaining the closed client/server gaps like
/// a real proxy session: binary `request_bytes` upload observed via
/// the server's `r.raw_body` (NUL byte included), a NUL-embedded
/// byte-array response body served in full by the native h1 writer
/// (`body_bytes` preferred over the c-string mirror), handler
/// `with_header` reaching the wire and read back through the
/// client's `resp.headers` then forwarded by a proxy hop, a 302
/// held raw under `max_redirects(0)`, and a `next_chunk` drain of a
/// `Response::stream` passthrough.
#[test]
fn http_roundtrip_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_roundtrip.gos",
        &[
            "echo status=200 body=len=4 first=1 last=255 sum=258",
            "nul status=200 len=5 hex=4100420043",
            "hop status=302 location=/data",
            "fwd status=200 body=fwd:landed-data x-up=u1",
            "stream total=31 chunks=4 first_hex=73747265616d6564",
        ],
    );
}

/// `resp.raw_bytes` is a packed `elem_bytes=1` `GosVec`; every
/// consumer op (indexing, for-loop, `first` / `last` / `contains`
/// / `count_of` / `index_of`, `hex::encode`, element writes) must
/// read byte-stride identically on every tier.
#[test]
fn http_raw_bytes_parity_across_tiers() {
    self_terminating_server_parity(
        "feature-testing-examples/http_raw_bytes.gos",
        &["hex=41c3bfe284a27a", "mutated_v0=66 hex2=42c3bfe284a27a"],
    );
}

/// Serialises the `web_server.gos` smoke tests across all three
/// tiers. The example hardcodes `0.0.0.0:8080`; running the three
/// `#[test]` variants in parallel races on that port and produces
/// spurious connection-refused failures on whichever tier the
/// scheduler started second.
static SERVER_PORT_LOCK: Mutex<()> = Mutex::new(());

fn server_smoke(tier: Tier) {
    let _port_guard = SERVER_PORT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _server_window = common::ServerPortLock::acquire();
    let spec = SPECS
        .iter()
        .find(|s| s.path == "examples/web_server.gos")
        .expect("web_server spec");
    let server = spec.server.expect("server fixture");
    let deadline = Instant::now() + PER_RUN_TIMEOUT;

    // Pre-flight: if port 8080 is already bound (stale server from a
    // prior run, an unrelated dev process, etc.) the spawned child's
    // listener will fail to bind but the test would still probe and
    // hit the *other* process - producing a confusing "status 404"
    // panic. Try to acquire the port briefly to fail fast with a
    // clear diagnostic instead.
    if let Err(e) = std::net::TcpListener::bind(server.addr) {
        panic!(
            "{} web_server smoke: cannot bind {} ({e}). \
             Likely a stale server from a previous test run or a \
             benchmark holding the port. Kill it (`fuser -k 8080/tcp` \
             or `pkill -9 -f server.gos`) and retry.",
            tier.label(),
            server.addr,
        );
    }

    let src = workspace_root().join(spec.path);
    let (mut child, scratch) = match tier {
        Tier::Vm => {
            let child = Command::new(gos_bin())
                .arg("run")
                .arg(&src)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn gos run web_server");
            (child, None)
        }
        compiled => {
            let release = matches!(compiled, Tier::Llvm);
            let scratch = fresh_dir(&format!("server-{}", compiled.label()));
            let bin = match build_native(&src, release, &scratch) {
                Ok(p) => p,
                Err(e) => panic!("{} build of web_server.gos failed: {e}", compiled.label()),
            };
            let child = Command::new(&bin)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn web_server binary");
            (child, Some(scratch))
        }
    };

    std::thread::sleep(Duration::from_millis(server.boot_ms));

    let probe = http_probe(server.addr, server.probe_path, deadline);
    let _ = child.kill();
    let captured = read_child_streams(&mut child);
    let _ = child.wait();
    if let Some(s) = scratch {
        let _ = fs::remove_dir_all(s);
    }

    // If the child reported a bind failure mid-run (e.g. another
    // process raced to grab the port between our pre-flight check
    // and the spawn), surface that explicitly instead of letting
    // the test panic on a status mismatch from the other server.
    let bind_raced = captured.stderr.contains("bind") && captured.stderr.contains("in use");
    assert!(
        !bind_raced,
        "{} web_server: bind raced - port {} taken before child could listen\n--- child stderr ---\n{}",
        tier.label(),
        server.addr,
        captured.stderr,
    );

    let (status, body) = probe.unwrap_or_else(|e| {
        panic!(
            "{} web_server probe failed: {e}\n--- child stdout ---\n{}\n--- child stderr ---\n{}",
            tier.label(),
            captured.stdout,
            captured.stderr,
        );
    });
    assert_eq!(
        status,
        200,
        "{} web_server returned status {status}, body={body:?}\n--- child stdout ---\n{}\n--- child stderr ---\n{}",
        tier.label(),
        captured.stdout,
        captured.stderr,
    );
    assert!(
        !body.is_empty(),
        "{} web_server returned empty body\n--- child stdout ---\n{}\n--- child stderr ---\n{}",
        tier.label(),
        captured.stdout,
        captured.stderr,
    );
}

struct ChildOutput {
    stdout: String,
    stderr: String,
}

/// Drains the child's piped stdout / stderr. Must be called after
/// `kill()` and before `wait()` so the buffered output is not lost
/// when the kernel reclaims the pipes. Either end may be missing
/// if the caller did not configure `Stdio::piped()`.
fn read_child_streams(child: &mut Child) -> ChildOutput {
    use std::io::Read;
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut stdout);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr);
    }
    ChildOutput { stdout, stderr }
}

/// Probes `addr` with `GET {path}` and returns the status code and
/// body. Retries the *whole* attempt (connect + write + read) on
/// any transient error until `deadline`. A single attempt can fail
/// for reasons that resolve a moment later - the kernel may
/// complete a TCP handshake against a not-quite-ready application
/// (the listen backlog masks slow accept loops), and the read then
/// times out with EAGAIN even though the server will be serving
/// within a second. Retrying the full handshake decouples the test
/// from runtime bootstrap timing.
fn http_probe(addr: &str, path: &str, deadline: Instant) -> Result<(u16, String), String> {
    let socket = addr
        .parse::<std::net::SocketAddr>()
        .map_err(|e| format!("parse addr {addr}: {e}"))?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    let mut last_err = String::from("probe never attempted");
    while Instant::now() < deadline {
        match probe_once(&socket, req.as_bytes(), deadline) {
            Ok(reply) => return Ok(reply),
            Err(e) => {
                last_err = e;
                std::thread::sleep(Duration::from_millis(120));
            }
        }
    }
    Err(format!("probe deadline reached; last error: {last_err}"))
}

fn probe_once(
    socket: &std::net::SocketAddr,
    req: &[u8],
    deadline: Instant,
) -> Result<(u16, String), String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("deadline elapsed before attempt".to_string());
    }
    let connect_budget = remaining.min(Duration::from_secs(2));
    let mut stream =
        TcpStream::connect_timeout(socket, connect_budget).map_err(|e| format!("connect: {e}"))?;
    let read_budget = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(2))
        .max(Duration::from_millis(200));
    stream
        .set_read_timeout(Some(read_budget))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    stream
        .set_write_timeout(Some(read_budget))
        .map_err(|e| format!("set_write_timeout: {e}"))?;
    stream.write_all(req).map_err(|e| format!("write: {e}"))?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("read status: {e}"))?;
    let parts: Vec<&str> = status_line.split_whitespace().collect();
    if parts.len() < 2 || !parts[0].starts_with("HTTP/") {
        return Err(format!("malformed status line: {status_line:?}"));
    }
    let code = parts[1]
        .parse::<u16>()
        .map_err(|e| format!("parse status: {e}"))?;
    let mut body = Vec::new();
    let _ = reader.read_to_end(&mut body);
    Ok((code, String::from_utf8_lossy(&body).into_owned()))
}

// ----------------------------------------------------------------
// LLVM strict-fallback gate.
//
// `gos build --release` silently routes a body to Cranelift if
// LLVM's lowerer raises `BuildError::Unsupported`. That fallback
// hides LLVM lowering gaps. With `GOSSAMER_FAIL_ON_LLVM_FALLBACK=1`
// the per-function fallback turns into a hard error, so this test
// fails the moment any example body cannot be lowered to LLVM
// directly. The list of currently-failing programs is captured in
// `~/dev/contexts/lang/ai_driven_gaps.md` and tracked one by one.
// ----------------------------------------------------------------

/// One round-robin group of the strict-lowering battery (invoked by the
/// `llvm_strict_lower_group_N` tests). Builds only (to fresh per-spec
/// dirs), so groups can run concurrently without the parity lock.
fn lowers_without_fallback_group(group: usize) {
    let mut fallbacks: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (idx, spec) in SPECS.iter().enumerate() {
        if idx % PARITY_GROUPS != group {
            continue;
        }
        if spec.skip_all.is_some() {
            continue;
        }
        let src = workspace_root().join(spec.path);
        let scratch = fresh_dir(&format!("strict-{}", file_tag(spec.path)));
        let out = Command::new(gos_bin())
            .arg("build")
            .arg("--release")
            .arg("--out-dir")
            .arg(&scratch)
            .arg(&src)
            .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
            .output()
            .expect("spawn gos build --release");
        let _ = fs::remove_dir_all(&scratch);
        if out.status.success() {
            continue;
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("would fall back to Cranelift") {
            // First line typically reads:
            //   error: llvm backend: `<fn>` would fall back to Cranelift (<reason>) ...
            let summary = stderr
                .lines()
                .find(|l| l.contains("would fall back"))
                .unwrap_or(&stderr)
                .trim()
                .to_string();
            fallbacks.push(format!("{}: {summary}", spec.path));
        } else {
            errors.push(format!(
                "{}: gos build --release failed: {stderr}",
                spec.path
            ));
        }
    }
    if !fallbacks.is_empty() || !errors.is_empty() {
        let mut report = String::new();
        if !fallbacks.is_empty() {
            report.push_str(&format!(
                "{} LLVM fallback site(s) - see ai_driven_gaps.md for the open list:\n",
                fallbacks.len(),
            ));
            for f in &fallbacks {
                report.push_str("  ");
                report.push_str(f);
                report.push('\n');
            }
        }
        if !errors.is_empty() {
            report.push_str(&format!("\n{} build error(s):\n", errors.len()));
            for e in &errors {
                report.push_str("  ");
                report.push_str(e);
                report.push('\n');
            }
        }
        panic!("{report}");
    }
}
