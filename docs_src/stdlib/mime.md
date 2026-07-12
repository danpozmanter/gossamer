# `std::mime`

Status: shipped

RFC 2045 media type parsing, parameter extraction, and extension lookup.

## Public items

| Name | Kind | Description |
|---|---|---|
| `parse` | fn | Canonical `type/subtype` form of the input, or empty on parse failure. |
| `top` | fn | Top-level type (e.g. `text`) of a media type, or empty. |
| `sub` | fn | Subtype (e.g. `html`) of a media type, or empty. |
| `charset` | fn | Return the `charset` parameter, or empty. |
| `boundary` | fn | Return the multipart `boundary` parameter, or empty. |
| `param` | fn | Return an arbitrary parameter by key, or empty. |
| `type_by_extension` | fn | Canonical media type for a filename extension (dot optional), or empty. |
| `extension_by_type` | fn | Canonical extension (no leading dot) for a media type, or empty. |
| `is_valid` | fn | Return true iff the string parses as a valid media type. |

