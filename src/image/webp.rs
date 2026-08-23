//! WebP reader and writer.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Seek, Write};
use std::path::Path;

use image_webp::{ColorType, DecodingError, EncodingError, WebPDecoder, WebPEncoder};

use super::{check_dimension_limit, check_pixels, expand_to_rgba8, from_rgba8};
use crate::brush::Image;

/// The largest width or height a lossless WebP bitstream can express.
const MAX_DIMENSION: u32 = 16384;

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a WebP into `writer`.
///
/// Lossless, so the pixels — alpha included — survive exactly. There is no
/// quality parameter: the encoder writes the VP8L lossless bitstream, which
/// on the flat fills and hard edges of a plot lands smaller than the same
/// image as a PNG.
pub fn write_webp_to<W: Write>(
    writer: W,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> io::Result<()> {
    check_pixels(width, height, pixels)?;
    check_dimension_limit("WebP", MAX_DIMENSION, width, height)?;

    WebPEncoder::new(writer)
        .encode(pixels, width, height, ColorType::Rgba8)
        .map_err(io_err)
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a WebP and return the bytes.
///
/// For hosts that need the encoded image in memory — an HTTP response body, a
/// base64 data URL, a clipboard payload — rather than on disk.
pub fn encode_webp(width: u32, height: u32, pixels: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    write_webp_to(&mut out, width, height, pixels)?;
    Ok(out)
}

/// Write `pixels` (RGBA8 with straight alpha, length `width * height * 4`) to
/// `path` as a WebP.
pub fn write_webp(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> io::Result<()> {
    let file = File::create(path)?;
    write_webp_to(BufWriter::new(file), width, height, pixels)
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
        let bytes = encode_webp(4, 3, &gradient(4, 3)).expect("encode");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }

    #[test]
    fn pixels_including_alpha_round_trip_exactly() {
        let pixels = gradient(16, 9);
        let bytes = encode_webp(16, 9, &pixels).expect("encode");
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
        let bytes = encode_webp(8, 8, &pixels).expect("encode");
        let image = decode_webp(&bytes).expect("decode");
        assert!(image.data.as_ref().chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn encode_webp_and_write_webp_agree_byte_for_byte() {
        let pixels = gradient(5, 4);
        let in_memory = encode_webp(5, 4, &pixels).expect("encode");

        let dir = std::env::temp_dir().join("hephaestus_webp_roundtrip");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.webp");
        write_webp(&path, 5, 4, &pixels).expect("write");
        let on_disk = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(in_memory, on_disk);
    }

    #[test]
    fn wrong_length_buffer_is_rejected() {
        let short = vec![0u8; 4 * 3 * 4 - 1];
        let err = encode_webp(4, 3, &short).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let mut sink = Vec::new();
        let err = write_webp_to(&mut sink, 4, 3, &short).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(sink.is_empty(), "nothing should be written on a size error");
    }

    #[test]
    fn dimensions_beyond_the_bitstream_are_rejected() {
        let width = MAX_DIMENSION + 1;
        let pixels = vec![0u8; (width as usize) * 4];
        let err = encode_webp(width, 1, &pixels).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("WebP"),
            "message should name the format: {err}"
        );
    }
}
