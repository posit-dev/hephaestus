//! Raster image writers.
//!
//! Every writer consumes the buffer a [`Renderer`](crate::backend::Renderer)
//! fills: RGBA8 with straight (un-premultiplied) alpha, tightly packed
//! top-down rows, exactly `width * height * 4` bytes. Encoding lives here
//! rather than behind the `Renderer` trait so backends only need to produce
//! that buffer.
//!
//! One cargo feature per format, each pulling in exactly one encoder:
//!
//! - `write_png` (feature `png`) — lossless, alpha preserved.
//! - `write_jpeg` (feature `jpeg`) — lossy, and the format has no alpha
//!   channel, so the buffer is composited onto a background color.
//! - `write_tiff` (feature `tiff`) — lossless, alpha preserved, choice of
//!   compressor.
//! - `write_webp` (feature `webp`) — lossless, alpha preserved, and smaller
//!   than PNG on the flat fills and hard edges a plot is made of.
//!
//! Each format offers the same three entry points: `write_*` to a path,
//! `write_*_to` for an arbitrary writer, and `encode_*` for the bytes in
//! memory — an HTTP response body, a base64 data URL, a clipboard payload.

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
pub use jpeg::{encode_jpeg, write_jpeg, write_jpeg_to};

#[cfg(feature = "png")]
#[cfg_attr(docsrs, doc(cfg(feature = "png")))]
pub use png::{encode_png, write_png, write_png_to};

#[cfg(feature = "tiff")]
#[cfg_attr(docsrs, doc(cfg(feature = "tiff")))]
pub use tiff::{encode_tiff, write_tiff, write_tiff_to, TiffCompression};

#[cfg(feature = "webp")]
#[cfg_attr(docsrs, doc(cfg(feature = "webp")))]
pub use webp::{encode_webp, write_webp, write_webp_to};

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

#[cfg(test)]
mod tests {
    use super::*;

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
