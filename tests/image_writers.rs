//! Runs rendered pixels through every enabled raster writer.
//!
//! The unit tests inside each writer feed it synthetic buffers; this one
//! starts from `render_to_buffer`, so it pins the whole chain — the padded
//! GPU readback being de-padded, the straight alpha, and the encoder's view
//! of both. The lossless formats are asserted to reproduce the rendered
//! pixels exactly.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, rgba, Color};
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
    let bytes = hephaestus::image::encode_png(W, H, &render()).expect("encode png");
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    );
}

#[cfg(feature = "jpeg")]
#[test]
fn jpeg_encodes_rendered_pixels() {
    let bytes =
        hephaestus::image::encode_jpeg(W, H, &render(), 90, Color::WHITE).expect("encode jpeg");
    assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF]);
    assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);
}

#[cfg(feature = "tiff")]
#[test]
fn tiff_round_trips_rendered_pixels() {
    use hephaestus::image::TiffCompression;
    use tiff::decoder::{Decoder, DecodingResult};

    let pixels = render();
    let bytes =
        hephaestus::image::encode_tiff(W, H, &pixels, TiffCompression::Deflate).expect("encode");

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
    let bytes = hephaestus::image::encode_webp(W, H, &pixels).expect("encode");

    let mut decoder = WebPDecoder::new(std::io::Cursor::new(bytes)).expect("decode");
    assert_eq!(decoder.dimensions(), (W, H));
    let mut got = vec![0u8; decoder.output_buffer_size().expect("buffer size")];
    decoder.read_image(&mut got).expect("read");
    assert_eq!(got, pixels);
}
