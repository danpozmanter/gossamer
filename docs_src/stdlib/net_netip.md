# `std::net::netip`

Status: experimental

Typed IP-address parsing, classification, and addr:port helpers (Go's net/netip shape).

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`host_of`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn host_of(addr_port: String) -> String` | Host portion of an addr:port string, or empty on parse failure. |
| [`is_loopback`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn is_loopback(addr: String) -> bool` | Return true iff the string parses as a loopback IP (127.0.0.1 / ::1). |
| [`is_multicast`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn is_multicast(addr: String) -> bool` | Return true iff the string parses as a multicast IP. |
| [`is_private`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn is_private(addr: String) -> bool` | Return true iff the IP is RFC1918 (v4) or ULA fc00::/7 (v6). |
| [`is_unspecified`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn is_unspecified(addr: String) -> bool` | Return true iff the string parses as the unspecified IP (0.0.0.0 / ::). |
| [`is_v4`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn is_v4(addr: String) -> bool` | Return true iff the string parses as a v4 IP. |
| [`is_v6`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn is_v6(addr: String) -> bool` | Return true iff the string parses as a v6 IP. |
| [`is_valid`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn is_valid(addr: String) -> bool` | Return true iff the string parses as a v4 or v6 IP. |
| [`join_addr_port`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn join_addr_port(addr: String, port: i64) -> String` | Compose an addr:port string from host and port, or empty on failure. |
| [`normalize`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn normalize(addr: String) -> Result<String, errors::Error>` | Canonical lowercase form of the IP, or empty string on parse failure. |
| [`port_of`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/net_ip_typed.rs) | `fn port_of(addr_port: String) -> i64` | Port portion of an addr:port string, or -1 on parse failure. |
