//! Style deltas, the resolved style they cascade into, and the style
//! sheet that maps selector names to them.
//!
//! A [`StyleDelta`] is a sparse overlay: every field is `Option<...>`,
//! and `None` means "inherit". Deltas are what a sheet stores and what
//! a `{selector body}` span contributes.
//!
//! A [`ResolvedStyle`] is the concrete result of applying a chain of
//! deltas — every length already in points, every flag decided. The
//! reducer carries one per inline run and per block, so the shaping
//! pass never has to resolve a length itself.
//!
//! [`ResolvedStyle::apply`] is the cascade step. It resolves `size`
//! first, then every other field against that new own size, and honours
//! [`StyleDelta::skip_inherit`] by reading the grandparent for the
//! named fields.
//!
//! [`RichTextStyleSheet::new`] ships marquee's `classic_style()`
//! values against palette-relative colours;
//! [`RichTextStyleSheet::empty`] gives a blank slate.

use std::collections::HashMap;
use std::sync::Arc;

use super::length::{
    em, pt, relative, rem, FieldSet, LengthSpec, LineHeightSpec, RichMargin, StyleField,
};
use crate::scales::value::LinetypeStep;
use crate::style_vocab::{HAlign, ThemeColor};
use crate::text::{FontFeatureSetting, TextStyle};

// ─── Direction ──────────────────────────────────────────────────────────────

/// Block-axis writing direction.
///
/// `text_direction` is a **block-axis** property: it flips which
/// physical side counts as "start" for our own block-level primitives
/// (bullet placement, indent / hanging application, `HAlign::Start` /
/// `End` resolution, blockquote's start-side bar, and the l / r swap
/// on any block-level [`RichMargin`]). It does **not** touch parley's
/// shaping — glyph order within a line is determined entirely by the
/// text's actual script content via parley's UBA.
///
/// `Auto` (the default when the field is `None`) reads the direction
/// back from parley's own resolution (`parley::Layout::is_rtl`) after
/// shaping — for a block containing Arabic that resolves to `Rtl`, for
/// Latin to `Ltr`. Explicit `Ltr` / `Rtl` override our block-axis
/// interpretation even when parley infers the opposite; the source
/// text still shapes exactly as it would under `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Read the direction from parley's own UBA output on the block's
    /// text. Default.
    Auto,
    /// Left-to-right block-axis. Physical left = start.
    Ltr,
    /// Right-to-left block-axis. Physical right = start.
    Rtl,
}

// ─── StyleDelta ──────────────────────────────────────────────────────────────

/// Sparse overlay on a parent style. Every field is `Option<>` and
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
    /// Font size. `Relative(m)` compounds with the parent's size;
    /// `Em(m)` is the same thing (an element's size relative to its
    /// own size); `Rem(m)` reads against the run's base size.
    pub size: Option<LengthSpec>,
    /// Text colour. Resolved through the theme palette at draw time.
    pub color: Option<ThemeColor>,
    /// Letter spacing (tracking) in 1/1000 em — marquee's unit, so a
    /// value survives a font-size change unchanged.
    pub tracking: Option<f32>,
    /// Underline decoration.
    pub underline: Option<bool>,
    /// Strikethrough decoration.
    pub strikethrough: Option<bool>,
    /// Baseline shift (positive = up). Applied as a per-glyph-run
    /// y-offset at draw time — parley doesn't model this natively.
    /// Nested shifts compound.
    pub baseline: Option<LengthSpec>,
    /// Per-glyph outline colour applied to this span. Emitted as a
    /// stroke-only glyph pass BEHIND the fill pass, mirroring the
    /// `text_stroke` field on chrome theme text elements.
    pub text_stroke: Option<ThemeColor>,
    /// Outline stroke width for [`Self::text_stroke`]. No effect
    /// unless `text_stroke` resolves to a colour.
    pub text_stroke_width: Option<LengthSpec>,
    /// OpenType feature settings applied to this span (e.g. small
    /// caps, tabular numerals, stylistic sets). `None` inherits from
    /// the parent; `Some(vec)` merges tag-by-tag onto the parent (a
    /// child entry with the same tag replaces the parent's, others
    /// carry through). An empty `Some(vec![])` explicitly clears the
    /// parent's features.
    pub features: Option<Vec<FontFeatureSetting>>,

    // ── Block-level ──
    /// Line height.
    pub lineheight: Option<LineHeightSpec>,
    /// Horizontal alignment within the block's width.
    pub align: Option<HAlign>,
    /// Block-axis writing direction. `None` inherits (falling through
    /// to the ancestor chain, then to [`Direction::Auto`] at the root
    /// — parley's UBA-inferred direction).
    pub text_direction: Option<Direction>,
    /// First-line indent.
    pub indent: Option<LengthSpec>,
    /// Continuation-line indent — hanging indent for lists.
    pub hanging: Option<LengthSpec>,
    /// Trbl margin (outside the box).
    pub margin: Option<RichMargin>,
    /// Trbl padding (inside the box, before content).
    pub padding: Option<RichMargin>,
    /// Block background colour.
    pub background: Option<ThemeColor>,
    /// Block border colour.
    pub border_color: Option<ThemeColor>,
    /// Block border thickness per side (top / right / bottom / left).
    /// Set one side to a non-zero value and the rest to zero for a
    /// single-edge bar (e.g. a blockquote's left rule). Setting all
    /// four to the same value draws a uniform rectangle border, which
    /// combines cleanly with `border_radius`. Mixed per-side widths
    /// draw four independent segments with square corners (mixing
    /// per-side widths with `border_radius` yields undefined visuals —
    /// the four segments still emit but the radius is ignored on the
    /// mixed path).
    pub border_width: Option<RichMargin>,
    /// Block border corner radius.
    pub border_radius: Option<LengthSpec>,
    /// Block border dash pattern. `None` inherits (which resolves to
    /// a solid stroke). Uses the crate-wide [`LinetypeStep`]
    /// representation — dash / gap lengths in pt. Marker steps aren't
    /// drawn on block borders (they're stripped to `Gap`); use them
    /// only when your linetype is shared with a line-geom channel and
    /// needs to survive both contexts.
    pub border_type: Option<Arc<[LinetypeStep]>>,
    /// Per-nesting-depth bullet markers for list items. `None`
    /// inherits; `Some(vec![])` suppresses the bullet at every depth;
    /// entries index by the list's 0-based nesting depth and cycle
    /// when the depth exceeds the vector length (so a 3-entry vector
    /// serves any nesting). An individual empty-string entry
    /// suppresses the marker at that specific depth.
    pub bullet: Option<Vec<String>>,

    /// Fields that inherit from the **grandparent** rather than the
    /// parent. Marquee's `skip_inherit`: `sup { size: relative(0.5),
    /// skip_inherit: [Size] }` keeps a doubly-nested superscript from
    /// shrinking to a quarter.
    pub skip_inherit: FieldSet,
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
            tracking: over.tracking.or(self.tracking),
            underline: over.underline.or(self.underline),
            strikethrough: over.strikethrough.or(self.strikethrough),
            baseline: over.baseline.or(self.baseline),
            text_stroke: over
                .text_stroke
                .clone()
                .or_else(|| self.text_stroke.clone()),
            text_stroke_width: over.text_stroke_width.or(self.text_stroke_width),
            features: merge_features(self.features.as_deref(), over.features.as_deref()),
            lineheight: over.lineheight.or(self.lineheight),
            align: over.align.or(self.align),
            text_direction: over.text_direction.or(self.text_direction),
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
            border_type: over
                .border_type
                .clone()
                .or_else(|| self.border_type.clone()),
            bullet: over.bullet.clone().or_else(|| self.bullet.clone()),
            skip_inherit: self.skip_inherit.union(over.skip_inherit),
        }
    }
}

