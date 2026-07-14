# `std::crypto::x509`

Status: shipped

X.509 certificate parsing.

## Public items

| Name | Kind | Description |
|---|---|---|
| `CertInfo` | type | Inspected fields of an X.509 certificate. |
| `parse_pem` | fn | Parses one PEM-encoded certificate. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`CertInfo`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `type` — see the source declaration | Inspected fields of an X.509 certificate. |
| [`parse_pem`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `fn parse_pem(pem: String) -> Result<x509::Certificate, errors::Error>` | Parses one PEM-encoded certificate. |
