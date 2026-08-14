//! The three reusable element types — `TextElement`, `LineElement`,
//! `RectElement` — plus the `Element<T>` cascade wrapper and the
//! alignment enums.
//!
//! Every field on these element types is `Option<...>` — a `Some`
//! at any cascade layer wins, `None` falls through to the parent
//! layer, and ultimately to a per-type `ABSOLUTE_*` safety-net
//! constant. The cascade is per-field, so a `RectElement` that only
//! sets `linewidth_pt` inherits every other field from its parent
//! instead of clobbering them.
//!
//! ggplot2's `theme()` is built on a similar trio (`element_text`,
//! `element_line`, `element_rect`); we keep the structure because it
//! genuinely is the right factoring for chrome rendering. Where
//! ggplot2 had to collapse font choice into a four-variant `face`
//! field, our `TextElement` carries a full [`FontSpec`].

use std::sync::Arc;

use crate::scales::value::LinetypeStep;
use crate::stroke::{Cap, Join};

use super::font::FontSpec;
use super::length::{Length, Margin};
use super::palette::ThemeColor;

/// A single theme slot. `Inherit` walks up the inheritance chain to
/// the nearest `Set`; `Blank` hides the element (no draw call
/// emitted); `Set` overrides with a concrete value.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Element<T> {
    /// Walk up the inheritance chain.
    #[default]
    Inherit,
    /// Hide the element — the renderer skips its draw call entirely.
    Blank,
    /// Override with a concrete value.
    Set(T),
}

impl<T> Element<T> {
    /// `true` if this is `Element::Blank`.
    #[inline]
    pub fn is_blank(&self) -> bool {
        matches!(self, Element::Blank)
    }

    /// `true` if this is `Element::Inherit`.
    #[inline]
    pub fn is_inherit(&self) -> bool {
        matches!(self, Element::Inherit)
    }

    /// Borrow the inner value if this is `Element::Set`. `Inherit` and
    /// `Blank` both return `None`. Useful when the caller has already
    /// walked the inheritance chain and just wants the resolved
    /// element.
    #[inline]
    pub fn as_set(&self) -> Option<&T> {
        if let Element::Set(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Resolve against a parent: if `self` is `Set`, return it;
    /// if `Blank`, surface as `None`; if `Inherit`, fall through to
    /// `parent`.
    ///
    /// `Blank` short-circuits — it deliberately does not walk further,
    /// because the user explicitly asked to hide the element.
    pub fn cascade<'a>(&'a self, parent: Option<&'a T>) -> Option<&'a T> {
        match self {
            Element::Set(v) => Some(v),
            Element::Blank => None,
            Element::Inherit => parent,
        }
    }
}

/// Horizontal alignment — for text justification within a slot, and
/// for `hjust`-style anchor positioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HAlign {
    /// Align with the start edge (left in left-to-right scripts).
    #[default]
    Start,
    /// Centre within the slot.
    Center,
    /// Align with the end edge (right in left-to-right scripts).
    End,
    /// Stretch lines to fill the slot width.
    Justify,
}

/// Which region a plot-level text slot (title / subtitle / caption)
/// aligns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlignTo {
    /// Align to the panel column only — a centered title sits over
    /// the plotting area regardless of left-axis chrome width. The
    /// default (mirrors ggplot2's `plot.title.position = "panel"`).
    #[default]
    Panel,
    /// Align to the full plot interior (everything inside
    /// `plot_margin` + `plot_padding`, including axis chrome and
    /// legends). A centered title sits over the whole figure.
    /// Mirrors ggplot2's `plot.title.position = "plot"`.
    Plot,
}

/// Vertical alignment — for text baseline positioning within a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VAlign {
    /// Align with the top edge of the slot.
    Top,
    /// Centre vertically within the slot.
    #[default]
    Middle,
    /// Align with the alphabetic baseline within the slot.
    Baseline,
    /// Align with the bottom edge of the slot.
    Bottom,
}

