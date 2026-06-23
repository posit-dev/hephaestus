//! `GeometryGeom` — one spatial feature per row.
//!
//! Each row carries a [`Geometry`] value (Point, MultiPoint, LineString,
//! MultiLineString, Polygon, MultiPolygon, GeometryCollection, Empty) and
//! is rendered as the corresponding primitive: markers for points, strokes
//! for lines, fills + strokes for polygons. Mixed-variant columns are
//! supported — every row dispatches independently on its own variant.
//!
//! The full union of channels from [`PointGeom`](super::PointGeom),
//! [`LineGeom`](super::LineGeom), and [`PolygonGeom`](super::PolygonGeom)
//! is accepted, minus `x` / `y` / `ring` (carried by the geometry value).
//! Channels that don't apply to a row's variant are resolved but ignored —
//! `size` does nothing on a Polygon row, `expand` does nothing on a Point
//! row.
//!
//! Coordinates are mapped through the `x` and `y` scales bound on
//! the plot, exactly as for the per-vertex geoms — the geometry is walked
//! point-by-point at draw time and each coordinate is fed through
//! `resolve_position` + the panel projection.
//!
//! Per-variant theme defaults are read from `theme.geom.point` /
//! `theme.geom.line` / `theme.geom.polygon`, so a single themed
//! `GeometryGeom` picks up the same defaults as the corresponding typed
//! geoms.

use crate::brush::Brush;
use crate::geometry::{Affine, Point};
use crate::path::FillRule;
use crate::pick::PickId;
use crate::plot::value::Value;
use crate::primitives::CornerRounding;
use crate::scales::geometry::{Coord, Geometry, Polygon as GeoPolygon};
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::shape::{Shape, ShapeKind, ShapeStyle};
use crate::stroke::Stroke;

use super::outline::{
    draw_curve_outline, draw_polygon_fill_and_stroke, expand_polygons, EndpointMarker, OutlineSpec,
    PolygonSpec,
};
use super::point::GLYPH_BBOX_REFERENCE;
use super::resolve::{
    override_alpha, pt_to_px, resolve_angle_channel, resolve_bool_channel_or, resolve_cap_channel,
    resolve_color_channel, resolve_color_channel_or_theme, resolve_join_channel,
    resolve_linetype_channel, resolve_number_channel, resolve_number_channel_or, resolve_pick_id,
    resolve_position, resolve_str_channel_or,
};
use super::state::{finalize_state, require_data_column, GeomState, KeysStrategy};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

/// Miter clamp ratio passed to Clipper2 for `"expand"` offsets — matches
/// `PolygonGeom`.
const MITER_LIMIT: f64 = 4.0;

/// Channel catalog — the union of `PointGeom`, `LineGeom`, and
/// `PolygonGeom`'s channels minus `x` / `y` / `ring`, plus the mandatory
/// `geometry` column. Per-variant applicability is decided at draw time.
const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("geometry", ExpectedOutput::Any),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("fill", ExpectedOutput::Colors),
    ("fill_opacity", ExpectedOutput::Numbers),
    ("stroke", ExpectedOutput::Colors),
    ("stroke_opacity", ExpectedOutput::Numbers),
    ("linewidth", ExpectedOutput::Numbers),
    ("linetype", ExpectedOutput::Linetypes),
    ("dash_offset", ExpectedOutput::Numbers),
    ("cap", ExpectedOutput::Strings),
    ("join", ExpectedOutput::Strings),
    ("expand", ExpectedOutput::Numbers),
    ("corner_radius", ExpectedOutput::Numbers),
    ("corner_max_angle", ExpectedOutput::Numbers),
    ("clip_start_radius", ExpectedOutput::Numbers),
    ("clip_end_radius", ExpectedOutput::Numbers),
    ("size", ExpectedOutput::Numbers),
    ("size_band", ExpectedOutput::Numbers),
    ("shape", ExpectedOutput::Strings),
    ("angle", ExpectedOutput::Numbers),
    ("start_marker", ExpectedOutput::Strings),
    ("end_marker", ExpectedOutput::Strings),
    ("start_marker_size", ExpectedOutput::Numbers),
    ("end_marker_size", ExpectedOutput::Numbers),
    ("start_marker_fill", ExpectedOutput::Colors),
    ("end_marker_fill", ExpectedOutput::Colors),
    ("start_marker_invert", ExpectedOutput::Any),
    ("end_marker_invert", ExpectedOutput::Any),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── GeometryGeom ────────────────────────────────────────────────────────────

