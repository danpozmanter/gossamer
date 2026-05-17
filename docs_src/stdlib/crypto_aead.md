# `std::crypto::aead`

Status: shipped

Authenticated encryption with associated data.

## Public items

| Name | Kind | Description |
|---|---|---|
| `aes_256_gcm_seal` | fn | AES-256-GCM seal: encrypts plaintext with key, nonce, and AAD. |
| `aes_256_gcm_open` | fn | AES-256-GCM open: decrypts and authenticates ciphertext. |
| `chacha20_poly1305_seal` | fn | ChaCha20-Poly1305 seal. |
| `chacha20_poly1305_open` | fn | ChaCha20-Poly1305 open. |

