# `std::encoding::pem`

Status: shipped

PEM block encoder and decoder.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Block` | type | A decoded PEM block with type label and DER bytes. |
| `encode` | fn | Encodes a Block as a PEM string. |
| `decode` | fn | Decodes the first PEM block from a string. |
| `decode_all` | fn | Decodes all PEM blocks from a string. |

