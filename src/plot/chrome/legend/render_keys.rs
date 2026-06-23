//! Per-key swatch dim + render for legend rows.
//!
//! Each row's marker (Point / Line / Rect) is drawn by one of the
//! per-shape helpers in this module. The top-level [`Legend`] renderer
//! computes the cell rect for each row, walks the stack of keys, and
//! dispatches to [`render_key`] which fans out to the right shape
//! emitter. [`swatch_dim_for`] reports the minimum cell size each
//! shape needs so the legend can size its cell to fit the worst
//! contributor.

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point, Rect};
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::pt_to_px;
use crate::plot::geom::point::GLYPH_BBOX_REFERENCE;
use crate::primitives::{circle, segment};
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::shape::builtin::REFERENCE_RADIUS as POINT_SHAPE_RADIUS;
use crate::shape::{ShapeKind, ShapeRegistry, ShapeStyle};
use crate::stroke::Stroke;

use kurbo::Shape;

use super::{LegendKey, ResolvedKey};

/// Per-key minimum cell dimensions `(w, h)` in px. The legend takes
/// the max across keys to size the cell, then floors at the
/// `LegendTheme.key` width / height. Lines never grow the cell —
/// they render at the resolved cell's width via relative
/// coordinates (line spans 0..1 horizontally, sits at 0.5
/// vertically). Points grow the cell by their marker diameter.
/// Rects don't impose a minimum beyond the theme floor.
pub(super) fn swatch_dim_for(
    kind: LegendKey,
    peak: &ResolvedKey,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
) -> (f64, f64) {
    match kind {
        LegendKey::Point => {
            let size_pt = peak.size_pt.unwrap_or(geom.point.size_pt);
            // Match PointGeom's circle path (radius 0.8) so the
            // rendered marker matches the geom for the same size.
            let d = pt_to_px(size_pt * 2.0 * POINT_SHAPE_RADIUS, dpi);
            (d, d)
        }
        LegendKey::Line | LegendKey::Rect => (0.0, 0.0),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_key(
    kind: LegendKey,
    resolved: &ResolvedKey,
    cell: Rect,
    shapes: &ShapeRegistry,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
) {
    match kind {
        LegendKey::Point => render_point(resolved, cell, shapes, scene, dpi, geom),
        LegendKey::Line => render_line(resolved, cell, scene, dpi, geom, palette),
        LegendKey::Rect => render_rect(resolved, cell, scene, dpi, geom, palette),
    }
}

pub(super) fn apply_alpha(c: Color, alpha: Option<f64>) -> Color {
    match alpha {
        Some(a) => {
            let [r, g, b, base] = c.components;
            let combined = (base as f64 * a.clamp(0.0, 1.0)) as f32;
            Color::new([r, g, b, combined])
        }
        None => c,
    }
}

fn render_point(
    resolved: &ResolvedKey,
    cell: Rect,
    shapes: &ShapeRegistry,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
) {
    let size_pt = resolved.size_pt.unwrap_or(geom.point.size_pt);
    let size_px = pt_to_px(size_pt, dpi);
    let centre = Point::new(
        cell.x0 + (cell.x1 - cell.x0) * 0.5,
        cell.y0 + (cell.y1 - cell.y0) * 0.5,
    );

    // Honour `resolved.shape` if it names a registered shape with
    // path content. Same scaling convention as `PointGeom` (the
    // shape's path is scaled by `size_px`). For Glyph-backed
    // shapes (font glyphs) we fall back to the default circle —
    // the legend chrome doesn't currently shape glyph markers.
    let shape = resolved.shape.as_deref().and_then(|name| shapes.get(name));
    let xform = Affine::translate((centre.x, centre.y)) * Affine::scale(size_px);

    let fill_color = resolved
        .fill
        .map(|c| Brush::Solid(apply_alpha(c, resolved.alpha)));
    let stroke_brush = resolved
        .stroke
        .map(|c| Brush::Solid(apply_alpha(c, resolved.alpha)));
    let stroke = stroke_brush.as_ref().map(|_| {
        Stroke::new(pt_to_px(
            resolved.linewidth_pt.unwrap_or(geom.point.stroke_width_pt),
            dpi,
        ))
    });

    if let Some(s) = shape {
        match s.kind() {
            ShapeKind::Paths { paths, style } => {
                for sub in paths {
                    match style {
                        ShapeStyle::Fill => {
                            if let Some(fill) = &fill_color {
                                scene.fill(FillRule::NonZero, xform, fill, None, sub, PickId::Skip);
                            }
                            if let (Some(stroke_brush), Some(stroke)) = (&stroke_brush, &stroke) {
                                scene.stroke(stroke, xform, stroke_brush, None, sub, PickId::Skip);
                            }
                        }
                        ShapeStyle::Stroke => {
                            if let (Some(stroke_brush), Some(stroke)) = (&stroke_brush, &stroke) {
                                scene.stroke(stroke, xform, stroke_brush, None, sub, PickId::Skip);
                            }
                        }
                    }
                }
                return;
            }
            ShapeKind::Glyph {
                font,
                glyph_id,
                em_bbox,
                em_origin,
            } => {
                // Glyph marker — bake the em-to-pixel scale into
                // `font_size` rather than into the transform so
                // vello picks the right bitmap strike for colour
                // emoji fonts. Outline (scalable) fonts are
                // unaffected; bitmap fonts ship discrete strikes
                // at fixed pixel sizes and `font_size: 1.0` would
                // pick the smallest one and upscale (= fuzzy at
                // typical chart sizes).
                let Some(fill) = &fill_color else { return };
                let h = em_bbox.height();
                if !(h.is_finite() && h > 0.0) {
                    return;
                }
                let bbox_norm = GLYPH_BBOX_REFERENCE / h;
                let effective_font_size_px = size_px * bbox_norm;
                // The original transform multiplied em-space by
                // `size_px * bbox_norm`; doing that via `font_size`
                // means the transform is just a translate to the
                // cell centre + the em-space centring offset
                // converted to pixels.
                let centring_px =
                    (em_origin.to_vec2() - em_bbox.center().to_vec2()) * effective_font_size_px;
                let glyphs = [Glyph {
                    id: glyph_id,
                    x: 0.0,
                    y: 0.0,
                }];
                let run = GlyphRun {
                    font,
                    font_size: effective_font_size_px as f32,
                    transform: Affine::translate((
                        centre.x + centring_px.x,
                        centre.y + centring_px.y,
                    )),
                    glyph_transform: None,
                    brush: fill,
                    brush_alpha: 1.0,
                    hint: false,
                    glyphs: &glyphs,
                    style: None,
                };
                scene.draw_glyphs(&run, PickId::Skip);
                return;
            }
        }
    }

    // Default / fallback: circle, sized to match PointGeom's
    // built-in circle (radius 0.8 in shape space).
    let radius = size_px * POINT_SHAPE_RADIUS;
    let path = circle(centre, radius);
    if let Some(fill) = &fill_color {
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            fill,
            None,
            &path,
            PickId::Skip,
        );
    }
    if let (Some(stroke_brush), Some(stroke)) = (&stroke_brush, &stroke) {
        scene.stroke(
            stroke,
            Affine::IDENTITY,
            stroke_brush,
            None,
            &path,
            PickId::Skip,
        );
    }
}

fn render_line(
    resolved: &ResolvedKey,
    cell: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
) {
    // Pick stroke colour: explicit `stroke` channel wins, else fall
    // back to `fill` (callers sometimes write `color` → fill on the
    // ResolvedKey via the alias in `apply`). Geom-default line stroke
    // backstops both — palette-driven, no hardcoded black fallback.
    let color = resolved
        .stroke
        .or(resolved.fill)
        .map(|c| apply_alpha(c, resolved.alpha))
        .unwrap_or_else(|| {
            geom.line
                .stroke
                .as_ref()
                .map(|c| c.resolve(palette))
                .unwrap_or(palette.ink)
        });
    let lw_pt = resolved.linewidth_pt.unwrap_or(geom.line.linewidth_pt);
    let mid_y = cell.y0 + (cell.y1 - cell.y0) * 0.5;
    let p0 = Point::new(cell.x0, mid_y);
    let p1 = Point::new(cell.x1, mid_y);
    let path = segment(p0, p1);
    let stroke = match &resolved.linetype {
        Some(pattern) if !pattern.is_empty() => {
            let dashes_pt = crate::plot::geom::linetype::to_kurbo_dashes(pattern);
            let dashes_px: Vec<f64> = dashes_pt.into_iter().map(|d| pt_to_px(d, dpi)).collect();
            Stroke::new(pt_to_px(lw_pt, dpi)).with_dashes(0.0, dashes_px)
        }
        _ => Stroke::new(pt_to_px(lw_pt, dpi)),
    };
    scene.stroke(
        &stroke,
        Affine::IDENTITY,
        &Brush::Solid(color),
        None,
        &path,
        PickId::Skip,
    );
}

fn render_rect(
    resolved: &ResolvedKey,
    cell: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
) {
    let path: Path = cell.to_path(0.0);
    if let Some(fill) = resolved.fill {
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Solid(apply_alpha(fill, resolved.alpha)),
            None,
            &path,
            PickId::Skip,
        );
    }
    if let Some(stroke_color) = resolved.stroke {
        let lw = resolved.linewidth_pt.unwrap_or(geom.rect.linewidth_pt);
        let stroke = Stroke::new(pt_to_px(lw, dpi));
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &Brush::Solid(apply_alpha(stroke_color, resolved.alpha)),
            None,
            &path,
            PickId::Skip,
        );
    } else if resolved.fill.is_none() {
        // Placeholder outline so the row isn't visually empty —
        // palette ink so dark themes don't render an invisible
        // black-on-black stub.
        let stroke = Stroke::new(pt_to_px(geom.rect.linewidth_pt, dpi));
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &Brush::Solid(palette.ink),
            None,
            &path,
            PickId::Skip,
        );
    }
}
