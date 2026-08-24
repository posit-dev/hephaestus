//! Legend rendering — explicit, manual API.
//!
//! A [`Legend`] is composed by the caller (no inference from
//! bindings). It carries:
//!
//! - the **domain scale** whose `breaks()` drive the rows,
//! - a [`LegendSide`] + optional title,
//! - a stack of [`LegendKeySpec`]s — each a geom-shaped marker
//!   primitive ([`LegendKey::Point`] / `Line` / [`Rect`] /
//!   [`Text`](LegendKey::Text)) with its own per-aesthetic
//!   [`AestheticSource`] map.
//!
//! Each row of the legend computes a [`ResolvedKey`] per stack
//! member by walking its bindings (scale lookup at the row's domain
//! value, or fixed value), then renders the member's marker using
//! the resolved aesthetics. Different stack members can pull from
//! different scales, or hard-code fixed values, independently — so
//! e.g. a Line with a scaled stroke colour can sit under a Point
//! whose fill is scaled and whose stroke is a fixed black.
//!
//! Legends attached to one owner are collapsed per render by
//! [`collapse_legends`]: legends describing the same rows fold their
//! key stacks together so a colour legend and a shape legend over one
//! set of categories read as a single block. "The same rows" is
//! decided by comparing the *scales* the legends resolve to, not the
//! names they were given, so independently configured scales that end
//! up trained alike still collapse. Colorbars collapse on the stricter
//! condition that they draw the same bar — a shared domain isn't
//! enough when the surviving bar has to stand in for both palettes.
//!
//! This file owns the entry points — measure a legend or a stack of
//! them, then draw — plus the shell every body shares: the background
//! and frame paints, the cascaded text styles, and the panel-facing
//! gap. The pieces sit beside it: `spec` for the types a caller
//! composes, `layout` for the discrete stack's inner grid, `colorbar`
//! for the bar-shaped bodies, `measure` for the space reservation the
//! layout solver consumes, and `render_keys` for the per-key markers.

mod colorbar;
mod layout;
mod measure;
mod render_keys;
mod spec;

pub use render_keys::DEFAULT_KEY_TEXT;
pub use spec::{
    collapse_legends, AestheticSource, BinSpacing, ColorbarSpec, EndpointMarkerKey, Legend,
    LegendBody, LegendId, LegendKey, LegendKeySpec, ResolvedKey, StackBody,
};

use colorbar::{render_binned_stack_body, render_colorbar_body};
use layout::translate_rect;
use measure::{BodyMeasure, LegendMeasure, LegendStackMeasure};
use render_keys::render_key;
use spec::resolve_key;

use crate::brush::Brush;
use crate::geometry::Shape as _;
use crate::geometry::{Affine, Point, Rect};
use crate::layout::Measure;
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::{draw_axis_label, pt_to_px, AxisLabelAt};
use crate::plot::scale::ScaleRegistry;
// Inter-legend (and panel ↔ legend) gap parents — shared with
// `Theme::default()` so the `Length::Rel` resolve parent matches the
// bottom-of-cascade concrete value.
use crate::plot::chrome::text::ChromeRun;
use crate::plot::theme::{
    DEFAULT_LEGEND_GAP_PT as PANEL_LEGEND_GAP_PT, DEFAULT_LEGEND_SPACING_PT as LEGEND_GAP_PT,
};
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::chrome::{Anchor, LegendSide};
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::shape::ShapeRegistry;
use crate::text::TextStyle;

// ─── Shared shell helpers ───────────────────────────────────────────────────

/// Map a [`LegendSide`] to the cardinal direction the legend renders
/// against. The four anatomical-slot variants pass through; the
/// [`LegendSide::InPanel`] overlay variant renders with Right-style
/// vertical layout against its synthetic panel-anchored rect.
pub(super) fn cardinal_side(side: LegendSide) -> LegendSide {
    match side {
        LegendSide::Left => LegendSide::Left,
        LegendSide::Right => LegendSide::Right,
        LegendSide::Top => LegendSide::Top,
        LegendSide::Bottom => LegendSide::Bottom,
        LegendSide::InPanel { .. } => LegendSide::Right,
    }
}

/// Resolved panel-to-legend gap in px against the theme.
fn legend_gap_px(theme: &crate::plot::theme::Theme, dpi: f64) -> f64 {
    pt_to_px(theme.legend_gap.resolve(PANEL_LEGEND_GAP_PT), dpi)
}

