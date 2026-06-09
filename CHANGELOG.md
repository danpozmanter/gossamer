# Changelog

## 0.11.0 — Process isolation, cross platform parity, block scoped defer and derive.

A panic in a spawned goroutine now terminates only that goroutine: the process keeps running and exits cleanly, on every tier (bytecode VM, Cranelift JIT, LLVM AOT). A panic on the main goroutine stays fatal, as in Rust — isolation is goroutine-scoped, not panic-swallowing.

- **Goroutine fault isolation, verified across tiers.** The compiled tier's `gos_rt_panic` contains a panic raised inside a goroutine (the M:N scheduler keeps running other goroutines) and the interpreter catches the runtime error in the goroutine thread. `crates/gossamer-cli/tests/process_isolation.rs` builds and runs both a panic-in-goroutine and a panic-in-main program on `gos run` and `gos build`, asserting the process survives the former (and that the goroutine genuinely panicked) and dies on the latter.
- **Buffered stdout is flushed before a fatal panic.** A main-goroutine panic aborts the process; `gos_rt_panic` now flushes the runtime's line-buffered stdout first — as `gos_rt_exit` already does — so output printed before the panic is no longer swallowed by `abort()`.

### Language features

- **Real `select { }` on the compiled tiers.** Cranelift and LLVM previously lowered `select` to an "arm 0 always fires" stub; they now poll arms in source order and park the goroutine until one is ready (or a `default` arm fires) via a new `gos_rt_select_*` runtime, matching the VM walker bit-for-bit. Send arms (`tx.send(v) => …`) now parse. Fixture: `feature-testing-examples/select_multiplex.gos`.
- **Block-scoped `defer` (Swift/Zig style).** The reserved-but-no-op `defer` now runs its expression when control leaves the enclosing `{ }` block — fall-through, `return`, `break`, or `continue` — in LIFO order, on every tier. A `defer` in a loop body runs each iteration. Example: `examples/defer_cleanup.gos`.
- **`let PAT = expr else { … }`.** Refutable-let-or-diverge, desugared to a `match` so it runs on every tier. Fixture: `feature-testing-examples/let_else_binding.gos`.
- **`#[derive(Clone, PartialEq, Eq, Default, Debug)]` for structs and enums.** Synthesizes the matching methods as real Gossamer source (the same parse-time path that derives JSON/TOML/YAML), so `==` / `!=` (field-wise), `.clone()`, `Type::default()`, and `{:?}` / `{}` (rendering `Name { field: value }`) work identically on the VM walker, Cranelift, and LLVM. Struct fields may be primitives, `String`, `[T]`, **nested structs**, and the struct may be **generic** (`struct Wrap<T>`). Enums derive too when their variants are all **tuple** (`Circle(f64)`) or **unit** (`Point`) — `Debug` renders `Circle(5.0)` and `Default` picks the `#[default]` variant. Example: `examples/derive.gos`; fixture: `feature-testing-examples/derive_traits.gos`. (Struct-payload enum variants are not yet derivable.)
- **Structs and tuples as `HashMap` / `HashSet` keys.** Keys are now compared and hashed by *value* on every tier: two equal-valued keys at distinct allocations share a slot, a re-insert overwrites, and a distinct key is a distinct slot. Works for flat structs (`struct Point { x, y }`), `String`-field structs, nested structs, and tuples. The compiled tiers hash the key's content via a per-slot layout descriptor (dereferencing `String` fields); the VM keys aggregates structurally — previously it collapsed every aggregate key into a single slot (`len()` of a struct-keyed map was always 1). `#[derive(Hash)]` is accepted on a key type. Fixture: `feature-testing-examples/struct_map_keys.gos`.

### Compiled-tier correctness fixes

