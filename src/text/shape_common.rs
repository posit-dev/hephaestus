//! Shaping and glyph-emission pieces shared by the plain-text and
//! rich-text paths.
//!
//! Both paths build a parley `RangedBuilder`, push the same base
//! [`TextStyle`] properties onto it, walk the resulting layout's glyph
//! runs into `SceneBuilder::draw_glyphs` calls, and paint underline /
//! strikethrough rules by hand. Only the brush generic and the
//! per-range styling differ, so those pieces live here and are generic
//! over parley's brush parameter.

use parley::{
    FontFamily, FontFamilyName, FontStyle, FontWeight, GenericFamily, GlyphRun, StyleProperty,
};

use super::{FontFamilyEntry, FontStyleKind, GenericFamilyKind, LineHeight, TextStyle};
use crate::brush::Brush;
use crate::geometry::Affine;
use crate::path::FillRule;
use crate::pick::PickId;
use crate::scene::{Glyph, SceneBuilder};

/// Push every property `style` dictates onto `builder` as a default.
///
/// Covers size, weight, width, slant, line height, letter spacing,
/// decorations, the family chain, OpenType features, and variable-font
/// variations. The brush is not pushed — it's brush-type specific and
/// the caller supplies it.
pub(crate) fn push_style_defaults<B: parley::style::Brush>(
    builder: &mut parley::RangedBuilder<'_, B>,
    style: &TextStyle,
    dpi: f64,
) {
    let size_px = (style.size_pt as f64 * dpi / 72.0) as f32;
    builder.push_default(StyleProperty::FontSize(size_px));
    builder.push_default(StyleProperty::FontWeight(FontWeight::new(
        style.weight as f32,
    )));
    builder.push_default(StyleProperty::FontWidth(parley::FontWidth::from_ratio(
        style.width,
    )));
    let parley_style = match style.style {
        FontStyleKind::Normal => FontStyle::Normal,
        FontStyleKind::Italic => FontStyle::Italic,
        FontStyleKind::Oblique(angle) => FontStyle::Oblique(Some(angle)),
    };
    builder.push_default(StyleProperty::FontStyle(parley_style));
    let line_height = match style.line_height {
        LineHeight::Relative(mult) => parley::LineHeight::FontSizeRelative(mult),
        LineHeight::Absolute(pt) => parley::LineHeight::Absolute((pt as f64 * dpi / 72.0) as f32),
    };
    builder.push_default(StyleProperty::LineHeight(line_height));
    if style.letter_spacing_pt != 0.0 {
        let letter_spacing_px = (style.letter_spacing_pt as f64 * dpi / 72.0) as f32;
        builder.push_default(StyleProperty::LetterSpacing(letter_spacing_px));
    }
    if style.underline {
        builder.push_default(StyleProperty::Underline(true));
    }
    if style.strikethrough {
        builder.push_default(StyleProperty::Strikethrough(true));
    }
    // Owned families list — parley borrows from us via `Cow`s, so the
    // source strings must outlive `build()`. Constructing the names
    // eagerly and pushing them keeps the lifetimes local.
    if style.families.is_empty() {
        builder.push_default(StyleProperty::FontFamily(FontFamily::Single(
            FontFamilyName::Generic(GenericFamily::SansSerif),
        )));
    } else {
        let names: Vec<FontFamilyName<'_>> = style
            .families
            .iter()
            .map(|entry| match entry {
                FontFamilyEntry::Named(name) => FontFamilyName::named(name),
                FontFamilyEntry::Generic(kind) => {
                    FontFamilyName::Generic(generic_family_to_parley(*kind))
                }
            })
            .collect();
        builder.push_default(StyleProperty::FontFamily(if names.len() == 1 {
            FontFamily::Single(names[0].clone())
        } else {
            FontFamily::List(std::borrow::Cow::Owned(names))
        }));
    }
    if !style.features.is_empty() {
        builder.push_default(StyleProperty::FontFeatures(parley::FontFeatures::List(
            std::borrow::Cow::Owned(parley_features(&style.features)),
        )));
    }
    if !style.variations.is_empty() {
        let parley_variations: Vec<parley::FontVariation> = style
            .variations
            .iter()
            .map(|v| parley::FontVariation::new(parley::setting::Tag::from_bytes(v.tag), v.value))
            .collect();
        builder.push_default(StyleProperty::FontVariations(parley::FontVariations::List(
            std::borrow::Cow::Owned(parley_variations),
        )));
    }
}