/// Overlay `over` onto `parent` with tag-level merge semantics: the
/// child replaces same-tag entries from the parent and appends new
/// tags; `None` on either side falls through / passes the other. An
/// explicit empty `Some(vec![])` on the child clears the parent's
/// features.
fn merge_features(
    parent: Option<&[FontFeatureSetting]>,
    over: Option<&[FontFeatureSetting]>,
) -> Option<Vec<FontFeatureSetting>> {
    match (parent, over) {
        (None, None) => None,
        (Some(p), None) => Some(p.to_vec()),
        (None, Some(o)) => Some(o.to_vec()),
        (Some(p), Some(o)) => {
            let mut out: Vec<FontFeatureSetting> = p
                .iter()
                .filter(|f| !o.iter().any(|c| c.tag == f.tag))
                .copied()
                .collect();
            out.extend_from_slice(o);
            Some(out)
        }
    }
}

// ─── ResolvedStyle ──────────────────────────────────────────────────────────

/// A style with every length already in points and every flag decided.
/// One of these rides on each inline run and each block the reducer
/// emits, so the shaping pass reads concrete numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedStyle {
    // ── Glyph-level ──
    /// Font family override. `None` keeps the base style's chain.
    pub family: Option<String>,
    /// CSS-style font weight.
    pub weight: u16,
    /// Italic flag.
    pub italic: bool,
    /// CSS `font-width` ratio.
    pub width: f32,
    /// Font size in points.
    pub size_pt: f64,
    /// Text colour, still palette-relative until draw time.
    pub color: Option<ThemeColor>,
    /// Letter spacing in 1/1000 em.
    pub tracking: f32,
    /// Underline flag.
    pub underline: bool,
    /// Strikethrough flag.
    pub strikethrough: bool,
    /// Accumulated baseline shift in points (positive = up).
    pub baseline_pt: f64,
    /// Glyph outline colour. `None` = fill only.
    pub text_stroke: Option<ThemeColor>,
    /// Glyph outline width in points.
    pub text_stroke_width_pt: f64,
    /// OpenType features in effect.
    pub features: Vec<FontFeatureSetting>,

    // ── Block-level ──
    /// Line height.
    pub lineheight: LineHeightSpec,
    /// Horizontal alignment, `None` = defer to the caller.
    pub align: Option<HAlign>,
    /// Block-axis direction, `None` = read back from parley.
    pub text_direction: Option<Direction>,
    /// First-line indent in points.
    pub indent_pt: f64,
    /// Continuation-line indent in points.
    pub hanging_pt: f64,
    /// Outer spacing in points, `[top, right, bottom, left]`.
    pub margin_pt: [f64; 4],
    /// Inner spacing in points, `[top, right, bottom, left]`.
    pub padding_pt: [f64; 4],
    /// Block background colour.
    pub background: Option<ThemeColor>,
    /// Block border colour.
    pub border_color: Option<ThemeColor>,
    /// Block border widths in points, `[top, right, bottom, left]`.
    pub border_width_pt: [f64; 4],
    /// Block border corner radius in points.
    pub border_radius_pt: f64,
    /// Block border dash pattern.
    pub border_type: Option<Arc<[LinetypeStep]>>,
    /// Per-nesting-depth bullet markers.
    pub bullet: Option<Vec<String>>,
}

