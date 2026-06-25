# `std::encoding::base32`

Status: shipped

RFC 4648 base32 (uppercase) encode / decode.

## Public items

| Name | Kind | Description |
|---|---|---|
| `encode` | fn | Bytes -> base32 string. |
| `decode` | fn | Base32 string -> bytes. |
| `encode_string` | fn | Encodes a String as standard base32 text. |
| `decode_string` | fn | Decodes standard base32 text into a String. |
| `encode_hex` | fn | Encodes a String as extended-hex base32 text. |
| `decode_hex` | fn | Decodes extended-hex base32 text into a String. |

