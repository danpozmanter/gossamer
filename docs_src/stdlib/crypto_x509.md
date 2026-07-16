# `std::crypto::x509`

Status: experimental

X.509 certificate parsing.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`CertInfo`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `type CertInfo` | Inspected fields of an X.509 certificate. |
| [`parse_pem`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `fn parse_pem(pem: String) -> Result<x509::Certificate, errors::Error>` | Parses one PEM-encoded certificate. |
| [`verify_server_certificate_with_crls`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `fn verify_server_certificate_with_crls(chain_pem: String, roots_pem: String, hostname: String, crl_pem: String) -> Result<(), errors::Error>` | Verifies a leaf-first server chain and hostname against private roots and mandatory CRLs. Unknown revocation status and expired CRLs fail closed. |