/// A vectorised spatial-geometry geom. Each row holds one [`Geometry`]
/// value and renders as the matching primitive at draw time.
pub struct GeometryGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(GeometryGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for GeometryGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let geom_col = require_data_column("geometry", &channels, "GeometryGeom");
        if !matches!(geom_col, crate::plot::value::DataColumn::Geometry(_)) {
            panic!(
                "GeometryGeom::build: \"geometry\" must be a DataColumn::Geometry; got a different variant"
            );
        }
        let n = geom_col.len();
        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::PerRow,
            CHANNELS,
            "GeometryGeom",
        );
        GeometryGeom { state }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for GeometryGeom {
    fn state(&self) -> &GeomState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut GeomState {
        &mut self.state
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>) {
        let panel = ctx.panel_rect;
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        if panel_w <= 0.0 || panel_h <= 0.0 {
            return;
        }
        let n = self.len();
        if n == 0 {
            return;
        }

        // ── Scales ──
        let x_scale = ctx.scale_for("x");
        let y_scale = ctx.scale_for("y");
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let fill_scale = ctx.scale_for("fill");
        let stroke_scale = ctx.scale_for("stroke");
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let stroke_opacity_scale = ctx.scale_for("stroke_opacity");
        let linewidth_scale = ctx.scale_for("linewidth");
        let linetype_scale = ctx.scale_for("linetype");
        let dash_offset_scale = ctx.scale_for("dash_offset");
        let cap_scale = ctx.scale_for("cap");
        let join_scale = ctx.scale_for("join");
        let expand_scale = ctx.scale_for("expand");
        let corner_radius_scale = ctx.scale_for("corner_radius");
        let corner_max_angle_scale = ctx.scale_for("corner_max_angle");
        let size_scale = ctx.scale_for("size");
        let angle_scale = ctx.scale_for("angle");
        let pick_id_scale = ctx.scale_for("pick_id");
        let clip_start_radius_scale = ctx.scale_for("clip_start_radius");
        let clip_end_radius_scale = ctx.scale_for("clip_end_radius");
        let start_marker_scale = ctx.scale_for("start_marker");
        let end_marker_scale = ctx.scale_for("end_marker");
        let start_marker_size_scale = ctx.scale_for("start_marker_size");
        let end_marker_size_scale = ctx.scale_for("end_marker_size");
        let start_marker_fill_scale = ctx.scale_for("start_marker_fill");
        let end_marker_fill_scale = ctx.scale_for("end_marker_fill");
        let start_marker_invert_scale = ctx.scale_for("start_marker_invert");
        let end_marker_invert_scale = ctx.scale_for("end_marker_invert");

        // ── Channels ──
        let channels = &self.state.channels;
        let geom_col = match channels.get("geometry") {
            Some(Channel::Data(c)) | Some(Channel::RawData(c)) => c,
            _ => return,
        };
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let fill_ch = channels.get("fill");
        let stroke_ch = channels.get("stroke");
        let fill_opacity_ch = channels.get("fill_opacity");
        let stroke_opacity_ch = channels.get("stroke_opacity");
        let linewidth_ch = channels.get("linewidth");
        let linetype_ch = channels.get("linetype");
        let dash_offset_ch = channels.get("dash_offset");
        let cap_ch = channels.get("cap");
        let join_ch = channels.get("join");
        let expand_ch = channels.get("expand");
        let corner_radius_ch = channels.get("corner_radius");
        let corner_max_angle_ch = channels.get("corner_max_angle");
        let size_ch = channels.get("size");
        let shape_ch = channels.get("shape");
        let angle_ch = channels.get("angle");
        let pick_id_ch = channels.get("pick_id");
        let clip_start_radius_ch = channels.get("clip_start_radius");
        let clip_end_radius_ch = channels.get("clip_end_radius");
        let start_marker_ch = channels.get("start_marker");
        let end_marker_ch = channels.get("end_marker");
        let start_marker_size_ch = channels.get("start_marker_size");
        let end_marker_size_ch = channels.get("end_marker_size");
        let start_marker_fill_ch = channels.get("start_marker_fill");
        let end_marker_fill_ch = channels.get("end_marker_fill");
        let start_marker_invert_ch = channels.get("start_marker_invert");
        let end_marker_invert_ch = channels.get("end_marker_invert");

        // Project an (x, y) data coordinate through the bound scales,
        // offsets, and panel projection — returns panel-space pixels.
        // Per-row constants are passed in so the closure stays Fn (no
        // mutable state).
        let project = |xy: Coord, x_band: f64, y_band: f64, dx_px: f64, dy_px: f64| -> Point {
            let x_frac = resolve_position(Value::Number(xy.0), x_scale, x_band);
            let y_frac = resolve_position(Value::Number(xy.1), y_scale, y_band);
            let (px, py) = ctx.projection.project_to_panel_px(panel, &[x_frac, y_frac]);
            Point::new(px + dx_px, py - dy_px)
        };

        for i in 0..n {
            let geom = match geom_col.get(i) {
                Value::Geometry(g) => g,
                Value::Null => continue,
                _ => continue,
            };
            if geom.is_empty() {
                continue;
            }

            // Universal per-row offsets (translation in pt → px) and
            // band offsets (panel fractions, folded into the position
            // resolve so band-aware scales pick them up).
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
            let dx_px = resolve_number_channel(x_offset_ch, x_offset_scale, i)
                .map(|pt| pt_to_px(pt, ctx.dpi))
                .unwrap_or(0.0);
            let dy_px = resolve_number_channel(y_offset_ch, y_offset_scale, i)
                .map(|pt| pt_to_px(pt, ctx.dpi))
                .unwrap_or(0.0);
            let angle = resolve_angle_channel(angle_ch, angle_scale, i);
            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);

            // Per-variant render. Each branch reads only the channels its
            // primitive understands.
            draw_geometry(
                scene,
                &geom,
                ctx,
                DrawCtx {
                    i,
                    x_band,
                    y_band,
                    dx_px,
                    dy_px,
                    angle,
                    pick,
                    project: &project,
                    fill_ch,
                    fill_scale,
                    fill_opacity_ch,
                    fill_opacity_scale,
                    stroke_ch,
                    stroke_scale,
                    stroke_opacity_ch,
                    stroke_opacity_scale,
                    linewidth_ch,
                    linewidth_scale,
                    linetype_ch,
                    linetype_scale,
                    dash_offset_ch,
                    dash_offset_scale,
                    cap_ch,
                    cap_scale,
                    join_ch,
                    join_scale,
                    expand_ch,
                    expand_scale,
                    corner_radius_ch,
                    corner_radius_scale,
                    corner_max_angle_ch,
                    corner_max_angle_scale,
                    size_ch,
                    size_scale,
                    shape_ch,
                    clip_start_radius_ch,
                    clip_start_radius_scale,
                    clip_end_radius_ch,
                    clip_end_radius_scale,
                    start_marker_ch,
                    start_marker_scale,
                    end_marker_ch,
                    end_marker_scale,
                    start_marker_size_ch,
                    start_marker_size_scale,
                    end_marker_size_ch,
                    end_marker_size_scale,
                    start_marker_fill_ch,
                    start_marker_fill_scale,
                    end_marker_fill_ch,
                    end_marker_fill_scale,
                    start_marker_invert_ch,
                    start_marker_invert_scale,
                    end_marker_invert_ch,
                    end_marker_invert_scale,
                },
            );
        }
    }
}

