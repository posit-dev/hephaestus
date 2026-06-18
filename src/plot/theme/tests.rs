//! Unit tests for the theme module's resolution semantics.

#![cfg(test)]

use super::*;
use crate::color::rgb;

#[test]
fn theme_color_resolves_palette_anchors() {
    let p = Palette {
        paper: rgb(1.0, 0.0, 0.0),
        ink: rgb(0.0, 1.0, 0.0),
        accent: rgb(0.0, 0.0, 1.0),
    };
    assert_eq!(ThemeColor::Paper.resolve(&p), p.paper);
    assert_eq!(ThemeColor::Ink.resolve(&p), p.ink);
    assert_eq!(ThemeColor::Accent.resolve(&p), p.accent);
}

#[test]
fn theme_color_mix_is_linear_in_t() {
    let p = Palette {
        paper: rgb(0.0, 0.0, 0.0),
        ink: rgb(1.0, 1.0, 1.0),
        accent: rgb(0.0, 0.0, 0.0),
    };
    let mid = ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.5).resolve(&p);
    let [r, g, b, _] = mid.components;
    assert!((r - 0.5).abs() < 1e-6);
    assert!((g - 0.5).abs() < 1e-6);
    assert!((b - 0.5).abs() < 1e-6);
}

#[test]
fn theme_invert_swaps_paper_and_ink() {
    let t = Theme::default();
    let paper = t.palette.paper;
    let ink = t.palette.ink;
    let inv = t.invert();
    assert_eq!(inv.palette.paper, ink);
    assert_eq!(inv.palette.ink, paper);
}

#[test]
fn length_resolve_handles_abs_and_rel() {
    assert_eq!(Length::Abs(8.0).resolve(11.0), 8.0);
    assert_eq!(Length::Rel(1.5).resolve(11.0), 16.5);
    assert_eq!(Length::Rel(0.5).resolve(20.0), 10.0);
}

#[test]
fn element_cascade_walks_inherit_to_parent() {
    let parent = TextElement::default();
    let leaf: Element<TextElement> = Element::Inherit;
    assert!(std::ptr::eq(leaf.cascade(Some(&parent)).unwrap(), &parent));
}

#[test]
fn element_cascade_blank_shortcircuits_to_none() {
    let parent = TextElement::default();
    let leaf: Element<TextElement> = Element::Blank;
    assert!(leaf.cascade(Some(&parent)).is_none());
}

#[test]
fn element_cascade_set_wins_over_parent() {
    let parent = TextElement::default();
    let mine = TextElement::default();
    let leaf: Element<TextElement> = Element::Set(mine.clone());
    let resolved = leaf.cascade(Some(&parent)).unwrap();
    // Set wins — the returned reference is to the leaf's value,
    // not the parent.
    assert!(!std::ptr::eq(resolved, &parent));
}

#[test]
fn per_channel_resolves_by_channel_then_root() {
    let mut pc = PerChannel::new(LineElement {
        linewidth_pt: Some(Length::Abs(1.0)),
        ..LineElement::default()
    });
    let override_line = LineElement {
        linewidth_pt: Some(Length::Abs(2.0)),
        ..LineElement::default()
    };
    pc.by_channel[1] = Element::Set(override_line.clone());

    let ch0 = pc.resolve(0).unwrap();
    let ch1 = pc.resolve(1).unwrap();
    assert_eq!(ch0.linewidth_pt, Some(Length::Abs(1.0))); // from root
    assert_eq!(ch1.linewidth_pt, Some(Length::Abs(2.0))); // from override
}

#[test]
fn per_channel_blank_overrides_root() {
    let mut pc = PerChannel::new(LineElement {
        linewidth_pt: Some(Length::Abs(1.0)),
        ..LineElement::default()
    });
    pc.by_channel[0] = Element::Blank;

    assert!(pc.resolve(0).is_none()); // Blank short-circuits
    assert!(pc.resolve(1).is_some()); // other channel still uses root
}

#[test]
fn sided_resolves_most_specific_first() {
    let mut s = Sided::new(RectElement {
        linewidth_pt: Some(Length::Abs(1.0)),
        ..RectElement::default()
    });
    let ch_override = RectElement {
        linewidth_pt: Some(Length::Abs(2.0)),
        ..RectElement::default()
    };
    let side_override = RectElement {
        linewidth_pt: Some(Length::Abs(3.0)),
        ..RectElement::default()
    };
    s.by_channel[0] = Element::Set(ch_override);
    s.by_channel_side[0][1] = Element::Set(side_override);

    let r00 = s.resolve(0, 0).unwrap();
    let r01 = s.resolve(0, 1).unwrap();
    let r10 = s.resolve(1, 0).unwrap();

    assert_eq!(r00.linewidth_pt, Some(Length::Abs(2.0))); // from by_channel
    assert_eq!(r01.linewidth_pt, Some(Length::Abs(3.0))); // from by_channel_side
    assert_eq!(r10.linewidth_pt, Some(Length::Abs(1.0))); // from root
}

#[test]
fn per_axis_resolves_per_field() {
    let mut axis = PerAxis::new(axis_concrete_defaults());
    axis.by_channel[0].tick_length = Some(Length::Abs(8.0));
    axis.by_channel_side[1][1].tick_gap = Some(Length::Abs(5.0));

    let r00 = axis.resolve(0, 0);
    let r11 = axis.resolve(1, 1);

    assert_eq!(r00.tick_length, Length::Abs(8.0)); // channel-0 override
    assert_eq!(r00.tick_gap, axis.all.tick_gap.unwrap()); // unset — falls to root
    assert_eq!(r11.tick_gap, Length::Abs(5.0)); // (ch1, side1) override
    assert_eq!(r11.tick_length, axis.all.tick_length.unwrap()); // unset — root
}

