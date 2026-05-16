# `std::net::netip`

Typed IP-address parsing, classification, and addr:port helpers (Go's net/netip shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `is_valid` | fn | Return true iff the string parses as a v4 or v6 IP. |
| `is_v4` | fn | Return true iff the string parses as a v4 IP. |
| `is_v6` | fn | Return true iff the string parses as a v6 IP. |
| `is_loopback` | fn | Return true iff the string parses as a loopback IP (127.0.0.1 / ::1). |
| `is_unspecified` | fn | Return true iff the string parses as the unspecified IP (0.0.0.0 / ::). |
| `is_multicast` | fn | Return true iff the string parses as a multicast IP. |
| `is_private` | fn | Return true iff the IP is RFC1918 (v4) or ULA fc00::/7 (v6). |
| `normalize` | fn | Canonical lowercase form of the IP, or empty string on parse failure. |
| `host_of` | fn | Host portion of an addr:port string, or empty on parse failure. |
| `port_of` | fn | Port portion of an addr:port string, or -1 on parse failure. |
| `join_addr_port` | fn | Compose an addr:port string from host and port, or empty on failure. |