// ─── Per-row dispatch ────────────────────────────────────────────────────────

/// Bundle of per-row channel context passed down into `draw_geometry`.
/// Keeps the dispatch helper free of a long parameter list while still
/// avoiding shared mutable state.
struct DrawCtx<'a, F>
where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
{
    i: usize,
    x_band: f64,
    y_band: f64,
    dx_px: f64,
    dy_px: f64,
    angle: f64,
    pick: PickId,
    project: &'a F,
    fill_ch: Option<&'a Channel>,
    fill_scale: Option<&'a crate::plot::scale::Scale>,
    fill_opacity_ch: Option<&'a Channel>,
    fill_opacity_scale: Option<&'a crate::plot::scale::Scale>,
    stroke_ch: Option<&'a Channel>,
    stroke_scale: Option<&'a crate::plot::scale::Scale>,
    stroke_opacity_ch: Option<&'a Channel>,
    stroke_opacity_scale: Option<&'a crate::plot::scale::Scale>,
    linewidth_ch: Option<&'a Channel>,
    linewidth_scale: Option<&'a crate::plot::scale::Scale>,
    linetype_ch: Option<&'a Channel>,
    linetype_scale: Option<&'a crate::plot::scale::Scale>,
    dash_offset_ch: Option<&'a Channel>,
    dash_offset_scale: Option<&'a crate::plot::scale::Scale>,
    cap_ch: Option<&'a Channel>,
    cap_scale: Option<&'a crate::plot::scale::Scale>,
    join_ch: Option<&'a Channel>,
    join_scale: Option<&'a crate::plot::scale::Scale>,
    expand_ch: Option<&'a Channel>,
    expand_scale: Option<&'a crate::plot::scale::Scale>,
    corner_radius_ch: Option<&'a Channel>,
    corner_radius_scale: Option<&'a crate::plot::scale::Scale>,
    corner_max_angle_ch: Option<&'a Channel>,
    corner_max_angle_scale: Option<&'a crate::plot::scale::Scale>,
    size_ch: Option<&'a Channel>,
    size_scale: Option<&'a crate::plot::scale::Scale>,
    shape_ch: Option<&'a Channel>,
    clip_start_radius_ch: Option<&'a Channel>,
    clip_start_radius_scale: Option<&'a crate::plot::scale::Scale>,
    clip_end_radius_ch: Option<&'a Channel>,
    clip_end_radius_scale: Option<&'a crate::plot::scale::Scale>,
    start_marker_ch: Option<&'a Channel>,
    start_marker_scale: Option<&'a crate::plot::scale::Scale>,
    end_marker_ch: Option<&'a Channel>,
    end_marker_scale: Option<&'a crate::plot::scale::Scale>,
    start_marker_size_ch: Option<&'a Channel>,
    start_marker_size_scale: Option<&'a crate::plot::scale::Scale>,
    end_marker_size_ch: Option<&'a Channel>,
    end_marker_size_scale: Option<&'a crate::plot::scale::Scale>,
    start_marker_fill_ch: Option<&'a Channel>,
    start_marker_fill_scale: Option<&'a crate::plot::scale::Scale>,
    end_marker_fill_ch: Option<&'a Channel>,
    end_marker_fill_scale: Option<&'a crate::plot::scale::Scale>,
    start_marker_invert_ch: Option<&'a Channel>,
    start_marker_invert_scale: Option<&'a crate::plot::scale::Scale>,
    end_marker_invert_ch: Option<&'a Channel>,
    end_marker_invert_scale: Option<&'a crate::plot::scale::Scale>,
}