/// Shrink `slot_rect` by `gap_px` on the panel-facing edge for a
/// cardinal-side legend. The shrunk rect is what the renderer draws
/// into; the layout already reserved the full slot inclusive of the
/// gap via [`LegendMeasure::primary_dim_px`].
fn inset_for_panel_gap(slot_rect: Rect, side: LegendSide, gap_px: f64) -> Rect {
    match side {
        LegendSide::Right => Rect::new(
            slot_rect.x0 + gap_px,
            slot_rect.y0,
            slot_rect.x1,
            slot_rect.y1,
        ),
        LegendSide::Left => Rect::new(
            slot_rect.x0,
            slot_rect.y0,
            slot_rect.x1 - gap_px,
            slot_rect.y1,
        ),
        LegendSide::Bottom => Rect::new(
            slot_rect.x0,
            slot_rect.y0 + gap_px,
            slot_rect.x1,
            slot_rect.y1,
        ),
        LegendSide::Top => Rect::new(
            slot_rect.x0,
            slot_rect.y0,
            slot_rect.x1,
            slot_rect.y1 - gap_px,
        ),
        // In-panel legends use Anchor + inset_pt and don't go through
        // the cardinal slot machinery, so the gap doesn't apply.
        LegendSide::InPanel { .. } => slot_rect,
    }
}

/// Pin a `size` rectangle inside `panel` at `anchor`, offset from the
/// matching panel edge by `inset_px` on both axes. Centre anchors
/// receive no inset along their centred axis.
pub fn resolve_anchor(panel: Rect, anchor: Anchor, inset_px: f64, size: (f64, f64)) -> Rect {
    let (w, h) = size;
    let (x0, y0) = match anchor {
        Anchor::TopLeft => (panel.x0 + inset_px, panel.y0 + inset_px),
        Anchor::TopCenter => (
            panel.x0 + (panel.x1 - panel.x0 - w) * 0.5,
            panel.y0 + inset_px,
        ),
        Anchor::TopRight => (panel.x1 - w - inset_px, panel.y0 + inset_px),
        Anchor::CenterLeft => (
            panel.x0 + inset_px,
            panel.y0 + (panel.y1 - panel.y0 - h) * 0.5,
        ),
        Anchor::Center => (
            panel.x0 + (panel.x1 - panel.x0 - w) * 0.5,
            panel.y0 + (panel.y1 - panel.y0 - h) * 0.5,
        ),
        Anchor::CenterRight => (
            panel.x1 - w - inset_px,
            panel.y0 + (panel.y1 - panel.y0 - h) * 0.5,
        ),
        Anchor::BottomLeft => (panel.x0 + inset_px, panel.y1 - h - inset_px),
        Anchor::BottomCenter => (
            panel.x0 + (panel.x1 - panel.x0 - w) * 0.5,
            panel.y1 - h - inset_px,
        ),
        Anchor::BottomRight => (panel.x1 - w - inset_px, panel.y1 - h - inset_px),
    };
    Rect::new(x0, y0, x0 + w, y0 + h)
}

/// Combined primary + cross extent of a stack of in-panel legends.
/// In-panel legends use Right-style layout: primary = column width,
/// cross = stacked row heights + inter-legend gaps. Returns `(w, h)`
/// in pixels.
pub fn legend_stack_natural_size(
    legends: &[&Legend],
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
) -> (f64, f64) {
    let gap_px = legend_gap_px(theme, dpi);
    let measures: Vec<LegendMeasure> = legends
        .iter()
        .map(|l| {
            LegendMeasure::new(
                l,
                registry,
                shapes,
                images,
                dpi,
                theme.legend_for(l.theme_variant.as_deref()),
                theme,
                &theme.geom,
                gap_px,
                &theme.locale,
                crate::plot::chrome::root_text_pt(theme),
            )
        })
        .filter(|m| !m.is_empty())
        .collect();
    if measures.is_empty() {
        return (0.0, 0.0);
    }
    let gap_px = pt_to_px(theme.legend_spacing.resolve(LEGEND_GAP_PT), dpi);
    let primary = measures
        .iter()
        .map(|m| m.primary_dim_px(dpi))
        .fold(0.0_f64, f64::max);
    let cross: f64 = measures.iter().map(|m| m.cross_dim_px(dpi)).sum::<f64>()
        + gap_px * (measures.len() as f64 - 1.0).max(0.0);
    (primary, cross)
}

