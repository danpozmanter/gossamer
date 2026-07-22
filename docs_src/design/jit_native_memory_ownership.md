# JIT native-memory ownership

## Investigation

Before this change, `compile_bodies` in `gossamer-codegen-cranelift/src/jit.rs`
created a `JITModule`, finalized it, copied each callable entry pointer into a
`JitFn`, and stored the entire module in `JitArtifact`. `JitArtifact::drop`
called `JITModule::free_memory`. The interpreter retained the artifact in
`JitState` and in a weak per-thread cache. Dispatch maps retained `Arc<JitFn>`
metadata, but the VM's `Rc<JitArtifact>` was the actual allocation owner.

Only finalized function pointers leave the module. Gossamer does not retain a
Cranelift function ID, data ID, declaration reference, compilation context, or
module lookup handle for runtime use. `JitFn` additionally retains compact
Gossamer metadata: the source name, parameter kinds, result kind, and the
fresh-result aliasing flag.

Cranelift 0.134.2 routes all generated allocations through the public
`JITMemoryProvider` interface. The allocation kinds are executable, read-only,
and writable. Executable blobs include machine code, inline constants, jump
tables, and AArch64 call veneers. Read-only allocations contain finalized
constant data. Writable allocations contain mutable native data. Gossamer
builds the JIT with position-independent code disabled, as required by
`JITModule`, so Cranelift rejects GOT and PLT relocation forms. Direct and
absolute relocations, including references to imported runtime functions and
other compiled functions or data, are applied before provider finalization.
All relocation targets therefore resolve either into provider-owned mappings
or to process-lifetime symbols in the Gossamer executable.

The public `SystemMemoryProvider` uses anonymous `memmap2` mappings on Linux,
Windows, and macOS. During finalization it changes read-only pages to read-only
and executable pages from writable to read-execute. It also clears the
instruction cache, flushes the instruction pipeline, and applies AArch64 BTI
where supported. Writable data remains non-executable. Sharing this provider
preserves Cranelift's supported W^X and cache-coherence behavior instead of
duplicating platform memory code.

Gossamer sets Cranelift's `unwind_info` flag to `false`. The optional
`cranelift-jit/wasmtime-unwinder` feature is not enabled. Consequently,
`JITModule` owns no Windows function table, DWARF registration, compact-unwind
registration, or Wasmtime exception table needed by Gossamer's emitted code.
Language panic operations call process-lifetime Gossamer runtime functions;
the VM keeps panic-capable entries on bytecode. Cranelift traps are terminal
unreachable paths after those runtime calls rather than a module-owned trap
registry. This is the same arrangement on Linux, Windows, and macOS.

Gossamer already reclaimed JIT code at safe VM teardown by calling
`JITModule::free_memory`. It does not need individual-function reclamation.
Worker VMs complete before teardown, artifacts are thread-confined, weak cache
entries do not retain artifacts, and dispatch entries are cleared with their
owning VM state. The implementation therefore preserves whole-artifact
reclamation and does not add fine-grained unloading.

## Safety decision

The ownership gate passes with Cranelift 0.134.2's public API. A custom adapter
can share the complete backing provider with a lightweight artifact. Ordinary
`JITModule` destruction drops only its declarations, ISA, symbol maps,
compiled-blob relocation vectors, and its adapter reference. It does not call
`JITMemoryProvider::free_memory`. The artifact's reference keeps every native
mapping alive. The final reference explicitly invokes the public provider's
`free_memory` operation.

The runtime ownership graph is:

```text
NativeCodeHeap
    |-- temporary GossamerJitMemoryProvider adapter, owned by JITModule
    `-- JitArtifact reference, owned by the VM during native execution
