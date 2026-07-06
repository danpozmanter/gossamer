# Building with Zig

Zig is optional. Gossamer never requires it. It is a convenient,
self-contained C toolchain that helps in a few cross-compilation and
static-linking cases - most usefully cross-compiling to a different
architecture's musl target. This page explains when it helps, how to
use it, what it replaces, and how narrowly Gossamer itself relies on it.

## What a native build actually needs

`gos build` and `gos build --release` differ in the external tools they
call:

| Command | Backend | External tools |
| --- | --- | --- |
| `gos build` | Cranelift AOT | A C linker only (`cc` / `$CC` / `ld.lld`) |
| `gos build --release` | LLVM `-O3` | `llc` + `opt` (LLVM 18), via `PATH` or `GOS_LLC` / `GOS_LLVM_OPT` |

The `gos` binary links no libLLVM - it shells out to `llc` / `opt` - so
the toolchain itself is portable regardless of how you build your own
programs. Two consequences follow:

- **Zig does not replace LLVM.** `gos build --release` needs a real
  `llc` / `opt`. Zig bundles clang but not standalone `llc` / `opt`, so
  it cannot stand in for the `--release` codegen tools.
- **Zig can serve as the linker / C driver.** Where a build needs a C
  compiler or a libc sysroot - final linking, or building C
  dependencies such as mimalloc, ring, and zstd - `zig cc` supplies
  both from a single self-contained download.

## Using zig

The simplest path is [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild),
which wraps `zig cc` as the C compiler and linker:

```sh
cargo install cargo-zigbuild        # once; also needs `zig` on PATH
cargo zigbuild --release --target aarch64-unknown-linux-musl --bin gos
```

Or point Cargo at zig as the C compiler for a specific target directly:

```sh
rustup target add x86_64-unknown-linux-musl
CC_x86_64_unknown_linux_musl="zig cc -target x86_64-linux-musl" \
  cargo build --release --target x86_64-unknown-linux-musl --bin gos
```

Zig is a single extractable archive with no system installation, which
is what makes it convenient in a CI job or a clean container.

## You usually do not need it

For native builds zig is never required:

- **Native static musl x86_64.** `rustup target add
  x86_64-unknown-linux-musl` plus `musl-tools` (which provides
  `musl-gcc`) is enough - Gossamer is pure Rust plus a few C
  dependencies, with no cross-architecture sysroot problem on the
  native target. This is how the static `gos-<version>-linux-x86_64-musl`
  release binary is produced.
- **`gos build` (Cranelift).** Only a host `cc`.
- **`gos build --release` (LLVM).** `llc` / `opt` from a system LLVM 18
  (`apt-get install -y llvm clang`), or pointed at with `GOS_LLC` /
  `GOS_LLVM_OPT`.

## Why Gossamer uses zig, and how narrowly

In this repository zig appears in exactly one place: cross-compiling the
static-musl **aarch64** runtime archive in the release workflow. Bare
`clang --target=aarch64-unknown-linux-musl` has no musl sysroot and
fails to find `string.h`; zig ships a bundled musl sysroot plus a
working cross C compiler, so the aarch64 static-musl
`libgossamer_runtime.a` builds cleanly from an x86 CI host as a single
self-contained download.

It was chosen for convenience, not necessity, and it is replaceable:

- A real musl cross toolchain (`musl-cross` / `aarch64-linux-musl-gcc`).
- The [`cross`](https://github.com/cross-rs/cross) tool (Docker-based
  cross builds).
- For the native x86_64 static-musl binary, nothing extra at all -
  rustup's musl target plus `musl-tools`.

Zig earns its keep only for the cross-architecture (aarch64-from-x86)
musl case. It is not a Gossamer dependency, not part of `gos`, and never
required to build your own programs.

## See also

- [Installing Gossamer](../install.md) - dependencies and the supported
  platform matrix.
- [Deployment guide](../deployment.md) - per-target builds, static
  binaries, and container images.