// ─── Entry points ───────────────────────────────────────────────────────────

/// Pre-shape a legend into a [`Measure`] so the composition solver
/// can reserve space for its slot. Same machinery (peak resolved
/// aesthetics + per-key swatch dims) drives the draw step, so what
/// is reserved matches what is drawn — including `shapes`, which has
/// to be the registry the draw step resolves markers through for the
/// reserved cells to match the markers' bounds.
pub fn legend_measure(
    legend: &Legend,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
) -> Box<dyn Measure> {
    Box::new(LegendMeasure::new(
        legend,
        registry,
        shapes,
        images,
        dpi,
        theme.legend_for(legend.theme_variant.as_deref()),
        theme,
        &theme.geom,
        legend_gap_px(theme, dpi),
        &theme.locale,
        crate::plot::chrome::root_text_pt(theme),
    ))
}

/// Pre-shape a stack of legends sharing the same side. Reserves the
/// max primary extent (column width / row height) across children
/// and the sum of cross extents plus inter-legend gaps. Pair with
/// [`render_legend_stack`] at draw time, passing the same `shapes`
/// registry, so what's reserved matches what's drawn.
pub fn legend_stack_measure(
    legends: &[&Legend],
    side: LegendSide,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
) -> Box<dyn Measure> {
    let inter_gap_px = pt_to_px(theme.legend_spacing.resolve(LEGEND_GAP_PT), dpi);
    let panel_gap_px = legend_gap_px(theme, dpi);
    let children: Vec<LegendMeasure> = legends
        .iter()
        .map(|l| {
            LegendMeasure::new(
                l,
                registry,
                shapes,
                images,
                dpi,
                theme.legend_for(l.theme_variant.as_deref()),
                theme,
                &theme.geom,
                panel_gap_px,
                &theme.locale,
                crate::plot::chrome::root_text_pt(theme),
            )
        })
        .collect();
    Box::new(LegendStackMeasure {
        side: cardinal_side(side),
        children,
        gap_px: inter_gap_px,
    })
}

/// Draw a stack of same-side legends into `slot_rect`. Children
/// stack along the cross axis (Right/Left: top→bottom;
/// Top/Bottom: left→right) with [`LEGEND_GAP_PT`] between them.
/// Each child uses its own `cross_dim_px` for its share of the
/// slot; the full primary extent is available to every child.
/// `shapes` lets [`LegendKey::Point`] resolve a `shape` aesthetic
/// to a registered marker.
#[allow(clippy::too_many_arguments)]
pub fn render_legend_stack(
    legends: &[&Legend],
    side: LegendSide,
    slot_rect: Rect,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
) {
    let inter_gap_px = pt_to_px(theme.legend_spacing.resolve(LEGEND_GAP_PT), dpi);
    let panel_gap_px = legend_gap_px(theme, dpi);
    let measures: Vec<(usize, LegendMeasure)> = legends
        .iter()
        .enumerate()
        .map(|(i, l)| {
            (
                i,
                LegendMeasure::new(
                    l,
                    registry,
                    shapes,
                    images,
                    dpi,
                    theme.legend_for(l.theme_variant.as_deref()),
                    theme,
                    &theme.geom,
                    panel_gap_px,
                    &theme.locale,
                    crate::plot::chrome::root_text_pt(theme),
                ),
            )
        })
        .filter(|(_, m)| !m.is_empty())
        .collect();
    if measures.is_empty() {
        return;
    }
    let stack_axis_is_y = matches!(side, LegendSide::Right | LegendSide::Left);
    let mut cursor = if stack_axis_is_y {
        slot_rect.y0
    } else {
        slot_rect.x0
    };
    for (orig_idx, measure) in &measures {
        let cross = measure.cross_dim_px(dpi);
        let sub_rect = if stack_axis_is_y {
            Rect::new(slot_rect.x0, cursor, slot_rect.x1, cursor + cross)
        } else {
            Rect::new(cursor, slot_rect.y0, cursor + cross, slot_rect.y1)
        };
        // The stack already measured every child to place it; passing
        // that measure down saves re-shaping every label and re-solving
        // the discrete-stack grid a second time per legend per frame.
        render_legend_with_measure(
            legends[*orig_idx],
            measure,
            registry,
            shapes,
            images,
            sub_rect,
            scene,
            dpi,
            theme,
        );
        cursor += cross + inter_gap_px;
    }
}

