# `std::jwt`

Status: experimental

RFC 7519 sign / verify for HS256 / HS384 / HS512, ES256, and EdDSA tokens.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Alg`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `type Alg` | Signing algorithm: HS256 / HS384 / HS512 / ES256 / EdDSA. |
| [`Claims`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `type Claims` | Standard registered claims plus a free-form custom map. |
| [`Header`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `type Header` | JWS / JWT header (alg, kid, typ). |
| [`VerifyOpts`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `type VerifyOpts` | Expected issuer / audience / clock leeway used by verify. |
| [`sign_eddsa`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `fn sign_eddsa(claims_json: String, signing_key_pem: String) -> Result<String, errors::Error>` | Sign with Ed25519 from a PEM-encoded private key. |
| [`sign_es256`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `fn sign_es256(claims_json: String, signing_key_pem: String) -> Result<String, errors::Error>` | Sign with ECDSA P-256 from a PEM-encoded private key. |
| [`sign_hs`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `fn sign_hs(alg: String, claims_json: String, key: Vec<u8>) -> Result<String, errors::Error>` | Sign claims with HMAC-SHA family using a shared key. |
| [`verify_eddsa`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `fn verify_eddsa(token: String, verifying_key_pem: String, leeway_secs: i64) -> Result<String, errors::Error>` | Verify an EdDSA token against a PEM-encoded public key. |
| [`verify_es256`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `fn verify_es256(token: String, verifying_key_pem: String, leeway_secs: i64) -> Result<String, errors::Error>` | Verify an ES256 token against a PEM-encoded public key. |
| [`verify_hs`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/jwt.rs) | `fn verify_hs(token: String, alg: String, key: Vec<u8>, leeway_secs: i64) -> Result<String, errors::Error>` | Verify an HS* token; checks signature plus VerifyOpts. |
