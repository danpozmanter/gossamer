# `std::net::url`

Status: shipped

Network URL parsing and component escaping; never use filesystem-path rules.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/url.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Url`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/url.rs) | `type Url` | Parsed URL. |
| [`path_escape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/url.rs) | `fn path_escape(text: String) -> String` | Percent-encodes a URL path segment. |
| [`path_unescape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/url.rs) | `fn path_unescape(text: String) -> String` | Inverse of `path_escape`. |
| [`query_escape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/url.rs) | `fn query_escape(text: String) -> String` | Percent-encodes a query parameter. |
| [`query_unescape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/url.rs) | `fn query_unescape(text: String) -> String` | Inverse of `query_escape`. |
