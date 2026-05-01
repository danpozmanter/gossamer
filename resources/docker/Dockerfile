# Multi-stage build for the official Gossamer toolchain image.
# Stage 1 builds `gos` against musl on x86_64 so the resulting
# binary needs no glibc; stage 2 ships only the binary on Google's
# distroless static base for minimum surface area.

FROM rust:1.88-slim AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends \
        musl-tools \
        pkg-config \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# `rust-toolchain.toml` (pinned to channel = "stable") must be in
# place before `rustup target add`, otherwise rustup adds the target
# to the image's default toolchain — then `cargo build` auto-installs
# `stable` per the toml file, and the fresh `stable` toolchain has
# no musl target, producing `error[E0463]: can't find crate for
# `core``. Copy the toml on its own first so the target-add layer
# stays cached across source-only changes.
COPY rust-toolchain.toml ./
RUN rustup target add x86_64-unknown-linux-musl
COPY . .
RUN cargo build --release --bin gos --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /src/target/x86_64-unknown-linux-musl/release/gos /usr/local/bin/gos
COPY LICENSE /LICENSE
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/gos"]