fn draw_geometry<F>(
    scene: &mut dyn SceneBuilder,
    geom: &Geometry,
    ctx: &GeomContext<'_>,
    dc: DrawCtx<'_, F>,
) where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
{
    match geom {
        Geometry::Empty => {}
        Geometry::Point(c) => draw_point(scene, &[*c], ctx, &dc),
        Geometry::MultiPoint(cs) => draw_point(scene, cs, ctx, &dc),
        Geometry::LineString(cs) => draw_lines(scene, std::slice::from_ref(cs), ctx, &dc),
        Geometry::MultiLineString(lines) => draw_lines(scene, lines, ctx, &dc),
        Geometry::Polygon(p) => draw_polygons(scene, std::slice::from_ref(p), ctx, &dc),
        Geometry::MultiPolygon(ps) => draw_polygons(scene, ps, ctx, &dc),
        Geometry::GeometryCollection(children) => {
            for child in children {
                // Shallow-copy the per-row context so each child sees the
                // same row styling — `DrawCtx` is small (references and
                // resolved scalars), so the clone is cheap.
                draw_geometry(
                    scene,
                    child,
                    ctx,
                    DrawCtx {
                        project: dc.project,
                        ..clone_ctx(&dc)
                    },
                );
            }
        }
    }
}

fn clone_ctx<'a, F>(dc: &DrawCtx<'a, F>) -> DrawCtx<'a, F>
where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
{
    DrawCtx {
        i: dc.i,
        x_band: dc.x_band,
        y_band: dc.y_band,
        dx_px: dc.dx_px,
        dy_px: dc.dy_px,
        angle: dc.angle,
        pick: dc.pick,
        project: dc.project,
        fill_ch: dc.fill_ch,
        fill_scale: dc.fill_scale,
        fill_opacity_ch: dc.fill_opacity_ch,
        fill_opacity_scale: dc.fill_opacity_scale,
        stroke_ch: dc.stroke_ch,
        stroke_scale: dc.stroke_scale,
        stroke_opacity_ch: dc.stroke_opacity_ch,
        stroke_opacity_scale: dc.stroke_opacity_scale,
        linewidth_ch: dc.linewidth_ch,
        linewidth_scale: dc.linewidth_scale,
        linetype_ch: dc.linetype_ch,
        linetype_scale: dc.linetype_scale,
        dash_offset_ch: dc.dash_offset_ch,
        dash_offset_scale: dc.dash_offset_scale,
        cap_ch: dc.cap_ch,
        cap_scale: dc.cap_scale,
        join_ch: dc.join_ch,
        join_scale: dc.join_scale,
        expand_ch: dc.expand_ch,
        expand_scale: dc.expand_scale,
        corner_radius_ch: dc.corner_radius_ch,
        corner_radius_scale: dc.corner_radius_scale,
        corner_max_angle_ch: dc.corner_max_angle_ch,
        corner_max_angle_scale: dc.corner_max_angle_scale,
        size_ch: dc.size_ch,
        size_scale: dc.size_scale,
        shape_ch: dc.shape_ch,
        clip_start_radius_ch: dc.clip_start_radius_ch,
        clip_start_radius_scale: dc.clip_start_radius_scale,
        clip_end_radius_ch: dc.clip_end_radius_ch,
        clip_end_radius_scale: dc.clip_end_radius_scale,
        start_marker_ch: dc.start_marker_ch,
        start_marker_scale: dc.start_marker_scale,
        end_marker_ch: dc.end_marker_ch,
        end_marker_scale: dc.end_marker_scale,
        start_marker_size_ch: dc.start_marker_size_ch,
        start_marker_size_scale: dc.start_marker_size_scale,
        end_marker_size_ch: dc.end_marker_size_ch,
        end_marker_size_scale: dc.end_marker_size_scale,
        start_marker_fill_ch: dc.start_marker_fill_ch,
        start_marker_fill_scale: dc.start_marker_fill_scale,
        end_marker_fill_ch: dc.end_marker_fill_ch,
        end_marker_fill_scale: dc.end_marker_fill_scale,
        start_marker_invert_ch: dc.start_marker_invert_ch,
        start_marker_invert_scale: dc.start_marker_invert_scale,
        end_marker_invert_ch: dc.end_marker_invert_ch,
        end_marker_invert_scale: dc.end_marker_invert_scale,
    }
}

// ─── Point / MultiPoint ──────────────────────────────────────────────────────

