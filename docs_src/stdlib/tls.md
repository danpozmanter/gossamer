# `std::tls`

Rustls-backed TLS support exposed through `http::serve_tls` and `net::TcpStream` TLS upgrades. The configuration constructors are host-runtime internals, not Gossamer callables.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/tls.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

This library currently declares no additional public Gossamer-language items. Its runtime integration is documented in the implementation source above.
