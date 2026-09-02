//! TIFF reader and writer.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::Path;

use tiff::decoder::DecodingResult;
use tiff::encoder::{colortype, Compression, DeflateLevel, Rational, TiffEncoder};
use tiff::tags::{ExtraSamples, ResolutionUnit, Tag};
use tiff::ColorType;
use tiff::TiffError;

use super::{check_pixels, dpi_rational, expand_to_rgba8, from_rgba8};
use crate::brush::Image;

/// How a TIFF writer compresses image data.
///
/// All four are lossless; they trade encode cost against file size, and
/// against how old a reader can be and still open the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TiffCompression {
    /// Store rows verbatim. Largest files, and readable by anything.
    None,
    /// Deflate. The smallest of the four on plot output, and the default.
    #[default]
    Deflate,
    /// LZW. Larger than deflate here, and the most widely read compressed
    /// TIFF.
    Lzw,
    /// PackBits run-length encoding. Cheap, and effective on the flat fills a
    /// plot is mostly made of.
    Packbits,
}

impl TiffCompression {
    fn to_encoder(self) -> Compression {
        match self {
            TiffCompression::None => Compression::Uncompressed,
            TiffCompression::Deflate => Compression::Deflate(DeflateLevel::Balanced),
            TiffCompression::Lzw => Compression::Lzw,
            TiffCompression::Packbits => Compression::Packbits,
        }
    }
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a TIFF into `writer`, recording `dpi` as the resolution the image was
/// rendered at.
///
/// The resolution lands in the `XResolution` / `YResolution` tags with
/// `ResolutionUnit` set to inches. `None` leaves the encoder's own default
/// in place — one pixel per unit, with no unit.
///
/// The writer must seek: a TIFF's tag directory records offsets into the
/// image data that follows it, so the header is revisited once the data is
/// written.
pub fn write_tiff_to<W: Write + Seek>(
    writer: W,
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: TiffCompression,
    dpi: Option<f64>,
) -> io::Result<()> {
    check_pixels(width, height, pixels)?;

    let mut encoder = TiffEncoder::new(writer)
        .map_err(io_err)?
        .with_compression(compression.to_encoder());
    let mut image = encoder
        .new_image::<colortype::RGBA8>(width, height)
        .map_err(io_err)?;
    // The RGBA8 color type declares four samples but not what the fourth
    // means; readers need ExtraSamples to treat the alpha as straight rather
    // than premultiplied.
    image
        .encoder()
        .write_tag(
            Tag::ExtraSamples,
            &[ExtraSamples::UnassociatedAlpha.to_u16()][..],
        )
        .map_err(io_err)?;
    if let Some(dpi) = dpi {
        let (n, d) = dpi_rational(dpi);
        // Each tag replaces the placeholder the encoder wrote when the image
        // was opened, so all three have to be set together: a resolution in
        // inches with the unit left unset reads as a bare aspect ratio.
        image
            .encoder()
            .write_tag(Tag::ResolutionUnit, ResolutionUnit::Inch.to_u16())
            .map_err(io_err)?;
        image
            .encoder()
            .write_tag(Tag::XResolution, Rational { n, d })
            .map_err(io_err)?;
        image
            .encoder()
            .write_tag(Tag::YResolution, Rational { n, d })
            .map_err(io_err)?;
    }
    image.write_data(pixels).map_err(io_err)
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a TIFF and return the bytes.
///
/// For hosts that need the encoded image in memory — an HTTP response body, a
/// base64 data URL, a clipboard payload — rather than on disk. See
/// [`write_tiff_to`] for how `dpi` is treated.
pub fn encode_tiff(
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: TiffCompression,
    dpi: Option<f64>,
) -> io::Result<Vec<u8>> {
    let mut out = io::Cursor::new(Vec::new());
    write_tiff_to(&mut out, width, height, pixels, compression, dpi)?;
    Ok(out.into_inner())
}

/// Write `pixels` (RGBA8 with straight alpha, length `width * height * 4`) to
/// `path` as a TIFF.
///
/// See [`write_tiff_to`] for how `dpi` is treated.
pub fn write_tiff(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: TiffCompression,
    dpi: Option<f64>,
) -> io::Result<()> {
    let file = File::create(path)?;
    write_tiff_to(
        BufWriter::new(file),
        width,
        height,
        pixels,
        compression,
        dpi,
    )
}

/// Read a TIFF from `reader`.
///
/// The first image in the file, normalised to RGBA8 with straight alpha.
/// Grayscale and RGB gain an opaque alpha channel; anything whose samples
/// are not 8-bit, or whose colour model needs a conversion this module does
/// not do (CMYK, YCbCr, Lab), is refused rather than guessed at.
pub fn read_tiff_from<R: Read + Seek>(reader: R) -> io::Result<Image> {
    let mut decoder = tiff::decoder::Decoder::new(reader).map_err(decode_err)?;
    let (width, height) = decoder.dimensions().map_err(decode_err)?;
    let color = decoder.colortype().map_err(decode_err)?;
    let channels = match color {
        ColorType::Gray(8) => 1,
        ColorType::GrayA(8) => 2,
        ColorType::RGB(8) => 3,
        ColorType::RGBA(8) => 4,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot read a TIFF of color type {other:?} as RGBA8"),
            ))
        }
    };
    let samples = match decoder.read_image().map_err(decode_err)? {
        DecodingResult::U8(v) => v,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "TIFF decoded to {} samples, not the 8-bit ones its color type promised",
                    sample_kind(&other)
                ),
            ))
        }
    };
    let pixels = expand_to_rgba8(&samples, channels, width, height)?;
    from_rgba8(width, height, pixels)
}

