//! The top-level [`Theme`] struct + [`ThemePart`] sparse mirror.
//!
//! `Theme` aggregates every theme slot — the palette, root elements,
//! plot / panel / axis / legend / strip chrome, and a `legend_variants`
//! map for named per-legend overrides. `ThemePart` is its sparse
//! counterpart — every field is `Option<>` (or `Element::Inherit` for
//! the element wrappers) — used to express partial overrides on a
//! per-plot basis.

use std::collections::HashMap;
use std::sync::Arc;

use super::axis::PerAxis;
use super::cascade::{PerChannel, Sided};
use super::element::{AlignTo, Element, LineElement, RectElement, TextElement};
use super::geom::GeomTheme;
use super::legend::LegendTheme;
use super::length::{Length, Margin};
use super::palette::{Palette, ThemeColor};

/// Default gap between stacked legends on the same plot side, pt.
/// Shared by [`Theme::default`] (which wraps it in `Length::Abs`) and
/// any chrome site that needs the bottom-of-cascade parent value to
/// resolve a `Length::Rel`.
pub const DEFAULT_LEGEND_SPACING_PT: f64 = 10.0;

/// Default gap between the panel-facing edge of the legend slot and
/// the legend's outer block, pt. Separate from
/// [`DEFAULT_LEGEND_SPACING_PT`]: this is the panel ↔ legend gap, that
/// one is legend ↔ legend.
pub const DEFAULT_LEGEND_GAP_PT: f64 = 10.0;

/// The full theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Semantic color anchors. Every chrome color resolves through
    /// this.
    pub palette: Palette,

    // ── Root elements — inherited by every typed sub-element ───────
    /// Root text styling.
    pub text: TextElement,
    /// Root line styling.
    pub line: LineElement,
    /// Root rect styling.
    pub rect: RectElement,

    // ── Plot-level chrome ──────────────────────────────────────────
    /// Plot title text (above the panel).
    pub plot_title: Element<TextElement>,
    /// Plot subtitle text (below the title).
    pub plot_subtitle: Element<TextElement>,
    /// Plot caption text (below the panel).
    pub plot_caption: Element<TextElement>,
    /// Which region the plot-level text slots (title, subtitle,
    /// caption) align to:
    /// - [`AlignTo::Panel`] — title / subtitle / caption span the
    ///   panel column only, so a centered title sits over the
    ///   plotting area regardless of left-axis chrome width.
    /// - [`AlignTo::Plot`] — they span the full plot interior
    ///   (everything inside `plot_margin` + `plot_padding`,
    ///   including axis chrome and legends), so a centered title
    ///   sits over the whole figure.
    ///
    /// Drives all three text slots as a unit — ggplot2's
    /// `plot.title.position` model.
    pub plot_text_align_to: AlignTo,
    /// Plot background rect — fills the entire plot area behind every
    /// other element.
    pub plot_background: Element<RectElement>,
    /// Margin around the plot's outer edge. Sizes the patch anatomy's
    /// outermost ring of tracks; sits **outside** [`Self::plot_background`].
    pub plot_margin: Margin,
    /// Padding inside the plot background, between the background's
    /// edge and the start of chrome (title, axes, legends). Sizes the
    /// second-from-outermost ring of tracks; sits **inside**
    /// [`Self::plot_background`].
    pub plot_padding: Margin,

    // ── Panel chrome ───────────────────────────────────────────────
    /// Panel background — the plotting area's fill.
    pub panel_background: Element<RectElement>,
    /// Panel border — outline drawn around the panel. Fill is ignored.
    pub panel_border: Element<RectElement>,
    /// Major grid lines, per channel.
    pub panel_grid_major: PerChannel<LineElement>,
    /// Minor grid lines, per channel.
    pub panel_grid_minor: PerChannel<LineElement>,

    // ── Axis chrome ────────────────────────────────────────────────
    /// Per-(channel, side) axis theming. Cascade walks
    /// `by_channel_side[ch][side]` → `by_channel[ch]` → `all`,
    /// per `AxisTheme` field independently.
    pub axis: PerAxis,

    // ── Legend chrome ──────────────────────────────────────────────
    /// Default legend theme.
    pub legend: LegendTheme,
    /// Named legend variants. A `Legend` can opt into one via
    /// `Legend::theme_variant("name")`; the legend resolves through
    /// that variant instead of `theme.legend`.
    pub legend_variants: HashMap<String, LegendTheme>,
    /// Gap between stacked legends on the same plot side.
    pub legend_spacing: Length,
    /// Gap between the panel-facing edge of the legend's slot and the
    /// legend block. Distinct from [`Self::legend_spacing`] (inter-
    /// legend) so users can tighten one without changing the other.
    pub legend_gap: Length,

    // ── Strip chrome (facet labels) ────────────────────────────────
    /// Strip background rect, per (channel, side).
    pub strip_background: Sided<RectElement>,
    /// Strip label text, per (channel, side).
    pub strip_text: Sided<TextElement>,
    /// Inner padding inside the strip rect.
    pub strip_padding: Margin,

    // ── Geom defaults ──────────────────────────────────────────────
    /// Per-geom default style values. Each geom reads from this when
    /// a channel binding doesn't supply the value.
    pub geom: GeomTheme,
}

