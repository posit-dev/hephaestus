//! PNG writer.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use super::check_pixels;

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

fn io_err(e: png::EncodingError) -> io::Error {
    io::Error::other(e)
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
