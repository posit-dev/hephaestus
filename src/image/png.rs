//! PNG reader and writer.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Seek, Write};
use std::path::Path;

use super::{check_pixels, expand_to_rgba8, from_rgba8};
use crate::brush::Image;

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a PNG into `writer`.
pub fn write_png_to<W: Write>(writer: W, width: u32, height: u32, pixels: &[u8]) -> io::Result<()> {
    check_pixels(width, height, pixels)?;
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(io_err)?;
    writer.write_image_data(pixels).map_err(io_err)?;
    Ok(())
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a PNG and return the bytes.
///
/// For hosts that need the encoded image in memory — an HTTP response body, a
/// base64 data URL, a clipboard payload — rather than on disk.
pub fn encode_png(width: u32, height: u32, pixels: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    write_png_to(&mut out, width, height, pixels)?;
    Ok(out)
}

/// Write `pixels` (RGBA8 with straight alpha, length `width * height * 4`) to
/// `path` as a PNG.
pub fn write_png(path: impl AsRef<Path>, width: u32, height: u32, pixels: &[u8]) -> io::Result<()> {
    let file = File::create(path)?;
    write_png_to(BufWriter::new(file), width, height, pixels)
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
        let bytes = encode_png(4, 3, &checkerboard(4, 3)).expect("encode");
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
        let in_memory = encode_png(5, 4, &pixels).expect("encode");

        let dir = std::env::temp_dir().join("hephaestus_png_roundtrip");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.png");
        write_png(&path, 5, 4, &pixels).expect("write");
        let on_disk = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(in_memory, on_disk);
    }

    #[test]
    fn pixels_including_alpha_round_trip_exactly() {
        let pixels = checkerboard(16, 9);
        let bytes = encode_png(16, 9, &pixels).expect("encode");
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

    #[test]
    fn bytes_that_are_not_a_png_are_rejected() {
        let err = decode_png(b"not a png at all").expect_err("should refuse");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn wrong_length_buffer_is_rejected() {
        let short = vec![0u8; 4 * 3 * 4 - 1];
        let err = encode_png(4, 3, &short).expect_err("short buffer must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let mut sink = Vec::new();
        let err = write_png_to(&mut sink, 4, 3, &short).expect_err("short buffer must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(sink.is_empty(), "nothing should be written on a size error");
    }
}
