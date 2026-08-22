# src/image/CLAUDE.md

Raster writers. One cargo feature per format, one encoder each, nothing else.

## What this module does

Turns the pixel buffer a `Renderer` fills into an encoded image. Four formats, each behind its own feature: `png`, `jpeg`, `tiff`, `webp`. Every format offers the same three entry points, and `src/image/png.rs` is the template for a new one:

- `write_X_to<W: Write>(writer, width, height, pixels, ..)` — the primitive; the other two call it.
- `encode_X(width, height, pixels, ..)` — the bytes in memory, for an HTTP response body, a base64 data URL, a clipboard payload.
- `write_X(path, width, height, pixels, ..)` — `File::create` plus a `BufWriter`.

Per-format submodules are private; `mod.rs` re-exports their functions flat, so callers write `hephaestus::image::write_jpeg`. `src/png.rs` at the crate root aliases the PNG three so `hephaestus::png::write_png` also resolves.

## The buffer contract

Every writer takes the same thing: RGBA8, **straight (un-premultiplied) alpha**, tightly packed top-down rows, exactly `width * height * 4` bytes. That's what `Renderer::render_to_buffer` produces — the Vello backend de-pads the 256-byte-aligned GPU readback before it returns, so no writer ever sees a stride. `check_pixels` in `mod.rs` is the single enforcement point: non-zero dimensions, exact length, `io::ErrorKind::InvalidInput` on either failure, and nothing written to the writer when it trips. It computes the expected length in `u128` because `width * height * 4` overflows a 32-bit `usize` — and a `u64` at the top of the `u32` range — well inside dimensions the formats allow.

## Conventions

- **Encoding is not part of the `Renderer` trait.** Backends produce buffers; formats are a separate concern, which is why this module sits at the crate root rather than under `backend/`.
- **`io::Result`, no error type of our own.** These are the only modules in the crate that surface raw `std::io`; upstream encoder errors flatten through a private `io_err` via `io::Error::other`. A format-specific error enum would be the first of its kind here — don't add one without a reason the `io::Error` message can't carry.
- **One knob per format, and only where the format has one.** JPEG takes `quality` and a composite `background`; TIFF takes `TiffCompression`; PNG and WebP take nothing. Resist options structs until a second knob actually lands.
- **Our own enum for format choices.** `TiffCompression` is ours, mapped to the `tiff` crate's `Compression` inside the writer, for the same reason `FillRule` and `BlendMode` are ours: a dependency's enum in a public signature is a dependency in the semver contract.
- **All encoders are pure Rust** and build for wasm. That rules out libwebp (and so rules out lossy WebP — see below); it's a constraint worth keeping.

## Format gotchas

- **JPEG has no alpha channel.** `jpeg_encoder::ColorType::Rgba` silently *discards* it, which turns a transparent-background render black. So `write_jpeg` composites onto an explicit `background` and encodes `ColorType::Rgb`. Compositing happens in the encoded sRGB byte space, the space the renderer itself blends in; a fully opaque buffer is bit-exact passthrough. The background's own alpha is ignored.
- **JPEG dimensions are `u16`.** The frame header cannot express more than 65535, so the writer rejects larger images itself rather than surfacing an opaque encoder error.
- **TIFF needs `Write + Seek`.** The tag directory records offsets into the image data that follows it. `encode_tiff` therefore goes through an `io::Cursor`, and `write_tiff` relies on `BufWriter<File>` being `Seek`.
- **TIFF alpha must be declared.** The `tiff` crate's `colortype::RGBA8` writes four samples but no `ExtraSamples` tag, leaving the fourth undefined. The writer sets it to `UnassociatedAlpha` through `image.encoder().write_tag(..)` — deliberately not `ImageEncoder::extra_samples`, which also bumps the per-row sample count and would double-count the alpha RGBA8 already declares.
- **WebP is lossless only.** `image-webp` implements the VP8L bitstream and not lossy VP8 — that's a gap in the pure-Rust ecosystem, not a bug in the crate, since lossy WebP is intra-frame video encoding. Hence no quality parameter. Max dimension is 16384, checked here so the error names the limit. On plot output it's the smallest of the four; JPEG is the largest, flat fills being the worst case for a DCT codec.

## Adding a format

1. Add the feature and its optional dependency in `Cargo.toml`, next to the other three.
2. Add `src/image/<format>.rs` with the three entry points, using `super::check_pixels` first and `super::check_dimension_limit` if the format has a cap.
3. Declare the private `mod` and the gated flat re-export in `mod.rs`, with the `#[cfg_attr(docsrs, doc(cfg(..)))]` badge.
4. Add the feature to the `any(..)` gate on `pub mod image;` in `src/lib.rs`.
5. Unit tests in the writer: container magic bytes, `encode_*` and `write_*` agreeing byte-for-byte, wrong-length rejection with an untouched sink. Round-trip the pixels if the dependency ships a decoder.
6. Add a `#[cfg(feature = "<format>")]` case to `tests/image_writers.rs` so real rendered pixels go through it.
7. Register it: the per-feature loop in `.github/workflows/check.yml`, the README feature table, the root `CLAUDE.md` feature list, `CHANGELOG.md`, and `examples/image_formats.rs`.

## Cross-references

- `src/backend/CLAUDE.md` — the straight-alpha output convention and the de-padded readback these writers depend on.
- `tests/alpha_format.rs` — pins that convention; if it fails, every writer here is producing wrong pixels.
