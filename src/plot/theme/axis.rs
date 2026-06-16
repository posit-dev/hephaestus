//! [`AxisTheme`] — the bundle of elements that describe an "axis-like"
//! chrome surface — and its three-layer cascade.
//!
//! Every axis-like surface in hephaestus follows the same structural
//! pattern: optional baseline, ticks + minor ticks, tick labels,
//! title, plus the tick lengths and tick-label gap. This applies to
//! plot axes (one `AxisTheme` per (channel, side) of the panel) and
//! legends (one `AxisTheme` per `LegendTheme`, covering tick labels
//! beside keys / ticks alongside a colorbar / etc.). Capturing the
//! pattern once means a user who wants "thicker tick marks
//! everywhere" sets it on the shared `AxisTheme` root and it
//! propagates to plot axes and legend bar ticks alike.

use super::element::{Element, Rotation};
use super::length::Length;
use super::{LineElement, TextElement};

/// Where the axis title sits relative to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TitleLocation {
    /// Title sits in the outer chrome slot, beyond the tick labels,
    /// on the side of the panel the axis draws against. The default
    /// for Cartesian axes.
    #[default]
    Outside,
    /// Title sits inside the panel, aligned with the axis. Useful
    /// for compact plots and projection styles that put the title
    /// near the axis baseline.
    Inside,
}

/// Full axis theme — every field that drives axis-like chrome
/// rendering. Tick lengths are signed: positive extends ticks away
/// from the panel (outward); negative extends them inward.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisTheme {
    /// Axis title. For plot axes, this is the axis's textual label;
    /// for legends, the legend's overall title sits elsewhere
    /// (`LegendTheme.title`) and `AxisTheme.title` is ignored.
    pub title: Element<TextElement>,
    /// Tick labels. For plot axes these are tick labels; for
    /// standard legends they double as key labels.
    pub text: Element<TextElement>,
    /// Baseline. Set to `Element::Blank` when the surface has no
    /// baseline (most legends).
    pub line: Element<LineElement>,
    /// Major tick marks.
    pub ticks: Element<LineElement>,
    /// Minor tick marks. Typically only used by continuous scales.
    pub ticks_minor: Element<LineElement>,
    /// Major-tick length. **Sign flips direction**: positive extends
    /// outward (away from the panel); negative extends inward.
    pub tick_length: Length,
    /// Minor-tick length. Sign behaves the same way as
    /// `tick_length`.
    pub tick_length_minor: Length,
    /// Gap between the end of a tick and the near edge of its label
    /// (always positive — labels sit on the side the tick extends
    /// to).
    pub tick_gap: Length,
    /// Where the axis title sits relative to the panel.
    pub title_location: TitleLocation,
}

impl Default for AxisTheme {
    /// Defaults match today's hardcoded axis chrome — 4pt major
    /// ticks, 2pt minor, 2pt label gap, 1pt baseline, 10pt labels,
    /// 12pt title, outside title placement.
    fn default() -> Self {
        Self {
            // Axis titles read along the axis baseline — `Along` lets
            // the chrome resolve to 0° on Top / Bottom and 90° on
            // Left / Right (text reads up the column) without the
            // renderer special-casing vertical sides.
            title: Element::Set(TextElement {
                size_pt: Length::Abs(12.0),
                angle: Rotation::Along,
                ..TextElement::default()
            }),
            text: Element::Set(TextElement {
                size_pt: Length::Abs(10.0),
                ..TextElement::default()
            }),
            line: Element::Set(LineElement::default()),
            ticks: Element::Set(LineElement::default()),
            ticks_minor: Element::Set(LineElement::default()),
            tick_length: Length::Abs(4.0),
            tick_length_minor: Length::Abs(2.0),
            tick_gap: Length::Abs(2.0),
            title_location: TitleLocation::Outside,
        }
    }
}

/// Sparse mirror of [`AxisTheme`] used for per-channel and
/// per-(channel, side) overrides. Every field is `Option<>` (for
/// `Length` / `TitleLocation`) or `Element::Inherit` (for the
/// element fields), so partial overrides cascade per-field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AxisThemePart {
    /// Optional title override; `Inherit` by default.
    pub title: Element<TextElement>,
    /// Optional tick label override; `Inherit` by default.
    pub text: Element<TextElement>,
    /// Optional baseline override; `Inherit` by default.
    pub line: Element<LineElement>,
    /// Optional tick override; `Inherit` by default.
    pub ticks: Element<LineElement>,
    /// Optional minor-tick override; `Inherit` by default.
    pub ticks_minor: Element<LineElement>,
    /// Optional tick-length override; `None` by default.
    pub tick_length: Option<Length>,
    /// Optional minor-tick-length override; `None` by default.
    pub tick_length_minor: Option<Length>,
    /// Optional tick-gap override; `None` by default.
    pub tick_gap: Option<Length>,
    /// Optional title-location override; `None` by default.
    pub title_location: Option<TitleLocation>,
}

