//! WebP writer.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use image_webp::{ColorType, EncodingError, WebPEncoder};

use super::{check_dimension_limit, check_pixels};

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

fn io_err(e: EncodingError) -> io::Error {
    match e {
        EncodingError::IoError(err) => err,
        other => io::Error::other(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image_webp::WebPDecoder;

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

        let mut decoder = WebPDecoder::new(io::Cursor::new(bytes)).expect("decode");
        assert_eq!(decoder.dimensions(), (16, 9));
        assert!(decoder.has_alpha(), "alpha channel should survive");

        let mut got = vec![0u8; decoder.output_buffer_size().expect("buffer size")];
        decoder.read_image(&mut got).expect("read");
        assert_eq!(got, pixels);
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
