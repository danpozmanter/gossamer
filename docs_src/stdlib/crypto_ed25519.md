# `std::crypto::ed25519`

Status: shipped

Ed25519 digital signatures.

## Public items

| Name | Kind | Description |
|---|---|---|
| `keypair` | fn | Generates a fresh Ed25519 keypair from the host CSPRNG. |
| `sign` | fn | Signs a message with a 32-byte secret key. |
| `verify` | fn | Verifies a 64-byte signature against a 32-byte public key. |

