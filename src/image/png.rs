//! PNG reader and writer.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Seek, Write};
use std::path::Path;

use super::{check_pixels, expand_to_rgba8, from_rgba8, usable_dpi};
use crate::brush::Image;

/// The largest dpi the pixels-per-metre field of a `pHYs` chunk can express.
const MAX_DPI: f64 = u32::MAX as f64 * 0.0254;

/// How a PNG writer compresses image data.
///
/// All four are lossless; they trade encode cost against file size. The
/// spread is wide on plot output, and wide enough that the choice matters: a
/// caller writing one figure and a caller encoding a frame per animation
/// tick want opposite ends of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PngCompression {
    /// Store rows verbatim, unfiltered. Much the cheapest to produce, and
    /// far the largest — a plot frame lands near its raw
    /// `width * height * 4` bytes.
    None,
    /// Cheap filtering and a fast deflate. The end to reach for when the
    /// encode sits on a frame deadline: on a dense plot it costs a small
    /// fraction of [`PngCompression::Balanced`] for a file around half again
    /// as large.
    Fast,
    /// Adaptive filtering at deflate's default level, and the default here.
    #[default]
    Balanced,
    /// Spend considerably longer looking for a smaller file. Plot output is
    /// not the content that rewards it — the result lands within a percent
    /// of [`PngCompression::Balanced`] either way, so reach for this only
    /// when the bytes are worth measuring for.
    Small,
}

impl PngCompression {
    fn to_encoder(self) -> png::Compression {
        match self {
            PngCompression::None => png::Compression::NoCompression,
            PngCompression::Fast => png::Compression::Fast,
            PngCompression::Balanced => png::Compression::Balanced,
            PngCompression::Small => png::Compression::High,
        }
    }
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a PNG into `writer`, recording `dpi` as the resolution the image was
/// rendered at.
///
/// A PNG that declares no resolution is read as 72 dpi by whatever opens it,
/// so an image rendered at a device pixel ratio above one comes out claiming
/// the wrong physical size. `None` writes no `pHYs` chunk at all.
pub fn write_png_to<W: Write>(
    writer: W,
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: PngCompression,
    dpi: Option<f64>,
) -> io::Result<()> {
    check_pixels(width, height, pixels)?;
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(compression.to_encoder());
    encoder.set_pixel_dims(dpi.map(pixel_dims));
    let mut writer = encoder.write_header().map_err(io_err)?;
    writer.write_image_data(pixels).map_err(io_err)?;
    Ok(())
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a PNG and return the bytes.
///
/// For hosts that need the encoded image in memory — an HTTP response body, a
/// base64 data URL, a clipboard payload — rather than on disk. See
/// [`write_png_to`] for how `compression` and `dpi` are treated.
pub fn encode_png(
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: PngCompression,
    dpi: Option<f64>,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    write_png_to(&mut out, width, height, pixels, compression, dpi)?;
    Ok(out)
}

/// Write `pixels` (RGBA8 with straight alpha, length `width * height * 4`) to
/// `path` as a PNG.
///
/// See [`write_png_to`] for how `compression` and `dpi` are treated.
pub fn write_png(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: PngCompression,
    dpi: Option<f64>,
) -> io::Result<()> {
    let file = File::create(path)?;
    write_png_to(
        BufWriter::new(file),
        width,
        height,
        pixels,
        compression,
        dpi,
    )
}

/// Dots per inch as the pixels-per-metre pair `pHYs` carries.
fn pixel_dims(dpi: f64) -> png::PixelDimensions {
    // One inch is exactly 0.0254 m, so the pixels-per-metre field runs out
    // one inch's worth of dots before a u32 does.
    let per_metre = (usable_dpi(dpi, MAX_DPI) / 0.0254)
        .round()
        .min(f64::from(u32::MAX)) as u32;
    png::PixelDimensions {
        xppu: per_metre,
        yppu: per_metre,
        unit: png::Unit::Meter,
    }
}

/// Read a PNG from `reader`.
///
/// Whatever the file holds — palette, grayscale, 16-bit, interlaced — comes
/// back as RGBA8 with straight alpha.
pub fn read_png_from<R: BufRead + Seek>(reader: R) -> io::Result<Image> {
    let mut decoder = png::Decoder::new(reader);
    // `EXPAND` unpacks palettes and sub-byte grayscale and turns a tRNS
    // chunk into real alpha; `STRIP_16` narrows deep samples to 8;
    // `ALPHA` guarantees an alpha channel is present. What survives is
    // either RGBA8 or grayscale-plus-alpha, which `expand_to_rgba8`
    // widens.
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(decode_err)?;
    let size = reader.output_buffer_size().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "PNG frame is too large to buffer on this platform",
        )
    })?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(decode_err)?;
    let channels = info.color_type.samples();
    let pixels = expand_to_rgba8(&buf, channels, info.width, info.height)?;
    from_rgba8(info.width, info.height, pixels)
}

