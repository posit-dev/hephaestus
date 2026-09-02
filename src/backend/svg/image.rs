//! Raster images as base64 `<image>` definitions referenced by `<use>`.
//!
//! A scene's [`Image`](crate::brush::Image) holds decoded pixels — the
//! bytes the author originally loaded are long gone by the time one is
//! registered — so embedding means re-encoding. PNG is the choice for
//! the same reasons `document/` gives: lossless, alpha-preserving, and
//! one decoder covers every case.
//!
//! That makes drawing an image need the `png` feature. It is not
//! implied: the whole point of this backend's dependency story is that
//! `document-read` + `svg` builds with no renderer and no codec, and
//! implying `png` would grow that build for a capability most plots
//! never use. Without it an image reports
//! [`SvgWarning::MissingPngFeature`](super::SvgWarning::MissingPngFeature)
//! and draws nothing, which is the same degradation a document without
//! `png` produces.

use super::defs::{DefKind, Defs};
use super::writer::{num, transform_attr};
use super::{SvgWarning, Warnings};
use crate::brush::{Image, Sampling};
use crate::geometry::Affine;

/// Emit an image, interning its payload so repeated draws share it.
///
/// Dedup is not an optimization here but a requirement: an `ImageGeom`
/// drawing one marker for five thousand rows would otherwise write five
/// thousand copies of the same base64 payload. `image-rendering` and
/// `opacity` vary per draw, so they go on the `<use>` rather than the
/// shared `<image>`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit(
    out: &mut String,
    image: &Image,
    transform: Affine,
    sampling: Sampling,
    alpha: f32,
    defs: &mut Defs,
    doc_prefix: &str,
    decimals: u8,
    warnings: &mut Warnings,
) {
    let Some(href) = data_url(image, warnings) else {
        return;
    };
    let mut body = String::with_capacity(href.len() + 128);
    body.push_str("<image width=\"");
    num(&mut body, f64::from(image.width), decimals);
    body.push_str("\" height=\"");
    num(&mut body, f64::from(image.height), decimals);
    // With `width`/`height` at the intrinsic size the default
    // `xMidYMid meet` happens to be a no-op; `none` makes the mapping
    // exact by construction rather than by coincidence.
    body.push_str("\" preserveAspectRatio=\"none\" href=\"");
    body.push_str(&href);
    body.push_str("\"/>");
    let id = defs.intern(DefKind::Image, &body, doc_prefix);

    out.push_str("<use href=\"#");
    out.push_str(&id);
    out.push('"');
    // `draw_image`'s transform maps the image's own pixel space
    // (0, 0)-(width, height) onto the output, so the `<use>` needs
    // nothing but that transform.
    transform_attr(out, transform, decimals);
    if sampling == Sampling::Nearest {
        out.push_str(" image-rendering=\"pixelated\"");
    }
    if alpha < 1.0 {
        out.push_str(" opacity=\"");
        num(out, f64::from(alpha), decimals.max(3));
        out.push('"');
    }
    out.push_str("/>");
}

/// Re-encode `image` as a PNG data URL.
#[cfg(feature = "png")]
fn data_url(image: &Image, warnings: &mut Warnings) -> Option<String> {
    use crate::brush::{ImageAlphaType, ImageFormat};

    if image.width == 0 || image.height == 0 {
        return None;
    }
    let expected = u128::from(image.width) * u128::from(image.height) * 4;
    if image.data.as_ref().len() as u128 != expected {
        warnings.note(SvgWarning::UnembeddableImage);
        return None;
    }
    // PNG is a straight-alpha format, and both channel orders are legal
    // peniko values, so neither can be handed to the encoder as-is.
    let mut pixels: Vec<u8> = image.data.as_ref().to_vec();
    match image.format {
        ImageFormat::Rgba8 => {}
        ImageFormat::Bgra8 => {
            for px in pixels.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        _ => {
            warnings.note(SvgWarning::UnembeddableImage);
            return None;
        }
    }
    if image.alpha_type == ImageAlphaType::AlphaPremultiplied {
        for px in pixels.chunks_exact_mut(4) {
            let a = px[3];
            if a == 0 {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
            } else if a != 255 {
                for c in &mut px[..3] {
                    *c = ((u16::from(*c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8;
                }
            }
        }
    }
    // No dpi: the element carries an explicit width and height, so the
    // embedded bytes describe pixels rather than a physical size.
    let png = crate::image::encode_png(image.width, image.height, &pixels, None).ok()?;
    let mut url = String::from("data:image/png;base64,");
    super::base64::encode_into(&png, &mut url);
    Some(url)
}

/// Without a PNG encoder there is nothing to embed.
#[cfg(not(feature = "png"))]
fn data_url(_image: &Image, warnings: &mut Warnings) -> Option<String> {
    warnings.note(SvgWarning::MissingPngFeature);
    None
}
