//! Raster image readers and writers.
//!
//! Every writer consumes the buffer a [`Renderer`](crate::backend::Renderer)
//! fills: RGBA8 with straight (un-premultiplied) alpha, tightly packed
//! top-down rows, exactly `width * height * 4` bytes. Encoding lives here
//! rather than behind the `Renderer` trait so backends only need to produce
//! that buffer.
//!
//! One cargo feature per format, each pulling in exactly one codec:
//!
//! - `png` — lossless, alpha preserved.
//! - `jpeg` — lossy, and the format has no alpha channel, so writing
//!   composites the buffer onto a background color and reading yields an
//!   opaque image.
//! - `tiff` — lossless, alpha preserved, choice of compressor.
//! - `webp` — lossless, alpha preserved, and smaller than PNG on the flat
//!   fills and hard edges a plot is made of.
//!
//! Each format offers the same three write entry points: `write_*` to a path,
//! `write_*_to` for an arbitrary writer, and `encode_*` for the bytes in
//! memory — an HTTP response body, a base64 data URL, a clipboard payload.
//! And the same three read entry points: `read_*` from a path, `read_*_from`
//! for an arbitrary reader, and `decode_*` for bytes already in memory.
//!
//! Reading produces a [`brush::Image`](crate::brush::Image) — the type
//! [`SceneBuilder::draw_image`](crate::scene::SceneBuilder::draw_image) and
//! [`ImageGeom`](crate::plot::ImageGeom) consume. Whatever the file holds,
//! the result is normalised to the same contract the writers enforce: RGBA8,
//! straight alpha, tightly packed top-down rows. Grayscale expands to grey
//! RGB, a missing alpha channel becomes fully opaque, and 16-bit samples are
//! truncated to 8.

use std::io;

#[cfg(feature = "jpeg")]
mod jpeg;
#[cfg(feature = "png")]
mod png;
#[cfg(feature = "tiff")]
mod tiff;
#[cfg(feature = "webp")]
mod webp;

#[cfg(feature = "jpeg")]
#[cfg_attr(docsrs, doc(cfg(feature = "jpeg")))]
pub use jpeg::{decode_jpeg, encode_jpeg, read_jpeg, read_jpeg_from, write_jpeg, write_jpeg_to};

#[cfg(feature = "png")]
#[cfg_attr(docsrs, doc(cfg(feature = "png")))]
pub use png::{decode_png, encode_png, read_png, read_png_from, write_png, write_png_to};

#[cfg(feature = "tiff")]
#[cfg_attr(docsrs, doc(cfg(feature = "tiff")))]
pub use tiff::{
    decode_tiff, encode_tiff, read_tiff, read_tiff_from, write_tiff, write_tiff_to, TiffCompression,
};

#[cfg(feature = "webp")]
#[cfg_attr(docsrs, doc(cfg(feature = "webp")))]
pub use webp::{decode_webp, encode_webp, read_webp, read_webp_from, write_webp, write_webp_to};

/// Check `pixels` against the buffer contract every writer shares: a
/// non-empty image whose bytes are exactly one tightly packed RGBA8 row per
/// image row.
pub(crate) fn check_pixels(width: u32, height: u32, pixels: &[u8]) -> io::Result<()> {
    if width == 0 || height == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("image dimensions must be non-zero; got {width}x{height}"),
        ));
    }
    // Widened: the product overflows a 32-bit usize well inside the
    // dimensions the formats themselves allow, and overflows u64 at the top
    // of the u32 range.
    let expected = u128::from(width) * u128::from(height) * 4;
    if pixels.len() as u128 != expected {
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
    Ok(())
}

/// Reject an image larger than `format`'s headers can describe.
#[cfg(any(feature = "jpeg", feature = "webp"))]
pub(crate) fn check_dimension_limit(
    format: &str,
    limit: u32,
    width: u32,
    height: u32,
) -> io::Result<()> {
    if width > limit || height > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{width}x{height} exceeds the {limit}x{limit} {format} maximum"),
        ));
    }
    Ok(())
}

/// Wrap RGBA8 pixels as a [`brush::Image`](crate::brush::Image).
///
/// `pixels` must satisfy the same contract the writers enforce: straight
/// (un-premultiplied) alpha, tightly packed top-down rows, exactly
/// `width * height * 4` bytes. The way in for a caller holding pixels this
/// module's decoders didn't produce.
pub fn from_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> io::Result<crate::brush::Image> {
    check_pixels(width, height, &pixels)?;
    Ok(crate::brush::Image {
        data: crate::brush::Blob::new(std::sync::Arc::new(pixels)),
        format: crate::brush::ImageFormat::Rgba8,
        alpha_type: crate::brush::ImageAlphaType::Alpha,
        width,
        height,
    })
}

/// Which format a byte buffer holds, as far as its signature says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signature {
    Png,
    Jpeg,
    Tiff,
    WebP,
}

/// Read a buffer's format from its leading bytes. `None` when the bytes match
/// no format this module knows, whether or not its codec is compiled in.
fn signature(bytes: &[u8]) -> Option<Signature> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(Signature::Png);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(Signature::Jpeg);
    }
    if bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a") {
        return Some(Signature::Tiff);
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(Signature::WebP);
    }
    None
}

