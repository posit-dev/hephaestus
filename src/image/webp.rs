//! WebP reader and writer.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Seek, Write};
use std::path::Path;

use image_webp::{ColorType, DecodingError, EncodingError, WebPDecoder, WebPEncoder};

use super::{check_dimension_limit, check_pixels, dpi_rational, expand_to_rgba8, from_rgba8};
use crate::brush::Image;

/// The largest width or height a lossless WebP bitstream can express.
const MAX_DIMENSION: u32 = 16384;

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a WebP into `writer`, recording `dpi` as the resolution the image was
/// rendered at.
///
/// Lossless, so the pixels — alpha included — survive exactly. There is no
/// quality parameter: the encoder writes the VP8L lossless bitstream, which
/// on the flat fills and hard edges of a plot lands smaller than the same
/// image as a PNG.
///
/// The bitstream itself has no resolution field, so the figure travels as an
/// EXIF block holding the resolution tags — which is where every WebP records
/// one. Carrying it promotes the file to the extended container, so `None`
/// writes the plain one.
pub fn write_webp_to<W: Write>(
    writer: W,
    width: u32,
    height: u32,
    pixels: &[u8],
    dpi: Option<f64>,
) -> io::Result<()> {
    check_pixels(width, height, pixels)?;
    check_dimension_limit("WebP", MAX_DIMENSION, width, height)?;

    let mut encoder = WebPEncoder::new(writer);
    if let Some(dpi) = dpi {
        encoder.set_exif_metadata(exif_resolution(dpi));
    }
    encoder
        .encode(pixels, width, height, ColorType::Rgba8)
        .map_err(io_err)
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a WebP and return the bytes.
///
/// For hosts that need the encoded image in memory — an HTTP response body, a
/// base64 data URL, a clipboard payload — rather than on disk. See
/// [`write_webp_to`] for how `dpi` is treated.
pub fn encode_webp(
    width: u32,
    height: u32,
    pixels: &[u8],
    dpi: Option<f64>,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    write_webp_to(&mut out, width, height, pixels, dpi)?;
    Ok(out)
}

/// Write `pixels` (RGBA8 with straight alpha, length `width * height * 4`) to
/// `path` as a WebP.
///
/// See [`write_webp_to`] for how `dpi` is treated.
pub fn write_webp(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    pixels: &[u8],
    dpi: Option<f64>,
) -> io::Result<()> {
    let file = File::create(path)?;
    write_webp_to(BufWriter::new(file), width, height, pixels, dpi)
}

/// A little-endian EXIF block declaring `dpi` through the resolution tags.
///
/// One IFD with the three tags a resolution needs, in the ascending order an
/// IFD requires: `XResolution`, `YResolution`, then `ResolutionUnit` set to
/// inches. The two rationals do not fit a directory entry's four value bytes,
/// so the entries hold offsets to the pair written after the directory.
fn exif_resolution(dpi: f64) -> Vec<u8> {
    /// Where the first rational lands: the 8-byte header, the entry count,
    /// three 12-byte entries, and the offset of the next directory.
    const DATA: u32 = 8 + 2 + 3 * 12 + 4;
    const RATIONAL: u16 = 5;
    const SHORT: u16 = 3;
    const INCHES: u32 = 2;

    let (n, d) = dpi_rational(dpi);
    let mut out = Vec::with_capacity(DATA as usize + 16);
    out.extend_from_slice(b"II\x2a\x00");
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());

    let mut entry = |tag: u16, kind: u16, value: u32| {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    };
    entry(282, RATIONAL, DATA);
    entry(283, RATIONAL, DATA + 8);
    // A SHORT sits in the low half of the value field, the high half unused.
    entry(296, SHORT, INCHES);

    // No second directory follows.
    out.extend_from_slice(&0u32.to_le_bytes());
    for _ in 0..2 {
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&d.to_le_bytes());
    }
    out
}

/// Read a WebP from `reader`.
///
/// The first frame, normalised to RGBA8 with straight alpha — a file without
/// an alpha channel comes back opaque. An animation reads as its first frame.
pub fn read_webp_from<R: BufRead + Seek>(reader: R) -> io::Result<Image> {
    let mut decoder = WebPDecoder::new(reader).map_err(decode_err)?;
    let (width, height) = decoder.dimensions();
    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let size = decoder.output_buffer_size().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "WebP frame is too large to buffer on this platform",
        )
    })?;
    let mut buf = vec![0u8; size];
    decoder.read_image(&mut buf).map_err(decode_err)?;
    let pixels = expand_to_rgba8(&buf, channels, width, height)?;
    from_rgba8(width, height, pixels)
}

/// Read a WebP from bytes already in memory.
pub fn decode_webp(bytes: &[u8]) -> io::Result<Image> {
    read_webp_from(io::Cursor::new(bytes))
}

/// Read a WebP from `path`.
pub fn read_webp(path: impl AsRef<Path>) -> io::Result<Image> {
    let file = File::open(path)?;
    read_webp_from(BufReader::new(file))
}

