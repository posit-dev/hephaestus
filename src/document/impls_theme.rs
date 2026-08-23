//! Codec for the theme tree and the text-style vocabulary.
//!
//! This is the widest section of the document — around 180 leaf fields
//! across 30-odd types — and almost all of it is plain data with public
//! fields, so nearly every impl here is one [`impl_codec`] line.
//! `Theme` derives `Clone` and `PartialEq`, which is itself proof that
//! nothing in the tree is a closure or a trait object.
//!
//! A theme is written in full rather than as a diff against a named
//! builtin. Computing that diff needs the same field-by-field walk as
//! writing the fields does, so it would be more code for a few hundred
//! bytes — every unset slot already costs one byte as a `None` or an
//! `Element::Inherit` tag.
//!
//! The one field that isn't plain data is [`Theme::rich_text`], an
//! `Arc<RichTextStyleSheet>` whose *pointer* is what
//! [`RichShapeCache`](crate::text::rich::RichShapeCache) keys on. It
//! goes through the intern table, so a document with one logical sheet
//! rebuilds one `Arc` — see [`super::intern`].

use super::codec::impl_codec;
#[cfg(feature = "document-read")]
use super::codec::{Decode, Reader};
#[cfg(feature = "document-write")]
use super::codec::{Encode, Writer};
#[cfg(feature = "document-read")]
use super::DocumentError;
use crate::plot::theme::legend::Direction as LegendDirection;
use crate::plot::theme::{
    AlignTo, AxisTheme, BarTheme, Element, FontFamily, FontFeature, FontSpec, FontStyle,
    FontVariation, FontWeight, FontWidth, GeomTheme, KeyTheme, LegendTheme, LineDefaults,
    LineElement, PerAxis, PerChannel, PointDefaults, RectElement, Rotation, ShapeDefaults, Sided,
    TextDefaults, TextElement, TextFitDefaults, Theme, ThemePart, TitleLocation,
};
use crate::text::rich::style::Direction as RichDirection;
use crate::text::rich::{
    FieldSet, LengthSpec, LineHeightSpec, RichMargin, RichTextStyleSheet, StyleDelta, StyleField,
};
use crate::text::{
    FontFamilyEntry, FontFeatureSetting, FontStyleKind, FontVariationSetting, GenericFamilyKind,
    LineHeight, TextStyle,
};

// ─── Cascade wrappers ────────────────────────────────────────────────────────

impl_codec! {
    enum Element<T> {
        0 => Inherit,
        1 => Blank,
        2 => Set(v),
    }

    struct PerChannel<T> { all, by_channel }
    struct Sided<T> { all, by_channel, by_channel_side }
}

// ─── Fonts ───────────────────────────────────────────────────────────────────

impl_codec! {
    enum FontFamily {
        0 => Named(names),
        1 => Serif,
        2 => SansSerif,
        3 => Mono,
        4 => Cursive,
        5 => Fantasy,
        6 => SystemUi,
    }

    newtype FontWeight;

    enum FontWidth {
        0 => UltraCondensed,
        1 => ExtraCondensed,
        2 => Condensed,
        3 => SemiCondensed,
        4 => Normal,
        5 => SemiExpanded,
        6 => Expanded,
        7 => ExtraExpanded,
        8 => UltraExpanded,
    }

    enum FontStyle {
        0 => Normal,
        1 => Italic,
        2 => Oblique(degrees),
    }

    struct FontFeature { tag, value }
    struct FontVariation { tag, value }
    struct FontSpec { family, weight, width, style, features, variations }
}

// ─── Elements ────────────────────────────────────────────────────────────────

impl_codec! {
    enum AlignTo {
        0 => Panel,
        1 => Plot,
    }

    enum Rotation {
        0 => Degrees(deg),
        1 => Along,
        2 => Across,
    }

    struct TextElement {
        font,
        color,
        size_pt,
        align,
        valign,
        angle,
        lineheight,
        tracking,
        underline,
        strikethrough,
        margin,
        text_stroke,
        text_linewidth_pt,
        markdown,
    }

    struct LineElement { color, linewidth_pt, linetype, cap, join }
    struct RectElement { fill, color, linewidth_pt, linetype, corner_radius }
}

// ─── Axis ────────────────────────────────────────────────────────────────────

