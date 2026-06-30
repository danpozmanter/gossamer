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

## Target toolchains

`gos build --target <triple>` validates the triple against the
registered set - the supported platforms above plus
`riscv64gc-unknown-linux-gnu`, `wasm32-unknown-unknown`, and
`wasm32-wasi`. Only the host triple currently links to a runnable
binary, though: a non-host `--target` emits a placeholder artifact and
reports `cross-link pending`. Build each target on a native runner of
that architecture for now. (A fully-static single-file binary comes
from `gos build --release` on a Linux host with the musl rustup target
installed - no `--target` needed.)

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
