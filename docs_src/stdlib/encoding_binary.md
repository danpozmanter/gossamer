# `std::encoding::binary`

Big/little-endian integer packing and varint codecs.

## Public items

| Name | Kind | Description |
|---|---|---|
| `get_u8` | fn | Reads a single byte. |
| `put_u8` | fn | Writes a single byte. |
| `get_u16_be` | fn | Reads a big-endian u16. |
| `put_u16_be` | fn | Writes a big-endian u16. |
| `get_u16_le` | fn | Reads a little-endian u16. |
| `put_u16_le` | fn | Writes a little-endian u16. |
| `get_u32_be` | fn | Reads a big-endian u32. |
| `put_u32_be` | fn | Writes a big-endian u32. |
| `get_u32_le` | fn | Reads a little-endian u32. |
| `put_u32_le` | fn | Writes a little-endian u32. |
| `get_u64_be` | fn | Reads a big-endian u64. |
| `put_u64_be` | fn | Writes a big-endian u64. |
| `get_u64_le` | fn | Reads a little-endian u64. |
| `put_u64_le` | fn | Writes a little-endian u64. |
| `uvarint` | fn | Decodes an unsigned varint. |
| `varint` | fn | Decodes a signed varint (zigzag). |
| `put_uvarint` | fn | Encodes an unsigned varint. |
| `put_varint` | fn | Encodes a signed varint (zigzag). |

