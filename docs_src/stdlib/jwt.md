# `std::jwt`

Status: experimental

RFC 7519 sign / verify for HS256 / HS384 / HS512, ES256, and EdDSA tokens.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Alg` | type | Signing algorithm: HS256 / HS384 / HS512 / ES256 / EdDSA. |
| `Header` | type | JWS / JWT header (alg, kid, typ). |
| `Claims` | type | Standard registered claims plus a free-form custom map. |
| `VerifyOpts` | type | Expected issuer / audience / clock leeway used by verify. |
| `sign_hs` | fn | Sign claims with HMAC-SHA family using a shared key. |
| `verify_hs` | fn | Verify an HS* token; checks signature plus VerifyOpts. |
| `sign_es256` | fn | Sign with ECDSA P-256 from a PEM-encoded private key. |
| `verify_es256` | fn | Verify an ES256 token against a PEM-encoded public key. |
| `sign_eddsa` | fn | Sign with Ed25519 from a PEM-encoded private key. |
| `verify_eddsa` | fn | Verify an EdDSA token against a PEM-encoded public key. |

