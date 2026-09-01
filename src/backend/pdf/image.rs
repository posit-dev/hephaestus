//! Raster images as `/XObject /Image` streams, with alpha in a
//! separate `/SMask`.
//!
//! Unlike the SVG backend this needs no codec: PDF takes raw samples,
//! so a scene's already-decoded pixels go straight into a
//! `FlateDecode`d stream. The only preparation is what every consumer
//! of a [`peniko::ImageData`] has to do — normalize the channel order
//! and undo premultiplication.

use super::res::{ResKind, Resources};
use super::{PdfWarning, Warnings};
use crate::brush::{Image, ImageAlphaType, ImageFormat, Sampling};
use crate::geometry::Affine;

/// Maps the PDF unit square onto an image's own pixel space.
///
/// `draw_image`'s transform maps image pixel space — `(0, 0)` at the
/// top-left sample, `(w, h)` at the bottom-right — into scene space,
/// whereas an image XObject occupies the unit square with its first row
/// at `v = 1`. This is the step between.
pub(crate) fn unit_to_pixels(image: &Image) -> Affine {
    let (w, h) = (f64::from(image.width), f64::from(image.height));
    Affine::new([w, 0.0, 0.0, -h, 0.0, h])
}

/// Intern `image` as an image XObject and return the name that refers
/// to it, or `None` when there is nothing embeddable.
///
/// The dedup key is the blob's identity rather than the converted
/// bytes: an `ImageGeom` drawing one marker for five thousand rows
/// would otherwise convert five thousand times to discover a match.
/// Two distinct blobs with identical content embed twice, which is the
/// acceptable side of that trade.
pub(crate) fn intern(
    image: &Image,
    sampling: Sampling,
    res: &mut Resources,
    warnings: &mut Warnings,
) -> Option<String> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let key = format!(
        "image:{}:{}:{:?}:{:?}:{}x{}",
        image.data.id(),
        image.width,
        image.format,
        image.alpha_type,
        image.width,
        image.height,
    );
    // `Interpolate` varies per draw while the samples do not, so it is
    // part of the key rather than a reason to convert again.
    let key = format!("{key}:{}", sampling == Sampling::Bilinear);
    if let Some(name) = res.lookup(&key) {
        return Some(name.to_string());
    }
    let rgba = straight_rgba(image, warnings)?;
    Some(intern_samples(
        &key,
        image.width,
        image.height,
        &rgba,
        sampling == Sampling::Bilinear,
        res,
    ))
}

/// Intern straight-alpha RGBA8 samples as an image XObject.
///
/// The lower half of [`intern`], reached separately by the color-glyph
/// path: a bitmap strike is samples with no [`Image`] around them, and
/// wrapping one would mint a fresh blob whose identity no two draws
/// share.
pub(crate) fn intern_samples(
    key: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
    interpolate: bool,
    res: &mut Resources,
) -> String {
    if let Some(name) = res.lookup(key) {
        return name.to_string();
    }
    let (rgb, alpha) = planes(rgba);
    let smask = alpha.map(|a| {
        (
            format!(
                "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
                 /ColorSpace /DeviceGray /BitsPerComponent 8"
            ),
            a,
        )
    });
    let mut dict = format!(
        "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Interpolate {interpolate}"
    );
    if smask.is_some() {
        dict.push_str(" /SMask ");
        dict.push_str(super::res::SUB_REF);
    }
    res.intern_stream(ResKind::XObject, key, &dict, rgb, smask)
}

/// An image's samples as straight-alpha RGBA8, or `None` when the
/// layout is one this backend cannot embed.
fn straight_rgba(image: &Image, warnings: &mut Warnings) -> Option<Vec<u8>> {
    let expected = u128::from(image.width) * u128::from(image.height) * 4;
    if image.data.as_ref().len() as u128 != expected {
        warnings.note(PdfWarning::UnembeddableImage);
        return None;
    }
    let mut pixels: Vec<u8> = image.data.as_ref().to_vec();
    match image.format {
        ImageFormat::Rgba8 => {}
        ImageFormat::Bgra8 => {
            for px in pixels.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        _ => {
            warnings.note(PdfWarning::UnembeddableImage);
            return None;
        }
    }
    if image.alpha_type == ImageAlphaType::AlphaPremultiplied {
        unpremultiply(&mut pixels);
    }
    Some(pixels)
}

/// Undo premultiplication in place, with the rounding the SVG backend
/// uses so the two produce the same bytes from one source.
pub(crate) fn unpremultiply(pixels: &mut [u8]) {
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

/// Split straight-alpha RGBA8 samples into the RGB plane and the alpha
/// plane a PDF image and its soft mask need.
///
/// A fully opaque image gets no alpha plane, and so no `/SMask` — which
/// is both smaller and what a viewer's fast path expects.
pub(crate) fn planes(rgba: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    let n = rgba.len() / 4;
    let mut rgb = Vec::with_capacity(n * 3);
    let mut alpha = Vec::with_capacity(n);
    let mut translucent = false;
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
        alpha.push(px[3]);
        translucent |= px[3] != 255;
    }
    (rgb, translucent.then_some(alpha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point;
    use peniko::Blob;

    fn image(data: Vec<u8>, alpha_type: ImageAlphaType) -> Image {
        Image {
            data: Blob::from(data),
            format: ImageFormat::Rgba8,
            alpha_type,
            width: 2,
            height: 1,
        }
    }

    /// The unit square's first row must land on the image's first row,
    /// or every embedded image is upside down.
    #[test]
    fn the_unit_square_maps_onto_pixel_space_first_row_first() {
        let img = image(vec![0; 8], ImageAlphaType::Alpha);
        let m = unit_to_pixels(&img);
        assert_eq!(m * Point::new(0.0, 1.0), Point::new(0.0, 0.0));
        assert_eq!(m * Point::new(1.0, 0.0), Point::new(2.0, 1.0));
    }

    #[test]
    fn an_opaque_image_gets_no_alpha_plane() {
        let (rgb, alpha) = planes(&[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(rgb, vec![1, 2, 3, 4, 5, 6]);
        assert!(alpha.is_none());
    }

    #[test]
    fn a_translucent_image_splits_into_two_planes() {
        let (rgb, alpha) = planes(&[1, 2, 3, 255, 4, 5, 6, 128]);
        assert_eq!(rgb, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(alpha.unwrap(), vec![255, 128]);
    }

    #[test]
    fn premultiplied_samples_are_restored_to_straight_alpha() {
        let mut warnings = Warnings::default();
        // Half-alpha mid gray, premultiplied.
        let img = image(
            vec![64, 64, 64, 128, 0, 0, 0, 0],
            ImageAlphaType::AlphaPremultiplied,
        );
        let out = straight_rgba(&img, &mut warnings).unwrap();
        assert_eq!(out[0], 128, "64 over alpha 128 is 128");
        assert_eq!(&out[4..8], &[0, 0, 0, 0], "a zero alpha zeroes the color");
    }

    #[test]
    fn an_image_is_interned_once_however_often_it_is_drawn() {
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let img = image(vec![1, 2, 3, 255, 4, 5, 6, 255], ImageAlphaType::Alpha);
        let a = intern(&img, Sampling::Nearest, &mut res, &mut warnings).unwrap();
        let b = intern(&img, Sampling::Nearest, &mut res, &mut warnings).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_mis_sized_buffer_is_reported_rather_than_embedded() {
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let img = image(vec![1, 2, 3], ImageAlphaType::Alpha);
        assert!(intern(&img, Sampling::Nearest, &mut res, &mut warnings).is_none());
        assert!(warnings.contains(&PdfWarning::UnembeddableImage));
    }
}
