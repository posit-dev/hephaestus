//! Per-key swatch dim + render for legend rows.
//!
//! Each row's marker (Point / Line / Rect / Text) is drawn by one of
//! the per-shape helpers in this module. The top-level [`Legend`] renderer
//! computes the cell rect for each row, walks the stack of keys, and
//! dispatches to [`render_key`] which fans out to the right shape
//! emitter. [`swatch_dim_for`] reports the minimum cell size each
//! shape needs so the legend can size its cell to fit the worst
//! contributor.
//!
//! The Point key marker mirrors the geom: same intrinsic shape-space
//! radius as the builtin `circle`, same glyph bbox normalisation as
//! `geom::point`. Both are sourced from the canonical definitions so
//! legend and panel markers can't drift.

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point, Rect};
use crate::linetype::MARKER_INK_COVERAGE_BOOST;
use crate::path::FillRule;
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::pt_to_px;
use crate::plot::chrome::text::{ChromeRun, RichChrome};
use crate::plot::geom::outline::{draw_curve_outline, EndpointMarker, OutlineSpec};
use crate::plot::geom::point::GLYPH_BBOX_REFERENCE;
use crate::plot::geom::resolve::{auto_endpoint_clip_pt, endpoint_marker_outline_px};
use crate::primitives::{circle, rounded_rect};
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::shape::builtin::REFERENCE_RADIUS as POINT_SHAPE_RADIUS;
use crate::shape::{ShapeKind, ShapeRegistry, ShapeStyle};
use crate::stroke::{Cap, Join, Stroke};
use crate::text::TextStyle;

use crate::geometry::Shape as _;
use std::sync::Arc;

use super::{EndpointMarkerKey, LegendKey, ResolvedKey};

/// Glyph a [`LegendKey::Text`] draws when its `"text"` aesthetic is
/// unbound — a lowercase letter, so the key shows the ascender and
/// x-height a font size, weight or family scale is acting on.
pub const DEFAULT_KEY_TEXT: &str = "a";

/// Per-key minimum cell dimensions `(w, h)` in px — the painted
/// extent of the key's marker, so nothing a key draws lands outside
/// its cell. The legend takes the max across keys to size the cell,
/// then floors at the `LegendTheme.key` width / height.
///
/// Points reserve their shape's own bbox — rotated with the marker —
/// at the resolved size plus the outline width; lines and rects the
/// stroke width; text its shaped inked box, likewise rotated. A round-
/// or square-capped line also reserves a body at least as long as the
/// stroke is thick, so [`render_line`]'s cap inset can't collapse a
/// thick key to a dot.
pub(super) fn swatch_dim_for(
    kind: LegendKey,
    peak: &ResolvedKey,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    shapes: &ShapeRegistry,
    // Only the text key reads it — for the sheet and palette a
    // markdown swatch shapes through, so the reserved cell matches
    // what `render_text` paints.
    theme: &crate::plot::theme::Theme,
) -> (f64, f64) {
    match kind {
        LegendKey::Point => {
            let size_px = pt_to_px(peak.size_pt.unwrap_or(geom.point.size_pt), dpi);
            let shape = peak.shape.as_deref().and_then(|name| shapes.get(name));
            let (half_w, half_h) = rotate_half_extents(shape_half_extents(shape), peak.angle);
            // The outline straddles the path, so it adds half its
            // width on each side of the marker's own bbox.
            let outline = match point_paints(peak, geom, shape).1 {
                true => pt_to_px(peak.linewidth_pt.unwrap_or(geom.point.stroke_width_pt), dpi),
                false => 0.0,
            };
            (
                half_w * 2.0 * size_px + outline,
                half_h * 2.0 * size_px + outline,
            )
        }
        LegendKey::Line => {
            let lw_pt = peak.linewidth_pt.unwrap_or(geom.line.linewidth_pt);
            let lw = pt_to_px(lw_pt, dpi);
            let cap_body = match peak.cap.unwrap_or(geom.line.cap) {
                Cap::Butt => 0.0,
                Cap::Round | Cap::Square => lw * 2.0,
            };
            // Endpoint markers eat their forward extent off the line, so
            // the cell carries that on top of a body at least as long as
            // the stroke is thick — otherwise the trim leaves nothing.
            let (fwd_start, h_start) = marker_extents(&peak.start_marker, lw_pt, dpi, shapes);
            let (fwd_end, h_end) = marker_extents(&peak.end_marker, lw_pt, dpi, shapes);
            let markers = fwd_start + fwd_end;
            let body = match markers > 0.0 {
                true => cap_body.max(lw * 2.0),
                false => cap_body,
            };
            (body + markers, lw.max(h_start).max(h_end))
        }
        LegendKey::Rect => {
            // The border is inset to sit inside the cell, so the cell
            // only has to be wide enough to hold it.
            let lw = match rect_paints(peak, geom).1 {
                true => pt_to_px(peak.linewidth_pt.unwrap_or(geom.rect.linewidth_pt), dpi),
                false => 0.0,
            };
            (lw, lw)
        }
        LegendKey::Text => {
            let rich = text_key_rich(peak, theme, geom, dpi);
            let Some(run) = text_key_run(peak, dpi, geom, rich.as_ref()) else {
                return (0.0, 0.0);
            };
            // The inked box — ascender top to descender bottom — is
            // what `render_text` centres, so it's what the cell holds.
            let (half_w, half_h) =
                rotate_half_extents((run.width() * 0.5, run.inked_height() * 0.5), peak.angle);
            // The glyph outline straddles the letterform, adding half
            // its width on each side of the text box.
            let outline = match text_paints_outline(peak, geom) {
                true => pt_to_px(
                    peak.text_linewidth_pt
                        .unwrap_or(geom.text.text_linewidth_pt),
                    dpi,
                ),
                false => 0.0,
            };
            (half_w * 2.0 + outline, half_h * 2.0 + outline)
        }
    }
}

/// Shape a text key's glyph run from its resolved aesthetics, laid out
/// unwrapped. `None` when the key would paint nothing — a degenerate
/// font size or an empty string, the guards `TextGeom` applies per row.
fn text_key_run(
    resolved: &ResolvedKey,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    rich: Option<&RichChrome>,
) -> Option<ChromeRun> {
    let size_pt = resolved.size_pt.unwrap_or(geom.text.size_pt);
    if !size_pt.is_finite() || size_pt <= 0.0 {
        return None;
    }
    let text = resolved.text.as_deref().unwrap_or(DEFAULT_KEY_TEXT);
    if text.is_empty() {
        return None;
    }
    let mut style = TextStyle::new(size_pt as f32)
        .weight(resolved.weight.unwrap_or(geom.text.weight))
        .italic(resolved.italic.unwrap_or(false))
        .tracking(resolved.tracking.unwrap_or(geom.text.tracking) as f32)
        .underline(resolved.underline.unwrap_or(geom.text.underline))
        .strikethrough(resolved.strikethrough.unwrap_or(geom.text.strikethrough));
    if let Some(family) = resolved.family.as_deref() {
        style = style.family(family);
    }
    Some(ChromeRun::shape(text, &style, dpi, rich))
}