impl Default for Theme {
    /// Defaults match the pre-theme visual output exactly. Existing
    /// examples produce byte-identical PNGs against their baseline
    /// after this struct lands.
    fn default() -> Self {
        use super::axis::axis_concrete_defaults;
        use super::element::{
            line_concrete_defaults, rect_concrete_defaults, text_concrete_defaults, HAlign, VAlign,
        };
        use super::font::{FontSpec, FontWeight};

        let palette = Palette::default();

        // Root text / line / rect — fully populated so every
        // downstream override has a concrete parent to fall through
        // to. Sparse overrides (theme.plot_title, axis.text, etc.)
        // cascade through these roots, then ultimately through the
        // per-type concrete-default constants.
        let text = text_concrete_defaults();
        let line = line_concrete_defaults();
        let rect = rect_concrete_defaults();

        Theme {
            palette,
            text,
            line,
            rect,

            // Plot-level: title 16pt bold, subtitle 12pt, caption 10pt
            // italic, no plot background (the panel background is what
            // shows).
            plot_title: Element::Set(TextElement {
                size_pt: Some(Length::Abs(16.0)),
                font: FontSpec {
                    weight: Some(FontWeight::BOLD),
                    ..FontSpec::default()
                },
                align: Some(HAlign::Center),
                valign: Some(VAlign::Middle),
                ..TextElement::default()
            }),
            plot_subtitle: Element::Set(TextElement {
                size_pt: Some(Length::Abs(12.0)),
                ..TextElement::default()
            }),
            plot_caption: Element::Set(TextElement {
                size_pt: Some(Length::Abs(10.0)),
                font: FontSpec {
                    style: Some(super::font::FontStyle::Italic),
                    ..FontSpec::default()
                },
                ..TextElement::default()
            }),
            plot_text_align_to: AlignTo::default(),
            plot_background: Element::Blank,
            plot_margin: Margin::ZERO,
            plot_padding: Margin::ZERO,

            // Panel: rgb(.95) paper fill, ink border at 1pt, major
            // grid mixed 22% toward ink, minor 12% toward ink at
            // 0.5pt.
            panel_background: Element::Set(RectElement {
                fill: Some(ThemeColor::Paper),
                color: Some(ThemeColor::Ink),
                linewidth_pt: Some(Length::Abs(1.0)),
                ..RectElement::default()
            }),
            panel_border: Element::Set(RectElement {
                fill: None,
                color: Some(ThemeColor::Ink),
                linewidth_pt: Some(Length::Abs(1.0)),
                ..RectElement::default()
            }),
            // Grid colors expressed as palette mixes so `invert()`
            // produces a sensible dark-mode grid automatically. Light
            // theme: major lands ~rgb(0.779), minor ~rgb(0.874), close
            // to the historical 0.78 / 0.88 values.
            panel_grid_major: PerChannel::new(LineElement {
                color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.18)),
                linewidth_pt: Some(Length::Abs(0.5)),
                ..LineElement::default()
            }),
            panel_grid_minor: PerChannel::new(LineElement {
                color: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.08)),
                linewidth_pt: Some(Length::Abs(0.5)),
                ..LineElement::default()
            }),

            // Axis: root tuned to today's 4pt major / 2pt minor
            // ticks, 2pt label gap, 10pt labels, 12pt title. Per-
            // axis / per-side slots default to all-`None` so they
            // cascade through this root.
            axis: PerAxis::new(axis_concrete_defaults()),

            // Legend: defaults shipped with LegendTheme already match
            // the existing legend visual surface.
            legend: LegendTheme::default(),
            legend_variants: HashMap::new(),
            legend_spacing: Length::Abs(DEFAULT_LEGEND_SPACING_PT),
            legend_gap: Length::Abs(DEFAULT_LEGEND_GAP_PT),

            // Strips: default to inheriting from rect / text roots,
            // with a small inner padding so labels don't butt
            // against the strip edges.
            strip_background: Sided::new(RectElement {
                fill: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.10)),
                color: Some(ThemeColor::Ink),
                linewidth_pt: Some(Length::Abs(0.5)),
                ..RectElement::default()
            }),
            strip_text: Sided::new(TextElement {
                size_pt: Some(Length::Abs(10.0)),
                ..TextElement::default()
            }),
            strip_padding: Margin::all(Length::Abs(4.0)),
            geom: GeomTheme::default(),
        }
    }
}

impl Theme {
    /// Construct a theme from explicit fields. Most callers should
    /// start with `Theme::default()` and modify what they need.
    #[inline]
    pub fn new() -> Self {
        Theme::default()
    }

    /// Swap `paper` and `ink` in the palette. Every element that
    /// references them (chrome, grids, text) inverts in one
    /// operation. `Theme::dark()` is `Theme::default().invert()`.
    pub fn invert(mut self) -> Self {
        std::mem::swap(&mut self.palette.paper, &mut self.palette.ink);
        self
    }

