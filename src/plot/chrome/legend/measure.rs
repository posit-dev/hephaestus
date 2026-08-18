//! Space reservation for legends.
//!
//! [`LegendMeasure`] pre-shapes one legend — labels, title, and the
//! body's own geometry — and reports the block's primary (column width
//! / row height) and cross extents to the layout solver.
//! [`LegendStackMeasure`] composes several of them for one side.
//!
//! Building a measure does the work the draw pass would otherwise
//! repeat: it shapes every label and, for a discrete stack, solves the
//! whole inner grid. The draw pass takes the finished measure, so
//! reserved space and drawn space come from one computation.

use crate::layout::{Measure, WidthHint};
use crate::plot::chrome::linear_axis::pt_to_px;
use crate::plot::chrome::text::ChromeRun;
use crate::plot::scale::ScaleRegistry;
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::chrome::LegendSide;
use crate::scales::value::Value;
use crate::shape::ShapeRegistry;

use super::colorbar::{bin_edges, bin_midpoints};
use super::layout::{
    build_discrete_stack_layout, DiscreteStackLayout, LabelMeasure, SwatchCellMeasure,
};
use super::render_keys::swatch_dim_for;
use super::spec::{resolve_key, Legend, LegendBody};
use super::{cardinal_side, legend_text_elements};

/// Everything the layout solver needs to reserve a slot for one
/// legend, pre-shaped against the theme it will be drawn with.
pub(crate) struct LegendMeasure {
    side: LegendSide,
    /// Pre-solved geometry for whichever body the legend carries —
    /// the draw pass reads its own variant back out.
    pub(super) body: BodyMeasure,
    /// Shaped label dims, max across breaks.
    max_label_w_px: f64,
    max_label_h_px: f64,
    pub(super) title_w_px: f64,
    pub(super) title_h_px: f64,
    /// Number of non-null breaks the domain scale produces.
    entry_count: usize,
    // ── Layout sizes resolved from the LegendTheme at construction
    // so the `Measure` trait impl can use them without needing
    // `&LegendTheme` at width_hint/height_at call time.
    /// Inner padding in px (uniform — uses `lt.padding.left`).
    pub(super) padding_px: f64,
    /// Gap between adjacent keys in a single legend, px.
    pub(super) row_gap_px: f64,
    /// Gap between a key swatch and its label, px. Same value the
    /// legend's axis renders for tick → label gap — pre-resolved
    /// here only because [`build_discrete_stack_layout`] needs it as
    /// a `Fixed` track gap during construction.
    swatch_label_gap_px: f64,
    /// The legend's axis chrome — kept here so the binned + colorbar
    /// primary dim can ask the axis for its own thickness instead of
    /// re-summing tick / gap arms by hand. Cloned at construction
    /// because the `Measure` trait impl outlives `&LegendTheme`.
    axis: crate::plot::theme::AxisTheme,
    /// Gap between the panel-facing slot edge and the legend's outer
    /// block, px. Inflates the primary dim reservation; the renderer
    /// shrinks the slot rect on the panel-facing side by the same
    /// amount before drawing.
    legend_gap_px: f64,
}

pub(super) enum BodyMeasure {
    /// Discrete stack — entries area pre-solved by the layout grid.
    /// The grid handles per-row auto sizing for free: vertical legend
    /// rows pick the widest swatch as the uniform column width while
    /// each row's height tracks its own marker, horizontal legends
    /// transpose. `no_keys` short-circuits the measure to zero when
    /// the stack is empty.
    Stack {
        layout: DiscreteStackLayout,
        no_keys: bool,
    },
    /// Binned stack — N-bins-from-N+1-breaks layout with a between-row
    /// tick rail. `row_cells_px` holds one entry per **bin**, sized at
    /// the bin's midpoint so it matches what the renderer paints
    /// there; binned rows touch, so the row gap doesn't apply. Bin
    /// count and per-bin dimensions are independent of
    /// [`BinSpacing`](super::BinSpacing) — spacing only redistributes
    /// bins along the finished bar. An empty `row_cells_px` means the
    /// domain yields no bins and the body draws nothing. `no_keys`
    /// short-circuits the measure to zero when the stack is empty.
    BinnedStack {
        row_cells_px: Vec<(f64, f64)>,
        no_keys: bool,
    },
    /// Colorbar — `bar_thickness_px` is the bar's perpendicular
    /// extent; `samples` is forwarded to the renderer.
    Colorbar {
        bar_thickness_px: f64,
        samples: usize,
    },
}

