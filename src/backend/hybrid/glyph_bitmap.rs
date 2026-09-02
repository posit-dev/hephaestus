//! Bitmap color glyphs: the strikes a face carries in place of outlines.
//!
//! Apple Color Emoji and most Android emoji ship their glyphs as PNG images
//! rather than contours, and the outline the face carries beside a strike is
//! empty — so a backend that ignores strikes draws those glyphs as nothing at
//! all.
//!
//! The rasterizer has its own strike path, and this backend deliberately does
//! not use it. Two reasons. It reaches the GPU only through the glyph atlas,
//! which accepts no rotation or skew and no full page; anything else arrives
//! at the render pipeline as CPU pixels, which it rejects outright. And it
//! paints the strike's own colors, so the pick pass would read an emoji's
//! pixels back as a spray of ids that were never drawn — exactly what this
//! backend exists to prevent.
//!
//! Resolved here instead, a strike is an ordinary image: one atlas upload per
//! distinct strike whatever the transform, and a pick pass that paints the
//! caller's id like every other draw.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use skrifa::bitmap::{BitmapData, BitmapFormat, BitmapGlyph, BitmapStrikes, Origin};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::brush::Image;
use crate::color::Color;
use crate::geometry::Affine;
use crate::scene::{Font, Glyph};

/// One strike, decoded and placed.
pub(super) struct Strike {
    /// The strike's samples, RGBA8 with straight alpha.
    pub image: Image,
    /// Maps the image's pixel space onto the *run's* space, so a caller
    /// composes this with the run transform rather than using it alone.
    pub transform: Affine,
}

/// The color an alpha-mask strike is painted in: the run's own color when
/// it has one, black when the brush is a gradient or an image.
pub(super) fn foreground(brush: &crate::brush::Brush) -> Color {
    match brush {
        crate::brush::Brush::Solid(color) => *color,
        _ => Color::BLACK,
    }
}

/// Split a run's glyphs into the ones drawn from outlines and the ones a
/// bitmap strike serves.
///
/// `None` means every glyph is an outline glyph — the answer for any
/// ordinary text face — and says the caller should draw the run as it
/// stands instead of rebuilding a glyph list that would not change.
///
/// `foreground` colors an alpha-mask strike, the one strike format that
/// carries no color of its own.
pub(super) fn split(
    font: &Font,
    font_size: f32,
    glyph_transform: Option<Affine>,
    foreground: Color,
    glyphs: &[Glyph],
) -> Option<(Vec<Glyph>, Vec<Strike>)> {
    let data = font.data();
    let face_id = (data.data.id(), data.index);
    // Every run of every frame comes through here, and for an ordinary text
    // face the answer is always no — so the answer is remembered, and a face
    // is parsed to find it out once rather than per frame.
    if lock().faces.get(&face_id) == Some(&false) {
        return None;
    }
    let font_ref = FontRef::from_index(data.data.as_ref(), data.index).ok()?;
    let strikes = font_ref.bitmap_strikes();
    lock().faces.insert(face_id, !strikes.is_empty());
    if strikes.is_empty() {
        return None;
    }
    // Apple Color Emoji's zero y bearing is corrected for sbix alone, so
    // which table the strikes came from is part of the placement.
    let sbix = strikes.format() == Some(BitmapFormat::Sbix);
    // A face can carry both tables, and COLR wins — the order the other
    // backends resolve in.
    let colr = font_ref.color_glyphs();
    let upem = match font_ref
        .metrics(Size::unscaled(), LocationRef::default())
        .units_per_em
    {
        0 => 1000.0,
        upem => f64::from(upem),
    };

    let mut outlines = Vec::with_capacity(glyphs.len());
    let mut bitmaps = Vec::new();
    for glyph in glyphs {
        let id = GlyphId::new(glyph.id);
        if colr.get(id).is_none() {
            if let Some(strike) = resolve(
                &strikes,
                Face {
                    blob: data.data.id(),
                    index: data.index,
                    upem,
                    sbix,
                },
                font_size,
                glyph_transform,
                foreground,
                *glyph,
            ) {
                bitmaps.push(strike);
                continue;
            }
        }
        outlines.push(*glyph);
    }
    Some((outlines, bitmaps))
}

/// The face a run draws from, as strike resolution needs it.
struct Face {
    /// Identity of the font bytes, from the blob they arrived in.
    blob: u64,
    /// Which face within a collection.
    index: u32,
    /// Design units per em, for scaling font-unit bearings.
    upem: f64,
    /// True when the strikes come from an `sbix` table.
    sbix: bool,
}

/// Cache key for one decoded strike: a face, a glyph in it, the size the
/// strike was chosen for, and the color an alpha mask is painted in.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
struct Key {
    blob: u64,
    face: u32,
    glyph: u32,
    size: u32,
    color: u32,
}

