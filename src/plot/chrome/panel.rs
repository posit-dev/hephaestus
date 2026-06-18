//! Panel chrome — the visuals **inside** the plotting area, shared
//! by every projection.
//!
//! Every projection's panel chrome has the same structure, drawn in
//! this order so geoms paint on top:
//!
//! 1. **Background** fill, bounded by the panel outline.
//! 2. **Minor grid lines**, one set per channel (x / y for Cartesian,
//!    theta / radius for Polar) at each `scale.minor_breaks()` position.
//! 3. **Major grid lines**, same shape at each `scale.breaks()` position.
//! 4. **Panel outline stroke**, the boundary of the plotting area.
//!
//! The projection contributes the geometry — what the outline path
//! looks like, what a "grid line" is for each channel — via the
//! [`panel_outline_path`] / [`channel_grid_path`] free functions
//! below. The drawing order, styling, and scale-break iteration are
//! shared across all projections.

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect};
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::{stroke_from_line_element, stroke_from_rect_border};
use crate::plot::projection::{PolarEdgeStyle, PolarProjection, Projection};
use crate::plot::scale::Scale;
use crate::plot::theme::{LineElement, RectElement, Theme};
use crate::primitives::{
    annular_wedge, arc, circle, polygon, polyline, segment, wedge, PolygonOptions, PolylineOptions,
};
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use kurbo::Shape;

// ─── Entry point ────────────────────────────────────────────────────────────

/// Channels the projection consumes, in order. Channel 0 is x for
/// Cartesian, theta for Polar; channel 1 is y / radius. Passed in as
/// the scales bound to each channel — either may be `None`, in which
/// case the corresponding grid set is skipped.
pub struct PanelScales<'a> {
    pub channel_0: Option<&'a Scale>,
    pub channel_1: Option<&'a Scale>,
}

/// Draw the in-panel chrome: background fill, minor + major grid
/// lines for each channel, panel outline stroke. Drawn before the
/// geoms so they paint on top. Every visual element is sourced from
/// `theme` — `Element::Blank` skips that piece of chrome entirely.
pub fn draw_panel_chrome(
    scene: &mut dyn SceneBuilder,
    projection: &Projection,
    panel: Rect,
    scales: PanelScales<'_>,
    dpi: f64,
    theme: &Theme,
) {
    if panel.x1 <= panel.x0 || panel.y1 <= panel.y0 {
        return;
    }
    let corner_radius_px = panel_corner_radius_px(theme, dpi);
    let outline_path = panel_outline_path(projection, panel, corner_radius_px);

    // Background fill — sourced from theme.panel_background.
    if let Some(bg) = theme.panel_background.as_set() {
        fill_rect_element(scene, bg, &theme.palette, &outline_path);
    }

    // Grid lines, per channel. Minors drawn first so majors layer on
    // top at coincident fractions.
    let major_0 = theme.panel_grid_major.resolve(0);
    let minor_0 = theme.panel_grid_minor.resolve(0);
    if let Some(scale) = scales.channel_0 {
        draw_grid_lines(
            scene,
            scale,
            |frac| channel_grid_path(projection, panel, 0, frac),
            major_0,
            minor_0,
            &theme.palette,
            dpi,
        );
    }
    let major_1 = theme.panel_grid_major.resolve(1);
    let minor_1 = theme.panel_grid_minor.resolve(1);
    if let Some(scale) = scales.channel_1 {
        draw_grid_lines(
            scene,
            scale,
            |frac| channel_grid_path(projection, panel, 1, frac),
            major_1,
            minor_1,
            &theme.palette,
            dpi,
        );
    }

    // Panel outline.
    if let Some(border) = theme.panel_border.as_set() {
        stroke_rect_element_border(scene, border, &theme.palette, &outline_path, dpi);
    }
}

fn fill_rect_element(
    scene: &mut dyn SceneBuilder,
    rect: &RectElement,
    palette: &crate::plot::theme::Palette,
    path: &Path,
) {
    // `rect.fill` is `Option<ThemeColor>` — `None` after cascade
    // means an explicitly transparent interior (no fill drawn).
    let Some(fill) = rect.fill.clone() else {
        return;
    };
    let brush = Brush::Solid(fill.resolve(palette));
    scene.fill(
        FillRule::NonZero,
        Affine::IDENTITY,
        &brush,
        None,
        path,
        PickId::Skip,
    );
}