/// Translate our feature settings into parley's.
pub(crate) fn parley_features(features: &[super::FontFeatureSetting]) -> Vec<parley::FontFeature> {
    features
        .iter()
        .map(|f| parley::FontFeature::new(parley::setting::Tag::from_bytes(f.tag), f.value))
        .collect()
}

/// Translate a local [`GenericFamilyKind`] to parley's [`GenericFamily`].
pub(crate) fn generic_family_to_parley(kind: GenericFamilyKind) -> GenericFamily {
    match kind {
        GenericFamilyKind::Serif => GenericFamily::Serif,
        GenericFamilyKind::SansSerif => GenericFamily::SansSerif,
        GenericFamilyKind::Mono => GenericFamily::Monospace,
        GenericFamilyKind::Cursive => GenericFamily::Cursive,
        GenericFamilyKind::Fantasy => GenericFamily::Fantasy,
        GenericFamilyKind::SystemUi => GenericFamily::SystemUi,
    }
}

/// Recognise a CSS generic family name. `None` for a concrete family.
pub(crate) fn generic_family_from_str(s: &str) -> Option<GenericFamily> {
    match s.to_ascii_lowercase().as_str() {
        "serif" => Some(GenericFamily::Serif),
        "sans-serif" | "sans" => Some(GenericFamily::SansSerif),
        "monospace" | "mono" => Some(GenericFamily::Monospace),
        "cursive" => Some(GenericFamily::Cursive),
        "fantasy" => Some(GenericFamily::Fantasy),
        "system-ui" | "systemui" | "ui" => Some(GenericFamily::SystemUi),
        _ => None,
    }
}

/// Collect one parley glyph run's glyphs into scene-space positions,
/// offset by `(dx, dy)`.
///
/// Rich text passes a non-zero `dy` to apply a run's baseline shift.
pub(crate) fn glyphs_of_run<B: parley::style::Brush>(
    glyph_run: &GlyphRun<'_, B>,
    dx: f32,
    dy: f32,
) -> Vec<Glyph> {
    glyph_run
        .positioned_glyphs()
        .map(|g| Glyph {
            id: g.id,
            x: dx + g.x,
            y: dy + g.y,
        })
        .collect()
}

/// One filled axis-aligned rectangle representing an underline or
/// strikethrough decoration. Bundled as a struct to keep
/// [`emit_decoration_rect`] from accumulating positional args.
pub(crate) struct DecorationRect {
    /// Left edge.
    pub x0: f32,
    /// Right edge.
    pub x1: f32,
    /// Top edge.
    pub top: f32,
    /// Rule thickness, measured downward from `top`.
    pub thickness: f32,
}

/// Paint one decoration rule in the same pre-transform coordinate
/// frame as the glyphs, so a rotation applied via `transform` carries
/// the rule with the text.
pub(crate) fn emit_decoration_rect<S: SceneBuilder + ?Sized>(
    scene: &mut S,
    deco: DecorationRect,
    brush: &Brush,
    transform: Affine,
    pick_id: PickId,
) {
    let DecorationRect {
        x0,
        x1,
        top,
        thickness,
    } = deco;
    if !thickness.is_finite() || thickness <= 0.0 || x1 <= x0 {
        return;
    }
    let rect = kurbo::Rect::new(x0 as f64, top as f64, x1 as f64, (top + thickness) as f64);
    let path = crate::primitives::rect(rect);
    scene.fill(FillRule::NonZero, transform, brush, None, &path, pick_id);
}
