# Multi-stage build for the official Gossamer toolchain image.
# Stage 1 builds `gos` against musl on x86_64 so the resulting
# binary needs no glibc; stage 2 ships only the binary on Google's
# distroless static base for minimum surface area.

FROM rust:1.85-slim AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends \
        musl-tools \
        pkg-config \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl
COPY . .
RUN cargo build --release --bin gos --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /src/target/x86_64-unknown-linux-musl/release/gos /usr/local/bin/gos
COPY LICENSE /LICENSE
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/gos"]