fn decode_err(e: DecodingError) -> io::Error {
    match e {
        DecodingError::IoError(err) => err,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

fn io_err(e: EncodingError) -> io::Error {
    match e {
        EncodingError::IoError(err) => err,
        other => io::Error::other(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn encode_webp_writes_a_riff_webp_container() {
        let bytes = encode_webp(4, 3, &gradient(4, 3), None).expect("encode");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }

    #[test]
    fn pixels_including_alpha_round_trip_exactly() {
        let pixels = gradient(16, 9);
        let bytes = encode_webp(16, 9, &pixels, None).expect("encode");
        let image = decode_webp(&bytes).expect("decode");

        assert_eq!((image.width, image.height), (16, 9));
        assert_eq!(image.format, crate::brush::ImageFormat::Rgba8);
        assert_eq!(image.alpha_type, crate::brush::ImageAlphaType::Alpha);
        assert_eq!(image.data.as_ref(), pixels.as_slice());
    }

    /// A file with no alpha channel reads back opaque rather than
    /// transparent — the widening fills the missing samples with 255.
    #[test]
    fn an_opaque_webp_reads_back_opaque() {
        let mut pixels = gradient(8, 8);
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let bytes = encode_webp(8, 8, &pixels, None).expect("encode");
        let image = decode_webp(&bytes).expect("decode");
        assert!(image.data.as_ref().chunks_exact(4).all(|px| px[3] == 255));
    }

    /// A dpi travels in the EXIF chunk the extended container carries, since
    /// the bitstream itself has nowhere to record one.
    #[test]
    fn a_dpi_is_recorded_in_an_exif_chunk() {
        let bytes = encode_webp(4, 3, &gradient(4, 3), Some(192.0)).expect("encode");
        let mut decoder = WebPDecoder::new(io::Cursor::new(bytes)).expect("decode");
        assert_eq!(
            decoder.exif_metadata().expect("exif"),
            Some(exif_resolution(192.0))
        );
    }

    /// The block is a little-endian TIFF stream: a header pointing at one
    /// directory of three tags, with the two rationals laid out after it.
    #[test]
    fn the_exif_block_is_a_tiff_stream_of_resolution_tags() {
        let block = exif_resolution(192.0);
        assert_eq!(&block[..4], b"II\x2a\x00");
        assert_eq!(u32::from_le_bytes(block[4..8].try_into().unwrap()), 8);
        assert_eq!(u16::from_le_bytes(block[8..10].try_into().unwrap()), 3);

        let entry = |i: usize| -> (u16, u16, u32, u32) {
            let at = 10 + i * 12;
            (
                u16::from_le_bytes(block[at..at + 2].try_into().unwrap()),
                u16::from_le_bytes(block[at + 2..at + 4].try_into().unwrap()),
                u32::from_le_bytes(block[at + 4..at + 8].try_into().unwrap()),
                u32::from_le_bytes(block[at + 8..at + 12].try_into().unwrap()),
            )
        };
        // XResolution and YResolution point at the two rationals; the unit
        // is a SHORT small enough to sit in the entry itself, and 2 is
        // inches.
        assert_eq!(entry(0), (282, 5, 1, 50));
        assert_eq!(entry(1), (283, 5, 1, 58));
        assert_eq!(entry(2), (296, 3, 1, 2));
        // No second directory, then 192/1 twice — one rational per axis.
        assert_eq!(u32::from_le_bytes(block[46..50].try_into().unwrap()), 0);
        let rational = |at: usize| -> (u32, u32) {
            (
                u32::from_le_bytes(block[at..at + 4].try_into().unwrap()),
                u32::from_le_bytes(block[at + 4..at + 8].try_into().unwrap()),
            )
        };
        assert_eq!(rational(50), (192, 1));
        assert_eq!(rational(58), (192, 1));
        assert_eq!(block.len(), 66);
    }

    /// `None` keeps the plain container — no VP8X, no metadata chunks.
    #[test]
    fn no_dpi_writes_no_exif_chunk() {
        let without = encode_webp(4, 3, &gradient(4, 3), None).expect("encode");
        let mut decoder = WebPDecoder::new(io::Cursor::new(without)).expect("decode");
        assert_eq!(decoder.exif_metadata().expect("exif"), None);
    }

    /// A dpi that is not a usable number still has to produce a valid file.
    #[test]
    fn an_unusable_dpi_falls_back_to_the_floor() {
        let pixels = gradient(4, 3);
        for dpi in [0.0, -96.0, f64::NAN, f64::INFINITY, 1e9] {
            let bytes = encode_webp(4, 3, &pixels, Some(dpi)).expect("encode");
            let block = exif_resolution(dpi);
            let n = u32::from_le_bytes(block[50..54].try_into().unwrap());
            let d = u32::from_le_bytes(block[54..58].try_into().unwrap());
            assert!(n >= 1 && d >= 1, "dpi {dpi} produced {n}/{d}");
            let image = decode_webp(&bytes).expect("decode");
            assert_eq!(image.data.as_ref(), pixels.as_slice());
        }
    }

    #[test]
    fn encode_webp_and_write_webp_agree_byte_for_byte() {
        let pixels = gradient(5, 4);
        let in_memory = encode_webp(5, 4, &pixels, None).expect("encode");

        let dir = std::env::temp_dir().join("hephaestus_webp_roundtrip");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.webp");
        write_webp(&path, 5, 4, &pixels, None).expect("write");
        let on_disk = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(in_memory, on_disk);
    }

    #[test]
    fn wrong_length_buffer_is_rejected() {
        let short = vec![0u8; 4 * 3 * 4 - 1];
        let err = encode_webp(4, 3, &short, None).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let mut sink = Vec::new();
        let err = write_webp_to(&mut sink, 4, 3, &short, None).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(sink.is_empty(), "nothing should be written on a size error");
    }

    #[test]
    fn dimensions_beyond_the_bitstream_are_rejected() {
        let width = MAX_DIMENSION + 1;
        let pixels = vec![0u8; (width as usize) * 4];
        let err = encode_webp(width, 1, &pixels, None).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("WebP"),
            "message should name the format: {err}"
        );
    }
}