/// Three-layer axis cascade — `all` (every axis), `by_channel`
/// (every axis on a given channel), `by_channel_side` (a specific
/// (channel, side) axis).
///
/// Resolution walks `by_channel_side[ch][side]` → `by_channel[ch]`
/// → `all`, per `AxisTheme` field independently.
#[derive(Debug, Clone, PartialEq)]
pub struct PerAxis {
    /// Applies to every axis.
    pub all: AxisTheme,
    /// Per-channel override (sparse).
    pub by_channel: [AxisThemePart; 2],
    /// Per-(channel, side) override (sparse, most specific).
    pub by_channel_side: [[AxisThemePart; 2]; 2],
}

impl PerAxis {
    /// Construct with `all = root` and every override slot empty.
    pub fn new(root: AxisTheme) -> Self {
        Self {
            all: root,
            by_channel: [AxisThemePart::default(), AxisThemePart::default()],
            by_channel_side: [
                [AxisThemePart::default(), AxisThemePart::default()],
                [AxisThemePart::default(), AxisThemePart::default()],
            ],
        }
    }

    /// Resolve every `AxisTheme` field for `(ch, side)`, returning a
    /// borrowed bundle.
    pub fn resolve(&self, ch: u8, side: u8) -> ResolvedAxis<'_> {
        let ci = ch as usize;
        let si = side as usize;
        debug_assert!(ci < 2 && si < 2, "channel/side out of range: {ch}, {side}");

        let by_ch = &self.by_channel[ci];
        let by_cs = &self.by_channel_side[ci][si];

        let title = cascade_element_chain([&by_cs.title, &by_ch.title], self.all.title.as_set());
        let text = cascade_element_chain([&by_cs.text, &by_ch.text], self.all.text.as_set());
        let line = cascade_element_chain([&by_cs.line, &by_ch.line], self.all.line.as_set());
        let ticks = cascade_element_chain([&by_cs.ticks, &by_ch.ticks], self.all.ticks.as_set());
        let ticks_minor = cascade_element_chain(
            [&by_cs.ticks_minor, &by_ch.ticks_minor],
            self.all.ticks_minor.as_set(),
        );

        let tick_length = by_cs
            .tick_length
            .or(by_ch.tick_length)
            .unwrap_or(self.all.tick_length);
        let tick_length_minor = by_cs
            .tick_length_minor
            .or(by_ch.tick_length_minor)
            .unwrap_or(self.all.tick_length_minor);
        let tick_gap = by_cs
            .tick_gap
            .or(by_ch.tick_gap)
            .unwrap_or(self.all.tick_gap);
        let title_location = by_cs
            .title_location
            .or(by_ch.title_location)
            .unwrap_or(self.all.title_location);

        ResolvedAxis {
            title,
            text,
            line,
            ticks,
            ticks_minor,
            tick_length,
            tick_length_minor,
            tick_gap,
            title_location,
        }
    }
}

impl Default for PerAxis {
    fn default() -> Self {
        Self::new(AxisTheme::default())
    }
}

/// Bundle of resolved [`AxisTheme`] fields for one (channel, side).
/// Returned by [`PerAxis::resolve`]; the borrowed references point
/// into the theme so no allocation happens at resolution time.
///
/// `Length` fields remain unresolved here — they're resolved against
/// the relevant parent at draw time (text size against base text,
/// tick length against the line root, etc.).
#[derive(Debug, Clone)]
pub struct ResolvedAxis<'a> {
    /// Axis title element, or `None` if Blank / Inherit-with-no-set.
    pub title: Option<&'a TextElement>,
    /// Tick label element, or `None`.
    pub text: Option<&'a TextElement>,
    /// Baseline element, or `None`.
    pub line: Option<&'a LineElement>,
    /// Major tick element, or `None`.
    pub ticks: Option<&'a LineElement>,
    /// Minor tick element, or `None`.
    pub ticks_minor: Option<&'a LineElement>,
    /// Major-tick length (sign flips direction).
    pub tick_length: Length,
    /// Minor-tick length (sign flips direction).
    pub tick_length_minor: Length,
    /// Gap between tick end and label near-edge.
    pub tick_gap: Length,
    /// Axis-title placement.
    pub title_location: TitleLocation,
}

fn cascade_element_chain<'a, T>(chain: [&'a Element<T>; 2], root: Option<&'a T>) -> Option<&'a T> {
    // Walk most-specific to least-specific. `Blank` at any level
    // short-circuits to None (the user explicitly hid the element).
    // `Set` at any level wins. `Inherit` falls through.
    for e in chain {
        match e {
            Element::Set(v) => return Some(v),
            Element::Blank => return None,
            Element::Inherit => continue,
        }
    }
    root
}