/// Text rotation — either an absolute angle or a semantic
/// orientation relative to the surface's baseline direction.
///
/// `Along` / `Across` let chrome elements pick up the baseline's
/// orientation automatically:
/// - **Straight baselines (Cartesian axes, colorbar rails)**:
///   `Along` renders the text as a single rotated string parallel
///   to the baseline — 0° on Top / Bottom, 90° on Left / Right
///   (text runs up the column). `Across` is perpendicular.
/// - **Curved baselines (polar angular axes — title and tick
///   labels)**: `Along` lays the text out **along the arc** via
///   text-on-path rendering, so each glyph sits at its own tangent
///   on the circle and the whole title / label curves with the
///   ring. `Across` orients each character radially. The chrome
///   renderer picks the text-on-path technique when the surface
///   it's drawing into is curved.
///
/// Absolute `Degrees` ignores the surface and rotates the text as
/// a single straight string by the given angle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rotation {
    /// Absolute rotation in degrees, applied around the text's
    /// anchor. Positive = counterclockwise. Renders as a single
    /// straight string regardless of surface curvature.
    Degrees(f32),
    /// Aligned with the baseline direction. Straight baseline →
    /// single rotation matching the baseline angle. Curved
    /// baseline → text laid out along the curve via text-on-path.
    Along,
    /// Perpendicular to the baseline direction. Straight baseline
    /// → single rotation 90° off the baseline. Curved baseline →
    /// each character oriented perpendicular to the curve at its
    /// position (radial-outward on a polar ring).
    Across,
}

impl Default for Rotation {
    /// `Degrees(0.0)` — no rotation. `Along` / `Across` need a
    /// baseline to resolve against, so they aren't the right
    /// default for a free-floating `TextElement`.
    fn default() -> Self {
        Rotation::Degrees(0.0)
    }
}

impl Rotation {
    /// Resolve to an absolute angle in degrees, given the baseline
    /// direction (also in degrees, with 0° = pointing right / east
    /// and increasing counterclockwise). For `Degrees(d)` returns
    /// `d` regardless of baseline; for `Along` returns `baseline_deg`;
    /// for `Across` returns `baseline_deg + 90`.
    #[inline]
    pub fn resolve(self, baseline_deg: f32) -> f32 {
        match self {
            Rotation::Degrees(d) => d,
            Rotation::Along => baseline_deg,
            Rotation::Across => baseline_deg + 90.0,
        }
    }
}

/// Text styling — font selection, colour, size, alignment, rotation,
/// line height, margin.
///
/// Every field is `Option<...>` so an override can set just the
/// fields it cares about, with the rest cascading through the
/// parent chain. After cascading, callers fall through to
/// [`text_concrete_defaults`] for any remaining `None`s.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextElement {
    /// Font specification. Each `FontSpec` field cascades
    /// independently — a child can override weight while inheriting
    /// family.
    pub font: FontSpec,
    /// Ink colour. Resolved against the theme's palette at draw time.
    pub color: Option<ThemeColor>,
    /// Font size. Resolves against the inherited parent's resolved
    /// size — `Rel(1.5)` = 1.5× the parent.
    pub size_pt: Option<Length>,
    /// Horizontal alignment within the slot.
    pub align: Option<HAlign>,
    /// Vertical alignment within the slot.
    pub valign: Option<VAlign>,
    /// Rotation — absolute degrees, or `Along` / `Across` to follow
    /// the surface's baseline direction.
    pub angle: Option<Rotation>,
    /// Line height — typically `Rel(1.2)` (120% of the resolved size).
    pub lineheight: Option<Length>,
    /// Letter spacing (tracking). `Length::Abs(pt)` is absolute pt
    /// between glyph pairs; `Length::Rel(m)` is `m × resolved_size_pt`.
    /// `Abs(0.0)` (the default) is the natural font advance.
    pub letter_spacing: Option<Length>,
    /// Underline the text. Position and thickness are taken from the
    /// font's reported metrics.
    pub underline: Option<bool>,
    /// Strike through the text. Position and thickness are taken from
    /// the font's reported metrics.
    pub strikethrough: Option<bool>,
    /// Margin around the text block, each side independent.
    pub margin: Option<Margin>,
    /// Per-glyph outline color. `None` renders no outline — the one
    /// field [`text_concrete_defaults`] leaves unset, since a concrete
    /// value here would halo every string in the figure. A child layer
    /// therefore cannot clear an outline a parent set by writing
    /// `None`; set `text_linewidth_pt` to `Abs(0.0)` instead.
    ///
    /// Mirrors the `"text_stroke"` channel on the text geoms.
    pub text_stroke: Option<ThemeColor>,
    /// Outline thickness. Resolves against [`DEFAULT_LINEWIDTH_PT`],
    /// the parent every other linewidth in the theme resolves against,
    /// so `Rel(2.0)` is twice the standard hairline rather than a
    /// multiple of the font size. Has no effect unless `text_stroke`
    /// resolves to a color.
    ///
    /// The stroke centres on the glyph contour and does not grow the
    /// text's measured box, matching the text geoms — a deliberately
    /// heavy outline needs clearance from `margin`, which is measured.
    pub text_linewidth_pt: Option<Length>,
    /// Interpret the slot's string as marquee-flavoured markdown and
    /// shape it through the rich-text pipeline
    /// ([`crate::text::rich::RichTextRun`]). Off by default so every
    /// existing chrome label renders unchanged.
    ///
    /// When on, the slot's [`crate::plot::theme::Theme::rich_text`]
    /// style sheet drives the markdown styling; the `TextElement`'s
    /// resolved font / colour / size / letter-spacing feed into the
    /// rich run's base style so plain-text fragments still inherit
    /// the slot's ambient look.
    ///
    /// Text-outline (`text_stroke` / `text_linewidth_pt`) is not
    /// applied on markdown slots — the rich pipeline doesn't expose
    /// a glyph-outline surface. Chrome renderers fall back to plain
    /// text when an outline is needed on the same slot.
    pub markdown: Option<bool>,
}

