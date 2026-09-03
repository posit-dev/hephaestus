# src/image/CLAUDE.md

Raster readers and writers. One cargo feature per format, one codec each, nothing else.

## What this module does

Turns the pixel buffer a `Renderer` fills into an encoded image, and an encoded image back into pixels. Four formats, each behind its own feature: `png`, `jpeg`, `tiff`, `webp`. Every format offers the same three entry points per direction, and `src/image/png.rs` is the template for a new one:

- `write_X_to<W: Write>(writer, width, height, pixels, .., dpi)` — the primitive; the other two call it.
- `encode_X(width, height, pixels, .., dpi)` — the bytes in memory, for an HTTP response body, a base64 data URL, a clipboard payload.
- `write_X(path, width, height, pixels, .., dpi)` — `File::create` plus a `BufWriter`.
- `read_X_from<R: BufRead + Seek>(reader)` — the primitive on the read side.
- `decode_X(bytes)` — bytes already in memory.
- `read_X(path)` — `File::open` plus a `BufReader`.

Per-format submodules are private; `mod.rs` re-exports their functions flat, so callers write `hephaestus::image::write_jpeg`. `src/png.rs` at the crate root aliases the PNG three so `hephaestus::png::write_png` also resolves.

Two more read entry points name no format at all:

- `decode_image(bytes)` — dispatch on the buffer's signature, so a `.png` holding a JPEG still decodes and a location with no extension is no obstacle. A format whose codec this build lacks reports `io::ErrorKind::Unsupported`, which is the distinction between "cannot read that here" and "those bytes are not an image".
- `read_image(path)` — the same, reading the whole file first, since the signature is what picks the decoder.

They exist because a caller naming a *location* rather than a format cannot say which decoder it wants: `ImageRegistry::resolve` reads a path or URL for an `ImageGeom` channel or a markdown `![](…)` tag, and the file decides.

## Recording the dpi

Pixels alone do not say how large a picture is. A file that declares no resolution is read at whatever the viewer assumes — 72 dpi, typically — so a plot rendered at a device pixel ratio of 2 claims twice its intended physical size on a page. So every write entry point ends in an `Option<f64>` dpi: `None` records nothing, and a value lands wherever the format keeps one.

- **PNG** — a `pHYs` chunk, in pixels per metre. The one format that omits the field entirely for `None`.
- **JPEG** — the JFIF header's density fields, with the unit byte set to inches. `None` leaves the encoder's default, which declares a 1:1 pixel *aspect ratio* and no resolution.
- **TIFF** — `XResolution` / `YResolution` with `ResolutionUnit` set to inches. The three have to be written together: the encoder writes 1/1 with no unit when the image is opened, and a resolution in inches over an unset unit reads as a bare aspect ratio.
- **WebP** — an EXIF block, because the bitstream has no resolution field at all. It is a hand-built little-endian TIFF stream of exactly those three tags. Carrying it promotes the file to the extended (`VP8X`) container, which is why `None` matters here beyond saving bytes.

Two shared helpers in `mod.rs` keep the four agreeing. `usable_dpi` is the floor and ceiling: a figure that is not finite and positive declares no resolution, so it lands on 1 dpi rather than saturating or wrapping, and each format passes the largest its own field can express. `dpi_rational` is for the two formats storing a ratio — exact for a whole number of dots per inch, and over a denominator of 10000 otherwise, so a fractional display scale survives.

`sips -g dpiWidth <file>` reads all four on macOS, which is the check that the bytes say what we think they say.

## Reading

A reader hands back a `crate::brush::Image` — the type `SceneBuilder::draw_image` and `plot::ImageGeom` consume — normalised to the same buffer contract the writers enforce. `from_rgba8` in `mod.rs` is the single construction point (and is public, for a caller holding pixels from somewhere else); `expand_to_rgba8` is the single widening point.

Three normalisations, applied by every format:

- **Grey replicates** across the colour channels rather than staying one sample.
- **A missing alpha channel becomes opaque**, so an RGB or grey file does not read back invisible.
- **Deep samples narrow to 8 bits.** PNG asks its decoder for this via `STRIP_16`; TIFF and JPEG *refuse* anything that isn't 8-bit rather than guessing, and so does any colour model needing a conversion this module won't do without a profile (CMYK, YCbCr, Lab).

`BufRead + Seek` is the reader bound on all four, which is what the container formats actually need — the PNG and WebP decoders demand it outright. `BufReader<File>` and `io::Cursor` both satisfy it. Only JPEG could take less; it takes the same bound so the four read the same way.

## The buffer contract

Every writer takes the same thing: RGBA8, **straight (un-premultiplied) alpha**, tightly packed top-down rows, exactly `width * height * 4` bytes. That's what `Renderer::render_to_buffer` produces — the Vello backend de-pads the 256-byte-aligned GPU readback before it returns, so no writer ever sees a stride. `check_pixels` in `mod.rs` is the single enforcement point: non-zero dimensions, exact length, `io::ErrorKind::InvalidInput` on either failure, and nothing written to the writer when it trips. It computes the expected length in `u128` because `width * height * 4` overflows a 32-bit `usize` — and a `u64` at the top of the `u32` range — well inside dimensions the formats allow.

## Conventions

