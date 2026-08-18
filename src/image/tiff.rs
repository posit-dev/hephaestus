//! TIFF writer.

use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;

use tiff::encoder::{colortype, Compression, DeflateLevel, TiffEncoder};
use tiff::tags::{ExtraSamples, Tag};
use tiff::TiffError;

use super::check_pixels;

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
/// as a TIFF into `writer`.
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
    image.write_data(pixels).map_err(io_err)
}

/// Encode `pixels` (RGBA8 with straight alpha, length `width * height * 4`)
/// as a TIFF and return the bytes.
///
/// For hosts that need the encoded image in memory — an HTTP response body, a
/// base64 data URL, a clipboard payload — rather than on disk.
pub fn encode_tiff(
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: TiffCompression,
) -> io::Result<Vec<u8>> {
    let mut out = io::Cursor::new(Vec::new());
    write_tiff_to(&mut out, width, height, pixels, compression)?;
    Ok(out.into_inner())
}

/// Write `pixels` (RGBA8 with straight alpha, length `width * height * 4`) to
/// `path` as a TIFF.
pub fn write_tiff(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    pixels: &[u8],
    compression: TiffCompression,
) -> io::Result<()> {
    let file = File::create(path)?;
    write_tiff_to(BufWriter::new(file), width, height, pixels, compression)
}

fn io_err(e: TiffError) -> io::Error {
    io::Error::other(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::decoder::{Decoder, DecodingResult};
    use tiff::ColorType;

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
        let bytes = encode_tiff(4, 3, &gradient(4, 3), TiffCompression::Deflate).expect("encode");
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
            let bytes = encode_tiff(16, 9, &pixels, compression).expect("encode");
            let mut decoder = Decoder::new(io::Cursor::new(bytes)).expect("decode");

            assert_eq!(decoder.dimensions().expect("dimensions"), (16, 9));
            assert_eq!(decoder.colortype().expect("colortype"), ColorType::RGBA(8));
            match decoder.read_image().expect("read") {
                DecodingResult::U8(got) => {
                    assert_eq!(got, pixels, "{compression:?} altered the pixels")
                }
                other => panic!("{compression:?} decoded as {other:?}, expected 8-bit samples"),
            }
        }
    }

    #[test]
    fn alpha_is_declared_unassociated() {
        let bytes = encode_tiff(4, 3, &gradient(4, 3), TiffCompression::Deflate).expect("encode");
        let mut decoder = Decoder::new(io::Cursor::new(bytes)).expect("decode");
        assert_eq!(
            decoder
                .get_tag_u32_vec(Tag::ExtraSamples)
                .expect("ExtraSamples"),
            vec![u32::from(ExtraSamples::UnassociatedAlpha.to_u16())]
        );
    }

    #[test]
    fn encode_tiff_and_write_tiff_agree_byte_for_byte() {
        let pixels = gradient(5, 4);
        let in_memory = encode_tiff(5, 4, &pixels, TiffCompression::Lzw).expect("encode");

        let dir = std::env::temp_dir().join("hephaestus_tiff_roundtrip");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.tiff");
        write_tiff(&path, 5, 4, &pixels, TiffCompression::Lzw).expect("write");
        let on_disk = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(in_memory, on_disk);
    }

    #[test]
    fn wrong_length_buffer_is_rejected() {
        let short = vec![0u8; 4 * 3 * 4 - 1];
        let err = encode_tiff(4, 3, &short, TiffCompression::Deflate).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let mut sink = io::Cursor::new(Vec::new());
        let err = write_tiff_to(&mut sink, 4, 3, &short, TiffCompression::Deflate)
            .expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            sink.into_inner().is_empty(),
            "nothing should be written on a size error"
        );
    }
}
