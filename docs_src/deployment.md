# Deployment guide

This page walks through cross-compiling a Gossamer service,
shipping the binary to a Linux server, and supervising it under
`systemd`.

The story is intentionally boring: Gossamer compiles to a single
static (or near-static) ELF / Mach-O / PE binary. There is no
JVM, no interpreter shim, no separate runtime to install on the
target. If your CI can produce a Linux x86_64 binary on a Linux
x86_64 runner, you can `scp` it and run it.

## Targets

Pre-built `gos` toolchain binaries ship for:

| Triple | Notes |
|---|---|
| `x86_64-unknown-linux-gnu` | Default Linux server target. |
| `aarch64-unknown-linux-gnu` | ARM64 servers (Graviton, Ampere). |
| `x86_64-apple-darwin` | Intel macOS (development). |
| `aarch64-apple-darwin` | Apple Silicon macOS (development). |
| `x86_64-pc-windows-msvc` | Windows servers (best-effort). |

Compiled Gossamer programs target whatever triple was passed to
`gos build --target <triple>`. The release matrix in
[`.github/workflows/release.yml`](https://github.com/danpozmanter/gossamer/blob/main/.github/workflows/release.yml)
is the source of truth for what we test on.

## Cross-compiling

### From Linux x86_64 → Linux aarch64

```sh
gos build --release --target aarch64-unknown-linux-gnu src/main.gos
```

You need:

- The cross linker `aarch64-linux-gnu-gcc` on `$PATH`.
- The cross C runtime, typically via `apt install gcc-aarch64-linux-gnu`.

The toolchain shells out to `clang` / `lld` if available;
otherwise falls back to GCC + system ld. `gos build --release`
will print the exact link command on `--verbose`.

### From macOS aarch64 → Linux x86_64

The pragmatic approach is to ssh into a Linux build runner and
build there. Cross-compiling from macOS to Linux requires the
musl toolchain plus the libgcc stubs that match the target's
glibc; pinning that combination across CI matrices has been
fragile in our testing.

If you do want native cross from macOS, install the [musl
cross toolchain](https://github.com/FiloSottile/homebrew-musl-cross)
and target `x86_64-unknown-linux-musl`:

```sh
brew install filosottile/musl-cross/musl-cross
gos build --release --target x86_64-unknown-linux-musl src/main.gos
```

A musl build is statically linked against libc and ships a
single self-contained binary. This is the recommended target for
container images.

## Container images

For services we recommend a `scratch`-based or `distroless`-based
image. The compiled musl binary needs nothing else. Sample
`Dockerfile`:

```dockerfile
# Build stage
FROM debian:bookworm-slim AS build
RUN apt-get update && apt-get install -y curl ca-certificates build-essential
RUN curl -fsSL https://github.com/danpozmanter/gossamer/releases/latest/download/gos-x86_64-unknown-linux-musl -o /usr/local/bin/gos && chmod +x /usr/local/bin/gos
WORKDIR /src
COPY . .
RUN gos build --release --target x86_64-unknown-linux-musl src/main.gos -o /out/server

# Runtime stage
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/server /server
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/server"]
```

Image sizes settle around 10–15 MiB for a typical HTTP service.

## Process supervision: systemd

Drop a unit file at `/etc/systemd/system/myservice.service`:

```ini
[Unit]
Description=My Gossamer service
After=network.target
Documentation=https://example.com/myservice

[Service]
Type=simple
User=myservice
Group=myservice
ExecStart=/usr/local/bin/myservice
Restart=on-failure
RestartSec=5s

# Environment
Environment="GOSSAMER_LOG=info"
Environment="LISTEN_ADDR=0.0.0.0:8080"

# Hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service

# Tuning
LimitNOFILE=65536
TasksMax=4096

[Install]
WantedBy=multi-user.target
```

Reload + start:

```sh
systemctl daemon-reload
systemctl enable --now myservice
journalctl -u myservice -f
```

### Graceful shutdown

Gossamer services should handle SIGTERM (sent by `systemctl
stop`) so in-flight requests finish before the process exits.
Use `os::signal` (v1.x):

```gos
use std::http
use std::os
use std::os::signal

fn main() {
    let server = http::Server::bind("0.0.0.0:8080")?
    let term = signal::on(signal::SIGTERM)
    go fn() {
        term.wait()
        server.shutdown()
    }()
    server.serve(handler)
}
```

Until `os::signal` lands, set `KillSignal=SIGTERM` and
`TimeoutStopSec=30s` in the unit file, and rely on systemd to
escalate to SIGKILL only after the timeout.

## Log shipping

Gossamer services log to stdout / stderr by default. systemd
captures both into the journal; ship the journal to your
log aggregator. For structured logs, use `std::slog::JsonHandler`
so the lines are JSON-line-formatted:

```gos
use std::slog

fn main() {
    let logger = slog::Logger::new(slog::JsonHandler::new(io::stdout()))
    logger.info("listening", &[("addr", "0.0.0.0:8080")])
    // ...
}
```

For shipping to a log aggregator that doesn't read the journal:

- **Loki**: `promtail` watches stdout via the systemd journal
  driver.
- **Cloudwatch**: `awslogs` agent reads `/var/log/syslog` and
  ships journal-tagged messages.
- **Stdout to network**: write to `/dev/stdout`; let your
  runtime forwarder handle it. Common in Kubernetes setups.

## Tuning

### GOMAXPROCS-equivalent

Gossamer reads the OS CPU count at startup and runs that many
scheduler threads. Override with `GOSSAMER_PROCS`:

```sh
GOSSAMER_PROCS=4 ./myservice
```

Set this in the systemd unit's `Environment=` line.

### Stack size per goroutine

Gossamer goroutines today run on real OS threads. Each thread
gets the host's default stack (8 MiB on Linux). For services
that spawn thousands of goroutines, drop the stack with
`ulimit`:

```sh
ulimit -s 1024  # 1 MiB stacks
./myservice
```

Or in systemd: `LimitSTACK=1048576`.

When M:N scheduling lands in v1.x this becomes a no-op —
goroutines will be lightweight and the OS-thread limit will
no longer constrain goroutine count.

### Memory

Gossamer's GC runs concurrently with the program but pauses for
mark / sweep phases. For tail-latency-sensitive services, set
`GOSSAMER_GC_TARGET=2.0` (default `1.5`) to grow the heap more
aggressively before triggering a collection. Higher = fewer
pauses, more RAM.

For container memory limits, give the container at least 2× the
service's typical working set. The GC will not free memory back
to the OS aggressively.

## Health check / readiness

A typical HTTP service exposes `/healthz`:

```gos
fn handler(req: http::Request) -> http::Response {
    match req.path() {
        "/healthz" => http::Response::ok("ok"),
        _ => app_handler(req),
    }
}
```

Wire this into the load balancer's readiness probe. systemd has
no native HTTP probe; use `Type=notify` and a small `sd_notify`
shim, or rely on the load balancer.

## Updates / zero-downtime deploys

The current recommended pattern is rolling restarts behind a
load balancer:

1. Deploy new binary to half the fleet.
2. Wait for healthchecks to pass.
3. Drain old half.
4. Repeat.

In-place hot-swap (`SIGUSR2` exec-the-new-binary-without-dropping-listeners)
is not in v1; the `os::exec` and `os::signal` work in v1.x will
make it possible.

## Cross-references

- [`non_goals_v1.md`](non_goals_v1.md) — what's deferred.
- [`perf_characteristics.md`](perf_characteristics.md) — GC,
  goroutine memory, scheduler under load.
- [`stdlib.md`](stdlib.md) — `slog`, `http`, `os`.
