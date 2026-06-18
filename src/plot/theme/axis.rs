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
use super::font::FontSpec;
use super::length::Length;
use super::palette::ThemeColor;
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

/// Sparse axis theme — every field overrides the parent layer
/// when set, falls through when `None` / `Inherit`.
///
/// `Element<T>` fields use `Inherit` to mean "no opinion" (since
/// the variant already encodes the three-way Inherit / Blank / Set
/// distinction); plain typed fields wrap in `Option<...>` so partial
/// overrides cascade per-field without clobbering the rest. After
/// the three-layer cascade in [`PerAxis::resolve`], any remaining
/// `None` falls back to [`axis_concrete_defaults`].
#[derive(Debug, Clone, Default, PartialEq)]
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
    pub tick_length: Option<Length>,
    /// Minor-tick length. Sign behaves the same way as
    /// `tick_length`.
    pub tick_length_minor: Option<Length>,
    /// Gap between the end of a tick and the near edge of its label
    /// (always positive — labels sit on the side the tick extends
    /// to).
    pub tick_gap: Option<Length>,
    /// Where the axis title sits relative to the panel.
    pub title_location: Option<TitleLocation>,
}

/// Concrete fallback values for an `AxisTheme` — 4pt major ticks,
/// 2pt minor, 2pt label gap, `TitleLocation::Outside`, plus 12pt
/// `Along`-rotated title and 10pt tick labels. Used as the safety
/// net for any field still `None` after the three-layer cascade.
pub fn axis_concrete_defaults() -> AxisTheme {
    AxisTheme {
        // Axis titles read along the axis baseline — `Along` lets
        // the chrome resolve to 0° on Top / Bottom and 90° on
        // Left / Right (text reads up the column) without the
        // renderer special-casing vertical sides.
        title: Element::Set(TextElement {
            size_pt: Some(Length::Abs(12.0)),
            angle: Some(Rotation::Along),
            color: Some(ThemeColor::Ink),
            font: FontSpec::default(),
            ..TextElement::default()
        }),
        text: Element::Set(TextElement {
            size_pt: Some(Length::Abs(10.0)),
            color: Some(ThemeColor::Ink),
            font: FontSpec::default(),
            ..TextElement::default()
        }),
        line: Element::Set(super::element::line_concrete_defaults()),
        ticks: Element::Set(super::element::line_concrete_defaults()),
        ticks_minor: Element::Set(super::element::line_concrete_defaults()),
        tick_length: Some(Length::Abs(4.0)),
        tick_length_minor: Some(Length::Abs(2.0)),
        tick_gap: Some(Length::Abs(2.0)),
        title_location: Some(TitleLocation::Outside),
    }
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
    pub by_channel: [AxisTheme; 2],
    /// Per-(channel, side) override (sparse, most specific).
    pub by_channel_side: [[AxisTheme; 2]; 2],
}

impl PerAxis {
    /// Construct with `all = root` and every override slot empty.
    pub fn new(root: AxisTheme) -> Self {
        Self {
            all: root,
            by_channel: [AxisTheme::default(), AxisTheme::default()],
            by_channel_side: [
                [AxisTheme::default(), AxisTheme::default()],
                [AxisTheme::default(), AxisTheme::default()],
            ],
        }
    }

