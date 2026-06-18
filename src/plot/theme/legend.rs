//! [`LegendTheme`] and its two type-specific sub-themes: [`KeyTheme`]
//! (standard / discrete legends) and [`BarTheme`] (colorbar + binned
//! legends).
//!
//! The legend also reuses [`super::axis::AxisTheme`] for its tick-
//! labels-and-ticks component. `axis.title` is ignored — the
//! legend's overall title sits on `LegendTheme.title` instead.

use super::axis::AxisTheme;
use super::element::{Element, TextElement};
use super::length::{Length, Margin};
use super::RectElement;

/// Direction in which keys / bar flow within a legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// Pick direction from the legend's placement side: `Horizontal`
    /// for Top / Bottom (key stacks form a row beneath the title),
    /// `Vertical` for Left / Right (key stacks form a column).
    /// In-panel legends fall back to `Vertical`. The default — most
    /// users want this.
    #[default]
    Auto,
    /// Keys stack left-to-right (one row, multiple columns).
    Horizontal,
    /// Keys stack top-to-bottom (one column, multiple rows).
    Vertical,
}

impl Direction {
    /// Resolve `Auto` against a placement side; concrete variants pass
    /// through unchanged.
    pub fn resolve(self, side: crate::scales::chrome::LegendSide) -> ResolvedDirection {
        use crate::scales::chrome::LegendSide;
        match self {
            Direction::Horizontal => ResolvedDirection::Horizontal,
            Direction::Vertical => ResolvedDirection::Vertical,
            Direction::Auto => match side {
                LegendSide::Top | LegendSide::Bottom => ResolvedDirection::Horizontal,
                LegendSide::Left | LegendSide::Right => ResolvedDirection::Vertical,
                LegendSide::InPanel { .. } => ResolvedDirection::Vertical,
            },
        }
    }
}

/// Concrete flow direction after `Auto` has been resolved against
/// the legend's placement side. Returned by [`Direction::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolvedDirection {
    /// Keys flow left-to-right.
    Horizontal,
    /// Keys flow top-to-bottom.
    Vertical,
}

/// Theme for the discrete-key portion of a legend (standard legend).
/// Symmetric with [`BarTheme`].
#[derive(Debug, Clone, PartialEq)]
pub struct KeyTheme {
    /// Frame around each key cell — fill + border combined.
    /// `RectElement` carries both `fill` (Option<ThemeColor>) and
    /// `linewidth_pt`, so a single element covers backgrounds, borders,
    /// or both. Set `fill = None` for no background, `linewidth_pt =
    /// Abs(0.0)` for no border.
    pub frame: Element<RectElement>,
    /// Key cell width in pt. Every key kind sits inside this cell:
    /// line keys run the full width as a horizontal rule; point
    /// keys render their marker (sized by
    /// `theme.geom.point.size_pt` or a bound size scale) centered
    /// within the cell; rect keys fill the cell.
    pub width: Length,
    /// Key cell height in pt.
    pub height: Length,
    /// Gap between adjacent keys within a single legend (intra-legend
    /// spacing). Spacing **between** legends lives on
    /// `Theme::legend_spacing`. The gap between a key swatch and
    /// its label uses [`LegendTheme::axis`]'s `tick_gap` — same
    /// semantic as the gap between an axis tick mark and its
    /// label, so users tune one knob for both.
    pub spacing: Length,
}

impl Default for KeyTheme {
    /// 12×12pt square cells (matches ggplot2's
    /// `legend.key.size = unit(1.2, 'lines')` default at the
    /// standard 11pt base size). 4pt intra-legend key spacing,
    /// no frame.
    fn default() -> Self {
        Self {
            frame: Element::Blank,
            width: Length::Abs(12.0),
            height: Length::Abs(12.0),
            spacing: Length::Abs(4.0),
        }
    }
}

/// Theme for the bar portion of a legend (colorbar + binned).
/// Symmetric with [`KeyTheme`].
#[derive(Debug, Clone, PartialEq)]
pub struct BarTheme {
    /// Bar dimension along the legend's [`Direction`], pt.
    pub length: Length,
    /// Bar dimension perpendicular to the legend's [`Direction`], pt.
    pub width: Length,
    /// Outline around the bar. Fill is ignored — the bar's own
    /// gradient / bin colors fill the interior.
    pub frame: Element<RectElement>,
}

impl Default for BarTheme {
    /// 100pt × 12pt bar with a thin ink outline.
    fn default() -> Self {
        Self {
            length: Length::Abs(100.0),
            width: Length::Abs(12.0),
            frame: Element::Set(RectElement {
                // Explicit None: the bar's own gradient fills the
                // interior, so the frame stays unfilled.
                fill: None,
                ..RectElement::default()
            }),
        }
    }
}

/// Theme for a complete legend — overall framing, an axis-like
/// component, plus the type-specific [`KeyTheme`] and [`BarTheme`].
#[derive(Debug, Clone, PartialEq)]
pub struct LegendTheme {
    /// Background rect around the whole legend.
    pub background: Element<RectElement>,
    /// The legend's overall title — labels the legend itself.
    /// Distinct from `axis.title`, which lives on `AxisTheme` and is
    /// ignored by legends.
    pub title: Element<TextElement>,
    /// Outer margin around the whole legend.
    pub margin: Margin,
    /// Inner padding inside the background.
    pub padding: Margin,
    /// Direction keys / bar flow within the legend.
    pub direction: Direction,
    /// Shared axis-like component reused from plot axes. Covers tick
    /// labels (used as key labels in standard legends), ticks,
    /// baseline, tick lengths, tick gap. `axis.title` is ignored;
    /// the legend's overall title is `LegendTheme.title` above.
    pub axis: AxisTheme,
    /// Discrete-key styling.
    pub key: KeyTheme,
    /// Bar-legend styling.
    pub bar: BarTheme,
}

impl Default for LegendTheme {
    fn default() -> Self {
        Self {
            background: Element::Blank,
            title: Element::Set(TextElement {
                size_pt: Some(Length::Abs(11.0)),
                ..TextElement::default()
            }),
            margin: Margin::ZERO,
            padding: Margin::all(Length::Abs(6.0)),
            direction: Direction::default(),
            // Sparse AxisTheme: only override the slots that legends
            // suppress (no baseline, no ticks). Everything else
            // cascades through the resolver's per-type defaults at
            // resolve time, so legend tick labels pick up the same
            // 10pt sizing as axis tick labels.
            axis: AxisTheme {
                line: Element::Blank,
                ticks: Element::Blank,
                ticks_minor: Element::Blank,
                ..AxisTheme::default()
            },
            key: KeyTheme::default(),
            bar: BarTheme::default(),
        }
    }
}
