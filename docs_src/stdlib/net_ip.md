# `std::net::ip`

Status: shipped

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

