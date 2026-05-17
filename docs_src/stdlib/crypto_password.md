# `std::crypto::password`

Status: shipped

Argon2id password hashing facade: PHC-string hash / verify / re-hash policy.

## Public items

| Name | Kind | Description |
|---|---|---|
| `hash` | fn | Argon2id hash of plaintext; returns a PHC-format string for storage. |
| `verify` | fn | Constant-time verify of plaintext against a stored PHC string. |
| `needs_rehash` | fn | True iff the stored PHC's parameters are below the current defaults. |
| `current_algorithm` | fn | Active password-hash algorithm name (currently `argon2id`). |
| `current_params` | fn | Active parameters: (m_cost_kib, t_cost_iters, p_cost_parallelism). |

