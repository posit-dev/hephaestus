//! Bar-shaped legend bodies: the binned stack and the colorbar.
//!
//! Both draw a bar along the slot's cross axis and label it with an
//! axis-style tick rail, so they share the anchoring and rail helpers
//! here. A binned stack paints one marker-key cell per bin;
//! a colorbar paints one linear-gradient brush whose stops resolve
//! through the [`ColorbarSpec`]'s bindings. [`bin_edges`] /
//! [`bin_midpoints`] derive the rows both the measure and the draw
//! pass work from, which is what keeps reserved space equal to drawn
//! space.

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Shape as _;
use crate::geometry::{Affine, Point, Rect};
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::AxisTick;
use crate::plot::chrome::text::ChromeRun;
use crate::plot::pick::{part_scope, PlotPart};
use crate::plot::scale::ScaleRegistry;
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::chrome::LegendSide;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::shape::ShapeRegistry;

use super::measure::{BodyMeasure, LegendMeasure};
use super::render_keys::{render_key, with_opacity};
use super::spec::{
    resolve_key, AestheticSource, BinSpacing, ColorbarSpec, Legend, LegendKeySpec, ResolvedKey,
};
use super::{cardinal_side, legend_text_styles, paint_rect_frame};

/// Compute the title's `(x, y)` baseline against the slot rect for a
/// cardinal-side legend body. `block_h` is the legend's total primary
/// extent (so `Top` legends can anchor their title at the bottom of
/// the slot); `anchor_width` is whatever the body uses as its primary
/// reference (row width for stack legends, swatch dim for binned
/// stacks, bar thickness for colorbars).
fn title_anchor(
    side: LegendSide,
    slot_rect: Rect,
    padding: f64,
    block_h: f64,
    title_w_px: f64,
    anchor_width: f64,
) -> (f64, f64) {
    let y = match side {
        LegendSide::Top => slot_rect.y1 - block_h + padding,
        _ => slot_rect.y0 + padding,
    };
    let x = match side {
        LegendSide::Left => slot_rect.x1 - padding - title_w_px.max(anchor_width),
        _ => slot_rect.x0 + padding,
    };
    (x, y)
}

/// Pick the axis baseline (start, end, outward tick direction) for a
/// tick rail running along the panel-facing long edge of `bar_rect`.
/// Shared by binned stacks and colorbars — both lay a rail along the
/// bar's long edge with ticks pointing away from the panel.
fn axis_baseline(side: LegendSide, bar_rect: Rect) -> (Point, Point, (f64, f64)) {
    match side {
        LegendSide::Right => (
            Point::new(bar_rect.x1, bar_rect.y1),
            Point::new(bar_rect.x1, bar_rect.y0),
            (1.0, 0.0),
        ),
        LegendSide::Left => (
            Point::new(bar_rect.x0, bar_rect.y1),
            Point::new(bar_rect.x0, bar_rect.y0),
            (-1.0, 0.0),
        ),
        LegendSide::Top => (
            Point::new(bar_rect.x0, bar_rect.y0),
            Point::new(bar_rect.x1, bar_rect.y0),
            (0.0, -1.0),
        ),
        LegendSide::Bottom => (
            Point::new(bar_rect.x0, bar_rect.y1),
            Point::new(bar_rect.x1, bar_rect.y1),
            (0.0, 1.0),
        ),
        LegendSide::InPanel { .. } => unreachable!("cardinal_side flattens InPanel"),
    }
}
/// Numeric bin edges for a binned body: the domain scale's breaks
/// projected to f64, dropping nulls, non-numeric variants and
/// non-finite values. `N` edges describe `N - 1` bins.
pub(super) fn bin_edges(breaks: &[Value]) -> Vec<f64> {
    breaks
        .iter()
        .filter_map(|v| v.as_number().or_else(|| v.as_temporal_f64()))
        .filter(|n| n.is_finite())
        .collect()
}