impl_codec! {
    enum TitleLocation {
        0 => Outside,
        1 => Inside,
    }

    struct AxisTheme {
        title,
        text,
        line,
        ticks,
        ticks_minor,
        tick_length,
        tick_length_minor,
        tick_gap,
        title_gap,
        title_location,
    }

    struct PerAxis { all, by_channel, by_channel_side }
}

// ─── Legend ──────────────────────────────────────────────────────────────────

impl_codec! {
    enum LegendDirection {
        0 => Auto,
        1 => Horizontal,
        2 => Vertical,
    }

    struct KeyTheme { frame, width, height, spacing }
    struct BarTheme { length, width, frame }
    struct LegendTheme { background, title, margin, padding, direction, axis, key, bar }
}

// ─── Geom defaults ───────────────────────────────────────────────────────────

impl_codec! {
    struct PointDefaults { size_pt, shape, fill, stroke, stroke_width_pt }
    struct LineDefaults { stroke, linewidth_pt, cap, join }
    struct ShapeDefaults { fill, stroke, linewidth_pt, cap, join }

    struct TextDefaults {
        size_pt,
        weight,
        fill,
        anchor_x,
        anchor_y,
        bg_fill,
        bg_stroke,
        bg_linewidth_pt,
        tracking,
        underline,
        strikethrough,
        text_stroke,
        text_linewidth_pt,
        markdown,
    }

    struct TextFitDefaults {
        min_font_pt,
        max_font_pt,
        weight,
        fill,
        bg_fill,
        bg_stroke,
        bg_linewidth_pt,
        tracking,
        underline,
        strikethrough,
        text_stroke,
        text_linewidth_pt,
    }

    struct GeomTheme {
        point,
        line,
        segment,
        polygon,
        rect,
        ellipse,
        wedge,
        bspline,
        ribbon,
        ribbon_bspline,
        text,
        text_fit,
        text_path,
        marker_outline_pt,
    }
}

// ─── Text styles ─────────────────────────────────────────────────────────────

impl_codec! {
    enum GenericFamilyKind {
        0 => Serif,
        1 => SansSerif,
        2 => Mono,
        3 => Cursive,
        4 => Fantasy,
        5 => SystemUi,
    }

    enum FontFamilyEntry {
        0 => Named(name),
        1 => Generic(kind),
    }

    enum FontStyleKind {
        0 => Normal,
        1 => Italic,
        2 => Oblique(degrees),
    }

    struct FontFeatureSetting { tag, value }
    struct FontVariationSetting { tag, value }

    enum LineHeight {
        0 => Relative(factor),
        1 => Absolute(pt),
    }

    struct TextStyle {
        size_pt,
        families,
        weight,
        width,
        style,
        line_height,
        tracking,
        underline,
        strikethrough,
        features,
        variations,
    }
}

// ─── Rich text ───────────────────────────────────────────────────────────────

impl_codec! {
    enum LengthSpec {
        0 => Pt(v),
        1 => Relative(v),
        2 => Em(v),
        3 => Rem(v),
    }

    enum LineHeightSpec {
        0 => Mult(v),
        1 => Relative(v),
        2 => Pt(v),
    }

    struct RichMargin { top, right, bottom, left }

    enum RichDirection {
        0 => Auto,
        1 => Ltr,
        2 => Rtl,
    }

    enum StyleField {
        0 => Family,
        1 => Weight,
        2 => Italic,
        3 => Width,
        4 => Size,
        5 => Color,
        6 => Tracking,
        7 => Underline,
        8 => Strikethrough,
        9 => Baseline,
        10 => TextStroke,
        11 => TextStrokeWidth,
        12 => LineHeight,
        13 => Align,
        14 => Indent,
        15 => Hanging,
        16 => Margin,
        17 => Padding,
        18 => Background,
        19 => BorderColor,
        20 => BorderWidth,
        21 => BorderRadius,
        22 => Bullet,
    }

    struct StyleDelta {
        family,
        weight,
        italic,
        width,
        size,
        color,
        tracking,
        underline,
        strikethrough,
        baseline,
        text_stroke,
        text_stroke_width,
        features,
        lineheight,
        align,
        text_direction,
        indent,
        hanging,
        margin,
        padding,
        background,
        border_color,
        border_width,
        border_radius,
        border_type,
        bullet,
        skip_inherit,
    }
}

