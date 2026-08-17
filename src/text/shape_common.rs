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
    let rect =
        crate::geometry::Rect::new(x0 as f64, top as f64, x1 as f64, (top + thickness) as f64);
    let path = crate::primitives::rect(rect);
    scene.fill(FillRule::NonZero, transform, brush, None, &path, pick_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::FontFeatureSetting;
    use parley::{FontWidth, PositionedLayoutItem};

    const TEXT: &str = "Ag";

    /// Shape `TEXT` through nothing but [`push_style_defaults`], so every
    /// property observed on the layout came from `style`.
    fn layout_for(style: &TextStyle, dpi: f64) -> parley::Layout<()> {
        let fcx_mutex = super::super::font_context();
        let mut fcx = fcx_mutex.lock().expect("font context poisoned");
        let mut lcx: parley::LayoutContext<()> = parley::LayoutContext::new();
        let mut builder = lcx.ranged_builder(&mut fcx, TEXT, 1.0, true);
        push_style_defaults(&mut builder, style, dpi);
        let mut layout: parley::Layout<()> = builder.build(TEXT);
        layout.break_all_lines(None);
        layout
    }

    fn first_run(layout: &parley::Layout<()>) -> parley::layout::Run<'_, ()> {
        layout
            .lines()
            .flat_map(|line| line.runs())
            .next()
            .expect("shaped at least one run")
    }

    fn first_glyph_run_style(layout: &parley::Layout<()>) -> parley::layout::Style<()> {
        layout
            .lines()
            .flat_map(|line| line.items())
            .find_map(|item| match item {
                PositionedLayoutItem::GlyphRun(gr) => Some(gr.style().clone()),
                _ => None,
            })
            .expect("shaped at least one glyph run")
    }

    #[test]
    fn point_size_converts_to_pixels_at_the_shaping_dpi() {
        let style = TextStyle::new(12.0);
        assert_eq!(first_run(&layout_for(&style, 72.0)).font_size(), 12.0);
        assert_eq!(first_run(&layout_for(&style, 96.0)).font_size(), 16.0);
        assert_eq!(first_run(&layout_for(&style, 144.0)).font_size(), 24.0);
    }

    #[test]
    fn weight_width_and_slant_reach_the_shaper() {
        let style = TextStyle::new(12.0)
            .weight(700)
            .width(0.75)
            .style(FontStyleKind::Italic);
        let layout = layout_for(&style, 96.0);
        let attrs = *first_run(&layout).font_attrs();
        assert_eq!(attrs.weight, parley::FontWeight::new(700.0));
        assert_eq!(attrs.width, FontWidth::from_ratio(0.75));
        assert_eq!(attrs.style, FontStyle::Italic);
    }

    #[test]
    fn oblique_carries_its_slant_angle() {
        let style = TextStyle::new(12.0).style(FontStyleKind::Oblique(14.0));
        let layout = layout_for(&style, 96.0);
        assert_eq!(
            first_run(&layout).font_attrs().style,
            FontStyle::Oblique(Some(14.0))
        );
    }

    #[test]
    fn a_default_style_shapes_upright_at_regular_weight() {
        let layout = layout_for(&TextStyle::new(12.0), 96.0);
        let attrs = *first_run(&layout).font_attrs();
        assert_eq!(attrs.weight, parley::FontWeight::new(400.0));
        assert_eq!(attrs.width, FontWidth::from_ratio(1.0));
        assert_eq!(attrs.style, FontStyle::Normal);
    }

    #[test]
    fn absolute_line_height_is_the_pixel_height_of_a_single_line() {
        let style = TextStyle::new(12.0).line_height(LineHeight::Absolute(30.0));
        let layout = layout_for(&style, 96.0);
        // 30pt at 96dpi = 40px.
        assert!((layout.height() - 40.0).abs() < 1e-3, "{}", layout.height());
    }

    #[test]
    fn relative_line_height_scales_with_the_multiplier() {
        let single = layout_for(
            &TextStyle::new(12.0).line_height(LineHeight::Relative(1.0)),
            96.0,
        )
        .height();
        let doubled = layout_for(
            &TextStyle::new(12.0).line_height(LineHeight::Relative(2.0)),
            96.0,
        )
        .height();
        assert!(
            (doubled - 2.0 * single).abs() < 1e-3,
            "{single} → {doubled}"
        );
    }

    #[test]
    fn letter_spacing_widens_the_run_by_its_pixel_equivalent() {
        let plain = layout_for(&TextStyle::new(12.0), 96.0).width();
        let tracked = layout_for(&TextStyle::new(12.0).letter_spacing_pt(9.0), 96.0).width();
        // 9pt at 96dpi = 12px of extra advance per inter-glyph gap.
        assert!(
            tracked >= plain + 12.0,
            "expected at least one 12px gap: {plain} → {tracked}"
        );
    }

    #[test]
    fn decorations_are_pushed_only_when_the_style_asks_for_them() {
        let plain = first_glyph_run_style(&layout_for(&TextStyle::new(12.0), 96.0));
        assert!(plain.underline.is_none());
        assert!(plain.strikethrough.is_none());

        let underlined =
            first_glyph_run_style(&layout_for(&TextStyle::new(12.0).underline(true), 96.0));
        assert!(underlined.underline.is_some());
        assert!(underlined.strikethrough.is_none());

        let struck =
            first_glyph_run_style(&layout_for(&TextStyle::new(12.0).strikethrough(true), 96.0));
        assert!(struck.strikethrough.is_some());
        assert!(struck.underline.is_none());
    }

    #[test]
    fn an_empty_family_chain_shapes_as_the_generic_sans_serif() {
        let implicit = layout_for(&TextStyle::new(12.0), 96.0);
        let explicit = layout_for(
            &TextStyle::new(12.0).generic_family(GenericFamilyKind::SansSerif),
            96.0,
        );
        assert_eq!(
            first_run(&implicit).font().data.id(),
            first_run(&explicit).font().data.id()
        );
    }

    #[test]
    fn an_unresolvable_named_family_falls_through_to_the_next_chain_entry() {
        // A multi-entry chain is pushed as a list, so a missing face
        // hands the run to the next candidate rather than stranding it.
        let chained = layout_for(
            &TextStyle::new(12.0)
                .families([FontFamilyEntry::Named("NoSuchFaceAnywhere".into())])
                .generic_family(GenericFamilyKind::Mono),
            96.0,
        );
        let mono = layout_for(
            &TextStyle::new(12.0).generic_family(GenericFamilyKind::Mono),
            96.0,
        );
        assert_eq!(
            first_run(&chained).font().data.id(),
            first_run(&mono).font().data.id()
        );
    }

    // ── Translation helpers ────────────────────────────────────────

    #[test]
    fn generic_families_translate_to_their_parley_counterparts() {
        assert_eq!(
            generic_family_to_parley(GenericFamilyKind::Serif),
            GenericFamily::Serif
        );
        assert_eq!(
            generic_family_to_parley(GenericFamilyKind::SansSerif),
            GenericFamily::SansSerif
        );
        assert_eq!(
            generic_family_to_parley(GenericFamilyKind::Mono),
            GenericFamily::Monospace
        );
        assert_eq!(
            generic_family_to_parley(GenericFamilyKind::Cursive),
            GenericFamily::Cursive
        );
        assert_eq!(
            generic_family_to_parley(GenericFamilyKind::Fantasy),
            GenericFamily::Fantasy
        );
        assert_eq!(
            generic_family_to_parley(GenericFamilyKind::SystemUi),
            GenericFamily::SystemUi
        );
    }

    #[test]
    fn generic_family_names_parse_with_their_aliases() {
        assert_eq!(generic_family_from_str("serif"), Some(GenericFamily::Serif));
        assert_eq!(
            generic_family_from_str("SANS-SERIF"),
            Some(GenericFamily::SansSerif)
        );
        assert_eq!(
            generic_family_from_str("sans"),
            Some(GenericFamily::SansSerif)
        );
        assert_eq!(
            generic_family_from_str("mono"),
            Some(GenericFamily::Monospace)
        );
        assert_eq!(
            generic_family_from_str("Monospace"),
            Some(GenericFamily::Monospace)
        );
        assert_eq!(generic_family_from_str("ui"), Some(GenericFamily::SystemUi));
        assert_eq!(
            generic_family_from_str("system-ui"),
            Some(GenericFamily::SystemUi)
        );
        // A concrete family is not a generic one.
        assert_eq!(generic_family_from_str("Helvetica"), None);
    }

    #[test]
    fn feature_settings_keep_their_tag_and_value() {
        let out = parley_features(&[
            FontFeatureSetting {
                tag: *b"liga",
                value: 0,
            },
            FontFeatureSetting {
                tag: *b"ss01",
                value: 1,
            },
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tag, parley::setting::Tag::from_bytes(*b"liga"));
        assert_eq!(out[0].value, 0);
        assert_eq!(out[1].tag, parley::setting::Tag::from_bytes(*b"ss01"));
        assert_eq!(out[1].value, 1);
    }
}