fn stroke_rect_element_border(
    scene: &mut dyn SceneBuilder,
    rect: &RectElement,
    palette: &crate::plot::theme::Palette,
    path: &Path,
    dpi: f64,
) {
    use crate::plot::theme::rect_concrete_defaults;
    let defaults = rect_concrete_defaults();
    let lw = rect
        .linewidth_pt
        .or(defaults.linewidth_pt)
        .expect("rect linewidth default");
    if lw.resolve(1.0) <= 0.0 {
        return;
    }
    let stroke = stroke_from_rect_border(rect, dpi);
    let color = rect
        .color
        .clone()
        .or(defaults.color)
        .expect("rect color default");
    let brush = Brush::Solid(color.resolve(palette));
    scene.stroke(&stroke, Affine::IDENTITY, &brush, None, path, PickId::Skip);
}

/// Iterate the scale's minor and major breaks, stroking each
/// returned path with the appropriate brush. Minors first so a
/// major painted at the same frac wins on top. `major` / `minor`
/// are optional theme elements — `None` (Blank or unresolved)
/// suppresses that level entirely.
#[allow(clippy::too_many_arguments)]
fn draw_grid_lines<F>(
    scene: &mut dyn SceneBuilder,
    scale: &Scale,
    mut path_at: F,
    major: Option<&LineElement>,
    minor: Option<&LineElement>,
    palette: &crate::plot::theme::Palette,
    dpi: f64,
) where
    F: FnMut(f64) -> Option<Path>,
{
    use crate::plot::theme::line_concrete_defaults;
    let line_defaults = line_concrete_defaults();
    let resolve_color = |el: &LineElement| {
        let c = el
            .color
            .clone()
            .or_else(|| line_defaults.color.clone())
            .expect("line color default");
        Brush::Solid(c.resolve(palette))
    };
    let minor_resolved = minor.map(|el| (stroke_from_line_element(el, dpi), resolve_color(el)));
    let major_resolved = major.map(|el| (stroke_from_line_element(el, dpi), resolve_color(el)));

    if let Some((stroke, brush)) = &minor_resolved {
        for v in scale.minor_breaks(DEFAULT_BREAK_COUNT) {
            if matches!(v, Value::Null) {
                continue;
            }
            let frac = match scale.map(&v).as_number() {
                Some(f) if f.is_finite() && (0.0..=1.0).contains(&f) => f,
                _ => continue,
            };
            if let Some(path) = path_at(frac) {
                scene.stroke(stroke, Affine::IDENTITY, brush, None, &path, PickId::Skip);
            }
        }
    }
    if let Some((stroke, brush)) = &major_resolved {
        for v in scale.breaks(DEFAULT_BREAK_COUNT) {
            if matches!(v, Value::Null) {
                continue;
            }
            let frac = match scale.map(&v).as_number() {
                Some(f) if f.is_finite() && (0.0..=1.0).contains(&f) => f,
                _ => continue,
            };
            if let Some(path) = path_at(frac) {
                scene.stroke(stroke, Affine::IDENTITY, brush, None, &path, PickId::Skip);
            }
        }
    }
}

// ─── Per-projection geometry ────────────────────────────────────────────────

/// Closed path tracing the boundary of the plotting area. Used for
/// background fill, panel outline stroke, and the geom clip mask.
/// `corner_radius_px` rounds the Cartesian panel's four corners; the
/// polar panel is already curved and ignores it.
pub fn panel_outline_path(projection: &Projection, panel: Rect, corner_radius_px: f64) -> Path {
    match projection {
        Projection::Cartesian => {
            if corner_radius_px > 0.0 {
                crate::primitives::rounded_rect(panel, corner_radius_px)
            } else {
                panel.to_path(0.0)
            }
        }
        Projection::Polar(p) => polar_panel_outline(p, panel),
    }
}

/// Resolve the panel's corner radius from `theme.panel_background`'s
/// `corner_radius` (falling through to the rect concrete defaults
/// when None). Returns 0 for `Element::Blank` or sharp corners.
pub fn panel_corner_radius_px(theme: &Theme, dpi: f64) -> f64 {
    use crate::plot::theme::rect_concrete_defaults;
    let Some(bg) = theme.panel_background.as_set() else {
        return 0.0;
    };
    let defaults = rect_concrete_defaults();
    let pt = bg
        .corner_radius
        .or(defaults.corner_radius)
        .map(|l| l.resolve(0.0))
        .unwrap_or(0.0);
    (pt * dpi / 72.0).max(0.0)
}