// `FieldSet` is a `u32` bitset whose bit positions are the declaration
// order of `StyleField`. Writing the raw word would freeze that order
// into the file format; writing the members lets `StyleField` gain a
// variant without reinterpreting documents already written.
#[cfg(feature = "document-write")]
impl Encode for FieldSet {
    fn encode(&self, w: &mut Writer) {
        let members: Vec<StyleField> = StyleField::ALL
            .iter()
            .copied()
            .filter(|f| self.contains(*f))
            .collect();
        members.encode(w);
    }
}

#[cfg(feature = "document-read")]
impl Decode for FieldSet {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        Ok(FieldSet::of(&Vec::<StyleField>::decode(r)?))
    }
}

/// Write a style sheet's contents, for the table chunk that owns them.
///
/// A free function rather than an `Encode` impl: `RichTextStyleSheet`
/// deliberately has none, so that `Arc<RichTextStyleSheet>` can have an
/// interning impl without overlapping a blanket one.
#[cfg(feature = "document-write")]
pub(crate) fn encode_sheet(sheet: &RichTextStyleSheet, w: &mut Writer) {
    let mut entries: Vec<(&str, &StyleDelta)> = sheet.iter().collect();
    entries.sort_unstable_by_key(|(name, _)| *name);
    w.varint(entries.len() as u64);
    for (name, delta) in entries {
        name.encode(w);
        delta.encode(w);
    }
}

/// Read a style sheet's contents. See [`encode_sheet`].
#[cfg(feature = "document-read")]
pub(crate) fn decode_sheet(r: &mut Reader<'_>) -> Result<RichTextStyleSheet, DocumentError> {
    let n = r.count()?;
    let mut sheet = RichTextStyleSheet::empty();
    for _ in 0..n {
        let name = String::decode(r)?;
        sheet.set(name, StyleDelta::decode(r)?);
    }
    Ok(sheet)
}

#[cfg(feature = "document-write")]
impl Encode for std::sync::Arc<RichTextStyleSheet> {
    fn encode(&self, w: &mut Writer) {
        let index = w.tables().sheet(self);
        w.varint(u64::from(index));
    }
}

#[cfg(feature = "document-read")]
impl Decode for std::sync::Arc<RichTextStyleSheet> {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DocumentError> {
        let index = u32::decode(r)?;
        r.tables().sheet(index).ok_or(DocumentError::Invalid {
            what: "style sheet reference",
            why: format!("index {index} is past the end of the style-sheet table"),
        })
    }
}

// ─── Theme ───────────────────────────────────────────────────────────────────

impl_codec! {
    struct Theme {
        palette,
        text,
        line,
        rect,
        plot_title,
        plot_subtitle,
        plot_caption,
        plot_text_align_to,
        plot_background,
        plot_margin,
        plot_padding,
        panel_background,
        panel_border,
        panel_grid_major,
        panel_grid_minor,
        axis,
        legend,
        legend_variants,
        legend_spacing,
        legend_gap,
        strip_background,
        strip_text,
        strip_padding,
        geom,
        locale,
        rich_text,
    }

    struct ThemePart {
        palette,
        text,
        line,
        rect,
        plot_title,
        plot_subtitle,
        plot_caption,
        plot_text_align_to,
        plot_background,
        plot_margin,
        plot_padding,
        panel_background,
        panel_border,
        panel_grid_major,
        panel_grid_minor,
        axis,
        legend,
        legend_variants,
        legend_spacing,
        legend_gap,
        strip_background,
        strip_text,
        strip_padding,
        geom,
        locale,
        rich_text,
    }
}

#[cfg(all(test, feature = "document-read", feature = "document-write"))]
mod tests {
    use super::super::codec::test_support::{assert_roundtrip, roundtrip};
    use super::*;
    use crate::color::rgba;
    use crate::style_vocab::{HAlign, Length, Margin, VAlign};

    /// `Theme` derives `PartialEq` over its whole tree, so equality after
    /// a round trip covers every one of its ~180 leaf fields at once.
    /// Running it across all six builtins exercises very different
    /// combinations of `Element::Set` / `Inherit` / `Blank`.
    #[test]
    fn every_builtin_theme_round_trips_exactly() {
        let themes = [
            ("default", Theme::default()),
            ("dark", Theme::dark()),
            ("minimal", Theme::minimal()),
            ("classic", Theme::classic()),
            ("bw", Theme::bw()),
            ("void", Theme::void()),
        ];
        for (name, theme) in themes {
            let out = roundtrip(&theme);
            assert_eq!(out, theme, "{name} theme did not survive the round trip");
        }
    }