    /// Resolve every `AxisTheme` field for `(ch, side)`, returning
    /// concrete values. Walks `by_channel_side[ch][side]` →
    /// `by_channel[ch]` → `all` → [`axis_concrete_defaults`],
    /// per-field.
    pub fn resolve(&self, ch: u8, side: u8) -> ResolvedAxis {
        let ci = ch as usize;
        let si = side as usize;
        debug_assert!(ci < 2 && si < 2, "channel/side out of range: {ch}, {side}");

        let defaults = axis_concrete_defaults();
        let root_text = self.all.text.as_set();
        let root_line = self.all.line.as_set();

        let by_ch = &self.by_channel[ci];
        let by_cs = &self.by_channel_side[ci][si];

        let title = cascade_element_chain(
            [&by_cs.title, &by_ch.title, &self.all.title],
            defaults.title.as_set(),
            root_text,
        );
        let text = cascade_element_chain(
            [&by_cs.text, &by_ch.text, &self.all.text],
            defaults.text.as_set(),
            None,
        );
        let line = cascade_line_chain(
            [&by_cs.line, &by_ch.line, &self.all.line],
            defaults.line.as_set(),
            None,
        );
        let ticks = cascade_line_chain(
            [&by_cs.ticks, &by_ch.ticks, &self.all.ticks],
            defaults.ticks.as_set(),
            root_line,
        );
        let ticks_minor = cascade_line_chain(
            [
                &by_cs.ticks_minor,
                &by_ch.ticks_minor,
                &self.all.ticks_minor,
            ],
            defaults.ticks_minor.as_set(),
            root_line,
        );

        let tick_length = by_cs
            .tick_length
            .or(by_ch.tick_length)
            .or(self.all.tick_length)
            .or(defaults.tick_length)
            .expect("axis_concrete_defaults sets tick_length");
        let tick_length_minor = by_cs
            .tick_length_minor
            .or(by_ch.tick_length_minor)
            .or(self.all.tick_length_minor)
            .or(defaults.tick_length_minor)
            .expect("axis_concrete_defaults sets tick_length_minor");
        let tick_gap = by_cs
            .tick_gap
            .or(by_ch.tick_gap)
            .or(self.all.tick_gap)
            .or(defaults.tick_gap)
            .expect("axis_concrete_defaults sets tick_gap");
        let title_location = by_cs
            .title_location
            .or(by_ch.title_location)
            .or(self.all.title_location)
            .or(defaults.title_location)
            .expect("axis_concrete_defaults sets title_location");

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
/// Returned by [`PerAxis::resolve`]; elements are owned (cascaded
/// through every layer including the type's concrete fallback) so
/// callers can read each field without re-walking the chain.
///
/// `Length` fields remain unresolved here — they're resolved against
/// the relevant parent at draw time (text size against base text,
/// tick length against the line root, etc.).
#[derive(Debug, Clone)]
pub struct ResolvedAxis {
    /// Axis title element, or `None` if Blank.
    pub title: Option<TextElement>,
    /// Tick label element, or `None`.
    pub text: Option<TextElement>,
    /// Baseline element, or `None`.
    pub line: Option<LineElement>,
    /// Major tick element, or `None`.
    pub ticks: Option<LineElement>,
    /// Minor tick element, or `None`.
    pub ticks_minor: Option<LineElement>,
    /// Major-tick length (sign flips direction).
    pub tick_length: Length,
    /// Minor-tick length (sign flips direction).
    pub tick_length_minor: Length,
    /// Gap between tick end and label near-edge.
    pub tick_gap: Length,
    /// Axis-title placement.
    pub title_location: TitleLocation,
}

/// Walk the cascade chain for a `TextElement` slot and merge into
/// a single owned `TextElement`. `Blank` at any level short-
/// circuits to `None`. `Set` accumulates into the running merged
/// element; `Inherit` skips. After the chain, the default-axis
/// element is the next fallback, then any cross-element root (e.g.
/// `theme.axis.all.text` as the root for `theme.axis.all.title`).
fn cascade_element_chain<const N: usize>(
    chain: [&Element<TextElement>; N],
    axis_default: Option<&TextElement>,
    extra_root: Option<&TextElement>,
) -> Option<TextElement> {
    // Walk most-specific to least-specific. The first Blank short-
    // circuits to None; Set values merge child-over-parent via
    // TextElement::cascade. Inherit just skips that layer.
    let mut merged: Option<TextElement> = None;
    for e in chain {
        match e {
            Element::Blank => return None,
            Element::Set(v) => {
                merged = Some(match merged {
                    Some(m) => m.cascade(v),
                    None => v.clone(),
                });
            }
            Element::Inherit => {}
        }
    }
    if let Some(d) = axis_default {
        merged = Some(match merged {
            Some(m) => m.cascade(d),
            None => d.clone(),
        });
    }
    if let Some(r) = extra_root {
        merged = Some(match merged {
            Some(m) => m.cascade(r),
            None => r.clone(),
        });
    }
    merged
}

/// Same as [`cascade_element_chain`] but for `LineElement` slots.
fn cascade_line_chain<const N: usize>(
    chain: [&Element<LineElement>; N],
    axis_default: Option<&LineElement>,
    extra_root: Option<&LineElement>,
) -> Option<LineElement> {
    let mut merged: Option<LineElement> = None;
    for e in chain {
        match e {
            Element::Blank => return None,
            Element::Set(v) => {
                merged = Some(match merged {
                    Some(m) => m.cascade(v),
                    None => v.clone(),
                });
            }
            Element::Inherit => {}
        }
    }
    if let Some(d) = axis_default {
        merged = Some(match merged {
            Some(m) => m.cascade(d),
            None => d.clone(),
        });
    }
    if let Some(r) = extra_root {
        merged = Some(match merged {
            Some(m) => m.cascade(r),
            None => r.clone(),
        });
    }
    merged
}