/// Draw the legend into `slot_rect`. Dispatches on the legend's
/// [`LegendBody`]: stack legends render their marker stack per row;
/// colorbar legends render a gradient bar plus an axis-style tick
/// rail. The legend block hugs the panel-facing edge of the slot
/// regardless of how much extra space the layout solver leaves on
/// the far side.
#[allow(clippy::too_many_arguments)]
pub fn render_legend(
    legend: &Legend,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
) {
    let lt = theme.legend_for(legend.theme_variant.as_deref());
    let measure = LegendMeasure::new(
        legend,
        registry,
        shapes,
        images,
        dpi,
        lt,
        theme,
        &theme.geom,
        legend_gap_px(theme, dpi),
        &theme.locale,
        crate::plot::chrome::root_text_pt(theme),
    );
    render_legend_with_measure(
        legend, &measure, registry, shapes, images, slot_rect, scene, dpi, theme,
    );
}

/// [`render_legend`] against a measure the caller already built.
/// Building one shapes every label and, for a discrete stack, solves a
/// whole layout grid — so a caller that measured to place the legend
/// hands that work down rather than repeating it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_legend_with_measure(
    legend: &Legend,
    measure: &LegendMeasure,
    registry: &ScaleRegistry,
    shapes: &ShapeRegistry,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    slot_rect: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    theme: &crate::plot::theme::Theme,
) {
    let lt = theme.legend_for(legend.theme_variant.as_deref());
    let gap_px = legend_gap_px(theme, dpi);
    if measure.is_empty() {
        return;
    }
    // Shrink the slot on the panel-facing edge by `legend_gap` —
    // `primary_dim_px` already reserved that strip, but we don't paint
    // background, body, or chrome into it.
    let draw_rect = inset_for_panel_gap(slot_rect, legend.side, gap_px);
    // Background fill + border, sourced from the resolved
    // LegendTheme. Painted under the legend body so keys + text layer
    // on top.
    paint_legend_background(scene, lt, &theme.palette, draw_rect, dpi);

    match &legend.body {
        LegendBody::Stack(stack) if stack.binned => render_binned_stack_body(
            legend,
            &stack.keys,
            measure,
            registry,
            shapes,
            images,
            draw_rect,
            scene,
            dpi,
            lt,
            theme,
            &theme.geom,
            &theme.locale,
            crate::plot::chrome::root_text_pt(theme),
        ),
        LegendBody::Stack(stack) => render_stack_body(
            legend,
            &stack.keys,
            measure,
            registry,
            shapes,
            images,
            draw_rect,
            scene,
            dpi,
            lt,
            theme,
            &theme.geom,
            &theme.locale,
            crate::plot::chrome::root_text_pt(theme),
        ),
        LegendBody::Colorbar(spec) => render_colorbar_body(
            legend,
            spec,
            measure,
            registry,
            images,
            draw_rect,
            scene,
            dpi,
            lt,
            theme,
            &theme.geom,
            &theme.locale,
            crate::plot::chrome::root_text_pt(theme),
        ),
    }
}

// ─── Shared text + frame paint ──────────────────────────────────────────────

/// Resolved paint for one legend text slot — shaped style, fill brush,
/// and the optional outline pass drawn behind the fill.
pub(super) struct LegendTextPaint {
    pub(super) style: TextStyle,
    pub(super) brush: Brush,
    pub(super) outline: Option<crate::plot::chrome::text::TextOutline>,
    /// Markdown context for the slot. `Some` when the element opts
    /// in, in which case the outline rides on the sheet rather than
    /// on a separate pass.
    pub(super) rich: Option<crate::plot::chrome::text::RichChrome>,
}

/// Resolved per-legend text styling. Each slot is `None` when the
/// matching `Element` is `Blank` — call sites use that to skip the
/// draw entirely. `Inherit` and `Set` both resolve to `Some` with
/// the appropriate cascaded values.
pub(super) struct LegendTextStyles {
    pub(super) title: Option<LegendTextPaint>,
    pub(super) label: Option<LegendTextPaint>,
}

