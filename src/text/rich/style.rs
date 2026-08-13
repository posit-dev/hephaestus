//! Style deltas + the style sheet that maps class names to them.
//!
//! A [`StyleDelta`] is a sparse overlay on top of a base [`TextStyle`].
//! Every field is `Option<...>`; `None` = inherit. The overlay
//! semantics mirror `TextElement::cascade` — `child`'s `Some` wins,
//! `None` falls through to the parent.
//!
//! A [`RichTextStyleSheet`] is a lookup table from class names (plus
//! reserved names for markdown elements like `em`, `strong`, `code`,
//! `h1`..`h6`) to `StyleDelta`. [`RichTextStyleSheet::new`] ships
//! sensible defaults; [`RichTextStyleSheet::empty`] gives you a blank
//! slate.

use std::collections::HashMap;

use crate::plot::theme::{HAlign, Length, Margin, ThemeColor};

// ─── StyleDelta ──────────────────────────────────────────────────────────────

/// Sparse overlay on a base style. Every field is `Option<>` and
/// composes by overlay — `child`'s `Some` wins, `None` falls through
/// to the parent. Vector-valued fields (`features`) merge additively
/// by tag; a child entry with the same tag as a parent's replaces it.
///
/// Deltas cover both glyph-level styling (family / weight / italic /
/// … / colour / baseline shift) and block-level box properties
/// (margins, padding, backgrounds, borders, indent, bullets). Block
/// fields are ignored on inline spans and honoured on block-level
/// selectors (`paragraph`, `list_item`, `block_quote`, `h1`..`h6`,
/// `code_block`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleDelta {
    // ── Glyph-level ──
    /// Font family. Overrides `families` from the base style entirely.
    pub family: Option<String>,
    /// CSS-style font weight (100..=900).
    pub weight: Option<u16>,
    /// Italic on/off.
    pub italic: Option<bool>,
    /// CSS `font-width` ratio (1.0 = normal, 0.5 = ultra-condensed).
    pub width: Option<f32>,
    /// Font size. `Length::Abs(pt)` = absolute; `Rel(m)` = `m × parent`.
    pub size: Option<Length>,
    /// Text colour. Resolved through the theme palette at draw time.
    pub color: Option<ThemeColor>,
    /// Letter spacing (tracking) in pt.
    pub tracking_pt: Option<f32>,
    /// Underline decoration.
    pub underline: Option<bool>,
    /// Strikethrough decoration.
    pub strikethrough: Option<bool>,
    /// Baseline shift in em (positive = up; negative = down). Applied
    /// as a per-glyph-run y-offset at draw time — parley doesn't model
    /// this natively.
    pub baseline_em: Option<f32>,

    // ── Block-level ──
    /// Line height. `Rel(m)` = `m × parent size`; `Abs(pt)` = absolute.
    pub lineheight: Option<Length>,
    /// Horizontal alignment within the block's width.
    pub align: Option<HAlign>,
    /// First-line indent (pt).
    pub indent: Option<Length>,
    /// Continuation-line indent (pt) — hanging indent for lists.
    pub hanging: Option<Length>,
    /// Trbl margin (outside the box).
    pub margin: Option<Margin>,
    /// Trbl padding (inside the box, before content).
    pub padding: Option<Margin>,
    /// Block background colour.
    pub background: Option<ThemeColor>,
    /// Block border colour.
    pub border_color: Option<ThemeColor>,
    /// Block border thickness (pt).
    pub border_width: Option<Length>,
    /// Block border corner radius (pt).
    pub border_radius: Option<Length>,
    /// Bullet character(s) for list items. `None` inherits; empty
    /// string suppresses the bullet.
    pub bullet: Option<String>,
}

