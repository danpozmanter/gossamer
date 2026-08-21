# `std::http::query`

Status: experimental

A request's query string is already parsed: read `request.query` for the raw text and `request.query_pairs` for the decoded name/value pairs.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_query.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Query`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_query.rs) | `type Query` | Parsed query string with typed get / get_all / contains. |
