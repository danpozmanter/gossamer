# Installing Gossamer

Pre-release - the only supported install path today is a source
build.

## From source

```sh
git clone https://github.com/danpozmanter/gossamer
cd gossamer
cargo build --workspace --release
./target/release/gos --version
```

The `gos` binary is self-contained. Copy it anywhere on your
`PATH`:

```sh
install -m 0755 target/release/gos /usr/local/bin/gos
```

On macOS, a locally built `gos` carries only the linker's ad-hoc
code signature, which the system invalidates once the binary is
moved - a relocated copy is killed at launch with `Killed: 9`.
Re-sign it after copying:

```sh
codesign --force --sign - /usr/local/bin/gos
```

Published release binaries are already re-signed, so this step
applies only to a `cargo build` binary you relocate yourself.

## Dependencies

- **Rust toolchain** - 1.95.0, edition 2024, MSRV 1.95.
  `rust-toolchain.toml` pins the exact version and `profile =
  "minimal"`; rustup installs it on first `cargo` invocation.
  Bumps happen consciously, not via `stable` drift.
- **A C linker** - required by Cargo, not by Gossamer. `cc` /
  `gcc` / `clang` will do.

## Verifying

```sh
gos --version
gos new example.com/hello --path /tmp/hello
cd /tmp/hello
gos run src/main.gos
```

You should see `hello from hello`.

## Supported platforms

Gossamer goroutines are stackful coroutines (corosensei).
Switching contexts requires a per-architecture inline-assembly
implementation, so the supported platform matrix is narrower than
"anything Rust can build":

| OS       | Architecture                  | Status |
| -------- | ----------------------------- | ------ |
| Linux    | x86_64                        | First-class |
| Linux    | aarch64                       | First-class |
| Linux    | armv7 (32-bit ARM)            | Supported |
| macOS    | x86_64 (Intel)                | Supported |
| macOS    | aarch64 (Apple Silicon)       | First-class |
| Windows  | x86_64 (MSVC ABI)             | Supported |

Other targets compile but the goroutine scheduler refuses to start
because corosensei has no context-switch backend for them.

`aarch64` Linux - including Raspberry Pi OS 64-bit - is exercised in CI
across all three tiers (the bytecode VM, the in-process Cranelift JIT,
and native `gos build`), not just cross-built. `gos run` is fully
self-contained on a Pi; native compilation there uses the device's
system LLVM (`llc`/`opt`) and C compiler (`sudo apt-get install -y llvm
clang`).

## Target toolchains

`gos build --target <triple>` validates the triple against the
registered set. Every Linux target -
`{x86_64,aarch64}-unknown-linux-{gnu,musl}` - cross-compiles to a real,
runnable binary from a Linux, macOS, or Windows host (Cross output is
validated against the bytecode VM under QEMU in CI, including Raspberry
Pi). The musl-static path is host-agnostic (rustup's self-contained CRT
+ `ld.lld`); the gnu-dynamic path uses the matching `*-linux-gnu-gcc` on
a Linux host, or a `GOS_CROSS_SYSROOT` on macOS/Windows. Cross-compiling
*to* macOS or Windows as a target remains out of scope (needs external
SDKs). (A fully-static single-file binary also comes from `gos build
--release` on a Linux host with the musl rustup target installed - no
`--target` needed.)

Musl targets (`x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl`) are gated behind the `musl` Cargo
feature. Rebuild with:

```sh
cargo build --workspace --release -p gossamer-driver --features musl
```

## Editor support

Pre-built plug-ins for VSCode, Vim, Neovim, Helix, Emacs, Sublime,
and Zed (plus a tree-sitter grammar) live at
[`danpozmanter/gossamer-editor-support`](https://github.com/danpozmanter/gossamer-editor-support).
Each one drives `gos lsp` for diagnostics, hover, completion,
go-to-definition, references, rename, and inlay hints.

## Next

- [Running](running.md)
- [Syntax](syntax.md)