impl LegendMeasure {
    /// Shape a legend against the theme and registries it will be
    /// drawn with. `shapes` has to be the registry the draw pass
    /// resolves markers through for the reserved cells to match the
    /// markers' bounds.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        legend: &Legend,
        registry: &ScaleRegistry,
        shapes: &ShapeRegistry,
        dpi: f64,
        lt: &crate::plot::theme::LegendTheme,
        theme: &crate::plot::theme::Theme,
        geom: &crate::plot::theme::GeomTheme,
        legend_gap_px: f64,
        locale: &crate::scales::Locale,
        root_pt: f64,
    ) -> Self {
        // Label + title styles come from the LegendTheme so measure
        // and draw size identical glyph runs. `Blank` short-circuits
        // to `None` — the corresponding cell reserves zero space
        // because the renderer won't draw it. The markdown context
        // resolves here too: a slot that parses its text has to be
        // measured through the same pipeline that paints it.
        let (title_el, label_el) = legend_text_elements(lt, &theme.text);
        let label_rich = label_el
            .as_ref()
            .and_then(|el| crate::plot::chrome::text::rich_chrome_for(el, theme, dpi));
        let title_rich = title_el
            .as_ref()
            .and_then(|el| crate::plot::chrome::text::rich_chrome_for(el, theme, dpi));
        let label_style = label_el
            .as_ref()
            .map(|el| crate::plot::chrome::text::text_style_from(el, root_pt));
        let title_style = title_el
            .as_ref()
            .map(|el| crate::plot::chrome::text::text_style_from(el, root_pt));
        let domain = registry.get(&legend.domain_scale);
        let breaks = domain
            .map(|s| s.breaks(DEFAULT_BREAK_COUNT))
            .unwrap_or_default();

        let mut entry_count = 0usize;
        let mut max_label_w: f64 = 0.0;
        let mut max_label_h: f64 = 0.0;
        for v in &breaks {
            if matches!(v, Value::Null) {
                continue;
            }
            entry_count += 1;
            // Blank labels render nothing → no reserved label box.
            let Some(label_style) = label_style.as_ref() else {
                continue;
            };
            let label = domain.map(|s| s.format(v, locale)).unwrap_or_default();
            let run = ChromeRun::shape(&label, label_style, dpi, label_rich.as_ref());
            let h = run.line_box_height();
            // Labels render unwrapped, so the slot needs the full
            // single-line width — `width_hint` returns the
            // longest-unbreakable-cluster bound (one word), which
            // undershoots multi-word labels and clips them at draw.
            let w = run.width();
            max_label_w = max_label_w.max(w);
            max_label_h = max_label_h.max(h);
        }

        // Resolve LegendTheme layout sizes the construction step needs
        // as px. `swatch_label_gap_px` is pulled forward of the body
        // match so the grid build below sees a `Fixed` gap track.
        let padding_px = pt_to_px(lt.padding.left.resolve(0.0), dpi);
        let row_gap_px = pt_to_px(lt.key.spacing.resolve(0.0), dpi);
        let swatch_label_gap_px = pt_to_px(lt.axis.resolved().tick_gap.resolve(0.0), dpi);

        let body = match &legend.body {
            LegendBody::Stack(stack) if stack.binned => {
                let key_w_floor = pt_to_px(lt.key.width.resolve(0.0), dpi);
                let key_h_floor = pt_to_px(lt.key.height.resolve(0.0), dpi);
                // One row per bin, sized from the bin midpoint — the
                // value `render_binned_stack_body` resolves its keys
                // at — so reserved bar length and thickness match the
                // draw. Sampling the edges instead would yield one
                // row too many and size them off values no bin uses.
                let row_cells_px: Vec<(f64, f64)> = bin_midpoints(&bin_edges(&breaks))
                    .iter()
                    .map(|v| {
                        let (mut row_w, mut row_h) = (0.0_f64, 0.0_f64);
                        for key in &stack.keys {
                            let resolved = resolve_key(key, registry, v);
                            let (w, h) =
                                swatch_dim_for(key.kind, &resolved, dpi, geom, shapes, theme);
                            row_w = row_w.max(w);
                            row_h = row_h.max(h);
                        }
                        (row_w.max(key_w_floor), row_h.max(key_h_floor))
                    })
                    .collect();
                BodyMeasure::BinnedStack {
                    row_cells_px,
                    no_keys: stack.keys.is_empty(),
                }
            }
            LegendBody::Stack(stack) => {
                // Build the discrete-stack layout via the grid solver.
                // Each row contributes a SwatchCellMeasure (max of its
                // keys' intrinsic dims, floored at KeyTheme) and a
                // LabelMeasure (natural single-line width / height).
                // The grid's Auto tracks resolve to content, giving
                // per-row auto sizing with cross-axis uniformity.
                let key_w_floor = pt_to_px(lt.key.width.resolve(0.0), dpi);
                let key_h_floor = pt_to_px(lt.key.height.resolve(0.0), dpi);
                let horizontal = matches!(
                    cardinal_side(legend.side),
                    LegendSide::Top | LegendSide::Bottom
                );
                let mut rows: Vec<(SwatchCellMeasure, LabelMeasure)> = Vec::new();
                for v in breaks.iter().filter(|v| !matches!(v, Value::Null)) {
                    let (mut row_w, mut row_h) = (0.0_f64, 0.0_f64);
                    for key in &stack.keys {
                        let resolved = resolve_key(key, registry, v);
                        let (w, h) = swatch_dim_for(key.kind, &resolved, dpi, geom, shapes, theme);
                        row_w = row_w.max(w);
                        row_h = row_h.max(h);
                    }
                    let swatch = SwatchCellMeasure {
                        intrinsic_w_px: row_w,
                        intrinsic_h_px: row_h,
                        floor_w_px: key_w_floor,
                        floor_h_px: key_h_floor,
                    };
                    // Blank labels → zero-extent LabelMeasure (no
                    // draw, no reservation).
                    let label = match label_style.as_ref() {
                        Some(style) => {
                            let label_text =
                                domain.map(|s| s.format(v, locale)).unwrap_or_default();
                            let run =
                                ChromeRun::shape(&label_text, style, dpi, label_rich.as_ref());
                            let nat_h = run.line_box_height();
                            let nat_w = run.width();
                            LabelMeasure {
                                natural_w_px: nat_w,
                                natural_h_px: nat_h,
                            }
                        }
                        None => LabelMeasure {
                            natural_w_px: 0.0,
                            natural_h_px: 0.0,
                        },
                    };
                    rows.push((swatch, label));
                }
                let layout = build_discrete_stack_layout(
                    horizontal,
                    rows,
                    swatch_label_gap_px,
                    row_gap_px,
                    dpi,
                );
                BodyMeasure::Stack {
                    layout,
                    no_keys: stack.keys.is_empty(),
                }
            }
            LegendBody::Colorbar(spec) => BodyMeasure::Colorbar {
                bar_thickness_px: pt_to_px(lt.bar.width.resolve(0.0), dpi),
                samples: spec.samples.max(2),
            },
        };

        let (title_w_px, title_h_px) = match (&legend.title, title_style.as_ref()) {
            (Some(text), Some(style)) if !text.is_empty() => {
                let run = ChromeRun::shape(text, style, dpi, title_rich.as_ref());
                let h = run.line_box_height();
                // Titles render unwrapped — the natural width is the
                // actual draw width; `width_hint` would undershoot
                // for multi-word titles like "Category (hero)".
                let w = run.width();
                (w, h)
            }
            _ => (0.0, 0.0),
        };

        LegendMeasure {
            // Store the cardinal layout direction so primary/cross
            // dim matches can use a 4-arm pattern. In-panel legends
            // size themselves the same as a Right legend.
            side: cardinal_side(legend.side),
            body,
            entry_count,
            max_label_w_px: max_label_w,
            max_label_h_px: max_label_h,
            title_w_px,
            title_h_px,
            padding_px,
            row_gap_px,
            swatch_label_gap_px,
            axis: lt.axis.clone(),
            legend_gap_px,
        }
    }

    /// True when the legend draws nothing, so the layout reserves no
    /// slot and the renderer returns early.
    pub(super) fn is_empty(&self) -> bool {
        if self.entry_count == 0 {
            return true;
        }
        // A binned body with no bins reserves nothing: fewer than two
        // finite numeric breaks (a discrete domain, a collapsed span)
        // leaves `render_binned_stack_body` with nothing to draw, so
        // reserving space would leave a blank band.
        if matches!(&self.body, BodyMeasure::BinnedStack { row_cells_px, .. } if row_cells_px.is_empty())
        {
            return true;
        }
        matches!(
            self.body,
            BodyMeasure::Stack { no_keys: true, .. }
                | BodyMeasure::BinnedStack { no_keys: true, .. }
        )
    }

    /// Block dimension along the side's primary axis: column width
    /// for Right/Left, row height for Top/Bottom. Includes
    /// [`Self::legend_gap_px`] — the renderer shrinks the slot by the
    /// same amount on the panel-facing edge before drawing.
    pub(super) fn primary_dim_px(&self, dpi: f64) -> f64 {
        let padding = self.padding_px;
        let title_gap = if self.title_h_px > 0.0 {
            self.row_gap_px
        } else {
            0.0
        };
        // Bar-style bodies (binned + colorbar) carry an axis arm:
        // bar thickness + tick + gap + label. Tick + gap come from
        // the legend's own AxisTheme — same source the legend's axis
        // renderer reads, so the slot reservation matches the draw.
        // Reservation uses the tick magnitude — sign flips draw
        // direction but the slot fits the tick either way.
        let axis_resolved = self.axis.resolved();
        let tick_px = pt_to_px(axis_resolved.tick_length.resolve(0.0), dpi).abs();
        let label_gap_axis = pt_to_px(axis_resolved.tick_gap.resolve(0.0), dpi);

        let body_dim = match (&self.body, self.side) {
            (BodyMeasure::Stack { layout, .. }, LegendSide::Right | LegendSide::Left) => {
                // Vertical legend column width comes straight from
                // the inner grid's resolved entries width.
                layout.entries_w_px.max(self.title_w_px) + 2.0 * padding
            }
            (BodyMeasure::Stack { layout, .. }, LegendSide::Top | LegendSide::Bottom) => {
                // Horizontal legend row height: title above + entries
                // block height.
                self.title_h_px + title_gap + layout.entries_h_px + 2.0 * padding
            }
            (
                BodyMeasure::BinnedStack { row_cells_px, .. },
                LegendSide::Right | LegendSide::Left,
            ) => {
                // Binned vertical: bar thickness = max of row widths.
                let thickness = row_cells_px.iter().map(|(w, _)| *w).fold(0.0_f64, f64::max);
                let axis_arm = thickness + tick_px + label_gap_axis + self.max_label_w_px;
                axis_arm.max(self.title_w_px) + 2.0 * padding
            }
            (
                BodyMeasure::BinnedStack { row_cells_px, .. },
                LegendSide::Top | LegendSide::Bottom,
            ) => {
                // Binned horizontal: bar thickness = max of row heights.
                let thickness = row_cells_px.iter().map(|(_, h)| *h).fold(0.0_f64, f64::max);
                self.title_h_px
                    + title_gap
                    + thickness
                    + tick_px
                    + label_gap_axis
                    + self.max_label_h_px
                    + 2.0 * padding
            }
            (
                BodyMeasure::Colorbar {
                    bar_thickness_px: thickness,
                    ..
                },
                LegendSide::Right | LegendSide::Left,
            ) => {
                let axis_arm = thickness + tick_px + label_gap_axis + self.max_label_w_px;
                axis_arm.max(self.title_w_px) + 2.0 * padding
            }
            (
                BodyMeasure::Colorbar {
                    bar_thickness_px: thickness,
                    ..
                },
                LegendSide::Top | LegendSide::Bottom,
            ) => {
                // Row height = title + gap + bar thickness + tick + gap + label_h.
                self.title_h_px
                    + title_gap
                    + thickness
                    + tick_px
                    + label_gap_axis
                    + self.max_label_h_px
                    + 2.0 * padding
            }
            (_, LegendSide::InPanel { .. }) => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        };
        body_dim + self.legend_gap_px
    }

    /// Cross-axis dim: height for Right/Left, width for Top/Bottom.
    /// Used by [`LegendStackMeasure`] to split the slot rect among
    /// stacked legends.
    pub(super) fn cross_dim_px(&self, dpi: f64) -> f64 {
        let _ = dpi;
        let padding = self.padding_px;
        let row_gap = self.row_gap_px;
        let gap = self.swatch_label_gap_px;
        let title_gap = if self.title_h_px > 0.0 {
            self.row_gap_px
        } else {
            0.0
        };
        let n = self.entry_count as f64;

        match (&self.body, self.side) {
            (BodyMeasure::Stack { layout, .. }, LegendSide::Right | LegendSide::Left) => {
                // Vertical legend along-axis: entries block height
                // already accounts for per-row heights + inter-row
                // gaps via the grid's Fixed gap rows.
                self.title_h_px + title_gap + layout.entries_h_px + 2.0 * padding
            }
            (BodyMeasure::Stack { layout, .. }, LegendSide::Top | LegendSide::Bottom) => {
                // Horizontal legend along-axis: entries block width.
                layout.entries_w_px.max(self.title_w_px) + 2.0 * padding
            }
            (
                BodyMeasure::BinnedStack { row_cells_px, .. },
                LegendSide::Right | LegendSide::Left,
            ) => {
                // Binned vertical: rows touch, bar length = sum of
                // each bin's own h.
                let bar_len: f64 = row_cells_px.iter().map(|(_, h)| *h).sum();
                self.title_h_px + title_gap + bar_len + 2.0 * padding
            }
            (
                BodyMeasure::BinnedStack { row_cells_px, .. },
                LegendSide::Top | LegendSide::Bottom,
            ) => {
                let bar_len: f64 = row_cells_px.iter().map(|(w, _)| *w).sum();
                bar_len.max(self.title_w_px) + 2.0 * padding
            }
            (BodyMeasure::Colorbar { .. }, LegendSide::Right | LegendSide::Left) => {
                // Vertical bar length defaults to (n−1) × label-pitch
                // + label height — enough to space the major ticks
                // legibly. The actual rendered length scales to the
                // available slot height at draw time.
                let pitch = self.max_label_h_px + row_gap;
                let bar_len = (n - 1.0).max(1.0) * pitch + self.max_label_h_px;
                self.title_h_px + title_gap + bar_len + 2.0 * padding
            }
            (BodyMeasure::Colorbar { .. }, LegendSide::Top | LegendSide::Bottom) => {
                // Horizontal bar length: (n−1) × label-pitch +
                // label_w to leave clear gaps between tick labels.
                let pitch = self.max_label_w_px + gap * 3.0;
                let bar_len = (n - 1.0).max(1.0) * pitch + self.max_label_w_px;
                bar_len.max(self.title_w_px) + 2.0 * padding
            }
            (_, LegendSide::InPanel { .. }) => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        }
    }
}

