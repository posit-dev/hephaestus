//! JPEG reader and writer.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Seek, Write};
use std::path::Path;

use jpeg_decoder::PixelFormat;
use jpeg_encoder::{ColorType, Encoder, EncodingError, PixelDensity};

use super::{check_dimension_limit, check_pixels, expand_to_rgba8, from_rgba8, usable_dpi};
use crate::brush::Image;
use crate::color::Color;

/// The largest width or height a JPEG frame header can express.
const MAX_DIMENSION: u32 = u16::MAX as u32;

/// The largest dpi the JFIF density fields can express.
const MAX_DPI: f64 = u16::MAX as f64;

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a JPEG into `writer`, recording `dpi` as the resolution the image was
/// rendered at.
///
/// JPEG carries no alpha channel, so the buffer is composited onto
/// `background` — whose own alpha is ignored — before encoding. A fully
/// opaque buffer passes through unchanged whatever `background` is.
/// `quality` runs from 1 to 100 and is clamped into that range.
///
/// The resolution lands in the JFIF header's density fields, so an image
/// rendered at a device pixel ratio above one does not claim to be that many
/// times its physical size. `None` leaves the header declaring a pixel
/// aspect ratio and no resolution at all.
pub fn write_jpeg_to<W: Write>(
    writer: W,
    width: u32,
    height: u32,
    pixels: &[u8],
    quality: u8,
    background: Color,
    dpi: Option<f64>,
) -> io::Result<()> {
    check_pixels(width, height, pixels)?;
    check_dimension_limit("JPEG", MAX_DIMENSION, width, height)?;

    let rgb = flatten_onto(pixels, background);
    let mut encoder = Encoder::new(writer, quality.clamp(1, 100));
    if let Some(dpi) = dpi {
        encoder.set_density(PixelDensity::dpi(density(dpi)));
    }
    encoder
        .encode(&rgb, width as u16, height as u16, ColorType::Rgb)
        .map_err(io_err)
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a JPEG and return the bytes.
///
/// For hosts that need the encoded image in memory — an HTTP response body, a
/// base64 data URL, a clipboard payload — rather than on disk. See
/// [`write_jpeg_to`] for how `quality`, `background` and `dpi` are treated.
pub fn encode_jpeg(
    width: u32,
    height: u32,
    pixels: &[u8],
    quality: u8,
    background: Color,
    dpi: Option<f64>,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    write_jpeg_to(&mut out, width, height, pixels, quality, background, dpi)?;
    Ok(out)
}

/// Write `pixels` (RGBA8 with straight alpha, length `width * height * 4`) to
/// `path` as a JPEG.
///
/// See [`write_jpeg_to`] for how `quality`, `background` and `dpi` are
/// treated.
pub fn write_jpeg(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    pixels: &[u8],
    quality: u8,
    background: Color,
    dpi: Option<f64>,
) -> io::Result<()> {
    let file = File::create(path)?;
    write_jpeg_to(
        BufWriter::new(file),
        width,
        height,
        pixels,
        quality,
        background,
        dpi,
    )
}

/// Dots per inch as the whole number the JFIF density fields carry.
fn density(dpi: f64) -> u16 {
    usable_dpi(dpi, MAX_DPI).round() as u16
}

/// Composite straight-alpha RGBA8 onto an opaque `background`, yielding
/// tightly packed RGB8.
///
/// Blending happens in the encoded sRGB byte space, which is the space the
/// renderer itself composites in.
fn flatten_onto(pixels: &[u8], background: Color) -> Vec<u8> {
    let bg = background.to_rgba8();
    let mut out = Vec::with_capacity(pixels.len() / 4 * 3);
    for px in pixels.chunks_exact(4) {
        let alpha = u32::from(px[3]);
        let over = |src: u8, dst: u8| -> u8 {
            let weighted = u32::from(src) * alpha + u32::from(dst) * (255 - alpha);
            ((weighted + 127) / 255) as u8
        };
        out.extend_from_slice(&[over(px[0], bg.r), over(px[1], bg.g), over(px[2], bg.b)]);
    }
    out
}

/// Read a JPEG from `reader`.
///
/// The format carries no alpha channel, so the result is fully opaque.
/// Grayscale expands to grey RGB; CMYK is refused rather than converted
/// without a colour profile to convert through.
pub fn read_jpeg_from<R: BufRead + Seek>(reader: R) -> io::Result<Image> {
    let mut decoder = jpeg_decoder::Decoder::new(reader);
    let samples = decoder.decode().map_err(decode_err)?;
    let info = decoder.info().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "JPEG decoded without reporting its dimensions",
        )
    })?;
    let channels = match info.pixel_format {
        PixelFormat::L8 => 1,
        PixelFormat::RGB24 => 3,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot read a JPEG in pixel format {other:?} as RGBA8"),
            ))
        }
    };
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    let pixels = expand_to_rgba8(&samples, channels, width, height)?;
    from_rgba8(width, height, pixels)
}