impl TextElement {
    /// Merge `self` over `parent`: per-field, `self`'s `Some` wins;
    /// `None` falls through to `parent`'s value. `font` cascades
    /// through [`FontSpec::cascade`] (each `FontSpec` field merges
    /// independently; feature / variation lists merge by tag).
    pub fn cascade(&self, parent: &Self) -> Self {
        Self {
            font: parent.font.cascade(&self.font),
            color: self.color.clone().or_else(|| parent.color.clone()),
            size_pt: self.size_pt.or(parent.size_pt),
            align: self.align.or(parent.align),
            valign: self.valign.or(parent.valign),
            angle: self.angle.or(parent.angle),
            lineheight: self.lineheight.or(parent.lineheight),
            letter_spacing: self.letter_spacing.or(parent.letter_spacing),
            underline: self.underline.or(parent.underline),
            strikethrough: self.strikethrough.or(parent.strikethrough),
            margin: self.margin.or(parent.margin),
            text_stroke: self
                .text_stroke
                .clone()
                .or_else(|| parent.text_stroke.clone()),
            text_linewidth_pt: self.text_linewidth_pt.or(parent.text_linewidth_pt),
            markdown: self.markdown.or(parent.markdown),
        }
    }
}

/// Default text size, pt. Single source of truth shared by
/// [`text_concrete_defaults`] (which wraps it in `Length::Abs`) and
/// any chrome site that needs the bottom-of-cascade parent value to
/// resolve a `Length::Rel`. Matches ggplot2's `base_size = 11`.
pub const DEFAULT_TEXT_SIZE_PT: f64 = 11.0;
/// Default text lineheight multiplier — applied as `Rel(_)` against
/// the resolved text size.
pub const DEFAULT_TEXT_LINEHEIGHT: f64 = 1.2;

/// Concrete fallback values for a `TextElement` — 10pt regular ink
/// text, centered, no rotation, 1.2× lineheight, zero margin, no
/// glyph outline. Used as the safety net for any field still `None`
/// after cascading.
///
/// `text_stroke` is the single field left `None`: it is a terminal
/// value meaning "fill only", not a hole to fill, so chrome reads it
/// directly rather than through the `.expect("… default")` pattern the
/// other fields use.
pub fn text_concrete_defaults() -> TextElement {
    TextElement {
        font: FontSpec::default(),
        color: Some(ThemeColor::Ink),
        size_pt: Some(Length::Abs(DEFAULT_TEXT_SIZE_PT)),
        align: Some(HAlign::Center),
        valign: Some(VAlign::Middle),
        angle: Some(Rotation::default()),
        lineheight: Some(Length::Rel(DEFAULT_TEXT_LINEHEIGHT)),
        letter_spacing: Some(Length::Abs(0.0)),
        underline: Some(false),
        strikethrough: Some(false),
        margin: Some(Margin::ZERO),
        text_stroke: None,
        text_linewidth_pt: Some(Length::Abs(DEFAULT_LINEWIDTH_PT)),
        markdown: Some(false),
    }
}