/// Whether a text key draws a per-glyph outline under its fill. The
/// aesthetic wins and the geom default backs it up; unlike the shape
/// keys there's no fallback, since a text key always paints its fill.
fn text_paints_outline(resolved: &ResolvedKey, geom: &crate::plot::theme::GeomTheme) -> bool {
    resolved.text_stroke.is_some() || geom.text.text_stroke.is_some()
}

/// Forward extent and painted height of a line key's endpoint marker,
/// in px. Forward extent is what [`draw_curve_outline`] trims off the
/// line so the marker's tip lands at the line's own end; the height is
/// what the cell has to hold, measured from the line the marker sits on
/// (its anchor) since placement puts that anchor on the baseline.
/// `(0, 0)` when the endpoint carries no usable marker.
fn marker_extents(
    key: &EndpointMarkerKey,
    linewidth_pt: f64,
    dpi: f64,
    shapes: &ShapeRegistry,
) -> (f64, f64) {
    let marker = endpoint_marker(key, linewidth_pt);
    if marker.name.is_empty() || !(marker.size_pt.is_finite() && marker.size_pt > 0.0) {
        return (0.0, 0.0);
    }
    let Some(shape) = shapes.get(&marker.name) else {
        return (0.0, 0.0);
    };
    let size_px = pt_to_px(marker.size_pt, dpi);
    let forward = pt_to_px(
        auto_endpoint_clip_pt(&marker.name, marker.size_pt, marker.invert, shapes),
        dpi,
    );
    let bbox = shape.bounding_box();
    let (half_h, outline) = match shape.kind() {
        ShapeKind::Paths { style, .. } => {
            let anchor_y = shape.anchor().y;
            let half = (bbox.y0 - anchor_y).abs().max((bbox.y1 - anchor_y).abs());
            // A stroked marker straddles its own path; a filled one
            // paints inside it.
            let outline = match style {
                ShapeStyle::Stroke => endpoint_marker_outline_px(pt_to_px(linewidth_pt, dpi), dpi),
                ShapeStyle::Fill => 0.0,
            };
            (half, outline)
        }
        // Glyph markers scale their font size up by the ink-coverage
        // boost and centre on the anchor.
        ShapeKind::Glyph { .. } => (bbox.height() * MARKER_INK_COVERAGE_BOOST * 0.5, 0.0),
    };
    (forward, half_h * 2.0 * size_px + outline)
}

/// Half-extents of a marker's box after rotating it by `angle`
/// radians — the axis-aligned bounds the rotated marker occupies.
fn rotate_half_extents((half_w, half_h): (f64, f64), angle: Option<f64>) -> (f64, f64) {
    match angle {
        Some(a) if a != 0.0 && a.is_finite() => {
            let (sin, cos) = (a.sin().abs(), a.cos().abs());
            (half_w * cos + half_h * sin, half_w * sin + half_h * cos)
        }
        _ => (half_w, half_h),
    }
}

/// Half-width and half-height of a point shape's own bbox, in
/// multiples of the `"size"` aesthetic. Falls back to the built-in
/// circle's radius for an absent shape — what [`render_point`] draws
/// in that case. Off-centre shapes report their widest side, since the
/// marker is placed by its origin rather than its bbox centre.
fn shape_half_extents(shape: Option<&crate::shape::Shape>) -> (f64, f64) {
    let Some(shape) = shape else {
        return (POINT_SHAPE_RADIUS, POINT_SHAPE_RADIUS);
    };
    match shape.kind() {
        ShapeKind::Paths { paths, .. } => {
            let (mut half_w, mut half_h) = (0.0_f64, 0.0_f64);
            for sub in paths {
                let b = sub.bounding_box();
                half_w = half_w.max(b.x0.abs()).max(b.x1.abs());
                half_h = half_h.max(b.y0.abs()).max(b.y1.abs());
            }
            (half_w, half_h)
        }
        ShapeKind::Glyph { em_bbox, .. } => {
            // The glyph branch normalises em-box height to
            // `GLYPH_BBOX_REFERENCE`, so height matches a vector
            // shape at the same size and width follows the aspect.
            let h = em_bbox.height();
            let half_h = GLYPH_BBOX_REFERENCE * 0.5;
            match h.is_finite() && h > 0.0 {
                true => (half_h * em_bbox.width() / h, half_h),
                false => (half_h, half_h),
            }
        }
    }
}

/// The stroke a dashed or solid key paints with, built through the
/// same helper the geoms use so a key dashes, phases and caps exactly
/// as the marks it stands for — including patterns carrying marker
/// steps, which count as gaps here just as they do for every geom
/// other than `LineGeom`.
fn key_stroke(
    resolved: &ResolvedKey,
    width_px: f64,
    linewidth_pt: f64,
    cap: Cap,
    join: Join,
    dpi: f64,
) -> Stroke {
    crate::plot::geom::resolve::build_stroke_for_pattern(
        width_px,
        cap,
        join,
        resolved.linetype.as_deref().unwrap_or(&[]),
        resolved.dash_offset_pt.unwrap_or(0.0),
        linewidth_pt,
        dpi,
    )
}

/// Whether a rect key paints a fill and / or a border, as
/// `(fill, border)`. Aesthetic colours win and the geom defaults back
/// them up; a key carrying neither falls back on the border so the row
/// isn't visually empty.
fn rect_paints(resolved: &ResolvedKey, geom: &crate::plot::theme::GeomTheme) -> (bool, bool) {
    let fill = resolved.fill.is_some() || geom.rect.fill.is_some();
    let stroke = resolved.stroke.is_some() || geom.rect.stroke.is_some();
    (fill, stroke || !fill)
}

/// Whether a point key paints a fill and / or an outline, as
/// `(fill, stroke)`. Aesthetic colours win, the geom defaults back
/// them up, and a key carrying neither still paints through whichever
/// channel its shape draws with so the row isn't visually empty.
fn point_paints(
    resolved: &ResolvedKey,
    geom: &crate::plot::theme::GeomTheme,
    shape: Option<&crate::shape::Shape>,
) -> (bool, bool) {
    let mut fill = resolved.fill.is_some() || geom.point.fill.is_some();
    let mut stroke = resolved.stroke.is_some() || geom.point.stroke.is_some();
    if !fill && !stroke {
        match shape.map(|s| s.kind()) {
            Some(ShapeKind::Paths {
                style: ShapeStyle::Stroke,
                ..
            }) => stroke = true,
            _ => fill = true,
        }
    }
    (fill, stroke)
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
    theme: &crate::plot::theme::Theme,
) {
    match kind {
        LegendKey::Point => render_point(resolved, cell, shapes, scene, dpi, geom, palette),
        LegendKey::Line => render_line(resolved, cell, shapes, scene, dpi, geom, palette),
        LegendKey::Rect => render_rect(resolved, cell, scene, dpi, geom, palette),
        LegendKey::Text => render_text(resolved, cell, scene, dpi, geom, palette, theme),
    }
}