impl ResolvedStyle {
    /// The root of the cascade: everything the base [`TextStyle`]
    /// dictates, with no box spacing and no decorations.
    pub fn from_base(base: &TextStyle) -> Self {
        Self {
            family: None,
            weight: base.weight,
            italic: matches!(base.style, crate::text::FontStyleKind::Italic),
            width: base.width,
            size_pt: base.size_pt as f64,
            color: None,
            tracking: base.tracking,
            underline: base.underline,
            strikethrough: base.strikethrough,
            baseline_pt: 0.0,
            text_stroke: None,
            text_stroke_width_pt: 0.0,
            features: base.features.clone(),
            lineheight: match base.line_height {
                crate::text::LineHeight::Relative(m) => LineHeightSpec::Mult(m as f64),
                crate::text::LineHeight::Absolute(v) => LineHeightSpec::Pt(v as f64),
            },
            align: None,
            text_direction: None,
            indent_pt: 0.0,
            hanging_pt: 0.0,
            margin_pt: [0.0; 4],
            padding_pt: [0.0; 4],
            background: None,
            border_color: None,
            border_width_pt: [0.0; 4],
            border_radius_pt: 0.0,
            border_type: None,
            bullet: None,
        }
    }

    /// Apply `delta` on top of `self`, producing the child's resolved
    /// style.
    ///
    /// `grandparent` supplies the inherited value for any field named
    /// in `delta.skip_inherit`; `base_size_pt` anchors `Rem`. `size`
    /// resolves first so every other `Em` length measures against the
    /// child's own size rather than the parent's.
    pub fn apply(
        &self,
        delta: &StyleDelta,
        grandparent: &ResolvedStyle,
        base_size_pt: f64,
    ) -> ResolvedStyle {
        let src = |field: StyleField| -> &ResolvedStyle {
            if delta.skip_inherit.contains(field) {
                grandparent
            } else {
                self
            }
        };

        let parent_size = src(StyleField::Size).size_pt;
        // `Em` on `size` is degenerate — an element's size relative to
        // its own size — so it reads the same as `Relative`.
        let size_pt = delta
            .size
            .map(|s| s.resolve(parent_size, parent_size, base_size_pt))
            .unwrap_or(parent_size);
        let len = |spec: Option<LengthSpec>, parent: f64| -> f64 {
            spec.map(|s| s.resolve(parent, size_pt, base_size_pt))
                .unwrap_or(parent)
        };
        let sides = |spec: Option<RichMargin>, parent: [f64; 4]| -> [f64; 4] {
            spec.map(|m| m.resolve(parent, size_pt, base_size_pt))
                .unwrap_or(parent)
        };

        ResolvedStyle {
            family: delta
                .family
                .clone()
                .or_else(|| src(StyleField::Family).family.clone()),
            weight: delta.weight.unwrap_or(src(StyleField::Weight).weight),
            italic: delta.italic.unwrap_or(src(StyleField::Italic).italic),
            width: delta.width.unwrap_or(src(StyleField::Width).width),
            size_pt,
            color: delta
                .color
                .clone()
                .or_else(|| src(StyleField::Color).color.clone()),
            tracking: delta.tracking.unwrap_or(src(StyleField::Tracking).tracking),
            underline: delta
                .underline
                .unwrap_or(src(StyleField::Underline).underline),
            strikethrough: delta
                .strikethrough
                .unwrap_or(src(StyleField::Strikethrough).strikethrough),
            // Baseline shift is cumulative: a `sup` inside a `sup`
            // lifts twice.
            baseline_pt: {
                let inherited = src(StyleField::Baseline).baseline_pt;
                match delta.baseline {
                    Some(s) => inherited + s.resolve(inherited, size_pt, base_size_pt),
                    None => inherited,
                }
            },
            text_stroke: delta
                .text_stroke
                .clone()
                .or_else(|| src(StyleField::TextStroke).text_stroke.clone()),
            text_stroke_width_pt: len(
                delta.text_stroke_width,
                src(StyleField::TextStrokeWidth).text_stroke_width_pt,
            ),
            features: merge_features(
                Some(&src(StyleField::Family).features),
                delta.features.as_deref(),
            )
            .unwrap_or_default(),
            lineheight: delta
                .lineheight
                .map(|lh| lh.resolve(src(StyleField::LineHeight).lineheight))
                .unwrap_or(src(StyleField::LineHeight).lineheight),
            align: delta.align.or(src(StyleField::Align).align),
            text_direction: delta.text_direction.or(self.text_direction),
            indent_pt: len(delta.indent, src(StyleField::Indent).indent_pt),
            hanging_pt: len(delta.hanging, src(StyleField::Hanging).hanging_pt),
            margin_pt: sides(delta.margin, src(StyleField::Margin).margin_pt),
            padding_pt: sides(delta.padding, src(StyleField::Padding).padding_pt),
            background: delta
                .background
                .clone()
                .or_else(|| src(StyleField::Background).background.clone()),
            border_color: delta
                .border_color
                .clone()
                .or_else(|| src(StyleField::BorderColor).border_color.clone()),
            border_width_pt: sides(
                delta.border_width,
                src(StyleField::BorderWidth).border_width_pt,
            ),
            border_radius_pt: len(
                delta.border_radius,
                src(StyleField::BorderRadius).border_radius_pt,
            ),
            border_type: delta
                .border_type
                .clone()
                .or_else(|| self.border_type.clone()),
            bullet: delta
                .bullet
                .clone()
                .or_else(|| src(StyleField::Bullet).bullet.clone()),
        }
    }