/// Cascade the legend's title and break-label text elements against
/// the axis defaults, so a legend title matches an axis title and
/// legend break labels match axis break labels. `None` means the
/// slot is `Blank` and the call site skips both measure and draw.
/// Shared by the measure and draw passes so the two can't drift.
pub(super) fn legend_text_elements(
    lt: &crate::plot::theme::LegendTheme,
    root_text: &crate::plot::theme::TextElement,
) -> (
    Option<crate::plot::theme::TextElement>,
    Option<crate::plot::theme::TextElement>,
) {
    use crate::plot::theme::{axis_concrete_defaults, text_concrete_defaults, Element};
    let text_defaults = text_concrete_defaults();
    let axis_defaults = axis_concrete_defaults();
    // The theme's root text element sits between the legend's own
    // layers and the concrete fallbacks, exactly where
    // `Theme::resolved_axis` puts it — so a figure-wide font, colour
    // or markdown switch reaches a legend the way it reaches an axis.
    let root = root_text.cascade(&text_defaults);
    let axis_title = axis_defaults
        .title
        .as_set()
        .expect("axis_concrete_defaults sets title")
        .cascade(&root);
    let title = match &lt.title {
        Element::Set(child) => Some(child.cascade(&axis_title)),
        Element::Blank => None,
        Element::Inherit => Some(axis_title),
    };
    // Break labels come out of the legend's own `AxisTheme` cascade,
    // which already merges the axis tick-label defaults underneath
    // whatever the theme set.
    let label = lt
        .axis
        .resolved_with_root(Some(root_text))
        .text
        .map(|el| el.cascade(&root));
    (title, label)
}

/// Resolve the legend's title and label elements into ready-to-draw
/// paint — shaped style, fill brush, and optional outline pass.
pub(super) fn legend_text_styles(
    lt: &crate::plot::theme::LegendTheme,
    theme: &crate::plot::theme::Theme,
    dpi: f64,
    root_pt: f64,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
) -> LegendTextStyles {
    let palette = &theme.palette;
    let paint = |merged: crate::plot::theme::TextElement| {
        let color = merged
            .color
            .as_ref()
            .expect("text color default")
            .resolve(palette);
        LegendTextPaint {
            style: crate::plot::chrome::text::text_style_from(&merged, root_pt),
            brush: Brush::Solid(color),
            // Resolve off the cascaded element so the safety net's
            // unset outline and any themed value both come through.
            outline: crate::plot::chrome::text::text_outline_from(&merged, palette, dpi),
            rich: crate::plot::chrome::text::rich_chrome_for(&merged, theme, dpi, images),
        }
    };
    let (title, label) = legend_text_elements(lt, &theme.text);
    LegendTextStyles {
        title: title.map(paint),
        label: label.map(paint),
    }
}

/// Paint a `RectElement` frame around `rect` — fill first (under any
/// inner content), border stroke last (on top). Shared by every
/// frame paint site (legend background, bar frame, per-row key
/// frame) so corner-radius and the fill / stroke ordering stay
/// consistent.
pub(super) fn paint_rect_frame(
    scene: &mut dyn SceneBuilder,
    frame: &crate::plot::theme::RectElement,
    palette: &crate::plot::theme::Palette,
    rect: Rect,
    dpi: f64,
    paint_fill: bool,
    paint_stroke: bool,
) {
    use crate::plot::theme::rect_concrete_defaults;
    let defaults = rect_concrete_defaults();
    let path = path_for_rect_element(rect, frame, &defaults, dpi);
    if paint_fill {
        if let Some(fill) = frame.fill.clone() {
            let brush = Brush::Solid(fill.resolve(palette));
            scene.fill(
                crate::path::FillRule::NonZero,
                Affine::IDENTITY,
                &brush,
                None,
                &path,
                PickId::Skip,
            );
        }
    }
    if paint_stroke {
        let lw = frame
            .linewidth_pt
            .or(defaults.linewidth_pt)
            .expect("rect linewidth default");
        if lw.resolve(1.0) > 0.0 {
            let stroke = crate::plot::chrome::linear_axis::stroke_from_rect_border(frame, dpi);
            let color = frame
                .color
                .clone()
                .or(defaults.color)
                .expect("rect color default");
            let brush = Brush::Solid(color.resolve(palette));
            scene.stroke(&stroke, Affine::IDENTITY, &brush, None, &path, PickId::Skip);
        }
    }
}