fn draw_point<F>(
    scene: &mut dyn SceneBuilder,
    coords: &[Coord],
    ctx: &GeomContext<'_>,
    dc: &DrawCtx<'_, F>,
) where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
{
    let fill_color = override_alpha(
        resolve_color_channel_or_theme(
            dc.fill_ch,
            dc.fill_scale,
            dc.i,
            ctx.theme.geom.point.fill.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(dc.fill_opacity_ch, dc.fill_opacity_scale, dc.i),
    );
    let stroke_color = override_alpha(
        resolve_color_channel_or_theme(
            dc.stroke_ch,
            dc.stroke_scale,
            dc.i,
            ctx.theme.geom.point.stroke.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(dc.stroke_opacity_ch, dc.stroke_opacity_scale, dc.i),
    );

    let size_pt = resolve_number_channel_or(
        dc.size_ch,
        dc.size_scale,
        dc.i,
        ctx.theme.geom.point.size_pt,
    );
    let size_px = pt_to_px(size_pt, ctx.dpi);
    if !size_px.is_finite() || size_px <= 0.0 {
        return;
    }
    let shape_name = resolve_str_channel_or(dc.shape_ch, None, dc.i, &ctx.theme.geom.point.shape);
    let shape: &Shape = match ctx.shapes.get(&shape_name) {
        Some(s) => s,
        None => return,
    };
    let stroke_width_pt = resolve_number_channel_or(
        dc.linewidth_ch,
        dc.linewidth_scale,
        dc.i,
        ctx.theme.geom.point.stroke_width_pt,
    );
    let stroke_width_local = pt_to_px(stroke_width_pt, ctx.dpi) / size_px;

    for c in coords {
        let pt = (dc.project)(*c, dc.x_band, dc.y_band, dc.dx_px, dc.dy_px);
        if !pt.x.is_finite() || !pt.y.is_finite() {
            continue;
        }
        let xform = if dc.angle == 0.0 {
            Affine::translate((pt.x, pt.y)) * Affine::scale(size_px)
        } else {
            Affine::translate((pt.x, pt.y)) * Affine::rotate(-dc.angle) * Affine::scale(size_px)
        };
        match shape.kind() {
            ShapeKind::Paths { paths, style } => {
                for sub in paths {
                    match style {
                        ShapeStyle::Fill => {
                            if let Some(fc) = fill_color {
                                scene.fill(
                                    FillRule::NonZero,
                                    xform,
                                    &Brush::Solid(fc),
                                    None,
                                    sub,
                                    dc.pick,
                                );
                            }
                            if let Some(sc) = stroke_color {
                                let st = Stroke::new(stroke_width_local);
                                scene.stroke(&st, xform, &Brush::Solid(sc), None, sub, dc.pick);
                            }
                        }
                        ShapeStyle::Stroke => {
                            if let Some(sc) = stroke_color {
                                let st = Stroke::new(stroke_width_local);
                                scene.stroke(&st, xform, &Brush::Solid(sc), None, sub, dc.pick);
                            }
                        }
                    }
                }
            }
            ShapeKind::Glyph {
                font,
                glyph_id,
                em_bbox,
                em_origin,
            } => {
                let Some(fc) = fill_color else { continue };
                let h = em_bbox.height();
                if h <= 0.0 || !h.is_finite() {
                    continue;
                }
                let bbox_norm = GLYPH_BBOX_REFERENCE / h;
                let effective_font_size_px = size_px * bbox_norm;
                let centring_px =
                    (em_origin.to_vec2() - em_bbox.center().to_vec2()) * effective_font_size_px;
                let glyphs = [Glyph {
                    id: glyph_id,
                    x: 0.0,
                    y: 0.0,
                }];
                let brush = Brush::Solid(fc);
                let run = GlyphRun {
                    font,
                    font_size: effective_font_size_px as f32,
                    transform: Affine::translate((pt.x + centring_px.x, pt.y + centring_px.y)),
                    glyph_transform: None,
                    brush: &brush,
                    brush_alpha: 1.0,
                    hint: false,
                    glyphs: &glyphs,
                    style: None,
                };
                scene.draw_glyphs(&run, dc.pick);
            }
        }
    }
}

// ─── LineString / MultiLineString ────────────────────────────────────────────

fn draw_lines<F>(
    scene: &mut dyn SceneBuilder,
    lines: &[Vec<Coord>],
    ctx: &GeomContext<'_>,
    dc: &DrawCtx<'_, F>,
) where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
{
    let stroke_color = override_alpha(
        resolve_color_channel_or_theme(
            dc.stroke_ch,
            dc.stroke_scale,
            dc.i,
            ctx.theme.geom.line.stroke.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(dc.stroke_opacity_ch, dc.stroke_opacity_scale, dc.i),
    );
    let Some(sc) = stroke_color else { return };
    let linewidth_pt = resolve_number_channel_or(
        dc.linewidth_ch,
        dc.linewidth_scale,
        dc.i,
        ctx.theme.geom.line.linewidth_pt,
    );
    let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
    if !linewidth_px.is_finite() || linewidth_px <= 0.0 {
        return;
    }
    let cap = resolve_cap_channel(dc.cap_ch, dc.cap_scale, dc.i, ctx.theme.geom.line.cap);
    let join = resolve_join_channel(dc.join_ch, dc.join_scale, dc.i, ctx.theme.geom.line.join);
    let dash_pattern_pt = resolve_linetype_channel(dc.linetype_ch, dc.linetype_scale, dc.i);
    let dash_offset_pt =
        resolve_number_channel_or(dc.dash_offset_ch, dc.dash_offset_scale, dc.i, 0.0);
    let corner_radius_pt =
        resolve_number_channel_or(dc.corner_radius_ch, dc.corner_radius_scale, dc.i, 0.0);
    let corner_radius_px = pt_to_px(corner_radius_pt, ctx.dpi);
    let corner_max_angle_deg = resolve_number_channel_or(
        dc.corner_max_angle_ch,
        dc.corner_max_angle_scale,
        dc.i,
        f64::INFINITY,
    );
    // Marker stamps in the dash pattern inherit the fill color when bound,
    // else fall back to the stroke color — same contract as `LineGeom`.
    let marker_fill = resolve_color_channel(dc.fill_ch, dc.fill_scale, dc.i).unwrap_or(sc);

    let xform = rotation_about_centroid(lines.iter().flat_map(|l| l.iter().copied()), dc);

    let corner_rounding = (corner_radius_px > 0.0).then_some(CornerRounding {
        max_cut: corner_radius_px,
        max_angle_deg: corner_max_angle_deg,
    });

    // Endpoint markers + endpoint clip — per-row resolution shared
    // across every line in this row's geometry.
    let start_name = resolve_str_channel_or(dc.start_marker_ch, dc.start_marker_scale, dc.i, "");
    let end_name = resolve_str_channel_or(dc.end_marker_ch, dc.end_marker_scale, dc.i, "");
    let default_marker_size_pt = 3.0 * linewidth_pt;
    let start_marker_size_pt = resolve_number_channel_or(
        dc.start_marker_size_ch,
        dc.start_marker_size_scale,
        dc.i,
        default_marker_size_pt,
    );
    let end_marker_size_pt = resolve_number_channel_or(
        dc.end_marker_size_ch,
        dc.end_marker_size_scale,
        dc.i,
        default_marker_size_pt,
    );
    let start_marker_fill =
        resolve_color_channel(dc.start_marker_fill_ch, dc.start_marker_fill_scale, dc.i);
    let end_marker_fill =
        resolve_color_channel(dc.end_marker_fill_ch, dc.end_marker_fill_scale, dc.i);
    let start_invert = resolve_bool_channel_or(
        dc.start_marker_invert_ch,
        dc.start_marker_invert_scale,
        dc.i,
        false,
    );
    let end_invert = resolve_bool_channel_or(
        dc.end_marker_invert_ch,
        dc.end_marker_invert_scale,
        dc.i,
        false,
    );
    let user_clip_start_pt = resolve_number_channel_or(
        dc.clip_start_radius_ch,
        dc.clip_start_radius_scale,
        dc.i,
        0.0,
    );
    let user_clip_end_pt =
        resolve_number_channel_or(dc.clip_end_radius_ch, dc.clip_end_radius_scale, dc.i, 0.0);

    for line in lines {
        if line.len() < 2 {
            continue;
        }
        let projected: Vec<Point> = line
            .iter()
            .map(|c| (dc.project)(*c, dc.x_band, dc.y_band, dc.dx_px, dc.dy_px))
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .collect();
        if projected.len() < 2 {
            continue;
        }
        let spec = OutlineSpec {
            stroke_color: sc,
            linewidth_pt,
            dash_pattern_pt: dash_pattern_pt.clone(),
            dash_offset_pt,
            cap,
            join,
            marker_fill,
            user_clip_start_pt,
            user_clip_end_pt,
            start_marker: EndpointMarker {
                name: start_name.clone(),
                size_pt: start_marker_size_pt,
                fill: start_marker_fill,
                invert: start_invert,
            },
            end_marker: EndpointMarker {
                name: end_name.clone(),
                size_pt: end_marker_size_pt,
                fill: end_marker_fill,
                invert: end_invert,
            },
            pick: dc.pick,
            xform,
            corner_rounding,
        };
        draw_curve_outline(scene, ctx, &projected, &spec);
    }
}

// ─── Polygon / MultiPolygon ──────────────────────────────────────────────────

fn draw_polygons<F>(
    scene: &mut dyn SceneBuilder,
    polys: &[GeoPolygon],
    ctx: &GeomContext<'_>,
    dc: &DrawCtx<'_, F>,
) where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
{
    let fill_color = override_alpha(
        resolve_color_channel_or_theme(
            dc.fill_ch,
            dc.fill_scale,
            dc.i,
            ctx.theme.geom.polygon.fill.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(dc.fill_opacity_ch, dc.fill_opacity_scale, dc.i),
    );
    let stroke_color = override_alpha(
        resolve_color_channel_or_theme(
            dc.stroke_ch,
            dc.stroke_scale,
            dc.i,
            ctx.theme.geom.polygon.stroke.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(dc.stroke_opacity_ch, dc.stroke_opacity_scale, dc.i),
    );
    if fill_color.is_none() && stroke_color.is_none() {
        return;
    }
    let expand_pt = resolve_number_channel_or(dc.expand_ch, dc.expand_scale, dc.i, 0.0);
    let expand_px = pt_to_px(expand_pt, ctx.dpi);
    let corner_radius_pt =
        resolve_number_channel_or(dc.corner_radius_ch, dc.corner_radius_scale, dc.i, 0.0);
    let corner_radius_px = pt_to_px(corner_radius_pt, ctx.dpi);
    let corner_max_angle_deg = resolve_number_channel_or(
        dc.corner_max_angle_ch,
        dc.corner_max_angle_scale,
        dc.i,
        f64::INFINITY,
    );

    // Project every ring of every polygon up front so the rotation pivot
    // can be computed against the un-deformed outer ring centroid.
    let mut all_rings: Vec<Vec<Point>> = Vec::new();
    let mut ring_owners: Vec<usize> = Vec::new(); // index into `polys` for each entry
    let mut first_outer_idx: Option<usize> = None;
    for (pi, p) in polys.iter().enumerate() {
        let exterior_px = project_ring(&p.exterior, dc);
        if exterior_px.len() < 3 {
            continue;
        }
        if first_outer_idx.is_none() {
            first_outer_idx = Some(all_rings.len());
        }
        all_rings.push(exterior_px);
        ring_owners.push(pi);
        for hole in &p.interiors {
            let hole_px = project_ring(hole, dc);
            if hole_px.len() >= 3 {
                all_rings.push(hole_px);
                ring_owners.push(pi);
            }
        }
    }
    if all_rings.is_empty() {
        return;
    }

    let xform = if dc.angle == 0.0 {
        Affine::IDENTITY
    } else if let Some(idx) = first_outer_idx {
        let outer = &all_rings[idx];
        let n_pts = outer.len() as f64;
        let cx = outer.iter().map(|p| p.x).sum::<f64>() / n_pts;
        let cy = outer.iter().map(|p| p.y).sum::<f64>() / n_pts;
        Affine::rotate_about(-dc.angle, Point::new(cx, cy))
    } else {
        Affine::IDENTITY
    };

    let pick = dc.pick;
    let linewidth_pt = resolve_number_channel_or(
        dc.linewidth_ch,
        dc.linewidth_scale,
        dc.i,
        ctx.theme.geom.polygon.linewidth_pt,
    );
    let cap = resolve_cap_channel(dc.cap_ch, dc.cap_scale, dc.i, ctx.theme.geom.polygon.cap);
    let join = resolve_join_channel(dc.join_ch, dc.join_scale, dc.i, ctx.theme.geom.polygon.join);
    let dash_pattern_pt = resolve_linetype_channel(dc.linetype_ch, dc.linetype_scale, dc.i);
    let dash_offset_pt =
        resolve_number_channel_or(dc.dash_offset_ch, dc.dash_offset_scale, dc.i, 0.0);
    let corner_rounding = (corner_radius_px > 0.0).then_some(CornerRounding {
        max_cut: corner_radius_px,
        max_angle_deg: corner_max_angle_deg,
    });

    // `ring_owners` is contiguous-grouped (per-parent rings pushed
    // together in the projection loop above), so the helper iterates
    // once without any reshuffling. The marker-fill convention here
    // mirrors PolygonGeom but adds a fill→stroke fallback: dashed
    // marker stamps inherit the polygon fill when bound, else the
    // stroke color.
    let processed_rings = expand_polygons(all_rings, Some(&ring_owners), expand_px, MITER_LIMIT);

    let spec = PolygonSpec {
        fill_color,
        stroke_color,
        linewidth_pt,
        dash_pattern_pt,
        dash_offset_pt,
        cap,
        join,
        corner_rounding,
        marker_fill: fill_color.unwrap_or_else(|| {
            stroke_color.unwrap_or(crate::color::Color::new([0.0, 0.0, 0.0, 1.0]))
        }),
        xform,
        pick,
    };
    draw_polygon_fill_and_stroke(scene, ctx, &processed_rings, &spec);
}

fn project_ring<F>(ring: &[Coord], dc: &DrawCtx<'_, F>) -> Vec<Point>
where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
{
    ring.iter()
        .map(|c| (dc.project)(*c, dc.x_band, dc.y_band, dc.dx_px, dc.dy_px))
        .filter(|p| p.x.is_finite() && p.y.is_finite())
        .collect()
}

/// Pivot for `angle` on a multi-coordinate feature: mean of all projected
/// coordinates. Matches `PolygonGeom`'s "rotate about the outer-ring
/// centroid" convention for the single-polygon case, and gives a sensible
/// generalisation when the feature is a line or a multipart shape.
fn rotation_about_centroid<F, I>(coords: I, dc: &DrawCtx<'_, F>) -> Affine
where
    F: Fn(Coord, f64, f64, f64, f64) -> Point,
    I: IntoIterator<Item = Coord>,
{
    if dc.angle == 0.0 {
        return Affine::IDENTITY;
    }
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut count = 0usize;
    for c in coords {
        let p = (dc.project)(c, dc.x_band, dc.y_band, dc.dx_px, dc.dy_px);
        if p.x.is_finite() && p.y.is_finite() {
            sum_x += p.x;
            sum_y += p.y;
            count += 1;
        }
    }
    if count == 0 {
        return Affine::IDENTITY;
    }
    let cx = sum_x / count as f64;
    let cy = sum_y / count as f64;
    Affine::rotate_about(-dc.angle, Point::new(cx, cy))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::color::Color;
    use crate::geometry::Rect;
    use crate::plot::geom::DirectScaleResolver;
    use crate::plot::scale;
    use crate::scales::geometry::{Geometry, Polygon as GeoPolygon};
    use crate::scene::recording::{Op, RecordingScene};

    fn shapes() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: Rect,
        registry: &'a crate::shape::ShapeRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        GeomContext::new(panel, 96.0, registry, scales)
    }

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    #[test]
    fn build_requires_geometry_channel() {
        let result = std::panic::catch_unwind(|| GeometryGeom::builder().build());
        assert!(
            result.is_err(),
            "build with no geometry channel should panic"
        );
    }

    #[test]
    fn build_with_geometry_column() {
        let g = GeometryGeom::builder()
            .set(
                "geometry",
                vec![
                    Geometry::Point((1.0, 2.0)),
                    Geometry::LineString(vec![(0.0, 0.0), (1.0, 1.0)]),
                ],
            )
            .build();
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn empty_geometry_produces_no_draw_calls() {
        let g = GeometryGeom::builder()
            .set(
                "geometry",
                vec![Geometry::Empty, Geometry::MultiPoint(vec![])],
            )
            .build();
        let registry = shapes();
        let resolver = DirectScaleResolver::new();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let ctx = ctx(panel, &registry, &resolver);
        let mut scene = RecordingScene::new();
        g.draw(&mut scene, &ctx);
        assert!(
            scene
                .ops
                .iter()
                .all(|op| !matches!(op, Op::Fill { .. } | Op::Stroke { .. })),
            "empty rows should not emit fills or strokes"
        );
    }

    #[test]
    fn polygon_emits_fill() {
        let g = GeometryGeom::builder()
            .set(
                "geometry",
                vec![Geometry::Polygon(GeoPolygon::new(vec![
                    (0.0, 0.0),
                    (1.0, 0.0),
                    (1.0, 1.0),
                    (0.0, 1.0),
                    (0.0, 0.0),
                ]))],
            )
            .set("fill", red())
            .build();
        let registry = shapes();
        let xs = scale::continuous(0.0..=1.0);
        let ys = scale::continuous(0.0..=1.0);
        let resolver = DirectScaleResolver::new().with("x", &xs).with("y", &ys);
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let ctx = ctx(panel, &registry, &resolver);
        let mut scene = RecordingScene::new();
        g.draw(&mut scene, &ctx);
        assert!(
            scene.ops.iter().any(|op| matches!(op, Op::Fill { .. })),
            "polygon with fill should emit a Fill op"
        );
    }

    #[test]
    fn linestring_emits_stroke() {
        let g = GeometryGeom::builder()
            .set(
                "geometry",
                vec![Geometry::LineString(vec![(0.0, 0.0), (1.0, 1.0)])],
            )
            .set("stroke", red())
            .build();
        let registry = shapes();
        let xs = scale::continuous(0.0..=1.0);
        let ys = scale::continuous(0.0..=1.0);
        let resolver = DirectScaleResolver::new().with("x", &xs).with("y", &ys);
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let ctx = ctx(panel, &registry, &resolver);
        let mut scene = RecordingScene::new();
        g.draw(&mut scene, &ctx);
        assert!(
            scene.ops.iter().any(|op| matches!(op, Op::Stroke { .. })),
            "linestring with stroke should emit a Stroke op"
        );
    }

    #[test]
    fn point_emits_fill_through_marker() {
        let g = GeometryGeom::builder()
            .set("geometry", vec![Geometry::Point((0.5, 0.5))])
            .set("fill", red())
            .build();
        let registry = shapes();
        let xs = scale::continuous(0.0..=1.0);
        let ys = scale::continuous(0.0..=1.0);
        let resolver = DirectScaleResolver::new().with("x", &xs).with("y", &ys);
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let ctx = ctx(panel, &registry, &resolver);
        let mut scene = RecordingScene::new();
        g.draw(&mut scene, &ctx);
        // Default circle marker is a fill-style shape.
        assert!(
            scene.ops.iter().any(|op| matches!(op, Op::Fill { .. })),
            "point with fill should emit at least one Fill op"
        );
    }

    #[test]
    fn mixed_variants_in_one_column() {
        let g = GeometryGeom::builder()
            .set(
                "geometry",
                vec![
                    Geometry::Point((0.1, 0.1)),
                    Geometry::LineString(vec![(0.2, 0.2), (0.4, 0.4)]),
                    Geometry::Polygon(GeoPolygon::new(vec![
                        (0.5, 0.5),
                        (0.9, 0.5),
                        (0.9, 0.9),
                        (0.5, 0.5),
                    ])),
                ],
            )
            .set("fill", red())
            .set("stroke", red())
            .build();
        let registry = shapes();
        let xs = scale::continuous(0.0..=1.0);
        let ys = scale::continuous(0.0..=1.0);
        let resolver = DirectScaleResolver::new().with("x", &xs).with("y", &ys);
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let ctx = ctx(panel, &registry, &resolver);
        let mut scene = RecordingScene::new();
        g.draw(&mut scene, &ctx);
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
        assert!(
            fills >= 2,
            "expected fills from point and polygon, got {fills}"
        );
        assert!(strokes >= 1, "expected at least one stroke, got {strokes}");
    }

    #[test]
    fn linestring_marker_dash_stamps_shapes() {
        use crate::plot::geom::linetype;
        // A marker-bearing dash pattern: marker, gap, marker, gap, ...
        // routes through `draw_linetype_with_markers`, which fills marker
        // shapes in addition to stroking any dashes.
        let pattern = linetype::pattern([linetype::marker("circle"), linetype::gap(12.0)]);
        let g = GeometryGeom::builder()
            .set(
                "geometry",
                vec![Geometry::LineString(vec![(0.0, 0.0), (1.0, 1.0)])],
            )
            .set("stroke", red())
            .set("linetype", Value::Linetype(pattern))
            .build();
        let registry = shapes();
        let xs = scale::continuous(0.0..=1.0);
        let ys = scale::continuous(0.0..=1.0);
        let resolver = DirectScaleResolver::new().with("x", &xs).with("y", &ys);
        let panel = Rect::new(0.0, 0.0, 200.0, 200.0);
        let ctx = ctx(panel, &registry, &resolver);
        let mut scene = RecordingScene::new();
        g.draw(&mut scene, &ctx);
        // Marker-only pattern: every Marker step becomes a Fill stamp;
        // the marker-stamping path is exercised end-to-end here. A
        // dash+marker mix is exercised by `LineGeom`'s own tests.
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert!(
            fills > 0,
            "marker dash should emit fills for marker stamps, got {fills}"
        );
    }

    #[test]
    fn shared_arc_geometry_works() {
        let shared = Arc::new(Geometry::Point((0.5, 0.5)));
        let g = GeometryGeom::builder()
            .set("geometry", vec![shared.clone(), shared.clone()])
            .set("fill", red())
            .build();
        assert_eq!(g.len(), 2);
    }
}