/// What resolving a strike remembers between frames.
#[derive(Default)]
struct Cache {
    /// Whether a face carries bitmap strikes at all, by font blob and index.
    faces: HashMap<(u64, u32), bool>,
    /// Decoded strikes, so an emoji costs one decode rather than one per
    /// frame per pass.
    ///
    /// Holding the [`Image`] rather than its pixels matters for more than
    /// the decode: the backend keys atlas uploads by the identity of an
    /// image's bytes, so one entry here is also one upload. It is why
    /// decoding happens *under the lock* — two renderers resolving the same
    /// strike at once must come away with the same image, or one of them
    /// uploads bytes the other never looks up.
    strikes: HashMap<Key, Option<Image>>,
}

/// The process-wide strike cache, locked.
///
/// A poisoned lock means some other glyph's decode panicked; the maps are
/// still sound, and refusing to read them would drop every color glyph for
/// the rest of the process.
fn lock() -> MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The strike serving `glyph` at `font_size`, decoded and placed, or `None`
/// when the face has none for it or its samples cannot be read.
fn resolve(
    strikes: &BitmapStrikes<'_>,
    face: Face,
    font_size: f32,
    glyph_transform: Option<Affine>,
    foreground: Color,
    glyph: Glyph,
) -> Option<Strike> {
    let key = Key {
        blob: face.blob,
        face: face.index,
        glyph: glyph.id,
        size: font_size.to_bits(),
        color: foreground.to_rgba8().to_u32(),
    };
    // The strike's metrics are needed for placement whether or not its
    // pixels are cached, and reading them is a table lookup rather than a
    // decode.
    let bitmap = strikes.glyph_for_size(Size::new(font_size), GlyphId::new(glyph.id))?;

    let image = lock()
        .strikes
        .entry(key)
        .or_insert_with(|| decode(&bitmap, foreground))
        .clone()?;

    let transform = placement(&bitmap, &image, &face, font_size, glyph_transform, glyph);
    Some(Strike { image, transform })
}

/// Where a strike sits relative to the run's origin.
///
/// Skia's arithmetic, which its own comment attributes to CoreText
/// conformance testing, and which vello and the PDF backend both carry.
/// Derived from scratch it comes out subtly wrong for sbix faces.
fn placement(
    bitmap: &BitmapGlyph<'_>,
    image: &Image,
    face: &Face,
    font_size: f32,
    glyph_transform: Option<Affine>,
    glyph: Glyph,
) -> Affine {
    let size = f64::from(font_size);
    let font_units_to_size = size / face.upem;
    let scale = |ppem: f32| {
        if ppem > 0.0 {
            size / f64::from(ppem)
        } else {
            1.0
        }
    };
    // Apple Color Emoji reports a zero y bearing; Skia substitutes 100,
    // but only for sbix, so a face that comes to encode the offset keeps
    // whatever it says.
    let bearing_y = if bitmap.bearing_y == 0.0 && face.sbix {
        100.0
    } else {
        f64::from(bitmap.bearing_y)
    };

    let mut t = Affine::translate((f64::from(glyph.x), f64::from(glyph.y)));
    if let Some(gt) = glyph_transform {
        t *= gt;
    }
    t *= Affine::translate((
        -f64::from(bitmap.bearing_x) * font_units_to_size,
        bearing_y * font_units_to_size,
    )) * Affine::scale_non_uniform(scale(bitmap.ppem_x), scale(bitmap.ppem_y))
        * Affine::translate((
            -f64::from(bitmap.inner_bearing_x),
            -f64::from(bitmap.inner_bearing_y),
        ));
    if bitmap.placement_origin == Origin::BottomLeft {
        t *= Affine::translate((0.0, -f64::from(image.height)));
    }
    t
}

/// A strike's samples as an RGBA8 straight-alpha image.
fn decode(bitmap: &BitmapGlyph<'_>, foreground: Color) -> Option<Image> {
    let n = (bitmap.width as usize).checked_mul(bitmap.height as usize)?;
    let pixels = match &bitmap.data {
        BitmapData::Png(bytes) => return crate::image::decode_png(bytes).ok(),
        BitmapData::Bgra(bytes) => {
            let mut rgba = bytes.get(..n * 4)?.to_vec();
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
                unpremultiply(px);
            }
            rgba
        }
        BitmapData::Mask(mask) => {
            let mut alpha = vec![0u8; n];
            mask.decode_to_slice(bitmap.width, bitmap.height, &mut alpha)
                .ok()?;
            let [r, g, b, a] = foreground.to_rgba8().to_u8_array();
            let mut rgba = Vec::with_capacity(n * 4);
            for v in alpha {
                rgba.extend_from_slice(&[r, g, b, ((u16::from(v) * u16::from(a)) / 255) as u8]);
            }
            rgba
        }
    };
    crate::image::from_rgba8(bitmap.width, bitmap.height, pixels).ok()
}

/// One premultiplied RGBA8 pixel, in place, to straight alpha.
fn unpremultiply(px: &mut [u8]) {
    let a = u32::from(px[3]);
    if a == 0 || a == 255 {
        return;
    }
    for c in &mut px[..3] {
        *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
    }
}