#[test]
fn theme_part_merge_replaces_set_fields() {
    let base = Theme::default();
    let part = ThemePart {
        legend_spacing: Some(Length::Abs(20.0)),
        ..ThemePart::default()
    };

    let merged = base.merge(&part);
    assert_eq!(merged.legend_spacing, Length::Abs(20.0));
    // Other fields untouched.
    assert_eq!(merged.palette, base.palette);
}

#[test]
fn theme_part_merges_legend_variants_additively() {
    let base = Theme::default().with_legend_variant("a", LegendTheme::default());
    let mut variants = std::collections::HashMap::new();
    variants.insert("b".to_string(), LegendTheme::default());
    let part = ThemePart {
        legend_variants: variants,
        ..ThemePart::default()
    };

    let merged = base.merge(&part);
    assert!(merged.legend_variants.contains_key("a"));
    assert!(merged.legend_variants.contains_key("b"));
}

#[test]
fn font_spec_cascade_per_field() {
    let parent = FontSpec {
        family: Some(FontFamily::SansSerif),
        weight: Some(FontWeight::REGULAR),
        ..FontSpec::default()
    };
    let child = FontSpec {
        weight: Some(FontWeight::BOLD),
        ..FontSpec::default()
    };
    let merged = parent.cascade(&child);
    // Family inherits from parent (child didn't set it).
    assert!(matches!(merged.family, Some(FontFamily::SansSerif)));
    // Weight overridden by child.
    assert_eq!(merged.weight, Some(FontWeight::BOLD));
}

#[test]
fn rotation_along_across_resolve_against_baseline() {
    // Cartesian Bottom axis baseline points east (0°): Along = 0°,
    // Across = 90°.
    assert_eq!(Rotation::Along.resolve(0.0), 0.0);
    assert_eq!(Rotation::Across.resolve(0.0), 90.0);
    // Cartesian Left axis baseline points north (90°): Along = 90°
    // (text reads up the axis), Across = 180°.
    assert_eq!(Rotation::Along.resolve(90.0), 90.0);
    assert_eq!(Rotation::Across.resolve(90.0), 180.0);
    // Polar angular tick at 45° follows tangent 45°: Along = 45°,
    // Across = 135° (radial-outward direction).
    assert_eq!(Rotation::Along.resolve(45.0), 45.0);
    assert_eq!(Rotation::Across.resolve(45.0), 135.0);
    // Absolute Degrees pass through regardless of baseline.
    assert_eq!(Rotation::Degrees(30.0).resolve(0.0), 30.0);
    assert_eq!(Rotation::Degrees(30.0).resolve(90.0), 30.0);
}

#[test]
fn direction_auto_resolves_from_side() {
    use crate::scales::chrome::{Anchor, LegendSide};
    // Auto → Horizontal for Top / Bottom; Vertical for Left / Right
    // and InPanel.
    assert_eq!(
        Direction::Auto.resolve(LegendSide::Top),
        ResolvedDirection::Horizontal
    );
    assert_eq!(
        Direction::Auto.resolve(LegendSide::Bottom),
        ResolvedDirection::Horizontal
    );
    assert_eq!(
        Direction::Auto.resolve(LegendSide::Left),
        ResolvedDirection::Vertical
    );
    assert_eq!(
        Direction::Auto.resolve(LegendSide::Right),
        ResolvedDirection::Vertical
    );
    assert_eq!(
        Direction::Auto.resolve(LegendSide::InPanel {
            anchor: Anchor::TopRight,
            inset_pt: 4.0
        }),
        ResolvedDirection::Vertical
    );
    // Explicit variants pass through regardless of side.
    assert_eq!(
        Direction::Horizontal.resolve(LegendSide::Right),
        ResolvedDirection::Horizontal
    );
    assert_eq!(
        Direction::Vertical.resolve(LegendSide::Top),
        ResolvedDirection::Vertical
    );
}

#[cfg(feature = "text")]
#[test]
fn themed_grid_dash_pattern_reaches_stroke() {
    // theme.panel_grid_major with a dashed linetype produces a
    // non-empty `Stroke::dash_pattern` on the chrome-built stroke,
    // proving linetype actually wires through to the renderer.
    use crate::plot::chrome::linear_axis::stroke_from_line_element;
    use crate::scales::value::LinetypeStep;
    use std::sync::Arc;

    let element = LineElement {
        linewidth_pt: Some(Length::Abs(1.0)),
        linetype: Some(Arc::from([LinetypeStep::Dash(4.0), LinetypeStep::Gap(2.0)])),
        ..LineElement::default()
    };
    let stroke = stroke_from_line_element(&element, 96.0);
    assert!(
        !stroke.dash_pattern.is_empty(),
        "dashed LineElement must produce a non-empty stroke dash pattern"
    );
    assert_eq!(stroke.dash_pattern.len(), 2);
}

#[test]
fn font_features_merge_by_tag() {
    let parent = FontSpec {
        features: vec![FontFeature::new(*b"liga", 1), FontFeature::new(*b"kern", 1)],
        ..FontSpec::default()
    };
    let child = FontSpec {
        features: vec![
            FontFeature::new(*b"kern", 0), // override
            FontFeature::new(*b"ss01", 1), // new
        ],
        ..FontSpec::default()
    };
    let merged = parent.cascade(&child);
    let kern = merged.features.iter().find(|f| f.tag == *b"kern").unwrap();
    assert_eq!(kern.value, 0); // child wins for same tag
    assert!(merged.features.iter().any(|f| f.tag == *b"liga")); // parent kept
    assert!(merged.features.iter().any(|f| f.tag == *b"ss01")); // child added
}