impl StyleDelta {
    /// Empty (identity) delta — every field `None`, changes nothing.
    #[inline]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Overlay `over` on `self`. `over`'s `Some` fields win; `None`
    /// falls through to `self`.
    pub fn overlay(&self, over: &StyleDelta) -> StyleDelta {
        StyleDelta {
            family: over.family.clone().or_else(|| self.family.clone()),
            weight: over.weight.or(self.weight),
            italic: over.italic.or(self.italic),
            width: over.width.or(self.width),
            size: over.size.or(self.size),
            color: over.color.clone().or_else(|| self.color.clone()),
            tracking_pt: over.tracking_pt.or(self.tracking_pt),
            underline: over.underline.or(self.underline),
            strikethrough: over.strikethrough.or(self.strikethrough),
            baseline_em: over.baseline_em.or(self.baseline_em),
            lineheight: over.lineheight.or(self.lineheight),
            align: over.align.or(self.align),
            indent: over.indent.or(self.indent),
            hanging: over.hanging.or(self.hanging),
            margin: over.margin.or(self.margin),
            padding: over.padding.or(self.padding),
            background: over.background.clone().or_else(|| self.background.clone()),
            border_color: over
                .border_color
                .clone()
                .or_else(|| self.border_color.clone()),
            border_width: over.border_width.or(self.border_width),
            border_radius: over.border_radius.or(self.border_radius),
            bullet: over.bullet.clone().or_else(|| self.bullet.clone()),
        }
    }
}

// ─── RichTextStyleSheet ─────────────────────────────────────────────────────

/// Lookup table from selector names to [`StyleDelta`]. Constructed
/// via [`RichTextStyleSheet::new`] (with marquee-parity defaults) or
/// [`RichTextStyleSheet::empty`] (blank slate); extend with
/// [`RichTextStyleSheet::set`].
///
/// Reserved names populated by `new()`:
/// - Inline markdown: `em`, `strong`, `del`, `code`, `sup`, `sub`,
///   `link`.
/// - Block markdown: `paragraph`, `h1`..`h6`, `block_quote`,
///   `list_item`, `code_block`, `hr`.
///
/// Custom class names (from `{.warning …}` spans, `:::note …:::`
/// divs, etc.) are user-supplied via `set`. On a class-selector
/// lookup, `.name` first tries the sheet, then falls back to a CSS
/// colour keyword (see [`css_color`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RichTextStyleSheet {
    entries: HashMap<String, StyleDelta>,
}

impl Default for RichTextStyleSheet {
    fn default() -> Self {
        Self::new()
    }
}