/// Replace a colour's alpha with an explicit opacity, clamped to
/// `[0, 1]`. Overriding rather than modulating matches what the geoms'
/// `"fill_opacity"` / `"stroke_opacity"` channels do, so a key shows
/// the alpha its geom draws with.
pub(super) fn with_opacity(c: Color, opacity: Option<f64>) -> Color {
    match opacity {
        Some(a) => {
            let [r, g, b, _] = c.components;
            Color::new([r, g, b, a.clamp(0.0, 1.0) as f32])
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
    palette: &crate::plot::theme::Palette,
) {
    let size_pt = resolved.size_pt.unwrap_or(geom.point.size_pt);
    let size_px = pt_to_px(size_pt, dpi);
    // A degenerate size collapses the marker to nothing; bail before
    // it can divide the stroke width. Matches `PointGeom`'s guard.
    if !size_px.is_finite() || size_px <= 0.0 {
        return;
    }
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
    // Rotation is negated so positive reads counter-clockwise on
    // screen, and applied about the marker's own centre — `PointGeom`'s
    // convention. Glyph markers don't rotate there either.
    let angle = resolved.angle.unwrap_or(0.0);
    let xform = match angle == 0.0 {
        true => Affine::translate((centre.x, centre.y)) * Affine::scale(size_px),
        false => {
            Affine::translate((centre.x, centre.y))
                * Affine::rotate(-angle)
                * Affine::scale(size_px)
        }
    };

    // Palette ink is the backstop for the channel `point_paints` picks
    // when neither the key nor the theme names a colour.
    let (paints_fill, paints_stroke) = point_paints(resolved, geom, shape);
    let fill = paints_fill.then(|| {
        resolved
            .fill
            .or_else(|| geom.point.fill.as_ref().map(|c| c.resolve(palette)))
            .unwrap_or(palette.ink)
    });
    let stroke_color = paints_stroke.then(|| {
        resolved
            .stroke
            .or_else(|| geom.point.stroke.as_ref().map(|c| c.resolve(palette)))
            .unwrap_or(palette.ink)
    });

    let fill_color = fill.map(|c| Brush::Solid(with_opacity(c, resolved.fill_opacity)));
    let stroke_brush = stroke_color.map(|c| Brush::Solid(with_opacity(c, resolved.stroke_opacity)));
    let stroke_width_px = pt_to_px(
        resolved.linewidth_pt.unwrap_or(geom.point.stroke_width_pt),
        dpi,
    );
    // Path-backed shapes draw under `Affine::scale(size_px)`, so the
    // stroke width is divided by the same factor to land at
    // `stroke_width_px` in output pixels — the inversion `PointGeom`
    // applies for the identical transform.
    let path_stroke = stroke_brush
        .as_ref()
        .map(|_| Stroke::new(stroke_width_px / size_px));
    let stroke = stroke_brush.as_ref().map(|_| Stroke::new(stroke_width_px));

    if let Some(s) = shape {
        match s.kind() {
            ShapeKind::Paths { paths, style } => {
                for sub in paths {
                    match style {
                        ShapeStyle::Fill => {
                            if let Some(fill) = &fill_color {
                                scene.fill(FillRule::NonZero, xform, fill, None, sub, PickId::Skip);
                            }
                            if let (Some(stroke_brush), Some(stroke)) =
                                (&stroke_brush, &path_stroke)
                            {
                                scene.stroke(stroke, xform, stroke_brush, None, sub, PickId::Skip);
                            }
                        }
                        ShapeStyle::Stroke => {
                            if let (Some(stroke_brush), Some(stroke)) =
                                (&stroke_brush, &path_stroke)
                            {
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
    shapes: &ShapeRegistry,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
) {
    // Pick stroke colour: explicit `stroke` channel wins, else fall
    // back to `fill` (callers sometimes write `color` → fill on the
    // ResolvedKey via the alias in `apply`). Geom-default line stroke
    // backstops both — palette-driven, no hardcoded black fallback.
    let color = with_opacity(
        resolved.stroke.or(resolved.fill).unwrap_or_else(|| {
            geom.line
                .stroke
                .as_ref()
                .map(|c| c.resolve(palette))
                .unwrap_or(palette.ink)
        }),
        resolved.stroke_opacity,
    );
    let lw_pt = resolved.linewidth_pt.unwrap_or(geom.line.linewidth_pt);
    let lw_px = pt_to_px(lw_pt, dpi);
    let cap = resolved.cap.unwrap_or(geom.line.cap);
    let join = resolved.join.unwrap_or(geom.line.join);
    // Caps that extend past the endpoint eat into the line's span so
    // the painted stroke stays inside the cell. Clamped to half the
    // cell so a stroke thicker than the cell degenerates to a dot
    // instead of reversing the segment.
    let inset = match cap {
        Cap::Butt => 0.0,
        Cap::Round | Cap::Square => (lw_px * 0.5).min((cell.x1 - cell.x0) * 0.5),
    };
    let mid_y = cell.y0 + (cell.y1 - cell.y0) * 0.5;
    let p0 = Point::new(cell.x0 + inset, mid_y);
    let p1 = Point::new(cell.x1 - inset, mid_y);
    // Stroke through the geoms' own curve helper: the key then dashes,
    // phases, clips for its endpoint markers and stamps them exactly as
    // `LineGeom` does for the marks it stands for.
    let spec = OutlineSpec {
        stroke_color: color,
        linewidth_pt: lw_pt,
        dash_pattern_pt: resolved
            .linetype
            .clone()
            .unwrap_or_else(|| Arc::from(Vec::new())),
        dash_offset_pt: resolved.dash_offset_pt.unwrap_or(0.0),
        cap,
        join,
        marker_fill: color,
        user_clip_start_pt: 0.0,
        user_clip_end_pt: 0.0,
        start_marker: endpoint_marker(&resolved.start_marker, lw_pt),
        end_marker: endpoint_marker(&resolved.end_marker, lw_pt),
        pick: PickId::Skip,
        xform: Affine::IDENTITY,
        corner_rounding: None,
    };
    draw_curve_outline(scene, shapes, dpi, geom.marker_outline_pt, &[p0, p1], &spec);
}

/// Translate a key's endpoint-marker aesthetics into the geoms'
/// [`EndpointMarker`], applying the same `3 × linewidth` size default
/// `LineGeom` uses. An unset shape yields a disabled marker.
fn endpoint_marker(key: &EndpointMarkerKey, linewidth_pt: f64) -> EndpointMarker {
    EndpointMarker {
        name: key.shape.as_deref().unwrap_or("").to_string(),
        size_pt: key.size_pt.unwrap_or(3.0 * linewidth_pt),
        fill: key.fill,
        invert: key.invert.unwrap_or(false),
    }
}

fn render_rect(
    resolved: &ResolvedKey,
    cell: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
) {
    // Palette ink is the backstop for the border `rect_paints` falls
    // back on when neither the key nor the theme names a colour, so the
    // row isn't visually empty — and ink rather than black so dark
    // themes don't draw an invisible stub.
    let (paints_fill, paints_border) = rect_paints(resolved, geom);
    let radius_px = pt_to_px(resolved.corner_radius_pt.unwrap_or(0.0), dpi).max(0.0);
    if paints_fill {
        let color = resolved
            .fill
            .or_else(|| geom.rect.fill.as_ref().map(|c| c.resolve(palette)))
            .unwrap_or(palette.ink);
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Solid(with_opacity(color, resolved.fill_opacity)),
            None,
            &rect_key_path(cell, radius_px),
            PickId::Skip,
        );
    }
    // The border straddles the path it follows, so it runs half a
    // linewidth inside the cell to keep the painted ring off the
    // neighbouring rows.
    if paints_border {
        let color = with_opacity(
            resolved
                .stroke
                .or_else(|| geom.rect.stroke.as_ref().map(|c| c.resolve(palette)))
                .unwrap_or(palette.ink),
            resolved.stroke_opacity,
        );
        let lw_pt = resolved.linewidth_pt.unwrap_or(geom.rect.linewidth_pt);
        let lw = pt_to_px(lw_pt, dpi);
        let inset = (lw * 0.5)
            .min((cell.x1 - cell.x0) * 0.5)
            .min((cell.y1 - cell.y0) * 0.5);
        let stroke = key_stroke(
            resolved,
            lw,
            lw_pt,
            resolved.cap.unwrap_or(geom.rect.cap),
            resolved.join.unwrap_or(geom.rect.join),
            dpi,
        );
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &Brush::Solid(color),
            None,
            // Shrinking the radius with the inset keeps the border's
            // corners concentric with the fill's.
            &rect_key_path(cell.inset(-inset), radius_px - inset),
            PickId::Skip,
        );
    }
}

/// The markdown context a text key shapes through, or `None` when
/// the geom theme reads text as plain. Mirrors `TextGeom`'s own
/// switch, so a legend key previews the geom it stands for rather
/// than the chrome around it. The key's resolved fill and outline
/// bake into the context because the rich pipeline paints both from
/// the sheet rather than from separate brushes.
fn text_key_rich(
    resolved: &ResolvedKey,
    theme: &crate::plot::theme::Theme,
    geom: &crate::plot::theme::GeomTheme,
    dpi: f64,
) -> Option<RichChrome> {
    if !geom.text.markdown {
        return None;
    }
    let palette = &theme.palette;
    let fill = text_key_fill(resolved, geom, palette);
    let outline = text_key_outline(resolved, geom, palette, dpi);
    // Keeping the theme's own `Arc` when there's no outline is what
    // lets every key on a legend share one cache entry per label.
    let sheet = match outline {
        Some((color, width_px)) => {
            let mut sheet = (*theme.rich_text).clone();
            let base = sheet.get("base").cloned().unwrap_or_default();
            sheet.set(
                "base",
                crate::text::rich::StyleDelta {
                    text_stroke: Some(crate::plot::theme::ThemeColor::Fixed(color)),
                    text_stroke_width: Some(crate::text::rich::pt(width_px * 72.0 / dpi)),
                    ..base
                },
            );
            Arc::new(sheet)
        }
        None => Arc::clone(&theme.rich_text),
    };
    Some(RichChrome {
        sheet,
        palette: *palette,
        fill,
    })
}

/// The fill a text key paints with. Palette ink is the backstop for
/// a theme that leaves the geom's text fill unset, so the row isn't
/// visually empty.
fn text_key_fill(
    resolved: &ResolvedKey,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
) -> Color {
    with_opacity(
        resolved
            .fill
            .or_else(|| geom.text.fill.as_ref().map(|c| c.resolve(palette)))
            .unwrap_or(palette.ink),
        resolved.fill_opacity,
    )
}

/// A text key's glyph outline as `(color, width_px)`. `None` when the
/// key paints no outline or the width degenerates.
fn text_key_outline(
    resolved: &ResolvedKey,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
    dpi: f64,
) -> Option<(Color, f64)> {
    if !text_paints_outline(resolved, geom) {
        return None;
    }
    let color = resolved
        .text_stroke
        .or_else(|| geom.text.text_stroke.as_ref().map(|c| c.resolve(palette)))
        .unwrap_or(palette.ink);
    let width_px = pt_to_px(
        resolved
            .text_linewidth_pt
            .unwrap_or(geom.text.text_linewidth_pt),
        dpi,
    );
    match width_px.is_finite() && width_px > 0.0 {
        true => Some((color, width_px)),
        false => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_text(
    resolved: &ResolvedKey,
    cell: Rect,
    scene: &mut dyn SceneBuilder,
    dpi: f64,
    geom: &crate::plot::theme::GeomTheme,
    palette: &crate::plot::theme::Palette,
    theme: &crate::plot::theme::Theme,
) {
    let rich = text_key_rich(resolved, theme, geom, dpi);
    let Some(run) = text_key_run(resolved, dpi, geom, rich.as_ref()) else {
        return;
    };
    let centre = Point::new(
        cell.x0 + (cell.x1 - cell.x0) * 0.5,
        cell.y0 + (cell.y1 - cell.y0) * 0.5,
    );
    // The run is placed by its top-left, so the inked box is centred
    // by backing the ink's own offset out of the origin — the glyphs
    // sit on the cell's middle the way an axis label sits on its
    // tick, rather than hanging off the line box.
    let x = centre.x - run.width() * 0.5;
    let y = centre.y - run.inked_height() * 0.5 - run.ink_top_offset();

    // Rotation is negated so positive reads counter-clockwise on
    // screen, and pivots on the cell centre the glyph is placed
    // against — `TextGeom`'s convention about its own anchor.
    let angle = resolved.angle.unwrap_or(0.0);
    let xform = match angle == 0.0 {
        true => Affine::IDENTITY,
        false => Affine::rotate_about(-angle, centre),
    };

    let fill = text_key_fill(resolved, geom, palette);
    // Outline pass under the fill, the order `TextGeom` draws them
    // in. A markdown key carries its outline on the sheet instead,
    // and `ChromeRun::draw` ignores this argument there.
    let outline = text_key_outline(resolved, geom, palette, dpi).map(|(color, width_px)| {
        crate::plot::chrome::text::TextOutline {
            brush: Brush::Solid(color),
            stroke: Stroke::new(width_px),
        }
    });
    run.draw(
        scene,
        x,
        y,
        &Brush::Solid(fill),
        outline.as_ref(),
        xform,
        PickId::Skip,
    );
}

/// The swatch outline a [`LegendKey::Rect`] paints, rounded when the
/// key carries a corner radius.
fn rect_key_path(cell: Rect, radius_px: f64) -> crate::path::Path {
    match radius_px > 0.0 {
        true => rounded_rect(cell, radius_px),
        false => cell.to_path(0.0),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::theme::Theme;
    use crate::scene::recording::{Op, RecordingScene};
    use std::sync::Arc;

    const DPI: f64 = 96.0;

    fn cell() -> Rect {
        Rect::new(0.0, 0.0, 40.0, 40.0)
    }

    /// A key carrying only a `shape` aesthetic — the state a shape
    /// legend produces when no colour scale is bound alongside it.
    fn shape_only_key(name: &str) -> ResolvedKey {
        ResolvedKey {
            shape: Some(Arc::from(name)),
            ..Default::default()
        }
    }

    fn render(resolved: &ResolvedKey, theme: &Theme) -> RecordingScene {
        let shapes = ShapeRegistry::with_builtins();
        let mut scene = RecordingScene::default();
        render_point(
            resolved,
            cell(),
            &shapes,
            &mut scene,
            DPI,
            &theme.geom,
            &theme.palette,
        );
        scene
    }

    #[test]
    fn path_shape_stroke_lands_at_requested_pixel_width() {
        let mut theme = Theme::default();
        theme.geom.point.stroke = Some(crate::plot::theme::ThemeColor::Ink);
        let mut key = shape_only_key("square");
        key.size_pt = Some(6.0);
        key.linewidth_pt = Some(1.0);
        let scene = render(&key, &theme);

        let size_px = pt_to_px(6.0, DPI);
        let expected = pt_to_px(1.0, DPI) / size_px;
        let widths: Vec<f64> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, .. } => Some(stroke.width),
                _ => None,
            })
            .collect();
        assert!(!widths.is_empty(), "expected a stroked outline");
        for w in widths {
            assert!(
                (w - expected).abs() < 1e-9,
                "stroke width {w} should invert the size_px transform (expected {expected})"
            );
        }
    }

    #[test]
    fn shape_only_key_paints_without_a_color_aesthetic() {
        let scene = render(&shape_only_key("square"), &Theme::default());
        assert!(
            scene
                .ops
                .iter()
                .any(|op| matches!(op, Op::Fill { .. } | Op::Stroke { .. })),
            "a shape key with no colour aesthetic should still paint"
        );
    }

    #[test]
    fn stroke_style_shape_falls_back_on_the_stroke_channel() {
        // `plus` has no fill subpaths, so an ink fill would leave the
        // key blank; the backstop has to land on the stroke instead.
        let scene = render(&shape_only_key("plus"), &Theme::default());
        assert!(
            scene.ops.iter().any(|op| matches!(op, Op::Stroke { .. })),
            "a stroke-style shape key should paint via its outline"
        );
    }

    fn render_line_key(resolved: &ResolvedKey, theme: &Theme) -> RecordingScene {
        let mut scene = RecordingScene::default();
        render_line(
            resolved,
            cell(),
            &ShapeRegistry::with_builtins(),
            &mut scene,
            DPI,
            &theme.geom,
            &theme.palette,
        );
        scene
    }

    /// The one stroke a key renderer emitted, as `(stroke, path bbox)`.
    fn sole_stroke(scene: &RecordingScene) -> (crate::stroke::Stroke, Rect) {
        let mut it = scene.ops.iter().filter_map(|op| match op {
            Op::Stroke { stroke, path, .. } => Some((stroke.clone(), path.bounding_box())),
            _ => None,
        });
        let first = it.next().expect("expected a stroke");
        assert!(it.next().is_none(), "expected exactly one stroke");
        first
    }

    /// The rect a stroke actually paints into: its path bounds grown by
    /// the half-width the outline straddles, plus the cap projection on
    /// the ends for caps that extend past the endpoint.
    fn painted_bounds(stroke: &crate::stroke::Stroke, path_bbox: Rect) -> Rect {
        let half = stroke.width * 0.5;
        let along = match stroke.start_cap {
            Cap::Butt => 0.0,
            Cap::Round | Cap::Square => half,
        };
        Rect::new(
            path_bbox.x0 - along,
            path_bbox.y0 - half,
            path_bbox.x1 + along,
            path_bbox.y1 + half,
        )
    }

    #[test]
    fn line_key_takes_its_cap_and_join_from_the_theme() {
        let theme = Theme::default();
        let (stroke, _) = sole_stroke(&render_line_key(&ResolvedKey::default(), &theme));
        assert_eq!(stroke.start_cap, theme.geom.line.cap);
        assert_eq!(stroke.end_cap, theme.geom.line.cap);
        assert_eq!(stroke.join, theme.geom.line.join);
    }

    #[test]
    fn line_key_cap_aesthetic_overrides_the_theme() {
        let key = ResolvedKey {
            cap: Some(Cap::Round),
            join: Some(Join::Bevel),
            ..Default::default()
        };
        let (stroke, _) = sole_stroke(&render_line_key(&key, &Theme::default()));
        assert_eq!(stroke.start_cap, Cap::Round);
        assert_eq!(stroke.join, Join::Bevel);
    }

    #[test]
    fn thick_line_key_paints_inside_its_cell() {
        // Both caps, at a linewidth the cell only just holds: a
        // round-capped key has to give up the cap projection at each
        // end rather than overhang into the label column.
        for cap in [Cap::Butt, Cap::Round, Cap::Square] {
            let key = ResolvedKey {
                linewidth_pt: Some(21.0),
                cap: Some(cap),
                ..Default::default()
            };
            let (stroke, bbox) = sole_stroke(&render_line_key(&key, &Theme::default()));
            let painted = painted_bounds(&stroke, bbox);
            let c = cell();
            assert!(
                painted.x0 >= c.x0 - 1e-9
                    && painted.x1 <= c.x1 + 1e-9
                    && painted.y0 >= c.y0 - 1e-9
                    && painted.y1 <= c.y1 + 1e-9,
                "{cap:?} line key painted {painted:?} outside its cell {c:?}"
            );
            assert!(bbox.x1 > bbox.x0, "{cap:?} line key collapsed to a point");
        }
    }

    #[test]
    fn rect_key_border_paints_inside_its_cell() {
        let key = ResolvedKey {
            stroke: Some(crate::color::rgb(0.0, 0.0, 0.0)),
            linewidth_pt: Some(9.0),
            ..Default::default()
        };
        let mut scene = RecordingScene::default();
        let theme = Theme::default();
        render_rect(&key, cell(), &mut scene, DPI, &theme.geom, &theme.palette);
        let (stroke, bbox) = sole_stroke(&scene);
        let painted = painted_bounds(&stroke, bbox);
        let c = cell();
        assert!(
            painted.x0 >= c.x0 - 1e-9
                && painted.x1 <= c.x1 + 1e-9
                && painted.y0 >= c.y0 - 1e-9
                && painted.y1 <= c.y1 + 1e-9,
            "rect key border painted {painted:?} outside its cell {c:?}"
        );
        assert_eq!(stroke.join, theme.geom.rect.join);
    }

    #[test]
    fn line_and_rect_cells_grow_with_the_linewidth() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let key = ResolvedKey {
            linewidth_pt: Some(20.0),
            ..Default::default()
        };
        let lw = pt_to_px(20.0, DPI);
        for kind in [LegendKey::Line, LegendKey::Rect] {
            let (_, h) = swatch_dim_for(kind, &key, DPI, &theme.geom, &shapes, &theme);
            assert!(
                (h - lw).abs() < 1e-9,
                "{kind:?} key should reserve its {lw}px stroke, reserved {h}"
            );
        }
    }

    #[test]
    fn round_capped_line_cell_reserves_a_body_past_the_caps() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let key = ResolvedKey {
            linewidth_pt: Some(20.0),
            cap: Some(Cap::Round),
            ..Default::default()
        };
        let lw = pt_to_px(20.0, DPI);
        let (w, _) = swatch_dim_for(LegendKey::Line, &key, DPI, &theme.geom, &shapes, &theme);
        assert!(
            w > lw,
            "a round-capped key needs room for the caps plus a visible body, reserved {w} for a {lw}px stroke"
        );
    }

    #[test]
    fn point_cell_follows_the_shape_bbox() {
        // `star` reaches further from its centre than the reference
        // circle, so it has to reserve more than a circle key does.
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let mut key = shape_only_key("star");
        key.size_pt = Some(12.0);
        let (star_w, star_h) =
            swatch_dim_for(LegendKey::Point, &key, DPI, &theme.geom, &shapes, &theme);
        let circle = ResolvedKey {
            size_pt: Some(12.0),
            ..Default::default()
        };
        let (circle_w, circle_h) =
            swatch_dim_for(LegendKey::Point, &circle, DPI, &theme.geom, &shapes, &theme);
        assert!(
            star_w > circle_w && star_h > circle_h,
            "star ({star_w}×{star_h}) should reserve more than the circle ({circle_w}×{circle_h})"
        );
    }

    #[test]
    fn point_cell_reserves_the_marker_outline() {
        let mut theme = Theme::default();
        theme.geom.point.stroke = Some(crate::plot::theme::ThemeColor::Ink);
        let shapes = ShapeRegistry::with_builtins();
        let key = ResolvedKey {
            size_pt: Some(8.0),
            linewidth_pt: Some(12.0),
            ..Default::default()
        };
        let (w, h) = swatch_dim_for(LegendKey::Point, &key, DPI, &theme.geom, &shapes, &theme);
        let marker = pt_to_px(8.0, DPI) * 2.0 * POINT_SHAPE_RADIUS;
        let outline = pt_to_px(12.0, DPI);
        assert!((w - (marker + outline)).abs() < 1e-9, "reserved width {w}");
        assert!((h - (marker + outline)).abs() < 1e-9, "reserved height {h}");
    }

    #[test]
    fn fill_only_point_cell_reserves_no_outline() {
        // No stroke colour anywhere means no outline pass, so the
        // linewidth mustn't inflate the cell.
        let mut theme = Theme::default();
        theme.geom.point.fill = Some(crate::plot::theme::ThemeColor::Ink);
        let shapes = ShapeRegistry::with_builtins();
        let key = ResolvedKey {
            size_pt: Some(8.0),
            linewidth_pt: Some(12.0),
            ..Default::default()
        };
        let (w, _) = swatch_dim_for(LegendKey::Point, &key, DPI, &theme.geom, &shapes, &theme);
        let marker = pt_to_px(8.0, DPI) * 2.0 * POINT_SHAPE_RADIUS;
        assert!((w - marker).abs() < 1e-9, "reserved width {w}");
    }

    /// Alpha of the one fill and the one stroke a point key emitted.
    fn point_alphas(resolved: &ResolvedKey, theme: &Theme) -> (Option<f32>, Option<f32>) {
        let scene = render(resolved, theme);
        let mut fill = None;
        let mut stroke = None;
        for op in &scene.ops {
            let (slot, brush) = match op {
                Op::Fill { brush, .. } => (&mut fill, brush),
                Op::Stroke { brush, .. } => (&mut stroke, brush),
                _ => continue,
            };
            if let Brush::Solid(c) = brush {
                *slot = Some(c.components[3]);
            }
        }
        (fill, stroke)
    }

    fn opaque_point_key() -> ResolvedKey {
        ResolvedKey {
            fill: Some(crate::color::rgb(0.2, 0.4, 0.6)),
            stroke: Some(crate::color::rgb(0.0, 0.0, 0.0)),
            ..Default::default()
        }
    }

    #[test]
    fn fill_and_stroke_opacity_act_independently() {
        let key = ResolvedKey {
            fill_opacity: Some(0.25),
            stroke_opacity: Some(0.75),
            ..opaque_point_key()
        };
        let (fill, stroke) = point_alphas(&key, &Theme::default());
        assert_eq!(fill, Some(0.25));
        assert_eq!(stroke, Some(0.75));
    }

    #[test]
    fn an_unset_opacity_leaves_the_colors_own_alpha() {
        let key = ResolvedKey {
            fill: Some(crate::color::Color::new([0.2, 0.4, 0.6, 0.3])),
            stroke_opacity: Some(1.0),
            ..opaque_point_key()
        };
        let (fill, stroke) = point_alphas(&key, &Theme::default());
        assert_eq!(fill, Some(0.3), "no fill_opacity → the colour decides");
        assert_eq!(stroke, Some(1.0));
    }

    #[test]
    fn opacity_overrides_the_colors_own_alpha() {
        // The geoms' `*_opacity` channels replace the colour's alpha
        // rather than scaling it, so a key over a semi-transparent
        // colour has to land on the requested value, not the product.
        let key = ResolvedKey {
            fill: Some(crate::color::Color::new([0.2, 0.4, 0.6, 0.4])),
            fill_opacity: Some(0.8),
            ..Default::default()
        };
        let (fill, _) = point_alphas(&key, &Theme::default());
        assert_eq!(fill, Some(0.8));
    }

    #[test]
    fn point_key_rotates_with_the_angle_aesthetic() {
        use std::f64::consts::FRAC_PI_2;
        let mut key = shape_only_key("triangle-up");
        key.size_pt = Some(10.0);
        key.angle = Some(FRAC_PI_2);
        let scene = render(&key, &Theme::default());
        let xforms: Vec<Affine> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill { transform, .. } => Some(*transform),
                _ => None,
            })
            .collect();
        assert!(!xforms.is_empty(), "expected a filled marker");
        // The apex sits at path-local (0, -0.92); a quarter turn
        // counter-clockwise on screen sends it to the left of centre.
        let centre = Point::new(20.0, 20.0);
        for x in xforms {
            let apex = x * Point::new(0.0, -0.92);
            assert!(
                apex.x < centre.x - 1.0 && (apex.y - centre.y).abs() < 1e-6,
                "apex {apex:?} should swing left of centre {centre:?}"
            );
        }
    }

    #[test]
    fn rotated_point_cell_covers_the_turned_marker() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        // `hline` is wide and flat, so a quarter turn swaps its extents.
        let mut key = shape_only_key("hline");
        key.size_pt = Some(12.0);
        let (flat_w, flat_h) =
            swatch_dim_for(LegendKey::Point, &key, DPI, &theme.geom, &shapes, &theme);
        key.angle = Some(std::f64::consts::FRAC_PI_2);
        let (turned_w, turned_h) =
            swatch_dim_for(LegendKey::Point, &key, DPI, &theme.geom, &shapes, &theme);
        assert!(
            (turned_w - flat_h).abs() < 1e-9 && (turned_h - flat_w).abs() < 1e-9,
            "a quarter turn should swap the reserved extents: {flat_w}×{flat_h} → {turned_w}×{turned_h}"
        );
    }

    #[test]
    fn rect_key_border_dashes_with_the_linetype() {
        let key = ResolvedKey {
            stroke: Some(crate::color::rgb(0.0, 0.0, 0.0)),
            linetype: Some(Arc::from(crate::plot::geom::linetype::dashed().to_vec())),
            ..Default::default()
        };
        let mut scene = RecordingScene::default();
        let theme = Theme::default();
        render_rect(&key, cell(), &mut scene, DPI, &theme.geom, &theme.palette);
        let (stroke, _) = sole_stroke(&scene);
        assert!(
            !stroke.dash_pattern.is_empty(),
            "a rect key with a dashed linetype should dash its border"
        );
    }

    #[test]
    fn dash_offset_phases_the_pattern() {
        let key = ResolvedKey {
            linetype: Some(Arc::from(crate::plot::geom::linetype::dashed().to_vec())),
            dash_offset_pt: Some(3.0),
            ..Default::default()
        };
        let (stroke, _) = sole_stroke(&render_line_key(&key, &Theme::default()));
        assert!((stroke.dash_offset - pt_to_px(3.0, DPI)).abs() < 1e-9);
    }

    fn marker_bearing_pattern() -> ResolvedKey {
        let pattern = crate::plot::geom::linetype::pattern([
            crate::scales::value::LinetypeStep::Dash(4.0),
            crate::scales::value::LinetypeStep::Gap(2.0),
            crate::scales::value::LinetypeStep::Marker(Arc::from("circle")),
            crate::scales::value::LinetypeStep::Gap(4.0),
        ]);
        ResolvedKey {
            stroke: Some(crate::color::rgb(0.0, 0.0, 0.0)),
            linewidth_pt: Some(1.5),
            linetype: Some(Arc::from(pattern.to_vec())),
            ..Default::default()
        }
    }

    #[test]
    fn line_key_stamps_the_markers_in_its_linetype() {
        // A line key stands for a `LineGeom`, which walks the pattern and
        // stamps each marker; the dash steps alone would misrepresent it.
        let scene = render_line_key(&marker_bearing_pattern(), &Theme::default());
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert!(fills > 0, "expected stamped markers along the key");
    }

    #[test]
    fn rect_key_renders_linetype_markers_as_gaps() {
        // `RectGeom` doesn't stamp — its border dashes through the
        // marker-as-gap path — so its key mustn't stamp either.
        let mut scene = RecordingScene::default();
        let theme = Theme::default();
        render_rect(
            &marker_bearing_pattern(),
            cell(),
            &mut scene,
            DPI,
            &theme.geom,
            &theme.palette,
        );
        let (stroke, _) = sole_stroke(&scene);
        assert_eq!(stroke.dash_pattern.len(), 4);
    }

    fn arrow_key() -> ResolvedKey {
        ResolvedKey {
            stroke: Some(crate::color::rgb(0.0, 0.0, 0.0)),
            linewidth_pt: Some(1.5),
            end_marker: EndpointMarkerKey {
                shape: Some(Arc::from("arrow-closed")),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn line_key_stamps_its_endpoint_marker() {
        let plain = render_line_key(
            &ResolvedKey {
                end_marker: EndpointMarkerKey::default(),
                ..arrow_key()
            },
            &Theme::default(),
        );
        let with_arrow = render_line_key(&arrow_key(), &Theme::default());
        assert!(
            with_arrow.ops.len() > plain.ops.len(),
            "an end marker should add draw calls: {} vs {}",
            with_arrow.ops.len(),
            plain.ops.len()
        );
    }

    #[test]
    fn endpoint_marker_trims_the_line_it_terminates() {
        // The arrow's tip has to land at the line's own end, so the
        // stroke gives up the marker's forward extent.
        let plain = sole_stroke(&render_line_key(
            &ResolvedKey {
                end_marker: EndpointMarkerKey::default(),
                ..arrow_key()
            },
            &Theme::default(),
        ));
        let key = arrow_key();
        let scene = render_line_key(&key, &Theme::default());
        let stroked = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { path, .. } => Some(path.bounding_box()),
                _ => None,
            })
            .next()
            .expect("expected a stroked body");
        assert!(
            stroked.x1 < plain.1.x1 - 1e-9,
            "marker should trim the line: {stroked:?} vs {:?}",
            plain.1
        );
        // Nothing may spill past the cell: the trim is exactly the
        // marker's forward extent, so the tip lands on the far edge.
        assert!(stroked.x1 <= cell().x1 + 1e-9);
    }

    #[test]
    fn marked_line_cell_covers_the_marker() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let key = arrow_key();
        let (w, h) = swatch_dim_for(LegendKey::Line, &key, DPI, &theme.geom, &shapes, &theme);
        let lw = pt_to_px(1.5, DPI);
        // Default marker size is 3 × linewidth, and `arrow-closed`
        // reaches a full size unit either side of its axis.
        let marker = pt_to_px(3.0 * 1.5, DPI);
        assert!(
            h >= marker && h > lw,
            "cell height {h} should clear the {marker}px marker"
        );
        assert!(
            w > lw * 2.0,
            "cell width {w} should hold a body plus the marker's reach"
        );
    }

    #[test]
    fn a_key_with_no_marker_reserves_no_marker_room() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let key = ResolvedKey {
            linewidth_pt: Some(1.5),
            ..Default::default()
        };
        let (w, h) = swatch_dim_for(LegendKey::Line, &key, DPI, &theme.geom, &shapes, &theme);
        assert_eq!(w, 0.0, "butt-capped markerless key needs no width floor");
        assert!((h - pt_to_px(1.5, DPI)).abs() < 1e-9, "height {h}");
    }

    #[test]
    fn unknown_marker_shape_draws_nothing_extra() {
        let key = ResolvedKey {
            end_marker: EndpointMarkerKey {
                shape: Some(Arc::from("no-such-shape")),
                ..Default::default()
            },
            ..arrow_key()
        };
        let scene = render_line_key(&key, &Theme::default());
        let (_, bbox) = sole_stroke(&scene);
        // No marker, so no trim either — the body spans the full cell.
        assert!((bbox.x1 - cell().x1).abs() < 1e-9, "body bbox {bbox:?}");
    }

    fn render_text_key(resolved: &ResolvedKey, theme: &Theme) -> RecordingScene {
        let mut scene = RecordingScene::default();
        render_text(
            resolved,
            cell(),
            &mut scene,
            DPI,
            &theme.geom,
            &theme.palette,
            theme,
        );
        scene
    }

    /// The glyph runs a text key emitted, in emission order.
    fn glyph_runs(scene: &RecordingScene) -> Vec<&crate::scene::recording::OwnedGlyphRun> {
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(run) => Some(run),
                _ => None,
            })
            .collect()
    }

    fn text_key(size_pt: f64) -> ResolvedKey {
        ResolvedKey {
            size_pt: Some(size_pt),
            ..Default::default()
        }
    }

    #[test]
    fn text_key_draws_its_default_glyph() {
        let scene = render_text_key(&ResolvedKey::default(), &Theme::default());
        let runs = glyph_runs(&scene);
        assert_eq!(runs.len(), 1, "expected one filled glyph run");
        assert!(runs[0].style.is_none(), "the glyph pass should fill");
        assert!(!runs[0].glyphs.is_empty(), "expected a glyph");
    }

    #[test]
    fn text_key_font_size_follows_the_size_aesthetic() {
        let scene = render_text_key(&text_key(18.0), &Theme::default());
        let runs = glyph_runs(&scene);
        let expected = pt_to_px(18.0, DPI) as f32;
        assert!(
            (runs[0].font_size - expected).abs() < 1e-3,
            "font size {} should be the resolved {expected}px",
            runs[0].font_size
        );
    }

    #[test]
    fn text_key_centres_its_glyphs_in_the_cell() {
        let theme = Theme::default();
        let key = text_key(10.0);
        let scene = render_text_key(&key, &theme);
        let run = text_key_run(&key, DPI, &theme.geom, None).expect("a shaped run");
        let c = cell();
        let expected_x = (c.x0 + c.x1) * 0.5 - run.width() * 0.5;
        let glyph = glyph_runs(&scene)[0].glyphs[0];
        assert!(
            (glyph.x as f64 - expected_x).abs() < 1e-3,
            "glyph x {} should start half a text box left of centre ({expected_x})",
            glyph.x
        );
        // The baseline sits inside the cell: an uncentred key would put
        // it at the cell's top edge or beyond it.
        assert!(
            (glyph.y as f64) > c.y0 && (glyph.y as f64) < c.y1,
            "baseline {} should land inside the cell {c:?}",
            glyph.y
        );
    }

    #[test]
    fn text_key_fill_opacity_overrides_the_colors_alpha() {
        let key = ResolvedKey {
            fill: Some(crate::color::Color::new([0.2, 0.4, 0.6, 0.4])),
            fill_opacity: Some(0.8),
            ..Default::default()
        };
        let scene = render_text_key(&key, &Theme::default());
        let Brush::Solid(c) = glyph_runs(&scene)[0].brush else {
            panic!("expected a solid brush");
        };
        assert_eq!(c.components[3], 0.8);
    }

    #[test]
    fn text_key_outlines_under_its_fill() {
        let key = ResolvedKey {
            text_stroke: Some(crate::color::rgb(0.0, 0.0, 0.0)),
            text_linewidth_pt: Some(2.0),
            ..text_key(14.0)
        };
        let scene = render_text_key(&key, &Theme::default());
        let runs = glyph_runs(&scene);
        assert_eq!(runs.len(), 2, "expected an outline pass and a fill pass");
        let outline = runs[0].style.as_ref().expect("first pass should stroke");
        assert!((outline.width - pt_to_px(2.0, DPI)).abs() < 1e-9);
        assert!(runs[1].style.is_none(), "the fill pass should follow");
    }

    #[test]
    fn text_cell_grows_with_the_font_size() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let (small_w, small_h) = swatch_dim_for(
            LegendKey::Text,
            &text_key(8.0),
            DPI,
            &theme.geom,
            &shapes,
            &theme,
        );
        let (big_w, big_h) = swatch_dim_for(
            LegendKey::Text,
            &text_key(24.0),
            DPI,
            &theme.geom,
            &shapes,
            &theme,
        );
        assert!(
            big_w > small_w && big_h > small_h,
            "a 24pt glyph ({big_w}×{big_h}) should reserve more than an 8pt one ({small_w}×{small_h})"
        );
    }

    #[test]
    fn text_cell_reserves_the_glyph_outline() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let plain = swatch_dim_for(
            LegendKey::Text,
            &text_key(12.0),
            DPI,
            &theme.geom,
            &shapes,
            &theme,
        );
        let outlined = ResolvedKey {
            text_stroke: Some(crate::color::rgb(0.0, 0.0, 0.0)),
            text_linewidth_pt: Some(4.0),
            ..text_key(12.0)
        };
        let (w, h) = swatch_dim_for(
            LegendKey::Text,
            &outlined,
            DPI,
            &theme.geom,
            &shapes,
            &theme,
        );
        let outline = pt_to_px(4.0, DPI);
        assert!((w - (plain.0 + outline)).abs() < 1e-9, "reserved width {w}");
        assert!(
            (h - (plain.1 + outline)).abs() < 1e-9,
            "reserved height {h}"
        );
    }

    #[test]
    fn rotated_text_cell_covers_the_turned_glyph() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        let mut key = text_key(12.0);
        key.text = Some(Arc::from("Legend"));
        let (flat_w, flat_h) =
            swatch_dim_for(LegendKey::Text, &key, DPI, &theme.geom, &shapes, &theme);
        key.angle = Some(std::f64::consts::FRAC_PI_2);
        let (turned_w, turned_h) =
            swatch_dim_for(LegendKey::Text, &key, DPI, &theme.geom, &shapes, &theme);
        assert!(
            (turned_w - flat_h).abs() < 1e-9 && (turned_h - flat_w).abs() < 1e-9,
            "a quarter turn should swap the reserved extents: {flat_w}×{flat_h} → {turned_w}×{turned_h}"
        );
    }

    #[test]
    fn a_degenerate_text_key_draws_and_reserves_nothing() {
        let theme = Theme::default();
        let shapes = ShapeRegistry::with_builtins();
        for key in [
            text_key(0.0),
            ResolvedKey {
                text: Some(Arc::from("")),
                ..Default::default()
            },
        ] {
            assert_eq!(
                swatch_dim_for(LegendKey::Text, &key, DPI, &theme.geom, &shapes, &theme),
                (0.0, 0.0)
            );
            assert!(render_text_key(&key, &theme).ops.is_empty());
        }
    }

    #[test]
    fn rect_key_rounds_its_corners() {
        let key = ResolvedKey {
            fill: Some(crate::color::rgb(0.2, 0.4, 0.6)),
            corner_radius_pt: Some(4.0),
            ..Default::default()
        };
        let mut scene = RecordingScene::default();
        let theme = Theme::default();
        render_rect(&key, cell(), &mut scene, DPI, &theme.geom, &theme.palette);
        let curved = scene.ops.iter().any(|op| match op {
            Op::Fill { path, .. } => path.elements().iter().any(|el| {
                matches!(
                    el,
                    crate::path::PathEl::CurveTo(..) | crate::path::PathEl::QuadTo(..)
                )
            }),
            _ => false,
        });
        assert!(curved, "a corner radius should round the swatch");
    }
}