/// Grid line for `channel` (0 or 1) at fraction `frac` ∈ [0, 1].
/// Returns `None` when the grid line should be omitted (e.g., the
/// full-circle duplicate at `theta_frac == 1`, or a degenerate
/// zero-radius ring).
pub fn channel_grid_path(
    projection: &Projection,
    panel: Rect,
    channel: usize,
    frac: f64,
) -> Option<Path> {
    if !frac.is_finite() || !(0.0..=1.0).contains(&frac) {
        return None;
    }
    match projection {
        Projection::Cartesian => Some(cartesian_grid(panel, channel, frac)),
        Projection::Polar(p) => polar_grid(p, panel, channel, frac),
    }
}

// ─── Cartesian ──────────────────────────────────────────────────────────────

fn cartesian_grid(panel: Rect, channel: usize, frac: f64) -> Path {
    let w = panel.x1 - panel.x0;
    let h = panel.y1 - panel.y0;
    if channel == 0 {
        let x = panel.x0 + frac * w;
        segment(Point::new(x, panel.y0), Point::new(x, panel.y1))
    } else {
        let y = panel.y1 - frac * h;
        segment(Point::new(panel.x0, y), Point::new(panel.x1, y))
    }
}

// ─── Polar ──────────────────────────────────────────────────────────────────

fn polar_grid(p: &PolarProjection, panel: Rect, channel: usize, frac: f64) -> Option<Path> {
    let g = p.geometry(panel);
    let span = p.theta_end - p.theta_start;
    let is_full_circle = (span.abs() - std::f64::consts::TAU).abs() < 1e-6;
    let is_chord = matches!(p.edge_style, PolarEdgeStyle::Chord);

    if channel == 0 {
        // Spoke at theta_for_frac(frac), from r_inner to r_outer. Skip
        // the full-circle duplicate at frac=1 (same physical spoke as 0).
        if is_full_circle && frac >= 1.0 - 1e-9 {
            return None;
        }
        let theta = p.theta_for_frac(frac);
        let p_in = Point::new(
            g.cx + g.r_inner * theta.cos(),
            g.cy - g.r_inner * theta.sin(),
        );
        let p_out = Point::new(
            g.cx + g.r_outer * theta.cos(),
            g.cy - g.r_outer * theta.sin(),
        );
        Some(segment(p_in, p_out))
    } else {
        // Ring at radius r_inner + frac * (r_outer - r_inner).
        let r_px = g.r_inner + frac * (g.r_outer - g.r_inner);
        if r_px <= 0.0 {
            return None;
        }
        let centre = Point::new(g.cx, g.cy);
        Some(if is_chord && !p.theta_break_fracs.is_empty() {
            polar_polygon_ring(p, centre, r_px, is_full_circle)
        } else if is_full_circle {
            circle(centre, r_px)
        } else {
            // Negate so the math-convention CCW arc renders as the
            // visual sweep (consistent with how the projection maps
            // angles into screen y-down space).
            arc(centre, r_px, -p.theta_start, -span)
        })
    }
}

/// Path for the chord-style "ring" at a given pixel radius — polygon
/// vertices at each `theta_break_frac` (extended to `theta_start` /
/// `theta_end` for partial arcs). Closed for full-circle radars,
/// open polyline for partial-arc radars.
fn polar_polygon_ring(
    p: &PolarProjection,
    centre: Point,
    radius: f64,
    is_full_circle: bool,
) -> Path {
    let mut thetas: Vec<f64> = Vec::with_capacity(p.theta_break_fracs.len() + 2);
    if !is_full_circle {
        thetas.push(p.theta_start);
    }
    for &frac in &p.theta_break_fracs {
        thetas.push(p.theta_for_frac(frac));
    }
    if !is_full_circle {
        thetas.push(p.theta_end);
    }
    let mut pts: Vec<Point> = thetas
        .iter()
        .map(|&t| Point::new(centre.x + radius * t.cos(), centre.y - radius * t.sin()))
        .collect();
    if is_full_circle && pts.len() >= 2 {
        let first = pts[0];
        pts.push(first);
    }
    polyline(&pts, PolylineOptions::default())
}