impl Measure for LegendMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        if self.is_empty() {
            return WidthHint::Min(0.0);
        }
        match self.side {
            LegendSide::Right | LegendSide::Left => WidthHint::Min(self.primary_dim_px(dpi)),
            LegendSide::Top | LegendSide::Bottom => WidthHint::Min(0.0),
            LegendSide::InPanel { .. } => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        }
    }

    fn height_at(&self, _width: f64, dpi: f64) -> f64 {
        if self.is_empty() {
            return 0.0;
        }
        match self.side {
            LegendSide::Top | LegendSide::Bottom => self.primary_dim_px(dpi),
            LegendSide::Right | LegendSide::Left => 0.0,
            LegendSide::InPanel { .. } => {
                unreachable!("LegendMeasure stores cardinal side, never InPanel")
            }
        }
    }
}

/// Composite measure for multiple legends stacked on the same side.
/// The primary extent is reserved for the *widest* child (so all
/// children get the same column width / row height); the cross
/// extent is the sum of children plus inter-legend gaps.
pub(super) struct LegendStackMeasure {
    pub(super) side: LegendSide,
    pub(super) children: Vec<LegendMeasure>,
    /// Resolved inter-legend gap in pixels — sourced from
    /// `Theme.legend_spacing` at construction; used by
    /// `cross_dim_for_layout` to size the slot for the stack +
    /// gaps. (Not currently consumed for the cross dim sizing
    /// because the layout reserves max(primary) and the renderer
    /// stacks children using this gap; the field exists so the
    /// renderer and measure share the same gap value.)
    #[allow(dead_code)]
    pub(super) gap_px: f64,
}