/// Sample value per bin between adjacent `edges` — the midpoint each
/// bin's keys resolve at. Empty when `edges` describes no bins or the
/// edge span collapses, so the measure pass reserves nothing exactly
/// where the renderer draws nothing.
///
/// Both passes derive their rows from this, which is what keeps
/// reserved space equal to drawn space for a binned body.
pub(super) fn bin_midpoints(edges: &[f64]) -> Vec<Value> {
    if edges.len() < 2 {
        return Vec::new();
    }
    let span = edges[edges.len() - 1] - edges[0];
    if !span.is_finite() || span.abs() < f64::EPSILON {
        return Vec::new();
    }
    edges
        .windows(2)
        .map(|w| Value::Number((w[0] + w[1]) * 0.5))
        .collect()
}

/// Render a binned-stack legend: N+1 breaks define N bins; one row
/// of marker keys is drawn per bin (sampled at the bin's midpoint),
/// and an axis-style tick rail labels the boundaries between rows
/// — same `draw_linear_axis_at` helper the cartesian + colorbar
/// axes use.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_binned_stack_body(
    legend: &Legend,
    keys: &[LegendKeySpec],
    measure: &LegendMeasure,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    lt: &crate::plot::theme::LegendTheme,
    theme: &crate::plot::theme::Theme,
    geom: &crate::plot::theme::GeomTheme,
    locale: &crate::scales::Locale,
    root_pt: f64,
) {
    let palette = &theme.palette;
    let styles = legend_text_styles(lt, theme, dpi, root_pt, images);
    let domain = match registry.get(&legend.domain_scale) {
        Some(s) => s,
        None => return,
    };
    let side = cardinal_side(legend.side);
    let row_cells_px: &[(f64, f64)] = match &measure.body {
        BodyMeasure::BinnedStack { row_cells_px, .. } => row_cells_px.as_slice(),
        _ => return,
    };
    let breaks = bin_edges(&domain.breaks(DEFAULT_BREAK_COUNT));
    let midpoints = bin_midpoints(&breaks);
    if midpoints.is_empty() {
        return;
    }
    debug_assert_eq!(
        row_cells_px.len(),
        midpoints.len(),
        "measure and draw must agree on bin count"
    );
    let (min, max) = (breaks[0], *breaks.last().unwrap());
    let span = max - min;

    let padding = measure.padding_px;
    let title_gap = if legend.title.is_some() && measure.title_h_px > 0.0 {
        measure.row_gap_px
    } else {
        0.0
    };
    let block_h = measure.primary_dim_px(dpi);
    let n_bins = midpoints.len();
    // For binned legends the bar's bins touch each other. Vertical
    // legends use each row's height for the bin along-extent and
    // the max width as the bar thickness; horizontal legends swap
    // (max heights = thickness, each row's width = along-extent).
    let horizontal = matches!(side, LegendSide::Top | LegendSide::Bottom);
    let bar_thickness = if horizontal {
        row_cells_px.iter().map(|(_, h)| *h).fold(0.0_f64, f64::max)
    } else {
        row_cells_px.iter().map(|(w, _)| *w).fold(0.0_f64, f64::max)
    };
    let bar_len: f64 = if horizontal {
        row_cells_px.iter().map(|(w, _)| *w).sum()
    } else {
        row_cells_px.iter().map(|(_, h)| *h).sum()
    };

    // Anchor the legend block to the panel-facing slot edge.
    let (title_x, title_y) = title_anchor(
        side,
        slot_rect,
        padding,
        block_h,
        measure.title_w_px,
        bar_thickness,
    );
    if let (Some(title), Some(paint)) = (&legend.title, &styles.title) {
        let run = ChromeRun::shape(title, &paint.style, dpi, paint.rich.as_ref());
        run.draw(
            scene,
            title_x,
            title_y,
            &paint.brush,
            paint.outline.as_ref(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }

    // Bar rect = the stack of touching swatches. Long axis runs
    // along the slot's cross direction; short axis = bar_thickness.
    let bar_rect = match side {
        LegendSide::Right => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + bar_thickness,
            title_y + measure.title_h_px + title_gap + bar_len,
        ),
        LegendSide::Left => Rect::new(
            slot_rect.x1 - padding - bar_thickness,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x1 - padding,
            title_y + measure.title_h_px + title_gap + bar_len,
        ),
        LegendSide::Top => Rect::new(
            slot_rect.x0 + padding,
            slot_rect.y1 - padding - bar_thickness,
            slot_rect.x0 + padding + bar_len,
            slot_rect.y1 - padding,
        ),
        LegendSide::Bottom => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + bar_len,
            title_y + measure.title_h_px + title_gap + bar_thickness,
        ),
        LegendSide::InPanel { .. } => unreachable!("cardinal_side flattens InPanel"),
    };

    // Bar frame from LegendTheme.bar.frame — fill paints under the
    // bins (so a bin with a transparent / semi-transparent colour
    // lets the frame's fill show through), stroke paints last on top
    // of the bins.
    let bar_frame = lt.bar.frame.as_set();
    if let Some(frame_el) = bar_frame {
        paint_rect_frame(scene, frame_el, palette, bar_rect, dpi, true, false);
    }

    // For Right/Left the bar runs BOTTOM (low frac) to TOP (high
    // frac) — matches the cartesian y convention. For Top/Bottom
    // it's left → right. The stack is in domain order whichever way
    // the scale runs; a reversed scale flips the swatch colours,
    // which come from each bin's midpoint through `resolve_key`.
    let equal_bins = legend.bin_spacing == BinSpacing::Equal;
    for i in 0..n_bins {
        let (lo, hi) = (breaks[i], breaks[i + 1]);
        let midpoint = &midpoints[i];
        let (lo_t, hi_t) = if equal_bins {
            (i as f64 / n_bins as f64, (i + 1) as f64 / n_bins as f64)
        } else {
            ((lo - min) / span, (hi - min) / span)
        };
        let cell = if horizontal {
            Rect::new(
                bar_rect.x0 + lo_t * (bar_rect.x1 - bar_rect.x0),
                bar_rect.y0,
                bar_rect.x0 + hi_t * (bar_rect.x1 - bar_rect.x0),
                bar_rect.y1,
            )
        } else {
            // Flip so low_frac → bottom of bar (high y).
            Rect::new(
                bar_rect.x0,
                bar_rect.y0 + (1.0 - hi_t) * (bar_rect.y1 - bar_rect.y0),
                bar_rect.x1,
                bar_rect.y0 + (1.0 - lo_t) * (bar_rect.y1 - bar_rect.y0),
            )
        };
        for key in keys {
            let resolved = resolve_key(key, registry, midpoint);
            render_key(
                key.kind, &resolved, cell, shapes, scene, dpi, geom, palette, theme, images,
            );
        }
    }

    // Frame border on top of the bins.
    if let Some(frame_el) = bar_frame {
        paint_rect_frame(scene, frame_el, palette, bar_rect, dpi, false, true);
    }

    // Axis along the bar's long edge (away from the panel) with
    // ticks at each break boundary. Reuse `draw_linear_axis_at` so
    // the rail matches the cartesian / colorbar axes pixel-for-pixel.
    let (axis_start, axis_end, tick_direction) = axis_baseline(side, bar_rect);
    let majors_owned = colorbar_majors(domain, locale);
    let majors_owned = if legend.bin_spacing == BinSpacing::Equal {
        colorbar_majors_remap_equal(&majors_owned)
    } else {
        majors_owned
    };
    let majors = open_end_trim(&majors_owned, legend.open_lower, legend.open_upper);
    let style = crate::plot::chrome::linear_axis::AxisChromeStyle::from_resolved(
        &lt.axis.resolved_with_root(Some(&theme.text)),
        theme,
        dpi,
        root_pt,
        images,
    );
    crate::plot::chrome::linear_axis::draw_linear_axis_at(
        scene,
        axis_start,
        axis_end,
        tick_direction,
        majors,
        &[],
        &style,
        dpi,
    );
}