/// Read a TIFF from bytes already in memory.
pub fn decode_tiff(bytes: &[u8]) -> io::Result<Image> {
    read_tiff_from(io::Cursor::new(bytes))
}

/// Read a TIFF from `path`.
pub fn read_tiff(path: impl AsRef<Path>) -> io::Result<Image> {
    let file = File::open(path)?;
    read_tiff_from(BufReader::new(file))
}

fn io_err(e: TiffError) -> io::Error {
    io::Error::other(e)
}

fn decode_err(e: TiffError) -> io::Error {
    match e {
        TiffError::IoError(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

/// Name a decoded sample type, for the error a non-8-bit TIFF produces.
fn sample_kind(r: &DecodingResult) -> &'static str {
    match r {
        DecodingResult::U8(_) => "u8",
        DecodingResult::U16(_) => "u16",
        DecodingResult::U32(_) => "u32",
        DecodingResult::U64(_) => "u64",
        DecodingResult::F16(_) => "f16",
        DecodingResult::F32(_) => "f32",
        DecodingResult::F64(_) => "f64",
        DecodingResult::I8(_) => "i8",
        DecodingResult::I16(_) => "i16",
        DecodingResult::I32(_) => "i32",
        DecodingResult::I64(_) => "i64",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::decoder::ifd::Value;
    use tiff::decoder::Decoder;

    const ALL: [TiffCompression; 4] = [
        TiffCompression::None,
        TiffCompression::Deflate,
        TiffCompression::Lzw,
        TiffCompression::Packbits,
    ];

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for y in 0..height {
            for x in 0..width {
                px.extend_from_slice(&[x as u8, y as u8, 128, (x * 8) as u8]);
            }
        }
        px
    }

    #[test]
    fn encode_tiff_opens_with_a_byte_order_marker() {
        let bytes =
            encode_tiff(4, 3, &gradient(4, 3), TiffCompression::Deflate, None).expect("encode");
        assert!(
            bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a"),
            "not a TIFF header: {:?}",
            &bytes[..4]
        );
    }

    #[test]
    fn every_compression_round_trips_the_pixels_exactly() {
        let pixels = gradient(16, 9);
        for compression in ALL {
            let bytes = encode_tiff(16, 9, &pixels, compression, None).expect("encode");
            let image = decode_tiff(&bytes).expect("decode");

            assert_eq!((image.width, image.height), (16, 9));
            assert_eq!(image.format, crate::brush::ImageFormat::Rgba8);
            assert_eq!(image.alpha_type, crate::brush::ImageAlphaType::Alpha);
            assert_eq!(
                image.data.as_ref(),
                pixels.as_slice(),
                "{compression:?} altered the pixels"
            );
        }
    }

    /// A TIFF whose file holds fewer than four samples per pixel still
    /// reads as RGBA8 — grey replicates across the colour channels and the
    /// missing alpha becomes opaque.
    #[test]
    fn a_grayscale_tiff_widens_to_opaque_rgba() {
        let mut out = Vec::new();
        {
            let mut encoder = TiffEncoder::new(io::Cursor::new(&mut out)).expect("encoder");
            encoder
                .write_image::<colortype::Gray8>(2, 2, &[0u8, 64, 128, 255])
                .expect("write");
        }
        let image = decode_tiff(&out).expect("decode");
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(
            image.data.as_ref(),
            &[0, 0, 0, 255, 64, 64, 64, 255, 128, 128, 128, 255, 255, 255, 255, 255]
        );
    }

    #[test]
    fn alpha_is_declared_unassociated() {
        let bytes =
            encode_tiff(4, 3, &gradient(4, 3), TiffCompression::Deflate, None).expect("encode");
        let mut decoder = Decoder::new(io::Cursor::new(bytes)).expect("decode");
        assert_eq!(
            decoder
                .get_tag_u32_vec(Tag::ExtraSamples)
                .expect("ExtraSamples"),
            vec![u32::from(ExtraSamples::UnassociatedAlpha.to_u16())]
        );
    }

    /// A dpi lands in the resolution tags as an exact whole number of dots
    /// per inch, which is what an editor reads to place the image on a page.
    #[test]
    fn a_dpi_is_recorded_in_the_resolution_tags() {
        let bytes = encode_tiff(4, 3, &gradient(4, 3), TiffCompression::Deflate, Some(192.0))
            .expect("encode");
        let mut decoder = Decoder::new(io::Cursor::new(bytes)).expect("decode");
        assert_eq!(
            decoder.get_tag(Tag::ResolutionUnit).expect("unit"),
            Value::Short(ResolutionUnit::Inch.to_u16())
        );
        assert_eq!(
            decoder.get_tag(Tag::XResolution).expect("x"),
            Value::Rational(192, 1)
        );
        assert_eq!(
            decoder.get_tag(Tag::YResolution).expect("y"),
            Value::Rational(192, 1)
        );
    }

    /// A fractional dpi — a display scale that is not a whole multiple —
    /// survives as a rational rather than rounding to the nearest inch.
    #[test]
    fn a_fractional_dpi_keeps_its_fraction() {
        let bytes = encode_tiff(4, 3, &gradient(4, 3), TiffCompression::Deflate, Some(144.5))
            .expect("encode");
        let mut decoder = Decoder::new(io::Cursor::new(bytes)).expect("decode");
        assert_eq!(
            decoder.get_tag(Tag::XResolution).expect("x"),
            Value::Rational(1_445_000, 10_000)
        );
    }

    /// `None` leaves the encoder's own placeholder in place: one pixel per
    /// unit, and no unit to measure it in.
    #[test]
    fn no_dpi_leaves_the_resolution_undeclared() {
        let without =
            encode_tiff(4, 3, &gradient(4, 3), TiffCompression::Deflate, None).expect("encode");
        let mut decoder = Decoder::new(io::Cursor::new(without)).expect("decode");
        assert_eq!(
            decoder.get_tag(Tag::ResolutionUnit).expect("unit"),
            Value::Short(ResolutionUnit::None.to_u16())
        );
    }

    /// A dpi that is not a usable number still has to produce a valid file.
    #[test]
    fn an_unusable_dpi_falls_back_to_the_floor() {
        let pixels = gradient(4, 3);
        for dpi in [0.0, -96.0, f64::NAN, f64::INFINITY, 1e9] {
            let bytes =
                encode_tiff(4, 3, &pixels, TiffCompression::Deflate, Some(dpi)).expect("encode");
            let mut decoder = Decoder::new(io::Cursor::new(bytes.clone())).expect("decode");
            match decoder.get_tag(Tag::XResolution).expect("x") {
                Value::Rational(n, d) => assert!(n >= 1 && d >= 1, "dpi {dpi} produced {n}/{d}"),
                other => panic!("XResolution decoded as {other:?}"),
            }
            decode_tiff(&bytes).expect("decode");
        }
    }

    #[test]
    fn encode_tiff_and_write_tiff_agree_byte_for_byte() {
        let pixels = gradient(5, 4);
        let in_memory = encode_tiff(5, 4, &pixels, TiffCompression::Lzw, None).expect("encode");

        let dir = std::env::temp_dir().join("hephaestus_tiff_roundtrip");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.tiff");
        write_tiff(&path, 5, 4, &pixels, TiffCompression::Lzw, None).expect("write");
        let on_disk = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(in_memory, on_disk);
    }

    #[test]
    fn wrong_length_buffer_is_rejected() {
        let short = vec![0u8; 4 * 3 * 4 - 1];
        let err = encode_tiff(4, 3, &short, TiffCompression::Deflate, None).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let mut sink = io::Cursor::new(Vec::new());
        let err = write_tiff_to(&mut sink, 4, 3, &short, TiffCompression::Deflate, None)
            .expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            sink.into_inner().is_empty(),
            "nothing should be written on a size error"
        );
    }
}
