# `std::net`

Status: experimental

TCP/UDP networking primitives.

## Public items

| Name | Kind | Description |
|---|---|---|
| `TcpListener` | type | Accepts incoming TCP connections. |
| `TcpStream` | type | Bidirectional TCP byte stream; supports read/write, TLS upgrade, close, and read/write timeout setters. |
| `UdpSocket` | type | Bound UDP socket for datagram I/O. |
| `lookup` | fn | Resolves a hostname to its IP addresses. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`TcpListener`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `type` — see the source declaration | Accepts incoming TCP connections. |
| [`TcpStream`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `type` — see the source declaration | Bidirectional TCP byte stream; supports read/write, TLS upgrade, close, and read/write timeout setters. |
| [`UdpSocket`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `type` — see the source declaration | Bound UDP socket for datagram I/O. |
| [`is_loopback`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_loopback(addr: String) -> bool` | Reports whether the IP is a loopback address. |
| [`is_multicast`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_multicast(addr: String) -> bool` | Reports whether the IP is a multicast address. |
| [`is_private`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_private(addr: String) -> bool` | Reports whether the IP is in a private range. |
| [`is_unspecified`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_unspecified(addr: String) -> bool` | Reports whether the IP is the unspecified address. |
| [`is_v4`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_v4(addr: String) -> bool` | Reports whether the string is a valid v4 IP. |
| [`is_v6`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_v6(addr: String) -> bool` | Reports whether the string is a valid v6 IP. |
| [`is_valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_valid(addr: String) -> bool` | Reports whether the string is a valid v4 or v6 IP. |
| [`octets`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn octets(addr: net::ip::Addr) -> Vec<u8>` | Byte octets of the IP as a Vec. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn parse(addr: String) -> Result<net::ip::Addr, errors::Error>` | Parses an IP string, returning its canonical form or None. |
| [`to_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn to_string(addr: net::ip::Addr) -> String` | Canonical lowercase string form of the IP. |
| [`lookup`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn lookup(host: String) -> Result<Vec<String>, errors::Error>` | Resolves a hostname to its IP addresses. |
| [`host_of`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn host_of(addr_port: String) -> String` | Host portion of an addr:port string, or empty on parse failure. |
| [`is_loopback`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_loopback(addr: String) -> bool` | Return true iff the string parses as a loopback IP (127.0.0.1 / ::1). |
| [`is_multicast`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_multicast(addr: String) -> bool` | Return true iff the string parses as a multicast IP. |
| [`is_private`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_private(addr: String) -> bool` | Return true iff the IP is RFC1918 (v4) or ULA fc00::/7 (v6). |
| [`is_unspecified`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_unspecified(addr: String) -> bool` | Return true iff the string parses as the unspecified IP (0.0.0.0 / ::). |
| [`is_v4`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_v4(addr: String) -> bool` | Return true iff the string parses as a v4 IP. |
| [`is_v6`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_v6(addr: String) -> bool` | Return true iff the string parses as a v6 IP. |
| [`is_valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn is_valid(addr: String) -> bool` | Return true iff the string parses as a v4 or v6 IP. |
| [`join_addr_port`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn join_addr_port(addr: String, port: i64) -> String` | Compose an addr:port string from host and port, or empty on failure. |
| [`normalize`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn normalize(addr: String) -> Result<String, errors::Error>` | Canonical lowercase form of the IP, or empty string on parse failure. |
| [`port_of`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn port_of(addr_port: String) -> i64` | Port portion of an addr:port string, or -1 on parse failure. |
| [`Url`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `type` — see the source declaration | Parsed URL. |
| [`path_escape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn path_escape(text: String) -> String` | Percent-encodes a URL path segment. |
| [`path_unescape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn path_unescape(text: String) -> String` | Inverse of `path_escape`. |
| [`query_escape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn query_escape(text: String) -> String` | Percent-encodes a query parameter. |
| [`query_unescape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net.rs) | `fn query_unescape(text: String) -> String` | Inverse of `query_escape`. |