- **Nested structs by value work on the compiled tiers.** A struct with a struct-typed field (`struct Outer { inner: Inner }`) read garbage for `o.inner.tag` under `gos build` / `--jit` (a 1-slot aggregate field was stored as a pointer and read back inline). Aggregate construction now inlines such fields; multi-slot, deeply-nested, by-argument, by-return, and mutated cases all match the VM.
- **Struct-returning functions no longer corrupt their drop-pass temporaries.** The RC drop inserter typed its throwaway locals from the return slot, so a function returning a struct produced an aggregate-typed `gos_rt_rc_release` destination and a `memcpy` from `null`. It now uses the interned `()` type.
- **Chained field access on a call result resolves its type.** `let a = mk(); a.inner.tag` defaulted the leaf type to a pointer (crash) when `a` came from a struct-returning impl method; copy-type propagation now flows through one field projection, and aggregate-returning callees are no longer inlined (which dropped the type).
- **`Option` / `Result` equality on the VM.** `Some(5) == Some(5)` returned `false` on the VM (variant values weren't compared); enum variants now compare structurally, matching the compiled tiers.

### Cross-platform parity

- **Windows: user functions returning `Result`/`Option`/inline-enum no longer miscompile.** The Win64 `<16 x i8>` fat-return ABI was applied to user-function calls, not just runtime shims; it is now gated to the ABI registry, with both LLVM call emitters routed through one `needs_win64_fat_ret` decision.
- **`gos build` works from a released install.** Every release artifact (tarball, zip, deb, rpm, Inno Setup, Docker) now ships `libgossamer_runtime.a` / `gossamer_runtime.lib`; the installer places it where `gos build` resolves it, and the cross-compiled Linux-aarch64 / macOS-x86_64 jobs build the runtime for their target.
- **mimalloc is the process allocator** on every platform and binary (toolchain and compiled programs), replacing the platform default — notably musl `malloc` on the static-musl release path.
- **Windows credential and multipart-upload files get an owner-only DACL**, the analogue of the POSIX `0o600` they already set; the write fails closed rather than leaving a world-readable file.
- **`pid_alive` is accurate on macOS and Windows** (`kill(pid, 0)` / `OpenProcess` + `GetExitCodeProcess`), so a stale build lock from a crashed `gos` is reclaimed instead of waiting out the deadline.
- **The native HTTP client uses happy-eyeballs**, racing all resolved addresses so an unreachable first record (commonly a filtered AAAA) falls through instead of stalling for the whole timeout.
- **`Child::kill_group` documentation corrected** on Windows (it terminates the lead process via `TerminateProcess`; there is no process-group signalling).

### CI

- **Cross-platform perf gate.** A new `perf-native` matrix job times a `gos build --release` native binary on Linux, macOS, and Windows, so an allocator/codegen regression is visible off Linux.
- **AddressSanitizer now runs on macOS** as well as Linux, giving the RC use-after-free / double-free suite portable coverage (glibc `MALLOC_CHECK_` was Linux-only).

## 0.10.0 — LLVM AOT tier completeness and soundness in the GC + fixes

Audit-driven sweep that closes 43 wiring gaps where features worked in the VM and Cranelift JIT but diverged under `gos build --release`. A new gauge — `crates/gossamer-cli/tests/llvm_aot_coverage.rs` — builds a binary per feature, runs it, and asserts stdout, so regressions surface as red bars instead of silent miscompiles.

### Cross-platform native-build fixes

- **macOS native binaries no longer crash on string literals.** Header-carrying string constants (`<{ i32 len, i8 0xA8, [N x i8] }>` with a `base+5` body alias) were emitted `unnamed_addr`, so the Mach-O backend filed the 4/8/16-byte ones into the mergeable `__literal{4,8,16}` pools. ld64 coalesces and reorders literals there and ignores the interior `.alt_entry` body symbol, so the alias resolved into the wrong literal and the runtime read a corrupt length/tag header — SIGSEGV/SIGBUS on essentially every program with a short format fragment. The backing constant is now a plain `constant` (address-significant → `__const`, stable interior symbols on every target). Guarded by a unit test that rejects `unnamed_addr` on header string constants.
- **Windows-GNU native linking.** `gos build` drives mingw's `cc` directly, so unlike a rustc-driven link it must name the libraries the runtime needs that mingw's default specs don't auto-link. `-ldl` (which mingw has no library for) is now gated to Linux only, and the Win32 import libs `ws2_32` / `bcrypt` / `advapi32` / `userenv` / `ntdll` are added on Windows. The same fix is applied to the Cranelift crate's `native.rs` link check.
- **Windows native binaries no longer corrupt `Result` / `Option` / `Vec` across the runtime boundary.** The compiled tier carries every 2-word aggregate as a scalar `i128` (`AbiType::I128`), but a by-value `i128` has no shared `extern "C"` ABI on Win64: `llc` passes it in a GP register pair and returns it there, while rustc — which compiles the runtime — passes an `i128` argument *by pointer* and returns it in a `<16 x i8>` vector register. Every `gos build` binary therefore read a corrupt discriminant/payload on Windows (wrong output, or a SIGSEGV from a payload pointer read out of the low word); SysV happens to agree, so Linux/macOS were unaffected. The LLVM tier now emits the rustc-matching shape on Windows — an `i128` argument is spilled to a 16-byte-aligned slot and passed as `ptr`, an `i128` return is called as `<16 x i8>` and `bitcast` back — at every runtime-call emission site (the two central emitters plus the inline `gos_rt_vec_push_i128` fast path that pushes a `Result`/`Option` into a `Vec`), routed through one `fat_i128_call_arg` helper so a future site cannot silently diverge, with `RuntimeEntry::llvm_declare` rendering the matching declaration. No runtime, registry, or non-Windows codegen changes; verified by comparing `llc -mtriple=x86_64-pc-windows-gnu` output against rustc's ABI, and guarded by `gossamer-abi`'s `win64_marshals_fat_i128_across_the_ffi_boundary` test. This is the complete surface: a 2-word aggregate only crosses the runtime `extern "C"` boundary as a machine value in the LLVM AOT tier (now fixed). The bytecode interpreter calls the runtime as in-process Rust with no FFI boundary, and the Cranelift JIT does not compile `i128`-shaped bodies at all (no `JitKind::I128`; Cranelift panics on an `i128` argument/return without `enable_llvm_abi_extensions`), so such bodies fall back to the interpreter — correct on every platform. `gos run` of a fat program (`result::default_with`, `hex::decode`) produces correct output on Win64 through that path, JIT forced on or off.
- **Runtime staticlib is published atomically.** `gossamer-cli`'s build script copies the ~300 MB `libgossamer_runtime.a` into `target/<profile>/` for non-cargo linkers (`gos build`, the Cranelift `native.rs` link tests). The copy was a plain `fs::copy`, which truncates the destination and streams the bytes; because the script re-runs whenever a `GOS_*` env var changes (the diagnose CI step sets several), a parallel test reading the archive mid-write hit `ld: failed to set dynamic section sizes: file truncated`. The publish now copies to a per-pid temp file and `rename`s it into place, so a reader always sees a complete archive. (Surfaced because the `native.rs` link helper no longer silently skips link failures — see below.)
- **Native-build test diagnostics.** Link errors on a supported platform now fail loudly with the full `cc` stderr instead of silently skipping (a silent skip hid the `-ldl` break); `GOS_LINK_VERBOSE` echoes the resolved linker line + libraries; `GOS_KEEP_BUILD_ARTIFACTS` / failing three-tier harnesses preserve sources, objects and binaries for CI artifact upload; and exit codes are rendered as their cause (`killed by signal 11 (SIGSEGV)`, `exit code 0xC0000005 (STATUS_ACCESS_VIOLATION)`) rather than an opaque number.

### Compiled-tier reference counting replaces the tracing GC

The compiled tiers (Cranelift JIT, LLVM AOT) now manage recursive heap-enum lifetime with intrusive reference counting — matching the interpreter's `Arc`-payload semantics — instead of the raw-pointer tracing collector, which was unsound under `opt -O3` (live roots are not precisely discoverable) and leaked or crashed on tree-shaped heaps. Soundness is verified across aliasing, struct-embedding, return-of-argument, container, and payload-variant cases under glibc's `MALLOC_CHECK_=3`.

- **Intrusive RC runtime** (`gos_rt_rc_alloc` / `_retain` / `_release`, `c_abi/rc.rs`): every heap object carries a strong count plus a flat `[i64]` child-layout descriptor; release is iterative so deep structures cannot overflow the runtime stack. User enum constructors allocate through it, and a per-variant descriptor is emitted once as a module constant in both backends.
- **Balanced retain/release insertion** (`gossamer-mir`): retain on every aliasing copy / field store / aggregate / container insert, release every owned local at scope exit, with move elision so the construct-and-return pattern costs zero refcount traffic. Interior borrows (match bindings, accessor results) are never released.
- **Per-call tracing-GC instrumentation removed.** The shadow-stack save/push/restore and safepoint hooks previously emitted on every function call are gone (the collector they fed is superseded by RC). Hot leaf-math loops return to native parity after a large release-mode regression, and recursive-enum allocation workloads run several times faster.
- **Two latent optimizer miscompiles fixed** (`gossamer-mir::opt`): `const_value_of` and `copy_propagate` both treated a local's first constant assignment as its value, ignoring a later reassignment — a use after the reassignment could fold a live heap pointer to null.
- **Incremental object cache now keyed by compiler fingerprint.** The per-body LLVM object cache hashed only the MIR, target, and opt profile — so a rebuilt compiler that emits different IR for identical MIR (e.g. after the tracing-GC removal) silently reused stale objects, surfacing as link failures against removed runtime symbols or as "fixed-but-still-slow" binaries. The key now mixes the package version and the compiler executable's size + mtime.
- **Dead tracing-GC machinery removed.** The raw-pointer collector, shadow-stack roots, safepoint/write-barrier shims, allocation registry, and per-call instrumentation are deleted from the runtime, ABI registry, and codegen; the aggregate allocators (`gos_rt_aggr_alloc`/`_free`) and the deterministic drop pass remain. `std::runtime::gc_collect()` is retained as a no-op (RC reclaims automatically). A `--release` performance canary (`tests/perf_canary.rs`) guards against per-call-overhead regressions in the hot scalar path.

### RC for container / Result-nested enums

Four drop-pass / RC bugs that corrupted or miscompiled recursive enums carrying `Vec`, tuple, and `Result` payloads — the shape of a JSON-value tree — are fixed. Covered by `crates/gossamer-cli/tests/rc_nested_containers.rs`.

- **Loop element borrows are no longer released.** A `for x in xs` element loaded through a terminator-position `gos_load` (block boundary, not the `CallIntrinsic` form) was treated as owned and released each iteration, freeing the container's elements. `gos_load` / `gos_store` in terminator position are now recognised as borrows.
- **A `Vec` stored into a returned enum survives.** The drop pass freed a `Vec` local at return even after it was stored into a returned `J::Arr(v)`; the escape analysis now follows `gos_store(obj, off, val)` into an escaping object.
- **Deep container nesting composes.** `outer.push(J::Arr(inner))` then `J::Arr(outer)` lost the innermost `Vec` because the `vec_push` and `gos_store` escape rules ran in separate passes; they are now one fixpoint.

### By-value `Result` / `Option` and inline enum payloads

`Result<T, E>` and `Option<T>` are now a 2-word by-value `i128` (`[disc, payload]`) rather than a heap-boxed `*mut GosResult`. The box was allocated on every `Ok` / `Err` / `Some` / `None` and never reclaimed — an unbounded leak on every `?`. `ast` / `json` / `gc` workloads are unaffected and output is bit-identical across all tiers.

- **2-word representation** (`AbiType::I128`; `render_ty` and the Cranelift layout map the sentinel ADTs to `i128`; `pack_result` / `gos_rt_result_disc` / `gos_rt_result_payload` in `c_abi/vec.rs`): discriminant in the low word, payload (a scalar inline, or a pointer to a larger value) in the high word. The `?` desugar, `match`, field access, and the `result::*` / `option::*` combinators read and build it directly; `is_rc_managed` reports these as values, never RC pointers.
- **16-byte `Vec` / array elements.** A by-value `Result` / `Option` element occupies two slots: `slot_count` / `type_slot_bytes` / `aggr_size_bytes` report two slots for the sentinels, with `gos_rt_vec_push_i128` / `gos_rt_vec_get_i128` and matching push / index / for-loop element reads. `regex::captures` / `captures_all` (returning `Vec<Vec<Option<String>>>`) round-trip bit-identically across the VM, Cranelift, and LLVM tiers.
- **Inline enum payloads.** A user enum whose every variant has at most one field that fits in a single 8-byte slot (scalar / `String` / `Vec` / map / handle — the shape of a JSON-value enum) uses the same 2-word by-value representation: construction packs the discriminant and field with no heap node, `match` reads the discriminant from the low word, and the single field is the high word. Multi-field variants (e.g. a tree node) keep the heap-node representation.
- **Payload-less variant singleton.** A no-field variant (`Tree::Leaf`, `JsonVal::Null`, …) returns one process-pinned, globally-allocated per-discriminant node instead of allocating a fresh node per construction (the node is shared and never mutated).

### `for x in vec` single-slot element read

A `for`-loop over a `Vec` of single-slot, non-float elements (i64 / bool / `String` / handle) reads each element with one `gos_rt_vec_get_i64` instead of `gos_rt_vec_get_ptr` + `gos_load` (two runtime calls), halving the per-element call overhead on adjacency-style iteration (graph-bfs).

### HTTP server: per-request memory leak fixed

The compiled HTTP server leaked every request's `Ok(Response)` result box. Per-request reclamation had relied on a per-worker arena reset (`gos_rt_gc_reset`) that became a no-op when the bump arena was retired, and on the tracing GC that the reference-counting migration removed — so `gos_rt_result_new`'s `Box::into_raw` was never freed. Under load the server grew unboundedly; `drop_handler_result` now frees the result box after the response is written. 

### Lenient out-of-bounds indexing parity (VM matches compiled)

The interpreter aborted on an out-of-range index while the compiled tiers return the element zero value; `gos run` now matches `gos build` (any index outside `[0, len)` yields the zero value, no panic), so the two tiers are bit-identical on out-of-bounds access.

### Optimizer attributes on runtime declarations

Every `gos_rt_*` LLVM declaration now carries `nounwind` (correct: an `extern "C"` boundary aborts rather than unwinds, so the call never throws), and an audited set of pure getters (`vec_get`, `vec_len`, `arr_len`, `str_len`, `str_byte_at`, `str_eq`, `heap_i64_get`) additionally carries `memory(argmem: read)`. Without these, LLVM treated every runtime call as a potential exception edge and a full memory clobber, blocking reordering, hoisting, and CSE of surrounding loads/stores; the attributes let `opt` move loop-invariant runtime reads out of loops.

### Reference-counting memory-footprint fixes

Three coordinated fixes cut compiled-tier RAM on heap-heavy workloads. A named local bound to a recursive heap value and rebuilt each loop iteration was leaking every iteration's value until the function returned; a pathological loop that should hold ~11 MB held 863 MB. Covered by a new named-binding-loop RSS regression test (the prior test only exercised the temporary shape, which already released).

- **Release before reassignment, not only at return.** Owned reference-counted locals are now released before *any* reassignment (including the loop back-edge), not just before a fresh allocation. A `let t = build(d)` rebuilt each iteration frees the previous tree instead of accumulating all of them. The entry zero-init keeps the first release null-safe.
- **16-byte object header.** `RcHeader` shrank from 24 to 16 bytes (`strong` and `size` are now `u32` — 4 billion live refs / 4 GiB objects are unreachable ceilings), so a `Node(i64, Box, Box)` is 40 bytes instead of 48.
- **Byte-budgeted recycling pool.** The thread-local free-list is now capped by a 4 MiB-per-class byte budget instead of a flat 65k-block count, so a large size class can no longer pin tens of MiB of cached blocks.

### Container element ownership + per-iteration `Vec` reclaim

A string or nested container stored in a `Vec` no longer leaks, and a `Vec` rebuilt each loop iteration is reclaimed instead of accumulating.

- **No per-push element clone.** `gos_rt_vec_push` copied each STRING element into a vec-owned buffer (a value-semantics relic), while the drop pass separately retained the caller's original — so that original leaked once per push. Elements are now held by reference: the compile-time RC (retain at insert, `elem_kind` deep-free at container drop) owns each exactly once, the same model as struct fields. `string_in_vec` / `nested_vec_string` drop from O(n) live strings to O(1).
- **Loop-local `Vec` freed per iteration.** A `Vec` constructed in a loop body was freed only at function return, leaking every prior iteration's container and its elements. The drop pass now frees the previous value before each constructor reassignment (null-safe via an entry zero-init) and at each return, conservatively skipping any container that escapes into another container or the return value. A deterministic per-family allocation ledger (`c_abi/ledger.rs`, `GOS_LEAK_LEDGER`, unix) backs the leak-shape gate.
- **`HashMap` insert releases its inbound strings.** `gos_rt_map_insert_str_*` / `_i64_str` copy the key/value bytes into the map's own storage, so the consuming-call contract leaves the caller's `format!(...)` key/value as a leaked temporary. The runtime now releases each inbound gos-string after copying (rc-aware + tag-checked, so a moved temp is freed, a shared string is only decremented, and a literal is skipped).
- **Fresh string producers are owned.** `str_repeat` / `slice` / `substring` / `trim*` / `replace*` / `pad_*` / `to_upper`/`to_lower`/`to_title` return a freshly allocated owned `String`, so a standalone transient (`let big = strings::repeat(…); use(&big)`) is released at scope instead of leaking. A returned producer result is exempted (it flows to the caller). The substring-retention leak benchmark goes from ~290 MB to flat ~0.6 MB. Deliberately excludes `concat` (in-place-aliasing in `s += …`) and `Result`/`Option` payload extraction.

- **Loop-local `HashMap` / `HashSet` reclaimed.** `let m = HashMap::new()` lowered to `tmp = map_new(); m = Copy(tmp)`; the copy pinned the constructor result as aliased, so the reuse pass never reclaimed a loop-local map. Container constructors (and `Some` / `Ok` / `Err`) now write the binding directly — no copy, no alias — so a loop-local map / set is freed per iteration like a `Vec`. A map passed to a user function stays safe via the existing escape disqualification.
- **By-value enum payload extraction is a move.** A `String` moved out of a consumed `Result` / `Option` (`let s = f()?`, `r.unwrap()`, `match o { Some(s) => … }`) transfers the enum's single owning reference to the binding instead of retaining a second, so the binding releases it exactly once. When the extracted value is instead stored into an aggregate — the synthesized `from_json` parses a field `String` and places it in the result struct through copy temporaries — the retain is load-bearing and kept, detected by propagating "stored into an aggregate" transitively backward through copy edges. An aliased enum (`let o2 = o; match o2; match o`) is conservatively not owned (leak-not-double-free), and autoderive `from_json` / `to_json` round-trips clean under `MALLOC_CHECK_=3`.
- **`?` / match payload typing.** The extracted payload type is recovered from the scrutinee enum's substitution (the declared variant field type is the generic default, often `i64`), and concrete types are propagated forward through `Copy` chains, so a `?` extraction copied into an otherwise-`Var` binding is recognised as RC-managed and released.
- **Leak ledger no longer counts region-managed strings.** A string allocated inside an arena region is reclaimed wholesale at `region_pop` (and skipped by `gos_rt_str_free`), so an unmatched `str_inc` made the `GOS_LEAK_LEDGER` gauge report a false positive on region-heavy loops; region strings are no longer counted in the per-string gauge (the memory is bounded by the region).

### Arena regions wrap only allocating loops

- A loop body is wrapped in an arena region (`region_push` / `region_pop`) only when it actually allocates a heap value. A purely-scalar inner loop (a counter scan, byte stores) previously paid two region calls every iteration for nothing; eligibility now also requires a heap-allocating call or constructor in the body. Allocating loops stay regioned and bounded.
- A tuple field read out of a fixed array (`table[j].1`) lowers to a single combined index+field projection instead of materialising the whole tuple to extract one field, and `buf.set_byte(i, x)` lowers to an inlined branchless bounds-guarded store in the LLVM tier instead of a per-byte runtime call.

### Length-carrying strings — O(1) `len`/`slice`

Compiled-tier strings now store their byte length in the allocation header, so length and slicing are O(1) instead of `strlen`-per-call. A recursive-descent parser that slices a large input at growing offsets was O(n^2); **json-serde drops from 167s to 0.54s at N=50000** (now linear, output bit-identical to the Rust reference).

- Heap strings (`format!`, `slice`, file reads, every `alloc_cstring` caller) use the length-carrying builder layout, so `gos_rt_str_len` reads the stored length at `ptr[-5]`; foreign pointers fall back to `strlen`.
- `gos_rt_str_slice` bounds-checks against the O(1) length and copies the range directly — the safe out-of-bounds `Err` contract is preserved (no UB fast path).
- LLVM string literals emit a length-carrying header (`<{ i32 len, i8 tag, bytes }>`) with a global alias at the body, so literal references are unchanged while their length is O(1) too.
- C interop is unchanged: the body pointer still points at NUL-terminated bytes (the length header sits before it).

### `gos clean` removes build artifacts + caches

`gos clean` now also removes the project `target/` directory and the
per-project `.gos-cache` incremental IR-object cache (previously it dropped
only the frontend cache). `--dry-run` reports without deleting; `--vendor`
additionally drops `vendor/`. Idempotent — absent targets are noted and
skipped.

### Recycling RC allocator (thread-local slab)

`gos_rt_rc_alloc` / release now route small RC objects through a per-thread, lock-free size-class free-list that recycles freed blocks instead of round-tripping through libc `malloc`/`free` on every node. Allocation-heavy workloads (recursive-enum trees) roughly halve: the gc-trees stress test drops from ~20s to ~12s. The pool returns surplus blocks to the OS at a per-class cap and frees its cache on thread exit (so the HTTP server's per-connection threads don't leak); `GOS_RC_NO_POOL=1` disables it so `MALLOC_CHECK_` retains full double-free detection in the soundness tests.

### `String::byte_at` interpreter binding

`s.byte_at(i) -> i64` was wired through the compiled tiers but unbound on the interpreter. It is now a registered `String` method on every tier (the UTF-8 byte at `i`, or 0 out of range), matching `gos_rt_str_byte_at`.

### Generic-struct field types + `impl` method `self` typing

Three coordinated typechecker fixes ground inference results that previously leaked unresolved `Var`s into lowering, where the compiled tier defaulted them to i64/ptr and mis-stringified values.

- **Unsuffixed float literals default to `f64`.** `InferCtxt` gained a `float_literal` var flavour (mirroring the integer-constrained flavour) plus `default_unresolved_float_vars`. A bare `3.0` fed into a generic position (`Triple { third: 3.0 }`) previously left its inference var unbound; the field then printed the value's IEEE-754 bit pattern through `gos_rt_concat_i64` (`4613937818241073152` instead of `3`). Float literals now take their use-site width when constrained and fall back to `f64` otherwise.
- **`deep_resolve` recurses into `Adt` substs.** The end-of-typecheck zonk only grounded `FnPtr` / `FnTrait` sigs, so a `Triple<?, ?, ?>` whose vars unified to `<i64, String, f64>` stayed recorded with unresolved substs. It now resolves each `Adt` type argument, so a generic struct's field access substitutes the concrete type.
- **`impl` method `self` binds to the concrete `Self` type.** The receiver was bound to a fresh inference var, so `self.field` reads left the field type unresolved — a `for x in self.items` over a `[String]` field bound `x` at the i64 default (the auto-derived `to_json` serialised a `[String]` field as integer pointers: `["2100555", …]`). `self` now binds to the impl's `Self` (wrapped in `&` / `&mut` for `&self` / `&mut self` receivers).

### Native bytecode-VM `match` compilation

The bytecode VM (`gos run`) now lowers `match` expressions to a native test-and-branch chain instead of routing every arm evaluation through the bundled tree-walker via `Op::EvalDeferred`. Across the example suite the walker-fallback count drops sharply (`shapes.gos` 20 → 0, `temperature.gos` 18 → 0, `json_structs.gos` 24 → 4).

- **Three new opcodes** — `VariantIs` (enum/tuple-struct name + arity test), `VariantField` (positional payload extract), and `StructIs` (struct-name test) — back the pattern tests; literals compare via `Eq`, ranges via `Ge`/`Le`/`Lt`, tuple/struct fields project via the existing `TupleIndex` / `FieldGet` ops.
- **`compile_match` + `emit_pattern_test`** lower every native-expressible pattern shape: wildcard, binding, literal, range, enum variant (with nested payload patterns), tuple (including a `..` rest), struct (with field-shorthand binding), `&`-ref, `@`-binding, and or-patterns of non-binding alternatives. Guards compile inline after the pattern test.
- **Fallback preserved** — an or-pattern that introduces bindings still routes the whole `match` through the walker, so semantics stay correct while the common 95% runs natively. (Closures, `go`, and `select` remain walker-evaluated; the walker is not yet deleted.)
- **`get()` bare-name router** — exercising `match` scrutinees natively exposed a latent dispatch collision: `install_module("json", …)` registered `("get", builtin_json_get)` after the HashMap getter, so a natively-evaluated `m.get(&k)` returned `None` and `match m.get(&k) { Some(v) => … }` always took the `None` arm. A receiver-dispatching `builtin_get_router` (mirroring the `keys`/`values` routers) sends `Map`/`IntMap` receivers to the map getter and struct/json receivers to the json getter.
- **Compiled-tier tuple-match binding** — `match` on a tuple whose element types inference left loose (`let pair = (10, "hi")`) bound each element through a pointer-shaped local, so the `println!` arg dispatcher routed the `i64` element through `gos_rt_concat_str` and strlen'd the integer → segfault. The MIR tuple-pattern lowering now recovers each element type from the sub-pattern when the tuple's recorded type is unresolved.

### Free-fn dispatch wired through MIR

- `strconv::parse_i64` / `parse_f64` / `parse_bool` / `parse_u64` / `atoi` / `format_i64` / `format_f64` / `format_bool` / `itoa` — new `gos_rt_strconv_*` shims (`c_abi/strconv.rs`) with Result-shaped payloads where the VM returns Result.
- `strings::trim` / `trim_start` / `trim_end` / `split` / `to_upper` / `to_lower` / `contains` / `replace` / `starts_with` / `ends_with` / `lines` / `find` / `repeat`.
- `math::tan` / `asin` / `acos` / `atan` / `atan2` / `sinh` / `cosh` / `tanh` / `log2` / `log10` / `cbrt` / `round` / `exp2` / `fmod` / `hypot` / `copysign` / `dim`.
- `path::parent` / `stem` / `file_name` — new Option-returning shims.
- `env::set_var`, `env::program_name` (registry entry was missing), `crypto::rand::bytes` (new `getrandom`-backed shim), `fs::metadata`, `time::Duration::as_millis` / `from_micros` / `as_secs` / `as_micros`, `sync::AtomicBool::new` / `sync::AtomicU64::new` (alias to AtomicI64).
- `encoding::xml::escape`, `encoding::base32::encode` / `encode_string` / `decode_string`, `encoding::base64::encode` / `decode`, `encoding::hex::encode` / `decode`, `html::escape` / `unescape`, `compress::flate::compress` / `decompress`, `compress::zlib::compress` / `decompress`, `crypto::hmac::sha256_mac`, `result::default_with` — previously emitted an undefined `@module::fn` reference at the `opt` stage of `gos build`. New `gos_rt_*` shims (`c_abi/encoding.rs`, plus flate/zlib in `c_abi/gzip.rs`, hmac in `c_abi/crypto.rs`), MIR dispatch arms, and ABI-registry entries lower them across the compiled tiers. A `Vec<u8>` is stored i64-per-element (each byte zero-extended to an 8-byte slot), and the byte readers/builders in the new shims respect that. Acceptance gate: `crates/gossamer-cli/tests/stdlib_lowering.rs` builds + runs a probe per function.

### More VM-only stdlib surface wired through MIR

A reverse audit (interp-registered builtins with no compiled-tier lowering) found a large further set of `module::fn` calls that ran under `gos run` but emitted an undefined `@module::fn` symbol at the `opt` stage of `gos build`. The `dispatch_parity` test only checks the runtime→codegen direction, so this whole class was ungated. Each function below now has a `gos_rt_*` shim, an ABI-registry entry, and a MIR dispatch arm, and is exercised by a `feature-testing-examples/` fixture that asserts bit-identical stdout across VM / Cranelift / LLVM.

- **`strings`** — `splitn`, `split_whitespace`, `fields`, `replacen`, `to_title`, `trim_matches`, `pad_left`, `pad_right`, `contains_rune`, `contains_any`, `equal_fold`, `index_rune`, `index_any`, `last_index_any`, `strip_prefix`, `strip_suffix`.
- **`path`** — `clean`, `normalize`, `is_absolute`, `has_prefix`, `extension` (aliases the existing `ext` Option shim).
- **`time`** — `sleep`, `now`, `unix_ms`, `now_nanos`, `monotonic_ms`, `monotonic_nanos`, `since_ms` (monotonic shims already existed; these route the language-level calls plus new epoch-nanos / since shims).
- **`hash`** — `crc32::{checksum, checksum_string, update}`, `adler32::{checksum, checksum_string, update}`, `fnv::{hash32, hash64, hash_string}` (new `c_abi/hash.rs`).
- **`math::bits`** — scalar primitives `count_ones`, `count_zeros`, `leading_zeros`, `trailing_zeros`, `reverse_bits`, `reverse_bytes`, `len`, `rotate_left`, `rotate_right`.
- **`os` / `fs`** — `copy` (Result<i64>), `canonicalize` (Result<String>).
- **`crypto::subtle::constant_time_eq`** — length-aware constant-time byte compare.
- **`encoding::ascii85`** — `encode`, `decode`.
- **`encoding::utf16`** — `is_surrogate`, `rune_len`, `decode_surrogate_pair` (Option<char>), `encode_string` ([u16]), `decode_to_string`. The interp registration was also fixed to bind the canonical `encoding::utf16::*` path (it previously only bound the bare `utf16::*` form, so `use std::encoding; encoding::utf16::…` failed in the VM too).
- **`encoding::binary`** — `put_u16/u32/u64_be/le` ([u8]), `get_u16/u32/u64_be/le` (Result<i64>), `uvarint` / `varint` (Result<(i64, i64)>).
- **`encoding::csv`** — `parse_line` ([String]), `read` (Result<[[String]]>), `write` (String). Exercises the nested `Vec<Vec<String>>` representation across the by-value-aggregate ABI.
- **`bufio`** — `read_to_string` (Result<String>), `read_lines_of` (Result<[String]>), `split_whitespace`.
- **`net`** — `resolve` / `lookup` (Result<[String]>).

The carrying `math::bits::{add, sub, mul, div}` and the `utf8::{decode_rune, decode_rune_in_string, decode_last_rune, decode_last_rune_in_string}` family return by-value tuples (`(i64, i64)` / `(char, i64)`); `utf8::append_rune` returns `[u8]`. These exercise the compiled-tier by-value-aggregate ABI — a runtime helper returns a GC-allocated multi-slot heap buffer that the caller memcpys into its destination, the same shape user-defined tuple/struct returns already use across both backends.

### Struct-returning stdlib functions via injected real-struct wrappers

The last VM-only class was stdlib functions that build or return a *named struct* (`pem::Block`, `x509::CertInfo`, `tar::TarEntry`, `zip::ZipEntry`). Rather than a fragile sentinel-DefId opaque handle (which disagrees with the multi-slot inline layout the compiled tier gives real structs), each is wired through the serde-autoderive precedent: `gossamer-parse` injects real Gossamer `struct` + wrapper-fn source, and a `VisitorMut` rewrites the public call/type sites (`pem::decode`, `x509::CertInfo`, …) to the mangled wrappers. Each wrapper calls a leaf intrinsic that returns the proven tuple / `[u8]` ABI shapes; the wrapper folds the tuple into the real struct, which then constructs, indexes, and field-accesses identically on every tier.

- **`encoding::pem`** — `decode` / `decode_all` / `encode` over a real `Block { block_type, bytes }`. Leaf intrinsics `gos_rt_pem_decode_raw` (`Result<(String, [u8])>`), `gos_rt_pem_decode_all_raw` (`Result<[(String, [u8])]>`), `gos_rt_pem_encode_raw`.
- **`crypto::x509::parse_pem`** — `Result<CertInfo, Error>` over a real 7-field struct, via a single `gos_rt_x509_parse_pem_raw` leaf returning a 7-slot `(subject, issuer, serial, not_before_unix, not_after_unix, san_dns, sha256)` tuple. The runtime shim reuses `x509-parser` + `sha2` so the compiled tier matches the VM byte-for-byte.
- **`archive::tar` / `archive::zip`** — `read` returns `[TarEntry]` / `[ZipEntry]` (each `{ name, data, is_dir }`) via a `[(String, [u8], bool)]` tuple-vec leaf; `write([(String, [u8])])` returns `Result<[u8]>` directly (no struct). Runtime shims use the `tar` / `zip` crates.

Three general codegen fixes fell out of this work and benefit all user structs, not just the stdlib wrappers:

- **`[u8]` / `[T]` field method dispatch** — a struct extracted from a `Result` (`match Ok(q) => q.bytes.len()`) lost contact with its field types, so `.len()` on a `[u8]` field dispatched to `strlen` and read the i64-per-element Vec as a C string (returning 1, or crashing on a misaligned pointer). The method-call lowering now recovers the field's declared type from the parent struct's `Adt` def — ground truth — instead of the wrongly-resolved HIR type.
- **Array-literal struct fields coerce to heap Vec** — `Q { bytes: [1, 2, 3] }` where `bytes: [u8]` stored the 3-slot inline array straight into the 1-slot Vec field, overflowing the aggregate. The struct-literal lowering coerces an array-literal value to a `GosVec` when the field is declared `[T]` / slice.
- **Field-access type recovery** prefers the struct's declared field type whenever the receiver is an `Adt` with known fields, not only when the HIR type is an unresolved `Var`.
- **Array/tuple-literal arguments re-type to the parameter.** A literal argument is re-recorded against the callee parameter type, so a nested `[1, 2, 3]` byte array inside a `(String, [u8])` tuple inside a `[(String, [u8])]` parameter (the `archive::tar`/`zip` `write` shape) is typed as a heap Vec at every level rather than a fixed `[i64; N]` — the compiled tier then lays out the same heap structure the runtime shim reads. A per-body pre-scan extends this through a `let` binding (`let files = […]; tar::write(files)`): the binding whose value flows into such a call is re-typed up front, the backward inference the single-pass checker can't otherwise reach.

### Method dispatch fallthroughs

- `HashMap::contains` aliases `contains_key`; `BTreeMap::get` / `contains` / `contains_key` — three new btmap shims.

### Result<f64> bit-pattern preservation

- `Ok(f64)` packs via `gos_rt_result_new_f64` (`to_bits`) and unpacks via `gos_rt_result_payload_f64` (bit reinterpretation). The prior path went through `fptosi`/`sitofp` and silently truncated `3.5` to `3`.

### Closure ABI through unified Fn trampoline

- `gossamer-hir::lift_closures` now pins unresolved (`Var`/`Error`/`Param`) closure param + return types to `i64` after the lift pass. LLVM was emitting `__closure_N(ptr) -> ptr` for `|n| println!("{}", n)`-style closures while the trampoline called them as `(i64) -> i64`; the ABI mismatch segfaulted inside `iter::for_each` / `option::map` / `result::map`. New `gos_rt_option_map_i64` / `gos_rt_result_map_i64` complete the map surface for Some/Ok payloads.

### Silent miscompiles closed

- `let mut xs = [1, 2, 3]; xs.push(4)` — MIR's let-lowering promotes `mut` array-literal bindings to `Vec<T>` so `.push` / `.sort` / `.iter` don't write through a stack `[i64; N]` interpreted as a `GosVec` header.
- `gos_rt_set_args` captures `argv[0]` whenever `argc >= 1` (was gated behind `argc > 1`), so `env::program_name()` returns the binary path even when run with no user args.
- `gos_rt_crypto_rand_bytes` writes the requested length into the `GosVec` header after filling the buffer.
- `regex::captures_all` / `captures` build canonical `Option<String>` capture groups. The runtime pushed a bare c-string pointer (or 0) per group, but each group's source type is `Option<String>`; when the element typed as a concrete `Option<String>` (e.g. through a function whose declared return is `[[Option<String>]]`), the compiled-tier `match group { Some(k) => …, None => … }` read the tagged-union discriminant (`gos_rt_result_disc`) off the pointer and saw a c-string's first bytes as garbage, so the match fell through and produced no output. The runtime now pushes `gos_rt_result_new(disc, payload)` Options and the MIR pins the result element to `Option<String>` (`captures_all` → `Vec<Vec<Option<String>>>`, `captures` → `Option<Vec<Option<String>>>`, and the `for row in captures_all(…)` element to `Vec<Option<String>>`).

### Coverage gauge

- `tests/llvm_aot_coverage.rs` — 43 round-tripped tests, 0 ignored. Each test pins a behaviour the audit found broken; the suite is the regression gate for future LLVM-tier work.

### `&mut T` deref-assign and `&mut self` field mutation

Three coordinated fixes close a class of LLVM AOT segfaults / silent miscompiles where `&mut scalar` was passed as an i64-as-ptr and `*s = expr` was silently dropped.

- **`*place = expr` (deref-assign) routes through a Place with `Projection::Deref`** — `gossamer_mir::lower::builder::expr::lower_place_expr` gained a `HirUnaryOp::Deref` arm that appends a `Projection::Deref` step. Previously the match defaulted to `None`, so `lower_assign` silently returned without emitting any store, and the program silently dropped the entire assignment.
- **LLVM `lower_place_address` skips its prefix auto-deref when the first projection is itself `Deref`** — the auto-deref exists for the common shape `let r: &T = &x; r.field` (loads the local's pointer slot once before walking field offsets). When `*r = expr` arrives with `Place { local: r, projection: [Deref] }`, both the auto-deref and the explicit `Deref` would fire — the second load reads garbage at the pointee's first 8 bytes. The new `skip_auto_deref` check on `place.projection.first()` keeps single-level pointer semantics correct for both shapes.
- **`&mut`-on-place-of-scalar emits `Rvalue::Ref`** — `lower_unary` previously returned `Some(inner)` for every `RefShared` / `RefMut`. For aggregates (Vec/String/struct/opaque-handle Adts whose locals already hold a pointer) that's correct. For `&mut` on a scalar place (`&mut state`, `&mut p.field`, `&mut arr[i]` where the element is `i64`/`f64`/`bool`/`char`), the caller used to hand the callee the **value as a pointer**, segfaulting on the first deref. The lowerer now narrows to the `&mut` + scalar + genuine-place shape and emits `Rvalue::Ref { mutable: true, place }` so backends compute a real slot address. Shared `&` on scalars and `&` on literals keep their historical value-passthrough so existing dispatch (e.g. `map.get(&k)` → `gos_rt_map_get_i64(m, k_value)`) continues to work.
- **Cranelift `Rvalue::Ref` for bare scalar locals materialises a stack slot** — when the address is asked for a local that lives in an SSA `Variable` (the common cranelift shape for `i64`/`f64`/`bool`/`char`), the handler now allocates an 8-byte stack slot, stores the current value, and returns `stack_addr`. The LLVM tier didn't need this because alloca-backed locals always have an address; cranelift required the explicit promotion for the `&mut state` path to produce a real pointer.
- **Net effect** — `fn lcg(s: &mut i64) { *s = *s * K + C }` now runs correctly under both `gos build` and `gos build --release` instead of segfaulting; `impl P { fn advance(&mut self) { self.pos += 1 } }` writes back through the pointer. The bytecode-VM / walker tier still has the long-standing `&mut self` writeback gap on field mutation.

### Multi-dim fixed-array indexing

- **`lower_place_address` advances `current_ty` after every `Index` step** — when projecting `arr[i][j]` over `[[T; A]; B]`, the LLVM lowerer previously left `current_ty` pinned at the outer array type and reset `stride_slots` to 1 after the first index. The second index then used the outer array's bounds (panic with `len is 2 but index is 2` after a clean exit from a `while s < 2` loop), and the stride was wrong for the element width — corrupting the data. The Index arm now matches the Field arm and walks into the element type, recomputing `stride_slots = elem_slots(elem_ty)`. The chess-engine `make_zobrist`-style writes over `[[[i64; 64]; 6]; 2]` round-trip cleanly across all tiers.

### `env::args()` empty-iteration safety

- **`gos_rt_set_args` materialises an empty `GosVec` when `argc <= 1`** — previously the no-user-arg branch stored a null pointer into `ARGS_VEC`, and any iteration over `env::args()` (`for a in args { ... }`) dereferenced the null header and segfaulted. The header is now a zero-length stack-stable `GosVec` with `ptr = null`, `len = 0`, `cap = 0`, so the iterator's `header.ptr + 0 * elem_bytes` walk is a clean zero-trip.

### `xs.pop()` on typed-storage arrays

- **`builtin_pop` handles `Value::IntArray` and `Value::FloatVec`** — the receiver dispatch previously only covered `Value::Array`. A `let mut xs: [i64] = [..]` lands as `Value::IntArray`, fell into the `_ => empty_array` fallback, and the writeback then moved the empty result into `xs` — clobbering the entire vector. Both typed-storage variants now shrink by one element instead of being zeroed out.

### Interpreter RAM — shared prelude, interned identifiers, end-of-load compaction

- **Process-shared prelude `Arc<FxHashMap<&'static str, Global>>`** — `builtins::prelude_globals()` builds the ~330-entry built-in dispatch table once via `OnceLock`; every `Vm::new` and `Vm::with_globals` `Arc::clones` it. New `Vm::lookup_global` / `lookup_global_ref` two-tier helpers consult the per-Vm overlay first, then the shared prelude on miss. Goroutine-heavy programs no longer pay per-Vm prelude duplication. Every `Op::Call` / `Op::MethodCall` / `Op::LoadGlobal` / `Op::SpawnMethod` dispatch site now routes through `lookup_global*`.
- **`Vm.globals` keyed by `&'static str`** — `Arc<HashMap<String, Global>>` → `Arc<FxHashMap<&'static str, Global>>`. Dynamic qualified keys (`format!("{prefix}::{name}")`) intern through `value::intern_type_name`. Eliminates ~330 per-Vm `String` heap allocations and the `to_string()` calls that fed them.
- **`FnChunk::name: &'static str`** — interned at chunk construction (in `compile_fn`). `FnBuilder::name` follows. Recursive programs no longer allocate one `String` per call-stack frame.
- **`Vm.call_stack: RefCell<Vec<&'static str>>`** — interned chunk-name push instead of `String::clone` per `apply` entry. `call_stack_snapshot` still returns `Vec<String>` for API stability.
- **Interner pools migrated to `FxHashSet<&'static str>`** — the process-global `value::intern_type_name` and the per-thread `vm::intern_type_name` / `vm::intern_qualified` swapped from `Vec<(String, &'static str)>` linear scan to a hash-set of leaked `&'static str`. Lookups stay O(1) past the small-program range; hits no longer allocate a probe `String`.
- **`FnBuilder::finish()` folds in `compact()`** — every chunk-construction path now `shrink_to_fit`s its Vec storage automatically; new code that produces a chunk through `finish` cannot accidentally skip the compaction.
- **`Vm::load` ends with `globals.shrink_to_fit()`** — releases hashbrown's growth-by-doubling slack on the overlay once every item is registered.
- **`release_jit_prelude` extended** — drops `mir_bodies` + `tcx_snapshot` and now also `shrink_to_fit`s `chunk_state_arena` + `chunk_state_map` so the post-`call` Vm's RSS reflects steady state while goroutines drain.

### Short-circuit `&&` / `||` in the compiled tier

- **`lower_binary` branches on the LHS for logical AND/OR** — the MIR lowerer previously called `lower_expr` on both sides up front. Any guarded RHS (`while j > 0 && arr[j - 1] < x`) evaluated the bounds-violating index unconditionally and panicked with `index is -1` once the LHS guard kicked in. The lowering now emits a small branch lattice: LHS → switch → (short-circuit constant) or (eval RHS) → merge. VM tier was already correct via the walker's expression evaluator; this brings the compiled tier in line.

### `HashMap` bare-name dispatch router

- **`builtin_keys_router` / `builtin_values_router`** — `install_module("json", …)`'s unconditional bare-name push registered `("keys", builtin_json_keys)` AFTER the HashMap surface's `("keys", builtin_map_keys)`. The later json push silently overrode the bare-name registry, so every `m.keys()` on a HashMap dispatched to the JSON helper which returns `None` for non-Struct receivers — surfacing as `ks.len() == 0` even with multiple inserts. A small router dispatches on the Value variant so both surfaces work without depending on registration order.

### Array literal → `Vec` / `Slice` return coercion

- **`Return` lowering coerces `Array<T; N>` to `Vec<T>` when the declared return is `Vec(elem)` / `Slice(elem)`** — `fn f() -> [String] { return ["a", "b"] }` previously lowered the literal as a flat stack-aggregate that the caller dereferenced as a `*mut GosVec` (len read as garbage bits, all subsequent reads silently empty). The Return path now routes the value through `coerce_array_to_vec` (which calls `gos_rt_vec_from_arr`) when the shapes match.

### `HashMap.iter()` direct-binding guard

- **MIR's method-call dispatch rejects `.iter()` on a `HashMap` receiver outside the for-loop shape** — `for (k, v) in m.iter()` is still handled by `try_lower_for_hashmap_iter` (a real entries walk on every tier). The direct-binding form `let xs = m.iter()` previously dispatched the `*mut GosMap` receiver through `gos_rt_arr_iter`, which reads the map handle's first 8 bytes as a `GosVec` length header and walks garbage — silent miscompile / segfault. The dispatch now `return None`s for HashMap receivers so the compiler emits a clear error pointing users at `m.keys()` / `m.values()` / the for-loop form instead of producing a broken binary.

### `Vec<Struct>` place-indexing + fixed-array promotion

Two coordinated fixes close a class of multi-slot-element corruption under `gos build --release`.

- **`bodies[i].field` over a `Vec<Struct>` routes through `gos_rt_vec_get_ptr`** — the place-expression Index arm previously appended a flat `Projection::Index`, which the LLVM lowerer strode off the `*mut GosVec` *header* rather than the data buffer. Element 0 happened to alias the header's first field, so reads/writes past index 0 hit garbage (the chess / nbody struct-array corruption). The Index arm now detects a Vec / Slice base with multi-slot elements (consulting the base local's MIR-resolved type so promoted bindings are seen), materialises the element address via `gos_rt_vec_get_ptr`, and binds it to a `&elem`-typed local; the appended `Field` projection auto-derefs that pointer so both reads and writes land inside the Vec's storage.
- **`let mut [T; N]` promotion to `Vec` is gated on actual growth** — a `mut` array-literal binding was unconditionally rewritten to a heap `Vec`, even for an explicitly-sized `[Body; 5]` that is only indexed, field-mutated, or passed to a `[T; N]`-typed parameter. The promotion desynchronised the element stride at call boundaries (`energy(&bodies)` declared `&[Body; 5]` strode the GosVec header as inline data → NaN). The MIR builder now pre-scans the function body for growth / reshape receivers (`push`, `pop`, `insert`, `remove`, `extend`, `truncate`, `clear`, `retain`, `append`, `resize`, `drain`, `split_off`, `sort`, `sort_by`) and promotes a `let mut [literal]` only when its binding is grown somewhere; otherwise it keeps the inline fixed-array layout that matches every use site. `let mut xs = [3, 1, 2]; xs.push(4); xs.sort()` still promotes; `let mut bodies: [Body; 5]` passed to a `[Body; 5]` parameter no longer does.

### `sort_by` comparator over aggregate elements

- **The closure-lift pass no longer pins aggregate-typed comparator params to i64** — `xs.sort_by(|a, b| a.1 < b.1)` on a `Vec<(String, i64)>` produced a no-op / wrong order. Inference left the closure params `a` / `b` as `Var` (the expected `FnTrait((T, T) -> i64)` signature wasn't propagated into the closure body), and `lift_closures` blanket-pinned every unresolved closure param to i64. The lifted comparator then computed `a.1`'s field offset off a junk integer rather than the element pointer the runtime sort (`gos_rt_vec_sort_by_aggr`) passes it. The lift pass now walks each closure body first and skips the i64 pin for any param used through a `TupleIndex` / `Field` projection or as a method-call receiver — those params hold aggregates passed by pointer. Scalar comparator params (`|n| n * 2`) keep the i64 pin they need. Works without the previously-required explicit `|a: (String, i64), b: (String, i64)|` annotation.

### `for e in &Vec<Enum>` slot-pointer dereference

- **`lower_for_vec` checks slot width before treating the element as inline** — the for-loop helper previously flagged any `TyKind::Adt` element as "inline aggregate" and bound the loop variable to the slot's address. For multi-slot user structs (`Projection { a: i64, b: i64 }` = 16 bytes inline) that's the right move; field projections walk off the slot address. For single-slot Adts — enums, sentinel-handle structs whose 8-byte slot *holds* a heap pointer — the loop body needs the pointer value, one `gos_load` away. The previous binding handed each iteration the slot address; `match e { … }` then read the first 8 bytes of the heap allocation as the pattern scrutinee, every variant arm failed to match, and `for e in &Vec<Expr>` silently produced no output. The check is now `slot_bytes > 8` rather than just "is Adt"; single-slot Adts route through the scalar `gos_load(ptr, 0)` path.

### `Vec<UserStruct>` inline element width

- **`type_slot_bytes` for user `Adt` sums registered field widths** — every user struct collapsed to 8 bytes regardless of field count, so `gos_rt_vec_new(elem_bytes)` for a `Vec<Projection>` whose `Projection { a: i64, b: i64 }` is two slots reserved 8-byte slots, and each push truncated to the first field. `for p in &xs { p.b }` then read garbage at the wrong offset (and any `String` field's `len()` segfaulted on the stray pointer). `type_slot_bytes` now consults `tcx.struct_field_tys(def)` and returns the slot-sum × 8 for user structs, leaving sentinel stdlib structs (DirInfo, Output, ResponseStream, Response — `u32::MAX - 5 ..= u32::MAX`) at the pointer-sized 8 bytes their runtime helpers require.

### Typed-storage fast paths tolerate generic-Array receivers

- **`Op::IntArrayGetI64` / `Op::FloatVecGetF64` fall back to `Value::Array`** — the compiler's `flat_int_locals` / `flat_float_locals` tracking can outlive the receiver's concrete `Value::IntArray` / `Value::FloatVec` payload when the call-args path doesn't typed-promote across a function boundary. The runtime fast paths now accept the generic `Value::Array(Vec<Value::Int>)` / `Vec<Value::Float>` shape (one discriminant match per index) instead of aborting with `receiver lost flat invariant`. Hot-path performance is unchanged on the typed path; the fallback rescues calls that previously panicked.

### Regression coverage

`tests/bug_regressions.rs` gains tests pinning the above behaviours through both VM and LLVM AOT tiers:

- `deref_assign_through_mut_i64_runs_under_llvm` — LCG `*s = *s * K + C` runs correctly instead of segfaulting.
- `mut_self_field_compound_assign_writes_back` — `self.n += 1` writes back through the pointer.
- `multi_dim_fixed_array_index_walks_inner_strides` — `arr[i][j][k]` over `[[[T; A]; B]; C]` lands on the correct element.
- `env_args_empty_iter_does_not_segfault` — `for a in env::args() { … }` is a clean no-trip when no user args supplied.
- `vec_pop_on_typed_storage_shrinks_by_one` — `[i64]` / `[f64]` slices shrink by exactly one after `xs.pop()`.
- `hashmap_keys_router_does_not_get_shadowed_by_json` — `m.keys()` returns all keys regardless of registration order with module-prefixed bare-name pushes.
- `return_array_literal_coerces_to_slice` — array-literal return to a `Vec`/`Slice`-typed function produces a real GosVec.
- `typed_int_array_get_falls_back_to_generic_array` — `arr[i]` inside `fn slide(arr: [i64; N])` works for repeated calls inside a loop.
- `logical_and_or_short_circuit_in_compiled_tier` — `&&` / `||` short-circuit RHS evaluation under `gos build --release`.
- `sort_by_on_tuple_vec_orders_by_comparator` — `xs.sort_by(|a, b| …)` on a `Vec<(String, i64)>` orders by the comparator without explicit closure-param type annotations.
- `vec_of_struct_index_field_reads_and_writes_through_data_buffer` — `bs[i].x` read and `bs[i].x = v` write on a `Vec<Struct>` land in the Vec's storage.
- `mut_fixed_struct_array_not_promoted_keeps_layout_across_calls` — `let mut bodies: [Body; N]` passed to a `&[Body; N]` parameter keeps its inline layout.
- `mut_scalar_array_with_push_still_promotes_to_vec` — a `mut` array literal that calls `push` / `sort` still promotes to a heap Vec.
- `vec_of_enum_for_loop_dereferences_slot_pointer` — `for e in &Vec<Enum>` reads the heap pointer out of the slot before passing the element to the body.
- `vec_of_multi_slot_struct_round_trips_all_fields` — `Vec<Projection>` where `Projection` has multiple scalar fields preserves every field across `push` / `for` iteration.

## 0.9.0 — Production hardening, tooling, observability, and SQL pluggability

### Language

- **`?` on `Option<T>` and `Result<T, E>`** — `try_propagation_kind` selects the propagation shape; `ast_is_option_shaped` is the AST-level fallback when typechecker hasn't pinned the return type. Error paths auto-route through `gos_rt_error_from` so `let x: A = fallible_b()?` works when `A: From<B>`.
- **User `impl Iterator for T` end-to-end** across all three tiers — HIR `lower_for` splits into `lower_for_user_iter` (Adt receivers; threads through a `__for_iter` let-binding and a `.next() -> Option<T>` call) and `lower_for_inline` (range / array / Vec fast paths). MIR for-loop fast-path bails to the generic shape for Adt receivers. Interp's `invoke_method` + `apply_closure_capture_self` write a mutated `&mut self` back to the receiver place; `&self` / `&mut self` are typed `Ref<SelfType>` in HIR.
- **`UnknownTraitBound` (GT0011)** — `register_fn_sig` validates declared trait names against `known_builtin_trait` (the eight built-in kinds) + the user's `declared_trait_names`. Typo'd bounds (`Itarator` for `Iterator`) now surface as a type error with a span.

### Tooling

- **`gos bench [PATH] [--parallel N]`** — discovers every `#[bench]`-annotated function under `PATH` (defaults to `src/`) and reports `ns/op` plus `allocs/op` per benchmark. Per-bench iteration counts auto-tune to a 50ms calibration window (capped at 2^20); allocation deltas read from `gossamer_runtime::gc::stats().bytes_allocated`. `std::testing::Bencher` ships as the future-facing argument type; zero-arg `#[bench]` fns keep working.
- check.sh extended to mirror more of the CI workflow with Github Actions.

### Runtime — production safety

- **Stack-overflow guard** — `stack_guard::install_stack_guard()` runs at scheduler start and per worker. Unix installs `sigaltstack(2)` + `SA_ONSTACK` SIGSEGV handler with async-signal-safe diagnostics; Windows uses `SetThreadStackGuarantee` + `SetUnhandledExceptionFilter`. Faults outside the guard window restore `SIG_DFL` and re-raise.
- **`safe_daemon::daemonize`** — Unix `fork` + `setsid` + second-`fork` detach so `gossamer-std` (`#![forbid(unsafe_code)]`) can run a daemon without losing that guarantee. `Unsupported` on non-Unix.
- **OOM no longer crosses the FFI boundary** — `gos_rt_gc_alloc` + `gos_rt_aggr_alloc_leak` `alloc_zeroed`-null paths `eprintln!` + `std::process::abort()` instead of `std::alloc::handle_alloc_error` (which panics; panic-across-FFI into compiled Gossamer is UB).
- **FFI transmute audit** — `c_abi/mime.rs::mime_str` no longer launders the borrow into `&'static str` via `mem::transmute`; returns an owned `String`.
- **`WorkerHandleGuard`** — RAII over `WorkerSlot::thread_handle`. On panic-unwind, swap-to-0 and call `preempt::release_thread_handle`. Closes a long-running Windows-service handle leak.
- **Typed function-pointer registry** — `c_abi::fn_registry` with `FnKind` enum (I64ArgsToI64, EnvI64ArgsToI64, HttpHandlerBare/Env, SortCmp/SortCmpAggr, UnaryI64ToI64, BinaryI64ToI64, PredI64, JitEntry, GoSpawnEntry, CtxCancelI64, Generic). `verify` runs at every `gos_rt_fn_tramp_N` / `gos_rt_go_spawn_call_N` site; registered-with-different-kind aborts. `parking_lot::RwLock<HashMap>` keeps the read path uncontended.
- **`GosMutex` owner tracking** — `owner: AtomicI64`; cross-goroutine unlock aborts with a diagnostic rather than corrupting lock state.
- **`parking_lot::Mutex` everywhere** — every internal `std::sync::Mutex` migrated. No poisoning, smaller footprint, faster uncontended path. `.lock().unwrap_or_else(PoisonError::into_inner)` collapses to `.lock()`.
- **`tests/audit_unsafe.rs`** — CI gate asserts every `unsafe { ... }` block in `gossamer-runtime/src/` (excluding the FFI surface in `c_abi/` + `ffi.rs`) carries a `// SAFETY:` comment within 8 lines above. Backfills `http2_server.rs` + `stack_guard.rs`.
- **`gossamer-runtime::replay`** — deterministic record + replay modes via `GOS_TRACE` / `GOS_REPLAY`. Length-prefixed binary records cover channel send/recv, goroutine spawn/yield, RNG seed draws.

### Runtime — performance

- **`gos_rt_str_concat`** — single-allocation path via `alloc_cstring_from_slices` (was three allocations per concat). `try_extend_last_cstring` removed.
- **`ChannelInner.closed`: `AtomicBool`** (was `Cell<bool>`) — `close()` uses `compare_exchange` so concurrent close-and-recv races converge deterministically.
- **Scheduler yield-rate tracking** — per-worker `last_yield_micros: AtomicU64` + `process_start()` / `now_micros_since_start()` helpers. `should_yield()` uses `Acquire` ordering.
- **Interp allocator-pressure shaves** — `apply_closure_capture_self` borrows the self-param name as `&str` from the closure (was a per-call `String::clone`); `builtin_map_inc_at` builds the map key via `SmolStr::from_str(&str)` directly (was `to_string()` then wrap). Every user `impl Iterator` `.next(&mut self)` benefits.

### Garbage collector

- **Overflow safety** — `Heap`'s two `u32::try_from` sites `eprintln!` + `abort` instead of `.expect()`. Weak-ref `generation` widened to `u64` with `checked_add`; closes the 2^32-churn use-after-free.
- **Pause-time histogram** — `GcStats::pause_histogram` (6-bucket: `<100us` / `<1ms` / `<10ms` / `<100ms` / `<1s` / `>=1s`) updated per `collect()` cycle.
- **Precise pointer-mask tracing** — `gos_rt_gc_alloc_traced(size, mask_ptr, mask_len)` registers an aggregate with an explicit `u32` pointer-offset mask. The marker walks only the recorded offsets; `null` mask opts into the conservative word-scan. Closes the false-retention hazard from `i64` payload words colliding with live addresses.
- **`gos_rt_gc_collect` thread-local `CollectBuffers`** — the snapshot `HashMap`, marked `HashSet`, and worklist `Vec` live in a `thread_local!` cell and are `.clear()`'d (capacity preserved) between cycles. Removes the per-cycle alloc/free churn on HashMap-heavy workloads.
- **`gos_rt_fs_list_dir` / `gos_rt_fs_walk_dir`** — per-entry blobs now allocate through `gos_rt_gc_alloc` so the collector can reclaim them. The prior path leaked one 56-byte payload per directory entry.

### Codegen — LLVM

- **`render_ir_to_string(bodies, tcx, allow_fallback)`** — runs the standard LLVM pipeline and returns `.ll` IR as `String`. Used by snapshot / smoke tests in downstream crates.
- **`gos build --release` strict-lowering on by default** — `set_strict_lowering(true)`; any MIR shape the LLVM backend can't lower is a hard build failure. `--allow-llvm-fallback` is the explicit opt-out.
- **`pipeline_tmp_dir`** suffixes the per-process directory with a per-call atomic counter so parallel `render_ir_to_string` / `compile_to_object` calls don't trample each other's `unit.ll` / `unit.o`.
- **`crates/gossamer-codegen-llvm/tests/lower_shapes.rs`** — 14 deterministic tests hand-roll a `Body` per MIR shape (constants + binop variants for add/sub/mul/div/rem/and/or/xor/shl/shr) and assert substring properties on the rendered IR.

### Codegen — Cranelift

- **Closure-callback JIT dispatch entries** — `gos_rt_arr_sort_by_i64`, `gos_rt_vec_sort_by_i64`, `gos_rt_vec_sort_i64`, `gos_rt_{arr,vec}_sort_by_aggr`, `gos_rt_callback_invoke`, `gos_rt_iter_map_i64` now in the JIT symbol table. User bodies calling these no longer skip JIT compilation.
- **`intrinsic_g{0,1,2,3}.rs` → `intrinsic_{io_math,collections,handles,string}.rs`** — names describe contents rather than alphabet position; module-level docs added.

### Diagnostics + LSP + Parse + CLI

- **Centralised error-code registry** — `gossamer-diagnostics::REGISTRY` is the single source of truth for every `GL`/`GP`/`GR`/`GT`/`GM`/`GX` code; `gos explain CODE` reads from it. `tests/registry.rs` enforces alphabetical order + non-empty text; `tests/snapshots.rs` renders every code (plain + framed) via `insta`.
- **LSP — 67 new integration tests** across `completion`, `hover`, `diagnostics`, `document_symbol`, `code_actions`, `format`, `semantic_tokens`, `inlay_hints`. `ServerHandle` (test-only) gains 13 request methods + four `params`-building helpers.
- **`crates/gossamer-parse/tests/proptest_round_trip.rs`** — five proptest properties exercise int literals, binary ops, `let` bindings, function definitions, and nested blocks. Capped at `cases: 64` / `max_shrink_time: 2s` for CI determinism.
- **`crates/gossamer-cli/tests/repl.rs`** — seven scripted-stdin tests for the `gos repl` binary covering the happy path and error reporting.
- **`examples/projects/rust_binding_add/`** — minimal Rust-bindings project demonstrating `gos add --rust-binding`.

### Stdlib — `std::database::sql`

- **`pool` submodule** — bounded-semaphore connection pool with idle-timeout recycling and a per-checkout retry budget.
- **`migrate` submodule** — forward-only schema migrations from a `<version>_<slug>.sql` directory; each migration runs in its own transaction; concurrent runners coordinate via an advisory lock on `schema_migrations`.
- **`query::Select` builder** — fluent SELECT renderer emitting `(sql, params)` with `Value`-bound parameters and Postgres-style `$N` placeholders (SQLite also accepts).
- **Trait surface extensions** — `Error::driver(...)` + `Error::PoolExhausted`, `IsolationLevel` enum (`Default` / `Read{Uncommitted,Committed}` / `RepeatableRead` / `Serializable`). `Conn::begin_with(iso)` / `ping()` / `execute_many(sql, rows)` ship as default impls on the facade for incremental driver adoption.
- **Native lowering** — `gossamer-runtime::sql` (trait surface relocated from `gossamer-std`) + `c_abi::sql` (33 `gos_rt_sql_*` shims over five handle registries: Conn / Stmt / Rows / Row / Tx / Value). Cranelift JIT + LLVM AOT dispatch through `Both`-tier ABI registry entries. `Conn::interrupt()` / `execute_ctx(ctx, ...)` / `query_ctx(ctx, ...)` check `ctx.is_cancelled()` on either side of the call.
- **SQLite driver removed** — `rusqlite` dependency dropped, `database/sql/sqlite.rs` deleted. The facade stays; third-party drivers register through `gossamer-runtime::sql::Driver`.

### Stdlib — web + networking

- **`std::http_h3`** — first-party HTTP/3 server + client (RFC 9114) wrapping `quinn` (QUIC) + `h3`. Each `serve` / `Client` instance owns a private current-thread tokio runtime; callers see only synchronous entry points mirroring `std::http_h2` and `std::http`.
- **`std::http_native_client` TLS** — `NativeClient` wraps the TCP stream in `rustls::StreamOwned<ClientConnection, _>` for `https://`; per-request setup amortises through `Arc<rustls::ClientConfig>`.
- **`http_state::attach_to_router`** — `Router` gains an optional `AppState` field + `set_state` / `state` accessors; `State::<T>::from_router(&router)` is the typed extractor handlers use.

### Stdlib — observability + compression

- **`std::metrics`** — Prometheus-compatible `Counter`, `Gauge`, `Histogram` + a `Registry` holding them in registration order; outputs the text-exposition format.
- **`std::trace`** — W3C trace-context distributed tracing (`TraceId`, `SpanId`, `SpanContext`, `Span`, `Tracer`). OTLP JSON exporter pushes ended spans to a sidecar collector — no `opentelemetry-otlp` dependency.
- **`std::compress::zstd`** — Zstandard encoder/decoder wrapping vendored libzstd. Same byte-in/byte-out shape as `gzip` / `flate` / `zlib`; level 1–22, default 3.

### Stdlib — fs

- **Watch / mmap / locks / atomic writes** — `fs::watch::Watcher` (`notify`), `mmap_read` / `mmap_write` (`memmap2`), `lock_exclusive` / `lock_shared` (`fs2`), `write_atomic` (temp-file + rename). `hard_link`, `set_permissions_mode`, `chown` close the niche-fs gap.
- **`fs::TempDir`** — RAII temp directory; `new()` / `with_prefix(prefix)` under `env::temp_dir()`; `path()` / `into_path()` / `Drop`-cleanup.
- **`fs::temp_file(prefix)`** — `(File, PathBuf)` for a uniquely-named writable scratch file.

### Stdlib — crypto + jwt

- **Hex digest C-ABI shims** — `gos_rt_sha256_hex` / `gos_rt_sha512_hex` / `gos_rt_blake3_hex` / `gos_rt_hmac_sha256_hex` under `c_abi::crypto`, alphabetically registered in `gossamer-abi::registry`. Tier-parity bit-identical via `feature-testing-examples/crypto_sha_hex.gos`.
- **`std::jwt` RS256 / RS384 / RS512 verify** — RSA PKCS#1 v1.5 via `ring`'s audited constant-time RSA. The vulnerable `rsa` crate (RUSTSEC-2023-0071) stays out of the tree.

### Stdlib — unicode + iter + regex

- **Grapheme cluster iteration** — `std::unicode::graphemes(s)` / `grapheme_count(s)` walk UAX #29 extended grapheme clusters via `unicode-segmentation`. `👨‍👩‍👧` counts as one.
- **`std::iter::Lazy<I>`** — lazy adapter over any Rust `Iterator` with `map` / `filter` / `take` / `skip` / `step_by` adapters and `sum` / `min` / `max` / `count` / `first` / `fold` / `any` / `all` / `to_vec` / `product` terminals. Allocation-free until the terminal materialises.
- **Free iter combinators** — `iter::sum`, `iter::product`, `iter::min`, `iter::max`, `iter::step_by`, `iter::once`, `iter::empty`, `iter::collect` join the existing family.
- **Regex named groups** — `regex::capture_names(pat)`, `regex::captures_named(pat, hay)`, `regex::captures_named_all(pat, hay)`. `(?P<year>\d{4})` lookups return `HashMap<String, String>` directly.

### CI

- **`cargo doc --workspace`** under `RUSTDOCFLAGS=-D rustdoc::broken_intra_doc_links` + `cargo test --doc --workspace --release` — doc-test drift fails CI.
- **Cross-target check matrix** — `aarch64-unknown-linux-gnu`, `riscv64gc-unknown-linux-gnu`, `wasm32-unknown-unknown`, `wasm32-wasip1` each `cargo check` the platform-agnostic crates (runtime, abi, binding{,-macros}, pkg, gc, sched).

### Test fixtures

- **`feature-testing-examples/iterator_trait_user_impl.gos`** — user `impl Iterator for Counter` driving `for x in c`; tier-parity bit-identical.
- **`feature-testing-examples/try_option_propagation.gos`** + **`try_err_conversion.gos`** — `?` on `Option` and `?` with `From`-conversion in the error path.
- **`feature-testing-examples/crypto_sha_hex.gos`** — every hex-digest shim exercised end-to-end.
- **`crates/gossamer-runtime/tests/gc_collect_concurrent.rs`** — concurrent-allocator non-starvation, precise pointer-mask registration, fs blob GC reclaimability.
- **`crates/gossamer-std/tests/{iter_lazy,python_ergonomics}.rs`** — `iter::Lazy` chain round-trips; regex named groups + `TempDir` cleanup + `temp_file` uniqueness.
- **`crates/gossamer-hir/tests/lower.rs`** — trait-bound validation, Option-shape `?` propagation.

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
  recovery in `gos build --release`.
- **HTTP server thread-per-connection restored.** 0.6.0 had
  swapped `gos_rt_http_serve` from "spawn a dedicated OS thread
  per accepted socket"  to "fixed worker pool + bounded
  `sync_channel`". With `available_parallelism() * 2` workers
  (≈ 48 on a 12-core box), > 48 concurrent clients saturated
  the pool, the queue filled, `try_send` started silently
  dropping sockets (RST'd by the OS), and the bench saw
  connection errors. The dedicated-thread shape (capped by
  `HTTP_ACTIVE_CONNS` / `GOSSAMER_HTTP_MAX_CONN` — default
  4096 — so a runaway client cannot bomb the thread / fd
  budget; past the cap responds 503 cleanly) is back.
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