/// Resolve `el.corner_radius` to px and build either a sharp or
/// rounded-rect path. Shared by every RectElement paint site in the
/// legend renderer so corner_radius behaves consistently across the
/// outer background, key frames, and bar frames.
fn path_for_rect_element(
    rect: Rect,
    el: &crate::plot::theme::RectElement,
    defaults: &crate::plot::theme::RectElement,
    dpi: f64,
) -> crate::path::Path {
    let radius_pt = el
        .corner_radius
        .or(defaults.corner_radius)
        .map(|l| l.resolve(0.0))
        .unwrap_or(0.0);
    let radius_px = (radius_pt * dpi / 72.0).max(0.0);
    if radius_px > 0.0 {
        crate::primitives::rounded_rect(rect, radius_px)
    } else {
        rect.to_path(0.0)
    }
}

/// Paint the legend's background rect (fill + border) into
/// `slot_rect`. Sourced from `lt.background`. `Blank` skips.
fn paint_legend_background(
    scene: &mut dyn SceneBuilder,
    lt: &crate::plot::theme::LegendTheme,
    palette: &crate::plot::theme::Palette,
    slot_rect: Rect,
    dpi: f64,
) {
    let Some(bg) = lt.background.as_set() else {
        return;
    };
    paint_rect_frame(scene, bg, palette, slot_rect, dpi, true, true);
}