```

The lifecycle is building, finalized, detached, then retired. Allocation is
accepted only while building. Provider finalization establishes page
permissions and cache coherence before the state becomes finalized. Entry
pointers are copied out, the module is explicitly dropped, and only then is
the heap marked detached and returned. Artifact destruction changes the state
to retired before releasing mappings. There is no lock, reference-count
operation, or lifecycle check on native dispatch.

On compilation or finalization failure, local references and the module adapter
are dropped. The last `NativeCodeHeap` reference then frees all partial
allocations. A successful artifact owns its mappings until VM teardown. Native
entry pointers never escape without the owning artifact, and Gossamer's
thread-confined VM model prevents artifact destruction racing an active call.

## Platform behavior

Linux, Windows, and macOS all use Cranelift's public `SystemMemoryProvider`.
No platform-specific allocation implementation is added. There are no helper
processes, compiler or linker invocations, object files, generated shared
libraries, or runtime toolchain dependencies. The JIT remains entirely inside
the Gossamer executable and normal operating-system mappings.

Windows and macOS execute the same post-module-drop integration tests in the
existing native CI matrix. Any future enabling of Cranelift unwind metadata or
the `wasmtime-unwinder` feature invalidates this analysis and must add an
explicit registration owner before detachment remains allowed.

## Reclamation boundary

Whole-artifact executable-memory reclamation remains supported at safe VM
teardown. Early reclamation of individual functions is not supported. It would
require proof that no native stack, function value, dispatch slot, callback,
or other compiled artifact can reference the function, which the current VM
does not track at that granularity.

## Linux measurements

The release executable was measured twice with `/usr/bin/time`, using the
checked-in benchmark inputs. The baseline is the previously recorded
`gos-jit` result in each suite. It predates both the Cranelift 0.134.2 upgrade
and detachment, so these numbers establish the current result but do not by
themselves attribute every byte to detachment.

| benchmark | input | recorded peak KB | detached median peak KB | change KB | recorded wall s | detached median wall s |
|---|---:|---:|---:|---:|---:|---:|
| fasta | 25,000,000 | 22,040 | 20,498 | -1,542 | 3.27 | 3.24 |
| n-body | 50,000,000 | 23,400 | 22,012 | -1,388 | 2.07 | 2.09 |
| fannkuch-redux | 10 | 21,520 | 20,380 | -1,140 | 0.36 | 0.37 |
| spectral-norm | 5,500 | 21,712 | 20,294 | -1,418 | 2.76 | 2.78 |
| mnist-slp | 2,000 | 23,120 | 22,014 | -1,106 | 0.34 | 0.34 |
| edit-distance | 10,000 | 22,132 | 20,576 | -1,556 | 0.75 | 0.75 |
| json-serde | 50,000 | 117,520 | 115,850 | -1,670 | 0.14 | 0.15 |

For n-body, an instrumented run reported 20,602,880 bytes immediately before
compilation, 24,244,224 bytes immediately after module drop, and 23,285,760
bytes at execution completion. The process high-water mark was 23,994,368
bytes. The n-body artifact contained 7,027 generated native bytes. Compilation
itself still determines the high-water mark; detachment reduces the live set
retained for native execution. Native byte counts are reported separately by
`GOS_JIT_TRACE` and the existing `gos bench` metrics.

Timings are coarse wall-clock samples, but show no systematic regression. The
native call path is unchanged and does not consult the heap owner.

## Validation

Linux debug and release tests explicitly call finalized entries after module
drop. They cover integer and floating-point ABIs, recursion, native-to-native calls, runtime
calls, writable statics, struct results, divide-by-zero panic behavior,
artifact reuse, and repeated create/call/drop cycles. The full Cranelift crate
suite and the 19 focused interpreter JIT tests pass in both profiles.

The existing native CI matrix runs the same executable JIT tests on Linux
x86-64, Linux AArch64, Windows, and macOS. The new integration test is not in
the platform-invariant skip list, so CI executes it on Windows and macOS rather
than merely compiling it.

A local `x86_64-pc-windows-gnu` check passes. The local macOS cross-check cannot
build C dependencies because no Apple C cross-toolchain is installed; the host
`cc` rejects Apple `-arch` and deployment-target flags. Native macOS execution
therefore remains a CI validation item.

The full workspace test command is currently blocked by unrelated compile
errors in `gossamer-interp` tests at `vm/call_dispatch.rs:603` and `:605`.
Those errors concern string comparison in concurrent Phase 6 work and were not
changed as part of this phase.
