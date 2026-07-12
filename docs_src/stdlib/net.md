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

