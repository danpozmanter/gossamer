cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