/// Render a gradient colorbar + tick rail. The bar is approximated by
/// `samples` constant-colour rects (each sampled from the domain
/// scale's colour output range); the tick rail goes through the
/// shared [`draw_linear_axis_at`] so it stays visually consistent
/// with the cartesian + polar radius axes.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_colorbar_body(
    legend: &Legend,
    spec: &ColorbarSpec,
    measure: &LegendMeasure,
    registry: &ScaleRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    lt: &crate::plot::theme::LegendTheme,
    theme: &crate::plot::theme::Theme,
    geom: &crate::plot::theme::GeomTheme,
    locale: &crate::scales::Locale,
    root_pt: f64,
) {
    let palette = &theme.palette;
    let styles = legend_text_styles(lt, theme, dpi, root_pt, images);
    let domain = match registry.get(&legend.domain_scale) {
        Some(s) => s,
        None => return,
    };
    let side = cardinal_side(legend.side);
    let (bar_thickness_px, samples) = match measure.body {
        BodyMeasure::Colorbar {
            bar_thickness_px,
            samples,
        } => (bar_thickness_px, samples),
        _ => return,
    };

    let padding = measure.padding_px;
    let title_gap = if legend.title.is_some() && measure.title_h_px > 0.0 {
        measure.row_gap_px
    } else {
        0.0
    };
    let block_h = measure.primary_dim_px(dpi);

    // Anchor the colorbar block to the panel-facing slot edge.
    let (title_x, title_y) = title_anchor(
        side,
        slot_rect,
        padding,
        block_h,
        measure.title_w_px,
        bar_thickness_px,
    );

    if let (Some(title), Some(paint)) = (&legend.title, &styles.title) {
        let run = ChromeRun::shape(title, &paint.style, dpi, paint.rich.as_ref());
        run.draw(
            scene,
            title_x,
            title_y,
            &paint.brush,
            paint.outline.as_ref(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }

    // The bar's body rect. Long axis lies along the slot's cross
    // direction; short axis is `bar_thickness_px`.
    let cross_dim_px = measure.cross_dim_px(dpi);
    let bar_rect = match side {
        LegendSide::Right => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + bar_thickness_px,
            title_y
                + measure.title_h_px
                + title_gap
                + (cross_dim_px - 2.0 * padding - measure.title_h_px - title_gap),
        ),
        LegendSide::Left => Rect::new(
            slot_rect.x1 - padding - bar_thickness_px,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x1 - padding,
            title_y
                + measure.title_h_px
                + title_gap
                + (cross_dim_px - 2.0 * padding - measure.title_h_px - title_gap),
        ),
        LegendSide::Top => Rect::new(
            slot_rect.x0 + padding,
            slot_rect.y1 - padding - bar_thickness_px,
            slot_rect.x0 + padding + (cross_dim_px - 2.0 * padding),
            slot_rect.y1 - padding,
        ),
        LegendSide::Bottom => Rect::new(
            slot_rect.x0 + padding,
            title_y + measure.title_h_px + title_gap,
            slot_rect.x0 + padding + (cross_dim_px - 2.0 * padding),
            title_y + measure.title_h_px + title_gap + bar_thickness_px,
        ),
        LegendSide::InPanel { .. } => unreachable!("cardinal_side flattens InPanel"),
    };
    // Bar frame from LegendTheme.bar.frame — fill, gradient, stroke
    // in that order. The frame's fill paints first so semi-transparent
    // gradient stops let the background show through; the stroke
    // paints last so the border sits on top of the gradient. Frame
    // semantics mirror KeyTheme.frame exactly.
    use crate::plot::theme::rect_concrete_defaults;
    let rect_defaults = rect_concrete_defaults();
    let frame = lt.bar.frame.as_set();
    let bar_radius_px = frame
        .and_then(|f| f.corner_radius.or(rect_defaults.corner_radius))
        .map(|l| (l.resolve(0.0) * dpi / 72.0).max(0.0))
        .unwrap_or(0.0);
    if let Some(frame_el) = frame {
        paint_rect_frame(scene, frame_el, palette, bar_rect, dpi, true, false);
    }
    // Bar and frame are one target: hovering the ramp should not report
    // something different depending on whether the pointer is over its edge.
    scene.push_pick_scope(&part_scope(PlotPart::ColorbarBar));
    draw_gradient_bar(
        domain,
        spec,
        legend.bin_spacing,
        registry,
        &bar_rect,
        side,
        bar_radius_px,
        scene,
        palette,
        geom,
    );
    if let Some(frame_el) = frame {
        paint_rect_frame(scene, frame_el, palette, bar_rect, dpi, false, true);
    }
    scene.pop_pick_scope();
    let _ = samples; // sample count carried on the spec, used inside draw_gradient_bar

    // Axis along the bar's long edge — uses the shared linear-axis
    // function so ticks and labels match the cartesian / polar
    // radius axes pixel-for-pixel.
    let (axis_start, axis_end, tick_direction) = axis_baseline(side, bar_rect);

    let majors_owned = colorbar_majors(domain, locale);
    let majors_owned = if legend.bin_spacing == BinSpacing::Equal {
        colorbar_majors_remap_equal(&majors_owned)
    } else {
        majors_owned
    };
    let majors = open_end_trim(&majors_owned, legend.open_lower, legend.open_upper);
    let style = crate::plot::chrome::linear_axis::AxisChromeStyle::from_resolved(
        &lt.axis.resolved_with_root(Some(&theme.text)),
        theme,
        dpi,
        root_pt,
        images,
    );
    crate::plot::chrome::linear_axis::draw_linear_axis_at(
        scene,
        axis_start,
        axis_end,
        tick_direction,
        majors,
        &[],
        &style,
        dpi,
    );
}

/// Remap each major's fraction to its equal-spaced position
/// (`i / (n − 1)`), preserving order and labels. Used when the legend
/// is in [`BinSpacing::Equal`] mode so the tick rail's labels still
/// report the underlying break values but their positions line up
/// with the equal-width bin / colour blocks.
fn colorbar_majors_remap_equal(majors: &[AxisTick]) -> Vec<AxisTick> {
    let n = majors.len();
    if n <= 1 {
        return majors
            .iter()
            .map(|t| AxisTick {
                break_index: t.break_index,
                frac: t.frac,
                label: t.label.clone(),
            })
            .collect();
    }
    majors
        .iter()
        .enumerate()
        .map(|(i, t)| AxisTick {
            // Position is remapped; identity is not.
            break_index: t.break_index,
            frac: i as f64 / (n - 1) as f64,
            label: t.label.clone(),
        })
        .collect()
}

/// Drop the first and / or last element from a majors slice when the
/// caller has marked the corresponding outer bin as open. Operates on
/// the per-break [`AxisTick`]s `draw_linear_axis_at` consumes — the
/// swatches / gradient blocks themselves are unaffected.
///
/// Trimming shifts positions but not identities: each surviving tick keeps
/// the `break_index` it arrived with, so an open-ended colorbar still
/// reports the break a tick actually came from.
fn open_end_trim(majors: &[AxisTick], open_lower: bool, open_upper: bool) -> &[AxisTick] {
    let start = if open_lower && !majors.is_empty() {
        1
    } else {
        0
    };
    let end_excl = if open_upper && majors.len() > start {
        majors.len() - 1
    } else {
        majors.len()
    };
    if start <= end_excl {
        &majors[start..end_excl]
    } else {
        &[]
    }
}

/// Domain-fraction (axis-frac) + label string per break, for the
/// colorbar's tick rail. The frac is `(break - min) / (max - min)`
/// — the position the break maps to along the bar regardless of the
/// scale's output range, and regardless of its
/// [`Direction`](crate::scales::Direction): a legend lists its domain in
/// domain order either way, so a reversed scale shows the same labels
/// against a mirrored ramp rather than a mirrored rail.
fn colorbar_majors(
    domain: &crate::plot::scale::Scale,
    locale: &crate::scales::Locale,
) -> Vec<AxisTick> {
    let (min, max) = match domain.input_range() {
        Some(crate::scales::input::InputRange::Continuous { min, max }) => (*min, *max),
        _ => return Vec::new(),
    };
    let span = max - min;
    if !span.is_finite() || span.abs() < f64::EPSILON {
        return Vec::new();
    }
    // `enumerate` before the filters — see the note in `chrome::axis::draw`.
    domain
        .breaks(DEFAULT_BREAK_COUNT)
        .iter()
        .enumerate()
        .filter(|(_, v)| !matches!(v, Value::Null))
        .filter_map(|(break_index, v)| {
            let n = v.as_number().or_else(|| v.as_temporal_f64())?;
            if !n.is_finite() {
                return None;
            }
            Some(AxisTick {
                break_index,
                frac: (n - min) / span,
                label: domain.format(v, locale),
            })
        })
        .collect()
}

/// Fill the bar with a single linear-gradient brush whose stops
/// resolve a [`ResolvedKey`] per sample from the spec's bindings,
/// picking `fill`, at its `fill_opacity` if set, as the stop colour.
/// `fill` defaults to the legend's `domain_scale` if not in
/// `bindings`. Single `scene.fill` call — no AA seams between
/// adjacent sample rects.
#[allow(clippy::too_many_arguments)]
fn draw_gradient_bar(
    domain: &crate::plot::scale::Scale,
    spec: &ColorbarSpec,
    bin_spacing: BinSpacing,
    registry: &ScaleRegistry,
    bar: &Rect,
    side: LegendSide,
    corner_radius_px: f64,
    scene: &mut dyn SceneBuilder,
    palette: &crate::plot::theme::Palette,
    geom: &crate::plot::theme::GeomTheme,
) {
    let (min, max) = match domain.input_range() {
        Some(crate::scales::input::InputRange::Continuous { min, max }) => (*min, *max),
        _ => return,
    };
    let span = max - min;
    if !span.is_finite() || span.abs() < f64::EPSILON {
        return;
    }
    let n = spec.samples.max(2);
    let horizontal = matches!(side, LegendSide::Top | LegendSide::Bottom);

    // Gradient endpoints (in pixel space). For Right/Left the
    // gradient runs from BOTTOM (low frac) to TOP (high frac) so
    // positive y_frac maps "up" — same convention as the cartesian
    // y-axis. For Top/Bottom it runs left → right.
    let (grad_start, grad_end) = if horizontal {
        (
            Point::new(bar.x0, bar.y0 + (bar.y1 - bar.y0) * 0.5),
            Point::new(bar.x1, bar.y0 + (bar.y1 - bar.y0) * 0.5),
        )
    } else {
        (
            Point::new(bar.x0 + (bar.x1 - bar.x0) * 0.5, bar.y1),
            Point::new(bar.x0 + (bar.x1 - bar.x0) * 0.5, bar.y0),
        )
    };

    // Implicit `fill = Scaled(domain_scale)` if the spec doesn't
    // bind it explicitly. Same semantics as a Rect key with a
    // single scaled fill binding.
    let has_explicit_fill =
        spec.bindings.contains_key("fill") || spec.bindings.contains_key("color");

    // Resolve one stop colour at a domain value, honouring the
    // spec's bindings, the implicit fill fallback, and the fill
    // opacity. Shared between the smooth and stepped paths.
    let resolve_stop_colour = |value: Value| -> Color {
        let mut resolved = ResolvedKey::default();
        for (aesthetic, source) in &spec.bindings {
            let v = match source {
                AestheticSource::Scaled(name) => match registry.get(name) {
                    Some(scale) => scale.map(&value),
                    None => continue,
                },
                AestheticSource::Fixed(val) => val.clone(),
            };
            resolved.apply(aesthetic, v);
        }
        if !has_explicit_fill {
            if let Some(c) = domain.map(&value).as_color() {
                resolved.fill = Some(c);
            }
        }
        // A stop the bindings can't colour falls through to the same
        // rect-key default a discrete key would use, so a colorbar
        // never paints a colour the palette doesn't own.
        let fallback = || {
            geom.rect
                .fill
                .as_ref()
                .map(|c| c.resolve(palette))
                .unwrap_or_else(|| palette.ink)
        };
        with_opacity(
            resolved.fill.unwrap_or_else(fallback),
            resolved.fill_opacity,
        )
    };

    let stops: Vec<crate::brush::ColorStop> = if spec.stepped {
        // Constant-colour blocks between adjacent breaks. Two stops
        // per bin at the *same* colour share offsets with the
        // adjacent bin's outer stop — peniko interpolates between
        // them across zero distance, producing an instant
        // transition (a step) in the gradient.
        let mut break_values: Vec<f64> = domain
            .breaks(DEFAULT_BREAK_COUNT)
            .iter()
            .filter_map(|v| {
                let n = v.as_number().or_else(|| v.as_temporal_f64())?;
                if !n.is_finite() {
                    return None;
                }
                Some(n)
            })
            .filter(|n| *n >= min && *n <= max)
            .collect();
        // Make sure the bar is fully covered even if the breaks
        // don't reach the domain endpoints — clamp to [min, max] on
        // either side.
        if break_values.first().copied().unwrap_or(max) > min {
            break_values.insert(0, min);
        }
        if break_values.last().copied().unwrap_or(min) < max {
            break_values.push(max);
        }
        if break_values.len() < 2 {
            return;
        }
        let n_bins = break_values.len() - 1;
        let mut out = Vec::with_capacity(break_values.len() * 2);
        for (i, w) in break_values.windows(2).enumerate() {
            let (lo, hi) = (w[0], w[1]);
            let mid_value = Value::Number((lo + hi) * 0.5);
            let (lo_t, hi_t) = match bin_spacing {
                BinSpacing::Proportional => ((lo - min) / span, (hi - min) / span),
                BinSpacing::Equal => (i as f64 / n_bins as f64, (i + 1) as f64 / n_bins as f64),
            };
            let color = resolve_stop_colour(mid_value);
            out.push(crate::brush::ColorStop {
                offset: lo_t as f32,
                color: color.into(),
            });
            out.push(crate::brush::ColorStop {
                offset: hi_t as f32,
                color: color.into(),
            });
        }
        out
    } else {
        (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                let value = Value::Number(min + t * span);
                crate::brush::ColorStop {
                    offset: t as f32,
                    color: resolve_stop_colour(value).into(),
                }
            })
            .collect()
    };

    let gradient =
        crate::brush::Gradient::new_linear(grad_start, grad_end).with_stops(stops.as_slice());
    // Clip the gradient to the rounded bar shape — fill via the
    // rounded path so the gradient never paints past the frame's
    // rounded corners.
    let path: Path = if corner_radius_px > 0.0 {
        crate::primitives::rounded_rect(*bar, corner_radius_px)
    } else {
        bar.to_path(0.0)
    };
    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &Brush::Gradient(gradient),
        None,
        &path,
        PickId::Skip,
    );
    // Suppress unused param when no resolver is needed — keeps the
    // signature stable for future callers that might want to inspect
    // the legend's domain scale name.
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn bin_edges_drops_nulls_and_non_numeric_breaks() {
        let breaks = vec![
            Value::Null,
            Value::Number(0.0),
            Value::String(Arc::from("x")),
            Value::Number(10.0),
            Value::Number(f64::NAN),
        ];
        assert_eq!(bin_edges(&breaks), vec![0.0, 10.0]);
    }

    #[test]
    fn bin_midpoints_yields_one_value_per_bin() {
        let mids: Vec<f64> = bin_midpoints(&[0.0, 10.0, 20.0, 30.0])
            .iter()
            .filter_map(|v| v.as_number())
            .collect();
        assert_eq!(mids, vec![5.0, 15.0, 25.0]);
        // No bins: too few edges, or a collapsed span.
        assert!(bin_midpoints(&[]).is_empty());
        assert!(bin_midpoints(&[5.0]).is_empty());
        assert!(bin_midpoints(&[5.0, 5.0]).is_empty());
    }
    /// A tick whose `break_index` is deliberately *not* its position, so a
    /// test that confuses the two fails.
    fn tick(break_index: usize, frac: f64, label: &str) -> AxisTick {
        AxisTick {
            break_index,
            frac,
            label: label.to_string(),
        }
    }

    fn sample_majors() -> Vec<AxisTick> {
        vec![
            tick(10, 0.0, "0"),
            tick(11, 0.25, "1"),
            tick(12, 0.5, "2"),
            tick(13, 0.75, "3"),
            tick(14, 1.0, "4"),
        ]
    }

    #[test]
    fn open_lower_drops_first_major() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, true, false);
        assert_eq!(trimmed.len(), 4);
        assert_eq!(trimmed[0].label, "1");
        assert_eq!(trimmed[3].label, "4");
    }

    #[test]
    fn open_upper_drops_last_major() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, false, true);
        assert_eq!(trimmed.len(), 4);
        assert_eq!(trimmed[0].label, "0");
        assert_eq!(trimmed[3].label, "3");
    }

    #[test]
    fn open_both_drops_both_terminals() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, true, true);
        assert_eq!(trimmed.len(), 3);
        assert_eq!(trimmed[0].label, "1");
        assert_eq!(trimmed[2].label, "3");
    }

    #[test]
    fn open_neither_returns_full_slice() {
        let m = sample_majors();
        let trimmed = open_end_trim(&m, false, false);
        assert_eq!(trimmed.len(), 5);
    }

    #[test]
    fn trimming_shifts_positions_but_not_break_indices() {
        // The whole point of carrying `break_index`: after a trim, a tick's
        // position in the drawn set no longer matches its position in the
        // scale's break list, and the identity that survives is the latter.
        let m = sample_majors();
        let trimmed = open_end_trim(&m, true, true);
        assert_eq!(
            trimmed.iter().map(|t| t.break_index).collect::<Vec<_>>(),
            vec![11, 12, 13]
        );
    }

    #[test]
    fn open_trim_handles_short_slices() {
        // Single element + open_lower yields empty.
        let one = vec![tick(0, 0.5, "mid")];
        assert!(open_end_trim(&one, true, false).is_empty());
        // Empty slice in is empty slice out.
        let empty: Vec<AxisTick> = vec![];
        assert!(open_end_trim(&empty, true, true).is_empty());
    }

    #[test]
    fn equal_remap_spaces_majors_uniformly() {
        // Pathological proportional split with five breaks.
        let m = vec![
            tick(3, 0.0, "0"),
            tick(4, 0.01, "1"),
            tick(5, 0.05, "5"),
            tick(6, 0.5, "50"),
            tick(7, 1.0, "100"),
        ];
        let remapped = colorbar_majors_remap_equal(&m);
        assert_eq!(remapped.len(), 5);
        // Labels preserved in order.
        assert_eq!(remapped[0].label, "0");
        assert_eq!(remapped[4].label, "100");
        // Remapping moves ticks; it does not renumber them.
        assert_eq!(
            remapped.iter().map(|t| t.break_index).collect::<Vec<_>>(),
            vec![3, 4, 5, 6, 7]
        );
        // Fractions are i / (n - 1) = i / 4.
        for (i, t) in remapped.iter().enumerate() {
            let expected = i as f64 / 4.0;
            assert!(
                (t.frac - expected).abs() < 1e-12,
                "remap[{i}] = {}, expected {expected}",
                t.frac
            );
        }
    }

    #[test]
    fn equal_remap_short_slice_is_passthrough() {
        let single = vec![tick(9, 0.42, "lonely")];
        let remapped = colorbar_majors_remap_equal(&single);
        assert_eq!(remapped.len(), 1);
        assert!((remapped[0].frac - 0.42).abs() < 1e-12);
        assert_eq!(remapped[0].break_index, 9);
    }
}
