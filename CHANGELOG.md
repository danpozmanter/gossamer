# Changelog

## 0.8.0 — Unicode, web stack, publish flow, LSP, fixes, and Rust-binding ergonomics

### Language

- Identifiers follow UAX #31 `XID_Start` / `XID_Continue` (same surface as Rust 2024) — `let café = 1`, `let π = 3.14`, `let 名前 = "x"` all parse.
- New `docs_src/language/` reference site (33 pages: `if_let`, `while_let`, `pipe`, patterns, traits, …) generated from the manifest registry.

### Stdlib — std::unicode

The hand-rolled ASCII / BMP-range stubs are gone; every predicate answers against the Unicode 16 tables via `unicode-properties`, `unicode-normalization`, and `unicode-segmentation`.

- General-category predicates now correct for non-ASCII: `is_digit('٧')` (Arabic-Indic), `is_punct('—')` (em dash), `is_symbol('¥')`, `is_mark('\u{0301}')`, `is_number('Ⅴ')`, `is_title('ǅ')`.
- Added `is_assigned(r) -> bool` and `combining_class(r) -> i64`.
- Added whole-string casing helpers: `to_upper_str(s)` (ß → SS), `to_lower_str(s)` (Σ → σ), `fold_case(s)`.
- Added normalization: `nfc(s)`, `nfd(s)`, `nfkc(s)`, `nfkd(s)`, plus `is_nfc` / `is_nfd` / `is_nfkc` / `is_nfkd`.
- Added segmentation (UAX #29): `graphemes(s) -> Vec<String>`, `grapheme_count(s) -> i64`, `words(s)`, `word_bounds(s)`, `word_count(s)`, `sentences(s)`, `sentence_count(s)`. Family ZWJ sequences count as one grapheme; `cafe\u{0301}` is four.

### Stdlib — std::utf8

- `full_rune_in_string(s) -> bool` exposed alongside the existing byte-slice `full_rune`.

### Stdlib — HTTP server stack

- `std::http::cookie` — RFC 6265 `Cookie` / `CookieBuilder`, `SameSite` enum, `parse_cookie_header`, `parse_set_cookie`.
- `std::http::csrf` — double-submit cookie + Origin/Referer check: `issue_token`, `verify_token`, `extract_token`, `origin_allowed`, `check`, `attach_cookie`, `RouteAuth`.
- `std::http::form` — `application/x-www-form-urlencoded` parser and builder.
- `std::http::multipart` — streaming RFC 7578 parser: `parse_boundary`, `parse_bytes`, `parse<R: Read>`, with `Part` / `PartData` / `Form` types.
- `std::http::query` — typed `Query` wrapper over URL query strings.
- `std::http::session` — signed-cookie session store: `SessionConfig`, `Session`, `SessionStore` trait, `SignedCookieStore`, `with_session`.
- `std::http::state` — `AppState` typemap + `State<T>(Arc<T>)` DI container for handlers.
- `std::http::health` — `Probe` trait + `Health` aggregator with `always_ok` / `always_fail` / `tcp_probe` helpers.

### Stdlib — HTTP middleware

`std::http::middleware` gained `body_limit`, `timeout`, `hsts`, `security_headers`, `cache_control`, `etag`, `bearer_auth`, `rate_limit`, `compress_gzip`, and a `safe_defaults` bundle — alongside the existing `logger`, `recoverer`, `request_id`, `cors`, and `basic_auth`.

### Stdlib — HTTP/2

- Server push: `PushOptions`, `PushStream`, `ResponseWriter::push_promise`.
- Trailers: `ResponseWriter::write_trailers`, `Request::trailers`, `Trailers` alias.

### Stdlib — std::process / std::exec

- `Pipeline` for stdout→stdin chaining: `pipeline_run`.
- `Signal` enum + `spawn`, `kill`, `signal(pid, sig)`, `kill_group(pgid, sig)`, `wait_timeout(child, ms)`.

### Stdlib — new modules

- `std::jwt` — RFC 7519 sign + verify for HS256/384/512, ES256, and EdDSA; `Alg`, `Header`, `Claims`, `VerifyOpts`.
- `std::lifecycle` — graceful-shutdown hooks, signal handling, sd_notify.
- `std::validate` — `Validate` trait plus `FieldError` / `Errors` for form/field validation.
- `std::crypto::password` — Argon2id facade: `hash`, `verify`, `needs_rehash` (PHC strings).

### Package manager

- `gos publish` / `yank` / `login` / `logout` / `owner` — full registry workflow.
- Credential store (`~/.config/gossamer/credentials.toml`): `CredentialStore`, `Credential`, `load_default`, `get`, `insert`, `remove`.
- Ed25519 publish keys: `load_publish_key`, `sign_bytes`, `verify_bytes`.
- `pack_crate`, `upload_with`, `yank_with`, `owner_op_with` round out the publish pipeline.
- Transitive resolution: `CatalogueEntry`, `resolve_transitive`, `CacheBackedLoader`, `FnLoader`, `NoopLoader`.
- Disk-backed source cache under `default_cache_root()` (digest-keyed).
- Tarball + Git + registry sources with sha256 verification; `tarball_sha256` recorded in `LockedEntry`.
- `[rust-bindings.<name>] src = "bindings/x.rs"` — single-file binding; `gos` auto-scaffolds a wrapper crate under `.gos-bindings/__srcwrap-<name>/` with an optional `deps = "..."` Cargo-deps fragment.
- `[rust-bindings.<name>] prebuilt = "lib/x.a", abi = "1.0"` — pre-built static archive for hermetic / no-cargo-at-build-time deployments.

### LSP

- New request handlers: `textDocument/typeDefinition`, `references`, `documentHighlight`, `prepareRename`, `rename`, `inlayHint`, `documentSymbol`, `workspace/symbol`, `foldingRange`, `signatureHelp`, `formatting`, `codeAction`, `semanticTokens/full`.
- Cross-file `WorkspaceIndex` (`SymbolBucket` over Items / Variants / Fields / Methods, qualified `SymbolKey`) rebuilt incrementally on `didOpen` / `didChange`; powers references + rename across files.

### Rust bindings

- `gossamer-binding` ABI frozen at (1, 0). Wire shapes (`GosVec`, `GosVariant`, `GosVariantValue`, `GosTuple`, `GosBytes`, `BindingGosMap`, `GosDynVariant`, `GosCallback`) are stable; minor releases add shapes but never reorder fields.
- New `#[gos_module("path")]` proc-macro attribute: replaces `register_module!`'s triple-declaration; auto-publishes `__bindings_force_link()` via `FORCE_LINK_FNS`; doc-comments flow through to `gos doc`.
- `register_module!` gains a `name: <ident>` short form with compile-time `SigType` validation per param + return.
- New `#[derive(GosStruct)]` for user structs (round-trips through `Value::Struct` / `GosDynVariant`).
- New `#[gos_opaque]` on `impl Type` blocks: each `pub fn` becomes a binding item named `Type::method`.
- New `#[gos_blocking]` attribute: dispatches the body through a blocking pool with inline fallback.
- Extended type vocabulary: `Option<T>`, `Result<T, String>`, `Result<T, GosError>` for common `T`; `HashMap<String, Vec<i64>>`, `<i64, String>`, `<String, bool>`, `<String, f64>`; tuples up to arity 4 with generic `SigType` / `FromGos` / `ToGos`.
- New `GosError` with `From` for `io::Error`, `ParseIntError`, `ParseFloatError`, `Utf8Error`, `FromUtf8Error`, `fmt::Error`, `SystemTimeError`, `Infallible`; propagates via `?` with full cause chain on render.
- New `PersistentCallback`: long-lived callable handle that survives past the binding return (complements the call-scoped `BindingCallback`).
- New `gossamer-binding-macros` proc-macro crate; re-exported transparently from `gossamer-binding`.

### CLI

- `gos test --coverage <path>` writes lcov reports; `--parallel N` / `--serial`, `--format junit`, `--tier-parity --report=status`.
- `gos feature-status` — list and check the feature-status registry: `--status shipped|experimental|planned|removed`, `--format table|json|markdown`, `--check` drift gate.
- New `std::manifest::feature_status` registry covers stdlib modules and `lang::*` entries; rendered docs gain a `Status:` marker per page.
- `gos new --template binding NAME` scaffolds a ready-to-edit binding crate.
- `gos bindgen INPUT --output DIR --module PATH` walks a Rust source file's `pub fn` surface, classifies each by ABI vocabulary support, and emits a ready-to-edit binding crate; unsupported items are flagged with their blocking type.

### Runtime

- Coverage: `runtime::coverage::{Counter, register, bump, record, snapshot, reset, set_enabled}` plus C-ABI shims `gos_rt_cov_record`, `_bump`, `_reset`, `_set_enabled`.
- Exec C-ABI shims: `gos_rt_exec_pipeline_run`, `_signal`, `_kill_group`, `_wait_timeout`.
- Unicode C-ABI: 37 `gos_rt_unicode_*` shims (predicates, case, normalization, segmentation).
- UTF-8 C-ABI: 9 `gos_rt_utf8_*` shims (rune count, validity, boundaries).
- Vec/array slice helpers: `gos_rt_intarr_slice_result`, `gos_rt_floatarr_slice_result`, plus the existing string and generic Vec variants.
- Panic traces: per-goroutine call-stack tracker (`Frame`, `set_active_gid`, `stack_push` / `_pop` / `set_active_line`, `active_frames`, `render_active_panic_trace`); both backends emit prologue/return push+pop calls.

### Compiled tier

- Every new `std::unicode`, `std::utf8`, `std::exec`, and slice helper has a typed entry in the ABI registry and a dispatch arm in MIR `stdlib_free.rs` / `method_call_dispatch.rs`. Bit-identical output across VM, Cranelift, and LLVM tiers — verified by `feature-testing-examples/unicode_full.gos`, `slice_methods.gos`, `exec_pipeline.gos`, `exec_signal_group.gos`, `exec_wait_timeout.gos`, `http2_push.gos`, and `http2_trailers.gos` under `tier_parity`.
- Cranelift soft-zero fallback for unknown call names removed — unresolved calls are now a hard error (the `GOSSAMER_STRICT_LOWER` env var is retired).

### Fixes

- LLVM tier silent miscompile when `if let Some(p) = m.get(&k); p.field` was used for `HashMap<_, Struct>`: the dispatcher pinned the call's return type to bare `i64`, so the match arm couldn't recover `&V` from `Option<V>` and field projection fell through to `ptr`. New `gos_rt_map_get_i64_opt` / `gos_rt_map_get_str_opt` return `Option<V>` as a `*mut GosResult`, with `pinned_ret` synthesised from the receiver's HashMap value Ty. Side effect: `m.get(missing)` for `HashMap<_, i64>` now correctly returns `None` (previously the no-Adt happy-path encoded missing keys as `Some(0)`).
- LLVM tier stack-pointer bug on `HashMap.insert` with struct values: the inserted value was the stack address of the literal alloca, so subsequent reads in any other frame saw stale data. `maybe_heap_copy_aggregate` heap-copies the struct via `gos_rt_aggr_alloc` before passing to `gos_rt_map_insert_i64_i64` and `_str_i64`. The wrapper is marked `#[inline(never)]` plus a `#[used]` static anchor (`GOS_RT_AGGR_ALLOC_KEEP`) so neither the rustc inliner nor the linker's dead-strip collapses it back into `gos_rt_gc_alloc` — that collapse silently elides the heap copy and reintroduces the stack-pointer regression. Cross-tier parity verified by `feature-testing-examples/hashmap_get_some_field.gos` and the aether_ecs build benchmark, which now matches the interp tier bit-for-bit (`pos_x_sum=9990959.95`).
- LLVM tier GC blindness through `HashMap` values: `GosMap` allocations live outside the GC registry (`Box::into_raw` in `gos_rt_map_new`), and the conservative payload scan can't walk the Rust-side bucket allocator, so heap-allocated struct values stored as i64 entries were unreachable from the tracing collector and reclaimed mid-program. `gos_map_register` / `gos_map_deregister` track every live `GosMap` in a dedicated registry; `gos_rt_gc_collect` now adds a second mark drain that walks every registered map's storage and emits each value as a candidate pointer for the registry-presence check. The conservative trace tolerates raw i64 values in primitive maps (`HashMap<_, i64>`) — they don't match registered allocations so they don't over-mark.

### Tooling

- `check.sh` runs `gos feature-status --status experimental --check` to gate accidental drift.
- New CLI test surface: `feature_status.rs`, `http_h2_alpn.rs`, `http_h2_conformance.rs`; LSP `workspace_refs_rename.rs`; pkg `registry_publish.rs`.
- Fuzz harnesses (smoke + weekly) now cap inputs with `-rss_limit_mb=2048 -malloc_limit_mb=2048 -timeout=30` so a single pathological seed records a crash artefact instead of OOM-killing the runner.
- `gossamer-runtime::ffi::tests::opens_libc_and_calls_strlen` and the `gossamer-coro` test suite gated behind `#[cfg(not(miri))]` (`libloading::dlopen` and `corosensei`'s `mmap(PROT_NONE)` are unsupported by Miri); host-CPU runs still cover both.

### Cross-platform

- `std::signal` on Windows now bridges `SetConsoleCtrlHandler`: Ctrl+C → SIGINT, Ctrl+Break → SIGQUIT (+ goroutine-stack dump via `sigquit::render_to`), close / logoff / shutdown → SIGTERM. Previously `signal::on(SIGINT).wait()` deadlocked because nothing flipped the notifier flag.
- `std::lifecycle` Windows arm consumes those notifiers — `Lifecycle::install_default()` now runs registered shutdown hooks on Ctrl+C / supervisor close, mirroring the unix dispatcher's double-signal force-exit semantics.
- `find_clang_rt_profile` searches macOS Homebrew (`/opt/homebrew/opt/llvm@*`, `/usr/local/opt/llvm@*`, `darwin/libclang_rt.profile_osx.a`) and Windows MSYS2 (`C:\msys64\mingw64\lib\clang\*\lib\windows\clang_rt.profile-*.lib`) layouts; honours `$GOS_LLVM_PROFILE_RT` for explicit overrides and walks the `$GOS_LLVM_OPT` parent tree.
- `std::net::UnixListener` / `UnixStream` gain `#[cfg(not(unix))]` stub arms so `use std::net::UnixListener` resolves on Windows; every method returns `Err("AF_UNIX sockets are not supported on this platform")` until the real Win10+ AF_UNIX surface lands.
- `gossamer-std`'s `unicode-properties` / `unicode-normalization` / `unicode-segmentation` deps moved out of `[target.'cfg(unix)'.dependencies]` — they were used unconditionally by `std::unicode` and would have failed to resolve on a Windows build.

## 0.7.0 — Stdlib, stability, refactoring, and build optimizations

### Build

- Debug builds use a minimal opt pass set (`mem2reg`, `instcombine`, `simplifycfg`) instead of `-O1`; cuts `gos build` wall-clock time by 100–200 ms on typical programs.
- Release builds parallelize per-body `opt`+`llc` across up to 8 threads; wall-clock time falls roughly `(N-1)/N` on N-body programs.
- Incremental object cache under `~/.cache/gossamer/ir-cache` (or `GOS_BUILD_CACHE`); repeat builds reuse unchanged bodies. Disable with `GOS_NO_CACHE=1`.

### Performance

- `gos_rt_panic_oob`, `gos_rt_panic`, and `gos_rt_process_abort` declared `noreturn cold nounwind` in emitted LLVM IR; restores inner-loop vectorization that the 0.6.0 bounds-check pass had blocked.

### Stdlib — new modules

- `std::encoding::yaml::to_json` / `from_json` — YAML ↔ JSON text converters, mirroring `toml::to_json` / `from_json`.
- `std::sync::Map` — concurrent string-keyed string-value map: `new`, `set`, `get`, `delete`, `len`, `contains`, `keys`.

### Stdlib — string

All methods also available as `std::strings` free functions.

- `s.split_once(sep)` / `s.rsplit_once(sep)` → `Option<(String, String)>`
- `s.count(needle) -> i64` — non-overlapping occurrence count
- `s.strip_chars(cutset)` / `s.lstrip_chars(cutset)` / `s.rstrip_chars(cutset)`
- `s.zfill(width)` and `s.center(width, pad_char)`
- `s.slice(start, end) -> Result<String, errors::Error>` — non-panicking byte-range slice

### Stdlib — Vec / `[T]`

- `xs.contains(&v) -> bool`, `xs.index_of(&v) -> Option<i64>`, `xs.count_of(&v) -> i64`
- `xs.first() -> Option<T>` and `xs.last() -> Option<T>`
- `xs.reversed() -> Vec<T>` — non-mutating counterpart to the in-place `xs.reverse()`
- `xs.slice(start, end) -> Result<Vec<T>, errors::Error>`
- `Vec::insert(xs, i, v) -> Result<Vec<T>, errors::Error>` and `Vec::remove(xs, i) -> Result<T, errors::Error>` — safe qualified forms; the legacy method-call shape keeps its existing behaviour

### Stdlib — HashMap

- `m.keys() -> Vec<K>` and `m.values() -> Vec<V>`
- `HashMap::pop(m, k) -> Option<V>` — removes and returns the previous value

### Stdlib — scalar prelude

- `min(a, b)`, `max(a, b)`, `clamp(x, lo, hi)` — bare prelude functions for scalar pairs

### Stdlib — auto-derive

- Narrow integer fields (`i8`, `i16`, `i32`, `u8`, `u16`, `u32`, `f32`) now supported in `from_json` / `to_json` auto-derive; previously the entire struct was silently skipped.
- `from_yaml` / `to_yaml` auto-derived on every eligible struct alongside the existing JSON and TOML pairs.

### Stdlib — misc

- `flag::Cell<T>` auto-derefs at comparisons, function arguments, and typed register unboxes; `*flags.field` still works explicitly.
- `errors::newf(fmt, args…)` — format-shaped error constructor; rewritten at parse time to `errors::new(format!(fmt, args…))`.
- `http::Response.raw_bytes` — body as `Vec<u8>` for binary responses; compiled tier now matches the VM tier.
- `os::write_file(path, &Vec<u8>)` — binary-safe write preserving embedded NULs.
- `os::read_file(path) -> Result<Vec<u8>, errors::Error>` — raw bytes counterpart to `read_file_to_string`.

### Fixes

- LLVM `slot_count` for `http::Response` corrected to `None`; the previous inline-alloca layout truncated the heap pointer, causing segfaults in LLVM AOT builds when accessing `.body`.
- Resolver allows user-defined items to shadow prelude entries without collision.

## 0.6.0 — Stability hardening

### Safety

- `catch_unwind` at every `gos_rt_*` and JIT-call boundary — runtime
  panics no longer cross `extern "C"` as UB.
- Recoverable language panics (e.g. chan double-close) return a typed
  error instead of `process::abort`.
- `gos_rt_str_free` validates the allocator tag before freeing.
- No `process::abort` / `process::exit` outside sanctioned entries.

### Codegen

- Cranelift sign discipline: `coerce_arg_to` / `coerce_store_value`
  sign-extend by default; `Shr` dispatches `sshr` vs `ushr` from MIR
  operand type.
- Bounds checks on dynamic array indexing in both backends; opt out
  via `GOSSAMER_DISABLE_BOUNDS_CHECK`.
- Cranelift soft-zero fallback for unknown call names warns at
  compile time; `GOSSAMER_STRICT_LOWER=1` promotes to a hard error.
- LLVM IR verification (`opt -passes=verify`) runs before the
  optimisation pipeline.
- LLVM `gos_rt_*` declarations route through a single `declare_rt`;
  the synthesized-decl path is gone.
- Cranelift `Rvalue::Aggregate` allocates through `gos_rt_aggr_alloc`
  (GC-tracked) rather than raw `calloc`.

### Containers

- Typed `Vec<T>` allocation in both backends. `Vec<String>`,
  `Vec<Vec<_>>`, `Vec<HashMap<_,_>>` emit `gos_rt_vec_new_typed`
  with an element-kind tag.
- `gos_rt_vec_free` deep-frees STRING / VEC / MAP / ERROR element
  payloads via the elem-kind tag.
- `gos_rt_vec_push` clones inbound strings for STRING-typed vecs
  into the tagged allocator domain.

### IR validation

- MIR verifier gained 8 type-aware checks (call arity vs callee,
  return ty != Error, aggregate operand count, branch cond is bool,
  drop target is owning, unary-neg `i128::MIN`, switchint disc
  int/bool, call dest typed). Runs in `debug_assertions` at every
  pass boundary.
- Bytecode validator runs at `Vm::load` (PC bounds, register
  bounds, jump targets, constant-pool bounds).
- Conditional-init drop pass is now flow-sensitive (forward must-init
  dataflow); refuses uninit free path-sensitively.
- `i128::MIN` const-fold uses `checked_neg` (was overflow-panic).

### Frontend

- Recursion-depth cap (256) on parser, type-checker, and HIR lowerer
  with `GP0017` / `GT0008` diagnostics. Closes brace-bomb crashes.
- Parse-error nodes are typed: `ExprKind::Error` / `PatternKind::Error`
  replace silent `Literal::Unit` / `Wildcard` fallbacks.
- Integer-literal magnitude validation at typecheck (`GT0009`).
- `\u{...}` / `\x..` string escapes decoded with surrogate /
  ASCII-bound validation.

### Binding ABI

- `ABI_VERSION = (0, 6)` const plus `__gos_binding_abi_version`
  static the runtime sniffs at startup.
- Runtime `GosMap` and binding-side `BindingGosMap` layouts split;
  new `gos_rt_binding_map_free` for the binding struct.
- `gos_rt_callback_invoke` is a loud stub (eprintln + zero-fill of
  `result_out`); closes the silent-Err(-1) regression.

### Runtime

- `gos_rt_http_serve` bounded thread spawn: `GOSSAMER_HTTP_MAX_CONN`
  (default 4096); past the cap responds 503.
- VM `MAX_CALL_DEPTH = 512` in release (was 40).

### Tracing GC connected end-to-end

The compiled tier now has an active tracing collector.

- Raw-pointer aggregate registry (`gos_rt_gc_alloc` /
  `gos_rt_aggr_alloc`) backed by a `HashMap<usize, AllocEntry>`
  carrying `(size, mark, generation)`. Tracking is on by default;
  `GOS_GC=leak` opts out for benchmarks measuring raw-allocator cost.
- Stop-the-world conservative mark + sweep (`gos_rt_gc_collect`).
  Mark phase snapshots every thread's raw-pointer shadow stack and
  transitively traces each marked allocation's payload with
  pointer-sized validated word scans (alignment, bounds, and
  registry-presence checked per word). Sweep deallocates unmarked
  entries and bumps the registry generation so cross-thread races
  against a stale snapshot fail fast.
- Thread-local raw-pointer shadow stack with `gos_rt_gc_root_push`,
  `gos_rt_gc_root_save`, `gos_rt_gc_root_restore`. Stored as
  `usize` so `Send + Sync` is structural, not bespoke.
- Safepoints at function prologues and loop back-edges in both
  Cranelift and LLVM. A per-function MIR pre-scan identifies
  back-edge targets; codegen opens those blocks with
  `gos_rt_gc_safepoint()`. Atomic-load + compare in the common case;
  runs a full collect when `GOS_GC_THRESHOLD` (default 4 MiB) trips.
- Per-function root save/restore emitted at every prologue and every
  return in both backends. Aggregate-return heap copies push the
  returned pointer after the callee's restore so the root persists
  into the caller's frame.
- `Layout::from_size_align_unchecked` removed from the GC path; every
  allocation routes through a single validated helper that fails fast
  on overflow or bad alignment.
- Cycle reclamation proven by a runtime unit test plus two
  cross-tier stress regressions (10 000-iteration aggregate loops
  under `GOS_GC_THRESHOLD=4096` across VM, debug LLVM, and release
  LLVM). Spectral-norm at `N=5500` produces the bit-exact reference
  value `1.274224153` with the collector firing throughout.

### Tracing GC hardening

- `PtrKey` reduced to a `usize` newtype. Registry is structurally
  thread-safe; pointer dereference happens only after registry
  validation under the collect lock.
- `ThreadRoots.stack` stores `usize`; the marker is the sole code
  path that converts back to a pointer, only after re-validating
  the address against the registry.
- Generation counter on `AllocEntry` bumped at sweep so a pointer
  the marker observed in a stale shadow-stack snapshot can't be
  silently re-traced. Marker checks `(addr, generation)` together.
- Word scan replaces raw pointer arithmetic with `scan_payload_words`,
  which re-derives word count from the registry's authoritative size
  and uses `ptr::read_unaligned` defensively.
- Shadow-stack bounded growth: per-thread stack capped at
  `GOS_GC_SHADOW_MAX` (default 1 048 576); pushes past the cap
  trigger an immediate stop-the-world collect.
- `gos_rt_write_barrier_ptr(slot, new_val)` exposed as a runtime
  symbol (no-op under STW); reserves the ABI slot for a future
  concurrent-mark phase.
- `gos_rt_gc_assert_consistent()` debug-only registry walker wired
  into the STW collect path.
- Miri-clean GC unit tests (`cargo +nightly miri test -p
  gossamer-runtime --lib tracing_gc_tests`).
- Every `unsafe` block in the GC path carries a structured SAFETY
  comment (provenance, aliasing, synchronization, failure mode).

### Stdlib

- Auto-derived `<Type>::from_json(text)` / `<Type>::to_json(self)`
  on every user struct. Strict, one-line, serde-style
  (de)serialization built at `Vm::load` from the typechecker's
  field-type table. The decoder validates each field against its
  declared shape and rejects type mismatches and missing required
  fields with a path-qualified error (e.g.
  `User::from_json: field 'age': expected integer, got string`).
  Nested structs resolve by source name; `[T]` / `Vec<T>` /
  `[T; N]` / tuples / `Option<T>` / `HashMap<String, V>` walk
  recursively; `json::Value` fields pass through untouched.

### Cleanup

- Deleted dead interpreter modules (`peephole.rs`, `goroutine_pool.rs`).

### Tooling

- Toolchain locked to Rust 1.95.0 across the repo: `channel =
  "1.95.0"` and `profile = "minimal"` in `rust-toolchain.toml`,
  workspace MSRV bumped to 1.95, every CI `dtolnay/rust-toolchain`
  reference pinned to `@1.95.0`, the `rustup default stable` step
  in the shim-guard replaced with `rustup show` (idempotent,
  serial), and a `rustup set profile minimal` step inserted after
  every dtolnay install (including the nightly fuzz / miri /
  sanitizer jobs). The redundant MSRV CI job is gone.
  Without all three locks in place, the GitHub Actions runner
  images' user-default rustup profile is `complete`, so the
  rustup-shim invoked by cargo decides the project needs rust-src
  and races the parent + every nested `build.rs` cargo invocation
  to download it — one of them dies with `could not rename
  'downloaded' .partial` (Linux) or `detected conflict:
  rust-src Cargo.lock` (macOS / Windows, where the runner image
  has a partial rust-src dir from a previous stable build). The
  three locks make rustup stop deciding rust-src "should be
  there".
- Weekly fuzz + corpus-minimization jobs moved out of `fuzz.yml`
  into a separate `fuzz-weekly.yml`. The `if: github.event_name
  == 'schedule'` gate hid them on push / PR, but the GitHub UI
  still rendered each skipped job with its unexpanded
  `${{ matrix.target }}` placeholder. A schedule-only file is
  cleaner.
- `check.sh` fuzz loop covers all 10 targets (added `resolve`,
  `hir_lower`, `vm_run` — they were missing, which is how the
  CI build broke without local notice).
- CI runners standardised on `*-latest` (`macos-13` pin retired —
  retired runners stalled jobs in the queue).
- Adopted `clippy::duration_suboptimal_units` (new in 1.95);
  `Duration::from_secs(60)` rewritten to `from_mins(1)` across the
  tree.

### Fixes

- **Perf regression recovered.** The 0.6.0 GC work emitted a
  `gos_rt_gc_safepoint` call at every function prologue and every
  loop back-edge plus a `gos_rt_gc_root_save`/`_restore` pair around
  every function. The runtime calls are opaque to `opt -O3` and
  block inner-loop vectorisation; pure leaf math helpers (called
  > 10⁹ times in spectral-norm / n-body) paid the FFI cost on
  every invocation. The codegen now elides the prologue safepoint
  + shadow-stack save/restore when the body cannot allocate (new
  `gossamer_mir::body_might_allocate` helper) and drops the
  loop-back-edge safepoint outright — allocation routines update
  the byte-pressure counter and the next allocating function's
  prologue safepoint runs the collect when the threshold trips,
  which is sufficient for any body that grows the heap. Measured
  recovery in `gos build --release`: spectral-norm 47.8s → 0.92s,
  fannkuch 1.12s → 0.13s, fasta 9.22s → 2.00s, n-body 16.5s →
  1.49s, k-nucleotide 1.09s → 0.51s.
- **HTTP server thread-per-connection restored.** 0.6.0 had
  swapped `gos_rt_http_serve` from "spawn a dedicated OS thread
  per accepted socket" (`http_blocking_io_fix.md`, 2026-05-12:
  272k RPS / 0 fails) to "fixed worker pool + bounded
  `sync_channel`". With `available_parallelism() * 2` workers
  (≈ 48 on a 12-core box), > 48 concurrent clients saturated
  the pool, the queue filled, `try_send` started silently
  dropping sockets (RST'd by the OS), and the bench saw
  connection errors. The dedicated-thread shape (capped by
  `HTTP_ACTIVE_CONNS` / `GOSSAMER_HTTP_MAX_CONN` — default
  4096 — so a runaway client cannot bomb the thread / fd
  budget; past the cap responds 503 cleanly) is back.
  Recovery on web-server bench: text 198k → 263k RPS / 174 →
  0 fails; json 221k → 258k RPS / 169 → 0 fails.
- **Fuzz targets `hir_lower` + `vm_run` were broken on `cargo
  +nightly fuzz build`.** `grammar::render_source` was
  `pub(crate)` (invisible to fuzz-target bins, which are
  separate crates from the fuzz lib), and the call sites still
  used the pre-0.5.0 `parse_source_file(String, _)` /
  `vm.call(&str, &[])` signatures. Renamed to `pub`, swapped to
  `parse_source_file(&str, _)` / `vm.call(&str, Vec<Value>)`.
- `c_abi::tracing_gc_tests::ptr_key_is_send_sync_via_usize` now
  acquires `GC_TEST_LOCK`; previously raced the process-wide GC
  registry against sibling tests, producing intermittent
  "alloc_count = 0" / "freed = 0" failures.
- Removed unused `CloseHandle` import from `preempt.rs`
  (`-D warnings` failed the Windows build on Rust 1.95).

### Behavior changes

- Stricter at every IR boundary; some previously-silent miscompiles
  now refuse to compile.
- `gos build` is LLVM-only (Cranelift remains the in-process JIT for
  `gos run`); `--release` runs the full `opt -O3 | llc -O3` pipeline.

## 0.5.1

### Bug fixes

- **`json::render(&adt)` now works in compiled mode.** Calling
  `json::render` on a user-defined struct previously fell through to the
  raw `gos_rt_json_render` path in compiled (Cranelift/LLVM) code,
  where the runtime misinterpreted the struct pointer as a `GosJson`
  Arc — crashing on the first field access.

- **Compiled-mode segfault when `json::render` appears in one branch of
  an if-else.** `lower_json_render_adt` allocates a `pairs_vec` (via
  `Vec::new`) only inside the JSON arm. `insert_drops_at_returns`
  scanned all blocks globally and emitted `gos_rt_vec_free(pairs_vec)`
  at every `Return` — including the other arm where `pairs_vec` was
  never initialised, producing `gos_rt_vec_free(0x21)` → segfault in
  `__GI___libc_free`.

## 0.5.0

### Language

- **Tree-walker retired.** `gos run` now exclusively uses the register-based
  bytecode VM. The `--tree-walker` / `--vm` flags are removed; `gos run` has
  no mode selector. Programs that previously required the walker fall back to
  the VM or should use `gos build`.
- **Generic structs.** `struct Pair<A, B> { fst: A, snd: B }` is typechecked
  across multiple instantiation sites. Per-instance substitution at field-read
  sites lets field arithmetic (`p.fst + p.snd`) resolve to the correct
  concrete type. Supported in the VM tier; compiled-tier parity tracked
  separately.
- **`extern "C" { }` rejected at parse time (GP0016).** Parser previously
  infinite-looped on any `extern` block. Fixed: the extern item is consumed
  cleanly and GP0016 is emitted. Applies to bare block,
  `#[no_mangle] extern "C" fn`, and `unsafe extern "C" { }` forms.
  `gos explain GP0016` directs users to `[rust-bindings]`.
- **`vec![...]` macro confirmed for 0.5.0.** `assert!`, `assert_eq!`,
  `debug_assert!`, `todo!`, `unimplemented!`, `write!`, `writeln!` are
  rejected at parse time (SPEC §14 not-in-0.5.0).

### VM / runtime

- **Call depth limit with clean diagnostic (GX0008).** Unbounded recursion
  now produces `error[GX0008]: stack overflow — call depth exceeded 40 frames`
  with a call-stack trace instead of a Rust stack overflow / SIGSEGV. The
  limit is calibrated for debug builds; `gos build` is not affected — native
  code uses the OS call stack. `gos explain GX0008` registered.

### Correctness

- **MIR verifier wired into optimization pipeline.** `verify_body` runs after
  every optimization pass. Structural drift (bad block ids, out-of-range
  locals, missing call targets) panics immediately under debug assertions
  instead of silently miscompiling.
- **GC write barriers emitted.** New shared `gossamer_mir::insert_gc_barriers`
  pass walks every projected pointer-store and emits
  `StatementKind::GcWriteBarrier`; both LLVM and Cranelift backends emit
  `gos_rt_write_barrier`. Concurrent collector is now safe as the default;
  `GOSSAMER_GC_MODE=stw` disables the allocation-driven incremental drive.
- **Race detector: multi-reader RAW/WAR tracking.** Per-address state now
  stores the last write and up to four concurrent active reads. Write accesses
  check all active readers for write-after-read conflicts; read accesses check
  the last write for read-after-write conflicts. Previous single-entry
  tracking missed races where a reader's record was overwritten before the
  conflicting write arrived.
- **LSP did-you-mean quick-fix.** `textDocument/codeAction` now surfaces
  machine-applicable `Suggestion` objects for unresolved-name diagnostics,
  not just help text. Editors that support quick-fixes receive a one-click
  rename to the nearest spelling match.
- **`ExprKind::Error` AST variant.** All compiler passes (HIR lower,
  typechecker, resolver, MIR lower, interpreter, LSP passes) handle the new
  `Error` expression variant. Malformed sub-expressions can now be represented
  in the AST instead of being silently dropped, enabling error-recovery paths
  that suppress cascading diagnostics.
- **Native codegen — zero LLVM fallbacks on `tier_parity`.** Aggregate
  Display formatting (Vec / Array / `JsonValue` / `DynError`) lowers inline
  via `gos_rt_*_format_*` helpers; struct-update aggregate-store path
  handles 1-slot fields; `Ok(struct)` heap-copies the aggregate so the
  payload pointer outlives the producer's frame; `gos_rt_chan_send`
  stack-spills its value arg; `channel()` materialises a fresh 16-byte pair
  buffer so `(tx, rx)` destructuring can't overflow a 1-slot alloca;
  `bitcast void` IR errors fixed.
- **Unary `Not` type inference.** MIR `lower_unary` inherits the operand's
  concrete type when the HIR result is `Var(_)`, fixing `!fs::exists(p)`
  segfaults where the `i1` result was being routed through `print_str`.

### Context cancellation

- **`rx.recv_ctx(&ctx)` end-to-end.** New runtime helper
  `gos_rt_chan_recv_ctx_option` plus cross-crate hook bridge
  (`gos_rt_install_ctx_hooks`); MIR dispatches the method name to the
  helper, and interp gains a matching `Channel::recv_ctx` builtin. OS-thread
  callers observe cancel within 50 ms via a bounded `wait_timeout`;
  goroutine callers via the scheduler's existing unpark path. Context flows
  in from any surface that hands one out (today: HTTP `r.context`).
- **Cancellation tests.** 4 channel-context tests, 3 net-context tests
  (`TcpListener::accept_ctx`, `TcpStream::read_ctx`).

### Tooling / CI

- **Miri nightly workflow.** `.github/workflows/miri.yml` runs `cargo miri
  test --lib` weekly against the seven safety-load-bearing crates (gc, mir,
  types, resolve, runtime, coro, sched).
- **Workspace lint debt.** `unsafe_code` workspace level changed `forbid`
  → `deny` so per-fn `#[allow(unsafe_code)]` works without each crate
  re-listing every workspace lint. Four of five unsafe-using crates dropped
  their duplicated `[lints]` overrides.
- **`tier_parity` flake fix.** New `PARITY_WALK_LOCK` serialises the
  cranelift/llvm parity walks so concurrent test functions can't race on
  shared `/tmp/gossamer_test_*` fixture paths.
- **`release_perf` tolerance fix.** Sub-50 ms wallclock skips the
  ratio check (both backends constant-folded the loop to startup-noise);
  live-loop tolerance bumped 1.10× → 1.25× for CI jitter.
- **Every bug-tracking `#[ignore]` closed.** 6 previously-ignored tests
  unblocked (channel drain, nested format precision, capturing closure as
  goroutine, `?`-through-indexed-Vec-field, 1k and 10k goroutine stress);
  the only remaining `#[ignore]`s are explicitly opt-in perf
  characterizations.

### Serialization safety

- **Depth and size limits for JSON, XML, and YAML.** Default: 128 levels deep,
  16 MiB. Pre-parse size rejection avoids allocation; depth is tracked live
  during parse. Process-wide overrides via `set_max_depth` / `set_max_size`.

### Fuzzing

- **7 fuzz targets.** `lex`, `parse`, `manifest`, `http_request`, `typecheck`,
  `mir_lower` (includes verifier), `vm_compile`. 30-second smoke CI on every
  PR; 1-hour weekly deep run.

### Perf CI

- **Baseline-pinned regression gate.** Per-benchmark baselines are cached
  between CI runs; any benchmark that exceeds 2× its baseline fails the build.
  Three representative programs exercise arithmetic, recursion, and I/O on
  every PR.

### SPEC conformance tests

- 9 tests in `spec_conformance` pin every 0.5.0 conformance banner
  behaviorally: GP0016 rejection, macro subset, integer overflow no-panic,
  borrow-check not enforced, `--message-format json` schema.

### Edge-case tests

- 3 tests in `edge_case_battery`: NaN propagation, double-close channel panic,
  stack-overflow → GX0008. All use spawn + timeout so they cannot seize CI.

## 0.4.0

### Stdlib reorganization (Rust-style `fs` / `env` / `process`, Go-style HTTP/2)

The standard library's process-level surface was restructured for
intuitiveness. Filesystem ops moved out of `os`, environment +
argv split into `env`, child processes into `process`. HTTP/2 was
dissolved into `std::http` exactly as Go does in `net/http` — no
separate `std::http2` namespace.

**New modules:**

- **`std::env`** — `args`, `program_name`, `var`, `set_var`,
  `unset_var`, `current_dir`, `set_current_dir`, `home_dir`,
  `temp_dir`. Mirrors Rust's `std::env`.
- **`std::process`** — `Command`, `Output`, `Stdio`, `ExitStatus`,
  `Child`, `run`, `spawn`, `kill`, `exit`, `id`, `abort`. Mirrors
  Rust's `std::process`.

**Expanded `std::fs`** with the full filesystem surface, no longer
sparse: `read`, `read_to_string`, `write`, `read_dir`, `walk_dir`,
`create_dir`, `create_dir_all`, `remove_file`, `remove_dir`,
`remove_dir_all`, `remove_all`, `copy`, `rename`, `exists`,
`is_file`, `is_dir`, `is_symlink`, `file_size`, `metadata`,
`canonicalize`, `glob`, `eval_symlinks`. `fs::is_file`,
`fs::is_dir`, `fs::is_symlink`, `fs::file_size` are wired through
the compiled tier with new `gos_rt_os_is_symlink` /
`gos_rt_os_file_size` runtime helpers.

**HTTP/2 folded into `std::http`** (Go-style). `std::http2` is
gone. Renamed entry points live under `std::http`:

| Old (`std::http2::*`) | New (`std::http::*`) |
| --- | --- |
| `bind_and_run_h2c` | `serve_h2c` |
| `bind_and_run_h2c_streaming` | `serve_h2c_streaming` |
| `serve_connection` | `serve_h2_connection` |
| `serve_connection_streaming` | `serve_h2_connection_streaming` |
| `Handler` | `Http2Handler` |
| `StreamingHandler` | `Http2StreamingHandler` |
| `ResponseWriter` | `StreamingResponseWriter` |
| `Config` | `Http2Config` |
| `ServerHandle` | `Http2ServerHandle` |
| `Error` | `Http2Error` |

**`std::path` is now I/O-free.** `path::walk` was removed
(`fs::walk_dir` is canonical); `glob` and `eval_symlinks` moved to
`fs::glob` / `fs::eval_symlinks`.

**`std::os` shrunk to OS identity.** New: `os::family()`
(`"unix"`/`"windows"`), `os::arch()` (CPU triple component). The
old filesystem/env/process functions stay callable for one minor
release as deprecated re-exports — every entry in the `os::`
manifest now says "Deprecated: use ...".

**New documented modules:** `std::log` (Go-style flat log shape)
and `std::thread` (native OS threads) both existed in source but
were absent from the manifest; both now documented.

**Naming aliases (no behavior change):**

- `strings::to_lower` / `to_upper` — short alias for
  `to_lowercase` / `to_uppercase`, matching SKILL.md and Go.
- `strconv::parse_int` / `atoi` / `parse_float` / `format_int` /
  `itoa` / `format_float` — Go-style aliases for the existing
  `parse_i64` / `parse_f64` / `format_i64` / `format_f64`.

**Manifest dedup:** the split `ENCODING_BINARY` /
`ENCODING_BINARY_FULL` entries collapsed into a single
`std::encoding::binary` block.

**Dropped bare-module aliases:** `gzip::*` was a back-compat alias
for `compress::gzip::*` — removed; the canonical path was already
the dispatch shape every example used. Bare `exec::*` retained
for back-compat alongside the new `process::*`.

**Migration:** `docs_src/migration/rust.md` and
`docs_src/migration/go.md` now ship a "Standard library mapping"
table each, calling out the Rust → Gossamer and Go → Gossamer
shape of every common entry. `examples/cat.gos`, `grep.gos`,
`environment.gos`, `cli_args.gos`, `simple_cli_args.gos`,
`list_dir.gos`, `http2_server.gos`, and
`projects/web_service_full/src/main.gos` all rewritten to the
canonical names.

A new `stdlib_surface_snapshot` regression test in
`crates/gossamer-std/tests/` pins the documented item count so
future drops require a deliberate floor adjustment.

### Binding ABI

Four new shapes in the Rust-binding system; every 0.3 binding crate
recompiles unchanged.

- **`Type::Bytes`** — first-class byte payload, distinct from
  `Vec<i64>` at the source level. Rust shape is the new
  `gossamer_binding::Bytes` newtype (transparent `Vec<u8>`).
  Compiled tier uses a `GosBytes { len, cap, ptr }` C-ABI struct;
  interp tier stores as `Value::IntArray`.
- **`Type::Map<K, V>`** — keyed collection backed by `HashMap<K, V>`.
  Compiled tier uses `GosMap { keys, values }` parallel-vec headers.
  Concrete impls for `HashMap<String, String>`,
  `HashMap<String, i64>`, `HashMap<i64, i64>`.
- **`Type::Variant<arms...>`** — tagged-union return backed by the
  new `gossamer_binding::DynValue` enum (Nil, Bool, Int, Float,
  Char, String, Bytes, List, Map, Tagged). Compiled tier uses
  `GosDynVariant { name, payload_len, payload }` with arena-
  allocated arm names.
- **`Type::Callback(args, ret)`** — Gossamer-side callable that
  bindings may invoke during their call. `BindingCallback` for
  interp (wraps a `Value`), `NativeCallback` for compiled (wraps a
  `u64` handle). Lifetime is strictly call-scoped — retaining past
  the binding return is undefined behaviour.

`gossamer_resolve::BindingType`, `gossamer_driver::DumpedType`,
`gossamer_runner_template/sigs_dump.rs.tmpl`, and
`gossamer_mir::lower::binding_type_to_mir` all extended to handle
the new shapes. Architecture spec at
`crates/gossamer-binding/ABI_0_4.md`.

### CI test reliability

- **Port-bind race in HTTP tests fixed.** The `pick_port()` helper
  in `gossamer-std`'s `http_server`, `http_proxy`, and
  `http_native_client` test modules bound `127.0.0.1:0`, read the
  assigned port, **dropped the listener**, then expected the test
  to re-bind the same port. On Windows CI agents and busy hosts
  the gap was reliably exploited, producing intermittent
  `AddrInUse` panics and `gossamer-std --lib` / `--test http_server`
  failures with exit code 101. Replaced with `bind_loopback() ->
  (TcpListener, SocketAddr)` that hands the live listener back.

### Language / parser

- **Statement boundary for leading `&` / `*` / `-`.** A newline
  followed by one of these three operators now ends the previous
  statement, so `let s = read(p)?\n&s |> ...` parses as two
  statements instead of `let s = read(p)? & s |> ...`. Multi-line
  continuation still works when the operator sits at the end of
  the previous line (`let x = a -\n  b`) or inside parentheses;
  all other binary operators continue across newlines
  unconditionally. SPEC §2.7.
- **`?` in macro argument position propagates early-return.**
  `print!("{}", expr?)` correctly returns `Err(e)` from the
  enclosing function when `expr` is `Err`; previously the result
  was silently passed to `__concat`.

### Manifest

- **Explicit `[[bin]]` and `[lib]` tables in `project.toml`.**
  Array-of-tables for `bin`, single-table for `lib`. Duplicate
  bin names rejected. Implicit filesystem convention
  (`src/main.gos` / `src/lib.gos`) still works when neither is
  present.

### HTTP — wire correctness

- **`Client::builder().tls(...)` and `.cookies(...)` now work.**
  Previous behaviour silently dropped both. `ClientConfig` retains
  the source PEM bytes so the ureq bridge can rebuild TLS state.
- **`Date` and `Server` headers auto-inserted on every response**
  per RFC 9110 §6.6.1. `Server` value is configurable via
  `Config.server_name` (default `gossamer/0.4.0`); handler-supplied
  `Date` / `Server` headers are preserved without duplication.
  New `std::time::format_rfc1123_gmt` helper.
- **Chunked transfer encoding** (RFC 7230 §4.1) for both inbound
  request bodies and outbound responses. New `std::http_chunked`
  module (`ChunkedReader` + `ChunkedWriter`) with malformed-input
  hardening (bad hex, premature EOF, missing CRLF, oversize
  length, chunk-extensions). Trailer headers on inbound chunked
  bodies merge into `request.headers`. Outbound chunked is
  triggered by the handler setting `Transfer-Encoding: chunked`;
  `Content-Length` is stripped when both are present. Combination
  of chunked + `Content-Length` on the request is rejected with
  `400`.
- **`Expect: 100-continue`** support (RFC 7231 §5.1.1) on both
  plain-TCP and TLS paths. The HTTP parser is split into
  `parse_request_head_generic` and `finish_request` so the server
  can write the interim response between head parse and body read.
- **Path / query split.** `Request.path` is now the URL path alone
  (Go's `URL.Path` semantics); `Request.query` carries the raw
  query string (no leading `?`). New helpers: `Request::query()`,
  `Request::request_uri()`, `Request::query_pairs()` (percent-
  decoding), and `std::http::split_path_query()`.
- **`Headers::remove(name)`** added.
- **Unified HTTP/1.1 parser.** The TLS and plain-TCP paths now
  share a single generic `parse_request_head_generic` +
  `finish_request` implementation.

### HTTP — timeouts and graceful shutdown

- **Timeout taxonomy.** `Config` gains `read_header_timeout`
  (10 s default, slowloris guard), `read_body_timeout` (30 s),
  `write_timeout` (30 s), `idle_timeout` (75 s). The legacy
  `read_timeout` knob still works as a blanket fallback. Per-phase
  deadlines enforced via `Instant`-based total-elapsed checks in
  the parser and body reader; per-syscall timeouts via
  `set_read_timeout` / `set_write_timeout` switching across the
  idle → header → body → write phases.
- **`Server::shutdown(&Config, Option<Duration>) -> bool`** —
  flips the shutdown flag, blocks until `Config.in_flight` drains
  to zero or the deadline elapses. Returns `true` on clean drain,
  `false` on timeout. Worker loop polls the flag between
  keep-alive requests so idle connections close promptly.
- **Per-request `Context` cancellation.** A watcher fires the
  cancel handle when `Config.shutdown` trips, so long-running
  handlers observe `request.context().is_cancelled() == true`.

### HTTP — router, middleware, static files, proxy

- **`http_router`** — Go 1.22-class `ServeMux` with `{name}`
  captures, `{rest...}` trailing captures, `*` wildcard, method
  gating (`get` / `post` / `put` / `delete` / `patch` / `head` /
  `options`). Precedence: method-specific beats method-agnostic;
  more-specific pattern wins; insertion-order breaks ties. Default
  404 / 405 responses with overridable hooks.
- **`http_middleware`** — Logger, Recoverer
  (`std::panic::catch_unwind`), RequestId (`X-Request-Id` stamping
  with carry-through), CORS (preflight + per-response headers),
  BasicAuth (RFC 7617), Compress (gzip body framing gated on
  `Accept-Encoding`, with min-bytes threshold).
- **`http_static_files`** — `FileServer` with configurable `etag`,
  `last_modified`, `range_support`, `max_file_bytes`. Path-
  traversal guard (`fs::canonicalize` + prefix check). 200 / 206
  / 304 / 404 / 416 response shaping. ETag from `mtime + size`,
  RFC 1123 GMT `Last-Modified`. MIME table covers 25 common
  extensions. `index.html` auto-served on directory hits.
- **`http_proxy`** (behind `http-client` feature) — `ReverseProxy`
  with caller-supplied `director`, `modify_response`,
  `error_handler`. `ReverseProxy::single_host` forwards path +
  query verbatim. Hop-by-hop header stripping per RFC 7230 §6.1.
  Auto-appends `X-Forwarded-For`, `X-Forwarded-Host`,
  `X-Forwarded-Proto`.

### HTTP — WebSocket and SSE

- **`http_websocket`** — RFC 6455 from scratch. `accept` performs
  the handshake (validates Upgrade / Connection /
  Sec-WebSocket-Version=13 / Sec-WebSocket-Key, computes
  Sec-WebSocket-Accept via inline SHA-1 + base64). `WebSocket`
  exposes `send_text` / `send_binary` / `send_ping` / `send_pong`
  / `send_close` / `receive` over any `Read + Write` stream.
  Auto-pong on inbound ping. Fragmented frame reassembly via
  continuation opcodes. Server-mode requires client masking;
  client-mode masks outbound frames. Length encoding handles
  7-bit, 16-bit, and 64-bit forms. Inline SHA-1 + base64
  implementations (no extra deps).
- **`http_sse`** — Server-Sent Events (`text/event-stream`)
  encoder: `SseStream::send` (event name / id / data lines),
  `send_retry`, `send_comment` (heartbeat). `event_stream_headers()`
  + `response_skeleton()` helpers.

### HTTP/2 server

- **`std::http::serve_h2c`** in both `gos run` and `gos build`.
  (Renamed from `std::http2::bind_and_run_h2c` during 0.4.0 dev —
  HTTP/2 is now folded into `std::http` per the Go model; see
  "Stdlib reorganization" above.) The `h2` crate runs on
  Gossamer's own goroutine scheduler via `runtime_future::drive`
  (a future-pump) + `async_tcp::AsyncTcpStream` (mio-bridge over
  non-blocking TCP). Tokio is consumed only for its `AsyncRead` /
  `AsyncWrite` trait surface.
- Bounded `Http2Handler` (`fn serve(req) -> Response`) and chunked
  `Http2StreamingHandler` (`fn serve(req, StreamingResponseWriter)`)
  shapes both supported. `StreamingResponseWriter::write_chunk`
  flushes the response head on first call and emits one `DATA`
  frame per call; `finish` (or `Drop`) sends the terminating
  `END_STREAM`.
- **ALPN-driven HTTPS dispatch** via `bind_and_run_tls_h2`
  (tokio-rustls trait-only).
- Architecture documented at `crates/gossamer-std/HTTP_H2_ARCH.md`.

### Native HTTP/1 client

- **`http_native_client`** built on `std::net::TcpStream`.
  `NativeClient::{get, post, put, delete, request}` with per-
  client connection pool (keyed by host:port), configurable
  redirect policy (default 10 hops), chunked response decoding,
  user-agent / timeout / max-body-bytes config. HTTPS not yet
  supported; TLS stays on the existing ureq path.

### HTTP module bridges — interp + compiled parity

Eight stdlib HTTP modules now callable from Gossamer source in
both tiers, byte-identical across `gos run` and `gos build`.

- **router / FileServer / NativeClient / Proxy** — stateful,
  method-chain dispatch. `Router::new()`, `r.get(path, Handler {})`,
  `r.serve(req)` and the rest of the verb chain work end-to-end.
  22 new `gos_rt_*` runtime symbols. MIR auto-synthesises
  `gos_fn_addr("{Handler}::serve")` for HTTP-verb methods so the
  runtime can transmute and invoke user handlers through the same
  fn-pointer ABI as `gos_rt_http_serve`. `gos_fn_addr` now
  resolves to `intrinsics.externs` for runtime symbols.
- **chunked / sse / middleware / websocket-accept-key /
  static_files-mime** — stateless free-fn shapes.
  `chunked::encode` / `decode`, `sse::encode_event` / `comment` /
  `retry`, `middleware::new_request_id` / `accepts_gzip`,
  `websocket::accept_key`, `static_files::mime_for_path`. Self-
  contained SHA-1 + base64 in the runtime for the WS accept-key
  derivation.
- **MIR runtime-kind tag from rendered type** — `lower_fn`'s
  parameter binding now reads the rendered type of the param (in
  addition to the binding name), so `r: http::Request` resolves
  the same as `request: http::Request`. Fixes garbage reads on
  `r.path` / `r.body` for handler params named anything other than
  `request` / `req`.

### Netpoller latency

- Tightened the `globals().poller.lock()` hold during
  `mio::Poll::poll()` so registering goroutines no longer wait up
  to 50 ms per IO op. New `mio::Waker` interrupts in-flight polls
  when `with_poller` mutates state; poll cycle dropped to 1 ms.
  Multiplexed h2c: 3.7 ms/req, was 100 ms.

### Networking

- **`net::TcpStream::set_keepalive(Option<Duration>)`** — socket2-
  backed `SO_KEEPALIVE` toggle.
- **`net::TcpStream::connect_happy_eyeballs(addrs, stagger,
  timeout)`** — Go 1.21-style v6/v4-interleaved race with per-
  candidate staggered start.
- **`net::UnixListener` / `net::UnixStream`** (Unix-only) — bind /
  accept / connect / read / write / shutdown.
- **`net::IpNet`** — RFC 4632 prefix parsing for IPv4 and IPv6,
  `contains(&Ip)` predicate, `prefix_len()`, `render()`. Cross-
  family addresses are rejected.
- **`net::url`** — `path_escape` / `path_unescape`,
  `UserInfo { username, password: Option<String> }` (parse +
  render with percent-encoding), `Values` (Go's `url.Values`:
  `add` / `set` / `get` / `get_all` / `delete` / `encode` /
  `parse`).
- New `socket2 = "0.5"` dependency in `gossamer-std`.

### Stdlib

- **`std::io`** — `copy`, `copy_n`, `read_all`, `LimitReader`,
  `TeeReader`, `MultiReader`, `pipe` (paired `PipeReader` +
  `PipeWriter` with cross-thread blocking semantics).
- **`std::log`** (new) — Go-style flat logger: `println`,
  `printf`, `fatal`, `panic_msg`; `set_output`, `set_prefix`,
  `set_flags`; flag constants `L_DATE`, `L_TIME`, `L_MICROSECONDS`,
  `L_JSON`, etc. Global process-wide sink protected by
  `parking_lot::Mutex`.
- **`std::time`** — `Ticker` (recurring callback every interval;
  `stop()` / Drop-safe), `after_func` (one-shot timer returning a
  cancellable `TimerHandle`), `SystemTime::from_std` / `as_std` /
  `unix_seconds`.
- **`std::sync`** — `SyncMap<K, V>` (read-heavy `RwLock`-backed
  concurrent map: `store` / `load` / `load_or_store` / `delete` /
  `contains` / `range`), `Pool<T>` (factory-backed freelist),
  `Cond` (`parking_lot::Condvar` wrapper for `signal` /
  `broadcast` / `wait`).
- **`std::path`** — Go `path/filepath` parity: `glob(pattern)`
  (literal, `*`, `?`, `[class]`, `**` recursive), `matches(pattern,
  name)` segment matcher (no `/` crossing), `walk(root, visit)`
  with `SKIP_DIR` / `SKIP_ALL` sentinels, `eval_symlinks(path)`.
- **`std::crypto::cipher`** — `aes_ctr_xor` (in-place encrypt/
  decrypt for 128/192/256-bit keys), `aes_cbc_encrypt` +
  `aes_cbc_decrypt` with PKCS#7 padding. Bad key sizes and bad
  IVs return typed errors.
- **`std::runtime`** — `caller(skip)` returns
  `Option<StackFrame>`, `stack()` returns `Vec<StackFrame>` (both
  backed by the `backtrace` crate). `set_finalizer(arc, fn)`
  returns a `Finalizer<T>` guard with `cancel()` and Arc-aware
  drop semantics — fires only when the last clone goes away.
- **`std::text::template`** — `FuncMap` registry with default
  helpers (`upper`, `lower`, `trim`, `len`, `default`,
  `html_escape`); pipelines (`{{ .x | upper | trim }}`);
  `Template::render_with_funcs(data, funcs)` and free
  `render_with_funcs(source, data, funcs)`. Unknown function names
  raise `Error::Parse`.

### Stdlib feature gates removed

`gossamer-std` no longer ships behind feature flags — every
module (regex, tls, crypto, compress, archive, http2, templates,
sql, ureq, …) is unconditionally compiled. The `[features]`
table is gone; consumers depend on the crate plain. 58
`#[cfg(feature = …)]` sites stripped.

### Tooling

- **`gos doc --emit-stdlib DIR`** — walks `manifest::ALL_MODULES`
  and emits one Markdown page per module under `DIR` plus an
  `index.md` landing page. `--check` mode compares disk against
  generated output and fails the build on drift. Wired into
  `check.sh` and the `stdlib-docs-drift` GitHub Actions job. 79
  stdlib pages committed under `docs_src/stdlib/`.

## 0.3.0

### Added

- **`std::compress` expanded.** New `flate` (raw DEFLATE), `zlib`, and
  `bzip2` submodules join the existing `gzip` module. All three are
  feature-gated (`compress` / `bzip2-compress`).
- **`std::archive`.** New `tar` and `zip` submodules for reading and
  writing archives, backed by the `tar` and `zip` crates.
- **`std::hash::fnv`.** FNV-1a and FNV-1 hashes in 32- and 64-bit
  variants; no new dependencies.
- **`std::encoding` expanded.** New `base32` (RFC 4648 standard and hex
  alphabets), `ascii85` (Adobe / btoa), and `xml` (quick-xml backed)
  submodules. Qualified-path access for `encoding::base64` and
  `encoding::hex` is now wired.
- **`std::crypto::insecure`.** MD5 and SHA-1 for legacy-compatibility
  contexts; feature-gated as `insecure-crypto`.
- **`std::math::big`.** Arbitrary-precision integers via decimal-string
  representation. Exposes `Int::parse`, `Uint::parse`, `Int::compare`,
  and `factorial`.
- **`std::sync::AtomicU64` and `sync::Barrier`** wired to the interpreter.
- **52 integration tests** for all new stdlib modules in
  `crates/gossamer-cli/tests/stdlib_new_modules.rs`.
- **5 new examples**: `crypto_hashing.gos`, `encoding_codecs.gos`,
  `big_numbers.gos`, `compress_demo.gos`, `html_escape.gos`.

### Performance

- **Parallel Cranelift body lowering.** Function bodies now compile
  concurrently via rayon. An `OfflineModule` snapshot pre-declares all
  symbols in a single-threaded phase; each rayon worker then lowers its
  assigned body without taking any global lock.
- **Auto-drop pass overhaul.** Ten stacked compiler and runtime fixes
  make the heap-free pipeline produce IR that actually executes
  `gos_rt_*_free` calls. Changes include: per-block liveness-based drop
  placement, copy-alias chain tracking, inter-procedural escape analysis
  (`CaptureSummary`), a sentinel-pointer skiplist for globally-owned
  buffers, and `gos_rt_str_free` for owning strings. Effect on benchmarks
  (source unchanged): k-nucleotide −33% peak RSS, spectral-norm −8%.
- **`gos test` parallel by default.** Defaults to
  `available_parallelism()` workers. `--serial` (alias `--parallel 1`)
  opts back to sequential execution.
- **`define_only` allow-list check is O(1).** Converted from a linear
  scan to a `HashSet` in `lower_program_full`.

### Architecture

- **Incremental GC drive wired into the allocation fast path.**
  `gos_rt_gc_alloc_rooted` calls `drive_incremental()` after each
  rooted allocation: starts a new concurrent cycle when RSS exceeds the
  threshold (default 4 MB; override with `GOSSAMER_GC_TARGET`), marks a
  32-object batch during marking, and finalises when the grey set is
  exhausted.

### Fixes

- **`Result` used as a bare statement is now a compile error (GT0007).**
  Every `Result<T, E>` expression must be handled via `?`,
  `match`/`if let`, or `let _ = expr`. `gos explain GT0007` documents
  the rationale.

## 0.2.0

### Performance

- **JIT peak RSS reduced for programs with large array initialisers.**
  - `Rvalue::Repeat` in the Cranelift backend now skips all stores for
    zero-constant fills (`[0.0; N]`, `[false; N]`, `[(); N]`) — `calloc`
    already zeroes memory, so the stores were redundant. Non-zero fills
    larger than 16 elements emit a counted loop (O(1) IR) instead of N
    unrolled `store` instructions (O(N) IR), matching the LLVM backend.
  - Array-typed return values in the Cranelift backend are now returned
    directly (the existing `calloc`-allocated local is passed back as-is)
    instead of going through a second `gos_rt_gc_alloc` + memcpy escape.
    Saves one allocation per array-returning call.
  - JIT compilation now pre-filters to the minimal set of bodies needed:
    a BFS from JIT-promotable roots (functions with scalar-only
    param/return types) collects their transitive user-function callees.
    Bodies that can never be promoted (aggregate params/returns) are
    skipped entirely, cutting JIT compile time proportionally.
- **HIR and type-context dropped before `vm.call()`.** The CLI's `gos run`
  path now explicitly drops the `HirProgram` and `TyCtxt` before entering
  the main call, then releases the MIR/TyCtxt JIT prelude after `vm.call()`
  returns and before goroutine-join. Frees the per-program compilation data
  while goroutines are still running, reducing peak RSS for large programs.

### Architecture

- **ABI registry (`gossamer-abi` crate) for typed `gos_rt_*` declarations.**
  A new `gossamer-abi` crate holds a single source-of-truth for every
  `gos_rt_*` symbol's name and C-ABI signature. The Cranelift backend's
  `extern_fn_by_name` and the LLVM lowerer's `declare_rt` both derive
  function declarations from this registry, eliminating the previously
  parallel string arrays. Typos in symbol names now panic at test time
  rather than silently producing wrong code.

### Fixes

- **LLVM write-barrier correctness.** The write barrier was being emitted
  for `ptr`-typed LLVM values (raw machine pointers). `gos_rt_write_barrier`
  expects a `u32` GcRef index (widened to i64 in the flat ABI); truncating
  a pointer to i32 is both invalid IR and semantically wrong. The barrier
  is now suppressed for all `ptr`-typed values; the GC tracks those through
  its allocation registry.
- **LLVM aggregate-return memcpy for runtime helpers.** When a runtime call
  returns a heap pointer to a multi-slot aggregate (e.g.
  `gos_rt_result_payload` returning an `ExecOutput` blob), the destination
  is an inline `[N x i64]` alloca. A bare `store ptr` only wrote the blob
  address into slot 0, making subsequent field reads load the blob pointer
  instead of the actual field value. The LLVM lowerer now emits a full
  memcpy for these cases.
- **LLVM call-site type declarations match the call instruction.** Runtime
  functions whose registry ABI type differs from the LLVM call-site type
  (e.g. `gos_rt_result_payload` is `I64` in the registry but called as
  `ptr` in compiled MIR) now always declare using the call-site type.
  Registry-type declarations caused `opt` to miscompile the wrong type.

### Added

- **PGO support for `gos build --release`.**
  Two environment variables drive a standard three-step LLVM PGO workflow:
  - `GOS_PGO_COLLECT=<output.profraw>` builds an instrumented binary that
    writes raw profile data on exit. Links `libclang_rt.profile-x86_64.a`
    automatically.
  - `GOS_PGO_PROFILE=<merged.profdata>` feeds a previously collected and
    `llvm-profdata`-merged profile into `opt --pgo-kind=pgo-instr-use-pipeline`.
  The `gos build` command prints the three-step workflow on first use.
- **Binary size reduction for `gos build`.**
  Release builds now strip all symbols and dead sections (`-Wl,--gc-sections`
  on Linux, `-dead_strip` on macOS). Debug builds without `-g` strip only
  debug sections, keeping symbol names for crash reports. Brings the
  Cranelift-generated binary floor down ~75%.
- Github Actions tests do not fail fast.
## 0.1.8

### Fixes

- **`STATUS_HEAP_CORRUPTION` crash in native iterator test on Windows.**
  The MIR drop-insertion pass pins the destination local of `gos_rt_arr_iter`
  to the source `Vec<T>` type so `.next()` dispatch can recover the element
  kind. The type-based `inferred_free` path then incorrectly scheduled
  `gos_rt_vec_free` on the `*mut GosArrIter` pointer, interpreting the
  iterator's raw bytes as a `GosVec` header and corrupting the heap on free.
  Fixed by adding `gos_rt_arr_iter_free` to the runtime and registering
  `"gos_rt_arr_iter" => "gos_rt_arr_iter_free"` in `ctor_to_free`, so the
  drop pass emits the correct free instead of `gos_rt_vec_free`.

- **Missing `.exe` suffix on Windows in two multi-file regression tests.**
  `cross_file_project_bundles_sibling_modules` and
  `cross_file_chained_sibling_module_calls` constructed expected binary paths
  as bare stems (`target/debug/probe`, `target/debug/chained`) without the
  `.exe` extension. Fixed with `set_extension(EXE_EXTENSION)`, matching the
  pattern used in `parity.rs`.

- **Missing LLVM declaration for `gos_rt_arr_iter_free`.**
  The `dispatch_parity` test enforces that every symbol exported from
  `c_abi.rs` has a matching `declare` line in the LLVM prelude. Added
  `declare void @gos_rt_arr_iter_free(ptr)` to `gossamer-codegen-llvm/src/emit.rs`.

- **Directory sizes report 0 in native tiers on Windows.**
  `gos_rt_fs_list_dir` and `gos_rt_fs_walk_dir` used `DirEntry::metadata()`
  which reads from `WIN32_FIND_DATA` — a cached struct that stores
  `nFileSize = 0` for directories. The interpreter uses
  `std::fs::metadata(path)`, which opens a file handle and calls
  `GetFileInformationByHandle`, returning the real NTFS directory-index
  size. Both native functions now use `std::fs::metadata` to match.

- **Missing `.exe` suffix in `codegen_correct` and `native` integration tests on Windows.**
  `every_correct_program_matches_across_tiers` checked for binary artifacts at
  `target/debug/<stem>` and `target/release/<stem>` without the `.exe` extension,
  causing 16 failures (8 programs × 2 profiles). All 13 `gos build`-driven binary
  path constructions in `codegen_correct.rs` and `native.rs` now use
  `set_extension(EXE_EXTENSION)` or the new `debug_bin(&dir, stem)` helper.
  `gos_binary()` in `codegen_correct.rs` (the release `gos` tool path) is also fixed.

## 0.1.7

### Fixes

- **`exec::kill` interpreter implementation no longer uses unsafe on Windows.**
  `gossamer-interp` has `#![forbid(unsafe_code)]`, so the Win32 FFI approach
  from 0.1.6 was rejected at compile time. Replaced with `taskkill /F /PID
  <pid>` via `std::process::Command` — the same safe shell-out pattern used
  for `/bin/kill` on Unix. The compiled-tier runtime (`c_abi.rs`) keeps the
  direct `OpenProcess`/`TerminateProcess` approach, which is correct there
  since `gossamer-runtime` permits unsafe.

## 0.1.6

### Fixes

- **`unsafe extern` required in Rust 2024 edition.**
  The `extern "system"` blocks added in 0.1.5 for the Windows
  `exec::kill` implementation must be `unsafe extern "system"` in
  edition 2024. Fixed in both `gossamer-runtime/src/c_abi.rs` and
  `gossamer-interp/src/builtins.rs`.

## 0.1.5

### Fixes

- **`exec::kill` now terminates processes on Windows.**
  Both the compiled-tier runtime (`gos_rt_exec_kill` in `c_abi.rs`) and the
  interpreter (`builtin_exec_kill` in `builtins.rs`) returned `false`
  unconditionally on Windows. Both now call `OpenProcess(PROCESS_TERMINATE)`
  + `TerminateProcess` + `CloseHandle` via inline `extern "system"`
  declarations (no new dependencies). The `#[cfg(not(unix))]` fallback is
  split into `#[cfg(windows)]` (real implementation) and
  `#[cfg(not(any(unix, windows)))]` (stub for other platforms).

## 0.1.4

### Fixes

- **Test binary paths now include `.exe` on Windows.**
  Integration tests constructed expected output paths with bare stem names
  (`"agg"`, `"concurrent"`, etc.) but `gos build` correctly emits `<stem>.exe`
  via `platform_exe_name`. Fixed by appending `std::env::consts::EXE_SUFFIX`
  at every call site across seven test files (`aggregate_print_fallback`,
  `cli`, `format_precision_parity`, `memory_growth_bounded`, `parity`,
  `release_stability`, `stdout_concurrent_print`).


## 0.1.3

### Fixes

- **`gos build` now produces a `.exe` binary on Windows.**
  `output_path` and `resolve_output_path` were appending the bare unit name to
  the output directory on every platform. `rust-lld -flavor link` (unlike
  classic MSVC `link.exe`) writes the binary at the exact `/OUT:` path given,
  with no automatic `.exe` suffix. The result was a binary with no extension
  that `is_executable` on non-Unix (which checks for `.exe`) could not find,
  causing all `aggregate_abi` (and related) test cases to report
  "no binary in … \cl" on Windows CI. Fixed by adding a `platform_exe_name`
  helper in `paths.rs` that appends `.exe` on Windows, used consistently in
  both the `--out-dir` fast path and the default `target/{debug,release}/`
  path.

## 0.1.2

### Fixes

- **`os_env_compiled` test helpers no longer trigger dead-code warnings on Windows.**
  `os_set_env_round_trips_through_os_env_in_all_tiers` had an unnecessary
  `#[cfg(unix)]` guard — the test body is pure env-var I/O and runs on all
  platforms. Windows variant of the child-propagation test added using
  `cmd /c set`.

## 0.1.1

### Fixes

- **`exec_spawn` test helpers no longer trigger dead-code warnings on Windows.**
  All helper functions were ungated; Windows-equivalent test variants added
  (`ping 127.0.0.1` in place of `/bin/sleep`) so `exec::spawn` / `exec::kill`
  coverage runs on both platforms.

## 0.1.0

### Fixes

- use std::process::{Command as StdCommand, Stdio as StdStdio} inside 
  builtin_exec_kill is now #[cfg(unix)]-gated, since those aliases are only 
  used inside the #[cfg(unix)] block - dead warnings on Windows.
- **`Ok(N)` / `Some(N)` payload-literal matching in compiled mode.**
  `match r { Ok(1) => …, Ok(2) => … }` always took the first `Ok` arm
  because MIR only compared the discriminant, never the payload value.
  Now ANDs a `gos_rt_result_payload`-extracted value predicate with the
  disc predicate. Applies to all non-binding, non-wildcard payload
  patterns: literals, ranges, nested variants, or-patterns.
- **LLVM prelude missing `gos_rt_arr_iter`, `gos_rt_arr_iter_next`,
  `gos_rt_json_set`.** Three helpers declared in `c_abi.rs` had no
  `declare` entry in the LLVM IR prelude; they silently linked to zero.
  Added declarations; `dispatch_parity` test now enforces this for all
  future helpers.
- **`Option<T>` discriminator regression in compiled tiers fixed.**
  `match json::get(...)` returning `Some(v)` matched neither
  `Some` nor `None` arms — both fell through silently. Root cause:
  the runtime helpers `gos_rt_json_get`, `gos_rt_json_keys`,
  `gos_rt_json_as_array` returned bare `*mut GosJson`/`*mut GosVec`
  pointers, but user-level `json::get` is typed as `Option<&Value>`
  so MIR expected an Option-shaped `*mut GosResult` (16 bytes:
  `disc: i64, payload: i64`, `disc == 0` = Some, `disc == 1` =
  None). Added three new opt-flavoured helpers
  (`gos_rt_json_get_opt`, `gos_rt_json_keys_opt`,
  `gos_rt_json_as_array_opt`); MIR routes user-level json calls
  through them while internal field-access lowering keeps the
  bare helpers. Interp tier wraps the same calls via
  `some_variant(...)` / `none_variant()`. The bare helpers stay
  for chained MIR field-projection (`root.a.b.c`) so the wrap
  cost only lands on user-visible Option boundaries. Tests
  `json_get_returns_option_with_correct_discriminator`,
  `malformed_json_returns_none_not_segfault`,
  `json_as_array_iter_native` all pass now.
- **Cranelift native codegen — nested struct field offsets.**
  `o.inner.x` segfaulted: field projections used flat `idx*8`
  offsets that ignored embedded struct widths. Rewrote
  `lower_place_address` / `resolve_place_cl_type` /
  `resolve_place_ty` to sum `type_slot_count` of preceding
  fields. `Aggregate` construction (struct + tuple) now uses
  per-field widths from `tcx.struct_field_tys` and walks the
  nested layout. Also added a projected-aggregate-read
  shortcut so a slot returns the field address rather than
  collapsing to first slot. Same flat-`idx` bug fixed in the
  LLVM lowerer.
- **Cranelift call-site struct alias bug.** Multi-slot
  aggregates passed by value aliased the caller's storage
  (`shift(p)` mutated `original`). Added defensive copy via
  new `operand_aggregate_slots` / `clone_aggregate_value`
  helpers — fresh storage + per-slot memcpy at every
  by-value pass.
- **MIR `lower_place_expr` resolved nested fields against
  the wrong struct.** `o.inner.x = 100` looked up `x` on the
  outer struct, didn't find it, and silently dropped the
  assign. Now prefers `struct_name_from_expr(receiver)` (the
  projected type), with `local_struct[base.local]` as
  fallback.
- **`..base` functional-update was discarded.** HIR
  `lower_struct_literal` ignored the `base` field. Added a
  synthetic `__base` key that carries the base expression;
  MIR projects `base.field` for every unprovided field; VM
  `builtin_struct_new` strips the synthetic key.
- **Closure free-var capture pulled in synthetic helpers.**
  `__concat`, `__struct`, `__fmt_prec`, `format!`, etc. were
  walked as free variables and captured into closure envs.
  `walk_free` now excludes them.
- **`FnTrait`-typed locals weren't recognised as indirect
  callees.** Closure-returning-closure fell through to
  direct-name dispatch. MIR's callee-kind match now accepts
  `TyKind::FnTrait(_)` and routes through `Operand::Copy`.
- **`errors::Error` printed as a struct literal.** Now
  renders as the `message` field via the `Display` impl.
- **`os::exit(N)` swallowed buffered stdout.** Calls
  `gos_rt_flush_stdout` before `process::exit`.
- **`i128` / `u128` silently truncated to i64 in compiled
  tiers.** Now bails with a "compiled tier" diagnostic the
  test gate matches.
- **User-defined `pub fn substring(s, ...)` recursed via
  method dispatch.** `s.substring(a, b)` is now resolved
  to the runtime `String::substring` helper before falling
  through to user dispatch — restores `String::method` to
  the qualified-method dispatch keys.
- **Stray `feature-testing-examples/project.toml` was
  forcing all 52 examples into every build.** The CLI's
  sibling-bundle walked one parent up; the stray manifest
  made it bundle examples too. Removed the stray file.
- **Multi-file sibling-module regression fixed.** Cross-module
  function calls (`mod foo;` + `foo::bar()` from sibling
  `src/*.gos` files) compiled clean under `gos check` but failed
  at runtime with `error[GX0002]: name 'foo::bar' is not bound in
  this scope`. Root cause was layered: the CLI driver didn't
  auto-bundle siblings; the resolver only registered `mod` heads
  (no recursion into nested items); the type checker /
  exhaustiveness walkers stopped at module boundaries; the parser
  silently dropped `use` decls inside inline `mod` bodies; HIR
  carried no module-path so the interp / VM globals lost the
  qualified spelling; and the Cranelift JIT was missing
  `gos_rt_eprint_str` / `gos_rt_eprintln`. Each layer was
  patched. New regression test
  `cross_file_chained_sibling_module_calls` covers a 3-deep call
  chain across all three execution tiers.
- `String::as_bytes()` was registered as a runtime global but
  silently mis-wrote the byte slice through `os::write_file`.
  The method is now rejected at `gos check` time
  (`GT0002: no method named 'as_bytes' found for type 'String'`)
  via a new `KNOWN_METHOD_NAMES` allow-list in the type checker.
  Pass `&String` directly to byte-consuming APIs; the runtime
  binding is gone.
- `encoding::json` parser cast UTF-8 bytes through `char` as
  Latin-1, mangling all non-ASCII text. `\uXXXX` escapes weren't
  handled either. Now reads bytes properly and decodes
  `\uXXXX` (including surrogate pairs). The previously-broken
  `unicode_strings_preserve_through_round_trip` test now passes.
- Aggregate construction is now heap-allocated (`calloc`) instead
  of stack-slot. Returning a struct from a method (e.g.
  `Celsius { value: ... }.to_fahrenheit()`) no longer aliases the
  next call's stack slot; `temperature.gos` now matches across
  tiers.
- `loop { ... break <expr> }` captures the break expression's
  value in compiled mode. Previously
  `let x = loop { ... break sq }` returned 0 instead of `sq`.
- `result.map_err(closure)` and `result.map(closure)` dispatch
  correctly when the receiver type is unresolved at HIR time
  (e.g. `text.parse().map_err(...)?`). The closure was being
  built and silently dropped.
- String equality (`s == "literal"`, `s != "literal"`) routes
  through `gos_rt_str_eq`. Previously a pointer-compare that
  silently disagreed with interpreted output whenever the string
  came from a runtime helper rather than a literal-pinned slot.
- Reference deref (`*p` where `p: &i64` / `&f64` / `&bool` /
  `&char`) emits a real load instead of returning the pointer
  unchanged. Affected every iterator pattern that yields scalar
  references.
- `s.as_bytes()` returns a `Vec<i64>` shape (one zero-extended
  byte per slot) instead of a packed `Vec<u8>`. Compiled `bytes[i]`
  indexing now reads the byte's value through
  `gos_rt_vec_get_i64` rather than reading 8 packed buffer
  bytes as a single i64 (`reverse_string.gos` reproducer).
- `<chain>.method().to_string()` dispatches to the right runtime
  formatter (`gos_rt_i64_to_str` / `gos_rt_f64_to_str` /
  identity for strings) when the typechecker leaves the chain's
  HIR type as a `Var(_)`. Previously the identity-copy fallback
  fed an i64 to `gos_rt_str_concat` as a c_char* — segfault.
- Better error messages.
- Actual Error types.

### Added

- **`std::http` client now covers GET, POST, PUT, OPTIONS,
  DELETE, HEAD plus `http::request(method, url, body, headers)`
  for arbitrary methods.** ureq + rustls under the hood; HTTPS
  via Mozilla roots. Free-function wrappers (`http::get`,
  `http::post`, ...) and method-style on `Client` (`Client::post`,
  `Client::put`, ...) both round-trip through one
  `do_request(method, url, body, headers)` core. Unknown method
  strings return `Err(transport)`.
- **`http::stream(method, url, body, headers) -> ResponseStream`**
  for SSE / chunked bodies. `ResponseStream::next_line()` reads
  one line at a time from a `BufReader<Box<dyn Read + Send +
  Sync>>` over `ureq::Response::into_reader()`. Stream handles
  live in a process-wide registry keyed by i64 so they survive
  across `next_line()` calls. No temp files, no shell-out --
  this replaces the curl-and-poll pattern users were forced
  into for streaming.
- **Stdlib surface filled in.** `std::os` gained `cwd`,
  `set_cwd`, `set_env`, `unset_env`, `is_file`, `is_dir`,
  `is_symlink`, `file_size`, `remove_dir`, `remove_dir_all`,
  `copy`, `canonicalize`, `home`, `temp_dir`. `std::fs::metadata`
  returns a real `Metadata` struct. `std::net` exposes
  `TcpListener::{bind, accept, local_addr, close}`,
  `TcpStream::{connect, read, read_to_string, write, write_all,
  close}`, `UdpSocket::{bind, send_to, recv_from, local_addr,
  close}`, `net::resolve` / `net::lookup`. `std::sync` exposes
  `AtomicI64::{new, load, store, fetch_add, fetch_sub,
  compare_and_swap}`, `AtomicBool::{new, load, store,
  compare_and_swap}`, `Mutex::{new, lock, store}`, `Once::{new,
  call}`. `std::strings` adds `join`, `trim_start`, `trim_end`,
  `strip_prefix`, `strip_suffix`, `pad_left`, `pad_right`,
  `rfind`, `replacen`. `std::strconv` adds `parse_int`,
  `parse_i64`, `parse_u64`, `parse_float`, `parse_f64`,
  `parse_bool`, `format_int`, `format_i64`, `format_float`,
  `format_f64`, `itoa`/`atoi`. `std::time` adds `Instant::{now,
  elapsed_ms}`, `Duration::{from_millis, from_secs, from_micros,
  as_millis, as_secs, as_micros}`, `time::now_nanos`,
  `monotonic_ms`, `monotonic_nanos`, `since_ms`. `std::path`
  adds `parent`, `file_name`, `stem`, `ext`, `is_absolute`,
  `normalize`. `std::utf8` adds `count_runes`, `rune_len`,
  `is_valid`. `std::bufio` adds `read_to_string`,
  `read_lines_of`, `split_whitespace`.
  `std::collections::HashSet` was a HashMap stub; now a real set
  with `insert`, `remove`, `contains`, `len`, `is_empty`,
  `clear`, `to_vec`, `iter`.
- **Type-checker shifted left.** New `KNOWN_METHOD_NAMES` gate
  in `gossamer-types/src/checker.rs` rejects calls to method
  names that aren't bound at runtime (catches `as_bytes`,
  `and_then`, `filter`, `collect`, etc.) at `gos check` time
  with `GT0002` instead of letting them through to a runtime
  panic. User-defined `impl` methods are tracked separately so
  they're never falsely flagged.

### Performance

- Bytecode VM method-call IC hit path now takes a shared
  `RefCell::borrow()` instead of `borrow_mut()`. The cache is
  read-only on hit; the previous `borrow_mut()` serialised every
  call against any other borrow on the same RefCell.
- JIT tier-up threshold scales by chunk instruction count
  (`HOT_THRESHOLD_BASE * 50 / max(50, instr_count)`, floored at
  `HOT_THRESHOLD_FLOOR = 16`). Big functions now tier up after a
  handful of entries instead of waiting for 100 full calls of an
  expensive body. Honoured by `GOSSAMER_JIT_THRESHOLD` env var.
- Bytecode VM now decrements the JIT hot counter on backward
  `Op::Jump` and on the new fused `IncJumpIfLt/LeI64` ops.
  Loop-shaped chunks reachable only through their own internal
  control flow (rather than via repeated call entries) tier up
  on the same path.
- Cranelift `gos_rt_vec_len` is inlined as a null-check + offset-0
  load (matches the GosVec `repr(C)` layout). For-loop bounds and
  every `vec.len()` access in compiled code skip the C-ABI call.
- Per-thread shadow-stack root tracking is lock-free on the hot
  path. Owner reads/writes a 1024-slot `Box<[AtomicU32]>` with
  Relaxed stores and a Release-published `len`; the cross-thread
  mark snapshot Acquire-loads `len` and walks slots without
  taking any lock. Spillover into a `Mutex<Vec<u32>>` only when
  call depth exceeds the in-array capacity. The earlier design
  paid an uncontended `parking_lot::Mutex` lock+unlock at every
  function prologue and epilogue.
- For-range `for i in a..b { ... }` lowers to a header
  bounds-check + body + fused `IncJumpIfLt/LeI64` op that
  combines the per-iter `AddI64 + Jump` into one dispatch.
- `format!`, `panic!`, `eprintln!`, `eprint!` now build their
  message through the runtime's batched concat buffer
  (`gos_rt_concat_init` / `_str` / `_i64` / `_f64` / `_bool` /
  `_char` / `_finish`) instead of chaining N-1 pairwise
  `gos_rt_str_concat` calls. Eliminates the throwaway
  intermediate strings that the serial chain allocated and
  dropped between each pair of args.
- JIT trampoline `MAX_ARGS` raised from 8 to 12 (homogeneous
  i64-only and f64-only shapes for arities 9-12). HTTP handlers,
  multi-arg `format!` callees, and other 8+-arg helpers no
  longer fall back to bytecode purely because of arity.

### Stdlib parity

- `flag` stdlib fully wired in compiled mode. Default values for
  `int`, `float`, `duration`, `string_list`, `short`, `usage` are now
  honoured (previously every non-`string`/`uint`/`bool` flag silently
  zeroed). `parse` accepts the `=` form, short aliases, `--`, and
  `--help` / `-h`. Interp gained matching `float` / `duration` /
  `string_list` / `usage` builtins so both tiers produce identical
  output across every flag method.
- `flag::define(name, [flag::int(...), flag::string(...),
  flag::bool(...)])` (declarative one-shot constructor) now lowers
  to the imperative `flag::Set` builder chain at MIR time.
  Previously interp-only — compiled mode silently returned a
  null-shaped struct so `*flags.<long>` always yielded the
  primitive zero.
- `os::env`, `os::cwd` wired in both tiers. Compiled mode was
  returning `0` for every env var lookup and `0` for `cwd`.
- `fs::list_dir` wired in compiled mode (returns
  `Result<[DirInfo], Error>`).
- `time::Duration::from_secs` / `from_millis` lower in compiled mode.

### Test coverage

- `cargo test -p gossamer-cli --test parity --features
  exhaustive_tests --release` walks every example in
  `examples/*.gos` under both tiers and asserts byte-identical
  stdout/stderr/exit code. Two examples (`go_spawn.gos`,
  `list_dir.gos`) are listed in `KNOWN_DIVERGENT_EXAMPLES` with
  explicit root-cause comments — go_spawn requires a
  deterministic scheduler shared between tiers, list_dir
  requires registering `fs::DirInfo` as a stdlib struct in
  `gossamer-types::TyCtxt::register_struct_fields` at
  typechecker startup. Every other example round-trips.
- `crates/gossamer-codegen-cranelift/tests/correct/p51_flag_defaults`
  walks every flag type through interp + Cranelift + LLVM tiers.

## 0.0.1

### Stdlib parity

- `flag` stdlib fully wired in compiled mode. Default values for
  `int`, `float`, `duration`, `string_list`, `short`, `usage` are now
  honoured (previously every non-`string`/`uint`/`bool` flag silently
  zeroed). `parse` accepts the `=` form, short aliases, `--`, and
  `--help` / `-h`. Interp gained matching `float` / `duration` /
  `string_list` / `usage` builtins so both tiers produce identical
  output across every flag method.
- `flag::define(name, [flag::int(...), flag::string(...),
  flag::bool(...)])` (declarative one-shot constructor) now lowers
  to the imperative `flag::Set` builder chain at MIR time.
  Previously interp-only — compiled mode silently returned a
  null-shaped struct so `*flags.<long>` always yielded the
  primitive zero.
- `os::env`, `os::cwd` wired in both tiers. Compiled mode was
  returning `0` for every env var lookup and `0` for `cwd`.
- `fs::list_dir` wired in compiled mode (returns
  `Result<[DirInfo], Error>`).
- `time::Duration::from_secs` / `from_millis` lower in compiled mode.

### Compiler / codegen fixes

- Aggregate construction is now heap-allocated (`calloc`) instead
  of stack-slot. Returning a struct from a method (e.g.
  `Celsius { value: ... }.to_fahrenheit()`) no longer aliases the
  next call's stack slot; `temperature.gos` now matches across
  tiers.
- `loop { ... break <expr> }` captures the break expression's
  value in compiled mode. Previously
  `let x = loop { ... break sq }` returned 0 instead of `sq`.
- `result.map_err(closure)` and `result.map(closure)` dispatch
  correctly when the receiver type is unresolved at HIR time
  (e.g. `text.parse().map_err(...)?`). The closure was being
  built and silently dropped.
- String equality (`s == "literal"`, `s != "literal"`) routes
  through `gos_rt_str_eq`. Previously a pointer-compare that
  silently disagreed with interpreted output whenever the string
  came from a runtime helper rather than a literal-pinned slot.
- Reference deref (`*p` where `p: &i64` / `&f64` / `&bool` /
  `&char`) emits a real load instead of returning the pointer
  unchanged. Affected every iterator pattern that yields scalar
  references.
- `s.as_bytes()` returns a `Vec<i64>` shape (one zero-extended
  byte per slot) instead of a packed `Vec<u8>`. Compiled `bytes[i]`
  indexing now reads the byte's value through
  `gos_rt_vec_get_i64` rather than reading 8 packed buffer
  bytes as a single i64 (`reverse_string.gos` reproducer).
- `<chain>.method().to_string()` dispatches to the right runtime
  formatter (`gos_rt_i64_to_str` / `gos_rt_f64_to_str` /
  identity for strings) when the typechecker leaves the chain's
  HIR type as a `Var(_)`. Previously the identity-copy fallback
  fed an i64 to `gos_rt_str_concat` as a c_char* — segfault.
- Better error messages.
- Actual Error types.

### Test coverage

- `cargo test -p gossamer-cli --test parity --features
  exhaustive_tests --release` walks every example in
  `examples/*.gos` under both tiers and asserts byte-identical
  stdout/stderr/exit code. Two examples (`go_spawn.gos`,
  `list_dir.gos`) are listed in `KNOWN_DIVERGENT_EXAMPLES` with
  explicit root-cause comments — go_spawn requires a
  deterministic scheduler shared between tiers, list_dir
  requires registering `fs::DirInfo` as a stdlib struct in
  `gossamer-types::TyCtxt::register_struct_fields` at
  typechecker startup. Every other example round-trips.
- `crates/gossamer-codegen-cranelift/tests/correct/p51_flag_defaults`
  walks every flag type through interp + Cranelift + LLVM tiers.

## 0.0.0

Initial release. Not production ready.
