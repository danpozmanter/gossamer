# `std::crypto::ecdsa`

ECDSA over the NIST P-256 curve.

## Public items

| Name | Kind | Description |
|---|---|---|
| `keypair_pem` | fn | Generates (secret_pem, public_pem) for a fresh P-256 keypair. |
| `sign_pem` | fn | Signs a message with a PKCS#8-PEM-encoded P-256 secret key. |
| `verify_pem` | fn | Verifies a DER-encoded signature against an SPKI-PEM public key. |