/// Decode image bytes of any format this build reads.
///
/// Dispatch is on the buffer's signature rather than on a filename, so a
/// `.png` holding a JPEG still decodes and a location with no extension is no
/// obstacle. A format whose codec is not compiled in reports
/// [`io::ErrorKind::Unsupported`] — which is the distinction a caller needs to
/// tell "this build cannot read that" from "those bytes are not an image".
pub fn decode_image(bytes: &[u8]) -> io::Result<crate::brush::Image> {
    let Some(signature) = signature(bytes) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bytes match no supported image format",
        ));
    };
    match signature {
        #[cfg(feature = "png")]
        Signature::Png => decode_png(bytes),
        #[cfg(feature = "jpeg")]
        Signature::Jpeg => decode_jpeg(bytes),
        #[cfg(feature = "tiff")]
        Signature::Tiff => decode_tiff(bytes),
        #[cfg(feature = "webp")]
        Signature::WebP => decode_webp(bytes),
        #[allow(unreachable_patterns)]
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{other:?} images need a codec this build was not compiled with"),
        )),
    }
}

/// Read an image file of any format this build reads.
///
/// The whole file is read before dispatch, since the signature decides which
/// decoder gets it. See [`decode_image`] for how a format this build cannot
/// decode is reported.
pub fn read_image(path: impl AsRef<std::path::Path>) -> io::Result<crate::brush::Image> {
    decode_image(&std::fs::read(path)?)
}

/// Widen interleaved 8-bit samples to RGBA8.
///
/// `channels` counts samples per pixel: 1 grey, 2 grey + alpha, 3 RGB, 4
/// RGBA. Grey replicates across the colour channels and a missing alpha
/// channel becomes opaque, so every format's decoder lands on one buffer
/// shape regardless of what its file held.
#[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
pub(crate) fn expand_to_rgba8(
    samples: &[u8],
    channels: usize,
    width: u32,
    height: u32,
) -> io::Result<Vec<u8>> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{width}x{height} pixel count overflows this platform's usize"),
            )
        })?;
    if !(1..=4).contains(&channels) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot read a {channels}-sample-per-pixel image as RGBA8"),
        ));
    }
    if samples.len() < count * channels {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "decoded {} samples; expected at least {} for {}x{} at {} per pixel",
                samples.len(),
                count * channels,
                width,
                height,
                channels
            ),
        ));
    }
    if channels == 4 {
        return Ok(samples[..count * 4].to_vec());
    }
    let mut out = Vec::with_capacity(count * 4);
    for px in samples[..count * channels].chunks_exact(channels) {
        let (r, g, b, a) = match channels {
            1 => (px[0], px[0], px[0], 255),
            2 => (px[0], px[0], px[0], px[1]),
            _ => (px[0], px[1], px[2], 255),
        };
        out.extend_from_slice(&[r, g, b, a]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_rgba8_wraps_the_buffer_it_is_given() {
        let image = from_rgba8(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).expect("valid buffer");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.format, crate::brush::ImageFormat::Rgba8);
        assert_eq!(image.alpha_type, crate::brush::ImageAlphaType::Alpha);
        assert_eq!(image.data.as_ref(), &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn from_rgba8_holds_the_buffer_contract() {
        let err = from_rgba8(2, 2, vec![0; 4]).expect_err("too short");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    #[test]
    fn expanding_grey_replicates_across_the_colour_channels() {
        let got = expand_to_rgba8(&[0, 128, 255], 1, 3, 1).expect("expand");
        assert_eq!(
            got,
            vec![0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255]
        );
    }

    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    #[test]
    fn expanding_grey_plus_alpha_keeps_the_alpha() {
        let got = expand_to_rgba8(&[10, 20, 30, 40], 2, 2, 1).expect("expand");
        assert_eq!(got, vec![10, 10, 10, 20, 30, 30, 30, 40]);
    }

    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    #[test]
    fn expanding_rgb_fills_an_opaque_alpha() {
        let got = expand_to_rgba8(&[1, 2, 3, 4, 5, 6], 3, 2, 1).expect("expand");
        assert_eq!(got, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    #[test]
    fn expanding_rgba_passes_the_samples_through() {
        let got = expand_to_rgba8(&[1, 2, 3, 4, 5, 6, 7, 8], 4, 2, 1).expect("expand");
        assert_eq!(got, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    /// Trailing samples beyond the stated pixel count are dropped rather
    /// than folded in — a decoder that padded its buffer to a row or block
    /// boundary must not shift the image.
    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    #[test]
    fn expanding_ignores_samples_past_the_pixel_count() {
        let got = expand_to_rgba8(&[1, 2, 3, 9, 9, 9], 3, 1, 1).expect("expand");
        assert_eq!(got, vec![1, 2, 3, 255]);
    }

    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    #[test]
    fn expanding_too_few_samples_is_rejected() {
        let err = expand_to_rgba8(&[1, 2], 3, 2, 1).expect_err("short buffer");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(any(feature = "png", feature = "jpeg", feature = "tiff", feature = "webp"))]
    #[test]
    fn expanding_an_unsupported_sample_count_is_rejected() {
        let err = expand_to_rgba8(&[0; 10], 5, 2, 1).expect_err("five samples");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        for (w, h) in [(0, 4), (4, 0), (0, 0)] {
            let err = check_pixels(w, h, &[]).expect_err("zero area must fail");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn buffer_must_hold_exactly_one_rgba8_pixel_per_pixel() {
        assert!(check_pixels(4, 3, &[0u8; 4 * 3 * 4]).is_ok());

        for len in [4 * 3 * 4 - 1, 4 * 3 * 4 + 1] {
            let err = check_pixels(4, 3, &vec![0u8; len]).expect_err("wrong length must fail");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    /// A buffer length that overflows a 32-bit `usize` must still be compared
    /// against the real expected size rather than a wrapped one.
    #[test]
    fn oversized_dimensions_do_not_wrap() {
        let err = check_pixels(u32::MAX, u32::MAX, &[0u8; 16]).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
