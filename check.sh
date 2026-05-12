cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
# Stdlib docs drift gate — verifies docs_src/stdlib/ pages match
# what `manifest::ALL_MODULES` would emit. Build the binary first
# so the check uses the freshly built crate.
cargo build --bin gos
./target/debug/gos doc --emit-stdlib docs_src/stdlib --check
