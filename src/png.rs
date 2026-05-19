//! Thin PNG encoder helper. Not part of the `Renderer` trait — kept separate
//! so backends only need to produce RGBA8 buffers.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

/// Write `pixels` (RGBA8, premultiplied, length `width * height * 4`) to `path`
/// as a PNG.
pub fn write_png(path: impl AsRef<Path>, width: u32, height: u32, pixels: &[u8]) -> io::Result<()> {
    let expected = (width as usize) * (height as usize) * 4;
    if pixels.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "pixel buffer is {} bytes; expected {} for {}x{}",
                pixels.len(),
                expected,
                width,
                height
            ),
        ));
    }
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(io_err)?;
    writer.write_image_data(pixels).map_err(io_err)?;
    Ok(())
}

fn io_err(e: png::EncodingError) -> io::Error {
    io::Error::other(e)
}
