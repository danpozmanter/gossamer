# `std::net::ip`

Status: experimental

String-level IPv4 / IPv6 parsing and classification helpers.

## Public items

| Name | Kind | Description |
|---|---|---|
| `parse` | fn | Parses an IP string, returning its canonical form or None. |
| `is_valid` | fn | Reports whether the string is a valid v4 or v6 IP. |
| `is_v4` | fn | Reports whether the string is a valid v4 IP. |
| `is_v6` | fn | Reports whether the string is a valid v6 IP. |
| `to_string` | fn | Canonical lowercase string form of the IP. |
| `is_loopback` | fn | Reports whether the IP is a loopback address. |
| `is_private` | fn | Reports whether the IP is in a private range. |
| `is_multicast` | fn | Reports whether the IP is a multicast address. |
| `is_unspecified` | fn | Reports whether the IP is the unspecified address. |
| `octets` | fn | Byte octets of the IP as a Vec. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`is_loopback`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn is_loopback(addr: String) -> bool` | Reports whether the IP is a loopback address. |
| [`is_multicast`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn is_multicast(addr: String) -> bool` | Reports whether the IP is a multicast address. |
| [`is_private`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn is_private(addr: String) -> bool` | Reports whether the IP is in a private range. |
| [`is_unspecified`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn is_unspecified(addr: String) -> bool` | Reports whether the IP is the unspecified address. |
| [`is_v4`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn is_v4(addr: String) -> bool` | Reports whether the string is a valid v4 IP. |
| [`is_v6`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn is_v6(addr: String) -> bool` | Reports whether the string is a valid v6 IP. |
| [`is_valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn is_valid(addr: String) -> bool` | Reports whether the string is a valid v4 or v6 IP. |
| [`octets`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn octets(addr: net::ip::Addr) -> Vec<u8>` | Byte octets of the IP as a Vec. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn parse(addr: String) -> Result<net::ip::Addr, errors::Error>` | Parses an IP string, returning its canonical form or None. |
| [`to_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/net/ip.rs) | `fn to_string(addr: net::ip::Addr) -> String` | Canonical lowercase string form of the IP. |
