//! Runs rendered pixels through every enabled raster writer.
//!
//! The unit tests inside each writer feed it synthetic buffers; this one
//! starts from `render_to_buffer`, so it pins the whole chain — the padded
//! GPU readback being de-padded, the straight alpha, and the encoder's view
//! of both. The lossless formats are asserted to reproduce the rendered
//! pixels exactly.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, rgba};
// Only the JPEG writer takes a background colour — it has no alpha channel
// to leave transparent.
#[cfg(feature = "jpeg")]
use hephaestus::color::Color;
use hephaestus::geometry::{Affine, Rect};
use hephaestus::path::FillRule;
use hephaestus::pick::PickId;
use hephaestus::primitives;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

const W: u32 = 61;
const H: u32 = 37;

/// An opaque background with an off-center translucent patch over it: a
/// width that is not a multiple of the 256-byte readback alignment, plus
/// alpha for the formats that carry it.
fn render() -> Vec<u8> {
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    renderer.scene().clear();
    renderer.scene().fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &rgba(0.1, 0.6, 0.9, 0.5).into(),
        None,
        &primitives::rect(Rect::new(7.0, 5.0, 40.0, 31.0)),
        PickId::Skip,
    );
    let mut pixels = vec![0u8; (W * H * 4) as usize];
    renderer
        .render_to_buffer(W, H, rgb8(20, 22, 28), &mut pixels)
        .expect("render");
    pixels
}

#[cfg(feature = "png")]
#[test]
fn png_encodes_rendered_pixels() {
    use hephaestus::image::PngCompression;
    let bytes = hephaestus::image::encode_png(W, H, &render(), PngCompression::Balanced, None)
        .expect("encode png");
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    );
}

#[cfg(feature = "jpeg")]
#[test]
fn jpeg_encodes_rendered_pixels() {
    let bytes = hephaestus::image::encode_jpeg(W, H, &render(), 90, Color::WHITE, None)
        .expect("encode jpeg");
    assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF]);
    assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);
}

#[cfg(feature = "tiff")]
#[test]
fn tiff_round_trips_rendered_pixels() {
    use hephaestus::image::TiffCompression;
    use tiff::decoder::{Decoder, DecodingResult};

    let pixels = render();
    let bytes = hephaestus::image::encode_tiff(W, H, &pixels, TiffCompression::Deflate, None)
        .expect("encode");

    let mut decoder = Decoder::new(std::io::Cursor::new(bytes)).expect("decode");
    assert_eq!(decoder.dimensions().expect("dimensions"), (W, H));
    match decoder.read_image().expect("read") {
        DecodingResult::U8(got) => assert_eq!(got, pixels),
        other => panic!("decoded as {other:?}, expected 8-bit samples"),
    }
}

#[cfg(feature = "webp")]
#[test]
fn webp_round_trips_rendered_pixels() {
    use image_webp::WebPDecoder;

    let pixels = render();
    let bytes = hephaestus::image::encode_webp(W, H, &pixels, None).expect("encode");

    let mut decoder = WebPDecoder::new(std::io::Cursor::new(bytes)).expect("decode");
    assert_eq!(decoder.dimensions(), (W, H));
    let mut got = vec![0u8; decoder.output_buffer_size().expect("buffer size")];
    decoder.read_image(&mut got).expect("read");
    assert_eq!(got, pixels);
}

/// The resolution the pixels above were rendered at: 192 dpi is a 2x display,
/// and the case a file declaring nothing gets wrong.
#[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
const DPI: f64 = 192.0;

#[cfg(feature = "png")]
#[test]
fn png_records_the_render_dpi() {
    use hephaestus::image::PngCompression;
    let bytes = hephaestus::image::encode_png(W, H, &render(), PngCompression::Balanced, Some(DPI))
        .expect("encode png");
    let reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .expect("read info");
    let dims = reader.info().pixel_dims.expect("pHYs chunk");
    assert_eq!(dims.unit, png::Unit::Meter);
    assert_eq!(dims.xppu, (DPI / 0.0254).round() as u32);
    assert_eq!(dims.yppu, dims.xppu);
}

#[cfg(feature = "jpeg")]
#[test]
fn jpeg_records_the_render_dpi() {
    let bytes = hephaestus::image::encode_jpeg(W, H, &render(), 90, Color::WHITE, Some(DPI))
        .expect("encode jpeg");
    // The JFIF APP0 segment: "JFIF\0", two version bytes, the density unit,
    // then the two densities big-endian.
    assert_eq!(&bytes[6..11], b"JFIF\0");
    assert_eq!(bytes[13], 1, "unit must be inches");
    assert_eq!(u16::from_be_bytes(bytes[14..16].try_into().unwrap()), 192);
    assert_eq!(u16::from_be_bytes(bytes[16..18].try_into().unwrap()), 192);
}

#[cfg(feature = "tiff")]
#[test]
fn tiff_records_the_render_dpi() {
    use hephaestus::image::TiffCompression;
    use tiff::decoder::{ifd::Value, Decoder};
    use tiff::tags::{ResolutionUnit, Tag};

    let bytes =
        hephaestus::image::encode_tiff(W, H, &render(), TiffCompression::Deflate, Some(DPI))
            .expect("encode");
    let mut decoder = Decoder::new(std::io::Cursor::new(bytes)).expect("decode");
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

#[cfg(feature = "webp")]
#[test]
fn webp_records_the_render_dpi() {
    use image_webp::WebPDecoder;

    let bytes = hephaestus::image::encode_webp(W, H, &render(), Some(DPI)).expect("encode");
    let mut decoder = WebPDecoder::new(std::io::Cursor::new(bytes)).expect("decode");
    let exif = decoder.exif_metadata().expect("exif").expect("EXIF chunk");
    // The two rationals sit after the header and its one three-tag directory.
    assert_eq!(u32::from_le_bytes(exif[50..54].try_into().unwrap()), 192);
    assert_eq!(u32::from_le_bytes(exif[54..58].try_into().unwrap()), 1);
}
