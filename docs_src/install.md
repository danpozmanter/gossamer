# Installing Gossamer

Pre-release — the only supported install path today is a source
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

- **Rust toolchain** — stable, edition 2024, MSRV 1.88. The
  workspace's `rust-toolchain.toml` pins a minimum.
- **A C linker** — required by Cargo, not by Gossamer. `cc` /
  `gcc` / `clang` will do.

## Verifying

```sh
gos --version
gos new example.com/hello --path /tmp/hello
cd /tmp/hello
gos run src/main.gos
```

You should see `hello from hello`.

## Target toolchains

`gos build --target <triple>` enables cross-compilation. The
default registered set includes:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`
- `riscv64gc-unknown-linux-gnu`
- `wasm32-unknown-unknown`
- `wasm32-wasi`

Musl targets (`*-unknown-linux-musl`) are gated behind the
`musl` Cargo feature. Rebuild with:

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
