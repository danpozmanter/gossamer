# `std::image`

Status: unproven

Opaque RGBA8 image handles with PNG and JPEG codecs.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Handle API and codec behavior

`new`, `filled`, and `decode_base64` return opaque nonzero `i64` image
handles. Pixels are packed `0xRRGGBBAA`; `pixel` returns `-1` out of bounds,
and `set_pixel` returns `false` out of bounds. Invalid dimensions and malformed
or unsupported base64 input return handle `0` without panicking.

`encode_png_base64` is lossless for RGBA8. `encode_jpeg_base64` accepts quality
`1..=100`; JPEG composites alpha against black and returns opaque decoded
pixels. The API has VM, forced-Cranelift-JIT, and LLVM-native output parity,
with malformed-input coverage in `image_png_jpeg_round_trip_and_invalid_input_match_native`.

See `examples/image_png_jpeg.gos` and `benchmarks/perf/image_png_jpeg.gos` for
consumer and benchmark workloads.