/// Read a PNG from bytes already in memory.
pub fn decode_png(bytes: &[u8]) -> io::Result<Image> {
    read_png_from(io::Cursor::new(bytes))
}

/// Read a PNG from `path`.
pub fn read_png(path: impl AsRef<Path>) -> io::Result<Image> {
    let file = File::open(path)?;
    read_png_from(BufReader::new(file))
}

fn io_err(e: png::EncodingError) -> io::Error {
    io::Error::other(e)
}

fn decode_err(e: png::DecodingError) -> io::Error {
    match e {
        png::DecodingError::IoError(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 8-byte signature every PNG stream opens with.
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

    /// The data bytes of the first chunk with `tag`, walking the stream the
    /// way a reader does: length, tag, data, CRC.
    fn find_chunk<'a>(png: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
        let mut at = SIGNATURE.len();
        while at + 8 <= png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let body = at + 8;
            if &png[at + 4..body] == tag {
                return png.get(body..body + len);
            }
            at = body + len + 4;
        }
        None
    }

    fn checkerboard(width: u32, height: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for y in 0..height {
            for x in 0..width {
                let on = (x + y) % 2 == 0;
                px.extend_from_slice(&[if on { 255 } else { 0 }, 64, 128, 200]);
            }
        }
        px
    }

    #[test]
    fn encode_png_starts_with_the_png_signature() {
        let bytes =
            encode_png(4, 3, &checkerboard(4, 3), PngCompression::Balanced, None).expect("encode");
        assert!(
            bytes.len() > SIGNATURE.len(),
            "encoded stream is only {} bytes",
            bytes.len()
        );
        assert_eq!(&bytes[..8], &SIGNATURE);
    }

    #[test]
    fn encode_png_and_write_png_agree_byte_for_byte() {
        let pixels = checkerboard(5, 4);
        let in_memory = encode_png(5, 4, &pixels, PngCompression::Balanced, None).expect("encode");

        let dir = std::env::temp_dir().join("hephaestus_png_roundtrip");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.png");
        write_png(&path, 5, 4, &pixels, PngCompression::Balanced, None).expect("write");
        let on_disk = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(in_memory, on_disk);
    }

    #[test]
    fn pixels_including_alpha_round_trip_exactly() {
        let pixels = checkerboard(16, 9);
        let bytes = encode_png(16, 9, &pixels, PngCompression::Balanced, None).expect("encode");
        let image = decode_png(&bytes).expect("decode");

        assert_eq!((image.width, image.height), (16, 9));
        assert_eq!(image.format, crate::brush::ImageFormat::Rgba8);
        assert_eq!(image.alpha_type, crate::brush::ImageAlphaType::Alpha);
        assert_eq!(image.data.as_ref(), pixels.as_slice());
    }

    /// A PNG written as 8-bit grayscale with no alpha still reads as
    /// RGBA8: grey replicates across the colour channels and `ALPHA`
    /// synthesises an opaque channel.
    #[test]
    fn a_grayscale_png_widens_to_opaque_rgba() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[0, 64, 128, 255]).expect("data");
        }
        let image = decode_png(&bytes).expect("decode");
        assert_eq!(
            image.data.as_ref(),
            &[0, 0, 0, 255, 64, 64, 64, 255, 128, 128, 128, 255, 255, 255, 255, 255]
        );
    }

    /// A dpi lands in `pHYs` as pixels per metre, which is what an editor
    /// reads to know an image rendered at 2x is not twice its physical size.
    #[test]
    fn a_dpi_is_recorded_as_pixels_per_metre() {
        let bytes = encode_png(
            4,
            3,
            &checkerboard(4, 3),
            PngCompression::Balanced,
            Some(192.0),
        )
        .expect("encode");
        let chunk = find_chunk(&bytes, b"pHYs").expect("pHYs chunk");
        assert_eq!(chunk.len(), 9);

        // 192 / 0.0254, rounded — the same arithmetic the wasm client's
        // `withPngDpi` performs on a captured frame.
        let expected = (192.0f64 / 0.0254).round() as u32;
        assert_eq!(
            u32::from_be_bytes(chunk[0..4].try_into().unwrap()),
            expected
        );
        assert_eq!(
            u32::from_be_bytes(chunk[4..8].try_into().unwrap()),
            expected
        );
        assert_eq!(chunk[8], 1, "unit must be metres");
    }

    /// `None` is the shape a caller with no resolution to declare asks for,
    /// and it must write nothing rather than a zero or a default.
    #[test]
    fn no_dpi_writes_no_phys_chunk() {
        let without =
            encode_png(4, 3, &checkerboard(4, 3), PngCompression::Balanced, None).expect("encode");
        assert!(find_chunk(&without, b"pHYs").is_none());
    }

    /// A dpi that is not a usable number still has to produce a valid file.
    #[test]
    fn an_unusable_dpi_falls_back_to_the_floor() {
        for dpi in [0.0, -96.0, f64::NAN, f64::INFINITY] {
            let bytes = encode_png(
                2,
                2,
                &checkerboard(2, 2),
                PngCompression::Balanced,
                Some(dpi),
            )
            .expect("encode");
            let chunk = find_chunk(&bytes, b"pHYs").expect("pHYs chunk");
            let ppu = u32::from_be_bytes(chunk[0..4].try_into().unwrap());
            assert!(ppu >= 1, "dpi {dpi} produced {ppu} pixels per metre");
            decode_png(&bytes).expect("decode");
        }
    }

    #[test]
    fn bytes_that_are_not_a_png_are_rejected() {
        let err = decode_png(b"not a png at all").expect_err("should refuse");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn wrong_length_buffer_is_rejected() {
        let short = vec![0u8; 4 * 3 * 4 - 1];
        let err = encode_png(4, 3, &short, PngCompression::Balanced, None)
            .expect_err("short buffer must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let mut sink = Vec::new();
        let err = write_png_to(&mut sink, 4, 3, &short, PngCompression::Balanced, None)
            .expect_err("short buffer must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(sink.is_empty(), "nothing should be written on a size error");
    }

    /// A buffer that compresses, so the levels have something to trade
    /// against each other.
    fn gradient_with_noise(width: u32, height: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
        let mut s = 0x2545_f491_4f6c_dd1du64;
        for y in 0..height {
            for x in 0..width {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                px.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, (s >> 59) as u8, 255]);
            }
        }
        px
    }

    /// Every level is lossless, which is the property that lets a caller pick
    /// one on encode cost alone.
    #[test]
    fn every_compression_level_round_trips_the_same_pixels() {
        let pixels = checkerboard(16, 9);
        for level in [
            PngCompression::None,
            PngCompression::Fast,
            PngCompression::Balanced,
            PngCompression::Small,
        ] {
            let bytes = encode_png(16, 9, &pixels, level, None).expect("encode");
            let image = decode_png(&bytes).expect("decode");
            assert_eq!(
                image.data.as_ref(),
                pixels.as_slice(),
                "{level:?} did not round-trip"
            );
        }
    }

    /// The four are ordered, so naming one says something about the file it
    /// produces and not only about how long it takes.
    #[test]
    fn compression_levels_order_by_size() {
        let (w, h) = (200, 150);
        let pixels = gradient_with_noise(w, h);
        let size = |level| {
            encode_png(w, h, &pixels, level, None)
                .expect("encode")
                .len()
        };

        let none = size(PngCompression::None);
        let fast = size(PngCompression::Fast);
        let balanced = size(PngCompression::Balanced);

        assert!(
            none > fast && fast > balanced,
            "expected None > Fast > Balanced, got {none} {fast} {balanced}"
        );
        // Storing rows verbatim still costs a filter byte a row on top of the
        // samples, so it comes out above the raw buffer rather than at it.
        assert!(none > pixels.len());
    }

    /// `Small` spends much longer than `Balanced` without reliably beating
    /// it, so the two are pinned as near-equals rather than ordered: an
    /// assertion that `Small` wins holds on some content and fails on plot
    /// output, where the pair land within a percent of each other.
    #[test]
    fn the_smallest_level_barely_beats_balanced() {
        let (w, h) = (200, 150);
        let pixels = gradient_with_noise(w, h);
        let size = |level| {
            encode_png(w, h, &pixels, level, None)
                .expect("encode")
                .len() as f64
        };

        let balanced = size(PngCompression::Balanced);
        let small = size(PngCompression::Small);
        assert!(
            (small - balanced).abs() / balanced < 0.10,
            "Small and Balanced should land close together, got {small} vs {balanced}"
        );
    }

    /// A caller with no opinion gets the level that favours file size, since
    /// writing a figure is the common case and encode cost only matters on a
    /// frame deadline.
    #[test]
    fn the_default_level_is_balanced() {
        assert_eq!(PngCompression::default(), PngCompression::Balanced);
        let pixels = checkerboard(8, 8);
        assert_eq!(
            encode_png(8, 8, &pixels, PngCompression::default(), None).expect("encode"),
            encode_png(8, 8, &pixels, PngCompression::Balanced, None).expect("encode")
        );
    }
}