- **Encoding is not part of the `Renderer` trait.** Backends produce buffers; formats are a separate concern, which is why this module sits at the crate root rather than under `backend/`.
- **`io::Result`, no error type of our own.** These are the only modules in the crate that surface raw `std::io`; upstream encoder errors flatten through a private `io_err` via `io::Error::other`. A format-specific error enum would be the first of its kind here — don't add one without a reason the `io::Error` message can't carry.
- **One knob per format, and only where the format has one.** JPEG takes `quality` and a composite `background`; TIFF takes `TiffCompression`; PNG takes `PngCompression`; WebP takes nothing beyond the dpi, which every writer takes as its last argument. Resist options structs until a second knob actually lands.
- **Our own enum for format choices.** `TiffCompression` and `PngCompression` are ours, each mapped to its crate's own `Compression` inside the writer, for the same reason `FillRule` and `BlendMode` are ours: a dependency's enum in a public signature is a dependency in the semver contract. They name intent rather than mirroring the backing crate's levels, which is why `PngCompression` has four variants over the `png` crate's five.
- **All encoders are pure Rust** and build for wasm. That rules out libwebp (and so rules out lossy WebP — see below); it's a constraint worth keeping.

## Format gotchas

- **JPEG has no alpha channel.** `jpeg_encoder::ColorType::Rgba` silently *discards* it, which turns a transparent-background render black. So `write_jpeg` composites onto an explicit `background` and encodes `ColorType::Rgb`. Compositing happens in the encoded sRGB byte space, the space the renderer itself blends in; a fully opaque buffer is bit-exact passthrough. The background's own alpha is ignored. Reading is the same story from the other side: `decode_jpeg` always returns a fully opaque image, and the pixels are not the ones that went in, so the JPEG test asserts the shape rather than equality.
- **JPEG is the one format whose two directions come from two crates.** `jpeg-encoder` is write-only, so the `jpeg` feature also pulls `jpeg-decoder` — with `default-features = false`, which drops `rayon`: a plot places one image at a time, and a thread pool does not build for wasm.
- **JPEG dimensions are `u16`.** The frame header cannot express more than 65535, so the writer rejects larger images itself rather than surfacing an opaque encoder error.
- **TIFF needs `Write + Seek`.** The tag directory records offsets into the image data that follows it. `encode_tiff` therefore goes through an `io::Cursor`, and `write_tiff` relies on `BufWriter<File>` being `Seek`.
- **TIFF alpha must be declared.** The `tiff` crate's `colortype::RGBA8` writes four samples but no `ExtraSamples` tag, leaving the fourth undefined. The writer sets it to `UnassociatedAlpha` through `image.encoder().write_tag(..)` — deliberately not `ImageEncoder::extra_samples`, which also bumps the per-row sample count and would double-count the alpha RGBA8 already declares.
- **PNG compression is the widest knob here, and the default is not the fast one.** All four `PngCompression` levels are lossless, but the cost between them scales with how much detail the plot holds rather than with its pixel count. At 1200x800: a sparse plot encodes in 1.9 ms at `Fast` against 8.9 ms at `Balanced`, for 61 KB against 43 KB; a 50k-mark scatter encodes in 8 ms against **146 ms**, for 2305 KB against 1483 KB. So `Balanced` costs 4x on a figure and 18x on a dense frame, buying about a third off the bytes either way. It is the default because writing a figure is the common case; a caller encoding a frame per animation tick wants `Fast`. `Small` is a trap on this content — it lands within a percent of `Balanced` for considerably more time, which is why the size test pins the two as near-equals instead of ordering them.
- **WebP is lossless only.** `image-webp` implements the VP8L bitstream and not lossy VP8 — that's a gap in the pure-Rust ecosystem, not a bug in the crate, since lossy WebP is intra-frame video encoding. Hence no quality parameter. Max dimension is 16384, checked here so the error names the limit. On sparse plot output it's the smallest of the four — 31 KB at 1200x800 against PNG `Balanced`'s 43 KB — but that reverses as marks fill the frame: the same 50k-mark scatter is 1859 KB against PNG's 1483 KB. JPEG is the largest, flat fills being the worst case for a DCT codec.

## Adding a format

1. Add the feature and its optional dependency in `Cargo.toml`, next to the other three.
2. Add `src/image/<format>.rs` with the three entry points, using `super::check_pixels` first and `super::check_dimension_limit` if the format has a cap. `write_*_to` is the primitive; the other two delegate to it. Take the trailing dpi through `super::usable_dpi`, or `super::dpi_rational` if the field is a ratio.
3. Add the three read entry points beside them, normalising through `super::expand_to_rgba8` and `super::from_rgba8`. If the dependency has no decoder, that is a second optional dependency on the same feature gate — see the JPEG note above.
4. Declare the private `mod` and the gated flat re-export in `mod.rs`, with the `#[cfg_attr(docsrs, doc(cfg(..)))]` badge.
5. Add the feature to the `any(..)` gate on `pub mod image;` in `src/lib.rs`. That gate is also why `from_rgba8` is not reachable with no format feature at all — a build that decodes nothing constructs a `brush::Image` directly.
6. Unit tests in the writer: container magic bytes, `encode_*` and `write_*` agreeing byte-for-byte, wrong-length rejection with an untouched sink, an `encode_* → decode_*` pixel round-trip, a fewer-than-four-channel file widening to opaque RGBA, garbage bytes reported as `InvalidData` rather than panicking, a dpi read back out of the file, a `None` declaring no resolution, and an unusable dpi still producing a decodable file.
7. Add a `#[cfg(feature = "<format>")]` case to `tests/image_writers.rs` so real rendered pixels go through it.
8. Register it: the per-feature loop in `.github/workflows/check.yml`, the README feature table, the root `CLAUDE.md` feature list, `CHANGELOG.md`, and `examples/image_formats.rs`.

## Cross-references

- `src/backend/CLAUDE.md` — the straight-alpha output convention and the de-padded readback these writers depend on.
- `tests/alpha_format.rs` — pins that convention; if it fails, every writer here is producing wrong pixels.