    /// The sheet is interned, so a theme rebuilt from a document holds
    /// one `Arc` per logical sheet. That identity is what
    /// `RichShapeCache` keys on — a fresh `Arc` per label would miss the
    /// cache on every frame.
    #[test]
    fn two_themes_sharing_a_sheet_still_share_it_after_a_round_trip() {
        let base = Theme::default();
        let pair = vec![base.clone(), base.clone()];
        let out = roundtrip(&pair);
        assert!(std::sync::Arc::ptr_eq(&out[0].rich_text, &out[1].rich_text));
    }

    #[test]
    fn a_theme_with_overrides_round_trips() {
        let mut theme = Theme::default();
        theme.palette.accent = rgba(0.9, 0.1, 0.2, 1.0);
        theme.plot_title = Element::Set(TextElement {
            size_pt: Some(Length::Abs(18.0)),
            align: Some(HAlign::Center),
            valign: Some(VAlign::Top),
            angle: Some(Rotation::Degrees(0.0)),
            underline: Some(true),
            markdown: Some(true),
            margin: Some(Margin::all(Length::Abs(4.0))),
            ..TextElement::default()
        });
        theme.panel_border = Element::Blank;
        theme.legend_variants.insert(
            "compact".to_string(),
            LegendTheme {
                direction: LegendDirection::Horizontal,
                ..LegendTheme::default()
            },
        );
        theme.geom.point.shape = std::sync::Arc::from("diamond");
        theme.locale = crate::scales::locale::Locale::DE_DE;

        assert_eq!(roundtrip(&theme), theme);
    }

    #[test]
    fn a_sparse_theme_part_round_trips() {
        let part = ThemePart {
            plot_title: Some(Element::Blank),
            legend_gap: Some(Length::Rel(2.0)),
            ..ThemePart::default()
        };
        assert_eq!(roundtrip(&part), part);
    }

    #[test]
    fn an_empty_theme_part_round_trips() {
        assert_eq!(roundtrip(&ThemePart::default()), ThemePart::default());
    }

    #[test]
    fn the_cascade_wrappers_round_trip_through_every_slot() {
        assert_roundtrip(Element::<LineElement>::Inherit);
        assert_roundtrip(Element::<LineElement>::Blank);
        assert_roundtrip(Element::Set(LineElement {
            linewidth_pt: Some(Length::Abs(2.0)),
            ..LineElement::default()
        }));
        assert_roundtrip(PerChannel::<LineElement>::default());
        assert_roundtrip(Sided::<RectElement>::default());
    }

    #[test]
    fn font_descriptors_round_trip() {
        assert_roundtrip(FontSpec {
            family: Some(FontFamily::Named(vec!["Inter".into(), "Helvetica".into()])),
            weight: Some(FontWeight(600)),
            width: Some(FontWidth::SemiCondensed),
            style: Some(FontStyle::Oblique(12.5)),
            features: vec![FontFeature {
                tag: *b"liga",
                value: 1,
            }],
            variations: vec![FontVariation {
                tag: *b"wght",
                value: 600.0,
            }],
        });
        for f in [
            FontFamily::Serif,
            FontFamily::SansSerif,
            FontFamily::Mono,
            FontFamily::Cursive,
            FontFamily::Fantasy,
            FontFamily::SystemUi,
        ] {
            assert_roundtrip(f);
        }
    }

    #[test]
    fn text_styles_round_trip() {
        assert_roundtrip(TextStyle {
            size_pt: 11.5,
            families: vec![
                FontFamilyEntry::Named("Inter".into()),
                FontFamilyEntry::Generic(GenericFamilyKind::SansSerif),
            ],
            weight: 700,
            width: 0.875,
            style: FontStyleKind::Italic,
            line_height: LineHeight::Absolute(14.0),
            tracking: 0.25,
            underline: true,
            strikethrough: false,
            features: vec![FontFeatureSetting {
                tag: *b"tnum",
                value: 1,
            }],
            variations: vec![FontVariationSetting {
                tag: *b"slnt",
                value: -8.0,
            }],
        });
    }

    // ── Rich text ──

    #[test]
    fn a_field_set_round_trips_its_members() {
        let set = FieldSet::of(&[StyleField::Size, StyleField::Baseline, StyleField::Bullet]);
        let out = roundtrip(&set);
        assert_eq!(out, set);
        assert!(out.contains(StyleField::Size));
        assert!(out.contains(StyleField::Baseline));
        assert!(out.contains(StyleField::Bullet));
        assert!(!out.contains(StyleField::Weight));
    }

