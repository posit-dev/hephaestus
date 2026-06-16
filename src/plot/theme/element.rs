//! The three reusable element types — `TextElement`, `LineElement`,
//! `RectElement` — plus the `Element<T>` cascade wrapper and the
//! alignment enums.
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
#[derive(Debug, Clone, PartialEq)]
pub struct TextElement {
    /// Font specification. Each `FontSpec` field cascades
    /// independently — a child can override weight while inheriting
    /// family.
    pub font: FontSpec,
    /// Ink colour. Resolved against the theme's palette at draw time.
    pub color: ThemeColor,
    /// Font size. Resolves against the inherited parent's resolved
    /// size — `Rel(1.5)` = 1.5× the parent.
    pub size_pt: Length,
    /// Horizontal alignment within the slot.
    pub align: HAlign,
    /// Vertical alignment within the slot.
    pub valign: VAlign,
    /// Rotation — absolute degrees, or `Along` / `Across` to follow
    /// the surface's baseline direction.
    pub angle: Rotation,
    /// Line height — typically `Rel(1.2)` (120% of the resolved size).
    pub lineheight: Length,
    /// Margin around the text block, each side independent.
    pub margin: Margin,
}

impl Default for TextElement {
    /// 10pt regular ink-colored text, centered, no rotation, 1.2×
    /// lineheight, zero margin.
    fn default() -> Self {
        Self {
            font: FontSpec::default(),
            color: ThemeColor::Ink,
            size_pt: Length::Abs(10.0),
            align: HAlign::Center,
            valign: VAlign::Middle,
            angle: Rotation::default(),
            lineheight: Length::Rel(1.2),
            margin: Margin::ZERO,
        }
    }
}

/// Stroke styling — colour, width, dash pattern, cap, join.
#[derive(Debug, Clone, PartialEq)]
pub struct LineElement {
    /// Stroke colour.
    pub color: ThemeColor,
    /// Stroke width. Resolves against the inherited parent's resolved
    /// linewidth — `Rel(2.0)` = twice the parent.
    pub linewidth_pt: Length,
    /// Dash pattern. Empty = solid stroke. Reuses the same
    /// `LinetypeStep` machinery the geom layer already ships.
    pub linetype: Arc<[LinetypeStep]>,
    /// Line end cap.
    pub cap: Cap,
    /// Line join.
    pub join: Join,
}

impl Default for LineElement {
    /// 1pt solid ink line, butt cap, miter join.
    fn default() -> Self {
        Self {
            color: ThemeColor::Ink,
            linewidth_pt: Length::Abs(1.0),
            linetype: Arc::from([]),
            cap: Cap::Butt,
            join: Join::Miter,
        }
    }
}

/// Filled-rectangle styling — fill, border colour, border width, border
/// dash, corner radius.
#[derive(Debug, Clone, PartialEq)]
pub struct RectElement {
    /// Fill colour. `None` = no fill (transparent interior).
    pub fill: Option<ThemeColor>,
    /// Border colour. Ignored when `linewidth_pt` resolves to 0.
    pub color: ThemeColor,
    /// Border width. `Abs(0.0)` = no border drawn.
    pub linewidth_pt: Length,
    /// Border dash pattern. Empty = solid border.
    pub linetype: Arc<[LinetypeStep]>,
    /// Corner radius. `Abs(0.0)` = sharp corners.
    pub corner_radius: Length,
}

impl Default for RectElement {
    /// Paper fill, ink border, 1pt border width, solid stroke, sharp
    /// corners.
    fn default() -> Self {
        Self {
            fill: Some(ThemeColor::Paper),
            color: ThemeColor::Ink,
            linewidth_pt: Length::Abs(1.0),
            linetype: Arc::from([]),
            corner_radius: Length::Abs(0.0),
        }
    }
}