    /// Replace the palette wholesale. Element references re-resolve
    /// at next render.
    pub fn with_palette(mut self, palette: Palette) -> Self {
        self.palette = palette;
        self
    }

    /// Register a named legend variant. A `Legend` can opt into the
    /// variant via `Legend::theme_variant("name")`.
    pub fn with_legend_variant(mut self, name: impl Into<String>, variant: LegendTheme) -> Self {
        self.legend_variants.insert(name.into(), variant);
        self
    }

    /// Apply a [`ThemePart`] override onto self, returning a new
    /// `Theme`. `Some(...)` / `Set(...)` fields on `part` win;
    /// `None` / `Inherit` fields keep `self`'s value.
    pub fn merge(&self, part: &ThemePart) -> Theme {
        let mut out = self.clone();
        part.apply(&mut out);
        out
    }

    /// Resolve the [`LegendTheme`] for a legend that opted into the
    /// given variant name. Falls back to the default `legend` when
    /// the variant isn't registered.
    pub fn legend_for(&self, variant: Option<&str>) -> &LegendTheme {
        variant
            .and_then(|name| self.legend_variants.get(name))
            .unwrap_or(&self.legend)
    }
}

/// Sparse mirror of [`Theme`] — every field is optional. Used for
/// per-`Plot` overrides applied on top of the composition's theme at
/// render time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemePart {
    /// Optional palette override (replaces wholesale).
    pub palette: Option<Palette>,
    /// Optional root-text override.
    pub text: Option<TextElement>,
    /// Optional root-line override.
    pub line: Option<LineElement>,
    /// Optional root-rect override.
    pub rect: Option<RectElement>,

    /// Optional plot-title override.
    pub plot_title: Option<Element<TextElement>>,
    /// Optional plot-subtitle override.
    pub plot_subtitle: Option<Element<TextElement>>,
    /// Optional plot-caption override.
    pub plot_caption: Option<Element<TextElement>>,
    /// Optional plot-text-align-to override.
    pub plot_text_align_to: Option<AlignTo>,
    /// Optional plot-background override.
    pub plot_background: Option<Element<RectElement>>,
    /// Optional plot-margin override.
    pub plot_margin: Option<Margin>,
    /// Optional plot-padding override.
    pub plot_padding: Option<Margin>,

    /// Optional panel-background override.
    pub panel_background: Option<Element<RectElement>>,
    /// Optional panel-border override.
    pub panel_border: Option<Element<RectElement>>,
    /// Optional major-grid override.
    pub panel_grid_major: Option<PerChannel<LineElement>>,
    /// Optional minor-grid override.
    pub panel_grid_minor: Option<PerChannel<LineElement>>,

    /// Optional axis override.
    pub axis: Option<PerAxis>,

    /// Optional legend override.
    pub legend: Option<LegendTheme>,
    /// Optional named legend variants — merged into the existing map
    /// (a variant key in `part` replaces the entry of the same name
    /// in `self`).
    pub legend_variants: HashMap<String, LegendTheme>,
    /// Optional legend-spacing override.
    pub legend_spacing: Option<Length>,
    /// Optional legend-gap override (panel ↔ legend rail).
    pub legend_gap: Option<Length>,

    /// Optional strip-background override.
    pub strip_background: Option<Sided<RectElement>>,
    /// Optional strip-text override.
    pub strip_text: Option<Sided<TextElement>>,
    /// Optional strip-padding override.
    pub strip_padding: Option<Margin>,

    /// Optional geom defaults override (replaces the whole
    /// `GeomTheme` wholesale).
    pub geom: Option<GeomTheme>,
}

impl ThemePart {
    /// Apply this override in place onto `theme`. Set fields on
    /// `self` win; unset fields leave `theme` untouched.
    pub fn apply(&self, theme: &mut Theme) {
        macro_rules! set_field {
            ($name:ident) => {
                if let Some(ref v) = self.$name {
                    theme.$name = v.clone();
                }
            };
        }
        set_field!(palette);
        set_field!(text);
        set_field!(line);
        set_field!(rect);
        set_field!(plot_title);
        set_field!(plot_subtitle);
        set_field!(plot_caption);
        set_field!(plot_text_align_to);
        set_field!(plot_background);
        set_field!(plot_margin);
        set_field!(plot_padding);
        set_field!(panel_background);
        set_field!(panel_border);
        set_field!(panel_grid_major);
        set_field!(panel_grid_minor);
        set_field!(axis);
        set_field!(legend);
        set_field!(legend_spacing);
        set_field!(legend_gap);
        set_field!(strip_background);
        set_field!(strip_text);
        set_field!(strip_padding);
        set_field!(geom);
        for (k, v) in &self.legend_variants {
            theme.legend_variants.insert(k.clone(), v.clone());
        }
    }
}

/// `Arc<Theme>` is the standard shape for the orchestrator's
/// theme handle — cheap clone, read-only at draw time.
pub type SharedTheme = Arc<Theme>;