    /// Copy with every box-level field reset.
    ///
    /// Used when a block's resolved style seeds the inline cascade for
    /// its own content: the block already draws its background, border
    /// and spacing itself, so a descendant span must not inherit them
    /// and paint a second chip. Marquee gets the same rendered result
    /// from its `classic_style()` carrying explicit resets on every
    /// inline tag.
    pub fn for_inline(&self) -> ResolvedStyle {
        ResolvedStyle {
            align: None,
            indent_pt: 0.0,
            hanging_pt: 0.0,
            margin_pt: [0.0; 4],
            padding_pt: [0.0; 4],
            background: None,
            border_color: None,
            border_width_pt: [0.0; 4],
            border_radius_pt: 0.0,
            border_type: None,
            bullet: None,
            ..self.clone()
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
/// - Root: `base`.
/// - Inline markdown: `em`, `strong`, `underline`, `del`, `code`,
///   `sup`, `sub`, `link`, `outline`.
/// - Block markdown: `paragraph`, `h1`..`h6`, `block_quote`, `list`,
///   `list_ordered`, `list_item`, `list_item_body`, `code_block`,
///   `hr`.
///
/// Custom class names (from `{.warning …}` spans, `:::note …:::`
/// divs, etc.) are user-supplied via `set`. On a class-selector
/// lookup, `.name` first tries the sheet, then falls back to a CSS
/// colour keyword (see [`css_color`]). A `{#name …}` span looks up
/// `#name` verbatim, so ids and classes share the map without
/// colliding.
///
/// **A sheet is immutable once installed.** Caches key on the `Arc`
/// identity of the sheet they shaped against; mutating a sheet that a
/// live cache has already seen would leave stale entries. Build a new
/// sheet instead.
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
    /// See the type docs for the complete list.
    pub fn new() -> Self {
        let mut s = Self::empty();
        s.install_root_defaults();
        s.install_inline_defaults();
        s.install_heading_defaults();
        s.install_block_defaults();
        s
    }

    /// The root selector every document starts from.
    ///
    /// Deliberately empty. Marquee's `classic_style()` sets a `1.6`
    /// line height here because its base style *is* the caller's
    /// style; here the caller passes a [`TextStyle`] that
    /// [`ResolvedStyle::from_base`] already folds in, so a value on
    /// `base` would be the one field of that style the sheet
    /// overrides — leaving a chrome slot unable to reach its own
    /// theme's line height. A document that wants marquee's leading
    /// asks for it on the style it passes, or sets `base` itself.
    ///
    /// [`TextStyle`]: crate::text::TextStyle
    fn install_root_defaults(&mut self) {
        self.set("base", StyleDelta::empty());
    }

    fn install_inline_defaults(&mut self) {
        self.set(
            "em",
            StyleDelta {
                italic: Some(true),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "strong",
            StyleDelta {
                weight: Some(700),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "underline",
            StyleDelta {
                underline: Some(true),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "del",
            StyleDelta {
                strikethrough: Some(true),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "code",
            StyleDelta {
                family: Some("monospace".to_string()),
                size: Some(relative(0.85)),
                background: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.07)),
                // Small chip: horizontal padding reserves shape space
                // (parley pushes glyphs over via `InlineBox`), vertical
                // padding inflates the visible rect without changing
                // line height.
                padding: Some(RichMargin::all(rem(3.0 / 16.0))),
                border_radius: Some(rem(3.0 / 16.0)),
                ..StyleDelta::empty()
            },
        );
        // `sup` / `sub` shrink once per nesting level, not once per
        // ancestor: `skip_inherit` on `Size` reads the grandparent, so
        // `x^a^^b^` keeps `b` at the same size as `a`.
        self.set(
            "sup",
            StyleDelta {
                size: Some(em(0.5)),
                baseline: Some(em(1.0)),
                skip_inherit: FieldSet::of(&[StyleField::Size]),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "sub",
            StyleDelta {
                size: Some(em(0.5)),
                baseline: Some(em(-0.2)),
                skip_inherit: FieldSet::of(&[StyleField::Size]),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "link",
            StyleDelta {
                color: Some(ThemeColor::Accent),
                ..StyleDelta::empty()
            },
        );
        // Paper-filled glyphs with an ink outline — legible over busy
        // backgrounds. Marquee's `out` tag.
        self.set(
            "outline",
            StyleDelta {
                color: Some(ThemeColor::Paper),
                text_stroke: Some(ThemeColor::Ink),
                text_stroke_width: Some(rem(1.0 / 16.0)),
                ..StyleDelta::empty()
            },
        );
    }

    fn install_heading_defaults(&mut self) {
        // A hairline rule under h1 / h2, matching classic_style.
        let rule = |width: LengthSpec| StyleDelta {
            border_color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.07)),
            border_width: Some(RichMargin::new(pt(0.0), pt(0.0), width, pt(0.0))),
            padding: Some(RichMargin::new(pt(0.0), pt(0.0), em(0.3), pt(0.0))),
            ..StyleDelta::empty()
        };
        let heading = |size: f64, extra: StyleDelta| StyleDelta {
            size: Some(relative(size)),
            weight: Some(700),
            lineheight: Some(LineHeightSpec::Mult(1.2)),
            margin: Some(RichMargin::new(em(1.0), pt(0.0), rem(1.0), pt(0.0))),
            ..extra
        };
        self.set("h1", heading(2.25, rule(rem(1.0 / 16.0))));
        self.set("h2", heading(1.75, rule(rem(1.0 / 16.0))));
        self.set("h3", heading(1.5, StyleDelta::empty()));
        self.set("h4", heading(1.25, StyleDelta::empty()));
        self.set("h5", heading(1.0, StyleDelta::empty()));
        self.set(
            "h6",
            heading(
                1.0,
                StyleDelta {
                    color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.53)),
                    ..StyleDelta::empty()
                },
            ),
        );
    }

    fn install_block_defaults(&mut self) {
        self.set(
            "paragraph",
            StyleDelta {
                margin: Some(RichMargin::new(pt(0.0), pt(0.0), rem(1.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "block_quote",
            StyleDelta {
                color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.53)),
                border_color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.2)),
                // Start-side bar only.
                border_width: Some(RichMargin::new(pt(0.0), pt(0.0), pt(0.0), rem(0.25))),
                padding: Some(RichMargin::new(pt(0.0), pt(0.0), pt(0.0), em(1.0))),
                margin: Some(RichMargin::new(rem(1.0), pt(0.0), rem(1.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
        // Lists indent from surrounding prose through the container's
        // start padding; the item markers draw into that gutter.
        let list_container = StyleDelta {
            margin: Some(RichMargin::new(rem(1.0), pt(0.0), rem(1.0), pt(0.0))),
            padding: Some(RichMargin::new(pt(0.0), pt(0.0), pt(0.0), em(2.0))),
            ..StyleDelta::empty()
        };
        self.set("list", list_container.clone());
        self.set("list_ordered", list_container);
        self.set(
            "list_item",
            StyleDelta {
                bullet: Some(
                    ["•", "◦", "▪", "▫", "‣", "⁃"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                ),
                ..StyleDelta::empty()
            },
        );
        // `list_item_body` styles the paragraph a tight-list item's
        // body opens in. Empty by default — tight items stack with
        // no extra vertical margin. Loose items instead style their
        // body as `paragraph` (which carries a bottom margin).
        self.set("list_item_body", StyleDelta::empty());
        self.set(
            "code_block",
            StyleDelta {
                family: Some("monospace".to_string()),
                size: Some(relative(0.85)),
                lineheight: Some(LineHeightSpec::Mult(1.45)),
                background: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.07)),
                padding: Some(RichMargin::all(em(1.0))),
                border_radius: Some(rem(3.0 / 16.0)),
                margin: Some(RichMargin::new(rem(1.0), pt(0.0), rem(1.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
        self.set(
            "hr",
            StyleDelta {
                border_color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.07)),
                // Bottom-only border. The layout pass stretches the
                // block's shape width to the run's full content width
                // post-hoc, so the line spans the whole column even
                // though the block has no shaped text.
                border_width: Some(RichMargin::new(pt(0.0), pt(0.0), rem(1.0 / 16.0), pt(0.0))),
                margin: Some(RichMargin::new(rem(1.0), pt(0.0), rem(1.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
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

    /// Iterate over every selector in the sheet with its delta, in
    /// unspecified order.
    ///
    /// The counterpart to [`Self::set`]: enough to copy a sheet
    /// selector by selector, which is how one is reproduced somewhere
    /// the original couldn't reach.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &StyleDelta)> + '_ {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// How many selectors the sheet carries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the sheet carries no selectors.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ─── CSS colour-name lookup ─────────────────────────────────────────────────

/// Resolve `name` as a CSS colour keyword and return the RGB triple.
/// Case-insensitive; covers the full CSS Color 4 named-colour list,
/// including both `gray` and `grey` spellings.
///
/// Returns `None` for unknown names.
pub fn css_color(name: &str) -> Option<[u8; 3]> {
    let n = name.to_ascii_lowercase();
    let rgb = match n.as_str() {
        "aliceblue" => [240, 248, 255],
        "antiquewhite" => [250, 235, 215],
        "aqua" | "cyan" => [0, 255, 255],
        "aquamarine" => [127, 255, 212],
        "azure" => [240, 255, 255],
        "beige" => [245, 245, 220],
        "bisque" => [255, 228, 196],
        "black" => [0, 0, 0],
        "blanchedalmond" => [255, 235, 205],
        "blue" => [0, 0, 255],
        "blueviolet" => [138, 43, 226],
        "brown" => [165, 42, 42],
        "burlywood" => [222, 184, 135],
        "cadetblue" => [95, 158, 160],
        "chartreuse" => [127, 255, 0],
        "chocolate" => [210, 105, 30],
        "coral" => [255, 127, 80],
        "cornflowerblue" => [100, 149, 237],
        "cornsilk" => [255, 248, 220],
        "crimson" => [220, 20, 60],
        "darkblue" => [0, 0, 139],
        "darkcyan" => [0, 139, 139],
        "darkgoldenrod" => [184, 134, 11],
        "darkgray" | "darkgrey" => [169, 169, 169],
        "darkgreen" => [0, 100, 0],
        "darkkhaki" => [189, 183, 107],
        "darkmagenta" => [139, 0, 139],
        "darkolivegreen" => [85, 107, 47],
        "darkorange" => [255, 140, 0],
        "darkorchid" => [153, 50, 204],
        "darkred" => [139, 0, 0],
        "darksalmon" => [233, 150, 122],
        "darkseagreen" => [143, 188, 143],
        "darkslateblue" => [72, 61, 139],
        "darkslategray" | "darkslategrey" => [47, 79, 79],
        "darkturquoise" => [0, 206, 209],
        "darkviolet" => [148, 0, 211],
        "deeppink" => [255, 20, 147],
        "deepskyblue" => [0, 191, 255],
        "dimgray" | "dimgrey" => [105, 105, 105],
        "dodgerblue" => [30, 144, 255],
        "firebrick" => [178, 34, 34],
        "floralwhite" => [255, 250, 240],
        "forestgreen" => [34, 139, 34],
        "fuchsia" | "magenta" => [255, 0, 255],
        "gainsboro" => [220, 220, 220],
        "ghostwhite" => [248, 248, 255],
        "gold" => [255, 215, 0],
        "goldenrod" => [218, 165, 32],
        "gray" | "grey" => [128, 128, 128],
        "green" => [0, 128, 0],
        "greenyellow" => [173, 255, 47],
        "honeydew" => [240, 255, 240],
        "hotpink" => [255, 105, 180],
        "indianred" => [205, 92, 92],
        "indigo" => [75, 0, 130],
        "ivory" => [255, 255, 240],
        "khaki" => [240, 230, 140],
        "lavender" => [230, 230, 250],
        "lavenderblush" => [255, 240, 245],
        "lawngreen" => [124, 252, 0],
        "lemonchiffon" => [255, 250, 205],
        "lightblue" => [173, 216, 230],
        "lightcoral" => [240, 128, 128],
        "lightcyan" => [224, 255, 255],
        "lightgoldenrodyellow" => [250, 250, 210],
        "lightgray" | "lightgrey" => [211, 211, 211],
        "lightgreen" => [144, 238, 144],
        "lightpink" => [255, 182, 193],
        "lightsalmon" => [255, 160, 122],
        "lightseagreen" => [32, 178, 170],
        "lightskyblue" => [135, 206, 250],
        "lightslategray" | "lightslategrey" => [119, 136, 153],
        "lightsteelblue" => [176, 196, 222],
        "lightyellow" => [255, 255, 224],
        "lime" => [0, 255, 0],
        "limegreen" => [50, 205, 50],
        "linen" => [250, 240, 230],
        "maroon" => [128, 0, 0],
        "mediumaquamarine" => [102, 205, 170],
        "mediumblue" => [0, 0, 205],
        "mediumorchid" => [186, 85, 211],
        "mediumpurple" => [147, 112, 219],
        "mediumseagreen" => [60, 179, 113],
        "mediumslateblue" => [123, 104, 238],
        "mediumspringgreen" => [0, 250, 154],
        "mediumturquoise" => [72, 209, 204],
        "mediumvioletred" => [199, 21, 133],
        "midnightblue" => [25, 25, 112],
        "mintcream" => [245, 255, 250],
        "mistyrose" => [255, 228, 225],
        "moccasin" => [255, 228, 181],
        "navajowhite" => [255, 222, 173],
        "navy" => [0, 0, 128],
        "oldlace" => [253, 245, 230],
        "olive" => [128, 128, 0],
        "olivedrab" => [107, 142, 35],
        "orange" => [255, 165, 0],
        "orangered" => [255, 69, 0],
        "orchid" => [218, 112, 214],
        "palegoldenrod" => [238, 232, 170],
        "palegreen" => [152, 251, 152],
        "paleturquoise" => [175, 238, 238],
        "palevioletred" => [219, 112, 147],
        "papayawhip" => [255, 239, 213],
        "peachpuff" => [255, 218, 185],
        "peru" => [205, 133, 63],
        "pink" => [255, 192, 203],
        "plum" => [221, 160, 221],
        "powderblue" => [176, 224, 230],
        "purple" => [128, 0, 128],
        "rebeccapurple" => [102, 51, 153],
        "red" => [255, 0, 0],
        "rosybrown" => [188, 143, 143],
        "royalblue" => [65, 105, 225],
        "saddlebrown" => [139, 69, 19],
        "salmon" => [250, 128, 114],
        "sandybrown" => [244, 164, 96],
        "seagreen" => [46, 139, 87],
        "seashell" => [255, 245, 238],
        "sienna" => [160, 82, 45],
        "silver" => [192, 192, 192],
        "skyblue" => [135, 206, 235],
        "slateblue" => [106, 90, 205],
        "slategray" | "slategrey" => [112, 128, 144],
        "snow" => [255, 250, 250],
        "springgreen" => [0, 255, 127],
        "steelblue" => [70, 130, 180],
        "tan" => [210, 180, 140],
        "teal" => [0, 128, 128],
        "thistle" => [216, 191, 216],
        "tomato" => [255, 99, 71],
        "turquoise" => [64, 224, 208],
        "violet" => [238, 130, 238],
        "wheat" => [245, 222, 179],
        "white" => [255, 255, 255],
        "whitesmoke" => [245, 245, 245],
        "yellow" => [255, 255, 0],
        "yellowgreen" => [154, 205, 50],
        _ => return None,
    };
    Some(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextStyle;

    fn base() -> ResolvedStyle {
        ResolvedStyle::from_base(&TextStyle::new(10.0))
    }

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
    fn overlay_prefers_child_text_direction_over_parent() {
        let parent = StyleDelta {
            text_direction: Some(Direction::Ltr),
            ..StyleDelta::empty()
        };
        let child = StyleDelta {
            text_direction: Some(Direction::Rtl),
            ..StyleDelta::empty()
        };
        assert_eq!(
            parent.overlay(&child).text_direction,
            Some(Direction::Rtl),
            "child text_direction wins"
        );
        assert_eq!(
            parent.overlay(&StyleDelta::empty()).text_direction,
            Some(Direction::Ltr),
            "None on child falls through to parent"
        );
    }

    #[test]
    fn overlay_merges_features_by_tag() {
        let parent = StyleDelta {
            features: Some(vec![
                FontFeatureSetting {
                    tag: *b"liga",
                    value: 1,
                },
                FontFeatureSetting {
                    tag: *b"kern",
                    value: 1,
                },
            ]),
            ..StyleDelta::empty()
        };
        let child = StyleDelta {
            features: Some(vec![
                FontFeatureSetting {
                    tag: *b"liga",
                    value: 0,
                },
                FontFeatureSetting {
                    tag: *b"smcp",
                    value: 1,
                },
            ]),
            ..StyleDelta::empty()
        };
        let merged = parent.overlay(&child).features.unwrap();
        // kern from parent (child didn't touch), liga replaced by
        // child (0), smcp added by child.
        assert!(merged.contains(&FontFeatureSetting {
            tag: *b"kern",
            value: 1
        }));
        assert!(merged.contains(&FontFeatureSetting {
            tag: *b"liga",
            value: 0
        }));
        assert!(merged.contains(&FontFeatureSetting {
            tag: *b"smcp",
            value: 1
        }));
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn overlay_none_child_falls_through() {
        let parent = StyleDelta {
            color: Some(ThemeColor::Ink),
            ..StyleDelta::empty()
        };
        let merged = parent.overlay(&StyleDelta::empty());
        assert_eq!(merged.color, Some(ThemeColor::Ink));
    }

    #[test]
    fn relative_sizes_compound_through_nesting() {
        let b = base();
        let half = StyleDelta {
            size: Some(relative(0.5)),
            ..StyleDelta::empty()
        };
        let one = b.apply(&half, &b, 10.0);
        let two = one.apply(&half, &b, 10.0);
        assert!((one.size_pt - 5.0).abs() < 1e-9);
        assert!((two.size_pt - 2.5).abs() < 1e-9, "got {}", two.size_pt);
    }

    #[test]
    fn em_on_size_reads_the_same_as_relative() {
        let b = base();
        let by_em = b.apply(
            &StyleDelta {
                size: Some(em(0.5)),
                ..StyleDelta::empty()
            },
            &b,
            10.0,
        );
        let by_rel = b.apply(
            &StyleDelta {
                size: Some(relative(0.5)),
                ..StyleDelta::empty()
            },
            &b,
            10.0,
        );
        assert_eq!(by_em.size_pt, by_rel.size_pt);
    }

    #[test]
    fn em_spacing_measures_against_the_elements_own_size() {
        let b = base();
        let h = b.apply(
            &StyleDelta {
                size: Some(relative(2.0)),
                margin: Some(RichMargin::new(em(1.0), pt(0.0), pt(0.0), pt(0.0))),
                ..StyleDelta::empty()
            },
            &b,
            10.0,
        );
        // 2 × 10pt size, so `em(1)` of top margin is 20pt — not the
        // 10pt a base-anchored reading would give.
        assert!(
            (h.margin_pt[0] - 20.0).abs() < 1e-9,
            "got {:?}",
            h.margin_pt
        );
    }

    #[test]
    fn rem_ignores_nesting_depth() {
        let b = base();
        let shrink = StyleDelta {
            size: Some(relative(0.5)),
            indent: Some(rem(1.0)),
            ..StyleDelta::empty()
        };
        let deep = b.apply(&shrink, &b, 10.0).apply(&shrink, &b, 10.0);
        assert!((deep.indent_pt - 10.0).abs() < 1e-9);
    }

    #[test]
    fn skip_inherit_reads_the_grandparent() {
        let b = base();
        let sup = StyleDelta {
            size: Some(relative(0.5)),
            skip_inherit: FieldSet::of(&[StyleField::Size]),
            ..StyleDelta::empty()
        };
        let one = b.apply(&sup, &b, 10.0);
        // The nested sup's parent is `one`, its grandparent `b`; the
        // skip makes it read `b`, so it stays at half the base size.
        let two = one.apply(&sup, &b, 10.0);
        assert!((one.size_pt - 5.0).abs() < 1e-9);
        assert!((two.size_pt - 5.0).abs() < 1e-9, "got {}", two.size_pt);
    }

    #[test]
    fn baseline_shifts_accumulate_through_nesting() {
        let b = base();
        let sup = StyleDelta {
            baseline: Some(em(1.0)),
            ..StyleDelta::empty()
        };
        let one = b.apply(&sup, &b, 10.0);
        let two = one.apply(&sup, &b, 10.0);
        assert!((one.baseline_pt - 10.0).abs() < 1e-9);
        assert!((two.baseline_pt - 20.0).abs() < 1e-9);
    }

    #[test]
    fn for_inline_clears_box_fields_but_keeps_glyph_fields() {
        let b = base();
        let block = b.apply(
            &StyleDelta {
                weight: Some(700),
                background: Some(ThemeColor::Accent),
                padding: Some(RichMargin::all(pt(4.0))),
                bullet: Some(vec!["•".to_string()]),
                ..StyleDelta::empty()
            },
            &b,
            10.0,
        );
        let inline = block.for_inline();
        assert_eq!(inline.weight, 700, "glyph fields survive");
        assert!(inline.background.is_none());
        assert_eq!(inline.padding_pt, [0.0; 4]);
        assert!(inline.bullet.is_none());
    }

    #[test]
    fn default_sheet_populates_reserved_selectors() {
        let sheet = RichTextStyleSheet::new();
        for name in [
            "base",
            "em",
            "strong",
            "underline",
            "del",
            "code",
            "code_block",
            "sup",
            "sub",
            "link",
            "outline",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "paragraph",
            "block_quote",
            "list",
            "list_ordered",
            "list_item",
            "list_item_body",
            "hr",
        ] {
            assert!(sheet.get(name).is_some(), "missing sheet entry: {name}");
        }
    }

    #[test]
    fn strong_delta_has_bold_weight() {
        assert_eq!(
            RichTextStyleSheet::new().get("strong").unwrap().weight,
            Some(700)
        );
    }

    #[test]
    fn em_delta_toggles_italic() {
        assert_eq!(
            RichTextStyleSheet::new().get("em").unwrap().italic,
            Some(true)
        );
    }

    #[test]
    fn sup_and_sub_shift_in_opposite_directions() {
        let sheet = RichTextStyleSheet::new();
        let up = sheet.get("sup").unwrap().baseline.unwrap();
        let down = sheet.get("sub").unwrap().baseline.unwrap();
        let b = base();
        assert!(up.resolve(0.0, 10.0, 10.0) > 0.0);
        assert!(down.resolve(0.0, 10.0, 10.0) < 0.0);
        // Both shrink relative to their own em.
        let sup = b.apply(sheet.get("sup").unwrap(), &b, 10.0);
        assert!(sup.size_pt < b.size_pt);
    }

    #[test]
    fn heading_sizes_ascend_h6_to_h1() {
        let sheet = RichTextStyleSheet::new();
        let s = |name: &str| match sheet.get(name).unwrap().size {
            Some(LengthSpec::Relative(m)) => m,
            other => panic!("expected a relative size on {name}, got {other:?}"),
        };
        assert!(s("h1") > s("h2"));
        assert!(s("h2") > s("h3"));
        assert!(s("h3") > s("h4"));
        assert!(s("h4") > s("h5"));
    }

    #[test]
    fn links_are_colored_but_not_underlined() {
        let link = RichTextStyleSheet::new().get("link").unwrap().clone();
        assert_eq!(link.color, Some(ThemeColor::Accent));
        assert_eq!(link.underline, None);
    }

    #[test]
    fn horizontal_rule_draws_on_its_bottom_edge() {
        let hr = RichTextStyleSheet::new().get("hr").unwrap().clone();
        let w = hr.border_width.unwrap();
        assert_eq!(w.top, pt(0.0));
        assert_ne!(w.bottom, pt(0.0));
    }

    #[test]
    fn lists_indent_through_container_padding() {
        let list = RichTextStyleSheet::new().get("list").unwrap().clone();
        assert_eq!(list.padding.unwrap().left, em(2.0));
    }

    #[test]
    fn bullet_set_covers_six_nesting_levels() {
        let item = RichTextStyleSheet::new().get("list_item").unwrap().clone();
        assert_eq!(item.bullet.unwrap().len(), 6);
    }

    #[test]
    fn css_color_lookup_case_insensitive() {
        assert_eq!(css_color("red"), Some([255, 0, 0]));
        assert_eq!(css_color("Red"), Some([255, 0, 0]));
        assert_eq!(css_color("STEELBLUE"), Some([70, 130, 180]));
        assert_eq!(css_color("not-a-colour"), None);
    }

    #[test]
    fn css_color_covers_both_gray_spellings_and_css4_additions() {
        assert_eq!(css_color("grey"), css_color("gray"));
        assert_eq!(css_color("darkslategrey"), css_color("darkslategray"));
        assert_eq!(css_color("rebeccapurple"), Some([102, 51, 153]));
        assert_eq!(css_color("cyan"), css_color("aqua"));
    }

    #[test]
    fn iter_visits_every_selector_that_was_set() {
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "em",
            StyleDelta {
                italic: Some(true),
                ..StyleDelta::empty()
            },
        );
        sheet.set(
            "strong",
            StyleDelta {
                weight: Some(700),
                ..StyleDelta::empty()
            },
        );

        let mut names: Vec<&str> = sheet.iter().map(|(name, _)| name).collect();
        names.sort_unstable();
        assert_eq!(names, ["em", "strong"]);
        assert_eq!(sheet.len(), 2);
        assert!(!sheet.is_empty());
    }

    /// Copying a sheet selector by selector reproduces it — the property
    /// that lets a sheet be rebuilt where the original can't reach.
    #[test]
    fn a_sheet_can_be_rebuilt_from_its_iter() {
        let original = RichTextStyleSheet::new();
        let mut copy = RichTextStyleSheet::empty();
        for (name, delta) in original.iter() {
            copy.set(name, delta.clone());
        }
        assert_eq!(copy, original);
    }

    #[test]
    fn empty_sheet_has_no_entries() {
        assert!(RichTextStyleSheet::empty().get("strong").is_none());
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