/// Render a discrete-stack legend: one row of marker keys per break,
/// each labelled from the domain scale, laid out by the pre-solved
/// grid the measure carries.
#[allow(clippy::too_many_arguments)]
fn render_stack_body(
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
    let side = cardinal_side(legend.side);
    let domain = match registry.get(&legend.domain_scale) {
        Some(s) => s,
        None => return,
    };
    let layout = match &measure.body {
        BodyMeasure::Stack { layout, .. } => layout,
        _ => return,
    };

    let padding = measure.padding_px;
    let title_gap = if legend.title.is_some() && measure.title_h_px > 0.0 {
        measure.row_gap_px
    } else {
        0.0
    };
    let styles = legend_text_styles(lt, theme, dpi, root_pt, images);

    let entries = domain.breaks(DEFAULT_BREAK_COUNT);
    let entries: Vec<&Value> = entries
        .iter()
        .filter(|v| !matches!(v, Value::Null))
        .collect();

    // Anchor the legend block to the panel-facing slot edge. The
    // entries grid was solved at local origin (0, 0); compute a
    // single `(dx, dy)` offset and translate every cached cell rect
    // by it at draw time.
    let block_w = layout.entries_w_px.max(measure.title_w_px);
    let block_h = measure.title_h_px + title_gap + layout.entries_h_px;
    let title_x = match side {
        LegendSide::Left => slot_rect.x1 - padding - block_w,
        _ => slot_rect.x0 + padding,
    };
    let title_y = match side {
        LegendSide::Top => slot_rect.y1 - padding - block_h,
        _ => slot_rect.y0 + padding,
    };
    let entries_x = title_x;
    let entries_y = title_y + measure.title_h_px + title_gap;

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

    let key_frame = lt.key.frame.as_set();
    for (idx, v) in entries.iter().enumerate() {
        if idx >= layout.entries.len() {
            break;
        }
        let (swatch_local, label_local) = &layout.entries[idx];
        let swatch_rect = translate_rect(*swatch_local, entries_x, entries_y);
        let label_rect = translate_rect(*label_local, entries_x, entries_y);
        // Per-row key frame: fill paints under the key (so a key
        // with a transparent rect / point shape lets the frame's
        // fill show through), stroke paints on top.
        if let Some(frame_el) = key_frame {
            paint_rect_frame(scene, frame_el, palette, swatch_rect, dpi, true, false);
        }
        for key in keys {
            let resolved = resolve_key(key, registry, v);
            render_key(
                key.kind,
                &resolved,
                swatch_rect,
                shapes,
                scene,
                dpi,
                geom,
                palette,
                theme,
                images,
            );
        }
        if let Some(frame_el) = key_frame {
            paint_rect_frame(scene, frame_el, palette, swatch_rect, dpi, false, true);
        }
        if let Some(paint) = &styles.label {
            let label = domain.format(v, locale);
            let anchor = Point::new(label_rect.x0, (label_rect.y0 + label_rect.y1) * 0.5);
            draw_axis_label(
                scene,
                &label,
                &paint.style,
                &paint.brush,
                paint.outline.as_ref(),
                paint.rich.as_ref(),
                AxisLabelAt {
                    anchor,
                    direction: (1.0, 0.0),
                },
                dpi,
            );
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb;
    use crate::plot::scale;
    use crate::plot::theme::Theme;
    use crate::scene::recording::{Op, RecordingScene};
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

    /// A theme whose every text slot reads markdown, the way a host
    /// that wants rich chrome sets it: once, on the root element.
    fn markdown_theme() -> Theme {
        let mut t = default_theme();
        t.text.markdown = Some(true);
        t
    }

    fn glyph_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(run) => Some(run.glyphs.len()),
                _ => None,
            })
            .sum()
    }

    fn render_titled(title: &str, theme: &Theme) -> RecordingScene {
        let legend = Legend::new("category_color")
            .title(title)
            .key(LegendKeySpec::point().scaled("fill", "category_color"));
        let mut scene = RecordingScene::default();
        render_legend(
            &legend,
            &build_registry(),
            &shape_reg(),
            &crate::image_registry::no_images(),
            Rect::new(0.0, 0.0, 300.0, 300.0),
            &mut scene,
            dpi_96(),
            theme,
        );
        scene
    }

    /// A legend title reads its markers as syntax once the theme opts
    /// in — the four `*`s stop being glyphs.
    #[test]
    fn a_markdown_legend_title_reads_its_markers_as_syntax() {
        let plain = glyph_count(&render_titled("**Hue**", &default_theme()));
        let md = glyph_count(&render_titled("**Hue**", &markdown_theme()));
        assert_eq!(
            plain - md,
            4,
            "the markdown title should drop four asterisks (plain {plain}, markdown {md})"
        );
    }

    /// Break labels come off the same cascade, so a category that
    /// spells markdown gets parsed too.
    #[test]
    fn markdown_break_labels_read_their_markers_as_syntax() {
        let mut reg = ScaleRegistry::new();
        reg.insert(
            "md_color",
            scale::discrete([
                Value::String(Arc::from("*A*")),
                Value::String(Arc::from("*B*")),
            ])
            .range_colors([rgb(1.0, 0.0, 0.0), rgb(0.0, 1.0, 0.0)]),
        );
        let legend = Legend::new("md_color").key(LegendKeySpec::point().scaled("fill", "md_color"));
        let render = |theme: &Theme| {
            let mut scene = RecordingScene::default();
            render_legend(
                &legend,
                &reg,
                &shape_reg(),
                &crate::image_registry::no_images(),
                Rect::new(0.0, 0.0, 300.0, 300.0),
                &mut scene,
                dpi_96(),
                theme,
            );
            glyph_count(&scene)
        };
        assert_eq!(
            render(&default_theme()) - render(&markdown_theme()),
            4,
            "two italic labels should drop two asterisks each"
        );
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
    fn fixed_stroke_is_applied_alongside_scaled_fill() {
        // Three rows; Point key with scaled fill + fixed black stroke.
        // The renderer should emit 3 fills (one per row, from the
        // scale) and 3 strokes (all black, fixed).
        let legend = Legend::new("category_color").key(
            LegendKeySpec::point()
                .scaled("fill", "category_color")
                .fixed("stroke", Value::Color(rgb(0.0, 0.0, 0.0))),
        );
        let reg = build_registry();
        let mut scene = RecordingScene::default();
        let shapes = ShapeRegistry::with_builtins();
        render_legend(
            &legend,
            &reg,
            &shapes,
            &crate::image_registry::no_images(),
            Rect::new(0.0, 0.0, 200.0, 200.0),
            &mut scene,
            dpi_96(),
            &default_theme(),
        );
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(fills, 3);
        assert_eq!(strokes, 3);
    }
    #[test]
    fn linewidth_scaled_line_keys_do_not_overlap_their_neighbours() {
        // The widest row of a linewidth legend is several times the
        // theme's key height, so the rows have to be sized from the
        // strokes they draw rather than from the floor alone.
        let mut reg = build_registry();
        reg.insert(
            "category_linewidth",
            scale::discrete([
                Value::String(Arc::from("A")),
                Value::String(Arc::from("B")),
                Value::String(Arc::from("C")),
            ])
            .range_numbers([25.0, 30.0, 35.0]),
        );
        let legend = Legend::new("category_color")
            .key(LegendKeySpec::line().scaled("linewidth", "category_linewidth"));
        let mut scene = RecordingScene::default();
        let shapes = ShapeRegistry::with_builtins();
        render_legend(
            &legend,
            &reg,
            &shapes,
            &crate::image_registry::no_images(),
            Rect::new(0.0, 0.0, 300.0, 300.0),
            &mut scene,
            dpi_96(),
            &default_theme(),
        );
        // Each key is a horizontal segment, so its painted band is the
        // baseline ± half the stroke width (butt caps by default).
        let mut bands: Vec<(f64, f64)> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, path, .. } => {
                    let b = path.bounding_box();
                    Some((b.y0 - stroke.width * 0.5, b.y1 + stroke.width * 0.5))
                }
                _ => None,
            })
            .collect();
        assert_eq!(bands.len(), 3, "one line key per break");
        bands.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in bands.windows(2) {
            assert!(
                pair[0].1 <= pair[1].0 + 1e-9,
                "key bands overlap: {:?} into {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn line_plus_point_keys_emit_both() {
        // Two-key stack: Line under Point. Three rows → 3 line
        // strokes + 3 point fills (Point has no stroke binding here).
        let legend = Legend::new("category_color")
            .key(LegendKeySpec::line().scaled("stroke", "category_color"))
            .key(LegendKeySpec::point().scaled("fill", "category_color"));
        let reg = build_registry();
        let mut scene = RecordingScene::default();
        let shapes = ShapeRegistry::with_builtins();
        render_legend(
            &legend,
            &reg,
            &shapes,
            &crate::image_registry::no_images(),
            Rect::new(0.0, 0.0, 200.0, 200.0),
            &mut scene,
            dpi_96(),
            &default_theme(),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(strokes, 3, "one line stroke per row");
        assert_eq!(fills, 3, "one point fill per row");
    }

    #[test]
    fn resolve_anchor_top_right_six_pt() {
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let rect = resolve_anchor(panel, Anchor::TopRight, 6.0, (20.0, 10.0));
        // Legend's TR corner sits at (94, 6) = panel TR - inset; size is (20, 10).
        assert!((rect.x1 - 94.0).abs() < 1e-12);
        assert!((rect.y0 - 6.0).abs() < 1e-12);
        assert!((rect.x0 - 74.0).abs() < 1e-12);
        assert!((rect.y1 - 16.0).abs() < 1e-12);
    }

    #[test]
    fn resolve_anchor_centre_centres_on_panel() {
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let rect = resolve_anchor(panel, Anchor::Center, 6.0, (20.0, 10.0));
        // Centre anchor ignores inset — the legend bbox centre lands on the panel centre.
        let cx = (rect.x0 + rect.x1) * 0.5;
        let cy = (rect.y0 + rect.y1) * 0.5;
        assert!((cx - 50.0).abs() < 1e-12);
        assert!((cy - 50.0).abs() < 1e-12);
    }

    #[test]
    fn resolve_anchor_bottom_left() {
        let panel = Rect::new(10.0, 20.0, 110.0, 120.0);
        let rect = resolve_anchor(panel, Anchor::BottomLeft, 4.0, (30.0, 12.0));
        // BL anchor: legend BL = panel BL offset inward by 4 on both axes.
        assert!((rect.x0 - 14.0).abs() < 1e-12);
        assert!((rect.y1 - 116.0).abs() < 1e-12);
    }

    #[test]
    fn in_panel_legend_natural_size_is_nonzero_for_populated_stack() {
        let legend = Legend::new("category_color")
            .side(LegendSide::InPanel {
                anchor: Anchor::TopRight,
                inset_pt: 6.0,
            })
            .key(LegendKeySpec::point().scaled("fill", "category_color"));
        let reg = build_registry();
        let (w, h) = legend_stack_natural_size(
            &[&legend],
            &reg,
            &shape_reg(),
            &crate::image_registry::no_images(),
            dpi_96(),
            &default_theme(),
        );
        assert!(w > 0.0);
        assert!(h > 0.0);
    }

    #[test]
    fn in_panel_legend_natural_size_zero_for_empty_stack() {
        let legend = Legend::new("category_color").side(LegendSide::InPanel {
            anchor: Anchor::TopRight,
            inset_pt: 6.0,
        });
        let reg = build_registry();
        let (w, h) = legend_stack_natural_size(
            &[&legend],
            &reg,
            &shape_reg(),
            &crate::image_registry::no_images(),
            dpi_96(),
            &default_theme(),
        );
        assert_eq!(w, 0.0);
        assert_eq!(h, 0.0);
    }
}