    #[test]
    fn an_empty_field_set_round_trips() {
        assert!(roundtrip(&FieldSet::NONE).is_empty());
    }

    /// A `FieldSet` holding every field, to catch a `StyleField` tag
    /// that collides with another.
    #[test]
    fn a_full_field_set_round_trips() {
        let set = FieldSet::of(&StyleField::ALL);
        let out = roundtrip(&set);
        for f in StyleField::ALL {
            assert!(out.contains(f), "{f:?} was lost");
        }
    }

    #[test]
    fn every_style_field_gets_a_distinct_tag() {
        let mut seen: Vec<Vec<u8>> = StyleField::ALL
            .iter()
            .map(|f| {
                let mut w = Writer::new();
                f.encode(&mut w);
                w.finish()
            })
            .collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), StyleField::ALL.len());
    }

    #[test]
    fn a_style_delta_round_trips_every_kind_of_field() {
        let delta = StyleDelta {
            family: Some("Inter".to_string()),
            weight: Some(700),
            italic: Some(true),
            width: Some(0.9),
            size: Some(LengthSpec::Em(1.5)),
            color: Some(crate::style_vocab::ThemeColor::Accent),
            tracking: Some(20.0),
            underline: Some(false),
            strikethrough: Some(true),
            baseline: Some(LengthSpec::Rem(0.3)),
            text_stroke: Some(crate::style_vocab::ThemeColor::Ink),
            text_stroke_width: Some(LengthSpec::Pt(0.5)),
            features: Some(vec![FontFeatureSetting {
                tag: *b"kern",
                value: 1,
            }]),
            lineheight: Some(LineHeightSpec::Mult(1.4)),
            align: Some(HAlign::Justify),
            text_direction: Some(RichDirection::Rtl),
            indent: Some(LengthSpec::Pt(12.0)),
            hanging: Some(LengthSpec::Relative(0.5)),
            margin: Some(RichMargin {
                top: LengthSpec::Pt(1.0),
                right: LengthSpec::Em(2.0),
                bottom: LengthSpec::Rem(3.0),
                left: LengthSpec::Relative(4.0),
            }),
            padding: Some(RichMargin {
                top: LengthSpec::Pt(0.0),
                right: LengthSpec::Pt(0.0),
                bottom: LengthSpec::Pt(0.0),
                left: LengthSpec::Pt(0.0),
            }),
            background: Some(crate::style_vocab::ThemeColor::Paper),
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin {
                top: LengthSpec::Pt(1.0),
                right: LengthSpec::Pt(1.0),
                bottom: LengthSpec::Pt(1.0),
                left: LengthSpec::Pt(1.0),
            }),
            border_radius: Some(LengthSpec::Pt(3.0)),
            border_type: Some(crate::linetype::dashed()),
            bullet: Some(vec!["•".to_string(), "◦".to_string()]),
            skip_inherit: FieldSet::of(&[StyleField::Size]),
        };
        assert_eq!(roundtrip(&delta), delta);
    }

    #[test]
    fn an_empty_style_delta_round_trips() {
        assert_eq!(roundtrip(&StyleDelta::empty()), StyleDelta::empty());
    }

    /// The marquee-parity sheet has an entry for every reserved
    /// selector, so it exercises far more of `StyleDelta` than a
    /// hand-built one would.
    #[test]
    fn the_default_style_sheet_round_trips_selector_for_selector() {
        let sheet = RichTextStyleSheet::new();
        let mut w = Writer::new();
        super::encode_sheet(&sheet, &mut w);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        let out = super::decode_sheet(&mut r).expect("a well-formed sheet");
        assert!(r.is_empty());
        assert_eq!(out, sheet);
    }

    #[test]
    fn an_empty_style_sheet_round_trips() {
        let sheet = RichTextStyleSheet::empty();
        let mut w = Writer::new();
        super::encode_sheet(&sheet, &mut w);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(super::decode_sheet(&mut r).expect("an empty sheet"), sheet);
    }

    #[test]
    fn a_sheet_index_past_the_table_is_rejected() {
        let mut w = Writer::new();
        w.varint(4);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert!(matches!(
            std::sync::Arc::<RichTextStyleSheet>::decode(&mut r),
            Err(DocumentError::Invalid {
                what: "style sheet reference",
                ..
            })
        ));
    }
}