impl LegendStackMeasure {
    fn non_empty(&self) -> impl Iterator<Item = &LegendMeasure> {
        self.children.iter().filter(|c| !c.is_empty())
    }
    fn primary_max(&self, dpi: f64) -> f64 {
        self.non_empty()
            .map(|c| c.primary_dim_px(dpi))
            .fold(0.0_f64, f64::max)
    }
}

impl Measure for LegendStackMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        let any = self.non_empty().next().is_some();
        if !any {
            return WidthHint::Min(0.0);
        }
        match self.side {
            LegendSide::Right | LegendSide::Left => WidthHint::Min(self.primary_max(dpi)),
            LegendSide::Top | LegendSide::Bottom => WidthHint::Min(0.0),
            LegendSide::InPanel { .. } => {
                unreachable!("LegendStackMeasure is constructed with a cardinal side")
            }
        }
    }
    fn height_at(&self, _width: f64, dpi: f64) -> f64 {
        let any = self.non_empty().next().is_some();
        if !any {
            return 0.0;
        }
        match self.side {
            LegendSide::Top | LegendSide::Bottom => self.primary_max(dpi),
            LegendSide::Right | LegendSide::Left => 0.0,
            LegendSide::InPanel { .. } => {
                unreachable!("LegendStackMeasure is constructed with a cardinal side")
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb;
    use crate::plot::chrome::legend::{legend_measure, LegendKeySpec};
    use crate::plot::scale;
    use crate::plot::theme::Theme;
    use std::sync::Arc;

    fn dpi_96() -> f64 {
        96.0
    }

    fn default_theme() -> Theme {
        // Blank the key frame so tests count only emissions driven by
        // the key specs themselves, not the theme's swatch background.
        let mut t = Theme::default();
        t.legend.key.frame = crate::plot::theme::Element::Blank;
        t
    }

    fn shape_reg() -> ShapeRegistry {
        ShapeRegistry::with_builtins()
    }

    fn build_registry() -> ScaleRegistry {
        let mut reg = ScaleRegistry::new();
        reg.insert(
            "category_color",
            scale::discrete([
                Value::String(Arc::from("A")),
                Value::String(Arc::from("B")),
                Value::String(Arc::from("C")),
            ])
            .range_colors([
                rgb(1.0, 0.0, 0.0),
                rgb(0.0, 1.0, 0.0),
                rgb(0.0, 0.0, 1.0),
            ]),
        );
        reg.insert(
            "category_size",
            scale::discrete([
                Value::String(Arc::from("A")),
                Value::String(Arc::from("B")),
                Value::String(Arc::from("C")),
            ])
            .range_numbers([4.0, 8.0, 12.0]),
        );
        reg
    }
    #[test]
    fn empty_legend_reports_zero_size() {
        let legend = Legend::new("category_color");
        let reg = build_registry();
        let m = legend_measure(&legend, &reg, &shape_reg(), dpi_96(), &default_theme());
        assert_eq!(m.width_hint(dpi_96()), WidthHint::Min(0.0));
        assert_eq!(m.height_at(100.0, dpi_96()), 0.0);
    }

    #[test]
    fn point_legend_with_scaled_fill_reports_nonzero() {
        let legend = Legend::new("category_color")
            .title("Category")
            .key(LegendKeySpec::point().scaled("fill", "category_color"));
        let reg = build_registry();
        let m = legend_measure(&legend, &reg, &shape_reg(), dpi_96(), &default_theme());
        let w = match m.width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seed,
        };
        assert!(w > 0.0);
    }
    #[test]
    fn point_swatch_dim_scales_with_size_channel() {
        let small = Legend::new("category_color").key(
            LegendKeySpec::point()
                .scaled("fill", "category_color")
                .fixed("size", 4.0_f64),
        );
        let large = Legend::new("category_color").key(
            LegendKeySpec::point()
                .scaled("fill", "category_color")
                .scaled("size", "category_size"),
        );
        let reg = build_registry();
        let s_w = match legend_measure(&small, &reg, &shape_reg(), dpi_96(), &default_theme())
            .width_hint(dpi_96())
        {
            WidthHint::Min(w) => w,
            _ => 0.0,
        };
        let l_w = match legend_measure(&large, &reg, &shape_reg(), dpi_96(), &default_theme())
            .width_hint(dpi_96())
        {
            WidthHint::Min(w) => w,
            _ => 0.0,
        };
        assert!(
            l_w > s_w,
            "legend with scaled size up to 12pt should be wider than fixed-4pt: {s_w} vs {l_w}"
        );
    }

    // ─── Binned-stack measure / draw symmetry ────────────────────────

    /// Four edges → three bins. `bin_fill` indexes a palette by bin;
    /// `bin_size` gives each bin a distinct pt size so per-bin row
    /// extents are strictly increasing and measurable.
    fn binned_registry() -> ScaleRegistry {
        let mut reg = ScaleRegistry::new();
        let edges = vec![0.0, 10.0, 20.0, 30.0];
        reg.insert(
            "bin_fill",
            scale::binned(0.0..=30.0, edges.clone()).range_colors([
                rgb(1.0, 0.0, 0.0),
                rgb(0.0, 1.0, 0.0),
                rgb(0.0, 0.0, 1.0),
            ]),
        );
        reg.insert(
            "bin_size",
            scale::binned(0.0..=30.0, edges).range_numbers([12.0, 18.0, 24.0]),
        );
        reg
    }

    fn binned_rows(legend: &Legend, reg: &ScaleRegistry, theme: &Theme) -> Vec<(f64, f64)> {
        let m = LegendMeasure::new(
            legend,
            reg,
            &shape_reg(),
            dpi_96(),
            theme.legend_for(legend.theme_variant.as_deref()),
            theme,
            &theme.geom,
            0.0,
            &theme.locale,
            crate::plot::chrome::root_text_pt(theme),
        );
        match m.body {
            BodyMeasure::BinnedStack { row_cells_px, .. } => row_cells_px,
            _ => panic!("expected a binned stack body"),
        }
    }

    #[test]
    fn binned_stack_measures_one_row_per_bin() {
        let legend = Legend::new("bin_fill")
            .binned()
            .key(LegendKeySpec::rect().scaled("fill", "bin_fill"));
        let theme = default_theme();
        let rows = binned_rows(&legend, &binned_registry(), &theme);
        // Three bins from four edges — not four rows from four edges.
        assert_eq!(rows.len(), 3, "one row per bin, got {rows:?}");
    }

    #[test]
    fn binned_size_legend_measures_at_bin_midpoints() {
        let legend = Legend::new("bin_size")
            .binned()
            .key(LegendKeySpec::point().scaled("size", "bin_size"));
        let theme = default_theme();
        let rows = binned_rows(&legend, &binned_registry(), &theme);
        assert_eq!(rows.len(), 3, "one row per bin, got {rows:?}");
        // Bins 0/1/2 resolve to 12/18/24 pt, so row extents must be
        // strictly increasing. Edge sampling would repeat the last bin
        // (edges 0,10,20,30 land in bins 0,1,2,2) and give a flat tail.
        assert!(
            rows[0].1 < rows[1].1 && rows[1].1 < rows[2].1,
            "row heights should follow the per-bin size palette: {rows:?}"
        );
    }

    #[test]
    fn binned_stack_reserved_length_matches_the_bins_drawn() {
        let legend = Legend::new("bin_fill")
            .binned()
            .key(LegendKeySpec::rect().scaled("fill", "bin_fill"));
        let theme = default_theme();
        let reg = binned_registry();
        let m = LegendMeasure::new(
            &legend,
            &reg,
            &shape_reg(),
            dpi_96(),
            theme.legend_for(None),
            &theme,
            &theme.geom,
            0.0,
            &theme.locale,
            crate::plot::chrome::root_text_pt(&theme),
        );
        // Derive the expectation from the bin count independently of
        // the measure, so an extra reserved row can't hide inside a
        // self-consistent sum. Rect keys sit at the theme floor.
        let n_bins = bin_midpoints(&bin_edges(
            &reg.get("bin_fill").expect("bin_fill").breaks(5),
        ))
        .len();
        assert_eq!(n_bins, 3);
        let key_h_floor = pt_to_px(theme.legend.key.height.resolve(0.0), dpi_96());
        // No title, so the reserved cross extent is bar + padding only.
        let expected = (n_bins as f64) * key_h_floor + 2.0 * m.padding_px;
        assert!(
            (m.cross_dim_px(dpi_96()) - expected).abs() < 1e-9,
            "reserved {} should equal {n_bins} bins + padding {}",
            m.cross_dim_px(dpi_96()),
            expected
        );
    }

    #[test]
    fn binned_stack_over_a_non_numeric_domain_reserves_nothing() {
        // A discrete domain yields no numeric edges, so the renderer
        // draws no body — the measure must not reserve a band for it.
        let legend = Legend::new("category_color")
            .binned()
            .key(LegendKeySpec::rect().scaled("fill", "category_color"));
        let theme = default_theme();
        let reg = build_registry();
        assert!(binned_rows(&legend, &reg, &theme).is_empty());
        let measure = legend_measure(&legend, &reg, &shape_reg(), dpi_96(), &theme);
        assert_eq!(measure.width_hint(dpi_96()), WidthHint::Min(0.0));
        assert_eq!(measure.height_at(0.0, dpi_96()), 0.0);
    }
}