/// Closed boundary path for the polar plotting area: outer ring
/// (arc or polygon) + side caps + inner ring (if the projection has
/// a non-zero `inner_radius_frac`). Filled for the background and
/// stroked for the outline.
fn polar_panel_outline(p: &PolarProjection, panel: Rect) -> Path {
    let g = p.geometry(panel);
    let span = p.theta_end - p.theta_start;
    let is_full_circle = (span.abs() - std::f64::consts::TAU).abs() < 1e-6;
    let is_chord = matches!(p.edge_style, PolarEdgeStyle::Chord);
    let centre = Point::new(g.cx, g.cy);
    let has_inner = g.r_inner > 0.0;

    match (is_chord, is_full_circle, has_inner) {
        // Geodesic full disk.
        (false, true, false) => circle(centre, g.r_outer),
        // Geodesic full annulus — two subpaths, callers fill with
        // NonZero (the inner subpath's reversed winding cancels the
        // outer fill, leaving a true ring).
        (false, true, true) => {
            let mut path = circle(centre, g.r_outer);
            // Inner circle traced CW (opposite winding) to cancel the
            // outer's fill. `circle` returns CCW by default; we
            // reverse-construct manually here.
            let inner = reverse_circle(centre, g.r_inner);
            for el in inner.elements() {
                path.push(*el);
            }
            path
        }
        // Geodesic partial pie / annular wedge.
        (false, false, false) => wedge(centre, g.r_outer, -p.theta_start, -span),
        (false, false, true) => annular_wedge(centre, g.r_inner, g.r_outer, -p.theta_start, -span),
        // Chord-style full polygon / annular polygon.
        (true, true, false) => closed_polygon_ring(p, centre, g.r_outer),
        (true, true, true) => {
            let mut path = closed_polygon_ring(p, centre, g.r_outer);
            // Reverse-wound inner polygon so the annulus fills correctly.
            for el in reverse_polygon_ring(p, centre, g.r_inner).elements() {
                path.push(*el);
            }
            path
        }
        // Chord-style partial: trace outer polygon (theta_start →
        // breaks → theta_end), then inner polygon back (if any).
        (true, false, false) => chord_partial_filled(p, centre, g.r_outer, 0.0),
        (true, false, true) => chord_partial_filled(p, centre, g.r_outer, g.r_inner),
    }
}

fn closed_polygon_ring(p: &PolarProjection, centre: Point, radius: f64) -> Path {
    let pts: Vec<Point> = p
        .theta_break_fracs
        .iter()
        .map(|&frac| {
            let t = p.theta_for_frac(frac);
            Point::new(centre.x + radius * t.cos(), centre.y - radius * t.sin())
        })
        .collect();
    polygon(&[&pts], PolygonOptions::default())
}

fn reverse_polygon_ring(p: &PolarProjection, centre: Point, radius: f64) -> Path {
    let mut pts: Vec<Point> = p
        .theta_break_fracs
        .iter()
        .map(|&frac| {
            let t = p.theta_for_frac(frac);
            Point::new(centre.x + radius * t.cos(), centre.y - radius * t.sin())
        })
        .collect();
    pts.reverse();
    polygon(&[&pts], PolygonOptions::default())
}

fn reverse_circle(centre: Point, radius: f64) -> Path {
    // kurbo's Arc with a negative sweep traces clockwise — reverse of
    // the default `circle` (CCW).
    arc(centre, radius, 0.0, -std::f64::consts::TAU)
}

/// Closed boundary for a chord-style partial arc: outer polygon from
/// `theta_start` through each break to `theta_end`, then either back
/// to centre (no inner) or back along the inner polygon (with inner).
fn chord_partial_filled(p: &PolarProjection, centre: Point, r_outer: f64, r_inner: f64) -> Path {
    let thetas: Vec<f64> = std::iter::once(p.theta_start)
        .chain(p.theta_break_fracs.iter().map(|&f| p.theta_for_frac(f)))
        .chain(std::iter::once(p.theta_end))
        .collect();
    let outer: Vec<Point> = thetas
        .iter()
        .map(|&t| Point::new(centre.x + r_outer * t.cos(), centre.y - r_outer * t.sin()))
        .collect();
    let inner: Vec<Point> = if r_inner > 0.0 {
        thetas
            .iter()
            .rev()
            .map(|&t| Point::new(centre.x + r_inner * t.cos(), centre.y - r_inner * t.sin()))
            .collect()
    } else {
        // Pie slice: close back through the centre.
        vec![centre]
    };
    let mut pts = outer;
    pts.extend(inner);
    polygon(&[&pts], PolygonOptions::default())
}