/// Read a JPEG from bytes already in memory.
pub fn decode_jpeg(bytes: &[u8]) -> io::Result<Image> {
    read_jpeg_from(io::Cursor::new(bytes))
}

/// Read a JPEG from `path`.
pub fn read_jpeg(path: impl AsRef<Path>) -> io::Result<Image> {
    let file = File::open(path)?;
    read_jpeg_from(BufReader::new(file))
}

fn io_err(e: EncodingError) -> io::Error {
    io::Error::other(e)
}

fn decode_err(e: jpeg_decoder::Error) -> io::Error {
    match e {
        jpeg_decoder::Error::Io(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb8;

    /// The bytes every JFIF stream opens with: start-of-image plus the first
    /// marker's prefix.
    const SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];
    /// End-of-image, the last two bytes of a complete stream.
    const EOI: [u8; 2] = [0xFF, 0xD9];

    fn checkerboard(width: u32, height: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for y in 0..height {
            for x in 0..width {
                let on = (x + y) % 2 == 0;
                px.extend_from_slice(&[if on { 255 } else { 0 }, 64, 128, 255]);
            }
        }
        px
    }

    #[test]
    fn encode_jpeg_brackets_the_stream_with_soi_and_eoi() {
        let bytes = encode_jpeg(8, 8, &checkerboard(8, 8), 90, Color::WHITE, None).expect("encode");
        assert_eq!(&bytes[..3], &SOI);
        assert_eq!(&bytes[bytes.len() - 2..], &EOI);
    }

    #[test]
    fn encode_jpeg_and_write_jpeg_agree_byte_for_byte() {
        let pixels = checkerboard(8, 5);
        let in_memory = encode_jpeg(8, 5, &pixels, 75, Color::WHITE, None).expect("encode");

        let dir = std::env::temp_dir().join("hephaestus_jpeg_roundtrip");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.jpg");
        write_jpeg(&path, 8, 5, &pixels, 75, Color::WHITE, None).expect("write");
        let on_disk = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(in_memory, on_disk);
    }

    /// JPEG is lossy, so the pixels cannot round-trip exactly. What must
    /// survive is the shape: the right dimensions, and an opaque alpha
    /// channel the format itself never stored.
    #[test]
    fn a_written_jpeg_reads_back_at_the_same_size_and_opaque() {
        let pixels = checkerboard(16, 9);
        let bytes = encode_jpeg(16, 9, &pixels, 90, Color::WHITE, None).expect("encode");
        let image = decode_jpeg(&bytes).expect("decode");

        assert_eq!((image.width, image.height), (16, 9));
        assert_eq!(image.format, crate::brush::ImageFormat::Rgba8);
        assert_eq!(image.alpha_type, crate::brush::ImageAlphaType::Alpha);
        assert_eq!(image.data.as_ref().len(), 16 * 9 * 4);
        assert!(
            image.data.as_ref().chunks_exact(4).all(|px| px[3] == 255),
            "JPEG carries no alpha, so every pixel must read back opaque"
        );
    }

    /// The JFIF APP0 segment's units byte and the two density fields, which
    /// is where a JPEG records the resolution it was rendered at.
    fn jfif_density(jpeg: &[u8]) -> (u8, u16, u16) {
        // SOI, then APP0: marker, length, "JFIF\0", two version bytes, the
        // unit, and the two 16-bit densities, all big-endian.
        assert_eq!(&jpeg[2..4], &[0xFF, 0xE0], "first segment must be APP0");
        assert_eq!(&jpeg[6..11], b"JFIF\0");
        (
            jpeg[13],
            u16::from_be_bytes(jpeg[14..16].try_into().unwrap()),
            u16::from_be_bytes(jpeg[16..18].try_into().unwrap()),
        )
    }

    /// A dpi lands in the JFIF header as a density in inches, which is what
    /// keeps an image rendered at 2x from claiming twice its physical size.
    #[test]
    fn a_dpi_is_recorded_as_a_density_in_inches() {
        let bytes =
            encode_jpeg(8, 8, &checkerboard(8, 8), 90, Color::WHITE, Some(192.0)).expect("encode");
        assert_eq!(jfif_density(&bytes), (1, 192, 192));
    }

    /// `None` is the shape a caller with no resolution to declare asks for:
    /// the header falls back to a bare pixel aspect ratio.
    #[test]
    fn no_dpi_records_a_pixel_aspect_ratio() {
        let without =
            encode_jpeg(8, 8, &checkerboard(8, 8), 90, Color::WHITE, None).expect("encode");
        assert_eq!(jfif_density(&without), (0, 1, 1));
    }

    /// A dpi that is not a usable number still has to produce a valid file.
    #[test]
    fn an_unusable_dpi_falls_back_to_the_floor() {
        let pixels = checkerboard(8, 8);
        for dpi in [0.0, -96.0, f64::NAN, f64::INFINITY, 1e9] {
            let bytes = encode_jpeg(8, 8, &pixels, 90, Color::WHITE, Some(dpi)).expect("encode");
            let (unit, x, y) = jfif_density(&bytes);
            assert_eq!(unit, 1);
            assert!(x >= 1 && y >= 1, "dpi {dpi} produced {x}x{y}");
            decode_jpeg(&bytes).expect("decode");
        }
    }

    #[test]
    fn bytes_that_are_not_a_jpeg_are_rejected() {
        let err = decode_jpeg(b"not a jpeg at all").expect_err("should refuse");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn wrong_length_buffer_is_rejected() {
        let short = vec![0u8; 8 * 8 * 4 - 1];
        let err = encode_jpeg(8, 8, &short, 90, Color::WHITE, None).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let mut sink = Vec::new();
        let err =
            write_jpeg_to(&mut sink, 8, 8, &short, 90, Color::WHITE, None).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(sink.is_empty(), "nothing should be written on a size error");
    }

    #[test]
    fn dimensions_beyond_the_frame_header_are_rejected() {
        let width = MAX_DIMENSION + 1;
        let pixels = vec![0u8; (width as usize) * 4];
        let err = encode_jpeg(width, 1, &pixels, 90, Color::WHITE, None).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("JPEG"),
            "message should name the format: {err}"
        );
    }

    #[test]
    fn quality_is_clamped_to_the_encoders_range() {
        let pixels = checkerboard(8, 8);
        let at_zero = encode_jpeg(8, 8, &pixels, 0, Color::WHITE, None).expect("encode");
        let at_one = encode_jpeg(8, 8, &pixels, 1, Color::WHITE, None).expect("encode");
        assert_eq!(at_zero, at_one);

        let over = encode_jpeg(8, 8, &pixels, u8::MAX, Color::WHITE, None).expect("encode");
        let at_hundred = encode_jpeg(8, 8, &pixels, 100, Color::WHITE, None).expect("encode");
        assert_eq!(over, at_hundred);
    }

    #[test]
    fn opaque_pixels_pass_through_the_composite_unchanged() {
        let pixels = [10, 20, 30, 255, 200, 210, 220, 255];
        assert_eq!(
            flatten_onto(&pixels, rgb8(255, 0, 0)),
            vec![10, 20, 30, 200, 210, 220]
        );
    }

    #[test]
    fn transparent_pixels_become_the_background() {
        let pixels = [10, 20, 30, 0];
        assert_eq!(flatten_onto(&pixels, rgb8(7, 8, 9)), vec![7, 8, 9]);
    }

    #[test]
    fn half_transparent_pixels_land_midway() {
        // Alpha 128/255 is a hair over half, so the source is weighted just
        // above the background.
        let pixels = [0, 0, 0, 128];
        assert_eq!(flatten_onto(&pixels, rgb8(200, 100, 50)), vec![100, 50, 25]);
    }
}
