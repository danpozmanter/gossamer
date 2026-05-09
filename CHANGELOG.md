# Changelog

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