/// Stroke styling — colour, width, dash pattern, cap, join.
///
/// Every field is `Option<...>` so an override can set just the
/// fields it cares about, with the rest cascading through the
/// parent chain. After cascading, callers fall through to
/// [`line_concrete_defaults`] for any remaining `None`s.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineElement {
    /// Stroke colour.
    pub color: Option<ThemeColor>,
    /// Stroke width. Resolves against the inherited parent's resolved
    /// linewidth — `Rel(2.0)` = twice the parent.
    pub linewidth_pt: Option<Length>,
    /// Dash pattern. Empty = solid stroke. Reuses the same
    /// `LinetypeStep` machinery the geom layer already ships.
    pub linetype: Option<Arc<[LinetypeStep]>>,
    /// Line end cap.
    pub cap: Option<Cap>,
    /// Line join.
    pub join: Option<Join>,
}

impl LineElement {
    /// Merge `self` over `parent`: per-field, `self`'s `Some` wins;
    /// `None` falls through to `parent`'s value.
    pub fn cascade(&self, parent: &Self) -> Self {
        Self {
            color: self.color.clone().or_else(|| parent.color.clone()),
            linewidth_pt: self.linewidth_pt.or(parent.linewidth_pt),
            linetype: self.linetype.clone().or_else(|| parent.linetype.clone()),
            cap: self.cap.or(parent.cap),
            join: self.join.or(parent.join),
        }
    }
}

/// Default line width, pt. Shared by [`line_concrete_defaults`] and
/// any chrome site that needs the bottom-of-cascade parent value to
/// resolve a `Length::Rel`.
pub const DEFAULT_LINEWIDTH_PT: f64 = 1.0;

/// Concrete fallback values for a `LineElement` — 1pt solid ink
/// line, butt cap, miter join. Used as the safety net for any
/// field still `None` after cascading.
pub fn line_concrete_defaults() -> LineElement {
    LineElement {
        color: Some(ThemeColor::Ink),
        linewidth_pt: Some(Length::Abs(DEFAULT_LINEWIDTH_PT)),
        linetype: Some(Arc::from([])),
        cap: Some(Cap::Butt),
        join: Some(Join::Miter),
    }
}

/// Filled-rectangle styling — fill, border colour, border width, border
/// dash, corner radius.
///
/// Every field is `Option<...>` so an override can set just the
/// fields it cares about, with the rest cascading through the
/// parent chain. After cascading, callers fall through to
/// [`rect_concrete_defaults`] for any remaining `None`s.
///
/// **Fill semantics quirk:** `fill` after cascading represents the
/// resolved fill colour. `None` after cascading means **no fill
/// drawn** (transparent interior). [`rect_concrete_defaults`]
/// preserves that semantic by leaving `fill` itself wrapped in
/// `Some(Some(...))` — the inner `Option` carries the
/// transparent-vs-paper distinction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RectElement {
    /// Fill colour. Cascade resolves through layers per-field; the
    /// final resolved `Option<ThemeColor>` is `None` → no fill (the
    /// interior stays transparent), `Some(c)` → fill with `c`.
    pub fill: Option<ThemeColor>,
    /// Border colour. Ignored when `linewidth_pt` resolves to 0.
    pub color: Option<ThemeColor>,
    /// Border width. `Abs(0.0)` = no border drawn.
    pub linewidth_pt: Option<Length>,
    /// Border dash pattern. Empty = solid border.
    pub linetype: Option<Arc<[LinetypeStep]>>,
    /// Corner radius. `Abs(0.0)` = sharp corners.
    pub corner_radius: Option<Length>,
}

impl RectElement {
    /// Merge `self` over `parent`: per-field, `self`'s `Some` wins;
    /// `None` falls through to `parent`'s value.
    pub fn cascade(&self, parent: &Self) -> Self {
        Self {
            fill: self.fill.clone().or_else(|| parent.fill.clone()),
            color: self.color.clone().or_else(|| parent.color.clone()),
            linewidth_pt: self.linewidth_pt.or(parent.linewidth_pt),
            linetype: self.linetype.clone().or_else(|| parent.linetype.clone()),
            corner_radius: self.corner_radius.or(parent.corner_radius),
        }
    }
}

/// Concrete fallback values for a `RectElement` — paper fill, ink
/// border, 1pt border width, solid stroke, sharp corners. Used as
/// the safety net for any field still `None` after cascading.
pub fn rect_concrete_defaults() -> RectElement {
    RectElement {
        fill: Some(ThemeColor::Paper),
        color: Some(ThemeColor::Ink),
        linewidth_pt: Some(Length::Abs(1.0)),
        linetype: Some(Arc::from([])),
        corner_radius: Some(Length::Abs(0.0)),
    }
}