impl RichTextStyleSheet {
    /// Empty sheet — no defaults. Use for full manual control.
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Sheet with marquee-parity defaults for every reserved selector.
    /// See the module docstring for the complete list.
    pub fn new() -> Self {
        let mut s = Self::empty();
        s.set(
            "em",
            StyleDelta {
                italic: Some(true),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "strong",
            StyleDelta {
                weight: Some(700),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "del",
            StyleDelta {
                strikethrough: Some(true),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "code",
            StyleDelta {
                family: Some("monospace".to_string()),
                background: Some(ThemeColor::alpha(ThemeColor::Accent, 0.12)),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "sup",
            StyleDelta {
                size: Some(Length::Rel(0.75)),
                baseline_em: Some(0.4),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "sub",
            StyleDelta {
                size: Some(Length::Rel(0.75)),
                baseline_em: Some(-0.15),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "link",
            StyleDelta {
                underline: Some(true),
                color: Some(ThemeColor::Accent),
                ..StyleDelta::empty()
            },
        );
        // Heading defaults — block-level entries. Sizes are relative
        // to the base text size; weight is bold.
        for (name, mult) in [
            ("h1", 2.0),
            ("h2", 1.5),
            ("h3", 1.25),
            ("h4", 1.1),
            ("h5", 1.0),
            ("h6", 0.9),
        ] {
            s.set(
                name,
                StyleDelta {
                    size: Some(Length::Rel(mult)),
                    weight: Some(700),
                    margin: Some(Margin::new(
                        Length::Rel(0.5),
                        Length::Abs(0.0),
                        Length::Rel(0.3),
                        Length::Abs(0.0),
                    )),
                    ..StyleDelta::empty()
                },
            );
        }
        s.set(
            "paragraph",
            StyleDelta {
                margin: Some(Margin::new(
                    Length::Abs(0.0),
                    Length::Abs(0.0),
                    Length::Rel(0.5),
                    Length::Abs(0.0),
                )),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "block_quote",
            StyleDelta {
                padding: Some(Margin::new(
                    Length::Rel(0.25),
                    Length::Abs(0.0),
                    Length::Rel(0.25),
                    Length::Rel(1.0),
                )),
                border_color: Some(ThemeColor::alpha(ThemeColor::Accent, 0.4)),
                border_width: Some(Length::Abs(3.0)),
                margin: Some(Margin::new(
                    Length::Rel(0.5),
                    Length::Abs(0.0),
                    Length::Rel(0.5),
                    Length::Abs(0.0),
                )),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "list_item",
            StyleDelta {
                hanging: Some(Length::Rel(1.5)),
                bullet: Some("•".to_string()),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "code_block",
            StyleDelta {
                family: Some("monospace".to_string()),
                background: Some(ThemeColor::alpha(ThemeColor::Accent, 0.08)),
                padding: Some(Margin::new(
                    Length::Rel(0.5),
                    Length::Rel(0.75),
                    Length::Rel(0.5),
                    Length::Rel(0.75),
                )),
                margin: Some(Margin::new(
                    Length::Rel(0.5),
                    Length::Abs(0.0),
                    Length::Rel(0.5),
                    Length::Abs(0.0),
                )),
                ..StyleDelta::empty()
            },
        );
        s.set(
            "hr",
            StyleDelta {
                border_color: Some(ThemeColor::Ink),
                border_width: Some(Length::Abs(1.0)),
                margin: Some(Margin::new(
                    Length::Rel(0.5),
                    Length::Abs(0.0),
                    Length::Rel(0.5),
                    Length::Abs(0.0),
                )),
                ..StyleDelta::empty()
            },
        );
        s
    }

    /// Add or replace the delta for `name`. Returns `&mut Self` so
    /// `.set(...).set(...)` chains.
    pub fn set(&mut self, name: impl Into<String>, delta: StyleDelta) -> &mut Self {
        self.entries.insert(name.into(), delta);
        self
    }

    /// Look up a selector by name. Returns `None` if the name isn't
    /// in the sheet. Callers of `.name`-style lookups should fall
    /// back to [`css_color`] on `None`.
    pub fn get(&self, name: &str) -> Option<&StyleDelta> {
        self.entries.get(name)
    }
}

// ─── CSS colour-name lookup ─────────────────────────────────────────────────

/// Resolve `name` as a CSS colour keyword and return the RGB triple.
/// Case-insensitive. Covers the common CSS colours that authors
/// reach for in rich-text spans (`{.red …}`, `{.steelblue …}`, etc.).
/// The table is intentionally short — a curated subset of the CSS 4
/// named-colour list — so we're not shipping 150 entries by rote.
///
/// Returns `None` for unknown names.
pub fn css_color(name: &str) -> Option<[u8; 3]> {
    let n = name.to_ascii_lowercase();
    match n.as_str() {
        // Neutrals
        "black" => Some([0, 0, 0]),
        "white" => Some([255, 255, 255]),
        "gray" | "grey" => Some([128, 128, 128]),
        "lightgray" | "lightgrey" => Some([211, 211, 211]),
        "darkgray" | "darkgrey" => Some([169, 169, 169]),
        "silver" => Some([192, 192, 192]),

        // Reds / warms
        "red" => Some([255, 0, 0]),
        "crimson" => Some([220, 20, 60]),
        "firebrick" => Some([178, 34, 34]),
        "salmon" => Some([250, 128, 114]),
        "coral" => Some([255, 127, 80]),
        "tomato" => Some([255, 99, 71]),
        "darkred" => Some([139, 0, 0]),
        "maroon" => Some([128, 0, 0]),
        "pink" => Some([255, 192, 203]),
        "hotpink" => Some([255, 105, 180]),
        "orange" => Some([255, 165, 0]),
        "darkorange" => Some([255, 140, 0]),
        "gold" => Some([255, 215, 0]),
        "yellow" => Some([255, 255, 0]),
        "khaki" => Some([240, 230, 140]),

        // Greens
        "green" => Some([0, 128, 0]),
        "darkgreen" => Some([0, 100, 0]),
        "lime" => Some([0, 255, 0]),
        "limegreen" => Some([50, 205, 50]),
        "forestgreen" => Some([34, 139, 34]),
        "seagreen" => Some([46, 139, 87]),
        "olive" => Some([128, 128, 0]),
        "olivedrab" => Some([107, 142, 35]),
        "teal" => Some([0, 128, 128]),
        "aquamarine" => Some([127, 255, 212]),

        // Blues
        "blue" => Some([0, 0, 255]),
        "navy" => Some([0, 0, 128]),
        "royalblue" => Some([65, 105, 225]),
        "steelblue" => Some([70, 130, 180]),
        "dodgerblue" => Some([30, 144, 255]),
        "skyblue" => Some([135, 206, 235]),
        "deepskyblue" => Some([0, 191, 255]),
        "cornflowerblue" => Some([100, 149, 237]),
        "cyan" | "aqua" => Some([0, 255, 255]),
        "turquoise" => Some([64, 224, 208]),
        "darkblue" => Some([0, 0, 139]),

        // Purples / pinks
        "purple" => Some([128, 0, 128]),
        "magenta" | "fuchsia" => Some([255, 0, 255]),
        "violet" => Some([238, 130, 238]),
        "indigo" => Some([75, 0, 130]),
        "orchid" => Some([218, 112, 214]),
        "plum" => Some([221, 160, 221]),

        // Browns / earth
        "brown" => Some([165, 42, 42]),
        "chocolate" => Some([210, 105, 30]),
        "sienna" => Some([160, 82, 45]),
        "tan" => Some([210, 180, 140]),
        "wheat" => Some([245, 222, 179]),
        "beige" => Some([245, 245, 220]),
        "ivory" => Some([255, 255, 240]),

        _ => None,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_prefers_child_some_over_parent_some() {
        let parent = StyleDelta {
            weight: Some(400),
            italic: Some(false),
            ..StyleDelta::empty()
        };
        let child = StyleDelta {
            weight: Some(700),
            ..StyleDelta::empty()
        };
        let merged = parent.overlay(&child);
        assert_eq!(merged.weight, Some(700), "child weight wins");
        assert_eq!(merged.italic, Some(false), "parent italic falls through");
    }

    #[test]
    fn overlay_none_child_falls_through() {
        let parent = StyleDelta {
            color: Some(ThemeColor::Ink),
            ..StyleDelta::empty()
        };
        let child = StyleDelta::empty();
        let merged = parent.overlay(&child);
        assert_eq!(merged.color, Some(ThemeColor::Ink));
    }

    #[test]
    fn default_sheet_populates_reserved_selectors() {
        let sheet = RichTextStyleSheet::new();
        for name in [
            "em",
            "strong",
            "del",
            "code",
            "code_block",
            "sup",
            "sub",
            "link",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "paragraph",
            "block_quote",
            "list_item",
            "hr",
        ] {
            assert!(sheet.get(name).is_some(), "missing sheet entry: {name}");
        }
    }

    #[test]
    fn strong_delta_has_bold_weight() {
        let sheet = RichTextStyleSheet::new();
        assert_eq!(sheet.get("strong").unwrap().weight, Some(700));
    }

    #[test]
    fn em_delta_toggles_italic() {
        let sheet = RichTextStyleSheet::new();
        assert_eq!(sheet.get("em").unwrap().italic, Some(true));
    }

    #[test]
    fn sup_delta_has_size_and_baseline_shift() {
        let sheet = RichTextStyleSheet::new();
        let sup = sheet.get("sup").unwrap();
        assert_eq!(sup.size, Some(Length::Rel(0.75)));
        assert_eq!(sup.baseline_em, Some(0.4));
    }

    #[test]
    fn sub_delta_has_negative_baseline_shift() {
        let sheet = RichTextStyleSheet::new();
        assert_eq!(sheet.get("sub").unwrap().baseline_em, Some(-0.15));
    }

    #[test]
    fn heading_sizes_ascend_h6_to_h1() {
        let sheet = RichTextStyleSheet::new();
        let s = |name: &str| match sheet.get(name).unwrap().size {
            Some(Length::Rel(m)) => m,
            _ => panic!("expected Rel size"),
        };
        assert!(s("h1") > s("h2"));
        assert!(s("h2") > s("h3"));
        assert!(s("h3") > s("h4"));
    }

    #[test]
    fn css_color_lookup_case_insensitive() {
        assert_eq!(css_color("red"), Some([255, 0, 0]));
        assert_eq!(css_color("Red"), Some([255, 0, 0]));
        assert_eq!(css_color("STEELBLUE"), Some([70, 130, 180]));
        assert_eq!(css_color("not-a-colour"), None);
    }

    #[test]
    fn empty_sheet_has_no_entries() {
        let s = RichTextStyleSheet::empty();
        assert!(s.get("strong").is_none());
    }

    #[test]
    fn set_overwrites_default() {
        let mut s = RichTextStyleSheet::new();
        s.set(
            "strong",
            StyleDelta {
                weight: Some(900),
                ..StyleDelta::empty()
            },
        );
        assert_eq!(s.get("strong").unwrap().weight, Some(900));
    }
}
